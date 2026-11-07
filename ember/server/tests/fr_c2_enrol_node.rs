//! FR-C2h (step N2, [provisional]): `ember_node::enrol_cli::run` against the REAL enrol door on a
//! loopback TCP listener (`EnrolDoor::serve`), with the owner's side (`Enrol::confirm` / `deny`)
//! driven from the test through a fake registrar over an in-memory store.
//!
//! CI compiled and ran the tests of PR #614. The write-failure tests and everything from the N2b
//! follow-up (the preflight tests and the scripted STUB door at the end) have not been compiled. Every expected value was read from
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
            enrol.confirm(&id, None, None, now_ms(), &*fake).unwrap();
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
    // The preflight probe and the staging files are gone.
    let mut left: Vec<String> =
        std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    left.sort();
    assert_eq!(left, vec![".enrol.lock".to_string(), "allowed-peers".to_string(), ENROL_FILE.to_string()]);
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
/// (The failing write is provoked by planting the symlink after the redeem: a symlink planted
/// before is caught by the preflight, see the next test.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_enrol_a_refused_write_after_delivery_says_the_token_is_lost() {
    let sid = peer_id();
    let h = start(Some(sid.clone())).await;
    let (code, id) = h.mint("build-box");
    let dir = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let target = other.path().join("real-peers");
    std::fs::write(&target, format!("{}\n", peer_id())).unwrap();
    // The preflight would refuse a symlink planted beforehand, before the redeem. The symlink
    // appears after the node redeemed (a race the preflight cannot see), just before the owner
    // confirms, so the final write is what refuses.
    let confirm = {
        let (enrol, fake, id, link, target) = (h.enrol.clone(), h.fake.clone(), id.clone(), peers_path(dir.path()), target.clone());
        tokio::spawn(async move {
            wait_redeemed(&enrol, &id).await;
            std::os::unix::fs::symlink(&target, &link).unwrap();
            enrol.confirm(&id, None, None, now_ms(), &*fake).unwrap();
        })
    };
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
        enrol.confirm(&eid, None, None, now_ms(), &*fake).unwrap();
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

// ---- the preflight (N2b) --------------------------------------------------------------------

/// An `allowed-peers` that is a symlink is found before the redeem: the door never sees a
/// request and the code is still good afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_preflight_refuses_a_symlinked_allowed_peers_before_the_redeem() {
    let sid = peer_id();
    let h = start(Some(sid.clone())).await;
    let (code, _id) = h.mint("build-box");
    let dir = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let target = other.path().join("real-peers");
    std::fs::write(&target, format!("{}\n", peer_id())).unwrap();
    std::os::unix::fs::symlink(&target, peers_path(dir.path())).unwrap();
    let mut o = h.opts(&code, dir.path());
    o.pin = Some(sid);
    let mut io = Script::new(false, &[]);
    let err = format!("{:#}", run(o, &mut io).await.unwrap_err());
    assert!(err.contains("symbolic link") && err.contains("nothing was redeemed"), "{err}");
    assert_eq!(h.cap.redeems.load(Ordering::SeqCst), 0, "the door was never called");
    assert!(h.enrol.redeem(&code, &NodeInfo::minimal("probe"), now_ms(), "127.0.0.1").is_ok(), "the code is still good");
    assert!(!dir.path().join(ENROL_FILE).exists());
    assert!(std::fs::symlink_metadata(peers_path(dir.path())).unwrap().file_type().is_symlink());
}

// ---- a scripted STUB door -------------------------------------------------------------------
//
// The real door cannot misbehave on demand, so these tests run `run` against a small axum server
// that answers `POST /enrol/redeem` and `GET /enrol/status/{id}` from a script and counts the
// requests per route. Not tested here: `no_proxy` (it needs a process-wide environment variable,
// which would leak into the other tests of this binary).

#[derive(Clone)]
enum Payload {
    Text(String),
    /// A body of this many bytes with a Content-Length.
    Big(usize),
    /// A chunked body that never ends.
    Endless,
}

#[derive(Clone)]
struct Reply {
    status: u16,
    payload: Payload,
    delay_ms: u64,
    location: Option<String>,
}

impl Reply {
    fn json(status: u16, body: &str) -> Reply {
        Reply { status, payload: Payload::Text(body.to_string()), delay_ms: 0, location: None }
    }
    fn big(n: usize) -> Reply {
        Reply { status: 200, payload: Payload::Big(n), delay_ms: 0, location: None }
    }
    fn endless() -> Reply {
        Reply { status: 200, payload: Payload::Endless, delay_ms: 0, location: None }
    }
    fn redirect(to: &str) -> Reply {
        Reply { status: 302, payload: Payload::Text(String::new()), delay_ms: 0, location: Some(to.to_string()) }
    }
}

#[derive(Default)]
struct Route {
    script: Mutex<VecDeque<Reply>>,
    hits: AtomicUsize,
}

impl Route {
    fn with(replies: Vec<Reply>) -> Route {
        Route { script: Mutex::new(replies.into()), ..Route::default() }
    }
    /// The next reply; the last one repeats.
    fn next(&self) -> Reply {
        self.hits.fetch_add(1, Ordering::SeqCst);
        let mut q = self.script.lock().unwrap();
        if q.len() > 1 {
            q.pop_front().unwrap()
        } else {
            q.front().cloned().unwrap_or_else(|| Reply::json(500, "{}"))
        }
    }
    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

struct Stub {
    redeem: Route,
    status: Route,
}

async fn reply_of(r: Reply) -> Response {
    if r.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(r.delay_ms)).await;
    }
    let mut b = Response::builder().status(r.status).header("content-type", "application/json");
    if let Some(l) = &r.location {
        b = b.header("location", l.as_str());
    }
    let body = match r.payload {
        Payload::Text(t) => Body::from(t),
        Payload::Big(n) => Body::from(vec![b' '; n]),
        Payload::Endless => {
            use futures::StreamExt;
            let chunks = futures::stream::repeat_with(|| Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(vec![b' '; 8192])))
                .then(|c| async move {
                    tokio::task::yield_now().await;
                    c
                });
            Body::from_stream(chunks)
        }
    };
    b.body(body).unwrap()
}

/// A stub door on a loopback port: (its base URL, its counters).
async fn stub(redeem: Vec<Reply>, status: Vec<Reply>) -> (String, Arc<Stub>) {
    let st = Arc::new(Stub { redeem: Route::with(redeem), status: Route::with(status) });
    let app = axum::Router::new()
        .route(
            "/enrol/redeem",
            axum::routing::post(|State(st): State<Arc<Stub>>| async move { reply_of(st.redeem.next()).await }),
        )
        .route(
            "/enrol/status/{id}",
            axum::routing::get(|State(st): State<Arc<Stub>>| async move { reply_of(st.status.next()).await }),
        )
        .with_state(st.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (base, st)
}

const REDEEM_OK: &str = r#"{"enrol_id":"enrol-1","poll_secret":"poll-secret-1","expires_ms":4102444800000}"#;
const STUB_TOKEN: &str = "tok_stub-token.1";

fn joined(server_peer_id: Option<&str>) -> String {
    let peer = server_peer_id.map(|p| format!(r#","server_peer_id":"{p}""#)).unwrap_or_default();
    format!(
        r#"{{"status":"joined","credential":{{"node_token":"{STUB_TOKEN}","name":"stub-box","computer_id":"computer-1"{peer}}}}}"#
    )
}

fn stub_opts(base: &str, dir: &Path) -> Options {
    let mut o = Options::new("dpe_stubcode".to_string(), base.to_string(), dir.to_path_buf());
    o.yes = true;
    o.poll_every = Duration::from_millis(40);
    o.deadline = Some(Duration::from_secs(10));
    o
}

/// N7: a redirect is not followed, so the code never reaches another host.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_stub_a_redirect_from_the_redeem_is_not_followed() {
    let (second, second_st) = stub(vec![Reply::json(200, REDEEM_OK)], vec![Reply::json(200, &joined(None))]).await;
    let (first, first_st) = stub(vec![Reply::redirect(&format!("{second}/enrol/redeem"))], vec![]).await;
    let dir = tempfile::tempdir().unwrap();
    let mut io = Script::new(false, &[]);
    let err = format!("{:#}", run(stub_opts(&first, dir.path()), &mut io).await.unwrap_err());
    assert!(err.contains("302"), "{err}");
    assert_eq!(first_st.redeem.hits(), 1);
    assert_eq!(second_st.redeem.hits(), 0, "the redirect target received nothing");
    assert_eq!(second_st.status.hits(), 0);
    nothing_written(dir.path());
    assert!(!io.text().contains("dpe_stubcode"));
}

/// N12: a response body over 64 KiB is refused, with a Content-Length and when chunked without end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_stub_an_oversized_redeem_answer_is_refused() {
    for reply in [Reply::big(70 * 1024), Reply::endless()] {
        let (base, st) = stub(vec![reply], vec![]).await;
        let dir = tempfile::tempdir().unwrap();
        let mut io = Script::new(false, &[]);
        let err = format!("{:#}", run(stub_opts(&base, dir.path()), &mut io).await.unwrap_err());
        assert!(err.contains("the server's answer is too large"), "{err}");
        assert_eq!(st.status.hits(), 0, "no poll after a refused redeem");
        nothing_written(dir.path());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_stub_an_oversized_poll_answer_is_refused() {
    for reply in [Reply::big(70 * 1024), Reply::endless()] {
        let (base, st) = stub(vec![Reply::json(200, REDEEM_OK)], vec![reply]).await;
        let dir = tempfile::tempdir().unwrap();
        let mut io = Script::new(false, &[]);
        let err = format!("{:#}", run(stub_opts(&base, dir.path()), &mut io).await.unwrap_err());
        assert!(err.contains("the server's answer is too large"), "{err}");
        assert_eq!(st.status.hits(), 1, "the oversized answer stops the run at once");
        nothing_written(dir.path());
    }
}

/// Two 429 answers double the delay (40 ms, then 80, then 160 ms before the third poll); the run
/// still succeeds. No upper bound is asserted. Without a delivered server id nothing is trusted
/// and only `enrol.json` is written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_stub_429_answers_back_the_polling_off() {
    let (base, st) = stub(
        vec![Reply::json(200, REDEEM_OK)],
        vec![Reply::json(429, "{}"), Reply::json(429, "{}"), Reply::json(200, &joined(None))],
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let mut io = Script::new(false, &[]);
    let t0 = std::time::Instant::now();
    let out = run(stub_opts(&base, dir.path()), &mut io).await.unwrap();
    let elapsed = t0.elapsed();
    assert_eq!(st.status.hits(), 3);
    assert!(!out.server_trusted && !out.peer_added);
    assert!(elapsed >= Duration::from_millis(40 + 80 + 160), "the delay doubled: {elapsed:?}");
    let e = read_enrolment(dir.path()).unwrap().expect("enrol.json");
    assert_eq!(e.node_token, STUB_TOKEN);
    assert!(!peers_path(dir.path()).exists());
    assert!(!io.text().contains(STUB_TOKEN));
}

/// A delivered server id that is not 64 lowercase hex aborts with nothing written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_node_stub_a_malformed_delivered_server_id_aborts() {
    let bad = "zz".repeat(32);
    let (base, _st) = stub(vec![Reply::json(200, REDEEM_OK)], vec![Reply::json(200, &joined(Some(&bad)))]).await;
    let dir = tempfile::tempdir().unwrap();
    let mut io = Script::new(false, &[]);
    let err = format!("{:#}", run(stub_opts(&base, dir.path()), &mut io).await.unwrap_err());
    let said = io.text();
    assert!(err.contains("malformed") && err.contains("nothing was written"), "{err}");
    assert!(said.contains("malformed"), "{said}");
    nothing_written(dir.path());
    assert!(!said.contains(STUB_TOKEN) && !err.contains(STUB_TOKEN), "the token is in no output");
    assert!(!said.contains(&bad), "the malformed id is not echoed");
}

// ---- end-to-end transport-only (behind NAT) enrolment & revocation (FR-C2, #625) -------------

/// Send a JSON request directly into an axum Router (`oneshot`) and decode `(StatusCode, Value)`.
async fn app_req(app: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    use tower::ServiceExt;
    let bytes = body.map(|b| serde_json::to_vec(&b).unwrap()).unwrap_or_default();
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(bytes))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let raw = axum::body::to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
    let val = serde_json::from_slice(&raw).unwrap_or(Value::Null);
    (status, val)
}

/// FR-C2 & #625: a transport-only (behind NAT, `url = None`, `transport = true`) node enrols
/// through the real `EnrolDoor` and `ComputersRegistrar` (backed by `Computers::with_transport`),
/// starts its in-process `Node` over the transport with the token from `enrol.json` and the
/// `allowed-peers` written by `enrol_cli::run`, and is listed online by `GET /api/computers`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_enrolled_node_authenticates_and_is_listed_online() {
    use ember_node::api::Node;
    use ember_node::config::NodeConfig;
    use ember_server::agents::scripted::ScriptedAdapter;
    use ember_server::agents::AgentAdapter;
    use ember_server::computers::enrol::{ComputersRegistrar, DoorUrl};
    use ember_server::computers::{self, Computers, Registry};
    use ember_server::session::Sessions;
    use ember_transport::mem::MemNetwork;
    use ember_transport::{Dialer, PeerGate, SecretKey};

    let net = MemNetwork::new();
    let server_t = net.transport();
    let server_id = server_t.peer_id().to_string();
    let dialer = Dialer::new(server_t.clone());

    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let computers = Computers::with_transport(
        Registry::new(store.clone()),
        computers::connector(Some(dialer.clone())),
        None,
        Some(dialer),
    );
    let registrar = Arc::new(ComputersRegistrar::new(computers.clone()).with_server_peer_id(Some(server_id.clone())));
    let enrol = Enrol::with_server_peer_id(store.clone(), Some(server_id.clone()));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let door_base = format!("http://127.0.0.1:{port}");
    let vars: HashMap<String, String> = [(ENROL_LISTEN_VAR.to_string(), format!("127.0.0.1:{port}"))].into_iter().collect();
    let door = EnrolDoor::from_lookup(|k| vars.get(k).cloned(), "127.0.0.1:1".parse().unwrap(), None)
        .unwrap()
        .expect("door on");
    let door_app = door.app(enrol.clone());
    let (_stop, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(EnrolDoor::serve(listener, door_app, async move {
        let _ = rx.await;
    }));

    let owner_app = computers::api::router(computers.clone(), sessions.clone()).merge(computers::enrol::router(
        enrol.clone(),
        DoorUrl::Url(door_base.clone()),
        registrar.clone(),
    ));

    // 1. Owner mints an enrol code over HTTP.
    let (st, minted) = app_req(
        &owner_app,
        "POST",
        "/api/computers/enrol-codes",
        Some(serde_json::json!({ "name": "nat-box" })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{minted}");
    let code = minted["code"].as_str().unwrap().to_string();
    let enrol_id = minted["id"].as_str().unwrap().to_string();
    assert_eq!(minted["server_peer_id"].as_str(), Some(server_id.as_str()));

    // 2. Prepare node state dir and pre-generate its transport key so MemNetwork uses the same key.
    let node_dir = tempfile::tempdir().unwrap();
    let node_key = SecretKey::load_or_generate(node_dir.path().join(ember_node::transport::KEY_FILE)).unwrap();
    let node_peer_id = node_key.peer_id().to_string();
    let node_fp = fp(&node_peer_id);

    // 3. Owner task waits for `awaiting_confirmation`, checks `registered_by == "peer"`, and confirms
    //    with `expect_fingerprint`.
    let owner_for_confirm = owner_app.clone();
    let enrol_for_wait = enrol.clone();
    let enrol_id_clone = enrol_id.clone();
    let node_fp_clone = node_fp.clone();
    let confirm_task = tokio::spawn(async move {
        wait_redeemed(&enrol_for_wait, &enrol_id_clone).await;
        let (st, detail) = app_req(
            &owner_for_confirm,
            "GET",
            &format!("/api/computers/enrol-codes/{enrol_id_clone}"),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{detail}");
        assert_eq!(detail["registered_by"].as_str(), Some("peer"));
        assert_eq!(detail["fingerprint"].as_str(), Some(node_fp_clone.as_str()));

        let (st, confirmed) = app_req(
            &owner_for_confirm,
            "POST",
            &format!("/api/computers/enrol-codes/{enrol_id_clone}/confirm"),
            Some(serde_json::json!({ "expect_fingerprint": node_fp_clone })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{confirmed}");
        confirmed["computer_id"].as_str().unwrap().to_string()
    });

    // 4. Node runs `enrol_cli::run` behind NAT (`url = None`, `transport = true`).
    let mut o = Options::new(code, door_base, node_dir.path().to_path_buf());
    o.poll_every = POLL;
    o.yes = true;
    o.transport = true;
    o.pin = Some(server_id.clone());
    let mut io = Script::new(false, &[]);
    let out = run(o, &mut io).await.unwrap();
    let confirmed_cid = confirm_task.await.unwrap();
    assert_eq!(out.computer_id, confirmed_cid);
    assert!(out.server_trusted && out.peer_added);

    // 5. Start the in-process `Node` over the transport using only what `enrol` wrote on disk.
    let token = ember_node::config::token_from(None, Some(node_dir.path())).unwrap();
    let allowed = ember_node::transport::allowed_peers(Some(node_dir.path())).unwrap();
    assert_eq!(allowed, vec![server_t.peer_id()]);
    let node_t = net.transport_with_key(node_key);
    let mut cfg = NodeConfig::new(token, vec![node_dir.path().to_path_buf()]);
    cfg.state_dir = Some(node_dir.path().to_path_buf());
    let node = Node::new(cfg).unwrap();
    let gate = PeerGate::allow_list(allowed);
    let node_srv = tokio::spawn(ember_node::transport::serve(&node_t, node, gate).unwrap());

    // 6. `GET /api/computers` and `GET /api/computers/{id}` list the transport-only node online!
    let (st, list) = app_req(&owner_app, "GET", "/api/computers", None).await;
    assert_eq!(st, StatusCode::OK, "{list}");
    let items = list.as_array().expect("computers array");
    let entry = items
        .iter()
        .find(|c| c["id"].as_str() == Some(out.computer_id.as_str()))
        .expect("enrolled transport node in list");
    assert_eq!(entry["name"].as_str(), Some("nat-box"));
    assert_eq!(entry["url"].as_str(), Some(""));
    assert_eq!(entry["peer"].as_str(), Some(node_peer_id.as_str()));
    assert_eq!(entry["reachable"].as_bool(), Some(true), "{entry}");

    let (st, status) = app_req(&owner_app, "GET", &format!("/api/computers/{}", out.computer_id), None).await;
    assert_eq!(st, StatusCode::OK, "{status}");
    assert_eq!(status["reachable"].as_bool(), Some(true), "{status}");
    assert!(status["env"]["hostname"].as_str().is_some(), "{status}");

    node_srv.abort();
}

/// FR-C2: a revoked computer's old credential is refused, and a fresh code with `--replace`
/// re-enrols the transport-only node with a new token.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_c2_a_revoked_computer_needs_a_new_code() {
    use ember_node::api::Node;
    use ember_node::config::NodeConfig;
    use ember_server::agents::scripted::ScriptedAdapter;
    use ember_server::agents::AgentAdapter;
    use ember_server::computers::enrol::{ComputersRegistrar, DoorUrl};
    use ember_server::computers::{self, Computers, Registry};
    use ember_server::session::Sessions;
    use ember_transport::mem::MemNetwork;
    use ember_transport::{Dialer, PeerAddr, PeerGate, SecretKey};

    let net = MemNetwork::new();
    let server_t = net.transport();
    let server_id = server_t.peer_id().to_string();
    let dialer = Dialer::new(server_t.clone());

    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let computers = Computers::with_transport(
        Registry::new(store.clone()),
        computers::connector(Some(dialer.clone())),
        None,
        Some(dialer.clone()),
    );
    let registrar = Arc::new(ComputersRegistrar::new(computers.clone()).with_server_peer_id(Some(server_id.clone())));
    let enrol = Enrol::with_server_peer_id(store.clone(), Some(server_id.clone()));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let door_base = format!("http://127.0.0.1:{port}");
    let vars: HashMap<String, String> = [(ENROL_LISTEN_VAR.to_string(), format!("127.0.0.1:{port}"))].into_iter().collect();
    let door = EnrolDoor::from_lookup(|k| vars.get(k).cloned(), "127.0.0.1:1".parse().unwrap(), None)
        .unwrap()
        .expect("door on");
    let (_stop, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(EnrolDoor::serve(listener, door.app(enrol.clone()), async move {
        let _ = rx.await;
    }));

    let owner_app = computers::api::router(computers.clone(), sessions.clone()).merge(computers::enrol::router(
        enrol.clone(),
        DoorUrl::Url(door_base.clone()),
        registrar.clone(),
    ));

    let node_dir = tempfile::tempdir().unwrap();
    let node_key = SecretKey::load_or_generate(node_dir.path().join(ember_node::transport::KEY_FILE)).unwrap();
    let node_fp = fp(&node_key.peer_id().to_string());

    // First enrolment.
    let (_, m1) = app_req(
        &owner_app,
        "POST",
        "/api/computers/enrol-codes",
        Some(serde_json::json!({ "name": "nat-box" })),
    )
    .await;
    let code1 = m1["code"].as_str().unwrap().to_string();
    let eid1 = m1["id"].as_str().unwrap().to_string();
    let owner1 = owner_app.clone();
    let enrol1 = enrol.clone();
    let fp1 = node_fp.clone();
    let c1 = tokio::spawn(async move {
        wait_redeemed(&enrol1, &eid1).await;
        let (st, _) = app_req(
            &owner1,
            "POST",
            &format!("/api/computers/enrol-codes/{eid1}/confirm"),
            Some(serde_json::json!({ "expect_fingerprint": fp1 })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    });
    let mut o1 = Options::new(code1.clone(), door_base.clone(), node_dir.path().to_path_buf());
    o1.poll_every = POLL;
    o1.yes = true;
    o1.transport = true;
    o1.pin = Some(server_id.clone());
    let out1 = run(o1, &mut Script::new(false, &[])).await.unwrap();
    c1.await.unwrap();
    let old_token = ember_node::config::token_from(None, Some(node_dir.path())).unwrap();

    // Revoke the computer on the server (`DELETE /api/computers/{id}`).
    let (st, _) = app_req(&owner_app, "DELETE", &format!("/api/computers/{}", out1.computer_id), None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = app_req(&owner_app, "GET", &format!("/api/computers/{}", out1.computer_id), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // Reusing the old code is refused.
    let mut reuse = Options::new(code1, door_base.clone(), node_dir.path().to_path_buf());
    reuse.poll_every = POLL;
    reuse.yes = true;
    reuse.replace = true;
    reuse.transport = true;
    reuse.pin = Some(server_id.clone());
    assert!(run(reuse, &mut Script::new(false, &[])).await.is_err());

    // Mint a new code and re-enrol with `--replace`.
    let (_, m2) = app_req(
        &owner_app,
        "POST",
        "/api/computers/enrol-codes",
        Some(serde_json::json!({ "name": "nat-box" })),
    )
    .await;
    let code2 = m2["code"].as_str().unwrap().to_string();
    let eid2 = m2["id"].as_str().unwrap().to_string();
    let owner2 = owner_app.clone();
    let enrol2 = enrol.clone();
    let fp2 = node_fp.clone();
    let c2 = tokio::spawn(async move {
        wait_redeemed(&enrol2, &eid2).await;
        let (st, _) = app_req(
            &owner2,
            "POST",
            &format!("/api/computers/enrol-codes/{eid2}/confirm"),
            Some(serde_json::json!({ "expect_fingerprint": fp2 })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    });
    let mut o2 = Options::new(code2, door_base, node_dir.path().to_path_buf());
    o2.poll_every = POLL;
    o2.yes = true;
    o2.replace = true;
    o2.transport = true;
    o2.pin = Some(server_id);
    let out2 = run(o2, &mut Script::new(false, &[])).await.unwrap();
    c2.await.unwrap();

    let new_token = ember_node::config::token_from(None, Some(node_dir.path())).unwrap();
    assert_ne!(old_token, new_token, "re-enrolment must mint a fresh token");
    assert_ne!(out1.computer_id, out2.computer_id);

    // Start the node with the new token; the old token is rejected (401) and the new one works.
    let node_t = net.transport_with_key(node_key.clone());
    let allowed = ember_node::transport::allowed_peers(Some(node_dir.path())).unwrap();
    let node = Node::new(NodeConfig::new(new_token, vec![node_dir.path().to_path_buf()])).unwrap();
    let node_srv = tokio::spawn(ember_node::transport::serve(&node_t, node, PeerGate::allow_list(allowed)).unwrap());

    let stale_client = ember_node::client::NodeClient::over_transport(
        dialer.clone(),
        PeerAddr::new(node_key.peer_id()),
        &old_token,
    );
    assert!(stale_client.env().await.is_err(), "old token must be refused by the re-enrolled node");

    let (st, status) = app_req(&owner_app, "GET", &format!("/api/computers/{}", out2.computer_id), None).await;
    assert_eq!(st, StatusCode::OK, "{status}");
    assert_eq!(status["reachable"].as_bool(), Some(true), "{status}");

    node_srv.abort();
}
