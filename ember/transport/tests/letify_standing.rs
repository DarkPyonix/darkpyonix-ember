//! `FR-N9` (#898): a device that is given only the main server's peer id and the server's own
//! relay URL reaches the server through that relay. It has no direct address of the server, no
//! address directory and no rendezvous (no hub), which is a phone on mobile data. The server
//! keeps a request waiting at its own relay for every device it allows, so the relay has a
//! counterpart to pair the device with.
//!
//! What this network models: real TCP on loopback, the real letify-link relay and sessions. What
//! it does not model: a NAT or a router between the device and the relay port.
#![cfg(feature = "letify")]

mod common;

use std::time::{Duration, Instant};

use common::{echo_roundtrip, spawn_echo};
use ember_transport::{
    BackendKind, LetifyConfig, PathState, PeerAddr, PeerGate, RelayConfig, SecretKey, Transport, TransportConfig,
    TransportError,
};

/// A loopback TCP port that nothing listens on.
fn dead_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A main server with the relay role on loopback, and the relay URL it reports.
async fn main_server() -> (Transport, String) {
    let server = Transport::bind(
        TransportConfig::new(SecretKey::generate())
            .backend(BackendKind::Letify)
            .bind_addr("127.0.0.1:0".parse().unwrap())
            .letify(LetifyConfig { relay_listen: Some("127.0.0.1:0".parse().unwrap()), ..LetifyConfig::default() }),
    )
    .await
    .unwrap();
    let relay_url = server.local_addr().relays.remove(0);
    (server, relay_url)
}

/// A device that knows the relay URL and nothing else: no rendezvous, no directory, no hub.
async fn device(key: SecretKey, relay_url: &str, connect_timeout: Duration) -> Transport {
    let letify = LetifyConfig { connect_timeout, ..LetifyConfig::default() };
    Transport::bind(
        TransportConfig::new(key)
            .backend(BackendKind::Letify)
            .bind_addr("127.0.0.1:0".parse().unwrap())
            .relay(RelayConfig::Custom(vec![relay_url.to_string()]))
            .letify(letify),
    )
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n9_a_device_with_only_the_relay_url_reaches_the_main_server() {
    let (server, relay_url) = main_server().await;
    spawn_echo(&server, "svc");
    let key = SecretKey::generate();
    let gate = PeerGate::allow_list([key.peer_id()]);
    gate.attach(&server);

    let phone = device(key, &relay_url, Duration::from_secs(20)).await;
    // Only the peer id: no direct address and no relay hint in the address itself.
    let addr = PeerAddr::new(server.peer_id());
    assert!(addr.is_empty());
    let conn = tokio::time::timeout(Duration::from_secs(20), phone.connect(addr, "svc"))
        .await
        .expect("connect through the server's own relay timed out")
        .expect("a device with only the relay URL connects");
    assert_eq!(conn.peer(), server.peer_id());
    assert_eq!(echo_roundtrip(&conn, b"through the server's own relay").await, b"through the server's own relay");
    let big = vec![7u8; 1 << 20];
    assert_eq!(echo_roundtrip(&conn, &big).await, big);

    // The only path there is goes through the relay.
    let settle = Instant::now() + Duration::from_secs(5);
    while !matches!(conn.path_state(), PathState::Relayed { .. }) {
        assert!(Instant::now() < settle, "the connection never reported a relayed path: {:?}", conn.path_state());
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // A second device allowed later (as `owner devices add` does while the server runs).
    let later = SecretKey::generate();
    gate.allow(later.peer_id());
    let tablet = device(later, &relay_url, Duration::from_secs(20)).await;
    let conn2 = tokio::time::timeout(Duration::from_secs(20), tablet.connect(server.peer_id(), "svc"))
        .await
        .expect("the second device timed out")
        .expect("a device allowed while the server runs connects");
    assert_eq!(echo_roundtrip(&conn2, b"second").await, b"second");
    // The first device's connection is undisturbed.
    assert_eq!(echo_roundtrip(&conn, b"still here").await, b"still here");
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n9_the_same_device_reaches_the_server_again_after_its_transport_is_bound_again() {
    let (server, relay_url) = main_server().await;
    spawn_echo(&server, "svc");
    let key = SecretKey::generate();
    PeerGate::allow_list([key.peer_id()]).attach(&server);

    // The standing request is posted again after each pairing, so a later bind of the same
    // device (an app restart) is paired too.
    for round in 0..3 {
        let phone = device(key.clone(), &relay_url, Duration::from_secs(20)).await;
        let conn = tokio::time::timeout(Duration::from_secs(20), phone.connect(server.peer_id(), "svc"))
            .await
            .unwrap_or_else(|_| panic!("round {round} timed out"))
            .unwrap_or_else(|e| panic!("round {round}: {e}"));
        assert_eq!(echo_roundtrip(&conn, b"again").await, b"again");
        phone.close().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n9_a_device_the_server_did_not_allow_is_refused_at_the_relay() {
    let (server, relay_url) = main_server().await;
    spawn_echo(&server, "svc");
    let allowed = SecretKey::generate();
    PeerGate::allow_list([allowed.peer_id()]).attach(&server);

    let stranger = device(SecretKey::generate(), &relay_url, Duration::from_secs(3)).await;
    let started = Instant::now();
    let refused = tokio::time::timeout(Duration::from_secs(15), stranger.connect(server.peer_id(), "svc"))
        .await
        .expect("a refused device fails within its connect timeout");
    assert!(matches!(refused, Err(TransportError::Connect { .. })), "{refused:?}");
    assert!(started.elapsed() < Duration::from_secs(12), "took {:?}", started.elapsed());
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n9_a_wrong_relay_address_fails_within_the_connect_timeout() {
    let (server, _relay_url) = main_server().await;
    let key = SecretKey::generate();
    PeerGate::allow_list([key.peer_id()]).attach(&server);

    // The right peer id on a port where no relay listens.
    let wrong = format!("letify://{}@127.0.0.1:{}", server.peer_id(), dead_port());
    let phone = device(key.clone(), &wrong, Duration::from_secs(2)).await;
    let failed = tokio::time::timeout(Duration::from_secs(15), phone.connect(server.peer_id(), "svc"))
        .await
        .expect("a dead relay address fails within the connect timeout");
    assert!(matches!(failed, Err(TransportError::Connect { .. })), "{failed:?}");

    // A relay that is not the peer being dialed is not a path to it without a rendezvous: the
    // dial fails at once and says so.
    let other = SecretKey::generate().peer_id();
    let not_the_server = format!("letify://{other}@127.0.0.1:{}", dead_port());
    let phone = device(key, &not_the_server, Duration::from_secs(2)).await;
    match phone.connect(server.peer_id(), "svc").await {
        Err(TransportError::Connect { reason, .. }) => assert!(reason.contains("no rendezvous"), "{reason}"),
        other => panic!("expected a connect error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n9_revoking_a_device_ends_its_path_through_the_relay() {
    let (server, relay_url) = main_server().await;
    spawn_echo(&server, "svc");
    let key = SecretKey::generate();
    let gate = PeerGate::allow_list([key.peer_id()]);
    gate.attach(&server);

    let phone = device(key.clone(), &relay_url, Duration::from_secs(3)).await;
    let conn = tokio::time::timeout(Duration::from_secs(20), phone.connect(server.peer_id(), "svc")).await.unwrap().unwrap();
    assert_eq!(echo_roundtrip(&conn, b"alive").await, b"alive");

    gate.revoke(&key.peer_id());
    tokio::time::timeout(Duration::from_secs(20), conn.closed()).await.expect("the revoked device's connection ends");
    // And it cannot come back: the relay refuses its handshake and no request waits for it.
    let again = tokio::time::timeout(Duration::from_secs(15), phone.connect(server.peer_id(), "svc"))
        .await
        .expect("a revoked device fails within its connect timeout");
    assert!(again.is_err(), "a revoked device must not reconnect");
}
