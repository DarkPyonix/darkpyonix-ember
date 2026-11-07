//! SPEC FR-W4, FR-N3, FR-T14 (#458, #461): authenticated WebSocket relay for the OSE socket
//! (`GET /api/computers/{computer}/ide/relay/{launch_id}`) and C2 placeholder-to-real-token
//! substitution in the first 8 KiB of the client-to-OSE stream.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::api::ide::{self, IdeConfig, RELAY_SUBSTITUTION_WINDOW};
use ember_server::auth::{self, ServerAuth};
use ember_server::channels::guest::guest_router;
use ember_server::computers::{self, Computers, Registry};
use ember_server::devices::Devices;
use ember_server::guest_door::GuestDoor;
use ember_server::ose::OseManager;
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

const REAL_TOKEN: &str = "1111111111111111222222222222222233333333333333334444444444444444";

struct RelayHarness {
    addr: SocketAddr,
    raw_addr: SocketAddr,
    token: String,
    ose: Arc<OseManager>,
    store: Arc<Store>,
    app: Router,
    _dir: tempfile::TempDir,
}

async fn start_harness(ose: Arc<OseManager>) -> RelayHarness {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let computers = Computers::new(Registry::new(store.clone()), computers::default_connector());
    let raw_app = ide::router_with_computers_and_ose(
        sessions,
        Arc::new(IdeConfig::default()),
        computers,
        ose.clone(),
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
    RelayHarness { addr, raw_addr, token, ose, store, app, _dir: dir }
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

/// Spawns a fake OSE TCP echo server and returns `(address, connection_counter)`.
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

#[tokio::test]
async fn fr_w4_ide_json_has_relay_and_placeholder_not_the_real_token() {
    let dir = tempfile::tempdir().unwrap();
    let ose = Arc::new(OseManager::with_data_root(None, dir.path().join("ose-data")));
    let (ose_addr, _) = start_echo_ose().await;
    let conn = ose.register_launch_for_test("local", dir.path(), ose_addr, REAL_TOKEN);
    assert_eq!(conn.connection_token.len(), 64);
    assert_ne!(conn.connection_token, REAL_TOKEN, "JSON must carry a placeholder, never the real token");
    assert!(conn.relay.as_deref().unwrap_or("").starts_with("/api/computers/local/ide/relay/"));

    let h = start_harness(ose).await;
    let sid = h
        .store
        .create_session("proj", AgentKind::Scripted, &dir.path().to_string_lossy(), None, "t")
        .unwrap()
        .id;
    let req = Request::builder()
        .method(Method::GET)
        .uri(format!("/api/sessions/{sid}/ide"))
        .header("host", h.addr.to_string())
        .header("authorization", format!("Bearer {}", h.token))
        .body(Body::empty())
        .unwrap();
    let res = h.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let ose_json = &body["targets"][0]["launch"]["ose"];
    assert_eq!(ose_json["relay"], conn.relay.as_deref().unwrap());
    assert_eq!(ose_json["connection_token"], conn.connection_token);
    assert_ne!(ose_json["connection_token"], REAL_TOKEN);
    assert!(!body.to_string().contains(REAL_TOKEN), "real token leaked in /ide JSON: {body}");
}

#[tokio::test]
async fn fr_w4_relay_requires_server_token_on_the_local_listener() {
    let dir = tempfile::tempdir().unwrap();
    let ose = Arc::new(OseManager::with_data_root(None, dir.path().join("ose-data")));
    let (ose_addr, _) = start_echo_ose().await;
    let conn = ose.register_launch_for_test("local", dir.path(), ose_addr, REAL_TOKEN);
    let relay_path = conn.relay.unwrap();
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
    let ose = Arc::new(OseManager::with_data_root(None, dir.path().join("ose-data")));
    let (ose_addr, dials) = start_echo_ose().await;
    let conn = ose.register_launch_for_test("local", dir.path(), ose_addr, REAL_TOKEN);
    let relay_path = conn.relay.unwrap();
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
    let ose = Arc::new(OseManager::with_data_root(None, dir.path().join("ose-data")));
    let (ose_addr, dials) = start_echo_ose().await;
    let _conn = ose.register_launch_for_test("local", dir.path(), ose_addr, REAL_TOKEN);
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
    let ose = Arc::new(OseManager::with_data_root(None, dir.path().join("ose-data")));
    let (ose_addr, _) = start_echo_ose().await;
    let conn = ose.register_launch_for_test("local", dir.path(), ose_addr, REAL_TOKEN);
    let relay_path = conn.relay.unwrap();
    let h = start_harness(ose).await;

    let url = format!("ws://{}{relay_path}", h.addr);
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert("Authorization", format!("Bearer {}", h.token).parse().unwrap());
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    // Send 8 MiB across 64 KiB binary frames (padded past RELAY_SUBSTITUTION_WINDOW first so the
    // last 63 bytes of the window are flushed immediately) and read them back in exact order.
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
    let ose = Arc::new(OseManager::with_data_root(None, dir.path().join("ose-data")));
    let conn = ose.register_launch_for_test("local", dir.path(), ose_addr, REAL_TOKEN);
    let h = start_harness(ose).await;

    let url = format!("ws://{}{}", h.addr, conn.relay.unwrap());
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
    let ose = Arc::new(
        OseManager::with_data_root(None, dir.path().join("ose-data"))
            .with_relay_idle_timeout(Duration::from_millis(250)),
    );
    let conn = ose.register_launch_for_test("local", dir.path(), ose_addr, REAL_TOKEN);
    let h = start_harness(ose).await;

    let url = format!("ws://{}{}", h.addr, conn.relay.unwrap());
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
    let ose = Arc::new(
        OseManager::with_data_root(None, dir.path().join("ose-data")).with_relay_limits(2, 3),
    );
    let (ose_addr, _) = start_echo_ose().await;
    let c1 = ose.register_launch_for_test("local", &d1, ose_addr, REAL_TOKEN);
    let c2 = ose.register_launch_for_test("local", &d2, ose_addr, REAL_TOKEN);
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

    let (mut ws1, _) = open_ws(c1.relay.clone().unwrap()).await.expect("1st on launch 1");
    let (mut ws2, _) = open_ws(c1.relay.clone().unwrap()).await.expect("2nd on launch 1");

    // 3rd on launch 1 exceeds per-launch limit (2) -> 429 before upgrade.
    let status = try_ws_status(h.addr, c1.relay.as_ref().unwrap(), Some(&h.token), None).await;
    assert_eq!(status, 429, "per-launch cap");

    // Launch 2 can still open 1 stream (reaching the server cap of 3), then the next is 429.
    let (mut ws3, _) = open_ws(c2.relay.clone().unwrap()).await.expect("1st on launch 2 (3rd total)");
    let status = try_ws_status(h.addr, c2.relay.as_ref().unwrap(), Some(&h.token), None).await;
    assert_eq!(status, 429, "per-server cap");

    // Closing ws1 releases the RAII permit so launch 1 can open again.
    let _ = ws1.close(None).await;
    drop(ws1);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Ok((mut ws_new, _)) = open_ws(c1.relay.clone().unwrap()).await {
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
    let ose = Arc::new(OseManager::with_data_root(None, dir.path().join("ose-data")));
    let c1 = ose.register_launch_for_test("local", dir.path(), ose_addr, REAL_TOKEN);
    let h = start_harness(ose).await;

    let url = format!("ws://{}{}", h.addr, c1.relay.unwrap());
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
    let c2 = h.ose.register_launch_for_test("local", &d2, ose_addr2, REAL_TOKEN);
    let url2 = format!("ws://{}{}", h.addr, c2.relay.unwrap());
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
    let c3 = h.ose.register_launch_for_test("local", &d3, ose_addr3, REAL_TOKEN);
    let url3 = format!("ws://{}{}", h.addr, c3.relay.unwrap());
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

#[tokio::test]
async fn fr_w4_relay_substitutes_the_placeholder_in_the_first_window() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ose_addr = listener.local_addr().unwrap();
    let (got_tx, got_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        if let Ok((mut s, _)) = listener.accept().await {
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf).await;
            let _ = got_tx.send(buf);
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let ose = Arc::new(OseManager::with_data_root(None, dir.path().join("ose-data")));
    let conn = ose.register_launch_for_test("local", dir.path(), ose_addr, REAL_TOKEN);
    let placeholder = conn.connection_token.clone();
    assert_eq!(placeholder.len(), 64);
    assert_ne!(placeholder, REAL_TOKEN);
    let h = start_harness(ose).await;

    let url = format!("ws://{}{}", h.addr, conn.relay.unwrap());
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert("Authorization", format!("Bearer {}", h.token).parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    // 1. Send `auth` message with placeholder intact in one frame.
    let prefix = b"{\"type\":\"auth\",\"auth\":\"";
    let mid = b"\",\"signedData\":\"";
    let suffix = b"\"}";
    let mut first_part = Vec::new();
    first_part.extend_from_slice(prefix);
    first_part.extend_from_slice(placeholder.as_bytes());
    first_part.extend_from_slice(mid);
    // 2. Split the second occurrence (`signedData`) across two binary frames (25 bytes + 39 bytes)!
    first_part.extend_from_slice(&placeholder.as_bytes()[..25]);
    ws.send(Message::Binary(first_part.clone().into())).await.unwrap();

    let mut second_part = Vec::new();
    second_part.extend_from_slice(&placeholder.as_bytes()[25..]);
    second_part.extend_from_slice(suffix);
    // Also include a wrong 64-hex string inside the window: must pass untouched.
    let wrong_tok = b"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
    second_part.extend_from_slice(wrong_tok);
    ws.send(Message::Binary(second_part.clone().into())).await.unwrap();

    // 3. Pad to the end of RELAY_SUBSTITUTION_WINDOW (8192 bytes), then send the placeholder
    // AFTER the window: bytes after 8 KiB must pass through untouched!
    let sent_so_far = first_part.len() + second_part.len();
    let pad_len = RELAY_SUBSTITUTION_WINDOW - sent_so_far;
    ws.send(Message::Binary(vec![b'x'; pad_len].into())).await.unwrap();
    ws.send(Message::Binary(placeholder.as_bytes().to_vec().into())).await.unwrap();
    ws.close(None).await.unwrap();

    let received = tokio::time::timeout(Duration::from_secs(3), got_rx).await.unwrap().unwrap();
    let mut expected = Vec::new();
    expected.extend_from_slice(prefix);
    expected.extend_from_slice(REAL_TOKEN.as_bytes());
    expected.extend_from_slice(mid);
    expected.extend_from_slice(REAL_TOKEN.as_bytes());
    expected.extend_from_slice(suffix);
    expected.extend_from_slice(wrong_tok);
    expected.extend_from_slice(&vec![b'x'; pad_len]);
    // After 8 KiB, placeholder is NOT substituted:
    expected.extend_from_slice(placeholder.as_bytes());

    assert_eq!(received.len(), expected.len());
    assert_eq!(received, expected);
    assert!(
        !std::str::from_utf8(&received[..RELAY_SUBSTITUTION_WINDOW]).unwrap().contains(&placeholder),
        "placeholder must never reach the OSE inside the substitution window"
    );
}

#[tokio::test]
async fn fr_n3_ide_relay_ends_on_revoke() {
    let net = MemNetwork::new();
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let ose = Arc::new(OseManager::with_data_root(None, dir.path().join("ose-data")));
    let (ose_addr, _) = start_echo_ose().await;
    let conn = ose.register_launch_for_test("local", dir.path(), ose_addr, REAL_TOKEN);
    let relay_path = conn.relay.unwrap();

    let app = ember_server::api::router(sessions).merge(ide::router_with_ose(
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
    phone_ws.send(Message::Binary(b"ping-phone".to_vec().into())).await.unwrap();
    assert_eq!(phone_ws.next().await.unwrap().unwrap(), Message::Binary(b"ping-phone".to_vec().into()));
    laptop_ws.send(Message::Binary(b"ping-laptop".to_vec().into())).await.unwrap();
    assert_eq!(laptop_ws.next().await.unwrap().unwrap(), Message::Binary(b"ping-laptop".to_vec().into()));

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
