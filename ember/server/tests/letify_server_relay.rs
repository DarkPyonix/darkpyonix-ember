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
}

/// A port that is free for TCP and for UDP on every address: the server uses one number for the relay (TCP) and
/// the observer (UDP). Asking the OS for a UDP port on 127.0.0.1 only is not enough: the same number can be in use
/// for TCP, for instance as the local port of another test's connection.
fn port_free_for_tcp_and_udp() -> u16 {
    for _ in 0..50 {
        let tcp = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let port = tcp.local_addr().unwrap().port();
        if std::net::UdpSocket::bind(("0.0.0.0", port)).is_ok() {
            return port;
        }
    }
    panic!("no port free for both TCP and UDP");
}

#[tokio::test]
async fn fr_n2_server_bind_automatically_runs_relay_and_qad() {
    // The port is chosen, released and bound again, so another process can take it in between: try again with
    // another port when the bind says the address is in use (a few times, not forever).
    let mut attempt = 0;
    let (t, qad) = loop {
        attempt += 1;
        let port = port_free_for_tcp_and_udp();
        let qad: std::net::SocketAddr = ([127, 0, 0, 1], port).into();
        let cfg = TransportConfig::new(SecretKey::generate())
            .backend(BackendKind::Letify)
            .bind_addr("127.0.0.1:0".parse().unwrap());
        assert!(cfg.letify.relay_listen.is_none(), "no explicit relay listen address");
        match Transport::bind(with_main_server_roles(cfg, port)).await {
            Ok(t) => break (t, qad),
            Err(e) if attempt < 10 && e.to_string().contains("Address already in use") => continue,
            Err(e) => panic!("bind failed after {attempt} attempt(s): {e}"),
        }
    };
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
