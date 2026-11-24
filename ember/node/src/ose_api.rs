//! The node's editor routes (SPEC FR-W4a, FR-W4b, `INTENT.md` D55, #1064): the editor server of
//! this computer's projects is the DarkPyonix manager's ([`crate::editor_manager`]).
//!
//! | Method | Path | Body | Answer |
//! | ------ | ---- | ---- | ------ |
//! | POST | `/ide/ose` | [`OseLaunchRequest`] | 200 [`OseLaunchResponse`] |
//! | GET (WS) | `/ide/ose/{launch_id}/relay` | binary frames | binary frames |
//! | POST | `/ide/extensions` | `{"id":"publisher.name","version":"1.2.3"}` | the manager's status and body |
//!
//! All three sit behind the node's bearer token (and the peer allow-list over the transport), like
//! every route but `/health`. The folder of a launch is checked by the path policy first (400 for a
//! relative path or a file, 403 outside the roots, 404 when missing).
//!
//! **The node starts no editor server.** `POST /ide/ose {folder, target}` asks the manager's
//! `POST /api/editor/ose/launch` (both targets use the `ose` runtime) and answers `{launch_id,
//! path, ticket, ose_commit?}`. No manager known: 503 `manager_not_installed`. The manager's 409
//! (no editor server installed): 503 `ose_not_installed` with the manager's reason. Another
//! refusal: 502 `manager_error`. No answer: 502 `manager_unreachable`. The relay joins the client's
//! bytes to the manager's endpoint and replaces nothing; the manager authorises them by the ticket,
//! so the manager token never leaves this computer.
//!
//! [`pump_ose_relay`] is also what the server runs for the manager of its own machine.

use std::path::{Path as FsPath, PathBuf};

use axum::body::Bytes;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::api::{ApiError, Node};
use crate::client::{ClientError, NodeClient, NodeIo};
use crate::editor_manager::{
    connect_endpoint, ManagerError, RelayLaunch, RelayLookupError, RelayTarget, CONNECT_TIMEOUT, OSE_RUNTIME,
};
use crate::proto::ErrorCode;

/// Timeout for opening the connection to the manager.
const RELAY_CONNECT_TIMEOUT: std::time::Duration = CONNECT_TIMEOUT;
/// Maximum chunk size read from the manager per WebSocket binary frame.
const RELAY_READ_CHUNK: usize = 32 * 1024;

/// The client a launch serves (SPEC FR-W4b, D55): the Ember editor or VS Code Web in the IDE
/// window. Both reach the same editor server; the value is kept so a request names its client.
/// The JSON value is `"ember"` or `"vscode"`; a request without one is the Ember target.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OseTarget {
    #[default]
    Ember,
    Vscode,
}

impl OseTarget {
    /// True for the default target (`skip_serializing_if`).
    pub fn is_ember(&self) -> bool {
        *self == OseTarget::Ember
    }
}

/// Body of `POST /ide/ose`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OseLaunchRequest {
    /// The project folder, absolute, inside the node's allowed roots.
    pub folder: PathBuf,
    /// The client the launch serves; absent is the Ember target (D55).
    #[serde(default, skip_serializing_if = "OseTarget::is_ember")]
    pub target: OseTarget,
}

/// Answer of `POST /ide/ose`: `{launch_id, path, ticket, ose_commit?}`.
///
/// `path` and `ticket` default to empty so the answer of a node older than D55 (which had
/// `endpoint` and `connection_token` instead) still decodes, and the caller tells it by the empty
/// ticket.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OseLaunchResponse {
    /// 32 hex characters: the `{launch_id}` of the relay route.
    pub launch_id: String,
    /// `/ide/<runtime>/`: where the manager serves the editor server, behind the relay.
    #[serde(default)]
    pub path: String,
    /// The manager's launch ticket (`?dpx-ticket=` on the first page, or `Cookie: dpx-editor=`).
    #[serde(default)]
    pub ticket: String,
    /// The Code-OSS commit of the manager's editor server (SPEC FR-E10, #32), when it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ose_commit: Option<String>,
}

impl std::fmt::Debug for OseLaunchResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OseLaunchResponse")
            .field("launch_id", &self.launch_id)
            .field("path", &self.path)
            .field("ticket", &"[REDACTED]")
            .field("ose_commit", &self.ose_commit)
            .finish()
    }
}

impl From<RelayLaunch> for OseLaunchResponse {
    fn from(l: RelayLaunch) -> Self {
        OseLaunchResponse { launch_id: l.launch_id, path: l.launch.path, ticket: l.launch.ticket, ose_commit: l.launch.commit }
    }
}

pub(crate) fn routes() -> Router<Node> {
    Router::new()
        .route("/ide/ose", post(launch))
        .route("/ide/ose/{launch_id}/relay", get(relay))
        .route("/ide/extensions", post(install_extension))
}

/// The node's answer for a manager that gave no launch (SPEC FR-W4b).
fn manager_error(e: ManagerError) -> ApiError {
    match e {
        ManagerError::NotInstalled(m) => ApiError::new(StatusCode::SERVICE_UNAVAILABLE, ErrorCode::ManagerNotInstalled, m),
        ManagerError::Refused { status: 409, error } => {
            ApiError::new(StatusCode::SERVICE_UNAVAILABLE, ErrorCode::OseNotInstalled, error)
        }
        ManagerError::Refused { status, error } => {
            ApiError::new(StatusCode::BAD_GATEWAY, ErrorCode::ManagerError, format!("the manager answered {status}: {error}"))
        }
        ManagerError::Unreachable(m) => ApiError::new(StatusCode::BAD_GATEWAY, ErrorCode::ManagerUnreachable, m),
    }
}

async fn launch(State(n): State<Node>, Json(r): Json<OseLaunchRequest>) -> Result<Json<OseLaunchResponse>, ApiError> {
    let folder = n.policy().resolve_existing(&r.folder)?;
    if !folder.is_dir() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            ErrorCode::BadRequest,
            format!("not a directory: {}", r.folder.display()),
        ));
    }
    // Both targets use the manager's `ose` runtime; the folder is chosen per connection by the
    // client (`?folder=` of the first page), so the launch does not carry it.
    let launched = n.editor().launch(OSE_RUNTIME).await.map_err(manager_error)?;
    Ok(Json(launched.into()))
}

async fn relay(State(n): State<Node>, Path(launch_id): Path<String>, ws: WebSocketUpgrade) -> Response {
    match n.editor().open_relay(&launch_id) {
        Ok(target) => ws.on_upgrade(move |socket| pump_ose_relay(socket, target)).into_response(),
        Err(RelayLookupError::NotFound) => {
            ApiError::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, format!("unknown OSE launch: {launch_id}"))
                .into_response()
        }
        Err(RelayLookupError::TooManyRelays) => {
            ApiError::new(StatusCode::TOO_MANY_REQUESTS, ErrorCode::Conflict, "too many active IDE relays")
                .into_response()
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallRequest {
    id: String,
    version: Option<String>,
}

/// `POST /ide/extensions`: checks the body (400 for a bad one or a bad id), then forwards it to
/// the manager's `POST /api/editor/ose/extensions` and answers the manager's status and body
/// unchanged. No manager: 503 `manager_not_installed`; no answer: 502 `manager_unreachable`.
async fn install_extension(State(n): State<Node>, body: Bytes) -> Response {
    let req: InstallRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return ApiError::new(StatusCode::BAD_REQUEST, ErrorCode::BadRequest, format!("bad request body: {e}"))
                .into_response()
        }
    };
    if let Err(m) = crate::ose::extension_spec(&req.id, req.version.as_deref()) {
        return ApiError::new(StatusCode::BAD_REQUEST, ErrorCode::BadRequest, m).into_response();
    }
    match n.editor().install_extension(OSE_RUNTIME, body.to_vec()).await {
        Ok((status, answer)) => {
            let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
            (status, [(header::CONTENT_TYPE, "application/json")], answer).into_response()
        }
        Err(e) => manager_error(e).into_response(),
    }
}

/// One relayed editor connection: the client's WebSocket on one side, a connection to the
/// DarkPyonix manager of `target` on the other (TCP, or TLS for an `https` manager), until either
/// side ends or both are idle for the target's idle timeout. Nothing is rewritten: each client
/// message goes to the manager as exactly one write (`FR-N9.1`), and the client authenticates to
/// the manager with its launch ticket. The relay permit in `target` is released when this returns.
pub async fn pump_ose_relay(socket: WebSocket, target: RelayTarget) {
    let stream = match tokio::time::timeout(RELAY_CONNECT_TIMEOUT, connect_endpoint(&target.endpoint)).await {
        Ok(Ok(s)) => s,
        _ => {
            let mut socket = socket;
            let _ = socket
                .send(Message::Close(Some(CloseFrame {
                    code: 1011,
                    reason: "cannot connect to the DarkPyonix manager".into(),
                })))
                .await;
            return;
        }
    };
    let (mut tcp_read, mut tcp_write) = tokio::io::split(stream);
    let (mut ws_out, mut ws_in) = socket.split();
    let idle = target.idle_timeout;
    let last_activity_ns = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let started = std::time::Instant::now();
    let touch = {
        let last = last_activity_ns.clone();
        move || {
            last.store(
                started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                std::sync::atomic::Ordering::Relaxed,
            );
        }
    };

    // Bounded channel (capacity 4) for control frames (Pong / 1003 Close) from the reader side so
    // `ws_in` and `ws_out` never block each other across `tokio::select!`.
    let (ctrl_tx, mut ctrl_rx) = tokio::sync::mpsc::channel::<Message>(4);

    let touch_in = touch.clone();
    let c2s = async move {
        while let Some(msg) = ws_in.next().await {
            touch_in();
            match msg {
                Ok(Message::Binary(bytes)) => {
                    if !bytes.is_empty() && tcp_write.write_all(&bytes).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Text(_)) => {
                    let _ = ctrl_tx
                        .send(Message::Close(Some(CloseFrame {
                            code: 1003,
                            reason: "text frames are not allowed on the OSE binary relay".into(),
                        })))
                        .await;
                    let _ = tcp_write.shutdown().await;
                    break;
                }
                Ok(Message::Ping(p)) => {
                    if ctrl_tx.send(Message::Pong(p)).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Pong(_)) => {}
                Ok(Message::Close(_)) | Err(_) => break,
            }
        }
        let _ = tcp_write.shutdown().await;
    };

    let s2c = async move {
        let mut read_buf = vec![0u8; RELAY_READ_CHUNK];
        loop {
            tokio::select! {
                biased;
                ctrl = ctrl_rx.recv(), if !ctrl_rx.is_closed() => {
                    if let Some(frame) = ctrl {
                        let is_close = matches!(frame, Message::Close(_));
                        if ws_out.send(frame).await.is_err() || is_close {
                            break;
                        }
                    }
                }
                n = tcp_read.read(&mut read_buf) => {
                    match n {
                        Ok(0) | Err(_) => {
                            if let Ok(frame) = ctrl_rx.try_recv() {
                                let _ = ws_out.send(frame).await;
                            } else {
                                let _ = ws_out.close().await;
                            }
                            break;
                        }
                        Ok(n) => {
                            touch();
                            if ws_out.send(Message::Binary(read_buf[..n].to_vec().into())).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        }
    };

    let idle_watchdog = async {
        let check_step = (idle / 4).clamp(Duration::from_millis(20), Duration::from_secs(5));
        loop {
            tokio::time::sleep(check_step).await;
            let last = Duration::from_nanos(last_activity_ns.load(std::sync::atomic::Ordering::Relaxed));
            if started.elapsed().saturating_sub(last) >= idle {
                break;
            }
        }
    };

    let both = async {
        tokio::join!(c2s, s2c);
    };
    tokio::select! {
        _ = both => {}
        _ = idle_watchdog => {}
    }
}

/// Client side of `/ide/ose/{launch_id}/relay`: the byte stream of one editor socket, as binary
/// frames, over TCP or the transport alike ([`crate::client::NodeIo`]).
pub type OseRelayStream = tokio_tungstenite::WebSocketStream<NodeIo>;

impl NodeClient {
    /// A launch of the editor server for `folder` on the node (`POST /ide/ose`), for the Ember
    /// target. The node's refusal is [`ClientError::Api`] with its status: 403 outside the roots,
    /// 404 for a missing folder, 503 `manager_not_installed` or `ose_not_installed`, 502
    /// `manager_unreachable` or `manager_error`.
    pub async fn ide_ose(&self, folder: impl AsRef<FsPath>) -> Result<OseLaunchResponse, ClientError> {
        self.ide_ose_for(folder, OseTarget::Ember).await
    }

    /// Like [`NodeClient::ide_ose`] for one target. Both targets use the manager's `ose` runtime
    /// (D55). A node older than D55 answers an `endpoint` and a `connection_token` and no
    /// `ticket`, which the caller can tell by the empty `ticket`.
    pub async fn ide_ose_for(&self, folder: impl AsRef<FsPath>, target: OseTarget) -> Result<OseLaunchResponse, ClientError> {
        self.post("/ide/ose", &OseLaunchRequest { folder: folder.as_ref().into(), target }).await
    }

    /// Open the relay of a launch. Send the editor's bytes as Binary frames; the manager's bytes
    /// arrive as Binary frames. An unknown launch is [`ClientError::Api`] 404, a full relay 429.
    pub async fn ide_ose_relay(&self, launch_id: &str) -> Result<OseRelayStream, ClientError> {
        self.websocket(&format!("/ide/ose/{launch_id}/relay")).await
    }
}
