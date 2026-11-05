//! SPEC FR-N3 and FR-N7: revoking a device ends everything it has open over the transport, not
//! only its HTTP requests. `fr_n3_revocation_bound.rs` measures the connection close and
//! `fr_n7_remote_device.rs` the route calls before and after; this file covers the long-lived
//! streams and the state a revoked device leaves behind (INTENT D37: an admitted device has the
//! server user's authority in v1, so revocation is the main control).
//!
//! In scope here, on the in-memory transport:
//! - an open `/api/push` WebSocket ends within one heartbeat, and a new one is refused;
//! - an open terminal attach WebSocket ends within one heartbeat, and the terminal itself keeps
//!   running on the node (the node, not the device, owns it);
//! - an in-flight HTTP request on a second stream of the connection fails within one heartbeat;
//! - a session the device created is not touched by the revocation;
//! - a second admitted device keeps its push stream.
//!
//! Not testable here: the handler of an in-flight request after its stream is gone (whether
//! hyper drops it is hyper's behaviour; a request that already reached a handler may finish its
//! side effects), and the wire of the real transport backends (`CLOSE_REVOKED` over real sockets is
//! covered by the transport crate's gate tests).

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::routing::get;
use ember_node::api::{self, Node};
use ember_node::client::NodeClient;
use ember_node::config::NodeConfig;
use ember_node::proto::*;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::AgentAdapter;
use ember_server::computers::{self, Computers, Registry};
use ember_server::devices::Devices;
use ember_server::session::{Push, Sessions};
use ember_server::store::Store;
use ember_server::transport::{self as server_transport, SERVER_SERVICE};
use ember_transport::http::http1_handshake;
use ember_transport::mem::MemNetwork;
use ember_transport::{BiStream, Dialer, PeerAddr, Transport};
use futures::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use tower::ServiceExt;

/// One heartbeat, the bound FR-N3 gives.
const HEARTBEAT: Duration = Duration::from_secs(5);
/// A wait that only guards against a hang.
const WAIT: Duration = Duration::from_secs(30);
const NODE_TOKEN: &str = "node-token";

type DeviceWs = WebSocketStream<BiStream>;

struct Fx {
    net: MemNetwork,
    server_t: Transport,
    devices: Arc<Devices>,
    sessions: Arc<Sessions>,
    /// The same router the transport serves, for local calls (the local door).
    app: axum::Router,
}

/// Routes added on top of the real ones: `/slow` holds a request open, as a long call would.
fn slow_route() -> axum::Router {
    axum::Router::new().route(
        "/slow",
        get(|| async {
            tokio::time::sleep(Duration::from_secs(20)).await;
            "done"
        }),
    )
}

fn fixture(computers: Option<Arc<Computers>>) -> Fx {
    let net = MemNetwork::new();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let mut app = ember_server::api::router(sessions.clone()).merge(slow_route());
    if let Some(c) = computers {
        app = app.merge(computers::terms::router(c));
    }
    let devices = Devices::open(store).unwrap();
    let server_t = net.transport();
    tokio::spawn(server_transport::serve(&server_t, app.clone(), devices.gate().clone()).unwrap());
    Fx { net, server_t, devices, sessions, app }
}

async fn open_stream(device: &Dialer, server: &Transport) -> Result<BiStream, String> {
    tokio::time::timeout(WAIT, device.open_bi(&PeerAddr::new(server.peer_id()), SERVER_SERVICE))
        .await
        .map_err(|_| "open_bi timed out".to_string())?
        .map_err(|e| e.to_string())
}

/// A WebSocket to `path` on its own stream of the device's connection. No header: the device key
/// is the credential (FR-N7).
async fn device_ws(device: &Dialer, server: &Transport, path: &str) -> Result<DeviceWs, String> {
    let stream = open_stream(device, server).await?;
    let url = format!("ws://ember-server{path}");
    tokio::time::timeout(Duration::from_secs(5), tokio_tungstenite::client_async(url, stream))
        .await
        .map_err(|_| "websocket handshake timed out".to_string())?
        .map(|(ws, _)| ws)
        .map_err(|e| e.to_string())
}

/// The next text frame, or `None` when the stream ended (close frame, error, EOF).
async fn next_text(ws: &mut DeviceWs, within: Duration) -> Result<Option<String>, &'static str> {
    let fut = async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => return Some(t.as_str().to_string()),
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return None,
                Some(Ok(_)) => continue,
            }
        }
    };
    tokio::time::timeout(within, fut).await.map_err(|_| "still open")
}

/// Frames that were broadcast to both streams while the other one was being checked are still
/// buffered here; read them out so that what arrives after the revoke is only what was sent after.
async fn drain(ws: &mut DeviceWs) {
    while let Ok(Some(_)) = next_text(ws, Duration::from_millis(150)).await {}
}

/// After a revoke the device's WebSocket must end, whatever the server sends or does not.
async fn assert_ends_within_heartbeat(ws: &mut DeviceWs, started: Instant) {
    match next_text(ws, HEARTBEAT).await {
        Ok(None) => assert!(started.elapsed() < HEARTBEAT, "ended after {:?}", started.elapsed()),
        Ok(Some(t)) => panic!("a revoked device still received a frame: {t}"),
        Err(e) => panic!("the revoked device's websocket is {e} after {HEARTBEAT:?}"),
    }
}

async fn wait_live(count: impl Fn() -> usize, want: usize) {
    let deadline = Instant::now() + WAIT;
    while count() != want && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(count(), want, "live connections of the device");
}

fn sample_push() -> Push {
    Push::ScheduleDeleted { id: "s1".into(), project: "acme".into() }
}

/// A push event reaches an open stream, which proves the stream is live before the revoke.
async fn assert_push_flows(ws: &mut DeviceWs, f: &Fx) {
    f.sessions.publish(sample_push());
    let text = next_text(ws, Duration::from_secs(5)).await.expect("no push frame").expect("stream ended");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["type"], "schedule_deleted", "{v}");
}

#[tokio::test]
async fn fr_n3_revoked_device_push_stream_closes_within_one_heartbeat() {
    let f = fixture(None);
    let phone_t = f.net.transport();
    f.devices.add(phone_t.peer_id(), "phone").unwrap();
    let phone = Dialer::new(phone_t.clone());

    let mut push = device_ws(&phone, &f.server_t, "/api/push").await.expect("admitted device opens the push socket");
    assert_push_flows(&mut push, &f).await;
    wait_live(|| f.devices.gate().live_connections(&phone_t.peer_id()), 1).await;

    let started = Instant::now();
    assert_eq!(f.devices.remove(&phone_t.peer_id()).unwrap(), 1);
    assert_ends_within_heartbeat(&mut push, started).await;

    // Nothing published afterwards reaches the device, and a new push socket is refused.
    f.sessions.publish(sample_push());
    assert!(matches!(next_text(&mut push, Duration::from_millis(300)).await, Ok(None)));
    assert!(
        device_ws(&phone, &f.server_t, "/api/push").await.is_err(),
        "a revoked device opened a new push socket"
    );
}

/// Revoking one device leaves another device's push stream alone.
#[tokio::test]
async fn fr_n3_revoking_one_device_keeps_another_devices_push_stream() {
    let f = fixture(None);
    let (phone_t, laptop_t) = (f.net.transport(), f.net.transport());
    f.devices.add(phone_t.peer_id(), "phone").unwrap();
    f.devices.add(laptop_t.peer_id(), "laptop").unwrap();
    let (phone, laptop) = (Dialer::new(phone_t.clone()), Dialer::new(laptop_t.clone()));

    let mut phone_push = device_ws(&phone, &f.server_t, "/api/push").await.unwrap();
    let mut laptop_push = device_ws(&laptop, &f.server_t, "/api/push").await.unwrap();
    assert_push_flows(&mut laptop_push, &f).await;
    assert_push_flows(&mut phone_push, &f).await;
    drain(&mut phone_push).await;
    drain(&mut laptop_push).await;

    let started = Instant::now();
    f.devices.remove(&phone_t.peer_id()).unwrap();
    assert_ends_within_heartbeat(&mut phone_push, started).await;
    assert_push_flows(&mut laptop_push, &f).await;
}

/// A request that is still being served on a second stream fails for the device when it is
/// revoked; the connection close takes every stream with it, not only the idle ones.
#[tokio::test]
async fn fr_n3_in_flight_http_request_fails_within_one_heartbeat() {
    let f = fixture(None);
    let phone_t = f.net.transport();
    f.devices.add(phone_t.peer_id(), "phone").unwrap();
    let phone = Dialer::new(phone_t.clone());

    let stream = open_stream(&phone, &f.server_t).await.unwrap();
    let mut http = http1_handshake::<Full<hyper::body::Bytes>>(stream).await.unwrap();
    let req = hyper::Request::get("http://ember-server/slow").body(Full::default()).unwrap();
    let pending = tokio::spawn(async move {
        let resp = http.send_request(req).await.map_err(|e| e.to_string())?;
        resp.into_body().collect().await.map_err(|e| e.to_string()).map(|b| b.to_bytes())
    });
    // Let the request reach the handler.
    tokio::time::sleep(Duration::from_millis(300)).await;
    wait_live(|| f.devices.gate().live_connections(&phone_t.peer_id()), 1).await;

    let started = Instant::now();
    f.devices.remove(&phone_t.peer_id()).unwrap();
    let out = tokio::time::timeout(HEARTBEAT, pending)
        .await
        .expect("the in-flight request of a revoked device stayed open")
        .unwrap();
    assert!(out.is_err(), "a revoked device got an answer: {out:?}");
    assert!(started.elapsed() < HEARTBEAT, "failed after {:?}", started.elapsed());
}

/// FR-N7 / D37: a session the device created belongs to the server user, not to the device. It
/// keeps running after the device is revoked and stays reachable through the local door. This is
/// the decision, not an accident: admission was full trust, and cleanup of what an admitted
/// device started is a separate, deliberate action.
#[tokio::test]
async fn fr_n7_session_created_by_a_revoked_device_survives_the_revocation() {
    let f = fixture(None);
    let phone_t = f.net.transport();
    f.devices.add(phone_t.peer_id(), "phone").unwrap();
    let phone = Dialer::new(phone_t.clone());

    let stream = open_stream(&phone, &f.server_t).await.unwrap();
    let mut http = http1_handshake::<Full<hyper::body::Bytes>>(stream).await.unwrap();
    let body = json!({ "project": "acme", "agent": "scripted", "cwd": "/tmp" }).to_string();
    let req = hyper::Request::post("http://ember-server/api/sessions")
        .header("content-type", "application/json")
        .body(Full::new(hyper::body::Bytes::from(body)))
        .unwrap();
    let resp = http.send_request(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let rec: Value = serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let id = rec["id"].as_str().unwrap().to_string();

    f.devices.remove(&phone_t.peer_id()).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert!(f.sessions.store().session(&id).unwrap().is_some(), "the session vanished with the device");
    let res = f
        .app
        .clone()
        .oneshot(Request::builder().method(Method::GET).uri(format!("/api/sessions/{id}")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

// ---- terminal attach ----

struct NodeFx {
    url: String,
    direct: NodeClient,
    root: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

async fn start_node() -> NodeFx {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let node = Node::new(NodeConfig::new(NODE_TOKEN, vec![root.clone()])).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node));
    NodeFx { direct: NodeClient::new(&url, NODE_TOKEN).unwrap(), url, root, _dir: dir }
}

fn hello(device: &str) -> String {
    serde_json::to_string(&TermHello {
        device: device.into(),
        kind: Some("test".into()),
        pid: None,
        size: None,
        active: true,
        read_only: false,
        snapshot: true,
    })
    .unwrap()
}

/// The device attaches to a terminal of a node the server relays to, then is revoked: the attach
/// socket ends within one heartbeat, the node detaches the client, and the terminal keeps running.
#[tokio::test]
async fn fr_n3_revoked_device_terminal_attach_closes_and_the_terminal_keeps_running() {
    let node = start_node().await;
    let store = Arc::new(Store::open_in_memory().unwrap());
    let computers = Computers::new(Registry::new(store), computers::default_connector());
    let computer = computers.registry().insert("box", &node.url, NODE_TOKEN).unwrap();
    let f = fixture(Some(computers));

    let phone_t = f.net.transport();
    f.devices.add(phone_t.peer_id(), "phone").unwrap();
    let phone = Dialer::new(phone_t.clone());

    // The terminal is made through the local door; the device only attaches.
    let create = json!({
        "program": { "argv": ["/bin/sh"] },
        "cwd": node.root,
        "env": { "PS1": "$ " },
        "size": { "rows": 24, "cols": 80 },
        "origin": "ide-ember",
        "project": node.root.display().to_string(),
        "key": "revoke-1",
    });
    let res = f
        .app
        .clone()
        .oneshot(
            Request::post(format!("/api/computers/{}/terms", computer.id))
                .header("content-type", "application/json")
                .body(Body::from(create.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let created: TermCreateResponse =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let tid = created.term.id.clone();

    let mut ws = device_ws(&phone, &f.server_t, &format!("/api/computers/{}/terms/{tid}/attach", computer.id))
        .await
        .expect("admitted device attaches");
    ws.send(Message::text(hello("phone"))).await.unwrap();
    let first = next_text(&mut ws, Duration::from_secs(10)).await.expect("no frame").expect("ended");
    assert!(first.contains("attached"), "{first}");
    assert_eq!(node.direct.term(&tid).await.unwrap().clients.len(), 1);
    wait_live(|| f.devices.gate().live_connections(&phone_t.peer_id()), 1).await;

    let started = Instant::now();
    f.devices.remove(&phone_t.peer_id()).unwrap();
    assert_ends_within_heartbeat(&mut ws, started).await;

    // The relay notices the lost stream and detaches the client on the node.
    let deadline = Instant::now() + HEARTBEAT;
    loop {
        if node.direct.term(&tid).await.unwrap().clients.is_empty() {
            break;
        }
        assert!(Instant::now() < deadline, "the node still lists the revoked device's client");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // The terminal is the user's, not the device's: it is still running.
    assert_eq!(node.direct.term(&tid).await.unwrap().state, TermState::Running);
    // And the device cannot attach again.
    assert!(device_ws(&phone, &f.server_t, &format!("/api/computers/{}/terms/{tid}/attach", computer.id))
        .await
        .is_err());
}
