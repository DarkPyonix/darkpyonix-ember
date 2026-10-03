//! iroh 1.x backend (SPEC `FR-N1`). The only module in Ember that names iroh (`FR-N5`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use iroh::address_lookup::{self, AddressLookup, EndpointData, EndpointInfo, MemoryLookup};
use iroh::endpoint::{presets, ConnectionError, PathEvent, VarInt, WeakConnectionHandle};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl, TransportAddr};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::{
    AddressDirectory, BiStream, CloseReason, Connection, ConnectionBackend, PathState, PeerAddr,
    PeerId, RelayConfig, Result, ServiceTable, TransportBackend, TransportConfig, TransportError,
};

pub(crate) struct IrohBackend {
    endpoint: Endpoint,
    peer: PeerId,
    services: Arc<ServiceTable>,
    hints: MemoryLookup,
    relays_enabled: bool,
    accept_task: Mutex<Option<JoinHandle<()>>>,
}

impl IrohBackend {
    pub(crate) async fn bind(config: TransportConfig) -> Result<Arc<Self>> {
        let peer = config.secret_key.peer_id();
        let mut builder = Endpoint::builder(presets::Minimal)
            .secret_key(iroh::SecretKey::from_bytes(&config.secret_key.to_bytes()));
        if config.public_lookup {
            builder = builder.preset(presets::N0);
        }
        let relay_mode = match &config.relay {
            RelayConfig::Default => iroh::endpoint::default_relay_mode(),
            RelayConfig::Disabled => RelayMode::Disabled,
            RelayConfig::Custom(urls) => {
                let urls = urls
                    .iter()
                    .map(|u| {
                        u.parse::<RelayUrl>()
                            .map_err(|e| TransportError::Config(format!("relay url {u:?}: {e}")))
                    })
                    .collect::<Result<Vec<_>>>()?;
                RelayMode::custom(urls)
            }
        };
        let relays_enabled = !matches!(relay_mode, RelayMode::Disabled);
        builder = builder.relay_mode(relay_mode);

        let hints = MemoryLookup::new();
        builder = builder.address_lookup(hints.clone());
        if let Some(directory) = config.directory.clone() {
            builder = builder.address_lookup(DirectoryLookup { peer, directory });
        }
        if let Some(addr) = config.bind_addr {
            builder = builder
                .bind_addr(addr)
                .map_err(|e| TransportError::Config(format!("bind address {addr}: {e}")))?;
        }

        let endpoint = builder.bind().await.map_err(|e| TransportError::Bind(e.to_string()))?;
        let services = Arc::new(ServiceTable::default());
        let backend = Arc::new(Self {
            endpoint: endpoint.clone(),
            peer,
            services: services.clone(),
            hints,
            relays_enabled,
            accept_task: Mutex::new(None),
        });
        let task = tokio::spawn(accept_loop(endpoint, services));
        *backend.accept_task.lock().unwrap() = Some(task);
        Ok(backend)
    }
}

async fn accept_loop(endpoint: Endpoint, services: Arc<ServiceTable>) {
    while let Some(incoming) = endpoint.accept().await {
        let services = services.clone();
        tokio::spawn(async move {
            let conn = match incoming.await {
                Ok(conn) => conn,
                Err(e) => {
                    tracing::debug!(error = %e, "incoming connection failed handshake");
                    return;
                }
            };
            let service = String::from_utf8_lossy(conn.alpn()).into_owned();
            let peer = match to_peer_id(conn.remote_id()) {
                Ok(p) => p,
                Err(_) => return,
            };
            let Some(tx) = services.sender(&service) else {
                conn.close(VarInt::from_u32(1), b"service not available");
                return;
            };
            let wrapped = Connection::new(peer, &service, IrohConnection::new(conn));
            if tx.send(wrapped).await.is_err() {
                tracing::debug!(%service, "listener dropped while accepting");
            }
        });
    }
}

#[async_trait]
impl TransportBackend for IrohBackend {
    fn peer_id(&self) -> PeerId {
        self.peer
    }

    fn services(&self) -> &ServiceTable {
        &self.services
    }

    fn services_changed(&self) {
        let alpns = self.services.names().into_iter().map(String::into_bytes).collect();
        self.endpoint.set_alpns(alpns);
    }

    async fn connect(&self, addr: PeerAddr, service: &str) -> Result<Connection> {
        let peer = addr.peer;
        let fail = |reason: String| TransportError::Connect { peer, reason };
        let target = to_endpoint_addr(&addr).map_err(|e| fail(e.to_string()))?;
        let conn = self
            .endpoint
            .connect(target, service.as_bytes())
            .await
            .map_err(|e| fail(format!("{e:#}")))?;
        Ok(Connection::new(peer, service, IrohConnection::new(conn)))
    }

    fn local_addr(&self) -> PeerAddr {
        from_endpoint_addr(self.peer, &self.endpoint.addr())
    }

    fn add_peer_addr(&self, addr: PeerAddr) {
        match to_endpoint_addr(&addr) {
            Ok(a) => self.hints.add_endpoint_info(a),
            Err(e) => tracing::warn!(error = %e, "ignoring invalid peer address hint"),
        }
    }

    async fn wait_online(&self, timeout: Duration) -> bool {
        if !self.relays_enabled {
            return false;
        }
        tokio::time::timeout(timeout, self.endpoint.online()).await.is_ok()
    }

    async fn close(&self) {
        self.endpoint.close().await;
        if let Some(task) = self.accept_task.lock().unwrap().take() {
            task.abort();
        }
    }
}

impl Drop for IrohBackend {
    fn drop(&mut self) {
        if let Some(task) = self.accept_task.lock().unwrap().take() {
            task.abort();
        }
    }
}

struct IrohConnection {
    conn: iroh::endpoint::Connection,
    path_tx: watch::Sender<PathState>,
    monitor: JoinHandle<()>,
}

impl IrohConnection {
    fn new(conn: iroh::endpoint::Connection) -> Arc<Self> {
        let (path_tx, _) = watch::channel(current_path(&conn));
        let monitor = tokio::spawn(monitor_paths(
            conn.weak_handle(),
            conn.path_events(),
            path_tx.clone(),
        ));
        Arc::new(Self { conn, path_tx, monitor })
    }
}

impl Drop for IrohConnection {
    fn drop(&mut self) {
        self.monitor.abort();
    }
}

/// Follows path events and publishes route changes. Holds only a weak handle so it never
/// keeps the connection alive.
async fn monitor_paths(
    weak: WeakConnectionHandle,
    mut events: iroh::endpoint::PathEventStream,
    tx: watch::Sender<PathState>,
) {
    while let Some(event) = events.next().await {
        if let PathEvent::Lagged { missed, .. } = event {
            tracing::debug!(missed, "path events lagged; re-reading current path");
        }
        let Some(conn) = weak.upgrade() else { break };
        let state = current_path(&conn);
        drop(conn);
        tx.send_if_modified(|old| {
            if old.same_route(&state) {
                false
            } else {
                tracing::debug!(from = %old, to = %state, "path changed");
                *old = state;
                true
            }
        });
    }
    tx.send_replace(PathState::Unknown);
}

fn current_path(conn: &iroh::endpoint::Connection) -> PathState {
    let paths = conn.paths();
    let Some(path) = paths.iter().find(|p| p.is_selected()) else {
        return PathState::Unknown;
    };
    let rtt = path.rtt();
    match path.remote_addr() {
        TransportAddr::Ip(addr) => PathState::Direct { rtt, remote: Some(*addr) },
        TransportAddr::Relay(url) => PathState::Relayed { relay: url.to_string(), rtt },
        _ => PathState::Unknown,
    }
}

fn close_reason(err: &ConnectionError) -> CloseReason {
    match err {
        ConnectionError::LocallyClosed => CloseReason::Local,
        ConnectionError::ApplicationClosed(close) => CloseReason::Remote {
            code: u32::try_from(close.error_code.into_inner()).unwrap_or(u32::MAX),
            reason: String::from_utf8_lossy(&close.reason).into_owned(),
        },
        other => CloseReason::Lost(other.to_string()),
    }
}

#[async_trait]
impl ConnectionBackend for IrohConnection {
    async fn open_bi(&self) -> Result<BiStream> {
        let (send, recv) = self
            .conn
            .open_bi()
            .await
            .map_err(|e| TransportError::Closed(close_reason(&e)))?;
        Ok(BiStream::new(send, recv))
    }

    async fn accept_bi(&self) -> Result<BiStream> {
        let (send, recv) = self
            .conn
            .accept_bi()
            .await
            .map_err(|e| TransportError::Closed(close_reason(&e)))?;
        Ok(BiStream::new(send, recv))
    }

    fn path_state(&self) -> PathState {
        if self.conn.close_reason().is_some() {
            return PathState::Unknown;
        }
        current_path(&self.conn)
    }

    fn watch_path(&self) -> watch::Receiver<PathState> {
        self.path_tx.subscribe()
    }

    fn close(&self, code: u32, reason: &str) {
        self.conn.close(VarInt::from_u32(code), reason.as_bytes());
    }

    async fn closed(&self) -> CloseReason {
        close_reason(&self.conn.closed().await)
    }
}

// ---------------------------------------------------------------------------------------------
// Conversions between our address types and iroh's.
// ---------------------------------------------------------------------------------------------

fn to_peer_id(id: EndpointId) -> Result<PeerId> {
    PeerId::from_bytes(*id.as_bytes())
}

fn to_endpoint_addr(addr: &PeerAddr) -> Result<EndpointAddr> {
    let id = EndpointId::from_bytes(addr.peer.as_bytes())
        .map_err(|e| TransportError::InvalidKey(e.to_string()))?;
    let mut out = EndpointAddr::new(id);
    for relay in &addr.relays {
        let url = relay
            .parse::<RelayUrl>()
            .map_err(|e| TransportError::Config(format!("relay url {relay:?}: {e}")))?;
        out = out.with_relay_url(url);
    }
    for ip in &addr.direct {
        out = out.with_ip_addr(*ip);
    }
    Ok(out)
}

fn from_endpoint_addr(peer: PeerId, addr: &EndpointAddr) -> PeerAddr {
    PeerAddr {
        peer,
        relays: addr.relay_urls().map(|u| u.to_string()).collect(),
        direct: addr.ip_addrs().copied().collect(),
    }
}

/// Bridges our [`AddressDirectory`] into iroh's address lookup.
#[derive(Debug)]
struct DirectoryLookup {
    peer: PeerId,
    directory: Arc<dyn AddressDirectory>,
}

impl AddressLookup for DirectoryLookup {
    fn publish(&self, data: &EndpointData) {
        let addr = PeerAddr {
            peer: self.peer,
            relays: data.relay_urls().map(|u| u.to_string()).collect(),
            direct: data.ip_addrs().copied().collect(),
        };
        self.directory.publish(&addr);
    }

    fn resolve(
        &self,
        endpoint_id: EndpointId,
    ) -> Option<futures::stream::BoxStream<'static, Result<address_lookup::Item, address_lookup::Error>>>
    {
        let peer = to_peer_id(endpoint_id).ok()?;
        let directory = self.directory.clone();
        let stream = futures::stream::once(async move { directory.resolve(peer).await })
            .filter_map(move |found| async move {
                let found = found?;
                let addr = to_endpoint_addr(&found).ok()?;
                let info = EndpointInfo::from_parts(
                    endpoint_id,
                    EndpointData::new(addr.addrs.into_iter().collect()),
                );
                Some(Ok(address_lookup::Item::new(info, "ember-directory", None)))
            });
        Some(Box::pin(stream))
    }
}
