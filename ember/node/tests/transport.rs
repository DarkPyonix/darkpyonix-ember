//! The node API over the in-memory fake transport (SPEC FR-N1, FR-N3, FR-N5): the same router
//! and the same `NodeClient` API as over TCP, reached by peer id; unknown and revoked peers are
//! refused.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ember_node::api::Node;
use ember_node::client::{shell, ClientError, NodeClient};
use ember_node::config::NodeConfig;
use ember_node::proto::*;
use ember_node::transport;
use ember_transport::mem::MemNetwork;
use ember_transport::{Dialer, PeerGate, Transport};

const TOKEN: &str = "test-token";

struct Fixture {
    net: MemNetwork,
    node_t: Transport,
    server_t: Transport,
    gate: PeerGate,
    client: NodeClient,
    root: PathBuf,
    _dir: tempfile::TempDir,
}

async fn start() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let node = Node::new(NodeConfig::new(TOKEN, vec![root.clone()])).unwrap();

    let net = MemNetwork::new();
    let (node_t, server_t) = (net.transport(), net.transport());
    // The node admits only the server.
    let gate = PeerGate::allow_list([server_t.peer_id()]);
    tokio::spawn(transport::serve(&node_t, node, gate.clone()).unwrap());

    let client = NodeClient::over_transport(Dialer::new(server_t.clone()), node_t.peer_id(), TOKEN);
    Fixture { net, node_t, server_t, gate, client, root, _dir: dir }
}

async fn within<T>(fut: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(20), fut).await.expect("timed out")
}

/// The first health check (retried briefly, though the listener is registered synchronously).
async fn wait_up(c: &NodeClient) -> Health {
    for _ in 0..200 {
        if let Ok(h) = c.health().await {
            return h;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("node never came up over the transport");
}

#[tokio::test]
async fn http_and_websockets_work_over_the_transport() {
    let f = start().await;
    let h = within(wait_up(&f.client)).await;
    assert!(h.ok);
    assert_eq!(h.protocol, PROTOCOL_VERSION);
    assert!(f.client.peer().is_some());
    assert!(f.client.to_string().starts_with("peer:"));

    // Bearer token is still enforced on top of the peer allow-list.
    let wrong = NodeClient::over_transport(Dialer::new(f.server_t.clone()), f.node_t.peer_id(), "wrong");
    let e = within(wrong.env()).await.unwrap_err();
    assert!(matches!(e, ClientError::Api { status: 401, .. }), "{e:?}");

    // Plain requests: write, read, env.
    let path = f.root.join("hello.txt");
    within(f.client.write_file(&path, b"over the transport".to_vec(), None)).await.unwrap();
    let r = within(f.client.read_file(&path)).await.unwrap();
    assert_eq!(r.data, b"over the transport");
    let env = within(f.client.env()).await.unwrap();
    assert_eq!(env.roots, vec![f.root.clone()]);

    // A WebSocket: exec run.
    let out = within(f.client.run(shell("echo hi; echo err >&2; exit 3", &f.root))).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hi\n");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "err\n");
    assert_eq!(out.code, Some(3));

    // Several commands at once share one transport connection, one stream each.
    let (a, b) = tokio::join!(
        f.client.run(shell("echo a", &f.root)),
        f.client.run(shell("echo b", &f.root))
    );
    assert_eq!(a.unwrap().stdout, b"a\n");
    assert_eq!(b.unwrap().stdout, b"b\n");

    // Query strings survive the trip (terms listing).
    let none = within(f.client.terms(&TermListQuery { running: Some(true), ..Default::default() }))
        .await
        .unwrap();
    assert!(none.is_empty());
}

fn term_req(root: &Path) -> TermCreateRequest {
    let mut env = std::collections::BTreeMap::new();
    env.insert("PS1".to_string(), "$ ".to_string());
    TermCreateRequest {
        program: Some(Program::Argv(vec!["/bin/sh".into()])),
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

#[tokio::test]
async fn terminal_attach_over_the_transport() {
    let f = start().await;
    within(wait_up(&f.client)).await;
    let created = within(f.client.term_create(&term_req(&f.root))).await.unwrap();
    let id = created.term.id.clone();
    let hello = TermHello {
        device: "transport-test".into(),
        kind: Some("test".into()),
        pid: None,
        size: Some(PtySize { rows: 24, cols: 80 }),
        active: true,
        read_only: false,
        snapshot: true,
    };
    let mut a = within(f.client.term_attach(&id, &hello)).await.unwrap();
    assert_eq!(a.term.id, id);
    a.tx.input(b"echo over-$((40+2))\n".to_vec()).await.unwrap();
    let mut out = String::new();
    within(async {
        while !out.contains("over-42") {
            match a.recv().await.unwrap() {
                Some(TermEvent::Output { data }) | Some(TermEvent::Snapshot { data, .. }) => {
                    out.push_str(&String::from_utf8_lossy(&data))
                }
                Some(TermEvent::Exit { .. }) | None => panic!("terminal ended: {out:?}"),
                Some(_) => {}
            }
        }
    })
    .await;
    let (tx, _rx) = a.into_split();
    tx.detach().await.unwrap();
    within(f.client.term_kill(&id, Some(9))).await.unwrap();
}

#[tokio::test]
async fn unknown_peers_are_refused_and_revocation_disconnects() {
    let f = start().await;
    within(wait_up(&f.client)).await;

    // A peer that is not on the node's allow-list cannot even read /v1/health.
    let stranger = f.net.transport();
    let s = NodeClient::over_transport(Dialer::new(stranger), f.node_t.peer_id(), TOKEN);
    assert!(within(s.health()).await.is_err());

    // A long-lived stream (events) is cut when the server is revoked, and new requests fail.
    let mut events = within(f.client.events(0)).await.unwrap();
    assert_eq!(f.gate.revoke(&f.server_t.peer_id()), 1);
    let ended = within(async {
        while let Ok(Some(_)) = events.recv().await {}
    });
    ended.await;
    assert!(within(f.client.health()).await.is_err());

    // Re-allowing restores access.
    f.gate.allow(f.server_t.peer_id());
    within(wait_up(&f.client)).await;
}

#[tokio::test]
async fn deadlines_bound_requests_over_the_transport() {
    let f = start().await;
    within(wait_up(&f.client)).await;

    // A deadline does not get in the way of a healthy node (requests and WebSockets).
    let bounded = f.client.clone().with_deadline(Duration::from_secs(5));
    assert_eq!(bounded.deadline(), Some(Duration::from_secs(5)));
    let path = f.root.join("d.txt");
    within(bounded.write_file(&path, b"in time".to_vec(), None)).await.unwrap();
    assert_eq!(within(bounded.read_file(&path)).await.unwrap().data, b"in time");
    let out = within(bounded.run(shell("echo ok", &f.root))).await.unwrap();
    assert_eq!(out.stdout, b"ok\n");

    // A peer that accepts streams for the node service but never answers.
    let stuck = f.net.transport();
    let mut listener = stuck.listen(ember_node::client::NODE_SERVICE).unwrap();
    let held = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let h = held.clone();
    tokio::spawn(async move {
        while let Some(conn) = listener.accept().await {
            let h = h.clone();
            tokio::spawn(async move {
                while let Ok(s) = conn.accept_bi().await {
                    h.lock().unwrap().push(s);
                }
            });
        }
    });
    let dialer = Dialer::new(f.server_t.clone());
    let unbounded = NodeClient::over_transport(dialer, stuck.peer_id(), TOKEN);
    assert!(tokio::time::timeout(Duration::from_millis(300), unbounded.health()).await.is_err(), "no deadline: waits");

    let bounded = unbounded.with_deadline(Duration::from_millis(200));
    let started = std::time::Instant::now();
    let e = within(bounded.stat(&f.root)).await.unwrap_err();
    assert!(matches!(e, ClientError::Timeout(d) if d == Duration::from_millis(200)), "{e:?}");
    assert!(e.is_timeout() && e.is_transport());
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    drop(held);
}
