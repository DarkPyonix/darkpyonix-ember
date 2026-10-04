//! Peer addressing and the pluggable address directory (SPEC `FR-N2`).
//!
//! A peer is dialed by its [`PeerId`]. How the transport finds where that peer is reachable is
//! an [`AddressDirectory`]: today an in-memory one (tests) or iroh's own lookup; later the
//! `darkpyonix.dev` address directory, implemented against this trait without touching callers.

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::PeerId;

/// Where a peer can be reached: its identity plus optional hints.
///
/// `relays` are relay server URLs (the peer's home relay first); `direct` are UDP socket
/// addresses that may work for a direct path. Both may be empty, in which case the transport
/// resolves the peer through its address directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerAddr {
    pub peer: PeerId,
    #[serde(default)]
    pub relays: Vec<String>,
    #[serde(default)]
    pub direct: Vec<SocketAddr>,
}

impl PeerAddr {
    pub fn new(peer: PeerId) -> Self {
        Self { peer, relays: Vec::new(), direct: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.relays.is_empty() && self.direct.is_empty()
    }
}

impl From<PeerId> for PeerAddr {
    fn from(peer: PeerId) -> Self {
        Self::new(peer)
    }
}

/// A service that publishes our addresses and resolves other peers' addresses.
///
/// `publish` is fire-and-forget (called whenever our reachable addresses change); an
/// implementation that talks to the network spawns its own task. `resolve` returns the best
/// known address for a peer, or `None`.
#[async_trait]
pub trait AddressDirectory: Send + Sync + fmt::Debug + 'static {
    fn publish(&self, _addr: &PeerAddr) {}
    async fn resolve(&self, peer: PeerId) -> Option<PeerAddr>;
}

/// A process-local directory: everything published is resolvable by every transport sharing
/// this value. For tests and for running several peers in one process.
#[derive(Clone, Debug, Default)]
pub struct MemoryDirectory {
    entries: Arc<Mutex<HashMap<PeerId, PeerAddr>>>,
}

impl MemoryDirectory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or replaces an entry by hand.
    pub fn insert(&self, addr: PeerAddr) {
        self.entries.lock().unwrap().insert(addr.peer, addr);
    }

    pub fn get(&self, peer: &PeerId) -> Option<PeerAddr> {
        self.entries.lock().unwrap().get(peer).cloned()
    }
}

#[async_trait]
impl AddressDirectory for MemoryDirectory {
    fn publish(&self, addr: &PeerAddr) {
        self.insert(addr.clone());
    }

    async fn resolve(&self, peer: PeerId) -> Option<PeerAddr> {
        self.get(&peer)
    }
}
