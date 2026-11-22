//! SPEC FR-N9 (the pairing flow through the hub, #1044), FR-N2 (Defaults: a client finds the
//! relay URL in the main server's hub record): the app's own pairing code
//! (`ember_app::hub_pairing`) against `FakeHub` and a real letify main server on loopback that
//! publishes its record to the fake hub.
//!
//! The app registers its device key as a `client` device, lists the account's main servers,
//! and pairs with one: the saved server address is the hub's peer id and the saved relay the
//! `letify://` relay of the hub record. A device the hub removed cannot pair.

use std::path::Path;
use std::time::{Duration, Instant};

use ember_app::device_link::{self, PeerSetting};
use ember_app::hub_pairing::{self, REVOKED};
use ember_app::prefs::Prefs;
use ember_hub::fake::FakeHub;
use ember_hub::{HubConfig, Role};
use ember_transport::{BackendKind, LetifyConfig, PeerId, RelayConfig, SecretKey, Transport};

fn scratch() -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::tempdir_in(base).unwrap()
}

/// A letify main server with the relay role on loopback, registered on `hub`, whose record (with
/// its relay) is on the hub. Returns the transport and the relay URL it reports.
async fn main_server(hub: &FakeHub) -> (Transport, String) {
    let key = SecretKey::generate();
    let token = hub.register(key.peer_id(), "mini", Role::MainServer);
    let cfg = HubConfig::new(hub.url())
        .transport_config(key.clone(), hub.resolve_token(&key.peer_id()))
        .backend(BackendKind::Letify)
        .bind_addr("127.0.0.1:0".parse().unwrap())
        .letify(LetifyConfig { relay_listen: Some("127.0.0.1:0".parse().unwrap()), ..LetifyConfig::default() });
    let server = Transport::bind(cfg).await.unwrap();
    let relay = server.local_addr().relays.first().cloned().expect("the relay role reports its URL");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let rec = hub.client().with_token(&token).addresses(&server.peer_id()).await;
        if rec.is_ok_and(|r| r.relay_urls.contains(&relay)) {
            break;
        }
        assert!(Instant::now() < deadline, "the server's record never named its relay");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    (server, relay)
}

/// Registers the app's device key (in `data_dir`) on `hub`, approved as the signed-in person.
async fn register(hub: &FakeHub, data_dir: &Path) -> PeerId {
    let key = device_link::load_key(data_dir).unwrap();
    let link = hub_pairing::start(&HubConfig::new(hub.url()), &key, "phone").await.unwrap();
    assert!(!link.user_code().is_empty() && link.pending().verification_uri_complete.contains(link.user_code()));
    assert!(hub.approve(link.user_code()));
    let reg = hub_pairing::finish(&link, &hub_pairing::registration_file(data_dir)).await.unwrap();
    reg.endpoint_id()
}

#[tokio::test]
async fn fr_n9_the_app_registers_its_device_key_with_the_hub() {
    let hub = FakeHub::start().await;
    let dir = scratch();
    let me = register(&hub, dir.path()).await;
    assert_eq!(me, device_link::load_key(dir.path()).unwrap().peer_id(), "the hub id is the device key");
    let listed = hub.devices().into_iter().find(|d| d.endpoint_id == me).expect("the hub lists the app");
    assert_eq!(listed.role, Role::Client);

    // The registration is kept in <data dir>/hub.json, mode 0600.
    let file = hub_pairing::registration_file(dir.path());
    assert_eq!(file.path(), dir.path().join("hub.json"));
    let saved = file.load().unwrap().expect("saved");
    assert_eq!(saved.endpoint_id(), me);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(file.path()).unwrap().permissions().mode() & 0o777, 0o600);
    }
}

#[tokio::test]
async fn fr_n9_the_app_lists_the_accounts_one_server() {
    let hub = FakeHub::start().await;
    let server = SecretKey::generate().peer_id();
    hub.register(server, "mini", Role::MainServer);
    hub.register(SecretKey::generate().peer_id(), "build box", Role::Computer);
    let dir = scratch();
    register(&hub, dir.path()).await;

    let rows = hub_pairing::servers(&hub_pairing::registration_file(dir.path())).await.unwrap();
    assert_eq!(rows.len(), 1, "only the main server, not the computer nor the app itself: {rows:?}");
    assert_eq!(rows[0].peer, server);
    assert_eq!(rows[0].name, "mini");

    // Not registered yet: a Korean sentence, not a call to the hub.
    let empty = scratch();
    let e = hub_pairing::servers(&hub_pairing::registration_file(empty.path())).await.unwrap_err();
    assert!(!e.is_ascii(), "{e}");
}

#[tokio::test]
async fn fr_n9_pairing_saves_the_hubs_peer_id_and_relay() {
    let hub = FakeHub::start().await;
    let (server, relay) = main_server(&hub).await;
    let dir = scratch();
    register(&hub, dir.path()).await;

    let saved = hub_pairing::pair(&hub_pairing::registration_file(dir.path()), &server.peer_id()).await.unwrap();
    assert_eq!(saved.server_peer, server.peer_id().to_string(), "the hub's peer id, as 설정 stores it");
    assert_eq!(saved.server_relay.as_deref(), Some(relay.as_str()), "the relay of the hub record");

    // Stored in prefs as if typed in 설정, and read back as the transport reach at start.
    let mut prefs = Prefs::default();
    saved.apply(&mut prefs);
    assert_eq!(prefs.server_peer.as_deref(), Some(saved.server_peer.as_str()));
    assert_eq!(prefs.server_relay.as_deref(), Some(relay.as_str()));
    let setting = PeerSetting::parse(prefs.server_peer.as_deref().unwrap(), prefs.server_relay.as_deref()).unwrap();
    assert_eq!(setting.addr.peer, server.peer_id());
    assert_eq!(setting.relay, Some(RelayConfig::Custom(vec![relay])));
    server.close().await;
}

#[tokio::test]
async fn fr_n9_a_revoked_device_cannot_pair() {
    let hub = FakeHub::start().await;
    let server = SecretKey::generate().peer_id();
    hub.register(server, "mini", Role::MainServer);
    let dir = scratch();
    let me = register(&hub, dir.path()).await;
    let file = hub_pairing::registration_file(dir.path());

    assert!(hub.remove(&me), "the owner removes the app on the hub");
    assert_eq!(hub_pairing::pair(&file, &server).await.unwrap_err(), REVOKED);
    assert!(file.load().unwrap().unwrap().is_revoked(), "the revocation is recorded in hub.json");
    assert_eq!(hub_pairing::servers(&file).await.unwrap_err(), REVOKED);

    // Linking the same key again is refused until the owner re-admits it (FR-H11).
    let key = device_link::load_key(dir.path()).unwrap();
    let again = hub_pairing::start(&HubConfig::new(hub.url()), &key, "phone").await;
    assert!(again.is_err_and(|t| !t.is_ascii()), "refused with a Korean sentence");
}
