//! FR-A10..FR-A10f: the Gemini CLI as the `gemini` ACP preset, against a fake `gemini` shell
//! script (`tests/fixtures/acp/fake-gemini.sh`). The real Gemini CLI is not installed on the
//! machine that wrote these tests, so none of this proves the real CLI's behaviour: that is
//! FR-A10g (`tests/acp_gemini.rs`), UNVERIFIED until run.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use ember_server::agents::acp::{AcpAdapter, AcpConfig};
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::computers::{default_connector, Computers, Registry};
use ember_server::events::{AgentEvent, SessionStatus, TurnOutcome};
use ember_server::session::{NewSession, Sessions};
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

const LISTEN: &str = "127.0.0.1:8740";
const AUTH_TEXT: &str =
    "sign in needed: set GEMINI_API_KEY (individual Google accounts are no longer served by the Gemini CLI, use Antigravity)";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/acp/fake-gemini.sh")
}

/// A `gemini` preset pointing at `command`, with `env` for the fake.
fn adapter(command: &Path, env: &[(&str, &str)]) -> Arc<dyn AgentAdapter> {
    let mut config = AcpConfig::preset("gemini", Some(command.to_path_buf())).unwrap();
    config.env = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    Arc::new(AcpAdapter::new(config).unwrap())
}


fn app(adapters: Vec<Arc<dyn AgentAdapter>>) -> (Router, Arc<Sessions>) {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), adapters);
    let computers = Computers::new(Registry::new(store), default_connector());
    let app = ember_server::api::router(sessions.clone())
        .merge(ember_server::api::harnesses::router(sessions.clone(), computers));
    (app, sessions)
}

async fn call(app: &Router, method: Method, path: &str) -> (StatusCode, Value) {
    let r = Request::builder().method(method).uri(path).header("host", LISTEN).body(Body::empty()).unwrap();
    let r = app.clone().oneshot(r).await.unwrap();
    let status = r.status();
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// Write an executable script: flush to disk and close the handle before chmod and use, because a
/// script still open for writing is "text file busy" (ETXTBSY) when executed (#546).
fn write_exec(path: &Path, content: &str) {
    {
        use std::io::Write;
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.sync_all().unwrap();
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn script(path: &Path, body: &str) {
    write_exec(path, &format!("#!/bin/sh\n{body}\n"));
}

// ---- FR-A10a: detection ------------------------------------------------------------------

#[tokio::test]
async fn fr_a10a_detect_reports_the_fake_gemini_version_and_path() {
    let dir = tempfile::tempdir().unwrap();
    let gemini = dir.path().join("gemini");
    script(&gemini, "echo 0.99.1");
    let (app, _) = app(vec![adapter(&gemini, &[])]);
    let (st, v) = call(&app, Method::POST, "/api/computers/local/harnesses/detect").await;
    assert_eq!(st, StatusCode::OK);
    let r = v.as_array().unwrap().iter().find(|r| r["harness"] == "gemini").expect("gemini in the answer");
    assert_eq!(r["installed"], true);
    assert_eq!(r["version"], "0.99.1");
    assert_eq!(r["path"], gemini.to_str().unwrap());
    assert!(r["detected_at"].is_i64());
    let (_, v) = call(&app, Method::GET, "/api/agents").await;
    let r = v.as_array().unwrap().iter().find(|r| r["kind"] == "gemini").expect("gemini in /api/agents");
    assert_eq!((r["installed"].clone(), r["version"].clone()), (true.into(), "0.99.1".into()));
}

#[tokio::test]
async fn fr_a10a_an_absent_gemini_is_not_installed_with_no_version() {
    let dir = tempfile::tempdir().unwrap();
    let (app, _) = app(vec![adapter(&dir.path().join("no-such-gemini"), &[])]);
    let (_, v) = call(&app, Method::POST, "/api/computers/local/harnesses/detect").await;
    let r = v.as_array().unwrap().iter().find(|r| r["harness"] == "gemini").unwrap();
    assert_eq!(r["installed"], false);
    assert!(r["version"].is_null() && r["path"].is_null(), "{r}");
    assert!(!v.to_string().contains("1.0.0"));
}

#[tokio::test]
async fn fr_a10a_garbage_version_output_is_installed_with_a_null_version() {
    let dir = tempfile::tempdir().unwrap();
    let gemini = dir.path().join("gemini");
    script(&gemini, "echo 'garbage in, garbage out'");
    let (app, _) = app(vec![adapter(&gemini, &[])]);
    let (_, v) = call(&app, Method::POST, "/api/computers/local/harnesses/detect").await;
    let r = v.as_array().unwrap().iter().find(|r| r["harness"] == "gemini").unwrap();
    assert_eq!(r["installed"], true);
    assert!(r["version"].is_null(), "{r}");
}

// ---- turns through the session layer -----------------------------------------------------

struct Fx {
    app: Router,
    sessions: Arc<Sessions>,
    id: String,
    _cwd: tempfile::TempDir,
}

fn session(env: &[(&str, &str)]) -> Fx {
    let a = adapter(&fixture(), env);
    let kind = a.kind();
    let (app, sessions) = app(vec![a]);
    let cwd = tempfile::tempdir().unwrap();
    let rec = sessions
        .create(NewSession {
            project: "p".into(),
            agent: kind,
            cwd: cwd.path().to_path_buf(),
            model: None,
            title: "t".into(),
        })
        .unwrap();
    Fx { app, sessions, id: rec.id, _cwd: cwd }
}

/// Send `text` and wait until the turn ended; the events stored so far.
async fn turn(f: &Fx, text: &str) -> Vec<AgentEvent> {
    let before = f.sessions.store().events_after(&f.id, 0).unwrap().iter().filter(|e| matches!(e.event, AgentEvent::TurnEnded { .. })).count();
    f.sessions.send(&f.id, text).await.unwrap();
    for _ in 0..1000 {
        let events: Vec<AgentEvent> = f.sessions.store().events_after(&f.id, 0).unwrap().into_iter().map(|e| e.event).collect();
        if events.iter().filter(|e| matches!(e, AgentEvent::TurnEnded { .. })).count() > before {
            return events;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the turn never ended");
}

fn typed(events: &[AgentEvent], code: &str) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TurnError { code: c, message } if c == code => Some(message.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn fr_a10_a_turn_runs_through_the_preset_and_fr_a10d_reports_no_usage() {
    let f = session(&[]);
    let events = turn(&f, "hi").await;
    assert!(events.iter().any(|e| matches!(e, AgentEvent::AssistantMessage { text } if text == "ok")));
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }));
    assert!(!events.iter().any(|e| matches!(e, AgentEvent::Usage { .. })));
    let (st, v) = call(&f.app, Method::GET, &format!("/api/sessions/{}/usage", f.id)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["reported"], false, "not measured, not zero: {v}");
    assert_eq!(v["total_tokens"], 0);
    assert_eq!(v["turns"], 0);
    assert!(v["cost_usd"].is_null());
    assert_eq!(f.sessions.store().session(&f.id).unwrap().unwrap().status, SessionStatus::Finished);
}

#[tokio::test]
async fn fr_a10_a_cli_that_rejects_acp_is_retried_once_with_experimental_acp() {
    let f = session(&[("FAKE_GEMINI_MODE", "old")]);
    let events = turn(&f, "hi").await;
    assert!(events.iter().any(|e| matches!(e, AgentEvent::AssistantMessage { text } if text == "ok")), "{events:?}");
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }));
}

#[tokio::test]
async fn fr_a10b_a_cli_that_needs_authentication_ends_each_turn_with_auth_required() {
    let f = session(&[("FAKE_GEMINI_MODE", "auth")]);
    let events = turn(&f, "hi").await;
    assert_eq!(typed(&events, "auth_required"), [AUTH_TEXT]);
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Failed }));
    assert_eq!(f.sessions.store().session(&f.id).unwrap().unwrap().status, SessionStatus::Failed);
    let events = turn(&f, "again").await;
    assert_eq!(typed(&events, "auth_required").len(), 2, "the second turn fails the same way, no crash");
}

#[tokio::test]
async fn fr_a10c_a_quota_error_is_typed_and_the_next_turn_runs() {
    let f = session(&[("FAKE_GEMINI_MODE", "quota")]);
    let events = turn(&f, "hi").await;
    let quota = typed(&events, "quota_exceeded");
    assert_eq!(quota.len(), 1);
    assert!(quota[0].contains("RESOURCE_EXHAUSTED"), "{quota:?}");
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Failed }));
    let events = turn(&f, "again").await;
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::AssistantMessage { text } if text == "ok")));
}

// ---- upstream text never reaches the user, the store or the logs unredacted ----------------

const FAKE_KEY: &str = "AIzaSyD-FAKEFAKEFAKEFAKEFAKEFAKEFAKE123";
const FAKE_EMAIL: &str = "dev.person@example.com";
const FAKE_BEARER: &str = "FAKEbearerTOKEN9876543210";

/// A `tracing` writer that keeps what the default-level subscriber printed.
#[derive(Clone, Default)]
struct Capture(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
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

/// Install a thread-local subscriber at the default level (INFO); `#[tokio::test]` runs the
/// whole test on this thread.
fn capture_logs() -> (Capture, tracing::subscriber::DefaultGuard) {
    let cap = Capture::default();
    let sub = tracing_subscriber::fmt()
        .with_writer(cap.clone())
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .finish();
    (cap.clone(), tracing::subscriber::set_default(sub))
}

fn logged(cap: &Capture) -> String {
    String::from_utf8(cap.0.lock().unwrap().clone()).unwrap()
}

#[tokio::test]
async fn fr_a10_redact_a_quota_error_with_a_key_and_an_email_reaches_neither_the_event_nor_the_logs() {
    assert_eq!(FAKE_KEY.len(), 39);
    let (cap, _guard) = capture_logs();
    let f = session(&[("FAKE_GEMINI_MODE", "quota-secret")]);
    let events = turn(&f, "hi").await;
    let quota = typed(&events, "quota_exceeded");
    assert_eq!(quota.len(), 1, "{events:?}");
    assert!(quota[0].contains("RESOURCE_EXHAUSTED") && quota[0].contains("[redacted]"), "{quota:?}");
    let stored = serde_json::to_string(&f.sessions.store().events_after(&f.id, 0).unwrap().into_iter().map(|e| e.event).collect::<Vec<_>>()).unwrap();
    let logs = logged(&cap);
    for text in [&quota[0], &stored, &logs] {
        for secret in [FAKE_KEY, FAKE_EMAIL, FAKE_BEARER, "AIzaSy"] {
            assert!(!text.contains(secret), "{secret} leaked into {text}");
        }
    }
}

#[tokio::test]
async fn fr_a10_redact_an_auth_error_on_session_new_is_warned_redacted_and_the_turn_text_is_fixed() {
    let (cap, _guard) = capture_logs();
    let f = session(&[("FAKE_GEMINI_MODE", "auth-secret")]);
    let events = turn(&f, "hi").await;
    assert_eq!(typed(&events, "auth_required"), [AUTH_TEXT]);
    let stored = serde_json::to_string(&f.sessions.store().events_after(&f.id, 0).unwrap().into_iter().map(|e| e.event).collect::<Vec<_>>()).unwrap();
    let logs = logged(&cap);
    assert!(logs.contains("needs authentication"), "the warning is emitted: {logs}");
    for text in [&stored, &logs] {
        for secret in [FAKE_KEY, FAKE_EMAIL, FAKE_BEARER] {
            assert!(!text.contains(secret), "{secret} leaked into {text}");
        }
    }
}

// ---- the gemini child gets an allow-listed environment -------------------------------------

#[tokio::test]
async fn fr_a10_env_the_gemini_child_does_not_inherit_the_server_environment() {
    // cargo sets CARGO_MANIFEST_DIR for every test: it stands in for a server secret.
    assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some());
    let dir = tempfile::tempdir().unwrap();
    let dump = dir.path().join("env.txt");
    let f = session(&[("FAKE_GEMINI_ENV_DUMP", dump.to_str().unwrap())]);
    let events = turn(&f, "hi").await;
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }));
    let env = std::fs::read_to_string(&dump).unwrap();
    let names: Vec<&str> = env.lines().filter_map(|l| l.split_once('=').map(|(k, _)| k)).collect();
    assert!(!names.contains(&"CARGO_MANIFEST_DIR"), "a server variable reached the child: {names:?}");
    assert!(!names.iter().any(|n| n.starts_with("CARGO_")), "{names:?}");
    assert!(names.contains(&"PATH") && names.contains(&"HOME"), "{names:?}");
    assert!(names.contains(&"FAKE_GEMINI_ENV_DUMP"), "the config's own env is kept: {names:?}");
}
