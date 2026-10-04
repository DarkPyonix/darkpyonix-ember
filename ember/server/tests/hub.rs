//! ember server and the darkpyonix.dev hub (SPEC FR-N2), against the in-process fake hub and the
//! in-memory transport: the server registers to the account through a device link (token sealed
//! at rest), adds a computer by picking it from the account's device list (no address pasted),
//! approves a node's user code, syncs the devices allow-list, and notices when the hub removes it.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use ember_hub::fake::FakeHub;
use ember_hub::{DeviceLink, HubConfig, Role};
use ember_node::api::Node;
use ember_node::config::NodeConfig;
use ember_server::accounts::secrets::SecretBox;
use ember_server::accounts::Accounts;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::AgentAdapter;
use ember_server::computers::{self, Computers, Registry};
use ember_server::devices::Devices;
use ember_server::hub::ServerHub;
use ember_server::session::Sessions;
use ember_server::store::Store;
use ember_server::transport::ServerTransport;
use ember_transport::mem::MemNetwork;
use ember_transport::{PeerGate, SecretKey, Transport};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const TOKEN: &str = "node-token";

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

struct Fixture {
    hub: FakeHub,
    server_hub: Arc<ServerHub>,
    computers: Arc<Computers>,
    devices: Arc<Devices>,
    app: axum::Router,
    net: MemNetwork,
    server_t: Transport,
    store: Arc<Store>,
    accounts: Arc<Accounts>,
    key: SecretKey,
    _dir: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let hub = FakeHub::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&dir.path().join("ember.db")).unwrap());
    let accounts = Accounts::with_secrets(store.clone(), &dir.path().join("accounts"), SecretBox::ephemeral()).unwrap();
    let devices = Devices::open(store.clone()).unwrap();
    let net = MemNetwork::new();
    let key = SecretKey::generate();
    let server_t = net.transport_with_key(key.clone());
    let st = ServerTransport::new(server_t.clone());
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    let sessions = Sessions::new(store.clone(), adapters);
    let computers = Computers::with_transport(
        Registry::new(store.clone()),
        computers::connector(Some(st.dialer.clone())),
        None,
        Some(st.dialer.clone()),
    );
    computers.install(&sessions);
    let mut server_hub =
        ServerHub::new(HubConfig::new(hub.url()), store.clone(), accounts.clone(), key.clone(), devices.clone());
    server_hub.set_poll_interval(Duration::from_millis(50));
    server_hub.attach_transport(server_t.clone());
    let app = ember_server::hub::api::router(Some(server_hub.clone()), computers.clone())
        .merge(ember_server::hub::api::admin_router(Some(server_hub.clone()), computers.clone()))
        .merge(computers::api::router(computers.clone(), sessions.clone()));
    Fixture { hub, server_hub, computers, devices, app, net, server_t, store, accounts, key, _dir: dir }
}

async fn register_server(f: &Fixture) {
    let (st, pending) = call(&f.app, Method::POST, "/api/v1/hub/link", Some(json!({ "name": "home server" }))).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{pending}");
    let code = pending["user_code"].as_str().unwrap().to_string();
    assert!(pending["verification_uri_complete"].as_str().unwrap().ends_with(&code));
    // Shown in the status while waiting.
    let (_, status) = call(&f.app, Method::GET, "/api/v1/hub", None).await;
    assert_eq!(status["pending"]["user_code"], code);
    assert_eq!(status["registered"], false);

    // The person approves it in the browser.
    assert!(f.hub.approve(&code));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let (_, status) = call(&f.app, Method::GET, "/api/v1/hub", None).await;
        if status["registered"] == true {
            assert_eq!(status["device"]["role"], "main_server");
            assert_eq!(status["device"]["name"], "home server");
            assert!(status["pending"].is_null());
            break;
        }
        assert!(status["last_error"].is_null(), "{status}");
        assert!(std::time::Instant::now() < deadline, "registration never completed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A real ember node on the fake network that admits only `server`.
fn start_node(net: &MemNetwork, server: &Transport) -> (Transport, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let node = Node::new(NodeConfig::new(TOKEN, vec![root])).unwrap();
    let t = net.transport();
    tokio::spawn(ember_node::transport::serve(&t, node, PeerGate::allow_list([server.peer_id()])).unwrap());
    (t, dir)
}

#[tokio::test]
async fn register_then_add_a_computer_from_the_hub_device_list() {
    let f = fixture().await;

    // Before registering, hub calls say so.
    let (st, _) = call(&f.app, Method::GET, "/api/v1/hub/devices", None).await;
    assert_eq!(st, StatusCode::CONFLICT);

    register_server(&f).await;
    // The token is sealed at rest: the raw row does not contain it.
    let reg = f.server_hub.registration().unwrap().unwrap();
    // A second connection to the same database file, as anything reading the disk would see it.
    let raw: Vec<u8> = rusqlite::Connection::open(f._dir.path().join("ember.db"))
        .unwrap()
        .query_row("SELECT token_ciphertext FROM hub_registration", [], |r| r.get(0))
        .unwrap();
    assert!(!raw.windows(reg.device_token.len()).any(|w| w == reg.device_token.as_bytes()));
    // Registering twice is refused.
    let (st, _) = call(&f.app, Method::POST, "/api/v1/hub/link", None).await;
    assert_eq!(st, StatusCode::CONFLICT);

    // A node joins the account (registered on its own; see ember/node/tests/hub.rs for its link).
    let (node_t, _node_dir) = start_node(&f.net, &f.server_t);
    f.hub.register(node_t.peer_id(), "gpu box", Role::Computer);

    let (st, list) = call(&f.app, Method::GET, "/api/v1/hub/devices", None).await;
    assert_eq!(st, StatusCode::OK, "{list}");
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 2);
    let me = list.iter().find(|d| d["this_server"] == true).unwrap();
    assert_eq!(me["endpoint_id"], f.server_t.peer_id().to_string());
    let gpu = list.iter().find(|d| d["name"] == "gpu box").unwrap();
    assert!(gpu["computer_id"].is_null());

    // Pick it: only the endpoint id (from the list) and the node's API token.
    let node_id = node_t.peer_id().to_string();
    let (st, c) = call(&f.app, Method::POST, &format!("/api/v1/hub/devices/{node_id}/computer"), Some(json!({ "token": TOKEN }))).await;
    assert_eq!(st, StatusCode::CREATED, "{c}");
    assert_eq!(c["name"], "gpu box");
    assert_eq!(c["peer"]["peer"], node_id);
    let cid = c["id"].as_str().unwrap().to_string();

    // It is reachable over the transport by peer id alone.
    let (st, status) = call(&f.app, Method::GET, &format!("/api/v1/computers/{cid}"), None).await;
    assert_eq!(st, StatusCode::OK, "{status}");
    assert_eq!(status["reachable"], true, "{status}");

    // The list now shows the computer; this server itself and unknown devices are refused.
    let (_, list) = call(&f.app, Method::GET, "/api/v1/hub/devices", None).await;
    assert_eq!(list.as_array().unwrap().iter().find(|d| d["name"] == "gpu box").unwrap()["computer_id"], cid);
    let me = f.server_t.peer_id().to_string();
    let (st, _) = call(&f.app, Method::POST, &format!("/api/v1/hub/devices/{me}/computer"), Some(json!({ "token": TOKEN }))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let stranger = SecretKey::generate().peer_id().to_string();
    let (st, _) = call(&f.app, Method::POST, &format!("/api/v1/hub/devices/{stranger}/computer"), Some(json!({ "token": TOKEN }))).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert_eq!(f.computers.registry().list().unwrap().len(), 1);
}

#[tokio::test]
async fn the_server_approves_a_nodes_user_code() {
    let f = fixture().await;
    register_server(&f).await;

    let node_key = SecretKey::generate();
    let link = DeviceLink::start(&f.hub.client(), &node_key, "pi", Role::Computer)
        .await
        .unwrap()
        .with_poll_interval(Duration::from_millis(50));
    let code = link.user_code().to_string();

    let (st, info) = call(&f.app, Method::GET, &format!("/api/v1/hub/link-codes/{code}"), None).await;
    assert_eq!(st, StatusCode::OK, "{info}");
    assert_eq!(info["endpoint_id"], node_key.peer_id().to_string());
    assert_eq!(info["role"], "computer");
    let (st, _) = call(&f.app, Method::POST, &format!("/api/v1/hub/link-codes/{code}"), Some(json!({ "approve": true }))).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let reg = tokio::time::timeout(Duration::from_secs(10), link.wait()).await.unwrap().unwrap();
    assert_eq!(reg.device.name, "pi");
    // Decided codes are gone.
    let (st, _) = call(&f.app, Method::GET, &format!("/api/v1/hub/link-codes/{code}"), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn devices_allow_list_syncs_from_the_hub() {
    let f = fixture().await;
    register_server(&f).await;
    let phone = SecretKey::generate().peer_id();
    let laptop = SecretKey::generate().peer_id();
    f.hub.register(phone, "phone", Role::Computer);
    f.hub.register(laptop, "laptop", Role::Computer);

    let (st, report) = call(&f.app, Method::POST, "/api/v1/hub/sync-devices", None).await;
    assert_eq!(st, StatusCode::OK, "{report}");
    assert_eq!(report["added"].as_array().unwrap().len(), 2);
    assert!(f.devices.gate().is_allowed(&phone) && f.devices.gate().is_allowed(&laptop));
    assert!(!f.devices.gate().is_allowed(&f.server_t.peer_id()), "the server is not its own device");

    // Removed on the hub (through the server) → revoked here.
    let (st, _) = call(&f.app, Method::PUT, "/api/v1/hub/sync-devices", Some(json!({ "enabled": true }))).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = call(&f.app, Method::DELETE, &format!("/api/v1/hub/devices/{phone}"), None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert!(!f.devices.gate().is_allowed(&phone));
    assert!(f.devices.gate().is_allowed(&laptop));

    // Removed in the browser → the next sync revokes it.
    assert!(f.hub.remove(&laptop));
    let (_, report) = call(&f.app, Method::POST, "/api/v1/hub/sync-devices", None).await;
    assert_eq!(report["removed"], json!([laptop.to_string()]));
    assert!(!f.devices.gate().is_allowed(&laptop));
}

#[tokio::test]
async fn revocation_on_the_hub_is_detected_and_surfaced() {
    let f = fixture().await;
    register_server(&f).await;
    let (_, state) = call(&f.app, Method::POST, "/api/v1/hub/check", None).await;
    assert_eq!(state["state"], "active");
    assert_eq!(state["github_login"], "octocat");

    // The owner removes the server in the browser.
    assert!(f.hub.remove(&f.server_t.peer_id()));
    let (_, state) = call(&f.app, Method::POST, "/api/v1/hub/check", None).await;
    assert_eq!(state["state"], "revoked");
    let (_, status) = call(&f.app, Method::GET, "/api/v1/hub", None).await;
    assert_eq!(status["revoked"], true);
    assert_eq!(status["registered"], false);
    assert!(status["revoked_at"].is_i64());
    // Calls needing the token answer 410 Gone with the reason.
    let (st, body) = call(&f.app, Method::GET, "/api/v1/hub/devices", None).await;
    assert_eq!(st, StatusCode::GONE, "{body}");
    assert!(body["error"].as_str().unwrap().contains("removed"));
    // A removed key cannot simply link again: the hub refuses until the owner re-admits it.
    let (st, body) = call(&f.app, Method::POST, "/api/v1/hub/link", None).await;
    assert_eq!(st, StatusCode::CONFLICT, "{body}");
}

#[tokio::test]
async fn revocation_is_noticed_by_any_hub_call_and_by_the_watcher() {
    let f = fixture().await;
    register_server(&f).await;
    assert!(f.hub.remove(&f.server_t.peer_id()));
    // A device-list call gets 401 and records the revocation.
    let (st, _) = call(&f.app, Method::GET, "/api/v1/hub/devices", None).await;
    assert_eq!(st, StatusCode::GONE);
    assert!(f.server_hub.status().unwrap().revoked);

    // The watcher stops on a revoked registration.
    let task = f.server_hub.spawn_watch(Duration::from_millis(20));
    tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap();
}

#[tokio::test]
async fn long_poll_watcher_sees_removal_at_once_and_syncs_devices() {
    let f = fixture().await;
    register_server(&f).await;
    // Registration asked the hub's /v1/config: the fake offers the long-poll.
    let (_, status) = call(&f.app, Method::GET, "/api/v1/hub", None).await;
    assert_eq!(status["long_poll"], true, "{status}");
    assert_eq!(status["pkarr_url"], format!("{}/pkarr", f.hub.url()));
    assert_eq!(status["relay_source"], "derived");

    f.server_hub.set_sync_devices(true);
    // A one-minute period: only the long-poll can be this quick.
    let task = f.server_hub.spawn_watch(Duration::from_secs(60));
    let node = SecretKey::generate().peer_id();
    f.hub.register(node, "gpu box", Role::Computer);
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !f.devices.list().unwrap().iter().any(|d| d.peer_id == node) {
        assert!(std::time::Instant::now() < deadline, "device list change not synced by the long-poll");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Let the next held request reach the hub, then remove this server there.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let t0 = std::time::Instant::now();
    assert!(f.hub.remove(&f.server_t.peer_id()));
    tokio::time::timeout(Duration::from_secs(2), task).await.expect("watcher stops on removal").unwrap();
    assert!(t0.elapsed() < Duration::from_secs(2));
    assert!(f.server_hub.status().unwrap().revoked);
}

#[tokio::test]
async fn a_rejected_token_is_not_a_revocation() {
    let f = fixture().await;
    register_server(&f).await;
    // `401 {code: invalid_credentials}` (a token the hub does not know) does not revoke.
    let e = f.hub.client().with_token("dpd_unknown").devices().await.unwrap_err();
    assert!(!e.is_revocation(), "{e:?}");
    let (st, body) = call(&f.app, Method::POST, "/api/v1/hub/check", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["state"], "active");
    // `401 {code: device_removed}` does.
    assert!(f.hub.remove(&f.server_t.peer_id()));
    let (_, body) = call(&f.app, Method::POST, "/api/v1/hub/check", None).await;
    assert_eq!(body["state"], "revoked");
    assert!(f.server_hub.status().unwrap().revoked);
}

#[tokio::test]
async fn registration_keeps_a_sealed_resolve_token_and_reports_the_app() {
    let f = fixture().await;
    register_server(&f).await;
    let me = f.server_t.peer_id();
    // The directory resolves with the resolve token, never the device token (NFR-H2).
    let resolve = f.server_hub.directory_token().expect("resolve token stored");
    assert!(resolve.starts_with("dpr_"));
    assert_eq!(Some(resolve.clone()), f.hub.resolve_token(&me));
    // Stored sealed (with the server's secret.key) and read back after a "restart".
    let devices = Devices::open(f.store.clone()).unwrap();
    let again = ServerHub::new(HubConfig::new(f.hub.url()), f.store.clone(), f.accounts.clone(), f.key.clone(), devices);
    assert_eq!(again.directory_token(), Some(resolve));
    // The app record (FR-H10) was published with the server's own token.
    let device = f.hub.devices().into_iter().find(|d| d.endpoint_id == me).unwrap();
    let app = device.app.expect("app reported");
    assert_eq!(app.kind, "ember-server");
    assert_eq!(app.services, vec!["ember-server-v1"]);
}

#[tokio::test]
async fn a_main_server_code_is_approved_in_the_browser_not_by_the_server() {
    let f = fixture().await;
    register_server(&f).await;
    let link = DeviceLink::start(&f.hub.client(), &SecretKey::generate(), "second", Role::MainServer).await.unwrap();
    let code = link.user_code().to_string();
    let (st, body) =
        call(&f.app, Method::POST, &format!("/api/v1/hub/link-codes/{code}"), Some(json!({ "approve": true }))).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
    assert!(body["error"].as_str().unwrap().contains("/link?code="), "{body}");
    // Denying is allowed.
    let (st, _) =
        call(&f.app, Method::POST, &format!("/api/v1/hub/link-codes/{code}"), Some(json!({ "approve": false }))).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert!(!f.server_hub.status().unwrap().revoked, "a 403 is not a revocation");
}

#[tokio::test]
async fn leaving_removes_the_server_on_the_hub() {
    let f = fixture().await;
    register_server(&f).await;
    let me = f.server_t.peer_id();
    let (st, _) = call(&f.app, Method::DELETE, "/api/v1/hub/registration", None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert!(f.hub.devices().iter().all(|d| d.endpoint_id != me), "removed on the hub with its own token");
    assert!(!f.server_hub.status().unwrap().registered);

    // `?local=1` only forgets here.
    let f = fixture().await;
    register_server(&f).await;
    let (st, _) = call(&f.app, Method::DELETE, "/api/v1/hub/registration?local=1", None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert!(f.hub.devices().iter().any(|d| d.endpoint_id == f.server_t.peer_id()));
}

#[tokio::test]
async fn a_link_pending_at_restart_is_resumed() {
    let f = fixture().await;
    let (st, pending) = call(&f.app, Method::POST, "/api/v1/hub/link", Some(json!({ "name": "home" }))).await;
    assert_eq!(st, StatusCode::ACCEPTED);
    let code = pending["user_code"].as_str().unwrap().to_string();

    // "Restart": a new ServerHub on the same store finds the link and follows it.
    let devices = Devices::open(f.store.clone()).unwrap();
    let mut again = ServerHub::new(HubConfig::new(f.hub.url()), f.store.clone(), f.accounts.clone(), f.key.clone(), devices);
    again.set_poll_interval(Duration::from_millis(50));
    let view = again.resume_link().await.unwrap().expect("pending link resumed");
    assert_eq!(view.user_code, code);
    assert!(f.hub.approve(&code));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !again.status().unwrap().registered {
        assert!(std::time::Instant::now() < deadline, "resumed link never completed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Nothing left to resume.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(again.resume_link().await.unwrap().is_none());
}

#[tokio::test]
async fn a_removed_server_relinks_after_readmission() {
    let f = fixture().await;
    register_server(&f).await;
    let me = f.server_t.peer_id();
    assert!(f.hub.remove(&me));
    let (st, _) = call(&f.app, Method::POST, "/api/v1/hub/check", None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(f.server_hub.status().unwrap().revoked);

    // Not re-admitted: 409 with the advice.
    let (st, body) = call(&f.app, Method::POST, "/api/v1/hub/link", Some(json!({ "name": "home" }))).await;
    assert_eq!(st, StatusCode::CONFLICT, "{body}");
    assert!(body["error"].as_str().unwrap().contains("re-admit"), "{body}");

    // The owner re-admits it, the server links again, the person approves in the browser.
    assert!(f.hub.readmit(&me));
    let (st, pending) = call(&f.app, Method::POST, "/api/v1/hub/link", Some(json!({ "name": "home" }))).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{pending}");
    assert!(f.hub.approve(pending["user_code"].as_str().unwrap()));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let st = f.server_hub.status().unwrap();
        if st.registered && !st.revoked {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "re-admitted link never completed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn hub_off_answers_503() {
    let net = MemNetwork::new();
    let st = ServerTransport::new(net.transport());
    let computers = Computers::with_transport(
        Registry::open_in_memory().unwrap(),
        computers::connector(Some(st.dialer.clone())),
        None,
        Some(st.dialer.clone()),
    );
    let app = ember_server::hub::api::router(None, computers);
    let (st, body) = call(&app, Method::GET, "/api/v1/hub", None).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body["error"].as_str().unwrap().contains("EMBER_TRANSPORT"));
}
