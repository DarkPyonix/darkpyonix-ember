//! SPEC FR-N7: a remote device's credential is its device key. A request that arrives through
//! the transport is authenticated by the gate's peer identity (`PeerGate`, FR-N3), not by the
//! local token (FR-S9). The two doors differ on purpose: the local TCP listener wraps the router
//! in `auth::protect`; `server_transport::serve` gets the router unwrapped.
//!
//! The revocation timing itself is FR-N3's (`fr_n3_revocation_bound.rs`); this file asserts only
//! that the bound holds for a route call and that calls fail afterwards.

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::AgentAdapter;
use ember_server::auth::{self, ServerAuth};
use ember_server::devices::Devices;
use ember_server::session::Sessions;
use ember_server::store::Store;
use ember_server::transport::{self as server_transport, SERVER_SERVICE};
use ember_transport::http::http1_handshake;
use ember_transport::mem::MemNetwork;
use ember_transport::{Dialer, PeerAddr, Transport};
use http_body_util::{BodyExt, Full};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// One heartbeat, the bound FR-N3 gives.
const HEARTBEAT: Duration = Duration::from_secs(5);
/// A route that is neither exempt from the token nor special: it lists projects.
const NORMAL_ROUTE: &str = "/api/projects";

fn router() -> (axum::Router, Arc<Store>) {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    (ember_server::api::router(sessions), store)
}

/// A server on `net` that serves the plain router to the devices in the returned allow-list.
fn serve_server(net: &MemNetwork) -> (Transport, Arc<Devices>) {
    let (app, store) = router();
    let devices = Devices::open(store).unwrap();
    let server_t = net.transport();
    tokio::spawn(server_transport::serve(&server_t, app, devices.gate().clone()).unwrap());
    (server_t, devices)
}

/// GET `path` through the transport as a device. No `Authorization` header is sent: the device
/// key is the credential. `Err` when there is no connection or no answer.
async fn device_get(device: &Dialer, server: &Transport, path: &str) -> Result<(StatusCode, Vec<u8>), String> {
    let fut = async {
        let stream = device
            .open_bi(&PeerAddr::new(server.peer_id()), SERVER_SERVICE)
            .await
            .map_err(|e| e.to_string())?;
        let mut http = http1_handshake::<Full<hyper::body::Bytes>>(stream).await.map_err(|e| e.to_string())?;
        let req = hyper::Request::get(format!("http://ember-server{path}")).body(Full::default()).unwrap();
        let resp = http.send_request(req).await.map_err(|e| e.to_string())?;
        let status = resp.status();
        let body = resp.into_body().collect().await.map_err(|e| e.to_string())?.to_bytes();
        Ok((status, body.to_vec()))
    };
    tokio::time::timeout(Duration::from_secs(5), fut).await.map_err(|_| "timed out".to_string())?
}

/// Serves `app` wrapped by `auth::protect` on a local TCP listener, as `main.rs` does. Returns
/// the address and the token.
async fn serve_local(app: axum::Router) -> (std::net::SocketAddr, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let token = auth::load_or_create_token(dir.path()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = auth::protect(app, ServerAuth::new(token.clone(), addr, []));
    tokio::spawn(async move { axum::serve(listener, app).await });
    (addr, token, dir)
}

/// A bare HTTP/1.1 GET over TCP; the status code of the answer.
async fn tcp_get(addr: std::net::SocketAddr, path: &str, bearer: Option<&str>) -> u16 {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let auth = bearer.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    let req = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n{auth}Connection: close\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut out)).await.expect("no answer").unwrap();
    let text = String::from_utf8_lossy(&out);
    text.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or_else(|| panic!("no status line: {text}"))
}

/// An admitted device calls a normal route through the transport with no local token.
#[tokio::test]
async fn fr_n7_admitted_device_calls_a_route_without_the_local_token() {
    let net = MemNetwork::new();
    let (server_t, devices) = serve_server(&net);
    let phone_t = net.transport();
    devices.add(phone_t.peer_id(), "phone").unwrap();

    let phone = Dialer::new(phone_t);
    let (status, body) = device_get(&phone, &server_t, NORMAL_ROUTE).await.expect("admitted device got no answer");
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    serde_json::from_slice::<serde_json::Value>(&body).expect("a JSON body");
}

/// A peer the gate does not admit gets no connection, so no route answers it.
#[tokio::test]
async fn fr_n7_device_the_gate_does_not_admit_gets_no_connection() {
    let net = MemNetwork::new();
    let (server_t, devices) = serve_server(&net);
    let (known_t, stranger_t) = (net.transport(), net.transport());
    devices.add(known_t.peer_id(), "phone").unwrap();

    let stranger = Dialer::new(stranger_t);
    for path in [NORMAL_ROUTE, "/api/health", "/api/sessions"] {
        assert!(
            device_get(&stranger, &server_t, path).await.is_err(),
            "a peer that is not on the allow-list got an answer for {path}"
        );
    }
    // Admitting the other device does not admit this one, and the admitted one still works.
    let (status, _) = device_get(&Dialer::new(known_t), &server_t, NORMAL_ROUTE).await.unwrap();
    assert_eq!(status, StatusCode::OK);
}

/// The same router behind `auth::protect` on a local listener needs the token: the two doors
/// differ as FR-N7 specifies. The transport door answers the same route without one.
#[tokio::test]
async fn fr_n7_local_door_needs_the_token_and_the_transport_door_does_not() {
    let net = MemNetwork::new();
    let (app, store) = router();
    let devices = Devices::open(store).unwrap();
    let server_t = net.transport();
    tokio::spawn(server_transport::serve(&server_t, app.clone(), devices.gate().clone()).unwrap());
    let (addr, token, _dir) = serve_local(app).await;

    // Local TCP: 401 without the token, also with a wrong one; 200 with the right one.
    assert_eq!(tcp_get(addr, NORMAL_ROUTE, None).await, 401);
    assert_eq!(tcp_get(addr, NORMAL_ROUTE, Some("wrong")).await, 401);
    assert_eq!(tcp_get(addr, NORMAL_ROUTE, Some(&token)).await, 200);

    // Transport: the device key is enough.
    let phone_t = net.transport();
    devices.add(phone_t.peer_id(), "phone").unwrap();
    let (status, _) = device_get(&Dialer::new(phone_t), &server_t, NORMAL_ROUTE).await.unwrap();
    assert_eq!(status, StatusCode::OK);
}

/// An admitted device that is then revoked is dropped within one heartbeat and its later route
/// calls fail. The tight timing (one second on the default transport) is FR-N3's, asserted in
/// `fr_n3_revocation_bound.rs`; here only the heartbeat bound is.
#[tokio::test]
async fn fr_n7_revoked_device_is_dropped_within_one_heartbeat_and_cannot_call_again() {
    let net = MemNetwork::new();
    let (server_t, devices) = serve_server(&net);
    let phone_t = net.transport();
    devices.add(phone_t.peer_id(), "phone").unwrap();
    let phone = Dialer::new(phone_t.clone());

    let (status, _) = device_get(&phone, &server_t, NORMAL_ROUTE).await.expect("admitted");
    assert_eq!(status, StatusCode::OK);
    let conn = phone.connection(&PeerAddr::new(server_t.peer_id()), SERVER_SERVICE).await.unwrap();

    let start = std::time::Instant::now();
    assert_eq!(devices.remove(&phone_t.peer_id()).unwrap(), 1);
    tokio::time::timeout(Duration::from_secs(30), conn.closed()).await.expect("connection never closed");
    assert!(start.elapsed() < HEARTBEAT, "dropped after {:?}", start.elapsed());

    for path in [NORMAL_ROUTE, "/api/health"] {
        assert!(device_get(&phone, &server_t, path).await.is_err(), "a revoked device still called {path}");
    }
}

/// Over the real letify localhost transport (`FR-N7` acceptance: "the letify localhost test
/// of #229 extended to a route call"): an admitted device calls `/api/projects` without the local
/// token, revoking the device closes its letify session within one second, and later route calls
/// fail.
#[tokio::test(flavor = "multi_thread")]
async fn fr_n7_letify_localhost_route_call_and_revocation() {
    use ember_transport::{BackendKind, LetifyConfig, MemoryDirectory, SecretKey, TransportConfig};

    let dir = MemoryDirectory::new();
    let bind_letify = |dir: &MemoryDirectory| {
        let cfg = TransportConfig::new(SecretKey::generate())
            .backend(BackendKind::Letify)
            .bind_addr("127.0.0.1:0".parse().unwrap())
            .directory(Arc::new(dir.clone()))
            .letify(LetifyConfig {
                connect_timeout: Duration::from_secs(3),
                ..LetifyConfig::default()
            });
        Transport::bind(cfg)
    };
    let (server_t, phone_t, stranger_t) = (
        bind_letify(&dir).await.unwrap(),
        bind_letify(&dir).await.unwrap(),
        bind_letify(&dir).await.unwrap(),
    );
    let (app, store) = router();
    let devices = Devices::open(store).unwrap();
    devices.gate().attach(&server_t);
    let task = tokio::spawn(server_transport::serve(&server_t, app, devices.gate().clone()).unwrap());

    devices.add(phone_t.peer_id(), "phone").unwrap();
    let phone = Dialer::new(phone_t.clone());
    let stranger = Dialer::new(stranger_t.clone());

    // Stranger is refused on the letify transport.
    assert!(device_get(&stranger, &server_t, NORMAL_ROUTE).await.is_err());

    // Admitted device calls `/api/projects` over the letify transport without a bearer token.
    let (status, body) = device_get(&phone, &server_t, NORMAL_ROUTE).await.expect("letify route call failed");
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    serde_json::from_slice::<serde_json::Value>(&body).expect("a JSON body");

    // Revoke: the letify session closes within 1 s (`FR-N3`) and subsequent route calls fail.
    let conn = phone.connection(&PeerAddr::new(server_t.peer_id()), SERVER_SERVICE).await.unwrap();
    let start = std::time::Instant::now();
    assert_eq!(devices.remove(&phone_t.peer_id()).unwrap(), 1);
    tokio::time::timeout(Duration::from_secs(2), conn.closed())
        .await
        .expect("letify connection did not close on revoke");
    assert!(start.elapsed() < Duration::from_secs(1), "closed after {:?}", start.elapsed());
    assert!(device_get(&phone, &server_t, NORMAL_ROUTE).await.is_err());

    phone_t.close().await;
    stranger_t.close().await;
    server_t.close().await;
    task.abort();
}

// ---- FR-N8: per-device scope on the transport door (#366) --------------------------------
// Named `fr_n7_scope_*` so the existing `fr_n7_*` CI filter runs them.

use ember_server::devices::Scope;

/// A server whose transport door enforces device scopes, as `main.rs` serves it.
fn serve_scoped_server(net: &MemNetwork) -> (Transport, Arc<Devices>) {
    let (app, store) = router();
    let devices = Devices::open(store).unwrap();
    let server_t = net.transport();
    tokio::spawn(server_transport::serve_scoped(&server_t, app, devices.clone()).unwrap());
    (server_t, devices)
}

/// A device with `scope`, dialing `server_t`.
fn device_with(net: &MemNetwork, devices: &Devices, scope: Scope) -> (Dialer, ember_transport::PeerId) {
    let t = net.transport();
    let peer = t.peer_id();
    devices.add(peer, "phone").unwrap();
    devices.set_scope(&peer, scope).unwrap();
    (Dialer::new(t), peer)
}

/// `method path` with an optional JSON body through the transport as a device.
async fn device_call(
    device: &Dialer,
    server: &Transport,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let fut = async {
        let stream = device.open_bi(&PeerAddr::new(server.peer_id()), SERVER_SERVICE).await.unwrap();
        let mut http = http1_handshake::<Full<hyper::body::Bytes>>(stream).await.unwrap();
        let bytes = body.map(|b| b.to_string()).unwrap_or_default();
        let req = hyper::Request::builder()
            .method(method)
            .uri(format!("http://ember-server{path}"))
            .header("content-type", "application/json")
            .body(Full::new(hyper::body::Bytes::from(bytes)))
            .unwrap();
        let resp = http.send_request(req).await.unwrap();
        let status = resp.status();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null))
    };
    tokio::time::timeout(Duration::from_secs(5), fut).await.expect("timed out")
}

fn assert_denied(got: &(StatusCode, serde_json::Value), what: &str) {
    assert_eq!(got.0, StatusCode::FORBIDDEN, "{what}: {:?}", got.1);
    assert_eq!(got.1["code"], "scope_denied", "{what}: {:?}", got.1);
}

fn assert_not_denied(got: &(StatusCode, serde_json::Value), what: &str) {
    assert!(
        !(got.0 == StatusCode::FORBIDDEN && got.1["code"] == "scope_denied"),
        "{what} was refused by the scope: {:?}",
        got.1
    );
}

fn new_session() -> serde_json::Value {
    serde_json::json!({ "agent": "scripted", "cwd": "/tmp" })
}

/// Rows from before the migration, and new rows, are `full`.
#[tokio::test]
async fn fr_n7_scope_defaults_to_full_and_full_is_unchanged() {
    let net = MemNetwork::new();
    let (server_t, devices) = serve_scoped_server(&net);
    let t = net.transport();
    devices.add(t.peer_id(), "laptop").unwrap();
    assert_eq!(devices.scope_of(&t.peer_id()), Some(Scope::Full));
    let laptop = Dialer::new(t);

    let got = device_call(&laptop, &server_t, "GET", NORMAL_ROUTE, None).await;
    assert_eq!(got.0, StatusCode::OK);
    let got = device_call(&laptop, &server_t, "POST", "/api/sessions", Some(new_session())).await;
    assert_not_denied(&got, "full: POST /api/sessions");
}

/// `read_only` gets GETs and is refused on every state-changing route and on shells and tokens.
#[tokio::test]
async fn fr_n7_scope_read_only_allows_gets_and_refuses_changes() {
    let net = MemNetwork::new();
    let (server_t, devices) = serve_scoped_server(&net);
    let (d, _) = device_with(&net, &devices, Scope::ReadOnly);

    for path in ["/api/health", "/api/sessions", NORMAL_ROUTE] {
        let got = device_call(&d, &server_t, "GET", path, None).await;
        assert_eq!(got.0, StatusCode::OK, "GET {path}: {:?}", got.1);
    }
    for (m, path) in [
        ("POST", "/api/sessions"),
        ("PATCH", "/api/sessions/s1"),
        ("POST", "/api/sessions/s1/messages"),
        ("POST", "/api/projects"),
        ("DELETE", "/api/projects/p"),
        ("POST", "/api/computers/local/terms"),
        ("GET", "/api/computers/local/terms/t1/attach"),
        ("POST", "/api/computers/local/fs/write"),
        ("GET", "/api/sessions/s1/ide"),
        ("POST", "/api/server-token/rotate"),
        ("GET", "/api/devices"),
    ] {
        let body = (m != "GET").then(|| serde_json::json!({}));
        assert_denied(&device_call(&d, &server_t, m, path, body).await, &format!("read_only: {m} {path}"));
    }
}

/// `chat_only` talks in channels and to existing sessions, nothing else.
#[tokio::test]
async fn fr_n7_scope_chat_only_cannot_create_sessions_shell_fs_or_tokens() {
    let net = MemNetwork::new();
    let (server_t, devices) = serve_scoped_server(&net);
    let (d, _) = device_with(&net, &devices, Scope::ChatOnly);

    for (m, path) in [
        ("POST", "/api/sessions"),
        ("POST", "/api/sessions/s1/fork"),
        ("POST", "/api/sessions/s1/approvals/a1"),
        ("POST", "/api/computers/local/terms"),
        ("GET", "/api/computers/local/terms/t1/attach"),
        ("POST", "/api/a2a/terminal/s1/text"),
        ("POST", "/api/computers/local/fs/read"),
        ("GET", NORMAL_ROUTE),
        ("POST", "/api/channels"),
        ("POST", "/api/server-token/rotate"),
        ("GET", "/api/server-token"),
        ("PATCH", "/api/devices/x"),
    ] {
        let body = (m != "GET").then(|| serde_json::json!({}));
        assert_denied(&device_call(&d, &server_t, m, path, body).await, &format!("chat_only: {m} {path}"));
    }
    // Allowed families reach their handler (which may answer 404 for an unknown id).
    let got = device_call(&d, &server_t, "GET", "/api/sessions", None).await;
    assert_eq!(got.0, StatusCode::OK, "{:?}", got.1);
    let msg = serde_json::json!({ "text": "hi" });
    assert_not_denied(&device_call(&d, &server_t, "POST", "/api/sessions/s1/messages", Some(msg.clone())).await, "message");
    assert_not_denied(&device_call(&d, &server_t, "POST", "/api/channels/c1/messages", Some(msg)).await, "channel");
}

/// A device cannot widen its own scope: `/api/devices` is not on the transport, a narrow scope
/// is refused there outright, and re-adding a device keeps its scope. Only the owner, on the
/// local listener with the token, sets it.
#[tokio::test]
async fn fr_n7_scope_device_cannot_raise_its_own_scope() {
    let net = MemNetwork::new();
    let (server_t, devices) = serve_scoped_server(&net);
    let (narrow, narrow_peer) = device_with(&net, &devices, Scope::ChatOnly);
    let (full, full_peer) = device_with(&net, &devices, Scope::Full);
    let raise = serde_json::json!({ "scope": "full" });

    let path = format!("/api/devices/{narrow_peer}");
    assert_denied(&device_call(&narrow, &server_t, "PATCH", &path, Some(raise.clone())).await, "self raise");
    let got = device_call(&full, &server_t, "PATCH", &path, Some(raise.clone())).await;
    assert!(!got.0.is_success(), "a full device changed another's scope over the transport: {:?}", got);
    assert_eq!(devices.scope_of(&narrow_peer), Some(Scope::ChatOnly));
    devices.add(narrow_peer, "renamed").unwrap();
    assert_eq!(devices.scope_of(&narrow_peer), Some(Scope::ChatOnly), "re-adding widened the scope");
    let _ = full_peer;

    // The owner's door: the devices router behind the token.
    let (addr, token, _dir) = serve_local(ember_server::devices::api::router(devices.clone())).await;
    let patch = |bearer: Option<String>| {
        let path = path.clone();
        async move {
            let client = reqwest::Client::new();
            let mut req = client.patch(format!("http://{addr}{path}")).json(&serde_json::json!({ "scope": "read_only" }));
            if let Some(t) = bearer {
                req = req.bearer_auth(t);
            }
            req.send().await.unwrap().status().as_u16()
        }
    };
    assert_eq!(patch(None).await, 401);
    assert_eq!(patch(Some(token)).await, 200);
    assert_eq!(devices.scope_of(&narrow_peer), Some(Scope::ReadOnly));
}

// ---- FR-N8: `GET /api/whoami` (#818) -------------------------------------------------------

/// The local listener (behind the server token) answers door `local`, scope `full`, no peer id.
#[tokio::test]
async fn fr_n7_whoami_local_is_local_and_full() {
    let (app, _store) = router();
    let (addr, token, _dir) = serve_local(app).await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/api/whoami"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body, serde_json::json!({ "door": "local", "scope": "full" }));
}

async fn whoami_for(scope: Scope) {
    let net = MemNetwork::new();
    let (server_t, devices) = serve_scoped_server(&net);
    let (device, peer) = device_with(&net, &devices, scope);
    let got = device_call(&device, &server_t, "GET", "/api/whoami", None).await;
    assert_eq!(got.0, StatusCode::OK, "{:?}: {:?}", scope, got.1);
    assert_eq!(
        got.1,
        serde_json::json!({ "door": "transport", "peer_id": peer.to_string(), "scope": scope.as_str() })
    );
}

#[tokio::test]
async fn fr_n7_whoami_transport_full() {
    whoami_for(Scope::Full).await;
}

#[tokio::test]
async fn fr_n7_whoami_transport_read_only() {
    whoami_for(Scope::ReadOnly).await;
}

#[tokio::test]
async fn fr_n7_whoami_transport_chat_only() {
    whoami_for(Scope::ChatOnly).await;
}
