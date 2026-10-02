//! In-memory fake transport, always available, for tests of crates built on the transport
//! (SPEC `FR-N5`: ember server / node test suites run against it unchanged).
//!
//! ```
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! use ember_transport::mem::MemNetwork;
//! let net = MemNetwork::new();
//! let (a, b) = (net.transport(), net.transport());
//! let mut listener = b.listen("echo").unwrap();
//! let conn = a.connect(b.peer_id(), "echo").await.unwrap();
//! let accepted = listener.accept().await.unwrap();
//! assert_eq!(accepted.peer(), a.peer_id());
//! # drop(conn);
//! # });
//! ```
//!
//! Streams are `tokio::io::duplex` pipes; there is no latency or loss. Path state starts as
//! `Direct { rtt: 0 }` and can be driven with [`MemNetwork::set_path`] to test path-change
//! handling.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, watch};

use crate::{
    BiStream, CloseReason, Connection, ConnectionBackend, PathState, PeerAddr, PeerId, Result,
    SecretKey, ServiceTable, Transport, TransportBackend, TransportError,
};

const PIPE_BUFFER: usize = 256 * 1024;

/// A process-local network of fake transports. Cheap to clone.
#[derive(Clone, Default)]
pub struct MemNetwork {
    inner: Arc<NetInner>,
}

/// Live fake connections between each pair of peers.
type Links = HashMap<(PeerId, PeerId), Vec<Weak<MemConn>>>;

#[derive(Default)]
struct NetInner {
    nodes: Mutex<HashMap<PeerId, Weak<MemNode>>>,
    /// Live connection halves per unordered peer pair, for `set_path`.
    links: Mutex<Links>,
}

fn pair_key(a: PeerId, b: PeerId) -> (PeerId, PeerId) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

impl MemNetwork {
    pub fn new() -> Self {
        Self::default()
    }

    /// A new transport on this network with a random identity.
    pub fn transport(&self) -> Transport {
        self.transport_with_key(SecretKey::generate())
    }

    /// A new transport with the given identity. Replaces any earlier node with the same id.
    pub fn transport_with_key(&self, key: SecretKey) -> Transport {
        let node = Arc::new(MemNode {
            peer: key.peer_id(),
            net: Arc::downgrade(&self.inner),
            services: ServiceTable::default(),
            conns: Mutex::new(Vec::new()),
            closed: watch::channel(false).0,
        });
        self.inner.nodes.lock().unwrap().insert(node.peer, Arc::downgrade(&node));
        Transport::from_backend(node)
    }

    /// Sets the path state reported by every open connection between `a` and `b` (both
    /// sides), firing their path watchers.
    pub fn set_path(&self, a: PeerId, b: PeerId, state: PathState) {
        let mut links = self.inner.links.lock().unwrap();
        if let Some(list) = links.get_mut(&pair_key(a, b)) {
            list.retain(|w| w.strong_count() > 0);
            for conn in list.iter().filter_map(Weak::upgrade) {
                conn.path.send_replace(state.clone());
            }
        }
    }
}

struct MemNode {
    peer: PeerId,
    net: Weak<NetInner>,
    services: ServiceTable,
    conns: Mutex<Vec<Weak<MemConn>>>,
    closed: watch::Sender<bool>,
}

#[async_trait]
impl TransportBackend for MemNode {
    fn peer_id(&self) -> PeerId {
        self.peer
    }

    fn services(&self) -> &ServiceTable {
        &self.services
    }

    async fn connect(&self, addr: PeerAddr, service: &str) -> Result<Connection> {
        let fail = |reason: &str| TransportError::Connect { peer: addr.peer, reason: reason.into() };
        if *self.closed.borrow() {
            return Err(fail("local transport is closed"));
        }
        let net = self.net.upgrade().ok_or_else(|| fail("network dropped"))?;
        let remote = net
            .nodes
            .lock()
            .unwrap()
            .get(&addr.peer)
            .and_then(Weak::upgrade)
            .ok_or_else(|| fail("no such peer on this network"))?;
        if *remote.closed.borrow() {
            return Err(fail("peer transport is closed"));
        }
        let tx = remote
            .services
            .sender(service)
            .ok_or_else(|| fail("peer does not accept this service"))?;

        let (local, far) = MemConn::pair();
        net.links
            .lock()
            .unwrap()
            .entry(pair_key(self.peer, remote.peer))
            .or_default()
            .extend([Arc::downgrade(&local), Arc::downgrade(&far)]);
        self.conns.lock().unwrap().push(Arc::downgrade(&local));
        remote.conns.lock().unwrap().push(Arc::downgrade(&far));

        tx.send(Connection::new(self.peer, service, far))
            .await
            .map_err(|_| fail("peer stopped listening"))?;
        Ok(Connection::new(remote.peer, service, local))
    }

    fn local_addr(&self) -> PeerAddr {
        PeerAddr::new(self.peer)
    }

    fn add_peer_addr(&self, _addr: PeerAddr) {}

    async fn wait_online(&self, _timeout: Duration) -> bool {
        true
    }

    async fn close(&self) {
        self.closed.send_replace(true);
        for conn in self.conns.lock().unwrap().drain(..).filter_map(|w| w.upgrade()) {
            conn.close(0, "transport closed");
        }
    }
}

/// One side of a fake connection.
struct MemConn {
    /// Streams the peer opened towards us.
    incoming: tokio::sync::Mutex<mpsc::UnboundedReceiver<BiStream>>,
    /// Where our `open_bi` delivers the far end.
    outgoing: mpsc::UnboundedSender<BiStream>,
    path: watch::Sender<PathState>,
    /// Shared close state: `Some((closed_by_side_a, code, reason))`.
    close: Arc<watch::Sender<Option<(bool, u32, String)>>>,
    is_a: bool,
}

impl MemConn {
    fn pair() -> (Arc<Self>, Arc<Self>) {
        let (a_tx, b_rx) = mpsc::unbounded_channel();
        let (b_tx, a_rx) = mpsc::unbounded_channel();
        let close = Arc::new(watch::channel(None).0);
        let path = || watch::channel(PathState::Direct { rtt: Duration::ZERO, remote: None }).0;
        let a = Arc::new(Self {
            incoming: tokio::sync::Mutex::new(a_rx),
            outgoing: a_tx,
            path: path(),
            close: close.clone(),
            is_a: true,
        });
        let b = Arc::new(Self {
            incoming: tokio::sync::Mutex::new(b_rx),
            outgoing: b_tx,
            path: path(),
            close,
            is_a: false,
        });
        (a, b)
    }

    fn reason(&self, state: &(bool, u32, String)) -> CloseReason {
        let (by_a, code, reason) = state;
        if *by_a == self.is_a {
            CloseReason::Local
        } else {
            CloseReason::Remote { code: *code, reason: reason.clone() }
        }
    }

    fn closed_reason(&self) -> Option<CloseReason> {
        self.close.borrow().as_ref().map(|s| self.reason(s))
    }
}

impl Drop for MemConn {
    fn drop(&mut self) {
        // Last handle to this side gone: the connection ends, as with QUIC.
        self.close.send_if_modified(|s| {
            if s.is_none() {
                *s = Some((self.is_a, 0, "connection dropped".into()));
                true
            } else {
                false
            }
        });
    }
}

#[async_trait]
impl ConnectionBackend for MemConn {
    async fn open_bi(&self) -> Result<BiStream> {
        if let Some(r) = self.closed_reason() {
            return Err(TransportError::Closed(r));
        }
        let (near, far) = tokio::io::duplex(PIPE_BUFFER);
        let (near_r, near_w) = tokio::io::split(near);
        let (far_r, far_w) = tokio::io::split(far);
        self.outgoing
            .send(BiStream::new(far_w, far_r))
            .map_err(|_| TransportError::Closed(CloseReason::Lost("peer gone".into())))?;
        Ok(BiStream::new(near_w, near_r))
    }

    async fn accept_bi(&self) -> Result<BiStream> {
        let mut close = self.close.subscribe();
        let mut incoming = self.incoming.lock().await;
        loop {
            if let Some(r) = self.closed_reason() {
                return Err(TransportError::Closed(r));
            }
            tokio::select! {
                s = incoming.recv() => {
                    return s.ok_or_else(|| {
                        TransportError::Closed(
                            self.closed_reason().unwrap_or(CloseReason::Lost("peer gone".into())),
                        )
                    });
                }
                _ = close.changed() => {}
            }
        }
    }

    fn path_state(&self) -> PathState {
        if self.closed_reason().is_some() {
            return PathState::Unknown;
        }
        self.path.borrow().clone()
    }

    fn watch_path(&self) -> watch::Receiver<PathState> {
        self.path.subscribe()
    }

    fn close(&self, code: u32, reason: &str) {
        self.close.send_if_modified(|s| {
            if s.is_none() {
                *s = Some((self.is_a, code, reason.to_string()));
                true
            } else {
                false
            }
        });
        self.path.send_replace(PathState::Unknown);
    }

    async fn closed(&self) -> CloseReason {
        let mut rx = self.close.subscribe();
        let state = rx.wait_for(Option::is_some).await;
        match state {
            Ok(s) => self.reason(s.as_ref().expect("waited for Some")),
            Err(_) => CloseReason::Local,
        }
    }
}
