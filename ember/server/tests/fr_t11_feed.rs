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
                needs_person: false,
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

/// A question with one single-select question `q0` (Red or Blue, free text allowed).
fn colour_event(id: &str) -> ember_server::events::AgentEvent {
    use ember_server::events::{AgentEvent, Question, QuestionOption};
    AgentEvent::QuestionAsked {
        question_id: id.into(),
        questions: vec![Question {
            id: "q0".into(),
            header: Some("Colour".into()),
            text: "Which colour do you prefer?".into(),
            options: vec![
                QuestionOption { label: "Red".into(), description: Some("Choose red".into()) },
                QuestionOption { label: "Blue".into(), description: None },
            ],
            multi_select: false,
            secret: false,
            allow_other: true,
        }],
    }
}

fn ask_question(store: &Store, session: &str, id: &str) {
    store.append(session, &colour_event(id)).unwrap();
}

#[tokio::test]
async fn fr_a5_a_pending_question_appears_in_the_feed_with_labels_only() {
    let f = fx();
    let a = session(&f.store, "core", "alpha");
    ask_question(&f.store, &a, "r1");
    let (st, feed) = call(&f.app, "GET", "/api/channels/default-core/feed", None).await;
    assert_eq!(st, StatusCode::OK);
    let items = feed.as_array().unwrap();
    assert_eq!(items.len(), 1, "{feed}");
    let it = &items[0];
    assert_eq!(it["kind"], "question");
    assert_eq!(it["id"], "question:r1");
    assert_eq!(it["question_id"], "r1");
    assert_eq!(it["agent"]["name"], "alpha");
    assert_eq!(it["session_id"], a.as_str());
    assert_eq!(it["link"], format!("/sessions/{a}"));
    assert_eq!(it["questions"][0]["header"], "Colour");
    assert_eq!(it["questions"][0]["text"], "Which colour do you prefer?");
    assert_eq!(it["questions"][0]["options"], json!(["Red", "Blue"]));
    assert!(it.get("approval_id").is_none() && it.get("input").is_none(), "{it}");
    // The approval answer route does not take a question id (the question route does, see below).
    for path in ["r1", "question:r1"] {
        let (st, _) = call(
            &f.app,
            "POST",
            &format!("/api/channels/default-core/feed/{path}/answer"),
            Some(json!({"decision":"allow_once"})),
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn fr_a5_a_question_clears_when_answered_skipped_or_the_turn_ends() {
    use ember_server::events::{AgentEvent, TurnOutcome};
    let f = fx();
    let a = session(&f.store, "core", "alpha");
    let count = |feed: &Value| feed.as_array().unwrap().iter().filter(|i| i["kind"] == "question").count();
    ask_question(&f.store, &a, "r1");
    ask_question(&f.store, &a, "r2");
    ask_question(&f.store, &a, "r3");
    let (_, feed) = call(&f.app, "GET", "/api/channels/default-core/feed", None).await;
    assert_eq!(count(&feed), 3);
    // Answered, and skipped (no answers): each clears only itself.
    f.store
        .append(&a, &AgentEvent::QuestionAnswered { question_id: "r1".into(), answers: Vec::new() })
        .unwrap();
    f.store
        .append(
            &a,
            &AgentEvent::QuestionAnswered {
                question_id: "r2".into(),
                answers: vec![ember_server::events::QuestionAnswer { question_id: "q0".into(), values: vec!["Red".into()] }],
            },
        )
        .unwrap();
    let (_, feed) = call(&f.app, "GET", "/api/channels/default-core/feed", None).await;
    assert_eq!(count(&feed), 1);
    assert_eq!(feed[0]["question_id"], "r3");
    // The turn ends (an interrupt too): the last one is withdrawn.
    f.store.append(&a, &AgentEvent::TurnEnded { outcome: TurnOutcome::Interrupted }).unwrap();
    let (_, feed) = call(&f.app, "GET", "/api/channels/default-core/feed", None).await;
    assert_eq!(count(&feed), 0, "{feed}");
}

#[tokio::test]
async fn fr_t11_a_large_approval_input_is_replaced_by_a_marker() {
    let f = fx();
    let a = session(&f.store, "core", "alpha");
    let big = "x".repeat(8191); // serializes to 8193 bytes
    ask(&f.store, &a, "big", "Write", json!(big));
    ask(&f.store, &a, "edge", "Write", json!("x".repeat(8190))); // exactly 8192
    let (_, feed) = call(&f.app, "GET", "/api/channels/default-core/feed", None).await;
    let items = feed.as_array().unwrap();
    let find = |id: &str| items.iter().find(|i| i["approval_id"] == id).unwrap();
    assert_eq!(find("big")["input"], json!({"truncated": true, "bytes": 8193}));
    assert_eq!(find("edge")["input"], json!("x".repeat(8190)));
}

// ---------------------------------------------------------------------------------------------
// Answering a question from the feed (FR-T11, FR-A5, PR 2b). Written test first, not yet compiled.
// ---------------------------------------------------------------------------------------------

type Log = Arc<std::sync::Mutex<Vec<String>>>;

/// A fake agent: every message it receives asks the colour question whose id is the message text.
/// It logs how it is answered: `q:<id>:<question id>=<values>;...` or `d:<id>`.
struct Asker {
    log: Log,
}

#[async_trait::async_trait]
impl AgentAdapter for Asker {
    fn kind(&self) -> AgentKind {
        AgentKind::Scripted
    }

    async fn detect(&self) -> ember_server::agents::Detected {
        ember_server::agents::Detected { kind: AgentKind::Scripted, installed: true, version: None }
    }

    async fn start(
        &self,
        _req: ember_server::agents::StartRequest,
        events: tokio::sync::mpsc::Sender<ember_server::events::AgentEvent>,
    ) -> anyhow::Result<Box<dyn ember_server::agents::AgentRun>> {
        Ok(Box::new(AskerRun { events, log: self.log.clone() }))
    }
}

struct AskerRun {
    events: tokio::sync::mpsc::Sender<ember_server::events::AgentEvent>,
    log: Log,
}

#[async_trait::async_trait]
impl ember_server::agents::AgentRun for AskerRun {
    async fn send(&mut self, text: &str) -> anyhow::Result<()> {
        self.events.send(colour_event(text)).await?;
        Ok(())
    }

    async fn answer(&mut self, _approval_id: &str, _decision: ember_server::events::ApprovalDecision) -> anyhow::Result<()> {
        Ok(())
    }

    async fn answer_question(
        &mut self,
        question_id: &str,
        answers: &[ember_server::events::QuestionAnswer],
    ) -> anyhow::Result<()> {
        let a: Vec<String> = answers.iter().map(|a| format!("{}={}", a.question_id, a.values.join("|"))).collect();
        self.log.lock().unwrap().push(format!("q:{question_id}:{}", a.join(";")));
        Ok(())
    }

    async fn decline_question(&mut self, question_id: &str) -> anyhow::Result<()> {
        self.log.lock().unwrap().push(format!("d:{question_id}"));
        Ok(())
    }

    async fn interrupt(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn shutdown(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

fn fx_asker() -> (Fx, Log) {
    let log: Log = Arc::default();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), vec![Arc::new(Asker { log: log.clone() }) as Arc<dyn AgentAdapter>]);
    let app = ember_server::channels::api::router(store.clone(), sessions.clone());
    (Fx { store, sessions, app }, log)
}

/// Poll until `cond` holds, for at most 5 s. Positive waits only: nothing here asserts an absence.
async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    for _ in 0..1000 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("never happened within 5 s: {what}");
}

/// Send `question_id` to the fake agent of `session` and wait until its question is pending.
async fn ask_live(f: &Fx, session: &str, question_id: &str) {
    f.sessions.send(session, question_id).await.unwrap();
    wait_until("the question is pending", || {
        f.sessions.pending_questions(session).unwrap().iter().any(|(q, _)| q == question_id)
    })
    .await;
}

/// A channel of `project` with exactly the `agents`.
async fn channel_of(f: &Fx, name: &str, agents: &[&str]) -> String {
    let (st, ch) = call(&f.app, "POST", "/api/channels", Some(json!({"project":"core","name":name,"agents":agents}))).await;
    assert_eq!(st, StatusCode::CREATED, "{ch}");
    ch["id"].as_str().unwrap().to_string()
}

fn qurl(channel: &str, question_id: &str) -> String {
    format!("/api/channels/{channel}/questions/{question_id}/answer")
}

fn red() -> Value {
    json!({"answers": [{"question_id": "q0", "values": ["Red"]}]})
}

fn answered_count(f: &Fx, session: &str) -> usize {
    f.store
        .events_after(session, 0)
        .unwrap()
        .iter()
        .filter(|e| matches!(e.event, ember_server::events::AgentEvent::QuestionAnswered { .. }))
        .count()
}

async fn question_items(f: &Fx, channel: &str) -> usize {
    let (_, feed) = call(&f.app, "GET", &format!("/api/channels/{channel}/feed"), None).await;
    feed.as_array().unwrap().iter().filter(|i| i["kind"] == "question").count()
}

#[tokio::test]
async fn fr_t11_a_member_question_is_answered_through_the_channel_route() {
    let (f, log) = fx_asker();
    let a = session(&f.store, "core", "alpha");
    ask_live(&f, &a, "r1").await;
    assert_eq!(question_items(&f, "default-core").await, 1);
    let (st, body) = call(&f.app, "POST", &qurl("default-core", "r1"), Some(red())).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"status": "answered"}));
    assert_eq!(*log.lock().unwrap(), vec!["q:r1:q0=Red".to_string()]);
    // The session records the answer and the feed item clears with it.
    wait_until("QuestionAnswered stored", || answered_count(&f, &a) == 1).await;
    let stored = f.store.events_after(&a, 0).unwrap();
    let answers = stored
        .iter()
        .find_map(|e| match &e.event {
            ember_server::events::AgentEvent::QuestionAnswered { answers, .. } => Some(answers.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].values, vec!["Red".to_string()]);
    assert_eq!(question_items(&f, "default-core").await, 0);
    assert!(f.sessions.pending_questions(&a).unwrap().is_empty());
}

#[tokio::test]
async fn fr_t11_a_member_question_is_declined_through_the_channel_route() {
    let (f, log) = fx_asker();
    let a = session(&f.store, "core", "alpha");
    ask_live(&f, &a, "r1").await;
    let (st, body) = call(&f.app, "POST", &qurl("default-core", "r1"), Some(json!({"decline": true}))).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"status": "declined"}));
    assert_eq!(*log.lock().unwrap(), vec!["d:r1".to_string()]);
    wait_until("QuestionAnswered stored", || answered_count(&f, &a) == 1).await;
    // A skip is recorded as an answer with no values.
    let skipped = f.store.events_after(&a, 0).unwrap().iter().any(|e| {
        matches!(&e.event, ember_server::events::AgentEvent::QuestionAnswered { answers, .. } if answers.is_empty())
    });
    assert!(skipped);
    assert_eq!(question_items(&f, "default-core").await, 0);
}

#[tokio::test]
async fn fr_t11_a_question_of_a_session_outside_the_channel_is_404_and_untouched() {
    let (f, log) = fx_asker();
    let a = session(&f.store, "core", "alpha");
    let b = session(&f.store, "core", "beta");
    let cid = channel_of(&f, "review", &[a.as_str()]).await;
    ask_live(&f, &b, "rb").await;
    // The channel does not list it, and its route does not reach it.
    assert_eq!(question_items(&f, &cid).await, 0);
    for body in [red(), json!({"decline": true})] {
        let (st, resp) = call(&f.app, "POST", &qurl(&cid, "rb"), Some(body)).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{resp}");
        assert_eq!(resp["code"], "not_found");
        assert_eq!(resp["error"], format!("pending question rb in channel {cid} not found"));
    }
    assert!(log.lock().unwrap().is_empty(), "the agent was not told anything");
    assert_eq!(answered_count(&f, &b), 0);
    assert_eq!(f.sessions.pending_questions(&b).unwrap().len(), 1);
    // The default channel does carry it, so it is answerable there.
    let (st, _) = call(&f.app, "POST", &qurl("default-core", "rb"), Some(red())).await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn fr_t11_an_unknown_or_answered_question_is_404() {
    let (f, log) = fx_asker();
    let a = session(&f.store, "core", "alpha");
    ask_live(&f, &a, "r1").await;
    let (st, body) = call(&f.app, "POST", &qurl("default-core", "nope"), Some(red())).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "not_found");
    let (st, _) = call(&f.app, "POST", &qurl("no-such-channel", "r1"), Some(red())).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // An approval id is not a question id, and the other way round.
    let (st, _) = call(&f.app, "POST", "/api/channels/default-core/feed/r1/answer", Some(json!({"decision":"allow_once"}))).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert!(log.lock().unwrap().is_empty());
    // Answered once, then no longer pending: the second answer is 404 and sends nothing.
    let (st, _) = call(&f.app, "POST", &qurl("default-core", "r1"), Some(red())).await;
    assert_eq!(st, StatusCode::OK);
    wait_until("QuestionAnswered stored", || answered_count(&f, &a) == 1).await;
    let (st, _) = call(&f.app, "POST", &qurl("default-core", "r1"), Some(red())).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call(&f.app, "POST", &qurl("default-core", "r1"), Some(json!({"decline": true}))).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert_eq!(*log.lock().unwrap(), vec!["q:r1:q0=Red".to_string()]);
    assert_eq!(answered_count(&f, &a), 1);
}

#[tokio::test]
async fn fr_t11_an_invalid_answer_is_400_with_the_fixed_message_and_the_question_stays() {
    let (f, log) = fx_asker();
    let a = session(&f.store, "core", "alpha");
    ask_live(&f, &a, "r1").await;
    let url = qurl("default-core", "r1");
    // A question without an answer.
    let (st, body) = call(&f.app, "POST", &url, Some(json!({"answers": []}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "every question needs an answer");
    assert_eq!(body["code"], "bad_request");
    // A value over 1000 characters (an unknown option is fine: it is free text).
    let long = "x".repeat(1001);
    let (st, body) = call(&f.app, "POST", &url, Some(json!({"answers": [{"question_id": "q0", "values": [long]}]}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "a value needs 1 to 1000 characters");
    // Both, or neither, of answers and decline.
    for b in [json!({}), json!({"decline": false}), json!({"decline": true, "answers": []})] {
        let (st, body) = call(&f.app, "POST", &url, Some(b.clone())).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
        assert_eq!(body["error"], "send either answers or decline: true, not both");
    }
    // Unknown fields are refused by the body type, as on the session route.
    for b in [
        json!({"decline": true, "extra": 1}),
        json!({"answers": [{"question_id": "q0", "values": ["Red"], "extra": 1}]}),
    ] {
        let (st, _) = call(&f.app, "POST", &url, Some(b.clone())).await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    }
    // Nothing was sent or recorded, and the question can still be answered.
    assert!(log.lock().unwrap().is_empty());
    assert_eq!(answered_count(&f, &a), 0);
    assert_eq!(question_items(&f, "default-core").await, 1);
    let (st, _) = call(&f.app, "POST", &url, Some(red())).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(*log.lock().unwrap(), vec!["q:r1:q0=Red".to_string()]);
}

#[tokio::test]
async fn fr_t11_the_route_of_one_channel_cannot_answer_a_question_only_another_has() {
    let (f, log) = fx_asker();
    let a = session(&f.store, "core", "alpha");
    let b = session(&f.store, "core", "beta");
    let ca = channel_of(&f, "chan-a", &[a.as_str()]).await;
    let cb = channel_of(&f, "chan-b", &[b.as_str()]).await;
    ask_live(&f, &a, "ra").await;
    assert_eq!(question_items(&f, &ca).await, 1);
    assert_eq!(question_items(&f, &cb).await, 0);
    let (st, _) = call(&f.app, "POST", &qurl(&cb, "ra"), Some(red())).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert!(log.lock().unwrap().is_empty());
    assert_eq!(answered_count(&f, &a), 0);
    let (st, _) = call(&f.app, "POST", &qurl(&ca, "ra"), Some(red())).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(*log.lock().unwrap(), vec!["q:ra:q0=Red".to_string()]);
}

#[tokio::test]
async fn fr_t11_a_question_whose_agent_is_not_running_is_409() {
    let (f, log) = fx_asker();
    let a = session(&f.store, "core", "alpha");
    // Seeded in the store only: the feed lists it, but no agent process holds it.
    ask_question(&f.store, &a, "r1");
    assert_eq!(question_items(&f, "default-core").await, 1);
    let (st, body) = call(&f.app, "POST", &qurl("default-core", "r1"), Some(red())).await;
    assert_eq!(st, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "conflict");
    assert!(log.lock().unwrap().is_empty());
    assert_eq!(question_items(&f, "default-core").await, 1);
}
