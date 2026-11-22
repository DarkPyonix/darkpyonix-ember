//! FR-U1 for ACP agents: an account of an ACP agent keeps its credentials in its own directory,
//! through the agent's own variable, and an ACP agent with no such variable gets no accounts.
//!
//! The agent is the fake `gemini` script of FR-A10 (`tests/fixtures/acp/fake-gemini.sh`), which
//! dumps the environment it was started with. That the real Gemini CLI then keeps its login under
//! `GEMINI_CLI_HOME` is read from its source (see `accounts/mod.rs`), not run here.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ember_server::accounts::secrets::SecretBox;
use ember_server::accounts::Accounts;
use ember_server::agents::acp::{AcpAdapter, AcpConfig};
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::events::{AgentEvent, TurnOutcome};
use ember_server::session::{NewSession, Sessions};
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/acp/fake-gemini.sh")
}

struct Fx {
    _dir: tempfile::TempDir,
    dump: PathBuf,
    kind: AgentKind,
    sessions: Arc<Sessions>,
    accounts: Arc<Accounts>,
}

/// The `gemini` preset on the fake script, plus a configured ACP agent `acp-noiso` that has no
/// known isolation variable, with accounts installed on the sessions.
fn setup() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let dump = dir.path().join("env.txt");
    let mut gemini = AcpConfig::preset("gemini", Some(fixture())).unwrap();
    gemini.env = vec![("FAKE_GEMINI_ENV_DUMP".into(), dump.to_string_lossy().into_owned())];
    let gemini: Arc<dyn AgentAdapter> = Arc::new(AcpAdapter::new(gemini).unwrap());
    let mut other = AcpConfig::preset("gemini", Some(fixture())).unwrap();
    other.name = "acp-noiso".into();
    let other: Arc<dyn AgentAdapter> = Arc::new(AcpAdapter::new(other).unwrap());
    let kind = gemini.kind();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), vec![gemini, other]);
    let accounts =
        Accounts::with_secrets(store, &dir.path().join("accounts"), SecretBox::ephemeral()).unwrap();
    accounts.install(&sessions);
    Fx { _dir: dir, dump, kind, sessions, accounts }
}

fn new_session(kind: AgentKind, cwd: &Path) -> NewSession {
    NewSession { project: "p".into(), agent: kind, cwd: cwd.to_path_buf(), model: None, title: "t".into() }
}

/// Run one turn on session `id` and return the environment the agent process was started with.
async fn env_of_turn(f: &Fx, id: &str) -> Vec<(String, String)> {
    let _ = std::fs::remove_file(&f.dump);
    f.sessions.send(id, "hi").await.unwrap();
    for _ in 0..1000 {
        let ended = f
            .sessions
            .store()
            .events_after(id, 0)
            .unwrap()
            .into_iter()
            .any(|e| e.event == AgentEvent::TurnEnded { outcome: TurnOutcome::Completed });
        if ended {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    std::fs::read_to_string(&f.dump)
        .unwrap()
        .lines()
        .filter_map(|l| l.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
        .collect()
}

fn value<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    env.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

#[tokio::test]
async fn fr_u1_a_gemini_account_process_gets_gemini_cli_home_and_two_accounts_differ() {
    let f = setup();
    let cwd = tempfile::tempdir().unwrap();
    let a = f.accounts.create(f.kind, "work").unwrap();
    let b = f.accounts.create(f.kind, "home").unwrap();
    let sa = f.sessions.create_with_account(new_session(f.kind, cwd.path()), Some(&a.id)).unwrap();
    let sb = f.sessions.create_with_account(new_session(f.kind, cwd.path()), Some(&b.id)).unwrap();

    let ea = env_of_turn(&f, &sa.id).await;
    let eb = env_of_turn(&f, &sb.id).await;
    assert_eq!(value(&ea, "GEMINI_CLI_HOME"), Some(a.config_dir.as_str()), "{ea:?}");
    assert_eq!(value(&eb, "GEMINI_CLI_HOME"), Some(b.config_dir.as_str()), "{eb:?}");
    assert_ne!(a.config_dir, b.config_dir);
    // The stored API key and OAuth token go to a file under the account dir, not the OS keychain
    // that every account of the user would share.
    assert_eq!(value(&ea, "GEMINI_FORCE_FILE_STORAGE"), Some("true"));
    assert_eq!(value(&eb, "GEMINI_FORCE_FILE_STORAGE"), Some("true"));
    // The old, ignored name is gone.
    assert_eq!(value(&ea, "EMBER_ACP_HOME"), None);
}

#[tokio::test]
async fn fr_u1_a_gemini_session_without_an_account_is_unchanged() {
    let f = setup();
    let cwd = tempfile::tempdir().unwrap();
    let s = f.sessions.create(new_session(f.kind, cwd.path())).unwrap();
    assert_eq!(s.account_id, None);
    let env = env_of_turn(&f, &s.id).await;
    for name in ["GEMINI_CLI_HOME", "GEMINI_FORCE_FILE_STORAGE", "EMBER_ACP_HOME"] {
        assert_eq!(value(&env, name), None, "{name} set for the server's own login: {env:?}");
    }
}

#[tokio::test]
async fn fr_u1_the_api_refuses_an_account_for_an_acp_agent_without_isolation() {
    let f = setup();
    let app = ember_server::accounts::api::router(f.accounts.clone());
    let req = Request::builder()
        .method("POST")
        .uri("/api/accounts")
        .header("content-type", "application/json")
        .body(Body::from(json!({"agent": "acp-noiso", "label": "x"}).to_string()))
        .unwrap();
    let r = app.oneshot(req).await.unwrap();
    let status = r.status();
    let body = String::from_utf8(r.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert_ne!(status, StatusCode::CREATED, "{body}");
    assert!(body.contains("acp-noiso"), "the reason names the agent: {body}");
    assert!(body.contains("credentials"), "the reason says why: {body}");
    assert!(f.accounts.list(None).unwrap().is_empty());
}
