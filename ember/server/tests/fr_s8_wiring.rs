//! FR-S8 wiring: the adapters start their agent under `ember-shim` through the shared spawn
//! helper; the agent outlives the server, a new server reattaches from the stored offset, and
//! `EMBER_SESSION_SHIM=off` keeps direct children.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ember_server::agents::claude_code::ClaudeCodeAdapter;
use ember_server::agents::proc::{
    clear_runtime, install_runtime, spawn_agent, AgentCommand, ProcHandle, ShimRuntime, SESSION_ENV,
};
use ember_server::agents::{AgentAdapter, StartRequest};
use ember_server::events::{AgentEvent, ApprovalDecision, TurnOutcome};
use ember_server::session_shim::pid_alive;
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
