//! Hole punching through letify-link's public punch driver (EM-11, #10, SPEC `FR-N1`, `FR-N2`).
//!
//! letify-link 3.0.6 runs a whole punch in `letify_link::driver`: the offer and answer over a
//! caller's `Rendezvous`, the timing, the TCP simultaneous open and the UDP probes, and the QUIC
//! connect of a UDP path. This module is the Ember side of it and owns only what letify leaves to
//! the embedder:
//!
//! - **Routing.** Ember has one rendezvous (`SignalAdapter`), read by the backend's signal loop.
//!   The driver reads its rendezvous itself, so each punch gets a [`PeerRendezvous`]: it sends
//!   through the shared channel, and receives the punch messages the signal loop routes to it.
//! - **Where the path goes.** Ember keeps one session per peer, started by a `PathManager` over
//!   TCP and the relay. The punched path joins that session: on the dialing side the punch is a
//!   path manager candidate (driver `punch` with `Role::Initiator`, then `Punched::vet`), and on
//!   the answering side every punched path goes to the backend's acceptor
//!   (`Punched::accept_with`). `driver::punch_session` is not used: it starts a session of its
//!   own, which would be a second session to the same peer, and its responder drops a path that
//!   joins a live session.
//! - **The relay fallback.** The relay stays in the same path manager (`RelayDialer` candidate).
//!   A punch that fails (`PunchError`) only fails this candidate: the session keeps the relay, or
//!   the path manager dials it again, and the relay is closed once a punched path carries
//!   traffic. A `Usage` error is a bug in the options here and is logged as an error; an
//!   `Infrastructure` error is the normal fallback and is logged at debug level.
//! - **The public address.** The driver classifies a NAT only with three STUN servers. Ember has
//!   one QAD observer (the main server's), so it asks that observer for its public IP and passes
//!   it as `PunchOptions::public_ip` with no observers: each socket is offered on that IP with
//!   its bound port.
//! - **Backoff.** Each offer is a rendezvous message (pkarr requests on the hub, SPEC `FR-N2`
//!   budget), so after a failed punch the next offer to that peer waits.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letify_link::driver::{self, PunchError, PunchErrorKind, PunchOptions, PunchedPath, Role};
use letify_link::observe::AddressObserver;
use letify_link::rendezvous::{Message, Proto, Rendezvous};
use letify_link::{BoxTransport, Candidate, NodeId};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, Semaphore};

/// How long after the offer both sides start, so the answer can come back first. The
/// rendezvous may delay a message by seconds; an answer that arrives after the start still
/// works, because both sides try until the end of the window.
const LEAD: Duration = Duration::from_secs(1);
/// How long both sides try. With the lead and the observation it stays under the backend's dial
/// timeout (8 s by default).
const WINDOW: Duration = Duration::from_secs(5);
/// Sockets per punch: one TCP simultaneous open and one UDP probe socket. Each dial of the
/// candidate yields one path, so more sockets would only be dropped.
const TCP_SOCKETS: usize = 1;
const UDP_SOCKETS: usize = 1;
/// At most this many offers are answered at once.
const MAX_ANSWERING: usize = 4;
/// A responder's route takes the offers of one punch (one TCP and one UDP offer, which leave
/// together) for this long; a later offer, or a second offer of the same protocol, is a new
/// punch and gets a new responder.
const OFFER_GATHER: Duration = Duration::from_secs(1);
/// After a failed punch to a peer the next offer waits this long, doubling per failure.
const BACKOFF_FIRST: Duration = Duration::from_secs(30);
const BACKOFF_MAX: Duration = Duration::from_secs(300);

/// The options of every punch: this side's public IP, no STUN observers, the sockets and the
/// timing above. The same on both sides.
pub(crate) fn options(public_ip: IpAddr) -> PunchOptions {
    PunchOptions {
        tcp_sockets: TCP_SOCKETS,
        udp_sockets: UDP_SOCKETS,
        public_ip,
        observers: None,
        police: None,
        lead: LEAD,
        window: WINDOW,
        ..PunchOptions::default()
    }
}

/// Whether `msg` is a punch offer, which starts a responder.
pub(crate) fn is_offer(msg: &Message) -> bool {
    matches!(msg, Message::PunchOffer(_) | Message::PunchOfferExt(..))
}

/// The protocol of a punch offer.
fn offer_proto(msg: &Message) -> Option<Proto> {
    match msg {
        Message::PunchOffer(p) | Message::PunchOfferExt(p, _) => Some(p.proto),
        _ => None,
    }
}

fn is_answer(msg: &Message) -> bool {
    matches!(msg, Message::PunchAnswer(_) | Message::PunchAnswerExt(..))
}

/// A failed punch as an I/O error for the path manager, logged with its kind and message.
fn punch_failed(peer: NodeId, e: PunchError) -> io::Error {
    match e.kind() {
        PunchErrorKind::Usage => {
            tracing::error!(peer = %peer, kind = ?e.kind(), reason = e.message(), "a punch was refused as a usage error, which is a bug in the backend's punch options");
            io::Error::new(io::ErrorKind::InvalidInput, e.message().to_string())
        }
        PunchErrorKind::Infrastructure => {
            tracing::debug!(peer = %peer, kind = ?e.kind(), reason = e.message(), "the punch failed; the session stays on TCP or the relay");
            io::Error::new(io::ErrorKind::TimedOut, e.message().to_string())
        }
    }
}

/// Where the punch messages of one peer go while a punch with it runs.
struct Route {
    id: u64,
    tx: mpsc::UnboundedSender<(NodeId, Vec<u8>)>,
    /// True for a responder, which takes offers; an initiator takes answers.
    responder: bool,
    opened: Instant,
    /// Responder: the offers taken so far, TCP and UDP.
    seen_tcp: bool,
    seen_udp: bool,
}

impl Route {
    /// Takes an offer of `proto` when this responder still gathers the offers of its punch.
    fn takes_offer(&mut self, proto: Proto) -> bool {
        let seen = match proto {
            Proto::Tcp => &mut self.seen_tcp,
            Proto::Udp => &mut self.seen_udp,
        };
        if !self.responder || *seen || self.opened.elapsed() >= OFFER_GATHER {
            return false;
        }
        *seen = true;
        true
    }
}

type Routes = Arc<Mutex<HashMap<NodeId, Route>>>;

/// One punch's view of the shared rendezvous: it sends through the shared channel and receives
/// the punch messages routed to it. Dropping it ends the route.
pub(crate) struct PeerRendezvous<R> {
    shared: Arc<R>,
    inbox: tokio::sync::Mutex<mpsc::UnboundedReceiver<(NodeId, Vec<u8>)>>,
    routes: Routes,
    key: NodeId,
    id: u64,
}

impl<R> Drop for PeerRendezvous<R> {
    fn drop(&mut self) {
        let mut routes = self.routes.lock().unwrap();
        if routes.get(&self.key).is_some_and(|r| r.id == self.id) {
            routes.remove(&self.key);
        }
    }
}

impl<R: Rendezvous> Rendezvous for PeerRendezvous<R> {
    async fn send(&self, to: NodeId, msg: Vec<u8>) -> io::Result<()> {
        self.shared.send(to, msg).await
    }

    async fn recv(&self) -> io::Result<(NodeId, Vec<u8>)> {
        self.inbox
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "the punch route was replaced"))
    }
}

/// Failed punches to one peer in a row, and when the next offer may go.
struct Backoff {
    failures: u32,
    until: Instant,
}

/// Runs punches with letify-link's driver over the shared rendezvous `R`.
pub(crate) struct Puncher<R> {
    shared: Arc<R>,
    /// Asked in turn for this side's public IP.
    observers: Vec<Arc<dyn AddressObserver>>,
    routes: Routes,
    next_id: AtomicU64,
    backoff: Mutex<HashMap<NodeId, Backoff>>,
    answering: Arc<Semaphore>,
}

impl<R: Rendezvous + 'static> Puncher<R> {
    /// Punches over `shared`, learning this side's public IP from `observers`.
    pub(crate) fn new(shared: Arc<R>, observers: Vec<Arc<dyn AddressObserver>>) -> Arc<Self> {
        Arc::new(Self {
            shared,
            observers,
            routes: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            backoff: Mutex::new(HashMap::new()),
            answering: Arc::new(Semaphore::new(MAX_ANSWERING)),
        })
    }

    /// The dialing side: a candidate that punches a path to `peer` on every dial and hands the
    /// first usable punched path (TCP first, then UDP) to the path manager.
    pub(crate) fn candidate(self: &Arc<Self>, peer: NodeId) -> Candidate {
        let puncher = self.clone();
        Candidate::new(format!("punch:{peer}"), move || {
            let puncher = puncher.clone();
            async move {
                let out = puncher.initiate(peer).await;
                puncher.note(peer, out.as_ref().err());
                out
            }
        })
    }

    /// Hands a punch message from `from` to the punch that waits for it. False when none
    /// does: then an offer starts a responder ([`Puncher::answer`]) and anything else is dropped.
    pub(crate) fn deliver(&self, from: NodeId, msg: &Message) -> bool {
        let mut routes = self.routes.lock().unwrap();
        let Some(route) = routes.get_mut(&from.canonical()) else { return false };
        let takes = match offer_proto(msg) {
            Some(proto) => route.takes_offer(proto),
            None => is_answer(msg) && !route.responder,
        };
        takes && route.tx.send((from, msg.encode())).is_ok()
    }

    /// The answering side: answers the punch that `offer` starts and hands every punched path to
    /// `handler`. The caller checks that it admits `from` first; `handler` gives the path to the
    /// acceptor, which authenticates the peer. Does nothing while this side punches to `from`
    /// itself, or while [`MAX_ANSWERING`] punches are answered.
    pub(crate) fn answer<F, Fut>(self: &Arc<Self>, from: NodeId, offer: &Message, handler: F)
    where
        F: Fn(PunchedPath) -> Fut + Clone + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let Ok(permit) = self.answering.clone().try_acquire_owned() else {
            tracing::debug!(peer = %from, "too many punches in progress; ignoring an offer");
            return;
        };
        let Some(rdv) = self.register(from, true) else {
            tracing::debug!(peer = %from, "a punch to this peer is running; ignoring its offer");
            return;
        };
        if let (Some(r), Some(proto)) = (self.routes.lock().unwrap().get_mut(&from.canonical()), offer_proto(offer)) {
            if r.takes_offer(proto) {
                let _ = r.tx.send((from, offer.encode()));
            }
        }
        let puncher = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let public_ip = match puncher.public_ip().await {
                Ok(ip) => ip,
                Err(e) => {
                    tracing::debug!(peer = %from, error = %e, "no public address to answer a punch with");
                    return;
                }
            };
            let punched = match driver::punch(&rdv, from, Role::Responder, &options(public_ip)).await {
                Ok(p) => p,
                Err(e) => {
                    let _ = punch_failed(from, e);
                    return;
                }
            };
            drop(rdv);
            if let Err(e) = punched.accept_with(handler) {
                let _ = punch_failed(from, e);
            }
        });
    }

    async fn initiate(&self, peer: NodeId) -> io::Result<BoxTransport> {
        self.check_backoff(peer)?;
        let public_ip = self.public_ip().await?;
        let rdv = self
            .register(peer, false)
            .ok_or_else(|| io::Error::new(io::ErrorKind::WouldBlock, "a punch with this peer is already running"))?;
        let punched = driver::punch(&rdv, peer, Role::Initiator, &options(public_ip)).await.map_err(|e| punch_failed(peer, e))?;
        drop(rdv);
        let vetting = punched.vet("punch").await.map_err(|e| punch_failed(peer, e))?;
        let report = vetting.report();
        let dropped = vetting.dropped_message();
        let Some((name, path)) = vetting.usable.into_iter().next() else {
            tracing::debug!(peer = %peer, %report, "no punched path was usable; the session stays on TCP or the relay");
            return Err(io::Error::new(io::ErrorKind::TimedOut, dropped));
        };
        tracing::debug!(peer = %peer, %name, udp = path.is_udp(), %report, "punched a direct path");
        Ok(path.into_transport())
    }

    /// A route for the punch messages of `peer`, or `None` when another punch holds it. A new
    /// responder replaces an earlier one (its offers are all in, or this is a new punch); an
    /// initiator's route is never replaced.
    fn register(&self, peer: NodeId, responder: bool) -> Option<PeerRendezvous<R>> {
        let key = peer.canonical();
        let mut routes = self.routes.lock().unwrap();
        if routes.get(&key).is_some_and(|r| !(responder && r.responder)) {
            return None;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        routes.insert(key, Route { id, tx, responder, opened: Instant::now(), seen_tcp: false, seen_udp: false });
        Some(PeerRendezvous {
            shared: self.shared.clone(),
            inbox: tokio::sync::Mutex::new(rx),
            routes: self.routes.clone(),
            key,
            id,
        })
    }

    /// This side's public IP, from the first observer that answers. No answer, no punch: a host
    /// address alone never starts one.
    async fn public_ip(&self) -> io::Result<IpAddr> {
        // The driver binds IPv4 sockets, so the IP is observed from one.
        let socket = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))).await?;
        for observer in &self.observers {
            if let Ok(m) = observer.observe(&socket).await {
                return Ok(m.mapped.ip());
            }
        }
        Err(io::Error::new(io::ErrorKind::NotConnected, "no public address was observed"))
    }

    fn check_backoff(&self, peer: NodeId) -> io::Result<()> {
        match self.backoff.lock().unwrap().get(&peer.canonical()) {
            Some(b) if Instant::now() < b.until => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "an earlier punch to this peer failed; waiting before the next offer",
            )),
            _ => Ok(()),
        }
    }

    /// Records the outcome of one dial: a success clears the backoff, a failure extends it. A
    /// dial that did not run (backoff, another punch running) changes nothing.
    fn note(&self, peer: NodeId, failure: Option<&io::Error>) {
        let mut backoff = self.backoff.lock().unwrap();
        let key = peer.canonical();
        let Some(e) = failure else {
            backoff.remove(&key);
            return;
        };
        if e.kind() == io::ErrorKind::WouldBlock {
            return;
        }
        let now = Instant::now();
        let b = backoff.entry(key).or_insert(Backoff { failures: 0, until: now });
        let wait = BACKOFF_FIRST.saturating_mul(1 << b.failures.min(8)).min(BACKOFF_MAX);
        b.failures = b.failures.saturating_add(1);
        b.until = now + wait;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use letify_link::rendezvous::{MemoryHub, Punch};
    use letify_link::Identity;

    fn punch(proto: Proto) -> Punch {
        Punch { proto, token: [1; 16], start_at_ms: 1, until_ms: 2, mappings: vec!["127.0.0.1:9".parse().unwrap()] }
    }

    fn puncher(me: NodeId) -> Arc<Puncher<letify_link::rendezvous::MemoryRendezvous>> {
        Puncher::new(Arc::new(MemoryHub::new().endpoint(me)), Vec::new())
    }

    #[test]
    fn em11_only_punch_offers_start_a_responder() {
        assert!(is_offer(&Message::PunchOffer(punch(Proto::Udp))));
        assert!(is_offer(&Message::PunchOffer(punch(Proto::Tcp))));
        assert!(!is_offer(&Message::PunchAnswer(punch(Proto::Udp))));
        assert!(!is_offer(&Message::Record(Vec::new())));
    }

    /// EM-11: an answer reaches only the punch waiting on its peer, an offer never reaches an
    /// initiator, and a failed punch backs off until a success clears it.
    #[tokio::test]
    async fn em11_an_answer_reaches_only_the_punch_of_its_peer_and_a_failed_punch_backs_off() {
        let ids = || Identity::generate().node_id();
        let (me, peer, other) = (ids(), ids(), ids());
        let p = puncher(me);
        let rdv = p.register(peer, false).expect("a route for the dial");
        assert!(p.register(peer, false).is_none(), "one punch per peer");
        assert!(!p.deliver(other, &Message::PunchAnswer(punch(Proto::Udp))), "a stranger's answer");
        assert!(!p.deliver(peer, &Message::PunchOffer(punch(Proto::Udp))), "an offer to an initiator");
        assert!(p.deliver(peer, &Message::PunchAnswer(punch(Proto::Udp))));
        let (from, bytes) = rdv.recv().await.unwrap();
        assert_eq!(from, peer);
        assert_eq!(Message::decode(&bytes).unwrap(), Message::PunchAnswer(punch(Proto::Udp)));
        drop(rdv);
        assert!(!p.deliver(peer, &Message::PunchAnswer(punch(Proto::Udp))), "the route ends with the punch");

        // A responder takes one offer of each protocol; a repeated one is a new punch, whose
        // responder replaces the old route, and the old one ending leaves the new route alone.
        let first = p.register(other, true).expect("a responder route");
        assert!(p.deliver(other, &Message::PunchOffer(punch(Proto::Tcp))));
        assert!(p.deliver(other, &Message::PunchOffer(punch(Proto::Udp))));
        assert!(!p.deliver(other, &Message::PunchOffer(punch(Proto::Tcp))), "a second TCP offer is a new punch");
        assert!(!p.deliver(other, &Message::PunchAnswer(punch(Proto::Tcp))), "a responder takes no answer");
        let second = p.register(other, true).expect("a new responder replaces the old");
        assert!(p.register(other, false).is_none(), "a dial waits for the responder");
        drop(first);
        assert!(p.deliver(other, &Message::PunchOffer(punch(Proto::Tcp))), "the new route is still there");
        drop(second);

        assert!(p.check_backoff(peer).is_ok());
        p.note(peer, Some(&io::Error::from(io::ErrorKind::TimedOut)));
        assert_eq!(p.check_backoff(peer).unwrap_err().kind(), io::ErrorKind::WouldBlock);
        p.note(peer, None);
        assert!(p.check_backoff(peer).is_ok());
        p.note(peer, Some(&io::Error::from(io::ErrorKind::WouldBlock)));
        assert!(p.check_backoff(peer).is_ok(), "a dial that did not run does not back off");
    }

    /// EM-11: the options punch with TCP and UDP on the given public IP, with no STUN observers,
    /// within the dial timeout.
    #[test]
    fn em11_punch_options_offer_tcp_and_udp_on_the_observed_ip() {
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        let o = options(ip);
        assert_eq!((o.tcp_sockets, o.udp_sockets, o.public_ip), (1, 1, ip));
        assert!(o.observers.is_none() && o.police.is_none());
        assert!(o.lead + o.window < Duration::from_secs(8));
    }

    /// EM-11: a failed punch falls back without retrying blindly: the driver's `Usage` error is a
    /// bug (`InvalidInput`), its `Infrastructure` error the normal fallback (`TimedOut`).
    #[tokio::test]
    async fn em11_punch_errors_are_told_apart_for_the_relay_fallback() {
        let hub = MemoryHub::new();
        let (a, b) = (Identity::generate().node_id(), Identity::generate().node_id());
        let rdv = hub.endpoint(a);
        let _silent = hub.endpoint(b);
        let none = PunchOptions { tcp_sockets: 0, udp_sockets: 0, ..options([127, 0, 0, 1].into()) };
        let usage = match driver::punch(&rdv, b, Role::Initiator, &none).await {
            Err(e) => e,
            Ok(_) => panic!("a punch with no sockets"),
        };
        assert_eq!(punch_failed(b, usage).kind(), io::ErrorKind::InvalidInput);
        let quick = PunchOptions { lead: Duration::from_millis(50), window: Duration::from_millis(200), ..options([127, 0, 0, 1].into()) };
        let silent = match driver::punch(&rdv, b, Role::Initiator, &quick).await {
            Err(e) => e,
            Ok(_) => panic!("a silent peer cannot be punched"),
        };
        assert_eq!(silent.kind(), PunchErrorKind::Infrastructure);
        assert_eq!(punch_failed(b, silent).kind(), io::ErrorKind::TimedOut);
    }
}
