//! SPEC FR-N3: revoking a device closes its live connections within one heartbeat, with a
//! numeric bound measured here, on the in-memory transport. Two paths reach the gate: the local
//! `DELETE /api/devices/{peer}` and a device removed on the hub (`FakeHub`), which the server's
//! devices sync turns into `Devices::sync_from_hub`, which calls `PeerGate::revoke`.
//!
//! The heartbeat is 5 s (the bound `ember/transport/tests/gate.rs` uses for its first backend).
//! SPEC also says a device on the default transport sees close code 401 within a second, so the local path
//! asserts one second. Waits carry larger timeouts, so a slow CI machine fails on the product
//! bound and not on a hung wait.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use ember_hub::fake::FakeHub;
use ember_hub::{HubConfig, Role};
use ember_server::accounts::secrets::SecretBox;
use ember_server::accounts::Accounts;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::AgentAdapter;
use ember_server::computers::{self, Computers, Registry};
use ember_server::devices::Devices;
use ember_server::hub::ServerHub;
use ember_server::session::Sessions;
use ember_server::store::Store;
use ember_server::transport::{self as server_transport, ServerTransport, SERVER_SERVICE};
use ember_transport::gate::CLOSE_REVOKED;
use ember_transport::mem::MemNetwork;
use ember_transport::{CloseReason, Connection, Dialer, PeerAddr, SecretKey, Transport};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

/// One heartbeat, the bound FR-N3 gives.
const HEARTBEAT: Duration = Duration::from_secs(5);
/// The default transport's bound in the FR-N3 acceptance column.
const ONE_SECOND: Duration = Duration::from_secs(1);
/// A wait that only guards against a hang; the product bounds are asserted separately.
const WAIT: Duration = Duration::from_secs(30);

async fn call(app: &axum::Router, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let req = Request::builder().method(method).uri(uri);
    let req = match body {
        Some(b) => req.header("content-type", "application/json").body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
    (status, value)
}

fn assert_revoked(reason: &CloseReason) {
    assert!(matches!(reason, CloseReason::Remote { code: CLOSE_REVOKED, .. }), "{reason:?}");
}

/// A server on `net` serving its API to the devices in `devices`.
fn serve_server(net: &MemNetwork, devices: &Arc<Devices>, store: Arc<Store>) -> Transport {
    serve_server_with(net.transport(), devices, store)
}

fn serve_server_with(server_t: Transport, devices: &Arc<Devices>, store: Arc<Store>) -> Transport {
    let sessions = Sessions::new(store, vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let app = ember_server::api::router(sessions);
    tokio::spawn(server_transport::serve(&server_t, app, devices.gate().clone()).unwrap());
    server_t
}

/// The gate registers a connection on the server side after the dial returns, so wait for the
/// count instead of reading it at once.
async fn wait_live(count: impl Fn() -> usize, want: usize) {
    let deadline = Instant::now() + WAIT;
    while count() != want && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(count(), want, "live connections of the device");
}

async fn open_connection(device: &Dialer, server: &Transport) -> Connection {
    tokio::time::timeout(WAIT, device.connection(&PeerAddr::new(server.peer_id()), SERVER_SERVICE))
        .await
        .expect("dial timed out")
        .expect("device was admitted")
}

#[tokio::test]
async fn fr_n3_revoked_device_connection_closes_within_one_second() {
    let net = MemNetwork::new();
    let device_t = net.transport();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let devices = Devices::open(store.clone()).unwrap();
    let server_t = serve_server(&net, &devices, store);
    let admin = ember_server::devices::api::router(devices.clone());

    let (st, d) = call(&admin, Method::POST, "/api/devices", Some(json!({ "peer_id": device_t.peer_id().to_string(), "name": "phone" }))).await;
    assert_eq!(st, StatusCode::CREATED, "{d}");
    let dialer = Dialer::new(device_t.clone());
    let conn = open_connection(&dialer, &server_t).await;
    wait_live(|| devices.gate().live_connections(&device_t.peer_id()), 1).await;

    // From the DELETE request to the device's own closed().
    let start = Instant::now();
    let (st, out) = call(&admin, Method::DELETE, &format!("/api/devices/{}", device_t.peer_id()), None).await;
    assert_eq!(st, StatusCode::OK, "{out}");
    assert_eq!(out["closed"], 1);
    let reason = tokio::time::timeout(WAIT, conn.closed()).await.expect("connection never closed");
    let elapsed = start.elapsed();
    assert_revoked(&reason);
    assert!(elapsed < ONE_SECOND, "closed after {elapsed:?}, FR-N3 says within a second");
    assert!(elapsed < HEARTBEAT);
    wait_live(|| devices.gate().live_connections(&device_t.peer_id()), 0).await;
}

/// Every live connection of the revoked device closes in the same bound, not only the first.
#[tokio::test]
async fn fr_n3_revoking_closes_every_connection_of_the_device_within_one_heartbeat() {
    let net = MemNetwork::new();
    let (phone_t, other_t) = (net.transport(), net.transport());
    let store = Arc::new(Store::open_in_memory().unwrap());
    let devices = Devices::open(store.clone()).unwrap();
    let server_t = serve_server(&net, &devices, store);
    devices.add(phone_t.peer_id(), "phone").unwrap();
    devices.add(other_t.peer_id(), "laptop").unwrap();

    let phone = Dialer::new(phone_t.clone());
    let other = Dialer::new(other_t.clone());
    let conn = open_connection(&phone, &server_t).await;
    let bystander = open_connection(&other, &server_t).await;

    let start = Instant::now();
    assert_eq!(devices.remove(&phone_t.peer_id()).unwrap(), 1);
    let reason = tokio::time::timeout(WAIT, conn.closed()).await.expect("connection never closed");
    assert!(start.elapsed() < HEARTBEAT, "closed after {:?}", start.elapsed());
    assert_revoked(&reason);

    // The other device is untouched, and the revoked one cannot return.
    assert!(tokio::time::timeout(Duration::from_millis(200), bystander.closed()).await.is_err());
    wait_live(|| devices.gate().live_connections(&other_t.peer_id()), 1).await;
    assert!(tokio::time::timeout(WAIT, phone.connection(&PeerAddr::new(server_t.peer_id()), SERVER_SERVICE))
        .await
        .expect("re-dial timed out")
        .is_err());
}

struct HubFixture {
    hub: FakeHub,
    server_hub: Arc<ServerHub>,
    devices: Arc<Devices>,
    app: axum::Router,
    server_t: Transport,
    _dir: tempfile::TempDir,
}

async fn hub_fixture(net: &MemNetwork) -> HubFixture {
    let hub = FakeHub::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&dir.path().join("ember.db")).unwrap());
    let accounts = Accounts::with_secrets(store.clone(), &dir.path().join("accounts"), SecretBox::ephemeral()).unwrap();
    let devices = Devices::open(store.clone()).unwrap();
    let key = SecretKey::generate();
    let server_t = serve_server_with(net.transport_with_key(key.clone()), &devices, store.clone());
    let st = ServerTransport::new(server_t.clone());
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let computers = Computers::with_transport(
        Registry::new(store.clone()),
        computers::connector(Some(st.dialer.clone())),
        None,
        Some(st.dialer.clone()),
    );
    computers.install(&sessions);
    let mut server_hub = ServerHub::new(HubConfig::new(hub.url()), store, accounts, key, devices.clone());
    server_hub.set_poll_interval(Duration::from_millis(50));
    server_hub.attach_transport(server_t.clone());
    let app = ember_server::hub::api::router(Some(server_hub.clone()), computers.clone())
        .merge(ember_server::hub::api::admin_router(Some(server_hub.clone()), computers.clone()));
    HubFixture { hub, server_hub, devices, app, server_t, _dir: dir }
}

async fn register_server(f: &HubFixture) {
    let (st, pending) = call(&f.app, Method::POST, "/api/hub/link", Some(json!({ "name": "home server" }))).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{pending}");
    assert!(f.hub.approve(pending["user_code"].as_str().unwrap()));
    let deadline = Instant::now() + WAIT;
    loop {
        let (_, status) = call(&f.app, Method::GET, "/api/hub", None).await;
        if status["registered"] == true {
            break;
        }
        assert!(Instant::now() < deadline, "registration never completed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A device on the hub account, synced into the allow list, with a live connection to the server.
async fn synced_device_with_connection(f: &HubFixture, net: &MemNetwork) -> (Transport, Connection, Dialer) {
    let device_t = net.transport();
    f.hub.register(device_t.peer_id(), "phone", Role::Computer);
    let (st, report) = call(&f.app, Method::POST, "/api/hub/sync-devices", None).await;
    assert_eq!(st, StatusCode::OK, "{report}");
    assert!(f.devices.gate().is_allowed(&device_t.peer_id()));
    let dialer = Dialer::new(device_t.clone());
    let conn = open_connection(&dialer, &f.server_t).await;
    wait_live(|| f.devices.gate().live_connections(&device_t.peer_id()), 1).await;
    (device_t, conn, dialer)
}

/// Hub-side removal, then the devices sync: the gate drops the live connection with code 401.
#[tokio::test]
async fn fr_n3_device_removed_on_the_hub_is_dropped_by_the_gate_on_sync() {
    let net = MemNetwork::new();
    let f = hub_fixture(&net).await;
    register_server(&f).await;
    let (device_t, conn, dialer) = synced_device_with_connection(&f, &net).await;

    assert!(f.hub.remove(&device_t.peer_id()));
    let start = Instant::now();
    let (st, report) = call(&f.app, Method::POST, "/api/hub/sync-devices", None).await;
    assert_eq!(st, StatusCode::OK, "{report}");
    assert_eq!(report["removed"], json!([device_t.peer_id().to_string()]));
    assert_eq!(report["closed"], 1, "{report}");
    let reason = tokio::time::timeout(WAIT, conn.closed()).await.expect("connection never closed");
    assert_revoked(&reason);
    assert!(start.elapsed() < HEARTBEAT, "closed after {:?}", start.elapsed());
    assert!(!f.devices.gate().is_allowed(&device_t.peer_id()));
    assert!(tokio::time::timeout(WAIT, dialer.connection(&PeerAddr::new(f.server_t.peer_id()), SERVER_SERVICE))
        .await
        .expect("re-dial timed out")
        .is_err());
}

/// With sync on and no manual call, the server's own watcher notices the removal and the live
/// connection closes within one heartbeat of the removal on the hub. The watcher period is one
/// minute, so only the hub long-poll can meet the bound. If this fails, hub-side removal does
/// not reach the gate in time (a real gap: report it, do not loosen the bound).
#[tokio::test]
async fn fr_n3_hub_removal_reaches_the_gate_within_one_heartbeat_without_a_manual_sync() {
    let net = MemNetwork::new();
    let f = hub_fixture(&net).await;
    register_server(&f).await;
    let (device_t, conn, _dialer) = synced_device_with_connection(&f, &net).await;

    f.server_hub.set_sync_devices(true);
    let task = f.server_hub.spawn_watch(Duration::from_secs(60));
    // Let the first held long-poll request reach the hub.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let start = Instant::now();
    assert!(f.hub.remove(&device_t.peer_id()));
    let reason = tokio::time::timeout(WAIT, conn.closed()).await.expect("connection never closed");
    let elapsed = start.elapsed();
    assert_revoked(&reason);
    assert!(elapsed < HEARTBEAT, "hub removal reached the gate after {elapsed:?}, bound is {HEARTBEAT:?}");
    assert!(!f.devices.gate().is_allowed(&device_t.peer_id()));
    task.abort();
}
