//! File transfer client (SPEC `FR-F3`, `FR-F4`, `FR-F5`, `FR-F8`, `FR-F10`, `FR-L27.8`, #487).
//!
//! The wire types of the server's `/api/transfers*` (see `ember/server/src/transfers.rs`) and
//! the sending side built on them: [`prepare`] reads the local files' sizes and SHA-256,
//! [`offer`] offers them, and [`upload`] waits for the answer and streams the bytes in chunks.
//!
//! [`upload`] never holds a whole file in memory (one chunk at a time), asks the receiver for
//! the verified offset before every run of chunks, so a dropped connection resumes where the
//! receiver stopped instead of at zero, and follows the receiver's state: a pause holds the
//! stream, a cancel or a decline ends it.
//!
//! Every failure is a [`TransferError`] with a fixed Korean sentence ([`TransferError::text_ko`]).
//! The server's own text is English and may echo a file name, so it is never shown.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::api::{Api, ApiError};

/// Bytes per chunk. A chunk travels as base64 inside a JSON body, so it stays well under the
/// server's 2 MiB body limit.
pub const CHUNK_BYTES: usize = 256 * 1024;

// ---------------------------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    Send,
    Receive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferStatus {
    Queued,
    Sending,
    Paused,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferResult {
    Done,
    Failed,
    Declined,
    Cancelled,
}

/// One file of an offer: the name as the sender has it, its size, and its SHA-256 (lowercase
/// hex) when the sender knows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredFile {
    pub name: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TransferItem {
    pub id: String,
    pub name: String,
    /// The sending device's name.
    pub device: String,
    pub direction: TransferDirection,
    pub total: u64,
    pub done: u64,
    #[serde(default)]
    pub bytes_per_sec: Option<u64>,
    pub state: TransferStatus,
    #[serde(default)]
    pub files: Vec<DeclaredFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PendingOffer {
    pub id: String,
    pub from: String,
    #[serde(default)]
    pub peer_id: Option<String>,
    #[serde(default)]
    pub files: Vec<DeclaredFile>,
    /// The names after the receiver cleaned them (`FR-F9`): the only names to show.
    #[serde(default)]
    pub cleaned_names: Vec<String>,
    pub total: u64,
    #[serde(default)]
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TransferDevice {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub online: bool,
    #[serde(default)]
    pub peer_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StoredFile {
    /// The path below the received-files folder, with `/`.
    pub name: String,
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub from: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HistoryRecord {
    pub id: String,
    pub name: String,
    pub device: String,
    pub direction: TransferDirection,
    pub size: u64,
    pub result: TransferResult,
    pub timestamp_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TransferSettings {
    /// `"ask"`, `"auto_accept"` or `"off"`.
    pub receive_mode: String,
    #[serde(default)]
    pub auto_accept_peers: Vec<String>,
    pub max_bytes: u64,
    pub received_dir: String,
}

/// `GET /api/transfers`: everything the receiving computer holds.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TransfersSnapshot {
    pub ready: bool,
    #[serde(default)]
    pub devices: Vec<TransferDevice>,
    #[serde(default)]
    pub transfers: Vec<TransferItem>,
    #[serde(default)]
    pub pending_offers: Vec<PendingOffer>,
    #[serde(default)]
    pub received: Vec<StoredFile>,
    #[serde(default)]
    pub history: Vec<HistoryRecord>,
    pub settings: TransferSettings,
}

/// `POST /api/transfers/offers` answer: `status` is `"offered"` (waiting for the receiver's
/// answer) or `"accepted"` (the receiver auto-accepts this device).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OfferCreated {
    pub status: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct ResumeOffset {
    pub offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct Completed {
    #[serde(default)]
    pub stored: Vec<StoredFile>,
}

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

/// Why a transfer did not happen or stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferError {
    /// Nothing was chosen.
    NoFiles,
    /// The path is missing or is not a regular file (a folder is not sent yet).
    NotAFile(String),
    /// The local file could not be read.
    Read(String),
    /// The local file changed size after it was offered.
    Changed(String),
    /// The offer is above the receiver's limit (`FR-F10`); nothing moved.
    TooLarge,
    /// The receiver does not take files (`FR-F9` receive mode off).
    ReceiveOff,
    /// This device is not registered with the receiver (`FR-F9`).
    NotRegistered,
    /// The receiver did not answer in time (`FR-F3`).
    Expired,
    /// The finished file did not match its SHA-256 (`FR-F5`).
    HashMismatch,
    /// The connection stayed down through every retry. The transfer can be resumed.
    Connection,
    /// The server refused the token (`FR-S11`).
    Unauthorized,
    /// The server has no transfer API (an older server).
    Unsupported,
    /// Anything else the server refused, with its status code.
    Server(u16),
}

impl TransferError {
    /// The fixed sentence for this failure, in 합니다체.
    pub fn text_ko(&self) -> String {
        match self {
            TransferError::NoFiles => "보낼 파일을 선택해야 합니다.".into(),
            TransferError::NotAFile(name) => format!("{name}은(는) 보낼 수 있는 파일이 아닙니다."),
            TransferError::Read(name) => format!("{name}을(를) 읽지 못했습니다."),
            TransferError::Changed(name) => {
                format!("{name}이(가) 전송 중에 바뀌어서 전송을 멈췄습니다. 다시 보내야 합니다.")
            }
            TransferError::TooLarge => "받는 쪽의 크기 제한을 넘어서 보낼 수 없습니다.".into(),
            TransferError::ReceiveOff => "받는 쪽이 파일 받기를 꺼 두었습니다.".into(),
            TransferError::NotRegistered => "이 장비는 받는 쪽에 등록되어 있지 않습니다.".into(),
            TransferError::Expired => "받는 쪽에서 응답이 없어 전송을 취소했습니다.".into(),
            TransferError::HashMismatch => {
                "받은 파일이 원본과 달라서 저장하지 않았습니다. 다시 시도해 주세요.".into()
            }
            TransferError::Connection => {
                "연결이 끊겨 전송을 멈췄습니다. 연결을 확인하고 다시 시도하면 이어서 보냅니다.".into()
            }
            TransferError::Unauthorized => "서버가 이 앱의 인증을 거부했습니다.".into(),
            TransferError::Unsupported => "이 서버는 파일 전송을 지원하지 않습니다.".into(),
            TransferError::Server(status) => format!("전송에 실패했습니다. (서버 응답 {status})"),
        }
    }

    /// Whether trying the same transfer again can continue it (`FR-F5`).
    pub fn can_resume(&self) -> bool {
        matches!(self, TransferError::Connection | TransferError::Server(_))
    }

    /// What an API failure means for a transfer.
    pub fn from_api(e: &ApiError) -> TransferError {
        match e {
            ApiError::Transport(_) | ApiError::Peer(_) | ApiError::Push(_) => TransferError::Connection,
            ApiError::Unauthorized(_) => TransferError::Unauthorized,
            ApiError::Status { status: 413, .. } => TransferError::TooLarge,
            ApiError::Status { status: 403, message, .. } if message.contains("ReceiveOff") => TransferError::ReceiveOff,
            ApiError::Status { status: 403, message, .. } if message.contains("NotRegistered") => {
                TransferError::NotRegistered
            }
            ApiError::Status { status: 400, message, .. } if message.contains("SHA-256 mismatch") => {
                TransferError::HashMismatch
            }
            ApiError::Status { status: 404 | 405, .. } => TransferError::Unsupported,
            ApiError::Status { status, .. } => TransferError::Server(*status),
            ApiError::NotAMember { .. } => TransferError::Server(400),
            ApiError::Decode(_) | ApiError::BadUrl(_) => TransferError::Server(0),
        }
    }
}

impl std::fmt::Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text_ko())
    }
}

impl std::error::Error for TransferError {}

fn is_connection(e: &ApiError) -> bool {
    match e {
        ApiError::Transport(_) | ApiError::Peer(_) => true,
        // A relay or proxy in front of the server answering for it.
        ApiError::Status { status, .. } => matches!(status, 502 | 503 | 504),
        _ => false,
    }
}

// ---------------------------------------------------------------------------------------------
// Local files
// ---------------------------------------------------------------------------------------------

/// A local file ready to be offered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFile {
    pub path: PathBuf,
    pub declared: DeclaredFile,
}

/// Lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// The SHA-256 of `data`, lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    hex(digest.as_slice())
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Reads each file's size and SHA-256 without holding a file in memory (`FR-F5`). The offered
/// name is the base name; the receiver cleans it again (`FR-F9`).
pub async fn prepare(paths: &[PathBuf]) -> Result<Vec<LocalFile>, TransferError> {
    if paths.is_empty() {
        return Err(TransferError::NoFiles);
    }
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let name = display_name(path);
        let meta = tokio::fs::metadata(path).await.map_err(|_| TransferError::NotAFile(name.clone()))?;
        if !meta.is_file() {
            return Err(TransferError::NotAFile(name));
        }
        let mut file = tokio::fs::File::open(path).await.map_err(|_| TransferError::Read(name.clone()))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut size: u64 = 0;
        loop {
            let n = file.read(&mut buf).await.map_err(|_| TransferError::Read(name.clone()))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            size += n as u64;
        }
        let digest = hasher.finalize();
        out.push(LocalFile {
            path: path.clone(),
            declared: DeclaredFile { name, size, sha256: Some(hex(digest.as_slice())) },
        });
    }
    Ok(out)
}

/// The sum of the files' sizes.
pub fn total_bytes(files: &[LocalFile]) -> u64 {
    files.iter().fold(0u64, |acc, f| acc.saturating_add(f.declared.size))
}

// ---------------------------------------------------------------------------------------------
// Offer and upload
// ---------------------------------------------------------------------------------------------

/// Offers `files` to the receiver as device `from`. `peer_id` is this device's transport peer id
/// when it has one; the receiver then requires it to be registered (`FR-F9`). A refusal (too
/// large, receiving off, not registered) comes back here, before any byte moves (`FR-F10`).
pub async fn offer(
    api: &Api,
    from: &str,
    peer_id: Option<&str>,
    files: &[LocalFile],
) -> Result<OfferCreated, TransferError> {
    if files.is_empty() {
        return Err(TransferError::NoFiles);
    }
    let declared: Vec<DeclaredFile> = files.iter().map(|f| f.declared.clone()).collect();
    api.offer_transfer(from, peer_id, &declared).await.map_err(|e| TransferError::from_api(&e))
}

/// How [`upload`] paces itself.
#[derive(Debug, Clone)]
pub struct UploadOptions {
    pub chunk_bytes: usize,
    /// How often the receiver's state is read while waiting (for the answer, or during a pause).
    pub poll: Duration,
    /// How long an offer waits for the receiver's answer before the sender withdraws it (`FR-F3`).
    pub answer_timeout: Duration,
    /// Failures in a row (a dropped connection, a refused chunk) before the upload gives up.
    pub max_retries: u32,
    /// The wait after the first failure; it grows with each further one, up to five times.
    pub retry_delay: Duration,
}

impl Default for UploadOptions {
    fn default() -> Self {
        UploadOptions {
            chunk_bytes: CHUNK_BYTES,
            poll: Duration::from_secs(1),
            answer_timeout: Duration::from_secs(300),
            max_retries: 8,
            retry_delay: Duration::from_secs(1),
        }
    }
}

/// How an upload ended without an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadEnd {
    /// Every file arrived and matched its hash. The list is empty when the answer to the final
    /// request was lost and the result was read from the history instead.
    Done(Vec<StoredFile>),
    /// The receiver declined the offer (`FR-F3`).
    Declined,
    /// Either side cancelled (`FR-F4`). The receiver keeps no partial file.
    Cancelled,
}

/// Where the receiver has a transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Whereabouts {
    /// Offered, not answered yet.
    Pending,
    Active(TransferItem),
    /// Not pending and not active: the history's result for it, if it has one.
    Ended(Option<TransferResult>),
}

/// Finds transfer `id` in a snapshot.
pub fn locate(snapshot: &TransfersSnapshot, id: &str) -> Whereabouts {
    if snapshot.pending_offers.iter().any(|o| o.id == id) {
        return Whereabouts::Pending;
    }
    if let Some(t) = snapshot.transfers.iter().find(|t| t.id == id) {
        return Whereabouts::Active(t.clone());
    }
    Whereabouts::Ended(snapshot.history.iter().find(|h| h.id == id).map(|h| h.result))
}

fn end_of(result: Option<TransferResult>) -> Result<UploadEnd, TransferError> {
    match result {
        Some(TransferResult::Done) => Ok(UploadEnd::Done(Vec::new())),
        Some(TransferResult::Declined) => Ok(UploadEnd::Declined),
        Some(TransferResult::Failed) => Err(TransferError::HashMismatch),
        // No record: the receiver dropped it (a cancel, or its history was cleared).
        Some(TransferResult::Cancelled) | None => Ok(UploadEnd::Cancelled),
    }
}

/// What the stream does after a failed request.
enum Next {
    /// Ask for the verified offset again and continue from it.
    Reoffset,
    End(UploadEnd),
}

struct Uploader<'a> {
    api: &'a Api,
    id: &'a str,
    opts: &'a UploadOptions,
    failures: u32,
}

impl Uploader<'_> {
    async fn pause_for_retry(&mut self, e: TransferError) -> Result<(), TransferError> {
        self.failures += 1;
        if self.failures > self.opts.max_retries {
            return Err(e);
        }
        tokio::time::sleep(self.opts.retry_delay * self.failures.min(5)).await;
        Ok(())
    }

    /// Decides what a failed request means, by the receiver's state.
    async fn recover(&mut self, e: ApiError) -> Result<Next, TransferError> {
        if is_connection(&e) {
            self.pause_for_retry(TransferError::Connection).await?;
            return Ok(Next::Reoffset);
        }
        match e.status() {
            Some(404) | Some(409) => {}
            Some(400) => {
                // A chunk the receiver refused (its hash, or its offset after a lost answer).
                self.pause_for_retry(TransferError::from_api(&e)).await?;
                return Ok(Next::Reoffset);
            }
            _ => return Err(TransferError::from_api(&e)),
        }
        let snapshot = loop {
            match self.api.transfers().await {
                Ok(s) => break s,
                Err(e) if is_connection(&e) => self.pause_for_retry(TransferError::Connection).await?,
                Err(e) => return Err(TransferError::from_api(&e)),
            }
        };
        match locate(&snapshot, self.id) {
            Whereabouts::Pending => {
                tokio::time::sleep(self.opts.poll).await;
                Ok(Next::Reoffset)
            }
            Whereabouts::Active(t) => match t.state {
                // A pause holds the stream for as long as it lasts; it is not a failure.
                TransferStatus::Paused => {
                    tokio::time::sleep(self.opts.poll).await;
                    Ok(Next::Reoffset)
                }
                TransferStatus::Failed => {
                    self.pause_for_retry(TransferError::Server(409)).await?;
                    if let Err(e) = self.api.resume_transfer(self.id).await {
                        if !is_connection(&e) && e.status() != Some(409) {
                            return Err(TransferError::from_api(&e));
                        }
                    }
                    Ok(Next::Reoffset)
                }
                TransferStatus::Queued | TransferStatus::Sending => {
                    // The offset moved (an answer was lost on the way back).
                    self.pause_for_retry(TransferError::Server(409)).await?;
                    Ok(Next::Reoffset)
                }
                TransferStatus::Done => Ok(Next::End(UploadEnd::Done(Vec::new()))),
                TransferStatus::Cancelled => Ok(Next::End(UploadEnd::Cancelled)),
            },
            Whereabouts::Ended(result) => end_of(result).map(Next::End),
        }
    }

    /// Waits until the receiver answers the offer. `None`: accepted, go on.
    async fn wait_for_answer(&mut self) -> Result<Option<UploadEnd>, TransferError> {
        let deadline = tokio::time::Instant::now() + self.opts.answer_timeout;
        loop {
            match self.api.transfers().await {
                Ok(snapshot) => match locate(&snapshot, self.id) {
                    Whereabouts::Pending => {}
                    Whereabouts::Active(_) => return Ok(None),
                    Whereabouts::Ended(result) => return end_of(result).map(Some),
                },
                Err(e) if is_connection(&e) => {}
                Err(e) => return Err(TransferError::from_api(&e)),
            }
            if tokio::time::Instant::now() >= deadline {
                // Withdraw the offer so the receiver's prompt goes away too.
                let _ = self.api.cancel_transfer(self.id).await;
                return Err(TransferError::Expired);
            }
            tokio::time::sleep(self.opts.poll).await;
        }
    }

    /// Streams one file from the receiver's verified offset. `Some`: the transfer ended early.
    async fn stream_file(
        &mut self,
        index: usize,
        file: &LocalFile,
        progress: &mut (dyn FnMut(u64) + Send),
    ) -> Result<Option<UploadEnd>, TransferError> {
        let name = file.declared.name.clone();
        let size = file.declared.size;
        let mut source =
            tokio::fs::File::open(&file.path).await.map_err(|_| TransferError::Read(name.clone()))?;
        let mut buf = vec![0u8; self.opts.chunk_bytes.max(1)];
        loop {
            let mut offset = match self.api.transfer_resume_offset(self.id, index).await {
                Ok(o) => o,
                Err(e) => match self.recover(e).await? {
                    Next::Reoffset => continue,
                    Next::End(end) => return Ok(Some(end)),
                },
            };
            if offset >= size {
                return Ok(None);
            }
            source
                .seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(|_| TransferError::Read(name.clone()))?;
            while offset < size {
                let want = buf.len().min((size - offset).min(usize::MAX as u64) as usize);
                let mut filled = 0;
                while filled < want {
                    let n = source
                        .read(&mut buf[filled..want])
                        .await
                        .map_err(|_| TransferError::Read(name.clone()))?;
                    if n == 0 {
                        break;
                    }
                    filled += n;
                }
                if filled < want {
                    // Shorter than it was when offered: its hash can no longer match.
                    return Err(TransferError::Changed(name));
                }
                let chunk = &buf[..filled];
                let digest = sha256_hex(chunk);
                match self.api.transfer_chunk(self.id, index, offset, chunk, Some(&digest)).await {
                    Ok(item) => {
                        offset += filled as u64;
                        self.failures = 0;
                        progress(item.done);
                    }
                    Err(e) => match self.recover(e).await? {
                        Next::Reoffset => break,
                        Next::End(end) => return Ok(Some(end)),
                    },
                }
            }
            if offset >= size {
                return Ok(None);
            }
        }
    }

    async fn complete(&mut self) -> Result<UploadEnd, TransferError> {
        loop {
            match self.api.complete_transfer(self.id).await {
                Ok(stored) => return Ok(UploadEnd::Done(stored)),
                Err(e) if is_connection(&e) => self.pause_for_retry(TransferError::Connection).await?,
                Err(e) if e.status() == Some(404) => {
                    // Not active any more: an earlier answer was lost, or it was cancelled.
                    let snapshot = self.api.transfers().await.map_err(|e| TransferError::from_api(&e))?;
                    return match locate(&snapshot, self.id) {
                        Whereabouts::Ended(result) => end_of(result),
                        _ => Err(TransferError::Server(404)),
                    };
                }
                Err(e) => return Err(TransferError::from_api(&e)),
            }
        }
    }
}

/// Sends the bytes of offer `id`: waits for the receiver's answer, then streams every file from
/// the receiver's verified offset and asks it to verify and store them (`FR-F3`, `FR-F5`).
///
/// `progress` gets the receiver's count of verified bytes after each chunk. Calling `upload`
/// again with the same `id` and `files` continues an interrupted transfer (`FR-F5`).
pub async fn upload(
    api: &Api,
    id: &str,
    files: &[LocalFile],
    opts: &UploadOptions,
    progress: &mut (dyn FnMut(u64) + Send),
) -> Result<UploadEnd, TransferError> {
    let mut up = Uploader { api, id, opts, failures: 0 };
    if let Some(end) = up.wait_for_answer().await? {
        return Ok(end);
    }
    for (index, file) in files.iter().enumerate() {
        if let Some(end) = up.stream_file(index, file, progress).await? {
            return Ok(end);
        }
    }
    up.complete().await
}

// ---------------------------------------------------------------------------------------------
// Rate
// ---------------------------------------------------------------------------------------------

/// Bytes per second from successive readings of a transfer's verified bytes (`FR-F4`). The
/// server reports no rate, so the reader measures it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RateMeter {
    last: Option<(i64, u64)>,
    rate: Option<u64>,
}

impl RateMeter {
    /// Feeds one reading (`now_ms`, bytes done) and answers the current rate. A reading less
    /// than 200 ms after the last one keeps the previous rate; bytes going backwards (a restart
    /// after a failed check) start the measurement again.
    pub fn observe(&mut self, now_ms: i64, done: u64) -> Option<u64> {
        match self.last {
            None => self.last = Some((now_ms, done)),
            Some((_, d0)) if done < d0 => {
                self.last = Some((now_ms, done));
                self.rate = None;
            }
            Some((t0, d0)) => {
                let dt = now_ms - t0;
                if dt >= 200 {
                    let instant = (done - d0).saturating_mul(1000) / dt as u64;
                    // Smooth, so one slow second does not swing the time left.
                    self.rate = Some(match self.rate {
                        Some(prev) => (prev + instant) / 2,
                        None => instant,
                    });
                    self.last = Some((now_ms, done));
                }
            }
        }
        self.rate
    }

    pub fn rate(&self) -> Option<u64> {
        self.rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_errors() -> Vec<TransferError> {
        vec![
            TransferError::NoFiles,
            TransferError::NotAFile("a.txt".into()),
            TransferError::Read("a.txt".into()),
            TransferError::Changed("a.txt".into()),
            TransferError::TooLarge,
            TransferError::ReceiveOff,
            TransferError::NotRegistered,
            TransferError::Expired,
            TransferError::HashMismatch,
            TransferError::Connection,
            TransferError::Unauthorized,
            TransferError::Unsupported,
            TransferError::Server(500),
        ]
    }

    #[test]
    fn fr_f_errors_are_korean_polite_and_have_no_em_dash() {
        for e in all_errors() {
            let text = e.text_ko();
            assert!(text.chars().any(|c| ('가'..='힣').contains(&c)), "{e:?}: {text}");
            assert!(!text.contains('\u{2014}'), "{e:?}: {text}");
            assert!(text.contains("니다."), "{e:?} is not in 합니다체: {text}");
        }
    }

    fn status(status: u16, message: &str) -> ApiError {
        ApiError::Status { status, message: message.into(), code: None }
    }

    #[test]
    fn fr_f_api_failures_map_to_their_reason() {
        assert_eq!(TransferError::from_api(&status(413, "TooLarge: over")), TransferError::TooLarge);
        assert_eq!(
            TransferError::from_api(&status(403, "ReceiveOff: receive mode is off")),
            TransferError::ReceiveOff
        );
        assert_eq!(
            TransferError::from_api(&status(403, "NotRegistered: peer is not a registered device")),
            TransferError::NotRegistered
        );
        assert_eq!(TransferError::from_api(&status(400, "SHA-256 mismatch for \"a\": expected 00, got ff")), TransferError::HashMismatch);
        assert_eq!(TransferError::from_api(&status(404, "")), TransferError::Unsupported);
        assert_eq!(TransferError::from_api(&status(500, "boom")), TransferError::Server(500));
        assert_eq!(TransferError::from_api(&ApiError::Peer("gone".into())), TransferError::Connection);
        assert_eq!(TransferError::from_api(&ApiError::Unauthorized("no".into())), TransferError::Unauthorized);
        // The server's text never reaches the user.
        assert!(!TransferError::from_api(&status(500, "boom")).text_ko().contains("boom"));
    }

    #[test]
    fn fr_f5_only_interruptions_can_resume() {
        assert!(TransferError::Connection.can_resume());
        assert!(!TransferError::TooLarge.can_resume());
        assert!(!TransferError::Changed("a".into()).can_resume());
    }

    #[test]
    fn fr_f4_rate_follows_the_bytes() {
        let mut m = RateMeter::default();
        assert_eq!(m.observe(0, 0), None);
        assert_eq!(m.observe(1000, 1000), Some(1000));
        // Too soon: the rate stays.
        assert_eq!(m.observe(1100, 5000), Some(1000));
        // Smoothed against the previous rate: (1000 + 3000) / 2.
        assert_eq!(m.observe(2000, 4000), Some(2000));
        // Bytes going backwards start again.
        assert_eq!(m.observe(3000, 10), None);
        assert_eq!(m.observe(4000, 510), Some(500));
    }

    #[test]
    fn sha256_of_known_input() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(hex(&[0, 255, 16]), "00ff10");
    }

    fn snapshot(json: serde_json::Value) -> TransfersSnapshot {
        serde_json::from_value(json).expect("snapshot decodes")
    }

    #[test]
    fn fr_f_locate_reads_pending_active_and_history() {
        let s = snapshot(serde_json::json!({
            "ready": true,
            "devices": [],
            "transfers": [{
                "id": "tr-2", "name": "b.bin", "device": "phone", "direction": "receive",
                "total": 10, "done": 4, "bytes_per_sec": null, "state": "paused",
                "files": [{ "name": "b.bin", "size": 10, "sha256": null }]
            }],
            "pending_offers": [{
                "id": "tr-1", "from": "phone", "peer_id": null,
                "files": [{ "name": "../a.txt", "size": 3 }],
                "cleaned_names": ["a.txt"], "total": 3, "created_at": 5
            }],
            "received": [],
            "history": [{
                "id": "tr-0", "name": "z", "device": "phone", "direction": "receive",
                "size": 1, "result": "declined", "timestamp_ms": 1
            }],
            "settings": { "receive_mode": "ask", "auto_accept_peers": [], "max_bytes": 100, "received_dir": "/x" }
        }));
        assert_eq!(locate(&s, "tr-1"), Whereabouts::Pending);
        assert!(matches!(locate(&s, "tr-2"), Whereabouts::Active(t) if t.state == TransferStatus::Paused));
        assert_eq!(locate(&s, "tr-0"), Whereabouts::Ended(Some(TransferResult::Declined)));
        assert_eq!(locate(&s, "tr-9"), Whereabouts::Ended(None));
        assert_eq!(end_of(Some(TransferResult::Declined)), Ok(UploadEnd::Declined));
        assert_eq!(end_of(None), Ok(UploadEnd::Cancelled));
    }
}
