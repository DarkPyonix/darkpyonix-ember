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
    main_server_with(LetifyConfig::default().standing_requests).await
}

/// The same, keeping at most `standing_requests` standing requests (the default is 48).
async fn main_server_with(standing_requests: usize) -> (Transport, String) {
    let letify = LetifyConfig {
        relay_listen: Some("127.0.0.1:0".parse().unwrap()),
        standing_requests,
        ..LetifyConfig::default()
    };
    let server = Transport::bind(
        TransportConfig::new(SecretKey::generate())
            .backend(BackendKind::Letify)
            .bind_addr("127.0.0.1:0".parse().unwrap())
            .letify(letify),
    )
    .await
    .unwrap();
    let relay_url = server.local_addr().relays.remove(0);
    (server, relay_url)
}

/// The `host:port` of a relay URL `letify://<peer id>@host:port`.
fn relay_addr(relay_url: &str) -> &str {
    relay_url.rsplit_once('@').unwrap().1
}

/// The reason of a failed connect.
fn reason<T: std::fmt::Debug>(r: Result<T, TransportError>) -> String {
    match r {
        Err(TransportError::Connect { reason, .. }) => reason,
        other => panic!("expected a connect error, got {other:?}"),
    }
}

/// Dials the server with only the relay, up to three times (the redial is the stated recovery
/// while a request from an earlier dial or a revoke still waits at the relay).
async fn reach(dev: &Transport, server: &Transport) -> ember_transport::Connection {
    let mut last = String::new();
    for _ in 0..3 {
        match tokio::time::timeout(Duration::from_secs(30), dev.connect(server.peer_id(), "svc")).await {
            Ok(Ok(conn)) => return conn,
            Ok(Err(e)) => last = e.to_string(),
            Err(_) => last = "timed out".into(),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("the device never reached the server through the relay: {last}");
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

    // The default connect timeout (20 s): the refusal must not wait it out.
    let stranger = device(SecretKey::generate(), &relay_url, LetifyConfig::default().connect_timeout).await;
    let started = Instant::now();
    let refused = tokio::time::timeout(Duration::from_secs(30), stranger.connect(server.peer_id(), "svc"))
        .await
        .expect("a refused device fails within its connect timeout");
    let why = reason(refused);
    // Where it was refused: at the relay, before any pairing. The relay closes a peer it does
    // not admit with no reply, so the reason names the relay and both causes it cannot tell apart.
    assert!(why.contains("ended the connection before pairing"), "{why}");
    assert!(why.contains(relay_addr(&relay_url)), "names the relay address: {why}");
    assert!(why.contains("does not admit this peer id"), "{why}");
    // The app reads these words as the server's own close codes; the relay's refusal is not one.
    assert!(!why.contains("not allowed") && !why.contains("revoked") && !why.contains("code 40"), "{why}");
    assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n9_a_wrong_relay_address_fails_at_once() {
    let (server, _relay_url) = main_server().await;
    let key = SecretKey::generate();
    PeerGate::allow_list([key.peer_id()]).attach(&server);

    // The right peer id on a port where no relay listens, with the default connect timeout
    // (20 s): the refused TCP connect ends the dial.
    let port = dead_port();
    let wrong = format!("letify://{}@127.0.0.1:{port}", server.peer_id());
    let phone = device(key.clone(), &wrong, LetifyConfig::default().connect_timeout).await;
    let started = Instant::now();
    let failed = tokio::time::timeout(Duration::from_secs(30), phone.connect(server.peer_id(), "svc"))
        .await
        .expect("a dead relay address fails within the connect timeout");
    let why = reason(failed);
    assert!(why.contains("cannot be reached"), "{why}");
    assert!(why.contains(&format!("127.0.0.1:{port}")), "names the relay address: {why}");
    assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());

    // Two calls at once and one after: none waits for another's timeout.
    let started = Instant::now();
    let (a, b) = tokio::join!(phone.connect(server.peer_id(), "svc"), phone.connect(server.peer_id(), "svc"));
    assert!(reason(a).contains("cannot be reached"));
    assert!(reason(b).contains("cannot be reached"));
    assert!(reason(phone.connect(server.peer_id(), "svc").await).contains("cannot be reached"));
    assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());

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

/// The relay admits the device but the server keeps no request for it (no slot): the device's
/// request waits at the relay, bounded by the connect timeout here (3 s; 8 s by default), and
/// the call queued behind the dial gets the same failure and does not wait again.
#[tokio::test(flavor = "multi_thread")]
async fn fr_n9_calls_queued_behind_a_failed_relay_dial_share_its_failure() {
    let (server, relay_url) = main_server_with(0).await;
    spawn_echo(&server, "svc");
    let key = SecretKey::generate();
    PeerGate::allow_list([key.peer_id()]).attach(&server);

    let phone = device(key, &relay_url, Duration::from_secs(3)).await;
    let started = Instant::now();
    let (a, b) = tokio::join!(phone.connect(server.peer_id(), "svc"), phone.connect(server.peer_id(), "svc"));
    let took = started.elapsed();
    let (a, b) = (reason(a), reason(b));
    assert!(a.contains("did not answer at its relay"), "{a}");
    assert!(a.contains(relay_addr(&relay_url)), "names the relay address: {a}");
    assert_eq!(a, b, "the queued call gets the failure of the dial it waited behind");
    // One wait of 3 s, not two in turn (6 s).
    assert!(took < Duration::from_secs(5), "took {took:?}");
}

/// The cap counts peers. With one slot, the second allowed peer is queued: the relay admits it
/// but no request waits for it, so its dial ends after 8 s (not the 20 s connect timeout). When
/// the first peer is revoked the queued one gets the slot and reaches the server.
#[tokio::test(flavor = "multi_thread")]
async fn fr_n9_a_peer_over_the_standing_cap_is_queued_and_gets_a_slot_after_a_revoke() {
    let (server, relay_url) = main_server_with(1).await;
    spawn_echo(&server, "svc");
    let (first, second) = (SecretKey::generate(), SecretKey::generate());
    let gate = PeerGate::allow_list([first.peer_id()]);
    gate.attach(&server);
    gate.allow(second.peer_id());

    // One device at a time: the relay holds at most four unauthenticated connections from one
    // address, and the server's own requests come from loopback too.
    let phone = device(first.clone(), &relay_url, LetifyConfig::default().connect_timeout).await;
    let conn = reach(&phone, &server).await;
    assert_eq!(echo_roundtrip(&conn, b"first").await, b"first");

    let tablet = device(second, &relay_url, LetifyConfig::default().connect_timeout).await;
    let started = Instant::now();
    let queued = tokio::time::timeout(Duration::from_secs(30), tablet.connect(server.peer_id(), "svc"))
        .await
        .expect("a queued peer's dial ends within the connect timeout");
    let why = reason(queued);
    assert!(why.contains("did not answer at its relay"), "{why}");
    assert!(started.elapsed() < Duration::from_secs(15), "bounded at 8 s, took {:?}", started.elapsed());

    // The first peer goes: its slot passes to the queued one.
    gate.revoke(&first.peer_id());
    let conn2 = reach(&tablet, &server).await;
    assert_eq!(echo_roundtrip(&conn2, b"second").await, b"second");
    assert_eq!(conn2.peer(), server.peer_id());
}

/// Known limit (SPEC `FR-N9`): the relay cannot withdraw the server's waiting request at a
/// revoke, so a device allowed again at once may first be paired with that dead request. The
/// redial recovers it.
#[tokio::test(flavor = "multi_thread")]
async fn fr_n9_a_device_allowed_again_right_after_a_revoke_reaches_the_server() {
    let (server, relay_url) = main_server().await;
    spawn_echo(&server, "svc");
    let key = SecretKey::generate();
    let gate = PeerGate::allow_list([key.peer_id()]);
    gate.attach(&server);

    let letify = LetifyConfig { resume_window: Duration::from_secs(2), ..LetifyConfig::default() };
    let phone = Transport::bind(
        TransportConfig::new(key.clone())
            .backend(BackendKind::Letify)
            .bind_addr("127.0.0.1:0".parse().unwrap())
            .relay(RelayConfig::Custom(vec![relay_url.clone()]))
            .letify(letify),
    )
    .await
    .unwrap();
    let conn = reach(&phone, &server).await;
    assert_eq!(echo_roundtrip(&conn, b"alive").await, b"alive");

    gate.revoke(&key.peer_id());
    tokio::time::timeout(Duration::from_secs(20), conn.closed()).await.expect("the revoked device's connection ends");
    phone.close().await;
    gate.allow(key.peer_id());

    let again = device(key, &relay_url, LetifyConfig::default().connect_timeout).await;
    let conn = reach(&again, &server).await;
    assert_eq!(echo_roundtrip(&conn, b"back").await, b"back");
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n9_revoking_a_device_ends_its_path_through_the_relay() {
    let (server, relay_url) = main_server().await;
    spawn_echo(&server, "svc");
    let key = SecretKey::generate();
    let gate = PeerGate::allow_list([key.peer_id()]);
    gate.attach(&server);

    // A short resume window: the relay cuts the splice at the revoke, possibly before the
    // session's close arrives, and with no other path the session then ends after the window.
    let letify = LetifyConfig {
        connect_timeout: Duration::from_secs(3),
        resume_window: Duration::from_secs(2),
        ..LetifyConfig::default()
    };
    let phone = Transport::bind(
        TransportConfig::new(key.clone())
            .backend(BackendKind::Letify)
            .bind_addr("127.0.0.1:0".parse().unwrap())
            .relay(RelayConfig::Custom(vec![relay_url.clone()]))
            .letify(letify),
    )
    .await
    .unwrap();
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

/// `FR-N9` (#920): flooding the public relay address with unauthenticated TCP connections
/// from loopback cannot starve the server's own standing requests or block an allowed peer
/// from completing a relay handshake once the flooder's per-source budget is capped.
#[tokio::test]
async fn fr_n9_relay_flood_from_loopback_does_not_block_standing_requests() {
    let (server, relay_url) = main_server().await;
    let addr: std::net::SocketAddr = relay_addr(&relay_url).parse().expect("relay socket addr");
    spawn_echo(&server, "svc");

    let key = SecretKey::generate();
    let gate = PeerGate::allow_list([key.peer_id()]);
    gate.attach(&server);

    // Open 16 unauthenticated TCP connections from loopback and hold them open to try to
    // exhaust the relay's pre-auth slots. RelayGuard caps per-source concurrency and rate,
    // while the server's standing requests dial the backend loopback relay directly.
    let mut held = Vec::new();
    for _ in 0..16 {
        if let Ok(s) = tokio::net::TcpStream::connect(addr).await {
            held.push(s);
        }
    }
    drop(held);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let phone = Transport::bind(
        TransportConfig::new(key)
            .backend(BackendKind::Letify)
            .bind_addr("127.0.0.1:0".parse().unwrap())
            .relay(RelayConfig::Custom(vec![relay_url])),
    )
    .await
    .unwrap();
    // Wait past the 4 s rate window since the flood above also came from 127.0.0.1.
    tokio::time::sleep(Duration::from_secs(4)).await;
    let conn = tokio::time::timeout(Duration::from_secs(20), phone.connect(server.peer_id(), "svc"))
        .await
        .expect("allowed device connects after flood")
        .expect("relay handshake succeeds");
    assert_eq!(echo_roundtrip(&conn, b"after-flood").await, b"after-flood");
}
