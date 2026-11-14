//! The 파일 screen's real transfer service (SPEC `FR-L27.8`, `FR-F3`, `FR-F4`, `FR-F5`, #487):
//! [`ServerTransferService`] implements [`TransferService`] on the server's `/api/transfers*`
//! through `ember_client`.
//!
//! What it shows is the server computer's transfer state (its offers, the transfers it is
//! receiving, its received files and its history) plus this app's own sends to it. A send reads
//! the files on this device, offers them, and streams them in chunks; the client resumes from
//! the receiver's verified offset after a dropped connection.
//!
//! The decisions live in [`Book`], a plain value tested without a server or a renderer. The
//! service around it only moves data: a poll task reads the snapshot (the server pushes no
//! transfer events yet), and each user action is one API call on the app's runtime.
//!
//! Limits, stated where the user meets them:
//! - A receiver is the server computer (`/api/transfers`) or an online registered computer
//!   reached over the transport, through the server's relay to its node
//!   (`TransferTarget::Computer`, `FR-F14`). Phones and other devices cannot receive yet
//!   ([`SEND_OTHER_DEVICE`]), nor a computer registered by plain HTTP ([`SEND_PLAIN_HTTP`]),
//!   because the relay would carry its bytes unencrypted (`FR-F6`).
//! - The app accepts its own offer to the server computer: the person sending through the owner
//!   API is the person who would answer the prompt on the same screen. A node accepts an owner's
//!   offer by itself (`FR-F15`), so the app never accepts for a computer. Offers from other
//!   devices still ask.
//! - A history row is shown as sent only for this run of the app; the server records every
//!   transfer from its own side, as received.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ember_client::api::Api;
use ember_client::transfers::{
    self as wire, LocalFile, RateMeter, TransferError, TransferResult, TransferStatus, TransferTarget,
    TransfersSnapshot, UploadEnd, UploadOptions,
};

use crate::ui::files::{
    next_state, Answer, Device, Direction, HistoryEntry, HistoryResult, Offer, ReceivedFile, Transfer,
    TransferAction, TransferService, TransferState,
};

/// The id of the server computer in the computers list (`ember_server::computers::LOCAL`).
pub const SERVER_DEVICE: &str = crate::ui::files::SERVER_COMPUTER_ID;

/// Why a phone or another device that is not a registered computer cannot receive yet.
pub const SEND_OTHER_DEVICE: &str = "휴대폰이나 다른 기기로는 아직 파일을 보낼 수 없습니다. 컴퓨터를 골라 주세요.";
/// Why a computer registered by plain HTTP cannot receive (`FR-F14` `InsecureTarget`).
pub const SEND_PLAIN_HTTP: &str = "이 컴퓨터는 암호화되지 않은 연결로 등록되어 있어서 파일을 보낼 수 없습니다.";
/// Why a computer that is not online cannot receive.
pub const SEND_OFFLINE: &str = "이 컴퓨터가 온라인이 아니어서 파일을 보낼 수 없습니다.";
/// Why nothing can be sent while the server does not answer.
pub const NOT_CONNECTED: &str = "서버에 연결되어 있지 않아 파일을 보낼 수 없습니다";
/// The server computer's name when the computers list does not have it.
pub const SERVER_NAME_FALLBACK: &str = crate::ui::files::SERVER_COMPUTER_NAME;

const ACTION_FAILED: &str = "요청을 서버에 전하지 못했습니다. 연결을 확인하고 다시 시도해 주세요.";

// ---------------------------------------------------------------------------------------------
// Where a send goes
// ---------------------------------------------------------------------------------------------

/// What the app knows about a chosen receiver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receiver {
    pub id: String,
    /// It is one of the registered computers (the server's `GET /computers`).
    pub computer: bool,
    /// The computer is reached over the transport (it has a peer id), not by a plain HTTP URL.
    pub peer: bool,
    pub online: bool,
}

/// The target a send to `r` goes through, or the reason it cannot go there.
pub fn target_of(r: &Receiver) -> Result<TransferTarget, &'static str> {
    if r.id == SERVER_DEVICE {
        return Ok(TransferTarget::Server);
    }
    if !r.computer {
        return Err(SEND_OTHER_DEVICE);
    }
    if !r.peer {
        return Err(SEND_PLAIN_HTTP);
    }
    if !r.online {
        return Err(SEND_OFFLINE);
    }
    Ok(TransferTarget::Computer(r.id.clone()))
}

/// The registered computers' transport ids, from the last `GET /computers` (`None`: plain HTTP).
static PEERS: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();

/// Records which registered computers are reached over the transport. Called with every
/// computers list the app reads.
pub fn note_computers(list: &[crate::server::ComputerStatus]) {
    let map = list.iter().map(|c| (c.id.clone(), c.local || c.peer.as_deref().is_some_and(|p| !p.is_empty()))).collect();
    *PEERS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner()) = map;
}

fn has_peer(id: &str) -> Option<bool> {
    PEERS.get()?.lock().unwrap_or_else(|e| e.into_inner()).get(id).copied()
}

/// Writes why a send to a computer failed to the log; the user sees only the fixed sentence.
fn log_failure(target: &TransferTarget, e: &TransferError) {
    if let TransferTarget::Computer(id) = target {
        tracing::warn!(computer = %id, error = ?e, "a file transfer to a computer failed");
    }
}

// ---------------------------------------------------------------------------------------------
// The book: everything the service knows, and what each action means
// ---------------------------------------------------------------------------------------------

/// One send started on this device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    /// The row's id on this device; the server's id comes with the offer.
    pub local_id: String,
    pub server_id: Option<String>,
    pub name: String,
    /// The receiver's name.
    pub device: String,
    /// Where the send goes: the server computer, or a computer through the relay.
    pub target: TransferTarget,
    pub paths: Vec<PathBuf>,
    /// Sizes and hashes, once read.
    pub prepared: Option<Vec<LocalFile>>,
    pub total: u64,
    pub done: u64,
    pub state: TransferState,
    /// Whether a retry continues the same transfer (`FR-F5`) or must offer again.
    pub resumable: bool,
}

/// The API call a user action on a transfer needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    Nothing,
    Pause(String),
    Resume(String),
    Cancel(String),
    /// Run the send with this local id again.
    Restart(String),
}

/// A history row of a relayed send, with the time it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntryAt {
    pub name: String,
    pub device: String,
    pub size: u64,
    pub result: HistoryResult,
    pub at_ms: i64,
}

#[derive(Debug, Default)]
pub struct Book {
    snapshot: Option<TransfersSnapshot>,
    /// Whether the last read of the snapshot succeeded.
    online: bool,
    outgoing: Vec<Outgoing>,
    /// Server ids of the sends finished in this run, with the receiver's name.
    sent: Vec<(String, String)>,
    /// Sends to other computers that ended in this run. The server's history holds only its own
    /// computer's transfers, so these rows name the receiving computer from here.
    relayed: Vec<HistoryEntryAt>,
    rates: HashMap<String, RateMeter>,
    seq: u64,
}

fn state_of(s: TransferStatus) -> TransferState {
    match s {
        TransferStatus::Queued => TransferState::Queued,
        TransferStatus::Sending => TransferState::Sending,
        TransferStatus::Paused => TransferState::Paused,
        TransferStatus::Done => TransferState::Done,
        TransferStatus::Failed => TransferState::Failed,
        TransferStatus::Cancelled => TransferState::Cancelled,
    }
}

fn result_of(r: TransferResult) -> HistoryResult {
    match r {
        TransferResult::Done => HistoryResult::Done,
        TransferResult::Failed => HistoryResult::Failed,
        TransferResult::Declined => HistoryResult::Declined,
        TransferResult::Cancelled => HistoryResult::Cancelled,
    }
}

/// The history groups a real record falls in. The screen groups rows by the first word of
/// `when` (the mockup's day label). The app has no local time zone to name a calendar day
/// with, so the groups are the last 24 hours and everything before.
pub const DAY_RECENT: &str = "최근";
pub const DAY_EARLIER: &str = "이전";

/// "최근 방금", "최근 10분 전", "이전 3일 전": the group, a space, then how long ago.
pub fn when_text(now_ms: i64, at_ms: i64) -> String {
    let r = crate::ui::cr::ko::relative_time(now_ms, at_ms);
    if r.is_empty() {
        return r;
    }
    let day = if now_ms - at_ms < 86_400_000 { DAY_RECENT } else { DAY_EARLIER };
    if r == "방금" { format!("{day} {r}") } else { format!("{day} {r} 전") }
}

/// "a.txt", or "a.txt 외 2개" for several files.
pub fn send_label(paths: &[PathBuf]) -> String {
    let base = |p: &Path| {
        p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string_lossy().into_owned())
    };
    match paths {
        [] => "파일".to_string(),
        [one] => base(one),
        [first, rest @ ..] => format!("{} 외 {}개", base(first), rest.len()),
    }
}

impl Book {
    /// The lists are known: the server answered at least once and says transfers work. It stays
    /// true when the server is lost afterwards, so the screen keeps what it last showed.
    pub fn ready(&self) -> bool {
        self.snapshot.as_ref().is_some_and(|s| s.ready)
    }

    /// A send can start: the lists are known and the server answered the last read.
    pub fn can_send(&self) -> bool {
        self.online && self.ready()
    }

    /// The server computer's transfer `server_id` is one of this device's sends. A node numbers
    /// its transfers on its own, so only sends to the server computer can match.
    fn owns(&self, server_id: &str) -> bool {
        self.outgoing.iter().any(|o| o.target == TransferTarget::Server && o.server_id.as_deref() == Some(server_id))
    }

    /// Takes a fresh snapshot. `true` when the screen has something new to draw.
    pub fn apply_snapshot(&mut self, snapshot: TransfersSnapshot, now_ms: i64) -> bool {
        let changed = !self.online || self.snapshot.as_ref() != Some(&snapshot);
        self.online = true;
        for t in &snapshot.transfers {
            self.rates.entry(t.id.clone()).or_default().observe(now_ms, t.done);
        }
        let relayed: Vec<String> = self
            .outgoing
            .iter()
            .filter(|o| o.target != TransferTarget::Server)
            .filter_map(|o| o.server_id.as_ref().map(|id| relay_rate_key(&o.target, id)))
            .collect();
        self.rates.retain(|id, _| relayed.contains(id) || snapshot.transfers.iter().any(|t| &t.id == id));
        follow(&mut self.outgoing, &TransferTarget::Server, &snapshot);
        self.snapshot = Some(snapshot);
        changed
    }

    /// The computers this device is sending to now, whose own transfer lists the poll reads.
    pub fn relay_targets(&self) -> Vec<TransferTarget> {
        let mut out: Vec<TransferTarget> = Vec::new();
        for o in &self.outgoing {
            if o.target != TransferTarget::Server && o.server_id.is_some() && !out.contains(&o.target) {
                out.push(o.target.clone());
            }
        }
        out
    }

    /// Takes a computer's transfer list (`transfers_at`): its rows of this device's sends follow
    /// the node's state and progress. Nothing else of it is shown.
    pub fn apply_relay_snapshot(&mut self, target: &TransferTarget, snapshot: &TransfersSnapshot, now_ms: i64) {
        for o in self.outgoing.iter().filter(|o| &o.target == target) {
            let Some(id) = &o.server_id else { continue };
            if let Some(t) = snapshot.transfers.iter().find(|t| &t.id == id) {
                self.rates.entry(relay_rate_key(target, id)).or_default().observe(now_ms, t.done);
            }
        }
        follow(&mut self.outgoing, target, snapshot);
    }

    /// The server did not answer. The last lists stay on screen; sending is blocked.
    pub fn mark_offline(&mut self) -> bool {
        let changed = self.online;
        self.online = false;
        changed
    }

    /// This device's sends first, then what the server computer is receiving from others.
    pub fn transfers(&self) -> Vec<Transfer> {
        let mut out: Vec<Transfer> = self
            .outgoing
            .iter()
            .rev()
            .map(|o| Transfer {
                id: o.local_id.clone(),
                name: o.name.clone(),
                device: o.device.clone(),
                direction: Direction::Send,
                total: o.total,
                done: o.done.min(o.total),
                bytes_per_sec: match o.state {
                    TransferState::Sending => o
                        .server_id
                        .as_ref()
                        .and_then(|id| self.rates.get(&relay_rate_key(&o.target, id)))
                        .and_then(|m| m.rate()),
                    _ => None,
                },
                state: o.state,
            })
            .collect();
        if let Some(s) = &self.snapshot {
            for t in s.transfers.iter().filter(|t| !self.owns(&t.id)) {
                let state = state_of(t.state);
                out.push(Transfer {
                    id: t.id.clone(),
                    name: t.name.clone(),
                    device: t.device.clone(),
                    direction: Direction::Receive,
                    total: t.total,
                    done: t.done.min(t.total),
                    bytes_per_sec: match state {
                        TransferState::Sending => self.rates.get(&t.id).and_then(|m| m.rate()),
                        _ => None,
                    },
                    state,
                });
            }
        }
        out
    }

    pub fn received(&self) -> Vec<ReceivedFile> {
        let Some(s) = &self.snapshot else { return Vec::new() };
        s.received.iter().map(|f| ReceivedFile { name: f.name.clone(), size: f.size, from: f.from.clone() }).collect()
    }

    pub fn history(&self, now_ms: i64) -> Vec<HistoryEntry> {
        let Some(s) = &self.snapshot else { return Vec::new() };
        let relayed = self.relayed.iter().rev().map(|h| HistoryEntry {
            name: h.name.clone(),
            device: h.device.clone(),
            direction: Direction::Send,
            size: h.size,
            result: h.result,
            when: when_text(now_ms, h.at_ms),
        });
        let recorded = s.history.iter().map(|h| {
            let mine = self.sent.iter().find(|(id, _)| id == &h.id);
            HistoryEntry {
                name: h.name.clone(),
                device: mine.map(|(_, device)| device.clone()).unwrap_or_else(|| h.device.clone()),
                direction: match (mine, h.direction) {
                    (Some(_), _) | (None, wire::TransferDirection::Send) => Direction::Send,
                    (None, wire::TransferDirection::Receive) => Direction::Receive,
                },
                size: h.size,
                result: result_of(h.result),
                when: when_text(now_ms, h.timestamp_ms),
            }
        });
        relayed.chain(recorded).collect()
    }

    /// Offers from other devices waiting for an answer, with the cleaned names only (`FR-F9`).
    pub fn offers(&self) -> Vec<Offer> {
        let Some(s) = &self.snapshot else { return Vec::new() };
        s.pending_offers
            .iter()
            .filter(|o| !self.owns(&o.id))
            .map(|o| Offer { id: o.id.clone(), from: o.from.clone(), files: o.cleaned_names.clone(), total: o.total })
            .collect()
    }

    /// The receiver's folder for received files (`FR-F8`).
    pub fn inbox(&self) -> Option<String> {
        self.snapshot.as_ref().map(|s| s.settings.received_dir.clone()).filter(|d| !d.is_empty())
    }

    /// Queues a send to the server computer and answers its local id, or says why it cannot be
    /// sent. Any other id is taken as a device that is not a registered computer.
    pub fn queue_send(&mut self, device_id: &str, device_name: &str, files: &[String]) -> Result<String, String> {
        let receiver = Receiver { id: device_id.to_string(), computer: device_id == SERVER_DEVICE, peer: true, online: true };
        self.queue_send_to(&receiver, device_name, files)
    }

    /// [`Book::queue_send`] with what the app knows about the receiver.
    pub fn queue_send_to(&mut self, receiver: &Receiver, device_name: &str, files: &[String]) -> Result<String, String> {
        if !self.can_send() {
            return Err(NOT_CONNECTED.to_string());
        }
        let target = target_of(receiver).map_err(str::to_string)?;
        if files.is_empty() {
            return Err(TransferError::NoFiles.text_ko());
        }
        self.seq += 1;
        let local_id = format!("send-{}", self.seq);
        let paths: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
        self.outgoing.push(Outgoing {
            local_id: local_id.clone(),
            server_id: None,
            name: send_label(&paths),
            device: device_name.to_string(),
            target,
            paths,
            prepared: None,
            total: 0,
            done: 0,
            state: TransferState::Queued,
            resumable: false,
        });
        Ok(local_id)
    }

    /// What a user action needs. An action the transfer's state does not allow needs nothing.
    pub fn plan_control(&mut self, id: &str, action: TransferAction) -> Plan {
        self.plan_control_at(id, action).1
    }

    /// [`Book::plan_control`] with the target the call goes to.
    pub fn plan_control_at(&mut self, id: &str, action: TransferAction) -> (TransferTarget, Plan) {
        if let Some(pos) = self.outgoing.iter().position(|o| o.local_id == id) {
            let o = &mut self.outgoing[pos];
            let target = o.target.clone();
            if next_state(o.state, action).is_none() {
                return (target, Plan::Nothing);
            }
            let plan = match action {
                TransferAction::Pause => o.server_id.clone().map(Plan::Pause).unwrap_or(Plan::Nothing),
                TransferAction::Resume => o.server_id.clone().map(Plan::Resume).unwrap_or(Plan::Nothing),
                TransferAction::Cancel => {
                    // The row goes at once; the running send sees it gone and stops.
                    let gone = self.outgoing.remove(pos);
                    gone.server_id.map(Plan::Cancel).unwrap_or(Plan::Nothing)
                }
                TransferAction::Retry => {
                    o.state = TransferState::Queued;
                    if !o.resumable {
                        o.done = 0;
                    }
                    Plan::Restart(o.local_id.clone())
                }
                _ => Plan::Nothing,
            };
            return (target, plan);
        }
        let server = TransferTarget::Server;
        let Some(t) = self.snapshot.as_ref().and_then(|s| s.transfers.iter().find(|t| t.id == id)) else {
            return (server, Plan::Nothing);
        };
        if next_state(state_of(t.state), action).is_none() {
            return (server, Plan::Nothing);
        }
        let plan = match action {
            TransferAction::Pause => Plan::Pause(t.id.clone()),
            // The receiver takes chunks again; the sending device continues from its offset.
            TransferAction::Resume | TransferAction::Retry => Plan::Resume(t.id.clone()),
            TransferAction::Cancel => Plan::Cancel(t.id.clone()),
            _ => Plan::Nothing,
        };
        (server, plan)
    }

    fn outgoing_mut(&mut self, local_id: &str) -> Option<&mut Outgoing> {
        self.outgoing.iter_mut().find(|o| o.local_id == local_id)
    }

    /// The send ended: done, declined and cancelled leave the list, a failure stays for a retry.
    pub fn finish_send(&mut self, local_id: &str, end: &Result<UploadEnd, TransferError>) {
        self.finish_send_at(local_id, end, crate::demo::now_ms())
    }

    /// [`Book::finish_send`] at `now_ms`. A send to another computer that ended becomes a history
    /// row naming that computer.
    pub fn finish_send_at(&mut self, local_id: &str, end: &Result<UploadEnd, TransferError>, now_ms: i64) {
        let Some(pos) = self.outgoing.iter().position(|o| o.local_id == local_id) else { return };
        let ended = match end {
            Ok(UploadEnd::Done(_)) => Some(HistoryResult::Done),
            Ok(UploadEnd::Declined) => Some(HistoryResult::Declined),
            _ => None,
        };
        if let (Some(result), o) = (ended, &self.outgoing[pos]) {
            if o.target != TransferTarget::Server {
                self.relayed.push(HistoryEntryAt {
                    name: o.name.clone(),
                    device: o.device.clone(),
                    size: o.total,
                    result,
                    at_ms: now_ms,
                });
            }
        }
        match end {
            Ok(UploadEnd::Done(_)) => {
                let o = self.outgoing.remove(pos);
                if let (Some(id), TransferTarget::Server) = (o.server_id, &o.target) {
                    self.sent.push((id, o.device));
                }
            }
            Ok(UploadEnd::Declined) | Ok(UploadEnd::Cancelled) => {
                self.outgoing.remove(pos);
            }
            Err(e) => {
                let o = &mut self.outgoing[pos];
                o.state = TransferState::Failed;
                o.resumable = e.can_resume() && o.server_id.is_some();
            }
        }
    }
}

/// The rate meter's key: a node's transfer ids may repeat the server's.
fn relay_rate_key(target: &TransferTarget, id: &str) -> String {
    match target {
        TransferTarget::Server => id.to_string(),
        TransferTarget::Computer(c) => format!("{c}/{id}"),
    }
}

/// The rows of sends to `target` follow `snapshot`, its receiver's list.
fn follow(outgoing: &mut [Outgoing], target: &TransferTarget, snapshot: &TransfersSnapshot) {
    for o in outgoing.iter_mut().filter(|o| &o.target == target) {
        let Some(t) = o.server_id.as_ref().and_then(|id| snapshot.transfers.iter().find(|t| &t.id == id)) else {
            continue;
        };
        // A send that failed here stays failed, whatever the receiver still holds.
        if matches!(o.state, TransferState::Sending | TransferState::Paused) {
            o.state = if t.state == TransferStatus::Paused { TransferState::Paused } else { TransferState::Sending };
            o.done = o.done.max(t.done);
        }
    }
}

/// What the user is told when a send ends, if anything.
pub fn end_notice(name: &str, end: &Result<UploadEnd, TransferError>) -> Option<String> {
    match end {
        Ok(UploadEnd::Done(_)) => Some(format!("{name} 전송을 마쳤습니다.")),
        Ok(UploadEnd::Declined) => Some(format!("받는 쪽에서 {name} 전송을 거절했습니다.")),
        // The user cancelled it, here or on the receiver: the row leaving is the answer.
        Ok(UploadEnd::Cancelled) => None,
        Err(e) => Some(e.text_ko()),
    }
}

// ---------------------------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------------------------

/// [`TransferService`] on the server's transfer API.
pub struct ServerTransferService {
    book: Arc<Mutex<Book>>,
    api: Api,
}

fn lock(book: &Mutex<Book>) -> std::sync::MutexGuard<'_, Book> {
    book.lock().unwrap_or_else(|e| e.into_inner())
}

/// Redraws the 파일 screen (it reads the launcher revision; the service has no signal of its own).
fn redraw() {
    if let Some(s) = crate::services::try_services() {
        crate::ui::coexist::SharedLive::bump(&s.ui.live.launcher);
    }
}

fn notify(text: impl Into<String>) {
    if crate::services::try_services().is_some() {
        crate::ui::cr::notice::post(text);
    }
}

async fn refresh(book: &Mutex<Book>, api: &Api) -> Result<(), TransferError> {
    let targets = lock(book).relay_targets();
    for target in targets {
        // A computer that does not answer leaves its rows as they are; the upload reports it.
        if let Ok(snapshot) = api.transfers_at(&target).await {
            lock(book).apply_relay_snapshot(&target, &snapshot, crate::demo::now_ms());
        }
    }
    match api.transfers().await {
        Ok(snapshot) => {
            if lock(book).apply_snapshot(snapshot, crate::demo::now_ms()) {
                redraw();
            }
            Ok(())
        }
        Err(e) => {
            if lock(book).mark_offline() {
                redraw();
            }
            Err(TransferError::from_api(&e))
        }
    }
}

/// Reads the snapshot for as long as the app runs: every second while something moves or
/// waits for an answer, every five otherwise, and rarely against a server without the API.
async fn poll(book: Arc<Mutex<Book>>, api: Api) {
    loop {
        let wait = match refresh(&book, &api).await {
            Ok(()) => {
                let b = lock(&book);
                let busy = !b.outgoing.is_empty()
                    || b.snapshot.as_ref().is_some_and(|s| !s.transfers.is_empty() || !s.pending_offers.is_empty());
                if busy { 1 } else { 5 }
            }
            Err(TransferError::Unsupported) => 60,
            Err(_) => 5,
        };
        if wait == 1 {
            // Progress and rates move without the snapshot's shape changing.
            redraw();
        }
        tokio::time::sleep(Duration::from_secs(wait)).await;
    }
}

/// One send, from reading the files to the end. Run again for a retry.
async fn run_send(book: Arc<Mutex<Book>>, api: Api, local_id: String, from: String) {
    let end = send_steps(&book, &api, &local_id, &from).await;
    if let Err(e) = &end {
        let target = lock(&book).outgoing_mut(&local_id).map(|o| o.target.clone());
        if let Some(target) = target {
            log_failure(&target, e);
        }
    }
    let name = {
        let mut b = lock(&book);
        let name = b.outgoing_mut(&local_id).map(|o| o.name.clone());
        b.finish_send(&local_id, &end);
        name
    };
    match name {
        Some(name) => {
            if let Some(text) = end_notice(&name, &end) {
                notify(text);
            }
        }
        // The row was cancelled while the send ran: its cancel call already told the receiver.
        None => {}
    }
    let _ = refresh(&book, &api).await;
    redraw();
}

async fn send_steps(
    book: &Arc<Mutex<Book>>,
    api: &Api,
    local_id: &str,
    from: &str,
) -> Result<UploadEnd, TransferError> {
    let Some(row) = lock(book).outgoing_mut(local_id).map(|o| o.clone()) else {
        return Ok(UploadEnd::Cancelled);
    };
    let files = match row.prepared {
        Some(files) => files,
        None => {
            let files = wire::prepare(&row.paths).await?;
            match lock(book).outgoing_mut(local_id) {
                Some(o) => {
                    o.total = wire::total_bytes(&files);
                    o.prepared = Some(files.clone());
                }
                None => return Ok(UploadEnd::Cancelled),
            }
            redraw();
            files
        }
    };
    let target = row.target.clone();
    let id = match row.server_id {
        Some(id) if row.resumable => id,
        stale => {
            if let Some(old) = stale {
                // A transfer that cannot continue: drop what the receiver kept and offer again.
                let _ = api.cancel_transfer_at(&target, &old).await;
            }
            let created = wire::offer_to(api, &target, from, None, &files).await?;
            let recorded = match lock(book).outgoing_mut(local_id) {
                Some(o) => {
                    o.server_id = Some(created.id.clone());
                    true
                }
                None => false,
            };
            if !recorded {
                // Cancelled before the offer had an id on record: withdraw it.
                let _ = api.cancel_transfer_at(&target, &created.id).await;
                return Ok(UploadEnd::Cancelled);
            }
            // Only the server computer's offer is accepted here. A node accepts the owner's
            // offer itself (`FR-F15`) and accepting for a computer is not relayed (403).
            if created.status == "offered" && target == TransferTarget::Server {
                api.accept_transfer_offer(&created.id).await.map_err(|e| TransferError::from_api(&e))?;
            }
            created.id
        }
    };
    if let Some(o) = lock(book).outgoing_mut(local_id) {
        o.state = TransferState::Sending;
    }
    redraw();

    let progress_book = book.clone();
    let progress_id = local_id.to_string();
    let mut last_draw = Instant::now();
    let mut progress = move |done: u64| {
        if let Some(o) = lock(&progress_book).outgoing_mut(&progress_id) {
            o.done = done;
        }
        if last_draw.elapsed() >= Duration::from_millis(250) {
            last_draw = Instant::now();
            redraw();
        }
    };
    wire::upload_to(api, &target, &id, &files, &UploadOptions::default(), &mut progress).await
}

impl ServerTransferService {
    pub fn new(api: Api) -> ServerTransferService {
        ServerTransferService { book: Arc::new(Mutex::new(Book::default())), api }
    }

    fn spawn(&self, work: impl std::future::Future<Output = ()> + Send + 'static) {
        if let Some(s) = crate::services::try_services() {
            s.spawn(work);
        }
    }

    /// One API call for a user action, then a fresh snapshot.
    fn call(&self, target: TransferTarget, plan: Plan) {
        let (book, api) = (self.book.clone(), self.api.clone());
        self.spawn(async move {
            let result = match &plan {
                Plan::Nothing | Plan::Restart(_) => return,
                Plan::Pause(id) => api.transfer_control_at(&target, id, "pause").await.map(|_| ()),
                Plan::Resume(id) => api.transfer_control_at(&target, id, "resume").await.map(|_| ()),
                Plan::Cancel(id) => api.cancel_transfer_at(&target, id).await.map(|_| ()),
            };
            // 404 and 409: the transfer already moved on; the fresh snapshot shows where to.
            if let Err(e) = result {
                if !matches!(e.status(), Some(404) | Some(409)) {
                    notify(ACTION_FAILED);
                }
            }
            let _ = refresh(&book, &api).await;
            redraw();
        });
    }

    fn start_send(&self, local_id: String) {
        let from = crate::terminal_stream::device_name();
        self.spawn(run_send(self.book.clone(), self.api.clone(), local_id, from));
    }
}

impl TransferService for ServerTransferService {
    fn ready(&self) -> bool {
        lock(&self.book).ready()
    }

    /// `None`: the screen lists the registered computers with their real reachability. The
    /// transfer API's own device list reports every device as online, so it is not shown.
    fn devices(&self) -> Option<Vec<Device>> {
        None
    }

    fn transfers(&self) -> Vec<Transfer> {
        lock(&self.book).transfers()
    }

    fn received(&self) -> Vec<ReceivedFile> {
        lock(&self.book).received()
    }

    fn history(&self) -> Vec<HistoryEntry> {
        lock(&self.book).history(crate::demo::now_ms())
    }

    fn pending_offers(&self) -> Vec<Offer> {
        lock(&self.book).offers()
    }

    fn answer(&self, offer_id: &str, answer: Answer) {
        let (book, api, id) = (self.book.clone(), self.api.clone(), offer_id.to_string());
        self.spawn(async move {
            let result = match answer {
                Answer::Accept => api.accept_transfer_offer(&id).await.map(|_| ()),
                Answer::Decline => api.decline_transfer_offer(&id).await.map(|_| ()),
            };
            if let Err(e) = result {
                // 404: the sender withdrew it, or another client answered first.
                notify(if e.status() == Some(404) { "이미 끝난 요청입니다." } else { ACTION_FAILED });
            }
            let _ = refresh(&book, &api).await;
            redraw();
        });
    }

    fn control(&self, transfer_id: &str, action: TransferAction) {
        let (target, plan) = lock(&self.book).plan_control_at(transfer_id, action);
        match plan {
            Plan::Nothing => {}
            Plan::Restart(local_id) => self.start_send(local_id),
            other => self.call(target, other),
        }
    }

    fn can_send(&self) -> bool {
        lock(&self.book).can_send()
    }

    fn send(&self, device_id: &str, files: Vec<String>) -> Result<(), String> {
        let computer = crate::services::try_services().and_then(|s| {
            s.client.read(|st| {
                st.computers().iter().find(|c| c.id == device_id).map(|c| (c.name.clone(), c.reachability))
            })
        });
        let device_name = computer
            .as_ref()
            .map(|(name, _)| crate::ui::files::computer_display_name(device_id, name))
            .unwrap_or_else(|| {
                if device_id == SERVER_DEVICE { SERVER_NAME_FALLBACK.to_string() } else { device_id.to_string() }
            });
        let receiver = Receiver {
            id: device_id.to_string(),
            computer: device_id == SERVER_DEVICE || computer.is_some(),
            // Before the first computers list is read, a computer counts as plain HTTP: the
            // server would refuse it anyway (`InsecureTarget`).
            peer: has_peer(device_id).unwrap_or(false),
            online: computer.as_ref().is_some_and(|(_, r)| *r == ember_client::state::Reachability::Online),
        };
        let local_id = lock(&self.book).queue_send_to(&receiver, &device_name, &files)?;
        self.start_send(local_id);
        Ok(())
    }

    /// Only the peer-to-peer transport encrypts end to end; plain HTTP to the server does not.
    fn encrypted(&self) -> bool {
        self.api.peer().is_some()
    }

    fn inbox(&self) -> Option<String> {
        lock(&self.book).inbox()
    }
}

static SERVICE: OnceLock<ServerTransferService> = OnceLock::new();

/// The real service, once the app's services exist and it is not serving demo data. The first
/// call starts the poll task.
pub fn server_service() -> Option<&'static ServerTransferService> {
    let s = crate::services::try_services()?;
    if s.demo.is_some() {
        return None;
    }
    Some(SERVICE.get_or_init(|| {
        let svc = ServerTransferService::new(s.client.api().clone());
        s.spawn(poll(svc.book.clone(), svc.api.clone()));
        svc
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_800_000_000_000;

    fn snapshot(transfers: serde_json::Value, offers: serde_json::Value) -> TransfersSnapshot {
        serde_json::from_value(json!({
            "ready": true,
            "devices": [{ "id": "p1", "name": "BREW iPhone", "kind": "device", "online": true, "peer_id": "p1" }],
            "transfers": transfers,
            "pending_offers": offers,
            "received": [{ "name": "run.sh", "path": "/data/received/run.sh", "size": 21, "sha256": "ab", "from": "BREW iPhone" }],
            "history": [
                { "id": "tr-7", "name": "run.sh", "device": "BREW iPhone", "direction": "receive", "size": 21, "result": "done", "timestamp_ms": NOW - 600_000 },
                { "id": "tr-6", "name": "old.zip", "device": "BREW iPhone", "direction": "receive", "size": 9, "result": "declined", "timestamp_ms": NOW - 10_000 }
            ],
            "settings": { "receive_mode": "ask", "auto_accept_peers": [], "max_bytes": 4294967296u64, "received_dir": "/data/received" }
        }))
        .expect("snapshot decodes")
    }

    fn receiving(id: &str, done: u64, state: &str) -> serde_json::Value {
        json!({
            "id": id, "name": "photo.jpg", "device": "BREW iPhone", "direction": "receive",
            "total": 1000, "done": done, "bytes_per_sec": null, "state": state,
            "files": [{ "name": "photo.jpg", "size": 1000 }]
        })
    }

    fn offer(id: &str) -> serde_json::Value {
        json!({
            "id": id, "from": "BREW iPhone", "peer_id": null,
            "files": [{ "name": "../.run.sh", "size": 21 }],
            "cleaned_names": ["run.sh"], "total": 21, "created_at": NOW
        })
    }

    fn online_book() -> Book {
        let mut b = Book::default();
        assert!(b.apply_snapshot(snapshot(json!([]), json!([])), NOW));
        b
    }

    #[test]
    fn fr_l27_8_before_the_first_answer_nothing_is_ready_or_invented() {
        let mut b = Book::default();
        assert!(!b.ready());
        assert!(b.transfers().is_empty());
        assert!(b.received().is_empty());
        assert!(b.history(NOW).is_empty());
        assert!(b.offers().is_empty());
        assert_eq!(b.inbox(), None);
        assert_eq!(b.queue_send(SERVER_DEVICE, "mini", &["/tmp/a.txt".into()]), Err(NOT_CONNECTED.to_string()));
    }

    #[test]
    fn fr_l27_8_the_snapshot_becomes_the_screen_lists() {
        let mut b = Book::default();
        b.apply_snapshot(snapshot(json!([receiving("tr-9", 250, "sending")]), json!([offer("tr-10")])), NOW);
        assert!(b.ready());

        let t = b.transfers();
        assert_eq!(t.len(), 1);
        assert_eq!((t[0].id.as_str(), t[0].direction, t[0].state), ("tr-9", Direction::Receive, TransferState::Sending));
        assert_eq!((t[0].done, t[0].total, t[0].device.as_str()), (250, 1000, "BREW iPhone"));

        assert_eq!(b.received(), vec![ReceivedFile { name: "run.sh".into(), size: 21, from: "BREW iPhone".into() }]);
        assert_eq!(b.inbox().as_deref(), Some("/data/received"));

        let h = b.history(NOW);
        assert_eq!((h[0].result, h[0].when.as_str()), (HistoryResult::Done, "최근 10분 전"));
        assert_eq!((h[1].result, h[1].when.as_str()), (HistoryResult::Declined, "최근 방금"));
        assert_eq!(h[0].direction, Direction::Receive);

        // Only the cleaned name reaches the prompt (FR-F9).
        let o = b.offers();
        assert_eq!(o, vec![Offer { id: "tr-10".into(), from: "BREW iPhone".into(), files: vec!["run.sh".into()], total: 21 }]);
    }

    #[test]
    fn fr_l27_8_a_lost_server_blocks_sending_and_keeps_the_lists() {
        let mut b = Book::default();
        b.apply_snapshot(snapshot(json!([receiving("tr-9", 250, "sending")]), json!([])), NOW);
        assert!(b.mark_offline());
        assert!(!b.mark_offline(), "nothing new to draw the second time");
        // The lists stay known, so empty sections keep their own text and rows stay (state G).
        assert!(b.ready());
        assert!(!b.can_send());
        assert_eq!(b.transfers().len(), 1);
        assert_eq!(b.received().len(), 1);
        assert_eq!(b.history(NOW).len(), 2);
        assert_eq!(b.inbox().as_deref(), Some("/data/received"));
        assert_eq!(b.queue_send(SERVER_DEVICE, "mini", &["/tmp/a.txt".into()]), Err(NOT_CONNECTED.to_string()));
        // Back: the same snapshot is still a change, because sending is possible again.
        assert!(b.apply_snapshot(snapshot(json!([receiving("tr-9", 250, "sending")]), json!([])), NOW));
        assert!(b.can_send());
    }

    #[test]
    fn fr_l27_8_a_send_is_refused_with_its_reason() {
        let mut b = online_book();
        assert_eq!(b.queue_send("c2", "lab-pc", &["/tmp/a.txt".into()]), Err(SEND_OTHER_DEVICE.to_string()));
        assert_eq!(b.queue_send(SERVER_DEVICE, "mini", &[]), Err("보낼 파일을 선택해야 합니다.".to_string()));
        assert!(b.transfers().is_empty(), "a refused send leaves no row");
    }

    #[test]
    fn fr_l27_8_a_send_is_a_queued_row_named_after_its_files() {
        let mut b = online_book();
        let id = b.queue_send(SERVER_DEVICE, "mini", &["/tmp/a.txt".into(), "/tmp/b.txt".into(), "/tmp/c.txt".into()]).unwrap();
        let t = b.transfers();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].id, id);
        assert_eq!((t[0].name.as_str(), t[0].device.as_str()), ("a.txt 외 2개", "mini"));
        assert_eq!((t[0].direction, t[0].state, t[0].done, t[0].total), (Direction::Send, TransferState::Queued, 0, 0));
        assert_eq!(send_label(&[PathBuf::from("/x/one.png")]), "one.png");
    }

    /// A send that reached the server as `tr-3`.
    fn sending_book() -> (Book, String) {
        let mut b = online_book();
        let id = b.queue_send(SERVER_DEVICE, "mini", &["/tmp/a.bin".into()]).unwrap();
        let o = b.outgoing_mut(&id).unwrap();
        o.server_id = Some("tr-3".into());
        o.total = 1000;
        o.state = TransferState::Sending;
        (b, id)
    }

    #[test]
    fn fr_l27_8_my_send_is_one_row_and_never_a_prompt_for_myself() {
        let (mut b, id) = sending_book();
        // The server lists the same transfer from its side, and first as a pending offer.
        b.apply_snapshot(snapshot(json!([]), json!([offer("tr-3"), offer("tr-4")])), NOW);
        assert_eq!(b.offers().iter().map(|o| o.id.as_str()).collect::<Vec<_>>(), vec!["tr-4"]);

        b.apply_snapshot(snapshot(json!([receiving("tr-3", 400, "sending"), receiving("tr-9", 1, "sending")]), json!([])), NOW);
        let t = b.transfers();
        assert_eq!(t.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), vec![id.as_str(), "tr-9"]);
        assert_eq!((t[0].direction, t[0].done), (Direction::Send, 400));
    }

    #[test]
    fn fr_l27_8_progress_and_rate_follow_the_snapshots() {
        let (mut b, _) = sending_book();
        b.apply_snapshot(snapshot(json!([receiving("tr-3", 100, "sending")]), json!([])), NOW);
        assert_eq!(b.transfers()[0].bytes_per_sec, None, "one reading gives no rate");
        b.apply_snapshot(snapshot(json!([receiving("tr-3", 600, "sending")]), json!([])), NOW + 1000);
        let t = &b.transfers()[0];
        assert_eq!((t.done, t.bytes_per_sec), (600, Some(500)));
        assert_eq!(crate::ui::files::percent(t), 60);
        // Paused: no rate is shown, and the row follows the receiver's state.
        b.apply_snapshot(snapshot(json!([receiving("tr-3", 600, "paused")]), json!([])), NOW + 2000);
        let t = &b.transfers()[0];
        assert_eq!((t.state, t.bytes_per_sec), (TransferState::Paused, None));
        b.apply_snapshot(snapshot(json!([receiving("tr-3", 600, "sending")]), json!([])), NOW + 3000);
        assert_eq!(b.transfers()[0].state, TransferState::Sending);
    }

    #[test]
    fn fr_l27_8_actions_become_calls_only_where_the_state_allows() {
        let (mut b, id) = sending_book();
        b.apply_snapshot(snapshot(json!([receiving("tr-3", 100, "sending"), receiving("tr-9", 1, "paused")]), json!([])), NOW);
        // My send, by its local id, acts on the server's id.
        assert_eq!(b.plan_control(&id, TransferAction::Pause), Plan::Pause("tr-3".into()));
        assert_eq!(b.plan_control(&id, TransferAction::Resume), Plan::Nothing, "it is not paused");
        assert_eq!(b.plan_control(&id, TransferAction::Retry), Plan::Nothing);
        // Another device's transfer into the server.
        assert_eq!(b.plan_control("tr-9", TransferAction::Resume), Plan::Resume("tr-9".into()));
        assert_eq!(b.plan_control("tr-9", TransferAction::Pause), Plan::Nothing);
        assert_eq!(b.plan_control("tr-9", TransferAction::Cancel), Plan::Cancel("tr-9".into()));
        assert_eq!(b.plan_control("nope", TransferAction::Cancel), Plan::Nothing);
    }

    #[test]
    fn fr_l27_8_cancel_removes_my_row_at_once() {
        let (mut b, id) = sending_book();
        assert_eq!(b.plan_control(&id, TransferAction::Cancel), Plan::Cancel("tr-3".into()));
        assert!(b.transfers().is_empty());
        // The running send then ends: nothing comes back.
        b.finish_send(&id, &Ok(UploadEnd::Cancelled));
        assert!(b.transfers().is_empty());

        // Before the server knows it: no call is needed.
        let queued = b.queue_send(SERVER_DEVICE, "mini", &["/tmp/a.bin".into()]).unwrap();
        assert_eq!(b.plan_control(&queued, TransferAction::Cancel), Plan::Nothing);
        assert!(b.transfers().is_empty());
    }

    #[test]
    fn fr_l27_8_a_dropped_connection_fails_the_row_and_retry_continues_it() {
        let (mut b, id) = sending_book();
        b.outgoing_mut(&id).unwrap().done = 400;
        b.finish_send(&id, &Err(TransferError::Connection));
        assert_eq!(b.transfers()[0].state, TransferState::Failed);
        // The receiver still lists it as sending: the row stays failed until the user retries.
        b.apply_snapshot(snapshot(json!([receiving("tr-3", 400, "sending")]), json!([])), NOW);
        assert_eq!(b.transfers()[0].state, TransferState::Failed);
        assert_eq!(b.plan_control(&id, TransferAction::Pause), Plan::Nothing);

        assert_eq!(b.plan_control(&id, TransferAction::Retry), Plan::Restart(id.clone()));
        let t = &b.transfers()[0];
        assert_eq!((t.state, t.done), (TransferState::Queued, 400), "it continues, it does not start over");
        assert!(b.outgoing_mut(&id).unwrap().resumable);
    }

    #[test]
    fn fr_l27_8_a_failed_check_retries_from_the_start() {
        let (mut b, id) = sending_book();
        b.outgoing_mut(&id).unwrap().done = 1000;
        b.finish_send(&id, &Err(TransferError::HashMismatch));
        assert_eq!(b.plan_control(&id, TransferAction::Retry), Plan::Restart(id.clone()));
        let o = b.outgoing_mut(&id).unwrap();
        assert!(!o.resumable);
        assert_eq!(o.done, 0);
    }

    #[test]
    fn fr_l27_8_a_finished_send_leaves_the_list_and_reads_as_sent_in_the_history() {
        let (mut b, id) = sending_book();
        b.finish_send(&id, &Ok(UploadEnd::Done(Vec::new())));
        assert!(b.transfers().is_empty());
        let mut s = snapshot(json!([]), json!([]));
        s.history[0].id = "tr-3".into();
        b.apply_snapshot(s, NOW);
        let h = b.history(NOW);
        assert_eq!((h[0].direction, h[0].device.as_str()), (Direction::Send, "mini"));
        assert_eq!(h[1].direction, Direction::Receive);
    }

    #[test]
    fn fr_l27_8_texts_are_polite_korean_without_an_em_dash() {
        let ends: [Result<UploadEnd, TransferError>; 4] = [
            Ok(UploadEnd::Done(Vec::new())),
            Ok(UploadEnd::Declined),
            Err(TransferError::Connection),
            Err(TransferError::TooLarge),
        ];
        let mut all: Vec<String> = ends.iter().filter_map(|e| end_notice("a.txt", e)).collect();
        assert_eq!(all.len(), 4);
        assert_eq!(end_notice("a.txt", &Ok(UploadEnd::Cancelled)), None);
        all.extend(
            [SEND_OTHER_DEVICE, SEND_PLAIN_HTTP, SEND_OFFLINE, NOT_CONNECTED, ACTION_FAILED, SERVER_NAME_FALLBACK]
                .map(String::from),
        );
        for text in &all {
            assert!(!text.contains('\u{2014}'), "{text}");
            assert!(text.chars().any(|c| ('가'..='힣').contains(&c)), "{text}");
        }
        assert_eq!(SERVER_NAME_FALLBACK, "이 서버");
        for text in all.iter().filter(|t| t.as_str() != SERVER_NAME_FALLBACK) {
            assert!(text.contains("니다") || text.contains("주세요"), "not 합니다체: {text}");
        }
        assert_eq!(when_text(NOW, NOW - 3 * 3_600_000), "최근 3시간 전");
        assert_eq!(when_text(NOW, NOW - 3 * 86_400_000), "이전 3일 전");
        assert_eq!(when_text(NOW, 0), "");
    }

    #[test]
    fn fr_l27_8_history_rows_group_under_a_day_label_like_the_mockup() {
        // The screen takes the first word as the group label and the rest as the row's time:
        // an empty label would leave the label column blank beside the rows.
        for at in [NOW - 1000, NOW - 600_000, NOW - 5 * 3_600_000, NOW - 2 * 86_400_000, NOW - 30 * 86_400_000] {
            let when = when_text(NOW, at);
            let (day, time) = crate::ui::cr::files::split_when(&when);
            assert!(day == DAY_RECENT || day == DAY_EARLIER, "{when}");
            assert!(!time.is_empty(), "{when}");
        }
        assert!(!DAY_RECENT.contains(' ') && !DAY_EARLIER.contains(' '));
    }

    fn computer(id: &str, peer: bool, online: bool) -> Receiver {
        Receiver { id: id.into(), computer: true, peer, online }
    }

    #[test]
    fn fr_l27_8_a_send_goes_to_the_server_or_to_an_online_computer_with_a_peer() {
        let server = Receiver { id: SERVER_DEVICE.into(), computer: true, peer: false, online: false };
        assert_eq!(target_of(&server), Ok(TransferTarget::Server), "the server computer keeps /api/transfers");
        assert_eq!(target_of(&computer("c2", true, true)), Ok(TransferTarget::Computer("c2".into())));

        let mut b = online_book();
        let id = b.queue_send_to(&computer("c2", true, true), "lab-pc", &["/tmp/a.txt".into()]).unwrap();
        assert_eq!(b.outgoing_mut(&id).unwrap().target, TransferTarget::Computer("c2".into()));
        let t = b.transfers();
        assert_eq!((t[0].device.as_str(), t[0].direction), ("lab-pc", Direction::Send));
    }

    #[test]
    fn fr_l27_8_phones_other_devices_and_plain_http_computers_are_refused() {
        let phone = Receiver { id: "p1".into(), computer: false, peer: true, online: true };
        assert_eq!(target_of(&phone), Err(SEND_OTHER_DEVICE));
        assert_eq!(target_of(&computer("c3", false, true)), Err(SEND_PLAIN_HTTP));
        assert_eq!(target_of(&computer("c4", true, false)), Err(SEND_OFFLINE));

        let mut b = online_book();
        for (r, why) in [(phone, SEND_OTHER_DEVICE), (computer("c3", false, true), SEND_PLAIN_HTTP)] {
            assert_eq!(b.queue_send_to(&r, "x", &["/tmp/a.txt".into()]), Err(why.to_string()));
        }
        assert!(b.transfers().is_empty(), "a refused send leaves no row");
        // The plain HTTP refusal says the same as the server's own InsecureTarget answer.
        assert_eq!(SEND_PLAIN_HTTP, TransferError::InsecureTarget.text_ko());
    }

    #[test]
    fn fr_l27_8_insecure_target_and_no_space_are_fixed_korean_sentences() {
        assert_eq!(
            end_notice("a.txt", &Err(TransferError::InsecureTarget)).as_deref(),
            Some("이 컴퓨터는 암호화되지 않은 연결로 등록되어 있어서 파일을 보낼 수 없습니다.")
        );
        assert_eq!(
            end_notice("a.txt", &Err(TransferError::NoSpace)).as_deref(),
            Some("받는 컴퓨터의 저장 공간이 부족해서 보낼 수 없습니다.")
        );
    }

    /// A send that reached computer `c2`'s node as `tr-3` (the server has its own `tr-3`).
    fn relayed_book() -> (Book, String) {
        let mut b = online_book();
        let id = b.queue_send_to(&computer("c2", true, true), "lab-pc", &["/tmp/a.bin".into()]).unwrap();
        let o = b.outgoing_mut(&id).unwrap();
        o.server_id = Some("tr-3".into());
        o.total = 1000;
        o.state = TransferState::Sending;
        (b, id)
    }

    #[test]
    fn fr_l27_8_a_computer_send_follows_its_node_and_not_the_server() {
        let (mut b, id) = relayed_book();
        assert_eq!(b.relay_targets(), vec![TransferTarget::Computer("c2".into())]);
        // The server's own tr-3 is another transfer: it stays a receive row and an offer to answer.
        b.apply_snapshot(snapshot(json!([receiving("tr-3", 900, "paused")]), json!([offer("tr-3")])), NOW);
        assert_eq!(b.offers().len(), 1);
        let t = b.transfers();
        assert_eq!(t.len(), 2);
        assert_eq!((t[0].id.as_str(), t[0].state, t[0].done), (id.as_str(), TransferState::Sending, 0));
        // The node's list moves the row.
        b.apply_relay_snapshot(&TransferTarget::Computer("c2".into()), &snapshot(json!([receiving("tr-3", 100, "sending")]), json!([])), NOW);
        b.apply_relay_snapshot(&TransferTarget::Computer("c2".into()), &snapshot(json!([receiving("tr-3", 600, "paused")]), json!([])), NOW + 1000);
        assert_eq!((b.transfers()[0].state, b.transfers()[0].done), (TransferState::Paused, 600));
        // Actions go to the computer.
        assert_eq!(b.plan_control_at(&id, TransferAction::Resume), (TransferTarget::Computer("c2".into()), Plan::Resume("tr-3".into())));
    }

    #[test]
    fn fr_l27_8_history_names_the_receiving_computer() {
        let (mut b, id) = relayed_book();
        b.finish_send_at(&id, &Ok(UploadEnd::Done(Vec::new())), NOW - 60_000);
        assert!(b.transfers().is_empty());
        let h = b.history(NOW);
        assert_eq!(h.len(), 3, "the relayed send, then the server's own two rows");
        assert_eq!((h[0].name.as_str(), h[0].device.as_str()), ("a.bin", "lab-pc"));
        assert_eq!((h[0].direction, h[0].result, h[0].size), (Direction::Send, HistoryResult::Done, 1000));
        assert_eq!(crate::ui::files::history_peer_label(h[0].direction), "받는 곳");
        // The server's history row with the same id is not taken for it.
        assert_eq!(h[1].direction, Direction::Receive);

        // A failure stays a row to retry, not a history entry.
        let (mut b, id) = relayed_book();
        b.finish_send_at(&id, &Err(TransferError::NoSpace), NOW);
        assert_eq!(b.history(NOW).len(), 2);
        assert_eq!(b.transfers()[0].state, TransferState::Failed);
    }
}
