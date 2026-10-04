//! Agent-to-agent messaging against the scripted agent: FR-T1–FR-T6.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::a2a::{A2a, A2aConfig, A2aStore, Limits};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind, AgentRun, Detected, StartRequest};
use ember_server::events::{AgentEvent, ApprovalDecision, SessionStatus};
use ember_server::session::{NewSession, Sessions};
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tower::ServiceExt;

/// The scripted agent, remembering each process's start request by `EMBER_SESSION_ID`.
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
        let id = req
            .env
            .iter()
            .find(|(k, _)| k == "EMBER_SESSION_ID")
            .map(|(_, v)| v.clone());
        self.0
            .lock()
            .unwrap()
            .insert(id.unwrap_or_default(), req.clone());
        ScriptedAdapter.start(req, events).await
    }
}

impl Recording {
    fn env(&self, session: &str, key: &str) -> String {
        let seen = self.0.lock().unwrap();
        let req = seen
            .get(session)
            .unwrap_or_else(|| panic!("session {session} never started"));
        req.env
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap()
    }
    fn token(&self, session: &str) -> String {
        self.env(session, "EMBER_RUNTIME_TOKEN")
    }
}

struct World {
    sessions: Arc<Sessions>,
    a2a: Arc<A2a>,
    agents: Recording,
    app: Router,
}

fn world_with(store: Arc<Store>, a2a_store: A2aStore, limits: Limits) -> World {
    let agents = Recording::default();
    let sessions = Sessions::new(
        store,
        vec![Arc::new(agents.clone()) as Arc<dyn AgentAdapter>],
    );
    let mut config = A2aConfig::new("http://127.0.0.1:1");
    config.limits = limits;
    let a2a = A2a::new(sessions.clone(), a2a_store, config);
    a2a.install();
    let app = ember_server::api::router(sessions.clone())
        .merge(ember_server::a2a::api::router(a2a.clone()));
    World {
        sessions,
        a2a,
        agents,
        app,
    }
}

fn world() -> World {
    world_with(
        Arc::new(Store::open_in_memory().unwrap()),
        A2aStore::open_in_memory().unwrap(),
        Limits::default(),
    )
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
    wait_for(&format!("{id} to be {want:?}"), || {
        s.store().session(id).unwrap().unwrap().status == want
    })
    .await;
}

fn user_messages(s: &Sessions, id: &str) -> Vec<String> {
    s.store()
        .events_after(id, 0)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            AgentEvent::UserMessage { text } => Some(text),
            _ => None,
        })
        .collect()
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

/// Run one scripted turn to completion so the session is started and between turns.
async fn finish_turn(s: &Arc<Sessions>, id: &str, turn: u32) {
    wait_status(s, id, SessionStatus::WaitingForApproval).await;
    s.answer(id, &format!("approval-{turn}"), ApprovalDecision::AllowOnce)
        .await
        .unwrap();
    wait_status(s, id, SessionStatus::Finished).await;
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, v)
}

async fn send(
    w: &World,
    from: &str,
    to: &str,
    text: &str,
    reply_to: Option<&str>,
) -> (StatusCode, Value) {
    let token = w.agents.token(from);
    call(
        &w.app,
        "POST",
        "/api/a2a/messages",
        Some(&token),
        Some(json!({ "to": to, "text": text, "reply_to": reply_to })),
    )
    .await
}

#[tokio::test]
async fn agents_get_credentials_and_instructions() {
    let w = world();
    let a = new_session(&w.sessions, "alpha");
    w.sessions.send(&a, "hi").await.unwrap();
    wait_status(&w.sessions, &a, SessionStatus::WaitingForApproval).await;
    assert_eq!(w.agents.env(&a, "EMBER_URL"), "http://127.0.0.1:1");
    let token = w.agents.token(&a);
    assert_eq!(w.a2a.authenticate(&token).unwrap(), a);
    let req = w.agents.0.lock().unwrap().get(&a).cloned().unwrap();
    assert!(req
        .instructions
        .unwrap()
        .contains("ember-a2a send <session-id> --reply-to"));

    // No or a bad token is refused.
    let (st, v) = call(&w.app, "GET", "/api/a2a/targets", None, None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(v["code"], "unauthorized");
    let (st, _) = call(&w.app, "GET", "/api/a2a/targets", Some("nope"), None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn two_sessions_exchange_a_message_and_a_reply() {
    // FR-T1, FR-T3: list, send, the target wakes, replies with one call using the reference.
    let w = world();
    let a = new_session(&w.sessions, "alpha");
    let b = new_session(&w.sessions, "beta");
    w.sessions.send(&a, "start").await.unwrap();
    finish_turn(&w.sessions, &a, 1).await;

    let (st, targets) = call(
        &w.app,
        "GET",
        "/api/a2a/targets",
        Some(&w.agents.token(&a)),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let targets = targets.as_array().unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0]["id"], b.as_str());
    assert_eq!(targets[0]["title"], "beta");
    assert_eq!(targets[0]["project"], "acme");
    assert_eq!(targets[0]["agent"], "scripted");
    assert_eq!(targets[0]["status"], "idle");

    // B has never run: the message starts it.
    let (st, r) = send(&w, &a, &b, "can you check the tests?", None).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{r}");
    assert_eq!(r["status"], "delivered");
    let msg_id = r["id"].as_str().unwrap().to_string();
    assert!(w.sessions.is_live(&b).await);
    wait_for("delivery", || user_messages(&w.sessions, &b).len() == 1).await;
    let got = user_messages(&w.sessions, &b);
    assert_eq!(got.len(), 1);
    assert!(got[0].starts_with(&format!("[ember a2a] Message {msg_id} from session {a} \"alpha\" (project acme, agent scripted)")), "{}", got[0]);
    assert!(got[0].contains(&format!("ember-a2a send {a} --reply-to {msg_id}")));
    assert!(got[0].ends_with("\n\ncan you check the tests?"));
    finish_turn(&w.sessions, &b, 1).await;

    let (st, r) = send(&w, &b, &a, "all green", Some(&msg_id)).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{r}");
    assert_eq!(r["status"], "delivered");
    wait_for("reply delivery", || {
        user_messages(&w.sessions, &a).len() == 2
    })
    .await;
    let got = user_messages(&w.sessions, &a);
    assert_eq!(got.len(), 2);
    assert!(got[1].contains(&format!("from session {b} \"beta\"")));
    assert!(got[1].contains(&format!("In reply to message {msg_id}.")));
    assert!(got[1].ends_with("all green"));

    // A reply reference must belong to the conversation.
    let c = new_session(&w.sessions, "gamma");
    w.sessions.send(&c, "start").await.unwrap();
    wait_status(&w.sessions, &c, SessionStatus::WaitingForApproval).await;
    let (st, v) = send(&w, &c, &a, "hijack", Some(&msg_id)).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (st, _) = send(&w, &a, &a, "self", None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = send(&w, &a, "missing", "x", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn messages_to_a_busy_session_wait_for_the_turn_boundary() {
    // FR-T4 (mid-turn): queued, then delivered together when the turn ends.
    let w = world();
    let a = new_session(&w.sessions, "alpha");
    let b = new_session(&w.sessions, "beta");
    w.sessions.send(&a, "start").await.unwrap();
    wait_status(&w.sessions, &a, SessionStatus::WaitingForApproval).await;
    w.sessions.send(&b, "busy work").await.unwrap();
    wait_status(&w.sessions, &b, SessionStatus::WaitingForApproval).await;

    let (_, r1) = send(&w, &a, &b, "first", None).await;
    let (_, r2) = send(&w, &a, &b, "second", None).await;
    assert_eq!(r1["status"], "queued");
    assert_eq!(r2["status"], "queued");
    assert_eq!(
        user_messages(&w.sessions, &b),
        vec!["busy work".to_string()]
    );

    w.sessions
        .answer(&b, "approval-1", ApprovalDecision::AllowOnce)
        .await
        .unwrap();
    wait_for("delivery at turn end", || {
        user_messages(&w.sessions, &b).len() == 2
    })
    .await;
    let got = user_messages(&w.sessions, &b);
    let first = got[1].find("first").unwrap();
    let second = got[1].find("second").unwrap();
    assert!(first < second);
    assert!(
        got[1].contains(r1["id"].as_str().unwrap()) && got[1].contains(r2["id"].as_str().unwrap())
    );
    wait_status(&w.sessions, &b, SessionStatus::WaitingForApproval).await;
    assert!(w.a2a.store().pending_for(&b).unwrap().is_empty());

    // Nothing more is delivered while the delivered turn runs.
    let (_, r3) = send(&w, &a, &b, "third", None).await;
    assert_eq!(r3["status"], "queued");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(user_messages(&w.sessions, &b).len(), 2);
    w.sessions
        .answer(&b, "approval-2", ApprovalDecision::AllowOnce)
        .await
        .unwrap();
    wait_for("third delivery", || {
        user_messages(&w.sessions, &b).len() == 3
    })
    .await;
}

#[tokio::test]
async fn messages_to_a_released_session_wake_it() {
    // FR-T4 (sleeping): an idle-released session is resumed by the delivery.
    let w = world();
    let a = new_session(&w.sessions, "alpha");
    let b = new_session(&w.sessions, "beta");
    for id in [&a, &b] {
        w.sessions.send(id, "start").await.unwrap();
        finish_turn(&w.sessions, id, 1).await;
    }
    w.sessions.release(&b).await.unwrap();
    assert!(!w.sessions.is_live(&b).await);
    let (_, r) = send(&w, &a, &b, "wake up", None).await;
    assert_eq!(r["status"], "delivered");
    assert!(w.sessions.is_live(&b).await);
    wait_status(&w.sessions, &b, SessionStatus::WaitingForApproval).await;
}

#[tokio::test]
async fn queued_messages_are_delivered_after_a_restart() {
    // FR-T4: restart the main server with a message queued; it is delivered after restart.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ember.db");
    let (a, b, msg_id);
    {
        let w = world_with(
            Arc::new(Store::open(&db).unwrap()),
            A2aStore::open(&db).unwrap(),
            Limits::default(),
        );
        a = new_session(&w.sessions, "alpha");
        b = new_session(&w.sessions, "beta");
        w.sessions.send(&a, "start").await.unwrap();
        wait_status(&w.sessions, &a, SessionStatus::WaitingForApproval).await;
        w.sessions.send(&b, "long task").await.unwrap();
        wait_status(&w.sessions, &b, SessionStatus::WaitingForApproval).await;
        let (_, r) = send(&w, &a, &b, "after you restart", None).await;
        assert_eq!(r["status"], "queued");
        msg_id = r["id"].as_str().unwrap().to_string();
        for id in [&a, &b] {
            w.sessions.release(id).await.unwrap();
        }
    }

    let store = Arc::new(Store::open(&db).unwrap());
    assert_eq!(store.reset_live_statuses().unwrap(), 2);
    let w = world_with(store, A2aStore::open(&db).unwrap(), Limits::default());
    wait_for("delivery after restart", || {
        user_messages(&w.sessions, &b).len() == 2
    })
    .await;
    let got = user_messages(&w.sessions, &b);
    assert!(got[1].contains(&msg_id) && got[1].ends_with("after you restart"));
    assert!(w
        .a2a
        .store()
        .message(&msg_id)
        .unwrap()
        .unwrap()
        .delivered_at
        .is_some());
    wait_status(&w.sessions, &b, SessionStatus::WaitingForApproval).await;
}

#[tokio::test]
async fn a_reply_loop_is_stopped_with_a_notice_in_both_sessions() {
    // FR-T5: two agents replying to each other forever hit the pair limit.
    let w = world_with(
        Arc::new(Store::open_in_memory().unwrap()),
        A2aStore::open_in_memory().unwrap(),
        Limits {
            window: Duration::from_secs(600),
            per_session: 20,
            per_pair: 6,
        },
    );
    let a = new_session(&w.sessions, "alpha");
    let b = new_session(&w.sessions, "beta");
    for id in [&a, &b] {
        w.sessions.send(id, "start").await.unwrap();
        finish_turn(&w.sessions, id, 1).await;
    }
    let mut turn = HashMap::from([(a.clone(), 1u32), (b.clone(), 1u32)]);
    let (mut from, mut to) = (a.clone(), b.clone());
    let mut reply_to: Option<String> = None;
    let mut sent = 0;
    let rejected = loop {
        let (st, r) = send(&w, &from, &to, "your turn", reply_to.as_deref()).await;
        if st != StatusCode::ACCEPTED {
            break (st, r);
        }
        sent += 1;
        assert_eq!(r["status"], "delivered");
        reply_to = Some(r["id"].as_str().unwrap().to_string());
        // The receiver's turn ends, then it "replies".
        let t = turn.get_mut(&to).unwrap();
        *t += 1;
        finish_turn(&w.sessions, &to, *t).await;
        std::mem::swap(&mut from, &mut to);
        assert!(sent < 100, "loop was never stopped");
    };
    assert_eq!(sent, 6);
    assert_eq!(rejected.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(rejected.1["code"], "rate_limited");
    assert!(rejected.1["error"]
        .as_str()
        .unwrap()
        .contains("loop protection"));
    for id in [&a, &b] {
        let n = notices(&w.sessions, id);
        assert_eq!(n.len(), 1, "{id}: {n:?}");
        assert!(n[0].contains("loop protection"));
    }
    // Retrying does not flood the transcripts.
    let (st, _) = send(&w, &from, &to, "again", None).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(notices(&w.sessions, &a).len(), 1);
    // The notice leaves the session's status alone.
    assert_eq!(
        w.sessions.store().session(&a).unwrap().unwrap().status,
        SessionStatus::Finished
    );
}

#[tokio::test]
async fn per_session_send_rate_is_limited() {
    // FR-T5: one session fanning out to many is limited too.
    let w = world_with(
        Arc::new(Store::open_in_memory().unwrap()),
        A2aStore::open_in_memory().unwrap(),
        Limits {
            window: Duration::from_secs(600),
            per_session: 3,
            per_pair: 10,
        },
    );
    let a = new_session(&w.sessions, "alpha");
    w.sessions.send(&a, "start").await.unwrap();
    wait_status(&w.sessions, &a, SessionStatus::WaitingForApproval).await;
    let targets: Vec<String> = (0..4)
        .map(|i| new_session(&w.sessions, &format!("t{i}")))
        .collect();
    for t in &targets[..3] {
        let (st, _) = send(&w, &a, t, "hello", None).await;
        assert_eq!(st, StatusCode::ACCEPTED);
    }
    let (st, v) = send(&w, &a, &targets[3], "hello", None).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .contains(&format!("session {a} has sent 3")),
        "{v}"
    );
    assert_eq!(notices(&w.sessions, &a).len(), 1);
    assert_eq!(notices(&w.sessions, &targets[3]).len(), 1);
    assert!(user_messages(&w.sessions, &targets[3]).is_empty());
}

#[tokio::test]
async fn a2a_can_be_switched_off_per_session_and_globally() {
    // FR-T6.
    let w = world();
    let a = new_session(&w.sessions, "alpha");
    let b = new_session(&w.sessions, "beta");
    for id in [&a, &b] {
        w.sessions.send(id, "start").await.unwrap();
        finish_turn(&w.sessions, id, 1).await;
    }
    let (st, v) = call(
        &w.app,
        "PUT",
        &format!("/api/sessions/{b}/a2a"),
        None,
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!((st, v), (StatusCode::OK, json!({"enabled": false})));
    let (_, v) = call(
        &w.app,
        "GET",
        &format!("/api/sessions/{b}/a2a"),
        None,
        None,
    )
    .await;
    assert_eq!(v, json!({"enabled": false}));

    // Sends to and from the switched-off session are refused with a reason.
    let (st, v) = send(&w, &a, &b, "hi", None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(v["code"], "disabled");
    assert!(v["error"]
        .as_str()
        .unwrap()
        .contains("turned off for the target session"));
    let (st, v) = send(&w, &b, &a, "hi", None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert!(v["error"]
        .as_str()
        .unwrap()
        .contains("turned off for this session"));
    // And it is not offered as a target.
    let (_, t) = call(
        &w.app,
        "GET",
        "/api/a2a/targets",
        Some(&w.agents.token(&a)),
        None,
    )
    .await;
    assert_eq!(t, json!([]));

    call(
        &w.app,
        "PUT",
        &format!("/api/sessions/{b}/a2a"),
        None,
        Some(json!({"enabled": true})),
    )
    .await;
    let (st, _) = send(&w, &a, &b, "hi", None).await;
    assert_eq!(st, StatusCode::ACCEPTED);

    // Server-wide off.
    let (st, _) = call(
        &w.app,
        "PUT",
        "/api/a2a/settings",
        None,
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, v) = send(&w, &a, &b, "hi", None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert!(v["error"]
        .as_str()
        .unwrap()
        .contains("turned off on this server"));
    let (st, _) = call(
        &w.app,
        "GET",
        "/api/a2a/targets",
        Some(&w.agents.token(&a)),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (_, v) = call(&w.app, "GET", "/api/a2a/settings", None, None).await;
    assert_eq!(v, json!({"enabled": false}));
    let (st, _) = call(&w.app, "GET", "/api/sessions/nope/a2a", None, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_ember_a2a_cli_lists_and_sends_over_http() {
    // FR-T2: the agent-side tool, run as a process with the session's environment.
    let w = world();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = w.app.clone();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let a = new_session(&w.sessions, "alpha");
    let b = new_session(&w.sessions, "beta");
    w.sessions.send(&a, "start").await.unwrap();
    finish_turn(&w.sessions, &a, 1).await;
    let token = w.agents.token(&a);
    let cli = |args: &[&str]| {
        tokio::process::Command::new(env!("CARGO_BIN_EXE_ember-a2a"))
            .args(args)
            .env("EMBER_URL", &url)
            .env("EMBER_RUNTIME_TOKEN", &token)
            .stdin(std::process::Stdio::null())
            .output()
    };

    let out = cli(&["list"]).await.unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listed = String::from_utf8_lossy(&out.stdout);
    assert!(
        listed.contains(&b) && listed.contains("beta") && listed.contains("scripted"),
        "{listed}"
    );

    let out = cli(&["send", &b, "please", "review", "PR", "7"])
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("delivered"));
    wait_for("cli delivery", || user_messages(&w.sessions, &b).len() == 1).await;
    assert!(user_messages(&w.sessions, &b)[0].ends_with("please review PR 7"));
    assert_eq!(w.a2a.store().sent_since(&a, 0).unwrap(), 1);

    let out = cli(&["send", &b, "--reply-to", "msg_unknown", "x"])
        .await
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown message msg_unknown"));

    let out = cli(&["send"]).await.unwrap();
    assert_eq!(out.status.code(), Some(2));
}
