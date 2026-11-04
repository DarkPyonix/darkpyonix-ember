//! `FR-N2`: the main server is the relay. Two devices that cannot reach each other directly
//! meet at the main server, which splices their end to end encrypted session; the path is
//! reported as relayed. Also the address observer role and revocation at the relay.
#![cfg(feature = "letify")]

mod common;

use std::time::{Duration, Instant};

use common::{echo_roundtrip, spawn_echo};
use ember_transport::{
    BackendKind, CloseReason, LetifyConfig, MemorySignalHub, ObserverAddr, PathState, PeerAddr, PeerGate, RelayConfig,
    SecretKey, Transport, TransportConfig,
};

/// A loopback TCP address that nothing listens on, so a dial is refused at once.
fn dead_address() -> std::net::SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap()
}

struct Net {
    server: Transport,
    node: Transport,
    client: Transport,
    gate: PeerGate,
    relay_url: String,
}

/// A main server running the relay role, a node whose only advertised address is dead (a
/// blocked direct path), and a client. Both register the server's relay and share a rendezvous.
async fn net(resume_window: Duration) -> Net {
    let signals = MemorySignalHub::new();
    let (node_key, client_key) = (SecretKey::generate(), SecretKey::generate());
    let (node_id, client_id) = (node_key.peer_id(), client_key.peer_id());

    let server = Transport::bind(
        TransportConfig::new(SecretKey::generate())
            .backend(BackendKind::Letify)
            .bind_addr("127.0.0.1:0".parse().unwrap())
            .letify(LetifyConfig { relay_listen: Some("127.0.0.1:0".parse().unwrap()), ..LetifyConfig::default() }),
    )
    .await
    .unwrap();
    let gate = PeerGate::allow_list([node_id, client_id]);
    gate.attach(&server);
    let relay_url = server.local_addr().relays.remove(0);

    let device = |key: SecretKey, advertise: Vec<std::net::SocketAddr>| {
        let letify = LetifyConfig {
            signal: Some(signals.channel(key.peer_id())),
            advertise,
            resume_window,
            connect_timeout: Duration::from_secs(20),
            ..LetifyConfig::default()
        };
        TransportConfig::new(key)
            .backend(BackendKind::Letify)
            .bind_addr("127.0.0.1:0".parse().unwrap())
            .relay(RelayConfig::Custom(vec![relay_url.clone()]))
            .letify(letify)
    };
    let node = Transport::bind(device(node_key, vec![dead_address()])).await.unwrap();
    let client = Transport::bind(device(client_key, vec![])).await.unwrap();
    // The client knows only the node's blocked address.
    client.add_peer_addr(PeerAddr { peer: node_id, relays: vec![], direct: node.local_addr().direct });
    Net { server, node, client, gate, relay_url }
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n2_letify_the_relay_url_names_the_main_server() {
    let n = net(Duration::from_secs(60)).await;
    assert_eq!(n.relay_url, format!("letify://{}@127.0.0.1:{}", n.server.peer_id(), n.relay_url.rsplit(':').next().unwrap()));
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n2_letify_two_devices_that_cannot_connect_directly_meet_at_the_main_server() {
    let n = net(Duration::from_secs(60)).await;
    spawn_echo(&n.node, "svc");

    let started = Instant::now();
    let conn = tokio::time::timeout(Duration::from_secs(20), n.client.connect(n.node.peer_id(), "svc"))
        .await
        .expect("connect through the relay timed out")
        .unwrap();
    eprintln!("relayed connect took {:?}", started.elapsed());
    assert_eq!(conn.peer(), n.node.peer_id());
    assert_eq!(echo_roundtrip(&conn, b"through the main server").await, b"through the main server");
    let big = vec![5u8; 2 << 20];
    assert_eq!(echo_roundtrip(&conn, &big).await, big);

    match conn.path_state() {
        PathState::Relayed { .. } => {}
        other => panic!("the direct path is blocked, so the path must be relayed, got {other}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n2_letify_a_device_the_server_does_not_admit_cannot_use_the_relay() {
    let n = net(Duration::from_secs(60)).await;
    let signals = MemorySignalHub::new();
    // An outsider on the same rendezvous as nobody: it has the relay URL but the relay does not
    // know it, so it cannot pair even when it shouts at the node.
    let outsider_key = SecretKey::generate();
    let letify = LetifyConfig {
        signal: Some(signals.channel(outsider_key.peer_id())),
        connect_timeout: Duration::from_secs(3),
        ..LetifyConfig::default()
    };
    let outsider = Transport::bind(
        TransportConfig::new(outsider_key)
            .backend(BackendKind::Letify)
            .bind_addr("127.0.0.1:0".parse().unwrap())
            .relay(RelayConfig::Custom(vec![n.relay_url.clone()]))
            .letify(letify),
    )
    .await
    .unwrap();
    outsider.add_peer_addr(PeerAddr { peer: n.node.peer_id(), relays: vec![], direct: vec![dead_address()] });
    assert!(outsider.connect(n.node.peer_id(), "svc").await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n3_letify_revoking_a_device_cuts_its_relayed_path_at_once() {
    // A short resume window: once the splice is cut and no other path exists, the session ends.
    let n = net(Duration::from_secs(2)).await;
    spawn_echo(&n.node, "svc");
    let conn = n.client.connect(n.node.peer_id(), "svc").await.unwrap();
    assert_eq!(echo_roundtrip(&conn, b"alive").await, b"alive");
    assert!(matches!(conn.path_state(), PathState::Relayed { .. }));

    n.gate.revoke(&n.node.peer_id());
    // The relayed path is gone within a moment (not at the next keepalive).
    let deadline = Instant::now() + Duration::from_secs(3);
    while matches!(conn.path_state(), PathState::Relayed { .. }) {
        assert!(Instant::now() < deadline, "the relayed path outlived the revocation");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // The node can no longer be paired, so after the resume window the connection ends.
    let reason = tokio::time::timeout(Duration::from_secs(20), conn.closed()).await.expect("the connection ends");
    assert!(matches!(reason, CloseReason::Lost(_)), "{reason:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n2_letify_the_main_server_observes_the_address_of_a_device() {
    // The server answers QAD requests; a device asks it for its public address and publishes
    // the result. On loopback that address is 127.0.0.1.
    let qad = {
        let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        s.local_addr().unwrap()
    };
    let server_key = SecretKey::generate();
    let server_id = server_key.peer_id();
    let _server = Transport::bind(
        TransportConfig::new(server_key)
            .backend(BackendKind::Letify)
            .bind_addr("127.0.0.1:0".parse().unwrap())
            .letify(LetifyConfig { qad_listen: Some(qad), ..LetifyConfig::default() }),
    )
    .await
    .unwrap();
    let device = Transport::bind(
        TransportConfig::new(SecretKey::generate())
            .backend(BackendKind::Letify)
            // Any interface: the advertised address then comes from the observation alone.
            .bind_addr("0.0.0.0:0".parse().unwrap())
            .letify(LetifyConfig {
                observer: Some(ObserverAddr { addr: qad, node: Some(server_id) }),
                ..LetifyConfig::default()
            }),
    )
    .await
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let direct = device.local_addr().direct;
        if !direct.is_empty() {
            assert!(direct.iter().all(|a| a.ip().is_loopback()), "{direct:?}");
            break;
        }
        assert!(Instant::now() < deadline, "the address was never observed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
