//! FR-T8 (#995): completion, failure, needs-input and stall reports reach the delegating session
//! without polling. A puppet agent plays each kind of turn from a keyword in its message:
//!
//! - default: edits `/w/<word>.rs`, replies and ends the turn (done);
//! - `fail`: replies, reports a turn error and ends the turn as failed;
//! - `ask`: asks for an approval and waits; the answer ends the turn;
//! - `hang`: starts a tool call and goes quiet until interrupted (a stall);
//! - `twice`: ends the turn, then continues on its own with a second turn (automatic follow-up).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::a2a::report::ReportTimes;
use ember_server::a2a::{A2a, A2aConfig, A2aStore};
use ember_server::agents::{AgentAdapter, AgentKind, AgentRun, Detected, StartRequest};
use ember_server::events::{AgentEvent, ApprovalDecision, SessionStatus, TurnOutcome};
use ember_server::session::{NewSession, Sessions};
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tokio::sync::{mpsc, Notify};
use tower::ServiceExt;

#[derive(Clone, Default)]
struct Puppet(Arc<Mutex<HashMap<String, StartRequest>>>, Arc<std::sync::atomic::AtomicBool>);

struct PuppetRun {
    /// When set, a turn that receives a report fails (the re-queue test).
    fail_reports: Arc<std::sync::atomic::AtomicBool>,
    events: mpsc::Sender<AgentEvent>,
    interrupt: Arc<Notify>,
    turn: u32,
}

#[async_trait]
impl AgentAdapter for Puppet {
    fn kind(&self) -> AgentKind {
        AgentKind::Scripted
    }
    async fn detect(&self) -> Detected {
        Detected { kind: AgentKind::Scripted, installed: true, version: Some("puppet".into()) }
    }
    async fn start(&self, req: StartRequest, events: mpsc::Sender<AgentEvent>) -> anyhow::Result<Box<dyn AgentRun>> {
        let id = req.env.iter().find(|(k, _)| k == "EMBER_SESSION_ID").map(|(_, v)| v.clone()).unwrap_or_default();
        self.0.lock().unwrap().insert(id, req);
        events.send(AgentEvent::NativeSession { native_id: uuid::Uuid::new_v4().to_string() }).await?;
        Ok(Box::new(PuppetRun { fail_reports: self.1.clone(), events, interrupt: Arc::new(Notify::new()), turn: 0 }))
    }
}

impl Puppet {
    fn token(&self, session: &str) -> Option<String> {
        let seen = self.0.lock().unwrap();
        seen.get(session)?.env.iter().find(|(k, _)| k == "EMBER_RUNTIME_TOKEN").map(|(_, v)| v.clone())
    }
}

async fn done_turn(events: &mpsc::Sender<AgentEvent>, call: String, word: &str) -> anyhow::Result<()> {
    events.send(AgentEvent::ToolCall { call_id: call.clone(), name: "Edit".into(), input: json!({ "file_path": format!("/w/{word}.rs") }) }).await?;
    events.send(AgentEvent::ToolResult { call_id: call, output: "ok".into(), is_error: false }).await?;
    events.send(AgentEvent::AssistantMessage { text: format!("did {word}") }).await?;
    events.send(AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }).await?;
    Ok(())
}

#[async_trait]
impl AgentRun for PuppetRun {
    async fn send(&mut self, text: &str) -> anyhow::Result<()> {
        self.turn += 1;
        let call = format!("call-{}", self.turn);
        let gets_report = text.contains("[ember a2a] Report ");
        // The last line is the instruction (a delivered A2A message carries a header above it).
        let text = text.lines().last().unwrap_or("").to_string();
        let word = text.split_whitespace().next().unwrap_or("x").to_string();
        let ev = self.events.clone();
        if text.contains("fail") || (gets_report && self.fail_reports.load(std::sync::atomic::Ordering::SeqCst)) {
            ev.send(AgentEvent::AssistantMessage { text: "trying".into() }).await?;
            ev.send(AgentEvent::TurnError { code: "boom".into(), message: "boom: the tool crashed".into() }).await?;
            ev.send(AgentEvent::TurnEnded { outcome: TurnOutcome::Failed }).await?;
        } else if text.contains("ask") {
            ev.send(AgentEvent::ToolCall { call_id: call.clone(), name: "Bash".into(), input: json!({ "command": "make" }) }).await?;
            ev.send(AgentEvent::ApprovalRequested { approval_id: format!("approval-{}", self.turn), tool: "Bash".into(), input: json!({ "command": "make" }), needs_person: false }).await?;
        } else if text.contains("hang") {
            ev.send(AgentEvent::ToolCall { call_id: call, name: "Bash".into(), input: json!({ "command": "sleep 1000" }) }).await?;
            let interrupt = self.interrupt.clone();
            tokio::spawn(async move {
                interrupt.notified().await;
                let _ = ev.send(AgentEvent::TurnEnded { outcome: TurnOutcome::Interrupted }).await;
            });
        } else if text.contains("twice") {
            done_turn(&ev, call.clone(), "first").await?;
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(30)).await;
                let _ = done_turn(&ev, format!("{call}-b"), "second").await;
            });
        } else {
            done_turn(&ev, call, &word).await?;
        }
        Ok(())
    }

    async fn answer(&mut self, _approval_id: &str, _decision: ApprovalDecision) -> anyhow::Result<()> {
        done_turn(&self.events, format!("call-{}-a", self.turn), "answered").await
    }

    async fn interrupt(&mut self) -> anyhow::Result<()> {
        self.interrupt.notify_one();
        Ok(())
    }

    async fn shutdown(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

struct World {
    sessions: Arc<Sessions>,
    a2a: Arc<A2a>,
    agents: Puppet,
    app: Router,
}

const SETTLE: Duration = Duration::from_millis(150);

fn world() -> World {
    let agents = Puppet::default();
    let sessions = Sessions::new(Arc::new(Store::open_in_memory().unwrap()), vec![Arc::new(agents.clone()) as Arc<dyn AgentAdapter>]);
    let mut config = A2aConfig::new("http://127.0.0.1:1");
    config.reports = ReportTimes {
        settle: SETTLE,
        batch: Duration::from_millis(150),
        stall: Duration::from_millis(400),
        stall_check: Duration::from_millis(50),
    };
    let a2a = A2a::new(sessions.clone(), A2aStore::open_in_memory().unwrap(), config);
    a2a.install();
    let app = ember_server::api::router(sessions.clone()).merge(ember_server::a2a::api::router(a2a.clone()));
    World { sessions, a2a, agents, app }
}

fn new_session(s: &Sessions, title: &str) -> String {
    s.create(NewSession { project: "acme".into(), agent: AgentKind::Scripted, cwd: std::env::temp_dir(), model: None, title: title.into() })
        .unwrap()
        .id
}

async fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    for _ in 0..1000 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for {what}");
}

fn status(w: &World, id: &str) -> SessionStatus {
    w.sessions.store().session(id).unwrap().unwrap().status
}

fn user_messages(w: &World, id: &str) -> Vec<String> {
    w.sessions
        .store()
        .events_after(id, 0)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            AgentEvent::UserMessage { text } => Some(text),
            _ => None,
        })
        .collect()
}

/// Every report stored for `to` so far, delivered or not.
fn reports_to(w: &World, to: &str) -> Vec<String> {
    let mut all: Vec<String> = user_messages(w, to)
        .iter()
        .flat_map(|m| m.split("\n\n---\n\n").map(String::from).collect::<Vec<_>>())
        .filter(|p| p.starts_with("[ember a2a] Report "))
        .collect();
    all.extend(w.a2a.store().pending_for(to).unwrap().into_iter().filter(|m| m.is_report()).map(|m| format!("(queued) {}", m.text)));
    all
}

fn count(reports: &[String], needle: &str) -> usize {
    reports.iter().filter(|r| r.contains(needle)).count()
}

async fn call(w: &World, who: &str, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let token = w.agents.token(who).unwrap_or_else(|| panic!("session {who} never started"));
    let req = Request::builder().method(method).uri(uri).header("authorization", format!("Bearer {token}"));
    let req = match body {
        Some(b) => req.header("content-type", "application/json").body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let res = w.app.clone().oneshot(req).await.unwrap();
    let st = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (st, if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() })
}

/// A started leader between turns (its first turn reports to nobody).
async fn leader(w: &World) -> String {
    let id = new_session(&w.sessions, "lead");
    w.sessions.send(&id, "start").await.unwrap();
    wait_for("the leader's first turn", || status(w, &id) == SessionStatus::Finished).await;
    id
}

async fn spawn(w: &World, lead: &str, name: &str, prompt: &str) -> String {
    let (st, v) = call(w, lead, "POST", "/api/a2a/team/members", Some(json!({ "name": name, "prompt": prompt }))).await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    v["member"]["session_id"].as_str().unwrap().to_string()
}

/// Let every pending settle, batch and stall timer run out.
async fn quiet() {
    tokio::time::sleep(SETTLE * 6).await;
}

// FR-T8: a done report reaches the leader once, with its fields, wakes the idle leader within
// 5 s into a turn of its own, and that turn reports to nobody (reports do not beget reports).
#[tokio::test]
async fn fr_t8_done_report_arrives_once_and_wakes_the_idle_leader() {
    let w = world();
    let lead = leader(&w).await;
    let alice = spawn(&w, &lead, "alice", "build").await;
    let start = std::time::Instant::now();
    wait_for("the leader to be woken by the report", || user_messages(&w, &lead).iter().any(|m| m.contains("[ember a2a] Report "))).await;
    assert!(start.elapsed() < Duration::from_secs(5));
    quiet().await;
    let got = reports_to(&w, &lead);
    assert_eq!(got.len(), 1, "{got:?}");
    let r = &got[0];
    assert!(r.contains(&format!("from session {alice}")) && r.contains("No reply is needed"), "{r}");
    for field in ["Outcome: done", "Last reply: did build", "Files changed: /w/build.rs", &format!("Transcript: session {alice}, events ")] {
        assert!(r.contains(field), "{field} missing: {r}");
    }
    assert_eq!(status(&w, &lead), SessionStatus::Finished, "the woken leader ran a turn and ended it");
    assert!(reports_to(&w, &alice).is_empty(), "the leader's report-woken turn reported back");
}

// FR-T8: a failed turn's report carries the error, once.
#[tokio::test]
async fn fr_t8_failed_report_carries_the_error_once() {
    let w = world();
    let lead = leader(&w).await;
    spawn(&w, &lead, "bob", "fail please").await;
    wait_for("the failure report", || count(&reports_to(&w, &lead), "Outcome: failed") == 1).await;
    quiet().await;
    let got = reports_to(&w, &lead);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].contains("Error: boom: the tool crashed") && got[0].contains("Last reply: trying"), "{}", got[0]);
}

// FR-T8: an approval left open past the settle time is reported once as needs input; the turn's
// end after the answer is reported as done.
#[tokio::test]
async fn fr_t8_needs_input_is_reported_once_then_the_done_turn() {
    let w = world();
    let lead = leader(&w).await;
    let carol = spawn(&w, &lead, "carol", "ask first").await;
    wait_for("the needs-input report", || count(&reports_to(&w, &lead), "Outcome: needs input") == 1).await;
    let got = reports_to(&w, &lead);
    assert!(got[0].contains("Waiting on: approval for Bash"), "{}", got[0]);
    quiet().await;
    assert_eq!(count(&reports_to(&w, &lead), "Outcome: needs input"), 1);
    w.sessions.answer(&carol, "approval-1", ApprovalDecision::AllowOnce).await.unwrap();
    wait_for("the done report", || count(&reports_to(&w, &lead), "Outcome: done") == 1).await;
    quiet().await;
    let got = reports_to(&w, &lead);
    assert_eq!(got.len(), 2, "{got:?}");
}

// FR-T8: a running teammate with no event for the stall time is reported once per stall.
#[tokio::test]
async fn fr_t8_stall_is_reported_once() {
    let w = world();
    let lead = leader(&w).await;
    let dave = spawn(&w, &lead, "dave", "hang on").await;
    wait_for("the stall report", || count(&reports_to(&w, &lead), "Outcome: stalled") == 1).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let got = reports_to(&w, &lead);
    assert_eq!(count(&got, "Outcome: stalled"), 1, "{got:?}");
    assert!(got.iter().any(|r| r.contains("No event for ") && r.contains(" while running")), "{got:?}");
    w.sessions.interrupt(&dave).await.unwrap();
    wait_for("the interrupted report", || count(&reports_to(&w, &lead), "Outcome: interrupted") == 1).await;
}

// FR-T8: a session nobody delegated to sends no report.
#[tokio::test]
async fn fr_t8_no_report_without_a_delegator() {
    let w = world();
    let solo = new_session(&w.sessions, "solo");
    w.sessions.send(&solo, "work").await.unwrap();
    wait_for("the turn", || status(&w, &solo) == SessionStatus::Finished).await;
    quiet().await;
    assert_eq!(w.a2a.store().sent_since(&solo, 0).unwrap(), 0);
}

// FR-T8, deviation [leader, 2026-11-22] (#997): reports to a busy leader wait, and its turn end
// delivers them together in one turn.
#[tokio::test]
async fn fr_t8_reports_to_a_busy_leader_arrive_together_at_its_turn_end() {
    let w = world();
    let lead = leader(&w).await;
    let a = spawn(&w, &lead, "a", "one").await;
    let b = spawn(&w, &lead, "b", "two").await;
    // Before either report is delivered, the leader starts a long turn.
    w.sessions.send(&lead, "hang here").await.unwrap();
    wait_for("both reports queued", || {
        let q = w.a2a.store().pending_for(&lead).unwrap();
        q.iter().filter(|m| m.is_report()).count() == 2
    })
    .await;
    let before = user_messages(&w, &lead).len();
    w.sessions.interrupt(&lead).await.unwrap();
    wait_for("the reports delivered", || user_messages(&w, &lead).len() == before + 1).await;
    let last = user_messages(&w, &lead).pop().unwrap();
    assert!(last.contains(&format!("from session {a}")) && last.contains(&format!("from session {b}")), "{last}");
    assert_eq!(last.matches("[ember a2a] Report ").count(), 2, "{last}");
}

// FR-T8: messages back and forth over several rounds form no report loop: the teammate reports
// each of its turns to the leader, the leader's report-woken turns report to nobody.
#[tokio::test]
async fn fr_t8_no_report_reply_loop_over_repeated_turns() {
    let w = world();
    let lead = leader(&w).await;
    let alice = spawn(&w, &lead, "alice", "round0").await;
    wait_for("report 1", || count(&reports_to(&w, &lead), "Outcome: done") == 1).await;
    for round in 1..=3 {
        wait_for("both idle", || status(&w, &lead) != SessionStatus::Running && status(&w, &alice) != SessionStatus::Running).await;
        let (st, v) = call(&w, &lead, "POST", "/api/a2a/messages", Some(json!({ "to": alice, "text": format!("round{round}") }))).await;
        assert!(st.is_success(), "{v}");
        wait_for("the next report", || count(&reports_to(&w, &lead), "Outcome: done") == round + 1).await;
    }
    quiet().await;
    assert_eq!(count(&reports_to(&w, &lead), "Outcome: done"), 4);
    assert!(reports_to(&w, &alice).is_empty(), "the teammate got a report");
}

// FR-T8: an automatic follow-up within the settle time reports once at its real end; turns
// further apart report once each.
#[tokio::test]
async fn fr_t8_a_burst_of_short_turns_reports_once_per_real_turn_end() {
    let w = world();
    let lead = leader(&w).await;
    let alice = spawn(&w, &lead, "alice", "twice over").await;
    wait_for("one report", || count(&reports_to(&w, &lead), "Outcome: done") == 1).await;
    quiet().await;
    let got = reports_to(&w, &lead);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].contains("Last reply: did second") && got[0].contains("Files changed: /w/first.rs, /w/second.rs"), "{}", got[0]);
    for round in 1..=2 {
        wait_for("idle", || status(&w, &alice) == SessionStatus::Finished && status(&w, &lead) != SessionStatus::Running).await;
        w.sessions.send(&alice, &format!("again{round}")).await.unwrap();
        wait_for("the report", || count(&reports_to(&w, &lead), "Outcome: done") == round + 1).await;
        quiet().await;
    }
    assert_eq!(count(&reports_to(&w, &lead), "Outcome: done"), 3);
}

// FR-T8 (4) [leader, 2026-11-22]: a report delivered into a turn that fails is queued again and
// delivered once more, and only once.
#[tokio::test]
async fn fr_t8_a_report_into_a_failed_turn_is_delivered_once_more() {
    let w = world();
    let lead = leader(&w).await;
    w.agents.1.store(true, std::sync::atomic::Ordering::SeqCst);
    spawn(&w, &lead, "alice", "work").await;
    let delivered = |w: &World| user_messages(w, &lead).iter().filter(|m| m.contains("[ember a2a] Report ")).count();
    wait_for("the second delivery", || delivered(&w) == 2).await;
    quiet().await;
    assert_eq!(delivered(&w), 2, "delivered again once, not more");
    assert!(w.a2a.store().pending_for(&lead).unwrap().is_empty());
    assert_eq!(status(&w, &lead), SessionStatus::Failed);
}
