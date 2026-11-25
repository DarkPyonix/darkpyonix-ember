//! FR-S8 wiring: the adapters start their agent under `ember-shim` through the shared spawn
//! helper; the agent outlives the server, a new server reattaches from the stored offset, and
//! `EMBER_SESSION_SHIM=off` keeps direct children.
//!
//! Every test that can start a shim holds a [`ShimGuard`] on its shim directory (#923): it kills
//! what the test started on scope exit, also on a panic, and fails the test if a shim of that
//! directory, or a process in such a shim's session, still runs after the test's own cleanup.

mod shim_guard;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ember_server::agents::acp::{AcpAdapter, AcpConfig, EnvPolicy};
use ember_server::agents::antigravity::{AntigravityAdapter, HookBroker};
use ember_server::agents::claude_code::ClaudeCodeAdapter;
use ember_server::agents::codex::CodexAdapter;
use ember_server::agents::proc::{
    clear_runtime, install_runtime, mem_limit_bytes, release_commit, require_commit, spawn_agent, AgentCommand,
    ProcHandle, ShimRuntime, SESSION_ENV,
};
use ember_server::agents::{AgentAdapter, AgentKind, StartRequest};
use ember_server::events::{AgentEvent, ApprovalDecision, SessionStatus, TurnOutcome};
use ember_server::session::{SessionError, Sessions};
use ember_server::session_shim::{pid_alive, scan};
use ember_server::store::Store;
use shim_guard::ShimGuard;
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
        mem_limit_mb: None,
        os_sandbox: false,
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
    let _guard = ShimGuard::new(&[shims.path()]);
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
    let _guard = ShimGuard::new(&[shims.path()]);
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
    let _guard = ShimGuard::new(&[shims.path()]);
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
    let _guard = ShimGuard::new(&[shims.path()]);
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

/// A named pipe at `path` (mode 0600).
#[cfg(unix)]
fn make_fifo(path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: a valid NUL-terminated path; mkfifo only creates a file.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "mkfifo {}", path.display());
}

/// Write one line to the named pipe at `path`; the open waits for the reader (the fixture).
#[cfg(unix)]
async fn open_fifo(path: &Path) {
    let path = path.to_path_buf();
    let write = tokio::task::spawn_blocking(move || {
        use std::io::Write;
        std::fs::OpenOptions::new().write(true).open(&path)?.write_all(b"go\n")
    });
    tokio::time::timeout(Duration::from_secs(60), write)
        .await
        .expect("the fixture opened its gate")
        .unwrap()
        .unwrap();
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
    let _guard = ShimGuard::new(&[shims.path()]);
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
    let _guard = ShimGuard::new(&[shims.path()]);
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
    // The final acknowledgement is written by the bridge's task on this runtime: let it run.
    _guard.settle().await;
    clear_runtime();
}

#[tokio::test]
async fn fr_s8_failed_event_store_never_acknowledges_later_output() {
    let _g = LOCK.lock().await;
    std::env::remove_var("EMBER_SESSION_SHIM");
    let shims = tempfile::tempdir().unwrap();
    let _guard = ShimGuard::new(&[shims.path()]);
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
    let _guard = ShimGuard::new(&[shims.path()]);
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
#[cfg(unix)]
#[tokio::test]
async fn fr_s8_codex_mid_turn_output_drained_and_persisted_after_server_crash() {
    let _g = LOCK.lock().await;
    std::env::remove_var("EMBER_SESSION_SHIM");
    let shims = tempfile::tempdir().unwrap();
    let _guard = ShimGuard::new(&[shims.path()]);
    let work = tempfile::tempdir().unwrap();
    let store = install(shims.path());
    let bin = crate_dir().join("tests/fixtures/codex/fake-app-server.sh");
    let id = store
        .create_session("p", AgentKind::Codex, work.path().to_str().unwrap(), None, "t")
        .unwrap()
        .id;
    // The fake pauses its turn after tick 2 until the gate is opened (#924): the turn is still
    // running when the server dies however slow this machine is, instead of the test having to
    // see two deltas within the 4.6 s the rest of the turn used to take.
    let gate = work.path().join("codex-gate");
    make_fifo(&gate);

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
    open_fifo(&gate).await;

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
        events.contains(&AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }),
        "{events:?}"
    );
    assert_eq!(store.session(&id).unwrap().unwrap().status, SessionStatus::Finished);
    wait_for("the surviving app-server to be stopped", || !pid_alive(pid)).await;
    clear_runtime();
}

/// An ACP agent mid-`session/prompt` when the server dies: the surviving agent's output is read
/// back and stored, the agent is stopped (its `initialize` handshake cannot be repeated), and the
/// session resumes natively on its next message (#284).
#[cfg(unix)]
#[tokio::test]
async fn fr_s8_acp_mid_turn_output_drained_and_persisted_after_server_crash() {
    let _g = LOCK.lock().await;
    std::env::remove_var("EMBER_SESSION_SHIM");
    let shims = tempfile::tempdir().unwrap();
    let _guard = ShimGuard::new(&[shims.path()]);
    let work = tempfile::tempdir().unwrap();
    let store = install(shims.path());
    let config = || AcpConfig {
        name: "fake-acp-s8".into(),
        command: crate_dir().join("tests/fixtures/acp/fake-acp-agent.sh"),
        args: Vec::new(),
        model_args: Vec::new(),
        instructions_args: Vec::new(),
        version_args: vec!["--version".into()],
        env: Vec::new(),
        env_policy: EnvPolicy::Inherit,
    };
    let kind = AcpAdapter::new(config()).unwrap().kind();
    let id = store.create_session("p", kind, work.path().to_str().unwrap(), None, "t").unwrap().id;

    {
        let (store, id) = (store.clone(), id.clone());
        let adapter = AcpAdapter::new(config()).unwrap();
        run_server1(async move {
            let sessions = Sessions::new(store.clone(), vec![Arc::new(adapter) as Arc<dyn AgentAdapter>]);
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

    let sessions = Sessions::new(
        store.clone(),
        vec![Arc::new(AcpAdapter::new(config()).unwrap()) as Arc<dyn AgentAdapter>],
    );
    let rejoined = sessions.rejoin_shims(&runtime(shims.path(), &store)).await;
    assert!(rejoined.is_empty(), "an ACP agent is drained, not rejoined");
    let events = events_of(&store, &id);
    assert!(events.len() > before, "the rest of the turn was stored");
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::AssistantDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(text.contains("tick 25 "), "the last chunk arrived: {text}");
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::AssistantMessage { text } if text.contains("tick 25 "))),
        "the message was completed: {events:?}"
    );
    assert!(
        events.contains(&AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }),
        "{events:?}"
    );
    assert_eq!(store.session(&id).unwrap().unwrap().status, SessionStatus::Finished);
    wait_for("the surviving agent to be stopped", || !pid_alive(pid)).await;
    clear_runtime();
}

/// #310: a tool call announced (and stored) before the server died is not stored a second time
/// when its updates are drained; it gets its final result once. A call that starts while no
/// server is attached is stored once as well.
#[cfg(unix)]
#[tokio::test]
async fn fr_s8_acp_drain_stores_each_tool_call_once() {
    let _g = LOCK.lock().await;
    std::env::remove_var("EMBER_SESSION_SHIM");
    let shims = tempfile::tempdir().unwrap();
    let _guard = ShimGuard::new(&[shims.path()]);
    let work = tempfile::tempdir().unwrap();
    let store = install(shims.path());
    let config = || AcpConfig {
        name: "fake-acp-s8".into(),
        command: crate_dir().join("tests/fixtures/acp/fake-acp-agent.sh"),
        args: Vec::new(),
        model_args: Vec::new(),
        instructions_args: Vec::new(),
        version_args: vec!["--version".into()],
        env: vec![("FAKE_TOOLS".into(), "1".into())],
        env_policy: EnvPolicy::Inherit,
    };
    let kind = AcpAdapter::new(config()).unwrap().kind();
    let id = store.create_session("p", kind, work.path().to_str().unwrap(), None, "t").unwrap().id;
    let calls = |events: &[AgentEvent], want: &str| {
        events.iter().filter(|e| matches!(e, AgentEvent::ToolCall { call_id, .. } if call_id == want)).count()
    };
    let results = |events: &[AgentEvent], want: &str| -> Vec<(String, bool)> {
        events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolResult { call_id, output, is_error } if call_id == want => {
                    Some((output.clone(), *is_error))
                }
                _ => None,
            })
            .collect()
    };

    {
        let (store, id) = (store.clone(), id.clone());
        let adapter = AcpAdapter::new(config()).unwrap();
        run_server1(async move {
            let sessions = Sessions::new(store.clone(), vec![Arc::new(adapter) as Arc<dyn AgentAdapter>]);
            sessions.send(&id, "go").await.unwrap();
            wait_for("the announced tool call and two deltas", || {
                let events = events_of(&store, &id);
                events.iter().any(|e| matches!(e, AgentEvent::ToolCall { call_id, .. } if call_id == "t1"))
                    && events.iter().filter(|e| matches!(e, AgentEvent::AssistantDelta { .. })).count() >= 2
            })
            .await;
        });
    }
    let before = events_of(&store, &id);
    assert!(!ended(&store, &id), "the turn is still running when the server dies");
    assert_eq!(calls(&before, "t1"), 1, "t1 was announced before the crash: {before:?}");
    assert!(results(&before, "t1").is_empty(), "t1 is not finished before the crash: {before:?}");
    assert_eq!(calls(&before, "t2"), 0, "t2 starts only after the crash: {before:?}");
    let pid = scan(shims.path()).into_iter().find(|m| m.session == id).expect("the shim survives").agent_pid;

    let sessions = Sessions::new(
        store.clone(),
        vec![Arc::new(AcpAdapter::new(config()).unwrap()) as Arc<dyn AgentAdapter>],
    );
    let rejoined = sessions.rejoin_shims(&runtime(shims.path(), &store)).await;
    assert!(rejoined.is_empty(), "an ACP agent is drained, not rejoined");
    let events = events_of(&store, &id);
    assert_eq!(calls(&events, "t1"), 1, "the stored call is not repeated: {events:?}");
    assert_eq!(results(&events, "t1"), vec![("one".to_string(), false)], "{events:?}");
    assert_eq!(calls(&events, "t2"), 1, "the call that started during the drain is stored once: {events:?}");
    assert_eq!(results(&events, "t2"), vec![("two".to_string(), false)], "{events:?}");
    assert!(
        events.contains(&AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }),
        "{events:?}"
    );
    assert_eq!(store.session(&id).unwrap().unwrap().status, SessionStatus::Finished);
    wait_for("the surviving agent to be stopped", || !pid_alive(pid)).await;
    clear_runtime();
}

/// Update the server with three sessions running: none stops, none loses a turn.
#[tokio::test]
async fn fr_s8_update_server_with_three_sessions_running_none_stops_or_loses_turn() {
    let _g = LOCK.lock().await;
    std::env::remove_var("EMBER_SESSION_SHIM");
    let shims = tempfile::tempdir().unwrap();
    let _guard = ShimGuard::new(&[shims.path()]);
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
    let _guard = ShimGuard::new(&[shims.path()]);
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

#[test]
fn fr_s8_memory_limit_request_wins_over_environment_and_zero_means_none() {
    assert_eq!(mem_limit_bytes(Some(512), Some("2048")), Some(512 * 1024 * 1024));
    assert_eq!(mem_limit_bytes(None, Some("2048")), Some(2048 * 1024 * 1024));
    assert_eq!(mem_limit_bytes(None, Some(" 64 ")), Some(64 * 1024 * 1024));
    assert_eq!(mem_limit_bytes(None, Some("many")), None);
    assert_eq!(mem_limit_bytes(None, None), None);
    assert_eq!(mem_limit_bytes(Some(0), None), None);
}

fn memhog_cmd(session: &str, cwd: &Path, limit_mb: Option<u64>) -> AgentCommand {
    AgentCommand {
        program: crate_dir().join("tests/fixtures/shim/memhog.sh"),
        args: Vec::new(),
        env: vec![(SESSION_ENV.into(), session.into())],
        cwd: cwd.to_path_buf(),
        reattachable: false,
        mem_limit_mb: limit_mb,
        os_sandbox: false,
    }
}

/// The limit set on the request reaches the shim: the session over it ends, another session
/// without a limit keeps running (#284, FR-L20).
#[cfg(unix)]
#[tokio::test]
async fn fr_s8_per_session_memory_limit_from_the_start_request_ends_only_that_session() {
    let _g = LOCK.lock().await;
    std::env::remove_var("EMBER_SESSION_SHIM");
    std::env::remove_var("EMBER_SESSION_MEM_LIMIT_MB");
    std::env::set_var("EMBER_SESSION_MEM_POLL_MS", "100");
    let shims = tempfile::tempdir().unwrap();
    let _guard = ShimGuard::new(&[shims.path()]);
    let work = tempfile::tempdir().unwrap();
    let _store = install(shims.path());

    let hog = spawn_agent(memhog_cmd("s-hog", work.path(), Some(1))).await.unwrap();
    let free = spawn_agent(memhog_cmd("s-free", work.path(), None)).await.unwrap();
    let ProcHandle::Shim { exited: hog_exited, .. } = &hog.handle else { panic!("runs under a shim") };
    let mut hog_exited = hog_exited.clone();
    tokio::time::timeout(Duration::from_secs(20), hog_exited.wait_for(|v| *v))
        .await
        .expect("the session over its limit ends")
        .unwrap();
    let ProcHandle::Shim { exited: free_exited, .. } = &free.handle else { panic!("runs under a shim") };
    assert!(!*free_exited.borrow(), "the session without a limit continues");
    let pid = free.pid.expect("pid");
    assert!(pid_alive(pid));
    // Both sessions end as the server ends them; the ended one is acknowledged too (#923), so
    // neither shim lingers.
    let _ = hog.handle.shutdown(Duration::from_millis(200)).await;
    let _ = free.handle.shutdown(Duration::from_millis(200)).await;
    std::env::remove_var("EMBER_SESSION_MEM_POLL_MS");
    clear_runtime();
}

/// `Sessions::set_mem_limit_mb` is what `ensure_live` hands each adapter as the start request's
/// limit (FR-L20): per agent kind, cleared by `None` or zero.
#[test]
fn fr_s8_sessions_keep_a_memory_limit_per_agent_kind() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store, Vec::new());
    assert_eq!(sessions.mem_limit_mb(AgentKind::Codex), None);
    sessions.set_mem_limit_mb(AgentKind::Codex, Some(2048));
    assert_eq!(sessions.mem_limit_mb(AgentKind::Codex), Some(2048));
    assert_eq!(sessions.mem_limit_mb(AgentKind::ClaudeCode), None, "only that agent kind");
    sessions.set_mem_limit_mb(AgentKind::Codex, Some(0));
    assert_eq!(sessions.mem_limit_mb(AgentKind::Codex), None);
}

/// `ensure_live` puts the configured limit on the start request the adapter receives (FR-L20).
#[tokio::test]
async fn fr_s8_start_request_carries_the_sessions_memory_limit() {
    use ember_server::agents::scripted::ScriptedAdapter;
    let work = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    sessions.set_mem_limit_mb(AgentKind::Scripted, Some(768));
    let seen = Arc::new(std::sync::Mutex::new(None));
    {
        let seen = seen.clone();
        sessions.add_start_config_hook(Arc::new(move |_rec, req| {
            *seen.lock().unwrap() = Some(req.mem_limit_mb);
            anyhow::bail!("stop here")
        }));
    }
    let id = store.create_session("p", AgentKind::Scripted, work.path().to_str().unwrap(), None, "t").unwrap().id;
    let _ = sessions.send(&id, "hi").await;
    assert_eq!(*seen.lock().unwrap(), Some(Some(768)));
}

fn acp_recovery_config() -> AcpConfig {
    AcpConfig {
        name: "fake-acp-s8".into(),
        command: crate_dir().join("tests/fixtures/acp/fake-acp-agent.sh"),
        args: Vec::new(),
        model_args: Vec::new(),
        instructions_args: Vec::new(),
        version_args: vec!["--version".into()],
        env: Vec::new(),
        env_policy: EnvPolicy::Inherit,
    }
}

fn status_of(store: &Store, id: &str) -> SessionStatus {
    store.session(id).unwrap().unwrap().status
}

/// A session left mid-turn in the store with no agent behind it: what a crash leaves when the
/// agent died with the server.
fn orphan_mid_turn(store: &Store, kind: AgentKind, cwd: &Path) -> String {
    let id = store.create_session("p", kind, cwd.to_str().unwrap(), None, "orphan").unwrap().id;
    store.append(&id, &AgentEvent::UserMessage { text: "hi".into() }).unwrap();
    assert_eq!(status_of(store, &id), SessionStatus::Running);
    id
}

/// #310, the startup order: the server start goes through `recover_after_restart`, and from the
/// state a crash leaves (the session still `running` in the store, nothing fixed up by hand) it
/// reaches the drain. With the old order (every mid-turn session marked idle first) the drain
/// never ran in the server binary, because it only looks at mid-turn sessions.
///
/// The drain runs in the background: `recover_after_restart` returns while the agent's turn is
/// still running, so the listeners are not held back. Until the drain ends the session is
/// guarded: a message to it is refused as busy and starts no second agent.
#[tokio::test]
async fn fr_s8_recovery_drains_a_running_acp_session_in_the_background_and_guards_it() {
    let _g = LOCK.lock().await;
    std::env::remove_var("EMBER_SESSION_SHIM");
    let shims = tempfile::tempdir().unwrap();
    let _guard = ShimGuard::new(&[shims.path()]);
    let work = tempfile::tempdir().unwrap();
    let store = install(shims.path());
    let kind = AcpAdapter::new(acp_recovery_config()).unwrap().kind();
    let id = store.create_session("p", kind, work.path().to_str().unwrap(), None, "t").unwrap().id;

    {
        let (store, id) = (store.clone(), id.clone());
        let adapter = AcpAdapter::new(acp_recovery_config()).unwrap();
        run_server1(async move {
            let sessions = Sessions::new(store.clone(), vec![Arc::new(adapter) as Arc<dyn AgentAdapter>]);
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
    // What the next server finds: the status as stored at the crash.
    assert_eq!(status_of(&store, &id), SessionStatus::Running);
    let pid = scan(shims.path()).into_iter().find(|m| m.session == id).expect("the shim survives").agent_pid;
    assert!(pid_alive(pid));

    let sessions = Sessions::new(
        store.clone(),
        vec![Arc::new(AcpAdapter::new(acp_recovery_config()).unwrap()) as Arc<dyn AgentAdapter>],
    );
    let recovery = sessions.recover_after_restart(Some(&runtime(shims.path(), &store))).await.unwrap();
    assert_eq!(recovery.draining, vec![id.clone()], "the interrupted turn is being drained");
    assert!(recovery.rejoined.is_empty(), "an ACP agent is drained, not rejoined");
    assert_eq!(recovery.reset, 0, "a draining session is not marked idle");
    // The function returned while the turn still runs (about 4 s of ticks are left): nothing
    // that follows it in the server start waits for the agent.
    assert!(!ended(&store, &id), "recover_after_restart did not wait for the turn to end");
    assert!(sessions.is_draining(&id));
    assert_eq!(status_of(&store, &id), SessionStatus::Running, "the stored status stays true while it drains");
    // A message to the draining session is refused as busy; it starts no second agent and
    // stores nothing.
    match sessions.send(&id, "are you there?").await {
        Err(SessionError::Busy(busy)) => assert_eq!(busy, id),
        other => panic!("a draining session must refuse a message as busy, got {other:?}"),
    }
    assert!(!sessions.is_live(&id).await, "no agent was started for the draining session");
    let shims_of_session: Vec<_> = scan(shims.path()).into_iter().filter(|m| m.session == id).collect();
    assert!(shims_of_session.iter().all(|m| m.agent_pid == pid), "no second agent: {}", shims_of_session.len());
    assert!(
        !events_of(&store, &id)
            .iter()
            .any(|e| matches!(e, AgentEvent::UserMessage { text } if text == "are you there?")),
        "the refused message is not stored"
    );
    // The background drain ends on its own; the guard is released with it.
    for _ in 0..1200 {
        if !sessions.is_draining(&id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!sessions.is_draining(&id), "the drain ended within 60 s");
    let events = events_of(&store, &id);
    assert!(events.len() > before, "the rest of the turn was stored");
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::AssistantMessage { text } if text.contains("tick 25 "))),
        "the message was completed: {events:?}"
    );
    assert!(
        events.contains(&AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }),
        "{events:?}"
    );
    assert_eq!(status_of(&store, &id), SessionStatus::Finished);
    wait_for("the surviving agent to be stopped", || !pid_alive(pid)).await;
    clear_runtime();
}

/// #310: a session whose agent is rejoined keeps the status it had at the crash (here it waits
/// on an approval), and in the same start a mid-turn session with no live shim is marked idle.
#[tokio::test]
async fn fr_s8_recovery_keeps_a_rejoined_session_mid_turn_and_marks_an_orphan_idle() {
    let _g = LOCK.lock().await;
    std::env::remove_var("EMBER_SESSION_SHIM");
    let shims = tempfile::tempdir().unwrap();
    let _guard = ShimGuard::new(&[shims.path()]);
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
            wait_for("a stored offset", || store.shim_offset(&id).unwrap() > 0).await;
            tokio::time::sleep(Duration::from_millis(700)).await;
        });
    }
    assert_eq!(status_of(&store, &id), SessionStatus::WaitingForApproval);
    // Its agent died with the old server: no shim, but the store still says running.
    let orphan = orphan_mid_turn(&store, AgentKind::ClaudeCode, work.path());

    let sessions =
        Sessions::new(store.clone(), vec![Arc::new(ClaudeCodeAdapter::new(bin)) as Arc<dyn AgentAdapter>]);
    let recovery = sessions.recover_after_restart(Some(&runtime(shims.path(), &store))).await.unwrap();
    assert_eq!(recovery.rejoined, vec![id.clone()]);
    assert!(recovery.draining.is_empty());
    assert_eq!(recovery.reset, 1, "only the orphan is marked idle");
    assert_eq!(status_of(&store, &orphan), SessionStatus::Idle);
    assert_eq!(
        status_of(&store, &id),
        SessionStatus::WaitingForApproval,
        "the rejoined session's turn is still running under its shim"
    );
    assert!(sessions.is_live(&id).await, "the rejoined agent is attached");
    assert!(!sessions.is_live(&orphan).await);
    // The turn goes on from where it was.
    sessions
        .answer(&id, "r1", ApprovalDecision::AllowOnce)
        .await
        .expect("the approval taken over from the old server can be answered");
    wait_for("the turn to end", || ended(&store, &id)).await;
    sessions.release(&id).await.unwrap();
    clear_runtime();
}

/// #310: with shims off (`rt = None`) no agent outlived the server, so every mid-turn session
/// is marked idle, as before; a session that is not mid-turn is left alone.
#[tokio::test]
async fn fr_s8_recovery_without_shims_marks_every_mid_turn_session_idle() {
    let work = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let running = orphan_mid_turn(&store, AgentKind::ClaudeCode, work.path());
    let waiting = orphan_mid_turn(&store, AgentKind::ClaudeCode, work.path());
    store
        .append(
            &waiting,
            &AgentEvent::ApprovalRequested {
                approval_id: "a1".into(),
                tool: "Bash".into(),
                input: serde_json::json!({}),
                needs_person: false,
            },
        )
        .unwrap();
    assert_eq!(status_of(&store, &waiting), SessionStatus::WaitingForApproval);
    let done = orphan_mid_turn(&store, AgentKind::ClaudeCode, work.path());
    store.append(&done, &AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }).unwrap();

    let bin = crate_dir().join("tests/fixtures/claude/fake-claude-approval.sh");
    let sessions =
        Sessions::new(store.clone(), vec![Arc::new(ClaudeCodeAdapter::new(bin)) as Arc<dyn AgentAdapter>]);
    let recovery = sessions.recover_after_restart(None).await.unwrap();
    assert!(recovery.rejoined.is_empty() && recovery.draining.is_empty());
    assert_eq!(recovery.reset, 2);
    assert_eq!(status_of(&store, &running), SessionStatus::Idle);
    assert_eq!(status_of(&store, &waiting), SessionStatus::Idle);
    assert_eq!(status_of(&store, &done), SessionStatus::Finished);
}

/// #923: a session the server had to kill is acknowledged, so its shim exits at once instead of
/// lingering for its linger time (an hour by default).
#[cfg(unix)]
#[tokio::test]
async fn fr_s8_shutdown_after_kill_lets_the_shim_exit() {
    let _g = LOCK.lock().await;
    std::env::remove_var("EMBER_SESSION_SHIM");
    std::env::remove_var("EMBER_SESSION_MEM_LIMIT_MB");
    let shims = tempfile::tempdir().unwrap();
    let _guard = ShimGuard::new(&[shims.path()]);
    let work = tempfile::tempdir().unwrap();
    install(shims.path());
    // memhog ignores its stdin, so closing it does not end the session: shutdown has to kill.
    let proc = spawn_agent(memhog_cmd("s-kill", work.path(), None)).await.unwrap();
    let shim = scan(shims.path()).into_iter().find(|m| m.session == "s-kill").expect("a shim").shim_pid;
    proc.handle.shutdown(Duration::from_millis(200)).await.unwrap();
    shim_guard::wait_ended(shim, "the shim after shutdown").await;
    drop(proc.stdout);
    clear_runtime();
}

/// #923: the same when the adapter dropped its reader of the agent's output before the shutdown
/// (the bridge still learns the end of the agent and acknowledges it).
#[cfg(unix)]
#[tokio::test]
async fn fr_s8_shutdown_after_the_reader_was_dropped_lets_the_shim_exit() {
    let _g = LOCK.lock().await;
    std::env::remove_var("EMBER_SESSION_SHIM");
    std::env::remove_var("EMBER_SESSION_MEM_LIMIT_MB");
    let shims = tempfile::tempdir().unwrap();
    let _guard = ShimGuard::new(&[shims.path()]);
    let work = tempfile::tempdir().unwrap();
    install(shims.path());
    let proc = spawn_agent(fake_agent_cmd("s-dropped", work.path(), 1000)).await.unwrap();
    let shim = scan(shims.path()).into_iter().find(|m| m.session == "s-dropped").expect("a shim").shim_pid;
    let mut lines = BufReader::new(proc.stdout).lines();
    tokio::time::timeout(Duration::from_secs(15), lines.next_line()).await.unwrap().unwrap().unwrap();
    drop(lines);
    // fake-agent keeps ticking after its stdin closes: shutdown has to kill it.
    proc.handle.shutdown(Duration::from_millis(200)).await.unwrap();
    shim_guard::wait_ended(shim, "the shim after shutdown").await;
    clear_runtime();
}
