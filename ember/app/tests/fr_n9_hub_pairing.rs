//! SPEC FR-N9, FR-N2 (#1044): the app registers its device key with the hub as a `client`
//! device and pairs with a main server of the account, against `FakeHub`. The main server is a
//! real loopback letify endpoint with its relay role on, publishing its signed record to the
//! fake hub, so the relay the app saves is the one the server's record names.

use std::time::{Duration, Instant};

use ember_app::device_link::{self, PeerSetting};
use ember_app::hub_pairing::{self, Approval, HubPairError, DEVICE_NAME};
use ember_app::prefs::Prefs;
use ember_hub::fake::FakeHub;
use ember_hub::{HubConfig, Registration, Role};
use ember_transport::{BackendKind, LetifyConfig, RelayConfig, SecretKey, Transport};

fn scratch() -> tempfile::TempDir {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::tempdir_in(base).unwrap()
}

/// A main server of the hub's account: letify with the relay role, publishing to the fake hub.
/// Returns it with the relay URL it advertises, once the hub's record of it names that relay.
async fn main_server(hub: &FakeHub) -> (Transport, String) {
    let key = SecretKey::generate();
    let token = hub.register(key.peer_id(), "home server", Role::MainServer);
    let cfg = HubConfig::new(hub.url())
        .transport_config(key.clone(), hub.resolve_token(&key.peer_id()))
        .backend(BackendKind::Letify)
        .bind_addr("127.0.0.1:0".parse().unwrap())
        .letify(LetifyConfig { relay_listen: Some("127.0.0.1:0".parse().unwrap()), ..LetifyConfig::default() });
    let server = Transport::bind(cfg).await.unwrap();
    let relay = server.local_addr().relays.first().cloned().expect("the relay role advertises a relay");
    let client = hub.client().with_token(&token);
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(rec) = client.addresses(&key.peer_id()).await {
            if !rec.letify_relays().is_empty() {
                break;
            }
        }
        assert!(Instant::now() < deadline, "the server's record never named its relay on the hub");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    (server, relay)
}

/// Registers the app's device key (made in `data_dir`) through the device-link flow, approved
/// in the "browser"; returns the approval shown and the stored registration.
async fn register(hub: &FakeHub, data_dir: &std::path::Path) -> (Approval, Registration) {
    let key = device_link::load_key(data_dir).unwrap();
    let link = hub_pairing::start(&hub.client(), &key, DEVICE_NAME)
        .await
        .unwrap()
        .with_poll_interval(Duration::from_millis(20));
    let approval = Approval::of(&link);
    assert!(hub.approve(&approval.user_code), "the fake hub knows the code");
    let file = hub_pairing::registration_file(data_dir);
    let reg = tokio::time::timeout(Duration::from_secs(10), hub_pairing::finish(&link, &file)).await.unwrap().unwrap();
    (approval, reg)
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n9_a_device_registers_with_the_hub_as_a_client() {
    let hub = FakeHub::start().await;
    let dir = scratch();
    let (approval, reg) = register(&hub, dir.path()).await;

    // What 설정 shows: the user code and the approval URL with the code in it.
    assert_eq!(approval.user_code.len(), 9, "{approval:?}");
    assert!(approval.url.starts_with(hub.url()) && approval.url.contains(&approval.user_code), "{approval:?}");

    // The device key is the one in <data dir>/device.key, registered as a client.
    let me = device_link::load_key(dir.path()).unwrap().peer_id();
    assert_eq!(reg.endpoint_id(), me);
    assert_eq!(reg.device.role, Role::Client);
    assert_eq!(reg.device.name, DEVICE_NAME);
    assert!(hub.devices().iter().any(|d| d.endpoint_id == me && d.role == Role::Client));

    // Kept in <data dir>/hub.json, mode 0600, and read back.
    let path = dir.path().join("hub.json");
    assert_eq!(hub_pairing::registration_file(dir.path()).path(), path);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    let loaded = hub_pairing::load(dir.path()).expect("the registration was kept");
    assert_eq!(loaded.endpoint_id(), me);
    assert!(!loaded.is_revoked());
    assert!(!std::fs::read_to_string(&path).unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n9_fr_n2_it_lists_the_one_main_server_with_its_relay() {
    let hub = FakeHub::start().await;
    let (server, relay) = main_server(&hub).await;
    // Another device of the account that is not a main server.
    hub.register(SecretKey::generate().peer_id(), "build box", Role::Computer);
    let dir = scratch();
    let (_, reg) = register(&hub, dir.path()).await;

    let servers = hub_pairing::main_servers(&reg, Some(&hub_pairing::registration_file(dir.path()))).await.unwrap();
    assert_eq!(servers.len(), 1, "{servers:?}");
    assert_eq!(servers[0].peer, server.peer_id());
    assert_eq!(servers[0].name, "home server");
    assert_eq!(servers[0].relay.as_deref(), Some(relay.as_str()));
    server.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n9_fr_n2_pairing_stores_the_hubs_peer_id_and_relay() {
    let hub = FakeHub::start().await;
    let (server, relay) = main_server(&hub).await;
    let computer = SecretKey::generate().peer_id();
    hub.register(computer, "build box", Role::Computer);
    let dir = scratch();
    let (_, reg) = register(&hub, dir.path()).await;
    let file = hub_pairing::registration_file(dir.path());

    let pairing = hub_pairing::pair(&reg, Some(&file), &server.peer_id()).await.unwrap();
    assert_eq!(pairing.peer, server.peer_id().to_string(), "the saved address is the hub record's peer id");
    assert_eq!(pairing.relay, relay, "the saved relay is the one in the server's hub record");
    assert!(pairing.relay.starts_with(&format!("letify://{}@", server.peer_id())));

    // Saved as the server address and relay of 설정, read at the next start like typed values.
    let mut prefs = Prefs::default();
    pairing.apply(&mut prefs);
    assert_eq!(prefs.server_peer.as_deref(), Some(pairing.peer.as_str()));
    assert_eq!(prefs.server_relay.as_deref(), Some(relay.as_str()));
    let setting = PeerSetting::parse(prefs.server_peer.as_deref().unwrap(), prefs.server_relay.as_deref()).unwrap();
    assert_eq!(setting.addr.peer, server.peer_id());
    assert!(setting.addr.direct.is_empty(), "only the peer id is saved");
    assert_eq!(setting.relay, Some(RelayConfig::Custom(vec![relay])));

    // A device of the account that is not a main server is not paired with.
    assert!(matches!(hub_pairing::pair(&reg, Some(&file), &computer).await, Err(HubPairError::NotAServer(_))));
    server.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n9_a_device_the_hub_removed_cannot_pair() {
    let hub = FakeHub::start().await;
    let (server, _) = main_server(&hub).await;
    let dir = scratch();
    let (_, reg) = register(&hub, dir.path()).await;
    let file = hub_pairing::registration_file(dir.path());

    assert!(hub.remove(&reg.endpoint_id()), "the owner removes the app on the hub");
    let err = hub_pairing::pair(&reg, Some(&file), &server.peer_id()).await.unwrap_err();
    assert!(matches!(err, HubPairError::Removed), "{err:?}");
    assert_eq!(err.text_ko(), hub_pairing::REMOVED);
    assert!(matches!(hub_pairing::main_servers(&reg, Some(&file)).await, Err(HubPairError::Removed)));

    // The removal is kept in hub.json, and the stored registration is refused without asking.
    let stored = hub_pairing::load(dir.path()).unwrap();
    assert!(stored.is_revoked(), "revoked_at is recorded");
    assert!(matches!(hub_pairing::pair(&stored, Some(&file), &server.peer_id()).await, Err(HubPairError::Removed)));
    server.close().await;
}
