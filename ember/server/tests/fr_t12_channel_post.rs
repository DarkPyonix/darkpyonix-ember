//! An agent posts into a channel with `ember-a2a channel post` (SPEC FR-T12, FR-S9).
//!
//! The route is `POST /api/channels/{id}/post`. It carries the session's runtime token, not the
//! server token, so `auth.rs` exempts exactly that path; every other channel route still needs
//! the server token.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::a2a::{A2a, A2aConfig, A2aStore};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::auth::{self, ServerAuth};
use ember_server::events::AgentEvent;
use ember_server::session::{NewSession, Push, Sessions};
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const LISTEN: &str = "127.0.0.1:8740";

struct World {
    sessions: Arc<Sessions>,
    a2a: Arc<A2a>,
    app: Router,
    guarded: Router,
    server_token: String,
    channel: String,
    alpha: String,
    beta: String,
    outsider: String,
    _dir: tempfile::TempDir,
}

async fn call(app: &Router, method: &str, uri: &str, auth: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri).header("host", LISTEN);
    if let Some(t) = auth {
        req = req.header("authorization", format!("Bearer {t}"));
    }
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

/// A channel `review` holding `alpha` and `beta`; `outsider` is not a member.
async fn world() -> World {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let a2a = A2a::new(sessions.clone(), A2aStore::open_in_memory().unwrap(), A2aConfig::new("http://127.0.0.1:1"));
    a2a.install();
    let dir = tempfile::tempdir().unwrap();
    let app = ember_server::channels::api::router_with_a2a(store, sessions.clone(), dir.path(), a2a.clone());
    let server_token = auth::load_or_create_token(dir.path()).unwrap();
    let guarded = auth::protect(app.clone(), ServerAuth::new(server_token.clone(), LISTEN.parse::<SocketAddr>().unwrap(), []));
    let (alpha, beta, outsider) =
        (new_session(&sessions, "alpha"), new_session(&sessions, "beta"), new_session(&sessions, "out"));
    let (_, c) = call(
        &app,
        "POST",
        "/api/channels",
        None,
        Some(json!({"project":"core","name":"review","agents":[alpha, beta]})),
    )
    .await;
    let channel = c["id"].as_str().unwrap().to_string();
    World { sessions, a2a, app, guarded, server_token, channel, alpha, beta, outsider, _dir: dir }
}

impl World {
    fn token(&self, session: &str) -> String {
        self.a2a.store().issue_token(session).unwrap()
    }

    /// Record a person's mention of `session` in the channel, as delivery does (FR-T12).
    fn mention(&self, session: &str) {
        self.a2a.store().insert_channel_message(&self.channel, "나", session, "please look").unwrap();
    }

    fn path(&self) -> String {
        format!("/api/channels/{}/post", self.channel)
    }

    async fn post_as(&self, token: Option<&str>, text: &str) -> (StatusCode, Value) {
        call(&self.guarded, "POST", &self.path(), token, Some(json!({ "text": text }))).await
    }

    async fn log(&self) -> Vec<Value> {
        let uri = format!("/api/channels/{}/messages", self.channel);
        let (_, all) = call(&self.guarded, "GET", &uri, Some(&self.server_token), None).await;
        all.as_array().unwrap().clone()
    }

    fn user_messages(&self, id: &str) -> usize {
        self.sessions
            .store()
            .events_after(id, 0)
            .unwrap()
            .into_iter()
            .filter(|e| matches!(e.event, AgentEvent::UserMessage { .. }))
            .count()
    }
}

#[tokio::test]
async fn fr_t12_channel_post_appends_agent_message_attributed_to_the_session() {
    let w = world().await;
    w.mention(&w.alpha);
    let mut push = w.sessions.subscribe();
    let (st, m) = w.post_as(Some(&w.token(&w.alpha)), "looks fine to me").await;
    assert_eq!(st, StatusCode::CREATED, "{m}");
    assert_eq!(m["from"], w.alpha.as_str());
    assert_eq!(m["text"], "looks fine to me");
    let log = w.log().await;
    assert_eq!(log.len(), 1);
    assert_eq!(log[0]["from"], w.alpha.as_str());
    let got = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match push.recv().await {
                Ok(Push::ChannelMessage { channel_id, message }) => break (channel_id, message),
                Ok(_) => continue,
                Err(e) => panic!("push closed: {e}"),
            }
        }
    })
    .await
    .expect("the post is pushed");
    assert_eq!(got.0, w.channel);
    assert_eq!(got.1.from, w.alpha);
}

#[tokio::test]
async fn fr_t12_channel_post_by_non_member_is_403() {
    let w = world().await;
    w.mention(&w.outsider);
    let (st, body) = w.post_as(Some(&w.token(&w.outsider)), "let me in").await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "forbidden");
    assert!(w.log().await.is_empty(), "nothing stored");
}

#[tokio::test]
async fn fr_t12_channel_post_without_or_with_bad_token_is_401() {
    let w = world().await;
    w.mention(&w.alpha);
    let (st, _) = w.post_as(None, "hi").await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, body) = w.post_as(Some("not-a-token"), "hi").await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["code"], "unauthorized", "the handler, not the server-token layer, answered: {body}");
    // The server token is not a session token either.
    let (st, _) = w.post_as(Some(&w.server_token), "hi").await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert!(w.log().await.is_empty());
}

#[tokio::test]
async fn fr_t12_channel_post_without_a_recent_mention_is_403() {
    let w = world().await;
    // alpha is a member but nobody mentioned it; beta was mentioned in another channel only.
    let (st, body) = w.post_as(Some(&w.token(&w.alpha)), "unprompted").await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "forbidden");
    assert!(body["error"].as_str().unwrap().contains("mention"), "{body}");
    w.a2a.store().insert_channel_message("some-other-channel", "나", &w.beta, "x").unwrap();
    let (st, _) = w.post_as(Some(&w.token(&w.beta)), "wrong channel").await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert!(w.log().await.is_empty());
}

#[tokio::test]
async fn fr_t12_channel_post_is_rate_limited_429() {
    let w = world().await;
    w.mention(&w.alpha);
    w.mention(&w.beta);
    let (ta, tb) = (w.token(&w.alpha), w.token(&w.beta));
    for i in 0..ember_server::a2a::CHANNEL_POSTS_PER_MINUTE {
        let (st, b) = w.post_as(Some(&ta), &format!("post {i}")).await;
        assert_eq!(st, StatusCode::CREATED, "{b}");
    }
    let (st, body) = w.post_as(Some(&ta), "one too many").await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["code"], "rate_limited");
    // The limit is per session: beta still posts.
    let (st, b) = w.post_as(Some(&tb), "my turn").await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    assert_eq!(w.log().await.len() as u32, ember_server::a2a::CHANNEL_POSTS_PER_MINUTE + 1);
}

#[tokio::test]
async fn fr_t12_channel_post_works_when_the_agents_own_a2a_is_off() {
    let w = world().await;
    w.mention(&w.alpha);
    w.a2a.set_session_enabled(&w.alpha, false).unwrap();
    let (st, b) = w.post_as(Some(&w.token(&w.alpha)), "replying anyway").await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    // Still gated by the mention: with A2A off, a new mention is refused, so no row exists for beta.
    w.a2a.set_session_enabled(&w.beta, false).unwrap();
    let (st, _) = w.post_as(Some(&w.token(&w.beta)), "no mention").await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn fr_t12_agent_message_with_at_name_triggers_no_delivery() {
    let w = world().await;
    w.mention(&w.alpha);
    let before = w.a2a.store().pending_for(&w.beta).unwrap().len();
    let (st, m) = w.post_as(Some(&w.token(&w.alpha)), "@beta please take over").await;
    assert_eq!(st, StatusCode::CREATED, "{m}");
    assert_eq!(m["mentions"], json!([]), "an agent's message parses no mentions: {m}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(w.user_messages(&w.beta), 0, "beta got no turn");
    assert_eq!(w.a2a.store().pending_for(&w.beta).unwrap().len(), before);
}

#[tokio::test]
async fn fr_s9_channels_user_routes_still_need_the_server_token_and_post_needs_the_runtime_token() {
    let w = world().await;
    w.mention(&w.alpha);
    let id = &w.channel;
    for (method, uri) in [
        ("POST", format!("/api/channels/{id}/messages")),
        ("GET", format!("/api/channels/{id}/messages")),
        ("GET", format!("/api/channels/{id}")),
        ("POST", format!("/api/channels/{id}/read")),
        ("GET", "/api/channels".to_string()),
        ("GET", format!("/api/channels/{id}/post")),
        ("POST", format!("/api/channels/{id}/post/extra")),
        ("POST", format!("/api/channels/{id}/x/post")),
    ] {
        let (st, _) = call(&w.guarded, method, &uri, None, Some(json!({"text": "hi"}))).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "{method} {uri} needs the server token");
    }
    // A runtime token does not open a user route.
    let (st, _) = call(&w.guarded, "GET", "/api/channels", Some(&w.token(&w.alpha)), None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    // The post route takes the runtime token and reaches the handler.
    let (st, b) = w.post_as(Some(&w.token(&w.alpha)), "hi").await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    // The Host check still applies to it.
    let req = Request::builder()
        .method("POST")
        .uri(w.path())
        .header("host", "evil.example")
        .header("authorization", format!("Bearer {}", w.token(&w.alpha)))
        .header("content-type", "application/json")
        .body(Body::from(json!({"text":"x"}).to_string()))
        .unwrap();
    assert_eq!(w.guarded.clone().oneshot(req).await.unwrap().status(), StatusCode::FORBIDDEN);
    let _ = &w.app;
}

#[tokio::test]
async fn fr_t12_instructions_name_channel_post() {
    let w = world().await;
    let text = w.a2a.instructions();
    assert!(text.contains("# Channels"), "{text}");
    assert!(text.contains("ember-a2a channel post <channel-id>"), "{text}");
    assert!(text.contains("not shown in the channel"), "{text}");
}
