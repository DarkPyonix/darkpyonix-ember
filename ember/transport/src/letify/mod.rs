//! letify-link backend (SPEC `FR-N1`, `FR-N2`, `FR-N5`, `INTENT.md` D36). The only module in
//! Ember, besides the iroh backend, that names a transport library.
//!
//! What it does, and what it leaves to letify-link:
//!
//! - **Identity**: Ember's Ed25519 [`SecretKey`] is the letify-link node key, so a [`PeerId`] is
//!   the node id (`FR-N6`: ids are canonical on both sides, and a revoked key stays revoked under
//!   its sign-flipped alias because letify-link keys revocation on `NodeId::canonical`).
//! - **Sessions**: one letify-link session per peer pair, many Ember connections on it
//!   ([`link`]). The dialing side runs a `PathManager` over the peer's TCP addresses and, when a
//!   relay is registered, a relayed path. The accepting side runs a TCP listener and an
//!   `Acceptor`.
//! - **Relay** (`FR-N2`): the main server runs the relay role (`LetifyConfig::relay_listen`);
//!   every other endpoint registers it as `letify://<server peer id>@host:port`. Two endpoints
//!   agree on a pairing token through the rendezvous ([`pkarr`]) and both dial the relay.
//! - **Standing request** (`FR-N9`, #898): the endpoint that runs the relay role keeps one request
//!   waiting at its own relay for every peer it was told to allow, under [`STANDING_TOKEN`]. A
//!   dialer with no rendezvous that dials the relay's own peer id asks the relay for it under the
//!   same token, so a device that knows only `letify://<server peer id>@host:port` reaches the
//!   server. letify-link's relay only pairs two requests; it never hands one to a local acceptor.
//! - **Address observation**: the main server runs a `QadResponder`; other endpoints ask it for
//!   their public address and publish it in their record.
//! - **Revocation** (`FR-N3`): [`crate::PeerGate`] calls `revoke_peer`, which revokes the key in
//!   the acceptor (handshakes, live sessions) and in the relay (waiting and spliced pairs).
//! - **Power** (`FR-N5` mobile): `set_power_state` and `resume_now` map to letify-link's
//!   `PowerState`, `Session::resume_now` and `PathManager::invalidate_paths`.
//! - Rekeying and resumption are letify-link's own: Noise IK, key epochs, resume on a new path.
//!
//! Not done yet (TODO in the SPEC): UDP paths and hole punching (TCP dialing plus the relay only),
//! NAT classification beyond the QAD mapping, and discovering the main server's relay address from
//! the hub (it is configured with `EMBER_RELAY_URL` for now).

mod link;
mod pkarr;
mod relay_guard;
mod relay_url;

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use letify_link::observe::{QadObserver, QadResponder, Reachability};
use letify_link::record::{self, AddressRecord};
use letify_link::relay::{new_token, Relay, RelayConfig as LinkRelayConfig, RelayDialer, RelayHandle, RelayPairing};
use letify_link::rendezvous::{self, Message, Rendezvous};
use letify_link::{
    Accepted, Acceptor, Candidate, Config, Identity, ManagerConfig, NodeId, PathEvent, PathKind, PathManager,
    PeerPolicy,
};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, Notify, Semaphore};
use tokio::task::JoinHandle;

pub use pkarr::HttpPkarrStore;

use link::{run_demux, run_path_watch, Cmd, OpenError, PeerLink};
use pkarr::PkarrNode;
pub use pkarr::{direct_addresses_from_txt, is_record_label};
use relay_url::{parse_relay_url, RelayTarget};

use crate::{
    AddressDirectory, Connection, HubDirectory, LetifyConfig, PeerAddr, PeerId, PkarrStore, PowerState,
    RelayConfig, Result, SecretKey, ServiceTable, SignalChannel, TransportBackend, TransportConfig,
    TransportError,
};

/// The identifier this backend puts in the `transports` field of its address record, and
/// expects in a peer's (SPEC `FR-N5`: a peer whose record does not list it is refused with both
/// lists named).
pub const LETIFY_TRANSPORT_ID: &str = record::DEFAULT_TRANSPORT;

/// At most this many peers' relay offers are served at once.
const MAX_MEETINGS: usize = 8;
/// At most this many candidate TCP addresses are dialed for one peer.
const MAX_TCP_CANDIDATES: usize = 8;

/// The pairing token of a standing request (SPEC `FR-N9`, #898): the main server waits at its own
/// relay for each allowed device under this token, and a device with no rendezvous asks the relay
/// for the server under it. It is public and fixed. It only matches the two requests: the relay
/// authenticates both ends by their node keys and pairs them only when each names the other, and
/// the session on the splice runs its own handshake.
const STANDING_TOKEN: [u8; 16] = *b"ember-standing-1";
/// At most this many standing requests are kept, one for each allowed peer: devices and
/// registered computers alike. letify-link's relay lets one client hold 64 waiting requests
/// (`max_pending`); the rest is left for the server's own relay offers. A peer allowed beyond
/// the cap is queued and gets a slot when one frees. `LetifyConfig::standing_requests` lowers it.
pub(crate) const MAX_STANDING: usize = 48;
/// After this many failures in a row to post one peer's standing request, one warning is logged.
const STANDING_WARN_AFTER: u32 = 5;
/// A standing dial still pending after this long has its request waiting at the relay: the
/// relay's handshake and request share a 4 s deadline.
const STANDING_POSTED_AFTER: Duration = Duration::from_secs(5);
/// How long a dial whose only path is the standing request waits for the pairing, and then for
/// the session on it, when the connect timeout is not shorter (`FR-N9`, provisional).
const STANDING_PAIR_WAIT: Duration = Duration::from_secs(8);
/// How long the failure of such a dial is kept for the calls that were waiting behind it.
const DIAL_FAILURE_SHARE: Duration = Duration::from_secs(2);
/// A relay that ends the connection before pairing is asked this many times, this far apart,
/// before the dial fails: the relay closes a peer it does not admit with no reply, and it closes
/// a connection over its budget of unauthenticated connections the same way.
const RELAY_CLOSED_TRIES: u32 = 3;
const RELAY_CLOSED_PAUSE: Duration = Duration::from_millis(300);
/// The pause after a refusal the relay states (a request of ours from an earlier dial still
/// waits there, too many waiting requests).
const RELAY_REFUSED_PAUSE: Duration = Duration::from_millis(500);
/// At most this many standing requests start their relay handshake at once, each for this long.
/// The relay holds at most four unauthenticated connections from one address
/// (`max_preauth_per_ip`), and every standing request comes from loopback.
const STANDING_STARTS: usize = 2;
const STANDING_HEAD_START: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Via {
    Direct,
    Relay,
}

fn node_of(peer: PeerId) -> NodeId {
    NodeId::from_bytes(*peer.as_bytes())
}

fn peer_of(node: NodeId) -> Result<PeerId> {
    PeerId::from_bytes(*node.as_bytes())
}

fn link_power(state: PowerState) -> letify_link::PowerState {
    match state {
        PowerState::Foreground => letify_link::PowerState::Foreground,
        PowerState::Background => letify_link::PowerState::Background,
        PowerState::PowerSave => letify_link::PowerState::PowerSave,
    }
}

/// letify-link's `Rendezvous` over an Ember [`SignalChannel`].
struct SignalAdapter(Arc<dyn SignalChannel>);

impl Rendezvous for SignalAdapter {
    async fn send(&self, to: NodeId, msg: Vec<u8>) -> io::Result<()> {
        let to = peer_of(to).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        self.0.send(to, msg).await.map_err(|e| io::Error::other(e.to_string()))
    }

    async fn recv(&self) -> io::Result<(NodeId, Vec<u8>)> {
        let (from, msg) = self.0.recv().await.map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e.to_string()))?;
        Ok((node_of(from), msg))
    }
}

/// The standing requests of an endpoint with the relay role (`FR-N9`, #898).
#[derive(Default)]
struct Standing {
    /// The running requests, by the canonical id of the peer each waits for.
    running: HashMap<NodeId, JoinHandle<()>>,
    /// Allowed peers with no slot yet, in the order they were allowed.
    queued: VecDeque<(NodeId, PeerId)>,
}

/// Failures in a row to post one standing request, and whether they were warned about.
#[derive(Default)]
struct Streak {
    failures: u32,
    warned: bool,
}

impl Streak {
    /// Counts one failure. True when it is the one to warn about: once per streak.
    fn failed(&mut self) -> bool {
        self.failures += 1;
        if self.failures >= STANDING_WARN_AFTER && !self.warned {
            self.warned = true;
            return true;
        }
        false
    }

    /// Ends the streak. True when it was warned about, so the recovery is worth a line.
    fn recovered(&mut self) -> bool {
        self.failures = 0;
        std::mem::take(&mut self.warned)
    }
}

/// Why one request at the relay did not end in a pairing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RelayFailure {
    /// The relay's address did not take the connection (refused, no route, no such name).
    Unreachable,
    /// The relay ended the connection before it paired: during the handshake or with no reply.
    Closed,
    /// The relay answered with a refusal.
    Refused,
    /// The relay held the request and nobody came.
    NoAnswer,
}

fn relay_failure(e: &io::Error) -> RelayFailure {
    use io::ErrorKind as K;
    // A handshake error comes wrapped (`From<letify_link::Error> for io::Error`).
    if letify_link::Error::from_io(e).is_some() {
        return RelayFailure::Closed;
    }
    match e.kind() {
        K::PermissionDenied => RelayFailure::Refused,
        K::TimedOut => RelayFailure::NoAnswer,
        K::UnexpectedEof | K::ConnectionReset | K::ConnectionAborted | K::BrokenPipe => RelayFailure::Closed,
        _ => RelayFailure::Unreachable,
    }
}

struct Inner {
    me: PeerId,
    secret: SecretKey,
    identity: Identity,
    cfg: LetifyConfig,
    link_cfg: Config,
    services: Arc<ServiceTable>,
    acceptor: Arc<Acceptor>,
    /// The policy the acceptor uses; sessions we start are closed when it revokes their peer.
    policy: PeerPolicy,
    listen: SocketAddr,
    directory: Option<Arc<dyn AddressDirectory>>,
    links: Mutex<HashMap<PeerId, Vec<Arc<PeerLink>>>>,
    dialing: Mutex<HashMap<PeerId, Arc<tokio::sync::Mutex<()>>>>,
    hints: Mutex<HashMap<PeerId, PeerAddr>>,
    /// The relay to use: configured, or this endpoint's own relay role.
    relay: Mutex<Option<RelayTarget>>,
    own_relay: Option<RelayTarget>,
    own_relay_standing: Option<RelayTarget>,
    relay_handle: Option<RelayHandle>,
    signal: Mutex<Option<Arc<SignalAdapter>>>,
    /// Devices admitted through an attached gate (canonical ids); `None` until a gate attaches.
    admitted: Mutex<Option<std::collections::HashSet<NodeId>>>,
    pkarr: Mutex<Option<Arc<PkarrNode>>>,
    publish_direct: AtomicBool,
    observed: Mutex<Option<SocketAddr>>,
    /// The address observer connected back to our TCP port: the guessed candidate is true.
    tcp_reachable: AtomicBool,
    reobserve: Notify,
    version: Mutex<u32>,
    power: Mutex<PowerState>,
    meetings: Arc<Semaphore>,
    /// The standing requests at our own relay (`FR-N9`, #898). Empty unless this endpoint runs
    /// the relay role.
    standing: Mutex<Standing>,
    /// The last failure of a dial whose only path was the standing request, by peer: calls that
    /// waited behind that dial get it for [`DIAL_FAILURE_SHARE`] and do not dial in turn.
    dial_failures: Mutex<HashMap<PeerId, (Instant, String)>>,
    standing_starts: Arc<Semaphore>,
    closed: AtomicBool,
    bg: Mutex<Vec<JoinHandle<()>>>,
}

/// The letify-link backend. Dropping the last handle stops its tasks.
pub(crate) struct LetifyBackend {
    inner: Arc<Inner>,
}

impl Drop for LetifyBackend {
    fn drop(&mut self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        for h in self.inner.bg.lock().unwrap().drain(..) {
            h.abort();
        }
        {
            let mut standing = self.inner.standing.lock().unwrap();
            standing.queued.clear();
            for (_, h) in standing.running.drain() {
                h.abort();
            }
        }
        for list in self.inner.links.lock().unwrap().values() {
            for l in list {
                l.abort_all();
            }
        }
        *self.inner.pkarr.lock().unwrap() = None;
    }
}

impl LetifyBackend {
    pub(crate) async fn bind(config: TransportConfig) -> Result<Arc<Self>> {
        let me = config.secret_key.peer_id();
        let identity = Identity::from_secret_bytes(config.secret_key.to_bytes());
        identity
            .node_id()
            .validate()
            .map_err(|e| TransportError::InvalidKey(format!("not usable as a letify-link node key: {e}")))?;
        let cfg = config.letify.clone();

        let bind_addr = config.bind_addr.unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 0)));
        let listener = TcpListener::bind(bind_addr)
            .await
            .map_err(|e| TransportError::Bind(format!("tcp {bind_addr}: {e}")))?;
        let listen = listener.local_addr().map_err(|e| TransportError::Bind(e.to_string()))?;

        let mut link_cfg = if cfg.mobile { Config::mobile() } else { Config::default() };
        link_cfg.resume_window = cfg.resume_window;
        // Admission is the PeerGate's job (it sees the service); revocation reaches the policy
        // through `revoke_peer`.
        let policy = PeerPolicy::from_fn(|_| true);
        let acceptor = Arc::new(Acceptor::new(identity.clone(), policy.clone(), link_cfg.clone()));

        let mut bg: Vec<JoinHandle<()>> = Vec::new();

        // The relay role (the main server).
        let mut relay_handle = None;
        let mut own_relay = None;
        let mut own_relay_standing = None;
        if let Some(addr) = cfg.relay_listen {
            let policy = if cfg.relay_open {
                PeerPolicy::from_fn(|_| true)
            } else {
                PeerPolicy::allow(vec![identity.node_id()])
            };
            // Bind the underlying relay on a private loopback port with headroom for both
            // guarded public peers and the server's own standing requests. Public peers enter
            // through `relay_guard` (per-source concurrency + sliding-window rate limit), while
            // the server's own standing requests dial the private loopback port directly so a
            // public connection flood cannot starve them (FR-N9, #920).
            let backend_bind = match addr.ip() {
                std::net::IpAddr::V6(_) => "[::1]:0",
                std::net::IpAddr::V4(_) => "127.0.0.1:0",
            };
            let relay_cfg = LinkRelayConfig {
                max_preauth: 256,
                max_preauth_per_ip: 256,
                ..LinkRelayConfig::default()
            };
            let relay = Relay::bind(backend_bind, identity.clone(), policy, relay_cfg)
                .await
                .map_err(|e| TransportError::Bind(format!("relay backend {backend_bind}: {e}")))?;
            let backend_bound = relay.local_addr();
            let (bound, guard_task) =
                relay_guard::bind_relay_guard(addr, backend_bound, relay_guard::RelayGuardConfig::default())
                    .await
                    .map_err(|e| TransportError::Bind(format!("relay {addr}: {e}")))?;
            relay.allow(identity.node_id());
            relay_handle = Some(relay.handle());
            let local = match bound.ip() {
                ip if !ip.is_unspecified() => bound,
                std::net::IpAddr::V6(_) => SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, bound.port())),
                std::net::IpAddr::V4(_) => SocketAddr::from(([127, 0, 0, 1], bound.port())),
            };
            own_relay = Some(RelayTarget {
                addr: local.to_string(),
                node: identity.node_id(),
                url: format!("letify://{me}@{local}"),
            });
            own_relay_standing = Some(RelayTarget {
                addr: backend_bound.to_string(),
                node: identity.node_id(),
                url: format!("letify://{me}@{backend_bound}"),
            });
            bg.push(tokio::spawn(relay.run()));
            bg.push(guard_task);
        }

        // The address observer role (the main server).
        if let Some(addr) = cfg.qad_listen {
            let responder = QadResponder::bind(&addr.to_string(), identity.clone())
                .await
                .map_err(|e| TransportError::Bind(format!("address observer {addr}: {e}")))?;
            bg.push(tokio::spawn(responder.run()));
        }

        let mut relay = None;
        if let RelayConfig::Custom(urls) = &config.relay {
            for url in urls {
                match parse_relay_url(url) {
                    Ok(t) => {
                        relay = Some(t);
                        break;
                    }
                    Err(e) => tracing::warn!("ignoring relay for the letify backend: {e}"),
                }
            }
        }
        let relay = relay.or_else(|| own_relay.clone());

        let inner = Arc::new(Inner {
            me,
            secret: config.secret_key.clone(),
            identity,
            cfg: cfg.clone(),
            link_cfg,
            services: Arc::new(ServiceTable::default()),
            acceptor,
            policy,
            listen,
            directory: config.directory.clone(),
            links: Mutex::new(HashMap::new()),
            dialing: Mutex::new(HashMap::new()),
            hints: Mutex::new(HashMap::new()),
            relay: Mutex::new(relay),
            own_relay,
            own_relay_standing,
            relay_handle,
            signal: Mutex::new(None),
            admitted: Mutex::new(None),
            pkarr: Mutex::new(None),
            publish_direct: AtomicBool::new(true),
            observed: Mutex::new(None),
            tcp_reachable: AtomicBool::new(false),
            reobserve: Notify::new(),
            version: Mutex::new(record::unix_now()),
            power: Mutex::new(PowerState::Foreground),
            meetings: Arc::new(Semaphore::new(MAX_MEETINGS)),
            standing: Mutex::new(Standing::default()),
            dial_failures: Mutex::new(HashMap::new()),
            standing_starts: Arc::new(Semaphore::new(STANDING_STARTS)),
            closed: AtomicBool::new(false),
            bg: Mutex::new(Vec::new()),
        });

        bg.push(tokio::spawn(accept_loop(inner.clone(), listener)));
        if let Some(obs) = cfg.observer {
            bg.push(tokio::spawn(observe_loop(inner.clone(), obs.addr, obs.node)));
        }
        inner.bg.lock().unwrap().extend(bg);

        if let Some(signal) = cfg.signal.clone() {
            inner.set_signal(signal);
        }
        if let Some(store) = cfg.pkarr.clone() {
            inner.start_pkarr(store, true);
        }
        let backend = Arc::new(Self { inner });
        if let Some(hub) = config.hub {
            backend.enable_hub(hub)?;
        }
        backend.inner.republish();
        Ok(backend)
    }
}

impl Inner {
    fn relay_target(&self) -> Option<RelayTarget> {
        self.relay.lock().unwrap().clone()
    }

    fn signal(&self) -> Option<Arc<SignalAdapter>> {
        self.signal.lock().unwrap().clone()
    }

    fn pkarr(&self) -> Option<Arc<PkarrNode>> {
        self.pkarr.lock().unwrap().clone()
    }

    fn watch(&self, peers: &[PeerId]) {
        if let Some(s) = self.signal() {
            s.0.watch(peers);
        }
        if let Some(p) = self.pkarr() {
            p.watch_peers(peers);
        }
    }

    /// The addresses peers may dial over TCP.
    fn advertised(&self) -> Vec<SocketAddr> {
        if !self.cfg.advertise.is_empty() {
            return self.cfg.advertise.clone();
        }
        let mut out = Vec::new();
        if !self.listen.ip().is_unspecified() {
            out.push(self.listen);
        }
        let seen = *self.observed.lock().unwrap();
        if let Some(seen) = seen.filter(|_| self.tcp_reachable.load(Ordering::Relaxed)) {
            // The observed public IP with our listening port, published only after the observer
            // connected back to it (QAD dial-back): a host with a public address or a forwarded
            // port (the main server). A NAT'd host is not reachable and publishes no guess.
            let guess = SocketAddr::new(seen.ip(), self.listen.port());
            if !out.contains(&guess) {
                out.push(guess);
            }
        }
        out
    }

    fn local_addr(&self) -> PeerAddr {
        let direct = self.advertised();
        let relays = match self.relay_target() {
            // This endpoint is the relay: other endpoints reach it where it advertises its TCP
            // address, on the relay's port.
            Some(t) if self.own_relay.as_ref().is_some_and(|own| own.url == t.url) => {
                let port = t.addr.parse::<SocketAddr>().map(|a| a.port()).unwrap_or(0);
                let ip = direct.first().map(|a| a.ip()).unwrap_or_else(|| [127, 0, 0, 1].into());
                vec![format!("letify://{}@{}", self.me, SocketAddr::new(ip, port))]
            }
            Some(t) => vec![t.url],
            None => Vec::new(),
        };
        PeerAddr { peer: self.me, relays, direct }
    }

    /// The signed record we publish, as TXT strings.
    fn record_txt(&self) -> Vec<String> {
        let version = {
            let mut v = self.version.lock().unwrap();
            *v = (*v + 1).max(record::unix_now());
            *v
        };
        let mut rec = AddressRecord::new(node_of(self.me), version);
        if self.publish_direct.load(Ordering::Relaxed) {
            rec.tcp = self.advertised();
        }
        rec.udp = self.observed.lock().unwrap().iter().copied().collect();
        rec.transports = vec![LETIFY_TRANSPORT_ID.to_string()];
        rec.encode_txt(&self.identity)
    }

    /// Our addresses changed: tell the directories.
    fn republish(&self) {
        if let Some(dir) = &self.directory {
            dir.publish(&self.local_addr());
        }
        if let Some(p) = self.pkarr() {
            p.republish();
        }
    }

    /// Whether to meet `sender` at `relay` (`RelayPairing`'s trust check): the relay must be
    /// our main server, and when a gate is attached the sender must be an admitted device.
    fn trusts(&self, sender: &NodeId, relay: &NodeId) -> bool {
        let ours = self.relay_target().is_some_and(|t| t.node.canonical() == relay.canonical());
        let allowed = match self.admitted.lock().unwrap().as_ref() {
            Some(set) => set.contains(&sender.canonical()),
            None => true,
        };
        ours && allowed
    }

    fn set_signal(self: &Arc<Self>, channel: Arc<dyn SignalChannel>) {
        let adapter = Arc::new(SignalAdapter(channel));
        {
            let mut slot = self.signal.lock().unwrap();
            if slot.is_some() {
                return;
            }
            *slot = Some(adapter.clone());
        }
        let known: Vec<PeerId> = self.hints.lock().unwrap().keys().copied().collect();
        adapter.0.watch(&known);
        let weak = Arc::downgrade(self);
        let pairing = RelayPairing::new(adapter.clone(), self.identity.clone(), self.acceptor.clone(), move |sender, relay, _address| {
            weak.upgrade().is_some_and(|inner| inner.trusts(sender, relay))
        })
        .pair_wait(Duration::from_secs(20));
        self.bg.lock().unwrap().push(tokio::spawn(signal_loop(self.clone(), adapter, pairing)));
    }

    fn start_pkarr(self: &Arc<Self>, store: Arc<dyn PkarrStore>, publish_direct: bool) {
        self.publish_direct.store(publish_direct, Ordering::Relaxed);
        let node = {
            let mut slot = self.pkarr.lock().unwrap();
            if slot.is_some() {
                None
            } else {
                let weak = Arc::downgrade(self);
                let record: pkarr::RecordFn =
                    Arc::new(move || weak.upgrade().map(|i| i.record_txt()).unwrap_or_default());
                let plan = pkarr::PollPlan::new(
                    self.cfg.poll_interval,
                    self.cfg.idle_poll_interval,
                    self.cfg.fast_window,
                );
                let node = PkarrNode::start(self.secret.clone(), store, record, plan);
                *slot = Some(node.clone());
                Some(node)
            }
        };
        if let Some(node) = node {
            let known: Vec<PeerId> = self.hints.lock().unwrap().keys().copied().collect();
            node.watch_peers(&known);
            let channel: Arc<dyn SignalChannel> = node;
            self.set_signal(channel);
        }
    }

    // ----- links -------------------------------------------------------------------------

    fn add_link(&self, link: Arc<PeerLink>) {
        let mut links = self.links.lock().unwrap();
        let list = links.entry(link.peer).or_default();
        list.retain(|l| !l.is_ended());
        list.push(link);
    }

    /// Registers `link` and closes it at once when the transport is already shutting down.
    /// `shutdown` sets `closed` FIRST and only then lists the links to close, so a link added after
    /// that listing would stay open on both ends (#411: after `b.close()` neither the client nor the
    /// accepted side of a just accepted connection closed). Registering first and reading the flag
    /// second means one of the two sees the other: either `shutdown` lists the link, or `closed` is
    /// already true here and the link is closed from here.
    fn register_link(&self, link: Arc<PeerLink>) {
        self.add_link(link.clone());
        if self.closed.load(Ordering::SeqCst) {
            tokio::spawn(async move { link.close_local(None).await });
        }
    }

    fn live_link(&self, peer: PeerId) -> Option<Arc<PeerLink>> {
        let mut links = self.links.lock().unwrap();
        let list = links.get_mut(&peer)?;
        list.retain(|l| !l.is_ended());
        list.first().cloned()
    }

    fn all_links(&self) -> Vec<Arc<PeerLink>> {
        self.links.lock().unwrap().values().flatten().cloned().collect()
    }

    fn dial_lock(&self, peer: PeerId) -> Arc<tokio::sync::Mutex<()>> {
        self.dialing.lock().unwrap().entry(peer).or_default().clone()
    }

    fn relay_label(&self) -> String {
        self.relay_target().map(|t| t.addr).unwrap_or_else(|| "relay".to_string())
    }

    /// A transport arrived (directly or through the relay) and the acceptor routed it.
    fn on_accepted(self: &Arc<Self>, accepted: Accepted, via: Via) {
        match accepted {
            Accepted::Session(session) => {
                let peer = match peer_of(session.peer_presented()) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!("refusing a session from an unusable peer id: {e}");
                        return;
                    }
                };
                let link = PeerLink::new(peer, false, &session, self.services.clone(), self.relay_label(), via == Via::Relay);
                let power = *self.power.lock().unwrap();
                if power != PowerState::Foreground {
                    session.set_power_state(link_power(power));
                }
                let watcher_session = session.downgrade().upgrade();
                let demux = tokio::spawn(run_demux(link.clone(), session));
                link.set_demux(demux.abort_handle());
                if let Some(s) = watcher_session {
                    link.add_task(tokio::spawn(run_path_watch(link.clone(), s)).abort_handle());
                }
                self.register_link(link);
            }
            Accepted::Subflow(id) => {
                if let Some(link) = self.all_links().into_iter().find(|l| l.session_id == id) {
                    if via == Via::Direct {
                        link.note_direct_join();
                    }
                    link.update_state();
                }
            }
        }
    }

    /// The session an initiating `PathManager` established.
    fn adopt_initiated(self: &Arc<Self>, peer: PeerId, pm: PathManager) -> Arc<PeerLink> {
        let link = PeerLink::new(peer, true, pm.session(), self.services.clone(), self.relay_label(), false);
        let kinds = pm.kinds();
        link.set_kind_of(Arc::new(move |id| kinds.of(id)));
        let (tx, rx) = mpsc::unbounded_channel();
        link.set_cmds(tx);
        let power = *self.power.lock().unwrap();
        if power != PowerState::Foreground {
            link.set_power(link_power(power));
        }
        let for_demux = pm.session().downgrade().upgrade();
        let for_watch = pm.session().downgrade().upgrade();
        // Close the session when the policy revokes its peer (also without going through
        // `revoke`), and do not redial.
        pm.close_when_revoked(&self.policy);
        link.add_task(tokio::spawn(manage(pm, rx)).abort_handle());
        if let Some(s) = for_demux {
            let demux = tokio::spawn(run_demux(link.clone(), s));
            link.set_demux(demux.abort_handle());
        }
        if let Some(s) = for_watch {
            link.add_task(tokio::spawn(run_path_watch(link.clone(), s)).abort_handle());
        }
        self.register_link(link.clone());
        link
    }

    async fn link_to(self: &Arc<Self>, addr: &PeerAddr) -> Result<Arc<PeerLink>> {
        if let Some(l) = self.live_link(addr.peer) {
            return Ok(l);
        }
        let arrived = Instant::now();
        let gate = self.dial_lock(addr.peer);
        let _guard = gate.lock().await;
        if let Some(l) = self.live_link(addr.peer) {
            return Ok(l);
        }
        if let Some(reason) = self.shared_failure(addr.peer, arrived) {
            return Err(TransportError::Connect { peer: addr.peer, reason });
        }
        self.dial(addr).await
    }

    /// The failure of the dial this call waited behind, when that dial had only the standing
    /// request to try and failed less than [`DIAL_FAILURE_SHARE`] ago. A call that arrived after
    /// the failure dials for itself.
    fn shared_failure(&self, peer: PeerId, arrived: Instant) -> Option<String> {
        let mut failures = self.dial_failures.lock().unwrap();
        let (at, reason) = failures.get(&peer).map(|(at, reason)| (*at, reason.clone()))?;
        if at.elapsed() >= DIAL_FAILURE_SHARE {
            failures.remove(&peer);
            return None;
        }
        (arrived <= at).then_some(reason)
    }

    /// Establishes a session with `addr.peer` over every path we know. The failure of a dial
    /// that had only the standing request is kept for the calls waiting behind it.
    async fn dial(self: &Arc<Self>, addr: &PeerAddr) -> Result<Arc<PeerLink>> {
        let mut relay_only = false;
        let out = self.dial_paths(addr, &mut relay_only).await;
        if relay_only {
            let mut failures = self.dial_failures.lock().unwrap();
            match &out {
                Err(TransportError::Connect { reason, .. }) => {
                    failures.insert(addr.peer, (Instant::now(), reason.clone()));
                }
                _ => {
                    failures.remove(&addr.peer);
                }
            }
        }
        out
    }

    /// `relay_only` is set when the standing request at the peer's own relay is the only path.
    async fn dial_paths(self: &Arc<Self>, addr: &PeerAddr, relay_only: &mut bool) -> Result<Arc<PeerLink>> {
        let peer = addr.peer;
        let fail = |reason: String| TransportError::Connect { peer, reason };
        let node = NodeId::try_from_bytes(*peer.as_bytes()).map_err(|e| fail(e.to_string()))?;
        self.watch(&[peer]);
        // A dial reads the peer's mailbox fast at once, so its answer is not held for the idle
        // interval (FR-N2, #783).
        if let Some(p) = self.pkarr() {
            p.kick(peer);
        }

        let mut tcp: Vec<SocketAddr> = addr.direct.clone();
        let hinted = self.hints.lock().unwrap().get(&peer).cloned();
        if let Some(h) = hinted {
            tcp.extend(h.direct);
        }
        if let Some(dir) = &self.directory {
            if let Some(found) = dir.resolve(peer).await {
                tcp.extend(found.direct);
            }
        }
        if let Some(pk) = self.pkarr() {
            match pk.lookup(peer).await {
                Ok(Some(rec)) => {
                    if !rec.speaks(LETIFY_TRANSPORT_ID) {
                        return Err(fail(format!(
                            "the peer accepts transports {:?}; this endpoint speaks [{LETIFY_TRANSPORT_ID:?}]",
                            rec.transports
                        )));
                    }
                    tcp.extend(rec.tcp);
                }
                Ok(None) => {}
                Err(e) => tracing::debug!(peer = %peer.fmt_short(), error = %e, "looking up the peer's record failed"),
            }
        }
        let mut seen = std::collections::HashSet::new();
        tcp.retain(|a| seen.insert(*a));
        tcp.truncate(MAX_TCP_CANDIDATES);

        let relay = addr
            .relays
            .iter()
            .find_map(|u| parse_relay_url(u).ok())
            .or_else(|| self.relay_target());
        let signal = self.signal();

        let mut candidates: Vec<Candidate> = tcp.iter().map(|a| Candidate::tcp(format!("tcp:{a}"), *a)).collect();
        let mut standing: Option<(RelayDialer, String)> = None;
        let relayed = match (&relay, &signal) {
            // The peer is the relay itself (the main server) and there is no rendezvous to agree
            // on a token through: the server keeps a request for every peer it allows waiting
            // at its own relay under the standing token (FR-N9, #898). Its candidate is added
            // below.
            (Some(relay), None) if relay.node.canonical() == node.canonical() => {
                standing = Some((RelayDialer::new(relay.addr.clone(), relay.node, self.identity.clone()), relay.addr.clone()));
                true
            }
            (Some(relay), Some(signal)) => {
                candidates.push(relay_candidate(self.identity.clone(), relay, signal.clone(), node));
                true
            }
            _ => false,
        };
        if candidates.is_empty() && standing.is_none() {
            return Err(fail(if relay.is_some() {
                "no address is known for the peer and there is no rendezvous to reach it through the relay".into()
            } else {
                "no address is known for the peer and no relay is registered".into()
            }));
        }
        if !relayed && refused_by_all(&tcp).await {
            return Err(fail("every known address refused the connection".into()));
        }

        let mut manager_cfg = ManagerConfig {
            dial_timeout: self.cfg.dial_timeout,
            start_timeout: self.cfg.connect_timeout,
            ..ManagerConfig::default()
        };
        if let Some((dialer, relay_addr)) = standing {
            // With no direct address that could answer, the standing request is the only path.
            // The path manager redials a failing candidate until the connect timeout, so the
            // first request is made here, where a relay that cannot be reached or that turns
            // this endpoint away ends the dial at once, and the wait for the server is bounded
            // (FR-N9). With a direct address in play the dial runs as before.
            *relay_only = tcp.is_empty() || refused_by_all(&tcp).await;
            let first = if *relay_only {
                let wait = self.cfg.connect_timeout.min(STANDING_PAIR_WAIT);
                manager_cfg.start_timeout = wait;
                Some(standing_first(&dialer, &relay_addr, node, wait).await.map_err(&fail)?)
            } else {
                None
            };
            candidates.push(standing_candidate(dialer, &relay_addr, node, first));
        }
        let pm = PathManager::start(self.identity.clone(), node, candidates, self.link_cfg.clone(), manager_cfg)
            .await
            .map_err(|e| fail(e.to_string()))?;
        Ok(self.adopt_initiated(peer, pm))
    }

    async fn connect(self: &Arc<Self>, addr: PeerAddr, service: &str) -> Result<Connection> {
        let peer = addr.peer;
        let fail = |reason: String| TransportError::Connect { peer, reason };
        if self.closed.load(Ordering::SeqCst) {
            return Err(fail("local transport is closed".into()));
        }
        if peer == self.me {
            return Err(fail("cannot connect to this endpoint itself".into()));
        }
        let mut last = String::new();
        for _ in 0..2 {
            let link = self.link_to(&addr).await?;
            match link.open_conn(service).await {
                Ok(conn) => return Ok(conn),
                Err(OpenError::Refused(reason)) => return Err(fail(reason)),
                Err(OpenError::Dead(why)) => {
                    // The session died under us; one fresh session may work.
                    link.abort_all();
                    last = why;
                }
            }
        }
        Err(fail(last))
    }

    async fn shutdown(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let bg: Vec<JoinHandle<()>> = self.bg.lock().unwrap().drain(..).collect();
        for h in &bg {
            h.abort();
        }
        // Wait for the listener tasks to drop their sockets, so a later connect is refused.
        for h in bg {
            let _ = h.await;
        }
        let standing: Vec<JoinHandle<()>> = {
            let mut standing = self.standing.lock().unwrap();
            standing.queued.clear();
            let handles: Vec<JoinHandle<()>> = standing.running.drain().map(|(_, h)| h).collect();
            handles
        };
        for h in &standing {
            h.abort();
        }
        for h in standing {
            let _ = h.await;
        }
        let links = self.all_links();
        futures::future::join_all(links.iter().map(|l| l.close_local(None))).await;
        *self.pkarr.lock().unwrap() = None;
    }

    fn revoke(self: &Arc<Self>, peer: PeerId) {
        let node = node_of(peer);
        if let Some(set) = self.admitted.lock().unwrap().as_mut() {
            set.remove(&node.canonical());
        }
        // Accepted sessions: refuse new handshakes and close live sessions with "revoked".
        self.acceptor.revoke(node);
        if let Some(h) = &self.relay_handle {
            h.revoke(node);
        }
        self.release_standing(node);
        // Sessions we started to that peer (or to an alias of its key): close, and do not redial.
        let class = node.canonical();
        let Ok(handle) = tokio::runtime::Handle::try_current() else { return };
        for link in self.all_links() {
            if node_of(link.peer).canonical() == class {
                handle.spawn(async move { link.close_local(Some("revoked")).await });
            }
        }
    }

    fn allow(self: &Arc<Self>, peer: PeerId) {
        let node = node_of(peer);
        self.admitted.lock().unwrap().get_or_insert_with(Default::default).insert(node.canonical());
        self.acceptor.reinstate(node);
        if let Some(h) = &self.relay_handle {
            h.allow(node);
        }
        self.watch(&[peer]);
        self.start_standing(node, peer);
    }

    /// The most standing requests this endpoint keeps.
    fn standing_cap(&self) -> usize {
        self.cfg.standing_requests.min(MAX_STANDING)
    }

    /// Keeps a request for `node` waiting at this endpoint's own relay (the main server only), so
    /// a peer that has the relay URL and no rendezvous is paired with us (`FR-N9`, #898). When
    /// every slot is in use the peer is queued and gets one when another peer is revoked. Does
    /// nothing without the relay role, outside a runtime, for ourselves, or when the peer has a
    /// request or a place in the queue already.
    fn start_standing(self: &Arc<Self>, node: NodeId, peer: PeerId) {
        if self.own_relay.is_none() || tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let class = node.canonical();
        if self.closed.load(Ordering::SeqCst) || class == self.identity.node_id().canonical() {
            return;
        }
        let mut standing = self.standing.lock().unwrap();
        standing.running.retain(|_, h| !h.is_finished());
        if standing.running.contains_key(&class) || standing.queued.iter().any(|(q, _)| q.canonical() == class) {
            return;
        }
        standing.queued.push_back((node, peer));
        self.fill_standing(&mut standing);
        if standing.queued.iter().any(|(q, _)| q.canonical() == class) {
            let cap = self.standing_cap();
            tracing::warn!(
                %peer,
                "peer {peer} is allowed, but all {cap} standing requests at this server's relay are in use (every allowed device and registered computer takes one), so it cannot reach this server through the relay alone yet. It is queued and gets a slot when another peer is removed: remove one that is no longer used (ember-server owner devices rm <peer id>, or remove a computer). Until then it reaches this server by a direct address (EMBER_TRANSPORT_LISTEN) or through the hub"
            );
        }
    }

    /// Ends the standing request for `node` (or its place in the queue) and gives the freed slot
    /// to the peer that has waited longest.
    fn release_standing(self: &Arc<Self>, node: NodeId) {
        let class = node.canonical();
        let mut standing = self.standing.lock().unwrap();
        if let Some(h) = standing.running.remove(&class) {
            h.abort();
        }
        standing.queued.retain(|(q, _)| q.canonical() != class);
        for peer in self.fill_standing(&mut standing) {
            tracing::info!(%peer, "a standing request slot is free: peer {peer} now waits at this server's relay and can reach it through the relay alone");
        }
    }

    /// Starts standing requests for queued peers while slots are free. Returns the peers started.
    fn fill_standing(self: &Arc<Self>, standing: &mut Standing) -> Vec<PeerId> {
        let mut started = Vec::new();
        let Some(own) = self.own_relay_standing.clone().or_else(|| self.own_relay.clone()) else { return started };
        let Ok(handle) = tokio::runtime::Handle::try_current() else { return started };
        if self.closed.load(Ordering::SeqCst) {
            return started;
        }
        standing.running.retain(|_, h| !h.is_finished());
        while standing.running.len() < self.standing_cap() {
            let Some((node, peer)) = standing.queued.pop_front() else { break };
            let task = handle.spawn(standing_loop(Arc::downgrade(self), own.clone(), node, self.standing_starts.clone()));
            standing.running.insert(node.canonical(), task);
            started.push(peer);
        }
        started
    }
}

/// The first request of a dial whose only path is the standing request: the transport the relay
/// paired with the server, or why there is none. It fails at once when the relay cannot be
/// reached, after [`RELAY_CLOSED_TRIES`] tries when the relay ends the connection before pairing,
/// and after `wait` when the server never comes. The reasons name no close code and do not say
/// "not allowed" or "revoked": the relay does not tell which it is, and the app reads those words
/// as the server's own refusal.
async fn standing_first(
    dialer: &RelayDialer,
    relay: &str,
    peer: NodeId,
    wait: Duration,
) -> std::result::Result<TcpStream, String> {
    let deadline = tokio::time::Instant::now() + wait;
    let no_answer = || {
        format!(
            "the server did not answer at its relay {relay} within {} s (it is not waiting there for this peer id, or it is not running behind that address)",
            wait.as_secs_f32()
        )
    };
    let mut closed = 0u32;
    loop {
        let e = match tokio::time::timeout_at(deadline, dialer.connect(peer, STANDING_TOKEN)).await {
            Ok(Ok(tcp)) => return Ok(tcp),
            Ok(Err(e)) => e,
            Err(_) => return Err(no_answer()),
        };
        let pause = match relay_failure(&e) {
            RelayFailure::Unreachable => return Err(format!("the relay at {relay} cannot be reached: {e}")),
            RelayFailure::NoAnswer => return Err(no_answer()),
            RelayFailure::Closed => {
                closed += 1;
                tracing::debug!(%relay, error = %e, "the relay ended the connection before pairing");
                if closed >= RELAY_CLOSED_TRIES {
                    return Err(format!(
                        "the relay at {relay} ended the connection before pairing: the server does not admit this peer id, or the relay is busy"
                    ));
                }
                RELAY_CLOSED_PAUSE
            }
            RelayFailure::Refused => RELAY_REFUSED_PAUSE,
        };
        if tokio::time::Instant::now() + pause >= deadline {
            return Err(format!("no pairing at the relay {relay} in time: {e}"));
        }
        tokio::time::sleep(pause).await;
    }
}

/// The path to the relay's own peer with no rendezvous: ask the relay for it under the standing
/// token, where the peer (the main server) keeps a request waiting for each peer it allows.
/// `first` is a transport the relay already paired; the first dial hands it over, later ones ask
/// the relay again.
fn standing_candidate(dialer: RelayDialer, relay: &str, peer: NodeId, first: Option<TcpStream>) -> Candidate {
    let first = Arc::new(Mutex::new(first));
    Candidate::new(format!("relay:{relay}"), move || {
        let dialer = dialer.clone();
        let ready = first.lock().unwrap().take();
        async move {
            match ready {
                Some(tcp) => Ok(tcp),
                None => dialer.connect(peer, STANDING_TOKEN).await,
            }
        }
    })
    .with_kind(PathKind::Relay)
}

/// One standing request, posted again after every pairing and every refusal (the relay refuses a
/// request whose counterpart did not arrive within its pair timeout, 30 s). A request left at the
/// relay by a device that gave up is paired with ours and fails in the acceptor, which also
/// clears it. Ends when the transport is dropped or closed; `revoke` aborts it. The abort closes
/// our end only: letify-link's relay has no call that withdraws a waiting request, so the entry
/// stays there until it is paired or times out (SPEC `FR-N9`, the re-allow limit).
///
/// Failures to post are logged at debug level, with one warning after [`STANDING_WARN_AFTER`] in
/// a row and one info line when a request is posted again.
async fn standing_loop(inner: Weak<Inner>, relay: RelayTarget, peer: NodeId, starts: Arc<Semaphore>) {
    let dialer = match inner.upgrade() {
        Some(i) => RelayDialer::new(relay.addr.clone(), relay.node, i.identity.clone()),
        None => return,
    };
    let peer_id = peer_of(peer).map(|p| p.to_string()).unwrap_or_default();
    let mut streak = Streak::default();
    let posted_again = |streak: &mut Streak| {
        if streak.recovered() {
            tracing::info!(relay = %relay.addr, peer = %peer_id, "the standing request at this server's own relay is posted again");
        }
    };
    loop {
        match inner.upgrade() {
            Some(i) if !i.closed.load(Ordering::SeqCst) => {}
            _ => return,
        }
        let dial = dialer.connect(peer, STANDING_TOKEN);
        tokio::pin!(dial);
        // The handshake's share of the dial runs under a permit; the wait for the device does not.
        let early = {
            let _permit = starts.acquire().await;
            tokio::time::timeout(STANDING_HEAD_START, &mut dial).await.ok()
        };
        let paired = match early {
            Some(done) => done,
            None => match tokio::time::timeout(STANDING_POSTED_AFTER, &mut dial).await {
                Ok(done) => done,
                // Still pending: the request waits at the relay.
                Err(_) => {
                    posted_again(&mut streak);
                    dial.await
                }
            },
        };
        match paired {
            Ok(tcp) => {
                posted_again(&mut streak);
                let Some(inner) = inner.upgrade() else { return };
                tokio::spawn(async move {
                    match inner.acceptor.accept(tcp).await {
                        Ok(accepted) => inner.on_accepted(accepted, Via::Relay),
                        Err(e) => tracing::debug!(error = %e, "a connection paired at our own relay was refused"),
                    }
                });
            }
            // The relay's own refusal because nobody came within its pair timeout: ask again at
            // once.
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && e.to_string().contains("no counterpart") => {
                posted_again(&mut streak);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            // A request of ours for this peer still waits at the relay: the one a revoke left
            // there. It is gone within the relay's pair timeout, so this is not a failure to
            // warn about.
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && e.to_string().contains("already waiting") => {
                tracing::debug!(error = %e, "an earlier standing request still waits at our own relay");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            // Everything else (the relay does not answer, it ends the connection, it holds too
            // many waiting requests) lasts for a while: retry slowly.
            Err(e) => {
                tracing::debug!(error = %e, "waiting at our own relay failed");
                if streak.failed() {
                    tracing::warn!(
                        relay = %relay.addr,
                        peer = %peer_id,
                        error = %e,
                        "the standing request at this server's own relay failed {STANDING_WARN_AFTER} times in a row; the peer cannot reach this server through the relay alone until it is posted again (retrying every second)"
                    );
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

/// A path through the relay: tell the peer which token to meet at, then dial the relay.
fn relay_candidate(identity: Identity, relay: &RelayTarget, signal: Arc<SignalAdapter>, peer: NodeId) -> Candidate {
    let dialer = RelayDialer::new(relay.addr.clone(), relay.node, identity);
    let (relay_node, relay_addr) = (relay.node, relay.addr.clone());
    Candidate::new(format!("relay:{}", relay.addr), move || {
        let dialer = dialer.clone();
        let signal = signal.clone();
        let relay_addr = relay_addr.clone();
        async move {
            let token = new_token();
            // "Meet me at this relay under this token."
            let offer = Message::RelayOffer { token, relay: relay_node, address: relay_addr };
            rendezvous::send_message(&*signal, peer, &offer).await?;
            dialer.connect(peer, token).await
        }
    })
    .with_kind(PathKind::Relay)
}

/// True when every address answers a TCP connect at once with a refusal, so waiting for a
/// session would only wait out the timeout.
async fn refused_by_all(addrs: &[SocketAddr]) -> bool {
    if addrs.is_empty() {
        return false;
    }
    let probes = addrs.iter().map(|a| async move {
        match tokio::time::timeout(Duration::from_millis(1500), TcpStream::connect(a)).await {
            Ok(Err(e)) => e.kind() == io::ErrorKind::ConnectionRefused,
            _ => false,
        }
    });
    futures::future::join_all(probes).await.into_iter().all(|refused| refused)
}

// ----- tasks -----------------------------------------------------------------------------

async fn accept_loop(inner: Arc<Inner>, listener: TcpListener) {
    loop {
        let (tcp, remote) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                tracing::debug!(error = %e, "accepting a letify-link connection failed");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let inner = inner.clone();
        tokio::spawn(async move {
            match inner.acceptor.accept_tcp(tcp).await {
                Ok(accepted) => inner.on_accepted(accepted, Via::Direct),
                Err(e) => tracing::debug!(%remote, error = %e, "incoming letify-link connection refused"),
            }
        });
    }
}

/// Keeps one initiating session's path manager alive and applies commands to it.
async fn manage(mut pm: PathManager, mut cmds: mpsc::UnboundedReceiver<Cmd>) {
    loop {
        tokio::select! {
            ev = pm.next_event() => match ev {
                Some(PathEvent::Added { name }) => tracing::debug!(%name, "letify path added"),
                Some(PathEvent::Lost { name, reason }) => tracing::debug!(%name, %reason, "letify path lost"),
                Some(_) => {}
                None => break,
            },
            cmd = cmds.recv() => match cmd {
                Some(Cmd::Power(state)) => pm.set_power_state(state),
                Some(Cmd::Resume) => pm.invalidate_paths(),
                None => break,
            },
        }
    }
}

/// Serves relay meeting requests from the rendezvous: a `RelayOffer` from a trusted device is
/// answered, the relay is dialed and the session on it is accepted.
async fn signal_loop(inner: Arc<Inner>, signal: Arc<SignalAdapter>, pairing: RelayPairing<SignalAdapter>) {
    loop {
        match rendezvous::recv_message(&*signal).await {
            Ok((from, Message::RelayOffer { token, relay, address })) => {
                let Ok(permit) = inner.meetings.clone().try_acquire_owned() else {
                    tracing::debug!("too many relay meetings in progress; dropping an offer");
                    continue;
                };
                let (inner, pairing) = (inner.clone(), pairing.clone());
                tokio::spawn(async move {
                    match pairing.handle(from, token, relay, &address).await {
                        Ok(Some(accepted)) => inner.on_accepted(accepted, Via::Relay),
                        Ok(None) => tracing::debug!("declined a relay offer"),
                        Err(e) => tracing::debug!(error = %e, "pairing at the relay failed"),
                    }
                    drop(permit);
                });
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {}
            Err(e) => {
                tracing::debug!(error = %e, "reading the rendezvous failed");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

/// Asks the address observer for our public address now and every few minutes.
async fn observe_loop(inner: Arc<Inner>, server: SocketAddr, pin: Option<PeerId>) {
    let bind = if server.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" };
    let socket = match UdpSocket::bind(bind).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "cannot bind a UDP socket to observe our address");
            return;
        }
    };
    let observer = QadObserver::new(server, pin.map(node_of));
    loop {
        match observer.observe_tcp(&socket, inner.listen.port()).await {
            Ok((mapping, reach)) => {
                let reachable = reach == Reachability::Reachable;
                let changed = {
                    let was_reachable = inner.tcp_reachable.swap(reachable, Ordering::Relaxed);
                    let mut seen = inner.observed.lock().unwrap();
                    let changed = *seen != Some(mapping.mapped) || was_reachable != reachable;
                    *seen = Some(mapping.mapped);
                    changed
                };
                if changed {
                    tracing::info!(address = %mapping.mapped, ?reach, "public address observed");
                    inner.republish();
                }
            }
            Err(e) => tracing::debug!(%server, error = %e, "address observation failed"),
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(300)) => {}
            _ = inner.reobserve.notified() => {}
        }
    }
}

// ----- the backend seam ------------------------------------------------------------------

#[async_trait]
impl TransportBackend for LetifyBackend {
    fn peer_id(&self) -> PeerId {
        self.inner.me
    }

    fn services(&self) -> &ServiceTable {
        &self.inner.services
    }

    async fn connect(&self, addr: PeerAddr, service: &str) -> Result<Connection> {
        self.inner.connect(addr, service).await
    }

    fn local_addr(&self) -> PeerAddr {
        self.inner.local_addr()
    }

    fn add_peer_addr(&self, addr: PeerAddr) {
        self.inner.watch(&[addr.peer]);
        self.inner.hints.lock().unwrap().insert(addr.peer, addr);
    }

    async fn wait_online(&self, _timeout: Duration) -> bool {
        !self.inner.closed.load(Ordering::SeqCst)
    }

    fn set_directory_token(&self, token: Option<String>) {
        if let Some(p) = self.inner.pkarr() {
            p.store().set_token(token);
        }
    }

    fn enable_hub(&self, hub: HubDirectory) -> Result<()> {
        if let Some(p) = self.inner.pkarr() {
            // Already running: only the token and the direct-address choice can change.
            p.store().set_token(hub.token);
            p.store().set_watch(hub.watch);
            self.inner.publish_direct.store(hub.publish_direct, Ordering::Relaxed);
            p.republish();
            return Ok(());
        }
        let store: Arc<dyn PkarrStore> = Arc::new(HttpPkarrStore::new(hub.pkarr_url, hub.token)?.with_watch(hub.watch));
        self.inner.start_pkarr(store, hub.publish_direct);
        Ok(())
    }

    async fn set_relays(&self, relays: RelayConfig) -> Result<()> {
        let target = match relays {
            RelayConfig::Custom(urls) => {
                let mut found = None;
                for url in &urls {
                    match parse_relay_url(url) {
                        Ok(t) => {
                            found = Some(t);
                            break;
                        }
                        Err(e) => tracing::warn!("ignoring relay for the letify backend: {e}"),
                    }
                }
                found
            }
            RelayConfig::Default | RelayConfig::Disabled => None,
        };
        *self.inner.relay.lock().unwrap() = target.or_else(|| self.inner.own_relay.clone());
        Ok(())
    }

    fn allow_peer(&self, peer: PeerId) {
        self.inner.allow(peer);
    }

    fn revoke_peer(&self, peer: PeerId) {
        self.inner.revoke(peer);
    }

    fn set_power_state(&self, state: PowerState) {
        *self.inner.power.lock().unwrap() = state;
        for link in self.inner.all_links() {
            link.set_power(link_power(state));
        }
    }

    fn resume_now(&self) {
        for link in self.inner.all_links() {
            link.resume_now();
        }
        self.inner.reobserve.notify_one();
        self.inner.republish();
    }

    async fn close(&self) {
        self.inner.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BackendKind, MemoryPkarrStore, Transport};

    /// `FR-N9`: one warning per streak of failures to post a standing request, after five in a
    /// row, and the recovery is worth a line only after a warning.
    #[test]
    fn fr_n9_a_failure_streak_warns_once_and_reports_its_recovery_once() {
        let mut s = Streak::default();
        for n in 1..STANDING_WARN_AFTER {
            assert!(!s.failed(), "failure {n} is not warned about");
        }
        assert!(s.failed(), "failure {STANDING_WARN_AFTER} is");
        for _ in 0..20 {
            assert!(!s.failed(), "one warning per streak");
        }
        assert!(s.recovered());
        assert!(!s.recovered(), "one recovery line");
        // A short streak is never mentioned, and a new long one warns again.
        assert!(!s.failed());
        assert!(!s.recovered());
        assert!((0..STANDING_WARN_AFTER).map(|_| s.failed()).any(|w| w));
    }

    /// `FR-N9`: what ends a relay-only dial at once, what is tried again, and what waits.
    #[test]
    fn fr_n9_relay_failures_are_told_apart() {
        use io::ErrorKind as K;
        assert_eq!(relay_failure(&io::Error::from(K::ConnectionRefused)), RelayFailure::Unreachable);
        assert_eq!(relay_failure(&io::Error::other("failed to lookup address information")), RelayFailure::Unreachable);
        // The relay closes a peer it does not admit during the handshake, or after the request.
        let handshake = io::Error::from(letify_link::Error::Handshake("the peer closed the transport".into()));
        assert_eq!(relay_failure(&handshake), RelayFailure::Closed);
        let eof = io::Error::from(letify_link::Error::Io(io::Error::from(K::UnexpectedEof)));
        assert_eq!(relay_failure(&eof), RelayFailure::Closed);
        assert_eq!(relay_failure(&io::Error::from(K::UnexpectedEof)), RelayFailure::Closed);
        assert_eq!(relay_failure(&io::Error::from(K::ConnectionReset)), RelayFailure::Closed);
        let refused = io::Error::new(K::PermissionDenied, "the relay refused: too many waiting requests");
        assert_eq!(relay_failure(&refused), RelayFailure::Refused);
        let late = io::Error::new(K::TimedOut, "the relay did not pair in time");
        assert_eq!(relay_failure(&late), RelayFailure::NoAnswer);
    }

    /// `FR-N5`: a peer whose record does not list `letify-link/1` is refused with both lists
    /// named, before any dial.
    #[tokio::test]
    async fn fr_n5_a_peer_that_does_not_speak_letify_link_is_refused_naming_both_lists() {
        let store = MemoryPkarrStore::new();
        let peer_key = SecretKey::generate();
        let identity = Identity::from_secret_bytes(peer_key.to_bytes());
        let mut rec = AddressRecord::new(node_of(peer_key.peer_id()), record::unix_now());
        rec.tcp = vec!["127.0.0.1:9".parse().unwrap()];
        rec.transports = vec!["quic-iroh/1".to_string()];
        let txt = rec.encode_txt(&identity);
        let _peer = PkarrNode::start(
            peer_key.clone(),
            Arc::new(store.clone()),
            Arc::new(move || txt.clone()),
            super::pkarr::PollPlan::new(Duration::from_secs(60), Duration::from_secs(60), Duration::ZERO),
        );
        let letify = LetifyConfig { pkarr: Some(Arc::new(store.clone())), ..LetifyConfig::default() };
        let me = Transport::bind(
            TransportConfig::new(SecretKey::generate())
                .backend(BackendKind::Letify)
                .bind_addr("127.0.0.1:0".parse().unwrap())
                .letify(letify),
        )
        .await
        .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while store.len() < 2 {
            assert!(std::time::Instant::now() < deadline, "records were never published");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        match me.connect(peer_key.peer_id(), "svc").await {
            Err(TransportError::Connect { reason, .. }) => {
                assert!(reason.contains("quic-iroh/1"), "names the peer's list: {reason}");
                assert!(reason.contains(LETIFY_TRANSPORT_ID), "names our list: {reason}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
}
