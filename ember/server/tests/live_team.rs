//! The team runner of FR-T7 (#82, `docs/design/POST-1-0-REAL-TEAMS.md` 4.2 and 4.3), V1 skeleton.
//!
//! One run sets up a team, gives the teammate work whose result can be judged, judges the
//! invariants and writes `evidence.json`. Evidence is built from typed fields only (an allow
//! list by construction): no runtime token, account credential or transcript text goes in.
//!
//! - `fr_t7_team_runner_judges_a_scripted_team` (CI, tier T0): the scripted agent stands in for
//!   both agents, and the test makes the teammate's calls itself with the teammate's own token,
//!   so the setup, the judging and the evidence file are proven without accounts.
//! - `fr_t7_live_team` (`#[ignore]`, `EMBER_E2E_TEAM=1`, `ember/server/scripts/team-run.sh`):
//!   tiers T1 and T2 with real Claude Code and Codex sessions. Not built (V2): it reads its
//!   configuration and reports what it would run, then stops with "skipped". Logins are the
//!   owner's (FR-U1), so a missing account is a skip, never a failure.
//!
//! Invariants judged today: I1 (leader and teammate), I2 (accounts apart), I3 (the teammate
//! reported the marker only its computer has), I4 (its written file is on that computer's disk),
//! I5 (task done, mail delivered). I6 (the leader got the report without polling) needs FR-T8,
//! which is not built, so it is `skipped` with that reason. I7 and I8 come with V2 and V3.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::a2a::{A2a, A2aConfig, A2aStore};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind, AgentRun, Detected, StartRequest};
use ember_server::events::{AgentEvent, ApprovalDecision, SessionStatus};
use ember_server::session::{NewSession, Sessions};
use ember_server::store::{now_ms, Store};
use http_body_util::BodyExt;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tower::ServiceExt;

// ---- evidence ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq)]
struct Verdict {
    /// `pass`, `fail` or `skipped`.
    result: &'static str,
    detail: String,
}

fn verdict(ok: bool, detail: impl Into<String>) -> Verdict {
    Verdict { result: if ok { "pass" } else { "fail" }, detail: detail.into() }
}

#[derive(Debug, Clone, Serialize)]
struct MemberEvidence {
    name: String,
    role: String,
    agent: String,
    account_id: Option<String>,
    designated_computer: Option<String>,
    current_computer: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct Evidence {
    /// `T0` (scripted), `T1` (same machine), `T2` (two computers), `T3` (two NATs).
    tier: &'static str,
    server_version: &'static str,
    started_at_ms: i64,
    duration_ms: u64,
    members: Vec<MemberEvidence>,
    /// Whether the value the teammate reported equals the marker; the marker itself is not kept.
    marker_match: bool,
    invariants: BTreeMap<&'static str, Verdict>,
}

impl Evidence {
    fn passed(&self) -> bool {
        self.invariants.values().all(|v| v.result != "fail")
    }
}

/// Write `evidence.json` into `dir` and return its path.
fn write_evidence(dir: &Path, ev: &Evidence) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join("evidence.json");
    std::fs::write(&path, serde_json::to_string_pretty(ev).unwrap() + "\n").unwrap();
    path
}

// ---- configuration ----------------------------------------------------------------------------

/// What a live run needs (environment, `team-run.sh`). Logins happen before, by the owner.
#[derive(Debug)]
struct LiveConfig {
    lead_account: String,
    mate_account: String,
    /// `None`: T1 (a second node on this machine); `Some`: T2/T3 on that computer.
    computer: Option<String>,
    /// A project path that exists on both computers.
    dir: Option<PathBuf>,
    out: PathBuf,
}

impl LiveConfig {
    /// `Err` is the reason the run is skipped.
    fn from_env(get: impl Fn(&str) -> Option<String>) -> Result<LiveConfig, String> {
        if get("EMBER_E2E_TEAM").as_deref() != Some("1") {
            return Err("EMBER_E2E_TEAM is not 1".into());
        }
        let need = |k: &str| get(k).filter(|v| !v.trim().is_empty()).ok_or_else(|| format!("{k} is not set (log in first; Ember does not log in for you, FR-U1)"));
        Ok(LiveConfig {
            lead_account: need("EMBER_E2E_TEAM_LEAD_ACCOUNT")?,
            mate_account: need("EMBER_E2E_TEAM_MATE_ACCOUNT")?,
            computer: get("EMBER_E2E_TEAM_COMPUTER").filter(|v| !v.trim().is_empty()),
            dir: get("EMBER_E2E_TEAM_DIR").filter(|v| !v.trim().is_empty()).map(PathBuf::from),
            out: get("EMBER_E2E_TEAM_OUT").map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("ember-team-run")),
        })
    }

    fn tier(&self) -> &'static str {
        if self.computer.is_some() { "T2" } else { "T1" }
    }
}

// ---- an in-process server with the scripted agent (T0) ----------------------------------------

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
    async fn start(&self, req: StartRequest, events: mpsc::Sender<AgentEvent>) -> anyhow::Result<Box<dyn AgentRun>> {
        let id = req.env.iter().find(|(k, _)| k == "EMBER_SESSION_ID").map(|(_, v)| v.clone());
        self.0.lock().unwrap().insert(id.unwrap_or_default(), req.clone());
        ScriptedAdapter.start(req, events).await
    }
}

impl Recording {
    fn token(&self, session: &str) -> Option<String> {
        let seen = self.0.lock().unwrap();
        seen.get(session)?.env.iter().find(|(k, _)| k == "EMBER_RUNTIME_TOKEN").map(|(_, v)| v.clone())
    }
}

struct World {
    sessions: Arc<Sessions>,
    agents: Recording,
    app: Router,
}

fn world() -> World {
    let agents = Recording::default();
    let sessions = Sessions::new(Arc::new(Store::open_in_memory().unwrap()), vec![Arc::new(agents.clone()) as Arc<dyn AgentAdapter>]);
    // Accounts as the router would hand them out: whatever was requested.
    sessions.set_account_router(Arc::new(|_agent, requested: Option<&str>| {
        Ok(requested.map(|id| (id.to_string(), "requested".to_string())))
    }));
    let a2a = A2a::new(sessions.clone(), A2aStore::open_in_memory().unwrap(), A2aConfig::new("http://127.0.0.1:1"));
    a2a.install();
    let app = ember_server::api::router(sessions.clone()).merge(ember_server::a2a::api::router(a2a.clone()));
    World { sessions, agents, app }
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

async fn call(w: &World, who: &str, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let token = w.agents.token(who).unwrap_or_else(|| panic!("session {who} never started"));
    let req = Request::builder().method(method).uri(uri).header("authorization", format!("Bearer {token}"));
    let req = match body {
        Some(b) => req.header("content-type", "application/json").body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let res = w.app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() })
}

/// A scratch folder for one run: `main/` is the project on the leader's computer, `b/` the same
/// project on the teammate's computer. In T0 both are on this machine.
fn scratch() -> PathBuf {
    let root = std::env::temp_dir().join(format!("ember-live-team-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("main")).unwrap();
    std::fs::create_dir_all(root.join("b")).unwrap();
    root
}

// ---- the run ----------------------------------------------------------------------------------

/// T0: a leader and a teammate on two accounts; the teammate copies a marker only "computer B"
/// has, mails the value to the leader and closes its task. Returns the evidence and every
/// runtime token handed out, so the caller can check none leaked into the evidence.
async fn run_scripted_team(root: &Path) -> (Evidence, Vec<String>) {
    let started = Instant::now();
    let started_at_ms = now_ms();
    let w = world();
    let marker = uuid::Uuid::new_v4().to_string();
    std::fs::write(root.join("b").join("marker.txt"), &marker).unwrap();

    // The leader, on account 1, between turns with a token.
    let lead = w
        .sessions
        .create_with_account(
            NewSession { project: "team-run".into(), agent: AgentKind::Scripted, cwd: root.join("main"), model: None, title: "lead".into() },
            Some("acct-lead"),
        )
        .unwrap()
        .id;
    w.sessions.send(&lead, "start").await.unwrap();
    wait_for("the leader's approval", || w.sessions.store().session(&lead).unwrap().unwrap().status == SessionStatus::WaitingForApproval).await;
    w.sessions.answer(&lead, "approval-1", ApprovalDecision::AllowOnce).await.unwrap();
    wait_for("the leader's turn", || w.sessions.store().session(&lead).unwrap().unwrap().status == SessionStatus::Finished).await;

    // What a real leader does on its own (V2 asks it in plain words): spawn and assign. The
    // account is pinned here because T0 verifies the accounts are apart (`--account` is for
    // verification, D30).
    let (st, v) = call(&w, &lead, "POST", "/api/a2a/team/members",
        Some(json!({ "name": "mate", "account": "acct-mate", "prompt": "Copy marker.txt into result.txt, mail me its value, close task 1." }))).await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    let mate = v["member"]["session_id"].as_str().unwrap().to_string();
    let (st, v) = call(&w, &lead, "POST", "/api/a2a/team/tasks", Some(json!({ "title": "copy the marker", "assignee": "mate" }))).await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    wait_for("the teammate to start", || w.agents.token(&mate).is_some()).await;

    // The teammate's work, made with its own token (a real teammate's tools do this on B).
    let reported = std::fs::read_to_string(root.join("b").join("marker.txt")).unwrap();
    std::fs::write(root.join("b").join("result.txt"), &reported).unwrap();
    let (st, v) = call(&w, &mate, "POST", "/api/a2a/team/mail", Some(json!({ "to": "lead", "text": format!("marker: {reported}") }))).await;
    assert!(st.is_success(), "{v}");
    let (st, v) = call(&w, &mate, "PATCH", "/api/a2a/team/tasks/1", Some(json!({ "status": "done" }))).await;
    assert!(st.is_success(), "{v}");

    // Judge.
    let (_, team) = call(&w, &lead, "GET", "/api/a2a/team", None).await;
    let store = w.sessions.store();
    let members: Vec<MemberEvidence> = team["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            let sid = m["session_id"].as_str().unwrap_or_default();
            MemberEvidence {
                name: m["name"].as_str().unwrap_or_default().into(),
                role: m["role"].as_str().unwrap_or_default().into(),
                agent: m["agent"].as_str().unwrap_or_default().into(),
                account_id: store.session(sid).ok().flatten().and_then(|r| r.account_id),
                designated_computer: m["designated_computer"].as_str().map(String::from),
                current_computer: m["current_computer"].as_str().map(String::from),
            }
        })
        .collect();
    let roles: Vec<&str> = members.iter().map(|m| m.role.as_str()).collect();
    let accounts: Vec<Option<&str>> = members.iter().map(|m| m.account_id.as_deref()).collect();
    let (_, mail) = call(&w, &lead, "GET", "/api/a2a/team/mail", None).await;
    let mailed = mail.as_array().into_iter().flatten().filter_map(|m| m["text"].as_str()).find_map(|t| t.strip_prefix("marker: ").map(String::from));
    let marker_match = mailed.as_deref() == Some(marker.as_str());
    let (_, tasks) = call(&w, &lead, "GET", "/api/a2a/team/tasks", None).await;
    let done = tasks.as_array().into_iter().flatten().any(|t| t["number"] == 1 && t["status"] == "done");
    let written = std::fs::read_to_string(root.join("b").join("result.txt")).ok();
    let on_main = root.join("main").join("marker.txt").exists();

    let mut invariants = BTreeMap::new();
    invariants.insert("I1", verdict(roles == ["leader", "teammate"], format!("roles {roles:?}")));
    invariants.insert("I2", verdict(accounts == [Some("acct-lead"), Some("acct-mate")], format!("accounts {accounts:?}")));
    invariants.insert("I3", verdict(marker_match && !on_main, "the mailed value equals the marker that only computer B has"));
    invariants.insert("I4", verdict(written.as_deref() == Some(marker.as_str()), "result.txt on computer B holds the marker"));
    invariants.insert("I5", verdict(done && mailed.is_some(), format!("task 1 done: {done}; mail delivered: {}", mailed.is_some())));
    invariants.insert("I6", Verdict { result: "skipped", detail: "needs the FR-T8 report to the leader, not built yet (#82)".into() });

    let tokens = [w.agents.token(&lead), w.agents.token(&mate)].into_iter().flatten().collect();
    let ev = Evidence {
        tier: "T0",
        server_version: env!("CARGO_PKG_VERSION"),
        started_at_ms,
        duration_ms: started.elapsed().as_millis() as u64,
        members,
        marker_match,
        invariants,
    };
    (ev, tokens)
}

#[tokio::test]
async fn fr_t7_team_runner_judges_a_scripted_team() {
    let root = scratch();
    let (ev, tokens) = run_scripted_team(&root).await;
    let path = write_evidence(&root.join("out"), &ev);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(ev.passed(), "{text}");
    for k in ["I1", "I2", "I3", "I4", "I5"] {
        assert_eq!(ev.invariants[k].result, "pass", "{k}: {text}");
    }
    assert_eq!(ev.invariants["I6"].result, "skipped");
    // Allow list: no token, no marker value, no prompt text in the evidence.
    assert!(!tokens.is_empty());
    for t in &tokens {
        assert!(!text.contains(t.as_str()), "a runtime token leaked into the evidence");
    }
    let marker = std::fs::read_to_string(root.join("b").join("marker.txt")).unwrap();
    assert!(!text.contains(marker.trim()), "the marker value leaked into the evidence");
    assert!(!text.contains("Copy marker.txt"), "prompt text leaked into the evidence");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn fr_t7_live_config_skips_without_logins_and_picks_the_tier() {
    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k: &str| map.get(k).cloned()
    }
    assert_eq!(LiveConfig::from_env(env(&[])).unwrap_err(), "EMBER_E2E_TEAM is not 1");
    let e = LiveConfig::from_env(env(&[("EMBER_E2E_TEAM", "1"), ("EMBER_E2E_TEAM_LEAD_ACCOUNT", "a")])).unwrap_err();
    assert!(e.contains("EMBER_E2E_TEAM_MATE_ACCOUNT") && e.contains("FR-U1"), "{e}");
    let one = [("EMBER_E2E_TEAM", "1"), ("EMBER_E2E_TEAM_LEAD_ACCOUNT", "a"), ("EMBER_E2E_TEAM_MATE_ACCOUNT", "b")];
    assert_eq!(LiveConfig::from_env(env(&one)).unwrap().tier(), "T1");
    let two = [("EMBER_E2E_TEAM", "1"), ("EMBER_E2E_TEAM_LEAD_ACCOUNT", "a"), ("EMBER_E2E_TEAM_MATE_ACCOUNT", "b"), ("EMBER_E2E_TEAM_COMPUTER", "pi")];
    assert_eq!(LiveConfig::from_env(env(&two)).unwrap().tier(), "T2");
}

/// Tiers T1 and T2 with real agents. Not built (V2, #82): it stops here with the reason.
#[tokio::test]
#[ignore = "real agents and logged-in accounts; run with ember/server/scripts/team-run.sh"]
async fn fr_t7_live_team() {
    match LiveConfig::from_env(|k| std::env::var(k).ok()) {
        Err(why) => println!("[team-run] skipped: {why}"),
        Ok(c) => println!(
            "[team-run] skipped: tier {} with accounts {} and {}{}{} is not built yet (V2, #82); evidence would go to {}",
            c.tier(),
            c.lead_account,
            c.mate_account,
            c.computer.as_deref().map(|x| format!(", computer {x}")).unwrap_or_default(),
            c.dir.as_deref().map(|d| format!(", project {}", d.display())).unwrap_or_default(),
            c.out.display()
        ),
    }
}
