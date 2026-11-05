//! Channels server API (SPEC FR-T10, FR-T16, FR-L12): list, create, post, read.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::session::{Push, Sessions};
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

fn sessions_of(store: &Arc<Store>) -> Arc<Sessions> {
    Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>])
}

fn app(store: &Arc<Store>) -> Router {
    ember_server::channels::api::router(store.clone(), sessions_of(store))
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

fn session(store: &Store, project: &str, title: &str) -> String {
    store.create_session(project, AgentKind::Scripted, "/tmp/x", None, title).unwrap().id
}

fn fixture() -> (Arc<Store>, String, String, String) {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let a = session(&store, "core", "alpha");
    let b = session(&store, "core", "beta");
    let other = session(&store, "web", "gamma");
    (store, a, b, other)
}

#[tokio::test]
async fn fr_t10_every_project_has_a_default_channel_with_all_its_agents() {
    let (store, a, b, other) = fixture();
    let (st, list) = call(&app(&store), "GET", "/api/channels", None).await;
    assert_eq!(st, StatusCode::OK);
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 2);
    let core = list.iter().find(|c| c["project"] == "core").unwrap();
    assert_eq!(core["default"], true);
    let ids: Vec<&str> = core["members"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&a.as_str()) && ids.contains(&b.as_str()) && ids.contains(&"user"));
    assert!(!ids.contains(&other.as_str()));
    // A list carries a last message but not the log.
    assert!(core["messages"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn fr_t10_created_channel_keeps_its_chosen_agents_and_none_other() {
    let (store, a, b, _) = fixture();
    let app = app(&store);
    let (st, c) = call(&app, "POST", "/api/channels", Some(json!({"project":"core","name":"review","agents":[a]}))).await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(c["default"], false);
    assert_eq!(c["name"], "review");
    let agents: Vec<&str> =
        c["members"].as_array().unwrap().iter().filter(|m| m["is_agent"] == true).map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(agents, vec![a.as_str()]);
    assert!(!agents.contains(&b.as_str()));
    let (_, list) = call(&app, "GET", "/api/channels?project=core", None).await;
    assert_eq!(list.as_array().unwrap().len(), 2, "default plus the new one");
}

#[tokio::test]
async fn fr_t10_a_channel_cannot_take_an_agent_of_another_project() {
    let (store, _, _, other) = fixture();
    let (st, e) = call(&app(&store), "POST", "/api/channels", Some(json!({"project":"core","name":"x","agents":[other]}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(e["code"], "bad_request");
    let (st, _) = call(&app(&store), "POST", "/api/channels", Some(json!({"project":"nope","name":"x"}))).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call(&app(&store), "POST", "/api/channels", Some(json!({"project":"core","name":"  "}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn fr_t10_post_and_read_messages_in_order_with_after() {
    let (store, a, _, _) = fixture();
    let app = app(&store);
    let (_, list) = call(&app, "GET", "/api/channels?project=core", None).await;
    let id = list[0]["id"].as_str().unwrap().to_string();
    let uri = format!("/api/channels/{id}/messages");
    let (st, m1) = call(&app, "POST", &uri, Some(json!({"text":"hello"}))).await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(m1["from"], "user");
    let (_, m2) = call(&app, "POST", &uri, Some(json!({"text":"done","from":a}))).await;
    assert_eq!(m2["from"], a.as_str());
    let (_, all) = call(&app, "GET", &uri, None).await;
    let texts: Vec<&str> = all.as_array().unwrap().iter().map(|m| m["text"].as_str().unwrap()).collect();
    assert_eq!(texts, vec!["hello", "done"]);
    let after = m1["seq"].as_i64().unwrap();
    let (_, rest) = call(&app, "GET", &format!("{uri}?after={after}"), None).await;
    assert_eq!(rest.as_array().unwrap().len(), 1);
    assert_eq!(rest[0]["text"], "done");
    // The detail has the log and the list has the last message.
    let (_, d) = call(&app, "GET", &format!("/api/channels/{id}"), None).await;
    assert_eq!(d["messages"].as_array().unwrap().len(), 2);
    assert_eq!(d["last_message"]["text"], "done");
    assert!(d["messages"][0]["at"].as_i64().unwrap() > 1_000_000_000, "unix seconds");
}

#[tokio::test]
async fn fr_t10_only_members_post_and_empty_text_is_refused() {
    let (store, a, b, other) = fixture();
    let app = app(&store);
    let (_, c) = call(&app, "POST", "/api/channels", Some(json!({"project":"core","name":"r","agents":[a]}))).await;
    let uri = format!("/api/channels/{}/messages", c["id"].as_str().unwrap());
    let (st, e) = call(&app, "POST", &uri, Some(json!({"text":"hi","from":b}))).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(e["code"], "forbidden");
    let (st, _) = call(&app, "POST", &uri, Some(json!({"text":"hi","from":other}))).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(&app, "POST", &uri, Some(json!({"text":" "}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call(&app, "GET", "/api/channels/nope/messages", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call(&app, "POST", "/api/channels/nope/messages", Some(json!({"text":"x"}))).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn fr_t16_the_log_survives_a_restart() {
    let dir = std::env::temp_dir().join(format!("ember-fr-t16-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ember.db");
    let id = {
        let store = Arc::new(Store::open(&path).unwrap());
        session(&store, "core", "alpha");
        let app = app(&store);
        let (_, list) = call(&app, "GET", "/api/channels", None).await;
        let id = list[0]["id"].as_str().unwrap().to_string();
        call(&app, "POST", &format!("/api/channels/{id}/messages"), Some(json!({"text":"kept"}))).await;
        id
    };
    let store = Arc::new(Store::open(&path).unwrap());
    let (st, msgs) = call(&app(&store), "GET", &format!("/api/channels/{id}/messages"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(msgs[0]["text"], "kept");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn fr_t10_a_posted_message_is_pushed_to_websocket_clients() {
    let (store, a, _, _) = fixture();
    let sessions = sessions_of(&store);
    let app = ember_server::channels::api::router(store.clone(), sessions.clone());
    let mut rx = sessions.subscribe();
    let (_, list) = call(&app, "GET", "/api/channels?project=core", None).await;
    let id = list[0]["id"].as_str().unwrap().to_string();
    let (st, m) = call(&app, "POST", &format!("/api/channels/{id}/messages"), Some(json!({"text":"hi","from":a}))).await;
    assert_eq!(st, StatusCode::CREATED);
    let Push::ChannelMessage { channel_id, message } = rx.recv().await.unwrap() else { panic!("not a channel_message") };
    assert_eq!(channel_id, id);
    assert_eq!((message.text.as_str(), message.from.as_str()), ("hi", a.as_str()));
    // The wire form is what /api/push sends.
    let wire = serde_json::to_value(Push::ChannelMessage { channel_id: id.clone(), message }).unwrap();
    assert_eq!(wire["type"], "channel_message");
    assert_eq!(wire["message"]["seq"], m["seq"]);
}

#[tokio::test]
async fn fr_t10_a_refused_message_is_not_pushed() {
    let (store, _, b, _) = fixture();
    let sessions = sessions_of(&store);
    let app = ember_server::channels::api::router(store.clone(), sessions.clone());
    let mut rx = sessions.subscribe();
    let (_, c) = call(&app, "POST", "/api/channels", Some(json!({"project":"core","name":"r","agents":[]}))).await;
    let uri = format!("/api/channels/{}/messages", c["id"].as_str().unwrap());
    let (st, _) = call(&app, "POST", &uri, Some(json!({"text":"hi","from":b}))).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn fr_t10_router_takes_sessions_and_store() {
    let (store, _, _, _) = fixture();
    let (st, _) = call(&app(&store), "GET", "/api/channels", None).await;
    assert_eq!(st, StatusCode::OK);
}
