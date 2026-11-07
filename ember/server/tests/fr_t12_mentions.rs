//! Channel mentions (SPEC FR-T12, FR-T13): parsing, the member gate and the composer preview.
//! Delivery to agents is not covered here (not built).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::channels::mention::parse_mentions;
use ember_server::channels::Member;
use ember_server::session::Sessions;
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

fn agent(id: &str, name: &str) -> Member {
    Member { id: id.into(), name: name.into(), is_agent: true, harness: "scripted".into() }
}

fn people() -> Vec<Member> {
    vec![
        Member { id: "user".into(), name: "me".into(), is_agent: false, harness: String::new() },
        agent("s1", "alpha"),
        agent("s2", "Beta"),
    ]
}

fn ids(text: &str) -> Vec<String> {
    parse_mentions(text, &people()).mentioned.into_iter().map(|m| m.id).collect()
}

#[test]
fn fr_t12_a_plain_mention_resolves_to_the_member() {
    assert_eq!(ids("@alpha please look"), vec!["s1"]);
    assert_eq!(ids("hey @alpha and @Beta"), vec!["s1", "s2"]);
}

#[test]
fn fr_t12_match_ignores_case_and_needs_the_exact_name() {
    assert_eq!(ids("@ALPHA"), vec!["s1"]);
    assert_eq!(ids("@beta"), vec!["s2"]);
    let m = parse_mentions("@alp", &people());
    assert!(m.mentioned.is_empty(), "a prefix does not match");
    assert_eq!(m.unknown, vec!["alp"]);
}

#[test]
fn fr_t12_email_and_double_at_are_not_mentions() {
    let m = parse_mentions("mail me at bob@alpha.com or @@alpha", &people());
    assert!(m.mentioned.is_empty());
    assert!(m.unknown.is_empty());
}

#[test]
fn fr_t12_punctuation_after_the_name_ends_it() {
    assert_eq!(ids("@alpha, go"), vec!["s1"]);
    assert_eq!(ids("(@alpha)"), vec!["s1"]);
    assert_eq!(ids("thanks @alpha."), vec!["s1"]);
    assert_eq!(ids("@alpha!"), vec!["s1"]);
}

#[test]
fn fr_t12_a_repeated_mention_counts_once() {
    let m = parse_mentions("@alpha @ALPHA @alpha @ghost @Ghost", &people());
    assert_eq!(m.mentioned.len(), 1);
    assert_eq!(m.unknown, vec!["ghost"]);
}

#[test]
fn fr_t13_a_person_or_unknown_name_is_unknown() {
    let m = parse_mentions("@me @zed", &people());
    assert!(m.mentioned.is_empty());
    assert_eq!(m.unknown, vec!["me", "zed"]);
}

fn app(store: &Arc<Store>) -> Router {
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    ember_server::channels::api::router(store.clone(), sessions)
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

/// A channel `review` with agent `alpha` only, in a project that also has `beta`.
async fn fixture() -> (Arc<Store>, Router, String, String, String) {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let a = session(&store, "core", "alpha");
    let b = session(&store, "core", "beta");
    let app = app(&store);
    let (_, c) = call(&app, "POST", "/api/channels", Some(json!({"project":"core","name":"review","agents":[a]}))).await;
    let id = c["id"].as_str().unwrap().to_string();
    (store, app, id, a, b)
}

#[tokio::test]
async fn fr_t12_member_mention_is_stored_with_session_ids() {
    let (_s, app, id, a, _) = fixture().await;
    let uri = format!("/api/channels/{id}/messages");
    let (st, m) = call(&app, "POST", &uri, Some(json!({"text":"@alpha look, @ALPHA"}))).await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(m["mentions"], json!([a]));
    let (_, all) = call(&app, "GET", &uri, None).await;
    assert_eq!(all[0]["mentions"], json!([a]), "persisted, not only echoed");
}

#[tokio::test]
async fn fr_t13_a_non_member_mention_is_refused_and_not_stored() {
    let (_s, app, id, _, _) = fixture().await;
    let uri = format!("/api/channels/{id}/messages");
    let (st, e) = call(&app, "POST", &uri, Some(json!({"text":"@alpha and @beta"}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(e["code"], "not_a_member");
    assert_eq!(e["unknown"], json!(["beta"]));
    let (_, all) = call(&app, "GET", &uri, None).await;
    assert!(all.as_array().unwrap().is_empty(), "nothing stored");
}

#[tokio::test]
async fn fr_t12_no_mention_is_stored_as_human_conversation() {
    let (_s, app, id, _, _) = fixture().await;
    let uri = format!("/api/channels/{id}/messages");
    let (st, m) = call(&app, "POST", &uri, Some(json!({"text":"lunch? mail bob@x.com, or @@alpha"}))).await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(m["mentions"], json!([]));
}

#[tokio::test]
async fn fr_t12_preview_says_who_a_message_reaches() {
    let (_s, app, id, a, _) = fixture().await;
    let uri = format!("/api/channels/{id}/mentions/preview");
    let (st, p) = call(&app, "POST", &uri, Some(json!({"text":"@alpha and @beta"}))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(p["reaches"], json!([{"id": a, "name": "alpha"}]));
    assert_eq!(p["unknown"], json!(["beta"]));
    let (_, none) = call(&app, "POST", &uri, Some(json!({"text":"hello"}))).await;
    assert_eq!(none, json!({"reaches": [], "unknown": []}));
    let (_, all) = call(&app, "GET", &format!("/api/channels/{id}/messages"), None).await;
    assert!(all.as_array().unwrap().is_empty(), "preview stores nothing");
    let (st, _) = call(&app, "POST", "/api/channels/nope/mentions/preview", Some(json!({"text":"x"}))).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}
