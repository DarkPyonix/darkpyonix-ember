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
use crate::names::{clean_offer_paths, MAX_OFFER_FILES};
use crate::state::{Clock, Event, State, Transfer, TransitionError};
use crate::store::{DeclaredFile, ReceiveWriter, StoreError, StoredFile, DEFAULT_CHUNK_BYTES};
use crate::wire::{read_message, write_message, DeclineCode, FileEntry, Message, WireError};
use sha2::{Digest, Sha256};

/// Default time limit for an unanswered offer.
pub const DEFAULT_OFFER_TTL: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Wire(#[from] WireError),
    #[error(transparent)]
    Transition(#[from] TransitionError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("protocol error: {0}")]
    Protocol(String),
}

fn random_id() -> Result<String, Error> {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b)
        .map_err(|e| Error::Protocol(format!("OS random source unavailable: {e}")))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
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

    /// Receives all files of this accepted offer into `received_dir` (`FR-F4`, `FR-F5`, `FR-F8`,
    /// `FR-F9`, `FR-F10`). Resumes any existing partial files from their verified byte offsets,
    /// checks every chunk and final SHA-256 digest, and moves completed files into `received_dir`
    /// with mode `0644`. If cancelled or failed, removes partial files (`FR-F4`).
    pub async fn receive_into(
        &mut self,
        received_dir: &std::path::Path,
        limits: Limits,
    ) -> Result<Vec<StoredFile>, Error> {
        let Some(mut halves) = self.halves.take() else {
            return Err(Error::Protocol("offer is not in an accepted state with open stream".into()));
        };
        if self.transfer.state() == &State::Accepted {
            self.transfer.apply(Event::Start, self.clock.now())?;
        }
        let declared: Vec<DeclaredFile> = self
            .files
            .iter()
            .map(|f| DeclaredFile { name: f.name.clone(), size: f.size, sha256: None })
            .collect();
        let mut writer = ReceiveWriter::open(
            received_dir,
            &self.id,
            &self.from.to_string(),
            &declared,
            limits,
        )?;
        for idx in 0..declared.len() {
            let offset = writer.resume_offset(idx)?;
            if offset > 0 {
                write_message(
                    &mut halves.send,
                    &Message::ResumeFrom {
                        id: self.id.clone(),
                        file: idx as u32,
                        offset,
                    },
                )
                .await?;
            }
        }

        loop {
            let Some(msg) = read_message(&mut halves.recv).await? else {
                writer.abort();
                let _ = self.transfer.apply(Event::Fail("stream closed before done".into()), self.clock.now());
                return Err(Error::Protocol("stream closed before done".into()));
            };
            if msg.id() != self.id {
                writer.abort();
                return Err(Error::Protocol("message for another offer".into()));
            }
            match msg {
                Message::Chunk { file, offset, data, sha256, .. } => {
                    match writer.write_chunk(file as usize, offset, &data, sha256.as_deref()) {
                        Ok(total) => {
                            let _ = self.transfer.apply(Event::Progress(total), self.clock.now());
                        }
                        Err(e) => {
                            writer.abort();
                            let _ = self.transfer.apply(Event::Fail(e.to_string()), self.clock.now());
                            return Err(Error::Store(e));
                        }
                    }
                }
                Message::Pause { .. } | Message::Resume { .. } => {}
                Message::Cancel { .. } => {
                    writer.abort();
                    self.transfer.apply(Event::Cancel, self.clock.now())?;
                    return Err(Error::Protocol("cancelled by sender".into()));
                }
                Message::Done { sha256, .. } => {
                    let stored = writer.finalize()?;
                    if !sha256.is_empty() && sha256.len() != stored.len() {
                        for s in &stored {
                            let _ = std::fs::remove_file(&s.path);
                        }
                        let _ = self.transfer.apply(Event::Fail("digest count mismatch".into()), self.clock.now());
                        return Err(Error::Protocol("digest count mismatch".into()));
                    }
                    for (s, want) in stored.iter().zip(sha256.iter()) {
                        if !s.sha256.eq_ignore_ascii_case(want) {
                            for st in &stored {
                                let _ = std::fs::remove_file(&st.path);
                            }
                            let err = StoreError::HashMismatch {
                                name: s.name.clone(),
                                expected: want.clone(),
                                got: s.sha256.clone(),
                            };
                            let _ = self.transfer.apply(Event::Fail(err.to_string()), self.clock.now());
                            return Err(Error::Store(err));
                        }
                    }
                    self.transfer.apply(Event::Finish, self.clock.now())?;
                    return Ok(stored);
                }
                _ => {
                    writer.abort();
                    return Err(Error::Protocol("unexpected message while receiving chunks".into()));
                }
            }
        }
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
        if files.len() > MAX_OFFER_FILES {
            return self
                .refuse(handled, send, DeclineCode::Invalid, "the offer has too many files")
                .await;
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
        // 3. Names (disambiguating duplicate cleaned paths within the offer, #527).
        let raw_names: Vec<String> = files.iter().map(|f| f.name.clone()).collect();
        let cleaned_paths = clean_offer_paths(&raw_names);
        let cleaned: Vec<FileEntry> = files
            .iter()
            .zip(cleaned_paths.into_iter())
            .map(|(f, comps)| FileEntry { name: comps.join("/"), size: f.size })
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
        let id = random_id()?;
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
            _ => Err(Error::Protocol("unexpected message before the answer".into())),
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

    /// Streams `payloads` (one byte slice per file in the offer) in chunks of at most `chunk_size`
    /// bytes, honouring any `ResumeFrom` offsets sent by the receiver, and finishes with `Done`
    /// carrying each file's SHA-256 digest (`FR-F5`).
    pub async fn send_bytes(&mut self, payloads: &[&[u8]], chunk_size: usize) -> Result<Vec<String>, Error> {
        let step = chunk_size.clamp(1, DEFAULT_CHUNK_BYTES);
        if self.transfer.state() == &State::Accepted {
            self.transfer.apply(Event::Start, self.clock.now())?;
        }
        let mut digests = Vec::with_capacity(payloads.len());
        let mut sent_total: u64 = 0;
        for (idx, bytes) in payloads.iter().enumerate() {
            let full_hash = hex::encode(Sha256::digest(bytes));
            digests.push(full_hash);
            let mut offset = 0usize;
            while offset < bytes.len() {
                let end = (offset + step).min(bytes.len());
                let slice = &bytes[offset..end];
                let chunk_hash = hex::encode(Sha256::digest(slice));
                write_message(
                    &mut self.send,
                    &Message::Chunk {
                        id: self.id.clone(),
                        file: idx as u32,
                        offset: offset as u64,
                        data: slice.to_vec(),
                        sha256: Some(chunk_hash),
                    },
                )
                .await?;
                offset = end;
                sent_total = sent_total.saturating_add(slice.len() as u64);
                let _ = self.transfer.apply(Event::Progress(sent_total), self.clock.now());
            }
        }
        write_message(
            &mut self.send,
            &Message::Done { id: self.id.clone(), sha256: digests.clone() },
        )
        .await?;
        self.transfer.apply(Event::Finish, self.clock.now())?;
        Ok(digests)
    }
}
