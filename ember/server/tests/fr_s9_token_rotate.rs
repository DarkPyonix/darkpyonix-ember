//! FR-S9, #369: rotating the server token after a device is revoked, and the push task that
//! ends as soon as its socket closes.
//!
//! The router is wrapped the way `main.rs` wraps the local listener: `ServerAuth::with_token_file`
//! plus `auth::rotate_router`, inside `auth::protect`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::AgentAdapter;
use ember_server::auth::{self, ServerAuth};
use ember_server::session::Sessions;
use ember_server::store::Store;
use futures::SinkExt;
use http_body_util::BodyExt;
use serde_json::Value;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tower::ServiceExt;

const LISTEN: &str = "127.0.0.1:8740";

fn sessions() -> Arc<Sessions> {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    Sessions::new(store, adapters)
}

fn app(dir: &std::path::Path, listen: std::net::SocketAddr, sessions: Arc<Sessions>) -> Router {
    let a = ServerAuth::with_token_file(dir, listen, []).unwrap();
    let inner = ember_server::api::router(sessions).merge(auth::rotate_router(a.clone()));
    auth::protect(inner, a)
}

async fn call(app: &Router, method: Method, path: &str, token: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("host", LISTEN)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    let status = r.status();
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn file_token(dir: &std::path::Path) -> String {
    std::fs::read_to_string(auth::token_path(dir)).unwrap().lines().next().unwrap().trim().to_string()
}

#[tokio::test]
async fn fr_s9_rotate_route_refuses_the_old_token_and_accepts_the_new_one() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path(), LISTEN.parse().unwrap(), sessions());
    let old = file_token(dir.path());
    assert_eq!(call(&app, Method::GET, "/api/sessions", &old).await.0, StatusCode::OK);

    // Without the current token the route is closed like every other one.
    assert_eq!(call(&app, Method::POST, auth::ROTATE_ROUTE, "nope").await.0, StatusCode::UNAUTHORIZED);

    let (st, body) = call(&app, Method::POST, auth::ROTATE_ROUTE, &old).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["rotated"], true);
    let new = file_token(dir.path());
    assert_ne!(new, old);
    // The answer never carries the token: a client re-reads the file.
    assert!(!body.to_string().contains(&new));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(auth::token_path(dir.path())).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    assert_eq!(call(&app, Method::GET, "/api/sessions", &old).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(call(&app, Method::GET, "/api/sessions", &new).await.0, StatusCode::OK);
    // The old token stays refused (no file re-read brings it back).
    assert_eq!(call(&app, Method::GET, "/api/sessions", &old).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn fr_s9_cli_rotation_takes_effect_on_a_running_server() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path(), LISTEN.parse().unwrap(), sessions());
    let old = file_token(dir.path());
    assert_eq!(call(&app, Method::GET, "/api/sessions", &old).await.0, StatusCode::OK);

    // `ember-server rotate-token` from another process: only the file changes. Wait a moment so
    // the new file's modification time differs on file systems with coarse timestamps.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let new = auth::rotate_token(dir.path()).unwrap();
    assert_eq!(file_token(dir.path()), new);

    assert_eq!(call(&app, Method::GET, "/api/sessions", &new).await.0, StatusCode::OK);
    assert_eq!(call(&app, Method::GET, "/api/sessions", &old).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn fr_s9_push_task_ends_when_the_socket_closes() {
    let s = sessions();
    let dir = tempfile::tempdir().unwrap();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let app = app(dir.path(), addr, s.clone());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let token = file_token(dir.path());

    let base = s.push_receiver_count();
    let mut req = format!("ws://{addr}/api/push").into_client_request().unwrap();
    req.headers_mut().insert("authorization", format!("Bearer {token}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    assert_eq!(s.push_receiver_count(), base + 1, "one receiver per open socket");

    // The client closes and no push is ever sent: the task must still end at once.
    ws.close(None).await.unwrap();
    drop(ws);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while s.push_receiver_count() != base {
        assert!(tokio::time::Instant::now() < deadline, "push receiver lingers after close");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    // Same for a connection that just drops (no close frame).
    let mut req = format!("ws://{addr}/api/push").into_client_request().unwrap();
    req.headers_mut().insert("authorization", format!("Bearer {token}").parse().unwrap());
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    assert_eq!(s.push_receiver_count(), base + 1);
    drop(ws);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while s.push_receiver_count() != base {
        assert!(tokio::time::Instant::now() < deadline, "push receiver lingers after a dropped connection");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}
