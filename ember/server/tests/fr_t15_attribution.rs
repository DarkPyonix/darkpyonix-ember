//! FR-T15, FR-T16: a message is attributed to the account that wrote it, in the owner's views
//! (`author_name`, `author_kind`), in the push, and in the JSONL file for a guest. Details are
//! [provisional].

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::channels::guest::guest_router_with_dir;
use ember_server::session::{Push, Sessions};
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

struct Fx {
    store: Arc<Store>,
    sessions: Arc<Sessions>,
    channel: String,
    agent: String,
    owner: Router,
    guest: Router,
}

fn fixture(dir: &Path) -> Fx {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let agent = store.create_session("core", AgentKind::Scripted, "/secret/cwd", None, "alpha").unwrap().id;
    let channel = store.channels(None).unwrap().remove(0).id;
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let owner = ember_server::channels::api::router_with_dir(store.clone(), sessions.clone(), dir);
    let guest = guest_router_with_dir(store.clone(), sessions.clone(), dir);
    Fx { store, sessions, channel, agent, owner, guest }
}

async fn call(app: &Router, method: &str, uri: &str, bearer: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri).header("host", "127.0.0.1:8740");
    if let Some(b) = bearer {
        req = req.header("authorization", format!("Bearer {b}"));
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

async fn join(fx: &Fx, name: &str) -> (String, String) {
    let (_, token) = fx.store.create_invite(&fx.channel, "user", None).unwrap();
    let (st, made) = call(&fx.guest, "POST", "/api/guest/accept", None, Some(json!({"token": token, "name": name}))).await;
    assert_eq!(st, StatusCode::CREATED, "{made}");
    (made["guest_id"].as_str().unwrap().into(), made["credential"].as_str().unwrap().into())
}

fn lines(dir: &Path, channel: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(dir.join("channels").join(format!("{channel}.jsonl"))).unwrap_or_default();
    text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

/// One message of each kind: user, agent, guest, system.
async fn mixed(fx: &Fx) -> String {
    let (gid, cred) = join(fx, "Mina").await;
    let post = format!("/api/channels/{}/messages", fx.channel);
    let (st, _) = call(&fx.owner, "POST", &post, None, Some(json!({"text": "from me"}))).await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, _) = call(&fx.owner, "POST", &post, None, Some(json!({"text": "from agent", "from": fx.agent}))).await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, _) = call(
        &fx.guest,
        "POST",
        &format!("/api/guest/channels/{}/messages", fx.channel),
        Some(&cred),
        Some(json!({"text": "from guest"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    fx.store.post_channel_system_message(&fx.channel, "a note").unwrap();
    gid
}

fn check(msgs: &[Value], agent: &str, gid: &str) {
    let by = |text: &str| msgs.iter().find(|m| m["text"] == text).unwrap_or_else(|| panic!("no {text}: {msgs:?}"));
    assert_eq!((by("from me")["author_name"].as_str(), by("from me")["author_kind"].as_str()), (Some("나"), Some("person")));
    assert_eq!(by("from agent")["author_name"], "alpha");
    assert_eq!(by("from agent")["author_kind"], "agent");
    assert_eq!(by("from agent")["from"], agent);
    assert_eq!(by("from guest")["author_name"], "Mina");
    assert_eq!(by("from guest")["author_kind"], "guest");
    assert_eq!(by("from guest")["from"], format!("guest:{gid}"));
    assert_eq!(by("a note")["author_name"], "system");
    assert_eq!(by("a note")["author_kind"], "system");
    for m in msgs {
        for k in ["id", "seq", "from", "text", "at", "unread", "mentions"] {
            assert!(m.get(k).is_some(), "existing field {k} kept");
        }
    }
}

#[tokio::test]
async fn fr_t15_owner_messages_show_author_name_and_kind() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    let gid = mixed(&fx).await;
    let (st, msgs) = call(&fx.owner, "GET", &format!("/api/channels/{}/messages", fx.channel), None, None).await;
    assert_eq!(st, StatusCode::OK);
    check(msgs.as_array().unwrap(), &fx.agent, &gid);
    let (_, detail) = call(&fx.owner, "GET", &format!("/api/channels/{}", fx.channel), None, None).await;
    check(detail["messages"].as_array().unwrap(), &fx.agent, &gid);
    assert_eq!(detail["last_message"]["author_kind"], "system");
}

#[tokio::test]
async fn fr_t15_the_push_carries_author_name_and_kind() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    let mut rx = fx.sessions.subscribe();
    let (_, cred) = join(&fx, "Mina").await;
    call(
        &fx.guest,
        "POST",
        &format!("/api/guest/channels/{}/messages", fx.channel),
        Some(&cred),
        Some(json!({"text": "pushed"})),
    )
    .await;
    let Push::ChannelMessage { message, .. } = rx.recv().await.unwrap() else { panic!("not a channel_message") };
    let wire = serde_json::to_value(Push::ChannelMessage { channel_id: fx.channel.clone(), message }).unwrap();
    assert_eq!(wire["message"]["author_name"], "Mina");
    assert_eq!(wire["message"]["author_kind"], "guest");
    assert!(wire["message"]["from"].as_str().unwrap().starts_with("guest:"));
}

#[tokio::test]
async fn fr_t16_a_guest_post_is_one_jsonl_line_with_name_and_an_agent_post_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    let gid = mixed(&fx).await;
    let got = lines(dir.path(), &fx.channel);
    assert_eq!(got.len(), 2, "the user line and the guest line only: {got:?}");
    assert_eq!(got[0]["from"], "user");
    assert!(got[0].get("name").is_none(), "only a guest line has name");
    assert_eq!(got[1]["from"], format!("guest:{gid}"));
    assert_eq!(got[1]["name"], "Mina");
    assert_eq!(got[1]["text"], "from guest");
    assert_eq!(got[1]["channel_id"], fx.channel.as_str());
    assert!(got[0]["seq"].as_i64().unwrap() < got[1]["seq"].as_i64().unwrap());
}

#[tokio::test]
async fn fr_t16_a_failed_guest_append_rolls_the_row_back_with_500() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(dir.path());
    let (_, cred) = join(&fx, "Mina").await;
    // A file where the directory should be makes every append fail.
    std::fs::write(dir.path().join("channels"), b"not a directory").unwrap();
    let (st, body) = call(
        &fx.guest,
        "POST",
        &format!("/api/guest/channels/{}/messages", fx.channel),
        Some(&cred),
        Some(json!({"text": "lost"})),
    )
    .await;
    assert_eq!(st, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["code"], "internal");
    let (_, msgs) = call(&fx.owner, "GET", &format!("/api/channels/{}/messages", fx.channel), None, None).await;
    assert!(msgs.as_array().unwrap().is_empty(), "the row is gone: {msgs}");
}
