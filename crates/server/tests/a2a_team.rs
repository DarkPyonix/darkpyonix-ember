//! Agent teams (FR-T7) and composer mentions (FR-T6) against the scripted agent.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::a2a::{A2a, A2aConfig, A2aStore, Limits};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind, AgentRun, Detected, StartRequest};
use ember_server::events::{AgentEvent, ApprovalDecision, SessionStatus};
use ember_server::session::{NewSession, Push, Sessions};
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tower::ServiceExt;

/// The scripted agent, remembering each process's start request by `EMBER_SESSION_ID`.
#[derive(Clone, Default)]
struct Recording(Arc<Mutex<HashMap<String, StartRequest>>>);

#[async_trait]
impl AgentAdapter for Recording {
    fn kind(&self) -> AgentKind {
        AgentKind::Scripted
    }
    async fn detect(&self) -> Detected {
        ScriptedAdapter.detect().await
    }
    async fn start(
        &self,
        req: StartRequest,
        events: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<Box<dyn AgentRun>> {
        let id = req
            .env
            .iter()
            .find(|(k, _)| k == "EMBER_SESSION_ID")
            .map(|(_, v)| v.clone());
        self.0.lock().unwrap().insert(id.unwrap_or_default(), req.clone());
        ScriptedAdapter.start(req, events).await
    }
}

impl Recording {
    fn token(&self, session: &str) -> String {
        let seen = self.0.lock().unwrap();
        let req = seen
            .get(session)
            .unwrap_or_else(|| panic!("session {session} never started"));
        req.env
            .iter()
            .find(|(k, _)| k == "EMBER_RUNTIME_TOKEN")
            .map(|(_, v)| v.clone())
            .unwrap()
    }
    fn instructions(&self, session: &str) -> String {
        self.0.lock().unwrap().get(session).and_then(|r| r.instructions.clone()).unwrap_or_default()
    }
}

struct World {
    sessions: Arc<Sessions>,
    a2a: Arc<A2a>,
    agents: Recording,
    app: Router,
}

fn world_with(limits: Limits) -> World {
    let agents = Recording::default();
    let sessions = Sessions::new(
        Arc::new(Store::open_in_memory().unwrap()),
        vec![Arc::new(agents.clone()) as Arc<dyn AgentAdapter>],
    );
    let mut config = A2aConfig::new("http://127.0.0.1:1");
    config.limits = limits;
    let a2a = A2a::new(sessions.clone(), A2aStore::open_in_memory().unwrap(), config);
    a2a.install();
    let app = ember_server::api::router(sessions.clone())
        .merge(ember_server::a2a::api::router(a2a.clone()));
    World { sessions, a2a, agents, app }
}

fn world() -> World {
    world_with(Limits::default())
}

fn new_session(s: &Sessions, title: &str) -> String {
    s.create(NewSession {
        project: "acme".into(),
        agent: AgentKind::Scripted,
        cwd: std::env::temp_dir(),
        model: None,
        title: title.into(),
    })
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

async fn wait_status(s: &Sessions, id: &str, want: SessionStatus) {
    wait_for(&format!("{id} to be {want:?}"), || {
        s.store().session(id).unwrap().unwrap().status == want
    })
    .await;
}

fn user_messages(s: &Sessions, id: &str) -> Vec<String> {
    s.store()
        .events_after(id, 0)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            AgentEvent::UserMessage { text } => Some(text),
            _ => None,
        })
        .collect()
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

/// Everything sent to `id` through A2A so far, delivered or still queued.
fn inbox(w: &World, id: &str) -> String {
    let mut all = user_messages(&w.sessions, id);
    all.extend(w.a2a.store().pending_for(id).unwrap().into_iter().map(|m| m.text));
    all.join("\n---\n")
}

/// Run one scripted turn to completion.
async fn finish_turn(s: &Arc<Sessions>, id: &str, turn: u32) {
    wait_status(s, id, SessionStatus::WaitingForApproval).await;
    s.answer(id, &format!("approval-{turn}"), ApprovalDecision::AllowOnce).await.unwrap();
    wait_status(s, id, SessionStatus::Finished).await;
}

async fn call(app: &Router, method: &str, uri: &str, token: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let req = match body {
        Some(b) => req.header("content-type", "application/json").body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
    (status, v)
}

/// Call an agent route as session `who`.
async fn agent(w: &World, who: &str, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let token = w.agents.token(who);
    call(&w.app, method, uri, Some(&token), body).await
}

/// A started leader session (one finished turn, so it has a token and is between turns).
async fn leader(w: &World, title: &str) -> String {
    let id = new_session(&w.sessions, title);
    w.sessions.send(&id, "start").await.unwrap();
    finish_turn(&w.sessions, &id, 1).await;
    id
}

/// Spawn `name` for `lead`; returns the teammate's session id.
async fn spawn(w: &World, lead: &str, name: &str, prompt: &str) -> String {
    let (st, v) = agent(w, lead, "POST", "/api/v1/a2a/team/members", Some(json!({ "name": name, "prompt": prompt }))).await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    v["member"]["session_id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn a_leader_spawns_a_teammate_that_keeps_its_own_approvals() {
    let w = world();
    let lead = leader(&w, "lead").await;
    let mut pushes = w.sessions.subscribe();

    let (st, v) = agent(
        &w,
        &lead,
        "POST",
        "/api/v1/a2a/team/members",
        Some(json!({ "name": "alice", "prompt": "write the tests" })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    assert_eq!(v["member"]["name"], "alice");
    assert_eq!(v["member"]["role"], "teammate");
    assert_eq!(v["message"]["status"], "delivered");
    let team_id = v["team_id"].as_str().unwrap().to_string();
    let alice = v["member"]["session_id"].as_str().unwrap().to_string();

    // Same project and agent; its own title, no model and no account inherited.
    let rec = w.sessions.store().session(&alice).unwrap().unwrap();
    assert_eq!((rec.project.as_str(), rec.agent, rec.title.as_str()), ("acme", AgentKind::Scripted, "alice \u{b7} lead"));
    assert_eq!((rec.model, rec.account_id), (None, None));

    // The first prompt arrives as an A2A message from the leader.
    wait_for("first prompt", || user_messages(&w.sessions, &alice).len() == 1).await;
    let got = &user_messages(&w.sessions, &alice)[0];
    assert!(got.starts_with("[ember a2a] Message") && got.contains(&format!("from session {lead} \"lead\"")), "{got}");
    assert!(got.ends_with("\n\nwrite the tests"), "{got}");
    // Its instructions name its role; the leader's do not.
    assert!(w.agents.instructions(&alice).contains("You are teammate `alice`"));
    assert!(w.agents.instructions(&alice).contains("ember-a2a team spawn"));
    assert!(!w.agents.instructions(&lead).contains("# Your team"));

    // The teammate's approval is its own: the leader is untouched.
    wait_status(&w.sessions, &alice, SessionStatus::WaitingForApproval).await;
    assert_eq!(w.sessions.store().session(&lead).unwrap().unwrap().status, SessionStatus::Finished);
    let lead_approvals = w
        .sessions
        .store()
        .events_after(&lead, 0)
        .unwrap()
        .into_iter()
        .filter(|e| matches!(e.event, AgentEvent::ApprovalRequested { .. }))
        .count();
    assert_eq!(lead_approvals, 1, "only the leader's own first turn");
    finish_turn(&w.sessions, &alice, 1).await;

    // Visible through the agent and user APIs.
    let (st, team) = agent(&w, &lead, "GET", "/api/v1/a2a/team", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(team["id"], team_id.as_str());
    assert_eq!(team["leader"], lead.as_str());
    let names: Vec<&str> = team["members"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["lead", "alice"]);
    let (_, by_session) = call(&w.app, "GET", &format!("/api/v1/sessions/{alice}/team"), None, None).await;
    assert_eq!(by_session["id"], team_id.as_str());
    let (_, by_id) = call(&w.app, "GET", &format!("/api/v1/teams/{team_id}"), None, None).await;
    assert_eq!(by_id["members"].as_array().unwrap().len(), 2);
    let outsider = new_session(&w.sessions, "outsider");
    let (st, none) = call(&w.app, "GET", &format!("/api/v1/sessions/{outsider}/team"), None, None).await;
    assert_eq!((st, none), (StatusCode::OK, Value::Null));

    // And pushed.
    let mut pushed = None;
    while let Ok(p) = pushes.try_recv() {
        if let Push::TeamUpdated { team, .. } = p {
            pushed = Some(team);
        }
    }
    let pushed = pushed.expect("a team_updated push");
    assert_eq!(pushed.id, team_id);
    assert_eq!(pushed.members.len(), 2);
}

#[tokio::test]
async fn only_the_leader_spawns_and_ends_teammates() {
    let w = world();
    let lead = leader(&w, "lead").await;
    let alice = spawn(&w, &lead, "alice", "hello").await;
    let outsider = leader(&w, "outsider").await;

    // A teammate cannot spawn or end.
    let (st, v) = agent(&w, &alice, "POST", "/api/v1/a2a/team/members", Some(json!({ "name": "bob", "prompt": "x" }))).await;
    assert_eq!((st, v["code"].as_str()), (StatusCode::FORBIDDEN, Some("forbidden")), "{v}");
    let (st, v) = agent(&w, &alice, "DELETE", "/api/v1/a2a/team/members/alice", None).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    // Someone outside the team cannot end its members.
    let (st, v) = agent(&w, &outsider, "DELETE", "/api/v1/a2a/team/members/alice", None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");

    // Bad names and agents.
    for (body, why) in [
        (json!({ "name": "lead", "prompt": "x" }), "reserved"),
        (json!({ "name": "has space", "prompt": "x" }), "space"),
        (json!({ "name": "alice", "prompt": "x" }), "taken"),
        (json!({ "name": "carol", "prompt": "  " }), "empty prompt"),
        (json!({ "name": "carol", "prompt": "x", "agent": "nope" }), "agent"),
    ] {
        let (st, v) = agent(&w, &lead, "POST", "/api/v1/a2a/team/members", Some(body)).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{why}: {v}");
    }
    let (st, v) = agent(
        &w,
        &lead,
        "POST",
        "/api/v1/a2a/team/members",
        Some(json!({ "name": "carol", "prompt": "x", "computer": "studio" })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "no computer setter installed: {v}");
    assert!(v["error"].as_str().unwrap().contains("archived"), "{v}");

    // The leader ends alice: her agent stops, she leaves the team, the session stays.
    wait_status(&w.sessions, &alice, SessionStatus::WaitingForApproval).await;
    let (st, v) = agent(&w, &lead, "DELETE", "/api/v1/a2a/team/members/alice", None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["name"], "alice");
    assert!(!v["ended_at"].is_null());
    assert!(!w.sessions.is_live(&alice).await);
    assert!(notices(&w.sessions, &alice).iter().any(|n| n.contains("ended by the team leader")));
    let (st, _) = agent(&w, &alice, "GET", "/api/v1/a2a/team/tasks", None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = agent(&w, &lead, "DELETE", "/api/v1/a2a/team/members/alice", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // The name is free again.
    let alice2 = spawn(&w, &lead, "alice", "second try").await;
    assert_ne!(alice2, alice);

    // The user can end a teammate too.
    let team_id = w.a2a.team_of(&lead).unwrap().unwrap().id;
    let (st, v) = call(&w.app, "POST", &format!("/api/v1/teams/{team_id}/members/{alice2}/end"), None, None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, _) = call(&w.app, "POST", &format!("/api/v1/teams/{team_id}/members/{lead}/end"), None, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "the leader is not a teammate");
}

#[tokio::test]
async fn tasks_are_shared_and_members_update_their_own() {
    let w = world();
    let lead = leader(&w, "lead").await;
    let alice = spawn(&w, &lead, "alice", "hi").await;
    let bob = spawn(&w, &lead, "bob", "hi").await;

    // The leader assigns; the assignee is told.
    let (st, t1) = agent(&w, &lead, "POST", "/api/v1/a2a/team/tasks", Some(json!({ "title": "write tests", "assignee": "alice" }))).await;
    assert_eq!(st, StatusCode::CREATED, "{t1}");
    assert_eq!((t1["number"].as_i64(), t1["status"].as_str(), t1["assignee_name"].as_str()), (Some(1), Some("open"), Some("alice")));
    assert!(inbox(&w, &alice).contains("Task #1 was assigned to you by lead: write tests"));

    // A teammate adds tasks, but assigns only to itself.
    let (st, v) = agent(&w, &alice, "POST", "/api/v1/a2a/team/tasks", Some(json!({ "title": "x", "assignee": "bob" }))).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    let (st, t2) = agent(&w, &alice, "POST", "/api/v1/a2a/team/tasks", Some(json!({ "title": "refactor", "detail": "the parser" }))).await;
    assert_eq!((st, t2["number"].as_i64()), (StatusCode::CREATED, Some(2)));
    let (st, v) = agent(&w, &alice, "PATCH", "/api/v1/a2a/team/tasks/2", Some(json!({ "assignee": "alice", "status": "in_progress" }))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!((v["assignee"].as_str(), v["status"].as_str()), (Some(alice.as_str()), Some("in_progress")));

    // Alice finishes task 1: the leader hears about it.
    let (st, v) = agent(&w, &alice, "PATCH", "/api/v1/a2a/team/tasks/1", Some(json!({ "status": "done" }))).await;
    assert_eq!((st, v["status"].as_str()), (StatusCode::OK, Some("done")), "{v}");
    assert!(inbox(&w, &lead).contains("Task #1 \"write tests\" is now done (updated by teammate alice)."), "{}", inbox(&w, &lead));

    // Bob cannot touch alice's tasks; the leader can.
    let (st, _) = agent(&w, &bob, "PATCH", "/api/v1/a2a/team/tasks/1", Some(json!({ "status": "open" }))).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = agent(&w, &bob, "PATCH", "/api/v1/a2a/team/tasks/2", Some(json!({ "assignee": "bob" }))).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, v) = agent(&w, &lead, "PATCH", "/api/v1/a2a/team/tasks/2", Some(json!({ "assignee": "bob" }))).await;
    assert_eq!((st, v["assignee_name"].as_str()), (StatusCode::OK, Some("bob")));
    assert!(inbox(&w, &bob).contains("Task #2 was assigned to you by lead: refactor\n\nthe parser"));
    let (st, v) = agent(&w, &lead, "PATCH", "/api/v1/a2a/team/tasks/2", Some(json!({ "assignee": "none" }))).await;
    assert_eq!((st, v["assignee"].clone()), (StatusCode::OK, Value::Null));

    // Errors.
    let (st, _) = agent(&w, &lead, "PATCH", "/api/v1/a2a/team/tasks/1", Some(json!({ "status": "finished" }))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = agent(&w, &lead, "PATCH", "/api/v1/a2a/team/tasks/9", Some(json!({ "status": "done" }))).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = agent(&w, &lead, "POST", "/api/v1/a2a/team/tasks", Some(json!({ "title": " " }))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // Everyone sees the same list, and so does the user.
    let (_, tasks) = agent(&w, &bob, "GET", "/api/v1/a2a/team/tasks", None).await;
    let tasks = tasks.as_array().unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0]["status"], "done");
    let view = w.a2a.team_of(&bob).unwrap().unwrap();
    assert_eq!(view.tasks.len(), 2);
    // Outside the team there are no tasks.
    let outsider = leader(&w, "outsider").await;
    let (st, _) = agent(&w, &outsider, "GET", "/api/v1/a2a/team/tasks", None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn mail_reaches_members_and_the_mailbox() {
    let w = world();
    let lead = leader(&w, "lead").await;
    let alice = spawn(&w, &lead, "alice", "hi").await;
    let bob = spawn(&w, &lead, "bob", "hi").await;
    for id in [&alice, &bob] {
        finish_turn(&w.sessions, id, 1).await;
    }

    let (st, r) = agent(&w, &lead, "POST", "/api/v1/a2a/team/mail", Some(json!({ "text": "standup in 5" }))).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{r}");
    let n = r["mail"]["id"].as_i64().unwrap();
    let deliveries = r["deliveries"].as_array().unwrap();
    assert_eq!(deliveries.len(), 2);
    assert!(deliveries.iter().all(|d| d["status"] == "delivered"), "{r}");
    for id in [&alice, &bob] {
        wait_for("broadcast", || user_messages(&w.sessions, id).len() == 2).await;
        let got = &user_messages(&w.sessions, id)[1];
        assert!(got.contains(&format!("Team mail #{n} from lead to the whole team:\n\nstandup in 5")), "{got}");
        assert!(got.starts_with("[ember a2a]"), "a reply reference comes with it: {got}");
    }

    let (st, r) = agent(&w, &alice, "POST", "/api/v1/a2a/team/mail", Some(json!({ "to": "lead", "text": "tests are green" }))).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{r}");
    wait_for("mail to the leader", || inbox(&w, &lead).contains("from alice to you:\n\ntests are green")).await;
    let (st, _) = agent(&w, &bob, "POST", "/api/v1/a2a/team/mail", Some(json!({ "to": "alice", "text": "can you review?" }))).await;
    assert_eq!(st, StatusCode::ACCEPTED);

    // Mailboxes: the leader sees everything; teammates see team-wide mail and their own.
    let texts = |v: &Value| -> Vec<String> { v.as_array().unwrap().iter().map(|m| m["text"].as_str().unwrap().to_string()).collect() };
    let (_, all) = agent(&w, &lead, "GET", "/api/v1/a2a/team/mail", None).await;
    assert_eq!(texts(&all), vec!["standup in 5", "tests are green", "can you review?"]);
    assert_eq!(all[1]["from_name"], "alice");
    assert_eq!(all[1]["to_name"], "lead");
    let (_, a) = agent(&w, &alice, "GET", "/api/v1/a2a/team/mail", None).await;
    assert_eq!(texts(&a).len(), 3);
    let (_, b) = agent(&w, &bob, "GET", "/api/v1/a2a/team/mail", None).await;
    assert_eq!(texts(&b), vec!["standup in 5", "can you review?"]);
    let (_, after) = agent(&w, &lead, "GET", &format!("/api/v1/a2a/team/mail?after={n}&limit=1"), None).await;
    assert_eq!(texts(&after), vec!["tests are green"]);
    let team_id = w.a2a.team_of(&lead).unwrap().unwrap().id;
    let (_, user) = call(&w.app, "GET", &format!("/api/v1/teams/{team_id}/mail"), None, None).await;
    assert_eq!(texts(&user).len(), 3);

    // Errors.
    let (st, _) = agent(&w, &alice, "POST", "/api/v1/a2a/team/mail", Some(json!({ "to": "alice", "text": "me" }))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = agent(&w, &alice, "POST", "/api/v1/a2a/team/mail", Some(json!({ "to": "zed", "text": "?" }))).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn teammates_are_reachable_only_from_their_team() {
    let w = world();
    let lead = leader(&w, "lead").await;
    let alice = spawn(&w, &lead, "alice", "hi").await;
    let other = leader(&w, "other").await;
    let ids = |v: &Value| -> Vec<String> { v.as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap().to_string()).collect() };

    let (_, t) = agent(&w, &other, "GET", "/api/v1/a2a/targets", None).await;
    assert!(ids(&t).contains(&lead) && !ids(&t).contains(&alice), "{t}");
    let (_, t) = agent(&w, &lead, "GET", "/api/v1/a2a/targets", None).await;
    assert!(ids(&t).contains(&alice) && ids(&t).contains(&other));
    let (_, t) = agent(&w, &alice, "GET", "/api/v1/a2a/targets", None).await;
    assert_eq!(ids(&t), vec![lead.clone()]);

    let msg = |to: &str| json!({ "to": to, "text": "hi" });
    let (st, v) = agent(&w, &other, "POST", "/api/v1/a2a/messages", Some(msg(&alice))).await;
    assert_eq!((st, v["code"].as_str()), (StatusCode::FORBIDDEN, Some("forbidden")));
    assert!(v["error"].as_str().unwrap().contains("only its team can message it"), "{v}");
    let (st, v) = agent(&w, &alice, "POST", "/api/v1/a2a/messages", Some(msg(&other))).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert!(v["error"].as_str().unwrap().contains("you can message only your team"), "{v}");
    let (st, _) = agent(&w, &lead, "POST", "/api/v1/a2a/messages", Some(msg(&alice))).await;
    assert_eq!(st, StatusCode::ACCEPTED);
    let (st, _) = agent(&w, &alice, "POST", "/api/v1/a2a/messages", Some(msg(&lead))).await;
    assert_eq!(st, StatusCode::ACCEPTED);

    // Ended, alice is an ordinary session again.
    agent(&w, &lead, "DELETE", "/api/v1/a2a/team/members/alice", None).await;
    let (_, t) = agent(&w, &other, "GET", "/api/v1/a2a/targets", None).await;
    assert!(ids(&t).contains(&alice));
}

#[tokio::test]
async fn team_traffic_is_loop_protected() {
    // FR-T5 applies to teams: a broadcast counts once per recipient, mail ping-pong stops.
    let w = world_with(Limits { window: Duration::from_secs(600), per_session: 4, per_pair: 3 });
    let lead = leader(&w, "lead").await;
    let alice = spawn(&w, &lead, "alice", "hi").await; // lead has sent 1
    let _bob = spawn(&w, &lead, "bob", "hi").await; // 2
    let (st, r) = agent(&w, &lead, "POST", "/api/v1/a2a/team/mail", Some(json!({ "text": "one" }))).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{r}"); // 4
    let (st, r) = agent(&w, &lead, "POST", "/api/v1/a2a/team/mail", Some(json!({ "text": "two" }))).await;
    assert_eq!((st, r["code"].as_str()), (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited")), "{r}");
    assert!(notices(&w.sessions, &lead).iter().any(|n| n.contains("loop protection")));
    // A refused broadcast stores nothing.
    let (_, mail) = agent(&w, &lead, "GET", "/api/v1/a2a/team/mail", None).await;
    assert_eq!(mail.as_array().unwrap().len(), 1);

    // alice <-> lead: prompt (1) + mail one (2) + alice's reply (3), then the pair limit.
    let (st, r) = agent(&w, &alice, "POST", "/api/v1/a2a/team/mail", Some(json!({ "to": "lead", "text": "ack" }))).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{r}");
    let (st, r) = agent(&w, &alice, "POST", "/api/v1/a2a/team/mail", Some(json!({ "to": "lead", "text": "ack again" }))).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "{r}");
    assert!(r["error"].as_str().unwrap().contains("have exchanged 3"), "{r}");

    // Task notices under the limit are skipped, the task change itself is kept.
    let (st, t) = agent(&w, &lead, "POST", "/api/v1/a2a/team/tasks", Some(json!({ "title": "t", "assignee": "alice" }))).await;
    assert_eq!(st, StatusCode::CREATED, "{t}");
    assert!(!inbox(&w, &alice).contains("Task #1 was assigned"));
}

#[tokio::test]
async fn a_mention_delivers_a_copy_with_the_users_text() {
    // FR-T6: the message stays here; the mentioned session gets it as A2A from this session.
    let w = world();
    let a = leader(&w, "alpha").await;
    let b = new_session(&w.sessions, "Beta Tests");
    let c = new_session(&w.sessions, "gamma");

    let (_, cands) = call(&w.app, "GET", &format!("/api/v1/sessions/{a}/mentions?q=bet"), None, None).await;
    let cands = cands.as_array().unwrap();
    assert_eq!(cands.len(), 1);
    assert_eq!(cands[0]["id"], b.as_str());
    let (_, all) = call(&w.app, "GET", &format!("/api/v1/sessions/{a}/mentions"), None, None).await;
    assert_eq!(all.as_array().unwrap().len(), 2);
    let prefix = &c[..8];
    let (_, by_id) = call(&w.app, "GET", &format!("/api/v1/sessions/{a}/mentions?q={prefix}"), None, None).await;
    assert_eq!(by_id[0]["id"], c.as_str());

    let text = "please look at this @@\"beta tests\", and @@nobody";
    let (st, _) = call(&w.app, "POST", &format!("/api/v1/sessions/{a}/messages"), None, Some(json!({ "text": text }))).await;
    assert_eq!(st, StatusCode::ACCEPTED);
    // The API accepts the message and records it asynchronously.
    wait_for("the user's message, unchanged", || user_messages(&w.sessions, &a).last().is_some_and(|m| m == text)).await;
    wait_for("mention delivery", || user_messages(&w.sessions, &b).len() == 1).await;
    let got = &user_messages(&w.sessions, &b)[0];
    assert!(got.starts_with(&format!("[ember a2a] Message msg_")) && got.contains(&format!("from session {a} \"alpha\"")), "{got}");
    assert!(got.contains(&format!("The user mentioned this session in session {a} (\"alpha\") and wrote:\n\n{text}")), "{got}");
    assert!(got.contains(&format!("ember-a2a send {a} --reply-to")), "{got}");
    let n = notices(&w.sessions, &a);
    assert!(n.iter().any(|m| m.contains("Mentioned session \"Beta Tests\"") && m.contains("delivered")), "{n:?}");
    assert!(n.iter().any(|m| m.contains("no session you can message matches @@nobody")), "{n:?}");
    assert!(user_messages(&w.sessions, &c).is_empty());

    // A plain message mentions nobody.
    finish_turn(&w.sessions, &a, 2).await;
    let before = notices(&w.sessions, &a).len();
    call(&w.app, "POST", &format!("/api/v1/sessions/{a}/messages"), None, Some(json!({ "text": "mail me@@x" }))).await;
    assert_eq!(notices(&w.sessions, &a).len(), before);
}

#[tokio::test]
async fn mentions_follow_switches_and_team_scope_but_not_rate_limits() {
    let w = world_with(Limits { window: Duration::from_secs(600), per_session: 1, per_pair: 1 });
    let a = leader(&w, "alpha").await;
    let b = new_session(&w.sessions, "beta");
    let c = new_session(&w.sessions, "gamma");

    // Two mentions over the per-session limit of one: a person sent them, both go through.
    let text = format!("@@beta and @@{}", &c[..8]);
    call(&w.app, "POST", &format!("/api/v1/sessions/{a}/messages"), None, Some(json!({ "text": text }))).await;
    wait_for("both mentions", || user_messages(&w.sessions, &b).len() == 1 && user_messages(&w.sessions, &c).len() == 1).await;
    // They count toward the window: the agent's own send is now refused.
    let (st, _) = agent(&w, &a, "POST", "/api/v1/a2a/messages", Some(json!({ "to": b, "text": "x" }))).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);

    // A teammate of another team cannot be mentioned from outside it.
    let w = world();
    let lead = leader(&w, "lead").await;
    spawn(&w, &lead, "alice", "hi").await;
    let x = leader(&w, "x").await;
    let (_, cands) = call(&w.app, "GET", &format!("/api/v1/sessions/{x}/mentions?q=alice"), None, None).await;
    assert_eq!(cands, json!([]));
    call(&w.app, "POST", &format!("/api/v1/sessions/{x}/messages"), None, Some(json!({ "text": "@@\"alice \u{b7} lead\"" }))).await;
    assert!(notices(&w.sessions, &x).iter().any(|n| n.contains("no session you can message matches")));

    // A2A off for the session: its mentions are not delivered, with the reason.
    finish_turn(&w.sessions, &x, 2).await;
    call(&w.app, "PUT", &format!("/api/v1/sessions/{x}/a2a"), None, Some(json!({ "enabled": false }))).await;
    call(&w.app, "POST", &format!("/api/v1/sessions/{x}/messages"), None, Some(json!({ "text": "@@lead" }))).await;
    assert!(notices(&w.sessions, &x).iter().any(|n| n.starts_with("Mentions were not delivered") && n.contains("turned off")));
    let (st, _) = call(&w.app, "GET", &format!("/api/v1/sessions/{x}/mentions"), None, None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_cli_runs_a_team_over_http() {
    let w = world();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = w.app.clone();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let lead = leader(&w, "lead").await;
    let token = w.agents.token(&lead);
    let cli = |args: &[&str]| {
        tokio::process::Command::new(env!("CARGO_BIN_EXE_ember-a2a"))
            .args(args)
            .env("EMBER_URL", &url)
            .env("EMBER_RUNTIME_TOKEN", &token)
            .stdin(std::process::Stdio::null())
            .output()
    };
    let ok = |out: &std::process::Output| {
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    let out = ok(&cli(&["team", "list"]).await.unwrap());
    assert!(out.contains("Not in a team"), "{out}");
    let out = ok(&cli(&["team", "spawn", "alice", "--title", "Alice", "write", "the", "tests"]).await.unwrap());
    assert!(out.contains("Spawned teammate alice") && out.contains("delivered"), "{out}");
    let alice = w.a2a.team_of(&lead).unwrap().unwrap().members[1].session_id.clone();
    assert_eq!(w.sessions.store().session(&alice).unwrap().unwrap().title, "Alice");
    let out = ok(&cli(&["task", "add", "write", "tests", "--assign", "alice"]).await.unwrap());
    assert!(out.contains("#1  [open]  write tests  (alice)"), "{out}");
    let out = ok(&cli(&["task", "update", "#1", "--status", "blocked"]).await.unwrap());
    assert!(out.contains("[blocked]"), "{out}");
    let out = ok(&cli(&["team", "list"]).await.unwrap());
    assert!(out.contains("alice  teammate") && out.contains("#1  [blocked]"), "{out}");
    let out = ok(&cli(&["mail", "send", "--all", "standup"]).await.unwrap());
    assert!(out.contains("to the whole team (1 recipient)"), "{out}");
    let out = ok(&cli(&["mail", "read"]).await.unwrap());
    assert!(out.contains("lead -> everyone:\nstandup"), "{out}");
    let out = ok(&cli(&["team", "end", "alice"]).await.unwrap());
    assert!(out.contains("Ended teammate alice"), "{out}");

    let out = cli(&["task", "update", "1"]).await.unwrap();
    assert_eq!(out.status.code(), Some(2), "nothing to update is a usage error");
    let out = cli(&["team", "spawn", "bob", "--bogus", "x"]).await.unwrap();
    assert_eq!(out.status.code(), Some(2));
}
