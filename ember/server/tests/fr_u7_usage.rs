//! FR-U7: tokens per session and per day, served by the API. Cost is null with a reason.
//!
//! Sessions run the scripted agent, which reports 10 input and 5 output tokens per turn. The
//! day bucketing across midnight is a unit test in `src/usage.rs`.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::events::SessionStatus;
use ember_server::session::{NewSession, Sessions};
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

fn sessions() -> Arc<Sessions> {
    let store = Arc::new(Store::open_in_memory().unwrap());
    Sessions::new(store, vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>])
}

fn new_session(project: &str) -> NewSession {
    NewSession {
        project: project.into(),
        agent: AgentKind::Scripted,
        cwd: std::env::temp_dir(),
        model: None,
        title: "t".into(),
    }
}

async fn turn(s: &Arc<Sessions>, id: &str, text: &str) {
    let before = s.store().session(id).unwrap().unwrap().last_seq;
    s.send(id, text).await.unwrap();
    for _ in 0..400 {
        let r = s.store().session(id).unwrap().unwrap();
        if r.status == SessionStatus::Finished && r.last_seq > before {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("turn never finished");
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn fr_u7_session_usage_sums_the_reported_turns() {
    let s = sessions();
    let app = ember_server::api::router(s.clone());
    let rec = s.create(new_session("acme")).unwrap();
    // Before any turn the harness has reported nothing: unknown, not zero used.
    let (st, v) = get(&app, &format!("/api/sessions/{}/usage", rec.id)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["reported"], false);

    turn(&s, &rec.id, "one").await;
    turn(&s, &rec.id, "two").await;
    let (_, v) = get(&app, &format!("/api/sessions/{}/usage", rec.id)).await;
    assert_eq!(v["session_id"], rec.id.as_str());
    assert_eq!(v["reported"], true);
    assert_eq!(v["input_tokens"], 20);
    assert_eq!(v["output_tokens"], 10);
    assert_eq!(v["total_tokens"], 30);
    assert_eq!(v["turns"], 2);

    // The session JSON carries the same object.
    let (_, v) = get(&app, &format!("/api/sessions/{}", rec.id)).await;
    assert_eq!(v["usage"]["total_tokens"], 30);

    let (st, _) = get(&app, "/api/sessions/nope/usage").await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn fr_u7_day_summary_totals_sessions_and_agents() {
    let s = sessions();
    let app = ember_server::api::router(s.clone());
    let a = s.create(new_session("acme")).unwrap();
    let b = s.create(new_session("other")).unwrap();
    turn(&s, &a.id, "x").await;
    turn(&s, &b.id, "y").await;
    turn(&s, &b.id, "z").await;

    let (st, v) = get(&app, "/api/usage/summary?days=3").await;
    assert_eq!(st, StatusCode::OK);
    let days = v["days"].as_array().unwrap();
    assert_eq!(days.len(), 3, "every day of the range is present");
    assert_eq!(days[0]["total_tokens"], 0);
    let today = days.last().unwrap();
    assert_eq!(today["input_tokens"], 30);
    assert_eq!(today["output_tokens"], 15);
    assert_eq!(today["turns"], 3);
    assert_eq!(today["sessions"], 2);
    assert_eq!(today["by_agent"][0]["agent"], "scripted");
    assert_eq!(today["by_agent"][0]["total_tokens"], 45);
    assert_eq!(v["total_tokens"], 45);
    assert_eq!(v["by_agent"][0]["agent"], "scripted");

    // One named day, and a project filter.
    let day = today["day"].as_str().unwrap().to_string();
    let (_, one) = get(&app, &format!("/api/usage/summary?day={day}&project=acme")).await;
    assert_eq!(one["days"].as_array().unwrap().len(), 1);
    assert_eq!(one["total_tokens"], 15);

    // A bad day is refused, a day in the past is empty.
    let (st, _) = get(&app, "/api/usage/summary?day=2026-02-31").await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, v) = get(&app, "/api/usage/summary?day=2020-01-01").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["total_tokens"], 0);
}

#[tokio::test]
async fn fr_u7_cost_is_null_with_a_reason_not_an_estimate() {
    let s = sessions();
    let app = ember_server::api::router(s.clone());
    let a = s.create(new_session("acme")).unwrap();
    turn(&s, &a.id, "x").await;
    let (_, v) = get(&app, "/api/usage/summary").await;
    assert!(v["cost_usd"].is_null());
    assert!(v["days"][6]["cost_usd"].is_null());
    assert!(v["cost_note"].as_str().unwrap().contains("price table"));
    let (_, v) = get(&app, &format!("/api/sessions/{}/usage", a.id)).await;
    assert!(v["cost_usd"].is_null());
    assert!(v["cost_note"].is_string());
}
