//! The hub's pkarr directory as the rendezvous of the letify-link backend (SPEC `FR-N2`).
//!
//! Polling is adaptive (#783): a watched peer is read at the fast interval while a dial or a
//! signal exchange with it is in flight and for a short window after it, and otherwise at an
//! interval that doubles up to the idle interval. The hub runs on a request quota, so an idle
//! pair must cost little.
//!
//! One pkarr packet per peer key carries two kinds of TXT records:
//!
//! - `_letify.<key>`: the signed letify-link address record (`record::to_pkarr_txt`), so a peer
//!   that knows our key finds our dialable addresses and the transports we accept.
//! - `m<recipient>.<key>`: our mailbox for one recipient, a short queue of messages. The packet
//!   is signed by our key, so the recipient authenticates the sender. The recipient reads the
//!   mailboxes of the peers it watches on the adaptive schedule below. Messages are delayed by up
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
use crate::{
    PeerId, PkarrStore, PkarrWatch, Result, SecretKey, SignalChannel, TransportError, WatchLimits, WatchOutcome,
};

/// Largest DNS packet we publish.
const MAX_DNS: usize = 1000;
/// Bytes of mailbox payload kept per recipient (older messages are dropped first).
const MAILBOX_BYTES: usize = 200;
/// A queued message older than this many seconds is dropped.
const MESSAGE_TTL_S: u32 = 120;
/// The record is re-signed and published at least this often (its max age is 10 minutes).
const REPUBLISH: Duration = Duration::from_secs(120);
/// Least time between two watch requests, so a hub that answers at once cannot make the
/// watcher spin (the hub allows about 6 a minute per token).
const WATCH_MIN_GAP: Duration = Duration::from_secs(1);
/// After a failed watch, mailboxes are polled adaptively for this long, doubling per failure
/// up to [`WATCH_FALLBACK_MAX`], before the watch is tried again.
const WATCH_FALLBACK_MIN: Duration = Duration::from_secs(30);
const WATCH_FALLBACK_MAX: Duration = Duration::from_secs(600);
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
    /// The hub's `GET /pkarr/watch`, when advertised (core FR-H2, #132).
    watch: Mutex<Option<PkarrWatch>>,
    client: reqwest::Client,
}

/// Decodes standard base64 with padding (the `packet` of `GET /pkarr/watch`).
fn base64_std(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    }
    let b = s.trim().as_bytes();
    if b.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(b.len() / 4 * 3);
    for (i, chunk) in b.chunks(4).enumerate() {
        let last = i == b.len() / 4 - 1;
        let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return None;
        }
        let mut n = 0u32;
        for &c in &chunk[..4 - pad] {
            n = (n << 6) | val(c)?;
        }
        n <<= 6 * pad as u32;
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..3 - pad]);
    }
    Some(out)
}

#[derive(serde::Deserialize)]
struct WatchBody {
    #[serde(default)]
    records: Vec<WatchRecord>,
}

#[derive(serde::Deserialize)]
struct WatchRecord {
    key: String,
    packet: String,
}

#[derive(serde::Deserialize)]
struct NotFoundBody {
    #[serde(default)]
    keys: Vec<String>,
}

impl HttpPkarrStore {
    /// `base` is the hub's pkarr URL (`https://api.darkpyonix.dev/pkarr`), `token` the device's
    /// resolve token.
    pub fn new(base: impl Into<String>, token: Option<String>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| TransportError::Config(format!("http client: {e}")))?;
        let base: String = base.into();
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            token: Mutex::new(token),
            watch: Mutex::new(None),
            client,
        })
    }

    /// Uses the hub's batched long-poll watch (`GET /pkarr/watch`).
    pub fn with_watch(self, watch: Option<PkarrWatch>) -> Self {
        *self.watch.lock().unwrap() = watch;
        self
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

    fn watch_limits(&self) -> Option<WatchLimits> {
        self.watch
            .lock()
            .unwrap()
            .as_ref()
            .map(|w| WatchLimits { max_keys: w.max_keys.max(1), max_wait: w.max_wait })
    }

    fn set_watch(&self, watch: Option<PkarrWatch>) {
        *self.watch.lock().unwrap() = watch;
    }

    /// `GET /pkarr/watch?keys=..&since=..&wait=..` with the device token (core FR-H2).
    async fn watch(&self, keys: &[(PeerId, u64)], wait: Duration) -> Result<WatchOutcome> {
        let Some(w) = self.watch.lock().unwrap().clone() else {
            return Err(TransportError::Stream("the hub advertises no pkarr watch".into()));
        };
        let wait = wait.min(w.max_wait);
        let names: Vec<(String, PeerId)> = keys.iter().map(|(k, _)| (z32(k.as_bytes()), *k)).collect();
        let joined_keys = names.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(",");
        let joined_since = keys.iter().map(|(_, ts)| ts.to_string()).collect::<Vec<_>>().join(",");
        let resp = self
            .client
            .get(format!("{}/watch", self.base))
            .query(&[("keys", joined_keys), ("since", joined_since), ("wait", wait.as_secs().to_string())])
            .bearer_auth(&w.device_token)
            // The hub holds the request for up to `wait`; the client's 10 s default is too short.
            .timeout(wait + Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| TransportError::Stream(format!("pkarr watch: {e}")))?;
        let status = resp.status();
        let lookup = |name: &str| names.iter().find(|(n, _)| n == name).map(|(_, k)| *k);
        match status.as_u16() {
            204 => Ok(WatchOutcome::Unchanged),
            200 => {
                let bytes = resp.bytes().await.map_err(|e| TransportError::Stream(format!("pkarr watch: {e}")))?;
                let body: WatchBody = serde_json::from_slice(&bytes)
                    .map_err(|e| TransportError::Stream(format!("pkarr watch: bad body: {e}")))?;
                let mut out = Vec::new();
                for r in body.records {
                    // A key we did not ask for, or a packet that does not decode, is skipped;
                    // the packet signature is checked by the caller.
                    let (Some(key), Some(packet)) = (lookup(&r.key), base64_std(&r.packet)) else { continue };
                    out.push((key, packet));
                }
                Ok(WatchOutcome::Changed(out))
            }
            404 => {
                let bytes = resp.bytes().await.unwrap_or_default();
                let body: NotFoundBody = serde_json::from_slice(&bytes).unwrap_or(NotFoundBody { keys: Vec::new() });
                Ok(WatchOutcome::NotFound(body.keys.iter().filter_map(|n| lookup(n)).collect()))
            }
            _ => Err(TransportError::Stream(format!("pkarr watch refused: {status}"))),
        }
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
    /// Per watched peer: when it is next read and how fast.
    cadence: HashMap<PeerId, Cadence>,
    /// Bumped when the watched set grows, so a running hub watch is restarted with it.
    watch_gen: u64,
}

/// How often watched mailboxes are read (SPEC `FR-N2`, #783).
#[derive(Clone, Copy, Debug)]
pub(super) struct PollPlan {
    /// The interval while an exchange with the peer is in flight.
    pub fast: Duration,
    /// The longest interval of an idle peer.
    pub idle: Duration,
    /// How long a peer stays fast after its last exchange.
    pub window: Duration,
}

impl PollPlan {
    /// The plan of a [`crate::LetifyConfig`]. `idle` is at least `fast`.
    pub(super) fn new(fast: Duration, idle: Duration, window: Duration) -> Self {
        let fast = fast.max(Duration::from_millis(1));
        Self { fast, idle: idle.max(fast), window }
    }
}

/// The polling state of one watched peer.
#[derive(Clone, Copy, Debug)]
struct Cadence {
    /// When the peer is read next.
    due: tokio::time::Instant,
    /// The interval since the last idle read; doubles up to [`PollPlan::idle`].
    step: Duration,
    /// The peer is fast until this instant.
    hot_until: tokio::time::Instant,
}

impl Cadence {
    fn idle_start(now: tokio::time::Instant, plan: &PollPlan) -> Self {
        Self { due: now + plan.fast, step: plan.fast, hot_until: now }
    }
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
    plan: PollPlan,
    /// Wakes the poller when a peer turns fast.
    kicked: Arc<Notify>,
    /// Restarts the hub watch when the watched set changes.
    rewatch: Arc<Notify>,
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

/// The direct (TCP) addresses in the letify address record published under the `_letify` label,
/// given the record's TXT text (its character-strings joined). `None` when the text is not a
/// valid signed record. A directory view such as the fake hub's reads published addresses
/// with this, without naming the backend library (FR-N5).
pub fn direct_addresses_from_txt(joined: &str) -> Option<Vec<std::net::SocketAddr>> {
    let strings: Vec<&str> = joined
        .as_bytes()
        .chunks(255)
        .map(|c| std::str::from_utf8(c).ok())
        .collect::<Option<_>>()?;
    let rec = AddressRecord::decode_txt(strings.as_slice(), record::unix_now()).ok()?;
    Some(rec.tcp)
}

/// Whether a DNS name is the `_letify` address record label.
pub fn is_record_label(name: &str) -> bool {
    first_label(name) == RECORD_LABEL
}

impl PkarrNode {
    /// Starts publishing and polling. `record` returns the fresh TXT strings of our address
    /// record (empty: publish mailboxes only).
    pub(super) fn start(
        key: SecretKey,
        store: Arc<dyn PkarrStore>,
        record: RecordFn,
        plan: PollPlan,
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
            plan,
            kicked: Arc::new(Notify::new()),
            rewatch: Arc::new(Notify::new()),
        });
        // The tasks hold weak references so dropping the node stops them.
        let publisher = tokio::spawn(publish_loop(Arc::downgrade(&node)));
        let poller = tokio::spawn(poll_loop(Arc::downgrade(&node)));
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
        let now = tokio::time::Instant::now();
        let mut added = false;
        {
            let mut st = self.state.lock().unwrap();
            for p in peers {
                if *p != self.me && st.watched.insert(*p) {
                    st.cadence.entry(*p).or_insert_with(|| Cadence::idle_start(now, &self.plan));
                    added = true;
                }
            }
            if added {
                st.watch_gen += 1;
            }
        }
        if added {
            // The poller recomputes its wake time so the new peer is read on schedule, or
            // restarts the hub watch with it.
            self.kicked.notify_one();
            self.rewatch.notify_one();
        }
    }

    /// An exchange with `peer` is in flight (a dial, a sent or received signal): read its
    /// mailbox now and at the fast interval until the fast window after this call ends.
    pub(super) fn kick(&self, peer: PeerId) {
        if peer == self.me {
            return;
        }
        let now = tokio::time::Instant::now();
        {
            let mut st = self.state.lock().unwrap();
            if st.watched.insert(peer) {
                st.watch_gen += 1;
                self.rewatch.notify_one();
            }
            let c = st.cadence.entry(peer).or_insert_with(|| Cadence::idle_start(now, &self.plan));
            c.hot_until = now + self.plan.window;
            c.step = self.plan.fast;
            c.due = c.due.min(now);
        }
        self.kicked.notify_one();
    }

    /// The watched peers with the timestamp of the last packet read from each (`0`: none), for
    /// the hub watch, and the generation of the watched set.
    fn watch_keys(&self) -> (Vec<(PeerId, u64)>, u64) {
        let st = self.state.lock().unwrap();
        let mut keys: Vec<(PeerId, u64)> =
            st.watched.iter().map(|p| (*p, st.seen.get(p).map_or(0, |(ts, _)| *ts))).collect();
        keys.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        (keys, st.watch_gen)
    }

    /// The watched peers due at `now`, with their next reads scheduled, and the next due time.
    fn take_due(&self, now: tokio::time::Instant) -> (Vec<PeerId>, Option<tokio::time::Instant>) {
        let plan = self.plan;
        let mut st = self.state.lock().unwrap();
        let State { watched, cadence, .. } = &mut *st;
        let mut due = Vec::new();
        let mut next: Option<tokio::time::Instant> = None;
        for p in watched.iter() {
            let c = cadence.entry(*p).or_insert_with(|| Cadence::idle_start(now, &plan));
            if c.due <= now {
                due.push(*p);
                if now < c.hot_until {
                    c.step = plan.fast;
                } else {
                    c.step = (c.step * 2).min(plan.idle);
                }
                c.due = now + c.step;
            }
            next = Some(next.map_or(c.due, |n| n.min(c.due)));
        }
        (due, next)
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
        self.take_packet(from, &payload)
    }

    /// Verifies a packet of `from` (from `GET /pkarr/{key}` or the hub watch) and queues the
    /// messages of its mailbox for us not seen before.
    fn take_packet(&self, from: PeerId, payload: &[u8]) -> Result<()> {
        let (ts, dns) = verify_pkarr_payload(&from, payload)?;
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
        if !fresh.is_empty() {
            self.kick(from);
        }
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

/// What the poller does next.
enum WatchStep {
    /// Back to the top of the loop (the watch ran, or the watched set changed).
    Again,
    /// The store has no watch, or it failed: poll adaptively once.
    Poll,
}

/// The watch state that lives across loop turns.
struct WatchState {
    /// Until then, mailboxes are polled (a watch failed).
    fallback_until: Option<tokio::time::Instant>,
    fallback_step: Duration,
    /// Keys the hub answered `404` for, until the watched set changes.
    not_found: HashSet<PeerId>,
    not_found_gen: u64,
}

/// One turn of the hub watch (SPEC `FR-N2`, core FR-H2 #132): one long-poll request per
/// `max_keys` watched peers instead of a `GET` per peer. A changed packet is delivered when the
/// first request returns; then all are restarted (the old ones are dropped, which the hub allows:
/// a new watch runs beside the old). A change of the watched set restarts the watch too.
async fn watch_turn(weak: &std::sync::Weak<PkarrNode>, ws: &mut WatchState) -> Option<WatchStep> {
    let node = weak.upgrade()?;
    let Some(limits) = node.store.watch_limits() else { return Some(WatchStep::Poll) };
    let now = tokio::time::Instant::now();
    if ws.fallback_until.is_some_and(|t| now < t) {
        return Some(WatchStep::Poll);
    }
    let store = node.store.clone();
    let rewatch = node.rewatch.clone();
    // Registered before the set is read, so a change in between is not missed.
    let changed = rewatch.notified();
    tokio::pin!(changed);
    changed.as_mut().enable();
    let (keys, gen) = node.watch_keys();
    // No strong reference is held while the hub holds the request.
    drop(node);
    if gen != ws.not_found_gen {
        ws.not_found.clear();
        ws.not_found_gen = gen;
    }
    let keys: Vec<(PeerId, u64)> = keys.into_iter().filter(|(k, _)| !ws.not_found.contains(k)).collect();
    if keys.is_empty() {
        let _ = tokio::time::timeout(limits.max_wait, changed).await;
        return Some(WatchStep::Again);
    }
    let wait = limits.max_wait;
    let calls: Vec<_> = keys
        .chunks(limits.max_keys.max(1))
        .map(|chunk| {
            let store = store.clone();
            let chunk = chunk.to_vec();
            Box::pin(async move { store.watch(&chunk, wait).await })
        })
        .collect();
    let answer = tokio::select! {
        _ = &mut changed => return Some(WatchStep::Again),
        (answer, _, _) = futures::future::select_all(calls) => answer,
    };
    let fail = |ws: &mut WatchState, why: String| {
        tracing::debug!(error = %why, "the hub pkarr watch failed; polling mailboxes for a while");
        ws.fallback_until = Some(tokio::time::Instant::now() + ws.fallback_step);
        ws.fallback_step = (ws.fallback_step * 2).min(WATCH_FALLBACK_MAX);
    };
    match answer {
        Ok(WatchOutcome::Changed(records)) => {
            ws.fallback_step = WATCH_FALLBACK_MIN;
            let node = weak.upgrade()?;
            for (peer, packet) in records {
                if let Err(e) = node.take_packet(peer, &packet) {
                    tracing::debug!(peer = %peer.fmt_short(), error = %e, "a watched pkarr packet was refused");
                }
            }
        }
        Ok(WatchOutcome::Unchanged) => ws.fallback_step = WATCH_FALLBACK_MIN,
        Ok(WatchOutcome::NotFound(gone)) if !gone.is_empty() => ws.not_found.extend(gone),
        Ok(WatchOutcome::NotFound(_)) => {
            fail(ws, "404 without keys".into());
            return Some(WatchStep::Poll);
        }
        Err(e) => {
            fail(ws, e.to_string());
            return Some(WatchStep::Poll);
        }
    }
    tokio::time::sleep_until(now + WATCH_MIN_GAP).await;
    Some(WatchStep::Again)
}

async fn poll_loop(weak: std::sync::Weak<PkarrNode>) {
    let mut wake = {
        let Some(node) = weak.upgrade() else { return };
        tokio::time::Instant::now() + node.plan.fast
    };
    let mut ws = WatchState {
        fallback_until: None,
        fallback_step: WATCH_FALLBACK_MIN,
        not_found: HashSet::new(),
        not_found_gen: 0,
    };
    loop {
        match watch_turn(&weak, &mut ws).await {
            None => return,
            Some(WatchStep::Again) => continue,
            Some(WatchStep::Poll) => {}
        }
        let Some((kicked, idle)) = weak.upgrade().map(|n| (n.kicked.clone(), n.plan.idle)) else { return };
        // A kick ends the wait at once. No strong reference is held while waiting, and the
        // wait is capped by the idle interval so a peer watched later is picked up (and, after
        // a failed hub watch, so the watch is tried again on time).
        let mut cap = tokio::time::Instant::now() + idle;
        if let Some(t) = ws.fallback_until {
            cap = cap.min(t.max(tokio::time::Instant::now()));
        }
        let _ = tokio::time::timeout_at(wake.min(cap), kicked.notified()).await;
        let Some(node) = weak.upgrade() else { return };
        let now = tokio::time::Instant::now();
        let (peers, next) = node.take_due(now);
        wake = next.unwrap_or(now + node.plan.idle).max(now + Duration::from_millis(1));
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
            st.seq = (st.seq + 1).max(unix_secs());
            let seq = st.seq;
            let queue = st.outbox.entry(to).or_default();
            queue.push_back((seq, msg));
            while queue.iter().map(|(_, m)| m.len() + 6).sum::<usize>() > MAILBOX_BYTES {
                queue.pop_front();
            }
        }
        self.kick(to);
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

    /// A plan that is always fast (the tests that check delivery, not cadence).
    fn fast(ms: u64) -> PollPlan {
        let d = Duration::from_millis(ms);
        PollPlan::new(d, d, Duration::ZERO)
    }

    /// Counts `GET`s per key on top of a [`crate::MemoryPkarrStore`].
    #[derive(Clone, Default)]
    struct CountingStore {
        inner: crate::MemoryPkarrStore,
        gets: Arc<Mutex<HashMap<PeerId, usize>>>,
    }

    impl CountingStore {
        fn gets(&self, key: &PeerId) -> usize {
            self.gets.lock().unwrap().get(key).copied().unwrap_or(0)
        }
    }

    #[async_trait]
    impl PkarrStore for CountingStore {
        async fn put(&self, key: PeerId, payload: Vec<u8>) -> Result<()> {
            self.inner.put(key, payload).await
        }
        async fn get(&self, key: PeerId) -> Result<Option<Vec<u8>>> {
            *self.gets.lock().unwrap().entry(key).or_default() += 1;
            self.inner.get(key).await
        }
    }

    /// The default plan: 1.5 s fast, 25 s idle, 30 s window.
    fn default_plan() -> PollPlan {
        let c = crate::LetifyConfig::default();
        PollPlan::new(c.poll_interval, c.idle_poll_interval, c.fast_window)
    }

    fn watcher(store: &CountingStore) -> Arc<PkarrNode> {
        PkarrNode::start(SecretKey::generate(), Arc::new(store.clone()), Arc::new(Vec::<String>::new), default_plan())
    }

    async fn gets_during(store: &CountingStore, peer: &PeerId, span: Duration) -> usize {
        let before = store.gets(peer);
        tokio::time::sleep(span).await;
        store.gets(peer) - before
    }

    #[tokio::test(start_paused = true)]
    async fn fr_n2_an_idle_pair_is_read_about_once_per_idle_interval() {
        let store = CountingStore::default();
        let node = watcher(&store);
        let peer = SecretKey::generate().peer_id();
        node.watch_peers(&[peer]);
        // The backoff 1.5, 3, 6, 12, 24 s ends at the 25 s idle interval within a minute.
        tokio::time::sleep(Duration::from_secs(60)).await;
        let hour = gets_during(&store, &peer, Duration::from_secs(3600)).await;
        assert!((143..=145).contains(&hour), "an idle pair made {hour} reads in an hour (expected 144)");
    }

    #[tokio::test(start_paused = true)]
    async fn fr_n2_a_dial_reads_the_peer_at_the_fast_interval_at_once() {
        let store = CountingStore::default();
        let node = watcher(&store);
        let peer = SecretKey::generate().peer_id();
        node.watch_peers(&[peer]);
        tokio::time::sleep(Duration::from_secs(600)).await;
        let before = store.gets(&peer);
        node.kick(peer);
        // The first read follows the kick without waiting for the idle interval.
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(store.gets(&peer), before + 1, "a kick reads the mailbox at once");
        let window = gets_during(&store, &peer, Duration::from_secs(29)).await;
        assert!((18..=21).contains(&window), "{window} reads in a 29 s fast window (expected about 19)");
    }

    #[tokio::test(start_paused = true)]
    async fn fr_n2_a_sent_signal_turns_the_peer_fast() {
        let store = CountingStore::default();
        let node = watcher(&store);
        let peer = SecretKey::generate().peer_id();
        node.watch_peers(&[peer]);
        tokio::time::sleep(Duration::from_secs(600)).await;
        node.send(peer, b"offer".to_vec()).await.unwrap();
        let window = gets_during(&store, &peer, Duration::from_secs(15)).await;
        assert!(window >= 9, "{window} reads in 15 s after a sent signal (expected about 10)");
    }

    #[tokio::test(start_paused = true)]
    async fn fr_n2_the_peer_returns_to_idle_after_the_fast_window() {
        let store = CountingStore::default();
        let node = watcher(&store);
        let peer = SecretKey::generate().peer_id();
        node.watch_peers(&[peer]);
        node.kick(peer);
        // The window (30 s) and the backoff ramp (about 47 s) are over after two minutes.
        tokio::time::sleep(Duration::from_secs(120)).await;
        let later = gets_during(&store, &peer, Duration::from_secs(600)).await;
        assert!((23..=25).contains(&later), "{later} reads in 10 idle minutes (expected 24)");
    }

    /// A store with the hub's batched long-poll watch (core FR-H2): `watch` returns the packets
    /// newer than `since`, or waits up to `wait` for one to be published. Counts calls.
    #[derive(Clone)]
    struct LongPollStore {
        inner: crate::MemoryPkarrStore,
        limits: Option<WatchLimits>,
        published: Arc<Notify>,
        /// The keys of each watch call, in order.
        calls: Arc<Mutex<Vec<Vec<PeerId>>>>,
        gets: Arc<Mutex<usize>>,
        /// Keys answered with `404` (not live devices of the account).
        unknown: Arc<Mutex<HashSet<PeerId>>>,
        /// Every watch fails (a hub error such as `429`).
        failing: Arc<AtomicBool>,
    }

    impl fmt::Debug for LongPollStore {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("LongPollStore")
        }
    }

    impl LongPollStore {
        fn new(max_keys: Option<usize>) -> Self {
            Self {
                inner: crate::MemoryPkarrStore::new(),
                limits: max_keys.map(|max_keys| WatchLimits { max_keys, max_wait: Duration::from_secs(25) }),
                published: Arc::new(Notify::new()),
                calls: Arc::default(),
                gets: Arc::default(),
                unknown: Arc::default(),
                failing: Arc::default(),
            }
        }
        fn calls(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
        fn last_call(&self) -> Vec<PeerId> {
            self.calls.lock().unwrap().last().cloned().unwrap_or_default()
        }
        fn gets(&self) -> usize {
            *self.gets.lock().unwrap()
        }
    }

    #[async_trait]
    impl PkarrStore for LongPollStore {
        async fn put(&self, key: PeerId, payload: Vec<u8>) -> Result<()> {
            self.inner.put(key, payload).await?;
            self.published.notify_waiters();
            Ok(())
        }
        async fn get(&self, key: PeerId) -> Result<Option<Vec<u8>>> {
            *self.gets.lock().unwrap() += 1;
            self.inner.get(key).await
        }
        fn watch_limits(&self) -> Option<WatchLimits> {
            self.limits
        }
        async fn watch(&self, keys: &[(PeerId, u64)], wait: Duration) -> Result<WatchOutcome> {
            let limits = self.limits.expect("watch called on a store without one");
            assert!(keys.len() <= limits.max_keys, "{} keys in one watch (max {})", keys.len(), limits.max_keys);
            self.calls.lock().unwrap().push(keys.iter().map(|(k, _)| *k).collect());
            if self.failing.load(Ordering::Relaxed) {
                return Err(TransportError::Stream("pkarr watch refused: 429".into()));
            }
            let gone: Vec<PeerId> = {
                let unknown = self.unknown.lock().unwrap();
                keys.iter().map(|(k, _)| *k).filter(|k| unknown.contains(k)).collect()
            };
            if !gone.is_empty() {
                return Ok(WatchOutcome::NotFound(gone));
            }
            let deadline = tokio::time::Instant::now() + wait;
            loop {
                let notified = self.published.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let mut changed = Vec::new();
                for (k, since) in keys {
                    if let Some(p) = self.inner.get(*k).await? {
                        let ts = u64::from_be_bytes(p[64..72].try_into().unwrap());
                        if ts > *since {
                            changed.push((*k, p));
                        }
                    }
                }
                if !changed.is_empty() {
                    return Ok(WatchOutcome::Changed(changed));
                }
                if tokio::time::timeout_at(deadline, notified).await.is_err() {
                    return Ok(WatchOutcome::Unchanged);
                }
            }
        }
    }

    fn start_on(store: &LongPollStore, key: SecretKey) -> Arc<PkarrNode> {
        PkarrNode::start(key, Arc::new(store.clone()), Arc::new(Vec::<String>::new), default_plan())
    }

    #[tokio::test(start_paused = true)]
    async fn fr_n2_one_watch_per_wait_covers_every_watched_peer() {
        // max_keys 2 and five peers: three requests per round, one round per 25 s wait.
        let store = LongPollStore::new(Some(2));
        let node = start_on(&store, SecretKey::generate());
        let peers: Vec<PeerId> = (0..5).map(|_| SecretKey::generate().peer_id()).collect();
        node.watch_peers(&peers);
        tokio::time::sleep(Duration::from_secs(5)).await;
        let before = store.calls();
        tokio::time::sleep(Duration::from_secs(600)).await;
        let rounds = (store.calls() - before) as f64 / 3.0;
        // A round ends when its first request returns; all three end at the same 25 s.
        assert!((22.0..=26.0).contains(&rounds), "{rounds} watch rounds in 10 minutes (expected 24)");
        assert_eq!(store.gets(), 0, "no per-peer GET while the hub watch works");
        let mut seen: Vec<PeerId> = store.calls.lock().unwrap().iter().rev().take(3).flatten().copied().collect();
        seen.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        let mut want = peers.clone();
        want.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        assert_eq!(seen, want, "one round covers every watched peer once");
    }

    #[tokio::test(start_paused = true)]
    async fn fr_n2_a_changed_record_is_delivered_at_once_through_the_watch() {
        let store = LongPollStore::new(Some(32));
        let (ka, kb) = (SecretKey::generate(), SecretKey::generate());
        let a = start_on(&store, ka.clone());
        let b = start_on(&store, kb.clone());
        a.watch_peers(&[kb.peer_id()]);
        // Let the watch settle into its long wait.
        tokio::time::sleep(Duration::from_secs(60)).await;
        let sent = tokio::time::Instant::now();
        b.send(ka.peer_id(), b"offer".to_vec()).await.unwrap();
        let (from, msg) = tokio::time::timeout(Duration::from_secs(25), a.recv()).await.unwrap().unwrap();
        assert_eq!((from, msg.as_slice()), (kb.peer_id(), &b"offer"[..]));
        let took = sent.elapsed();
        assert!(took < Duration::from_secs(1), "delivered after {took:?}, not within the watch's wait");
        assert_eq!(store.gets(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn fr_n2_keys_the_hub_does_not_know_are_dropped_from_the_watch() {
        let store = LongPollStore::new(Some(32));
        let (ka, kb) = (SecretKey::generate(), SecretKey::generate());
        let stranger = SecretKey::generate().peer_id();
        store.unknown.lock().unwrap().insert(stranger);
        let a = start_on(&store, ka.clone());
        let b = start_on(&store, kb.clone());
        a.watch_peers(&[kb.peer_id(), stranger]);
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert_eq!(store.last_call(), vec![kb.peer_id()], "the rest is watched without the 404 key");
        let calls = store.calls();
        assert!(calls <= 6, "{calls} watch calls in a minute: the 404 key must not be retried each turn");
        b.send(ka.peer_id(), b"still".to_vec()).await.unwrap();
        let (from, _) = tokio::time::timeout(Duration::from_secs(2), a.recv()).await.unwrap().unwrap();
        assert_eq!(from, kb.peer_id());
    }

    #[tokio::test(start_paused = true)]
    async fn fr_n2_without_an_advertised_watch_mailboxes_are_polled() {
        let store = LongPollStore::new(None);
        let node = start_on(&store, SecretKey::generate());
        node.watch_peers(&[SecretKey::generate().peer_id()]);
        tokio::time::sleep(Duration::from_secs(120)).await;
        assert_eq!(store.calls(), 0);
        assert!(store.gets() > 0, "the adaptive GET polling runs");
    }

    #[tokio::test(start_paused = true)]
    async fn fr_n2_a_failing_watch_falls_back_to_polling_and_retries() {
        let store = LongPollStore::new(Some(32));
        store.failing.store(true, Ordering::Relaxed);
        let node = start_on(&store, SecretKey::generate());
        node.watch_peers(&[SecretKey::generate().peer_id()]);
        tokio::time::sleep(Duration::from_secs(20)).await;
        assert_eq!(store.calls(), 1, "a failed watch is not retried at once");
        assert!(store.gets() > 0, "mailboxes are polled while the watch is down");
        store.failing.store(false, Ordering::Relaxed);
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert!(store.calls() >= 2, "the watch is tried again after the fallback");
        let gets = store.gets();
        tokio::time::sleep(Duration::from_secs(120)).await;
        assert_eq!(store.gets(), gets, "the watch took over again");
    }

    #[test]
    fn base64_std_decodes_padded_input() {
        assert_eq!(base64_std("aGVsbG8="), Some(b"hello".to_vec()));
        assert_eq!(base64_std("aGk="), Some(b"hi".to_vec()));
        assert_eq!(base64_std("YWJj"), Some(b"abc".to_vec()));
        assert_eq!(base64_std("+/8="), Some(vec![0xfb, 0xff]));
        assert_eq!(base64_std("aGk"), None);
        assert_eq!(base64_std("a=Gk"), None);
    }

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
        let a = PkarrNode::start(ka.clone(), Arc::new(store.clone()), none.clone(), fast(50));
        let b = PkarrNode::start(kb.clone(), Arc::new(store.clone()), none, fast(50));
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
        let rec_joined = txt.concat();
        let _b = PkarrNode::start(kb.clone(), Arc::new(store.clone()), Arc::new(move || txt.clone()), fast(60_000));
        let a = PkarrNode::start(ka, Arc::new(store.clone()), Arc::new(|| Vec::<String>::new()), fast(60_000));
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
        // The directory view decodes the same record from its joined TXT text.
        let joined = rec_joined.clone();
        assert_eq!(direct_addresses_from_txt(&joined), Some(vec!["127.0.0.1:4242".parse().unwrap()]));
        assert_eq!(direct_addresses_from_txt("not-a-record"), None);
        assert!(is_record_label("_letify.abc"));
        // Nothing is published for an unknown key.
        assert!(a.lookup(SecretKey::generate().peer_id()).await.unwrap().is_none());
    }
}
