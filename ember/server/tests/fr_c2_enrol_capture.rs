//! FR-C2g: the secrets are in no tracing output of confirm and deny.
//!
//! This test lives in its own file on purpose, and the helpers below are copied from
//! `fr_c2_enrol_confirm.rs` on purpose (issue #549). Every integration test file is its own
//! process. The tracing callsite interest cache is process-wide, so in the shared binary the
//! other tests called the same handlers with no subscriber and could cache the `enrol confirmed`
//! callsite as "never", and this test's thread-local subscriber then missed the line on CI. Here
//! exactly one test runs in the process, and the capture subscriber is installed as the global
//! default before the first event, so the interest is computed with the subscriber present.
//! Keep this file to ONE test.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use ember_server::auth::{self, ServerAuth};
use ember_server::computers::enrol::{Enrol, NodeInfo, RegisterError, Registrar};
use ember_server::store::{now_ms, Store};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const LISTEN: &str = "127.0.0.1:8740";
const BASE: &str = "http://192.168.0.5:8740";
const SRC: &str = "192.168.0.9:50123";

/// A registry that lives in a Vec.
#[derive(Default)]
struct Fake {
    registered: Mutex<Vec<(String, String)>>,
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

struct Fx {
    app: Router,
    enrol: Arc<Enrol>,
    token: String,
    _dir: tempfile::TempDir,
}

fn fx_with(registrar: impl FnOnce(&Arc<Store>) -> Arc<dyn Registrar>) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let enrol = Enrol::new(store.clone());
    let token = auth::load_or_create_token(dir.path()).unwrap();
    let a = ServerAuth::new(token.clone(), LISTEN.parse::<SocketAddr>().unwrap(), []);
    let reg = registrar(&store);
    let app = auth::protect(ember_server::computers::enrol::router(enrol.clone(), ember_server::computers::enrol::DoorUrl::Url(BASE.into()), reg), a);
    Fx { app, enrol, token, _dir: dir }
}

fn fx(fake: Arc<Fake>) -> Fx {
    fx_with(|_| fake)
}

async fn call(app: &Router, method: Method, path: &str, auth: Option<&str>, body: Option<Value>) -> (StatusCode, Value, String) {
    let mut b = Request::builder().method(method).uri(path).header("host", LISTEN);
    if let Some(a) = auth {
        b = b.header("authorization", a);
    }
    let body = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let r = app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
    let st = r.status();
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null), text)
}

fn info() -> NodeInfo {
    NodeInfo {
        hostname: "ibrew-laptop".into(),
        os: "macos".into(),
        arch: "aarch64".into(),
        node_version: "0.1.0".into(),
        url: Some("http://192.168.0.9:8741".into()),
        peer_id: None,
        // FR-C2d: a redeem needs an enrol key. These tests never open the credential.
        enrol_key: Some(ember_node::enrol_seal::NodeKey::generate().public_hex()),
    }
}

struct Opened {
    id: String,
    code: String,
    poll_secret: String,
}

/// Mint through the route and redeem through the core (the door does not exist yet).
async fn open(f: &Fx, name: &str) -> Opened {
    let h = format!("Bearer {}", f.token);
    let (st, body, _) = call(&f.app, Method::POST, "/api/computers/enrol-codes", Some(&h), Some(json!({ "name": name }))).await;
    assert_eq!(st, StatusCode::CREATED);
    let id = body["id"].as_str().unwrap().to_string();
    let code = body["code"].as_str().unwrap().to_string();
    let r = f.enrol.redeem(&code, &info(), now_ms(), SRC).unwrap();
    assert_eq!(r.enrol_id, id);
    Opened { id, code, poll_secret: r.poll_secret.expose().to_string() }
}

fn path(id: &str, tail: &str) -> String {
    format!("/api/computers/enrol-codes/{id}{tail}")
}

/// A tracing writer that collects everything written.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Capture {
        self.clone()
    }
}

#[tokio::test]
async fn fr_c2_the_secrets_are_in_no_tracing_output_of_confirm_and_deny() {
    let cap = Capture::default();
    let sub = tracing_subscriber::fmt()
        .with_writer(cap.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    // Global default, once, before any event: the callsite interest is computed with it present.
    tracing::subscriber::set_global_default(sub).expect("no other subscriber in this process");

    let fake = Arc::new(Fake::default());
    let f = fx(fake.clone());
    let h = format!("Bearer {}", f.token);
    let confirmed = open(&f, "one").await;
    let denied = open(&f, "two").await;
    let (cst, cbody, _) = call(&f.app, Method::POST, &path(&confirmed.id, "/confirm"), Some(&h), Some(json!({}))).await;
    let (dst, dbody, _) = call(&f.app, Method::POST, &path(&denied.id, "/deny"), Some(&h), None).await;
    // 1. Both calls succeeded, so a missing log line below is a logging problem, not a request one.
    assert_eq!(cst, StatusCode::OK, "confirm answered {cst}: {cbody}");
    assert_eq!(dst, StatusCode::OK, "deny answered {dst}: {dbody}");
    let d = f.enrol.take_delivery(&confirmed.id, &confirmed.poll_secret, now_ms());
    tracing::info!(delivery = ?d, "delivery");
    let token = fake.registered.lock().unwrap()[0].1.clone();

    let out = String::from_utf8(cap.0.lock().unwrap().clone()).unwrap();
    let thread = std::thread::current();
    let context = format!(
        "thread={:?}, runtime=current_thread, captured output:\n{out}",
        thread.name()
    );

    // 2. Secret absence, independent of the line checks: all failures are collected.
    let mut failures: Vec<String> = Vec::new();
    let secrets = [
        ("confirmed code", &confirmed.code),
        ("confirmed poll secret", &confirmed.poll_secret),
        ("denied code", &denied.code),
        ("denied poll secret", &denied.poll_secret),
        ("node token", &token),
        ("server token", &f.token),
    ];
    for (what, secret) in secrets {
        let body = secret.split_once('_').map_or(secret.as_str(), |(_, b)| b);
        if out.contains(body) {
            failures.push(format!("the {what} reached the log"));
        }
    }
    assert!(failures.is_empty(), "{failures:?}; {context}");

    // 3. Each expected line, checked separately.
    for line in ["enrol code minted", "enrol confirmed", "enrol denied"] {
        assert!(out.contains(line), "the line {line:?} is missing; {context}");
    }
}
