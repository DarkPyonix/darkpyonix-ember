//! Accounts end to end: isolation env per session (FR-U1), routing with reasons (FR-U2/U3),
//! usage API, and API keys that never leave the server (FR-U5).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use ember_server::accounts::secrets::SecretBox;
use ember_server::accounts::Accounts;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind, AgentRun, Detected, StartRequest};
use ember_server::events::{AgentEvent, SessionStatus};
use ember_server::session::{NewSession, Sessions};
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tower::ServiceExt;

type EnvLog = Arc<Mutex<Vec<Vec<(String, String)>>>>;

/// The scripted agent, recording the environment each start was given.
#[derive(Default)]
struct EnvRecorder {
    starts: EnvLog,
}

#[async_trait]
impl AgentAdapter for EnvRecorder {
    fn kind(&self) -> AgentKind {
        AgentKind::Scripted
    }
    async fn detect(&self) -> Detected {
        ScriptedAdapter.detect().await
    }
    async fn start(
        &self,
        req: StartRequest,
        events: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<Box<dyn AgentRun>> {
        self.starts.lock().unwrap().push(req.env.clone());
        ScriptedAdapter.start(req, events).await
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    sessions: Arc<Sessions>,
    accounts: Arc<Accounts>,
    starts: EnvLog,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let rec = EnvRecorder::default();
    let starts = rec.starts.clone();
    let sessions = Sessions::new(store.clone(), vec![Arc::new(rec) as Arc<dyn AgentAdapter>]);
    let accounts =
        Accounts::with_secrets(store, &dir.path().join("accounts"), SecretBox::ephemeral())
            .unwrap();
    accounts.install(&sessions);
    Fixture {
        _dir: dir,
        sessions,
        accounts,
        starts,
    }
}

fn new_session() -> NewSession {
    NewSession {
        project: "acme".into(),
        agent: AgentKind::Scripted,
        cwd: std::env::temp_dir(),
        model: None,
        title: "t".into(),
    }
}

async fn wait_status(s: &Sessions, id: &str, want: SessionStatus) {
    for _ in 0..200 {
        if s.store().session(id).unwrap().unwrap().status == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("session {id} never reached {want:?}");
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, String) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    let req = req
        .body(
            body.map(|b| Body::from(b.to_string()))
                .unwrap_or_else(Body::empty),
        )
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

fn app(f: &Fixture) -> axum::Router {
    ember_server::api::router(f.sessions.clone())
        .merge(ember_server::accounts::api::router(f.accounts.clone()))
}

#[tokio::test]
async fn two_accounts_run_two_sessions_with_isolated_config_dirs() {
    let f = fixture();
    let a = f.accounts.create(AgentKind::Scripted, "a").unwrap();
    let b = f.accounts.create(AgentKind::Scripted, "b").unwrap();
    let sa = f
        .sessions
        .create_with_account(new_session(), Some(&a.id))
        .unwrap();
    let sb = f
        .sessions
        .create_with_account(new_session(), Some(&b.id))
        .unwrap();
    assert_eq!(sa.account_id.as_deref(), Some(a.id.as_str()));
    assert!(sa
        .account_reason
        .as_deref()
        .unwrap()
        .starts_with("chosen by the user"));

    f.sessions.send(&sa.id, "one").await.unwrap();
    f.sessions.send(&sb.id, "two").await.unwrap();
    wait_status(&f.sessions, &sa.id, SessionStatus::WaitingForApproval).await;
    wait_status(&f.sessions, &sb.id, SessionStatus::WaitingForApproval).await;

    let starts = f.starts.lock().unwrap().clone();
    assert_eq!(starts.len(), 2);
    let dirs: Vec<&str> = starts
        .iter()
        .map(|env| {
            let found: Vec<_> = env
                .iter()
                .filter(|(k, _)| k == "EMBER_SCRIPTED_HOME")
                .collect();
            assert_eq!(found.len(), 1, "{env:?}");
            found[0].1.as_str()
        })
        .collect();
    assert!(dirs.contains(&a.config_dir.as_str()) && dirs.contains(&b.config_dir.as_str()));
    assert_ne!(dirs[0], dirs[1]);

    // A session without an account (no accounts for its agent) gets no account env at all.
    let plain = Sessions::new(
        Arc::new(Store::open_in_memory().unwrap()),
        vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>],
    );
    let rec = plain.create(new_session()).unwrap();
    assert_eq!(rec.account_id, None);
}

#[tokio::test]
async fn router_skips_a_limited_account_and_the_session_shows_why() {
    let f = fixture();
    let app = app(&f);
    let (st, body) = call(
        &app,
        "POST",
        "/api/accounts",
        Some(json!({"agent": "scripted", "label": "a"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
    let a: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(a["login"]["env"]["EMBER_SCRIPTED_HOME"], a["config_dir"]);
    let (_, body) = call(
        &app,
        "POST",
        "/api/accounts",
        Some(json!({"agent": "scripted", "label": "b"})),
    )
    .await;
    let b: Value = serde_json::from_str(&body).unwrap();
    let (a_id, b_id) = (a["id"].as_str().unwrap(), b["id"].as_str().unwrap());

    // a is the default but rate-limited: the router takes b and says why.
    call(
        &app,
        "POST",
        &format!("/api/accounts/{a_id}/default"),
        None,
    )
    .await;
    f.accounts
        .set_limit(
            a_id,
            ember_server::store::now_ms() + 3_600_000,
            "Claude five_hour limit reached",
        )
        .unwrap();
    let new = json!({"project": "acme", "agent": "scripted", "cwd": "/tmp"});
    let (st, body) = call(&app, "POST", "/api/sessions", Some(new.clone())).await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
    let s: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(s["account_id"], b_id);
    let reason = s["account_reason"].as_str().unwrap();
    assert!(reason.contains("skipped a (rate-limited until"), "{reason}");
    assert!(reason.contains("five_hour"), "{reason}");

    // The account stays with the session (FR-U2).
    let (_, body) = call(
        &app,
        "GET",
        &format!("/api/sessions/{}", s["id"].as_str().unwrap()),
        None,
    )
    .await;
    let got: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(got["session"]["account_id"], b_id);

    // Preview and list show the limit.
    let (_, body) = call(&app, "GET", "/api/accounts/route?agent=scripted", None).await;
    assert!(body.contains(b_id), "{body}");
    let (_, body) = call(&app, "GET", "/api/accounts", None).await;
    let list: Value = serde_json::from_str(&body).unwrap();
    let a_view = list
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == a_id)
        .unwrap();
    assert_eq!(a_view["limited"], true);

    // Both limited: creation fails with the reason rather than silently using another login.
    f.accounts
        .set_limit(b_id, ember_server::store::now_ms() + 60_000, "quota")
        .unwrap();
    let (st, body) = call(&app, "POST", "/api/sessions", Some(new.clone())).await;
    assert_eq!(st, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("no scripted account is available"), "{body}");

    // An unknown or deleted-while-used account is refused.
    let mut bad = new.clone();
    bad["account"] = json!("nope");
    assert_eq!(
        call(&app, "POST", "/api/sessions", Some(bad)).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(&app, "DELETE", &format!("/api/accounts/{b_id}"), None)
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn usage_is_reported_per_account() {
    let f = fixture();
    let app = app(&f);
    let a = f.accounts.create(AgentKind::Scripted, "a").unwrap();
    let s = f
        .sessions
        .create_with_account(new_session(), Some(&a.id))
        .unwrap();
    f.sessions.send(&s.id, "hi").await.unwrap();
    wait_status(&f.sessions, &s.id, SessionStatus::WaitingForApproval).await;
    f.sessions
        .answer(
            &s.id,
            "approval-1",
            ember_server::events::ApprovalDecision::AllowOnce,
        )
        .await
        .unwrap();
    wait_status(&f.sessions, &s.id, SessionStatus::Finished).await;

    let (st, body) = call(&app, "GET", "/api/usage?days=1", None).await;
    assert_eq!(st, StatusCode::OK);
    let rows: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1, "{body}");
    assert_eq!(rows[0]["account_id"], a.id.as_str());
    assert_eq!(rows[0]["input_tokens"], 10);
    assert_eq!(rows[0]["output_tokens"], 5);
    let (_, body) = call(&app, "GET", &format!("/api/accounts/{}", a.id), None).await;
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["tokens_today"],
        15
    );
}

#[tokio::test]
async fn api_keys_never_come_back_out() {
    let f = fixture();
    let app = app(&f);
    const KEY: &str = "sk-proj-THIS-MUST-NOT-LEAK-0123456789";
    let (st, body) = call(
        &app,
        "POST",
        "/api/providers",
        Some(json!({"label": "work", "kind": "openai", "api_key": KEY})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    assert!(!body.contains("MUST-NOT-LEAK"), "{body}");
    let id = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    for uri in [
        "/api/providers",
        "/api/accounts",
        "/api/sessions",
        "/api/usage",
    ] {
        let (_, body) = call(&app, "GET", uri, None).await;
        assert!(!body.contains("MUST-NOT-LEAK"), "{uri}: {body}");
    }
    // Still usable server-side.
    let key = ember_server::accounts::secrets::provider_key(
        f.accounts.store(),
        f.accounts.secrets(),
        &id,
    )
    .unwrap()
    .unwrap();
    assert_eq!(key.expose(), KEY);
    assert!(!format!("{key:?}").contains("MUST-NOT-LEAK"));
    assert_eq!(
        call(&app, "DELETE", &format!("/api/providers/{id}"), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
}
