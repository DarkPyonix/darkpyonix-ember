//! `PersistentProtocol` as a sans-IO state machine.
//!
//! Mirrors `PersistentProtocol` in `src/vs/base/parts/ipc/common/ipc.net.ts` (L816-1232 at the
//! pinned commit):
//!
//! * `send` (L1077): regular messages get ids 1, 2, 3…, carry the highest received id as `ack`,
//!   and stay in an unacknowledged queue until the peer acks them.
//! * `_receiveMessage` (L995): any frame's `ack` trims the queue; a regular frame with id
//!   `last+1` is delivered, a gap triggers a `ReplayRequest` (at most every 10 s), a duplicate
//!   is dropped; `ReplayRequest` resends the queue; `Pause`/`Resume` gate the writer.
//! * `endAcceptReconnection` (L974): after a reconnect handshake, send an `Ack` and the whole
//!   unacknowledged queue on the new socket.
//! * Timers (`ProtocolConstants`, L289-313): ack within 2 s, keep-alive every 5 s, timeout after
//!   20 s with no incoming data.
//!
//! The async driver ([`crate::connection`]) owns the socket and the clock; this type only decides
//! which frames to write.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::frame::{Frame, MessageType};

/// `ProtocolConstants.AcknowledgeTime`.
pub const ACKNOWLEDGE_TIME: Duration = Duration::from_millis(2000);
/// `ProtocolConstants.TimeoutTime`.
pub const TIMEOUT_TIME: Duration = Duration::from_millis(20_000);
/// `ProtocolConstants.KeepAliveSendTime`.
pub const KEEP_ALIVE_SEND_TIME: Duration = Duration::from_millis(5000);
/// Replay requests are rate-limited to one per 10 s (L1019).
pub const REPLAY_REQUEST_INTERVAL: Duration = Duration::from_millis(10_000);
/// `ProtocolConstants.ReconnectionGraceTime` (server default; the server may send a shorter one
/// in `IRemoteAgentEnvironmentDTO.reconnectionGraceTime`).
pub const RECONNECTION_GRACE_TIME: Duration = Duration::from_secs(3 * 60 * 60);
/// Reconnect back-off in seconds, `PersistentConnection._runReconnectingLoop`
/// (remoteAgentConnection.ts L649).
pub const RECONNECT_BACKOFF_SECS: [u64; 9] = [0, 5, 5, 10, 10, 10, 10, 10, 30];

/// What an incoming frame means to the layer above.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inbound {
    /// An in-order regular message: hand to IPC / RPC.
    Regular(Vec<u8>),
    /// A control message: handshake JSON.
    Control(Vec<u8>),
    /// The peer sent `Disconnect`: the connection is over for good.
    Disconnect,
}

#[derive(Debug)]
struct Unacked {
    frame: Frame,
    written_at: Option<Instant>,
}

#[derive(Debug)]
pub struct PersistentProtocol {
    outgoing_msg_id: u32,
    outgoing_ack_id: u32,
    unacked: VecDeque<Unacked>,

    incoming_msg_id: u32,
    incoming_ack_id: u32,
    last_incoming_regular: Option<Instant>,

    last_read: Instant,
    last_replay_request: Option<Instant>,
    last_timeout: Instant,

    reconnecting: bool,
    paused: bool,
    held: Vec<Frame>,
    did_send_disconnect: bool,
}

impl PersistentProtocol {
    pub fn new(now: Instant) -> Self {
        Self {
            outgoing_msg_id: 0,
            outgoing_ack_id: 0,
            unacked: VecDeque::new(),
            incoming_msg_id: 0,
            incoming_ack_id: 0,
            last_incoming_regular: None,
            last_read: now,
            last_replay_request: None,
            last_timeout: now,
            reconnecting: false,
            paused: false,
            held: Vec::new(),
            did_send_disconnect: false,
        }
    }

    /// Number of regular messages sent but not yet acknowledged.
    pub fn unacknowledged_count(&self) -> u32 {
        self.outgoing_msg_id - self.outgoing_ack_id
    }

    pub fn incoming_msg_id(&self) -> u32 {
        self.incoming_msg_id
    }

    pub fn is_reconnecting(&self) -> bool {
        self.reconnecting
    }

    fn emit(&mut self, f: Frame, out: &mut Vec<Frame>) {
        if self.paused {
            self.held.push(f);
        } else {
            out.push(f);
        }
    }

    /// Queue a regular message. Returns frames to write now (none while reconnecting).
    pub fn send(&mut self, data: Vec<u8>, now: Instant) -> Vec<Frame> {
        self.outgoing_msg_id += 1;
        self.incoming_ack_id = self.incoming_msg_id;
        let frame = Frame::new(MessageType::Regular, self.outgoing_msg_id, self.incoming_ack_id, data);
        let mut out = Vec::new();
        if self.reconnecting {
            self.unacked.push_back(Unacked { frame, written_at: None });
        } else {
            self.unacked.push_back(Unacked { frame: frame.clone(), written_at: Some(now) });
            self.emit(frame, &mut out);
        }
        out
    }

    /// A control (handshake) message; never counted or replayed.
    pub fn send_control(&mut self, data: Vec<u8>) -> Vec<Frame> {
        let mut out = Vec::new();
        self.emit(Frame::new(MessageType::Control, 0, 0, data), &mut out);
        out
    }

    /// `sendDisconnect`: tell the peer we are going away for good (sent once).
    pub fn send_disconnect(&mut self) -> Vec<Frame> {
        if self.did_send_disconnect {
            return Vec::new();
        }
        self.did_send_disconnect = true;
        // Upstream writes then flushes regardless of pause state.
        vec![Frame::empty(MessageType::Disconnect, 0)]
    }

    /// Process one decoded frame.
    pub fn receive(&mut self, frame: Frame, now: Instant) -> (Option<Inbound>, Vec<Frame>) {
        self.last_read = now;
        let mut out = Vec::new();

        if frame.ack > self.outgoing_ack_id {
            self.outgoing_ack_id = frame.ack;
            while let Some(first) = self.unacked.front() {
                if first.frame.id <= frame.ack {
                    self.unacked.pop_front();
                } else {
                    break;
                }
            }
        }

        let inbound = match frame.ty {
            MessageType::None | MessageType::Ack | MessageType::KeepAlive => None,
            MessageType::Regular => {
                if frame.id > self.incoming_msg_id {
                    if frame.id != self.incoming_msg_id + 1 {
                        let due = self
                            .last_replay_request
                            .is_none_or(|t| now.duration_since(t) > REPLAY_REQUEST_INTERVAL);
                        if due {
                            self.last_replay_request = Some(now);
                            self.emit(Frame::empty(MessageType::ReplayRequest, 0), &mut out);
                        }
                        None
                    } else {
                        self.incoming_msg_id = frame.id;
                        self.last_incoming_regular = Some(now);
                        Some(Inbound::Regular(frame.data))
                    }
                } else {
                    None // duplicate after replay
                }
            }
            MessageType::Control => Some(Inbound::Control(frame.data)),
            MessageType::Disconnect => Some(Inbound::Disconnect),
            MessageType::ReplayRequest => {
                let frames: Vec<Frame> = self.unacked.iter().map(|u| u.frame.clone()).collect();
                for u in self.unacked.iter_mut() {
                    u.written_at = Some(now);
                }
                for f in frames {
                    self.emit(f, &mut out);
                }
                None
            }
            MessageType::Pause => {
                self.paused = true;
                None
            }
            MessageType::Resume => {
                self.paused = false;
                out.append(&mut self.held);
                None
            }
        };
        (inbound, out)
    }

    /// Periodic housekeeping; call at least every [`ACKNOWLEDGE_TIME`].
    /// Returns frames to write and whether the socket should be considered dead.
    pub fn tick(&mut self, now: Instant) -> (Vec<Frame>, bool) {
        let mut out = Vec::new();
        if self.incoming_msg_id > self.incoming_ack_id {
            if let Some(t) = self.last_incoming_regular {
                if now.duration_since(t) >= ACKNOWLEDGE_TIME {
                    self.incoming_ack_id = self.incoming_msg_id;
                    self.emit(Frame::empty(MessageType::Ack, self.incoming_ack_id), &mut out);
                }
            }
        }
        let timed_out = !self.reconnecting
            && now.duration_since(self.last_read) >= TIMEOUT_TIME
            && now.duration_since(self.last_timeout) >= TIMEOUT_TIME;
        if timed_out {
            self.last_timeout = now;
        }
        (out, timed_out)
    }

    /// `_sendKeepAlive`: a keep-alive that also carries our latest ack.
    pub fn keep_alive(&mut self) -> Vec<Frame> {
        self.incoming_ack_id = self.incoming_msg_id;
        let mut out = Vec::new();
        self.emit(Frame::empty(MessageType::KeepAlive, self.incoming_ack_id), &mut out);
        out
    }

    /// `beginAcceptReconnection`: a new socket is about to carry the handshake.
    /// Regular sends are queued (not written) until [`Self::end_reconnect`].
    pub fn begin_reconnect(&mut self, now: Instant) {
        self.reconnecting = true;
        self.paused = false;
        self.held.clear();
        self.last_replay_request = None;
        self.last_read = now;
        self.last_timeout = now;
    }

    /// `endAcceptReconnection`: re-ack and replay everything unacknowledged.
    pub fn end_reconnect(&mut self, now: Instant) -> Vec<Frame> {
        self.reconnecting = false;
        self.incoming_ack_id = self.incoming_msg_id;
        let mut out = Vec::new();
        self.emit(Frame::empty(MessageType::Ack, self.incoming_ack_id), &mut out);
        let frames: Vec<Frame> = self.unacked.iter().map(|u| u.frame.clone()).collect();
        for u in self.unacked.iter_mut() {
            u.written_at = Some(now);
        }
        for f in frames {
            self.emit(f, &mut out);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regular(id: u32, ack: u32, data: &[u8]) -> Frame {
        Frame::new(MessageType::Regular, id, ack, data.to_vec())
    }

    #[test]
    fn send_numbers_and_piggybacks_ack() {
        let t = Instant::now();
        let mut p = PersistentProtocol::new(t);
        let (inb, _) = p.receive(regular(1, 0, b"a"), t);
        assert_eq!(inb, Some(Inbound::Regular(b"a".to_vec())));
        let out = p.send(b"x".to_vec(), t);
        assert_eq!(out, vec![regular(1, 1, b"x")]);
        let out = p.send(b"y".to_vec(), t);
        assert_eq!(out[0].id, 2);
        assert_eq!(p.unacknowledged_count(), 2);
    }

    #[test]
    fn ack_trims_queue() {
        let t = Instant::now();
        let mut p = PersistentProtocol::new(t);
        p.send(b"1".to_vec(), t);
        p.send(b"2".to_vec(), t);
        p.receive(Frame::empty(MessageType::Ack, 1), t);
        assert_eq!(p.unacknowledged_count(), 1);
        p.receive(Frame::empty(MessageType::KeepAlive, 2), t);
        assert_eq!(p.unacknowledged_count(), 0);
    }

    #[test]
    fn gap_requests_replay_and_duplicate_is_dropped() {
        let t = Instant::now();
        let mut p = PersistentProtocol::new(t);
        let (inb, out) = p.receive(regular(2, 0, b"b"), t);
        assert_eq!(inb, None);
        assert_eq!(out, vec![Frame::empty(MessageType::ReplayRequest, 0)]);
        // rate limited
        let (_, out) = p.receive(regular(3, 0, b"c"), t);
        assert!(out.is_empty());
        assert_eq!(p.receive(regular(1, 0, b"a"), t).0, Some(Inbound::Regular(b"a".to_vec())));
        assert_eq!(p.receive(regular(1, 0, b"a"), t).0, None);
    }

    #[test]
    fn replay_request_resends_unacked() {
        let t = Instant::now();
        let mut p = PersistentProtocol::new(t);
        p.send(b"1".to_vec(), t);
        p.send(b"2".to_vec(), t);
        let (_, out) = p.receive(Frame::empty(MessageType::ReplayRequest, 1), t);
        assert_eq!(out, vec![regular(2, 0, b"2")]);
    }

    #[test]
    fn pause_holds_writes_until_resume() {
        let t = Instant::now();
        let mut p = PersistentProtocol::new(t);
        p.receive(Frame::empty(MessageType::Pause, 0), t);
        assert!(p.send(b"1".to_vec(), t).is_empty());
        let (_, out) = p.receive(Frame::empty(MessageType::Resume, 0), t);
        assert_eq!(out, vec![regular(1, 0, b"1")]);
    }

    #[test]
    fn reconnect_queues_then_replays_with_ack() {
        let t = Instant::now();
        let mut p = PersistentProtocol::new(t);
        p.receive(regular(1, 0, b"in"), t);
        p.send(b"1".to_vec(), t);
        p.begin_reconnect(t);
        assert!(p.send(b"2".to_vec(), t).is_empty());
        // handshake control frames still flow
        assert_eq!(p.send_control(b"{}".to_vec()).len(), 1);
        let out = p.end_reconnect(t);
        assert_eq!(out[0], Frame::empty(MessageType::Ack, 1));
        assert_eq!(out[1], regular(1, 1, b"1"));
        assert_eq!(out[2], regular(2, 1, b"2"));
    }

    #[test]
    fn tick_acks_after_two_seconds_and_times_out_after_twenty() {
        let t = Instant::now();
        let mut p = PersistentProtocol::new(t);
        p.receive(regular(1, 0, b"a"), t);
        assert!(p.tick(t + Duration::from_millis(500)).0.is_empty());
        let (out, dead) = p.tick(t + ACKNOWLEDGE_TIME);
        assert_eq!(out, vec![Frame::empty(MessageType::Ack, 1)]);
        assert!(!dead);
        let (_, dead) = p.tick(t + TIMEOUT_TIME);
        assert!(dead);
    }
}
