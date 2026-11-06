//! Channel feed (SPEC FR-T11): member agents' approval requests and reports, answerable there.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::events::SessionStatus;
use ember_server::session::Sessions;
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

struct Fx {
    store: Arc<Store>,
    sessions: Arc<Sessions>,
    app: Router,
}

fn fx() -> Fx {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let app = ember_server::channels::api::router(store.clone(), sessions.clone());
    Fx { store, sessions, app }
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

async fn wait_status(store: &Store, id: &str, want: SessionStatus) {
    for _ in 0..200 {
        if store.session(id).unwrap().unwrap().status == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("session {id} never reached {want:?}");
}

#[tokio::test]
async fn fr_t11_pending_approval_of_a_member_appears_naming_the_agent_and_its_session() {
    let f = fx();
    let a = session(&f.store, "core", "alpha");
    f.sessions.send(&a, "hello").await.unwrap();
    wait_status(&f.store, &a, SessionStatus::WaitingForApproval).await;
    let (st, feed) = call(&f.app, "GET", "/api/channels/default-core/feed", None).await;
    assert_eq!(st, StatusCode::OK);
    let items = feed.as_array().unwrap();
    assert_eq!(items.len(), 1, "{feed}");
    let it = &items[0];
    assert_eq!(it["kind"], "approval");
    assert_eq!(it["approval_id"], "approval-1");
    assert_eq!(it["tool"], "Bash");
    assert_eq!(it["session_id"], a.as_str());
    assert_eq!(it["agent"]["id"], a.as_str());
    assert_eq!(it["agent"]["name"], "alpha");
    assert_eq!(it["link"], format!("/sessions/{a}"));
}

#[tokio::test]
async fn fr_t11_answering_from_the_feed_clears_it_and_finishes_the_session() {
    let f = fx();
    let a = session(&f.store, "core", "alpha");
    f.sessions.send(&a, "hello").await.unwrap();
    wait_status(&f.store, &a, SessionStatus::WaitingForApproval).await;
    let (st, _) = call(
        &f.app,
        "POST",
        "/api/channels/default-core/feed/approval-1/answer",
        Some(json!({"decision":"allow_once"})),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    wait_status(&f.store, &a, SessionStatus::Finished).await;
    let (_, feed) = call(&f.app, "GET", "/api/channels/default-core/feed", None).await;
    let items = feed.as_array().unwrap();
    assert!(items.iter().all(|i| i["kind"] != "approval"), "{feed}");
    // The finished turn is reported, with the agent's last reply.
    let report = items.iter().find(|i| i["kind"] == "report").expect("a completion report");
    assert_eq!(report["outcome"], "completed");
    assert_eq!(report["summary"], "echo: hello");
    assert_eq!(report["agent"]["id"], a.as_str());
    // Answering again: nothing is pending.
    let (st, body) = call(
        &f.app,
        "POST",
        "/api/channels/default-core/feed/approval-1/answer",
        Some(json!({"decision":"allow_once"})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
async fn fr_t11_an_agent_outside_the_channel_adds_nothing_and_cannot_be_answered_through_it() {
    let f = fx();
    let a = session(&f.store, "core", "alpha");
    let b = session(&f.store, "core", "beta");
    let (st, ch) = call(&f.app, "POST", "/api/channels", Some(json!({"project":"core","name":"review","agents":[a]}))).await;
    assert_eq!(st, StatusCode::CREATED);
    let cid = ch["id"].as_str().unwrap().to_string();
    f.sessions.send(&b, "outside").await.unwrap();
    wait_status(&f.store, &b, SessionStatus::WaitingForApproval).await;
    let (_, feed) = call(&f.app, "GET", &format!("/api/channels/{cid}/feed"), None).await;
    assert!(feed.as_array().unwrap().is_empty(), "{feed}");
    let (st, _) = call(
        &f.app,
        "POST",
        &format!("/api/channels/{cid}/feed/approval-1/answer"),
        Some(json!({"decision":"deny"})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // The default channel does carry it.
    let (_, all) = call(&f.app, "GET", "/api/channels/default-core/feed", None).await;
    assert_eq!(all.as_array().unwrap().len(), 1);
    assert_eq!(f.store.session(&b).unwrap().unwrap().status, SessionStatus::WaitingForApproval);
}

#[tokio::test]
async fn fr_t11_an_interrupted_turn_withdraws_its_approval_and_reports_interrupted() {
    let f = fx();
    let a = session(&f.store, "core", "alpha");
    f.sessions.send(&a, "slow one").await.unwrap();
    wait_status(&f.store, &a, SessionStatus::Running).await;
    f.sessions.interrupt(&a).await.unwrap();
    wait_status(&f.store, &a, SessionStatus::Finished).await;
    let (_, feed) = call(&f.app, "GET", "/api/channels/default-core/feed", None).await;
    let items = feed.as_array().unwrap();
    assert_eq!(items.len(), 1, "{feed}");
    assert_eq!(items[0]["kind"], "report");
    assert_eq!(items[0]["outcome"], "interrupted");
}

#[tokio::test]
async fn fr_t11_unknown_channel_is_not_found() {
    let f = fx();
    let (st, _) = call(&f.app, "GET", "/api/channels/nope/feed", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

fn ask(store: &Store, session: &str, id: &str, tool: &str, input: Value) {
    store
        .append(
            session,
            &ember_server::events::AgentEvent::ApprovalRequested {
                approval_id: id.into(),
                tool: tool.into(),
                input,
            },
        )
        .unwrap();
}

#[tokio::test]
async fn fr_t11_a_question_approval_carries_readable_questions_and_keeps_its_raw_input() {
    let f = fx();
    let a = session(&f.store, "core", "alpha");
    let long = "x".repeat(300);
    let input = json!({"questions": [{
        "question": "Which database?", "header": "DB", "multiSelect": true,
        "options": [{"label": "pg", "description": "d"}, {"label": "sqlite", "description": "d"}]
    }, {"question": long, "options": []}]});
    ask(&f.store, &a, "q1", "AskUserQuestion", input.clone());
    let (st, feed) = call(&f.app, "GET", "/api/channels/default-core/feed", None).await;
    assert_eq!(st, StatusCode::OK);
    let it = &feed.as_array().unwrap()[0];
    assert_eq!(it["tool"], "AskUserQuestion");
    assert_eq!(it["input"], input, "the raw input stays");
    assert_eq!(it["questions"][0]["header"], "DB");
    assert_eq!(it["questions"][0]["text"], "Which database?");
    assert_eq!(it["questions"][0]["options"], json!(["pg", "sqlite"]));
    assert_eq!(it["questions"][0]["multi_select"], true);
    assert_eq!(it["questions"][1]["multi_select"], false);
    assert!(it["questions"][1].get("header").is_none());
    assert_eq!(it["questions"][1]["text"].as_str().unwrap().chars().count(), 200);
}

#[tokio::test]
async fn fr_t11_a_malformed_question_input_and_a_normal_approval_have_no_questions() {
    let f = fx();
    let a = session(&f.store, "core", "alpha");
    ask(&f.store, &a, "q1", "AskUserQuestion", json!({"questions": "nope"}));
    ask(&f.store, &a, "b1", "Bash", json!({"questions": [{"question": "q"}]}));
    let (_, feed) = call(&f.app, "GET", "/api/channels/default-core/feed", None).await;
    let items = feed.as_array().unwrap();
    assert_eq!(items.len(), 2, "{feed}");
    for it in items {
        assert!(it.get("questions").is_none(), "{it}");
        assert!(it.get("input").is_some());
    }
}
