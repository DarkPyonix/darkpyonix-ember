//! SPEC FR-W4a and FR-W4b (`INTENT.md` D55, #1064): the editor routes of a node, through the
//! DarkPyonix manager.
//!
//! `POST /ide/ose {folder, target}` checks the folder against the node's roots and asks the
//! manager's `POST /api/editor/ose/launch`, answering `{launch_id, path, ticket, ose_commit?}`.
//! `GET /ide/ose/{launch_id}/relay` joins the client's binary frames to the manager's endpoint and
//! replaces nothing. `POST /ide/extensions` forwards to the manager's
//! `POST /api/editor/ose/extensions` and answers its status and body. All three sit behind the
//! node's token.
//!
//! The manager is a fake here: an axum server with the two control routes, which checks the
//! Bearer token and records what it got, and a loopback TCP listener that records and echoes for
//! the relay. A real manager and a real editor server are not covered.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path as AxPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, post};
use axum::{Json, Router};
use ember_node::api::{self, Node};
use ember_node::client::{ClientError, NodeClient};
use ember_node::config::NodeConfig;
use ember_node::editor_manager::{EditorRelays, ManagerEndpoint, ManagerSource};
use ember_node::ose_api::{OseRelayStream, OseTarget};
use ember_node::proto::ErrorCode;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;

const TOKEN: &str = "node-token";
const MANAGER_TOKEN: &str = "manager-master-token-0123456789";
const TICKET: &str = "ticket-abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdef";
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const WAIT: Duration = Duration::from_secs(20);

/// What the fake manager answers to a launch, and what it saw.
#[derive(Clone)]
struct FakeManager {
    /// The status of `launch`: 200 answers a ticket, anything else `{code, error}`.
    launch_status: Arc<Mutex<u16>>,
    /// The status and body of `extensions`.
    install_answer: Arc<Mutex<(u16, Value)>>,
    launches: Arc<AtomicUsize>,
    /// `(runtime, path, body)` of every control request.
    seen: Arc<Mutex<Vec<(String, String, Value)>>>,
}

impl FakeManager {
    fn new() -> Self {
        FakeManager {
            launch_status: Arc::new(Mutex::new(200)),
            install_answer: Arc::new(Mutex::new((200, json!({ "installed": true, "id": "a.b", "output": "ok" })))),
            launches: Arc::new(AtomicUsize::new(0)),
            seen: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

fn bearer_ok(headers: &HeaderMap) -> bool {
    headers.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {MANAGER_TOKEN}"))
}

async fn fake_launch(State(m): State<FakeManager>, AxPath(runtime): AxPath<String>, headers: HeaderMap) -> Response {
    if !bearer_ok(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "code": "unauthorized", "error": "bad token" }))).into_response();
    }
    m.launches.fetch_add(1, Ordering::SeqCst);
    m.seen.lock().unwrap().push((runtime.clone(), "launch".into(), Value::Null));
    let status = *m.launch_status.lock().unwrap();
    match status {
        200 => Json(json!({
            "ticket": TICKET, "path": format!("/ide/{runtime}/"), "commit": COMMIT, "version": "1.139.1"
        }))
        .into_response(),
        409 => (
            StatusCode::CONFLICT,
            Json(json!({ "code": "editor_not_installed", "error": "the ose editor server is not installed" })),
        )
            .into_response(),
        other => (StatusCode::from_u16(other).unwrap(), Json(json!({ "code": "boom", "error": "manager trouble" })))
            .into_response(),
    }
}

async fn fake_extensions(
    State(m): State<FakeManager>,
    AxPath(runtime): AxPath<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !bearer_ok(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "code": "unauthorized", "error": "bad token" }))).into_response();
    }
    m.seen.lock().unwrap().push((runtime, "extensions".into(), body));
    let (status, body) = m.install_answer.lock().unwrap().clone();
    (StatusCode::from_u16(status).unwrap(), Json(body)).into_response()
}

async fn fake_uninstall(
    State(m): State<FakeManager>,
    AxPath((runtime, extension)): AxPath<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if !bearer_ok(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "code": "unauthorized", "error": "bad token" }))).into_response();
    }
    m.seen.lock().unwrap().push((runtime, "uninstall".into(), Value::String(extension)));
    let (status, body) = m.install_answer.lock().unwrap().clone();
    (StatusCode::from_u16(status).unwrap(), Json(body)).into_response()
}

/// The fake manager on a loopback port: its url and its state.
async fn start_manager() -> (String, FakeManager) {
    let m = FakeManager::new();
    let app = Router::new()
        .route("/api/editor/{runtime}/launch", post(fake_launch))
        .route("/api/editor/{runtime}/extensions", post(fake_extensions))
        .route("/api/editor/{runtime}/extensions/{extension}", delete(fake_uninstall))
        .with_state(m.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, m)
}

/// What the echoing endpoint saw: every byte, and how many connections are open now.
#[derive(Clone, Default)]
struct Echo {
    received: Arc<Mutex<Vec<u8>>>,
    live: Arc<AtomicUsize>,
    /// The bytes of each read, in order (a client message arrives as one write).
    reads: Arc<Mutex<Vec<Vec<u8>>>>,
}

/// A loopback TCP "manager endpoint" that records what it receives and echoes it back.
async fn start_echo() -> (SocketAddr, Echo) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let echo = Echo::default();
    let bg = echo.clone();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            let echo = bg.clone();
            echo.live.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    match s.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            echo.received.lock().unwrap().extend_from_slice(&buf[..n]);
                            echo.reads.lock().unwrap().push(buf[..n].to_vec());
                            if s.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                    }
                }
                echo.live.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    (addr, echo)
}

struct Fx {
    client: NodeClient,
    base: String,
    root: PathBuf,
    outside: PathBuf,
    editor: Arc<EditorRelays>,
    _dir: tempfile::TempDir,
}

/// A node on a loopback port whose editor routes use `editor`.
async fn start(editor: EditorRelays) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let base_dir = dir.path().canonicalize().unwrap();
    let root = base_dir.join("root");
    let outside = base_dir.join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let editor = Arc::new(editor);
    let node = Node::with_editor(NodeConfig::new(TOKEN, vec![root.clone()]), editor.clone()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node));
    Fx { client: NodeClient::new(&base, TOKEN).unwrap(), base, root, outside, editor, _dir: dir }
}

/// Reads binary messages until `want` bytes arrived.
async fn read_bytes(ws: &mut OseRelayStream, want: usize) -> Vec<u8> {
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

/// FR-W4b (D55): a launch asks the manager with its master token and answers the path and the
/// ticket, for both targets, behind the node's token; the folder is checked first.
#[tokio::test]
async fn fr_w4b_launch_asks_the_manager_and_answers_path_and_ticket() {
    let (url, m) = start_manager().await;
    let f = start(EditorRelays::new(ManagerSource::fixed(url, MANAGER_TOKEN))).await;

    // A wrong node token is refused before the manager is asked.
    let bad = NodeClient::new(&f.base, "wrong").unwrap();
    let e = bad.ide_ose(&f.root).await.unwrap_err();
    assert!(matches!(&e, ClientError::Api { status: 401, .. }), "{e:?}");
    let e = bad.ide_ose_relay("0123456789abcdef0123456789abcdef").await.err().unwrap();
    assert!(matches!(&e, ClientError::Api { status: 401, .. }), "{e:?}");
    assert_eq!(m.launches.load(Ordering::SeqCst), 0);

    for target in [OseTarget::Ember, OseTarget::Vscode] {
        let launch = f.client.ide_ose_for(&f.root, target).await.unwrap();
        assert_eq!(launch.launch_id.len(), 32);
        assert!(launch.launch_id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(launch.path, "/ide/ose/");
        assert_eq!(launch.ticket, TICKET);
        assert_eq!(launch.ose_commit.as_deref(), Some(COMMIT));
        assert!(!format!("{launch:?}").contains(TICKET), "the ticket is printed");
    }
    assert!(m.seen.lock().unwrap().iter().all(|(runtime, _, _)| runtime == "ose"), "both targets use ose");

    // The exact JSON: no endpoint, no connection token, and never the manager token.
    let (status, body) = f.client.post_raw("/ide/ose", &json!({ "folder": f.root, "target": "vscode" })).await.unwrap();
    assert_eq!(status, 200);
    let text = String::from_utf8(body).unwrap();
    assert!(!text.contains(MANAGER_TOKEN), "the manager token left the node: {text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["launch_id", "ose_commit", "path", "ticket"]);

    // Each launch is its own id.
    let a = f.client.ide_ose(&f.root).await.unwrap();
    let b = f.client.ide_ose(&f.root).await.unwrap();
    assert_ne!(a.launch_id, b.launch_id);

    // Folder checks are the node's, before the manager.
    let before = m.launches.load(Ordering::SeqCst);
    let e = f.client.ide_ose(&f.outside).await.unwrap_err();
    assert!(matches!(&e, ClientError::Api { status: 403, .. }), "{e:?}");
    assert_eq!(e.code(), Some(ErrorCode::ForbiddenPath));
    let e = f.client.ide_ose(f.root.join("missing")).await.unwrap_err();
    assert!(matches!(&e, ClientError::Api { status: 404, .. }), "{e:?}");
    let e = f.client.ide_ose("relative/folder").await.unwrap_err();
    assert!(matches!(&e, ClientError::Api { status: 400, .. }), "{e:?}");
    let file = f.root.join("a-file");
    std::fs::write(&file, b"x").unwrap();
    let e = f.client.ide_ose(&file).await.unwrap_err();
    assert!(matches!(&e, ClientError::Api { status: 400, .. }), "{e:?}");
    assert_eq!(m.launches.load(Ordering::SeqCst), before, "a refused folder reached the manager");
}

/// FR-W4b (D55): the relay joins the client's bytes to the manager's endpoint and back, one write
/// per client message (`FR-N9.1`), with nothing replaced; an unknown launch is 404 before the
/// upgrade, a text frame closes with 1003, the limit is 429, and closing the client closes the
/// connection to the manager.
#[tokio::test]
async fn fr_w4b_relay_joins_the_manager_endpoint_without_replacement() {
    let (addr, echo) = start_echo().await;
    let endpoint = ManagerEndpoint { url: format!("http://{addr}"), token: MANAGER_TOKEN.into() };
    let f = start(EditorRelays::new(ManagerSource::fixed(endpoint.url.clone(), MANAGER_TOKEN)).with_relay_limits(1, 64)).await;
    let launch_id = f.editor.register_launch_for_test(endpoint);

    let e = f.client.ide_ose_relay("0123456789abcdef0123456789abcdef").await.err().unwrap();
    assert!(matches!(&e, ClientError::Api { status: 404, .. }), "{e:?}");
    assert_eq!(echo.live.load(Ordering::SeqCst), 0, "an unknown launch must dial nothing");

    let mut ws = f.client.ide_ose_relay(&launch_id).await.unwrap();
    // A 64-character token-like value and the ticket cookie pass unchanged.
    let token_like = "p".repeat(64);
    let mut first = format!(
        "GET /ide/ose/?folder=/w&dpx-ticket={TICKET}&tkn={token_like} HTTP/1.1\r\nCookie: dpx-editor={TICKET}\r\n\r\n"
    )
    .into_bytes();
    first.resize(9000, b'x');
    ws.send(Message::Binary(first.clone().into())).await.unwrap();
    assert_eq!(read_bytes(&mut ws, first.len()).await, first);
    let second = b"second message".to_vec();
    ws.send(Message::Binary(second.clone().into())).await.unwrap();
    assert_eq!(read_bytes(&mut ws, second.len()).await, second);
    let seen = echo.received.lock().unwrap().clone();
    assert_eq!(seen, [first.clone(), second.clone()].concat(), "the manager got other bytes than the client sent");
    assert!(!seen.windows(MANAGER_TOKEN.len()).any(|w| w == MANAGER_TOKEN.as_bytes()), "the relay added the manager token");

    // The limit of one relay per launch.
    let e = f.client.ide_ose_relay(&launch_id).await.err().unwrap();
    assert!(matches!(&e, ClientError::Api { status: 429, .. }), "{e:?}");

    ws.close(None).await.unwrap();
    let deadline = std::time::Instant::now() + WAIT;
    while echo.live.load(Ordering::SeqCst) != 0 {
        assert!(std::time::Instant::now() < deadline, "the connection to the manager stayed open");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // A text frame closes the relay with 1003 (the place is free again once the relay ended).
    let mut ws = loop {
        match f.client.ide_ose_relay(&launch_id).await {
            Ok(ws) => break ws,
            Err(ClientError::Api { status: 429, .. }) => tokio::time::sleep(Duration::from_millis(20)).await,
            Err(e) => panic!("{e:?}"),
        }
    };
    ws.send(Message::text("not binary")).await.unwrap();
    let code = loop {
        match tokio::time::timeout(WAIT, ws.next()).await.expect("no close in time") {
            Some(Ok(Message::Close(Some(frame)))) => break u16::from(frame.code),
            Some(Ok(Message::Close(None))) | Some(Err(_)) | None => panic!("closed without the 1003 frame"),
            Some(Ok(_)) => continue,
        }
    };
    assert_eq!(code, 1003);

    // A manager that is gone: the relay closes with 1011.
    let gone = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gone_url = format!("http://{}", gone.local_addr().unwrap());
    drop(gone);
    let dead = f.editor.register_launch_for_test(ManagerEndpoint { url: gone_url, token: MANAGER_TOKEN.into() });
    let mut ws = f.client.ide_ose_relay(&dead).await.unwrap();
    let code = loop {
        match tokio::time::timeout(WAIT, ws.next()).await.expect("no close in time") {
            Some(Ok(Message::Close(Some(frame)))) => break u16::from(frame.code),
            Some(Ok(Message::Close(None))) | Some(Err(_)) | None => panic!("closed without the 1011 frame"),
            Some(Ok(_)) => continue,
        }
    };
    assert_eq!(code, 1011);
}

/// FR-W4b (D55): with no manager known, a launch is 503 `manager_not_installed` and an install
/// is the same.
#[tokio::test]
async fn fr_w4b_no_manager_is_503() {
    let f = start(EditorRelays::new(ManagerSource::none())).await;
    let e = f.client.ide_ose(&f.root).await.unwrap_err();
    assert!(matches!(&e, ClientError::Api { status: 503, .. }), "{e:?}");
    assert_eq!(e.code(), Some(ErrorCode::ManagerNotInstalled));
    let (status, body) = f.client.post_raw("/ide/extensions", &json!({ "id": "a.b" })).await.unwrap();
    assert_eq!(status, 503, "{}", String::from_utf8_lossy(&body));
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["code"], "manager_not_installed");

    // A node built with `Node::new` and an empty state dir finds no manager file either (the
    // environment is not set in tests).
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let mut cfg = NodeConfig::new(TOKEN, vec![root.clone()]);
    cfg.state_dir = Some(root.join("state"));
    if std::env::var_os("DARKPYONIX_MANAGER_URL").is_none() {
        let node = Node::new(cfg).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(api::serve(listener, node));
        let e = NodeClient::new(&base, TOKEN).unwrap().ide_ose(&root).await.unwrap_err();
        assert!(matches!(&e, ClientError::Api { status: 503, .. }), "{e:?}");
        assert_eq!(e.code(), Some(ErrorCode::ManagerNotInstalled));
    }
}

/// FR-W4b (D55): the manager's 409 is 503 `ose_not_installed` with its reason, another refusal
/// (here a wrong manager token, 401) is 502 `manager_error`, and a manager that does not answer is
/// 502 `manager_unreachable`.
#[tokio::test]
async fn fr_w4b_manager_refusal_maps_to_codes() {
    let (url, m) = start_manager().await;
    let f = start(EditorRelays::new(ManagerSource::fixed(url.clone(), MANAGER_TOKEN))).await;

    *m.launch_status.lock().unwrap() = 409;
    let e = f.client.ide_ose(&f.root).await.unwrap_err();
    match &e {
        ClientError::Api { status: 503, body } => {
            assert_eq!(body.code, ErrorCode::OseNotInstalled);
            let release = include_str!("../../ose/VERSION").trim();
            let command = format!("darkpyonix editor install --runtime ose --release ose-v{release}");
            assert!(body.error.starts_with("the ose editor server is not installed"), "{body:?}");
            assert!(body.error.contains(&command), "{body:?}");
        }
        other => panic!("{other:?}"),
    }

    *m.launch_status.lock().unwrap() = 500;
    let e = f.client.ide_ose(&f.root).await.unwrap_err();
    assert!(matches!(&e, ClientError::Api { status: 502, .. }), "{e:?}");
    assert_eq!(e.code(), Some(ErrorCode::ManagerError));

    let wrong = start(EditorRelays::new(ManagerSource::fixed(url, "not-the-master-token"))).await;
    let e = wrong.client.ide_ose(&wrong.root).await.unwrap_err();
    assert!(matches!(&e, ClientError::Api { status: 502, .. }), "{e:?}");
    assert_eq!(e.code(), Some(ErrorCode::ManagerError));

    let gone = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gone_url = format!("http://{}", gone.local_addr().unwrap());
    drop(gone);
    let dead = start(EditorRelays::new(ManagerSource::fixed(gone_url, MANAGER_TOKEN))).await;
    let e = dead.client.ide_ose(&dead.root).await.unwrap_err();
    assert!(matches!(&e, ClientError::Api { status: 502, .. }), "{e:?}");
    assert_eq!(e.code(), Some(ErrorCode::ManagerUnreachable));
    let (status, body) = dead.client.post_raw("/ide/extensions", &json!({ "id": "a.b" })).await.unwrap();
    assert_eq!(status, 502, "{}", String::from_utf8_lossy(&body));
}

/// FR-W4b (3) (D55): an install is checked (400 for a bad body or id, nothing forwarded) and then
/// forwarded to the manager's `POST /api/editor/ose/extensions`, whose status and body come back
/// unchanged.
#[tokio::test]
async fn fr_w4b_extensions_forward_to_the_manager() {
    let (url, m) = start_manager().await;
    let f = start(EditorRelays::new(ManagerSource::fixed(url, MANAGER_TOKEN))).await;

    let (status, body) = f.client.post_raw("/ide/extensions", &json!({ "id": "redhat.vscode-yaml", "version": "2.0.0" })).await.unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), json!({ "installed": true, "id": "a.b", "output": "ok" }));
    {
        let seen = m.seen.lock().unwrap();
        let (runtime, what, sent) = seen.last().unwrap();
        assert_eq!((runtime.as_str(), what.as_str()), ("ose", "extensions"));
        assert_eq!(*sent, json!({ "id": "redhat.vscode-yaml", "version": "2.0.0" }));
    }

    // A refusal of the manager keeps its status and body.
    *m.install_answer.lock().unwrap() = (409, json!({ "code": "busy", "error": "an install is already running" }));
    let (status, body) = f.client.post_raw("/ide/extensions", &json!({ "id": "a.b" })).await.unwrap();
    assert_eq!(status, 409);
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), json!({ "code": "busy", "error": "an install is already running" }));
    *m.install_answer.lock().unwrap() = (502, json!({ "error": "install failed", "output": "x", "installed": false }));
    let (status, body) = f.client.post_raw("/ide/extensions", &json!({ "id": "a.b" })).await.unwrap();
    assert_eq!(status, 502);
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["output"], "x");

    // Bad input never reaches the manager.
    let before = m.seen.lock().unwrap().len();
    for bad in [json!({ "id": "../../etc" }), json!({ "id": "a.b", "extra": 1 }), json!({ "nope": true })] {
        let (status, _) = f.client.post_raw("/ide/extensions", &bad).await.unwrap();
        assert_eq!(status, 400, "{bad}");
    }
    assert_eq!(m.seen.lock().unwrap().len(), before);

    // Behind the node's token.
    let bad = NodeClient::new(&f.base, "wrong").unwrap();
    let (status, _) = bad.post_raw("/ide/extensions", &json!({ "id": "a.b" })).await.unwrap();
    assert_eq!(status, 401);
}

/// FR-W4c (D55): an uninstall is checked (400 for a bad id, nothing forwarded) and then forwarded
/// to the manager's `DELETE /api/editor/ose/extensions/{extension}`, whose status and body come
/// back unchanged; with no manager it is 503 like an install.
#[tokio::test]
async fn fr_w4c_uninstall_forwards_to_the_manager() {
    let (url, m) = start_manager().await;
    let f = start(EditorRelays::new(ManagerSource::fixed(url, MANAGER_TOKEN))).await;
    let del = |client: NodeClient, ext: &'static str| async move {
        client.request_raw(axum::http::Method::DELETE, &format!("/ide/extensions/{ext}"), None).await.unwrap()
    };

    *m.install_answer.lock().unwrap() = (200, json!({ "uninstalled": true, "id": "redhat.vscode-yaml", "output": "ok" }));
    let (status, body) = del(f.client.clone(), "redhat.vscode-yaml").await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["uninstalled"], true);
    {
        let seen = m.seen.lock().unwrap();
        let (runtime, what, sent) = seen.last().unwrap();
        assert_eq!((runtime.as_str(), what.as_str()), ("ose", "uninstall"));
        assert_eq!(*sent, json!("redhat.vscode-yaml"));
    }

    // A refusal of the manager keeps its status and body.
    *m.install_answer.lock().unwrap() = (409, json!({ "code": "busy", "error": "an install is already running" }));
    let (status, body) = del(f.client.clone(), "a.b").await;
    assert_eq!(status, 409);
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["code"], "busy");

    // Bad ids never reach the manager.
    let before = m.seen.lock().unwrap().len();
    for bad in ["-a.b", "a.b%401.2.3", "nodot"] {
        let (status, _) = del(f.client.clone(), bad).await;
        assert_eq!(status, 400, "{bad}");
    }
    assert_eq!(m.seen.lock().unwrap().len(), before);

    let none = start(EditorRelays::new(ManagerSource::none())).await;
    let (status, body) = del(none.client.clone(), "a.b").await;
    assert_eq!(status, 503);
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["code"], "manager_not_installed");
}
