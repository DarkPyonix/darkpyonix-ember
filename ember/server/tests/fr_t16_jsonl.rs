//! FR-T16: the per-channel JSONL file of human messages. Layout is [provisional].

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::channels::jsonl::JsonlLog;
use ember_server::channels::Message;
use ember_server::session::Sessions;
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

fn app(store: &Arc<Store>, dir: &Path) -> Router {
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    ember_server::channels::api::router_with_dir(store.clone(), sessions, dir)
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

/// A store with projects `core` and `web`, one agent in `core`; returns the store, the agent id
/// and the default channel ids of `core` and `web`.
async fn fixture(dir: &Path) -> (Arc<Store>, String, String, String, Router) {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let agent = store.create_session("core", AgentKind::Scripted, "/tmp/x", None, "alpha").unwrap().id;
    store.create_session("web", AgentKind::Scripted, "/tmp/x", None, "beta").unwrap();
    let app = app(&store, dir);
    let (_, list) = call(&app, "GET", "/api/channels", None).await;
    let id_of = |p: &str| list.as_array().unwrap().iter().find(|c| c["project"] == p).unwrap()["id"].as_str().unwrap().to_string();
    let (core, web) = (id_of("core"), id_of("web"));
    (store, agent, core, web, app)
}

fn lines(dir: &Path, channel: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(dir.join("channels").join(format!("{channel}.jsonl"))).unwrap_or_default();
    text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

#[tokio::test]
async fn fr_t16_a_user_message_is_one_line_with_the_right_fields_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let (_, _, core, _, app) = fixture(dir.path()).await;
    let uri = format!("/api/channels/{core}/messages");
    let (st, m1) = call(&app, "POST", &uri, Some(json!({"text":"hello"}))).await;
    assert_eq!(st, StatusCode::CREATED);
    let (_, m2) = call(&app, "POST", &uri, Some(json!({"text":"second \"quoted\"\nline"}))).await;
    let got = lines(dir.path(), &core);
    assert_eq!(got.len(), 2, "one line per message, a newline inside a text stays escaped");
    assert_eq!(got[0]["seq"], m1["seq"]);
    assert_eq!(got[0]["channel_id"], core.as_str());
    assert_eq!(got[0]["from"], "user");
    assert_eq!(got[0]["text"], "hello");
    assert_eq!(got[0]["at"], m1["at"]);
    assert_eq!(got[1]["seq"], m2["seq"]);
    assert_eq!(got[1]["text"], "second \"quoted\"\nline");
    assert!(got[0]["seq"].as_i64().unwrap() < got[1]["seq"].as_i64().unwrap());
    let keys: Vec<&str> = got[0].as_object().unwrap().keys().map(|k| k.as_str()).collect();
    assert_eq!(keys.len(), 5);
}

#[tokio::test]
async fn fr_t16_an_agent_message_is_not_written() {
    let dir = tempfile::tempdir().unwrap();
    let (store, agent, core, _, app) = fixture(dir.path()).await;
    let uri = format!("/api/channels/{core}/messages");
    let (st, _) = call(&app, "POST", &uri, Some(json!({"text":"done","from":agent}))).await;
    assert_eq!(st, StatusCode::CREATED);
    assert!(lines(dir.path(), &core).is_empty());
    assert!(!dir.path().join("channels").join(format!("{core}.jsonl")).exists());
    // It is still in the SQLite log.
    assert_eq!(store.channel(&core).unwrap().messages.len(), 1);
}

#[tokio::test]
async fn fr_t16_two_channels_write_different_files() {
    let dir = tempfile::tempdir().unwrap();
    let (_, _, core, web, app) = fixture(dir.path()).await;
    call(&app, "POST", &format!("/api/channels/{core}/messages"), Some(json!({"text":"in core"}))).await;
    call(&app, "POST", &format!("/api/channels/{web}/messages"), Some(json!({"text":"in web"}))).await;
    let (c, w) = (lines(dir.path(), &core), lines(dir.path(), &web));
    assert_eq!((c.len(), w.len()), (1, 1));
    assert_eq!(c[0]["text"], "in core");
    assert_eq!(w[0]["text"], "in web");
    assert_eq!(w[0]["channel_id"], web.as_str());
}

fn user_message(seq: i64, text: &str) -> Message {
    Message { id: seq.to_string(), seq, from: "user".into(), text: text.into(), at: 1, unread: 0 }
}

#[test]
fn fr_t16_a_restarted_writer_appends_and_never_truncates() {
    let dir = tempfile::tempdir().unwrap();
    JsonlLog::new(dir.path()).append("chan-1", &user_message(1, "before")).unwrap();
    // A new writer on the same directory, as after a restart of the main server.
    JsonlLog::new(dir.path()).append("chan-1", &user_message(2, "after")).unwrap();
    let got = lines(dir.path(), "chan-1");
    assert_eq!(got.iter().map(|l| l["text"].as_str().unwrap()).collect::<Vec<_>>(), vec!["before", "after"]);
}

#[test]
fn fr_t16_an_unsafe_channel_id_is_not_written_outside_the_dir() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let log = JsonlLog::new(&data);
    for bad in ["../escape", "a/b", "..", "", "a\\b", "/etc/x", "x\0y", "a.b"] {
        assert!(log.append(bad, &user_message(1, "x")).is_err(), "{bad:?} must be refused");
    }
    assert!(!root.path().join("escape.jsonl").exists());
    let mut all = Vec::new();
    for e in walk(root.path()) {
        all.push(e);
    }
    assert!(all.iter().all(|p| !p.to_string_lossy().ends_with(".jsonl")), "nothing written: {all:?}");
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[tokio::test]
async fn fr_t16_a_failed_append_is_500_and_leaves_no_row() {
    let dir = tempfile::tempdir().unwrap();
    // `channels` is a file, so the directory cannot be created.
    std::fs::write(dir.path().join("channels"), b"in the way").unwrap();
    let (store, _, core, _, app) = fixture(dir.path()).await;
    let (st, e) = call(&app, "POST", &format!("/api/channels/{core}/messages"), Some(json!({"text":"lost"}))).await;
    assert_eq!(st, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(e["code"], "internal");
    assert!(store.channel(&core).unwrap().messages.is_empty(), "the SQLite row is rolled back");
}
