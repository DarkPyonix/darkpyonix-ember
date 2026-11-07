//! Project settings over HTTP (SPEC FR-L26, #384 PR 2): `PATCH /api/projects/{name}` with the
//! scalar fields, and the rename cascade. Written before the code.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::channels::NewChannel;
use ember_server::schedules::{ManualClock, NewSchedule, ScheduleKind, ScheduleTarget, Scheduler};
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

async fn patch(f: &Fixture, name: &str, body: Value) -> (StatusCode, Value) {
    call(&f.app, Method::PATCH, &format!("/api/projects/{name}"), Some(body)).await
}

#[tokio::test]
async fn fr_l26_rename_cascades_to_sessions_channels_schedules_teams_and_computers() {
    let f = fixture();
    let leader = new_session(&f.sessions, "old");
    let other = new_session(&f.sessions, "bystander");
    f.store.assign_computer("old", "local").unwrap();
    let team = f.store.create_team(&leader, "old").unwrap();
    let channel = f
        .store
        .create_channel(NewChannel { project: "old".into(), name: "ops".into(), agents: vec![leader.clone()] })
        .unwrap();
    f.store.post_channel_message(&channel.id, "user", "hello").unwrap();
    let sched = Scheduler::new(f.sessions.clone(), ManualClock::new(1_000_000));
    sched
        .create(
            NewSchedule {
                project: "old".into(),
                agent: None,
                account: None,
                computer: None,
                cwd: None,
                prompt: "check".into(),
                kind: ScheduleKind::Interval { seconds: 3600 },
                target: ScheduleTarget::Continue { session_id: leader.clone() },
                catch_up: false,
                paused: false,
            },
            None,
        )
        .unwrap();
    let mut rx = f.sessions.subscribe();

    let (st, body) = patch(&f, "old", json!({ "name": "  fresh  " })).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["name"], "fresh");
    assert_eq!(body["computers"], json!(["local"]));

    assert!(f.store.project("old").unwrap().is_none());
    assert_eq!(f.store.session(&leader).unwrap().unwrap().project, "fresh");
    assert_eq!(f.store.session(&other).unwrap().unwrap().project, "bystander");
    assert_eq!(f.store.team_row(&team.id).unwrap().unwrap().project, "fresh");
    assert!(f.store.channels(Some("fresh")).unwrap().iter().any(|c| c.id == channel.id && c.name == "ops"));
    assert!(f.store.channels(Some("old")).unwrap().iter().all(|c| c.id != channel.id));
    let msgs = f.store.read_channel_messages(&channel.id, 0).unwrap();
    assert!(msgs.iter().any(|m| m.text == "hello"), "the log follows the channel id");
    assert_eq!(sched.list(Some("fresh")).unwrap().len(), 1);
    assert!(sched.list(Some("old")).unwrap().is_empty());
    assert_eq!(f.store.projects_of_computer("local").unwrap(), ["fresh"]);

    let (_, listed) = call(&f.app, Method::GET, "/api/projects", None).await;
    let names: Vec<&str> = listed.as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["bystander", "fresh"]);

    let mut pushed = false;
    while let Ok(p) = rx.try_recv() {
        pushed |= matches!(p, Push::ProjectUpdated { project } if project.name == "fresh");
    }
    assert!(pushed, "project_updated is pushed with the new name");
}

#[tokio::test]
async fn fr_l26_rename_onto_an_existing_name_is_409_and_changes_nothing() {
    let f = fixture();
    let a = new_session(&f.sessions, "a");
    new_session(&f.sessions, "b");
    let (st, body) = patch(&f, "a", json!({ "name": "b", "repository": "https://example.test/r.git" })).await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(body["code"], "name_taken");
    assert!(body["error"].is_string());
    assert_eq!(f.store.session(&a).unwrap().unwrap().project, "a");
    assert_eq!(f.store.project("a").unwrap().unwrap().repository, None, "the whole patch rolled back");
}

#[tokio::test]
async fn fr_l26_rename_of_the_default_project_is_403() {
    let f = fixture();
    new_session(&f.sessions, DEFAULT);
    let uri_name = DEFAULT.replace(' ', "%20");
    let (st, body) = patch(&f, &uri_name, json!({ "name": "elsewhere" })).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "protected_project");
    assert!(f.store.project(DEFAULT).unwrap().is_some());
    // Its other settings can change.
    let (st, body) = patch(&f, &uri_name, json!({ "character_style": "crew" })).await;
    assert_eq!(st, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn fr_l26_renaming_another_project_onto_the_default_name_is_409() {
    let f = fixture();
    new_session(&f.sessions, DEFAULT);
    new_session(&f.sessions, "x");
    let (st, body) = patch(&f, "x", json!({ "name": DEFAULT })).await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(body["code"], "name_taken");
}

#[tokio::test]
async fn fr_l26_unknown_project_is_404() {
    let f = fixture();
    let (st, body) = patch(&f, "nope", json!({ "repository": "x" })).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
async fn fr_l26_unknown_field_is_400() {
    let f = fixture();
    new_session(&f.sessions, "p");
    let (st, body) = patch(&f, "p", json!({ "colour": "red" })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "unknown_field");
    let (st, body) = patch(&f, "p", json!({ "member_defaults": { "speed": 1 } })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    let (st, body) = call(&f.app, Method::PATCH, "/api/projects/p", Some(json!([1]))).await;
    assert_eq!((st, body["code"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_body")));
}

#[tokio::test]
async fn fr_l26_fields_built_later_are_400_not_yet() {
    let f = fixture();
    new_session(&f.sessions, "p");
    for body in [
        json!({ "leader_session": null }),
        json!({ "leader_session": "s1" }),
        json!({ "archived": true }),
        json!({ "name": "q", "archived": false }),
    ] {
        let (st, resp) = patch(&f, "p", body.clone()).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(resp["code"], "not_yet", "{body}");
    }
    assert!(f.store.project("p").unwrap().is_some(), "nothing was renamed");
}

#[tokio::test]
async fn fr_l26_name_rules_match_the_client() {
    let f = fixture();
    new_session(&f.sessions, "p");
    for bad in ["", "   ", "a/b", "a\nb", &"가".repeat(65)] {
        let (st, body) = patch(&f, "p", json!({ "name": bad })).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad:?}");
        assert_eq!(body["code"], "invalid_field");
    }
    let (st, _) = patch(&f, "p", json!({ "name": "가".repeat(64) })).await;
    assert_eq!(st, StatusCode::OK, "64 characters is allowed");
}

#[tokio::test]
async fn fr_l26_settings_roundtrip_and_clear() {
    let f = fixture();
    new_session(&f.sessions, "p");
    let (st, p) = patch(
        &f,
        "p",
        json!({
            "repository": "https://example.test/r.git",
            "default_branch": "main",
            "member_defaults": { "agent": "codex", "model": "gpt-x", "mode": "plan" },
            "character_style": "pocket",
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{p}");
    assert_eq!(p["repository"], "https://example.test/r.git");
    assert_eq!(p["default_branch"], "main");
    assert_eq!(p["member_defaults"], json!({ "agent": "codex", "model": "gpt-x", "mode": "plan" }));
    assert_eq!(p["character_style"], "pocket");
    assert_eq!(p["archived"], false);

    let (_, got) = call(&f.app, Method::GET, "/api/projects/p", None).await;
    assert_eq!(got, p, "GET returns the same fields");
    let (_, listed) = call(&f.app, Method::GET, "/api/projects", None).await;
    assert_eq!(listed[0], p, "the list returns them too");

    // A partial patch leaves the other fields alone.
    let (_, p2) = patch(&f, "p", json!({ "default_branch": "develop" })).await;
    assert_eq!(p2["repository"], "https://example.test/r.git");
    assert_eq!(p2["default_branch"], "develop");

    // null and empty clear.
    let (_, p3) = patch(&f, "p", json!({ "repository": null, "default_branch": "", "member_defaults": null, "character_style": null })).await;
    assert_eq!((p3["repository"].clone(), p3["default_branch"].clone(), p3["character_style"].clone()), (Value::Null, Value::Null, Value::Null));
    assert_eq!(p3["member_defaults"], json!({}));

    // An empty body changes nothing.
    let (st, p4) = patch(&f, "p", json!({})).await;
    assert_eq!((st, p4["name"].as_str()), (StatusCode::OK, Some("p")));
}

#[tokio::test]
async fn fr_l26_values_are_validated() {
    let f = fixture();
    new_session(&f.sessions, "p");
    for body in [
        json!({ "repository": "has space" }),
        json!({ "repository": "x".repeat(513) }),
        json!({ "default_branch": "a b" }),
        json!({ "default_branch": "-x" }),
        json!({ "default_branch": "/x" }),
        json!({ "default_branch": "x/" }),
        json!({ "default_branch": "x.lock" }),
        json!({ "default_branch": "a..b" }),
        json!({ "member_defaults": { "mode": "yolo" } }),
        json!({ "member_defaults": { "agent": "no-such-agent" } }),
        json!({ "character_style": "neon" }),
        json!({ "repository": 5 }),
    ] {
        let (st, resp) = patch(&f, "p", body.clone()).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(resp["code"], "invalid_field", "{body}");
    }
    let p = f.store.project("p").unwrap().unwrap();
    assert!(p.repository.is_none() && p.default_branch.is_none() && p.character_style.is_none());
    for ok in ["feature/x", "release-1.0", "v1"] {
        let (st, _) = patch(&f, "p", json!({ "default_branch": ok })).await;
        assert_eq!(st, StatusCode::OK, "{ok}");
    }
}

#[tokio::test]
async fn fr_l26_a_new_project_has_empty_settings() {
    let f = fixture();
    let (st, p) = call(&f.app, Method::POST, "/api/projects", Some(json!({ "name": "fresh" }))).await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(
        (&p["repository"], &p["default_branch"], &p["character_style"], &p["archived"], &p["member_defaults"], &p["character_overrides"]),
        (&Value::Null, &Value::Null, &Value::Null, &json!(false), &json!({}), &json!({}))
    );
}

#[tokio::test]
async fn fr_l22_character_overrides_roundtrip_and_validate() {
    let f = fixture();
    new_session(&f.sessions, "p");
    let (st, p) = patch(
        &f,
        "p",
        json!({
            "character_overrides": {
                "ember-리더": {
                    "harness": "claude",
                    "role": "리더",
                    "style": "malang",
                    "state": "run",
                    "silhouette": 3
                }
            }
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{p}");
    assert_eq!(
        p["character_overrides"],
        json!({
            "ember-리더": {
                "harness": "claude",
                "role": "리더",
                "style": "malang",
                "state": "run",
                "silhouette": 3
            }
        })
    );

    // Renaming the project keeps character_overrides.
    let (st, renamed) = patch(&f, "p", json!({ "name": "p2" })).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(renamed["character_overrides"], p["character_overrides"]);

    // Sending null for a member removes that member; {} clears all.
    let (st, cleared_one) = patch(&f, "p2", json!({ "character_overrides": { "ember-리더": null } })).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(cleared_one["character_overrides"], json!({}));

    // Validation rejects bad harness, role, style, state, or silhouette out of range / on non-blob style.
    for bad in [
        json!({ "character_overrides": null }),
        json!({ "character_overrides": { "m": { "harness": "nope" } } }),
        json!({ "character_overrides": { "m": { "role": "없는역할" } } }),
        json!({ "character_overrides": { "m": { "style": "neon" } } }),
        json!({ "character_overrides": { "m": { "state": "sleep" } } }),
        json!({ "character_overrides": { "m": { "style": "malang", "silhouette": 8 } } }),
        json!({ "character_overrides": { "m": { "style": "crew", "silhouette": 1 } } }),
    ] {
        let (st, resp) = patch(&f, "p2", bad.clone()).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}: {resp}");
        assert_eq!(resp["code"], "invalid_field", "{bad}: {resp}");
    }
}
