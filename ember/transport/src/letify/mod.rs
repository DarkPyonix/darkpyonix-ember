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
mod relay_url;

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use letify_link::observe::{AddressObserver, QadObserver, QadResponder};
use letify_link::record::{self, AddressRecord};
use letify_link::relay::{new_token, Relay, RelayConfig as LinkRelayConfig, RelayDialer, RelayHandle};
use letify_link::rendezvous::{self, Message, Proto, Punch, Rendezvous};
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

struct Inner {
    me: PeerId,
    secret: SecretKey,
    identity: Identity,
    cfg: LetifyConfig,
    link_cfg: Config,
    services: Arc<ServiceTable>,
    acceptor: Arc<Acceptor>,
    listen: SocketAddr,
    directory: Option<Arc<dyn AddressDirectory>>,
    links: Mutex<HashMap<PeerId, Vec<Arc<PeerLink>>>>,
    dialing: Mutex<HashMap<PeerId, Arc<tokio::sync::Mutex<()>>>>,
    hints: Mutex<HashMap<PeerId, PeerAddr>>,
    /// The relay to use: configured, or this endpoint's own relay role.
    relay: Mutex<Option<RelayTarget>>,
    own_relay: Option<RelayTarget>,
    relay_handle: Option<RelayHandle>,
    signal: Mutex<Option<Arc<SignalAdapter>>>,
    pkarr: Mutex<Option<Arc<PkarrNode>>>,
    publish_direct: AtomicBool,
    observed: Mutex<Option<SocketAddr>>,
    reobserve: Notify,
    version: Mutex<u32>,
    power: Mutex<PowerState>,
    meetings: Arc<Semaphore>,
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
        let acceptor = Arc::new(Acceptor::new(identity.clone(), PeerPolicy::from_fn(|_| true), link_cfg.clone()));

        let mut bg: Vec<JoinHandle<()>> = Vec::new();

        // The relay role (the main server).
        let mut relay_handle = None;
        let mut own_relay = None;
        if let Some(addr) = cfg.relay_listen {
            let policy = if cfg.relay_open {
                PeerPolicy::from_fn(|_| true)
            } else {
                PeerPolicy::allow(vec![identity.node_id()])
            };
            let relay = Relay::bind(&addr.to_string(), identity.clone(), policy, LinkRelayConfig::default())
                .await
                .map_err(|e| TransportError::Bind(format!("relay {addr}: {e}")))?;
            let bound = relay.local_addr();
            relay.allow(identity.node_id());
            relay_handle = Some(relay.handle());
            let local = SocketAddr::from(([127, 0, 0, 1], bound.port()));
            own_relay = Some(RelayTarget {
                addr: local.to_string(),
                node: identity.node_id(),
                url: format!("letify://{me}@{local}"),
            });
            bg.push(tokio::spawn(relay.run()));
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
            listen,
            directory: config.directory.clone(),
            links: Mutex::new(HashMap::new()),
            dialing: Mutex::new(HashMap::new()),
            hints: Mutex::new(HashMap::new()),
            relay: Mutex::new(relay),
            own_relay,
            relay_handle,
            signal: Mutex::new(None),
            pkarr: Mutex::new(None),
            publish_direct: AtomicBool::new(true),
            observed: Mutex::new(None),
            reobserve: Notify::new(),
            version: Mutex::new(record::unix_now()),
            power: Mutex::new(PowerState::Foreground),
            meetings: Arc::new(Semaphore::new(MAX_MEETINGS)),
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
        if let Some(seen) = *self.observed.lock().unwrap() {
            // The observed public IP with our listening port: right for a host with a public
            // address or a forwarded port (the main server); a NAT'd host just fails to answer.
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
        self.bg.lock().unwrap().push(tokio::spawn(signal_loop(self.clone(), adapter)));
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
                let node = PkarrNode::start(self.secret.clone(), store, record, self.cfg.poll_interval);
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
                let peer = match peer_of(session.peer()) {
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
                self.add_link(link);
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
        link.add_task(tokio::spawn(manage(pm, rx)).abort_handle());
        if let Some(s) = for_demux {
            let demux = tokio::spawn(run_demux(link.clone(), s));
            link.set_demux(demux.abort_handle());
        }
        if let Some(s) = for_watch {
            link.add_task(tokio::spawn(run_path_watch(link.clone(), s)).abort_handle());
        }
        self.add_link(link.clone());
        link
    }

    async fn link_to(self: &Arc<Self>, addr: &PeerAddr) -> Result<Arc<PeerLink>> {
        if let Some(l) = self.live_link(addr.peer) {
            return Ok(l);
        }
        let gate = self.dial_lock(addr.peer);
        let _guard = gate.lock().await;
        if let Some(l) = self.live_link(addr.peer) {
            return Ok(l);
        }
        self.dial(addr).await
    }

    /// Establishes a session with `addr.peer` over every path we know.
    async fn dial(self: &Arc<Self>, addr: &PeerAddr) -> Result<Arc<PeerLink>> {
        let peer = addr.peer;
        let fail = |reason: String| TransportError::Connect { peer, reason };
        let node = NodeId::try_from_bytes(*peer.as_bytes()).map_err(|e| fail(e.to_string()))?;
        self.watch(&[peer]);

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
        let relayed = match (&relay, &signal) {
            (Some(relay), Some(signal)) => {
                candidates.push(relay_candidate(self.identity.clone(), relay, signal.clone(), node));
                true
            }
            _ => false,
        };
        if candidates.is_empty() {
            return Err(fail(if relay.is_some() {
                "no address is known for the peer and there is no rendezvous to reach it through the relay".into()
            } else {
                "no address is known for the peer and no relay is registered".into()
            }));
        }
        if !relayed && refused_by_all(&tcp).await {
            return Err(fail("every known address refused the connection".into()));
        }

        let manager_cfg = ManagerConfig {
            dial_timeout: self.cfg.dial_timeout,
            start_timeout: self.cfg.connect_timeout,
            ..ManagerConfig::default()
        };
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
        let links = self.all_links();
        futures::future::join_all(links.iter().map(|l| l.close_local(None))).await;
        *self.pkarr.lock().unwrap() = None;
    }

    fn revoke(&self, peer: PeerId) {
        let node = node_of(peer);
        // Accepted sessions: refuse new handshakes and close live sessions with "revoked".
        self.acceptor.revoke(node);
        if let Some(h) = &self.relay_handle {
            h.revoke(node);
        }
        // Sessions we started to that peer (or to an alias of its key): close, and do not redial.
        let class = node.canonical();
        let Ok(handle) = tokio::runtime::Handle::try_current() else { return };
        for link in self.all_links() {
            if node_of(link.peer).canonical() == class {
                handle.spawn(async move { link.close_local(Some("revoked")).await });
            }
        }
    }

    fn allow(&self, peer: PeerId) {
        let node = node_of(peer);
        self.acceptor.reinstate(node);
        if let Some(h) = &self.relay_handle {
            h.allow(node);
        }
        self.watch(&[peer]);
    }
}

/// A path through the relay: tell the peer which token to meet at, then dial the relay.
fn relay_candidate(identity: Identity, relay: &RelayTarget, signal: Arc<SignalAdapter>, peer: NodeId) -> Candidate {
    let dialer = RelayDialer::new(relay.addr.clone(), relay.node, identity);
    Candidate::new(format!("relay:{}", relay.addr), move || {
        let dialer = dialer.clone();
        let signal = signal.clone();
        async move {
            let token = new_token();
            // A TCP punch offer with no mappings is Ember's relay meeting request: "pair with
            // me at the relay under this token".
            let offer = Message::PunchOffer(Punch {
                proto: Proto::Tcp,
                token,
                start_at_ms: 0,
                until_ms: 0,
                mappings: Vec::new(),
            });
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

/// Serves relay meeting requests from the rendezvous.
async fn signal_loop(inner: Arc<Inner>, signal: Arc<SignalAdapter>) {
    loop {
        match rendezvous::recv_message(&*signal).await {
            Ok((from, Message::PunchOffer(p))) if p.proto == Proto::Tcp && p.mappings.is_empty() => {
                let Ok(permit) = inner.meetings.clone().try_acquire_owned() else {
                    tracing::debug!("too many relay meetings in progress; dropping an offer");
                    continue;
                };
                let inner = inner.clone();
                tokio::spawn(async move {
                    meet(inner, from, p.token).await;
                    drop(permit);
                });
            }
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(error = %e, "reading the rendezvous failed");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

/// Dials the relay to be paired with `from` under `token`, then accepts the session on it.
async fn meet(inner: Arc<Inner>, from: NodeId, token: [u8; 16]) {
    let Some(relay) = inner.relay_target() else {
        tracing::debug!("a relay meeting was requested but no relay is registered");
        return;
    };
    let dialer = RelayDialer::new(relay.addr, relay.node, inner.identity.clone()).pair_wait(Duration::from_secs(20));
    match dialer.connect(from, token).await {
        Ok(tcp) => match inner.acceptor.accept(tcp).await {
            Ok(accepted) => inner.on_accepted(accepted, Via::Relay),
            Err(e) => tracing::debug!(error = %e, "session over the relay refused"),
        },
        Err(e) => tracing::debug!(error = %e, "pairing at the relay failed"),
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
        match observer.observe(&socket).await {
            Ok(mapping) => {
                let changed = {
                    let mut seen = inner.observed.lock().unwrap();
                    let changed = *seen != Some(mapping.mapped);
                    *seen = Some(mapping.mapped);
                    changed
                };
                if changed {
                    tracing::info!(address = %mapping.mapped, "public address observed");
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
            self.inner.publish_direct.store(hub.publish_direct, Ordering::Relaxed);
            p.republish();
            return Ok(());
        }
        let store: Arc<dyn PkarrStore> = Arc::new(HttpPkarrStore::new(hub.pkarr_url, hub.token)?);
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
            Duration::from_secs(60),
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
