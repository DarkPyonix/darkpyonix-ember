//! Central MCP registry (FR-A7) and scheduled tasks (FR-A8) against the scripted agent.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::a2a::{A2a, A2aConfig, A2aStore};
use ember_server::accounts::secrets::SecretBox;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind, AgentRun, Detected, StartRequest};
use ember_server::events::{AgentEvent, ApprovalDecision, SessionStatus};
use ember_server::mcp::McpRegistry;
use ember_server::schedules::{
    ManualClock, NewSchedule, RunStatus, ScheduleKind, ScheduleTarget, Scheduler,
};
use ember_server::session::{NewSession, Push, Sessions};
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tower::ServiceExt;

const H: i64 = 3_600_000;
/// 2027-01-15T08:00:00Z, a fixed start for the manual clock.
const T0: i64 = 1_800_000_000_000;

/// The scripted agent, remembering each process's start request by its working directory.
#[derive(Clone, Default)]
struct Recording(Arc<Mutex<HashMap<PathBuf, StartRequest>>>);

#[async_trait]
impl AgentAdapter for Recording {
    fn kind(&self) -> AgentKind {
        AgentKind::Scripted
    }
    async fn detect(&self) -> Detected {
        ScriptedAdapter.detect().await
    }
    async fn start(&self, req: StartRequest, events: mpsc::Sender<AgentEvent>) -> anyhow::Result<Box<dyn AgentRun>> {
        self.0.lock().unwrap().insert(req.cwd.clone(), req.clone());
        ScriptedAdapter.start(req, events).await
    }
}

impl Recording {
    fn request(&self, cwd: &str) -> StartRequest {
        self.0.lock().unwrap().get(&PathBuf::from(cwd)).cloned().unwrap_or_else(|| panic!("{cwd} never started"))
    }
    /// The A2A runtime token of the process started in `cwd`.
    fn token(&self, cwd: &str) -> String {
        self.request(cwd).env.iter().find(|(k, _)| k == "EMBER_RUNTIME_TOKEN").map(|(_, v)| v.clone()).unwrap()
    }
}

fn sessions_on(store: Arc<Store>, agents: &Recording) -> Arc<Sessions> {
    Sessions::new(store, vec![Arc::new(agents.clone()) as Arc<dyn AgentAdapter>])
}

fn new_session(s: &Sessions, project: &str, cwd: &str) -> String {
    s.create(NewSession {
        project: project.into(),
        agent: AgentKind::Scripted,
        cwd: cwd.into(),
        model: None,
        title: "t".into(),
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
    wait_for(&format!("{id} to be {want:?}"), || s.store().session(id).unwrap().unwrap().status == want).await;
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

async fn call(app: &Router, method: &str, uri: &str, token: Option<&str>, body: Option<Value>) -> (StatusCode, Value, String) {
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
    let raw = String::from_utf8_lossy(&bytes).into_owned();
    let v = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
    (status, v, raw)
}

fn interval_continue(project: &str, session: &str, catch_up: bool) -> NewSchedule {
    NewSchedule {
        project: project.into(),
        agent: None,
        account: None,
        computer: None,
        cwd: None,
        prompt: "check the nightly build".into(),
        kind: ScheduleKind::Interval { seconds: 3600 },
        target: ScheduleTarget::Continue { session_id: session.into() },
        catch_up,
        paused: false,
    }
}

// -------------------------------------------------------------------------------------------
// FR-A7

#[tokio::test]
async fn mcp_registry_reaches_agents_in_scope_and_never_leaks_secrets() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let agents = Recording::default();
    let sessions = sessions_on(store.clone(), &agents);
    let registry = McpRegistry::with_secrets(store.clone(), SecretBox::ephemeral());
    registry.install(&sessions);
    let app = ember_server::mcp::api::router(registry.clone());

    let (st, v, raw) = call(
        &app,
        "POST",
        "/api/mcp",
        None,
        Some(json!({
            "name": "github",
            "command": "/usr/local/bin/github-mcp",
            "args": ["stdio"],
            "env": { "GITHUB_TOKEN": "ghp_TOPSECRET_1" },
            "scope": "project:acme"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{raw}");
    assert_eq!(v["env_keys"], json!(["GITHUB_TOKEN"]));
    assert!(!raw.contains("TOPSECRET"), "{raw}");
    let id = v["id"].as_str().unwrap().to_string();
    let (st, _, raw) = call(&app, "POST", "/api/mcp", None, Some(json!({ "name": "everywhere", "command": "x" }))).await;
    assert_eq!(st, StatusCode::CREATED, "{raw}");
    let (st, _, _) = call(&app, "POST", "/api/mcp", None, Some(json!({ "name": "github", "command": "x" }))).await;
    assert_eq!(st, StatusCode::CONFLICT);

    // Rotating the secret answers with names only.
    let (st, _, raw) = call(
        &app,
        "PATCH",
        &format!("/api/mcp/{id}"),
        None,
        Some(json!({ "env": { "GITHUB_TOKEN": "ghp_TOPSECRET_2" } })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(!raw.contains("TOPSECRET"), "{raw}");
    let (_, list, raw) = call(&app, "GET", "/api/mcp", None, None).await;
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert!(!raw.contains("TOPSECRET"), "{raw}");

    // Every agent start gets the servers in scope, with the current secret.
    let acme = new_session(&sessions, "acme", "/w/acme");
    let beta = new_session(&sessions, "beta", "/w/beta");
    sessions.send(&acme, "hi").await.unwrap();
    sessions.send(&beta, "hi").await.unwrap();
    wait_status(&sessions, &acme, SessionStatus::WaitingForApproval).await;
    let req = agents.request("/w/acme");
    let names: Vec<&str> = req.mcp_servers.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["everywhere", "github"]);
    let gh = req.mcp_servers.iter().find(|s| s.name == "github").unwrap();
    assert_eq!(gh.env, [("GITHUB_TOKEN".to_string(), "ghp_TOPSECRET_2".to_string())]);
    assert!(!format!("{req:?}").contains("TOPSECRET"), "Debug of a start request redacts MCP env");
    let names: Vec<String> = agents.request("/w/beta").mcp_servers.iter().map(|s| s.name.clone()).collect();
    assert_eq!(names, ["everywhere"]);

    // Claude Code gets it in --mcp-config (written to a private file by the adapter).
    let config = ember_server::agents::claude_code::mcp_config(&req.mcp_servers).unwrap();
    let config: Value = serde_json::from_str(&config).unwrap();
    assert_eq!(config["mcpServers"]["github"]["command"], "/usr/local/bin/github-mcp");
    assert_eq!(config["mcpServers"]["everywhere"]["command"], "x");

    // Nothing of it reaches the transcript.
    let transcript = serde_json::to_string(&sessions.store().events_after(&acme, 0).unwrap()).unwrap();
    assert!(!transcript.contains("TOPSECRET"));

    // Disabled servers are left out; deleted ones are gone.
    let (st, _, _) = call(&app, "PATCH", &format!("/api/mcp/{id}"), None, Some(json!({ "enabled": false }))).await;
    assert_eq!(st, StatusCode::OK);
    let rec = sessions.store().session(&acme).unwrap().unwrap();
    let mut next = StartRequest::default();
    registry.configure_start(&rec, &mut next).unwrap();
    assert_eq!(next.mcp_servers.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["everywhere"]);
    let (st, _, _) = call(&app, "DELETE", &format!("/api/mcp/{id}"), None, None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _, _) = call(&app, "DELETE", &format!("/api/mcp/{id}"), None, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

// -------------------------------------------------------------------------------------------
// FR-A8

#[tokio::test]
async fn continue_and_new_targets_send_the_prompt_and_record_runs() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let agents = Recording::default();
    let sessions = sessions_on(store, &agents);
    let clock = ManualClock::new(T0);
    let sched = Scheduler::new(sessions.clone(), clock.clone());
    let mut pushes = sessions.subscribe();

    let s1 = new_session(&sessions, "acme", "/w/one");
    let every = sched.create(interval_continue("acme", &s1, false), None).unwrap();
    assert_eq!(every.next_run_at, Some(T0 + H));
    assert!(sched.tick().await.is_empty(), "nothing due yet");

    // Continue: the prompt enters the existing session.
    clock.set(T0 + H);
    let runs = sched.tick().await;
    assert_eq!(runs.len(), 1);
    let run = runs[0].clone();
    assert_eq!((run.status, run.session_id.as_deref(), run.scheduled_at), (RunStatus::Running, Some(s1.as_str()), T0 + H));
    wait_status(&sessions, &s1, SessionStatus::WaitingForApproval).await;
    let msgs = user_messages(&sessions, &s1);
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].starts_with("[ember schedule]") && msgs[0].ends_with("check the nightly build"), "{}", msgs[0]);
    assert_eq!(sched.get(&every.id).unwrap().next_run_at, Some(T0 + 2 * H));

    // Mid-turn, a continue run is skipped rather than sent.
    let skipped = sched.run_now(&every.id).await.unwrap();
    assert_eq!(skipped.status, RunStatus::Skipped);
    assert_eq!(user_messages(&sessions, &s1).len(), 1);

    // The run finishes with the turn.
    sessions.answer(&s1, "approval-1", ApprovalDecision::AllowOnce).await.unwrap();
    wait_for("run completed", || sched.run(&run.id).unwrap().status == RunStatus::Completed).await;
    assert!(sched.run(&run.id).unwrap().finished_at.is_some());

    // New: each run starts a session in the project.
    let once = sched
        .create(
            NewSchedule {
                project: "acme".into(),
                agent: Some("scripted".into()),
                account: None,
                computer: None,
                cwd: Some("/w/new".into()),
                prompt: "write the weekly report".into(),
                kind: ScheduleKind::Once { at: T0 + H + 60_000 },
                target: ScheduleTarget::New { title: Some("Weekly report".into()) },
                catch_up: false,
                paused: false,
            },
            None,
        )
        .unwrap();
    clock.set(T0 + H + 60_000);
    let runs = sched.tick().await;
    assert_eq!(runs.len(), 1);
    let new_id = runs[0].session_id.clone().expect("a new session");
    assert_ne!(new_id, s1);
    let rec = sessions.store().session(&new_id).unwrap().unwrap();
    assert_eq!((rec.project.as_str(), rec.title.as_str(), rec.cwd.as_str()), ("acme", "Weekly report", "/w/new"));
    wait_status(&sessions, &new_id, SessionStatus::WaitingForApproval).await;
    assert!(user_messages(&sessions, &new_id)[0].ends_with("write the weekly report"));
    assert_eq!(sched.get(&once.id).unwrap().next_run_at, None, "a one-off fires once");
    clock.advance(10 * 60_000);
    assert!(sched.tick().await.is_empty());

    // Validation.
    let mut bad = interval_continue("acme", &s1, false);
    bad.kind = ScheduleKind::Cron { expr: "0 25 * * *".into(), tz: "UTC".into() };
    assert!(sched.create(bad, None).is_err());
    let mut bad = interval_continue("other", &s1, false);
    bad.kind = ScheduleKind::Interval { seconds: 3600 };
    assert!(sched.create(bad, None).is_err(), "a continued session must be in the schedule's project");

    // Created, updated and run events were pushed.
    let mut kinds = Vec::new();
    while let Ok(p) = pushes.try_recv() {
        kinds.push(match p {
            Push::ScheduleCreated { .. } => "created",
            Push::ScheduleRun { .. } => "run",
            _ => continue,
        });
    }
    assert!(kinds.contains(&"created") && kinds.contains(&"run"), "{kinds:?}");
}

#[tokio::test]
async fn triggers_missed_while_down_are_reported_not_all_run() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ember.db");
    let agents = Recording::default();

    // Before the "restart": two hourly schedules continuing two sessions.
    let (s_report, s_catch, report, catch) = {
        let sessions = sessions_on(Arc::new(Store::open(&path).unwrap()), &agents);
        let sched = Scheduler::new(sessions.clone(), ManualClock::new(T0));
        let a = new_session(&sessions, "acme", "/w/a");
        let b = new_session(&sessions, "acme", "/w/b");
        let report = sched.create(interval_continue("acme", &a, false), None).unwrap();
        let catch = sched.create(interval_continue("acme", &b, true), None).unwrap();
        (a, b, report.id, catch.id)
    };

    // The server comes back 5 h 2 min later: triggers at +1h … +5h were missed.
    let sessions = sessions_on(Arc::new(Store::open(&path).unwrap()), &agents);
    let clock = ManualClock::new(T0 + 5 * H + 120_000);
    let sched = Scheduler::new(sessions.clone(), clock.clone());
    sched.recover().unwrap();
    let runs = sched.tick().await;

    // Without catch-up: five `missed` runs, a notice in the session, nothing sent.
    let mut missed: Vec<i64> = runs
        .iter()
        .filter(|r| r.schedule_id == report)
        .map(|r| {
            assert_eq!(r.status, RunStatus::Missed);
            r.scheduled_at
        })
        .collect();
    missed.sort();
    assert_eq!(missed, (1..=5).map(|k| T0 + k * H).collect::<Vec<_>>());
    assert!(user_messages(&sessions, &s_report).is_empty(), "missed triggers are not run");
    let n = notices(&sessions, &s_report);
    assert_eq!(n.len(), 1, "{n:?}");
    assert!(n[0].contains("missed 5 trigger(s)") && n[0].contains("catch_up is off"), "{}", n[0]);
    assert_eq!(sched.get(&report).unwrap().next_run_at, Some(T0 + 6 * H));

    // With catch-up: only the latest runs; the four before it are missed.
    let of_catch: Vec<_> = runs.iter().filter(|r| r.schedule_id == catch).collect();
    assert_eq!(of_catch.iter().filter(|r| r.status == RunStatus::Missed).count(), 4);
    let ran: Vec<_> = of_catch.iter().filter(|r| r.status != RunStatus::Missed).collect();
    assert_eq!(ran.len(), 1);
    assert_eq!(ran[0].scheduled_at, T0 + 5 * H);
    wait_status(&sessions, &s_catch, SessionStatus::WaitingForApproval).await;
    assert_eq!(user_messages(&sessions, &s_catch).len(), 1);
    assert!(notices(&sessions, &s_catch)[0].contains("is run now (catch_up)"));

    // The run history keeps them; a second tick does nothing more.
    assert_eq!(sched.runs(&report, 50).unwrap().len(), 5);
    assert!(sched.tick().await.is_empty());

    // A trigger only seconds late is simply run.
    clock.set(T0 + 6 * H + 5_000);
    let runs = sched.tick().await;
    let r: Vec<_> = runs.iter().filter(|r| r.schedule_id == report).collect();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].status, RunStatus::Running);
}

#[tokio::test]
async fn paused_schedules_do_not_fire_or_miss() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let agents = Recording::default();
    let sessions = sessions_on(store, &agents);
    let clock = ManualClock::new(T0);
    let sched = Scheduler::new(sessions.clone(), clock.clone());
    let a = new_session(&sessions, "acme", "/w/a");
    let s = sched.create(interval_continue("acme", &a, false), None).unwrap();
    let p = sched.pause(&s.id).unwrap();
    assert!(p.paused && p.next_run_at.is_none());
    clock.set(T0 + 10 * H);
    assert!(sched.tick().await.is_empty());
    let r = sched.resume(&s.id).unwrap();
    assert_eq!(r.next_run_at, Some(T0 + 11 * H), "counts from the resume");
    assert!(sched.tick().await.is_empty());
    assert!(notices(&sessions, &a).is_empty());
}

#[tokio::test]
async fn http_api_and_agent_side_schedule_commands() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let agents = Recording::default();
    let sessions = sessions_on(store, &agents);
    let a2a = A2a::new(sessions.clone(), A2aStore::open_in_memory().unwrap(), A2aConfig::new("http://127.0.0.1:1"));
    a2a.install();
    let sched = Scheduler::new(sessions.clone(), ManualClock::new(T0));
    let app = ember_server::schedules::api::router(sched.clone())
        .merge(ember_server::schedules::api::agent_router(sched.clone(), a2a.clone()));

    // User routes.
    let a = new_session(&sessions, "acme", "/w/a");
    let (st, v, raw) = call(
        &app,
        "POST",
        "/api/schedules",
        None,
        Some(json!({
            "project": "acme",
            "prompt": "standup summary",
            "kind": { "type": "cron", "expr": "0 9 * * 1-5", "tz": "Asia/Seoul" },
            "target": { "type": "continue", "session_id": a }
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{raw}");
    let id = v["id"].as_str().unwrap().to_string();
    assert_eq!(v["agent"], "scripted");
    assert!(v["next_run_at"].as_i64().unwrap() > T0);
    let (st, v, _) = call(
        &app,
        "POST",
        "/api/schedules",
        None,
        Some(json!({
            "project": "acme", "prompt": "x",
            "kind": { "type": "cron", "expr": "0 9 * * *", "tz": "Nowhere/City" },
            "target": { "type": "continue", "session_id": a }
        })),
    )
    .await;
    assert_eq!((st, v["code"].as_str()), (StatusCode::BAD_REQUEST, Some("bad_request")));
    let (st, v, _) = call(&app, "POST", &format!("/api/schedules/{id}/pause"), None, None).await;
    assert_eq!((st, v["paused"].as_bool(), v["next_run_at"].is_null()), (StatusCode::OK, Some(true), true));
    let (st, v, _) = call(&app, "POST", &format!("/api/schedules/{id}/resume"), None, None).await;
    assert_eq!((st, v["paused"].as_bool()), (StatusCode::OK, Some(false)));
    let (st, v, _) = call(&app, "POST", &format!("/api/schedules/{id}/run"), None, None).await;
    assert_eq!((st, v["status"].as_str()), (StatusCode::ACCEPTED, Some("running")));
    let (_, v, _) = call(&app, "GET", &format!("/api/schedules/{id}/runs"), None, None).await;
    assert_eq!(v.as_array().unwrap().len(), 1);
    wait_status(&sessions, &a, SessionStatus::WaitingForApproval).await;

    // Agent routes: authenticated by the runtime token, confined to the caller's project.
    let token = agents.token("/w/a");
    assert!(agents.request("/w/a").instructions.unwrap().contains("ember-a2a schedule add"));
    let (st, _, _) = call(&app, "GET", "/api/a2a/schedules", None, None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, v, raw) = call(
        &app,
        "POST",
        "/api/a2a/schedules",
        Some(&token),
        Some(json!({ "prompt": "remind me", "kind": { "type": "interval", "seconds": 7200 } })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{raw}");
    assert_eq!(v["target"], json!({ "type": "continue", "session_id": a }));
    assert_eq!(v["created_by_session"], a.as_str());
    assert_eq!(v["project"], "acme");
    let (st, v, raw) = call(
        &app,
        "POST",
        "/api/a2a/schedules",
        Some(&token),
        Some(json!({ "prompt": "nightly", "kind": { "type": "cron", "expr": "0 2 * * *" }, "target": { "type": "new" } })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{raw}");
    assert_eq!((v["cwd"].as_str(), v["agent"].as_str()), (Some("/w/a"), Some("scripted")), "caller's defaults");

    // Another project's schedule is invisible to this agent.
    let b = new_session(&sessions, "other", "/w/b");
    let theirs = sched.create(interval_continue("other", &b, false), None).unwrap();
    let (_, list, _) = call(&app, "GET", "/api/a2a/schedules", Some(&token), None).await;
    assert_eq!(list.as_array().unwrap().len(), 3);
    assert!(list.as_array().unwrap().iter().all(|s| s["project"] == "acme"));
    let (st, _, _) = call(&app, "DELETE", &format!("/api/a2a/schedules/{}", theirs.id), Some(&token), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _, _) = call(&app, "DELETE", &format!("/api/a2a/schedules/{id}"), Some(&token), None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _, _) = call(&app, "GET", &format!("/api/schedules/{id}"), None, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}
