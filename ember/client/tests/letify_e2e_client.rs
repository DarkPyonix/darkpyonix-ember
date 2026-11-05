//! SPEC FR-N1, FR-N3 and FR-N5: client sync, push and revocation over letify.

use std::sync::Arc;
use std::time::Duration;

use ember_client::api::Api;
use ember_client::{Client, ClientConfig, ConnectionState};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::devices::Devices;
use ember_server::session::{NewSession, Sessions};
use ember_server::store::Store;
use ember_server::transport::SERVER_SERVICE;
use ember_transport::gate::CLOSE_REVOKED;
use ember_transport::{BackendKind, CloseReason, Dialer, SecretKey, Transport, TransportConfig};

async fn endpoint() -> Transport {
    let config = TransportConfig::new(SecretKey::generate())
        .backend(BackendKind::Letify)
        .bind_addr("127.0.0.1:0".parse().unwrap());
    Transport::bind(config).await.unwrap()
}

async fn until(what: &str, mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

fn new_session(title: &str, cwd: &std::path::Path) -> NewSession {
    NewSession {
        project: "acme".into(),
        agent: AgentKind::Scripted,
        cwd: cwd.into(),
        model: None,
        title: title.into(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n1_fr_n3_client_server_sync_and_revocation_over_letify() {
    let server = endpoint().await;
    let device = endpoint().await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let devices = Devices::open(store.clone()).unwrap();
    devices.add(device.peer_id(), "phone").unwrap();
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    let sessions = Sessions::new(store, adapters);
    let initial = sessions.create(new_session("initial", dir.path())).unwrap().id;
    let task = tokio::spawn(
        ember_server::transport::serve(&server, ember_server::api::router(sessions.clone()), devices.gate().clone())
            .unwrap(),
    );

    let addr = server.local_addr();
    let dialer = Dialer::new(device.clone());
    let api = Api::over_transport(dialer.clone(), addr.clone());
    assert!(tokio::time::timeout(Duration::from_secs(10), api.health()).await.unwrap().unwrap().ok);
    let connection = dialer.connection(&addr, SERVER_SERVICE).await.unwrap();
    let mut config = ClientConfig::new("http://unused.invalid");
    config.lease_interval = Duration::from_millis(50);
    config.backoff_min = Duration::from_millis(20);
    config.backoff_max = Duration::from_millis(200);
    config.resync_settle = Duration::from_millis(50);
    let client = Client::with_api(config, api).await;
    client.start();
    until("initial sync", || client.read(|s| s.session(&initial).is_some())).await;
    until("push connected", || client.read(|s| *s.connection() == ConnectionState::Connected)).await;
    let pushed = sessions.create(new_session("pushed", dir.path())).unwrap().id;
    until("session push", || client.read(|s| s.session(&pushed).is_some())).await;

    assert!(devices.remove(&device.peer_id()).unwrap() >= 1);
    let reason = tokio::time::timeout(Duration::from_secs(1), connection.closed())
        .await
        .expect("revoked connection stayed open for one second");
    assert!(matches!(reason, CloseReason::Remote { code: CLOSE_REVOKED, .. }), "{reason:?}");
    until("push disconnected", || {
        client.read(|s| matches!(s.connection(), ConnectionState::Reconnecting { .. } | ConnectionState::Connecting { .. }))
    })
    .await;
    assert!(tokio::time::timeout(Duration::from_secs(10), client.api().health()).await.unwrap().is_err());

    client.stop();
    device.close().await;
    server.close().await;
    task.abort();
}
