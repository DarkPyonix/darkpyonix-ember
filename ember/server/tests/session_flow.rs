//! End-to-end session flow against the scripted agent: FR-S1–S3, FR-A5, PR-1.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::events::{
    AgentEvent, ApprovalDecision, Question, QuestionAnswer, QuestionOption, SessionStatus, TurnOutcome,
};
use ember_server::session::{NewSession, Push, Sessions};
use ember_server::store::Store;
use http_body_util::BodyExt;
use tower::ServiceExt;

fn sessions_with(store: Arc<Store>) -> Arc<Sessions> {
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    Sessions::new(store, adapters)
}

fn new_session(s: &Sessions) -> String {
    s.create(NewSession {
        project: "acme".into(),
        agent: AgentKind::Scripted,
        cwd: std::env::temp_dir(),
        model: None,
        title: "t".into(),
    })
    .unwrap()
    .id
}

/// Wait until the stored session reaches `want`.
async fn wait_status(s: &Sessions, id: &str, want: SessionStatus) {
    for _ in 0..200 {
        if s.store().session(id).unwrap().unwrap().status == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("session {id} never reached {want:?}");
}

#[tokio::test]
async fn turn_completes_with_approval_and_is_stored_in_order() {
    let s = sessions_with(Arc::new(Store::open_in_memory().unwrap()));
    let id = new_session(&s);
    let mut push = s.subscribe();

    s.send(&id, "hello").await.unwrap();
    wait_status(&s, &id, SessionStatus::WaitingForApproval).await;
    s.answer(&id, "approval-1", ApprovalDecision::AllowOnce).await.unwrap();
    wait_status(&s, &id, SessionStatus::Finished).await;

    let kinds: Vec<_> = s
        .store()
        .events_after(&id, 0)
        .unwrap()
        .into_iter()
        .map(|e| e.event)
        .collect();
    assert!(matches!(kinds[0], AgentEvent::NativeSession { .. }));
    assert_eq!(kinds[1], AgentEvent::UserMessage { text: "hello".into() });
    assert!(matches!(kinds[2], AgentEvent::ToolCall { .. }));
    assert!(matches!(kinds[3], AgentEvent::ApprovalRequested { .. }));
    assert!(matches!(kinds[4], AgentEvent::ApprovalResolved { decision: ApprovalDecision::AllowOnce, .. }));
    assert_eq!(kinds.last().unwrap(), &AgentEvent::TurnEnded { outcome: TurnOutcome::Completed });
    assert!(kinds.contains(&AgentEvent::AssistantMessage { text: "echo: hello".into() }));

    // Every stored event was also pushed, with consecutive sequence numbers.
    let mut seqs = Vec::new();
    while let Ok(p) = push.try_recv() {
        if let Push::Event { event, .. } = p {
            seqs.push(event.seq);
        }
    }
    assert_eq!(seqs, (1..=kinds.len() as i64).collect::<Vec<_>>());
}

#[tokio::test]
async fn turn_completes_with_no_client_attached_and_late_reader_sees_all() {
    // FR-S3: nobody subscribes to push; a late reader replays from the store.
    let s = sessions_with(Arc::new(Store::open_in_memory().unwrap()));
    let id = new_session(&s);
    s.send(&id, "unattended").await.unwrap();
    wait_status(&s, &id, SessionStatus::WaitingForApproval).await;
    s.answer(&id, "approval-1", ApprovalDecision::AllowAlways).await.unwrap();
    wait_status(&s, &id, SessionStatus::Finished).await;
    let all = s.store().events_after(&id, 0).unwrap();
    assert!(all.iter().any(|e| e.event == AgentEvent::AssistantMessage { text: "echo: unattended".into() }));
}

#[tokio::test]
async fn interrupt_ends_turn_as_interrupted() {
    let s = sessions_with(Arc::new(Store::open_in_memory().unwrap()));
    let id = new_session(&s);
    s.send(&id, "slow task").await.unwrap();
    wait_status(&s, &id, SessionStatus::Running).await;
    s.interrupt(&id).await.unwrap();
    wait_status(&s, &id, SessionStatus::Finished).await;
    let last = s.store().events_after(&id, 0).unwrap().pop().unwrap();
    assert_eq!(last.event, AgentEvent::TurnEnded { outcome: TurnOutcome::Interrupted });
}

#[tokio::test]
async fn restart_resumes_with_the_stored_native_id() {
    // FR-S2: after a "restart" (new Sessions over the same file), the next message resumes the
    // agent with the native id recorded before, and the history is intact.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ember.db");
    let (id, native) = {
        let s = sessions_with(Arc::new(Store::open(&path).unwrap()));
        let id = new_session(&s);
        s.send(&id, "first").await.unwrap();
        wait_status(&s, &id, SessionStatus::WaitingForApproval).await;
        s.answer(&id, "approval-1", ApprovalDecision::AllowOnce).await.unwrap();
        wait_status(&s, &id, SessionStatus::Finished).await;
        let native = s.store().session(&id).unwrap().unwrap().native_id.unwrap();
        s.release(&id).await.unwrap();
        (id, native)
    };
    let store = Arc::new(Store::open(&path).unwrap());
    store.reset_live_statuses().unwrap();
    let s = sessions_with(store);
    let before = s.store().session(&id).unwrap().unwrap().last_seq;
    s.send(&id, "second").await.unwrap();
    wait_status(&s, &id, SessionStatus::WaitingForApproval).await;
    let rec = s.store().session(&id).unwrap().unwrap();
    assert_eq!(rec.native_id.as_deref(), Some(native.as_str()));
    // The resumed agent's repeated NativeSession was not stored again.
    let new_events = s.store().events_after(&id, before).unwrap();
    assert!(!new_events.iter().any(|e| matches!(e.event, AgentEvent::NativeSession { .. })));
}

#[tokio::test]
async fn http_create_send_and_read_events() {
    let s = sessions_with(Arc::new(Store::open_in_memory().unwrap()));
    let app = ember_server::api::router(s.clone());

    let body = serde_json::json!({ "project": "acme", "agent": "scripted", "cwd": "/tmp" });
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/sessions")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let rec: serde_json::Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let id = rec["id"].as_str().unwrap().to_string();

    let res = app
        .clone()
        .oneshot(
            Request::post(format!("/api/sessions/{id}/messages"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"text":"hi"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    wait_status(&s, &id, SessionStatus::WaitingForApproval).await;

    let res = app
        .clone()
        .oneshot(Request::get(format!("/api/sessions/{id}/events?after=1")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let events: Vec<serde_json::Value> =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(events[0]["seq"], 2);
    assert_eq!(events[0]["event"]["kind"], "user_message");

    let res = app
        .oneshot(Request::get("/api/sessions/nope/events").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn idle_agents_are_released_unless_leased_and_resume_on_next_message() {
    // FR-S6.
    let s = sessions_with(Arc::new(Store::open_in_memory().unwrap()));
    let a = new_session(&s);
    let b = new_session(&s);
    for id in [&a, &b] {
        s.send(id, "x").await.unwrap();
        wait_status(&s, id, SessionStatus::WaitingForApproval).await;
    }
    // Mid-turn sessions are never reaped.
    assert!(s.reap_idle(Duration::ZERO).await.is_empty());
    for id in [&a, &b] {
        s.answer(id, "approval-1", ApprovalDecision::AllowOnce).await.unwrap();
        wait_status(&s, id, SessionStatus::Finished).await;
    }
    s.lease(&b, Duration::from_secs(60)).unwrap();
    assert_eq!(s.reap_idle(Duration::ZERO).await, vec![a.clone()]);
    assert!(!s.is_live(&a).await);
    assert!(s.is_live(&b).await);
    // A released session comes back on the next message.
    s.send(&a, "again").await.unwrap();
    wait_status(&s, &a, SessionStatus::WaitingForApproval).await;
    assert!(s.is_live(&a).await);
}

#[tokio::test]
async fn start_hooks_run_for_each_agent_start() {
    let s = sessions_with(Arc::new(Store::open_in_memory().unwrap()));
    let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let log = seen.clone();
    s.add_start_hook(Arc::new(move |rec| {
        log.lock().unwrap().push(rec.id.clone());
        vec![("EMBER_SESSION_ID".into(), rec.id.clone())]
    }));
    let id = new_session(&s);
    s.send(&id, "x").await.unwrap();
    wait_status(&s, &id, SessionStatus::WaitingForApproval).await;
    assert_eq!(*seen.lock().unwrap(), vec![id]);
}

fn resolved(s: &Sessions, id: &str) -> Vec<ApprovalDecision> {
    s.store()
        .events_after(id, 0)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            AgentEvent::ApprovalResolved { decision, .. } => Some(decision),
            _ => None,
        })
        .collect()
}

/// FR-A5: with the mode on before the turn, later approvals are answered without a client.
#[tokio::test]
async fn approve_all_answers_later_approvals_on_every_turn() {
    let s = sessions_with(Arc::new(Store::open_in_memory().unwrap()));
    let id = new_session(&s);
    assert!(!s.approve_all(&id));
    s.set_approve_all(&id, true).await.unwrap();
    assert!(s.approve_all(&id));

    s.send(&id, "one").await.unwrap();
    wait_status(&s, &id, SessionStatus::Finished).await;
    s.send(&id, "two").await.unwrap();
    for _ in 0..200 {
        if resolved(&s, &id).len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(resolved(&s, &id), vec![ApprovalDecision::AllowOnce; 2]);
}

/// FR-A5: switching the mode on answers the approval already pending in the running turn.
#[tokio::test]
async fn approve_all_answers_the_pending_approval_when_switched_on() {
    let s = sessions_with(Arc::new(Store::open_in_memory().unwrap()));
    let id = new_session(&s);
    let mut push = s.subscribe();
    s.send(&id, "hello").await.unwrap();
    wait_status(&s, &id, SessionStatus::WaitingForApproval).await;

    s.set_approve_all(&id, true).await.unwrap();
    wait_status(&s, &id, SessionStatus::Finished).await;
    assert_eq!(resolved(&s, &id), vec![ApprovalDecision::AllowOnce]);

    let mut seen = Vec::new();
    while let Ok(p) = push.try_recv() {
        if let Push::ApproveAll { session_id, enabled } = p {
            seen.push((session_id, enabled));
        }
    }
    assert_eq!(seen, vec![(id.clone(), true)]);

    // Off again: the next approval waits for a person.
    s.set_approve_all(&id, false).await.unwrap();
    s.send(&id, "again").await.unwrap();
    wait_status(&s, &id, SessionStatus::WaitingForApproval).await;
}

#[tokio::test]
async fn approve_all_http_toggle_and_session_detail() {
    let s = sessions_with(Arc::new(Store::open_in_memory().unwrap()));
    let app = ember_server::api::router(s.clone());
    let id = new_session(&s);

    let res = app
        .clone()
        .oneshot(
            Request::put(format!("/api/sessions/{id}/approve-all"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"enabled":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let res = app
        .clone()
        .oneshot(Request::get(format!("/api/sessions/{id}")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let detail: serde_json::Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(detail["approve_all"], true);

    let res = app
        .oneshot(
            Request::put("/api/sessions/nope/approve-all")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"enabled":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

/// A fake agent that asks one question (`AskUserQuestion`, id `q1`) and one Bash approval (`b1`)
/// per message, and records the ids it was answered for.
struct AsksQuestion {
    answered: Arc<std::sync::Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl AgentAdapter for AsksQuestion {
    fn kind(&self) -> AgentKind {
        AgentKind::Scripted
    }

    async fn detect(&self) -> ember_server::agents::Detected {
        ember_server::agents::Detected { kind: AgentKind::Scripted, installed: true, version: None }
    }

    async fn start(
        &self,
        _req: ember_server::agents::StartRequest,
        events: tokio::sync::mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<Box<dyn ember_server::agents::AgentRun>> {
        Ok(Box::new(AsksQuestionRun { events, answered: self.answered.clone() }))
    }
}

struct AsksQuestionRun {
    events: tokio::sync::mpsc::Sender<AgentEvent>,
    answered: Arc<std::sync::Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl ember_server::agents::AgentRun for AsksQuestionRun {
    async fn send(&mut self, _text: &str) -> anyhow::Result<()> {
        let q = serde_json::json!({"questions": [{"question": "Which?", "header": "H",
            "options": [{"label": "a", "description": ""}, {"label": "b", "description": ""}],
            "multiSelect": false}]});
        self.events
            .send(AgentEvent::ApprovalRequested {
                approval_id: "q1".into(),
                tool: "AskUserQuestion".into(),
                input: q,
                needs_person: true,
            })
            .await?;
        self.events
            .send(AgentEvent::ApprovalRequested {
                approval_id: "b1".into(),
                tool: "Bash".into(),
                input: serde_json::json!({"command": "ls"}),
                needs_person: false,
            })
            .await?;
        Ok(())
    }

    async fn answer(&mut self, approval_id: &str, _decision: ApprovalDecision) -> anyhow::Result<()> {
        self.answered.lock().unwrap().push(approval_id.to_string());
        Ok(())
    }

    async fn interrupt(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn shutdown(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

fn asks_question_sessions() -> (Arc<Sessions>, Arc<std::sync::Mutex<Vec<String>>>) {
    let answered: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(AsksQuestion { answered: answered.clone() })];
    (Sessions::new(Arc::new(Store::open_in_memory().unwrap()), adapters), answered)
}

async fn wait_answered(answered: &Arc<std::sync::Mutex<Vec<String>>>, n: usize) {
    for _ in 0..200 {
        if answered.lock().unwrap().len() >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the agent was never answered {n} times");
}

fn notices(s: &Sessions, id: &str) -> Vec<String> {
    s.store()
        .events_after(id, 0)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            AgentEvent::Notice { message } => Some(message),
            _ => None,
        })
        .collect()
}

const QUESTION_NOTICE: &str = "질문에는 자동으로 답하지 않습니다. 직접 답해 주세요.";

/// FR-A5: with approve-everything on, a question waits for a person while a Bash approval in the
/// same turn is allowed.
#[tokio::test]
async fn fr_a5_approve_all_does_not_answer_a_question_but_answers_a_normal_approval() {
    let (s, answered) = asks_question_sessions();
    let id = new_session(&s);
    s.set_approve_all(&id, true).await.unwrap();
    s.send(&id, "go").await.unwrap();
    // The agent emits q1 before b1 and the pump handles events in order, so once b1 is answered
    // (the sentinel) q1 has been handled: if the mode had answered it, it would be logged.
    wait_answered(&answered, 1).await;
    assert_eq!(*answered.lock().unwrap(), vec!["b1".to_string()], "the question was not answered");
    let n: Vec<_> = notices(&s, &id).into_iter().filter(|m| m == QUESTION_NOTICE).collect();
    assert_eq!(n.len(), 1, "one notice says why the question waits");
    // A person can still answer it.
    s.answer(&id, "q1", ApprovalDecision::AllowOnce).await.unwrap();
    assert_eq!(*answered.lock().unwrap(), vec!["b1".to_string(), "q1".to_string()]);
}

/// FR-A5: switching approve-everything on answers the pending Bash approval, not the question.
#[tokio::test]
async fn fr_a5_switching_approve_all_on_skips_a_pending_question() {
    let (s, answered) = asks_question_sessions();
    let id = new_session(&s);
    s.send(&id, "go").await.unwrap();
    for _ in 0..200 {
        let both = s
            .store()
            .events_after(&id, 0)
            .unwrap()
            .iter()
            .filter(|e| matches!(e.event, AgentEvent::ApprovalRequested { .. }))
            .count()
            == 2;
        if both {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // `set_approve_all` answers every pending approval before it returns, so nothing is in
    // flight afterwards.
    s.set_approve_all(&id, true).await.unwrap();
    wait_answered(&answered, 1).await;
    assert_eq!(*answered.lock().unwrap(), vec!["b1".to_string()]);
}

// ---------------------------------------------------------------------------------------------
// Tool policy (one table) and question events. Written test first, not yet compiled.
// ---------------------------------------------------------------------------------------------

/// A fake agent that emits `script` on every message and logs how it was answered:
/// `a:<id>:<decision>`, `q:<id>:<question id>=<values>;...`, `d:<id>`.
struct Scripted {
    script: Vec<AgentEvent>,
    log: Arc<std::sync::Mutex<Vec<String>>>,
    knobs: Arc<Knobs>,
}

/// What a test can tell the fake to do when it is answered.
#[derive(Default)]
struct Knobs {
    /// The next `answer_question` / `decline_question` fails once (the CLI withdrew the question,
    /// or the write to its stdin failed).
    fail_next_answer: std::sync::atomic::AtomicBool,
    /// Every question answer waits for `release` after signalling `entered`.
    gated: std::sync::atomic::AtomicBool,
    /// When gated, the fake first emits a `TurnEnded` (the question is withdrawn mid-answer).
    end_turn_when_entered: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl Knobs {
    fn set(flag: &std::sync::atomic::AtomicBool) {
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    fn is_set(flag: &std::sync::atomic::AtomicBool) -> bool {
        flag.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl AgentAdapter for Scripted {
    fn kind(&self) -> AgentKind {
        AgentKind::Scripted
    }

    async fn detect(&self) -> ember_server::agents::Detected {
        ember_server::agents::Detected { kind: AgentKind::Scripted, installed: true, version: None }
    }

    async fn start(
        &self,
        _req: ember_server::agents::StartRequest,
        events: tokio::sync::mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<Box<dyn ember_server::agents::AgentRun>> {
        Ok(Box::new(ScriptedRun {
            script: self.script.clone(),
            events,
            log: self.log.clone(),
            knobs: self.knobs.clone(),
        }))
    }
}

struct ScriptedRun {
    script: Vec<AgentEvent>,
    events: tokio::sync::mpsc::Sender<AgentEvent>,
    log: Arc<std::sync::Mutex<Vec<String>>>,
    knobs: Arc<Knobs>,
}

impl ScriptedRun {
    /// The failure and the gate of [`Knobs`], before a question answer is logged.
    async fn before_question_answer(&self) -> anyhow::Result<()> {
        if Knobs::is_set(&self.knobs.gated) {
            if Knobs::is_set(&self.knobs.end_turn_when_entered) {
                self.events.send(AgentEvent::TurnEnded { outcome: TurnOutcome::Interrupted }).await?;
            }
            self.knobs.entered.notify_one();
            self.knobs.release.notified().await;
        }
        if self.knobs.fail_next_answer.swap(false, std::sync::atomic::Ordering::SeqCst) {
            anyhow::bail!("the CLI withdrew the question");
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl ember_server::agents::AgentRun for ScriptedRun {
    async fn send(&mut self, _text: &str) -> anyhow::Result<()> {
        for e in self.script.clone() {
            self.events.send(e).await?;
        }
        Ok(())
    }

    async fn answer(&mut self, approval_id: &str, decision: ApprovalDecision) -> anyhow::Result<()> {
        self.log.lock().unwrap().push(format!("a:{approval_id}:{decision:?}"));
        Ok(())
    }

    async fn answer_question(&mut self, question_id: &str, answers: &[QuestionAnswer]) -> anyhow::Result<()> {
        self.before_question_answer().await?;
        let a: Vec<String> = answers.iter().map(|a| format!("{}={}", a.question_id, a.values.join("|"))).collect();
        self.log.lock().unwrap().push(format!("q:{question_id}:{}", a.join(";")));
        Ok(())
    }

    async fn decline_question(&mut self, question_id: &str) -> anyhow::Result<()> {
        self.before_question_answer().await?;
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

fn scripted_sessions(script: Vec<AgentEvent>) -> (Arc<Sessions>, Arc<std::sync::Mutex<Vec<String>>>) {
    let (s, log, _) = scripted_sessions_with(script, Arc::new(Store::open_in_memory().unwrap()));
    (s, log)
}

fn scripted_sessions_with(
    script: Vec<AgentEvent>,
    store: Arc<Store>,
) -> (Arc<Sessions>, Arc<std::sync::Mutex<Vec<String>>>, Arc<Knobs>) {
    let log: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let knobs: Arc<Knobs> = Arc::default();
    let adapters: Vec<Arc<dyn AgentAdapter>> =
        vec![Arc::new(Scripted { script, log: log.clone(), knobs: knobs.clone() })];
    (Sessions::new(store, adapters), log, knobs)
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

fn stored_kinds(s: &Sessions, id: &str) -> Vec<AgentEvent> {
    s.store().events_after(id, 0).unwrap().into_iter().map(|e| e.event).collect()
}

fn approval(id: &str, tool: &str, needs_person: bool) -> AgentEvent {
    AgentEvent::ApprovalRequested {
        approval_id: id.into(),
        tool: tool.into(),
        input: serde_json::json!({}),
        needs_person,
    }
}

async fn wait_log(log: &Arc<std::sync::Mutex<Vec<String>>>, n: usize) {
    for _ in 0..200 {
        if log.lock().unwrap().len() >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the agent was never answered {n} times: {:?}", log.lock().unwrap());
}

async fn wait_events(s: &Sessions, id: &str, n: usize) {
    for _ in 0..200 {
        if s.store().events_after(id, 0).unwrap().len() >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("session {id} never stored {n} events");
}

const PLAN_NOTICE: &str = "계획을 자동 승인했습니다.";

/// FR-A5: approve-everything still allows a plan (ExitPlanMode) and says so in the transcript; an
/// unknown tool carrying the agent's own mark waits for a person.
#[tokio::test]
async fn fr_a5_approve_all_allows_a_plan_with_a_notice_and_never_a_flagged_unknown_tool() {
    let (s, log) = scripted_sessions(vec![
        approval("p1", "ExitPlanMode", true),
        approval("n1", "SomeNewTool", true),
        approval("b1", "Bash", false),
    ]);
    let id = new_session(&s);
    s.set_approve_all(&id, true).await.unwrap();
    s.send(&id, "go").await.unwrap();
    // Order p1, n1, b1: the pump decides n1 before it handles b1, so once the b1 sentinel is
    // answered the flagged tool has been handled.
    wait_log(&log, 2).await;
    let mut got = log.lock().unwrap().clone();
    got.sort();
    // Allowed once each, never "always"; the flagged unknown tool was not answered.
    assert_eq!(got, vec!["a:b1:AllowOnce".to_string(), "a:p1:AllowOnce".to_string()]);
    let n = notices(&s, &id);
    assert_eq!(n.iter().filter(|m| *m == PLAN_NOTICE).count(), 1, "{n:?}");
    assert_eq!(n.iter().filter(|m| *m == QUESTION_NOTICE).count(), 1, "{n:?}");
    // A person can still answer the unknown tool.
    s.answer(&id, "n1", ApprovalDecision::AllowOnce).await.unwrap();
}

/// FR-A5: switching approve-everything on answers a pending plan (with the notice) and skips a
/// pending flagged unknown tool.
#[tokio::test]
async fn fr_a5_switching_approve_all_on_allows_a_pending_plan_and_skips_a_flagged_tool() {
    let (s, log) = scripted_sessions(vec![approval("p1", "ExitPlanMode", true), approval("n1", "SomeNewTool", true)]);
    let id = new_session(&s);
    s.send(&id, "go").await.unwrap();
    wait_events(&s, &id, 3).await;
    s.set_approve_all(&id, true).await.unwrap();
    // `set_approve_all` answers every pending approval before it returns.
    wait_log(&log, 1).await;
    assert_eq!(*log.lock().unwrap(), vec!["a:p1:AllowOnce".to_string()]);
    assert_eq!(notices(&s, &id).iter().filter(|m| *m == PLAN_NOTICE).count(), 1);
}

fn colour_question() -> AgentEvent {
    AgentEvent::QuestionAsked {
        question_id: "r1".into(),
        questions: vec![
            Question {
                id: "q0".into(),
                header: Some("Colour".into()),
                text: "Which colour?".into(),
                options: vec![QuestionOption { label: "Red".into(), description: None }],
                multi_select: false,
                secret: false,
                allow_other: true,
            },
            Question {
                id: "q1".into(),
                header: None,
                text: "Token?".into(),
                options: Vec::new(),
                multi_select: false,
                secret: true,
                allow_other: true,
            },
        ],
    }
}

fn ans(q: &str, v: &str) -> QuestionAnswer {
    QuestionAnswer { question_id: q.into(), values: vec![v.into()] }
}

/// FR-A5: a question waits in the store, approve-everything does not touch it, an answer is
/// validated, recorded (a secret value as `***`) and sent to the agent with the real values.
#[tokio::test]
async fn fr_a5_a_question_is_answered_recorded_and_its_secret_is_not_stored() {
    // The script ends with a normal Bash approval (b1). Events are handled in order, so once b1
    // has been answered by the mode, the question before it has been handled and left alone.
    let (s, log) = scripted_sessions(vec![colour_question(), approval("b1", "Bash", false)]);
    let id = new_session(&s);
    s.set_approve_all(&id, true).await.unwrap();
    s.send(&id, "go").await.unwrap();
    wait_log(&log, 1).await;
    let sentinel = vec!["a:b1:AllowOnce".to_string()];
    assert_eq!(*log.lock().unwrap(), sentinel, "approve-everything answered b1 and not the question");
    let pending = s.pending_questions(&id).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].0, "r1");

    // Invalid answers change nothing.
    use ember_server::events::AnswerInvalid;
    use ember_server::session::QuestionError;
    let e = s.answer_question(&id, "r1", vec![ans("q0", "Red")]).await.unwrap_err();
    assert!(matches!(e, QuestionError::Invalid(AnswerInvalid::MissingAnswer)), "{e:?}");
    let e = s.answer_question(&id, "nope", vec![]).await.unwrap_err();
    assert!(matches!(e, QuestionError::NotPending(_)), "{e:?}");
    let e = s.answer_question("no-session", "r1", vec![]).await.unwrap_err();
    assert!(matches!(e, QuestionError::NoSession(_)), "{e:?}");
    assert_eq!(*log.lock().unwrap(), sentinel);

    s.answer_question(&id, "r1", vec![ans("q0", "Red"), ans("q1", "hunter2")]).await.unwrap();
    assert_eq!(*log.lock().unwrap(), vec!["a:b1:AllowOnce".to_string(), "q:r1:q0=Red;q1=hunter2".to_string()]);
    wait_until("QuestionAnswered stored", || {
        stored_kinds(&s, &id).iter().any(|e| matches!(e, AgentEvent::QuestionAnswered { .. }))
    })
    .await;
    let stored = s
        .store()
        .events_after(&id, 0)
        .unwrap()
        .into_iter()
        .find_map(|e| match e.event {
            AgentEvent::QuestionAnswered { question_id, answers } => Some((question_id, answers)),
            _ => None,
        })
        .expect("QuestionAnswered stored");
    assert_eq!(stored.0, "r1");
    assert_eq!(stored.1, vec![ans("q0", "Red"), ans("q1", "***")]);
    assert!(!serde_json::to_string(&s.store().events_after(&id, 0).unwrap()).unwrap().contains("hunter2"));
    assert!(s.pending_questions(&id).unwrap().is_empty());
    // A second answer finds nothing pending.
    let e = s.answer_question(&id, "r1", vec![ans("q0", "Red"), ans("q1", "x")]).await.unwrap_err();
    assert!(matches!(e, QuestionError::NotPending(_)), "{e:?}");
}

/// FR-A5: skipping a question is recorded with no answers and tells the agent.
#[tokio::test]
async fn fr_a5_declining_a_question_is_recorded_without_answers() {
    let (s, log) = scripted_sessions(vec![colour_question()]);
    let id = new_session(&s);
    s.send(&id, "go").await.unwrap();
    wait_status(&s, &id, SessionStatus::WaitingForApproval).await;
    s.decline_question(&id, "r1").await.unwrap();
    assert_eq!(*log.lock().unwrap(), vec!["d:r1".to_string()]);
    wait_events(&s, &id, 3).await;
    assert!(s.store().events_after(&id, 0).unwrap().iter().any(|e| matches!(
        &e.event,
        AgentEvent::QuestionAnswered { question_id, answers } if question_id == "r1" && answers.is_empty()
    )));
    assert!(s.pending_questions(&id).unwrap().is_empty());
}

/// FR-A5: a turn end withdraws a pending question.
#[tokio::test]
async fn fr_a5_a_turn_end_withdraws_a_pending_question() {
    let (s, _log) = scripted_sessions(vec![colour_question(), AgentEvent::TurnEnded { outcome: TurnOutcome::Interrupted }]);
    let id = new_session(&s);
    s.send(&id, "go").await.unwrap();
    wait_events(&s, &id, 3).await;
    assert!(s.pending_questions(&id).unwrap().is_empty());
}

/// FR-A5: an agent without question support refuses (the trait default).
#[tokio::test]
async fn fr_a5_an_agent_without_questions_refuses_to_answer_one() {
    let s = sessions_with(Arc::new(Store::open_in_memory().unwrap()));
    let id = new_session(&s);
    s.store()
        .append(&id, &colour_question())
        .unwrap();
    s.send(&id, "hello").await.unwrap();
    wait_status(&s, &id, SessionStatus::WaitingForApproval).await;
    let e = s
        .answer_question(&id, "r1", vec![ans("q0", "Red"), ans("q1", "x")])
        .await
        .unwrap_err();
    assert!(format!("{e}").contains("does not support questions"), "{e}");
}

async fn post_json(app: &axum::Router, uri: &str, body: &str) -> (StatusCode, serde_json::Value) {
    let res = app
        .clone()
        .oneshot(
            Request::post(uri).header("content-type", "application/json").body(Body::from(body.to_string())).unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

/// FR-A5: `POST /api/sessions/{id}/questions/{question_id}/answer`: 404, 400, 422 and 200.
#[tokio::test]
async fn fr_a5_question_route_statuses() {
    let (s, log) = scripted_sessions(vec![colour_question()]);
    let app = ember_server::api::router(s.clone());
    let id = new_session(&s);
    s.send(&id, "go").await.unwrap();
    wait_status(&s, &id, SessionStatus::WaitingForApproval).await;
    let url = |q: &str| format!("/api/sessions/{id}/questions/{q}/answer");

    let (st, _) = post_json(&app, "/api/sessions/nope/questions/r1/answer", r#"{"decline":true}"#).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = post_json(&app, &url("not-asked"), r#"{"decline":true}"#).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // Validation: a fixed message, nothing sent to the agent.
    let (st, body) = post_json(&app, &url("r1"), r#"{"answers":[{"question_id":"q0","values":["Red"]}]}"#).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "every question needs an answer");
    // Exactly one of answers and decline.
    for b in [r#"{}"#, r#"{"decline":false}"#, r#"{"decline":true,"answers":[]}"#] {
        let (st, _) = post_json(&app, &url("r1"), b).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    }
    // Unknown fields are refused.
    for b in [
        r#"{"decline":true,"extra":1}"#,
        r#"{"answers":[{"question_id":"q0","values":["a"],"extra":1}]}"#,
    ] {
        let (st, _) = post_json(&app, &url("r1"), b).await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    }
    assert!(log.lock().unwrap().is_empty());

    let ok = r#"{"answers":[{"question_id":"q0","values":["Red"]},{"question_id":"q1","values":["tok"]}]}"#;
    let (st, body) = post_json(&app, &url("r1"), ok).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body, serde_json::json!({"status": "answered"}));
    assert_eq!(*log.lock().unwrap(), vec!["q:r1:q0=Red;q1=tok".to_string()]);
    // Answered once: no longer pending.
    let (st, _) = post_json(&app, &url("r1"), ok).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn fr_a5_question_route_declines() {
    let (s, log) = scripted_sessions(vec![colour_question()]);
    let app = ember_server::api::router(s.clone());
    let id = new_session(&s);
    s.send(&id, "go").await.unwrap();
    wait_status(&s, &id, SessionStatus::WaitingForApproval).await;
    let (st, body) = post_json(&app, &format!("/api/sessions/{id}/questions/r1/answer"), r#"{"decline":true}"#).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body, serde_json::json!({"status": "declined"}));
    assert_eq!(*log.lock().unwrap(), vec!["d:r1".to_string()]);
}

// ---------------------------------------------------------------------------------------------
// Question answer ordering and withdrawn prompts (follow-ups of the question events review).
// ---------------------------------------------------------------------------------------------

fn valid_colour_answers() -> Vec<QuestionAnswer> {
    vec![ans("q0", "Red"), ans("q1", "x")]
}

fn answered_events(s: &Sessions, id: &str) -> usize {
    stored_kinds(s, id).iter().filter(|e| matches!(e, AgentEvent::QuestionAnswered { .. })).count()
}

/// FR-A5: when the agent fails the answer, nothing is recorded and the key is released, so the
/// retry (and a decline) succeed. The question is recorded only after the agent accepted it.
#[tokio::test]
async fn fr_a5_an_adapter_failure_leaves_no_answer_event_and_the_retry_succeeds() {
    use ember_server::session::QuestionError;
    let (s, log, knobs) = scripted_sessions_with(vec![colour_question()], Arc::new(Store::open_in_memory().unwrap()));
    let id = new_session(&s);
    s.send(&id, "go").await.unwrap();
    wait_status(&s, &id, SessionStatus::WaitingForApproval).await;

    Knobs::set(&knobs.fail_next_answer);
    let e = s.answer_question(&id, "r1", valid_colour_answers()).await.unwrap_err();
    assert!(matches!(e, QuestionError::Other(_)), "{e:?}");
    assert_eq!(answered_events(&s, &id), 0, "a refused answer is not recorded");
    assert_eq!(s.questions_taken_len(), 0, "the key was released");
    assert_eq!(s.pending_questions(&id).unwrap().len(), 1, "the question is still pending");
    assert!(log.lock().unwrap().is_empty());

    // The retry goes through.
    s.answer_question(&id, "r1", valid_colour_answers()).await.unwrap();
    assert_eq!(*log.lock().unwrap(), vec!["q:r1:q0=Red;q1=x".to_string()]);
    wait_until("QuestionAnswered stored", || answered_events(&s, &id) == 1).await;
    assert_eq!(s.questions_taken_len(), 0, "an answered question holds no key");
    assert!(s.pending_questions(&id).unwrap().is_empty());
}

/// FR-A5: a failed skip releases the key too, so a later answer can follow.
#[tokio::test]
async fn fr_a5_a_failed_decline_can_be_followed_by_an_answer() {
    let (s, log, knobs) = scripted_sessions_with(vec![colour_question()], Arc::new(Store::open_in_memory().unwrap()));
    let id = new_session(&s);
    s.send(&id, "go").await.unwrap();
    wait_status(&s, &id, SessionStatus::WaitingForApproval).await;
    Knobs::set(&knobs.fail_next_answer);
    s.decline_question(&id, "r1").await.unwrap_err();
    assert_eq!(answered_events(&s, &id), 0);
    s.answer_question(&id, "r1", valid_colour_answers()).await.unwrap();
    assert_eq!(*log.lock().unwrap(), vec!["q:r1:q0=Red;q1=x".to_string()]);
}

/// FR-A5: a second answer (or a skip) is refused while the first is in flight, and the question is
/// answered exactly once. The first answer is held inside the agent by a gate, not by a sleep.
#[tokio::test]
async fn fr_a5_a_double_answer_is_refused_while_the_first_is_in_flight() {
    use ember_server::session::QuestionError;
    let (s, log, knobs) = scripted_sessions_with(vec![colour_question()], Arc::new(Store::open_in_memory().unwrap()));
    let id = new_session(&s);
    s.send(&id, "go").await.unwrap();
    wait_status(&s, &id, SessionStatus::WaitingForApproval).await;

    Knobs::set(&knobs.gated);
    let first = {
        let (s, id) = (s.clone(), id.clone());
        tokio::spawn(async move { s.answer_question(&id, "r1", valid_colour_answers()).await })
    };
    tokio::time::timeout(Duration::from_secs(5), knobs.entered.notified())
        .await
        .expect("the first answer reached the agent");

    let e = s.answer_question(&id, "r1", valid_colour_answers()).await.unwrap_err();
    assert!(matches!(e, QuestionError::NotPending(_)), "{e:?}");
    let e = s.decline_question(&id, "r1").await.unwrap_err();
    assert!(matches!(e, QuestionError::NotPending(_)), "{e:?}");
    assert_eq!(answered_events(&s, &id), 0, "nothing is recorded before the agent accepted");

    knobs.release.notify_one();
    first.await.unwrap().unwrap();
    wait_until("QuestionAnswered stored", || answered_events(&s, &id) == 1).await;
    assert_eq!(*log.lock().unwrap(), vec!["q:r1:q0=Red;q1=x".to_string()], "answered once");
    assert_eq!(s.questions_taken_len(), 0);
}

/// FR-A5: a turn end releases the keys of its session, even with an answer still in flight, and a
/// withdrawn question is NotPending, not stuck.
#[tokio::test]
async fn fr_a5_a_turn_end_releases_the_keys_of_its_session() {
    use ember_server::session::QuestionError;
    let (s, _log, knobs) = scripted_sessions_with(vec![colour_question()], Arc::new(Store::open_in_memory().unwrap()));
    let id = new_session(&s);
    s.send(&id, "go").await.unwrap();
    wait_status(&s, &id, SessionStatus::WaitingForApproval).await;

    Knobs::set(&knobs.gated);
    Knobs::set(&knobs.end_turn_when_entered);
    let first = {
        let (s, id) = (s.clone(), id.clone());
        tokio::spawn(async move { s.answer_question(&id, "r1", valid_colour_answers()).await })
    };
    tokio::time::timeout(Duration::from_secs(5), knobs.entered.notified())
        .await
        .expect("the first answer reached the agent");
    wait_until("TurnEnded stored", || {
        stored_kinds(&s, &id).iter().any(|e| matches!(e, AgentEvent::TurnEnded { .. }))
    })
    .await;
    assert_eq!(s.questions_taken_len(), 0, "the turn end released the key while the answer was in flight");
    let e = s.answer_question(&id, "r1", valid_colour_answers()).await.unwrap_err();
    assert!(matches!(e, QuestionError::NotPending(_)), "{e:?}");

    knobs.release.notify_one();
    first.await.unwrap().unwrap();
    wait_until("QuestionAnswered stored", || answered_events(&s, &id) == 1).await;
    assert_eq!(s.questions_taken_len(), 0);
}

/// FR-A5: ending a session releases its keys.
#[tokio::test]
async fn fr_a5_session_end_releases_the_keys_of_its_session() {
    let (s, _log, knobs) = scripted_sessions_with(vec![colour_question()], Arc::new(Store::open_in_memory().unwrap()));
    let id = new_session(&s);
    s.send(&id, "go").await.unwrap();
    wait_status(&s, &id, SessionStatus::WaitingForApproval).await;
    Knobs::set(&knobs.gated);
    let first = {
        let (s, id) = (s.clone(), id.clone());
        tokio::spawn(async move { s.answer_question(&id, "r1", valid_colour_answers()).await })
    };
    tokio::time::timeout(Duration::from_secs(5), knobs.entered.notified())
        .await
        .expect("the first answer reached the agent");
    assert_eq!(s.questions_taken_len(), 1);
    s.session_ended(&id);
    assert_eq!(s.questions_taken_len(), 0);
    knobs.release.notify_one();
    first.await.unwrap().unwrap();
}

fn withdrawn(id: &str) -> AgentEvent {
    AgentEvent::PromptWithdrawn { prompt_id: id.into() }
}

/// Feed items of kind `kind` in the default channel of project `core`.
async fn feed_items(app: &axum::Router, kind: &str) -> Vec<serde_json::Value> {
    let res = app
        .clone()
        .oneshot(Request::get("/api/channels/default-core/feed").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let feed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    feed.as_array().unwrap().iter().filter(|i| i["kind"] == kind).cloned().collect()
}

/// Run `script` in a session of project `core` and wait until the last event of it is stored.
async fn run_withdraw_script(script: Vec<AgentEvent>) -> (Arc<Sessions>, axum::Router, String) {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let n = script.len();
    let (s, _log, _knobs) = scripted_sessions_with(script, store.clone());
    let app = ember_server::channels::api::router(store.clone(), s.clone());
    let id = store
        .create_session("core", AgentKind::Scripted, &std::env::temp_dir().to_string_lossy(), None, "alpha")
        .unwrap()
        .id;
    s.send(&id, "go").await.unwrap();
    // UserMessage plus the script (plus nothing else: no turn end).
    wait_until("the script is stored", || stored_kinds(&s, &id).len() >= n + 1).await;
    (s, app, id)
}

/// FR-A5: a question the agent withdrew (Claude Code `control_cancel_request`) leaves
/// `pending_questions` and the feed while the turn is still running. The sentinel approval after
/// it stays pending, so the absence is observed after the withdrawal was handled.
#[tokio::test]
async fn fr_a5_a_withdrawn_question_leaves_the_pending_list_and_the_feed_while_the_turn_runs() {
    let (s, app, id) =
        run_withdraw_script(vec![colour_question(), withdrawn("r1"), approval("sentinel", "Bash", false)]).await;
    assert!(s.pending_questions(&id).unwrap().is_empty());
    assert!(feed_items(&app, "question").await.is_empty());
    let approvals = feed_items(&app, "approval").await;
    assert_eq!(approvals.len(), 1, "{approvals:?}");
    assert_eq!(approvals[0]["approval_id"], "sentinel");
    assert!(!stored_kinds(&s, &id).iter().any(|e| matches!(e, AgentEvent::TurnEnded { .. })), "the turn is running");
    // Answering the withdrawn question is NotPending.
    let e = s.answer_question(&id, "r1", valid_colour_answers()).await.unwrap_err();
    assert!(matches!(e, ember_server::session::QuestionError::NotPending(_)), "{e:?}");
}

/// FR-A5: the same for a plain approval: withdrawn is neither pending nor denied.
#[tokio::test]
async fn fr_a5_a_withdrawn_approval_leaves_the_feed_while_the_turn_runs() {
    let (s, app, id) = run_withdraw_script(vec![
        approval("a1", "Bash", false),
        withdrawn("a1"),
        approval("sentinel", "Write", false),
    ])
    .await;
    let approvals = feed_items(&app, "approval").await;
    assert_eq!(approvals.len(), 1, "{approvals:?}");
    assert_eq!(approvals[0]["approval_id"], "sentinel");
    assert!(!stored_kinds(&s, &id).iter().any(|e| matches!(
        e,
        AgentEvent::ApprovalResolved { .. } | AgentEvent::TurnEnded { .. }
    )));
}
