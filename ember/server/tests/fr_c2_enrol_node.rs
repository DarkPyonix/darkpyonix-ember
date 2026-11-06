//! FR-C2h (step N2, [provisional]): `ember_node::enrol_cli::run` against the REAL enrol door on a
//! loopback TCP listener (`EnrolDoor::serve`), with the owner's side (`Enrol::confirm` / `deny`)
//! driven from the test through a fake registrar over an in-memory store.
//!
//! CI compiled and ran the tests of PR #614 (before the two write-failure tests below were added,
//! which have not been compiled). Every expected value was read from
//! `ember/server/src/enrol_door.rs`, `ember/server/src/computers/enrol.rs` and
//! `ember/node/src/enrol_cli.rs`.
//!
//! The door allows one poll per enrolment per 2 s, so the node polls every 2100 ms here and a test
//! confirms long before the first poll (a happy-path test takes about 2.1 s). HTTPS is NOT tested:
//! there is no TLS server in this harness. A thin middleware in front of the door counts redeem
//! requests and keeps the poll secret of each redeem answer (that is how the "lost answer" test
//! takes the credential before the node's poll). No tracing capture here (one capture test per
//! binary, issue #549).

use std::collections::{HashMap, VecDeque};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::Response;
use ember_node::enrol::{read_enrolment, ENROL_FILE};
use ember_node::enrol_cli::{run, EnrolIo, Options};
use ember_server::computers::enrol::{Delivery, Enrol, EnrolStatus, NodeInfo, RegisterError, Registrar};
use ember_server::enrol_door::{EnrolDoor, ENROL_LISTEN_VAR};
use ember_server::store::{now_ms, Store};
use serde_json::Value;

const POLL: Duration = Duration::from_millis(2100);

// ---- fixtures -------------------------------------------------------------------------------

/// A registry that lives in a Vec.
#[derive(Default)]
struct Fake {
    registered: Mutex<Vec<(String, String)>>,
}

impl Fake {
    fn count(&self) -> usize {
        self.registered.lock().unwrap().len()
    }
    fn token_of(&self, name: &str) -> String {
        self.registered.lock().unwrap().iter().find(|(n, _)| n == name).map(|(_, t)| t.clone()).expect("registered")
    }
}

impl Registrar for Fake {
    fn name_taken(&self, name: &str) -> bool {
        self.registered.lock().unwrap().iter().any(|(n, _)| n.eq_ignore_ascii_case(name))
    }
    fn register(&self, name: &str, token: &str, _i: &NodeInfo, _o: &str) -> Result<String, RegisterError> {
        if self.name_taken(name) {
            return Err(RegisterError::NameTaken);
        }
        let mut r = self.registered.lock().unwrap();
        r.push((name.to_string(), token.to_string()));
        Ok(format!("computer-{}", r.len()))
    }
    fn remove(&self, _id: &str) {}
}

/// What the middleware in front of the door saw.
#[derive(Default)]
struct Cap {
    redeems: AtomicUsize,
    secrets: Mutex<Vec<String>>,
}

async fn capture(State(cap): State<Arc<Cap>>, req: Request, next: Next) -> Response {
    let is_redeem = req.uri().path() == "/enrol/redeem";
    if is_redeem {
        cap.redeems.fetch_add(1, Ordering::SeqCst);
    }
    let res = next.run(req).await;
    if !is_redeem || res.status() != StatusCode::OK {
        return res;
    }
    let (parts, body) = res.into_parts();
    let bytes = axum::body::to_bytes(body, 64 * 1024).await.unwrap_or_default();
    if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
        if let Some(s) = v["poll_secret"].as_str() {
            cap.secrets.lock().unwrap().push(s.to_string());
        }
    }
    Response::from_parts(parts, Body::from(bytes))
}

struct Harness {
    enrol: Arc<Enrol>,
    fake: Arc<Fake>,
    cap: Arc<Cap>,
    base: String,
    _stop: tokio::sync::oneshot::Sender<()>,
}

fn peer_id() -> String {
    ember_transport::SecretKey::generate().peer_id().to_string()
}

fn fp(id: &str) -> String {
    ember_transport::fingerprint_hex(id).unwrap()
}

/// A door on 127.0.0.1:0 over a fresh `Enrol`. `server_id` is the transport id the door delivers.
async fn start(server_id: Option<String>) -> Harness {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let enrol = Enrol::with_server_peer_id(store, server_id.clone());
    let vars: HashMap<String, String> = [(ENROL_LISTEN_VAR.to_string(), format!("127.0.0.1:{port}"))].into_iter().collect();
    let door = EnrolDoor::from_lookup(|k| vars.get(k).cloned(), "127.0.0.1:1".parse().unwrap(), None)
        .unwrap()
        .expect("door on");
    let cap = Arc::new(Cap::default());
    let app = door.app(enrol.clone()).layer(middleware::from_fn_with_state(cap.clone(), capture));
    let (stop, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(EnrolDoor::serve(listener, app, async move {
        let _ = rx.await;
    }));
    Harness { enrol, fake: Arc::new(Fake::default()), cap, base: format!("http://127.0.0.1:{port}"), _stop: stop }
}

impl Harness {
    /// A fresh code for a computer to be called `name`: (code, enrol id).
    fn mint(&self, name: &str) -> (String, String) {
        let m = self.enrol.mint(name, now_ms(), None, &self.base).unwrap();
        (m.code.expose().to_string(), m.id.clone())
    }

    fn opts(&self, code: &str, dir: &Path) -> Options {
        let mut o = Options::new(code.to_string(), self.base.clone(), dir.to_path_buf());
        o.poll_every = POLL;
        o.yes = true;
        o
    }

    /// Confirms `id` as soon as the node has redeemed it.
    fn confirm_soon(&self, id: &str) -> tokio::task::JoinHandle<()> {
        let (enrol, fake, id) = (self.enrol.clone(), self.fake.clone(), id.to_string());
        tokio::spawn(async move {
            wait_redeemed(&enrol, &id).await;
            enrol.confirm(&id, None, now_ms(), &*fake).unwrap();
        })
    }

    fn deny_soon(&self, id: &str) -> tokio::task::JoinHandle<()> {
        let (enrol, id) = (self.enrol.clone(), id.to_string());
        tokio::spawn(async move {
            wait_redeemed(&enrol, &id).await;
            enrol.deny(&id, now_ms()).unwrap();
        })
    }
}

async fn wait_redeemed(enrol: &Enrol, id: &str) {
    for _ in 0..400 {
        if matches!(enrol.status(id, now_ms()), Ok(EnrolStatus::AwaitingConfirmation)) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the node never redeemed the code");
}

/// A scripted terminal.
struct Script {
    interactive: bool,
    answers: VecDeque<bool>,
    lines: Vec<String>,
}

impl Script {
    fn new(interactive: bool, answers: &[bool]) -> Script {
        Script { interactive, answers: answers.iter().copied().collect(), lines: Vec::new() }
    }
    fn text(&self) -> String {
        self.lines.join("\n")
    }
}

impl EnrolIo for Script {
    fn interactive(&self) -> bool {
        self.interactive
    }
    fn ask(&mut self, prompt: &str) -> bool {
        self.lines.push(prompt.to_string());
        self.answers.pop_front().unwrap_or(false)
    }
    fn say(&mut self, line: &str) {
        self.lines.push(line.to_string());
    }
}

fn peers_path(dir: &Path) -> std::path::PathBuf {
    dir.join("allowed-peers")
}

fn nothing_written(dir: &Path) {
    assert!(!dir.join(ENROL_FILE).exists(), "enrol.json must not exist");
    assert!(!peers_path(dir).exists(), "allowed-peers must not exist");
    let leftovers: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n != ".enrol.lock")
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

// ---- pinned servers -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_enrol_writes_enrol_json_0600_and_allowed_peers() {
    let sid = peer_id();
    let h = start(Some(sid.clone())).await;
    let (code, id) = h.mint("build-box");
    let confirm = h.confirm_soon(&id);
    let dir = tempfile::tempdir().unwrap();
    let mut o = h.opts(&code, dir.path());
    o.pin = Some(sid.clone());
    let mut io = Script::new(false, &[]);
    let out = run(o, &mut io).await.unwrap();
    confirm.await.unwrap();

    assert!(out.server_trusted && out.peer_added);
    assert_eq!((out.name.as_str(), out.computer_id.as_str()), ("build-box", "computer-1"));
    let token = h.fake.token_of("build-box");
    let e = read_enrolment(dir.path()).unwrap().expect("enrol.json");
    assert_eq!(e.node_token, token);
    assert_eq!(e.server_peer_id.as_deref(), Some(sid.as_str()));
    assert_eq!(e.server, h.base);
    let enrol_path = dir.path().join(ENROL_FILE);
    assert_eq!(std::fs::metadata(&enrol_path).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(std::fs::read_to_string(peers_path(dir.path())).unwrap().trim(), sid);
    assert_eq!(std::fs::read_to_string(peers_path(dir.path())).unwrap(), format!("{sid}\n"));
    let said = io.text();
    assert!(!said.contains(&token), "the token is never printed");
    assert!(!said.contains(&code), "the code is never printed");
    assert!(said.contains("SIGHUP") && !said.contains("short pin"), "{said}");
    assert!(said.contains(&fp(&sid)), "{said}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_enrol_short_pin_trusts_with_a_notice() {
    let sid = peer_id();
    let h = start(Some(sid.clone())).await;
    let (code, id) = h.mint("build-box");
    let confirm = h.confirm_soon(&id);
    let dir = tempfile::tempdir().unwrap();
    let mut o = h.opts(&code, dir.path());
    o.pin = Some(fp(&sid).to_uppercase());
    let mut io = Script::new(false, &[]);
    let out = run(o, &mut io).await.unwrap();
    confirm.await.unwrap();
    assert!(out.server_trusted && out.peer_added);
    assert_eq!(std::fs::read_to_string(peers_path(dir.path())).unwrap(), format!("{sid}\n"));
    let said = io.text();
    assert!(said.contains("short pin: 64 bits"), "{said}");
    assert!(!said.contains(&h.fake.token_of("build-box")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_enrol_pin_mismatch_writes_nothing_and_names_the_computer() {
    let sid = peer_id();
    let wrong = peer_id();
    let h = start(Some(sid.clone())).await;
    let (code, id) = h.mint("build-box");
    let confirm = h.confirm_soon(&id);
    let dir = tempfile::tempdir().unwrap();
    let mut o = h.opts(&code, dir.path());
    o.pin = Some(wrong.clone());
    let mut io = Script::new(false, &[]);
    let err = run(o, &mut io).await.unwrap_err();
    confirm.await.unwrap();
    let err = format!("{err:#}");
    nothing_written(dir.path());
    let said = io.text();
    assert!(said.contains(&fp(&sid)) && said.contains(&fp(&wrong)), "both fingerprints are shown: {said}");
    assert!(said.contains("build-box") && said.contains("computer-1"), "{said}");
    assert!(said.contains("remove it in Ember"), "{said}");
    let token = h.fake.token_of("build-box");
    assert!(!said.contains(&token) && !err.contains(&token));
    assert!(err.contains("does not match") && err.contains("nothing was written"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_enrol_pin_without_a_server_id_aborts() {
    let h = start(None).await;
    let (code, id) = h.mint("build-box");
    let confirm = h.confirm_soon(&id);
    let dir = tempfile::tempdir().unwrap();
    let mut o = h.opts(&code, dir.path());
    o.pin = Some(peer_id());
    let mut io = Script::new(false, &[]);
    let err = run(o, &mut io).await.unwrap_err();
    confirm.await.unwrap();
    assert!(format!("{err:#}").contains("nothing was written"));
    nothing_written(dir.path());
    assert!(io.text().contains("remove it in Ember"));
}

// ---- a write that fails after the delivery --------------------------------------------------

/// The server wiped the credential when it delivered it, so a refused write loses the token: the
/// node says so in plain words, names the computer, and prints neither the token nor the secrets.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_enrol_a_refused_write_after_delivery_says_the_token_is_lost() {
    let sid = peer_id();
    let h = start(Some(sid.clone())).await;
    let (code, id) = h.mint("build-box");
    let confirm = h.confirm_soon(&id);
    let dir = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let target = other.path().join("real-peers");
    std::fs::write(&target, format!("{}\n", peer_id())).unwrap();
    std::os::unix::fs::symlink(&target, peers_path(dir.path())).unwrap();
    let mut o = h.opts(&code, dir.path());
    o.pin = Some(sid.clone());
    let mut io = Script::new(false, &[]);
    let err = run(o, &mut io).await.unwrap_err();
    confirm.await.unwrap();
    let err = format!("{err:#}");
    let said = io.text();
    for text in [&err, &said] {
        assert!(text.contains("the token is lost"), "{text}");
        assert!(text.contains("build-box") && text.contains("computer-1"), "{text}");
        assert!(text.contains("Remove the computer"), "{text}");
    }
    assert!(!dir.path().join(ENROL_FILE).exists(), "enrol.json must not exist");
    assert!(std::fs::symlink_metadata(peers_path(dir.path())).unwrap().file_type().is_symlink());
    let token = h.fake.token_of("build-box");
    assert!(!said.contains(&token) && !err.contains(&token), "the token is in no output");
    assert!(!said.contains(&code) && !err.contains(&code), "the code is in no output");
    for secret in h.cap.secrets.lock().unwrap().iter() {
        assert!(!said.contains(secret) && !err.contains(secret), "the poll secret is in no output");
    }
    assert!(!said.contains("Enrolled as"), "{said}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_enrol_a_peer_that_is_already_listed_is_not_added_again() {
    let sid = peer_id();
    let h = start(Some(sid.clone())).await;
    let (code, id) = h.mint("build-box");
    let confirm = h.confirm_soon(&id);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(peers_path(dir.path()), format!("{sid}\n")).unwrap();
    let mut o = h.opts(&code, dir.path());
    o.pin = Some(sid.clone());
    let mut io = Script::new(false, &[]);
    let out = run(o, &mut io).await.unwrap();
    confirm.await.unwrap();
    assert!(out.server_trusted, "the server is trusted");
    assert!(!out.peer_added, "but no new line was added");
    assert_eq!(std::fs::read_to_string(peers_path(dir.path())).unwrap(), format!("{sid}\n"));
    assert!(read_enrolment(dir.path()).unwrap().is_some());
    let said = io.text();
    assert!(!said.contains("SIGHUP"), "nothing to reload: {said}");
}

// ---- no pin ---------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_enrol_no_pin_non_interactive_writes_enrol_json_only() {
    let sid = peer_id();
    let h = start(Some(sid.clone())).await;
    let (code, id) = h.mint("build-box");
    let confirm = h.confirm_soon(&id);
    let dir = tempfile::tempdir().unwrap();
    let mut io = Script::new(false, &[]);
    let out = run(h.opts(&code, dir.path()), &mut io).await.unwrap();
    confirm.await.unwrap();
    assert!(!out.server_trusted && !out.peer_added);
    let e = read_enrolment(dir.path()).unwrap().expect("enrol.json");
    assert_eq!(e.node_token, h.fake.token_of("build-box"));
    assert!(!peers_path(dir.path()).exists(), "allowed-peers is not created");
    let said = io.text();
    assert!(said.contains("allowed-peers") && said.contains(&sid), "it tells how to add the id: {said}");
    assert!(!said.contains(&e.node_token));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_enrol_trust_unverified_adds_the_id() {
    let sid = peer_id();
    let h = start(Some(sid.clone())).await;
    let (code, id) = h.mint("build-box");
    let confirm = h.confirm_soon(&id);
    let dir = tempfile::tempdir().unwrap();
    let mut o = h.opts(&code, dir.path());
    o.trust_unverified = true;
    let mut io = Script::new(false, &[]);
    let out = run(o, &mut io).await.unwrap();
    confirm.await.unwrap();
    assert!(out.server_trusted && out.peer_added);
    assert_eq!(std::fs::read_to_string(peers_path(dir.path())).unwrap(), format!("{sid}\n"));
    assert!(io.text().contains("unverified server"), "{}", io.text());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_enrol_interactive_yes_adds_the_id() {
    let sid = peer_id();
    let h = start(Some(sid.clone())).await;
    let (code, id) = h.mint("build-box");
    let confirm = h.confirm_soon(&id);
    let dir = tempfile::tempdir().unwrap();
    let mut o = h.opts(&code, dir.path());
    o.yes = false;
    // "Continue?" then the fingerprint question.
    let mut io = Script::new(true, &[true, true]);
    let out = run(o, &mut io).await.unwrap();
    confirm.await.unwrap();
    assert!(out.server_trusted && out.peer_added);
    assert_eq!(std::fs::read_to_string(peers_path(dir.path())).unwrap(), format!("{sid}\n"));
    assert!(io.text().contains("Does this match the fingerprint"), "{}", io.text());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_enrol_interactive_no_keeps_enrol_json_and_skips_the_id() {
    let sid = peer_id();
    let h = start(Some(sid.clone())).await;
    let (code, id) = h.mint("build-box");
    let confirm = h.confirm_soon(&id);
    let dir = tempfile::tempdir().unwrap();
    let mut o = h.opts(&code, dir.path());
    o.yes = false;
    let mut io = Script::new(true, &[true, false]);
    let out = run(o, &mut io).await.unwrap();
    confirm.await.unwrap();
    assert!(!out.server_trusted && !out.peer_added);
    assert!(read_enrolment(dir.path()).unwrap().is_some());
    assert!(!peers_path(dir.path()).exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_enrol_yes_never_trusts() {
    let sid = peer_id();
    let h = start(Some(sid.clone())).await;
    let (code, id) = h.mint("build-box");
    let confirm = h.confirm_soon(&id);
    let dir = tempfile::tempdir().unwrap();
    let o = h.opts(&code, dir.path()); // yes = true
    let mut io = Script::new(true, &[true, true, true]);
    let out = run(o, &mut io).await.unwrap();
    confirm.await.unwrap();
    assert!(!out.server_trusted && !out.peer_added);
    assert!(!peers_path(dir.path()).exists());
    assert!(!io.text().contains("Does this match"), "--yes never asks the fingerprint question");
}

// ---- refusals and endings -------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_enrol_refused_code_exits_non_zero_without_detail() {
    let h = start(Some(peer_id())).await;
    let dir = tempfile::tempdir().unwrap();
    let bogus = format!("dpe_{}", "a".repeat(64));
    let mut io = Script::new(false, &[]);
    let err = run(h.opts(&bogus, dir.path()), &mut io).await.unwrap_err();
    assert_eq!(err.to_string(), "this enrol code cannot be used");
    assert_eq!(format!("{err:#}"), "this enrol code cannot be used", "no context chain either");
    nothing_written(dir.path());
    assert_eq!(h.fake.count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_polls_until_denied() {
    let h = start(Some(peer_id())).await;
    let (code, id) = h.mint("build-box");
    let deny = h.deny_soon(&id);
    let dir = tempfile::tempdir().unwrap();
    let mut io = Script::new(false, &[]);
    let err = run(h.opts(&code, dir.path()), &mut io).await.unwrap_err();
    deny.await.unwrap();
    assert!(err.to_string().contains("denied"), "{err}");
    nothing_written(dir.path());
    assert_eq!(h.fake.count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_polls_until_the_deadline() {
    let h = start(Some(peer_id())).await;
    let (code, _id) = h.mint("build-box");
    let dir = tempfile::tempdir().unwrap();
    let mut o = h.opts(&code, dir.path());
    o.deadline = Some(Duration::from_secs(3));
    let mut io = Script::new(false, &[]);
    let err = run(o, &mut io).await.unwrap_err();
    assert!(err.to_string().contains("timed out waiting for the owner"), "{err}");
    nothing_written(dir.path());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_enrol_reports_a_credential_that_was_already_delivered() {
    let h = start(Some(peer_id())).await;
    let (code, id) = h.mint("build-box");
    // Simulates a lost answer: the credential is taken with the node's poll secret before the
    // node's own poll, so the node's poll finds it delivered.
    let (enrol, fake, cap, eid) = (h.enrol.clone(), h.fake.clone(), h.cap.clone(), id.clone());
    let thief = tokio::spawn(async move {
        wait_redeemed(&enrol, &eid).await;
        for _ in 0..400 {
            if !cap.secrets.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let secret = cap.secrets.lock().unwrap()[0].clone();
        enrol.confirm(&eid, None, now_ms(), &*fake).unwrap();
        let d = enrol.take_delivery(&eid, &secret, now_ms()).unwrap();
        assert!(matches!(d, Delivery::Joined { .. }));
    });
    let dir = tempfile::tempdir().unwrap();
    let mut io = Script::new(false, &[]);
    let err = run(h.opts(&code, dir.path()), &mut io).await.unwrap_err();
    thief.await.unwrap();
    assert!(err.to_string().contains("already delivered"), "{err}");
    nothing_written(dir.path());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_refuses_plain_http_to_a_public_address_without_allow_http() {
    let h = start(Some(peer_id())).await;
    let (code, _id) = h.mint("build-box");
    let dir = tempfile::tempdir().unwrap();
    let mut o = h.opts(&code, dir.path());
    o.server = "http://ember.example:1".into();
    let mut io = Script::new(false, &[]);
    let err = run(o, &mut io).await.unwrap_err();
    assert!(err.to_string().contains("plain HTTP"), "{err}");
    assert!(!dir.path().join(".enrol.lock").exists(), "refused before the state dir is touched");
    assert_eq!(h.cap.redeems.load(Ordering::SeqCst), 0, "the door saw no redeem request");
    // The code was not spent.
    assert!(h.enrol.redeem(&code, &NodeInfo::minimal("probe"), now_ms(), "127.0.0.1").is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_enrol_refuses_an_existing_enrol_json_without_replace() {
    let h = start(Some(peer_id())).await;
    let (code, _id) = h.mint("build-box");
    let dir = tempfile::tempdir().unwrap();
    let before = b"{\"previous\":\"enrolment\"}\n".to_vec();
    std::fs::write(dir.path().join(ENROL_FILE), &before).unwrap();
    let mut io = Script::new(false, &[]);
    let err = run(h.opts(&code, dir.path()), &mut io).await.unwrap_err();
    assert!(err.to_string().contains("--replace"), "{err}");
    assert_eq!(std::fs::read(dir.path().join(ENROL_FILE)).unwrap(), before, "unchanged byte for byte");
    assert!(!peers_path(dir.path()).exists());
    assert_eq!(h.cap.redeems.load(Ordering::SeqCst), 0, "refused before any request");
    assert!(h.enrol.redeem(&code, &NodeInfo::minimal("probe"), now_ms(), "127.0.0.1").is_ok());
}

// ---- two nodes, and the token in use --------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_two_nodes_two_codes() {
    let h = start(Some(peer_id())).await;
    let (code_a, id_a) = h.mint("node-a");
    let (code_b, id_b) = h.mint("node-b");
    let (ca, cb) = (h.confirm_soon(&id_a), h.confirm_soon(&id_b));
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (mut io_a, mut io_b) = (Script::new(false, &[]), Script::new(false, &[]));
    let (oa, ob) = (h.opts(&code_a, dir_a.path()), h.opts(&code_b, dir_b.path()));
    let (ra, rb) = tokio::join!(run(oa, &mut io_a), run(ob, &mut io_b));
    ca.await.unwrap();
    cb.await.unwrap();
    let (ra, rb) = (ra.unwrap(), rb.unwrap());
    assert_eq!(ra.name, "node-a");
    assert_eq!(rb.name, "node-b");
    let (ta, tb) = (h.fake.token_of("node-a"), h.fake.token_of("node-b"));
    assert_ne!(ta, tb);
    assert_eq!(read_enrolment(dir_a.path()).unwrap().unwrap().node_token, ta);
    assert_eq!(read_enrolment(dir_b.path()).unwrap().unwrap().node_token, tb);
    assert!(!io_a.text().contains(&tb) && !io_b.text().contains(&ta));
}

/// The lighter form of the acceptance row "the enrolled token authenticates": no in-process node
/// is started here. What is asserted is that `NodeConfig`'s token source (`config::token_from`,
/// the function `from_env` uses when `EMBER_NODE_TOKEN` is unset) returns exactly the token the
/// server's registrar received for this enrolment, and that the environment variable still wins.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_enrol_token_is_the_one_the_node_config_reads() {
    let h = start(Some(peer_id())).await;
    let (code, id) = h.mint("build-box");
    let confirm = h.confirm_soon(&id);
    let dir = tempfile::tempdir().unwrap();
    let mut io = Script::new(false, &[]);
    run(h.opts(&code, dir.path()), &mut io).await.unwrap();
    confirm.await.unwrap();
    let token = h.fake.token_of("build-box");
    assert_eq!(ember_node::config::token_from(None, Some(dir.path())).unwrap(), token);
    assert_eq!(ember_node::config::token_from(Some("env-wins".into()), Some(dir.path())).unwrap(), "env-wins");
}
