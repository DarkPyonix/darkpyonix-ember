//! SPEC FR-N2, INTENT D36: a main server on the letify backend is the relay and the address
//! observer without further configuration.

use ember_server::transport::{with_main_server_roles, DEFAULT_RELAY_PORT};
use ember_transport::{BackendKind, LetifyConfig, ObserverAddr, SecretKey, Transport, TransportConfig};
use std::time::{Duration, Instant};

#[test]
fn fr_n2_default_relay_port_is_7443() {
    assert_eq!(DEFAULT_RELAY_PORT, 7443);
}

#[test]
fn fr_n2_roles_respect_explicit_settings_and_other_backends() {
    let explicit = "127.0.0.1:9".parse().unwrap();
    let mut cfg = TransportConfig::new(SecretKey::generate()).backend(BackendKind::Letify);
    cfg.letify.relay_listen = Some(explicit);
    let cfg = with_main_server_roles(cfg, 0);
    assert_eq!(cfg.letify.relay_listen, Some(explicit), "an explicit relay address is kept");
    assert!(cfg.letify.qad_listen.is_some(), "the observer is filled in");

    let iroh = TransportConfig::new(SecretKey::generate()).backend(BackendKind::Iroh);
    let iroh = with_main_server_roles(iroh, 0);
    assert!(iroh.letify.relay_listen.is_none() && iroh.letify.qad_listen.is_none());
}

#[tokio::test]
async fn fr_n2_server_bind_automatically_runs_relay_and_qad() {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let qad = socket.local_addr().unwrap();
    drop(socket);
    let cfg = TransportConfig::new(SecretKey::generate())
        .backend(BackendKind::Letify)
        .bind_addr("127.0.0.1:0".parse().unwrap());
    assert!(cfg.letify.relay_listen.is_none(), "no explicit relay listen address");
    let t = Transport::bind(with_main_server_roles(cfg, qad.port())).await.unwrap();
    let addr = t.local_addr();
    assert!(
        addr.relays.iter().any(|r| r.starts_with("letify://")),
        "the server publishes itself as a letify relay: {addr:?}"
    );
    let device = Transport::bind(
        TransportConfig::new(SecretKey::generate())
            .backend(BackendKind::Letify)
            .bind_addr("0.0.0.0:0".parse().unwrap())
            .letify(LetifyConfig {
                observer: Some(ObserverAddr { addr: qad, node: Some(t.peer_id()) }),
                ..LetifyConfig::default()
            }),
    )
    .await
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let direct = device.local_addr().direct;
        if !direct.is_empty() {
            assert!(direct.iter().all(|addr| addr.ip().is_loopback()), "{direct:?}");
            break;
        }
        assert!(Instant::now() < deadline, "the QAD responder did not observe the device");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
