//! The offer handshake over one `ember-transport` stream (SPEC `FR-F3`, `FR-F9`, `FR-F10`).
//!
//! One offer is one bidirectional stream. The sender writes Offer; the receiver answers Accept or
//! Decline; either side may write Cancel. No file bytes move in this unit.
//!
//! The receiver runs its checks in a fixed order, and a failing check ends the offer before the
//! [`Prompter`] is asked:
//!
//! 1. [`PeerPolicy`]: only the user's registered devices are served (`FR-F9`, `FR-F10`).
//! 2. The offer is well formed (files, total equals the sum) and within [`Limits`] (`FR-F10`).
//! 3. Every name goes through [`crate::names`]; the cleaned name is what the prompt shows.
//! 4. [`decide`] with the receive mode: off refuses, auto-accept skips only the prompt.
//!
//! An offer is judged against the clock when the answer arrives (injected [`Clock`]), so an
//! answer after the time limit becomes Expired. A real-time cutoff for a prompt that never
//! returns is a follow-up with the UI (the prompt future can simply be dropped by the caller).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use ember_transport::{BiStream, PeerId, RecvHalf, SendHalf};

use crate::limits::{LimitError, Limits};
use crate::mode::{decide, Decision, ReceiveMode};
use crate::names::clean_relative_path;
use crate::state::{Clock, Event, State, Transfer, TransitionError};
use crate::wire::{read_message, write_message, DeclineCode, FileEntry, Message, WireError};

/// Default time limit for an unanswered offer.
pub const DEFAULT_OFFER_TTL: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Wire(#[from] WireError),
    #[error(transparent)]
    Transition(#[from] TransitionError),
    #[error("protocol error: {0}")]
    Protocol(String),
}

fn random_id() -> String {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).expect("OS random source unavailable");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Which peers are the user's registered, not revoked devices (`FR-F9`, `FR-N3`).
pub trait PeerPolicy: Send + Sync {
    fn is_registered(&self, peer: PeerId) -> bool;
}

impl PeerPolicy for HashSet<PeerId> {
    fn is_registered(&self, peer: PeerId) -> bool {
        self.contains(&peer)
    }
}

/// What the user is shown. The names are already cleaned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingOffer {
    pub id: String,
    pub from: PeerId,
    pub files: Vec<FileEntry>,
    pub total: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptAnswer {
    Accept,
    Decline,
}

/// Asks the user. Implemented by the UI (later) and by tests.
#[async_trait]
pub trait Prompter: Send + Sync {
    async fn prompt(&self, offer: &IncomingOffer) -> PromptAnswer;
}

/// The two halves of an accepted offer's stream, kept for the next unit (bytes).
pub struct Halves {
    pub send: SendHalf,
    pub recv: RecvHalf,
}

/// The result of handling one offer on the receiving side.
pub struct Handled {
    pub id: String,
    pub from: PeerId,
    /// Cleaned names.
    pub files: Vec<FileEntry>,
    pub total: u64,
    pub transfer: Transfer,
    /// Why the offer was refused, if the receiver refused it.
    pub refusal: Option<DeclineCode>,
    /// Present only while the offer is accepted.
    pub halves: Option<Halves>,
    clock: Arc<dyn Clock>,
}

impl Handled {
    pub fn state(&self) -> &State {
        self.transfer.state()
    }

    /// Cancels from the receiving side: tells the sender and ends the transfer (`FR-F4`).
    pub async fn cancel(&mut self) -> Result<(), Error> {
        self.transfer.apply(Event::Cancel, self.clock.now())?;
        if let Some(h) = self.halves.as_mut() {
            write_message(&mut h.send, &Message::Cancel { id: self.id.clone() }).await?;
        }
        self.halves = None;
        Ok(())
    }
}

/// The receiving side of the handshake.
pub struct Service {
    policy: Arc<dyn PeerPolicy>,
    prompter: Arc<dyn Prompter>,
    clock: Arc<dyn Clock>,
    ttl: Duration,
    limits: Mutex<Limits>,
    mode: Mutex<ReceiveMode>,
}

impl Service {
    pub fn new(
        policy: Arc<dyn PeerPolicy>,
        prompter: Arc<dyn Prompter>,
        clock: Arc<dyn Clock>,
        limits: Limits,
        mode: ReceiveMode,
    ) -> Self {
        Self {
            policy,
            prompter,
            clock,
            ttl: DEFAULT_OFFER_TTL,
            limits: Mutex::new(limits),
            mode: Mutex::new(mode),
        }
    }

    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// The next offer is judged against the new limit (`FR-F10`).
    pub fn set_limits(&self, limits: Limits) {
        *self.limits.lock().unwrap() = limits;
    }

    pub fn set_mode(&self, mode: ReceiveMode) {
        *self.mode.lock().unwrap() = mode;
    }

    /// Handles one offer on `stream` from `peer` (the identity of the transport connection).
    pub async fn handle_stream(&self, peer: PeerId, stream: BiStream) -> Result<Handled, Error> {
        let (mut send, mut recv) = stream.into_split();
        let first = read_message(&mut recv).await?;
        let Some(Message::Offer { id, files, total }) = first else {
            return Err(Error::Protocol("the stream did not start with an offer".into()));
        };
        let mut transfer = Transfer::new(self.clock.now(), self.ttl);
        let mut handled = Handled {
            id: id.clone(),
            from: peer,
            files: Vec::new(),
            total,
            transfer: transfer.clone(),
            refusal: None,
            halves: None,
            clock: self.clock.clone(),
        };

        // 1. Registered peers only, before anything else.
        if !self.policy.is_registered(peer) {
            return self.refuse(handled, send, DeclineCode::NotRegistered, "not a registered device").await;
        }
        // 2. Well formed and within the limit, before any prompt.
        if files.is_empty() {
            return self.refuse(handled, send, DeclineCode::Invalid, "the offer has no files").await;
        }
        let mut sum: u64 = 0;
        for f in &files {
            match sum.checked_add(f.size) {
                Some(s) => sum = s,
                None => return self.refuse(handled, send, DeclineCode::Invalid, "sizes overflow").await,
            }
        }
        if sum != total {
            return self.refuse(handled, send, DeclineCode::Invalid, "the total is not the sum of the files").await;
        }
        let limits = *self.limits.lock().unwrap();
        if let Err(e) = limits.check_total(total) {
            let code = match e {
                LimitError::TooLarge { .. } => DeclineCode::TooLarge,
                LimitError::Overflow => DeclineCode::Invalid,
            };
            return self.refuse(handled, send, code, &e.to_string()).await;
        }
        // 3. Names.
        let cleaned: Vec<FileEntry> = files
            .iter()
            .map(|f| FileEntry { name: clean_relative_path(&f.name).join("/"), size: f.size })
            .collect();
        handled.files = cleaned.clone();
        let incoming = IncomingOffer { id: id.clone(), from: peer, files: cleaned, total };

        // 4. The receive mode.
        let mode = self.mode.lock().unwrap().clone();
        let answer = match decide(peer, &mode) {
            Decision::Refuse => {
                return self.refuse(handled, send, DeclineCode::ReceiveOff, "the device does not receive files").await;
            }
            Decision::Accept => PromptAnswer::Accept,
            Decision::Prompt => {
                let prompted = {
                    let read = read_message(&mut recv);
                    tokio::pin!(read);
                    let prompt = self.prompter.prompt(&incoming);
                    tokio::pin!(prompt);
                    tokio::select! {
                        a = &mut prompt => Ok(a),
                        m = &mut read => Err(m),
                    }
                };
                match prompted {
                    Ok(a) => a,
                    Err(msg) => {
                        // The sender spoke or went away before the user answered.
                        match msg? {
                            Some(Message::Cancel { id: cid }) if cid == id => {}
                            None => {}
                            Some(_) => return Err(Error::Protocol("unexpected message before the answer".into())),
                        }
                        transfer.apply(Event::Cancel, self.clock.now())?;
                        handled.transfer = transfer;
                        return Ok(handled);
                    }
                }
            }
        };

        match answer {
            PromptAnswer::Decline => match transfer.apply(Event::Decline, self.clock.now()).map(|_| ()) {
                Ok(_) => {
                    handled.transfer = transfer;
                    handled.refusal = Some(DeclineCode::User);
                    write_message(
                        &mut send,
                        &Message::Decline { id, code: DeclineCode::User, detail: String::new() },
                    )
                    .await?;
                    Ok(handled)
                }
                Err(TransitionError::Expired) => self.expired(handled, transfer, send, id).await,
                Err(e) => Err(e.into()),
            },
            PromptAnswer::Accept => match transfer.apply(Event::Accept, self.clock.now()).map(|_| ()) {
                Ok(_) => {
                    handled.transfer = transfer;
                    write_message(&mut send, &Message::Accept { id }).await?;
                    handled.halves = Some(Halves { send, recv });
                    Ok(handled)
                }
                Err(TransitionError::Expired) => self.expired(handled, transfer, send, id).await,
                Err(e) => Err(e.into()),
            },
        }
    }

    async fn expired(
        &self,
        mut handled: Handled,
        transfer: Transfer,
        mut send: SendHalf,
        id: String,
    ) -> Result<Handled, Error> {
        handled.transfer = transfer;
        handled.refusal = Some(DeclineCode::Expired);
        write_message(
            &mut send,
            &Message::Decline {
                id,
                code: DeclineCode::Expired,
                detail: "no answer within the time limit".into(),
            },
        )
        .await?;
        Ok(handled)
    }

    async fn refuse(
        &self,
        mut handled: Handled,
        mut send: SendHalf,
        code: DeclineCode,
        detail: &str,
    ) -> Result<Handled, Error> {
        handled.transfer.apply(Event::Decline, self.clock.now())?;
        handled.refusal = Some(code);
        write_message(
            &mut send,
            &Message::Decline { id: handled.id.clone(), code, detail: detail.to_string() },
        )
        .await?;
        Ok(handled)
    }
}

/// What the receiver said to an offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Accepted,
    Declined { code: DeclineCode, detail: String },
    Expired,
    Cancelled,
}

/// The sending side of the handshake.
pub struct Outgoing {
    id: String,
    transfer: Transfer,
    send: SendHalf,
    recv: RecvHalf,
    clock: Arc<dyn Clock>,
}

impl Outgoing {
    /// Writes an offer for `files` on a new stream of `conn`. The total is the sum of the sizes
    /// (a sum that overflows is refused here). The offer expires after `ttl` without an answer.
    pub async fn start(
        conn: &ember_transport::Connection,
        files: Vec<FileEntry>,
        clock: Arc<dyn Clock>,
        ttl: Duration,
    ) -> Result<Self, Error> {
        let mut total: u64 = 0;
        for f in &files {
            total = total
                .checked_add(f.size)
                .ok_or_else(|| Error::Protocol("the sizes overflow 64 bits".into()))?;
        }
        let id = random_id();
        let stream = conn
            .open_bi()
            .await
            .map_err(|e| Error::Protocol(format!("cannot open a stream: {e}")))?;
        let (mut send, recv) = stream.into_split();
        write_message(&mut send, &Message::Offer { id: id.clone(), files, total }).await?;
        Ok(Self { id, transfer: Transfer::new(clock.now(), ttl), send, recv, clock })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn state(&self) -> &State {
        self.transfer.state()
    }

    /// Waits for the receiver's answer.
    pub async fn wait_answer(&mut self) -> Result<Reply, Error> {
        let msg = read_message(&mut self.recv)
            .await?
            .ok_or_else(|| Error::Protocol("the stream closed before an answer".into()))?;
        if msg.id() != self.id {
            return Err(Error::Protocol("an answer for another offer".into()));
        }
        let now = self.clock.now();
        match msg {
            Message::Accept { .. } => match self.transfer.apply(Event::Accept, now) {
                Ok(_) => Ok(Reply::Accepted),
                Err(TransitionError::Expired) => Ok(Reply::Expired),
                Err(e) => Err(e.into()),
            },
            Message::Decline { code: DeclineCode::Expired, .. } => {
                self.transfer.apply(Event::Expire, now)?;
                Ok(Reply::Expired)
            }
            Message::Decline { code, detail, .. } => {
                self.transfer.apply(Event::Decline, now)?;
                Ok(Reply::Declined { code, detail })
            }
            Message::Cancel { .. } => {
                self.transfer.apply(Event::Cancel, now)?;
                Ok(Reply::Cancelled)
            }
            Message::Offer { .. } => Err(Error::Protocol("an offer came back".into())),
        }
    }

    /// Waits until the receiver cancels an accepted transfer (`FR-F4`).
    pub async fn wait_cancel(&mut self) -> Result<(), Error> {
        match read_message(&mut self.recv).await? {
            Some(Message::Cancel { id }) if id == self.id => {
                self.transfer.apply(Event::Cancel, self.clock.now())?;
                Ok(())
            }
            _ => Err(Error::Protocol("expected a cancel".into())),
        }
    }

    /// Cancels from the sending side and tells the receiver (`FR-F4`).
    pub async fn cancel(&mut self) -> Result<(), Error> {
        self.transfer.apply(Event::Cancel, self.clock.now())?;
        write_message(&mut self.send, &Message::Cancel { id: self.id.clone() }).await?;
        Ok(())
    }
}
