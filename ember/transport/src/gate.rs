//! Per-device admission (SPEC `FR-N3`): which peers may connect, and revocation.
//!
//! The transport already authenticates every connection (the remote [`PeerId`] is proven by its
//! key). A [`PeerGate`] decides whether that identity is *allowed*: an accepting side passes
//! each new connection through [`PeerGate::admit`], which refuses (closes) connections from
//! peers not on the allow-list. [`PeerGate::revoke`] removes a peer and immediately closes every
//! connection it has open through this gate, so a revoked device is cut off at once rather than
//! at its next request.
//!
//! The gate is storage-agnostic: ember server fills it from its devices table, ember node from
//! its configuration. It holds no backend type (`FR-N5`).
//!
//! A backend that enforces admission below the gate (the letify-link relay on the main server,
//! and its handshake policy) is told about every change through [`PeerGate::attach`], so a
//! revoked device is refused there too and its relayed pairs are cut at once.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::{Connection, PeerId, Transport};

/// Close code for a connection refused at accept (peer not allowed).
pub const CLOSE_NOT_ALLOWED: u32 = 403;
/// Close code for a connection closed because its peer was revoked.
pub const CLOSE_REVOKED: u32 = 401;

/// An allow-list of peers plus the live connections admitted through it. Cheap to clone; all
/// clones share one list.
#[derive(Clone, Default)]
pub struct PeerGate {
    inner: Arc<GateInner>,
}

#[derive(Default)]
struct GateInner {
    /// One lock for both, so `admit` and `revoke` cannot interleave (a connection admitted
    /// concurrently with its peer's revocation is either refused or closed by the revocation).
    state: Mutex<GateState>,
    next_id: AtomicU64,
    /// Transports told about every allow and revoke (see [`PeerGate::attach`]).
    transports: Mutex<Vec<Transport>>,
}

#[derive(Default)]
struct GateState {
    /// `None`: every authenticated peer is admitted (tests, explicitly open services).
    allowed: Option<HashSet<PeerId>>,
    /// Revoked on an open gate: refused even though the gate is open.
    denied: HashSet<PeerId>,
    live: HashMap<PeerId, Vec<(u64, Connection)>>,
}

impl GateState {
    fn admits(&self, peer: &PeerId) -> bool {
        !self.denied.contains(peer) && self.allowed.as_ref().is_none_or(|a| a.contains(peer))
    }
}

impl std::fmt::Debug for PeerGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let st = self.inner.state.lock().unwrap();
        match &st.allowed {
            None => write!(f, "PeerGate(open, {} live)", st.live.len()),
            Some(a) => write!(f, "PeerGate({} allowed, {} live)", a.len(), st.live.len()),
        }
    }
}

impl PeerGate {
    /// A gate that admits only `peers` (an empty list admits nobody).
    pub fn allow_list(peers: impl IntoIterator<Item = PeerId>) -> Self {
        let gate = Self::default();
        gate.inner.state.lock().unwrap().allowed = Some(peers.into_iter().collect());
        gate
    }

    /// A gate that admits every authenticated peer. Connections are still tracked, so
    /// [`PeerGate::revoke`] can close them; a revoked peer is then refused until
    /// [`PeerGate::allow`]ed again.
    pub fn open() -> Self {
        Self::default()
    }

    /// Tells `transport` about this gate: it is told every peer admitted now and every later
    /// [`PeerGate::allow`], [`PeerGate::revoke`] and [`PeerGate::set_allowed`] change
    /// ([`Transport::allow_peer`], [`Transport::revoke_peer`]). With the letify backend this wires
    /// revocation to `PeerPolicy::revoke` (handshakes and live sessions) and to the relay.
    pub fn attach(&self, transport: &Transport) {
        for peer in self.allowed().unwrap_or_default() {
            transport.allow_peer(peer);
        }
        self.inner.transports.lock().unwrap().push(transport.clone());
    }

    fn each_transport(&self, f: impl Fn(&Transport)) {
        let list: Vec<Transport> = self.inner.transports.lock().unwrap().clone();
        list.iter().for_each(f);
    }

    /// Whether `peer` would be admitted now.
    pub fn is_allowed(&self, peer: &PeerId) -> bool {
        self.inner.state.lock().unwrap().admits(peer)
    }

    /// The allow-list, or `None` for an open gate.
    pub fn allowed(&self) -> Option<Vec<PeerId>> {
        let st = self.inner.state.lock().unwrap();
        st.allowed.as_ref().map(|a| {
            let mut v: Vec<PeerId> = a.iter().copied().collect();
            v.sort();
            v
        })
    }

    /// Adds `peer` to the allow-list (on an open gate: lifts an earlier revocation).
    pub fn allow(&self, peer: PeerId) {
        let mut st = self.inner.state.lock().unwrap();
        st.denied.remove(&peer);
        if let Some(a) = st.allowed.as_mut() {
            a.insert(peer);
        }
        drop(st);
        self.each_transport(|t| t.allow_peer(peer));
    }

    /// Removes `peer` and closes its open connections. Returns how many were closed.
    pub fn revoke(&self, peer: &PeerId) -> usize {
        let conns = {
            let mut guard = self.inner.state.lock().unwrap();
            let st = &mut *guard;
            match st.allowed.as_mut() {
                Some(a) => {
                    a.remove(peer);
                }
                None => {
                    st.denied.insert(*peer);
                }
            }
            st.live.remove(peer).unwrap_or_default()
        };
        for (_, c) in &conns {
            c.close(CLOSE_REVOKED, "device revoked");
        }
        // After the close frames are queued, so the peer sees the code before the session ends.
        self.each_transport(|t| t.revoke_peer(*peer));
        if !conns.is_empty() {
            tracing::info!(peer = %peer.fmt_short(), closed = conns.len(), "revoked peer disconnected");
        }
        conns.len()
    }

    /// Replaces the allow-list (e.g. after reloading configuration). Connections of peers no
    /// longer allowed are closed. Returns how many connections were closed.
    pub fn set_allowed(&self, peers: impl IntoIterator<Item = PeerId>) -> usize {
        let allowed: HashSet<PeerId> = peers.into_iter().collect();
        let (dropped, removed, added): (Vec<(u64, Connection)>, Vec<PeerId>, Vec<PeerId>) = {
            let mut guard = self.inner.state.lock().unwrap();
            let st = &mut *guard;
            let gone: Vec<PeerId> = st.live.keys().filter(|p| !allowed.contains(p)).copied().collect();
            let before = st.allowed.take().unwrap_or_default();
            let removed: Vec<PeerId> = before.iter().filter(|p| !allowed.contains(p)).copied().collect();
            let added: Vec<PeerId> = allowed.iter().filter(|p| !before.contains(p)).copied().collect();
            st.allowed = Some(allowed);
            st.denied.clear();
            let dropped = gone.iter().flat_map(|p| st.live.remove(p).unwrap_or_default()).collect();
            (dropped, removed, added)
        };
        for (_, c) in &dropped {
            c.close(CLOSE_REVOKED, "device no longer allowed");
        }
        self.each_transport(|t| {
            removed.iter().for_each(|p| t.revoke_peer(*p));
            added.iter().for_each(|p| t.allow_peer(*p));
        });
        dropped.len()
    }

    /// Admits an accepted connection, or refuses it: a refused connection is closed with
    /// [`CLOSE_NOT_ALLOWED`] and `None` is returned. An admitted connection is tracked until it
    /// closes, so a later revocation can close it.
    pub fn admit(&self, conn: Connection) -> Option<Connection> {
        let peer = conn.peer();
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut st = self.inner.state.lock().unwrap();
            if !st.admits(&peer) {
                drop(st);
                tracing::warn!(peer = %peer, service = conn.service(), "refused connection from unknown peer");
                conn.close(CLOSE_NOT_ALLOWED, "peer not allowed");
                return None;
            }
            st.live.entry(peer).or_default().push((id, conn.clone()));
        }
        // Drop the tracking entry once the connection ends.
        let weak = Arc::downgrade(&self.inner);
        let watched = conn.clone();
        tokio::spawn(async move {
            watched.closed().await;
            drop(watched);
            if let Some(inner) = weak.upgrade() {
                let mut guard = inner.state.lock().unwrap();
                let live = &mut guard.live;
                let empty = match live.get_mut(&peer) {
                    Some(list) => {
                        list.retain(|(i, _)| *i != id);
                        list.is_empty()
                    }
                    None => false,
                };
                if empty {
                    live.remove(&peer);
                }
            }
        });
        Some(conn)
    }

    /// Number of live admitted connections from `peer`.
    pub fn live_connections(&self, peer: &PeerId) -> usize {
        self.inner.state.lock().unwrap().live.get(peer).map_or(0, Vec::len)
    }
}
