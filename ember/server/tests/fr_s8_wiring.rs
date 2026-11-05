//! FR-S8 wiring: the adapters start their agent under `ember-shim` through the shared spawn
//! helper; the agent outlives the server, a new server reattaches from the stored offset, and
//! `EMBER_SESSION_SHIM=off` keeps direct children.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ember_server::agents::antigravity::{AntigravityAdapter, HookBroker};
use ember_server::agents::claude_code::ClaudeCodeAdapter;
use ember_server::agents::codex::CodexAdapter;
use ember_server::agents::proc::{
    clear_runtime, install_runtime, release_commit, require_commit, spawn_agent, AgentCommand,
    ProcHandle, ShimRuntime, SESSION_ENV,
};
use ember_server::agents::{AgentAdapter, AgentKind, StartRequest};
use ember_server::events::{AgentEvent, ApprovalDecision, SessionStatus, TurnOutcome};
use ember_server::session::Sessions;
use ember_server::session_shim::{pid_alive, scan};
use ember_server::store::Store;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

/// The runtime and `EMBER_SESSION_SHIM` are process-wide: one test at a time.
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn install(dir: &Path) -> Arc<Store> {
    let store = Arc::new(Store::open_in_memory().unwrap());
    install_runtime(ShimRuntime {
        bin: PathBuf::from(env!("CARGO_BIN_EXE_ember-shim")),
        dir: dir.to_path_buf(),
        store: store.clone(),
    });
    store
}

fn fake_agent_cmd(session: &str, cwd: &Path, ticks: u32) -> AgentCommand {
    AgentCommand {
        program: crate_dir().join("tests/fixtures/shim/fake-agent.sh"),
        args: Vec::new(),
        env: vec![
            (SESSION_ENV.into(), session.into()),
            ("FAKE_TICKS".into(), ticks.to_string()),
        ],
        cwd: cwd.to_path_buf(),
        reattachable: true,
    }
}

fn tick(line: &str) -> Option<u32> {
    line.strip_prefix("tick ")?.trim().parse().ok()
}

#[tokio::test]
async fn fr_s8_agent_survives_dropping_the_server_and_a_new_one_reattaches_without_loss() {
    let _g = LOCK.lock().await;
    std::env::remove_var("EMBER_SESSION_SHIM");
    let shims = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let store = install(shims.path());

    // Server 1 starts the agent and reads the first ticks.
    let proc = spawn_agent(fake_agent_cmd("s-survive", work.path(), 25)).await.unwrap();
    assert!(!proc.reattached);
    let pid = proc.pid.expect("shim reports the agent pid");
    let mut seen = Vec::new();
    {
        let mut lines = BufReader::new(proc.stdout).lines();
        while let Some(l) = tokio::time::timeout(Duration::from_secs(15), lines.next_line())
            .await
            .unwrap()
            .unwrap()
        {
            seen.extend(tick(&l));
            if seen.len() == 6 {
                break;
            }
        }
        // Server 1 goes away here: reader, stdin and handle drop without a shutdown.
    }
    drop(proc.stdin);
    drop(proc.handle);
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(pid_alive(pid), "the agent keeps running without its server");
    let stored = store.shim_offset("s-survive").unwrap();
    assert!(stored > 0, "the acknowledged offset is stored");

    // Server 2 starts the same session: it reattaches, it does not start a second agent.
    let proc = spawn_agent(fake_agent_cmd("s-survive", work.path(), 25)).await.unwrap();
    assert!(proc.reattached);
    assert_eq!(proc.pid, Some(pid));
    let mut lines = BufReader::new(proc.stdout).lines();
    while let Some(l) = tokio::time::timeout(Duration::from_secs(20), lines.next_line())
        .await
        .unwrap()
        .unwrap()
    {
        seen.extend(tick(&l));
    }
    let mut dedup = seen.clone();
    dedup.dedup();
    assert_eq!(dedup, (1..=25).collect::<Vec<_>>(), "no tick lost, in order: {seen:?}");
    assert!(seen.len() - dedup.len() <= 1, "at most the line in flight repeats: {seen:?}");
    // The agent ended and its output was read to the end: the offset is forgotten.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(store.shim_offset("s-survive").unwrap(), 0);
    clear_runtime();
}

#[tokio::test]
async fn fr_s8_offsets_persist_in_the_store_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ember.db");
    {
        let store = Store::open(&path).unwrap();
        store.set_shim_offset("a", 10).unwrap();
        store.set_shim_offset("a", 42).unwrap();
        store.set_shim_offset("b", 7).unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(store.shim_offset("a").unwrap(), 42);
    assert_eq!(store.shim_offset("missing").unwrap(), 0);
    let all = store.shim_offsets().unwrap();
    assert_eq!((all.get("a"), all.get("b")), (Some(&42), Some(&7)));
    store.clear_shim_offset("a").unwrap();
    assert_eq!(store.shim_offset("a").unwrap(), 0);
}

#[tokio::test]
async fn fr_s8_session_shim_off_keeps_direct_spawn() {
    let _g = LOCK.lock().await;
    let shims = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    install(shims.path());
    std::env::set_var("EMBER_SESSION_SHIM", "off");
    let mut proc = spawn_agent(fake_agent_cmd("s-direct", work.path(), 3)).await.unwrap();
    std::env::remove_var("EMBER_SESSION_SHIM");
    assert!(matches!(proc.handle, ProcHandle::Direct(_)));
    assert!(proc.stderr.is_some() && !proc.reattached);
    assert_eq!(std::fs::read_dir(shims.path()).unwrap().count(), 0, "no shim files");
    let mut lines = BufReader::new(&mut proc.stdout).lines();
    let first = lines.next_line().await.unwrap().unwrap();
    assert_eq!(first, "tick 1");
    clear_runtime();
}

#[tokio::test]
async fn fr_s8_without_a_session_id_or_runtime_the_spawn_is_direct() {
    let _g = LOCK.lock().await;
    std::env::remove_var("EMBER_SESSION_SHIM");
    let work = tempfile::tempdir().unwrap();
    clear_runtime();
    let proc = spawn_agent(fake_agent_cmd("s-none", work.path(), 1)).await.unwrap();
    assert!(matches!(proc.handle, ProcHandle::Direct(_)));
    let shims = tempfile::tempdir().unwrap();
    install(shims.path());
    let mut cmd = fake_agent_cmd("x", work.path(), 1);
    cmd.env.retain(|(k, _)| k != SESSION_ENV);
    let proc = spawn_agent(cmd).await.unwrap();
    assert!(matches!(proc.handle, ProcHandle::Direct(_)));
    clear_runtime();
}

async fn next(rx: &mut mpsc::Receiver<AgentEvent>) -> AgentEvent {
    tokio::time::timeout(Duration::from_secs(15), rx.recv())
        .await
        .expect("timed out waiting for an agent event")
        .expect("agent event channel closed")
}

/// The Claude Code adapter, started with a session id, runs the fake CLI under a shim: a whole
/// turn works through the bridge, and `shutdown` ends the agent (stdin closed through the shim).
#[tokio::test]
async fn fr_s8_claude_adapter_runs_under_the_shim() {
    let _g = LOCK.lock().await;
    std::env::remove_var("EMBER_SESSION_SHIM");
    let shims = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    install(shims.path());
    let (tx, mut rx) = mpsc::channel(64);
    let mut run = ClaudeCodeAdapter::new(crate_dir().join("tests/fixtures/claude/fake-claude.sh"))
        .start(
            StartRequest {
                cwd: work.path().to_path_buf(),
                env: vec![(SESSION_ENV.into(), "s-claude".into())],
                ..Default::default()
            },
            tx,
        )
        .await
        .unwrap();
    assert!(
        std::fs::read_dir(shims.path()).unwrap().flatten().any(|e| e.file_name() == "s-claude.sock"),
        "the agent runs under a shim"
    );
    run.send("hello").await.unwrap();
    assert_eq!(next(&mut rx).await, AgentEvent::NativeSession { native_id: "fake-session".into() });
    assert!(matches!(next(&mut rx).await, AgentEvent::ToolCall { .. }));
    let AgentEvent::ApprovalRequested { approval_id, .. } = next(&mut rx).await else {
        panic!("expected approval");
    };
    run.answer(&approval_id, ApprovalDecision::AllowAlways).await.unwrap();
    let AgentEvent::ApprovalRequested { approval_id, .. } = next(&mut rx).await else {
        panic!("expected approval");
    };
    run.answer(&approval_id, ApprovalDecision::Deny).await.unwrap();
    assert!(matches!(next(&mut rx).await, AgentEvent::ToolResult { .. }));
    assert!(matches!(next(&mut rx).await, AgentEvent::Usage { .. }));
    assert_eq!(next(&mut rx).await, AgentEvent::TurnEnded { outcome: TurnOutcome::Completed });
    run.shutdown().await.unwrap();
    let args = std::fs::read_to_string(work.path().join("args.log")).unwrap();
    assert!(args.contains("--permission-prompt-tool stdio"), "{args}");
    clear_runtime();
}

// ---------------------------------------------------------------------------------------------
// Transactional acknowledgement, approval restoration, Codex drain, three-session handoff.

/// Run "server 1" on its own runtime and drop that runtime afterwards: every task, socket and
/// child handle of the old server goes away at once, as in a crash, while the shims stay.
fn run_server1<F: std::future::Future<Output = ()> + Send + 'static>(f: F) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(f);
        rt.shutdown_background();
    })
    .join()
    .unwrap();
}

async fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    for _ in 0..400 {
        if ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

fn events_of(store: &Store, id: &str) -> Vec<AgentEvent> {
    store.events_after(id, 0).unwrap().into_iter().map(|e| e.event).collect()
}

fn ended(store: &Store, id: &str) -> bool {
    events_of(store, id).iter().any(|e| matches!(e, AgentEvent::TurnEnded { .. }))
}

fn runtime(dir: &Path, store: &Arc<Store>) -> ShimRuntime {
    ShimRuntime {
        bin: PathBuf::from(env!("CARGO_BIN_EXE_ember-shim")),
        dir: dir.to_path_buf(),
        store: store.clone(),
    }
}

/// A shim that dies during a turn must not leave a stale live run blocking native resume.
#[cfg(unix)]
#[tokio::test]
async fn fr_s8_shim_killed_mid_turn_triggers_native_resume_on_next_message() {
    let _g = LOCK.lock().await;
    std::env::remove_var("EMBER_SESSION_SHIM");
    let shims = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let store = install(shims.path());
    let bin = crate_dir().join("tests/fixtures/claude/fake-claude.sh");
    let id = store
        .create_session("p", AgentKind::ClaudeCode, work.path().to_str().unwrap(), None, "t")
        .unwrap()
        .id;
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ClaudeCodeAdapter::new(bin)) as Arc<dyn AgentAdapter>]);
    sessions.send(&id, "first").await.unwrap();
    wait_for("native id and an active approval", || {
        store.session(&id).unwrap().unwrap().native_id.is_some()
            && events_of(&store, &id).iter().any(|e| matches!(e, AgentEvent::ApprovalRequested { .. }))
    }).await;
    let meta = scan(shims.path()).into_iter().find(|m| m.session == id).unwrap();
    unsafe { libc::kill(meta.shim_pid as libc::pid_t, libc::SIGKILL); }
    wait_for("the interrupted turn to end", || ended(&store, &id)).await;
    sessions.send(&id, "second").await.unwrap();
    wait_for("a resumed agent", || {
        std::fs::read_to_string(work.path().join("args.log"))
            .is_ok_and(|args| args.split_whitespace().any(|arg| arg == "--resume=fake-session"))
    }).await;
    sessions.release(&id).await.unwrap();
    clear_runtime();
}

/// Offsets are acknowledged only after the event they cover was committed: a server that dies
/// between reading a line and storing its event leaves that line to the next server.
#[tokio::test]
async fn fr_s8_crash_between_stdout_read_and_store_commit_replays_event() {
    let _g = LOCK.lock().await;
    std::env::remove_var("EMBER_SESSION_SHIM");
    let shims = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let store = install(shims.path());

    let gate = require_commit("s-commit");
    let proc = spawn_agent(fake_agent_cmd("s-commit", work.path(), 10)).await.unwrap();
    let mut lines = BufReader::new(proc.stdout).lines();
    let mut seen = Vec::new();
    while let Some(l) = tokio::time::timeout(Duration::from_secs(15), lines.next_line())
        .await
        .unwrap()
        .unwrap()
    {
        seen.extend(tick(&l));
        if seen.len() == 3 {
            break;
        }
    }
    // Tick 3 was read but not stored; ticks 1 and 2 were handled, yet nothing is acknowledged
    // until the store has them.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(store.shim_offset("s-commit").unwrap(), 0, "no ack before a commit");
    gate.commit_ready();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        store.shim_offset("s-commit").unwrap(),
        "tick 1\ntick 2\n".len() as u64,
        "the commit acknowledges the end of tick 2, not tick 3"
    );
    // Crash before tick 3 is stored.
    drop(lines);
    drop(proc.stdin);
    drop(proc.handle);
    release_commit("s-commit");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let proc = spawn_agent(fake_agent_cmd("s-commit", work.path(), 10)).await.unwrap();
    assert!(proc.reattached);
    let mut lines = BufReader::new(proc.stdout).lines();
    let first = tokio::time::timeout(Duration::from_secs(15), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(first, "tick 3", "the line that was read but not stored is replayed");
    let mut rest = vec![3];
    while let Some(l) = tokio::time::timeout(Duration::from_secs(20), lines.next_line())
        .await
        .unwrap()
        .unwrap()
    {
        rest.extend(tick(&l));
    }
    assert_eq!(rest, (3..=10).collect::<Vec<_>>(), "nothing after it is lost: {rest:?}");
    clear_runtime();
}

#[tokio::test]
async fn fr_s8_failed_event_store_never_acknowledges_later_output() {
    let _g = LOCK.lock().await;
    std::env::remove_var("EMBER_SESSION_SHIM");
    let shims = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let store = install(shims.path());
    let gate = require_commit("s-store-failed");
    let proc = spawn_agent(fake_agent_cmd("s-store-failed", work.path(), 10)).await.unwrap();
    let mut lines = BufReader::new(proc.stdout).lines();
    for _ in 0..3 {
        tokio::time::timeout(Duration::from_secs(15), lines.next_line()).await.unwrap().unwrap().unwrap();
    }
    // The pump must retain every subsequent line when one append fails, including at EOF.
    gate.store_failed();
    gate.commit_if_idle(|| true);
    gate.commit_ready();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(store.shim_offset("s-store-failed").unwrap(), 0);
    drop(lines);
    drop(proc.stdin);
    drop(proc.handle);
    release_commit("s-store-failed");
    let proc = spawn_agent(fake_agent_cmd("s-store-failed", work.path(), 10)).await.unwrap();
    assert!(proc.reattached);
    let mut lines = BufReader::new(proc.stdout).lines();
    assert_eq!(lines.next_line().await.unwrap().as_deref(), Some("tick 1"));
    proc.handle.shutdown(Duration::from_secs(1)).await.unwrap();
    clear_runtime();
}

/// A permission prompt that was pending when the server died can still be answered after the
/// new server rejoins the agent (the adapter's pending table is rebuilt from the store).
#[tokio::test]
async fn fr_s8_pending_approval_restored_and_answerable_after_server_crash() {
    let _g = LOCK.lock().await;
    std::env::remove_var("EMBER_SESSION_SHIM");
    let shims = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let store = install(shims.path());
    let bin = crate_dir().join("tests/fixtures/claude/fake-claude-approval.sh");
    let id = store
        .create_session("p", AgentKind::ClaudeCode, work.path().to_str().unwrap(), None, "t")
        .unwrap()
        .id;

    {
        let (store, bin, id) = (store.clone(), bin.clone(), id.clone());
        run_server1(async move {
            let sessions =
                Sessions::new(store.clone(), vec![Arc::new(ClaudeCodeAdapter::new(bin)) as Arc<dyn AgentAdapter>]);
            sessions.send(&id, "hello").await.unwrap();
            wait_for("the approval request", || {
                events_of(&store, &id).iter().any(|e| matches!(e, AgentEvent::ApprovalRequested { .. }))
            })
            .await;
            // Let the offset settle past the approval line, so the new server does not see it
            // again: the pending prompt must come from the store.
            wait_for("a stored offset", || store.shim_offset(&id).unwrap() > 0).await;
            tokio::time::sleep(Duration::from_millis(700)).await;
        });
    }
    assert!(store.shim_offset(&id).unwrap() > 0);

    let sessions =
        Sessions::new(store.clone(), vec![Arc::new(ClaudeCodeAdapter::new(bin)) as Arc<dyn AgentAdapter>]);
    let rejoined = sessions.rejoin_shims(&runtime(shims.path(), &store)).await;
    assert_eq!(rejoined, vec![id.clone()]);
    sessions
        .answer(&id, "r1", ApprovalDecision::AllowOnce)
        .await
        .expect("the approval taken over from the old server can be answered");
    wait_for("the turn to end", || ended(&store, &id)).await;
    let log = std::fs::read_to_string(work.path().join("stdin.log")).unwrap();
    assert!(log.contains("\"behavior\":\"allow\""), "the CLI got the allow response: {log}");
    sessions.release(&id).await.unwrap();
    clear_runtime();
}

/// Codex cannot be rejoined as it runs (its `initialize` needs a fresh process), but a turn in
/// flight when the server died is drained from the surviving app-server into the store before
/// that process is stopped.
#[tokio::test]
async fn fr_s8_codex_mid_turn_output_drained_and_persisted_after_server_crash() {
    let _g = LOCK.lock().await;
    std::env::remove_var("EMBER_SESSION_SHIM");
    let shims = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let store = install(shims.path());
    let bin = crate_dir().join("tests/fixtures/codex/fake-app-server.sh");
    let id = store
        .create_session("p", AgentKind::Codex, work.path().to_str().unwrap(), None, "t")
        .unwrap()
        .id;

    {
        let (store, bin, id) = (store.clone(), bin.clone(), id.clone());
        run_server1(async move {
            let sessions =
                Sessions::new(store.clone(), vec![Arc::new(CodexAdapter::new(bin)) as Arc<dyn AgentAdapter>]);
            sessions.send(&id, "go").await.unwrap();
            wait_for("two deltas", || {
                events_of(&store, &id)
                    .iter()
                    .filter(|e| matches!(e, AgentEvent::AssistantDelta { .. }))
                    .count()
                    >= 2
            })
            .await;
        });
    }
    let before = events_of(&store, &id).len();
    assert!(!ended(&store, &id), "the turn is still running when the server dies");
    let pid = scan(shims.path()).into_iter().find(|m| m.session == id).expect("the shim survives").agent_pid;
    assert!(pid_alive(pid));

    let sessions =
        Sessions::new(store.clone(), vec![Arc::new(CodexAdapter::new(bin)) as Arc<dyn AgentAdapter>]);
    let rejoined = sessions.rejoin_shims(&runtime(shims.path(), &store)).await;
    assert!(rejoined.is_empty(), "Codex is drained, not rejoined");
    let events = events_of(&store, &id);
    assert!(events.len() > before, "the rest of the turn was stored");
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::AssistantDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(text.contains("tick 25 "), "the last delta arrived: {text}");
    assert!(
        events.iter().any(|e| *e == AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }),
        "{events:?}"
    );
    assert_eq!(store.session(&id).unwrap().unwrap().status, SessionStatus::Finished);
    wait_for("the surviving app-server to be stopped", || !pid_alive(pid)).await;
    clear_runtime();
}

/// Update the server with three sessions running: none stops, none loses a turn.
#[tokio::test]
async fn fr_s8_update_server_with_three_sessions_running_none_stops_or_loses_turn() {
    let _g = LOCK.lock().await;
    std::env::remove_var("EMBER_SESSION_SHIM");
    let shims = tempfile::tempdir().unwrap();
    let store = install(shims.path());
    let bin = crate_dir().join("tests/fixtures/claude/fake-claude-ticks.sh");
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let ids: Vec<String> = dirs
        .iter()
        .map(|d| {
            store
                .create_session("p", AgentKind::ClaudeCode, d.path().to_str().unwrap(), None, "t")
                .unwrap()
                .id
        })
        .collect();
    let ticks_of = |id: &str| -> Vec<u32> {
        events_of(&store, id)
            .iter()
            .filter_map(|e| match e {
                AgentEvent::AssistantMessage { text } => tick(text),
                _ => None,
            })
            .collect()
    };

    {
        let (store, bin, ids) = (store.clone(), bin.clone(), ids.clone());
        run_server1(async move {
            let sessions =
                Sessions::new(store.clone(), vec![Arc::new(ClaudeCodeAdapter::new(bin)) as Arc<dyn AgentAdapter>]);
            for id in &ids {
                sessions.send(id, "go").await.unwrap();
            }
            wait_for("three ticks in every session", || {
                ids.iter().all(|id| {
                    events_of(&store, id)
                        .iter()
                        .filter(|e| matches!(e, AgentEvent::AssistantMessage { .. }))
                        .count()
                        >= 3
                })
            })
            .await;
        });
    }
    // The old server is gone: all three agents and shims run on.
    let metas = scan(shims.path());
    assert_eq!(metas.len(), 3);
    for m in &metas {
        assert!(pid_alive(m.shim_pid) && pid_alive(m.agent_pid), "{m:?}");
    }
    assert!(ids.iter().all(|id| !ended(&store, id)), "no turn ended while the server was away");

    let sessions =
        Sessions::new(store.clone(), vec![Arc::new(ClaudeCodeAdapter::new(bin)) as Arc<dyn AgentAdapter>]);
    let mut rejoined = sessions.rejoin_shims(&runtime(shims.path(), &store)).await;
    rejoined.sort();
    let mut expect = ids.clone();
    expect.sort();
    assert_eq!(rejoined, expect, "all three sessions are rejoined");
    for id in &ids {
        wait_for("the turn to end", || ended(&store, id)).await;
        let seen = ticks_of(id);
        let mut dedup = seen.clone();
        dedup.dedup();
        assert_eq!(dedup, (1..=30).collect::<Vec<_>>(), "no tick lost, in order: {seen:?}");
        assert!(seen.len() - dedup.len() <= 1, "at most the line in flight repeats: {seen:?}");
    }
    for id in &ids {
        sessions.release(id).await.unwrap();
    }
    clear_runtime();
}

/// A per-turn Antigravity process survives a server restart and its remaining output is stored.
#[tokio::test]
async fn fr_s8_antigravity_turn_survives_server_restart() {
    let _g = LOCK.lock().await;
    std::env::remove_var("EMBER_SESSION_SHIM");
    let shims = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let store = install(shims.path());
    let bin = crate_dir().join("tests/fixtures/agy/fake-agy-ticks.sh");
    let id = store.create_session("p", AgentKind::Antigravity, work.path().to_str().unwrap(), None, "t").unwrap().id;
    {
        let (store, bin, id, home) = (store.clone(), bin.clone(), id.clone(), home.path().to_path_buf());
        run_server1(async move {
            let hooks = HookBroker::new();
            hooks.set_base_url("http://127.0.0.1:1");
            let sessions = Sessions::new(store.clone(), vec![Arc::new(AntigravityAdapter::new(bin, home, hooks)) as Arc<dyn AgentAdapter>]);
            sessions.send(&id, "go").await.unwrap();
            wait_for("Antigravity output", || events_of(&store, &id).iter().filter(|e| matches!(e, AgentEvent::AssistantDelta { .. })).count() >= 3).await;
        });
    }
    let meta = scan(shims.path()).into_iter().find(|m| m.session == id).expect("shim survives");
    assert!(pid_alive(meta.shim_pid) && pid_alive(meta.agent_pid));
    let hooks = HookBroker::new();
    hooks.set_base_url("http://127.0.0.1:1");
    let sessions = Sessions::new(store.clone(), vec![Arc::new(AntigravityAdapter::new(bin, home.path(), hooks)) as Arc<dyn AgentAdapter>]);
    assert_eq!(sessions.rejoin_shims(&runtime(shims.path(), &store)).await, vec![id.clone()]);
    wait_for("Antigravity turn end", || ended(&store, &id)).await;
    let text: String = events_of(&store, &id).iter().filter_map(|e| match e { AgentEvent::AssistantDelta { text } => Some(text.clone()), _ => None }).collect();
    assert!(text.contains("tick 25 "), "{text}");
    assert!(events_of(&store, &id).contains(&AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }));
    sessions.release(&id).await.unwrap();
    clear_runtime();
}
