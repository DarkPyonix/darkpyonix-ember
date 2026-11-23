//! EM-11 (#10, SPEC `FR-N2`): the letify signalling Worker as an auxiliary rendezvous for
//! punching. The dialing side wakes the answering side through Ember's own rendezvous (here an
//! in-memory signal hub, or the in-memory pkarr directory that stands in for the hub) and runs
//! the punch exchange over the Worker; the answering side long-polls the Worker only after a
//! wake-up, for one punch window. A Worker that refuses or rate-limits makes the dial fall back.
//!
//! The Worker is a fake: a small in-process HTTP server with `POST /send/<to>` and
//! `GET /recv/<node>?after=&wait=` as letify's spec describes them (202 with a seq; a long poll
//! that answers `{"messages":[{seq,from,at,body}],"cursor"}` and deletes up to `after`). It
//! trusts the `X-Letify-Node` header instead of verifying the signature, and counts requests.
#![cfg(all(feature = "letify", feature = "http"))]

mod common;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use common::{echo_roundtrip, spawn_echo};
use ember_transport::{
    BackendKind, Connection, LetifyConfig, MemoryPkarrStore, MemorySignalHub, ObserverAddr, PeerAddr, PeerGate,
    PeerId, RelayConfig, SecretKey, SignalChannel, Transport, TransportConfig,
};

// ----- the fake Worker ---------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Ok,
    /// Every request is answered 429 with this Retry-After.
    RateLimited(u64),
    /// Every request is answered 403.
    Forbidden,
}

#[derive(Default)]
struct FakeState {
    mode: Mode,
    seq: u64,
    /// Messages per recipient: (seq, sender, body).
    boxes: HashMap<String, Vec<(u64, String, Vec<u8>)>>,
    /// Requests by the node that made them (X-Letify-Node).
    sends: HashMap<String, usize>,
    recvs: HashMap<String, usize>,
    /// When the first refusal was answered, and the sends that arrived well after it (a send
    /// already in flight when the refusal left is not one).
    first_refusal: Option<Instant>,
    sends_after_refusal: usize,
    /// The longest a `/recv` is held, seconds (letify's Worker: 25).
    poll_cap: u64,
    /// `/recv` requests being held now, and the most held at once, by caller: one pump of a
    /// client holds at most one.
    inflight: HashMap<String, usize>,
    max_inflight: HashMap<String, usize>,
}

/// Counts one held `/recv` while it lives (also when the client drops the request).
struct Held {
    state: Arc<Mutex<FakeState>>,
    node: String,
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some(n) = self.state.lock().unwrap().inflight.get_mut(&self.node) {
            *n = n.saturating_sub(1);
        }
    }
}

#[derive(Clone)]
struct FakeWorker {
    state: Arc<Mutex<FakeState>>,
    arrived: Arc<tokio::sync::Notify>,
    url: String,
}

impl FakeWorker {
    async fn start(mode: Mode) -> Self {
        Self::start_with(mode, 25).await
    }

    /// A fake whose `/recv` is held at most `poll_cap` seconds.
    async fn start_with(mode: Mode, poll_cap: u64) -> Self {
        let state = Arc::new(Mutex::new(FakeState { mode, poll_cap, ..FakeState::default() }));
        let arrived = Arc::new(tokio::sync::Notify::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let fake = FakeWorker { state, arrived, url };
        let app = Router::new()
            .route("/send/{to}", post(send))
            .route("/recv/{node}", get(recv))
            .with_state(fake.clone());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        fake
    }

    fn sends(&self, node: PeerId) -> usize {
        self.state.lock().unwrap().sends.get(&node.to_string()).copied().unwrap_or(0)
    }

    fn recvs(&self, node: PeerId) -> usize {
        self.state.lock().unwrap().recvs.get(&node.to_string()).copied().unwrap_or(0)
    }

    fn max_inflight(&self, node: PeerId) -> usize {
        self.state.lock().unwrap().max_inflight.get(&node.to_string()).copied().unwrap_or(0)
    }

    fn sends_after_refusal(&self) -> usize {
        self.state.lock().unwrap().sends_after_refusal
    }

    fn requests(&self) -> usize {
        let st = self.state.lock().unwrap();
        st.sends.values().sum::<usize>() + st.recvs.values().sum::<usize>()
    }
}

fn caller(headers: &HeaderMap) -> String {
    headers.get("x-letify-node").and_then(|v| v.to_str().ok()).unwrap_or("").to_string()
}

fn refused(mode: Mode) -> Option<Response> {
    match mode {
        Mode::Ok => None,
        Mode::RateLimited(s) => Some(
            (StatusCode::TOO_MANY_REQUESTS, [("retry-after", s.to_string())], r#"{"error":"rate_limited"}"#).into_response(),
        ),
        Mode::Forbidden => Some((StatusCode::FORBIDDEN, r#"{"error":"forbidden"}"#).into_response()),
    }
}

async fn send(State(fake): State<FakeWorker>, Path(to): Path<String>, headers: HeaderMap, body: Bytes) -> Response {
    let from = caller(&headers);
    let mode = {
        let mut st = fake.state.lock().unwrap();
        *st.sends.entry(from.clone()).or_default() += 1;
        if st.first_refusal.is_some_and(|t| t.elapsed() > Duration::from_millis(300)) {
            st.sends_after_refusal += 1;
        }
        if st.mode != Mode::Ok && st.first_refusal.is_none() {
            st.first_refusal = Some(Instant::now());
        }
        st.mode
    };
    if let Some(r) = refused(mode) {
        return r;
    }
    if body.len() > 4096 {
        return (StatusCode::PAYLOAD_TOO_LARGE, r#"{"error":"too_large"}"#).into_response();
    }
    let seq = {
        let mut st = fake.state.lock().unwrap();
        st.seq += 1;
        let seq = st.seq;
        st.boxes.entry(to).or_default().push((seq, from, body.to_vec()));
        seq
    };
    fake.arrived.notify_waiters();
    (StatusCode::ACCEPTED, format!(r#"{{"ok":true,"seq":{seq}}}"#)).into_response()
}

async fn recv(
    State(fake): State<FakeWorker>,
    Path(node): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let who = caller(&headers);
    let (mode, cap) = {
        let mut st = fake.state.lock().unwrap();
        *st.recvs.entry(who.clone()).or_default() += 1;
        (st.mode, st.poll_cap)
    };
    if let Some(r) = refused(mode) {
        return r;
    }
    let _held = {
        let mut st = fake.state.lock().unwrap();
        let now = {
            let n = st.inflight.entry(who.clone()).or_default();
            *n += 1;
            *n
        };
        let most = st.max_inflight.entry(who.clone()).or_default();
        *most = (*most).max(now);
        Held { state: fake.state.clone(), node: who }
    };
    let after: u64 = q.get("after").and_then(|v| v.parse().ok()).unwrap_or(0);
    let wait: u64 = q.get("wait").and_then(|v| v.parse().ok()).unwrap_or(0).min(cap);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(wait);
    loop {
        let notified = fake.arrived.notified();
        let ready: Vec<(u64, String, Vec<u8>)> = {
            let mut st = fake.state.lock().unwrap();
            let queue = st.boxes.entry(node.clone()).or_default();
            queue.retain(|(seq, _, _)| *seq > after);
            queue.clone()
        };
        if !ready.is_empty() || tokio::time::Instant::now() >= deadline {
            let cursor = ready.last().map_or(after, |m| m.0);
            let messages: Vec<serde_json::Value> = ready
                .iter()
                .map(|(seq, from, body)| serde_json::json!({"seq": seq, "from": from, "at": 0, "body": base64(body)}))
                .collect();
            return (StatusCode::OK, serde_json::json!({"messages": messages, "cursor": cursor}).to_string()).into_response();
        }
        let _ = tokio::time::timeout_at(deadline, notified).await;
    }
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

// ----- the network -------------------------------------------------------------------------

/// A signal channel that drops every punch offer and answer (letify-link rendezvous kinds 3 to
/// 6, the first byte) and passes everything else: Ember's own rendezvous cannot carry the punch
/// exchange, but it carries relay offers and the wake-up.
#[derive(Debug)]
struct NoPunchSignal(Arc<dyn SignalChannel>);

#[async_trait::async_trait]
impl SignalChannel for NoPunchSignal {
    async fn send(&self, to: PeerId, msg: Vec<u8>) -> ember_transport::Result<()> {
        if matches!(msg.first(), Some(3..=6)) {
            return Ok(());
        }
        self.0.send(to, msg).await
    }

    async fn recv(&self) -> ember_transport::Result<(PeerId, Vec<u8>)> {
        self.0.recv().await
    }

    fn watch(&self, peers: &[PeerId]) {
        self.0.watch(peers)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Rdv {
    /// The in-memory signal hub.
    Memory,
    /// The in-memory signal hub with every punch message dropped.
    NoPunch,
    /// The in-memory pkarr directory (the hub's mailbox), no other rendezvous.
    Pkarr,
}

struct Opts {
    rdv: Rdv,
    relay: bool,
    client_worker: bool,
    node_worker: bool,
    mode: Mode,
}

struct World {
    _server: Transport,
    node: Transport,
    client: Transport,
    worker: FakeWorker,
}

fn dead_address() -> SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap()
}

/// A main server (address observer, and relay when `relay`), a node behind a gate that admits
/// the client, whose only TCP address refuses every dial, and the client. Both use the fake
/// Worker when their switch is on.
async fn world(o: Opts) -> World {
    let worker = FakeWorker::start(o.mode).await;
    let (node_key, client_key) = (SecretKey::generate(), SecretKey::generate());
    let (node_id, client_id) = (node_key.peer_id(), client_key.peer_id());
    let qad = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let server_key = SecretKey::generate();
    let server_id = server_key.peer_id();
    let server = Transport::bind(
        TransportConfig::new(server_key)
            .backend(BackendKind::Letify)
            .bind_addr("127.0.0.1:0".parse().unwrap())
            .letify(LetifyConfig {
                relay_listen: o.relay.then(|| "127.0.0.1:0".parse().unwrap()),
                qad_listen: Some(qad),
                worker_rendezvous: false,
                ..LetifyConfig::default()
            }),
    )
    .await
    .unwrap();
    let relay_url = o.relay.then(|| {
        PeerGate::allow_list([node_id, client_id]).attach(&server);
        server.local_addr().relays.remove(0)
    });
    let signals = MemorySignalHub::new();
    let store = MemoryPkarrStore::new();
    let device = |key: SecretKey, advertise: Vec<SocketAddr>, worker_on: bool| {
        let mut letify = LetifyConfig {
            observer: Some(ObserverAddr { addr: qad, node: Some(server_id) }),
            advertise,
            worker_rendezvous: worker_on,
            worker_url: Some(worker.url.clone()),
            connect_timeout: Duration::from_secs(20),
            ..LetifyConfig::default()
        };
        let channel: Arc<dyn SignalChannel> = signals.channel(key.peer_id());
        match o.rdv {
            Rdv::Memory => letify.signal = Some(channel),
            Rdv::NoPunch => letify.signal = Some(Arc::new(NoPunchSignal(channel))),
            Rdv::Pkarr => {
                letify.pkarr = Some(Arc::new(store.clone()));
                letify.poll_interval = Duration::from_millis(100);
                letify.idle_poll_interval = Duration::from_millis(200);
            }
        }
        let mut cfg = TransportConfig::new(key).backend(BackendKind::Letify).bind_addr("127.0.0.1:0".parse().unwrap());
        if let Some(url) = &relay_url {
            cfg = cfg.relay(RelayConfig::Custom(vec![url.clone()]));
        }
        cfg.letify(letify)
    };
    let node = Transport::bind(device(node_key, vec![dead_address()], o.node_worker)).await.unwrap();
    let client = Transport::bind(device(client_key, vec![], o.client_worker)).await.unwrap();
    PeerGate::allow_list([client_id]).attach(&node);
    spawn_echo(&node, "echo");
    client.add_peer_addr(PeerAddr { peer: node_id, relays: vec![], direct: node.local_addr().direct });
    World { _server: server, node, client, worker }
}

async fn connect(w: &World) -> Connection {
    tokio::time::timeout(Duration::from_secs(25), w.client.connect(w.node.peer_id(), "echo"))
        .await
        .expect("connect timed out")
        .unwrap()
}

async fn wait_direct(conn: &Connection) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !conn.path_state().is_direct() {
        assert!(Instant::now() < deadline, "no direct path: {}", conn.path_state());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Longer than one punch (lead and window) with the answering side's grace.
const PUNCH_OVER: Duration = Duration::from_secs(9);

// ----- the tests ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn em11_worker_a_punch_completes_through_the_worker_when_the_control_path_cannot_carry_it() {
    let w = world(Opts { rdv: Rdv::NoPunch, relay: false, client_worker: true, node_worker: true, mode: Mode::Ok }).await;
    let conn = connect(&w).await;
    wait_direct(&conn).await;
    assert_eq!(echo_roundtrip(&conn, b"over a punch the Worker carried").await, b"over a punch the Worker carried");
    assert!(w.worker.sends(w.client.peer_id()) >= 1, "the client sent its offers through the Worker");
    assert!(w.worker.recvs(w.node.peer_id()) >= 1, "the node read them from the Worker");
}

#[tokio::test(flavor = "multi_thread")]
async fn em11_worker_a_wake_up_through_the_pkarr_mailbox_makes_the_answerer_poll_briefly() {
    let w = world(Opts { rdv: Rdv::Pkarr, relay: false, client_worker: true, node_worker: true, mode: Mode::Ok }).await;
    let conn = connect(&w).await;
    wait_direct(&conn).await;
    assert_eq!(echo_roundtrip(&conn, b"woken by the mailbox").await, b"woken by the mailbox");
    assert!(w.worker.recvs(w.node.peer_id()) >= 1, "the wake-up made the node poll the Worker");
    // The node's long poll ends with the punch: no standing wait.
    tokio::time::sleep(PUNCH_OVER).await;
    let polls = w.worker.recvs(w.node.peer_id());
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(w.worker.recvs(w.node.peer_id()), polls, "the node kept polling the Worker after the punch");
}

#[tokio::test(flavor = "multi_thread")]
async fn em11_worker_no_worker_request_is_made_when_the_switch_is_off() {
    // EMBER_LETIFY_WORKER_RENDEZVOUS=0 on both sides: the punch runs over Ember's own rendezvous.
    let w = world(Opts { rdv: Rdv::Memory, relay: true, client_worker: false, node_worker: false, mode: Mode::Ok }).await;
    let conn = connect(&w).await;
    wait_direct(&conn).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(w.worker.requests(), 0, "a Worker request was made with the switch off");
}

#[tokio::test(flavor = "multi_thread")]
async fn em11_worker_the_answerer_does_not_poll_the_worker_without_a_wake_up() {
    // The node has the Worker on, the client off: nothing wakes the node, so it never polls,
    // neither while idle nor while answering a punch over Ember's own rendezvous.
    let w = world(Opts { rdv: Rdv::Memory, relay: true, client_worker: false, node_worker: true, mode: Mode::Ok }).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(w.worker.recvs(w.node.peer_id()), 0, "an idle node polled the Worker");
    let conn = connect(&w).await;
    wait_direct(&conn).await;
    tokio::time::sleep(PUNCH_OVER).await;
    assert_eq!(w.worker.requests(), 0, "the node polled the Worker without a wake-up");
}

#[tokio::test(flavor = "multi_thread")]
async fn em11_worker_429_retry_after_is_honoured() {
    // The Worker rate-limits with Retry-After: 60. The client's first offer gets the 429, so it
    // wakes nobody, punches over Ember's own rendezvous instead, and asks the Worker nothing
    // more within the Retry-After.
    let w = world(Opts { rdv: Rdv::Memory, relay: true, client_worker: true, node_worker: true, mode: Mode::RateLimited(60) })
        .await;
    let conn = connect(&w).await;
    wait_direct(&conn).await;
    assert_eq!(echo_roundtrip(&conn, b"after a 429").await, b"after a 429");
    tokio::time::sleep(PUNCH_OVER).await;
    assert!(w.worker.sends(w.client.peer_id()) >= 1, "the client tried the Worker first");
    assert_eq!(w.worker.sends_after_refusal(), 0, "the client asked the Worker again within its Retry-After");
    assert_eq!(w.worker.recvs(w.node.peer_id()), 0, "the node was woken although the Worker refused the offer");
}

#[tokio::test(flavor = "multi_thread")]
async fn em11_worker_a_worker_failure_falls_back_to_the_relay() {
    // The Worker refuses (403) and Ember's own rendezvous cannot carry a punch: the session
    // stays on the main server's relay and works.
    let w = world(Opts { rdv: Rdv::NoPunch, relay: true, client_worker: true, node_worker: true, mode: Mode::Forbidden }).await;
    let conn = connect(&w).await;
    assert_eq!(echo_roundtrip(&conn, b"through the relay").await, b"through the relay");
    tokio::time::sleep(PUNCH_OVER).await;
    assert!(conn.path_state().is_relayed(), "expected the relay, got {}", conn.path_state());
    assert_eq!(echo_roundtrip(&conn, b"still through the relay").await, b"still through the relay");
    assert_eq!(w.worker.recvs(w.node.peer_id()), 0, "the node was woken although the Worker refused the offer");
}

// ----- many devices at one answering node (letify-link 3.0.8 Demux) -------------------------

/// A node behind a gate that admits `n` clients, the fake Worker held at most `poll_cap`
/// seconds per `/recv`, and Ember's own rendezvous dropping punch messages, so every punch must
/// go through the Worker. No relay: a connect succeeds only over a punched path.
async fn crowd(n: usize, poll_cap: u64) -> (Transport, Transport, Vec<Transport>, FakeWorker) {
    let worker = FakeWorker::start_with(Mode::Ok, poll_cap).await;
    let qad = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let server_key = SecretKey::generate();
    let server_id = server_key.peer_id();
    let server = Transport::bind(
        TransportConfig::new(server_key)
            .backend(BackendKind::Letify)
            .bind_addr("127.0.0.1:0".parse().unwrap())
            .letify(LetifyConfig { qad_listen: Some(qad), worker_rendezvous: false, ..LetifyConfig::default() }),
    )
    .await
    .unwrap();
    let signals = MemorySignalHub::new();
    let device = |key: SecretKey, advertise: Vec<SocketAddr>| {
        let channel: Arc<dyn SignalChannel> = signals.channel(key.peer_id());
        let letify = LetifyConfig {
            observer: Some(ObserverAddr { addr: qad, node: Some(server_id) }),
            advertise,
            signal: Some(Arc::new(NoPunchSignal(channel))),
            worker_rendezvous: true,
            worker_url: Some(worker.url.clone()),
            connect_timeout: Duration::from_secs(20),
            ..LetifyConfig::default()
        };
        TransportConfig::new(key).backend(BackendKind::Letify).bind_addr("127.0.0.1:0".parse().unwrap()).letify(letify)
    };
    let node = Transport::bind(device(SecretKey::generate(), vec![dead_address()])).await.unwrap();
    let mut clients = Vec::new();
    for _ in 0..n {
        let c = Transport::bind(device(SecretKey::generate(), vec![])).await.unwrap();
        c.add_peer_addr(PeerAddr { peer: node.peer_id(), relays: vec![], direct: node.local_addr().direct });
        clients.push(c);
    }
    PeerGate::allow_list(clients.iter().map(|c| c.peer_id())).attach(&node);
    spawn_echo(&node, "echo");
    (server, node, clients, worker)
}

/// Every client connects to the node at once; each connection must reach a direct path.
async fn all_at_once(node: &Transport, clients: &[Transport]) -> Vec<Connection> {
    let dials = clients.iter().map(|c| async move {
        let conn = tokio::time::timeout(Duration::from_secs(25), c.connect(node.peer_id(), "echo"))
            .await
            .expect("connect timed out")
            .unwrap();
        wait_direct(&conn).await;
        conn
    });
    futures::future::join_all(dials).await
}

#[tokio::test(flavor = "multi_thread")]
async fn em11_worker_three_devices_woken_at_once_all_punch_through_the_worker() {
    let (_server, node, clients, worker) = crowd(3, 25).await;
    let conns = all_at_once(&node, &clients).await;
    for (i, conn) in conns.iter().enumerate() {
        let msg = format!("device {i} over its own punch");
        assert_eq!(echo_roundtrip(conn, msg.as_bytes()).await, msg.as_bytes());
    }
    for c in &clients {
        assert!(worker.sends(c.peer_id()) >= 1, "every device punched through the Worker");
    }
    assert!(worker.recvs(node.peer_id()) >= 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn em11_worker_a_second_wake_up_in_an_open_window_shares_its_one_pump() {
    // Two devices woken together: the node answers both from one windowed Demux, so it never
    // holds more than one `/recv` at the Worker at a time.
    let (_server, node, clients, worker) = crowd(2, 25).await;
    let conns = all_at_once(&node, &clients).await;
    assert_eq!(conns.len(), 2);
    assert!(worker.recvs(node.peer_id()) >= 1);
    assert_eq!(worker.max_inflight(node.peer_id()), 1, "the node ran more than one pump at once");
}

#[tokio::test(flavor = "multi_thread")]
async fn em11_worker_the_pump_stops_when_the_window_closes() {
    // A Worker that holds a `/recv` at most 2 s: a pump still running after the window would
    // poll again at least every 2 s, so a count that stays flat over 5 s means it stopped.
    let (_server, node, clients, worker) = crowd(2, 2).await;
    let _conns = all_at_once(&node, &clients).await;
    assert!(worker.recvs(node.peer_id()) >= 1);
    tokio::time::sleep(PUNCH_OVER).await;
    let polls = worker.recvs(node.peer_id());
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(worker.recvs(node.peer_id()), polls, "the node's pump outlived its window");
    for c in &clients {
        let polls = worker.recvs(c.peer_id());
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(worker.recvs(c.peer_id()), polls, "a client's pump outlived its dial");
    }
}
