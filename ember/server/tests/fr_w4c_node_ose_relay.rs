//! SPEC FR-W4c (#896, D55, #1064): the editor of a project on a registered computer.
//!
//! The editor server is the computer's DarkPyonix manager's; the node asks it for a launch ticket
//! (FR-W4b). The server asks the node for the launch (`GET /api/sessions/{sid}/ide`, and
//! `POST /api/sessions/{sid}/ide/vscode` on pick), bridges the editor's relay socket to the node's
//! relay (`GET /api/computers/{id}/ide/relay/{launch_id}`), which joins it to the manager's
//! endpoint with nothing replaced, and forwards an extension install
//! (`POST /api/computers/{id}/ide/extensions`).
//!
//! The node is a real in-process `ember-node` on a loopback port. Its manager is a fake: an axum
//! server with the launch and extension routes and a page under `/ide/ose/` that needs the ticket
//! cookie, or, for relay tests, a loopback TCP listener that records and echoes (registered with
//! `EditorRelays::register_launch_for_test`). Not covered: a real manager and editor server, a
//! second computer, a node reached over the transport (the bridge uses the same `NodeClient`
//! either way), and a node that hangs while a bridged relay closes.

#![cfg(unix)]

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
use ember_node::editor_manager::{EditorRelays, ManagerEndpoint, ManagerSource};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::api::ide::{self, IdeConfig};
use ember_server::api::ide_extensions;
use ember_server::computers::{self, Computers, Registry};
use ember_server::channels::guest::guest_router;
use ember_server::devices::{Devices, Scope};
use ember_server::guest_door::GuestDoor;
use ember_server::ose::OseManager;
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
const MANAGER_TOKEN: &str = "manager-master-token";
const TICKET: &str = "ticket-0123456789abcdef";
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
/// One heartbeat, the bound FR-N3 gives.
const HEARTBEAT: Duration = Duration::from_secs(5);
const WAIT: Duration = Duration::from_secs(20);

type DeviceWs = WebSocketStream<BiStream>;
type TcpWs = WebSocketStream<tokio::net::TcpStream>;

/// What the fake manager endpoint saw: every byte it received, and how many connections are open now.
#[derive(Clone, Default)]
struct FakeOse {
    received: Arc<Mutex<Vec<u8>>>,
    live: Arc<AtomicUsize>,
}

/// A loopback TCP "manager endpoint" that records what it receives and echoes it back.
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

/// What the fake manager answers to a launch (200 with a ticket, 409 not installed) and to an
/// install, and what it got.
#[derive(Clone)]
struct FakeManager {
    launch_status: Arc<Mutex<u16>>,
    installs: Arc<Mutex<Vec<Value>>>,
}

/// A fake DarkPyonix manager on a loopback port: `POST /api/editor/{runtime}/launch`,
/// `POST /api/editor/{runtime}/extensions` (the master token only), and `GET /ide/ose/`, which
/// answers `editor-ok` only with the ticket cookie.
async fn start_fake_manager() -> (String, FakeManager) {
    use axum::extract::{Path as AxPath, State};
    use axum::http::HeaderMap;
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    fn bearer_ok(h: &HeaderMap) -> bool {
        h.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {MANAGER_TOKEN}"))
    }
    let m = FakeManager { launch_status: Arc::new(Mutex::new(200)), installs: Arc::new(Mutex::new(Vec::new())) };
    let app = Router::new()
        .route(
            "/api/editor/{runtime}/launch",
            post(|State(m): State<FakeManager>, AxPath(runtime): AxPath<String>, h: HeaderMap| async move {
                if !bearer_ok(&h) {
                    return (StatusCode::FORBIDDEN, axum::Json(json!({ "code": "forbidden", "error": "not the master token" }))).into_response();
                }
                if *m.launch_status.lock().unwrap() == 409 {
                    return (StatusCode::CONFLICT, axum::Json(json!({ "code": "not_installed", "error": "the ose editor server is not installed" }))).into_response();
                }
                axum::Json(json!({ "ticket": TICKET, "path": format!("/ide/{runtime}/"), "commit": COMMIT, "version": "1.139.1" })).into_response()
            }),
        )
        .route(
            "/api/editor/{runtime}/extensions",
            post(|State(m): State<FakeManager>, AxPath(_runtime): AxPath<String>, h: HeaderMap, axum::Json(body): axum::Json<Value>| async move {
                if !bearer_ok(&h) {
                    return (StatusCode::FORBIDDEN, axum::Json(json!({ "code": "forbidden", "error": "not the master token" }))).into_response();
                }
                let id = body["id"].as_str().unwrap_or_default().to_string();
                m.installs.lock().unwrap().push(body);
                if id == "fail.ext" {
                    return (StatusCode::BAD_GATEWAY, axum::Json(json!({ "error": "install failed", "id": id, "output": "no such extension", "installed": false }))).into_response();
                }
                axum::Json(json!({ "installed": true, "id": id, "output": format!("installed {id} by the manager") })).into_response()
            }),
        )
        .route(
            "/api/editor/{runtime}/extensions/{extension}",
            axum::routing::delete(|State(m): State<FakeManager>, AxPath((_runtime, id)): AxPath<(String, String)>, h: HeaderMap| async move {
                if !bearer_ok(&h) {
                    return (StatusCode::FORBIDDEN, axum::Json(json!({ "code": "forbidden", "error": "not the master token" }))).into_response();
                }
                m.installs.lock().unwrap().push(json!({ "uninstall": id }));
                if id == "fail.ext" {
                    // The manager's error shape (core FR-M10).
                    return (StatusCode::UNPROCESSABLE_ENTITY, axum::Json(json!({ "error": { "code": "extension_failed",
                        "message": format!("{id} was not uninstalled"), "data": { "id": id, "output": "extension not installed" } } }))).into_response();
                }
                StatusCode::NO_CONTENT.into_response()
            }),
        )
        .route(
            "/ide/ose/",
            get(|h: HeaderMap| async move {
                let cookie = h.get("cookie").and_then(|v| v.to_str().ok()).unwrap_or("");
                if cookie.contains(&format!("dpx-editor={TICKET}")) {
                    (StatusCode::OK, "editor-ok").into_response()
                } else {
                    (StatusCode::UNAUTHORIZED, "no ticket").into_response()
                }
            }),
        )
        .with_state(m.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (url, m)
}

/// A real node on a loopback port, with the editor relays the test built.
struct NodeFx {
    url: String,
    /// Allowed root (canonical).
    root: PathBuf,
    /// A sibling directory outside the root.
    outside: PathBuf,
    editor: Arc<EditorRelays>,
    _dir: tempfile::TempDir,
}

async fn start_node(editor: EditorRelays) -> NodeFx {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let root = base.join("root");
    let outside = base.join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let editor = Arc::new(editor);
    let node = Node::with_editor(NodeConfig::new(NODE_TOKEN, vec![root.clone()]), editor.clone()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(node_api::serve(listener, node));
    NodeFx { url, root, outside, editor, _dir: dir }
}

/// A node with no manager: its launches are only those a test registers.
fn no_manager() -> EditorRelays {
    EditorRelays::new(ManagerSource::none())
}

/// A node whose manager is the fake at `url`.
fn with_manager(url: &str) -> EditorRelays {
    EditorRelays::new(ManagerSource::fixed(url, MANAGER_TOKEN))
}

/// Registers a launch on the node whose manager endpoint is `addr`; answers its id.
fn reg(node: &NodeFx, addr: SocketAddr) -> String {
    node.editor.register_launch_for_test(ManagerEndpoint { url: format!("http://{addr}"), token: MANAGER_TOKEN.into() })
}

/// The server side: the IDE routes and the extension routes over one registry, with no manager
/// for the local computer and a server OSE manager with no binary (so nothing here can come from
/// the server's own machine).
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
    let app = ide::router_with_computers_and_editor(
        sessions,
        Arc::new(IdeConfig::default()),
        computers.clone(),
        Arc::new(EditorRelays::new(ManagerSource::none())),
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

async fn delete_json(app: &Router, uri: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(Method::DELETE)
        .uri(uri)
        .header("host", "ember.test:8740")
        .body(Body::empty())
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


/// FR-W4c (1) (D55): the Ember target of a registered computer is the node's launch from its
/// manager, behind this server's relay path: `{relay, path, ticket, ose_commit}`, no endpoint, no
/// token, never the manager token. The node's refusals become the reason.
#[tokio::test]
async fn fr_w4c_ide_launch_for_node_computer_calls_node_and_returns_server_relay() {
    let (manager_url, manager) = start_fake_manager().await;
    let node = start_node(with_manager(&manager_url)).await;
    let s = server();
    let computer = s.computers.registry().insert("box", &node.url, NODE_TOKEN).unwrap();

    let (ember, text) = ember_target_for(&s, &node.root, &computer.id).await;
    assert_eq!(ember["available"], true, "{ember}");
    assert!(ember.get("reason").is_none(), "{ember}");
    let launch = &ember["launch"];
    assert_eq!(launch["computer"], computer.id.as_str());
    assert_eq!(launch["folder"].as_str(), Some(&*node.root.to_string_lossy()));
    let ose = &launch["ose"];
    let relay = ose["relay"].as_str().unwrap();
    let prefix = format!("/api/computers/{}/ide/relay/", computer.id);
    assert!(relay.starts_with(&prefix) && relay.len() == prefix.len() + 32, "{relay}");
    assert_eq!(*ose, json!({ "relay": relay, "path": "/ide/ose/", "ticket": TICKET, "ose_commit": COMMIT }));
    assert!(!text.contains(MANAGER_TOKEN), "the manager token is in the launch: {text}");

    // The computer's name resolves to a launch of the same shape.
    let (by_name, _) = ember_target_for(&s, &node.root, "box").await;
    assert_eq!(by_name["launch"]["ose"]["path"], "/ide/ose/");

    // A folder the node's path policy refuses: unavailable, with the node's reason.
    let (ember, _) = ember_target_for(&s, &node.outside, &computer.id).await;
    assert_eq!(ember["available"], false, "{ember}");
    assert!(ember["reason"].as_str().unwrap().contains("outside the allowed roots"), "{ember}");
    assert!(ember["launch"].get("ose").is_none(), "{ember}");

    // A manager without the editor server: unavailable, with the manager's reason.
    *manager.launch_status.lock().unwrap() = 409;
    let (ember, _) = ember_target_for(&s, &node.root, &computer.id).await;
    assert_eq!(ember["available"], false, "{ember}");
    assert!(ember["reason"].as_str().unwrap().contains("not installed"), "{ember}");

    // A node with no manager: unavailable, and the reason says so.
    let bare = start_node(no_manager()).await;
    let bare_computer = s.computers.registry().insert("bare", &bare.url, NODE_TOKEN).unwrap();
    let (ember, _) = ember_target_for(&s, &bare.root, &bare_computer.id).await;
    assert_eq!(ember["available"], false, "{ember}");
    assert!(ember["reason"].as_str().unwrap().contains("DarkPyonix manager is not installed"), "{ember}");

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
async fn read_bytes<S>(ws: &mut WebSocketStream<S>, want: usize) -> Vec<u8>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
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
    let node = start_node(no_manager()).await;
    let (ose_addr, fake) = start_fake_ose().await;
    let launch_id = reg(&node, ose_addr);
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

    // Bytes go both ways, unchanged (D55: nothing is replaced on the node or the server).
    let mut phone_ws = device_ws(&phone, &server_t, &relay).await.expect("an admitted device opens the relay");
    let sent = format!("GET /ide/ose/?tkn={} HTTP/1.1\r\nCookie: dpx-editor={TICKET}\r\n\r\n", "p".repeat(64)).into_bytes();
    phone_ws.send(Message::Binary(sent.clone().into())).await.unwrap();
    let echo = read_bytes(&mut phone_ws, sent.len()).await;
    assert_eq!(echo, sent);
    {
        let seen = fake.received.lock().unwrap();
        assert_eq!(*seen, sent, "the manager endpoint got other bytes than the client sent");
        assert!(!find(&seen, MANAGER_TOKEN.as_bytes()), "the manager token was added");
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

/// FR-W4c (3): an install on a registered computer goes to its node, which forwards it to its
/// manager; the manager's status and body come back unchanged.
#[tokio::test]
async fn fr_w4c_server_ide_extensions_forwards_to_node() {
    let (manager_url, manager) = start_fake_manager().await;
    let node = start_node(with_manager(&manager_url)).await;
    let s = server();
    let computer = s.computers.registry().insert("box", &node.url, NODE_TOKEN).unwrap();
    let uri = format!("/api/computers/{}/ide/extensions", computer.id);

    let (status, v) = post_json(&s.app, &uri, r#"{"id":"good.ext","version":"1.2.3"}"#).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v, json!({ "installed": true, "id": "good.ext", "output": "installed good.ext by the manager" }));
    assert_eq!(*manager.installs.lock().unwrap(), [json!({ "id": "good.ext", "version": "1.2.3" })]);

    // The node's refusal of a bad id and the manager's failure keep their status and body.
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
    assert_eq!(manager.installs.lock().unwrap().len(), 2, "a refused install reached the manager");

    // A node without a manager: its 503 and reason.
    let bare = start_node(no_manager()).await;
    let bare_computer = s.computers.registry().insert("bare", &bare.url, NODE_TOKEN).unwrap();
    let (status, v) =
        post_json(&s.app, &format!("/api/computers/{}/ide/extensions", bare_computer.id), r#"{"id":"a.b"}"#).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(v["code"], "manager_not_installed", "{v}");

    // A node that does not answer is 502; an unknown computer is 404.
    let gone = s.computers.registry().insert("gone", "http://127.0.0.1:1", "tok").unwrap();
    let (status, v) =
        post_json(&s.app, &format!("/api/computers/{}/ide/extensions", gone.id), r#"{"id":"a.b"}"#).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{v}");
    assert!(v["error"].as_str().unwrap().starts_with("node unreachable"), "{v}");
    let (status, _) = post_json(&s.app, "/api/computers/nope/ide/extensions", r#"{"id":"a.b"}"#).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// FR-W4c (3): an uninstall on a registered computer goes to its node, which forwards it to its
/// manager's `DELETE /api/editor/ose/extensions/{extension}`; the manager's status and body come
/// back unchanged.
#[tokio::test]
async fn fr_w4c_server_ide_extensions_uninstall_forwards_to_node() {
    let (manager_url, manager) = start_fake_manager().await;
    let node = start_node(with_manager(&manager_url)).await;
    let s = server();
    let computer = s.computers.registry().insert("box", &node.url, NODE_TOKEN).unwrap();
    let base = format!("/api/computers/{}/ide/extensions", computer.id);

    // The manager removed it: its 204 comes back.
    let (status, _) = delete_json(&s.app, &format!("{base}/good.ext")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(*manager.installs.lock().unwrap(), [json!({ "uninstall": "good.ext" })]);

    // Bad extension ids are rejected with 400 before reaching the manager.
    for bad in ["..%2Fx", "-a.b", "a.b%401.2.3"] {
        let (status, v) = delete_json(&s.app, &format!("{base}/{bad}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {v}");
    }
    assert_eq!(manager.installs.lock().unwrap().len(), 1, "a refused uninstall reached the manager");

    // The manager's failure keeps its status and body.
    let (status, v) = delete_json(&s.app, &format!("{base}/fail.ext")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert_eq!(v["error"]["data"]["output"], "extension not installed", "{v}");

    // A stale node token is 502 `node_auth_failed`, never the node's 401.
    let stale = s.computers.registry().insert("stale", &node.url, "stale-node-token").unwrap();
    let (status, v) = delete_json(&s.app, &format!("/api/computers/{}/ide/extensions/good.ext", stale.id)).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{v}");
    assert_eq!(v["code"], "node_auth_failed", "{v}");
    assert!(!v.to_string().contains("stale-node-token"), "{v}");

    // A node without a manager: its 503 and reason.
    let bare = start_node(no_manager()).await;
    let bare_computer = s.computers.registry().insert("bare", &bare.url, NODE_TOKEN).unwrap();
    let (status, v) = delete_json(&s.app, &format!("/api/computers/{}/ide/extensions/a.b", bare_computer.id)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(v["code"], "manager_not_installed", "{v}");

    // A node that does not answer is 502; an unknown computer is 404.
    let gone = s.computers.registry().insert("gone", "http://127.0.0.1:1", "tok").unwrap();
    let (status, v) = delete_json(&s.app, &format!("/api/computers/{}/ide/extensions/a.b", gone.id)).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{v}");
    assert!(v["error"].as_str().unwrap().starts_with("node unreachable"), "{v}");
    let (status, _) = delete_json(&s.app, "/api/computers/nope/ide/extensions/a.b").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---- Review follow-ups (#896): the node's refusals, pings, scopes and the guest door ----------

/// The server's routes on a loopback TCP port, as the local listener serves them (without the
/// token layer, which is not what these tests are about).
async fn serve_tcp(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

/// Opens the relay at `path` over TCP. Panics when the server refuses the upgrade.
async fn tcp_ws(addr: SocketAddr, path: &str) -> TcpWs {
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let url = format!("ws://{addr}{path}");
    match tokio::time::timeout(WAIT, tokio_tungstenite::client_async(url, tcp)).await {
        Ok(Ok((ws, _))) => ws,
        Ok(Err(e)) => panic!("the relay {path} was refused: {e}"),
        Err(_) => panic!("the relay {path} did not open in time"),
    }
}

/// Asks for the WebSocket upgrade of `path` with a plain HTTP client and returns the status and
/// JSON body of the answer. For refusals: an HTTP client reads the whole body of a refused
/// upgrade, which a WebSocket client does not promise.
async fn upgrade_answer(addr: SocketAddr, path: &str, origin: Option<&str>) -> (u16, Value) {
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut req = http
        .get(format!("http://{addr}{path}"))
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==");
    if let Some(origin) = origin {
        req = req.header("origin", origin);
    }
    let res = tokio::time::timeout(WAIT, req.send()).await.expect("no answer in time").unwrap();
    let status = res.status().as_u16();
    let bytes = res.bytes().await.unwrap_or_default();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// Waits for the relay to end and returns the close code, when a close frame carried one.
async fn wait_closed<S>(ws: &mut WebSocketStream<S>, within: Duration) -> Option<u16>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let ended = tokio::time::timeout(within, async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Close(frame))) => break frame.map(|f| u16::from(f.code)),
                Some(Err(_)) | None => break None,
                Some(Ok(_)) => continue,
            }
        }
    })
    .await;
    ended.unwrap_or_else(|_| panic!("the relay stayed open for {within:?}"))
}

/// A loopback address nothing listens on.
fn dead_address() -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap()
}

/// FR-W4c: a node that refuses the SERVER (a stale node token in the registry) is 502
/// `node_auth_failed` on the relay and on the install, never the node's 401: that status would
/// read as the caller's own credential being refused.
#[tokio::test]
async fn fr_w4c_stale_node_token_is_502_node_auth_failed_on_relay_and_install() {
    const STALE: &str = "stale-node-token";
    let node = start_node(no_manager()).await;
    let (ose_addr, fake) = start_fake_ose().await;
    let launch_id = reg(&node, ose_addr);
    let s = server();
    let stale = s.computers.registry().insert("stale", &node.url, STALE).unwrap();
    let good = s.computers.registry().insert("good", &node.url, NODE_TOKEN).unwrap();
    let addr = serve_tcp(s.app.clone()).await;

    let check = |what: &str, status: u16, v: &Value| {
        assert_eq!(status, 502, "{what}: {v}");
        assert_eq!(v["code"], "node_auth_failed", "{what}: {v}");
        let message = v["error"].as_str().unwrap_or_else(|| panic!("{what}: no error text in {v}"));
        assert!(message.contains("could not authenticate to computer"), "{what}: {message}");
        assert!(message.contains(&stale.id), "{what}: {message}");
        let text = v.to_string();
        assert!(!text.contains(STALE) && !text.contains(NODE_TOKEN), "{what}: a token is in the answer: {text}");
    };

    // The relay: the node answers 401 to the server, the client gets 502.
    let (status, v) =
        upgrade_answer(addr, &format!("/api/computers/{}/ide/relay/{launch_id}", stale.id), None).await;
    check("relay", status, &v);
    assert_eq!(fake.live.load(Ordering::SeqCst), 0, "a refused relay dialed the OSE");

    // The install: the same.
    let (status, v) =
        post_json(&s.app, &format!("/api/computers/{}/ide/extensions", stale.id), r#"{"id":"a.b"}"#).await;
    check("install", status.as_u16(), &v);

    // With the right node token every other node status still passes through: the 503 of a node
    // without the binary, and an open relay.
    let (status, v) =
        post_json(&s.app, &format!("/api/computers/{}/ide/extensions", good.id), r#"{"id":"a.b"}"#).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    let mut ws = tcp_ws(addr, &format!("/api/computers/{}/ide/relay/{launch_id}", good.id)).await;
    ws.send(Message::Binary(b"hello".to_vec().into())).await.unwrap();
    assert_eq!(read_bytes(&mut ws, 5).await, b"hello");
}

/// FR-W4c: the bridge carries pings, so a client that keeps alive with WebSocket pings alone is
/// not cut by the node's idle timeout, as on the local relay. Silence still ends the relay.
#[tokio::test]
async fn fr_w4c_pings_alone_keep_a_bridged_relay_alive_and_silence_ends_it() {
    const IDLE: Duration = Duration::from_secs(1);
    let node = start_node(no_manager().with_relay_idle_timeout(IDLE)).await;
    let (ose_addr, fake) = start_fake_ose().await;
    let launch_id = reg(&node, ose_addr);
    let s = server();
    let computer = s.computers.registry().insert("box", &node.url, NODE_TOKEN).unwrap();
    let addr = serve_tcp(s.app.clone()).await;
    let mut ws = tcp_ws(addr, &format!("/api/computers/{}/ide/relay/{launch_id}", computer.id)).await;
    wait_for("the OSE connection", WAIT, || fake.live.load(Ordering::SeqCst) == 1).await;

    // Three idle timeouts of pings only, no data.
    let started = Instant::now();
    while started.elapsed() < IDLE * 3 {
        ws.send(Message::Ping(b"keepalive".to_vec().into())).await.expect("the relay was cut while pinging");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(fake.live.load(Ordering::SeqCst), 1, "the node dropped the OSE connection while the client pinged");
    ws.send(Message::Binary(b"alive".to_vec().into())).await.unwrap();
    assert_eq!(read_bytes(&mut ws, 5).await, b"alive");

    // Silence: the node's idle timeout ends the relay, and the bridge tells the client.
    let quiet = Instant::now();
    wait_closed(&mut ws, IDLE * 6).await;
    assert!(quiet.elapsed() >= IDLE / 2, "the relay ended after {:?}, before the idle timeout", quiet.elapsed());
    wait_for("the idle relay's OSE connection", WAIT, || fake.live.load(Ordering::SeqCst) == 0).await;
}

/// FR-W4c: what the relay route refuses or passes on. A foreign origin is 403 before anything is
/// dialed, an unreachable node is 502, the node's relay limit is its 429, and a node that cannot
/// reach its OSE closes with 1011, which the bridge passes to the client.
#[tokio::test]
async fn fr_w4c_relay_refusals_origin_unreachable_limit_and_ose_down() {
    let node = start_node(no_manager().with_relay_limits(1, 64)).await;
    let (ose_addr, fake) = start_fake_ose().await;
    let launch_id = reg(&node, ose_addr);
    let s = server();
    let computer = s.computers.registry().insert("box", &node.url, NODE_TOKEN).unwrap();
    let relay = format!("/api/computers/{}/ide/relay/{launch_id}", computer.id);
    let addr = serve_tcp(s.app.clone()).await;

    // The origin check comes first.
    let (status, v) = upgrade_answer(addr, &relay, Some("https://evil.example")).await;
    assert_eq!(status, 403, "{v}");
    assert_eq!(fake.live.load(Ordering::SeqCst), 0, "a refused origin dialed the OSE");

    // A node that does not answer.
    let gone = s.computers.registry().insert("gone", "http://127.0.0.1:1", "tok").unwrap();
    let (status, v) =
        upgrade_answer(addr, &format!("/api/computers/{}/ide/relay/{launch_id}", gone.id), None).await;
    assert_eq!(status, 502, "{v}");
    assert!(v["error"].as_str().unwrap_or_default().starts_with("node unreachable"), "{v}");
    assert_ne!(v["code"], "node_auth_failed", "{v}");

    // The node's relay limit (one per launch here) keeps its status.
    let _held = tcp_ws(addr, &relay).await;
    let (status, v) = upgrade_answer(addr, &relay, None).await;
    assert_eq!(status, 429, "{v}");

    // A launch whose manager is gone: the upgrade succeeds, then the node's 1011 arrives.
    let down_id = reg(&node, dead_address());
    let mut ws = tcp_ws(addr, &format!("/api/computers/{}/ide/relay/{down_id}", computer.id)).await;
    assert_eq!(wait_closed(&mut ws, WAIT).await, Some(1011));
}

/// FR-W4c with FR-N8: the relay of a registered computer is an editor that runs code, so a
/// `read_only` and a `chat_only` device are refused at the transport door and a `full` one is not.
#[tokio::test]
async fn fr_w4c_relay_of_a_registered_computer_needs_a_full_scope_device() {
    let node = start_node(no_manager()).await;
    let (ose_addr, fake) = start_fake_ose().await;
    let launch_id = reg(&node, ose_addr);
    let s = server();
    let computer = s.computers.registry().insert("box", &node.url, NODE_TOKEN).unwrap();
    let relay = format!("/api/computers/{}/ide/relay/{launch_id}", computer.id);

    // The transport door as `main.rs` serves it: with the device scopes.
    let net = MemNetwork::new();
    let devices = Devices::open(s.store.clone()).unwrap();
    let server_t: Transport = net.transport();
    tokio::spawn(server_transport::serve_scoped(&server_t, s.app.clone(), devices.clone()).unwrap());
    let device = |name: &str, scope: Scope| {
        let t = net.transport();
        devices.add(t.peer_id(), name).unwrap();
        devices.set_scope(&t.peer_id(), scope).unwrap();
        Dialer::new(t)
    };
    let (reader, chatter, full) =
        (device("reader", Scope::ReadOnly), device("chatter", Scope::ChatOnly), device("laptop", Scope::Full));

    for (name, narrow) in [("read_only", &reader), ("chat_only", &chatter)] {
        let refused = device_ws(narrow, &server_t, &relay).await.err();
        let refused = refused.unwrap_or_else(|| panic!("a {name} device opened the relay"));
        assert!(refused.contains("403"), "{name}: {refused}");
    }
    assert_eq!(fake.live.load(Ordering::SeqCst), 0, "a refused device's relay dialed the OSE");

    let mut ws = device_ws(&full, &server_t, &relay).await.expect("a full-scope device opens the relay");
    ws.send(Message::Binary(b"full".to_vec().into())).await.unwrap();
    assert_eq!(read_bytes(&mut ws, 4).await, b"full");
}

/// FR-W4c with FR-T14: the guest door serves no IDE route of a registered computer.
#[tokio::test]
async fn fr_w4c_registered_computer_relay_is_404_on_the_guest_door() {
    let s = server();
    let computer = s.computers.registry().insert("box", "http://127.0.0.1:1", NODE_TOKEN).unwrap();
    let sessions = Sessions::new(s.store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let door = GuestDoor::from_lookup(
        |k| (k == "EMBER_GUEST_LISTEN").then(|| "127.0.0.1:9000".to_string()),
        "127.0.0.1:8740".parse::<SocketAddr>().unwrap(),
    )
    .map_err(|e| e.to_string())
    .unwrap()
    .expect("the door is on");
    let guest = door.wrap(guest_router(s.store.clone(), sessions));

    let relay = format!("/api/computers/{}/ide/relay/0123456789abcdef0123456789abcdef", computer.id);
    let req =
        Request::builder().method(Method::GET).uri(&relay).header("host", "127.0.0.1:9000").body(Body::empty()).unwrap();
    assert_eq!(guest.clone().oneshot(req).await.unwrap().status(), StatusCode::NOT_FOUND, "GET {relay}");
    // The door refuses every upgrade before routing (400 `bad_request`, FR-T14), so the relay
    // cannot be opened there either.
    let req = Request::builder()
        .method(Method::GET)
        .uri(&relay)
        .header("host", "127.0.0.1:9000")
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .body(Body::empty())
        .unwrap();
    assert_eq!(guest.clone().oneshot(req).await.unwrap().status(), StatusCode::BAD_REQUEST, "upgrade GET {relay}");

    let install = format!("/api/computers/{}/ide/extensions", computer.id);
    let req = Request::builder()
        .method(Method::POST)
        .uri(&install)
        .header("host", "127.0.0.1:9000")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"id":"a.b"}"#))
        .unwrap();
    assert_eq!(guest.clone().oneshot(req).await.unwrap().status(), StatusCode::NOT_FOUND, "POST {install}");

    // The VS Code pick (FR-W4c (1c)) is not served there either.
    let sid = s.store.create_session("proj", AgentKind::Scripted, "/tmp", None, "t").unwrap().id;
    let pick = format!("/api/sessions/{sid}/ide/vscode?computer={}", computer.id);
    let req =
        Request::builder().method(Method::POST).uri(&pick).header("host", "127.0.0.1:9000").body(Body::empty()).unwrap();
    assert_eq!(guest.clone().oneshot(req).await.unwrap().status(), StatusCode::NOT_FOUND, "POST {pick}");
}

// ---- The VS Code target (FR-W4c (1b), D55, #1064) ---------------------------------------------

fn vscode_target(v: &Value) -> &Value {
    v["targets"].as_array().unwrap().iter().find(|t| t["kind"] == "vscode").expect("a vscode target")
}

/// FR-W4c (1b), (1c) (D55): the listing names VS Code and asks nothing for it; the pick asks the
/// node, which asks its manager, and answers the launch behind this server's relay path with the
/// path and the ticket, no URL, no endpoint and no token; route (2) reaches the manager's editor
/// page with the ticket cookie, the bytes unchanged.
#[tokio::test]
async fn fr_w4c_vscode_target_of_a_node_computer_is_a_relay_launch_without_token() {
    let (manager_url, _manager) = start_fake_manager().await;
    let node = start_node(with_manager(&manager_url)).await;
    let s = server();
    let computer = s.computers.registry().insert("box", &node.url, NODE_TOKEN).unwrap();

    let sid = s.store.create_session("proj", AgentKind::Scripted, &node.root.to_string_lossy(), None, "t").unwrap().id;
    let (status, text) = get_json(&s.app, &format!("/api/sessions/{sid}/ide?computer={}", computer.id)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(*vscode_target(&v), json!({ "kind": "vscode" }), "the listing carries the kind alone");
    let ember = ember_target(&v).clone();

    let (status, launch) = post_json(&s.app, &format!("/api/sessions/{sid}/ide/vscode?computer={}", computer.id), "").await;
    assert_eq!(status, StatusCode::OK, "{launch}");
    for absent in ["url", "endpoint", "connection_token", "available", "reason"] {
        assert!(launch.get(absent).is_none(), "{absent} in {launch}");
        assert!(launch["ose"].get(absent).is_none(), "{absent} in {launch}");
    }
    assert_eq!(launch["session"], sid.as_str());
    assert_eq!(launch["computer"], computer.id.as_str());
    assert_eq!(launch["computer_name"], "box");
    assert_eq!(launch["folder"].as_str(), Some(&*node.root.to_string_lossy()));
    assert_eq!(launch["ose"]["path"], "/ide/ose/");
    assert_eq!(launch["ose"]["ticket"], TICKET);
    assert_eq!(launch["ose"]["ose_commit"], COMMIT);
    let relay = launch["ose"]["relay"].as_str().unwrap().to_string();
    assert!(relay.starts_with(&format!("/api/computers/{}/ide/relay/", computer.id)), "{relay}");
    assert_ne!(ember["launch"]["ose"]["relay"], launch["ose"]["relay"], "each launch is its own relay id");
    assert!(!launch.to_string().contains(MANAGER_TOKEN), "the manager token is in the launch: {launch}");

    // Route (2): an HTTP request with the ticket cookie reaches the manager's editor page.
    let addr = serve_tcp(s.app.clone()).await;
    let mut ws = tcp_ws(addr, &relay).await;
    let sent = format!("GET /ide/ose/ HTTP/1.1\r\nHost: 127.0.0.1\r\nCookie: dpx-editor={TICKET}\r\nConnection: close\r\n\r\n");
    ws.send(Message::Binary(sent.into_bytes().into())).await.unwrap();
    let mut answer = Vec::new();
    while !String::from_utf8_lossy(&answer).contains("editor-ok") {
        match tokio::time::timeout(WAIT, ws.next()).await.expect("no answer in time") {
            Some(Ok(Message::Binary(b))) => answer.extend_from_slice(&b),
            other => panic!("the relay ended before the page: {other:?} after {:?}", String::from_utf8_lossy(&answer)),
        }
    }
    assert!(String::from_utf8_lossy(&answer).starts_with("HTTP/1.1 200"), "{}", String::from_utf8_lossy(&answer));
}

/// What a recording stand-in node answers to `POST /ide/ose`.
#[derive(Clone, Copy)]
enum NodeAnswer {
    /// A node with D55: a manager launch `{launch_id, path, ticket, ose_commit}` for both targets.
    Current,
    /// A node older than D55: it ignores `target` and answers a TCP launch with a token.
    BeforeD55,
}

const MANAGER_LAUNCH_ID: &str = "0123456789abcdef0123456789abcdef";
const TCP_LAUNCH_ID: &str = "fedcba9876543210fedcba9876543210";

/// A stand-in node on a loopback port that records the body of every `POST /ide/ose`.
async fn start_recording_node(answer: NodeAnswer) -> (String, Arc<Mutex<Vec<Value>>>) {
    use axum::routing::post;
    let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen = calls.clone();
    let app = Router::new().route(
        "/ide/ose",
        post(move |axum::Json(body): axum::Json<Value>| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(body);
                axum::Json(match answer {
                    NodeAnswer::Current => json!({
                        "launch_id": MANAGER_LAUNCH_ID, "path": "/ide/ose/", "ticket": TICKET, "ose_commit": "c0ffee"
                    }),
                    NodeAnswer::BeforeD55 => json!({
                        "launch_id": TCP_LAUNCH_ID,
                        "endpoint": "http://127.0.0.1:9",
                        "connection_token": "p".repeat(64),
                    }),
                })
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, calls)
}

/// The targets a recorded call asked for (`ember` when the body has none).
fn recorded_targets(calls: &Mutex<Vec<Value>>) -> Vec<String> {
    calls
        .lock()
        .unwrap()
        .iter()
        .map(|c| c.get("target").and_then(Value::as_str).unwrap_or("ember").to_string())
        .collect()
}

/// FR-W4c (1b): listing the targets asks the node for the Ember launch alone; nothing is asked
/// for VS Code until it is picked.
#[tokio::test]
async fn fr_w4c_listing_the_targets_starts_no_vscode_ose() {
    let (url, calls) = start_recording_node(NodeAnswer::Current).await;
    let s = server();
    let computer = s.computers.registry().insert("box", &url, NODE_TOKEN).unwrap();
    let folder = tempfile::tempdir().unwrap();
    let sid = s.store.create_session("proj", AgentKind::Scripted, &folder.path().to_string_lossy(), None, "t").unwrap().id;

    for _ in 0..2 {
        let (status, text) = get_json(&s.app, &format!("/api/sessions/{sid}/ide?computer={}", computer.id)).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(*vscode_target(&v), json!({ "kind": "vscode" }));
        assert_eq!(ember_target(&v)["available"], true, "{v}");
    }
    assert_eq!(recorded_targets(&calls), ["ember", "ember"], "the listing asked the node for a VS Code OSE");
}

/// FR-W4c (1c): picking VS Code makes one `POST /ide/ose {folder, target: "vscode"}` and answers
/// the launch with this server's relay path, the node's path, ticket and `ose_commit`; the
/// computer's name resolves as on the listing.
#[tokio::test]
async fn fr_w4c_picking_vscode_starts_its_ose_and_answers_the_launch() {
    let (url, calls) = start_recording_node(NodeAnswer::Current).await;
    let s = server();
    let computer = s.computers.registry().insert("box", &url, NODE_TOKEN).unwrap();
    let folder = tempfile::tempdir().unwrap();
    let cwd = folder.path().to_string_lossy().into_owned();
    let sid = s.store.create_session("proj", AgentKind::Scripted, &cwd, None, "t").unwrap().id;

    for which in [computer.id.as_str(), "box"] {
        let (status, launch) = post_json(&s.app, &format!("/api/sessions/{sid}/ide/vscode?computer={which}"), "").await;
        assert_eq!(status, StatusCode::OK, "{launch}");
        assert_eq!(
            launch,
            json!({
                "session": sid,
                "project": "proj",
                "computer": computer.id,
                "computer_name": "box",
                "folder": cwd,
                "server_url": "http://ember.test:8740",
                "ose": {
                    "relay": format!("/api/computers/{}/ide/relay/{MANAGER_LAUNCH_ID}", computer.id),
                    "path": "/ide/ose/",
                    "ticket": TICKET,
                    "ose_commit": "c0ffee",
                },
            })
        );
    }
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded.len(), 2, "{recorded:?}");
    for call in &recorded {
        assert_eq!(*call, json!({ "folder": cwd, "target": "vscode" }));
    }
}

/// FR-W4c (1b), (1c): a VS Code failure appears only as the pick's error `{code, error}`: an
/// unreachable node 502, a node with no manager 503 `manager_not_installed`, a manager without the
/// editor server 503 `ose_not_installed`, a node older than D55 502 `node_too_old_for_vscode`, an
/// unknown session 404. The listing still offers VS Code on each.
#[tokio::test]
async fn fr_w4c_picking_vscode_on_an_unavailable_computer_is_an_error() {
    let s = server();
    let folder = tempfile::tempdir().unwrap();
    let sid = s.store.create_session("proj", AgentKind::Scripted, &folder.path().to_string_lossy(), None, "t").unwrap().id;

    let pick = |computer: String| {
        let app = s.app.clone();
        let sid = sid.clone();
        async move {
            let (status, text) = get_json(&app, &format!("/api/sessions/{sid}/ide?computer={computer}")).await;
            assert_eq!(status, StatusCode::OK, "{text}");
            let v: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(*vscode_target(&v), json!({ "kind": "vscode" }), "the listing dropped VS Code");
            post_json(&app, &format!("/api/sessions/{sid}/ide/vscode?computer={computer}"), "").await
        }
    };

    // A node that does not answer.
    let gone = s.computers.registry().insert("gone", "http://127.0.0.1:1", NODE_TOKEN).unwrap();
    let (status, body) = pick(gone.id.clone()).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["code"], ide::NODE_UNREACHABLE, "{body}");
    assert!(body["error"].as_str().unwrap().starts_with("node unreachable"), "{body}");

    // A node with no manager.
    let node = start_node(no_manager()).await;
    let bare = s.computers.registry().insert("bare", &node.url, NODE_TOKEN).unwrap();
    let sid_on_node =
        s.store.create_session("proj", AgentKind::Scripted, &node.root.to_string_lossy(), None, "t").unwrap().id;
    let (status, body) =
        post_json(&s.app, &format!("/api/sessions/{sid_on_node}/ide/vscode?computer={}", bare.id), "").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], ide::MANAGER_NOT_INSTALLED, "{body}");
    assert!(body["error"].as_str().unwrap().contains("not installed"), "{body}");

    // A manager without the editor server (its 409).
    let (manager_url, manager) = start_fake_manager().await;
    *manager.launch_status.lock().unwrap() = 409;
    let managed = start_node(with_manager(&manager_url)).await;
    let missing = s.computers.registry().insert("missing", &managed.url, NODE_TOKEN).unwrap();
    let sid_on_managed =
        s.store.create_session("proj", AgentKind::Scripted, &managed.root.to_string_lossy(), None, "t").unwrap().id;
    let (status, body) =
        post_json(&s.app, &format!("/api/sessions/{sid_on_managed}/ide/vscode?computer={}", missing.id), "").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], ide::OSE_NOT_INSTALLED, "{body}");
    assert_eq!(body["error"], "the ose editor server is not installed", "{body}");

    // A node older than D55: it ignored `target` and answered a token.
    let (old_url, calls) = start_recording_node(NodeAnswer::BeforeD55).await;
    let old = s.computers.registry().insert("old", &old_url, NODE_TOKEN).unwrap();
    let (status, body) = pick(old.id.clone()).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["code"], ide::NODE_TOO_OLD_FOR_VSCODE_CODE, "{body}");
    assert_eq!(body["error"], ide::NODE_TOO_OLD_FOR_VSCODE, "{body}");
    assert!(!body.to_string().contains(&"p".repeat(64)), "the node's token is in the error: {body}");
    assert_eq!(recorded_targets(&calls), ["ember", "vscode"]);

    // An unknown session.
    let (status, body) = post_json(&s.app, "/api/sessions/nope/ide/vscode", "").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["code"], ide::SESSION_NOT_FOUND, "{body}");
}
