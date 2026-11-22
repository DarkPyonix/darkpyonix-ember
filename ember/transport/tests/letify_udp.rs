//! EM-11 (#10, SPEC `FR-N1`..`FR-N5`, `NFR-N1`): the letify backend punches a direct path
//! (letify-link's driver: TCP simultaneous open first, UDP beside it) and falls back to the main server's relay when punching fails. Device admission and
//! revocation (`FR-N3`, #314) hold on a punched path as they do on TCP and on the relay.
//!
//! What this network models, in the style of letify-link's `tests/nat.rs`: real UDP and TCP on
//! loopback, the real letify-link relay, QAD address observer, punch and QUIC path. The node
//! advertises a TCP address that refuses every connection, so the only direct path between the
//! client and the node is the one the punch makes: a path reported as direct is a punched path
//! (TCP or UDP; the transport API does not report which). What it does not model: a NAT between the peers (letify-link has no NAT simulation on
//! loopback; NAT behaviour on real networks is not verified here).
#![cfg(all(feature = "letify", feature = "http"))]

mod common;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ConnectInfo;
use axum::routing::get;
use axum::Router;
use ember_transport::gate::{CLOSE_NOT_ALLOWED, CLOSE_REVOKED};
use ember_transport::http::{http1_client, HttpListener};
use ember_transport::{
    BackendKind, CloseReason, Connection, LetifyConfig, MemorySignalHub, ObserverAddr, PathState, PeerAddr, PeerGate,
    PeerId, RelayConfig, SecretKey, SignalChannel, Transport, TransportConfig,
};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;

const SERVICE: &str = "ember/http/0";

/// How long a punch on loopback may take before the path must be direct: the punch starts one
/// lead after the offer and the QUIC handshake follows at once.
const PUNCH_BOUND: Duration = Duration::from_secs(15);

/// The revocation bound of the TCP and relay tests (`tests/letify_gate.rs`).
const REVOKE_BOUND: Duration = Duration::from_secs(1);

/// A loopback TCP address that nothing listens on, so a dial is refused at once.
fn dead_address() -> SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap()
}

/// A free loopback UDP address for the server's address observer.
fn free_udp() -> SocketAddr {
    std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap()
}

/// A rendezvous channel that counts the punch offers it receives. The kinds are letify-link's
/// rendezvous message kinds 3 (`PunchOffer`) and 5 (`PunchOfferExt`), the first byte of a
/// message; this test names no letify-link type (`FR-N5`).
#[derive(Debug)]
struct CountingSignal {
    inner: Arc<dyn SignalChannel>,
    offers: Arc<AtomicUsize>,
}

impl CountingSignal {
    fn wrap(inner: Arc<dyn SignalChannel>) -> (Arc<dyn SignalChannel>, Arc<AtomicUsize>) {
        let offers = Arc::new(AtomicUsize::new(0));
        let wrapped: Arc<dyn SignalChannel> = Arc::new(Self { inner, offers: offers.clone() });
        (wrapped, offers)
    }
}

#[async_trait::async_trait]
impl SignalChannel for CountingSignal {
    async fn send(&self, to: PeerId, msg: Vec<u8>) -> ember_transport::Result<()> {
        self.inner.send(to, msg).await
    }

    async fn recv(&self) -> ember_transport::Result<(PeerId, Vec<u8>)> {
        let (from, msg) = self.inner.recv().await?;
        if matches!(msg.first(), Some(3 | 5)) {
            self.offers.fetch_add(1, Ordering::SeqCst);
        }
        Ok((from, msg))
    }

    fn watch(&self, peers: &[PeerId]) {
        self.inner.watch(peers)
    }
}

struct Net {
    _server: Transport,
    node: Transport,
    client: Transport,
    /// The node's gate: it admits the client only.
    gate: PeerGate,
    signals: MemorySignalHub,
    relay_url: String,
    observer: ObserverAddr,
    /// Punch offers the node received.
    node_offers: Arc<AtomicUsize>,
}

/// A main server (relay and address observer), a node behind a gate that admits only the
/// client, and the client. The node and the client share a rendezvous and use the server's
/// relay and observer. `node_punch` is false for a node whose UDP is blocked: it never answers
/// a punch offer.
async fn net(node_punch: bool) -> Net {
    net_with(node_punch, true).await
}

/// The same, with the client's `udp_punch` set to `client_punch`.
async fn net_with(node_punch: bool, client_punch: bool) -> Net {
    let signals = MemorySignalHub::new();
    let (node_key, client_key) = (SecretKey::generate(), SecretKey::generate());
    let (node_id, client_id) = (node_key.peer_id(), client_key.peer_id());

    let qad = free_udp();
    let server_key = SecretKey::generate();
    let server_id = server_key.peer_id();
    let server = Transport::bind(
        TransportConfig::new(server_key)
            .backend(BackendKind::Letify)
            .bind_addr("127.0.0.1:0".parse().unwrap())
            .letify(LetifyConfig {
                relay_listen: Some("127.0.0.1:0".parse().unwrap()),
                qad_listen: Some(qad),
                // The Worker rendezvous has its own tests (letify_worker.rs); these punch over Ember's own.
                worker_rendezvous: false,
                ..LetifyConfig::default()
            }),
    )
    .await
    .unwrap();
    let relay_gate = PeerGate::allow_list([node_id, client_id]);
    relay_gate.attach(&server);
    let relay_url = server.local_addr().relays.remove(0);
    let observer = ObserverAddr { addr: qad, node: Some(server_id) };

    let (node_signal, node_offers) = CountingSignal::wrap(signals.channel(node_id));
    let device = |key: SecretKey, signal: Arc<dyn SignalChannel>, advertise: Vec<SocketAddr>, udp_punch: bool| {
        let letify = LetifyConfig {
            signal: Some(signal),
            observer: Some(observer),
            advertise,
            udp_punch,
            connect_timeout: Duration::from_secs(20),
            // The Worker rendezvous has its own tests (letify_worker.rs); these punch over Ember's own.
            worker_rendezvous: false,
            ..LetifyConfig::default()
        };
        TransportConfig::new(key)
            .backend(BackendKind::Letify)
            .bind_addr("127.0.0.1:0".parse().unwrap())
            .relay(RelayConfig::Custom(vec![relay_url.clone()]))
            .letify(letify)
    };
    let node = Transport::bind(device(node_key, node_signal, vec![dead_address()], node_punch)).await.unwrap();
    let client_signal: Arc<dyn SignalChannel> = signals.channel(client_id);
    let client = Transport::bind(device(client_key, client_signal, vec![], client_punch)).await.unwrap();
    let gate = PeerGate::allow_list([client_id]);
    gate.attach(&node);
    serve_http(&node, gate.clone());
    // The client knows only the node's refused TCP address.
    client.add_peer_addr(PeerAddr { peer: node_id, relays: vec![], direct: node.local_addr().direct });
    Net { _server: server, node, client, gate, signals, relay_url, observer, node_offers }
}

/// Serves `GET /whoami` (the caller's peer id) on the node, behind `gate`.
fn serve_http(node: &Transport, gate: PeerGate) {
    let listener = HttpListener::with_gate(node.listen(SERVICE).unwrap(), gate);
    let router = Router::new().route("/whoami", get(|ConnectInfo(peer): ConnectInfo<PeerId>| async move { peer.to_string() }));
    tokio::spawn(async move {
        axum::serve(listener, router.into_make_service_with_connect_info::<PeerId>()).await.unwrap();
    });
}

/// One HTTP request over `conn`; returns the body.
async fn whoami(conn: &Connection) -> String {
    let mut http = http1_client::<Full<Bytes>>(conn).await.unwrap();
    let req = hyper::Request::get("http://peer/whoami").body(Full::default()).unwrap();
    let resp = http.send_request(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(body.to_vec()).unwrap()
}

/// Waits until `conn` reports a direct path.
async fn wait_direct(conn: &Connection) {
    let deadline = Instant::now() + PUNCH_BOUND;
    while !conn.path_state().is_direct() {
        assert!(Instant::now() < deadline, "no direct path within {PUNCH_BOUND:?}: {}", conn.path_state());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A client connected to the node over a punched path, with one HTTP request served.
async fn punched(n: &Net) -> Connection {
    let conn = tokio::time::timeout(Duration::from_secs(20), n.client.connect(n.node.peer_id(), SERVICE))
        .await
        .expect("connect timed out")
        .unwrap();
    wait_direct(&conn).await;
    conn
}

#[tokio::test(flavor = "multi_thread")]
async fn em11_two_peers_punch_a_direct_udp_path_and_carry_http_over_it() {
    let n = net(true).await;
    let conn = punched(&n).await;
    // The node's only TCP address refuses every dial, so direct means a punched path.
    match conn.path_state() {
        PathState::Direct { .. } => {}
        other => panic!("expected the punched direct path, got {other}"),
    }
    assert!(!conn.path_state().is_relayed());
    assert!(n.node_offers.load(Ordering::SeqCst) >= 1, "the client offered the punch over the rendezvous");
    assert_eq!(whoami(&conn).await, n.client.peer_id().to_string());
    // The session stays on the direct path while it carries traffic.
    for _ in 0..3 {
        assert_eq!(whoami(&conn).await, n.client.peer_id().to_string());
    }
    assert!(conn.path_state().is_direct(), "the path left the punched path: {}", conn.path_state());
}

#[tokio::test(flavor = "multi_thread")]
async fn em11_when_punching_fails_the_connection_falls_back_to_the_relay() {
    // The node's UDP is blocked: it never answers a punch offer, so the client's punch times
    // out and the relay carries the session.
    let n = net(false).await;
    let conn = tokio::time::timeout(Duration::from_secs(20), n.client.connect(n.node.peer_id(), SERVICE))
        .await
        .expect("connect through the relay timed out")
        .unwrap();
    assert_eq!(whoami(&conn).await, n.client.peer_id().to_string());
    let settle = Instant::now() + Duration::from_secs(5);
    while !conn.path_state().is_relayed() {
        assert!(Instant::now() < settle, "the connection never reported a relayed path: {}", conn.path_state());
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // Past the whole punch window the path is still relayed and still works.
    tokio::time::sleep(Duration::from_secs(8)).await;
    assert!(conn.path_state().is_relayed(), "expected the relay, got {}", conn.path_state());
    assert_eq!(whoami(&conn).await, n.client.peer_id().to_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn em11_fr_n3_a_revoked_device_on_a_punched_path_is_dropped_within_the_bound() {
    let n = net(true).await;
    let conn = punched(&n).await;
    assert_eq!(whoami(&conn).await, n.client.peer_id().to_string());

    let started = Instant::now();
    assert!(n.gate.revoke(&n.client.peer_id()) >= 1);
    let reason = tokio::time::timeout(REVOKE_BOUND, conn.closed()).await.expect("the punched path outlived the revocation");
    assert!(started.elapsed() < REVOKE_BOUND);
    assert!(matches!(reason, CloseReason::Remote { code: CLOSE_REVOKED, .. }), "the device sees the revoked code, got {reason:?}");

    // A new connection is refused: the node no longer punches for it, the relay meeting is
    // declined and the gate refuses it. The dial gives up after the client's connect timeout.
    let again = tokio::time::timeout(Duration::from_secs(40), n.client.connect(n.node.peer_id(), SERVICE))
        .await
        .expect("the refused dial ends");
    match again {
        Err(_) => {}
        Ok(again) => {
            let reason = tokio::time::timeout(Duration::from_secs(5), again.closed()).await.expect("refused");
            assert!(matches!(reason, CloseReason::Remote { code: CLOSE_NOT_ALLOWED, .. }), "{reason:?}");
        }
    }
    assert_eq!(n.gate.live_connections(&n.client.peer_id()), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn em11_fr_n3_a_device_the_node_does_not_admit_is_refused_on_the_punched_path() {
    let n = net(true).await;
    // A stranger on the same rendezvous with the same observer and relay URL, so it can offer a
    // punch to the node. The relay does not admit it, and the node's gate does not list it.
    let key = SecretKey::generate();
    let stranger_id = key.peer_id();
    let letify = LetifyConfig {
        signal: Some(n.signals.channel(stranger_id)),
        observer: Some(n.observer),
        connect_timeout: Duration::from_secs(8),
        // The Worker rendezvous has its own tests (letify_worker.rs); these punch over Ember's own.
        worker_rendezvous: false,
        ..LetifyConfig::default()
    };
    let stranger = Transport::bind(
        TransportConfig::new(key)
            .backend(BackendKind::Letify)
            .bind_addr("127.0.0.1:0".parse().unwrap())
            .relay(RelayConfig::Custom(vec![n.relay_url.clone()]))
            .letify(letify),
    )
    .await
    .unwrap();
    stranger.add_peer_addr(PeerAddr { peer: n.node.peer_id(), relays: vec![], direct: n.node.local_addr().direct });

    let attempt = tokio::time::timeout(Duration::from_secs(30), stranger.connect(n.node.peer_id(), SERVICE))
        .await
        .expect("the stranger's dial ends");
    match attempt {
        Err(_) => {}
        Ok(conn) => {
            let reason = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await.expect("refused at accept");
            assert!(matches!(reason, CloseReason::Remote { code: CLOSE_NOT_ALLOWED, .. }), "{reason:?}");
        }
    }
    assert_eq!(n.gate.live_connections(&stranger_id), 0);

    // The admitted client still gets its punched path.
    let conn = punched(&n).await;
    assert_eq!(whoami(&conn).await, n.client.peer_id().to_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn em11_with_udp_punch_off_no_punch_candidate_is_offered() {
    // EMBER_LETIFY_UDP_PUNCH=off on the client: it sends no punch offer, and the session stays on
    // the relay even though the node would answer one.
    let n = net_with(true, false).await;
    let conn = tokio::time::timeout(Duration::from_secs(20), n.client.connect(n.node.peer_id(), SERVICE))
        .await
        .expect("connect through the relay timed out")
        .unwrap();
    assert_eq!(whoami(&conn).await, n.client.peer_id().to_string());
    // Longer than a punch lead and window, so an offer would have arrived and a punch finished.
    tokio::time::sleep(Duration::from_secs(7)).await;
    assert_eq!(n.node_offers.load(Ordering::SeqCst), 0, "a client with punching off offered a punch");
    assert!(conn.path_state().is_relayed(), "expected the relay, got {}", conn.path_state());
}

#[tokio::test(flavor = "multi_thread")]
async fn em11_a_dial_to_the_relay_node_offers_no_punch() {
    // The main server is the relay node: a device dials it directly or through its relay, never
    // with a punch, even with punching on and an observer configured.
    let signals = MemorySignalHub::new();
    let qad = free_udp();
    let server_key = SecretKey::generate();
    let server_id = server_key.peer_id();
    let (server_signal, server_offers) = CountingSignal::wrap(signals.channel(server_id));
    let server = Transport::bind(
        TransportConfig::new(server_key)
            .backend(BackendKind::Letify)
            .bind_addr("127.0.0.1:0".parse().unwrap())
            .letify(LetifyConfig {
                relay_listen: Some("127.0.0.1:0".parse().unwrap()),
                qad_listen: Some(qad),
                signal: Some(server_signal),
                // The Worker rendezvous has its own tests (letify_worker.rs); these punch over Ember's own.
                worker_rendezvous: false,
                ..LetifyConfig::default()
            }),
    )
    .await
    .unwrap();
    common::spawn_echo(&server, "echo");
    let server_addr = server.local_addr();
    let relay_url = server_addr.relays[0].clone();
    let client_key = SecretKey::generate();
    let client = Transport::bind(
        TransportConfig::new(client_key.clone())
            .backend(BackendKind::Letify)
            .bind_addr("127.0.0.1:0".parse().unwrap())
            .relay(RelayConfig::Custom(vec![relay_url]))
            .letify(LetifyConfig {
                signal: Some(signals.channel(client_key.peer_id())),
                observer: Some(ObserverAddr { addr: qad, node: Some(server_id) }),
                // The Worker rendezvous has its own tests (letify_worker.rs); these punch over Ember's own.
                worker_rendezvous: false,
                ..LetifyConfig::default()
            }),
    )
    .await
    .unwrap();
    client.add_peer_addr(server_addr);
    let conn = tokio::time::timeout(Duration::from_secs(20), client.connect(server_id, "echo"))
        .await
        .expect("connect to the main server timed out")
        .unwrap();
    assert_eq!(common::echo_roundtrip(&conn, b"to the main server").await, b"to the main server");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(server_offers.load(Ordering::SeqCst), 0, "a dial to the relay node offered a punch");
}
