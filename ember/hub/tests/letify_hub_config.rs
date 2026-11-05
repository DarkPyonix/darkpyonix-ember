//! SPEC FR-N2, FR-N5, INTENT D36: the hub's transport configuration keeps the backend choice and
//! never points at a separate relay VPS (`relay.darkpyonix.dev`).

use ember_hub::{AddressRecord, HubConfig};
use ember_transport::{BackendKind, RelayConfig, SecretKey};

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
