//! ember node and the darkpyonix.dev hub (SPEC FR-N2), against the in-process fake hub:
//! registering through a device link (token in a 0600 file), the transport configuration that
//! follows, admitting the account's servers (opt-in) and noticing removal on the hub.

use std::time::{Duration, Instant};

use ember_hub::fake::FakeHub;
use ember_hub::{HubConfig, Role};
use ember_node::hub;
use ember_transport::mem::MemNetwork;
use ember_transport::{PeerGate, SecretKey};

async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn register_stores_a_private_registration_and_configures_the_transport() {
    let fake = FakeHub::start().await;
    let dir = tempfile::tempdir().unwrap();
    let key = SecretKey::generate();
    let config = HubConfig::new(fake.url());

    // The person approves the code as soon as it is shown.
    let (code_tx, code_rx) = tokio::sync::oneshot::channel::<String>();
    let browser = fake.browser();
    let approver = tokio::spawn(async move {
        let code = code_rx.await.unwrap();
        browser.decide_link_code(&code, true).await.unwrap();
    });
    let mut shown = None;
    let reg = hub::register(dir.path(), &key, &config, "gpu box", Some(Duration::from_millis(50)), |p| {
        shown = Some(p.clone());
        code_tx.send(p.user_code.clone()).unwrap();
    })
    .await
    .unwrap();
    approver.await.unwrap();
    let shown = shown.unwrap();
    assert!(shown.verification_uri_complete.contains(&shown.user_code));
    assert_eq!(reg.endpoint_id(), key.peer_id());
    assert_eq!(reg.device.role, Role::Computer);

    // Stored 0600, with the token.
    let file = hub::registration_file(dir.path());
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(file.path()).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(file.load().unwrap().unwrap(), reg);
    // Registering again is a no-op.
    let again = hub::register(dir.path(), &key, &config, "x", None, |_| panic!("no new link")).await.unwrap();
    assert_eq!(again, reg);

    // The transport then uses the hub's directory with the token.
    let cfg = hub::transport_config(dir.path(), key.clone()).unwrap();
    let h = cfg.hub.expect("hub directory configured");
    assert_eq!(h.pkarr_url, format!("{}/pkarr", fake.url()));
    assert_eq!(h.token.as_deref(), Some(reg.device_token.as_str()));
}

#[tokio::test]
async fn removal_on_the_hub_is_detected_and_hub_servers_are_admitted_until_then() {
    let fake = FakeHub::start().await;
    let dir = tempfile::tempdir().unwrap();
    let net = MemNetwork::new();
    let key = SecretKey::generate();
    let t = net.transport_with_key(key.clone());

    // Registered (shortcut) and stored as `ember-node hub register` would.
    let token = fake.register(key.peer_id(), "node", Role::Computer);
    let reg = ember_hub::Registration {
        hub_url: fake.url().to_string(),
        device: fake.devices().into_iter().find(|d| d.endpoint_id == key.peer_id()).unwrap(),
        device_token: token,
        registered_at: ember_hub::now_secs(),
        revoked_at: None,
    };
    hub::registration_file(dir.path()).save(&reg).unwrap();

    let pinned = SecretKey::generate().peer_id(); // from allowed-peers
    let server = SecretKey::generate().peer_id();
    fake.register(server, "home server", Role::MainServer);
    let gate = PeerGate::allow_list([pinned]);
    let task = hub::spawn_watch(dir.path(), t.clone(), gate.clone(), vec![pinned], true, Duration::from_millis(30));

    // The account's main server is admitted without being copied into allowed-peers.
    eventually("hub server admitted", || gate.is_allowed(&server)).await;
    assert!(gate.is_allowed(&pinned));

    // The owner removes the node on the hub.
    assert!(fake.remove(&key.peer_id()));
    tokio::time::timeout(Duration::from_secs(10), task).await.unwrap().unwrap();
    let stored = hub::registration_file(dir.path()).load().unwrap().unwrap();
    assert!(stored.is_revoked(), "revocation recorded in hub.json");
    assert!(hub::active_registration(dir.path()).unwrap().is_none());
    assert!(!gate.is_allowed(&server), "servers admitted through the hub are dropped");
    assert!(gate.is_allowed(&pinned), "allowed-peers entries stay");

    // A revoked key cannot register again.
    let err = hub::register(dir.path(), &key, &HubConfig::new(fake.url()), "again", None, |_| {}).await.unwrap_err();
    assert!(err.to_string().contains("removed"), "{err}");
}
