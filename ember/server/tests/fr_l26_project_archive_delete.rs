//! Project archive and delete over HTTP (SPEC FR-L26, #384 PR 4 and 5): `PATCH
//! /api/projects/{name}` with `archived`, and `DELETE /api/projects/{name}`. Written before the
//! code.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::channels::NewChannel;
use ember_server::session::{NewSession, Push, Sessions};
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const DEFAULT: &str = "기본 프로젝트";

struct Fixture {
    store: Arc<Store>,
    sessions: Arc<Sessions>,
    app: axum::Router,
}

fn fixture() -> Fixture {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    let sessions = Sessions::new(store.clone(), adapters);
    let app = ember_server::api::router(sessions.clone())
        .merge(ember_server::channels::api::router(store.clone(), sessions.clone()));
    Fixture { store, sessions, app }
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
    let value = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap_or(Value::Null) };
    (status, value)
}

fn uri(name: &str) -> String {
    format!("/api/projects/{}", urlencoding(name))
}

/// Percent-encode a name for the path (Hangul in 기본 프로젝트, spaces).
fn urlencoding(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[tokio::test]
async fn fr_l26_archive_and_unarchive_keep_sessions_and_push() {
    let f = fixture();
    let s = new_session(&f.sessions, "p");
    let mut rx = f.sessions.subscribe();

    let (st, body) = call(&f.app, Method::PATCH, &uri("p"), Some(json!({ "archived": true }))).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["archived"], true);
    assert!(f.store.project("p").unwrap().unwrap().archived);
    assert_eq!(f.store.session(&s).unwrap().unwrap().project, "p", "sessions and records stay");
    let (_, listed) = call(&f.app, Method::GET, "/api/projects", None).await;
    assert_eq!(listed[0]["archived"], true, "the list still returns an archived project; the client hides it");

    let mut pushed = false;
    while let Ok(p) = rx.try_recv() {
        pushed |= matches!(p, Push::ProjectUpdated { project } if project.name == "p" && project.archived);
    }
    assert!(pushed, "project_updated carries archived");

    let (st, body) = call(&f.app, Method::PATCH, &uri("p"), Some(json!({ "archived": false }))).await;
    assert_eq!((st, &body["archived"]), (StatusCode::OK, &json!(false)));
}

#[tokio::test]
async fn fr_l26_archive_takes_only_a_bool() {
    let f = fixture();
    new_session(&f.sessions, "p");
    for bad in [json!({ "archived": null }), json!({ "archived": "yes" }), json!({ "archived": 1 })] {
        let (st, body) = call(&f.app, Method::PATCH, &uri("p"), Some(bad.clone())).await;
        assert_eq!((st, body["code"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_field")), "{bad}");
    }
    assert!(!f.store.project("p").unwrap().unwrap().archived);
}

#[tokio::test]
async fn fr_l26_archive_rolls_back_with_a_bad_field() {
    let f = fixture();
    new_session(&f.sessions, "p");
    let (st, _) =
        call(&f.app, Method::PATCH, &uri("p"), Some(json!({ "archived": true, "character_style": "nope" }))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(!f.store.project("p").unwrap().unwrap().archived);
}

#[tokio::test]
async fn fr_l26_the_default_project_cannot_be_archived_or_deleted() {
    let f = fixture();
    f.store.ensure_project(DEFAULT).unwrap();
    let (st, body) = call(&f.app, Method::PATCH, &uri(DEFAULT), Some(json!({ "archived": true }))).await;
    assert_eq!((st, body["code"].as_str()), (StatusCode::FORBIDDEN, Some("protected_project")));
    let (st, _) = call(&f.app, Method::PATCH, &uri(DEFAULT), Some(json!({ "archived": false }))).await;
    assert_eq!(st, StatusCode::OK, "unarchiving is a no-op, not a refusal");
    let (st, body) = call(&f.app, Method::DELETE, &uri(DEFAULT), None).await;
    assert_eq!((st, body["code"].as_str()), (StatusCode::FORBIDDEN, Some("protected_project")));
    assert!(f.store.project(DEFAULT).unwrap().is_some());
}

#[tokio::test]
async fn fr_l26_delete_an_empty_project_removes_it_and_pushes() {
    let f = fixture();
    f.store.ensure_project("empty").unwrap();
    f.store.assign_computer("empty", "local").unwrap();
    f.store.ensure_project("other").unwrap();
    f.store.assign_computer("other", "local").unwrap();
    let mut rx = f.sessions.subscribe();

    let (st, body) = call(&f.app, Method::DELETE, &uri("empty"), None).await;
    assert_eq!(st, StatusCode::NO_CONTENT, "{body}");
    assert!(f.store.project("empty").unwrap().is_none());
    assert_eq!(f.store.projects_of_computer("local").unwrap(), ["other"], "its assignments go with it");

    let mut pushed = false;
    while let Ok(p) = rx.try_recv() {
        pushed |= matches!(p, Push::ProjectDeleted { ref name } if name == "empty");
    }
    assert!(pushed, "project_deleted is pushed");

    let (st, body) = call(&f.app, Method::DELETE, &uri("empty"), None).await;
    assert_eq!((st, body["code"].as_str()), (StatusCode::NOT_FOUND, Some("not_found")));
}

#[tokio::test]
async fn fr_l26_delete_refuses_a_project_with_sessions_or_channels() {
    let f = fixture();
    let s = new_session(&f.sessions, "busy");
    let (st, body) = call(&f.app, Method::DELETE, &uri("busy"), None).await;
    assert_eq!((st, body["code"].as_str()), (StatusCode::CONFLICT, Some("project_not_empty")), "{body}");
    assert_eq!(body["sessions"], 1);
    assert!(f.store.project("busy").unwrap().is_some());
    assert!(f.store.session(&s).unwrap().is_some(), "nothing was deleted");

    f.store.ensure_project("chan").unwrap();
    f.store.create_channel(NewChannel { project: "chan".into(), name: "ops".into(), agents: vec![] }).unwrap();
    let (st, body) = call(&f.app, Method::DELETE, &uri("chan"), None).await;
    assert_eq!((st, body["code"].as_str()), (StatusCode::CONFLICT, Some("project_not_empty")));
    assert_eq!(body["channels"], 1);
    assert!(f.store.project("chan").unwrap().is_some());
}
