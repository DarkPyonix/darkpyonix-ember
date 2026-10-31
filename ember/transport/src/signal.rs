//! Signalling and directory plumbing for the letify-link backend (SPEC `FR-N2`, `FR-N5`).
//!
//! These are plain Ember types: no letify-link type appears here. The letify backend adapts
//! [`SignalChannel`] to letify-link's `Rendezvous` trait internally.
//!
//! - [`SignalChannel`]: a small, authenticated, possibly delayed message channel between peers
//!   (the rendezvous). Two peers use it to agree on a relay pairing token. The production
//!   implementation is carried by the hub's pkarr publish and lookup; [`MemorySignalHub`] is the
//!   in-process one for tests.
//! - [`PkarrStore`]: publish and fetch signed pkarr packets by peer key (the hub's `/pkarr`).
//!   [`MemoryPkarrStore`] is the in-process hub for tests.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::{PeerId, Result, TransportError};

/// Largest signal message in bytes. letify-link's rendezvous messages are under 4 KiB; the pkarr
/// carrier is far smaller (a pkarr packet holds about 1000 bytes), which is enough for a relay
/// pairing offer.
pub const SIGNAL_MAX: usize = 4096;

/// A message channel between peers, used for rendezvous. Messages are small and may be delayed
/// by seconds; the sender of a received message is authenticated by the channel.
#[async_trait]
pub trait SignalChannel: Send + Sync + fmt::Debug + 'static {
    /// Sends `msg` to `to`.
    async fn send(&self, to: PeerId, msg: Vec<u8>) -> Result<()>;
    /// Waits for the next message and returns it with its authenticated sender.
    async fn recv(&self) -> Result<(PeerId, Vec<u8>)>;
    /// Tells a pull-based channel which peers may send to us, so it knows whose mailbox to look
    /// in. Push-based channels ignore it.
    fn watch(&self, _peers: &[PeerId]) {}
}

/// Publishes and fetches signed pkarr packets (the hub's `PUT/GET /pkarr/{key}`).
///
/// A payload is the pkarr relay form: 64 bytes of Ed25519 signature, 8 bytes of timestamp in
/// microseconds (big endian), then the DNS packet. The signature covers
/// `3:seqi<timestamp>e1:v<len>:<dns packet>`.
#[async_trait]
pub trait PkarrStore: Send + Sync + fmt::Debug + 'static {
    /// Publishes `payload` as the packet of `key`.
    async fn put(&self, key: PeerId, payload: Vec<u8>) -> Result<()>;
    /// The newest packet of `key`, or `None` when nothing is published for it.
    async fn get(&self, key: PeerId) -> Result<Option<Vec<u8>>>;
    /// Sets or clears the credential used to fetch (a hub resolve token). No-op by default.
    fn set_token(&self, _token: Option<String>) {}
}

/// Checks the signature and timestamp of a pkarr relay payload for `key`. Returns the timestamp
/// and the DNS packet.
pub fn verify_pkarr_payload(key: &PeerId, payload: &[u8]) -> Result<(u64, Vec<u8>)> {
    let bad = |why: &str| TransportError::Stream(format!("pkarr packet: {why}"));
    if payload.len() < 72 {
        return Err(bad("shorter than the 72 byte header"));
    }
    let sig: [u8; 64] = payload[..64].try_into().expect("slice of 64 bytes");
    let ts = u64::from_be_bytes(payload[64..72].try_into().expect("slice of 8 bytes"));
    let dns = &payload[72..];
    if !key.verify(&pkarr_signable(ts, dns), &sig) {
        return Err(bad("signature does not verify"));
    }
    Ok((ts, dns.to_vec()))
}

/// The bytes a pkarr packet signature covers.
pub fn pkarr_signable(timestamp_us: u64, dns: &[u8]) -> Vec<u8> {
    let mut out = format!("3:seqi{timestamp_us}e1:v{}:", dns.len()).into_bytes();
    out.extend_from_slice(dns);
    out
}

// ---------------------------------------------------------------------------------------------
// In-process implementations, for tests and for running several peers in one process.
// ---------------------------------------------------------------------------------------------

type Mailbox = mpsc::UnboundedSender<(PeerId, Vec<u8>)>;

/// An in-process signalling network. Cheap to clone.
#[derive(Clone, Default)]
pub struct MemorySignalHub {
    boxes: Arc<Mutex<HashMap<PeerId, Mailbox>>>,
}

impl MemorySignalHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// The channel of `peer`. Replaces an earlier channel of the same peer.
    pub fn channel(&self, peer: PeerId) -> Arc<MemorySignal> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.boxes.lock().unwrap().insert(peer, tx);
        Arc::new(MemorySignal { me: peer, hub: self.clone(), inbox: tokio::sync::Mutex::new(rx) })
    }
}

/// One peer's end of a [`MemorySignalHub`].
pub struct MemorySignal {
    me: PeerId,
    hub: MemorySignalHub,
    inbox: tokio::sync::Mutex<mpsc::UnboundedReceiver<(PeerId, Vec<u8>)>>,
}

impl fmt::Debug for MemorySignal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MemorySignal({})", self.me.fmt_short())
    }
}

#[async_trait]
impl SignalChannel for MemorySignal {
    async fn send(&self, to: PeerId, msg: Vec<u8>) -> Result<()> {
        if msg.len() > SIGNAL_MAX {
            return Err(TransportError::Stream("signal message too large".into()));
        }
        let tx = self.hub.boxes.lock().unwrap().get(&to).cloned();
        tx.ok_or_else(|| TransportError::Stream(format!("no signal endpoint for {}", to.fmt_short())))?
            .send((self.me, msg))
            .map_err(|_| TransportError::Stream("signal endpoint dropped".into()))
    }

    async fn recv(&self) -> Result<(PeerId, Vec<u8>)> {
        self.inbox
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| TransportError::Stream("signal hub closed".into()))
    }
}

/// An in-process pkarr directory that, like the hub, refuses a packet whose signature does not
/// verify or that is not newer than the stored one. Cheap to clone.
#[derive(Clone, Default)]
pub struct MemoryPkarrStore {
    packets: Arc<Mutex<HashMap<PeerId, (u64, Vec<u8>)>>>,
}

impl MemoryPkarrStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of peers with a stored packet.
    pub fn len(&self) -> usize {
        self.packets.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl fmt::Debug for MemoryPkarrStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MemoryPkarrStore({} packets)", self.len())
    }
}

#[async_trait]
impl PkarrStore for MemoryPkarrStore {
    async fn put(&self, key: PeerId, payload: Vec<u8>) -> Result<()> {
        let (ts, _) = verify_pkarr_payload(&key, &payload)?;
        let mut packets = self.packets.lock().unwrap();
        if packets.get(&key).is_some_and(|(old, _)| *old >= ts) {
            return Err(TransportError::Stream("pkarr packet is not newer than the stored one".into()));
        }
        packets.insert(key, (ts, payload));
        Ok(())
    }

    async fn get(&self, key: PeerId) -> Result<Option<Vec<u8>>> {
        Ok(self.packets.lock().unwrap().get(&key).map(|(_, p)| p.clone()))
    }
}
