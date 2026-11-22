//! FR-S11, #982: a 401 on the push socket is an authentication failure, not a lost connection.
//! The push worker reads the token again once (#369) and stops the reconnect loop when the
//! server still refuses it, naming where the token was looked for.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::WebSocketUpgrade;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use ember_client::api::{Api, ApiError};
use ember_client::{Client, ClientConfig, ConnectionState};

/// What the fake server saw on `/api/push`: the `Authorization` header of every attempt.
type Seen = Arc<Mutex<Vec<Option<String>>>>;

/// A server whose push socket accepts only `Bearer <current>`; health, sessions and projects
/// answer without a token, so only the push socket decides.
async fn server(current: Arc<Mutex<String>>) -> (String, Seen) {
    let seen: Seen = Arc::default();
    let log = seen.clone();
    let push = move |headers: HeaderMap, ws: WebSocketUpgrade| {
        let (log, current) = (log.clone(), current.clone());
        async move {
            let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).map(str::to_string);
            log.lock().unwrap().push(auth.clone());
            if auth.as_deref() != Some(&format!("Bearer {}", current.lock().unwrap())) {
                let body = axum::Json(serde_json::json!({"error": "missing or invalid server token", "code": "unauthorized"}));
                return (StatusCode::UNAUTHORIZED, body).into_response();
            }
            ws.on_upgrade(|mut socket| async move {
                // Stay open until the client goes away.
                while let Some(Ok(_)) = socket.recv().await {}
            })
        }
    };
    let app = axum::Router::new()
        .route("/api/health", get(|| async { axum::Json(serde_json::json!({"ok": true})) }))
        .route("/api/sessions", get(|| async { axum::Json(serde_json::json!([])) }))
        .route("/api/projects", get(|| async { axum::Json(serde_json::json!([])) }))
        .route("/api/push", get(push))
        .fallback(|| async { StatusCode::NOT_FOUND });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

fn config(url: &str) -> ClientConfig {
    let mut c = ClientConfig::new(url);
    c.backoff_min = Duration::from_millis(20);
    c.backoff_max = Duration::from_millis(100);
    c.resync_settle = Duration::from_millis(50);
    c
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

fn token_file(dir: &std::path::Path, text: &str) -> std::path::PathBuf {
    let f = dir.join("server-token");
    std::fs::write(&f, text).unwrap();
    f
}

#[tokio::test]
async fn fr_s11_a_refused_push_upgrade_is_unauthorized() {
    let (url, seen) = server(Arc::new(Mutex::new("right".into()))).await;
    let api = Api::with_token(&url, Some("wrong".into())).unwrap();
    match api.open_push().await {
        Err(ApiError::Unauthorized(m)) => assert!(m.contains("server token"), "{m}"),
        Err(other) => panic!("a 401 upgrade must be Unauthorized, got {other:?}"),
        Ok(_) => panic!("the upgrade must be refused"),
    }
    assert_eq!(seen.lock().unwrap().as_slice(), &[Some("Bearer wrong".to_string())]);
}

#[tokio::test]
async fn fr_s11_push_401_with_an_unchanged_token_stops_retrying_and_names_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = token_file(dir.path(), "stale\n");
    let current = Arc::new(Mutex::new("other".to_string()));
    let (url, seen) = server(current.clone()).await;
    let api = Api::with_token_file(&url, &file).unwrap();
    let c = Client::with_api(config(&url), api).await;
    c.start();
    until("unauthorized", || c.read(|st| matches!(st.connection(), ConnectionState::Unauthorized { .. }))).await;
    // Many backoff periods later: still one attempt, no 30-second loop.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(seen.lock().unwrap().len(), 1, "the same token is never sent again");
    let state = c.read(|st| st.connection().clone());
    match state {
        ConnectionState::Unauthorized { error, looked_in } => {
            assert!(error.contains("server token"), "{error}");
            assert_eq!(looked_in, vec![file.display().to_string()]);
        }
        other => panic!("expected Unauthorized, got {other:?}"),
    }

    // The owner fixes the token and the app asks for a reconnect: it connects.
    *current.lock().unwrap() = "stale".into();
    c.reconnect_push();
    until("connected", || c.read(|st| *st.connection() == ConnectionState::Connected)).await;
    c.stop();
}

#[tokio::test]
async fn fr_s11_push_401_reads_a_rotated_token_once_and_connects() {
    let dir = tempfile::tempdir().unwrap();
    let file = token_file(dir.path(), "old\n");
    let (url, seen) = server(Arc::new(Mutex::new("new".to_string()))).await;
    let api = Api::with_token_file(&url, &file).unwrap();
    // The server rotated the token after the app read it (#369).
    std::fs::write(&file, "new\n").unwrap();
    let c = Client::with_api(config(&url), api).await;
    c.start();
    until("connected", || c.read(|st| *st.connection() == ConnectionState::Connected)).await;
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        &[Some("Bearer old".to_string()), Some("Bearer new".to_string())],
        "one 401, one re-read, one retry"
    );
    c.stop();
}
