//! FR-P7: `GET /ports` lists the listening TCP ports of processes under node-owned jobs and
//! terminals, through the public HTTP API only. A listener opened by the test process itself
//! is not a node-owned descendant and must never be listed.

use std::path::PathBuf;
use std::time::Duration;

use ember_node::api::{self, Node};
use ember_node::client::{shell, ClientError, NodeClient};
use ember_node::config::NodeConfig;
use ember_node::proto::*;

const TOKEN: &str = "test-token";

struct Fixture {
    client: NodeClient,
    base: String,
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
    Fixture { client: NodeClient::new(&base, TOKEN).unwrap(), base, root, _dir: dir }
}

async fn within<T>(fut: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(90), fut).await.expect("timed out")
}

fn python_available() -> bool {
    std::process::Command::new("python3").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

const LISTENER: &str = "python3 -c 'import socket,time;s=socket.socket();s.bind((\"127.0.0.1\",0));s.listen();print(s.getsockname()[1],flush=True);time.sleep(60)'";

#[tokio::test]
async fn fr_p7_ports_needs_the_token() {
    let f = start().await;
    let e = NodeClient::new(&f.base, "wrong").unwrap().ports().await.unwrap_err();
    assert!(matches!(e, ClientError::Api { status: 401, .. }), "{e}");
}

#[tokio::test]
async fn fr_p7_no_sessions_means_no_ports() {
    let f = start().await;
    // A listener of the test process itself: not under any node-owned session.
    let mine = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let my_port = mine.local_addr().unwrap().port();
    let ports = within(f.client.ports()).await.unwrap().ports;
    assert!(ports.is_empty(), "{ports:?}");
    assert!(ports.iter().all(|p| p.port != my_port));
}

#[tokio::test]
async fn fr_p7_a_job_listener_is_listed_with_its_owner_and_the_test_process_is_not() {
    if !python_available() {
        eprintln!("python3 is missing: skipping fr_p7_a_job_listener_is_listed_with_its_owner_and_the_test_process_is_not");
        return;
    }
    if !cfg!(any(target_os = "linux", target_os = "macos")) {
        eprintln!("ports are only read on Linux and macOS: skipping");
        return;
    }
    let f = start().await;
    within(async {
        // The test process holds its own listener: it is not a descendant of a node job.
        let mine = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let my_port = mine.local_addr().unwrap().port();

        let job = f.client.start_job(&JobRequest { command: shell(LISTENER, &f.root), label: None }).await.unwrap();
        let port: u16 = loop {
            let d = f.client.job(&job.id, Some(4096)).await.unwrap();
            if let Some(p) = d.tail.lines().next().and_then(|l| l.trim().parse().ok()) {
                break p;
            }
            assert_eq!(d.info.state, JobState::Running, "the listener job ended early: {}", d.tail);
            tokio::time::sleep(Duration::from_millis(100)).await;
        };

        let ports = f.client.ports().await.unwrap().ports;
        let mine_listed = ports.iter().find(|p| p.port == port).unwrap_or_else(|| panic!("{port} not in {ports:?}"));
        assert_eq!(mine_listed.owner, PortOwner { kind: PortOwnerKind::Job, id: job.id.clone() });
        assert_eq!(mine_listed.scope, PortScope::Loopback);
        assert!(mine_listed.pid > 0);
        assert!(mine_listed.process.to_lowercase().contains("python"), "{}", mine_listed.process);

        // Security: the test process's own listener is absent.
        assert!(ports.iter().all(|p| p.port != my_port), "{ports:?}");

        // No command line text anywhere in the raw body.
        let raw = serde_json::to_string(&ports).unwrap();
        assert!(!raw.contains("socket"), "{raw}");
        assert!(!raw.contains("time.sleep"), "{raw}");
        assert!(!raw.contains("127.0.0.1\",0"), "{raw}");

        f.client.kill_job(&job.id, None).await.unwrap();
        drop(mine);
    })
    .await;
}
