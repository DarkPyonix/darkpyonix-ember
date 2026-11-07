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
