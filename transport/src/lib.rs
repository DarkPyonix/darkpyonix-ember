//! Ember's transport: the one interface through which all Ember code reaches the network
//! (SPEC `FR-N5`).
//!
//! - Peers are addressed by [`PeerId`] (an Ed25519 public key); [`SecretKey`] persists it.
//! - [`Transport::connect`] dials a peer for a named service; [`Transport::listen`] accepts
//!   connections for one.
//! - A [`Connection`] carries any number of bidirectional streams ([`BiStream`], plain tokio
//!   `AsyncRead + AsyncWrite`) and reports its [`PathState`] (direct or relayed).
//! - [`http`] serves an axum router over accepted streams and dials HTTP over them.
//! - [`PeerGate`] admits only allowed peers and closes a revoked peer's connections (`FR-N3`);
//!   [`Dialer`] caches one outgoing connection per (peer, service) for many streams.
//!
//! Backends: `iroh` (feature `iroh`, default; [`Transport::bind`]) and an always-available
//! in-memory fake ([`mem::MemNetwork`]) for other crates' tests. **No backend type appears in
//! this crate's public API**, so replacing iroh touches only this crate.

mod addr;
mod config;
mod dialer;
pub mod gate;
mod key;
pub mod mem;

#[cfg(feature = "http")]
pub mod http;
#[cfg(feature = "iroh")]
mod iroh_backend;

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{mpsc, watch};

pub use addr::{AddressDirectory, MemoryDirectory, PeerAddr};
pub use config::{HubDirectory, RelayConfig, TransportConfig, RELAY_URL_ENV};
pub use dialer::Dialer;
pub use gate::PeerGate;
pub use key::{PeerId, SecretKey};

/// Errors from the transport. Backend errors are carried as text so no backend type leaks.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("invalid key: {0}")]
    InvalidKey(String),
    #[error("invalid configuration: {0}")]
    Config(String),
    #[error("failed to bind transport: {0}")]
    Bind(String),
    #[error("cannot connect to {peer:?}: {reason}")]
    Connect { peer: PeerId, reason: String },
    #[error("service {0:?} is already being listened on")]
    AlreadyListening(String),
    #[error("connection closed: {0}")]
    Closed(CloseReason),
    #[error("stream error: {0}")]
    Stream(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub type Result<T, E = TransportError> = std::result::Result<T, E>;

/// Which network path a connection is currently using.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathState {
    /// A direct (hole-punched or LAN) UDP path.
    Direct { rtt: Duration, remote: Option<SocketAddr> },
    /// Traffic goes through a relay server.
    Relayed { relay: String, rtt: Duration },
    /// No path selected yet, or the connection is closed.
    Unknown,
}

impl PathState {
    pub fn is_direct(&self) -> bool {
        matches!(self, Self::Direct { .. })
    }

    pub fn is_relayed(&self) -> bool {
        matches!(self, Self::Relayed { .. })
    }

    /// Same route (kind and address), ignoring RTT. Path-change notifications fire only when
    /// this changes, not on every RTT sample.
    pub fn same_route(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Direct { remote: a, .. }, Self::Direct { remote: b, .. }) => a == b,
            (Self::Relayed { relay: a, .. }, Self::Relayed { relay: b, .. }) => a == b,
            (Self::Unknown, Self::Unknown) => true,
            _ => false,
        }
    }
}

impl fmt::Display for PathState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Direct { rtt, remote: Some(a) } => write!(f, "direct {a} rtt={rtt:?}"),
            Self::Direct { rtt, remote: None } => write!(f, "direct rtt={rtt:?}"),
            Self::Relayed { relay, rtt } => write!(f, "relayed via {relay} rtt={rtt:?}"),
            Self::Unknown => f.write_str("unknown"),
        }
    }
}

/// Why a connection ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloseReason {
    /// We closed it (or the transport shut down).
    Local,
    /// The peer closed it with this application code and reason.
    Remote { code: u32, reason: String },
    /// The connection was lost (timeout, reset, protocol error).
    Lost(String),
}

impl fmt::Display for CloseReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => f.write_str("closed locally"),
            Self::Remote { code, reason } => write!(f, "closed by peer (code {code}: {reason})"),
            Self::Lost(why) => write!(f, "lost: {why}"),
        }
    }
}

/// The write half of a stream. `shutdown()` finishes the stream (the peer reads EOF).
pub type SendHalf = Pin<Box<dyn AsyncWrite + Send + Sync>>;
/// The read half of a stream.
pub type RecvHalf = Pin<Box<dyn AsyncRead + Send + Sync>>;

/// A bidirectional byte stream inside a [`Connection`]. Implements tokio `AsyncRead` and
/// `AsyncWrite`, so hyper, axum, tungstenite, `tokio::io::copy` etc. work on it directly.
pub struct BiStream {
    send: SendHalf,
    recv: RecvHalf,
}

impl BiStream {
    pub fn new(
        send: impl AsyncWrite + Send + Sync + 'static,
        recv: impl AsyncRead + Send + Sync + 'static,
    ) -> Self {
        Self { send: Box::pin(send), recv: Box::pin(recv) }
    }

    pub fn into_split(self) -> (SendHalf, RecvHalf) {
        (self.send, self.recv)
    }
}

impl fmt::Debug for BiStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BiStream")
    }
}

impl AsyncRead for BiStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.recv.as_mut().poll_read(cx, buf)
    }
}

impl AsyncWrite for BiStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.send.as_mut().poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.send.as_mut().poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.send.as_mut().poll_shutdown(cx)
    }
}

// ---------------------------------------------------------------------------------------------
// Backend seam. Backends live in this crate; replacing iroh means adding another impl here.
// ---------------------------------------------------------------------------------------------

#[async_trait]
pub(crate) trait TransportBackend: Send + Sync + 'static {
    fn peer_id(&self) -> PeerId;
    fn services(&self) -> &ServiceTable;
    /// Called after the service table changed (iroh: update the accepted ALPNs).
    fn services_changed(&self) {}
    async fn connect(&self, addr: PeerAddr, service: &str) -> Result<Connection>;
    fn local_addr(&self) -> PeerAddr;
    fn add_peer_addr(&self, addr: PeerAddr);
    async fn wait_online(&self, timeout: Duration) -> bool;
    /// Sets or clears the hub token used to resolve peers through the hub directory. Backends
    /// without a hub directory ignore it.
    fn set_directory_token(&self, _token: Option<String>) {}
    /// Starts publishing to and resolving through the hub directory (no-op where unsupported).
    fn enable_hub(&self, _hub: HubDirectory) -> Result<()> {
        Ok(())
    }
    /// Replaces the relay servers at runtime (no-op where unsupported).
    async fn set_relays(&self, _relays: RelayConfig) -> Result<()> {
        Ok(())
    }
    async fn close(&self);
}

#[async_trait]
pub(crate) trait ConnectionBackend: Send + Sync + 'static {
    async fn open_bi(&self) -> Result<BiStream>;
    async fn accept_bi(&self) -> Result<BiStream>;
    fn path_state(&self) -> PathState;
    fn watch_path(&self) -> watch::Receiver<PathState>;
    fn close(&self, code: u32, reason: &str);
    async fn closed(&self) -> CloseReason;
}

/// Service name → channel of accepted connections.
#[derive(Default)]
pub(crate) struct ServiceTable {
    inner: Mutex<HashMap<String, (u64, mpsc::Sender<Connection>)>>,
    next_id: std::sync::atomic::AtomicU64,
}

impl ServiceTable {
    fn register(&self, service: &str) -> Result<(u64, mpsc::Receiver<Connection>)> {
        let mut map = self.inner.lock().unwrap();
        if let Some((_, tx)) = map.get(service) {
            if !tx.is_closed() {
                return Err(TransportError::AlreadyListening(service.to_string()));
            }
        }
        let id = self.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(64);
        map.insert(service.to_string(), (id, tx));
        Ok((id, rx))
    }

    fn unregister(&self, service: &str, id: u64) {
        let mut map = self.inner.lock().unwrap();
        if map.get(service).is_some_and(|(i, _)| *i == id) {
            map.remove(service);
        }
    }

    pub(crate) fn names(&self) -> Vec<String> {
        self.inner.lock().unwrap().keys().cloned().collect()
    }

    pub(crate) fn sender(&self, service: &str) -> Option<mpsc::Sender<Connection>> {
        self.inner.lock().unwrap().get(service).map(|(_, tx)| tx.clone())
    }

    pub(crate) fn clear(&self) {
        self.inner.lock().unwrap().clear();
    }
}

// ---------------------------------------------------------------------------------------------
// Public handles
// ---------------------------------------------------------------------------------------------

/// A bound transport endpoint. Cheap to clone; all clones share one endpoint.
#[derive(Clone)]
pub struct Transport {
    inner: Arc<dyn TransportBackend>,
}

impl fmt::Debug for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Transport({})", self.peer_id().fmt_short())
    }
}

impl Transport {
    pub(crate) fn from_backend(inner: Arc<dyn TransportBackend>) -> Self {
        Self { inner }
    }

    /// Binds the real peer-to-peer transport (iroh backend).
    #[cfg(feature = "iroh")]
    pub async fn bind(config: TransportConfig) -> Result<Self> {
        let backend = iroh_backend::IrohBackend::bind(config).await?;
        Ok(Self::from_backend(backend))
    }

    /// This endpoint's identity.
    pub fn peer_id(&self) -> PeerId {
        self.inner.peer_id()
    }

    /// Dials `peer` for `service`. With a bare [`PeerId`] the address comes from the
    /// configured directory / earlier [`Transport::add_peer_addr`] hints.
    pub async fn connect(&self, peer: impl Into<PeerAddr>, service: &str) -> Result<Connection> {
        self.inner.connect(peer.into(), service).await
    }

    /// Starts accepting connections for `service`. Dropping the [`Listener`] stops accepting.
    pub fn listen(&self, service: &str) -> Result<Listener> {
        let (id, rx) = self.inner.services().register(service)?;
        self.inner.services_changed();
        Ok(Listener { rx, service: service.to_string(), id, transport: self.inner.clone() })
    }

    /// Our current reachable address (identity, relay, direct addresses) to share out of band.
    pub fn local_addr(&self) -> PeerAddr {
        self.inner.local_addr()
    }

    /// Remembers an address hint for a peer (used by later `connect(peer_id, ..)` calls).
    pub fn add_peer_addr(&self, addr: PeerAddr) {
        self.inner.add_peer_addr(addr)
    }

    /// Sets (after hub registration) or clears (on revocation) the device token with which peers
    /// are resolved through the hub directory ([`TransportConfig::hub`]). No effect without one.
    pub fn set_directory_token(&self, token: Option<String>) {
        self.inner.set_directory_token(token)
    }

    /// Starts using the hub's address directory on a running transport (publish our record,
    /// resolve peers with `hub.token`), e.g. right after the device registered with the hub.
    /// Calling it again only updates the token. The in-memory backend ignores it.
    pub fn enable_hub(&self, hub: HubDirectory) -> Result<()> {
        self.inner.enable_hub(hub)
    }

    /// Replaces the relay servers on a running transport (e.g. switch to the hub's relay after
    /// registration). The in-memory backend ignores it.
    pub async fn set_relays(&self, relays: RelayConfig) -> Result<()> {
        self.inner.set_relays(relays).await
    }

    /// Waits until the endpoint is connected to its home relay. Returns `false` on timeout or
    /// when no relay is configured.
    pub async fn wait_online(&self, timeout: Duration) -> bool {
        self.inner.wait_online(timeout).await
    }

    /// Gracefully closes the endpoint and every connection on it.
    pub async fn close(&self) {
        self.inner.services().clear();
        self.inner.close().await
    }
}

/// Accepts connections for one service.
pub struct Listener {
    rx: mpsc::Receiver<Connection>,
    service: String,
    id: u64,
    transport: Arc<dyn TransportBackend>,
}

impl Listener {
    /// The next authenticated incoming connection, or `None` once the transport is closed.
    pub async fn accept(&mut self) -> Option<Connection> {
        self.rx.recv().await
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    pub fn local_peer(&self) -> PeerId {
        self.transport.peer_id()
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.transport.services().unregister(&self.service, self.id);
        self.transport.services_changed();
    }
}

impl fmt::Debug for Listener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Listener({:?})", self.service)
    }
}

/// An authenticated connection to one peer. Cheap to clone. The connection closes when
/// [`Connection::close`] is called or the last clone is dropped.
#[derive(Clone)]
pub struct Connection {
    peer: PeerId,
    service: Arc<str>,
    inner: Arc<dyn ConnectionBackend>,
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Connection({} {:?})", self.peer.fmt_short(), self.service)
    }
}

impl Connection {
    pub(crate) fn new(peer: PeerId, service: &str, inner: Arc<dyn ConnectionBackend>) -> Self {
        Self { peer, service: service.into(), inner }
    }

    /// The authenticated identity of the other side.
    pub fn peer(&self) -> PeerId {
        self.peer
    }

    /// The service this connection was opened for.
    pub fn service(&self) -> &str {
        &self.service
    }

    /// Opens a new bidirectional stream. With the iroh backend the peer only learns of the
    /// stream once the opener writes to it, so protocols should have the opener speak first.
    pub async fn open_bi(&self) -> Result<BiStream> {
        self.inner.open_bi().await
    }

    /// Waits for the peer to open a stream. Fails with [`TransportError::Closed`] once the
    /// connection ends.
    pub async fn accept_bi(&self) -> Result<BiStream> {
        self.inner.accept_bi().await
    }

    /// The path in use right now, with a fresh RTT sample.
    pub fn path_state(&self) -> PathState {
        self.inner.path_state()
    }

    /// Notifies on route changes (direct ↔ relayed, address change). The value's RTT is the
    /// one sampled at the change; call [`Connection::path_state`] for a fresh one.
    pub fn watch_path(&self) -> watch::Receiver<PathState> {
        self.inner.watch_path()
    }

    /// Closes the connection immediately with an application code and reason. Data not yet
    /// delivered may be dropped: finish streams and let the peer confirm before closing.
    pub fn close(&self, code: u32, reason: &str) {
        self.inner.close(code, reason)
    }

    /// Resolves when the connection has ended.
    pub async fn closed(&self) -> CloseReason {
        self.inner.closed().await
    }
}
