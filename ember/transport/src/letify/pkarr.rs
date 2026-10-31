//! The hub's pkarr directory as the rendezvous of the letify-link backend (SPEC `FR-N2`).
//!
//! One pkarr packet per peer key carries two kinds of TXT records:
//!
//! - `_letify.<key>`: the signed letify-link address record (`record::to_pkarr_txt`), so a peer
//!   that knows our key finds our dialable addresses and the transports we accept.
//! - `m<recipient>.<key>`: our mailbox for one recipient, a short queue of messages. The packet
//!   is signed by our key, so the recipient authenticates the sender. The recipient reads the
//!   mailboxes of the peers it watches every poll interval. Messages are therefore delayed by up
//!   to one interval, which letify-link's rendezvous allows ("may be delayed by seconds").
//!
//! A pkarr packet holds about 1000 bytes of DNS, so the mailbox is a few hundred bytes: enough
//! for relay pairing offers (a 16 byte token), not for bulk data.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use letify_link::record::{self, AddressRecord};
use letify_link::NodeId;
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;

use crate::signal::{pkarr_signable, verify_pkarr_payload};
use crate::{PeerId, PkarrStore, Result, SecretKey, SignalChannel, TransportError};

/// Largest DNS packet we publish.
const MAX_DNS: usize = 1000;
/// Bytes of mailbox payload kept per recipient (older messages are dropped first).
const MAILBOX_BYTES: usize = 130;
/// A queued message older than this many seconds is dropped.
const MESSAGE_TTL_S: u32 = 120;
/// The record is re-signed and published at least this often (its max age is 10 minutes).
const REPUBLISH: Duration = Duration::from_secs(120);
/// DNS name of the address record.
const RECORD_LABEL: &str = "_letify";

fn unix_secs() -> u32 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as u32).unwrap_or(0)
}

fn unix_micros() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_micros() as u64).unwrap_or(0)
}

const Z32: &[u8; 32] = b"ybndrfg8ejkmcpqxot1uwisza345h769";

/// z-base-32 of a 32 byte key, the encoding of the hub's `/pkarr/{key}` paths.
pub(super) fn z32(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
    let (mut buffer, mut bits) = (0u32, 0u32);
    for &b in bytes {
        buffer = (buffer << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            out.push(Z32[((buffer >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
        buffer &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(Z32[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// The mailbox label for `recipient`: `m` and the first 8 bytes in hex.
fn mailbox_label(recipient: &PeerId) -> String {
    format!("m{}", hex::encode(&recipient.as_bytes()[..8]))
}

// ---------------------------------------------------------------------------------------------
// Minimal DNS: build and read TXT answers.
// ---------------------------------------------------------------------------------------------

/// A DNS packet of TXT answers, one per `(name, character-strings)`.
pub(super) fn build_dns(answers: &[(String, Vec<String>)]) -> Vec<u8> {
    let mut out = vec![0, 0, 0x84, 0, 0, 0];
    out.extend_from_slice(&(answers.len() as u16).to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    for (name, strings) in answers {
        for label in name.split('.').filter(|l| !l.is_empty()) {
            let label = &label.as_bytes()[..label.len().min(63)];
            out.push(label.len() as u8);
            out.extend_from_slice(label);
        }
        out.push(0);
        out.extend_from_slice(&[0, 16, 0, 1]);
        out.extend_from_slice(&300u32.to_be_bytes());
        let rdata_len: usize = strings.iter().map(|s| 1 + s.len().min(255)).sum();
        out.extend_from_slice(&(rdata_len as u16).to_be_bytes());
        for s in strings {
            let s = &s.as_bytes()[..s.len().min(255)];
            out.push(s.len() as u8);
            out.extend_from_slice(s);
        }
    }
    out
}

fn read_name(b: &[u8], start: usize) -> Option<(String, usize)> {
    let mut labels = Vec::new();
    let mut off = start;
    let mut end = None;
    let mut jumps = 0;
    loop {
        let len = *b.get(off)? as usize;
        if len & 0xc0 == 0xc0 {
            let lo = *b.get(off + 1)? as usize;
            jumps += 1;
            if jumps > 32 {
                return None;
            }
            end.get_or_insert(off + 2);
            off = ((len & 0x3f) << 8) | lo;
            continue;
        }
        if len & 0xc0 != 0 {
            return None;
        }
        if len == 0 {
            end.get_or_insert(off + 1);
            break;
        }
        let label = b.get(off + 1..off + 1 + len)?;
        labels.push(String::from_utf8_lossy(label).to_ascii_lowercase());
        off += 1 + len;
    }
    Some((labels.join("."), end?))
}

/// `(name, character-strings)` of each TXT answer; `None` when the packet is malformed.
pub(super) fn parse_dns(b: &[u8]) -> Option<Vec<(String, Vec<String>)>> {
    let u16_at = |o: usize| -> Option<u16> { Some(u16::from_be_bytes([*b.get(o)?, *b.get(o + 1)?])) };
    let qd = u16_at(4)?;
    let an = u16_at(6)?;
    let mut off = 12;
    for _ in 0..qd {
        off = read_name(b, off)?.1 + 4;
    }
    let mut out = Vec::new();
    for _ in 0..an {
        let (name, end) = read_name(b, off)?;
        let ty = u16_at(end)?;
        let rdlen = u16_at(end + 8)? as usize;
        let rdata = b.get(end + 10..end + 10 + rdlen)?;
        off = end + 10 + rdlen;
        if ty == 16 {
            let mut strings = Vec::new();
            let mut o = 0;
            while o < rdata.len() {
                let n = rdata[o] as usize;
                strings.push(std::str::from_utf8(rdata.get(o + 1..o + 1 + n)?).ok()?.to_string());
                o += 1 + n;
            }
            out.push((name, strings));
        }
    }
    Some(out)
}

fn first_label(name: &str) -> &str {
    name.split('.').next().unwrap_or("")
}

// ---------------------------------------------------------------------------------------------
// HTTP store
// ---------------------------------------------------------------------------------------------

/// The hub's `/pkarr` over HTTP (`PUT` to publish, `GET` to fetch), SPEC `FR-N2`.
pub struct HttpPkarrStore {
    base: String,
    token: Mutex<Option<String>>,
    client: reqwest::Client,
}

impl HttpPkarrStore {
    /// `base` is the hub's pkarr URL (`https://api.darkpyonix.dev/pkarr`), `token` the device's
    /// resolve token.
    pub fn new(base: impl Into<String>, token: Option<String>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| TransportError::Config(format!("http client: {e}")))?;
        Ok(Self { base: base.into().trim_end_matches('/').to_string(), token: Mutex::new(token), client })
    }

    fn url(&self, key: &PeerId) -> String {
        format!("{}/{}", self.base, z32(key.as_bytes()))
    }
}

impl fmt::Debug for HttpPkarrStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpPkarrStore").field("base", &self.base).finish_non_exhaustive()
    }
}

#[async_trait]
impl PkarrStore for HttpPkarrStore {
    async fn put(&self, key: PeerId, payload: Vec<u8>) -> Result<()> {
        let resp = self
            .client
            .put(self.url(&key))
            .body(payload)
            .send()
            .await
            .map_err(|e| TransportError::Stream(format!("pkarr publish: {e}")))?;
        let status = resp.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(TransportError::Stream(format!("pkarr publish refused: {status}")))
        }
    }

    async fn get(&self, key: PeerId) -> Result<Option<Vec<u8>>> {
        let token = self.token.lock().unwrap().clone();
        let mut req = self.client.get(self.url(&key));
        if let Some(t) = token {
            req = req.query(&[("token", t)]);
        }
        let resp = req.send().await.map_err(|e| TransportError::Stream(format!("pkarr fetch: {e}")))?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(TransportError::Stream(format!("pkarr fetch refused: {status}")));
        }
        let bytes = resp.bytes().await.map_err(|e| TransportError::Stream(format!("pkarr fetch: {e}")))?;
        Ok(Some(bytes.to_vec()))
    }

    fn set_token(&self, token: Option<String>) {
        *self.token.lock().unwrap() = token;
    }
}

// ---------------------------------------------------------------------------------------------
// The node: publishes our record and mailboxes, reads watched peers' mailboxes.
// ---------------------------------------------------------------------------------------------

pub(super) type RecordFn = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

#[derive(Default)]
struct State {
    /// Messages for each recipient: `(sequence, bytes)`, oldest first.
    outbox: HashMap<PeerId, VecDeque<(u32, Vec<u8>)>>,
    seq: u32,
    last_ts: u64,
    watched: HashSet<PeerId>,
    /// Per sender: the packet timestamp and message sequence already delivered.
    seen: HashMap<PeerId, (u64, u32)>,
}

/// Publishes our record and mailboxes to a [`PkarrStore`], reads the mailboxes of watched peers
/// and looks peers' records up. Also the production [`SignalChannel`].
pub(super) struct PkarrNode {
    key: SecretKey,
    me: PeerId,
    store: Arc<dyn PkarrStore>,
    record: RecordFn,
    state: Mutex<State>,
    dirty: Arc<Notify>,
    inbox_tx: mpsc::UnboundedSender<(PeerId, Vec<u8>)>,
    inbox: tokio::sync::Mutex<mpsc::UnboundedReceiver<(PeerId, Vec<u8>)>>,
    start_secs: u32,
    publish_ok: AtomicBool,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl fmt::Debug for PkarrNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PkarrNode({})", self.me.fmt_short())
    }
}

impl Drop for PkarrNode {
    fn drop(&mut self) {
        for t in self.tasks.get_mut().unwrap_or_else(|e| e.into_inner()).drain(..) {
            t.abort();
        }
    }
}

impl PkarrNode {
    /// Starts publishing and polling. `record` returns the fresh TXT strings of our address
    /// record (empty: publish mailboxes only).
    pub(super) fn start(
        key: SecretKey,
        store: Arc<dyn PkarrStore>,
        record: RecordFn,
        poll_interval: Duration,
    ) -> Arc<Self> {
        let (inbox_tx, rx) = mpsc::unbounded_channel();
        let node = Arc::new(Self {
            me: key.peer_id(),
            key,
            store,
            record,
            state: Mutex::new(State::default()),
            dirty: Arc::new(Notify::new()),
            inbox_tx,
            inbox: tokio::sync::Mutex::new(rx),
            start_secs: unix_secs(),
            publish_ok: AtomicBool::new(false),
            tasks: Mutex::new(Vec::new()),
        });
        // The tasks hold weak references so dropping the node stops them.
        let publisher = tokio::spawn(publish_loop(Arc::downgrade(&node)));
        let poller = tokio::spawn(poll_loop(Arc::downgrade(&node), poll_interval));
        node.tasks.lock().unwrap().extend([publisher, poller]);
        node.dirty.notify_one();
        node
    }

    pub(super) fn store(&self) -> &Arc<dyn PkarrStore> {
        &self.store
    }

    /// Whether the last publish succeeded.
    #[allow(dead_code)]
    pub(super) fn published(&self) -> bool {
        self.publish_ok.load(Ordering::Relaxed)
    }

    /// The record changed: publish soon.
    pub(super) fn republish(&self) {
        self.dirty.notify_one();
    }

    pub(super) fn watch_peers(&self, peers: &[PeerId]) {
        let mut st = self.state.lock().unwrap();
        for p in peers {
            if *p != self.me {
                st.watched.insert(*p);
            }
        }
    }

    /// The DNS packet to publish now. When it is over the pkarr size, the oldest queued
    /// messages are dropped until it fits.
    fn build_packet(&self) -> Vec<u8> {
        let now = unix_secs();
        let zone = z32(self.me.as_bytes());
        let record_txt = (self.record)();
        let mut st = self.state.lock().unwrap();
        for queue in st.outbox.values_mut() {
            queue.retain(|(seq, _)| seq.saturating_add(MESSAGE_TTL_S) >= now);
        }
        st.outbox.retain(|_, q| !q.is_empty());
        loop {
            let mut answers: Vec<(String, Vec<String>)> = Vec::new();
            if !record_txt.is_empty() {
                answers.push((format!("{RECORD_LABEL}.{zone}"), record_txt.clone()));
            }
            for (to, queue) in st.outbox.iter() {
                let mut bytes = Vec::new();
                for (seq, msg) in queue.iter() {
                    bytes.extend_from_slice(&seq.to_be_bytes());
                    bytes.extend_from_slice(&(msg.len() as u16).to_be_bytes());
                    bytes.extend_from_slice(msg);
                }
                answers.push((format!("{}.{zone}", mailbox_label(to)), record::to_pkarr_txt(&bytes)));
            }
            let dns = build_dns(&answers);
            if dns.len() <= MAX_DNS {
                return dns;
            }
            // Over the size: drop the oldest queued message of all recipients.
            let oldest = st
                .outbox
                .iter()
                .filter_map(|(peer, q)| q.front().map(|(seq, _)| (*seq, *peer)))
                .min_by_key(|(seq, _)| *seq);
            let Some((_, peer)) = oldest else {
                return dns;
            };
            if let Some(q) = st.outbox.get_mut(&peer) {
                q.pop_front();
            }
            st.outbox.retain(|_, q| !q.is_empty());
        }
    }

    async fn publish_once(&self) -> Result<()> {
        let dns = self.build_packet();
        if dns.len() > MAX_DNS {
            return Err(TransportError::Stream(format!("pkarr packet of {} bytes is over {MAX_DNS}", dns.len())));
        }
        let ts = {
            let mut st = self.state.lock().unwrap();
            st.last_ts = unix_micros().max(st.last_ts + 1);
            st.last_ts
        };
        let sig = self.key.sign(&pkarr_signable(ts, &dns));
        let mut payload = Vec::with_capacity(72 + dns.len());
        payload.extend_from_slice(&sig);
        payload.extend_from_slice(&ts.to_be_bytes());
        payload.extend_from_slice(&dns);
        self.store.put(self.me, payload).await
    }

    /// The signed address record of `peer` from its pkarr packet.
    pub(super) async fn lookup(&self, peer: PeerId) -> Result<Option<AddressRecord>> {
        let Some(payload) = self.store.get(peer).await? else {
            return Ok(None);
        };
        let (_, dns) = verify_pkarr_payload(&peer, &payload)?;
        let answers = parse_dns(&dns).ok_or_else(|| TransportError::Stream("pkarr packet: bad DNS".into()))?;
        let Some((_, strings)) = answers.iter().find(|(name, _)| first_label(name) == RECORD_LABEL) else {
            return Ok(None);
        };
        let rec = AddressRecord::decode_txt(strings.as_slice(), record::unix_now())
            .map_err(|e| TransportError::Stream(format!("address record of {}: {e}", peer.fmt_short())))?;
        if rec.node != NodeId::from_bytes(*peer.as_bytes()) {
            return Err(TransportError::Stream("address record names another node".into()));
        }
        Ok(Some(rec))
    }

    /// Reads the mailbox `from` keeps for us and queues messages not seen before.
    async fn poll_peer(&self, from: PeerId) -> Result<()> {
        let Some(payload) = self.store.get(from).await? else {
            return Ok(());
        };
        let (ts, dns) = verify_pkarr_payload(&from, &payload)?;
        let (seen_ts, seen_seq) = {
            let st = self.state.lock().unwrap();
            st.seen.get(&from).copied().unwrap_or((0, self.start_secs.saturating_sub(30)))
        };
        if ts <= seen_ts {
            return Ok(());
        }
        let answers = parse_dns(&dns).ok_or_else(|| TransportError::Stream("pkarr packet: bad DNS".into()))?;
        let label = mailbox_label(&self.me);
        let mut top = seen_seq;
        let mut fresh: Vec<Vec<u8>> = Vec::new();
        for (name, strings) in &answers {
            if first_label(name) != label {
                continue;
            }
            let Ok(bytes) = record::from_pkarr_txt(strings.as_slice()) else { continue };
            let mut at = 0;
            while at + 6 <= bytes.len() {
                let seq = u32::from_be_bytes(bytes[at..at + 4].try_into().expect("4 bytes"));
                let len = u16::from_be_bytes(bytes[at + 4..at + 6].try_into().expect("2 bytes")) as usize;
                let Some(msg) = bytes.get(at + 6..at + 6 + len) else { break };
                at += 6 + len;
                if seq > seen_seq {
                    top = top.max(seq);
                    fresh.push(msg.to_vec());
                }
            }
        }
        self.state.lock().unwrap().seen.insert(from, (ts, top));
        for msg in fresh {
            let _ = self.inbox_tx.send((from, msg));
        }
        Ok(())
    }
}

async fn publish_loop(weak: std::sync::Weak<PkarrNode>) {
    let mut backoff = Duration::from_secs(2);
    loop {
        let Some(dirty) = weak.upgrade().map(|n| n.dirty.clone()) else { return };
        // A permit left by `republish` ends this wait at once. No strong reference to the node
        // is held while waiting, so dropping the node stops the loop.
        let _ = tokio::time::timeout(REPUBLISH, dirty.notified()).await;
        let Some(node) = weak.upgrade() else { return };
        match node.publish_once().await {
            Ok(()) => {
                node.publish_ok.store(true, Ordering::Relaxed);
                backoff = Duration::from_secs(2);
            }
            Err(e) => {
                node.publish_ok.store(false, Ordering::Relaxed);
                tracing::warn!(error = %e, "publishing to the pkarr directory failed; retrying");
                let again = backoff;
                backoff = (backoff * 2).min(Duration::from_secs(60));
                let n = weak.clone();
                drop(node);
                tokio::time::sleep(again).await;
                if let Some(node) = n.upgrade() {
                    node.dirty.notify_one();
                }
            }
        }
    }
}

async fn poll_loop(weak: std::sync::Weak<PkarrNode>, every: Duration) {
    loop {
        tokio::time::sleep(every).await;
        let Some(node) = weak.upgrade() else { return };
        let peers: Vec<PeerId> = node.state.lock().unwrap().watched.iter().copied().collect();
        for p in peers {
            if let Err(e) = node.poll_peer(p).await {
                tracing::debug!(peer = %p.fmt_short(), error = %e, "reading a pkarr mailbox failed");
            }
        }
    }
}

#[async_trait]
impl SignalChannel for PkarrNode {
    async fn send(&self, to: PeerId, msg: Vec<u8>) -> Result<()> {
        if msg.len() + 6 > MAILBOX_BYTES {
            return Err(TransportError::Stream("signal message too large for a pkarr mailbox".into()));
        }
        {
            let mut st = self.state.lock().unwrap();
            st.watched.insert(to);
            st.seq = (st.seq + 1).max(unix_secs());
            let seq = st.seq;
            let queue = st.outbox.entry(to).or_default();
            queue.push_back((seq, msg));
            while queue.iter().map(|(_, m)| m.len() + 6).sum::<usize>() > MAILBOX_BYTES {
                queue.pop_front();
            }
        }
        self.dirty.notify_one();
        Ok(())
    }

    async fn recv(&self) -> Result<(PeerId, Vec<u8>)> {
        self.inbox
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| TransportError::Stream("pkarr mailbox closed".into()))
    }

    fn watch(&self, peers: &[PeerId]) {
        self.watch_peers(peers);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn z32_matches_the_hub_key_length_and_alphabet() {
        let s = z32(&[0u8; 32]);
        assert_eq!(s.len(), 52);
        assert!(s.bytes().all(|b| Z32.contains(&b)));
    }

    #[test]
    fn dns_round_trips_txt_answers() {
        let answers = vec![
            ("_letify.abc".to_string(), vec!["one".to_string(), "two".to_string()]),
            ("mdeadbeef.abc".to_string(), vec!["x".repeat(255)]),
        ];
        let packet = build_dns(&answers);
        assert_eq!(parse_dns(&packet), Some(answers));
        assert_eq!(parse_dns(&packet[..packet.len() - 1]), None);
    }

    #[tokio::test]
    async fn a_message_reaches_a_watching_peer_through_the_signed_packets() {
        use crate::MemoryPkarrStore;
        let store = MemoryPkarrStore::new();
        let (ka, kb) = (SecretKey::generate(), SecretKey::generate());
        let none: RecordFn = Arc::new(|| Vec::<String>::new());
        let a = PkarrNode::start(ka.clone(), Arc::new(store.clone()), none.clone(), Duration::from_millis(50));
        let b = PkarrNode::start(kb.clone(), Arc::new(store.clone()), none, Duration::from_millis(50));
        a.watch_peers(&[kb.peer_id()]);
        b.send(ka.peer_id(), b"hello".to_vec()).await.unwrap();
        let (from, msg) = tokio::time::timeout(Duration::from_secs(5), a.recv()).await.unwrap().unwrap();
        assert_eq!(from, kb.peer_id());
        assert_eq!(msg, b"hello");
        // A reply travels the other way, through a's own packet.
        a.send(kb.peer_id(), b"reply".to_vec()).await.unwrap();
        let (from, msg) = tokio::time::timeout(Duration::from_secs(5), b.recv()).await.unwrap().unwrap();
        assert_eq!(from, ka.peer_id());
        assert_eq!(msg, b"reply");
    }

    #[tokio::test]
    async fn the_record_is_found_by_the_peer_key_and_must_name_that_peer() {
        use crate::MemoryPkarrStore;
        let store = MemoryPkarrStore::new();
        let (ka, kb) = (SecretKey::generate(), SecretKey::generate());
        let identity = letify_link::Identity::from_secret_bytes(kb.to_bytes());
        let mut rec = AddressRecord::new(NodeId::from_bytes(*kb.peer_id().as_bytes()), record::unix_now());
        rec.tcp = vec!["127.0.0.1:4242".parse().unwrap()];
        rec.transports = vec![record::DEFAULT_TRANSPORT.to_string()];
        let txt = rec.encode_txt(&identity);
        let _b = PkarrNode::start(kb.clone(), Arc::new(store.clone()), Arc::new(move || txt.clone()), Duration::from_secs(60));
        let a = PkarrNode::start(ka, Arc::new(store.clone()), Arc::new(|| Vec::<String>::new()), Duration::from_secs(60));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let found = loop {
            if let Some(found) = a.lookup(kb.peer_id()).await.unwrap() {
                break found;
            }
            assert!(std::time::Instant::now() < deadline, "the record was never published");
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert_eq!(found.tcp, vec!["127.0.0.1:4242".parse::<std::net::SocketAddr>().unwrap()]);
        assert!(found.speaks(record::DEFAULT_TRANSPORT));
        // Nothing is published for an unknown key.
        assert!(a.lookup(SecretKey::generate().peer_id()).await.unwrap().is_none());
    }
}
