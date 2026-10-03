//! ember server on the in-memory fake transport (SPEC FR-N1, FR-N3, FR-N5): a real ember node
//! registered by peer and reached through the server's dialer (health, env, exec, terminal
//! attach, the shim's loopback bridge), and the server's own API served to allowed devices only.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use ember_node::api::Node;
use ember_node::client::{shell, NodeClient};
use ember_node::config::NodeConfig;
use ember_node::proto::*;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind, StartRequest};
use ember_server::computers::{self, Computers, Registry};
use ember_server::devices::Devices;
use ember_server::events::SessionStatus;
use ember_server::session::Sessions;
use ember_server::store::{SessionRecord, Store};
use ember_server::transport::{self as server_transport, ServerTransport, SERVER_SERVICE};
use ember_transport::http::http1_handshake;
use ember_transport::mem::MemNetwork;
use ember_transport::{Dialer, PeerAddr, PeerGate, Transport};
use http_body_util::{BodyExt, Full};
use serde_json::{json, Value};
use tower::ServiceExt;

const TOKEN: &str = "node-token";

async fn within<T>(fut: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(20), fut).await.expect("timed out")
}

async fn call(app: &axum::Router, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let req = Request::builder().method(method).uri(uri);
    let req = match body {
        Some(b) => req.header("content-type", "application/json").body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
    (status, value)
}

struct NodeFixture {
    node_t: Transport,
    gate: PeerGate,
    root: PathBuf,
    _dir: tempfile::TempDir,
}

/// A real ember node on `net` that admits only `server`.
fn start_node(net: &MemNetwork, server: &Transport) -> NodeFixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let node = Node::new(NodeConfig::new(TOKEN, vec![root.clone()])).unwrap();
    let node_t = net.transport();
    let gate = PeerGate::allow_list([server.peer_id()]);
    tokio::spawn(ember_node::transport::serve(&node_t, node, gate.clone()).unwrap());
    NodeFixture { node_t, gate, root, _dir: dir }
}

struct ServerFixture {
    sessions: Arc<Sessions>,
    computers: Arc<Computers>,
    st: ServerTransport,
}

fn start_server(server_t: Transport) -> ServerFixture {
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), adapters);
    let st = ServerTransport::new(server_t);
    let computers = Computers::with_transport(
        Registry::new(store),
        computers::connector(Some(st.dialer.clone())),
        Some("/opt/ember/bin/ember-exec".into()),
        Some(st.dialer.clone()),
    );
    computers.install(&sessions);
    ServerFixture { sessions, computers, st }
}

fn record(id: &str, agent: AgentKind) -> SessionRecord {
    SessionRecord {
        id: id.into(),
        project: "acme".into(),
        agent,
        cwd: "/tmp".into(),
        model: None,
        native_id: None,
        status: SessionStatus::Idle,
        title: "t".into(),
        created_at: 0,
        updated_at: 0,
        last_seq: 0,
        account_id: None,
        account_reason: None,
        pinned: false,
        archived: false,
    }
}

fn term_req(root: &Path) -> TermCreateRequest {
    let mut env = BTreeMap::new();
    env.insert("PS1".to_string(), "$ ".to_string());
    TermCreateRequest {
        program: Some(Program::Argv(vec!["/bin/sh".into()])),
        cwd: root.to_path_buf(),
        env,
        env_clear: false,
        size: Some(PtySize { rows: 24, cols: 80 }),
        origin: TermOrigin::IdeVscode,
        project: None,
        title: None,
        key: None,
        tags: Default::default(),
    }
}

#[tokio::test]
async fn node_registered_by_peer_is_reached_over_the_transport() {
    let net = MemNetwork::new();
    let server_t = net.transport();
    let n = start_node(&net, &server_t);
    let s = start_server(server_t.clone());
    let app = computers::api::router(s.computers.clone(), s.sessions.clone());

    // Register by full PeerAddr JSON (as the node prints it) …
    let addr = serde_json::to_value(n.node_t.local_addr()).unwrap();
    let (st, gpu) = call(&app, Method::POST, "/api/v1/computers", Some(json!({ "name": "gpu", "peer": addr, "token": TOKEN }))).await;
    assert_eq!(st, StatusCode::CREATED, "{gpu}");
    assert!(gpu.get("token").is_none());
    assert_eq!(gpu["url"], "");
    let gpu_id = gpu["id"].as_str().unwrap().to_string();
    // … or by bare peer id; url and peer together are refused.
    let (st, _) = call(&app, Method::POST, "/api/v1/computers", Some(json!({ "name": "gpu2", "peer": n.node_t.peer_id().to_string(), "token": TOKEN }))).await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, _) = call(&app, Method::POST, "/api/v1/computers", Some(json!({ "name": "x", "url": "http://x:1", "peer": n.node_t.peer_id().to_string(), "token": TOKEN }))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // Health + env through the registry's probe.
    let (st, status) = within(call(&app, Method::GET, &format!("/api/v1/computers/{gpu_id}"), None)).await;
    assert_eq!(st, StatusCode::OK, "{status}");
    assert_eq!(status["reachable"], true, "{status}");
    assert_eq!(status["peer"], n.node_t.peer_id().to_string());
    assert_eq!(status["env"]["roots"][0], n.root.display().to_string());

    // Exec and a terminal attach through the same client the server uses.
    let client = s.computers.client(&gpu_id).unwrap();
    let out = within(client.run(shell("echo from-$((6*7))", &n.root))).await.unwrap();
    assert_eq!(out.stdout, b"from-42\n");
    assert_eq!(out.code, Some(0));

    let term = within(client.term_create(&term_req(&n.root))).await.unwrap().term;
    let hello = TermHello {
        device: "server-test".into(),
        kind: None,
        pid: None,
        size: None,
        active: true,
        read_only: false,
        snapshot: true,
    };
    let mut a = within(client.term_attach(&term.id, &hello)).await.unwrap();
    a.tx.input(b"echo attached-$((1+1))\n".to_vec()).await.unwrap();
    let mut seen = String::new();
    within(async {
        while !seen.contains("attached-2") {
            match a.recv().await.unwrap() {
                Some(TermEvent::Output { data }) | Some(TermEvent::Snapshot { data, .. }) => {
                    seen.push_str(&String::from_utf8_lossy(&data))
                }
                Some(TermEvent::Exit { .. }) | None => panic!("terminal ended: {seen:?}"),
                Some(_) => {}
            }
        }
    })
    .await;
    drop(a);
    within(client.term_kill(&term.id, Some(9))).await.unwrap();

    // Claude Code on a peer node: the shim gets a loopback bridge URL, which carries plain
    // HTTP/WebSocket to the node over the transport.
    s.computers
        .registry()
        .set_session_computer("s-claude", &gpu_id, Some(&client.env().await.unwrap()), None)
        .unwrap();
    let mut req = StartRequest::default();
    s.computers.configure_start(&record("s-claude", AgentKind::ClaudeCode), &mut req).unwrap();
    let env: BTreeMap<_, _> = req.env.iter().cloned().collect();
    let bridge = env[computers::shim::ENV_NODE_URL].clone();
    assert!(bridge.starts_with("http://127.0.0.1:"), "{bridge}");
    let via_bridge = NodeClient::new(&bridge, TOKEN).unwrap();
    let out = within(via_bridge.run(shell("echo bridged", &n.root))).await.unwrap();
    assert_eq!(out.stdout, b"bridged\n");

    // Codex: the exec-server relay is created for the peer node too.
    let mut req = StartRequest::default();
    s.computers.registry().set_session_computer("s-codex", &gpu_id, None, None).unwrap();
    s.computers.configure_start(&record("s-codex", AgentKind::Codex), &mut req).unwrap();
    assert!(req.remote.unwrap().exec_server_url.starts_with("ws://127.0.0.1:"));
}

#[tokio::test]
async fn node_refuses_a_revoked_server() {
    let net = MemNetwork::new();
    let server_t = net.transport();
    let n = start_node(&net, &server_t);
    let s = start_server(server_t.clone());
    let gpu = s.computers.register_peer("gpu", &PeerAddr::new(n.node_t.peer_id()), TOKEN).unwrap();
    assert_eq!(s.st.dialer.transport().peer_id(), server_t.peer_id());
    let client = s.computers.client(&gpu.id).unwrap();
    assert!(within(client.health()).await.unwrap().ok);

    // The node revokes the server: its connection is closed and new ones are refused.
    n.gate.revoke(&server_t.peer_id());
    assert!(within(client.health()).await.is_err());
    let status = within(s.computers.status(&gpu.id)).await.unwrap();
    assert_eq!(status.reachable, Some(false));

    // Another server (unknown peer) is refused too.
    let other = start_server(net.transport());
    let gpu = other.computers.register_peer("gpu", &PeerAddr::new(n.node_t.peer_id()), TOKEN).unwrap();
    assert!(within(other.computers.client(&gpu.id).unwrap().health()).await.is_err());
}

#[tokio::test]
async fn registering_by_peer_needs_a_transport() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let c = Computers::with_shim(Registry::new(store), computers::default_connector(), None);
    let peer = MemNetwork::new().transport().peer_id();
    assert!(c.register_peer("gpu", &PeerAddr::new(peer), TOKEN).is_err());
}

/// GET `/api/v1/health` over the server's transport service, as a device.
async fn device_health(device: &Dialer, server: &Transport) -> Result<Value, String> {
    let fut = async {
        let stream = device
            .open_bi(&PeerAddr::new(server.peer_id()), SERVER_SERVICE)
            .await
            .map_err(|e| e.to_string())?;
        let mut http = http1_handshake::<Full<hyper::body::Bytes>>(stream).await.map_err(|e| e.to_string())?;
        let req = hyper::Request::get("http://ember-server/api/v1/health").body(Full::default()).unwrap();
        let resp = http.send_request(req).await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("status {}", resp.status()));
        }
        let body = resp.into_body().collect().await.map_err(|e| e.to_string())?.to_bytes();
        serde_json::from_slice(&body).map_err(|e| e.to_string())
    };
    tokio::time::timeout(Duration::from_secs(5), fut).await.map_err(|_| "timed out".to_string())?
}

#[tokio::test]
async fn server_api_over_the_transport_admits_only_devices() {
    let net = MemNetwork::new();
    let (server_t, phone_t, stranger_t) = (net.transport(), net.transport(), net.transport());
    let store = Arc::new(Store::open_in_memory().unwrap());
    let devices = Devices::open(store.clone()).unwrap();
    let sessions = Sessions::new(store, vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let app = ember_server::api::router(sessions);
    tokio::spawn(server_transport::serve(&server_t, app, devices.gate().clone()).unwrap());

    let phone = Dialer::new(phone_t.clone());
    // Not yet a device: refused at accept.
    assert!(device_health(&phone, &server_t).await.is_err());

    // Added through the local devices API.
    let admin = ember_server::devices::api::router(devices.clone());
    let (st, d) = call(&admin, Method::POST, "/api/v1/devices", Some(json!({ "peer_id": phone_t.peer_id().to_string(), "name": "phone" }))).await;
    assert_eq!(st, StatusCode::CREATED, "{d}");
    let h = device_health(&phone, &server_t).await.unwrap();
    assert_eq!(h["ok"], true);

    // A stranger is still refused.
    assert!(device_health(&Dialer::new(stranger_t), &server_t).await.is_err());

    // Revoke: the phone's open connection is closed at once, and it cannot reconnect.
    let conn = phone.connection(&PeerAddr::new(server_t.peer_id()), SERVER_SERVICE).await.unwrap();
    let (st, out) = call(&admin, Method::DELETE, &format!("/api/v1/devices/{}", phone_t.peer_id()), None).await;
    assert_eq!(st, StatusCode::OK, "{out}");
    assert_eq!(out["closed"], 1);
    within(conn.closed()).await;
    assert!(device_health(&phone, &server_t).await.is_err());
    let (st, _) = call(&admin, Method::DELETE, &format!("/api/v1/devices/{}", phone_t.peer_id()), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}
