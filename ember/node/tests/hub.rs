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
    // The resolver URL gets the read-only resolve token, never the device token (NFR-H2).
    assert!(reg.resolve_token.as_deref().is_some_and(|t| t.starts_with("dpr_")));
    assert_eq!(h.token, reg.resolve_token);
    assert_ne!(h.token.as_deref(), Some(reg.device_token.as_str()));
    // The node reported its app with its own token (FR-H10).
    let app = fake.devices().into_iter().find(|d| d.endpoint_id == key.peer_id()).unwrap().app.expect("app");
    assert_eq!((app.kind.as_str(), app.services.clone()), ("ember-node", vec!["ember-node-v1".to_string()]));

    // With the hub's /v1/config, its advertised relay is used (an IP-address hub derives none).
    fake.set_config_relays(vec!["https://relay.example.net".into()]);
    let cfg = hub::discovered_transport_config(dir.path(), key.clone()).await.unwrap();
    assert_eq!(cfg.hub.unwrap().pkarr_url, format!("{}/pkarr", fake.url()));
    if std::env::var_os(ember_transport::RELAY_URL_ENV).is_none() && std::env::var_os(ember_hub::HUB_RELAY_URL_ENV).is_none() {
        assert_eq!(cfg.relay, ember_transport::RelayConfig::Custom(vec!["https://relay.example.net".into()]));
    }
    // Without it (404), the derived configuration, as before.
    fake.set_config_enabled(false);
    let cfg = hub::discovered_transport_config(dir.path(), key.clone()).await.unwrap();
    let plain = hub::transport_config(dir.path(), key.clone()).unwrap();
    assert_eq!(cfg.relay, plain.relay);
    assert_eq!(cfg.hub.unwrap().pkarr_url, plain.hub.unwrap().pkarr_url);
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
        // A registration from before resolve tokens: the watch / transport fetch one.
        resolve_token: None,
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

    // A revoked key cannot register again until the owner re-admits it.
    let err = hub::register(dir.path(), &key, &HubConfig::new(fake.url()), "again", None, |_| {}).await.unwrap_err();
    assert!(err.to_string().contains("removed") && err.to_string().contains("re-admit"), "{err}");

    // Re-admitted: the same key links again; the person approves in the browser.
    assert!(fake.readmit(&key.peer_id()));
    let browser = fake.browser();
    let reg = hub::register(dir.path(), &key, &HubConfig::new(fake.url()), "again", Some(Duration::from_millis(50)), |p| {
        let (browser, code) = (browser.clone(), p.user_code.clone());
        tokio::spawn(async move { browser.decide_link_code(&code, true).await.unwrap() });
    })
    .await
    .unwrap();
    assert!(!reg.is_revoked());
    assert!(hub::active_registration(dir.path()).unwrap().is_some());
}

#[tokio::test]
async fn an_old_registration_gets_a_resolve_token_and_forget_leaves_the_account() {
    let fake = FakeHub::start().await;
    let dir = tempfile::tempdir().unwrap();
    let key = SecretKey::generate();
    let token = fake.register(key.peer_id(), "node", Role::Computer);
    let reg = ember_hub::Registration {
        hub_url: fake.url().to_string(),
        device: fake.devices().into_iter().find(|d| d.endpoint_id == key.peer_id()).unwrap(),
        device_token: token,
        resolve_token: None,
        registered_at: ember_hub::now_secs(),
        revoked_at: None,
    };
    hub::registration_file(dir.path()).save(&reg).unwrap();

    // Without one, the synchronous config resolves nothing; binding fetches one first.
    assert_eq!(hub::transport_config(dir.path(), key.clone()).unwrap().hub.unwrap().token, None);
    let cfg = hub::discovered_transport_config(dir.path(), key.clone()).await.unwrap();
    let resolve = cfg.hub.unwrap().token.expect("resolve token fetched");
    assert_eq!(Some(resolve.clone()), fake.resolve_token(&key.peer_id()));
    assert_eq!(hub::active_registration(dir.path()).unwrap().unwrap().resolve_token, Some(resolve));

    // forget --local keeps the device on the hub; forget removes it there.
    assert!(hub::leave(dir.path(), true).await.unwrap());
    assert!(fake.devices().iter().any(|d| d.endpoint_id == key.peer_id()));
    hub::registration_file(dir.path()).save(&reg).unwrap();
    assert!(hub::leave(dir.path(), false).await.unwrap());
    assert!(fake.devices().iter().all(|d| d.endpoint_id != key.peer_id()), "removed on the hub with its own token");
    assert!(hub::registration_file(dir.path()).load().unwrap().is_none());
}

#[tokio::test]
async fn a_register_interrupted_while_waiting_resumes_the_same_link() {
    let fake = FakeHub::start().await;
    let dir = tempfile::tempdir().unwrap();
    let key = SecretKey::generate();
    let config = HubConfig::new(fake.url());

    // The first run shows a code and is interrupted before approval.
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    let first = {
        let (dir, key, config) = (dir.path().to_path_buf(), key.clone(), config.clone());
        tokio::spawn(async move {
            hub::register(&dir, &key, &config, "pi", Some(Duration::from_millis(50)), |p| {
                tx.send(p.user_code.clone()).unwrap();
            })
            .await
        })
    };
    let code = rx.await.unwrap();
    first.abort();
    let _ = first.await;
    assert!(dir.path().join(hub::PENDING_LINK_FILE).exists());

    // The second run shows the same code (no new link) and completes on approval.
    let browser = fake.browser();
    let reg = hub::register(dir.path(), &key, &config, "pi", Some(Duration::from_millis(50)), |p| {
        assert_eq!(p.user_code, code, "resumed, not restarted");
        let (browser, code) = (browser.clone(), p.user_code.clone());
        tokio::spawn(async move { browser.decide_link_code(&code, true).await.unwrap() });
    })
    .await
    .unwrap();
    assert_eq!(reg.endpoint_id(), key.peer_id());
    assert_eq!(fake.pending_codes().len(), 0);
    assert!(!dir.path().join(hub::PENDING_LINK_FILE).exists());
}
