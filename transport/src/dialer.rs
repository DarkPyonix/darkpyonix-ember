//! A connection cache for the dialing side: one [`Connection`] per (peer, service), reused for
//! every stream and re-dialed after it closes.
//!
//! Ember's HTTP-over-transport mapping opens a fresh stream per HTTP connection
//! ([`crate::http`]), so clients want one long-lived transport connection per peer and many
//! cheap streams on it. [`Dialer`] is that: [`Dialer::open_bi`] returns a new stream on the
//! cached connection, dialing (or re-dialing) as needed.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::{BiStream, Connection, PeerAddr, PeerId, Result, Transport};

type Key = (PeerId, String);

/// Cached outgoing connections over one [`Transport`]. Cheap to clone; clones share the cache.
#[derive(Clone)]
pub struct Dialer {
    transport: Transport,
    inner: Arc<DialerInner>,
}

#[derive(Default)]
struct DialerInner {
    conns: Mutex<HashMap<Key, (u64, Connection)>>,
    next_id: AtomicU64,
}

impl Drop for DialerInner {
    /// The watcher tasks hold connection handles; close the connections explicitly so dropping
    /// the last dialer clone ends them (and the watchers).
    fn drop(&mut self) {
        let conns = std::mem::take(self.conns.get_mut().unwrap_or_else(|e| e.into_inner()));
        for (_, (_, c)) in conns {
            c.close(0, "dialer dropped");
        }
    }
}

impl std::fmt::Debug for Dialer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Dialer({:?})", self.transport)
    }
}

impl Dialer {
    pub fn new(transport: Transport) -> Self {
        Self { transport, inner: Arc::default() }
    }

    pub fn transport(&self) -> &Transport {
        &self.transport
    }

    /// The cached connection to `addr.peer` for `service`, dialing if there is none.
    ///
    /// Two concurrent first calls may both dial; the later one wins the cache slot and the
    /// other connection is simply used once and dropped.
    pub async fn connection(&self, addr: &PeerAddr, service: &str) -> Result<Connection> {
        let key = (addr.peer, service.to_string());
        if let Some((_, c)) = self.inner.conns.lock().unwrap().get(&key) {
            return Ok(c.clone());
        }
        if !addr.is_empty() {
            self.transport.add_peer_addr(addr.clone());
        }
        let conn = self.transport.connect(addr.clone(), service).await?;
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        self.inner.conns.lock().unwrap().insert(key.clone(), (id, conn.clone()));
        // Drop the cache entry when the connection ends, so the next call re-dials.
        let weak = Arc::downgrade(&self.inner);
        let watched = conn.clone();
        tokio::spawn(async move {
            let reason = watched.closed().await;
            drop(watched);
            tracing::debug!(peer = %key.0.fmt_short(), service = %key.1, %reason, "dialed connection ended");
            if let Some(inner) = weak.upgrade() {
                let mut conns = inner.conns.lock().unwrap();
                if conns.get(&key).is_some_and(|(i, _)| *i == id) {
                    conns.remove(&key);
                }
            }
        });
        Ok(conn)
    }

    /// Opens a new stream to `addr.peer` for `service`. If the cached connection cannot open a
    /// stream, it is dropped and the peer is dialed once more.
    pub async fn open_bi(&self, addr: &PeerAddr, service: &str) -> Result<BiStream> {
        let conn = self.connection(addr, service).await?;
        match conn.open_bi().await {
            Ok(s) => Ok(s),
            Err(e) => {
                tracing::debug!(peer = %addr.peer.fmt_short(), "stream open failed ({e}); re-dialing");
                self.forget(&addr.peer, service);
                self.connection(addr, service).await?.open_bi().await
            }
        }
    }

    /// Closes and forgets the cached connection to `peer` for `service`; the next call dials.
    pub fn forget(&self, peer: &PeerId, service: &str) {
        let gone = self.inner.conns.lock().unwrap().remove(&(*peer, service.to_string()));
        if let Some((_, c)) = gone {
            c.close(0, "forgotten");
        }
    }

    /// Closes and forgets every cached connection to `peer`.
    pub fn disconnect(&self, peer: &PeerId) {
        let gone: Vec<Connection> = {
            let mut conns = self.inner.conns.lock().unwrap();
            let keys: Vec<Key> = conns.keys().filter(|(p, _)| p == peer).cloned().collect();
            keys.iter().filter_map(|k| conns.remove(k)).map(|(_, c)| c).collect()
        };
        for c in gone {
            c.close(0, "disconnect");
        }
    }
}
