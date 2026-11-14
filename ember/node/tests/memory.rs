//! FR-X5 / NFR-X1: the daemon's memory stays bounded however many sessions use it.
//!
//! The node runs in this test process on an ephemeral port, so the process RSS is the daemon's
//! RSS plus the test client's. Each "session" is one round of file write, read, stat, list and a
//! process run, the requests an agent session sends. No new dependencies: RSS comes from
//! `/proc/self/status` on Linux and `ps -o rss=` elsewhere (macOS); CPU time from `getrusage`.
//!
//! `bounded_rss_over_many_sessions` is the CI gate with a generous budget (the budget is
//! [provisional] in SPEC FR-X5). `record_per_session_cost` is `#[ignore]`d: run it by hand
//! (`cargo test --release --test memory -- --ignored --nocapture`) on the real computer to record
//! per-session memory and CPU for the later measurement (#81).

use std::path::PathBuf;
use std::time::Duration;

use ember_node::api::{self, Node};
use ember_node::client::{shell, NodeClient};
use ember_node::config::NodeConfig;

const TOKEN: &str = "test-token";
/// Growth allowed between the warmed-up baseline and the end of the run. Allocator noise on
/// either OS is a few MiB; a leak per session would be far above this over 300 sessions.
const BUDGET_KIB: u64 = 64 * 1024;

/// Resident set size of this process in KiB.
fn rss_kib() -> u64 {
    if let Ok(s) = std::fs::read_to_string("/proc/self/status") {
        for line in s.lines() {
            if let Some(v) = line.strip_prefix("VmRSS:") {
                return v.trim().trim_end_matches("kB").trim().parse().expect("VmRSS number");
            }
        }
    }
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps available");
    String::from_utf8_lossy(&out.stdout).trim().parse().expect("ps rss number")
}

/// User plus system CPU time of this process, in milliseconds.
fn cpu_ms() -> u64 {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    let tv = |t: libc::timeval| t.tv_sec as u64 * 1000 + t.tv_usec as u64 / 1000;
    tv(ru.ru_utime) + tv(ru.ru_stime)
}

struct Fixture {
    client: NodeClient,
    root: PathBuf,
    _dir: tempfile::TempDir,
}

async fn start() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let node = Node::new(NodeConfig::new(TOKEN, vec![root.clone()])).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node));
    Fixture { client: NodeClient::new(&base, TOKEN).unwrap(), root, _dir: dir }
}

/// One session's worth of requests.
async fn session(f: &Fixture, n: usize) {
    tokio::time::timeout(Duration::from_secs(30), async {
        let path = f.root.join(format!("s{}.txt", n % 8));
        f.client.write_file(&path, vec![b'x'; 64 * 1024], None).await.unwrap();
        let r = f.client.read_file(&path).await.unwrap();
        assert!(!r.data.is_empty());
        f.client.stat(&path).await.unwrap();
        f.client.list(&f.root).await.unwrap();
        let out = f.client.run(shell("echo hi", &f.root)).await.unwrap();
        assert_eq!(out.code, Some(0));
    })
    .await
    .expect("session timed out");
}

#[tokio::test]
async fn bounded_rss_over_many_sessions() {
    let f = start().await;
    for n in 0..40 {
        session(&f, n).await;
    }
    let baseline = rss_kib();
    for n in 0..300 {
        session(&f, n).await;
    }
    let after = rss_kib();
    let growth = after.saturating_sub(baseline);
    assert!(
        growth <= BUDGET_KIB,
        "RSS grew {growth} KiB over 300 sessions (baseline {baseline} KiB, after {after} KiB, budget {BUDGET_KIB} KiB)"
    );
}

/// Records RSS and CPU per batch of sessions as CSV on stdout, for the real measurement.
#[tokio::test]
#[ignore = "recording harness for #81: run by hand on the target computer, release build"]
async fn record_per_session_cost() {
    let f = start().await;
    println!("sessions,rss_kib,cpu_ms");
    println!("0,{},{}", rss_kib(), cpu_ms());
    let mut done = 0;
    for batch in [10, 40, 50, 100, 300, 500] {
        for _ in 0..batch {
            session(&f, done).await;
            done += 1;
        }
        println!("{done},{},{}", rss_kib(), cpu_ms());
    }
}
