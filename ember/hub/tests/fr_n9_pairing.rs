//! SPEC FR-N9 (the pairing flow through the hub, #1044) and FR-N2 (Defaults: a client finds the
//! relay URL in the main server's device record on the hub), against `FakeHub` and a real
//! letify main server on loopback that publishes its record to the fake hub.
//!
//! The flow: the device links itself as a `client` (approved by the signed-in person), lists the
//! account's main servers with its device token, and pairs with one: the server's peer id and
//! the `letify://` relay come from the hub record, not from a typed value.

use std::time::{Duration, Instant};

use ember_hub::fake::FakeHub;
use ember_hub::pairing::{self, PairError};
use ember_hub::{DeviceLink, HubConfig, LinkError, Registration, Role};
use ember_transport::{BackendKind, LetifyConfig, PeerId, SecretKey, Transport};

/// A letify main server with the relay role on loopback, registered on `hub` and publishing its
/// record there. Returns the transport, its device token and the relay URL it reports.
async fn main_server(hub: &FakeHub, name: &str) -> (Transport, String, String) {
    let key = SecretKey::generate();
    let token = hub.register(key.peer_id(), name, Role::MainServer);
    let cfg = HubConfig::new(hub.url())
        .transport_config(key.clone(), hub.resolve_token(&key.peer_id()))
        .backend(BackendKind::Letify)
        .bind_addr("127.0.0.1:0".parse().unwrap())
        .letify(LetifyConfig { relay_listen: Some("127.0.0.1:0".parse().unwrap()), ..LetifyConfig::default() });
    let server = Transport::bind(cfg).await.unwrap();
    let relay = server.local_addr().relays.first().cloned().expect("the relay role reports its URL");
    (server, token, relay)
}

/// Waits until the hub's view of `peer`'s record names a relay.
async fn wait_relay_published(hub: &FakeHub, token: &str, peer: &PeerId) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(rec) = hub.client().with_token(token).addresses(peer).await {
            if !rec.relay_urls.is_empty() {
                return rec.relay_urls;
            }
        }
        assert!(Instant::now() < deadline, "the main server's record never named its relay");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Links `key` as a `client` device, approved as the signed-in person would on the hub.
async fn link_client(hub: &FakeHub, key: &SecretKey) -> Registration {
    let link = DeviceLink::start(&hub.client(), key, "phone", Role::Client).await.unwrap();
    assert!(hub.approve(link.user_code()), "the code is pending on the hub");
    link.with_poll_interval(Duration::from_millis(50)).wait().await.unwrap()
}

#[tokio::test]
async fn fr_n2_the_main_servers_hub_record_names_its_relay() {
    let hub = FakeHub::start().await;
    let (server, token, relay) = main_server(&hub, "mini").await;
    assert!(relay.starts_with(&format!("letify://{}@127.0.0.1:", server.peer_id())), "{relay}");
    let published = wait_relay_published(&hub, &token, &server.peer_id()).await;
    assert_eq!(published, vec![relay], "the record carries the relay the server reports");
    server.close().await;
}

#[tokio::test]
async fn fr_n9_a_client_registers_lists_one_server_and_pairs_with_the_hubs_peer_id_and_relay() {
    let hub = FakeHub::start().await;
    let (server, token, relay) = main_server(&hub, "mini").await;
    // A computer of the same account is not a server to pair with.
    hub.register(SecretKey::generate().peer_id(), "build box", Role::Computer);
    wait_relay_published(&hub, &token, &server.peer_id()).await;

    let key = SecretKey::generate();
    let reg = link_client(&hub, &key).await;
    assert_eq!(reg.device.role, Role::Client);
    assert_eq!(reg.endpoint_id(), key.peer_id());

    let servers = pairing::servers(&reg).await.unwrap();
    assert_eq!(servers.len(), 1, "only the main server is offered: {servers:?}");
    assert_eq!(servers[0].endpoint_id, server.peer_id());
    assert_eq!(servers[0].name, "mini");

    let paired = pairing::pair(&reg, &server.peer_id()).await.unwrap();
    assert_eq!(paired.server, server.peer_id(), "the peer id is the hub's, not a typed one");
    assert_eq!(paired.relay.as_deref(), Some(relay.as_str()), "the relay comes from the hub record");
    server.close().await;
}

#[tokio::test]
async fn fr_n9_a_server_with_no_record_pairs_without_a_relay_and_a_computer_is_refused() {
    let hub = FakeHub::start().await;
    let server = SecretKey::generate().peer_id();
    hub.register(server, "mini", Role::MainServer);
    let computer = SecretKey::generate().peer_id();
    hub.register(computer, "build box", Role::Computer);
    let reg = link_client(&hub, &SecretKey::generate()).await;

    let paired = pairing::pair(&reg, &server).await.unwrap();
    assert_eq!(paired.server, server);
    assert_eq!(paired.relay, None, "nothing published yet: the relay is left to the environment");
    assert!(matches!(pairing::pair(&reg, &computer).await, Err(PairError::NotAServer(p)) if p == computer));
}

#[tokio::test]
async fn fr_n9_a_revoked_device_cannot_pair() {
    let hub = FakeHub::start().await;
    let server = SecretKey::generate().peer_id();
    hub.register(server, "mini", Role::MainServer);
    let key = SecretKey::generate();
    let reg = link_client(&hub, &key).await;
    assert!(pairing::pair(&reg, &server).await.is_ok());

    // The owner removes the device on the hub.
    assert!(hub.remove(&key.peer_id()));
    assert!(matches!(pairing::servers(&reg).await, Err(PairError::Revoked)));
    assert!(matches!(pairing::pair(&reg, &server).await, Err(PairError::Revoked)));
    // Linking the same key again is refused until the owner re-admits it (FR-H11).
    let again = DeviceLink::start(&hub.client(), &key, "phone", Role::Client).await;
    assert!(matches!(again, Err(LinkError::AlreadyRegistered(_))), "{again:?}");

    // A registration already marked revoked is not sent to the hub at all.
    let mut marked = reg.clone();
    marked.revoked_at = Some(1);
    assert!(matches!(pairing::pair(&marked, &server).await, Err(PairError::Revoked)));
}
