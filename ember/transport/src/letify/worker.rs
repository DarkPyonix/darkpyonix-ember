//! letify's signalling Worker (`letify.darkpyonix.dev`) as an auxiliary rendezvous for punching
//! (EM-11, #10, SPEC `FR-N2`; owner 2026-11-23: "써도 된다고").
//!
//! The client is letify-link's own `signal::SignalRendezvous` (3.0.7); this module owns only
//! Ember's use of it:
//!
//! - **Short waits only.** The Worker carries the messages of one punch. The dialing side's
//!   requests end with its punch; the answering side long-polls only after a wake-up, and for at
//!   most [`ANSWER_POLL`]. Nothing waits on the Worker while idle, so the free plan is not spent
//!   on standing waits and no `/ws` is needed.
//! - **The wake-up.** The answering side learns that a peer punches through the Worker from a
//!   small message on Ember's own rendezvous, which in production is the hub's pkarr mailbox
//!   (about 200 bytes per recipient). It is sent only after the Worker accepted the first offer,
//!   so a Worker that refuses wakes nobody. Format: [`WAKE_KIND`], [`WAKE_PUNCH`], then a 16
//!   byte random nonce (18 bytes). The kind byte is outside letify-link's rendezvous kinds
//!   (1 to 8), so an older peer that decodes it as a rendezvous message drops it.
//! - **Failures.** A refused request (401, 403, 413), a rate limit (429) or no answer fails the
//!   Worker exchange and is recorded; the dial then punches over Ember's own rendezvous, and the
//!   relay stays the last resort. A 429 keeps the Worker unused for its Retry-After.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letify_link::rendezvous::Rendezvous;
use letify_link::NodeId;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// The first byte of a wake-up: Ember's own kind, outside letify-link's rendezvous kinds.
pub(crate) const WAKE_KIND: u8 = 0xEB;
/// The second byte: "I am punching to you through the Worker", version 1.
pub(crate) const WAKE_PUNCH: u8 = 0x01;
const WAKE_LEN: usize = 18;
/// How long a woken answering side polls the Worker at most: one punch (lead and window) with a
/// margin for the wake-up's own delay.
pub(crate) const ANSWER_POLL: Duration = Duration::from_secs(8);
/// How long one send to the Worker may take before the Worker counts as not answering, so a
/// dial falls back within its own dial timeout.
const SEND_LIMIT: Duration = Duration::from_secs(3);
/// The Worker stays unused this long after a 429 that names no Retry-After.
const RATE_LIMIT_DEFAULT: Duration = Duration::from_secs(60);
/// Wake-up nonces remembered, so a mailbox that delivers one twice starts one poll.
const SEEN: usize = 64;

/// The wake-up message for a punch with `nonce`.
pub(crate) fn wake_up(nonce: [u8; 16]) -> Vec<u8> {
    let mut out = vec![WAKE_KIND, WAKE_PUNCH];
    out.extend(nonce);
    out
}

/// The nonce of a wake-up message, or `None` for anything else.
pub(crate) fn parse_wake_up(bytes: &[u8]) -> Option<[u8; 16]> {
    match bytes {
        [WAKE_KIND, WAKE_PUNCH, nonce @ ..] if bytes.len() == WAKE_LEN => nonce.try_into().ok(),
        _ => None,
    }
}

/// The Retry-After of a 429 as letify-link's client words it (`..., retry in <s> s)`).
pub(crate) fn retry_after(e: &io::Error) -> Option<Duration> {
    let text = e.to_string();
    let rest = &text[text.find("retry in ")? + "retry in ".len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok().map(Duration::from_secs)
}

/// The Worker client with Ember's limits on it.
pub(crate) struct Worker<W> {
    rdv: W,
    /// One Worker exchange at a time: the client's long poll and cursor are shared, so two
    /// punches would take each other's messages.
    busy: Arc<Semaphore>,
    blocked_until: Mutex<Option<Instant>>,
    seen: Mutex<VecDeque<[u8; 16]>>,
}

impl<W: Rendezvous> Worker<W> {
    pub(crate) fn new(rdv: W) -> Self {
        Self { rdv, busy: Arc::new(Semaphore::new(1)), blocked_until: Mutex::new(None), seen: Mutex::new(VecDeque::new()) }
    }

    /// A permit to use the Worker now, or `None` while a 429's Retry-After runs or another
    /// exchange uses it.
    pub(crate) fn ready(&self) -> Option<OwnedSemaphorePermit> {
        if self.blocked_until.lock().unwrap().is_some_and(|t| Instant::now() < t) {
            return None;
        }
        self.busy.clone().try_acquire_owned().ok()
    }

    /// True the first time `nonce` is seen.
    pub(crate) fn first_sight(&self, nonce: [u8; 16]) -> bool {
        let mut seen = self.seen.lock().unwrap();
        if seen.contains(&nonce) {
            return false;
        }
        if seen.len() == SEEN {
            seen.pop_front();
        }
        seen.push_back(nonce);
        true
    }

    /// Logs a failed Worker exchange; a 429 keeps the Worker unused for its Retry-After.
    pub(crate) fn note_failure(&self, e: &io::Error) {
        if e.kind() == io::ErrorKind::WouldBlock {
            let wait = retry_after(e).unwrap_or(RATE_LIMIT_DEFAULT);
            *self.blocked_until.lock().unwrap() = Some(Instant::now() + wait);
            tracing::info!(error = %e, wait_s = wait.as_secs(), "the signalling Worker is rate limited; punching over Ember's own rendezvous until its Retry-After");
        } else {
            tracing::info!(error = %e, kind = ?e.kind(), "the signalling Worker failed; punching over Ember's own rendezvous");
        }
    }

    /// The Worker as the rendezvous of one punch. With `wake`, the first offer the Worker
    /// accepts is followed by the wake-up through that rendezvous.
    pub(crate) fn exchange<'a, R: Rendezvous>(&'a self, wake: Option<(&'a R, NodeId, Vec<u8>)>) -> Exchange<'a, W, R> {
        Exchange { worker: &self.rdv, wake: Mutex::new(wake), failure: Mutex::new(None) }
    }

    #[cfg(test)]
    pub(crate) fn blocked_for(&self) -> Option<Duration> {
        self.blocked_until.lock().unwrap().map(|t| t.saturating_duration_since(Instant::now()))
    }
}

/// One punch's rendezvous over the Worker. It records the Worker's first failure, which tells a
/// failed Worker apart from a punch that failed on the network.
pub(crate) struct Exchange<'a, W, R> {
    worker: &'a W,
    wake: Mutex<Option<(&'a R, NodeId, Vec<u8>)>>,
    failure: Mutex<Option<io::Error>>,
}

impl<W, R> Exchange<'_, W, R> {
    /// The Worker's first failure in this punch, if any.
    pub(crate) fn failure(&self) -> Option<io::Error> {
        self.failure.lock().unwrap().as_ref().map(|e| io::Error::new(e.kind(), e.to_string()))
    }

    fn record(&self, e: io::Error) -> io::Error {
        let mut slot = self.failure.lock().unwrap();
        if slot.is_none() {
            *slot = Some(io::Error::new(e.kind(), e.to_string()));
        }
        e
    }
}

impl<W: Rendezvous, R: Rendezvous> Rendezvous for Exchange<'_, W, R> {
    async fn send(&self, to: NodeId, msg: Vec<u8>) -> io::Result<()> {
        match tokio::time::timeout(SEND_LIMIT, self.worker.send(to, msg)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(self.record(e)),
            Err(_) => return Err(self.record(io::Error::new(io::ErrorKind::TimedOut, "the signalling Worker did not answer in time"))),
        }
        let wake = self.wake.lock().unwrap().take();
        if let Some((rdv, peer, bytes)) = wake {
            rdv.send(peer, bytes).await.map_err(|e| self.record(e))?;
        }
        Ok(())
    }

    async fn recv(&self) -> io::Result<(NodeId, Vec<u8>)> {
        self.worker.recv().await.map_err(|e| self.record(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use letify_link::rendezvous::MemoryHub;
    use letify_link::Identity;

    #[test]
    fn em11_worker_the_wake_up_is_18_bytes_outside_letifys_kinds() {
        let w = wake_up([7; 16]);
        assert_eq!(w.len(), 18);
        assert!(w[0] > 8, "not a letify-link rendezvous kind");
        assert_eq!(parse_wake_up(&w), Some([7; 16]));
        assert_eq!(parse_wake_up(&w[..17]), None);
        assert_eq!(parse_wake_up(&[3, 0]), None);
        assert!(letify_link::rendezvous::Message::decode(&w).is_err(), "an older peer drops it");
    }

    #[test]
    fn em11_worker_the_retry_after_of_a_429_is_read() {
        let e = io::Error::new(io::ErrorKind::WouldBlock, "the signalling service refused the request: rate_limited (HTTP 429, retry in 17 s)");
        assert_eq!(retry_after(&e), Some(Duration::from_secs(17)));
        assert_eq!(retry_after(&io::Error::other("no hint")), None);
    }

    /// EM-11: a 429 keeps the Worker unused for its Retry-After; other failures do not; one
    /// exchange at a time; a wake-up nonce starts one poll.
    #[tokio::test]
    async fn em11_worker_a_429_keeps_the_worker_unused_for_its_retry_after() {
        let hub = MemoryHub::new();
        let worker = Worker::new(hub.endpoint(Identity::generate().node_id()));
        let permit = worker.ready().expect("ready");
        assert!(worker.ready().is_none(), "one exchange at a time");
        drop(permit);
        worker.note_failure(&io::Error::new(io::ErrorKind::PermissionDenied, "403"));
        assert!(worker.ready().is_some(), "a refusal does not block the next try");
        worker.note_failure(&io::Error::new(io::ErrorKind::WouldBlock, "rate_limited (HTTP 429, retry in 30 s)"));
        assert!(worker.ready().is_none());
        assert!(worker.blocked_for().is_some_and(|d| d > Duration::from_secs(28)));
        assert!(worker.first_sight([1; 16]));
        assert!(!worker.first_sight([1; 16]));
    }

    /// EM-11: the wake-up follows the first send the Worker accepted, and a Worker failure is
    /// recorded and sends no wake-up.
    #[tokio::test]
    async fn em11_worker_the_wake_up_follows_the_first_accepted_offer() {
        let hub = MemoryHub::new();
        let (me, peer) = (Identity::generate().node_id(), Identity::generate().node_id());
        let worker = Worker::new(hub.endpoint(me));
        let mailbox_hub = MemoryHub::new();
        let mailbox = mailbox_hub.endpoint(me);
        let peer_mailbox = mailbox_hub.endpoint(peer);
        let _peer_worker = hub.endpoint(peer);
        let ex = worker.exchange(Some((&mailbox, peer, wake_up([9; 16]))));
        ex.send(peer, b"offer".to_vec()).await.unwrap();
        ex.send(peer, b"offer 2".to_vec()).await.unwrap();
        let (from, got) = peer_mailbox.recv().await.unwrap();
        assert_eq!((from, parse_wake_up(&got)), (me, Some([9; 16])));
        assert!(ex.failure().is_none());

        let nobody = Identity::generate().node_id();
        let ex = worker.exchange(Some((&mailbox, peer, wake_up([8; 16]))));
        assert!(ex.send(nobody, b"offer".to_vec()).await.is_err(), "no endpoint for that node");
        assert!(ex.failure().is_some());
        assert!(tokio::time::timeout(Duration::from_millis(100), peer_mailbox.recv()).await.is_err(), "no wake-up after a failure");
    }
}
