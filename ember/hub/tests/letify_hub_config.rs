//! SPEC FR-N2, FR-N5, INTENT D36: the hub's transport configuration keeps the backend choice and
//! never points at a separate relay VPS (`relay.darkpyonix.dev`); the main server's record on the
//! hub names its relay (#1044).

use std::time::{Duration, Instant};

use ember_hub::fake::FakeHub;
use ember_hub::{AddressRecord, HubConfig, Role};
use ember_transport::{BackendKind, LetifyConfig, RelayConfig, SecretKey, Transport};

#[test]
fn fr_n2_hub_config_preserves_letify_backend_and_drops_vps_relay() {
    let hub = HubConfig::new("https://api.darkpyonix.dev");
    assert!(hub.relay_urls().is_empty(), "no relay is derived from the hub host: {:?}", hub.relay_urls());
    assert_eq!(hub.relay_config(), RelayConfig::Default);
    let cfg = hub.transport_config(SecretKey::generate(), None);
    assert_eq!(cfg.backend, BackendKind::Letify);
    assert_eq!(cfg.relay, RelayConfig::Default);
    assert!(cfg.hub.is_some());
    let text = format!("{:?}", cfg.relay);
    assert!(!text.contains("relay.darkpyonix.dev"), "{text}");
}

#[test]
fn fr_n2_relay_comes_from_the_server_device_record() {
    let server = SecretKey::generate().peer_id();
    let url = format!("letify://{server}@203.0.113.9:7443");
    let record = AddressRecord {
        endpoint_id: server,
        relay_urls: vec![url.clone()],
        direct_addresses: vec![],
        published_at_us: 0,
        signed_packet: String::new(),
    };
    let hub = HubConfig::new("https://api.darkpyonix.dev");
    // EMBER_RELAY_URL is not set by this test; the explicit override is covered by relay_config.
    if std::env::var_os(ember_transport::RELAY_URL_ENV).is_none() {
        assert_eq!(hub.relay_config_for(Some(&record)), RelayConfig::Custom(vec![url]));
        assert_eq!(hub.relay_config_for(None), RelayConfig::Default);
    }
}

/// A letify endpoint publishing to `hub`, with the relay role when `relay_listen` is set.
async fn bind_on_hub(hub: &FakeHub, key: &SecretKey, relay: RelayConfig, relay_listen: bool) -> Transport {
    let letify = LetifyConfig { relay_listen: relay_listen.then(|| "127.0.0.1:0".parse().unwrap()), ..LetifyConfig::default() };
    let cfg = HubConfig::new(hub.url())
        .transport_config(key.clone(), hub.resolve_token(&key.peer_id()))
        .backend(BackendKind::Letify)
        .relay(relay)
        .bind_addr("127.0.0.1:0".parse().unwrap())
        .letify(letify);
    Transport::bind(cfg).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n2_the_server_record_names_its_relay() {
    let hub = FakeHub::start().await;
    let server_key = SecretKey::generate();
    let token = hub.register(server_key.peer_id(), "server", Role::MainServer);
    let server = bind_on_hub(&hub, &server_key, RelayConfig::Default, true).await;
    let relay = server.local_addr().relays.first().cloned().expect("the relay role advertises a relay");
    assert!(relay.starts_with(&format!("letify://{}@127.0.0.1:", server_key.peer_id())), "{relay}");

    // A computer of the account that uses the server's relay but is not one.
    let node_key = SecretKey::generate();
    hub.register(node_key.peer_id(), "node", Role::Computer);
    let node = bind_on_hub(&hub, &node_key, RelayConfig::Custom(vec![relay.clone()]), false).await;

    // The hub's view of the server's signed record carries the relay, as a letify:// URL.
    let client = hub.client().with_token(&token);
    let deadline = Instant::now() + Duration::from_secs(15);
    let record = loop {
        if let Ok(rec) = client.addresses(&server_key.peer_id()).await {
            if !rec.relay_urls.is_empty() {
                break rec;
            }
        }
        assert!(Instant::now() < deadline, "the server's record never named its relay");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(record.letify_relays(), vec![relay.clone()]);
    if std::env::var_os(ember_transport::RELAY_URL_ENV).is_none() {
        assert_eq!(HubConfig::new(hub.url()).relay_config_for(Some(&record)), RelayConfig::Custom(vec![relay]));
    }

    // The computer's record names no relay: only the relay itself publishes one.
    let deadline = Instant::now() + Duration::from_secs(15);
    let node_record = loop {
        if let Ok(rec) = client.addresses(&node_key.peer_id()).await {
            break rec;
        }
        assert!(Instant::now() < deadline, "the node never published");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(node_record.relay_urls.is_empty(), "{node_record:?}");
    node.close().await;
    server.close().await;
}
