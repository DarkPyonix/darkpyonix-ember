//! A thin UDP punch driver (EM-11, #10, SPEC `FR-N1`, `FR-N2`).
//!
//! **Temporary.** letify-link 3.0.5 has no public punch driver: its own one, `op_punch` in
//! `sidecar.rs`, is private to the sidecar. A public driver over a caller's `Rendezvous` is
//! proposed on the letify side and not scheduled. Until it ships, this module stands in for it,
//! built only from letify-link's public pieces, and it is the only place that knows the punch
//! exchange. Its surface is the one type [`PunchDriver`] (and the helper [`udp_offer`]), so the
//! public letify driver can replace the module without touching the rest of the backend.
//!
//! What it does, per punch:
//!
//! 1. Binds a UDP socket and asks the address observers for its public mapping
//!    (`stun::classify_with` when there are enough observers to classify the NAT, otherwise one
//!    QAD observation and an unknown NAT type).
//! 2. The initiator sends a `PunchOfferExt` over the rendezvous and waits for the
//!    `PunchAnswerExt`; the responder answers with its own mapping.
//! 3. Both sides choose sockets and destinations with `plan::udp_plan` and probe with
//!    `punch::punch_udp_all` from the agreed start.
//! 4. The initiator opens a QUIC path (`udp::UdpPath::connect`) to the address that answered;
//!    the responder waits for it on every socket (`punch::accept_udp`).
//!
//! The initiating side is a [`letify_link::Candidate`] of the UDP kind, so the path manager
//! dials it beside the TCP and relay candidates, starts the session on whichever completes first,
//! and leaves the relay once the punched path carries traffic. The responding side hands the
//! path to the backend's `Acceptor`, which checks the peer's key exactly as on TCP.
//!
//! Not done here: TCP simultaneous open (`punch::punch_tcp_many` with `plan::tcp_mappings`).
//! Its port prediction needs a NAT classification, which needs at least three observers, and an
//! Ember endpoint has one (the main server's QAD responder); without a classification the
//! offered TCP mapping is the public address with the listening port, which the backend already
//! dials when the observer's dial-back proved it reachable.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letify_link::observe::AddressObserver;
use letify_link::plan::{self, Destinations};
use letify_link::punch::{self, Plan};
use letify_link::rendezvous::{self, Message, Proto, Punch, PunchExt, Rendezvous};
use letify_link::stun::{self, NatType};
use letify_link::udp::UdpPath;
use letify_link::{Candidate, NodeId};
use tokio::net::UdpSocket;
use tokio::sync::{oneshot, Semaphore};

/// How long after the offer both sides start to probe, so the answer can come back first. The
/// rendezvous may delay a message by seconds; an answer that arrives after the start still
/// works, because both sides probe until the end of the window.
const LEAD: Duration = Duration::from_secs(1);
/// How long both sides probe after the start. Kept under the backend's dial timeout (8 s by
/// default) with the lead and the observation.
const WINDOW: Duration = Duration::from_secs(5);
/// At most this many offers are answered at once.
const MAX_ANSWERING: usize = 4;
/// At most this many own addresses go into one offer or answer.
const MAX_MAPPINGS: usize = 4;
/// After a punch to a peer fails (no answer, a refusal, no path), the next offer to that peer
/// waits this long, doubling per failure up to [`BACKOFF_MAX`]. Every offer is a rendezvous
/// message, and on the hub's pkarr mailbox a message costs requests (SPEC `FR-N2` budget).
const BACKOFF_FIRST: Duration = Duration::from_secs(30);
const BACKOFF_MAX: Duration = Duration::from_secs(300);

/// The punch offer in `msg` when it is a UDP one, with the sender's NAT type.
pub(crate) fn udp_offer(msg: &Message) -> Option<(Punch, NatType)> {
    match msg {
        Message::PunchOffer(p) if p.proto == Proto::Udp => Some((p.clone(), NatType::Unknown)),
        Message::PunchOfferExt(p, ext) if p.proto == Proto::Udp => Some((p.clone(), ext.nat)),
        _ => None,
    }
}

/// An initiator waiting for the answer to its offer.
struct Waiter {
    peer: NodeId,
    answer: oneshot::Sender<(Punch, NatType)>,
}

/// Failed punches to one peer in a row, and when the next offer may go.
struct Backoff {
    failures: u32,
    until: Instant,
}

/// Our side of one punch: the socket the mapping was observed on, and what was observed.
struct Own {
    socket: UdpSocket,
    nat: NatType,
    mappings: Vec<SocketAddr>,
}

/// Punches UDP paths over a rendezvous `R`. A temporary stand-in for the public punch driver
/// proposed for letify-link; see the module documentation.
pub(crate) struct PunchDriver<R> {
    rendezvous: Arc<R>,
    /// The address the punch sockets bind to (the backend's listening address, port 0).
    bind_ip: IpAddr,
    observers: Vec<Arc<dyn AddressObserver>>,
    waiting: Mutex<HashMap<[u8; 16], Waiter>>,
    backoff: Mutex<HashMap<NodeId, Backoff>>,
    answering: Arc<Semaphore>,
}

impl<R: Rendezvous + 'static> PunchDriver<R> {
    /// A driver that sends its offers through `rendezvous`, binds its sockets on `bind_ip` and
    /// learns its public mapping from `observers`. Received punch messages are fed to it with
    /// [`PunchDriver::take_answer`] and [`PunchDriver::respond`].
    pub(crate) fn new(rendezvous: Arc<R>, bind_ip: IpAddr, observers: Vec<Arc<dyn AddressObserver>>) -> Arc<Self> {
        Arc::new(Self {
            rendezvous,
            bind_ip,
            observers,
            waiting: Mutex::new(HashMap::new()),
            backoff: Mutex::new(HashMap::new()),
            answering: Arc::new(Semaphore::new(MAX_ANSWERING)),
        })
    }

    /// The initiating side: a UDP candidate that punches a path to `peer` on every dial.
    pub(crate) fn candidate(self: &Arc<Self>, peer: NodeId) -> Candidate {
        let driver = self.clone();
        Candidate::udp(format!("udp-punch:{peer}"), move || {
            let driver = driver.clone();
            async move {
                let out = driver.initiate(peer).await;
                driver.note(peer, out.is_ok());
                out
            }
        })
    }

    /// Hands an answer to the dial that waits for it. True when `msg` was a punch answer, also
    /// one nobody waits for (it is dropped).
    pub(crate) fn take_answer(&self, from: NodeId, msg: &Message) -> bool {
        let (answer, nat) = match msg {
            Message::PunchAnswer(p) => (p, NatType::Unknown),
            Message::PunchAnswerExt(p, ext) => (p, ext.nat),
            _ => return false,
        };
        let mut waiting = self.waiting.lock().unwrap();
        // Only the peer the offer went to may answer it.
        if waiting.get(&answer.token).is_some_and(|w| w.peer.canonical() == from.canonical()) {
            if let Some(w) = waiting.remove(&answer.token) {
                let _ = w.answer.send((answer.clone(), nat));
            }
        }
        true
    }

    /// The responding side: answers `offer` from `from`, probes, and returns the QUIC path the
    /// initiator opens. The caller checks that it admits `from` first, and hands the path to its
    /// acceptor, which authenticates the peer.
    pub(crate) async fn respond(&self, from: NodeId, offer: Punch, peer_nat: NatType) -> io::Result<UdpPath> {
        let _permit = self
            .answering
            .clone()
            .try_acquire_owned()
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "too many punches in progress"))?;
        if offer.proto != Proto::Udp || offer.mappings.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a UDP punch offer with addresses"));
        }
        let now = punch::now_ms();
        if offer.until_ms <= now {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "the offer's punch window had closed"));
        }
        let own = self.observe().await;
        let latest = now + millis(LEAD + WINDOW);
        let answer = Punch {
            proto: Proto::Udp,
            token: offer.token,
            start_at_ms: offer.start_at_ms.min(latest),
            until_ms: offer.until_ms.min(latest),
            mappings: own.as_ref().map(|o| o.mappings.clone()).unwrap_or_default(),
        };
        let nat = own.as_ref().map_or(NatType::Unknown, |o| o.nat);
        // An answer with no address declines the punch, so the initiator stops waiting at once.
        rendezvous::send_message(&*self.rendezvous, from, &Message::PunchAnswerExt(answer.clone(), PunchExt { nat, police: false }))
            .await?;
        let own = own?;
        let plan = Plan::of(&answer);
        let (sockets, dests) = self.sockets_for(own, peer_nat, &offer.mappings).await?;
        let (sockets, _, _) = punch::punch_udp_all(sockets, &dests, &offer.token, plan, punch::SPRAY_PER_SECOND).await?;
        punch::accept_udp(sockets, plan).await
    }

    async fn initiate(&self, peer: NodeId) -> io::Result<UdpPath> {
        self.check_backoff(peer)?;
        let own = self.observe().await?;
        let mut token = [0u8; 16];
        getrandom::fill(&mut token).map_err(|e| io::Error::other(e.to_string()))?;
        let start = punch::now_ms() + millis(LEAD);
        let offer = Punch {
            proto: Proto::Udp,
            token,
            start_at_ms: start,
            until_ms: start + millis(WINDOW),
            mappings: own.mappings.clone(),
        };
        let (tx, rx) = oneshot::channel();
        self.waiting.lock().unwrap().insert(token, Waiter { peer, answer: tx });
        let _unregister = Unregister { waiting: &self.waiting, token };
        rendezvous::send_message(&*self.rendezvous, peer, &Message::PunchOfferExt(offer.clone(), PunchExt { nat: own.nat, police: false }))
            .await?;
        let left = Duration::from_millis(offer.until_ms.saturating_sub(punch::now_ms()));
        let (answer, peer_nat) = tokio::time::timeout(left, rx)
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the peer did not answer the punch offer"))?
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "the punch was abandoned"))?;
        if answer.mappings.is_empty() {
            return Err(io::Error::new(io::ErrorKind::ConnectionRefused, "the peer declined the punch"));
        }
        let plan = Plan::of(&offer).with_answer(&answer);
        let (sockets, dests) = self.sockets_for(own, peer_nat, &answer.mappings).await?;
        let (mut sockets, index, from) = punch::punch_udp_all(sockets, &dests, &token, plan, punch::SPRAY_PER_SECOND).await?;
        let socket = sockets.swap_remove(index);
        drop(sockets);
        UdpPath::connect(socket.into_std()?, from).await
    }

    /// Binds a punch socket and observes its public mapping. Once a mapping is observed, a host
    /// address that is not the unspecified one is offered too, for a peer on the same network.
    async fn observe(&self) -> io::Result<Own> {
        let socket = UdpSocket::bind(SocketAddr::new(self.bind_ip, 0)).await?;
        let mut nat = NatType::Unknown;
        let mut mappings = Vec::new();
        if self.observers.len() >= stun::MIN_SERVERS {
            let refs: Vec<&dyn AddressObserver> = self.observers.iter().map(|o| o.as_ref()).collect();
            if let Ok(c) = stun::classify_with(&socket, &refs).await {
                nat = c.nat;
                mappings.extend(c.last_mapped());
            }
        }
        if mappings.is_empty() {
            for observer in &self.observers {
                if let Ok(m) = observer.observe(&socket).await {
                    mappings.push(m.mapped);
                    break;
                }
            }
        }
        // No observation, no punch: an endpoint whose observer does not answer (none reachable,
        // or a reply not signed by the pinned key) offers nothing, so a host address alone
        // never starts a punch.
        if mappings.is_empty() {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "no public UDP address was observed"));
        }
        if let Ok(local) = socket.local_addr() {
            if !local.ip().is_unspecified() && !mappings.contains(&local) {
                mappings.push(local);
            }
        }
        mappings.truncate(MAX_MAPPINGS);
        Ok(Own { socket, nat, mappings })
    }

    /// The sockets to probe from and the destinations to probe, as `plan::udp_plan` decides.
    async fn sockets_for(&self, own: Own, peer_nat: NatType, peer_maps: &[SocketAddr]) -> io::Result<(Vec<UdpSocket>, Vec<SocketAddr>)> {
        let p = plan::udp_plan(own.nat, peer_nat, 1, peer_maps);
        let mut sockets = vec![own.socket];
        if p.sockets > 1 {
            sockets.extend(punch::bind_birthday_sockets(self.bind_ip, p.sockets - 1).await?);
        }
        let dests = match p.dests {
            Destinations::Fixed(v) => v,
            Destinations::Birthday { ip, count } => punch::birthday_destinations(ip, count),
        };
        Ok((sockets, dests))
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

    /// Records the outcome of one dial: a success clears the backoff, a failure (other than one
    /// made during the backoff) extends it.
    fn note(&self, peer: NodeId, ok: bool) {
        let mut backoff = self.backoff.lock().unwrap();
        let key = peer.canonical();
        if ok {
            backoff.remove(&key);
            return;
        }
        let now = Instant::now();
        let b = backoff.entry(key).or_insert(Backoff { failures: 0, until: now });
        if now < b.until {
            return;
        }
        let wait = BACKOFF_FIRST.saturating_mul(1 << b.failures.min(8)).min(BACKOFF_MAX);
        b.failures = b.failures.saturating_add(1);
        b.until = now + wait;
    }
}

/// Removes a waiting initiator when its dial ends, whatever the outcome.
struct Unregister<'a> {
    waiting: &'a Mutex<HashMap<[u8; 16], Waiter>>,
    token: [u8; 16],
}

impl Drop for Unregister<'_> {
    fn drop(&mut self) {
        self.waiting.lock().unwrap().remove(&self.token);
    }
}

fn millis(d: Duration) -> u64 {
    d.as_millis().min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use letify_link::rendezvous::MemoryHub;
    use letify_link::Identity;

    fn offer(proto: Proto) -> Punch {
        Punch { proto, token: [1; 16], start_at_ms: 1, until_ms: 2, mappings: vec!["127.0.0.1:9".parse().unwrap()] }
    }

    #[test]
    fn em11_only_udp_offers_are_answered() {
        assert!(udp_offer(&Message::PunchOffer(offer(Proto::Udp))).is_some());
        let ext = PunchExt { nat: NatType::Symmetric { step: 2 }, police: false };
        assert_eq!(udp_offer(&Message::PunchOfferExt(offer(Proto::Udp), ext)).map(|(_, n)| n), Some(ext.nat));
        assert!(udp_offer(&Message::PunchOffer(offer(Proto::Tcp))).is_none());
        assert!(udp_offer(&Message::PunchAnswer(offer(Proto::Udp))).is_none());
    }

    #[tokio::test]
    async fn em11_an_answer_from_another_peer_is_dropped_and_a_failed_punch_backs_off() {
        let hub = MemoryHub::new();
        let (me, peer, other) = (Identity::generate().node_id(), Identity::generate().node_id(), Identity::generate().node_id());
        let driver = PunchDriver::new(Arc::new(hub.endpoint(me)), "127.0.0.1".parse().unwrap(), Vec::new());
        let (tx, mut rx) = oneshot::channel();
        driver.waiting.lock().unwrap().insert([1; 16], Waiter { peer, answer: tx });
        assert!(driver.take_answer(other, &Message::PunchAnswer(offer(Proto::Udp))));
        assert!(rx.try_recv().is_err(), "a stranger's answer must not reach the dial");
        assert!(driver.take_answer(peer, &Message::PunchAnswer(offer(Proto::Udp))));
        assert!(rx.try_recv().is_ok());
        assert!(!driver.take_answer(peer, &Message::PunchOffer(offer(Proto::Udp))));

        assert!(driver.check_backoff(peer).is_ok());
        driver.note(peer, false);
        assert_eq!(driver.check_backoff(peer).unwrap_err().kind(), io::ErrorKind::WouldBlock);
        driver.note(peer, true);
        assert!(driver.check_backoff(peer).is_ok());
    }
}
