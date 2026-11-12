//! The node's file receiver (SPEC `FR-F13`, `FR-F15`, `FR-F16`, `INTENT.md` D24, #773).
//!
//! A computer receives files the owner sends it through the server: the server forwards each
//! call of `/api/computers/{id}/transfers/*` to the routes here (`FR-F14`), so the bodies and
//! answers are the server's own `/api/transfers*` ones and `ember-client` streams to either
//! receiver the same way.
//!
//! - Files go to `<state_dir>/received`, or `EMBER_NODE_RECEIVED_DIR` after the `FR-F12` check
//!   against the state folder ([`ember_transfer::received_dir::check`]).
//! - Writing is the shared [`ReceiveWriter`]: each chunk's SHA-256 before it is written, each
//!   file's SHA-256 at the end, `0644`, never overwriting.
//! - Pending offers and active transfers are kept in `<state_dir>/transfers/ledger.json`. After
//!   a restart each active transfer reopens its partial folder (adopted only when its record
//!   matches, `FR-F11`) and continues from the verified offset; partial folders the ledger does
//!   not name are removed.
//! - An offer marked `owner` by the server is accepted at once in the default mode; any other
//!   offer waits for an accept on this node (`FR-F15`).
//! - Before accepting, the free space of the received folder's file system must cover the bytes
//!   still to arrive plus [`FREE_MARGIN`] (`FR-F16`).
//!
//! Without a state folder (tests, embedding) the receiver is off and every route answers 409.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ember_transfer::limits::{Limits, DEFAULT_MAX_BYTES};
use ember_transfer::names::{clean_offer_paths, MAX_OFFER_FILES};
use ember_transfer::store::{
    DeclaredFile, HistoryRecord, ReceiveWriter, StoreError, StoredFile, TransferDirection, TransferResult,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Folder under the state folder that holds the ledger and the history.
pub const TRANSFERS_DIR: &str = "transfers";
/// Pending offers and active transfers, rewritten after every change but a chunk.
pub const LEDGER_FILE: &str = "ledger.json";
pub const HISTORY_FILE: &str = "history.json";
/// Free space kept beyond the bytes still to arrive (`FR-F16`).
pub const FREE_MARGIN: u64 = 64 * 1024 * 1024;

/// Who may start a transfer without an answer on this node (`FR-F15`, `EMBER_NODE_RECEIVE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReceivePolicy {
    /// Offers the server marks as the owner's are accepted at once; others wait.
    #[default]
    Owner,
    /// Every offer waits for an accept on this node.
    Ask,
    /// Every offer is refused.
    Off,
}

impl ReceivePolicy {
    /// `owner` (default when unset or empty), `ask` or `off`.
    pub fn parse(value: Option<&str>) -> anyhow::Result<Self> {
        match value.map(str::trim) {
            None | Some("") | Some("owner") => Ok(ReceivePolicy::Owner),
            Some("ask") => Ok(ReceivePolicy::Ask),
            Some("off") => Ok(ReceivePolicy::Off),
            Some(other) => anyhow::bail!("EMBER_NODE_RECEIVE must be owner, ask or off, not {other:?}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            ReceivePolicy::Owner => "owner",
            ReceivePolicy::Ask => "ask",
            ReceivePolicy::Off => "off",
        }
    }
}

/// Free bytes of the file system holding a path; `None` when unknown (then nothing is refused).
pub type FreeSpace = Arc<dyn Fn(&Path) -> Option<u64> + Send + Sync>;

/// The free bytes available to this user on the file system of `path`.
#[cfg(unix)]
#[allow(clippy::unnecessary_cast)]
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `c` is a valid NUL-terminated path and `s` is a plain C struct the call fills in.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    Some((s.f_bavail as u64).saturating_mul(s.f_frsize as u64))
}

/// The free bytes available on the disk whose mount point holds `path`.
#[cfg(not(unix))]
pub fn free_bytes(path: &Path) -> Option<u64> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter(|d| path.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| d.available_space())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Queued,
    Sending,
    Paused,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transfer {
    pub id: String,
    pub name: String,
    pub device: String,
    pub direction: TransferDirection,
    pub total: u64,
    pub done: u64,
    pub bytes_per_sec: Option<u64>,
    pub state: Status,
    pub files: Vec<DeclaredFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingOffer {
    pub id: String,
    pub from: String,
    pub peer_id: Option<String>,
    pub files: Vec<DeclaredFile>,
    pub cleaned_names: Vec<String>,
    pub total: u64,
    pub created_at: i64,
    /// Started by the owner account, as the server says (`FR-F15`).
    #[serde(default)]
    pub owner: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Ledger {
    #[serde(default)]
    pending: Vec<PendingOffer>,
    #[serde(default)]
    active: Vec<Transfer>,
}

/// `POST /transfers/offers` body. `owner` is set by the server only.
#[derive(Debug, Clone, Deserialize)]
pub struct OfferBody {
    pub from: String,
    #[serde(default)]
    pub peer_id: Option<String>,
    pub files: Vec<DeclaredFile>,
    #[serde(default)]
    pub owner: bool,
}

/// `POST /transfers/{id}/chunks` body.
#[derive(Debug, Clone, Deserialize)]
pub struct ChunkBody {
    #[serde(default)]
    pub file: usize,
    pub offset: u64,
    pub data_base64: String,
    #[serde(default)]
    pub sha256: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum RecvError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    TooLarge(String),
    #[error("{0}")]
    Conflict(String),
    /// Not enough free space (507, `FR-F16`).
    #[error("{0}")]
    NoSpace(String),
    #[error("{0}")]
    Internal(String),
}

impl From<std::io::Error> for RecvError {
    fn from(e: std::io::Error) -> Self {
        RecvError::Internal(e.to_string())
    }
}

type R<T> = Result<T, RecvError>;

struct State {
    received_dir: PathBuf,
    dir: PathBuf,
    limits: Limits,
    policy: ReceivePolicy,
    ledger: Ledger,
    writers: HashMap<String, ReceiveWriter>,
    received: Vec<StoredFile>,
    history: Vec<HistoryRecord>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

fn store_err(e: StoreError) -> RecvError {
    let msg = e.to_string();
    match e {
        StoreError::LimitExceeded { .. } | StoreError::FileSizeExceeded { .. } => RecvError::TooLarge(msg),
        StoreError::OffsetMismatch { .. } => RecvError::Conflict(msg),
        StoreError::Io(_) => RecvError::Internal(msg),
        _ => RecvError::BadRequest(msg),
    }
}

impl State {
    fn save_ledger(&self) -> R<()> {
        let bytes = serde_json::to_vec_pretty(&self.ledger).map_err(|e| RecvError::Internal(e.to_string()))?;
        Ok(write_atomic(&self.dir.join(LEDGER_FILE), &bytes)?)
    }

    fn save_history(&self) {
        if let Ok(bytes) = serde_json::to_vec_pretty(&self.history) {
            let _ = write_atomic(&self.dir.join(HISTORY_FILE), &bytes);
        }
    }

    fn open_writer(&self, t: &Transfer) -> R<ReceiveWriter> {
        ReceiveWriter::open(&self.received_dir, &t.id, &t.device, &t.files, self.limits).map_err(store_err)
    }

    fn record(&mut self, id: String, name: String, device: String, size: u64, result: TransferResult) -> HistoryRecord {
        let rec = HistoryRecord { id, name, device, direction: TransferDirection::Receive, size, result, timestamp_ms: now_ms() };
        self.history.insert(0, rec.clone());
        self.save_history();
        rec
    }
}

/// The node's receiver. Cheap to share behind an `Arc`.
pub struct Receiver {
    state: Option<Mutex<State>>,
    free: Mutex<FreeSpace>,
}

impl Receiver {
    /// A receiver that answers 409 to everything (no state folder).
    pub fn disabled() -> Self {
        Receiver { state: None, free: Mutex::new(Arc::new(free_bytes)) }
    }

    /// Opens the receiver over `state_dir`: checks `received_dir` (default
    /// `<state_dir>/received`), loads the ledger and history, reopens active transfers and removes
    /// partial folders the ledger does not name. `None` state folder: [`Receiver::disabled`].
    pub fn open(state_dir: Option<&Path>, received_dir: Option<&Path>, policy: ReceivePolicy) -> anyhow::Result<Self> {
        let Some(state_dir) = state_dir else {
            return Ok(Self::disabled());
        };
        std::fs::create_dir_all(state_dir)?;
        let state_dir = std::fs::canonicalize(state_dir)?;
        let received_dir = match received_dir {
            Some(d) => ember_transfer::received_dir::check(&state_dir, d)
                .map_err(|why| anyhow::anyhow!("EMBER_NODE_RECEIVED_DIR {}: {why}", d.display()))?,
            None => {
                let d = state_dir.join("received");
                std::fs::create_dir_all(&d)?;
                d
            }
        };
        let dir = state_dir.join(TRANSFERS_DIR);
        std::fs::create_dir_all(&dir)?;
        let ledger: Ledger = std::fs::read(dir.join(LEDGER_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let history: Vec<HistoryRecord> = std::fs::read(dir.join(HISTORY_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let mut st = State {
            received_dir,
            dir,
            limits: Limits { max_bytes: DEFAULT_MAX_BYTES },
            policy,
            ledger,
            writers: HashMap::new(),
            received: Vec::new(),
            history,
        };
        // Partial folders the ledger does not name belong to nothing and are removed (`FR-F13`).
        let keep: Vec<String> = st.ledger.active.iter().map(|t| t.id.clone()).collect();
        if let Ok(entries) = std::fs::read_dir(st.received_dir.join(".partial")) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if !keep.contains(&name) {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
        // Reopen each active transfer at its verified offset.
        let active = st.ledger.active.clone();
        for t in &active {
            if matches!(t.state, Status::Failed) {
                continue;
            }
            match st.open_writer(t) {
                Ok(w) => {
                    let done = w.total_written();
                    st.writers.insert(t.id.clone(), w);
                    if let Some(row) = st.ledger.active.iter_mut().find(|r| r.id == t.id) {
                        row.done = done;
                    }
                }
                Err(e) => tracing::warn!(transfer = %t.id, "cannot reopen a transfer after a restart: {e}"),
            }
        }
        let _ = st.save_ledger();
        Ok(Receiver { state: Some(Mutex::new(st)), free: Mutex::new(Arc::new(free_bytes)) })
    }

    /// Replaces the free-space probe (tests).
    pub fn set_free_space(&self, f: FreeSpace) {
        *self.free.lock().unwrap() = f;
    }

    fn st(&self) -> R<std::sync::MutexGuard<'_, State>> {
        match &self.state {
            Some(m) => Ok(m.lock().unwrap()),
            None => Err(RecvError::Conflict("this node has no state folder, so it does not receive files".into())),
        }
    }

    fn check_space(&self, dir: &Path, needed: u64) -> R<()> {
        let probe = self.free.lock().unwrap().clone();
        if let Some(free) = probe(dir) {
            if free < needed.saturating_add(FREE_MARGIN) {
                return Err(RecvError::NoSpace(format!(
                    "NoSpace: {needed} bytes to receive, {free} bytes free on this computer"
                )));
            }
        }
        Ok(())
    }

    /// `GET /transfers`: the same shape as the server's snapshot.
    pub fn snapshot(&self) -> R<Value> {
        let g = self.st()?;
        Ok(json!({
            "ready": true,
            "devices": [],
            "transfers": g.ledger.active,
            "pending_offers": g.ledger.pending,
            "received": g.received,
            "history": g.history,
            "settings": {
                "receive_mode": g.policy.as_str(),
                "auto_accept_peers": [],
                "max_bytes": g.limits.max_bytes,
                "received_dir": g.received_dir.to_string_lossy(),
            },
        }))
    }

    /// `POST /transfers/offers` (`FR-F3`, `FR-F10`, `FR-F15`, `FR-F16`).
    pub fn offer(&self, body: OfferBody) -> R<Value> {
        if body.files.is_empty() {
            return Err(RecvError::BadRequest("the offer has no files".into()));
        }
        if body.files.len() > MAX_OFFER_FILES {
            return Err(RecvError::BadRequest("the offer has too many files".into()));
        }
        let mut total: u64 = 0;
        for f in &body.files {
            total = total.checked_add(f.size).ok_or_else(|| RecvError::BadRequest("file sizes overflow".into()))?;
        }
        let (auto, received_dir) = {
            let g = self.st()?;
            if let Err(e) = g.limits.check_total(total) {
                return Err(RecvError::TooLarge(format!("TooLarge: {e}")));
            }
            if g.policy == ReceivePolicy::Off {
                return Err(RecvError::Forbidden("ReceiveOff: receive mode is off".into()));
            }
            (g.policy == ReceivePolicy::Owner && body.owner, g.received_dir.clone())
        };
        self.check_space(&received_dir, total)?;

        let raw: Vec<String> = body.files.iter().map(|f| f.name.clone()).collect();
        let cleaned_names: Vec<String> = clean_offer_paths(&raw).into_iter().map(|c| c.join("/")).collect();
        let files: Vec<DeclaredFile> = body
            .files
            .into_iter()
            .zip(cleaned_names.iter())
            .map(|(f, clean)| DeclaredFile { name: clean.clone(), size: f.size, sha256: f.sha256 })
            .collect();
        let id = format!("tr-{}", uuid::Uuid::new_v4().simple());
        let offer = PendingOffer {
            id: id.clone(),
            from: body.from.trim().to_string(),
            peer_id: body.peer_id,
            files,
            cleaned_names,
            total,
            created_at: now_ms(),
            owner: body.owner,
        };
        {
            let mut g = self.st()?;
            g.ledger.pending.push(offer.clone());
            g.save_ledger()?;
        }
        if auto {
            let t = self.accept(&id)?;
            Ok(json!({ "status": "accepted", "id": id, "transfer": t }))
        } else {
            Ok(json!({ "status": "offered", "id": id, "offer": offer }))
        }
    }

    /// `POST /transfers/offers/{id}/accept`.
    pub fn accept(&self, id: &str) -> R<Transfer> {
        let (total, dir) = {
            let g = self.st()?;
            let o = g
                .ledger
                .pending
                .iter()
                .find(|o| o.id == id)
                .ok_or_else(|| RecvError::NotFound(format!("offer {id} not found")))?;
            (o.total, g.received_dir.clone())
        };
        self.check_space(&dir, total)?;
        let mut g = self.st()?;
        let pos = g
            .ledger
            .pending
            .iter()
            .position(|o| o.id == id)
            .ok_or_else(|| RecvError::NotFound(format!("offer {id} not found")))?;
        let o = g.ledger.pending.remove(pos);
        let t = Transfer {
            id: o.id,
            name: o.cleaned_names.first().cloned().unwrap_or_else(|| "file".into()),
            device: o.from,
            direction: TransferDirection::Receive,
            total: o.total,
            done: 0,
            bytes_per_sec: None,
            state: Status::Sending,
            files: o.files,
        };
        let w = g.open_writer(&t)?;
        let mut t = t;
        t.done = w.total_written();
        g.writers.insert(t.id.clone(), w);
        g.ledger.active.insert(0, t.clone());
        g.save_ledger()?;
        Ok(t)
    }

    /// `POST /transfers/offers/{id}/decline`.
    pub fn decline(&self, id: &str) -> R<HistoryRecord> {
        let mut g = self.st()?;
        let pos = g
            .ledger
            .pending
            .iter()
            .position(|o| o.id == id)
            .ok_or_else(|| RecvError::NotFound(format!("offer {id} not found")))?;
        let o = g.ledger.pending.remove(pos);
        g.save_ledger()?;
        let name = o.cleaned_names.first().cloned().unwrap_or_else(|| "file".into());
        Ok(g.record(o.id, name, o.from, o.total, TransferResult::Declined))
    }

    fn writer_for<'a>(g: &'a mut State, id: &str) -> R<&'a mut ReceiveWriter> {
        if !g.writers.contains_key(id) {
            let t = g
                .ledger
                .active
                .iter()
                .find(|t| t.id == id)
                .cloned()
                .ok_or_else(|| RecvError::NotFound(format!("transfer {id} not found")))?;
            let w = g.open_writer(&t)?;
            g.writers.insert(id.to_string(), w);
        }
        Ok(g.writers.get_mut(id).expect("just inserted"))
    }

    /// `GET /transfers/{id}/resume-offset/{file}` (`FR-F5`).
    pub fn resume_offset(&self, id: &str, file: usize) -> R<u64> {
        let mut g = self.st()?;
        Self::writer_for(&mut g, id)?.resume_offset(file).map_err(store_err)
    }

    /// `POST /transfers/{id}/chunks` (`FR-F5`): the chunk's hash is checked before the write.
    pub fn chunk(&self, id: &str, body: &ChunkBody) -> R<Transfer> {
        use base64::Engine;
        let data = base64::engine::general_purpose::STANDARD
            .decode(body.data_base64.trim())
            .map_err(|e| RecvError::BadRequest(format!("invalid base64 chunk: {e}")))?;
        let mut g = self.st()?;
        let state = g
            .ledger
            .active
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.state)
            .ok_or_else(|| RecvError::NotFound(format!("transfer {id} not found")))?;
        match state {
            Status::Paused => return Err(RecvError::Conflict("transfer is paused".into())),
            Status::Sending | Status::Queued => {}
            other => return Err(RecvError::Conflict(format!("transfer is in state {other:?}"))),
        }
        let done = Self::writer_for(&mut g, id)?
            .write_chunk(body.file, body.offset, &data, body.sha256.as_deref())
            .map_err(store_err)?;
        let t = g.ledger.active.iter_mut().find(|t| t.id == id).expect("checked above");
        t.done = done;
        Ok(t.clone())
    }

    /// `POST /transfers/{id}/complete`: final hashes, then the files move into the folder.
    pub fn complete(&self, id: &str) -> R<Vec<StoredFile>> {
        let mut g = self.st()?;
        if !g.ledger.active.iter().any(|t| t.id == id) {
            return Err(RecvError::NotFound(format!("active transfer {id} not found")));
        }
        Self::writer_for(&mut g, id)?;
        let w = g.writers.remove(id).expect("opened above");
        let stored = match w.finalize() {
            Ok(s) => s,
            Err(e) => {
                if let Some(t) = g.ledger.active.iter_mut().find(|t| t.id == id) {
                    t.state = Status::Failed;
                    t.done = 0;
                }
                g.save_ledger()?;
                return Err(match e {
                    StoreError::Io(io) => RecvError::Internal(io.to_string()),
                    other => RecvError::BadRequest(other.to_string()),
                });
            }
        };
        let pos = g.ledger.active.iter().position(|t| t.id == id).expect("checked above");
        let t = g.ledger.active.remove(pos);
        g.save_ledger()?;
        for s in &stored {
            g.received.insert(0, s.clone());
            g.record(t.id.clone(), s.name.clone(), t.device.clone(), s.size, TransferResult::Done);
        }
        Ok(stored)
    }

    fn set_state(&self, id: &str, from: &[Status], to: Status) -> R<Transfer> {
        let mut g = self.st()?;
        let t = g
            .ledger
            .active
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| RecvError::NotFound(format!("transfer {id} not found")))?;
        if !from.contains(&t.state) {
            return Err(RecvError::Conflict(format!("cannot change a transfer in state {:?}", t.state)));
        }
        t.state = to;
        let t = t.clone();
        g.save_ledger()?;
        Ok(t)
    }

    pub fn pause(&self, id: &str) -> R<Transfer> {
        self.set_state(id, &[Status::Sending], Status::Paused)
    }

    pub fn resume(&self, id: &str) -> R<Transfer> {
        self.set_state(id, &[Status::Paused, Status::Failed], Status::Sending)
    }

    /// `POST /transfers/{id}/cancel`: an offer or a transfer; the partial folder goes (`FR-F4`).
    pub fn cancel(&self, id: &str) -> R<HistoryRecord> {
        let mut g = self.st()?;
        if let Some(w) = g.writers.remove(id) {
            w.abort();
        }
        if let Some(pos) = g.ledger.pending.iter().position(|o| o.id == id) {
            let o = g.ledger.pending.remove(pos);
            g.save_ledger()?;
            let name = o.cleaned_names.first().cloned().unwrap_or_else(|| "file".into());
            return Ok(g.record(o.id, name, o.from, o.total, TransferResult::Cancelled));
        }
        let pos = g
            .ledger
            .active
            .iter()
            .position(|t| t.id == id)
            .ok_or_else(|| RecvError::NotFound(format!("transfer {id} not found")))?;
        let t = g.ledger.active.remove(pos);
        let _ = std::fs::remove_dir_all(g.received_dir.join(".partial").join(&t.id));
        g.save_ledger()?;
        Ok(g.record(t.id, t.name, t.device, t.total, TransferResult::Cancelled))
    }

    pub fn history(&self) -> R<Vec<HistoryRecord>> {
        Ok(self.st()?.history.clone())
    }

    /// Clears the history; received files stay (`FR-F8`).
    pub fn clear_history(&self) -> R<()> {
        let mut g = self.st()?;
        g.history.clear();
        g.save_history();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_f15_receive_policy_parses_and_defaults_to_owner() {
        assert_eq!(ReceivePolicy::parse(None).unwrap(), ReceivePolicy::Owner);
        assert_eq!(ReceivePolicy::parse(Some("")).unwrap(), ReceivePolicy::Owner);
        assert_eq!(ReceivePolicy::parse(Some("ask")).unwrap(), ReceivePolicy::Ask);
        assert_eq!(ReceivePolicy::parse(Some("off")).unwrap(), ReceivePolicy::Off);
        assert!(ReceivePolicy::parse(Some("auto")).is_err());
    }

    #[test]
    fn fr_f16_free_bytes_reads_the_temp_folder() {
        let d = tempfile::tempdir().unwrap();
        assert!(free_bytes(d.path()).is_some_and(|n| n > 0));
    }
}
