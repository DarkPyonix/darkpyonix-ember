//! SPEC FR-N9: the app joins a main server as a device over the transport. The app's own
//! connection code (`ember_app::device_link`) against an in-process server, over the in-memory
//! transport and over loopback letify: an allowed device gets normal answers through the client
//! `Api` and through the app's `ServerApi`; a device not on the allow-list is refused; a revoked
//! device is refused afterwards; each refusal reads as a Korean sentence.

use std::sync::Arc;
use std::time::Duration;

use ember_app::device_link::{self, DeviceLink, PeerSetting, NOT_ALLOWED, UNREACHABLE};
use ember_app::server::ServerApi;
use ember_client::{Client, ClientConfig, ConnectionState};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::AgentAdapter;
use ember_server::devices::Devices;
use ember_server::session::Sessions;
use ember_server::store::Store;
use ember_transport::mem::MemNetwork;
use ember_transport::{BackendKind, LetifyConfig, SecretKey, Transport, TransportConfig};

const WAIT: Duration = Duration::from_secs(10);

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

/// A server on `server_t` that serves the plain router to the devices on the returned list.
fn serve(server_t: &Transport) -> Arc<Devices> {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let devices = Devices::open(store.clone()).unwrap();
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    let sessions = Sessions::new(store, adapters);
    let app = ember_server::api::router(sessions);
    tokio::spawn(ember_server::transport::serve(server_t, app, devices.gate().clone()).unwrap());
    devices
}

fn scratch() -> tempfile::TempDir {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::tempdir_in(base).unwrap()
}

fn is_refusal_sentence(t: &str) -> bool {
    t == NOT_ALLOWED || t == UNREACHABLE
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n9_app_device_link_over_the_in_memory_transport() {
    let net = MemNetwork::new();
    let (server_t, device_t, stranger_t) = (net.transport(), net.transport(), net.transport());
    let devices = serve(&server_t);
    devices.add(device_t.peer_id(), "phone").unwrap();

    // The address as the owner types it in 설정: the bare peer id.
    let setting = PeerSetting::parse(&server_t.peer_id().to_string(), None).unwrap();
    let link = DeviceLink::new(device_t.clone(), setting.addr.clone());
    assert_eq!(link.peer_id(), device_t.peer_id());
    let api = link.api();
    assert_eq!(api.base_url(), setting.base_url());

    // Allowed: normal answers through the client `Api` and the app's `ServerApi`.
    assert!(tokio::time::timeout(WAIT, api.health()).await.unwrap().unwrap().ok);
    assert_eq!(tokio::time::timeout(WAIT, device_link::check(&api)).await.unwrap(), Ok(()));
    assert!(tokio::time::timeout(WAIT, api.projects()).await.unwrap().unwrap().is_empty());
    let server = ServerApi::over_transport(link.api()).unwrap();
    let defaults = tokio::time::timeout(WAIT, server.agent_defaults()).await.unwrap();
    assert!(defaults.is_ok(), "{defaults:?}");
    assert!(tokio::time::timeout(WAIT, server.agent_settings()).await.unwrap().is_ok());

    // The client the app builds over it syncs and opens its push connection.
    let mut config = ClientConfig::new(setting.base_url());
    config.lease_interval = Duration::from_millis(50);
    config.backoff_min = Duration::from_millis(20);
    config.backoff_max = Duration::from_millis(200);
    let client = Client::with_api(config, link.api()).await;
    client.start();
    until("push connected", || client.read(|st| *st.connection() == ConnectionState::Connected)).await;

    // Not on the allow-list: refused, and the refusal reads in Korean.
    let stranger = DeviceLink::new(stranger_t, setting.addr.clone()).api();
    assert!(tokio::time::timeout(WAIT, stranger.health()).await.unwrap().is_err());
    let text = tokio::time::timeout(WAIT, device_link::check(&stranger)).await.unwrap().unwrap_err();
    assert!(is_refusal_sentence(text), "{text}");
    let refused = tokio::time::timeout(WAIT, ServerApi::over_transport(stranger).unwrap().agent_defaults()).await.unwrap();
    assert!(matches!(refused, Err(ember_app::server::ServerError::Client(_))), "{refused:?}");

    // Revoked: later calls fail, with the same sentence.
    assert!(devices.remove(&device_t.peer_id()).unwrap() >= 1);
    let mut failed = None;
    for _ in 0..100 {
        if let Err(t) = tokio::time::timeout(WAIT, device_link::check(&api)).await.unwrap() {
            failed = Some(t);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let text = failed.expect("a revoked device is refused");
    assert!(is_refusal_sentence(text), "{text}");
    assert!(tokio::time::timeout(WAIT, server.agent_defaults()).await.unwrap().is_err());
    client.stop();
}

/// The server's letify endpoint on loopback; dials give up after two seconds, so a refused
/// device fails well inside the test's timeouts.
async fn letify_server() -> Transport {
    let letify = LetifyConfig { connect_timeout: Duration::from_secs(2), ..LetifyConfig::default() };
    let config = TransportConfig::new(SecretKey::generate())
        .backend(BackendKind::Letify)
        .letify(letify)
        .bind_addr("127.0.0.1:0".parse().unwrap());
    Transport::bind(config).await.unwrap()
}

/// The app's own bind (`device_link::transport_config` with the device key from the data dir),
/// on loopback with the short dial timeout.
async fn letify_device(key: SecretKey, setting: &PeerSetting) -> DeviceLink {
    let mut config = device_link::transport_config(key, setting);
    config.backend = BackendKind::Letify;
    config.letify.connect_timeout = Duration::from_secs(2);
    config.bind_addr = Some("127.0.0.1:0".parse().unwrap());
    DeviceLink::bind_with(config, setting.addr.clone()).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n9_app_device_link_over_loopback_letify() {
    let server_t = letify_server().await;
    let devices = serve(&server_t);
    let data = scratch();
    let key = device_link::load_key(data.path()).unwrap();
    devices.add(key.peer_id(), "phone").unwrap();

    // The address as the server prints it at start (`ember server peer addr: {...}`), no relay.
    let printed = serde_json::to_string(&server_t.local_addr()).unwrap();
    let setting = PeerSetting::parse(&printed, Some("none")).unwrap();
    let link = letify_device(key, &setting).await;
    let api = link.api();
    assert_eq!(tokio::time::timeout(WAIT, device_link::check(&api)).await.unwrap(), Ok(()));
    let server = ServerApi::over_transport(link.api()).unwrap();
    assert!(tokio::time::timeout(WAIT, server.agent_defaults()).await.unwrap().is_ok());

    // A device whose key is not on the list.
    let other = scratch();
    let stranger = letify_device(device_link::load_key(other.path()).unwrap(), &setting).await;
    let text = tokio::time::timeout(WAIT, device_link::check(&stranger.api())).await.unwrap().unwrap_err();
    assert!(is_refusal_sentence(text), "{text}");

    // Revoked.
    assert!(devices.remove(&link.peer_id()).unwrap() >= 1);
    let mut failed = None;
    for _ in 0..40 {
        if let Err(t) = tokio::time::timeout(WAIT, device_link::check(&api)).await.unwrap() {
            failed = Some(t);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let text = failed.expect("a revoked device is refused");
    assert!(is_refusal_sentence(text), "{text}");

    link.dialer.transport().close().await;
    stranger.dialer.transport().close().await;
    server_t.close().await;
}

/// A free TCP port on loopback for the relay role (bound and released at once).
fn free_tcp_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// The main server's letify endpoint on loopback with `key`, its relay on `relay_port`, as
/// `ember_server::transport::with_main_server_roles` sets it up (the address observer on a free
/// loopback UDP port so it does not need the relay's port free on UDP too).
async fn letify_main_server(key: SecretKey, relay_port: u16) -> Transport {
    let letify = LetifyConfig {
        connect_timeout: Duration::from_secs(2),
        relay_listen: Some(std::net::SocketAddr::from(([127, 0, 0, 1], relay_port))),
        qad_listen: Some("127.0.0.1:0".parse().unwrap()),
        ..LetifyConfig::default()
    };
    let config = TransportConfig::new(key)
        .backend(BackendKind::Letify)
        .letify(letify)
        .bind_addr("127.0.0.1:0".parse().unwrap());
    let config = ember_server::transport::with_main_server_roles(config, relay_port);
    Transport::bind(config).await.unwrap()
}

/// Asks until the server answers, for a link whose server just came back.
async fn check_until_ok(api: &ember_client::api::Api) -> Result<(), &'static str> {
    let mut last = Err(UNREACHABLE);
    for _ in 0..20 {
        last = tokio::time::timeout(WAIT, device_link::check(api)).await.unwrap();
        if last.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    last
}

/// #863: the setting a device keeps is the server's peer id and the server's own relay (fixed
/// port), pasted as the line the server prints; it still reaches the server after the server's
/// transport is bound again with the same key and relay port and a new direct port.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "a device with only the peer id and the relay cannot reach the server: the letify dialer builds a relayed path only with a rendezvous (hub mailbox) and the letify-link relay never hands a request addressed to its own id to the server's acceptor, #863"]
async fn fr_n9_a_device_keeps_the_server_through_its_relay_after_a_restart() {
    let server_key = SecretKey::generate();
    let relay_port = free_tcp_port();
    let server_t = letify_main_server(server_key.clone(), relay_port).await;
    let devices = serve(&server_t);
    let data = scratch();
    let key = device_link::load_key(data.path()).unwrap();
    devices.add(key.peer_id(), "phone").unwrap();

    // The line the server prints at start, pasted into the address field in one go.
    let line = ember_server::transport::device_address_line(true, &server_t.local_addr()).expect("relay role on");
    let id = server_t.peer_id();
    assert_eq!(
        line,
        format!("{}{id} relay letify://{id}@127.0.0.1:{relay_port}", ember_server::transport::DEVICE_ADDRESS_PREFIX)
    );
    let setting = PeerSetting::parse(&line, None).unwrap();
    assert_eq!(setting.addr.peer, id);
    assert!(setting.addr.direct.is_empty(), "no direct address is saved: {setting:?}");
    assert!(setting.addr.relays.is_empty());
    assert_eq!(
        setting.relay,
        Some(ember_transport::RelayConfig::Custom(vec![format!("letify://{id}@127.0.0.1:{relay_port}")]))
    );

    let link = letify_device(key.clone(), &setting).await;
    let api = link.api();
    assert_eq!(tokio::time::timeout(WAIT, device_link::check(&api)).await.unwrap(), Ok(()));
    let server = ServerApi::over_transport(link.api()).unwrap();
    assert!(tokio::time::timeout(WAIT, server.agent_defaults()).await.unwrap().is_ok());

    // The server restarts: same key, same relay port, a new direct port.
    let old_direct = server_t.local_addr().direct.clone();
    server_t.close().await;
    drop(server_t);
    let server_t = letify_main_server(server_key, relay_port).await;
    assert_eq!(server_t.peer_id(), id);
    assert_ne!(server_t.local_addr().direct, old_direct, "a new direct port");
    let devices = serve(&server_t);
    devices.add(key.peer_id(), "phone").unwrap();

    // The same live app transport, with the unchanged setting.
    assert_eq!(check_until_ok(&api).await, Ok(()));
    // A fresh bind of the app side, as after an app restart.
    let again = letify_device(key, &setting).await;
    assert_eq!(tokio::time::timeout(WAIT, device_link::check(&again.api())).await.unwrap(), Ok(()));

    link.dialer.transport().close().await;
    again.dialer.transport().close().await;
    server_t.close().await;
}
