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

/// A port that is free for both TCP and UDP on 0.0.0.0 at this moment (it can still be taken before
/// the caller binds: the caller retries).
fn free_tcp_and_udp_port() -> u16 {
    loop {
        let tcp = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let port = tcp.local_addr().unwrap().port();
        if std::net::UdpSocket::bind(("0.0.0.0", port)).is_ok() {
            return port;
        }
    }
}

#[tokio::test]
async fn fr_n2_server_bind_automatically_runs_relay_and_qad() {
    // The server uses ONE number for the relay (TCP 0.0.0.0:port) and the observer (UDP
    // 0.0.0.0:port). A port that is free for UDP on 127.0.0.1 can be taken for TCP on 0.0.0.0 (an
    // ephemeral port of another test's connection), and the gap between choosing and binding is a
    // race too (issue #682): choose a port free for both protocols on 0.0.0.0 and, if the bind
    // still loses the race, choose another and bind again.
    let mut bound = None;
    for _ in 0..8 {
        let port = free_tcp_and_udp_port();
        let cfg = TransportConfig::new(SecretKey::generate())
            .backend(BackendKind::Letify)
            .bind_addr("127.0.0.1:0".parse().unwrap());
        assert!(cfg.letify.relay_listen.is_none(), "no explicit relay listen address");
        match Transport::bind(with_main_server_roles(cfg, port)).await {
            Ok(t) => {
                bound = Some((t, port));
                break;
            }
            Err(e) if format!("{e:?}").contains("Address already in use") => continue,
            Err(e) => panic!("the server transport did not bind: {e:?}"),
        }
    }
    let (t, port) = bound.expect("no free port for the relay and the observer in 8 tries");
    let qad = std::net::SocketAddr::from(([127, 0, 0, 1], port));
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
