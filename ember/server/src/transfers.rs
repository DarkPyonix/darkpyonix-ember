//! Server-side file transfer manager and HTTP API (SPEC `FR-F1`..`FR-F10`, `#487`, `#527`).
//!
//! Exposes the local device's file transfer state, offer inbox, active transfer controls
//! (pause, resume, cancel), chunk streaming, received-files folder (`<data_dir>/received/` by
//! default, mode `0644`, never overwriting existing files) and transfer history over the owner
//! API (`/api/transfers*`).
//!
//! All name cleaning (`FR-F9`, `#527`), 4 GiB default size limit (`FR-F10`), receive mode
//! decisions (`FR-F9`), chunk SHA-256 verification and resume (`FR-F5`), and partial-file
//! cleanup (`FR-F4`) are delegated to [`ember_transfer`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::extract::{Path as AxumPath, State as AxumState};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use base64::Engine;
use ember_transfer::limits::{Limits, DEFAULT_MAX_BYTES};
use ember_transfer::mode::ReceiveMode;
use ember_transfer::names::clean_offer_paths;
use ember_transfer::store::{
    DeclaredFile, HistoryRecord, ReceiveWriter, StoreError, StoredFile, TransferDirection,
    TransferResult,
};
use ember_transfer::wire::DeclineCode;
use ember_transport::PeerId;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::computers::{self, Computers};
use crate::devices::Devices;
use crate::store::now_ms;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveStatus {
    Queued,
    Sending,
    Paused,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferItem {
    pub id: String,
    pub name: String,
    pub device: String,
    pub direction: TransferDirection,
    pub total: u64,
    pub done: u64,
    pub bytes_per_sec: Option<u64>,
    pub state: ActiveStatus,
    pub files: Vec<DeclaredFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingOfferItem {
    pub id: String,
    pub from: String,
    pub peer_id: Option<String>,
    pub files: Vec<DeclaredFile>,
    pub cleaned_names: Vec<String>,
    pub total: u64,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferDeviceItem {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub online: bool,
    pub peer_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferSettingsView {
    /// `"ask"`, `"auto_accept"`, or `"off"`.
    pub receive_mode: String,
    pub auto_accept_peers: Vec<String>,
    pub max_bytes: u64,
    pub received_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransfersSnapshot {
    pub ready: bool,
    pub devices: Vec<TransferDeviceItem>,
    pub transfers: Vec<TransferItem>,
    pub pending_offers: Vec<PendingOfferItem>,
    pub received: Vec<StoredFile>,
    pub history: Vec<HistoryRecord>,
    pub settings: TransferSettingsView,
}

#[derive(Debug, thiserror::Error)]
pub enum TransferApiError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    PayloadTooLarge(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Internal(String),
}

impl IntoResponse for TransferApiError {
    fn into_response(self) -> Response {
        let code = match &self {
            TransferApiError::NotFound(_) => StatusCode::NOT_FOUND,
            TransferApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            TransferApiError::Forbidden(_) => StatusCode::FORBIDDEN,
            TransferApiError::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            TransferApiError::Conflict(_) => StatusCode::CONFLICT,
            TransferApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (code, Json(json!({ "error": self.to_string() }))).into_response()
    }
}

struct Inner {
    received_dir: PathBuf,
    history_path: PathBuf,
    limits: Limits,
    mode: ReceiveMode,
    /// Canonical data folder, for the `received_dir` setting check (`FR-F12`).
    data_dir: PathBuf,
    pending_offers: Vec<PendingOfferItem>,
    transfers: Vec<TransferItem>,
    writers: HashMap<String, ReceiveWriter>,
    received: Vec<StoredFile>,
    history: Vec<HistoryRecord>,
}

impl Inner {
    fn save_history(&self) {
        if let Ok(bytes) = serde_json::to_vec_pretty(&self.history) {
            let _ = std::fs::write(&self.history_path, bytes);
        }
    }
}

/// Server-side file transfer manager (`FR-F1`..`FR-F10`).
pub struct TransferManager {
    inner: Mutex<Inner>,
    devices: Option<Arc<Devices>>,
    computers: Option<Arc<Computers>>,
}

impl TransferManager {
    pub fn new(
        data_dir: &Path,
        devices: Option<Arc<Devices>>,
        computers: Option<Arc<Computers>>,
    ) -> Arc<Self> {
        let received_dir = data_dir.join("received");
        let _ = std::fs::create_dir_all(&received_dir);
        // No transfer state survives a restart, so every partial folder left behind is stale
        // and is removed before any offer can meet it (`FR-F11`, #773).
        let _ = std::fs::remove_dir_all(received_dir.join(".partial"));
        let data_dir = std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
        let history_path = data_dir.join("transfers-history.json");
        let history: Vec<HistoryRecord> = std::fs::read(&history_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Arc::new(Self {
            inner: Mutex::new(Inner {
                received_dir,
                history_path,
                limits: Limits { max_bytes: DEFAULT_MAX_BYTES },
                mode: ReceiveMode::Ask,
                data_dir,
                pending_offers: Vec::new(),
                transfers: Vec::new(),
                writers: HashMap::new(),
                received: Vec::new(),
                history,
            }),
            devices,
            computers,
        })
    }

    fn is_peer_registered(&self, peer_id: &str) -> bool {
        let Ok(peer) = peer_id.parse::<PeerId>() else {
            return false;
        };
        if let Some(devs) = &self.devices {
            if devs.gate().is_allowed(&peer) {
                return true;
            }
        }
        if let Some(comps) = &self.computers {
            if let Ok(list) = comps.registry().list() {
                if list.iter().any(|c| c.peer.as_ref().is_some_and(|p| p.peer == peer)) {
                    return true;
                }
            }
        }
        false
    }

    fn known_devices(&self) -> Vec<TransferDeviceItem> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        if let Some(devs) = &self.devices {
            if let Ok(list) = devs.list() {
                for d in list {
                    let id = d.peer_id.to_string();
                    if seen.insert(id.clone()) {
                        out.push(TransferDeviceItem {
                            id: id.clone(),
                            name: d.name,
                            kind: "device".into(),
                            online: true,
                            peer_id: Some(id),
                        });
                    }
                }
            }
        }
        if let Some(comps) = &self.computers {
            if let Ok(list) = comps.registry().list() {
                for c in list {
                    if c.id == computers::LOCAL {
                        continue;
                    }
                    let peer_str = c.peer.as_ref().map(|p| p.peer.to_string());
                    if seen.insert(c.id.clone()) {
                        out.push(TransferDeviceItem {
                            id: c.id,
                            name: c.name,
                            kind: "computer".into(),
                            online: true,
                            peer_id: peer_str,
                        });
                    }
                }
            }
        }
        out
    }

    pub fn snapshot(&self) -> TransfersSnapshot {
        let g = self.inner.lock().unwrap();
        let (mode_str, auto_peers) = match &g.mode {
            ReceiveMode::Ask => ("ask".to_string(), Vec::new()),
            ReceiveMode::Off => ("off".to_string(), Vec::new()),
            ReceiveMode::AutoAcceptFrom(set) => (
                "auto_accept".to_string(),
                set.iter().map(|p| p.to_string()).collect(),
            ),
        };
        TransfersSnapshot {
            ready: true,
            devices: self.known_devices(),
            transfers: g.transfers.clone(),
            pending_offers: g.pending_offers.clone(),
            received: g.received.clone(),
            history: g.history.clone(),
            settings: TransferSettingsView {
                receive_mode: mode_str,
                auto_accept_peers: auto_peers,
                max_bytes: g.limits.max_bytes,
                received_dir: g.received_dir.to_string_lossy().into_owned(),
            },
        }
    }

    /// Submits an incoming offer (`FR-F3`, `FR-F9`, `FR-F10`).
    ///
    /// Enforces:
    /// 1. Registered peer check when `peer_id` is supplied (`FR-F9`, `FR-F10`).
    /// 2. Non-empty file list, file count cap (`MAX_OFFER_FILES`), and `Limits` size check (`FR-F10`).
    /// 3. Path/name cleaning and intra-offer collision disambiguation (`FR-F9`, `#527`).
    /// 4. Receive mode (`Off` refuses immediately; `AutoAcceptFrom` starts receiving immediately;
    ///    `Ask` queues in `pending_offers`).
    pub fn receive_offer(
        &self,
        from: &str,
        peer_id: Option<&str>,
        files: Vec<DeclaredFile>,
    ) -> Result< serde_json::Value, TransferApiError> {
        if let Some(pid) = peer_id {
            if !self.is_peer_registered(pid) {
                return Err(TransferApiError::Forbidden(format!(
                    "{:?}: peer is not a registered device",
                    DeclineCode::NotRegistered
                )));
            }
        }
        if files.is_empty() {
            return Err(TransferApiError::BadRequest("the offer has no files".into()));
        }
        if files.len() > ember_transfer::names::MAX_OFFER_FILES {
            return Err(TransferApiError::BadRequest("the offer has too many files".into()));
        }
        let mut total: u64 = 0;
        for f in &files {
            total = total
                .checked_add(f.size)
                .ok_or_else(|| TransferApiError::BadRequest("file sizes overflow".into()))?;
        }

        let mut g = self.inner.lock().unwrap();
        if let Err(e) = g.limits.check_total(total) {
            return Err(TransferApiError::PayloadTooLarge(format!(
                "{:?}: {e}",
                DeclineCode::TooLarge
            )));
        }
        if matches!(g.mode, ReceiveMode::Off) {
            return Err(TransferApiError::Forbidden(format!(
                "{:?}: receive mode is off",
                DeclineCode::ReceiveOff
            )));
        }

        let raw_names: Vec<String> = files.iter().map(|f| f.name.clone()).collect();
        let cleaned_comps = clean_offer_paths(&raw_names);
        let cleaned_names: Vec<String> = cleaned_comps.into_iter().map(|c| c.join("/")).collect();
        let cleaned_files: Vec<DeclaredFile> = files
            .into_iter()
            .zip(cleaned_names.iter())
            .map(|(f, clean)| DeclaredFile {
                name: clean.clone(),
                size: f.size,
                sha256: f.sha256,
            })
            .collect();

        // 128 random bits, so ids never repeat across restarts (`FR-F11`, #773).
        let id = format!("tr-{}", uuid::Uuid::new_v4().simple());

        let auto_accept = match (&g.mode, peer_id.and_then(|p| p.parse::<PeerId>().ok())) {
            (ReceiveMode::AutoAcceptFrom(set), Some(peer)) => set.contains(&peer),
            _ => false,
        };

        let offer = PendingOfferItem {
            id: id.clone(),
            from: from.trim().to_string(),
            peer_id: peer_id.map(String::from),
            files: cleaned_files,
            cleaned_names,
            total,
            created_at: now_ms(),
        };
        g.pending_offers.push(offer.clone());
        drop(g);

        if auto_accept {
            let tr = self.accept_offer(&id)?;
            Ok(json!({ "status": "accepted", "id": id, "transfer": tr }))
        } else {
            Ok(json!({ "status": "offered", "id": id, "offer": offer }))
        }
    }

    /// Accepts a pending offer and opens its partial receive writer (`FR-F3`, `FR-F5`).
    pub fn accept_offer(&self, id: &str) -> Result<TransferItem, TransferApiError> {
        let mut g = self.inner.lock().unwrap();
        let pos = g
            .pending_offers
            .iter()
            .position(|o| o.id == id)
            .ok_or_else(|| TransferApiError::NotFound(format!("offer {id} not found")))?;
        let offer = g.pending_offers.remove(pos);
        let writer = ReceiveWriter::open(
            &g.received_dir,
            &offer.id,
            &offer.from,
            &offer.files,
            g.limits,
        )
        .map_err(|e| TransferApiError::BadRequest(e.to_string()))?;
        let done = writer.total_written();
        g.writers.insert(offer.id.clone(), writer);
        let label = offer
            .cleaned_names
            .first()
            .cloned()
            .unwrap_or_else(|| "file".into());
        let item = TransferItem {
            id: offer.id,
            name: label,
            device: offer.from,
            direction: TransferDirection::Receive,
            total: offer.total,
            done,
            bytes_per_sec: None,
            state: ActiveStatus::Sending,
            files: offer.files,
        };
        g.transfers.insert(0, item.clone());
        Ok(item)
    }

    /// Declines a pending offer (`FR-F3`).
    pub fn decline_offer(&self, id: &str) -> Result<HistoryRecord, TransferApiError> {
        let mut g = self.inner.lock().unwrap();
        let pos = g
            .pending_offers
            .iter()
            .position(|o| o.id == id)
            .ok_or_else(|| TransferApiError::NotFound(format!("offer {id} not found")))?;
        let offer = g.pending_offers.remove(pos);
        let rec = HistoryRecord {
            id: offer.id,
            name: offer
                .cleaned_names
                .first()
                .cloned()
                .unwrap_or_else(|| "file".into()),
            device: offer.from,
            direction: TransferDirection::Receive,
            size: offer.total,
            result: TransferResult::Declined,
            timestamp_ms: now_ms(),
        };
        g.history.insert(0, rec.clone());
        g.save_history();
        Ok(rec)
    }

    /// Returns the verified resume byte offset for `file` in transfer `id` (`FR-F5`).
    pub fn resume_offset(&self, id: &str, file: usize) -> Result<u64, TransferApiError> {
        let mut g = self.inner.lock().unwrap();
        if !g.writers.contains_key(id) {
            let tr = g
                .transfers
                .iter()
                .find(|t| t.id == id)
                .cloned()
                .ok_or_else(|| TransferApiError::NotFound(format!("transfer {id} not found")))?;
            let writer = ReceiveWriter::open(
                &g.received_dir,
                &tr.id,
                &tr.device,
                &tr.files,
                g.limits,
            )
            .map_err(|e| TransferApiError::BadRequest(e.to_string()))?;
            g.writers.insert(id.to_string(), writer);
        }
        let writer = g
            .writers
            .get(id)
            .ok_or_else(|| TransferApiError::NotFound(format!("transfer {id} not found")))?;
        writer
            .resume_offset(file)
            .map_err(|e| TransferApiError::BadRequest(e.to_string()))
    }

    /// Appends a chunk to an active transfer (`FR-F5`, `FR-F10`).
    pub fn write_chunk(
        &self,
        id: &str,
        file: usize,
        offset: u64,
        data: &[u8],
        sha256: Option<&str>,
    ) -> Result<TransferItem, TransferApiError> {
        let mut g = self.inner.lock().unwrap();
        let state = g
            .transfers
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.state)
            .ok_or_else(|| TransferApiError::NotFound(format!("transfer {id} not found")))?;
        if state == ActiveStatus::Paused {
            return Err(TransferApiError::Conflict("transfer is paused".into()));
        }
        if state != ActiveStatus::Sending && state != ActiveStatus::Queued {
            return Err(TransferApiError::Conflict(format!(
                "transfer is in state {state:?}"
            )));
        }
        if !g.writers.contains_key(id) {
            let tr = g.transfers.iter().find(|t| t.id == id).cloned().unwrap();
            let writer = ReceiveWriter::open(
                &g.received_dir,
                &tr.id,
                &tr.device,
                &tr.files,
                g.limits,
            )
            .map_err(|e| TransferApiError::BadRequest(e.to_string()))?;
            g.writers.insert(id.to_string(), writer);
        }
        let writer = g.writers.get_mut(id).unwrap();
        let done = writer
            .write_chunk(file, offset, data, sha256)
            .map_err(|e| match e {
                StoreError::LimitExceeded { .. } | StoreError::FileSizeExceeded { .. } => {
                    TransferApiError::PayloadTooLarge(e.to_string())
                }
                StoreError::OffsetMismatch { .. } => TransferApiError::Conflict(e.to_string()),
                _ => TransferApiError::BadRequest(e.to_string()),
            })?;
        let tr = g.transfers.iter_mut().find(|t| t.id == id).unwrap();
        tr.state = ActiveStatus::Sending;
        tr.done = done;
        Ok(tr.clone())
    }

    /// Finalizes a transfer, verifying final SHA-256 hashes and moving files to `received_dir`
    /// with mode `0644` (`FR-F5`, `FR-F8`, `FR-F9`).
    pub fn complete_transfer(&self, id: &str) -> Result<Vec<StoredFile>, TransferApiError> {
        let mut g = self.inner.lock().unwrap();
        let writer = g
            .writers
            .remove(id)
            .ok_or_else(|| TransferApiError::NotFound(format!("active transfer {id} not found")))?;
        let stored = writer.finalize().map_err(|e| {
            if let Some(tr) = g.transfers.iter_mut().find(|t| t.id == id) {
                tr.state = ActiveStatus::Failed;
            }
            TransferApiError::BadRequest(e.to_string())
        })?;
        if let Some(pos) = g.transfers.iter().position(|t| t.id == id) {
            let mut tr = g.transfers.remove(pos);
            tr.done = tr.total;
            tr.state = ActiveStatus::Done;
            for s in &stored {
                g.received.insert(0, s.clone());
                g.history.insert(
                    0,
                    HistoryRecord {
                        id: tr.id.clone(),
                        name: s.name.clone(),
                        device: tr.device.clone(),
                        direction: tr.direction,
                        size: s.size,
                        result: TransferResult::Done,
                        timestamp_ms: now_ms(),
                    },
                );
            }
            g.save_history();
        }
        Ok(stored)
    }

    pub fn pause_transfer(&self, id: &str) -> Result<TransferItem, TransferApiError> {
        let mut g = self.inner.lock().unwrap();
        let tr = g
            .transfers
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| TransferApiError::NotFound(format!("transfer {id} not found")))?;
        if tr.state != ActiveStatus::Sending {
            return Err(TransferApiError::Conflict(format!(
                "cannot pause transfer in state {:?}",
                tr.state
            )));
        }
        tr.state = ActiveStatus::Paused;
        Ok(tr.clone())
    }

    pub fn resume_transfer(&self, id: &str) -> Result<TransferItem, TransferApiError> {
        let mut g = self.inner.lock().unwrap();
        let tr = g
            .transfers
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| TransferApiError::NotFound(format!("transfer {id} not found")))?;
        if tr.state != ActiveStatus::Paused && tr.state != ActiveStatus::Failed {
            return Err(TransferApiError::Conflict(format!(
                "cannot resume transfer in state {:?}",
                tr.state
            )));
        }
        tr.state = ActiveStatus::Sending;
        Ok(tr.clone())
    }

    /// Cancels a pending offer or active transfer and removes partial files (`FR-F4`).
    pub fn cancel_transfer(&self, id: &str) -> Result<HistoryRecord, TransferApiError> {
        let mut g = self.inner.lock().unwrap();
        if let Some(writer) = g.writers.remove(id) {
            writer.abort();
        }
        if let Some(pos) = g.pending_offers.iter().position(|o| o.id == id) {
            let offer = g.pending_offers.remove(pos);
            let rec = HistoryRecord {
                id: offer.id,
                name: offer
                    .cleaned_names
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "file".into()),
                device: offer.from,
                direction: TransferDirection::Receive,
                size: offer.total,
                result: TransferResult::Cancelled,
                timestamp_ms: now_ms(),
            };
            g.history.insert(0, rec.clone());
            g.save_history();
            return Ok(rec);
        }
        let pos = g
            .transfers
            .iter()
            .position(|t| t.id == id)
            .ok_or_else(|| TransferApiError::NotFound(format!("transfer {id} not found")))?;
        let tr = g.transfers.remove(pos);
        let rec = HistoryRecord {
            id: tr.id,
            name: tr.name,
            device: tr.device,
            direction: tr.direction,
            size: tr.total,
            result: TransferResult::Cancelled,
            timestamp_ms: now_ms(),
        };
        g.history.insert(0, rec.clone());
        g.save_history();
        Ok(rec)
    }

    /// Clears transfer history without deleting received files on disk (`FR-F8`).
    pub fn clear_history(&self) {
        let mut g = self.inner.lock().unwrap();
        g.history.clear();
        g.save_history();
    }

    pub fn update_settings(
        &self,
        receive_mode: Option<&str>,
        auto_accept_peers: Option<Vec<String>>,
        max_bytes: Option<u64>,
        received_dir: Option<PathBuf>,
    ) -> Result<TransferSettingsView, TransferApiError> {
        let mut g = self.inner.lock().unwrap();
        // Check the folder first so a refused request changes nothing (`FR-F12`).
        let received_dir = match received_dir {
            Some(dir) => Some(check_received_dir(&g.data_dir, &dir)?),
            None => None,
        };
        if let Some(m) = receive_mode {
            g.mode = match m {
                "ask" => ReceiveMode::Ask,
                "off" => ReceiveMode::Off,
                "auto_accept" => {
                    let mut set = HashSet::new();
                    for p in auto_accept_peers.unwrap_or_default() {
                        let pid = p.parse::<PeerId>().map_err(|e| {
                            TransferApiError::BadRequest(format!("invalid peer_id {p:?}: {e}"))
                        })?;
                        set.insert(pid);
                    }
                    ReceiveMode::AutoAcceptFrom(set)
                }
                other => {
                    return Err(TransferApiError::BadRequest(format!(
                        "unknown receive_mode {other:?}"
                    )))
                }
            };
        }
        if let Some(max) = max_bytes {
            if max == 0 {
                return Err(TransferApiError::BadRequest(
                    "max_bytes must be greater than zero".into(),
                ));
            }
            g.limits = Limits { max_bytes: max };
        }
        if let Some(dir) = received_dir {
            g.received_dir = dir;
        }
        drop(g);
        Ok(self.snapshot().settings)
    }
}

/// Checks a requested received-files folder and returns its canonical path (`FR-F12`, #773).
/// The rules are shared with the node (`FR-F13`): [`ember_transfer::received_dir::check`].
fn check_received_dir(data_dir: &Path, dir: &Path) -> Result<PathBuf, TransferApiError> {
    ember_transfer::received_dir::check(data_dir, dir)
        .map_err(|why| TransferApiError::BadRequest(format!("invalid received_dir: {why}")))
}

#[derive(Deserialize)]
struct CreateOfferBody {
    from: String,
    #[serde(default)]
    peer_id: Option<String>,
    files: Vec<DeclaredFile>,
}

#[derive(Deserialize)]
struct WriteChunkBody {
    #[serde(default)]
    file: usize,
    offset: u64,
    data_base64: String,
    #[serde(default)]
    sha256: Option<String>,
}

#[derive(Deserialize)]
struct UpdateSettingsBody {
    #[serde(default)]
    receive_mode: Option<String>,
    #[serde(default)]
    auto_accept_peers: Option<Vec<String>>,
    #[serde(default)]
    max_bytes: Option<u64>,
    #[serde(default)]
    received_dir: Option<String>,
}

pub fn router(mgr: Arc<TransferManager>) -> Router {
    Router::new()
        .route("/api/transfers", get(get_snapshot))
        .route("/api/transfers/offers", post(post_offer))
        .route("/api/transfers/offers/{id}/accept", post(post_accept))
        .route("/api/transfers/offers/{id}/decline", post(post_decline))
        .route("/api/transfers/{id}/chunks", post(post_chunk))
        .route("/api/transfers/{id}/resume-offset/{file}", get(get_resume_offset))
        .route("/api/transfers/{id}/complete", post(post_complete))
        .route("/api/transfers/{id}/pause", post(post_pause))
        .route("/api/transfers/{id}/resume", post(post_resume))
        .route("/api/transfers/{id}/cancel", post(post_cancel))
        .route("/api/transfers/history", get(get_history).delete(delete_history))
        .route("/api/transfers/settings", put(put_settings))
        .with_state(mgr)
}

async fn get_snapshot(
    AxumState(mgr): AxumState<Arc<TransferManager>>,
) -> Result<impl IntoResponse, TransferApiError> {
    Ok(Json(mgr.snapshot()))
}

async fn post_offer(
    AxumState(mgr): AxumState<Arc<TransferManager>>,
    Json(body): Json<CreateOfferBody>,
) -> Result<impl IntoResponse, TransferApiError> {
    let val = mgr.receive_offer(&body.from, body.peer_id.as_deref(), body.files)?;
    Ok((StatusCode::CREATED, Json(val)))
}

async fn post_accept(
    AxumState(mgr): AxumState<Arc<TransferManager>>,
    AxumPath(id): AxumPath<String>,
) -> Result<impl IntoResponse, TransferApiError> {
    Ok(Json(mgr.accept_offer(&id)?))
}

async fn post_decline(
    AxumState(mgr): AxumState<Arc<TransferManager>>,
    AxumPath(id): AxumPath<String>,
) -> Result<impl IntoResponse, TransferApiError> {
    Ok(Json(mgr.decline_offer(&id)?))
}

async fn post_chunk(
    AxumState(mgr): AxumState<Arc<TransferManager>>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<WriteChunkBody>,
) -> Result<impl IntoResponse, TransferApiError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(body.data_base64.trim())
        .map_err(|e| TransferApiError::BadRequest(format!("invalid base64 chunk: {e}")))?;
    let tr = mgr.write_chunk(&id, body.file, body.offset, &bytes, body.sha256.as_deref())?;
    Ok(Json(tr))
}

async fn get_resume_offset(
    AxumState(mgr): AxumState<Arc<TransferManager>>,
    AxumPath((id, file)): AxumPath<(String, usize)>,
) -> Result<impl IntoResponse, TransferApiError> {
    let offset = mgr.resume_offset(&id, file)?;
    Ok(Json(json!({ "id": id, "file": file, "offset": offset })))
}

async fn post_complete(
    AxumState(mgr): AxumState<Arc<TransferManager>>,
    AxumPath(id): AxumPath<String>,
) -> Result<impl IntoResponse, TransferApiError> {
    let stored = mgr.complete_transfer(&id)?;
    Ok(Json(json!({ "stored": stored })))
}

async fn post_pause(
    AxumState(mgr): AxumState<Arc<TransferManager>>,
    AxumPath(id): AxumPath<String>,
) -> Result<impl IntoResponse, TransferApiError> {
    Ok(Json(mgr.pause_transfer(&id)?))
}

async fn post_resume(
    AxumState(mgr): AxumState<Arc<TransferManager>>,
    AxumPath(id): AxumPath<String>,
) -> Result<impl IntoResponse, TransferApiError> {
    Ok(Json(mgr.resume_transfer(&id)?))
}

async fn post_cancel(
    AxumState(mgr): AxumState<Arc<TransferManager>>,
    AxumPath(id): AxumPath<String>,
) -> Result<impl IntoResponse, TransferApiError> {
    Ok(Json(mgr.cancel_transfer(&id)?))
}

async fn get_history(
    AxumState(mgr): AxumState<Arc<TransferManager>>,
) -> Result<impl IntoResponse, TransferApiError> {
    Ok(Json(mgr.snapshot().history))
}

async fn delete_history(
    AxumState(mgr): AxumState<Arc<TransferManager>>,
) -> Result<impl IntoResponse, TransferApiError> {
    mgr.clear_history();
    Ok(Json(json!({ "cleared": true })))
}

async fn put_settings(
    AxumState(mgr): AxumState<Arc<TransferManager>>,
    Json(body): Json<UpdateSettingsBody>,
) -> Result<impl IntoResponse, TransferApiError> {
    let settings = mgr.update_settings(
        body.receive_mode.as_deref(),
        body.auto_accept_peers,
        body.max_bytes,
        body.received_dir.map(PathBuf::from),
    )?;
    Ok(Json(settings))
}
