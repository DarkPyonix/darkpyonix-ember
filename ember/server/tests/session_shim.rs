//! FR-S8: agents outlive the ember server. Uses the real `ember-shim` binary and a fake agent.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ember_server::session_shim::{
    attach, attach_or_resume, pid_alive, reattach_all, scan, spawn_shim, Reattach, ShimClient,
    ShimEvent, ShimSpec,
};

fn shim_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ember-shim"))
}

fn agent() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shim/fake-agent.sh")
}

fn spec(id: &str, cwd: &Path) -> ShimSpec {
    ShimSpec {
        session: id.into(),
        program: agent(),
        args: Vec::new(),
        cwd: cwd.to_path_buf(),
        env: Vec::new(),
        mem_limit_bytes: None,
        mem_poll_ms: Some(100),
        linger_secs: Some(5),
    }
}

/// Collect output text until `needle` appears or the stream ends; returns (text, end offset).
async fn read_until(c: &mut ShimClient, needle: &str) -> (String, u64) {
    let mut text = String::new();
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(15), c.events.recv())
            .await
            .expect("timed out")
            .expect("stream closed before needle");
        match ev {
            ShimEvent::Output { start, data } => {
                text.push_str(&String::from_utf8_lossy(&data));
                if text.contains(needle) {
                    return (text, start + data.len() as u64);
                }
            }
            ShimEvent::Exited(r) => panic!("agent exited ({r}) before {needle:?}; got {text:?}"),
        }
    }
}

async fn wait_exit(c: &mut ShimClient) -> String {
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(15), c.events.recv())
            .await
            .expect("timed out")
            .expect("closed");
        if let ShimEvent::Exited(r) = ev {
            return r;
        }
    }
}

#[tokio::test]
async fn fr_s8_agent_keeps_running_after_server_handle_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = spawn_shim(&shim_bin(), dir.path(), &spec("a", dir.path())).await.unwrap();
    read_until(&mut c, "tick 1").await;
    let meta = scan(dir.path()).pop().expect("meta file");
    drop(c);
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(pid_alive(meta.agent_pid), "agent died with the server handle");
    assert!(pid_alive(meta.shim_pid), "shim died with the server handle");
    // clean up
    let c = attach(dir.path(), "a", 0).await.unwrap();
    c.kill().await.unwrap();
}

#[tokio::test]
async fn fr_s8_new_server_reattaches_and_receives_subsequent_output() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = spawn_shim(&shim_bin(), dir.path(), &spec("b", dir.path())).await.unwrap();
    let (_, end) = read_until(&mut c, "tick 2").await;
    c.ack(end).await.unwrap();
    drop(c); // the "server" goes away mid-turn

    let reattached = reattach_all(dir.path(), &HashMap::from([("b".to_string(), end)])).await;
    assert_eq!(reattached.len(), 1);
    let (id, r) = reattached.into_iter().next().unwrap();
    assert_eq!(id, "b");
    let Reattach::Live(mut c2) = r else { panic!("expected a live shim") };
    let (text, _) = read_until(&mut c2, "tick 6").await;
    assert!(!text.contains("tick 1\n"), "replayed output that was already acked: {text:?}");
    c2.send_stdin(b"hello\n").await.unwrap();
    read_until(&mut c2, "echo hello").await;
    c2.kill().await.unwrap();
}

#[tokio::test]
async fn fr_s8_unacked_output_is_replayed_after_reattach() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = spawn_shim(&shim_bin(), dir.path(), &spec("r", dir.path())).await.unwrap();
    read_until(&mut c, "tick 2").await; // no ack
    drop(c);
    let Reattach::Live(mut c2) = attach_or_resume(dir.path(), "r", 0).await else {
        panic!("expected a live shim")
    };
    let (text, _) = read_until(&mut c2, "tick 3").await;
    assert!(text.starts_with("tick 1\n"), "{text:?}");
    c2.kill().await.unwrap();
}

#[tokio::test]
async fn fr_s8_missing_shim_falls_back_to_native_resume() {
    let dir = tempfile::tempdir().unwrap();
    // Never existed.
    assert!(matches!(attach_or_resume(dir.path(), "none", 0).await, Reattach::Resume));

    // Existed, then the shim was killed: stale files are left behind.
    let mut c = spawn_shim(&shim_bin(), dir.path(), &spec("dead", dir.path())).await.unwrap();
    read_until(&mut c, "tick 1").await;
    let meta = scan(dir.path()).pop().unwrap();
    drop(c);
    // SAFETY: killing the test's own shim and agent processes.
    unsafe {
        libc::kill(meta.shim_pid as i32, libc::SIGKILL);
        libc::kill(meta.agent_pid as i32, libc::SIGKILL);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let all = reattach_all(dir.path(), &HashMap::new()).await;
    assert_eq!(all.len(), 1);
    assert!(matches!(all[0].1, Reattach::Resume));
    assert!(scan(dir.path()).is_empty(), "stale shim files were not removed");
}

#[tokio::test]
async fn fr_s8_memory_limit_ends_only_the_offending_session() {
    let dir = tempfile::tempdir().unwrap();
    let mut limited = spec("limited", dir.path());
    limited.mem_limit_bytes = Some(1024); // any shell exceeds 1 KiB resident
    let mut bad = spawn_shim(&shim_bin(), dir.path(), &limited).await.unwrap();
    let mut good = spawn_shim(&shim_bin(), dir.path(), &spec("good", dir.path())).await.unwrap();

    assert_eq!(wait_exit(&mut bad).await, "memory-limit");
    // The other session is untouched and keeps producing output.
    read_until(&mut good, "tick 3").await;
    let meta = scan(dir.path()).into_iter().find(|m| m.session == "good").unwrap();
    assert!(pid_alive(meta.agent_pid));
    good.kill().await.unwrap();
}

#[tokio::test]
async fn fr_s8_exit_status_is_reported_to_a_reattached_server() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = spec("quick", dir.path());
    s.env = vec![("FAKE_TICKS".into(), "2".into())];
    let c = spawn_shim(&shim_bin(), dir.path(), &s).await.unwrap();
    drop(c);
    tokio::time::sleep(Duration::from_millis(800)).await; // agent ended while no server attached
    let Reattach::Live(mut c2) = attach_or_resume(dir.path(), "quick", 0).await else {
        panic!("shim should linger until the exit is acknowledged")
    };
    assert_eq!(wait_exit(&mut c2).await, "exit:0");
}
