//! Projects and computer assignment (FR-L4), session metadata (FR-L9), full-text search
//! (FR-S4), export (FR-L9) and the fork stub (FR-S5), over HTTP.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::computers::{self, Computers, Registry, LOCAL};
use ember_server::events::AgentEvent;
use ember_server::session::{NewSession, Push, Sessions};
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tokio::sync::broadcast::Receiver;
use tower::ServiceExt;

struct Fixture {
    sessions: Arc<Sessions>,
    computers: Arc<Computers>,
    app: axum::Router,
}

fn fixture() -> Fixture {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    let sessions = Sessions::new(store.clone(), adapters);
    let computers = Computers::new(Registry::new(store), computers::default_connector());
    let app = ember_server::api::router(sessions.clone())
        .merge(computers::api::router(computers.clone(), sessions.clone()));
    Fixture { sessions, computers, app }
}

fn new_session(s: &Sessions, project: &str) -> String {
    s.create(NewSession {
        project: project.into(),
        agent: AgentKind::Scripted,
        cwd: std::env::temp_dir(),
        model: None,
        title: "t".into(),
    })
    .unwrap()
    .id
}

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

/// Pushes received so far, as JSON (what a client sees on the socket).
fn drain(rx: &mut Receiver<Push>) -> Vec<Value> {
    let mut out = Vec::new();
    while let Ok(p) = rx.try_recv() {
        out.push(serde_json::to_value(&p).unwrap());
    }
    out
}

#[tokio::test]
async fn fr_l4_assign_and_unassign_computers_is_persisted_and_pushed() {
    let f = fixture();
    new_session(&f.sessions, "alpha");
    let studio = f.computers.registry().insert("studio", "http://studio:8741", "tok").unwrap();
    let mut rx = f.sessions.subscribe();

    let (st, list) = call(&f.app, Method::GET, "/api/v1/projects", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list, json!([{ "name": "alpha", "created_at": list[0]["created_at"], "computers": [] }]));

    // A project with no sessions yet; names are percent-encoded path segments.
    let (st, p) = call(&f.app, Method::POST, "/api/v1/projects", Some(json!({ "name": "web/app" }))).await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(p["name"], "web/app");
    let (st, _) = call(&f.app, Method::POST, "/api/v1/projects", Some(json!({ "name": "web/app" }))).await;
    assert_eq!(st, StatusCode::OK, "creating an existing project is not an error");
    let (st, _) = call(&f.app, Method::POST, "/api/v1/projects", Some(json!({ "name": " " }))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // Many-to-many: studio on both projects, local on alpha too.
    let uri = |p: &str, c: &str| format!("/api/v1/projects/{p}/computers/{c}");
    let (st, p) = call(&f.app, Method::PUT, &uri("alpha", &studio.id), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(p["computers"], json!([studio.id]));
    call(&f.app, Method::PUT, &uri("web%2Fapp", &studio.id), None).await;
    let (_, p) = call(&f.app, Method::PUT, &uri("alpha", LOCAL), None).await;
    let mut want = vec![studio.id.clone(), LOCAL.to_string()];
    want.sort();
    assert_eq!(p["computers"], json!(want));
    let (st, p) = call(&f.app, Method::GET, "/api/v1/projects/web%2Fapp", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(p["computers"], json!([studio.id]));

    // Unknown project or computer: 404.
    assert_eq!(call(&f.app, Method::PUT, &uri("nope", LOCAL), None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call(&f.app, Method::PUT, &uri("alpha", "nope"), None).await.0, StatusCode::NOT_FOUND);

    let (st, p) = call(&f.app, Method::DELETE, &uri("alpha", LOCAL), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(p["computers"], json!([studio.id]));

    // Every change was pushed with the project's full assignment.
    let pushes: Vec<Value> = drain(&mut rx).into_iter().filter(|p| p["type"] == "project_updated").collect();
    assert_eq!(pushes.len(), 5, "{pushes:?}");
    assert!(pushes.iter().all(|p| p["v"] == 1));
    assert_eq!(pushes.last().unwrap()["project"]["computers"], json!([studio.id]));

    // Removing the computer unassigns it everywhere and pushes the affected projects.
    let (st, _) = call(&f.app, Method::DELETE, &format!("/api/v1/computers/{}", studio.id), None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (_, list) = call(&f.app, Method::GET, "/api/v1/projects", None).await;
    assert!(list.as_array().unwrap().iter().all(|p| p["computers"] == json!([])), "{list}");
    let names: Vec<Value> = drain(&mut rx).into_iter().map(|p| p["project"]["name"].clone()).collect();
    assert_eq!(names, vec![json!("alpha"), json!("web/app")]);
}

#[tokio::test]
async fn fr_l9_patch_renames_pins_archives_and_pushes() {
    let f = fixture();
    let id = new_session(&f.sessions, "alpha");
    let mut rx = f.sessions.subscribe();

    let (st, rec) = call(&f.app, Method::GET, "/api/v1/sessions", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!((rec[0]["pinned"].clone(), rec[0]["archived"].clone()), (json!(false), json!(false)));
    assert!(rec[0].get("account_id").is_some(), "the record carries its account");

    let path = format!("/api/v1/sessions/{id}");
    let (st, rec) = call(&f.app, Method::PATCH, &path, Some(json!({ "title": " Deploy fix ", "pinned": true }))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!((rec["title"].clone(), rec["pinned"].clone(), rec["archived"].clone()), (json!("Deploy fix"), json!(true), json!(false)));
    let (_, rec) = call(&f.app, Method::PATCH, &path, Some(json!({ "archived": true, "pinned": false }))).await;
    assert_eq!((rec["title"].clone(), rec["pinned"].clone(), rec["archived"].clone()), (json!("Deploy fix"), json!(false), json!(true)));

    assert_eq!(call(&f.app, Method::PATCH, &path, Some(json!({ "title": "" }))).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        call(&f.app, Method::PATCH, "/api/v1/sessions/nope", Some(json!({ "pinned": true }))).await.0,
        StatusCode::NOT_FOUND
    );

    let (_, detail) = call(&f.app, Method::GET, &path, None).await;
    assert_eq!(detail["session"]["archived"], true);
    assert_eq!(detail["can_fork"], false);

    let pushes = drain(&mut rx);
    assert_eq!(pushes.len(), 2);
    assert_eq!(pushes[0]["type"], "session_updated");
    assert_eq!(pushes[1]["session"]["archived"], true);
}

#[tokio::test]
async fn fr_s4_search_finds_messages_in_every_session_including_korean() {
    let f = fixture();
    let a = new_session(&f.sessions, "alpha");
    let b = new_session(&f.sessions, "beta");
    let s = &f.sessions;
    s.record_event(&a, &AgentEvent::UserMessage { text: "배포 서버에서 오류가 발생했습니다".into() }).unwrap();
    s.record_event(&a, &AgentEvent::AssistantDelta { text: "로그를".into() }).unwrap();
    s.record_event(&a, &AgentEvent::AssistantMessage { text: "로그를 확인해 보겠습니다. The nginx config is wrong.".into() }).unwrap();
    s.record_event(&b, &AgentEvent::UserMessage { text: "nginx 설정 파일을 고쳐 주세요".into() }).unwrap();
    s.record_event(&b, &AgentEvent::Error { message: "오류 in a non-message event".into() }).unwrap();

    let get = |q: &str| format!("/api/v1/search?q={}", urlencode(q));

    let (st, hits) = call(&f.app, Method::GET, &get("오류"), None).await;
    assert_eq!(st, StatusCode::OK);
    let hits = hits.as_array().unwrap();
    assert_eq!(hits.len(), 1, "only messages are indexed: {hits:?}");
    assert_eq!(hits[0]["session_id"], a.as_str());
    assert_eq!(hits[0]["seq"], 1, "the hit links to the matching message");
    assert_eq!(hits[0]["kind"], "user_message");
    assert_eq!(hits[0]["project"], "alpha");
    assert!(hits[0]["snippet"].as_str().unwrap().contains("\u{ab}오류가\u{bb}"), "{}", hits[0]["snippet"]);

    // Across sessions, case-insensitive, prefix match on each word.
    let (_, hits) = call(&f.app, Method::GET, &get("NGINX"), None).await;
    let mut found: Vec<(String, i64)> = hits
        .as_array()
        .unwrap()
        .iter()
        .map(|h| (h["session_id"].as_str().unwrap().to_string(), h["seq"].as_i64().unwrap()))
        .collect();
    found.sort();
    let mut want = vec![(a.clone(), 3), (b.clone(), 1)];
    want.sort();
    assert_eq!(found, want, "the delta (seq 2) is not indexed");

    // Every word must match: Korean and English together.
    let (_, hits) = call(&f.app, Method::GET, &get("nginx 설정"), None).await;
    assert_eq!(hits.as_array().unwrap().len(), 1);
    assert_eq!(hits[0]["session_id"], b.as_str());

    // Archived sessions are still found, and say so.
    call(&f.app, Method::PATCH, &format!("/api/v1/sessions/{b}"), Some(json!({ "archived": true }))).await;
    let (_, hits) = call(&f.app, Method::GET, &get("설정"), None).await;
    assert_eq!(hits[0]["archived"], true);

    // Query syntax is literal; empty and limit behave.
    assert_eq!(call(&f.app, Method::GET, &get("\"nginx"), None).await.0, StatusCode::OK);
    assert_eq!(call(&f.app, Method::GET, &get("NEAR( OR"), None).await.0, StatusCode::OK);
    assert_eq!(call(&f.app, Method::GET, "/api/v1/search?q=", None).await.1, json!([]));
    let (_, hits) = call(&f.app, Method::GET, &format!("{}&limit=1", get("nginx")), None).await;
    assert_eq!(hits.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn fr_l9_export_is_a_self_contained_transcript() {
    let f = fixture();
    let id = new_session(&f.sessions, "alpha");
    f.sessions.record_event(&id, &AgentEvent::UserMessage { text: "안녕".into() }).unwrap();
    f.sessions.record_event(&id, &AgentEvent::AssistantMessage { text: "hello".into() }).unwrap();

    let req = Request::builder().uri(format!("/api/v1/sessions/{id}/export")).body(Body::empty()).unwrap();
    let resp = f.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let cd = resp.headers()["content-disposition"].to_str().unwrap().to_string();
    assert!(cd.starts_with("attachment;") && cd.contains(&id), "{cd}");
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["format"], "ember-transcript");
    assert_eq!(v["version"], 1);
    assert_eq!(v["session"]["id"], id.as_str());
    assert_eq!(v["session"]["project"], "alpha");
    let events = v["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["seq"], 1);
    assert_eq!(events[0]["event"], json!({ "kind": "user_message", "text": "안녕" }));
    assert_eq!(events[1]["event"]["text"], "hello");

    assert_eq!(call(&f.app, Method::GET, "/api/v1/sessions/nope/export", None).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn fr_s5_fork_is_not_implemented_with_a_reason() {
    let f = fixture();
    let id = new_session(&f.sessions, "alpha");
    let (st, body) = call(&f.app, Method::POST, &format!("/api/v1/sessions/{id}/fork"), Some(json!({}))).await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED);
    assert!(body["reason"].as_str().unwrap().contains("not supported"), "{body}");
    assert_eq!(call(&f.app, Method::POST, "/api/v1/sessions/nope/fork", Some(json!({}))).await.0, StatusCode::NOT_FOUND);
}

/// Percent-encode a query value (no extra dev-dependency).
fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}
