//! A2A terminal tools (FR-T9): read, send text and interrupt, gated by a grant.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::a2a::{A2a, A2aConfig, A2aStore};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind, AgentRun, Detected, StartRequest};
use ember_server::events::{AgentEvent, ApprovalDecision, SessionStatus, TurnOutcome};
use ember_server::session::{NewSession, Sessions};
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tower::ServiceExt;

#[derive(Clone, Default)]
struct Recording(Arc<Mutex<HashMap<String, StartRequest>>>);

#[async_trait]
impl AgentAdapter for Recording {
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
        let id = req.env.iter().find(|(k, _)| k == "EMBER_SESSION_ID").map(|(_, v)| v.clone());
        self.0.lock().unwrap().insert(id.unwrap_or_default(), req.clone());
        ScriptedAdapter.start(req, events).await
    }
}

impl Recording {
    fn token(&self, session: &str) -> String {
        let seen = self.0.lock().unwrap();
        let req = seen.get(session).unwrap_or_else(|| panic!("session {session} never started"));
        req.env.iter().find(|(k, _)| k == "EMBER_RUNTIME_TOKEN").map(|(_, v)| v.clone()).unwrap()
    }
}

struct World {
    sessions: Arc<Sessions>,
    agents: Recording,
    app: Router,
}

fn world() -> World {
    let agents = Recording::default();
    let sessions = Sessions::new(
        Arc::new(Store::open_in_memory().unwrap()),
        vec![Arc::new(agents.clone()) as Arc<dyn AgentAdapter>],
    );
    let a2a = A2a::new(sessions.clone(), A2aStore::open_in_memory().unwrap(), A2aConfig::new("http://127.0.0.1:1"));
    a2a.install();
    let app = ember_server::api::router(sessions.clone()).merge(ember_server::a2a::api::router(a2a));
    World { sessions, agents, app }
}

fn new_session(s: &Sessions, title: &str) -> String {
    s.create(NewSession {
        project: "acme".into(),
        agent: AgentKind::Scripted,
        cwd: std::env::temp_dir(),
        model: None,
        title: title.into(),
    })
    .unwrap()
    .id
}

async fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    for _ in 0..1000 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn wait_status(s: &Sessions, id: &str, want: SessionStatus) {
    wait_for(&format!("{id} to be {want:?}"), || s.store().session(id).unwrap().unwrap().status == want).await;
}

/// A started session with one finished turn (so it has a token).
async fn started(w: &World, title: &str) -> String {
    let id = new_session(&w.sessions, title);
    w.sessions.send(&id, "start").await.unwrap();
    wait_status(&w.sessions, &id, SessionStatus::WaitingForApproval).await;
    w.sessions.answer(&id, "approval-1", ApprovalDecision::AllowOnce).await.unwrap();
    wait_status(&w.sessions, &id, SessionStatus::Finished).await;
    id
}

async fn call(app: &Router, method: &str, uri: &str, token: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let req = match body {
        Some(b) => req.header("content-type", "application/json").body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
    (status, v)
}

async fn agent(w: &World, who: &str, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let token = w.agents.token(who);
    call(&w.app, method, uri, Some(&token), body).await
}

async fn grant(w: &World, reader: &str, target: &str, allowed: bool) {
    let (st, v) = call(
        &w.app,
        "PUT",
        &format!("/api/sessions/{reader}/terminal-grants"),
        None,
        Some(json!({ "target": target, "allowed": allowed })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
}

fn notices(s: &Sessions, id: &str) -> Vec<String> {
    s.store()
        .events_after(id, 0)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            AgentEvent::Notice { message } => Some(message),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn fr_t9_authorized_read_returns_recent_output() {
    let w = world();
    let reader = started(&w, "reader").await;
    let target = started(&w, "target").await;
    grant(&w, &reader, &target, true).await;
    let (st, v) = agent(&w, &reader, "GET", &format!("/api/a2a/terminal/{target}?lines=3"), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let lines = v["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 3, "{v}");
    assert!(lines.last().unwrap()["text"].as_str().unwrap().contains("turn ended"), "{v}");
    let all = agent(&w, &reader, "GET", &format!("/api/a2a/terminal/{target}"), None).await.1;
    assert!(all["lines"].to_string().contains("echo: start"), "{all}");
    // Follow on from the last seen line: nothing new yet, then the target's logged reads are not output.
    let last = all["last_seq"].as_i64().unwrap();
    let (_, more) = agent(&w, &reader, "GET", &format!("/api/a2a/terminal/{target}?since={last}"), None).await;
    assert!(more["lines"].as_array().unwrap().is_empty(), "{more}");
    // Each use is logged in the target's transcript.
    assert!(notices(&w.sessions, &target).iter().any(|n| n.contains(&reader) && n.contains("read")));
}

#[tokio::test]
async fn fr_t9_unauthorized_read_is_denied() {
    let w = world();
    let reader = started(&w, "reader").await;
    let target = started(&w, "target").await;
    let (st, v) = agent(&w, &reader, "GET", &format!("/api/a2a/terminal/{target}"), None).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["code"], "forbidden");
    let (st, _) = agent(&w, &reader, "POST", &format!("/api/a2a/terminal/{target}/text"), Some(json!({"text": "x"}))).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = agent(&w, &reader, "POST", &format!("/api/a2a/terminal/{target}/interrupt"), None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    // A grant is per target and revocable.
    grant(&w, &reader, &target, true).await;
    assert_eq!(agent(&w, &reader, "GET", &format!("/api/a2a/terminal/{target}"), None).await.0, StatusCode::OK);
    grant(&w, &reader, &target, false).await;
    assert_eq!(agent(&w, &reader, "GET", &format!("/api/a2a/terminal/{target}"), None).await.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn fr_t9_send_text_reaches_the_target() {
    let w = world();
    let reader = started(&w, "reader").await;
    let target = started(&w, "target").await;
    grant(&w, &reader, &target, true).await;
    let (st, v) = agent(&w, &reader, "POST", &format!("/api/a2a/terminal/{target}/text"), Some(json!({"text": "ping from reader"}))).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{v}");
    wait_for("text to arrive", || {
        w.sessions.store().events_after(&target, 0).unwrap().iter().any(
            |e| matches!(&e.event, AgentEvent::UserMessage { text } if text.contains("ping from reader")),
        )
    })
    .await;
    assert!(notices(&w.sessions, &target).iter().any(|n| n.contains("sent you text")));
}

#[tokio::test]
async fn fr_t9_interrupt_stops_a_running_turn() {
    let w = world();
    let reader = started(&w, "reader").await;
    let target = started(&w, "target").await;
    grant(&w, &reader, &target, true).await;
    w.sessions.send(&target, "slow task").await.unwrap();
    wait_status(&w.sessions, &target, SessionStatus::Running).await;
    let (st, v) = agent(&w, &reader, "POST", &format!("/api/a2a/terminal/{target}/interrupt"), None).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{v}");
    wait_for("interrupted turn", || {
        w.sessions.store().events_after(&target, 0).unwrap().iter().any(|e| {
            matches!(e.event, AgentEvent::TurnEnded { outcome: TurnOutcome::Interrupted })
        })
    })
    .await;
    assert!(notices(&w.sessions, &target).iter().any(|n| n.contains("interrupted")));
}

#[tokio::test]
async fn fr_t9_cli_terminal_commands_and_permissions() {
    let w = world();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = w.app.clone();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let reader = started(&w, "reader").await;
    let target = started(&w, "target").await;
    let token = w.agents.token(&reader);
    async fn cli(url: &str, token: &str, args: &[&str]) -> std::process::Output {
        tokio::process::Command::new(env!("CARGO_BIN_EXE_ember-a2a"))
            .args(args).env("EMBER_URL", url).env("EMBER_RUNTIME_TOKEN", token)
            .stdin(std::process::Stdio::null()).output().await.unwrap()
    }
    let denied = cli(&url, &token, &["terminal", "read", &target]).await;
    assert_eq!(denied.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&denied.stderr).contains("no terminal permission"));
    grant(&w, &reader, &target, true).await;
    let read = cli(&url, &token, &["terminal", "read", &target, "--lines", "2", "--json"]).await;
    assert!(read.status.success(), "{}", String::from_utf8_lossy(&read.stderr));
    let data: Value = serde_json::from_slice(&read.stdout).unwrap();
    assert_eq!(data["lines"].as_array().unwrap().len(), 2);
    let seq = data["last_seq"].as_i64().unwrap().to_string();
    let follow = cli(&url, &token, &["terminal", "read", &target, "--since", &seq, "--wait-ms", "1", "--json"]).await;
    assert!(follow.status.success());
    assert!(serde_json::from_slice::<Value>(&follow.stdout).unwrap()["lines"].as_array().unwrap().is_empty());
    let sent = cli(&url, &token, &["terminal", "send-text", &target, "hello", "target"]).await;
    assert!(sent.status.success(), "{}", String::from_utf8_lossy(&sent.stderr));
    wait_for("CLI text", || w.sessions.store().events_after(&target, 0).unwrap().iter().any(
        |e| matches!(&e.event, AgentEvent::UserMessage { text } if text == "hello target")
    )).await;
    wait_status(&w.sessions, &target, SessionStatus::WaitingForApproval).await;
    w.sessions.answer(&target, "approval-2", ApprovalDecision::AllowOnce).await.unwrap();
    wait_status(&w.sessions, &target, SessionStatus::Finished).await;
    w.sessions.send(&target, "slow task").await.unwrap();
    wait_status(&w.sessions, &target, SessionStatus::Running).await;
    let interrupted = cli(&url, &token, &["terminal", "interrupt", &target]).await;
    assert!(interrupted.status.success(), "{}", String::from_utf8_lossy(&interrupted.stderr));
    wait_for("CLI interrupt", || w.sessions.store().events_after(&target, 0).unwrap().iter().any(
        |e| matches!(e.event, AgentEvent::TurnEnded { outcome: TurnOutcome::Interrupted })
    )).await;
    let invalid = cli(&url, &token, &["terminal", "read", &target, "--since", "bad"]).await;
    assert_eq!(invalid.status.code(), Some(2));
    let instructions = w.agents.0.lock().unwrap().get(&reader).unwrap().instructions.clone().unwrap();
    assert!(instructions.contains("terminal read") && instructions.contains("terminal send-text") && instructions.contains("terminal interrupt"));
}
