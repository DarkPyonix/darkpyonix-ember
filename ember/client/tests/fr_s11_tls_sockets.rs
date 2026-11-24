//! FR-S11, #1048: the push and watch sockets reach an `https` server over `wss` (a TLS session
//! named after the host), and the watch socket re-reads the token once after a 401 (#369).

use std::sync::{Arc, Mutex};

use axum::extract::ws::WebSocketUpgrade;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use ember_client::api::{Api, ApiError};
use tokio::io::AsyncReadExt;

/// A TCP listener that records the first bytes a client sends, then closes.
async fn sniffer() -> (u16, Arc<Mutex<Vec<u8>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Arc<Mutex<Vec<u8>>> = Arc::default();
    let log = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut tcp, _)) = listener.accept().await {
            let mut buf = vec![0u8; 2048];
            let n = tcp.read(&mut buf).await.unwrap_or(0);
            log.lock().unwrap().extend_from_slice(&buf[..n]);
        }
    });
    (port, seen)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// With TLS built in, `wss` opens a TLS session: the first record is a handshake (0x16) whose
/// ClientHello names the host, instead of a plain HTTP upgrade or the old "only plain ws://"
/// refusal.
#[cfg(feature = "tls")]
#[tokio::test]
async fn fr_s11_push_and_watch_sockets_speak_tls_to_an_https_server() {
    for which in ["push", "watch"] {
        let (port, seen) = sniffer().await;
        let api = Api::with_token(&format!("https://localhost:{port}"), None).unwrap();
        let err = match which {
            "push" => api.open_push().await.err(),
            _ => api.watch_fs("local").await.err(),
        };
        let err = err.expect("the sniffer closes without a TLS answer");
        assert!(!err.to_string().contains("only plain ws://"), "{which}: {err}");
        let bytes = seen.lock().unwrap().clone();
        assert_eq!(bytes.first(), Some(&0x16), "{which}: the first record is a TLS handshake: {bytes:?}");
        assert!(contains(&bytes, b"localhost"), "{which}: the ClientHello names the host");
        assert!(!contains(&bytes, b"GET /api"), "{which}: no plain HTTP upgrade");
    }
}

/// Without TLS built in, a `wss` socket fails with a message that says so, before sending
/// anything in clear.
#[cfg(not(feature = "tls"))]
#[tokio::test]
async fn fr_s11_without_tls_a_wss_socket_says_so() {
    let (port, seen) = sniffer().await;
    let api = Api::with_token(&format!("https://localhost:{port}"), None).unwrap();
    match api.open_push().await {
        Err(ApiError::Push(m)) => assert!(m.contains("no TLS"), "{m}"),
        Err(e) => panic!("expected the no-TLS message, got {e:?}"),
        Ok(_) => panic!("expected the no-TLS message, got a socket"),
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(!contains(&seen.lock().unwrap(), b"GET /api"), "nothing in clear");
}

/// The watch socket of a server that accepts only `Bearer <current>`.
async fn watch_server(current: Arc<Mutex<String>>) -> (String, Arc<Mutex<Vec<Option<String>>>>) {
    let seen: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
    let log = seen.clone();
    let watch = move |headers: HeaderMap, ws: WebSocketUpgrade| {
        let (log, current) = (log.clone(), current.clone());
        async move {
            let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).map(str::to_string);
            log.lock().unwrap().push(auth.clone());
            if auth.as_deref() != Some(&format!("Bearer {}", current.lock().unwrap())) {
                let body = axum::Json(serde_json::json!({"error": "missing or invalid server token", "code": "unauthorized"}));
                return (StatusCode::UNAUTHORIZED, body).into_response();
            }
            ws.on_upgrade(|mut socket| async move { while let Some(Ok(_)) = socket.recv().await {} })
        }
    };
    let app = axum::Router::new().route("/api/computers/{id}/fs/watch", get(watch));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

/// A rotated token file is read again after the 401 and the socket opens on the second try; an
/// unchanged one is not retried.
#[tokio::test]
async fn fr_s11_the_watch_socket_rereads_a_rotated_token_once() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("server-token");
    std::fs::write(&file, "old").unwrap();
    let current = Arc::new(Mutex::new("new".to_string()));
    let (url, seen) = watch_server(current.clone()).await;
    let api = Api::with_token_file(&url, &file).unwrap();

    // Unchanged file: one attempt, Unauthorized.
    match api.watch_fs("local").await {
        Err(ApiError::Unauthorized(_)) => {}
        Err(e) => panic!("expected Unauthorized, got {e:?}"),
        Ok(_) => panic!("expected Unauthorized, got a socket"),
    }
    assert_eq!(seen.lock().unwrap().len(), 1, "an unchanged token is not retried");

    // Rotated file: the second attempt carries the new token and connects.
    std::fs::write(&file, "new").unwrap();
    assert!(api.watch_fs("local").await.is_ok(), "the rotated token is read again");
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen[1..], [Some("Bearer old".to_string()), Some("Bearer new".to_string())]);
}
