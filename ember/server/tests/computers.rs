//! Computers registry and switching a session between computers (FR-X3, FR-S7 v0), against the
//! scripted agent and fake nodes: no network, no agent CLI.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use ember_node::proto::{EnvInfo, Health};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind, AgentRun, Detected, StartRequest};
use ember_server::computers::{
    self, Computer, ComputerError, Computers, Connector, NodeApi, Registry, LOCAL,
};
use ember_server::events::{AgentEvent, ApprovalDecision, SessionStatus};
use ember_server::session::{NewSession, Sessions};
use ember_server::store::{SessionRecord, Store};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tower::ServiceExt;

// ---------------------------------------------------------------------------------------------
// Fakes

fn env_for(host: &str) -> EnvInfo {
    EnvInfo {
        os: "linux".into(),
        os_version: Some(format!("{host}-os 1.0")),
        kernel: Some("6.12".into()),
        arch: "aarch64".into(),
        hostname: host.into(),
        user: Some("me".into()),
        home: Some(format!("/home/{host}").into()),
        shell: Some("/bin/zsh".into()),
        roots: vec![format!("/home/{host}").into()],
        toolchains: BTreeMap::new(),
    }
}

/// A node that answers from memory; URLs containing "down" are unreachable.
struct FakeNode {
    up: bool,
    env: EnvInfo,
}

#[async_trait]
impl NodeApi for FakeNode {
    async fn health(&self) -> anyhow::Result<Health> {
        anyhow::ensure!(self.up, "connection refused");
        Ok(Health { ok: true, version: "test".into() })
    }

    async fn env(&self) -> anyhow::Result<EnvInfo> {
        Ok(self.env.clone())
    }
}

fn fake_connector() -> Connector {
    Arc::new(|c: Option<&Computer>| -> anyhow::Result<Arc<dyn NodeApi>> {
        Ok(match c {
            None => Arc::new(FakeNode { up: true, env: env_for("server") }),
            Some(c) => Arc::new(FakeNode { up: !c.url.contains("down"), env: env_for(&c.name) }),
        })
    })
}

/// The scripted agent, recording every start request.
struct Capturing {
    starts: Arc<Mutex<Vec<StartRequest>>>,
}

#[async_trait]
impl AgentAdapter for Capturing {
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
        self.starts.lock().unwrap().push(req.clone());
        ScriptedAdapter.start(req, events).await
    }
}

struct Fixture {
    sessions: Arc<Sessions>,
    computers: Arc<Computers>,
    starts: Arc<Mutex<Vec<StartRequest>>>,
}

fn fixture() -> Fixture {
    let starts = Arc::new(Mutex::new(Vec::new()));
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(Capturing { starts: starts.clone() })];
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), adapters);
    let computers = Computers::with_shim(
        Registry::new(store),
        fake_connector(),
        Some("/opt/ember/bin/ember-exec".into()),
    );
    computers.install(&sessions);
    Fixture { sessions, computers, starts }
}

fn new_session(s: &Sessions) -> String {
    s.create(NewSession {
        project: "acme".into(),
        agent: AgentKind::Scripted,
        cwd: std::env::temp_dir(),
        model: None,
        title: "t".into(),
    })
    .unwrap()
    .id
}

async fn wait_status(s: &Sessions, id: &str, want: SessionStatus) {
    for _ in 0..400 {
        if s.store().session(id).unwrap().unwrap().status == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("session {id} never reached {want:?}");
}

/// One full scripted turn: send, approve, finish. Returns the events it stored.
async fn turn(s: &Arc<Sessions>, id: &str, text: &str) -> Vec<AgentEvent> {
    let before = s.store().session(id).unwrap().unwrap().last_seq;
    s.send(id, text).await.unwrap();
    wait_status(s, id, SessionStatus::WaitingForApproval).await;
    let approval = s
        .store()
        .events_after(id, before)
        .unwrap()
        .into_iter()
        .rev()
        .find_map(|e| match e.event {
            AgentEvent::ApprovalRequested { approval_id, .. } => Some(approval_id),
            _ => None,
        })
        .unwrap();
    s.answer(id, &approval, ApprovalDecision::AllowOnce).await.unwrap();
    wait_status(s, id, SessionStatus::Finished).await;
    s.store().events_after(id, before).unwrap().into_iter().map(|e| e.event).collect()
}

fn assistant_text(events: &[AgentEvent]) -> String {
    events
        .iter()
        .find_map(|e| match e {
            AgentEvent::AssistantMessage { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap()
}

// ---------------------------------------------------------------------------------------------
// Switching

#[tokio::test]
async fn switch_replaces_environment_and_injects_the_notice_once() {
    let f = fixture();
    let (s, c) = (&f.sessions, &f.computers);
    let pi = c.register("pi", "http://pi:8741", "tok").unwrap();
    let mac = c.register("mac", "http://mac:8741/", "tok").unwrap();
    assert_eq!(mac.url, "http://mac:8741", "trailing slash trimmed");
    let id = new_session(s);

    // Before any message: implicit local, no instructions at start.
    let cur = c.current(s, &id).unwrap();
    assert!(cur.implicit);
    assert_eq!(cur.computer.id, LOCAL);

    // A session that never ran gets the computer but no notice.
    let out = c.switch(s, &id, &pi.id).await.unwrap();
    assert!(out.changed);
    assert_eq!(out.notice, None);
    assert!(out.env_description.contains("\"pi\" (hostname pi)"));

    let first = turn(s, &id, "hello").await;
    assert!(!first.iter().any(|e| matches!(e, AgentEvent::SystemNotice { .. })));
    assert_eq!(assistant_text(&first), "echo: hello");
    {
        let starts = f.starts.lock().unwrap();
        assert_eq!(starts.len(), 1);
        let instr = starts[0].instructions.as_deref().unwrap();
        assert!(instr.contains("\"pi\" (hostname pi)"), "{instr}");
        assert!(starts[0].env.contains(&("EMBER_COMPUTER_ID".to_string(), pi.id.clone())));
        // The scripted agent is neither Claude Code nor Codex: no shim, no remote executor.
        assert!(starts[0].remote.is_none());
        assert!(!starts[0].env.iter().any(|(k, _)| k == "CLAUDE_CODE_SHELL_PREFIX"));
    }
    assert!(s.is_live(&id).await);

    // Switch: the agent is released and a notice waits for the next message.
    let out = c.switch(s, &id, &mac.id).await.unwrap();
    assert!(out.changed);
    assert_eq!(out.previous.id, pi.id);
    assert_eq!(out.computer.id, mac.id);
    let notice = out.notice.clone().expect("notice for a session that already ran");
    assert!(notice.contains("from \"pi\" to \"mac\""), "{notice}");
    assert!(!s.is_live(&id).await, "agent released so the next start uses the new computer");
    let cur = c.current(s, &id).unwrap();
    assert_eq!(cur.computer.id, mac.id);
    assert!(cur.notice_pending);

    let second = turn(s, &id, "again").await;
    // Stored: the notice, then the user's own words, separately.
    let pos = second.iter().position(|e| matches!(e, AgentEvent::SystemNotice { .. })).unwrap();
    assert_eq!(second[pos], AgentEvent::SystemNotice { text: notice.clone() });
    assert_eq!(second[pos + 1], AgentEvent::UserMessage { text: "again".into() });
    // The agent received the notice in front of the message.
    assert_eq!(assistant_text(&second), format!("echo: {notice}\n\nagain"));
    {
        let starts = f.starts.lock().unwrap();
        assert_eq!(starts.len(), 2);
        // Native resume, with the environment block replaced (not appended).
        assert!(starts[1].resume_native_id.is_some());
        let instr = starts[1].instructions.as_deref().unwrap();
        assert!(instr.contains("\"mac\" (hostname mac)"), "{instr}");
        assert!(!instr.contains("hostname pi"), "{instr}");
        assert_eq!(instr.matches("# Current computer").count(), 1);
    }

    // Delivered once.
    let third = turn(s, &id, "third").await;
    assert!(!third.iter().any(|e| matches!(e, AgentEvent::SystemNotice { .. })));
    assert_eq!(assistant_text(&third), "echo: third");
    assert!(!c.current(s, &id).unwrap().notice_pending);

    // Switching to where it already is changes nothing.
    let out = c.switch(s, &id, &mac.id).await.unwrap();
    assert!(!out.changed);
    assert_eq!(out.notice, None);
    assert!(s.is_live(&id).await);

    // Back to this server: described too, with a notice.
    let out = c.switch(s, &id, LOCAL).await.unwrap();
    assert!(out.env_description.contains("\"this server\" (hostname server)"));
    assert!(out.notice.unwrap().contains("from \"mac\" to \"this server\""));
}

#[tokio::test]
async fn switch_is_refused_mid_turn_unreachable_or_unknown() {
    let f = fixture();
    let (s, c) = (&f.sessions, &f.computers);
    let pi = c.register("pi", "http://pi:8741", "tok").unwrap();
    let down = c.register("down", "http://down:8741", "tok").unwrap();
    let id = new_session(s);

    s.send(&id, "hello").await.unwrap();
    wait_status(s, &id, SessionStatus::WaitingForApproval).await;
    assert!(matches!(c.switch(s, &id, &pi.id).await, Err(ComputerError::Busy(_))));
    s.answer(&id, "approval-1", ApprovalDecision::AllowOnce).await.unwrap();
    wait_status(s, &id, SessionStatus::Finished).await;

    let err = c.switch(s, &id, &down.id).await.unwrap_err();
    assert!(matches!(&err, ComputerError::Unreachable(name, msg) if name == "down" && msg.contains("refused")), "{err:?}");
    assert!(c.current(s, &id).unwrap().implicit, "a failed switch changes nothing");
    assert!(s.is_live(&id).await, "and keeps the agent");

    assert!(matches!(c.switch(s, &id, "nope").await, Err(ComputerError::NotFound(_))));
    assert!(matches!(c.switch(s, "no-session", &pi.id).await, Err(ComputerError::SessionNotFound(_))));

    c.switch(s, &id, &pi.id).await.unwrap();
    assert!(matches!(c.remove(&pi.id), Err(ComputerError::InUse(_, 1))));
    assert!(matches!(c.remove(LOCAL), Err(ComputerError::BadRequest(_))));
    c.remove(&down.id).unwrap();
    assert!(matches!(c.remove(&down.id), Err(ComputerError::NotFound(_))));
}

#[tokio::test]
async fn registration_is_validated() {
    let f = fixture();
    let c = &f.computers;
    assert!(matches!(c.register("pi", "ftp://pi", "t"), Err(ComputerError::BadRequest(_))));
    assert!(matches!(c.register("pi", "http://pi:1", ""), Err(ComputerError::BadRequest(_))));
    assert!(matches!(c.register("local", "http://pi:1", "t"), Err(ComputerError::BadRequest(_))));
    assert!(matches!(c.register("  ", "http://pi:1", "t"), Err(ComputerError::BadRequest(_))));
    c.register("pi", "http://pi:1", "t").unwrap();
    assert!(matches!(c.register("pi", "http://pi:2", "t"), Err(ComputerError::BadRequest(_))));
}

// ---------------------------------------------------------------------------------------------
// Start configuration per agent

fn record(id: &str, agent: AgentKind) -> SessionRecord {
    SessionRecord {
        id: id.into(),
        project: "acme".into(),
        agent,
        cwd: "/home/pi/proj".into(),
        model: None,
        native_id: None,
        status: SessionStatus::Idle,
        title: "t".into(),
        created_at: 0,
        updated_at: 0,
        last_seq: 0,
        account_id: None,
        account_reason: None,
        pinned: false,
        archived: false,
    }
}

#[tokio::test]
async fn claude_code_on_a_node_gets_the_shell_shim() {
    let f = fixture();
    let c = &f.computers;
    let pi = c.register("pi", "http://pi:8741", "tok").unwrap();
    c.registry().set_session_computer("s-claude", &pi.id, Some(&env_for("pi")), None).unwrap();

    let mut req = StartRequest { cwd: "/home/pi/proj".into(), ..Default::default() };
    c.configure_start(&record("s-claude", AgentKind::ClaudeCode), &mut req).unwrap();
    let env: BTreeMap<_, _> = req.env.iter().cloned().collect();
    assert_eq!(env["CLAUDE_CODE_SHELL_PREFIX"], "/opt/ember/bin/ember-exec");
    assert_eq!(env[computers::shim::ENV_NODE_URL], "http://pi:8741");
    assert_eq!(env[computers::shim::ENV_NODE_TOKEN], "tok");
    assert_eq!(env[computers::shim::ENV_REMOTE_SHELL], "/bin/zsh");
    assert!(req.remote.is_none());
    // The environment block is an instructions hook, not part of configure_start.
    assert!(req.instructions.is_none());
    let instr = c.instructions(&record("s-claude", AgentKind::ClaudeCode)).unwrap().unwrap();
    assert!(instr.contains("hostname pi"), "{instr}");

    // Without a shim the start fails rather than silently running Bash on the server.
    let no_shim = Computers::new(Registry::open_in_memory().unwrap(), fake_connector());
    let pi2 = no_shim.register("pi", "http://pi:8741", "tok").unwrap();
    no_shim.registry().set_session_computer("s", &pi2.id, Some(&env_for("pi")), None).unwrap();
    let mut req = StartRequest::default();
    assert!(no_shim.configure_start(&record("s", AgentKind::ClaudeCode), &mut req).is_err());
}

#[tokio::test]
async fn codex_on_a_node_gets_a_loopback_exec_server() {
    let f = fixture();
    let c = &f.computers;
    let pi = c.register("pi", "http://pi:8741", "tok").unwrap();
    c.registry().set_session_computer("s-codex", &pi.id, Some(&env_for("pi")), None).unwrap();

    let mut req = StartRequest { cwd: "/home/pi/proj".into(), ..Default::default() };
    c.configure_start(&record("s-codex", AgentKind::Codex), &mut req).unwrap();
    let remote = req.remote.clone().unwrap();
    assert_eq!(remote.environment_id, format!("ember-{}", pi.id));
    assert!(remote.exec_server_url.starts_with("ws://127.0.0.1:"), "{}", remote.exec_server_url);
    assert!(!req.env.iter().any(|(k, _)| k == "CLAUDE_CODE_SHELL_PREFIX"));

    // One relay per computer, reused by the next start.
    let mut again = StartRequest::default();
    c.configure_start(&record("s-codex", AgentKind::Codex), &mut again).unwrap();
    assert_eq!(again.remote.unwrap().exec_server_url, remote.exec_server_url);

    // On this server, or with no recorded computer: no remote executor.
    c.registry().set_session_computer("s-local", LOCAL, Some(&env_for("server")), None).unwrap();
    let mut local = StartRequest::default();
    c.configure_start(&record("s-local", AgentKind::Codex), &mut local).unwrap();
    assert!(local.remote.is_none());
    let instr = c.instructions(&record("s-local", AgentKind::Codex)).unwrap().unwrap();
    assert!(instr.contains("\"this server\""), "{instr}");
    let mut untouched = StartRequest::default();
    c.configure_start(&record("s-none", AgentKind::Codex), &mut untouched).unwrap();
    assert!(untouched.remote.is_none() && untouched.instructions.is_none() && untouched.env.is_empty());
    assert!(c.instructions(&record("s-none", AgentKind::Codex)).unwrap().is_none());
}

// ---------------------------------------------------------------------------------------------
// HTTP

async fn call(app: &axum::Router, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let req = Request::builder().method(method).uri(uri);
    let req = match body {
        Some(b) => req.header("content-type", "application/json").body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
    (status, value)
}

#[tokio::test]
async fn http_register_list_and_switch() {
    let f = fixture();
    let app = ember_server::api::router(f.sessions.clone())
        .merge(computers::api::router(f.computers.clone(), f.sessions.clone()));

    let (st, pi) = call(&app, Method::POST, "/api/computers", Some(json!({ "name": "pi", "url": "http://pi:8741", "token": "tok" }))).await;
    assert_eq!(st, StatusCode::CREATED);
    assert!(pi.get("token").is_none(), "token never returned: {pi}");
    let pi_id = pi["id"].as_str().unwrap().to_string();
    call(&app, Method::POST, "/api/computers", Some(json!({ "name": "down", "url": "http://down:1", "token": "t" }))).await;

    let (st, list) = call(&app, Method::GET, "/api/computers", None).await;
    assert_eq!(st, StatusCode::OK);
    let list = list.as_array().unwrap();
    assert_eq!(list[0]["id"], LOCAL);
    assert_eq!(list[0]["reachable"], true);
    let by_name = |n: &str| list.iter().find(|c| c["name"] == n).unwrap().clone();
    assert_eq!(by_name("pi")["reachable"], true);
    assert_eq!(by_name("down")["reachable"], false);
    let (_, unprobed) = call(&app, Method::GET, "/api/computers?probe=false", None).await;
    assert!(unprobed.as_array().unwrap().iter().all(|c| c["reachable"].is_null()));

    let (st, one) = call(&app, Method::GET, &format!("/api/computers/{pi_id}"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(one["env"]["hostname"], "pi");

    let id = new_session(&f.sessions);
    let (st, out) = call(&app, Method::PUT, &format!("/api/sessions/{id}/computer"), Some(json!({ "computer_id": pi_id }))).await;
    assert_eq!(st, StatusCode::OK, "{out}");
    assert_eq!(out["computer"]["name"], "pi");
    assert_eq!(out["changed"], true);
    let (_, cur) = call(&app, Method::GET, &format!("/api/sessions/{id}/computer"), None).await;
    assert_eq!(cur["computer"]["id"], pi_id.as_str());
    assert_eq!(cur["implicit"], false);

    let down_id = by_name("down")["id"].as_str().unwrap().to_string();
    let (st, _) = call(&app, Method::PUT, &format!("/api/sessions/{id}/computer"), Some(json!({ "computer_id": down_id }))).await;
    assert_eq!(st, StatusCode::BAD_GATEWAY);
    let (st, _) = call(&app, Method::DELETE, &format!("/api/computers/{pi_id}"), None).await;
    assert_eq!(st, StatusCode::CONFLICT);
    let (st, _) = call(&app, Method::DELETE, &format!("/api/computers/{down_id}"), None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = call(&app, Method::GET, "/api/sessions/nope/computer", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}
