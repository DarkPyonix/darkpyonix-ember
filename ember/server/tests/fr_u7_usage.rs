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
use ember_server::events::{ApprovalDecision, SessionStatus};
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

/// One scripted turn: the agent asks to run a tool, the test allows it, and the turn ends with
/// 10 input and 5 output tokens. `n` is the turn number of this session (the approval id is
/// `approval-<n>`).
async fn turn(s: &Arc<Sessions>, id: &str, text: &str, n: u32) {
    s.send(id, text).await.unwrap();
    wait_status(s, id, SessionStatus::WaitingForApproval).await;
    s.answer(id, &format!("approval-{n}"), ApprovalDecision::AllowOnce).await.unwrap();
    wait_status(s, id, SessionStatus::Finished).await;
}

async fn wait_status(s: &Arc<Sessions>, id: &str, want: SessionStatus) {
    for _ in 0..2000 {
        if s.store().session(id).unwrap().unwrap().status == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("session never reached {want:?}");
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

    turn(&s, &rec.id, "one", 1).await;
    turn(&s, &rec.id, "two", 2).await;
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
    turn(&s, &a.id, "x", 1).await;
    turn(&s, &b.id, "y", 1).await;
    turn(&s, &b.id, "z", 2).await;

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
    turn(&s, &a.id, "x", 1).await;
    let (_, v) = get(&app, "/api/usage/summary").await;
    assert!(v["cost_usd"].is_null());
    assert!(v["days"][6]["cost_usd"].is_null());
    assert!(v["cost_note"].as_str().unwrap().contains("price table"));
    let (_, v) = get(&app, &format!("/api/sessions/{}/usage", a.id)).await;
    assert!(v["cost_usd"].is_null());
    assert!(v["cost_note"].is_string());
}
