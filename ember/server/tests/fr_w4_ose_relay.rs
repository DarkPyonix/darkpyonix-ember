//! SPEC FR-W4, FR-W4c, FR-N3, FR-T14 (#458, #461, D55, #1064): the authenticated WebSocket relay
//! of the local computer (`GET /api/computers/local/ide/relay/{launch_id}`) to the DarkPyonix
//! manager of this machine. Nothing is replaced: each client message is one write (`FR-N9.1`).
//!
//! The manager is a fake: a loopback TCP listener registered as a launch
//! (`EditorRelays::register_launch_for_test`), or an axum server with the manager's launch route
//! and a page under `/ide/ose/` that needs the ticket cookie.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::api::ide::{self, IdeConfig};
use ember_server::auth::{self, ServerAuth};
use ember_server::channels::guest::guest_router;
use ember_server::computers::{self, Computers, Registry};
use ember_server::devices::Devices;
use ember_server::guest_door::GuestDoor;
use ember_node::editor_manager::{EditorRelays, ManagerEndpoint, ManagerSource};
use ember_server::session::Sessions;
use ember_server::store::Store;
use ember_server::transport::{self as server_transport, SERVER_SERVICE};
use ember_transport::mem::MemNetwork;
use ember_transport::{BiStream, Dialer, PeerAddr, Transport};
use futures::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use tower::ServiceExt;

const MANAGER_TOKEN: &str = "manager-master-token";
const TICKET: &str = "ticket-0123456789abcdef";

struct RelayHarness {
    addr: SocketAddr,
    raw_addr: SocketAddr,
    token: String,
    editor: Arc<EditorRelays>,
    store: Arc<Store>,
    app: Router,
    _dir: tempfile::TempDir,
}

async fn start_harness(editor: Arc<EditorRelays>) -> RelayHarness {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let computers = Computers::new(Registry::new(store.clone()), computers::default_connector());
    let raw_app = ide::router_with_computers_and_editor(
        sessions,
        Arc::new(IdeConfig::default()),
        computers,
        editor.clone(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let token = auth::load_or_create_token(dir.path()).unwrap();
    let app = auth::protect(raw_app.clone(), ServerAuth::new(token.clone(), addr, []));
    let served = app.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, served).await;
    });
    let raw_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let raw_addr = raw_listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(raw_listener, raw_app).await;
    });
    RelayHarness { addr, raw_addr, token, editor, store, app, _dir: dir }
}

async fn try_ws_status(addr: SocketAddr, path: &str, token: Option<&str>, origin: Option<&str>) -> u16 {
    let url = format!("ws://{addr}{path}");
    let mut req = url.into_client_request().unwrap();
    if let Some(t) = token {
        req.headers_mut().insert("Authorization", format!("Bearer {t}").parse().unwrap());
    }
    if let Some(o) = origin {
        req.headers_mut().insert("Origin", o.parse().unwrap());
    }
    match tokio_tungstenite::connect_async(req).await {
        Ok((mut ws, resp)) => {
            let code = resp.status().as_u16();
            let _ = ws.close(None).await;
            code
        }
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => r.status().as_u16(),
        Err(e) => panic!("unexpected ws connect error: {e}"),
    }
}

/// Registers a launch whose manager endpoint is `addr` and answers its relay path.
fn register(editor: &EditorRelays, addr: SocketAddr) -> String {
    let id = editor.register_launch_for_test(ManagerEndpoint { url: format!("http://{addr}"), token: MANAGER_TOKEN.into() });
    format!("/api/computers/local/ide/relay/{id}")
}

/// Spawns a fake manager endpoint (TCP echo) and returns `(address, connection_counter)`.
async fn start_echo_ose() -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let dials = Arc::new(AtomicUsize::new(0));
    let dials_bg = dials.clone();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            dials_bg.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 32 * 1024];
                loop {
                    match s.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if s.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
    });
    (addr, dials)
}

/// A fake DarkPyonix manager: `POST /api/editor/{runtime}/launch` with the master token answers a
/// ticket, and `GET /ide/ose/` answers `editor-ok` only with the ticket cookie. Answers its address
/// and the number of launches.
async fn start_fake_manager() -> (SocketAddr, Arc<AtomicUsize>) {
    use axum::extract::Path as AxPath;
    use axum::http::HeaderMap;
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    let launches = Arc::new(AtomicUsize::new(0));
    let counted = launches.clone();
    let app = Router::new()
        .route(
            "/api/editor/{runtime}/launch",
            post(move |AxPath(runtime): AxPath<String>, headers: HeaderMap| {
                let counted = counted.clone();
                async move {
                    if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(&format!("Bearer {MANAGER_TOKEN}")) {
                        return (StatusCode::UNAUTHORIZED, "bad token").into_response();
                    }
                    counted.fetch_add(1, Ordering::SeqCst);
                    axum::Json(serde_json::json!({
                        "ticket": TICKET, "path": format!("/ide/{runtime}/"),
                        "commit": "0123456789abcdef0123456789abcdef01234567", "version": "1.139.1"
                    }))
                    .into_response()
                }
            }),
        )
        .route(
            "/ide/ose/",
            get(|headers: HeaderMap| async move {
                let cookie = headers.get("cookie").and_then(|v| v.to_str().ok()).unwrap_or("");
                if cookie.contains(&format!("dpx-editor={TICKET}")) {
                    (StatusCode::OK, "editor-ok").into_response()
                } else {
                    (StatusCode::UNAUTHORIZED, "no ticket").into_response()
                }
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, launches)
}

/// FR-W4c (D55, #1064): the local computer's Ember target and VS Code pick ask the manager of this
/// machine and answer `{relay, path, ticket, ose_commit}` with no endpoint, no token and never the
/// manager token; the relay of that launch reaches the manager's endpoint, which serves the editor
/// to a request carrying the ticket cookie.
#[tokio::test]
async fn fr_w4c_local_launch_asks_the_manager_and_relays_to_its_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let (manager_addr, launches) = start_fake_manager().await;
    let editor = Arc::new(EditorRelays::new(ManagerSource::fixed(format!("http://{manager_addr}"), MANAGER_TOKEN)));
    let h = start_harness(editor).await;
    let sid = h
        .store
        .create_session("proj", AgentKind::Scripted, &dir.path().to_string_lossy(), None, "t")
        .unwrap()
        .id;
    let call = |method: Method, uri: String| {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("host", h.addr.to_string())
            .header("authorization", format!("Bearer {}", h.token))
            .body(Body::empty())
            .unwrap()
    };
    let res = h.app.clone().oneshot(call(Method::GET, format!("/api/sessions/{sid}/ide"))).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["targets"][0]["available"], true, "{body}");
    let ose = body["targets"][0]["launch"]["ose"].clone();
    let mut keys: Vec<String> = ose.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["ose_commit", "path", "relay", "ticket"]);
    assert_eq!(ose["path"], "/ide/ose/");
    assert_eq!(ose["ticket"], TICKET);
    assert!(ose["relay"].as_str().unwrap().starts_with("/api/computers/local/ide/relay/"));
    assert!(!body.to_string().contains(MANAGER_TOKEN), "the manager token leaked: {body}");
    assert_eq!(body["targets"][1], serde_json::json!({ "kind": "vscode" }));
    assert_eq!(launches.load(Ordering::SeqCst), 1, "listing launched more than the Ember target");

    let res = h.app.clone().oneshot(call(Method::POST, format!("/api/sessions/{sid}/ide/vscode"))).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let pick: Value = serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(pick["ose"]["path"], "/ide/ose/");
    assert_eq!(pick["ose"]["ticket"], TICKET);
    assert!(pick["ose"].get("endpoint").is_none() && pick["ose"].get("connection_token").is_none(), "{pick}");
    assert_ne!(pick["ose"]["relay"], ose["relay"], "each launch is its own relay id");

    // Through the relay, an HTTP request with the ticket cookie reaches the manager's editor page.
    let url = format!("ws://{}{}", h.addr, pick["ose"]["relay"].as_str().unwrap());
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert("Authorization", format!("Bearer {}", h.token).parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let request = format!("GET /ide/ose/ HTTP/1.1\r\nHost: 127.0.0.1\r\nCookie: dpx-editor={TICKET}\r\nConnection: close\r\n\r\n");
    ws.send(Message::Binary(request.into_bytes().into())).await.unwrap();
    let mut answer = Vec::new();
    while !String::from_utf8_lossy(&answer).contains("editor-ok") {
        match tokio::time::timeout(Duration::from_secs(5), ws.next()).await.expect("no answer in time") {
            Some(Ok(Message::Binary(b))) => answer.extend_from_slice(&b),
            other => panic!("the relay ended before the page: {other:?} after {:?}", String::from_utf8_lossy(&answer)),
        }
    }
    assert!(String::from_utf8_lossy(&answer).starts_with("HTTP/1.1 200"), "{}", String::from_utf8_lossy(&answer));
}

#[tokio::test]
async fn fr_w4_relay_requires_server_token_on_the_local_listener() {
    let dir = tempfile::tempdir().unwrap();
    let ose = Arc::new(EditorRelays::new(ManagerSource::none()));
    let (ose_addr, _) = start_echo_ose().await;
    let conn = register(&ose, ose_addr);
    let relay_path = conn;
    let h = start_harness(ose).await;

    // 1. Missing token, wrong token, and guest token (dpg_) are all 401 before upgrade.
    for bad in [None, Some("Bearer wrong-token"), Some("Bearer dpg_guestsecret")] {
        let mut req = Request::builder()
            .method(Method::GET)
            .uri(&relay_path)
            .header("host", h.addr.to_string())
            .header("upgrade", "websocket")
            .header("connection", "Upgrade")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
            .header("sec-websocket-version", "13");
        if let Some(auth) = bad {
            req = req.header("authorization", auth);
        }
        let res = h.app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "auth={bad:?}");
    }

    // 2. Valid Authorization: Bearer <server-token> upgrades (101).
    let url = format!("ws://{}{relay_path}", h.addr);
    let mut req = url.clone().into_client_request().unwrap();
    req.headers_mut().insert("Authorization", format!("Bearer {}", h.token).parse().unwrap());
    let (mut ws, resp) = tokio_tungstenite::connect_async(req).await.expect("bearer upgrade");
    assert_eq!(resp.status(), StatusCode::SWITCHING_PROTOCOLS);
    let _ = ws.close(None).await;

    // 3. Valid Sec-WebSocket-Protocol: ember.token.<server-token>, ember.v1 upgrades (101) and selects ember.v1.
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        format!("ember.token.{}, ember.v1", h.token).parse().unwrap(),
    );
    let (mut ws, resp) = tokio_tungstenite::connect_async(req).await.expect("protocol token upgrade");
    assert_eq!(resp.status(), StatusCode::SWITCHING_PROTOCOLS);
    assert_eq!(
        resp.headers().get("sec-websocket-protocol").and_then(|v| v.to_str().ok()),
        Some("ember.v1")
    );
    let _ = ws.close(None).await;
}

#[tokio::test]
async fn fr_w4_relay_refuses_cross_origin_and_wrong_host() {
    let dir = tempfile::tempdir().unwrap();
    let ose = Arc::new(EditorRelays::new(ManagerSource::none()));
    let (ose_addr, dials) = start_echo_ose().await;
    let conn = register(&ose, ose_addr);
    let relay_path = conn;
    let h = start_harness(ose).await;
    let bearer = format!("Bearer {}", h.token);

    // Foreign Host -> 403 on protected router.
    let req = Request::builder()
        .method(Method::GET)
        .uri(&relay_path)
        .header("host", "evil.example.com")
        .header("authorization", &bearer)
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("sec-websocket-version", "13")
        .body(Body::empty())
        .unwrap();
    let res = h.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // Cross-origin on both the protected listener and the unwrapped transport listener -> 403.
    for target_addr in [h.addr, h.raw_addr] {
        for origin in ["https://evil.example.com", "null", "http://attacker.local:8080"] {
            let status = try_ws_status(target_addr, &relay_path, Some(&h.token), Some(origin)).await;
            assert_eq!(status, 403, "origin={origin}");
        }
    }
    assert_eq!(dials.load(Ordering::SeqCst), 0, "refused requests must never dial the OSE");
}

#[tokio::test]
async fn fr_w4_relay_unknown_launch_id_is_404_and_dials_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let ose = Arc::new(EditorRelays::new(ManagerSource::none()));
    let (ose_addr, dials) = start_echo_ose().await;
    let _conn = register(&ose, ose_addr);
    let h = start_harness(ose).await;

    for (path, want) in [
        ("/api/computers/local/ide/relay/nonexistent-launch-id", 404u16),
        ("/api/computers/unknown-box/ide/relay/nonexistent-launch-id", 404u16),
        ("/api/computers/a..b/ide/relay/nonexistent-launch-id", 400u16),
    ] {
        let status = try_ws_status(h.addr, path, Some(&h.token), None).await;
        assert_eq!(status, want, "{path}");
    }
    assert_eq!(dials.load(Ordering::SeqCst), 0, "unknown launch id must not dial the OSE");
}

#[tokio::test]
async fn fr_w4_relay_pumps_bytes_both_ways_and_preserves_order() {
    let dir = tempfile::tempdir().unwrap();
    let ose = Arc::new(EditorRelays::new(ManagerSource::none()));
    let (ose_addr, _) = start_echo_ose().await;
    let conn = register(&ose, ose_addr);
    let relay_path = conn;
    let h = start_harness(ose).await;

    let url = format!("ws://{}{relay_path}", h.addr);
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert("Authorization", format!("Bearer {}", h.token).parse().unwrap());
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    // Send 8 MiB across 64 KiB binary frames and read them back in exact order.
    let total = 8 * 1024 * 1024;
    let chunk_size = 64 * 1024;
    let mut expected = Vec::with_capacity(total);
    for i in 0..total {
        expected.push(((i * 31 + 7) & 0xff) as u8);
    }
    let to_send = expected.clone();
    let (mut sink, mut stream) = ws.split();
    let sender = tokio::spawn(async move {
        for chunk in to_send.chunks(chunk_size) {
            sink.send(Message::Binary(chunk.to_vec().into())).await.unwrap();
        }
        sink
    });
    let mut received = Vec::with_capacity(total);
    while received.len() < total {
        match tokio::time::timeout(Duration::from_secs(15), stream.next()).await.unwrap() {
            Some(Ok(Message::Binary(b))) => received.extend_from_slice(&b),
            other => panic!("unexpected frame at {} bytes: {other:?}", received.len()),
        }
    }
    assert_eq!(received.len(), expected.len());
    assert_eq!(received, expected);
    let mut sink = sender.await.unwrap();
    let _ = sink.close().await;
}

#[tokio::test]
async fn fr_w4_relay_backpressure_does_not_buffer_unboundedly() {
    // OSE server blasts bytes continuously while the WS client does not read; because the relay
    // awaits `ws_out.send(...).await` on each 32 KiB read chunk, the TCP socket backpressures and
    // the server stops being able to write after the socket + single-frame buffers fill.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ose_addr = listener.local_addr().unwrap();
    let written = Arc::new(AtomicUsize::new(0));
    let written_bg = written.clone();
    tokio::spawn(async move {
        if let Ok((mut s, _)) = listener.accept().await {
            let chunk = vec![0x5au8; 64 * 1024];
            while s.write_all(&chunk).await.is_ok() {
                written_bg.fetch_add(chunk.len(), Ordering::SeqCst);
            }
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let ose = Arc::new(EditorRelays::new(ManagerSource::none()));
    let conn = register(&ose, ose_addr);
    let h = start_harness(ose).await;

    let url = format!("ws://{}{}", h.addr, conn);
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert("Authorization", format!("Bearer {}", h.token).parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    // Wait 300 ms without reading from `ws`; an unbounded channel would accumulate hundreds of MiB.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let total_written = written.load(Ordering::SeqCst);
    assert!(
        total_written < 16 * 1024 * 1024,
        "backpressure failed: OSE wrote {total_written} bytes without the client reading"
    );
    let _ = ws.close(None).await;
}

#[tokio::test]
async fn fr_w4_relay_idle_timeout_closes_both_sides() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ose_addr = listener.local_addr().unwrap();
    let (tcp_closed_tx, tcp_closed_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        if let Ok((mut s, _)) = listener.accept().await {
            let mut buf = [0u8; 64];
            let _ = s.read(&mut buf).await;
            let _ = tcp_closed_tx.send(());
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let ose = Arc::new(EditorRelays::new(ManagerSource::none()).with_relay_idle_timeout(Duration::from_millis(250)));
    let conn = register(&ose, ose_addr);
    let h = start_harness(ose).await;

    let url = format!("ws://{}{}", h.addr, conn);
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert("Authorization", format!("Bearer {}", h.token).parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("WS did not close on idle timeout");
    assert!(
        matches!(msg, Some(Ok(Message::Close(_))) | Some(Err(_)) | None),
        "unexpected message: {msg:?}"
    );
    tokio::time::timeout(Duration::from_secs(2), tcp_closed_rx)
        .await
        .expect("TCP side did not close on idle timeout")
        .unwrap();
}

#[tokio::test]
async fn fr_w4_relay_limits_per_launch_and_per_server() {
    let dir = tempfile::tempdir().unwrap();
    let d1 = dir.path().join("p1");
    let d2 = dir.path().join("p2");
    std::fs::create_dir_all(&d1).unwrap();
    std::fs::create_dir_all(&d2).unwrap();
    // Limit: 2 per launch, 3 per server.
    let ose = Arc::new(EditorRelays::new(ManagerSource::none()).with_relay_limits(2, 3));
    let (ose_addr, _) = start_echo_ose().await;
    let c1 = register(&ose, ose_addr);
    let c2 = register(&ose, ose_addr);
    let h = start_harness(ose).await;

    let open_ws = |path: String| {
        let addr = h.addr;
        let token = h.token.clone();
        async move {
            let url = format!("ws://{addr}{path}");
            let mut req = url.into_client_request().unwrap();
            req.headers_mut().insert("Authorization", format!("Bearer {token}").parse().unwrap());
            tokio_tungstenite::connect_async(req).await
        }
    };

    let (mut ws1, _) = open_ws(c1.clone()).await.expect("1st on launch 1");
    let (mut ws2, _) = open_ws(c1.clone()).await.expect("2nd on launch 1");

    // 3rd on launch 1 exceeds per-launch limit (2) -> 429 before upgrade.
    let status = try_ws_status(h.addr, &c1, Some(&h.token), None).await;
    assert_eq!(status, 429, "per-launch cap");

    // Launch 2 can still open 1 stream (reaching the server cap of 3), then the next is 429.
    let (mut ws3, _) = open_ws(c2.clone()).await.expect("1st on launch 2 (3rd total)");
    let status = try_ws_status(h.addr, &c2, Some(&h.token), None).await;
    assert_eq!(status, 429, "per-server cap");

    // Closing ws1 releases the RAII permit so launch 1 can open again.
    let _ = ws1.close(None).await;
    drop(ws1);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Ok((mut ws_new, _)) = open_ws(c1.clone()).await {
            let _ = ws_new.close(None).await;
            break;
        }
        assert!(Instant::now() < deadline, "permit was not released after ws1 closed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let _ = ws2.close(None).await;
    let _ = ws3.close(None).await;
}

#[tokio::test]
async fn fr_w4_relay_closes_when_the_tcp_side_closes_and_when_the_ws_closes() {
    // Case 1: TCP side closes -> WS client sees Close/EOF.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ose_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((mut s, _)) = listener.accept().await {
            let _ = s.write_all(b"bye").await;
            let _ = s.shutdown().await;
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let ose = Arc::new(EditorRelays::new(ManagerSource::none()));
    let c1 = register(&ose, ose_addr);
    let h = start_harness(ose).await;

    let url = format!("ws://{}{}", h.addr, c1);
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert("Authorization", format!("Bearer {}", h.token).parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(2), ws.next()).await.unwrap().unwrap().unwrap();
    assert_eq!(first, Message::Binary(b"bye".to_vec().into()));
    let second = tokio::time::timeout(Duration::from_secs(2), ws.next()).await.unwrap();
    assert!(matches!(second, Some(Ok(Message::Close(_))) | None), "{second:?}");

    // Case 2: WS client closes -> TCP side sees EOF.
    let listener2 = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ose_addr2 = listener2.local_addr().unwrap();
    let (eof_tx, eof_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        if let Ok((mut s, _)) = listener2.accept().await {
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf).await;
            let _ = eof_tx.send(buf);
        }
    });
    let d2 = dir.path().join("p2");
    std::fs::create_dir_all(&d2).unwrap();
    let c2 = register(&h.editor, ose_addr2);
    let url2 = format!("ws://{}{}", h.addr, c2);
    let mut req2 = url2.into_client_request().unwrap();
    req2.headers_mut().insert("Authorization", format!("Bearer {}", h.token).parse().unwrap());
    let (mut ws2, _) = tokio_tungstenite::connect_async(req2).await.unwrap();
    ws2.send(Message::Binary(b"hello".to_vec().into())).await.unwrap();
    ws2.close(None).await.unwrap();
    let got = tokio::time::timeout(Duration::from_secs(2), eof_rx).await.unwrap().unwrap();
    assert_eq!(got, b"hello");

    // Case 3: Text frame from client is rejected with 1003.
    let (ose_addr3, _) = start_echo_ose().await;
    let d3 = dir.path().join("p3");
    std::fs::create_dir_all(&d3).unwrap();
    let c3 = register(&h.editor, ose_addr3);
    let url3 = format!("ws://{}{}", h.addr, c3);
    let mut req3 = url3.into_client_request().unwrap();
    req3.headers_mut().insert("Authorization", format!("Bearer {}", h.token).parse().unwrap());
    let (mut ws3, _) = tokio_tungstenite::connect_async(req3).await.unwrap();
    ws3.send(Message::Text("not allowed".into())).await.unwrap();
    let close_msg = tokio::time::timeout(Duration::from_secs(2), ws3.next()).await.unwrap();
    match close_msg {
        Some(Ok(Message::Close(Some(frame)))) => assert_eq!(u16::from(frame.code), 1003u16),
        other => panic!("expected 1003 close frame, got {other:?}"),
    }
}

/// Message boundaries are preserved exactly: N client messages in, N identical messages out
/// (`FR-N9.1`, #776), including messages that end with a prefix of a token-like value.
#[tokio::test]
async fn fr_w4_relay_preserves_client_message_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    let ose = Arc::new(EditorRelays::new(ManagerSource::none()));
    let (ose_addr, _) = start_echo_ose().await;
    let conn = register(&ose, ose_addr);
    let placeholder = "e".repeat(32) + &"f".repeat(32);
    let h = start_harness(ose).await;

    let url = format!("ws://{}{}", h.addr, conn);
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert("Authorization", format!("Bearer {}", h.token).parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    let p = placeholder.as_bytes();
    let mut messages: Vec<Vec<u8>> = vec![b"still-here".to_vec(), b"x".to_vec()];
    for k in [1usize, 2, 32, 63] {
        let mut m = b"tail:".to_vec();
        m.extend_from_slice(&p[..k]);
        messages.push(m);
        // The rest of a split placeholder, alone in the next message.
        messages.push(p[k..].to_vec());
    }
    // Every byte of the placeholder as the last byte of a message.
    for b in p {
        messages.push(vec![b'm', *b]);
    }

    // One at a time, so the echo OSE's TCP reads cannot coalesce two messages.
    for (n, m) in messages.iter().enumerate() {
        ws.send(Message::Binary(m.clone().into())).await.unwrap();
        let got = tokio::time::timeout(Duration::from_secs(2), ws.next()).await;
        match got {
            Ok(Some(Ok(Message::Binary(b)))) => assert_eq!(b.to_vec(), *m, "message {n} came back changed or split"),
            other => panic!("message {n} {m:?}: got {other:?}"),
        }
    }
    // Nothing extra arrives after the last echo.
    assert!(tokio::time::timeout(Duration::from_millis(300), ws.next()).await.is_err(), "an extra message arrived");
}

#[tokio::test]
async fn fr_n3_ide_relay_ends_on_revoke() {
    let net = MemNetwork::new();
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let ose = Arc::new(EditorRelays::new(ManagerSource::none()));
    let (ose_addr, _) = start_echo_ose().await;
    let conn = register(&ose, ose_addr);
    let relay_path = conn;

    let app = ember_server::api::router(sessions).merge(ide::router_with_editor(
        Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]),
        Arc::new(IdeConfig::default()),
        ose,
    ));
    let devices = Devices::open(store).unwrap();
    let server_t: Transport = net.transport();
    tokio::spawn(server_transport::serve(&server_t, app, devices.gate().clone()).unwrap());

    let (phone_t, laptop_t) = (net.transport(), net.transport());
    devices.add(phone_t.peer_id(), "phone").unwrap();
    devices.add(laptop_t.peer_id(), "laptop").unwrap();
    let (phone, laptop) = (Dialer::new(phone_t.clone()), Dialer::new(laptop_t.clone()));

    let open_device_ws = |dialer: &Dialer, server: &Transport, path: &str| {
        let dialer = dialer.clone();
        let peer = PeerAddr::new(server.peer_id());
        let url = format!("ws://ember-server{path}");
        async move {
            let stream: BiStream = dialer.open_bi(&peer, SERVER_SERVICE).await.map_err(|e| e.to_string())?;
            tokio_tungstenite::client_async(url, stream)
                .await
                .map(|(ws, _)| ws)
                .map_err(|e| e.to_string())
        }
    };

    let mut phone_ws: WebSocketStream<BiStream> = open_device_ws(&phone, &server_t, &relay_path).await.unwrap();
    let mut laptop_ws: WebSocketStream<BiStream> = open_device_ws(&laptop, &server_t, &relay_path).await.unwrap();

    // Verify both relays pump short frames before revocation.
    phone_ws.send(Message::Binary(b"ping-phone-client".to_vec().into())).await.unwrap();
    assert_eq!(phone_ws.next().await.unwrap().unwrap(), Message::Binary(b"ping-phone-client".to_vec().into()));
    laptop_ws.send(Message::Binary(b"ping-laptop-client".to_vec().into())).await.unwrap();
    assert_eq!(laptop_ws.next().await.unwrap().unwrap(), Message::Binary(b"ping-laptop-client".to_vec().into()));

    // Revoke phone: its relay ends within one heartbeat (5 s), while laptop's relay keeps working.
    let started = Instant::now();
    devices.remove(&phone_t.peer_id()).unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match phone_ws.next().await {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => continue,
            }
        }
    })
    .await;
    assert!(ended.is_ok(), "revoked device's IDE relay stayed open after 5s");
    assert!(started.elapsed() < Duration::from_secs(5));

    // A new relay from the revoked device is refused.
    assert!(open_device_ws(&phone, &server_t, &relay_path).await.is_err());

    // The second device keeps its relay.
    laptop_ws.send(Message::Binary(b"still-here".to_vec().into())).await.unwrap();
    assert_eq!(laptop_ws.next().await.unwrap().unwrap(), Message::Binary(b"still-here".to_vec().into()));
}

#[tokio::test]
async fn fr_t14_guest_door_does_not_serve_the_ide_relay() {
    let vars = std::collections::HashMap::from([("EMBER_GUEST_LISTEN".to_string(), "127.0.0.1:9000".to_string())]);
    let door = GuestDoor::from_lookup(|k| vars.get(k).cloned(), "127.0.0.1:8740".parse().unwrap())
        .unwrap()
        .expect("door on");
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let guest_app = door.wrap(guest_router(store, sessions));

    for method in ["GET", "POST", "OPTIONS"] {
        let req = Request::builder()
            .method(method)
            .uri("/api/computers/local/ide/relay/some-launch-id")
            .header("host", "127.0.0.1:9000")
            .header("upgrade", "websocket")
            .header("connection", "Upgrade")
            .body(Body::empty())
            .unwrap();
        let res = guest_app.clone().oneshot(req).await.unwrap();
        assert!(
            res.status() == StatusCode::NOT_FOUND
                || res.status() == StatusCode::BAD_REQUEST
                || res.status() == StatusCode::METHOD_NOT_ALLOWED,
            "{method} on guest door returned {}",
            res.status()
        );
    }
}
