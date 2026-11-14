//! SPEC FR-W4c (#896): the editor of a project on a registered computer.
//!
//! The OSE runs on the computer's node (FR-W4a, FR-W4b). The server asks the node for the launch
//! (`GET /api/sessions/{sid}/ide`), bridges the editor's relay socket to the node's relay
//! (`GET /api/computers/{id}/ide/relay/{launch_id}`) and forwards an extension install
//! (`POST /api/computers/{id}/ide/extensions`). The placeholder token is replaced on the node, so
//! the server never holds the real one.
//!
//! The node is a real in-process `ember-node` on a loopback port; its OSE is a loopback TCP
//! listener that records and echoes (registered with `OseManager::register_launch_for_test`), or a
//! shell script standing in for the binary. Not covered: a real Code-OSS runtime, a second
//! computer, and a node reached over the transport (the bridge uses the same `NodeClient` either
//! way).

mod common;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use ember_node::api::{self as node_api, Node};
use ember_node::config::NodeConfig;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::api::ide::{self, IdeConfig};
use ember_server::api::ide_extensions;
use ember_server::computers::{self, Computers, Registry};
use ember_server::devices::Devices;
use ember_server::ose::{OseManager, LOCAL_COMPUTER};
use ember_server::session::Sessions;
use ember_server::store::Store;
use ember_server::transport::{self as server_transport, SERVER_SERVICE};
use ember_transport::mem::MemNetwork;
use ember_transport::{BiStream, Dialer, PeerAddr, Transport};
use futures::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use tower::ServiceExt;

const NODE_TOKEN: &str = "node-token";
const REAL_TOKEN: &str = "1111111111111111222222222222222233333333333333334444444444444444";
/// One heartbeat, the bound FR-N3 gives.
const HEARTBEAT: Duration = Duration::from_secs(5);
const WAIT: Duration = Duration::from_secs(20);

type DeviceWs = WebSocketStream<BiStream>;

/// What the fake OSE saw: every byte it received, and how many connections are open now.
#[derive(Clone, Default)]
struct FakeOse {
    received: Arc<Mutex<Vec<u8>>>,
    live: Arc<AtomicUsize>,
}

/// A loopback TCP "OSE" that records what it receives and echoes it back.
async fn start_fake_ose() -> (SocketAddr, FakeOse) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let fake = FakeOse::default();
    let bg = fake.clone();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            let fake = bg.clone();
            fake.live.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 32 * 1024];
                loop {
                    match s.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            fake.received.lock().unwrap().extend_from_slice(&buf[..n]);
                            if s.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                    }
                }
                fake.live.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    (addr, fake)
}

/// A real node on a loopback port, with the OSE manager the test built.
struct NodeFx {
    url: String,
    /// Allowed root (canonical).
    root: PathBuf,
    /// A sibling directory outside the root.
    outside: PathBuf,
    ose: Arc<OseManager>,
    _dir: tempfile::TempDir,
}

async fn start_node(make: impl FnOnce(&std::path::Path) -> OseManager) -> NodeFx {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let root = base.join("root");
    let outside = base.join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let ose = Arc::new(make(&base));
    let node = Node::with_ose(NodeConfig::new(NODE_TOKEN, vec![root.clone()]), ose.clone()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(node_api::serve(listener, node));
    NodeFx { url, root, outside, ose, _dir: dir }
}

fn no_binary(base: &std::path::Path) -> OseManager {
    OseManager::with_data_root(None, base.join("ose-data"))
}

/// The server side: the IDE routes and the extension routes over one registry, with a server
/// OSE manager that has no binary (so nothing here can come from the server's own OSE).
struct ServerFx {
    store: Arc<Store>,
    computers: Arc<Computers>,
    app: Router,
    _dir: tempfile::TempDir,
}

fn server() -> ServerFx {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let computers = Computers::new(Registry::new(store.clone()), computers::default_connector());
    let server_ose = Arc::new(OseManager::with_data_root(None, dir.path().join("server-ose-data")));
    let app = ide::router_with_computers_and_ose(
        sessions,
        Arc::new(IdeConfig::default()),
        computers.clone(),
        server_ose.clone(),
    )
    .merge(ide_extensions::router(computers.clone(), server_ose));
    ServerFx { store, computers, app, _dir: dir }
}

async fn get_json(app: &Router, uri: &str) -> (StatusCode, String) {
    let req = Request::builder().method(Method::GET).uri(uri).header("host", "ember.test:8740").body(Body::empty()).unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn post_json(app: &Router, uri: &str, body: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("host", "ember.test:8740")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn ember_target(v: &Value) -> &Value {
    v["targets"].as_array().unwrap().iter().find(|t| t["kind"] == "ember").expect("an ember target")
}

/// The Ember target of `GET /api/sessions/{sid}/ide?computer=<id>` for a session in `cwd`.
async fn ember_target_for(s: &ServerFx, cwd: &std::path::Path, computer: &str) -> (Value, String) {
    let sid = s.store.create_session("proj", AgentKind::Scripted, &cwd.to_string_lossy(), None, "t").unwrap().id;
    let (status, text) = get_json(&s.app, &format!("/api/sessions/{sid}/ide?computer={computer}")).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    (ember_target(&v).clone(), text)
}

fn launch_id_of(relay: &str) -> String {
    relay.rsplit('/').next().unwrap().to_string()
}

#[tokio::test]
async fn fr_w4c_ide_launch_for_node_computer_calls_node_and_returns_server_relay() {
    let node = start_node(no_binary).await;
    let (ose_addr, _) = start_fake_ose().await;
    let conn = node.ose.register_launch_for_test(LOCAL_COMPUTER, &node.root, ose_addr, REAL_TOKEN);
    let launch_id = launch_id_of(conn.relay.as_deref().unwrap());
    let s = server();
    let computer = s.computers.registry().insert("box", &node.url, NODE_TOKEN).unwrap();

    // The node's launch comes back with the SERVER's relay path and the placeholder.
    let (ember, text) = ember_target_for(&s, &node.root, &computer.id).await;
    assert_eq!(ember["available"], true, "{ember}");
    assert!(ember.get("reason").is_none(), "{ember}");
    let launch = &ember["launch"];
    assert_eq!(launch["computer"], computer.id.as_str());
    assert_eq!(launch["folder"].as_str(), Some(&*node.root.to_string_lossy()));
    assert_eq!(launch["ose"]["relay"], format!("/api/computers/{}/ide/relay/{launch_id}", computer.id));
    assert_eq!(launch["ose"]["connection_token"], conn.connection_token.as_str());
    assert_eq!(launch["ose"]["endpoint"], format!("http://{ose_addr}"));
    assert!(!text.contains(REAL_TOKEN), "the real token is in the launch: {text}");

    // The computer's name resolves to the same launch.
    let (by_name, _) = ember_target_for(&s, &node.root, "box").await;
    assert_eq!(by_name["launch"]["ose"], ember["launch"]["ose"]);

    // A folder the node's path policy refuses: unavailable, with the node's reason.
    let (ember, _) = ember_target_for(&s, &node.outside, &computer.id).await;
    assert_eq!(ember["available"], false, "{ember}");
    assert!(ember["reason"].as_str().unwrap().contains("outside the allowed roots"), "{ember}");
    assert!(ember["launch"].get("ose").is_none(), "{ember}");

    // A folder with no running OSE on a node without the binary: unavailable, with the reason.
    let sub = node.root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let (ember, _) = ember_target_for(&s, &sub, &computer.id).await;
    assert_eq!(ember["available"], false, "{ember}");
    assert!(ember["reason"].as_str().unwrap().contains("not installed"), "{ember}");
    assert!(ember["launch"].get("ose").is_none(), "{ember}");

    // A node that does not answer: unavailable, and the reason says so.
    let gone = s.computers.registry().insert("gone", "http://127.0.0.1:1", "tok").unwrap();
    let (ember, _) = ember_target_for(&s, &node.root, &gone.id).await;
    assert_eq!(ember["available"], false, "{ember}");
    assert!(ember["reason"].as_str().unwrap().starts_with("node unreachable"), "{ember}");
    assert!(ember["launch"].get("ose").is_none(), "{ember}");
}

/// A WebSocket to `path` on its own stream of the device's connection (FR-N7: the device key is
/// the credential, no header).
async fn device_ws(device: &Dialer, server: &Transport, path: &str) -> Result<DeviceWs, String> {
    let stream = tokio::time::timeout(WAIT, device.open_bi(&PeerAddr::new(server.peer_id()), SERVER_SERVICE))
        .await
        .map_err(|_| "open_bi timed out".to_string())?
        .map_err(|e| e.to_string())?;
    let url = format!("ws://ember-server{path}");
    tokio::time::timeout(WAIT, tokio_tungstenite::client_async(url, stream))
        .await
        .map_err(|_| "websocket handshake timed out".to_string())?
        .map(|(ws, _)| ws)
        .map_err(|e| e.to_string())
}

/// Reads binary messages until `want` bytes arrived.
async fn read_bytes(ws: &mut DeviceWs, want: usize) -> Vec<u8> {
    let mut out = Vec::new();
    while out.len() < want {
        match tokio::time::timeout(WAIT, ws.next()).await.expect("no echo in time") {
            Some(Ok(Message::Binary(b))) => out.extend_from_slice(&b),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            other => panic!("the relay ended after {} of {want} bytes: {other:?}", out.len()),
        }
    }
    out
}

fn find(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

async fn wait_for(what: &str, within: Duration, done: impl Fn() -> bool) {
    let deadline = Instant::now() + within;
    while !done() {
        assert!(Instant::now() < deadline, "{what} after {within:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn fr_w4c_server_ide_relay_bridges_to_node_and_ends_on_device_revoke() {
    let node = start_node(no_binary).await;
    let (ose_addr, fake) = start_fake_ose().await;
    let conn = node.ose.register_launch_for_test(LOCAL_COMPUTER, &node.root, ose_addr, REAL_TOKEN);
    let launch_id = launch_id_of(conn.relay.as_deref().unwrap());
    let placeholder = conn.connection_token.clone();
    let s = server();
    let computer = s.computers.registry().insert("box", &node.url, NODE_TOKEN).unwrap();
    let relay = format!("/api/computers/{}/ide/relay/{launch_id}", computer.id);

    // The server's routes over the transport, behind the device gate.
    let net = MemNetwork::new();
    let devices = Devices::open(s.store.clone()).unwrap();
    let server_t: Transport = net.transport();
    tokio::spawn(server_transport::serve(&server_t, s.app.clone(), devices.gate().clone()).unwrap());
    let (phone_t, laptop_t) = (net.transport(), net.transport());
    devices.add(phone_t.peer_id(), "phone").unwrap();
    devices.add(laptop_t.peer_id(), "laptop").unwrap();
    let (phone, laptop) = (Dialer::new(phone_t.clone()), Dialer::new(laptop_t.clone()));

    // Refused before the upgrade: a launch the node does not know, and an unknown computer.
    let unknown = format!("/api/computers/{}/ide/relay/0123456789abcdef0123456789abcdef", computer.id);
    assert!(device_ws(&phone, &server_t, &unknown).await.is_err(), "an unknown launch was upgraded");
    assert!(device_ws(&phone, &server_t, &format!("/api/computers/nope/ide/relay/{launch_id}")).await.is_err());
    assert_eq!(fake.live.load(Ordering::SeqCst), 0, "a refused relay dialed the OSE");

    // Bytes go both ways, and the NODE put the real token in place of the placeholder.
    let mut phone_ws = device_ws(&phone, &server_t, &relay).await.expect("an admitted device opens the relay");
    let sent = format!("GET /?tkn={placeholder} HTTP/1.1\r\n\r\n").into_bytes();
    phone_ws.send(Message::Binary(sent.clone().into())).await.unwrap();
    let echo = read_bytes(&mut phone_ws, sent.len()).await;
    assert_eq!(echo, format!("GET /?tkn={REAL_TOKEN} HTTP/1.1\r\n\r\n").into_bytes());
    {
        let seen = fake.received.lock().unwrap();
        assert!(find(&seen, REAL_TOKEN.as_bytes()), "the OSE did not get the real token");
        assert!(!find(&seen, placeholder.as_bytes()), "the placeholder reached the OSE");
    }

    // A second device has its own relay.
    let mut laptop_ws = device_ws(&laptop, &server_t, &relay).await.unwrap();
    laptop_ws.send(Message::Binary(b"laptop".to_vec().into())).await.unwrap();
    assert_eq!(read_bytes(&mut laptop_ws, 6).await, b"laptop");
    wait_for("two OSE connections", WAIT, || fake.live.load(Ordering::SeqCst) == 2).await;

    // A text frame closes that relay with 1003, and its OSE connection goes away.
    let mut text_ws = device_ws(&laptop, &server_t, &relay).await.unwrap();
    wait_for("the third OSE connection", WAIT, || fake.live.load(Ordering::SeqCst) == 3).await;
    text_ws.send(Message::text("not binary")).await.unwrap();
    let code = loop {
        match tokio::time::timeout(WAIT, text_ws.next()).await.expect("no close in time") {
            Some(Ok(Message::Close(Some(frame)))) => break u16::from(frame.code),
            Some(Ok(Message::Close(None))) | Some(Err(_)) | None => panic!("closed without the 1003 frame"),
            Some(Ok(_)) => continue,
        }
    };
    assert_eq!(code, 1003);
    wait_for("the text relay's OSE connection", WAIT, || fake.live.load(Ordering::SeqCst) == 2).await;

    // Revoke the phone: its relay ends within one heartbeat, and so does the node side (the TCP
    // connection to the OSE is dropped), while the laptop's relay keeps working.
    let started = Instant::now();
    devices.remove(&phone_t.peer_id()).unwrap();
    let ended = tokio::time::timeout(HEARTBEAT, async {
        loop {
            match phone_ws.next().await {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => continue,
            }
        }
    })
    .await;
    assert!(ended.is_ok(), "the revoked device's relay stayed open after one heartbeat");
    wait_for("the revoked relay's OSE connection", HEARTBEAT.saturating_sub(started.elapsed()), || {
        fake.live.load(Ordering::SeqCst) == 1
    })
    .await;
    assert!(started.elapsed() < HEARTBEAT, "ended after {:?}", started.elapsed());
    assert!(device_ws(&phone, &server_t, &relay).await.is_err(), "a revoked device opened a new relay");

    laptop_ws.send(Message::Binary(b"still-here".to_vec().into())).await.unwrap();
    assert_eq!(read_bytes(&mut laptop_ws, 10).await, b"still-here");

    // The client closing ends the node side too.
    laptop_ws.close(None).await.unwrap();
    wait_for("the closed relay's OSE connection", WAIT, || fake.live.load(Ordering::SeqCst) == 0).await;
}

#[tokio::test]
async fn fr_w4c_server_ide_extensions_forwards_to_node() {
    // `$1` is `--install-extension`, `$2` the extension spec.
    let node = start_node(|base| {
        let script = base.join("fake-ose");
        common::write_exec_ready(
            &script,
            "#!/bin/sh\ncase \"$2\" in\n  fail.ext) echo \"no such extension\" >&2; exit 3 ;;\n  *) echo \"installed $2 on the node\" ;;\nesac\n",
        );
        OseManager::with_data_root(Some(script), base.join("ose-data"))
    })
    .await;
    let s = server();
    let computer = s.computers.registry().insert("box", &node.url, NODE_TOKEN).unwrap();
    let uri = format!("/api/computers/{}/ide/extensions", computer.id);

    // The node ran the install; its answer comes back unchanged.
    let (status, v) = post_json(&s.app, &uri, r#"{"id":"good.ext","version":"1.2.3"}"#).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["installed"], true);
    assert_eq!(v["id"], "good.ext");
    assert!(v["output"].as_str().unwrap().contains("installed good.ext@1.2.3 on the node"), "{v}");

    // The node's refusals keep their status and body.
    let (status, v) = post_json(&s.app, &uri, r#"{"id":"../x"}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert!(v["error"].is_string(), "{v}");
    let (status, v) = post_json(&s.app, &uri, r#"{"id":"fail.ext"}"#).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{v}");
    assert_eq!(v["installed"], false);
    assert_eq!(v["id"], "fail.ext");
    assert!(v["output"].as_str().unwrap().contains("no such extension"), "{v}");

    // A body that is not JSON never reaches the node.
    let (status, _) = post_json(&s.app, &uri, "not json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A node without the binary: its 503 and reason.
    let bare = start_node(no_binary).await;
    let bare_computer = s.computers.registry().insert("bare", &bare.url, NODE_TOKEN).unwrap();
    let (status, v) =
        post_json(&s.app, &format!("/api/computers/{}/ide/extensions", bare_computer.id), r#"{"id":"a.b"}"#).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert!(v["error"].as_str().unwrap().contains("not installed"), "{v}");

    // A node that does not answer is 502; an unknown computer is 404.
    let gone = s.computers.registry().insert("gone", "http://127.0.0.1:1", "tok").unwrap();
    let (status, v) =
        post_json(&s.app, &format!("/api/computers/{}/ide/extensions", gone.id), r#"{"id":"a.b"}"#).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{v}");
    assert!(v["error"].as_str().unwrap().starts_with("node unreachable"), "{v}");
    let (status, _) = post_json(&s.app, "/api/computers/nope/ide/extensions", r#"{"id":"a.b"}"#).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
