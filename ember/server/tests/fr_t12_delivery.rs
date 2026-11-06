//! Delivery of a person's channel mentions to agents (SPEC FR-T12, FR-T13).

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::a2a::{A2a, A2aConfig, A2aStore};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::events::AgentEvent;
use ember_server::session::{NewSession, Sessions};
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

struct World {
    sessions: Arc<Sessions>,
    a2a: Arc<A2a>,
    app: Router,
    channel: String,
    alpha: String,
    beta: String,
    _dir: std::path::PathBuf,
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn new_session(s: &Sessions, title: &str) -> String {
    s.create(NewSession {
        project: "core".into(),
        agent: AgentKind::Scripted,
        cwd: std::env::temp_dir(),
        model: None,
        title: title.into(),
    })
    .unwrap()
    .id
}

/// A channel `review` holding agents `alpha` and `beta`, with A2A wired in.
async fn world() -> World {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let a2a = A2a::new(sessions.clone(), A2aStore::open_in_memory().unwrap(), A2aConfig::new("http://127.0.0.1:1"));
    a2a.install();
    let dir = std::env::temp_dir().join(format!("ember-t12-{}", uuid::Uuid::new_v4().simple()));
    let app = ember_server::channels::api::router_with_a2a(store, sessions.clone(), &dir, a2a.clone());
    let (alpha, beta) = (new_session(&sessions, "alpha"), new_session(&sessions, "beta"));
    let (_, c) = call(
        &app,
        "POST",
        "/api/channels",
        Some(json!({"project":"core","name":"review","agents":[alpha, beta]})),
    )
    .await;
    let channel = c["id"].as_str().unwrap().to_string();
    World { sessions, a2a, app, channel, alpha, beta, _dir: dir }
}

impl World {
    async fn post(&self, text: &str) -> Value {
        let (st, m) = call(
            &self.app,
            "POST",
            &format!("/api/channels/{}/messages", self.channel),
            Some(json!({"text": text})),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED, "{m}");
        m
    }

    async fn log(&self) -> Vec<Value> {
        let (_, all) = call(&self.app, "GET", &format!("/api/channels/{}/messages", self.channel), None).await;
        all.as_array().unwrap().clone()
    }

    fn user_messages(&self, id: &str) -> Vec<String> {
        self.sessions
            .store()
            .events_after(id, 0)
            .unwrap()
            .into_iter()
            .filter_map(|e| match e.event {
                AgentEvent::UserMessage { text } => Some(text),
                _ => None,
            })
            .collect()
    }

    async fn wait_user_messages(&self, id: &str, n: usize) -> Vec<String> {
        for _ in 0..1000 {
            let got = self.user_messages(id);
            if got.len() >= n {
                return got;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("timed out waiting for {n} user message(s) in {id}");
    }

    async fn wait_system(&self) -> String {
        for _ in 0..1000 {
            if let Some(m) = self.log().await.iter().find(|m| m["from"] == "system") {
                return m["text"].as_str().unwrap().to_string();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("no system message appeared in the channel");
    }
}

#[tokio::test]
async fn fr_t12_mention_delivers_once_per_mentioned_agent() {
    let w = world().await;
    w.post("@alpha please look, @ALPHA").await;
    let got = w.wait_user_messages(&w.alpha, 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(w.user_messages(&w.alpha).len(), 1, "one mention name, one delivery: {got:?}");
    assert!(w.user_messages(&w.beta).is_empty(), "beta was not mentioned");
    assert!(got[0].contains("please look"));
}

#[tokio::test]
async fn fr_t12_message_without_mention_reaches_no_agent_and_spends_no_tokens() {
    let w = world().await;
    w.post("lunch, anyone? mail bob@x.com or @@alpha").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    for id in [&w.alpha, &w.beta] {
        assert!(
            w.sessions.store().events_after(id, 0).unwrap().is_empty(),
            "no event, so no turn and no tokens, in {id}"
        );
    }
    assert!(w.a2a.store().pending_for(&w.alpha).unwrap().is_empty());
    assert_eq!(w.log().await.len(), 1, "only the person's message is in the log");
}

#[tokio::test]
async fn fr_t12_header_names_person_channel_name_and_id() {
    let w = world().await;
    w.post("@beta ship it").await;
    let got = w.wait_user_messages(&w.beta, 1).await;
    let text = &got[0];
    assert!(text.starts_with("[ember a2a] Message msg_"), "{text}");
    assert!(
        text.lines().next().unwrap().ends_with(&format!("from 나 (channel #review, id {})", w.channel)),
        "{text}"
    );
    assert!(text.contains("from a person in a channel, not from another agent"), "{text}");
    assert!(
        text.contains(&format!("To reply in the channel: ember-a2a channel post {} \"...\"\n", w.channel)),
        "{text}"
    );
    assert!(text.ends_with("ship it"), "{text}");
    assert!(!text.contains("--reply-to"), "no session to reply to by A2A: {text}");
    let m = w.a2a.store().pending_for(&w.beta).unwrap();
    assert!(m.is_empty(), "delivered");
}

#[tokio::test]
async fn fr_t13_a2a_off_for_target_session_is_reported_in_the_channel() {
    let w = world().await;
    w.a2a.set_session_enabled(&w.alpha, false).unwrap();
    let posted = w.post("@alpha hello").await;
    let note = w.wait_system().await;
    assert_eq!(
        note,
        format!("@alpha not reached: agent-to-agent messaging is turned off for the target session ({})", w.alpha)
    );
    assert!(w.user_messages(&w.alpha).is_empty());
    let log = w.log().await;
    assert_eq!(log[0]["text"], posted["text"], "the message stays posted");
    assert_eq!(log[1]["from"], "system");
}

#[tokio::test]
async fn fr_t13_a2a_off_server_wide_is_reported_in_the_channel() {
    let w = world().await;
    w.a2a.set_enabled(false).unwrap();
    w.post("@beta hello").await;
    let note = w.wait_system().await;
    assert_eq!(note, "@beta not reached: agent-to-agent messaging is turned off on this server");
    assert!(w.user_messages(&w.beta).is_empty());
}

#[tokio::test]
async fn fr_t13_active_teammate_target_is_refused_by_team_rule() {
    let w = world().await;
    let leader = new_session(&w.sessions, "lead");
    let team = w.sessions.store().create_team(&leader, "core").unwrap();
    w.sessions.store().add_team_member(&team.id, &w.alpha, "helper").unwrap();
    w.post("@alpha hello").await;
    let note = w.wait_system().await;
    assert!(note.starts_with("@alpha not reached: "), "{note}");
    assert!(note.contains(&format!("is a teammate in team {}; only its team can message it", team.id)), "{note}");
    assert!(w.user_messages(&w.alpha).is_empty());
}
