//! FR-T2a (#102): the A2A runtime token is revoked when the agent run that got it ends, when its
//! session ends for good or is deleted, and it expires (idle and absolute lifetime). A token in
//! use by a live agent run never expires under that run, and an agent that outlives a server
//! restart and is rejoined keeps a working token.
//!
//! Taking the token out of the agent's environment is the other half of FR-T2a and is not
//! covered here (#102).

mod shim_guard;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::a2a::{A2a, A2aConfig, A2aStore, TokenTimes};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind, AgentRun, Detected, StartRequest};
use ember_server::events::{AgentEvent, ApprovalDecision, SessionStatus};
use ember_server::session::{NewSession, Sessions};
use ember_server::store::{SessionRecord, Store};
use http_body_util::BodyExt;
use tokio::sync::mpsc;
use tower::ServiceExt;

/// The shim runtime and `EMBER_SESSION_SHIM` are process-wide: one test at a time.
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The scripted agent, remembering the runtime token of each start by `EMBER_SESSION_ID`.
#[derive(Clone, Default)]
struct Recording(Arc<Mutex<HashMap<String, String>>>);

#[async_trait]
impl AgentAdapter for Recording {
    fn kind(&self) -> AgentKind {
        AgentKind::Scripted
    }
    async fn detect(&self) -> Detected {
        ScriptedAdapter.detect().await
    }
    async fn start(&self, req: StartRequest, events: mpsc::Sender<AgentEvent>) -> anyhow::Result<Box<dyn AgentRun>> {
        let env = |name: &str| req.env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
        if let (Some(id), Some(token)) = (env("EMBER_SESSION_ID"), env("EMBER_RUNTIME_TOKEN")) {
            self.0.lock().unwrap().insert(id, token);
        }
        ScriptedAdapter.start(req, events).await
    }
}

impl Recording {
    fn token(&self, session: &str) -> String {
        self.0.lock().unwrap().get(session).cloned().unwrap_or_else(|| panic!("{session} never started"))
    }
}

struct World {
    sessions: Arc<Sessions>,
    a2a: Arc<A2a>,
    agents: Recording,
    app: Router,
}

fn world_with(store: Arc<Store>, a2a_store: A2aStore, tokens: TokenTimes) -> World {
    let agents = Recording::default();
    let sessions = Sessions::new(store, vec![Arc::new(agents.clone()) as Arc<dyn AgentAdapter>]);
    let mut config = A2aConfig::new("http://127.0.0.1:1");
    config.tokens = tokens;
    let a2a = A2a::new(sessions.clone(), a2a_store, config);
    a2a.install();
    let app = ember_server::a2a::api::router(a2a.clone());
    World { sessions, a2a, agents, app }
}

fn world(tokens: TokenTimes) -> World {
    world_with(Arc::new(Store::open_in_memory().unwrap()), A2aStore::open_in_memory().unwrap(), tokens)
}

/// Lifetimes far longer than any test: nothing expires unless the test says so.
fn long() -> TokenTimes {
    TokenTimes { idle: Duration::from_secs(3600), max: Duration::from_secs(86_400), check: Duration::from_secs(3600) }
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
    for _ in 0..2000 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn wait_status(s: &Sessions, id: &str, want: SessionStatus) {
    wait_for(&format!("{id} to be {want:?}"), || s.store().session(id).unwrap().unwrap().status == want).await;
}

/// `GET /api/a2a/targets` with `token`: the status alone.
async fn targets(app: &Router, token: &str) -> StatusCode {
    let req = Request::builder()
        .method("GET")
        .uri("/api/a2a/targets")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let _ = res.into_body().collect().await.unwrap();
    status
}

/// Start the session's agent run and leave it mid-turn (the scripted agent waits for approval).
async fn start_run(w: &World, id: &str) -> String {
    w.sessions.send(id, "hello").await.unwrap();
    wait_status(&w.sessions, id, SessionStatus::WaitingForApproval).await;
    w.agents.token(id)
}

#[tokio::test]
async fn fr_t2a_the_token_of_a_stopped_session_gets_401() {
    let w = world(long());
    let id = new_session(&w.sessions, "a");
    let token = start_run(&w, &id).await;
    assert_eq!(targets(&w.app, &token).await, StatusCode::OK);
    // Stopping the agent run (idle release, team end and computer switch all stop it here).
    w.sessions.release(&id).await.unwrap();
    assert_eq!(targets(&w.app, &token).await, StatusCode::UNAUTHORIZED);
    assert!(w.a2a.authenticate(&token).is_err());
}

#[tokio::test]
async fn fr_t2a_the_token_of_a_session_ended_for_good_gets_401() {
    let w = world(long());
    let id = new_session(&w.sessions, "a");
    let token = start_run(&w, &id).await;
    w.sessions.release(&id).await.unwrap();
    // A token minted for a session whose run is gone is also dropped when the session ends.
    let spare = w.a2a.store().issue_token(&id).unwrap();
    assert_eq!(targets(&w.app, &spare).await, StatusCode::OK);
    w.sessions.session_ended(&id);
    assert_eq!(targets(&w.app, &spare).await, StatusCode::UNAUTHORIZED);
    assert_eq!(targets(&w.app, &token).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn fr_t2a_the_token_of_a_deleted_session_gets_401() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ember.db");
    let w = world_with(Arc::new(Store::open(&db).unwrap()), A2aStore::open(&db).unwrap(), long());
    let id = new_session(&w.sessions, "a");
    let token = start_run(&w, &id).await;
    assert_eq!(targets(&w.app, &token).await, StatusCode::OK);
    w.sessions.release(&id).await.unwrap();
    // A token that outlived its run (minted again) and then lost its session row.
    let token = w.a2a.store().issue_token(&id).unwrap();
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        conn.execute("DELETE FROM events WHERE session_id = ?1", [&id]).unwrap();
        conn.execute("DELETE FROM sessions WHERE id = ?1", [&id]).unwrap();
    }
    assert_eq!(targets(&w.app, &token).await, StatusCode::UNAUTHORIZED);
    // The row went with the refusal: the token stays refused if the id came back.
    assert_eq!(w.a2a.store().session_for_token(&token).unwrap(), None);
}

#[tokio::test]
async fn fr_t2a_an_idle_token_with_no_live_run_expires_and_gets_401() {
    let w = world(TokenTimes {
        idle: Duration::from_millis(300),
        max: Duration::from_secs(86_400),
        check: Duration::from_millis(50),
    });
    let id = new_session(&w.sessions, "a");
    // No agent run holds this token: nothing refreshes it but its own use.
    let token = w.a2a.store().issue_token(&id).unwrap();
    // Each authenticated use refreshes the idle lifetime.
    for _ in 0..6 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(targets(&w.app, &token).await, StatusCode::OK, "a token in use stays valid");
    }
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(targets(&w.app, &token).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn fr_t2a_a_token_past_its_absolute_lifetime_with_no_live_run_gets_401() {
    let w = world(TokenTimes {
        idle: Duration::from_secs(3600),
        max: Duration::from_millis(300),
        check: Duration::from_millis(50),
    });
    let id = new_session(&w.sessions, "a");
    let token = w.a2a.store().issue_token(&id).unwrap();
    assert_eq!(targets(&w.app, &token).await, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(600)).await;
    // Used all along or not, a token past its absolute lifetime is refused.
    assert_eq!(targets(&w.app, &token).await, StatusCode::UNAUTHORIZED);
}

#[test]
fn fr_t2a_expiry_follows_the_clock_given_to_the_store() {
    let s = A2aStore::open_in_memory().unwrap();
    s.set_token_times(TokenTimes {
        idle: Duration::from_secs(60),
        max: Duration::from_secs(600),
        check: Duration::from_secs(10),
    });
    let t0 = 1_000_000_i64;
    let token = s.issue_token_at("s1", t0).unwrap();
    assert_eq!(s.session_for_token_at(&token, t0 + 59_000).unwrap().as_deref(), Some("s1"));
    // The use at +59 s refreshed it: +118 s is 59 s after the last use.
    assert_eq!(s.session_for_token_at(&token, t0 + 118_000).unwrap().as_deref(), Some("s1"));
    // 61 s after the last use: idle-expired, and the row is gone.
    assert_eq!(s.session_for_token_at(&token, t0 + 179_001).unwrap(), None);
    assert_eq!(s.session_for_token_at(&token, t0 + 120_000).unwrap(), None, "an expired token is deleted");

    let token = s.issue_token_at("s2", t0).unwrap();
    for step in 1..=10 {
        assert_eq!(s.session_for_token_at(&token, t0 + step * 50_000).unwrap().as_deref(), Some("s2"));
    }
    // Used every 50 s, it still ends at its absolute lifetime (600 s).
    assert_eq!(s.session_for_token_at(&token, t0 + 600_001).unwrap(), None);
}

#[tokio::test]
async fn fr_t2a_a_token_in_use_does_not_expire_while_its_run_lives() {
    let w = world(TokenTimes {
        idle: Duration::from_millis(400),
        max: Duration::from_millis(400),
        check: Duration::from_millis(50),
    });
    let id = new_session(&w.sessions, "a");
    let token = start_run(&w, &id).await;
    // The turn runs (waits for approval) far past both lifetimes, with no A2A call in between.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(w.sessions.is_live(&id).await);
    assert_eq!(targets(&w.app, &token).await, StatusCode::OK, "a live run's token never expires under it");

    // The turn ends: past the absolute lifetime and idle, the run is released so the next start
    // mints a fresh token, and the old one is revoked with the run.
    w.sessions.answer(&id, "approval-1", ApprovalDecision::AllowOnce).await.unwrap();
    wait_status(&w.sessions, &id, SessionStatus::Finished).await;
    let mut released = false;
    for _ in 0..200 {
        if !w.sessions.is_live(&id).await {
            released = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(released, "an idle run past the absolute lifetime is released");
    assert_eq!(targets(&w.app, &token).await, StatusCode::UNAUTHORIZED);

    // The next message restarts the agent with a fresh, working token.
    w.sessions.send(&id, "again").await.unwrap();
    wait_status(&w.sessions, &id, SessionStatus::WaitingForApproval).await;
    let fresh = w.agents.token(&id);
    assert_ne!(fresh, token);
    assert_eq!(targets(&w.app, &fresh).await, StatusCode::OK);
    w.sessions.release(&id).await.unwrap();
}

#[tokio::test]
async fn fr_t2a_a_live_run_keeps_its_token_past_the_idle_lifetime_between_turns() {
    let w = world(TokenTimes {
        idle: Duration::from_millis(300),
        max: Duration::from_secs(86_400),
        check: Duration::from_millis(50),
    });
    let id = new_session(&w.sessions, "a");
    let token = start_run(&w, &id).await;
    w.sessions.answer(&id, "approval-1", ApprovalDecision::AllowOnce).await.unwrap();
    wait_status(&w.sessions, &id, SessionStatus::Finished).await;
    // Between turns the agent run is alive (not released): its token is kept.
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert!(w.sessions.is_live(&id).await);
    assert_eq!(targets(&w.app, &token).await, StatusCode::OK);
    w.sessions.release(&id).await.unwrap();
    assert_eq!(targets(&w.app, &token).await, StatusCode::UNAUTHORIZED);
}

// --- Server restart with a surviving agent (FR-S8) ---

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn shim_runtime(dir: &Path, store: &Arc<Store>) -> ember_server::agents::proc::ShimRuntime {
    ember_server::agents::proc::ShimRuntime {
        bin: PathBuf::from(env!("CARGO_BIN_EXE_ember-shim")),
        dir: dir.to_path_buf(),
        store: store.clone(),
    }
}

/// Run "server 1" on its own runtime and drop that runtime afterwards, as in a crash: every task
/// and handle of the old server goes, the shim and its agent stay.
fn run_server1<F: std::future::Future<Output = ()> + Send + 'static>(f: F) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(f);
        rt.shutdown_background();
    })
    .join()
    .unwrap();
}

/// A server's session manager with the Claude Code adapter and A2A hooks on `db`, plus a slot
/// that receives the runtime token of every agent start.
fn claude_server(
    store: &Arc<Store>,
    db: &Path,
    bin: &Path,
) -> (Arc<Sessions>, Arc<A2a>, Arc<Mutex<Vec<String>>>) {
    use ember_server::agents::claude_code::ClaudeCodeAdapter;
    let sessions =
        Sessions::new(store.clone(), vec![Arc::new(ClaudeCodeAdapter::new(bin.to_path_buf())) as Arc<dyn AgentAdapter>]);
    let mut config = A2aConfig::new("http://127.0.0.1:1");
    config.tokens = long();
    let a2a = A2a::new(sessions.clone(), A2aStore::open(db).unwrap(), config);
    a2a.install_hooks();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let slot = seen.clone();
    sessions.add_start_config_hook(Arc::new(move |_rec: &SessionRecord, req: &mut StartRequest| {
        if let Some((_, t)) = req.env.iter().find(|(k, _)| k == "EMBER_RUNTIME_TOKEN") {
            slot.lock().unwrap().push(t.clone());
        }
        Ok(())
    }));
    (sessions, a2a, seen)
}

#[cfg(unix)]
#[tokio::test]
async fn fr_t2a_a_reattached_agent_keeps_a_working_token_after_a_server_restart() {
    use ember_server::agents::proc::{clear_runtime, install_runtime};
    let _g = LOCK.lock().await;
    std::env::remove_var("EMBER_SESSION_SHIM");
    let shims = tempfile::tempdir().unwrap();
    let _guard = shim_guard::ShimGuard::new(&[shims.path()]);
    let work = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let db = data.path().join("ember.db");
    let store = Arc::new(Store::open(&db).unwrap());
    install_runtime(shim_runtime(shims.path(), &store));
    let bin = crate_dir().join("tests/fixtures/claude/fake-claude-approval.sh");
    let id = store.create_session("p", AgentKind::ClaudeCode, work.path().to_str().unwrap(), None, "t").unwrap().id;

    let first: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    {
        let (store, db, bin, id, first) = (store.clone(), db.clone(), bin.clone(), id.clone(), first.clone());
        run_server1(async move {
            let (sessions, a2a, seen) = claude_server(&store, &db, &bin);
            sessions.send(&id, "hello").await.unwrap();
            wait_for("the approval request", || {
                store
                    .events_after(&id, 0)
                    .unwrap()
                    .iter()
                    .any(|e| matches!(e.event, AgentEvent::ApprovalRequested { .. }))
            })
            .await;
            wait_for("a stored offset", || store.shim_offset(&id).unwrap() > 0).await;
            tokio::time::sleep(Duration::from_millis(700)).await;
            let token = seen.lock().unwrap().first().cloned().expect("the agent got a token");
            assert_eq!(a2a.authenticate(&token).unwrap(), id);
            *first.lock().unwrap() = Some(token);
        });
    }
    let old = first.lock().unwrap().clone().unwrap();

    // Server 2: the agent still runs under its shim and holds `old` in its environment.
    let (sessions, a2a, seen) = claude_server(&store, &db, &bin);
    let recovery = sessions.recover_after_restart(Some(&shim_runtime(shims.path(), &store))).await.unwrap();
    assert_eq!(recovery.rejoined, vec![id.clone()]);
    a2a.settle_tokens_after_restart(&recovery);
    assert_eq!(a2a.authenticate(&old).unwrap(), id, "the rejoined agent's token still works");
    // The start that rejoined it minted a token for later processes of the run; it works too.
    let minted = seen.lock().unwrap().first().cloned().expect("the rejoin ran the start hooks");
    assert_ne!(minted, old);
    assert_eq!(a2a.authenticate(&minted).unwrap(), id);

    sessions.answer(&id, "r1", ApprovalDecision::AllowOnce).await.unwrap();
    wait_for("the turn to end", || {
        store.events_after(&id, 0).unwrap().iter().any(|e| matches!(e.event, AgentEvent::TurnEnded { .. }))
    })
    .await;
    assert_eq!(a2a.authenticate(&old).unwrap(), id, "still valid after the rejoined turn");
    // Stopping the rejoined run revokes every token it held.
    sessions.release(&id).await.unwrap();
    assert!(a2a.authenticate(&old).is_err());
    assert!(a2a.authenticate(&minted).is_err());
    clear_runtime();
}

#[tokio::test]
async fn fr_t2a_a_restart_revokes_the_tokens_of_agents_that_did_not_survive() {
    let data = tempfile::tempdir().unwrap();
    let db = data.path().join("ember.db");
    let store = Arc::new(Store::open(&db).unwrap());
    let id = store.create_session("p", AgentKind::Scripted, "/tmp", None, "t").unwrap().id;
    let stale = A2aStore::open(&db).unwrap().issue_token(&id).unwrap();
    // A new server with no surviving agent (shims off): nobody can hold the old token.
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let a2a = A2a::new(sessions.clone(), A2aStore::open(&db).unwrap(), A2aConfig::new("http://127.0.0.1:1"));
    a2a.install_hooks();
    let recovery = sessions.recover_after_restart(None).await.unwrap();
    a2a.settle_tokens_after_restart(&recovery);
    assert!(a2a.authenticate(&stale).is_err());
}
