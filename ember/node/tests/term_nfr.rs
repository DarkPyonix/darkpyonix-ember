//! NFR-P1 measurement (SPEC §P): idle-session memory and attach-to-snapshot latency.
//!
//! Both tests are `#[ignore]` because they take about 15 s and are sensitive to machine load.
//! Run on demand: `cargo test --locked --test term_nfr -- --ignored --nocapture --test-threads=1`.
//!
//! Scope: **loopback only.** The 500 ms bound is asserted for a client on `127.0.0.1`; a LAN is
//! not measured here and the result must not be reported as a LAN result. Memory is the RSS
//! growth of this test process, which holds the node in-process; the shells and keepers are
//! separate processes and are not counted ("beyond the shell itself").

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ember_node::api::{self, Node};
use ember_node::client::NodeClient;
use ember_node::config::NodeConfig;
use ember_node::proto::*;

const TOKEN: &str = "test-token";
const SESSIONS: usize = 50;
const BYTES_PER_SESSION_LIMIT: u64 = 2 * 1024 * 1024;
const ATTACH_LIMIT: Duration = Duration::from_millis(500);
/// Housekeeping compacts a session after it has been idle this long (`IDLE_COMPACT_MS`), plus slack.
const IDLE_WAIT: Duration = Duration::from_secs(15);

struct Fixture {
    client: NodeClient,
    root: PathBuf,
    _node: Node,
    _dir: tempfile::TempDir,
}

async fn start() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let root = base.join("root");
    let state = base.join("s");
    std::fs::create_dir_all(&root).unwrap();
    let cfg = NodeConfig {
        state_dir: Some(state),
        pty_keeper: Some(PathBuf::from(env!("CARGO_BIN_EXE_ember-node"))),
        ..NodeConfig::new(TOKEN, vec![root.clone()])
    };
    let node = Node::new(cfg).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node.clone()));
    Fixture { client: NodeClient::new(&base_url, TOKEN).unwrap(), root, _node: node, _dir: dir }
}

fn req(root: &Path) -> TermCreateRequest {
    let mut env = std::collections::BTreeMap::new();
    env.insert("PS1".to_string(), "$ ".to_string());
    TermCreateRequest {
        program: Some(Program::Argv(vec!["/bin/sh".to_string()])),
        cwd: root.to_path_buf(),
        env,
        env_clear: false,
        size: Some(PtySize { rows: 24, cols: 80 }),
        origin: TermOrigin::IdeVscode,
        project: Some(root.display().to_string()),
        title: None,
        key: None,
        tags: Default::default(),
    }
}

fn hello() -> TermHello {
    TermHello {
        device: "nfr".into(),
        kind: Some("test".into()),
        pid: None,
        size: Some(PtySize { rows: 24, cols: 80 }),
        active: true,
        read_only: false,
        snapshot: true,
    }
}

/// Resident set size of this process in bytes (`ps` works on Linux and macOS; it reports KiB).
fn rss_bytes() -> u64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .expect("run ps");
    let kib: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().expect("parse ps rss");
    kib * 1024
}

#[tokio::test]
#[ignore = "measurement (about 15 s); run with --ignored --test-threads=1"]
async fn fifty_idle_sessions_cost_under_2_mb_each() {
    let f = start().await;
    // Warm up allocators, the thread pool and the first shell so the delta is per session.
    let warm = f.client.term_create(&req(&f.root)).await.unwrap().term;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let before = rss_bytes();

    let mut ids = vec![warm.id];
    for _ in 0..SESSIONS {
        ids.push(f.client.term_create(&req(&f.root)).await.unwrap().term.id);
    }
    // Let every shell print its prompt, then go idle so `Screen::compact` runs.
    tokio::time::sleep(IDLE_WAIT).await;
    let after = rss_bytes();

    let delta = after.saturating_sub(before);
    let per = delta / SESSIONS as u64;
    println!("NFR-P1 memory: {SESSIONS} idle sessions, RSS delta {delta} B, {per} B per session (limit {BYTES_PER_SESSION_LIMIT} B)");
    for id in ids {
        let _ = f.client.term_kill(&id, None).await;
    }
    assert!(per <= BYTES_PER_SESSION_LIMIT, "{per} B per idle session exceeds 2 MB");
}

#[tokio::test]
#[ignore = "measurement; run with --ignored --test-threads=1 (loopback only)"]
async fn attach_to_snapshot_is_under_500_ms_on_loopback() {
    let f = start().await;
    let t = f.client.term_create(&req(&f.root)).await.unwrap().term;
    // A screen with something on it, so the snapshot is not trivially empty.
    let mut a = f.client.term_attach(&t.id, &hello()).await.unwrap();
    a.tx.input("echo nfr-ready\n").await.unwrap();
    let mut out = String::new();
    while !out.contains("nfr-ready") {
        match a.recv().await.unwrap() {
            Some(TermEvent::Output { data }) | Some(TermEvent::Snapshot { data, .. }) => {
                out.push_str(&String::from_utf8_lossy(&data))
            }
            Some(_) => {}
            None => panic!("closed before output"),
        }
    }
    a.tx.detach().await.unwrap();
    drop(a);

    let mut worst = Duration::ZERO;
    for _ in 0..10 {
        let start = Instant::now();
        let mut b = f.client.term_attach(&t.id, &hello()).await.unwrap();
        loop {
            match tokio::time::timeout(Duration::from_secs(5), b.recv()).await.expect("no snapshot in 5 s").unwrap() {
                Some(TermEvent::Snapshot { .. }) => break,
                Some(_) => {}
                None => panic!("closed before snapshot"),
            }
        }
        worst = worst.max(start.elapsed());
        b.tx.detach().await.unwrap();
    }
    println!("NFR-P1 attach (loopback only, not a LAN figure): worst of 10 = {worst:?} (limit {ATTACH_LIMIT:?})");
    let _ = f.client.term_kill(&t.id, None).await;
    assert!(worst <= ATTACH_LIMIT, "attach to snapshot took {worst:?} on loopback");
}
