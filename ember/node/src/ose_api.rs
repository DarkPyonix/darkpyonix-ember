//! The node's OSE routes (SPEC FR-W4a, FR-W4b, #896): the Ember editor's runtime for the projects
//! of this computer.
//!
//! | Method | Path | Body | Answer |
//! | ------ | ---- | ---- | ------ |
//! | POST | `/ide/ose` | [`OseLaunchRequest`] | 200 [`OseLaunchResponse`] |
//! | GET (WS) | `/ide/ose/{launch_id}/relay` | binary frames | binary frames |
//! | POST | `/ide/extensions` | `{"id":"publisher.name","version":"1.2.3"}` | 200 `{installed,id,output}` |
//!
//! All three sit behind the node's bearer token (and the peer allow-list over the transport), like
//! every route but `/health`. The folder of a launch is checked by the path policy first (400 for a
//! relative path or a file, 403 outside the roots, 404 when missing). A node with no OSE binary
//! answers 503, a start that fails 500, both with the reason.
//!
//! **The real connection token never leaves this computer.** The launch answers a placeholder of
//! the same length. The relay replaces it with the real token inside the first
//! [`RELAY_SUBSTITUTION_WINDOW`] bytes the client sends, so whoever bridges the relay (the server,
//! SPEC FR-W4c) only carries bytes.
//!
//! [`pump_ose_relay`] is also what the server runs for the OSE of its own machine.

use std::path::{Path as FsPath, PathBuf};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::api::{ApiError, Node};
use crate::client::{ClientError, NodeClient, NodeIo};
use crate::ose::{InstallError, RelayLookupError, RelayTarget, LOCAL_COMPUTER, NOT_INSTALLED};
use crate::proto::ErrorCode;

/// Byte window at the start of the client-to-OSE stream in which the C2 placeholder is replaced
/// with the real OSE connection token (`FR-W4`, `#458`, `#461`).
pub const RELAY_SUBSTITUTION_WINDOW: usize = 8 * 1024;
/// Timeout for opening the loopback TCP connection to the local OSE process.
const RELAY_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Maximum chunk size read from the OSE TCP socket per WebSocket binary frame.
const RELAY_READ_CHUNK: usize = 32 * 1024;

/// Body of `POST /ide/ose`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OseLaunchRequest {
    /// The project folder, absolute, inside the node's allowed roots.
    pub folder: PathBuf,
}

/// Answer of `POST /ide/ose`: the launch of the folder's OSE on this node.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OseLaunchResponse {
    /// 32 hex characters: the `{launch_id}` of the relay route.
    pub launch_id: String,
    /// `http://127.0.0.1:<port>` on this node. Only the node's own relay dials it.
    pub endpoint: String,
    /// The placeholder token, never the real one: the relay puts the real token in its place.
    pub connection_token: String,
}

pub(crate) fn routes() -> Router<Node> {
    Router::new()
        .route("/ide/ose", post(launch))
        .route("/ide/ose/{launch_id}/relay", get(relay))
        .route("/ide/extensions", post(install_extension))
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal, e.to_string())
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
    let ose = n.ose().clone();
    // A new OSE takes up to 10 s to listen: keep the wait off the async workers.
    let started = tokio::task::spawn_blocking(move || ose.launch_for(LOCAL_COMPUTER, &folder))
        .await
        .map_err(|e| internal(format!("OSE launch task failed: {e}")))?;
    match started {
        Ok(l) => Ok(Json(OseLaunchResponse {
            launch_id: l.launch_id,
            endpoint: l.connection.endpoint,
            connection_token: l.connection.connection_token,
        })),
        Err(m) if m == NOT_INSTALLED => Err(ApiError::new(StatusCode::SERVICE_UNAVAILABLE, ErrorCode::Internal, m)),
        Err(m) => Err(internal(m)),
    }
}

async fn relay(State(n): State<Node>, Path(launch_id): Path<String>, ws: WebSocketUpgrade) -> Response {
    // The lookup takes the manager's lock, which a starting OSE holds for up to 10 s: wait for it
    // off the async workers, so exec and terminal traffic is not delayed.
    let ose = n.ose().clone();
    let id = launch_id.clone();
    let found = match tokio::task::spawn_blocking(move || ose.open_relay(LOCAL_COMPUTER, &id)).await {
        Ok(found) => found,
        Err(e) => return internal(format!("relay lookup task failed: {e}")).into_response(),
    };
    match found {
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

/// An error answer of the install route: the node's `{code, error}` body, which is also the
/// `{error}` shape of the server's own install route.
fn install_error(status: StatusCode, code: ErrorCode, msg: impl Into<String>) -> Response {
    ApiError::new(status, code, msg).into_response()
}

async fn install_extension(State(n): State<Node>, body: Bytes) -> Response {
    let req: InstallRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return install_error(StatusCode::BAD_REQUEST, ErrorCode::BadRequest, format!("bad request body: {e}")),
    };
    let ose = n.ose().clone();
    let (id, version) = (req.id.clone(), req.version.clone());
    let ran =
        tokio::task::spawn_blocking(move || ose.install_extension(LOCAL_COMPUTER, &id, version.as_deref())).await;
    match ran {
        Ok(Ok(output)) => Json(json!({ "installed": true, "id": req.id, "output": output })).into_response(),
        Ok(Err(InstallError::Bad(m))) => install_error(StatusCode::BAD_REQUEST, ErrorCode::BadRequest, m),
        Ok(Err(InstallError::NotInstalled)) => {
            install_error(StatusCode::SERVICE_UNAVAILABLE, ErrorCode::Internal, NOT_INSTALLED)
        }
        Ok(Err(InstallError::Busy)) => {
            install_error(StatusCode::CONFLICT, ErrorCode::Conflict, "an install is already running on this computer")
        }
        Ok(Err(InstallError::Failed { message, output })) => {
            let body: Value = json!({
                "code": ErrorCode::Internal,
                "error": message,
                "id": req.id,
                "output": output,
                "installed": false,
            });
            (StatusCode::BAD_GATEWAY, Json(body)).into_response()
        }
        Ok(Err(InstallError::Io(m))) => install_error(StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal, m),
        Err(e) => install_error(StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal, format!("install task failed: {e}")),
    }
}

/// C2 placeholder-to-real-token substitution for the first `window` bytes of the
/// client-to-OSE byte stream (`FR-W4`, `#458`, `#461`).
///
/// Substitution works within one client WebSocket message: every placeholder that starts inside
/// the window and lies whole in the message is replaced. Each client message goes to the OSE as
/// exactly one write, never split, held or merged with another (#776). A placeholder split across
/// two messages is not replaced; Code-OSS and `ember-editor-conn` write each protocol message
/// (header and body, including the `auth` control message) as one WebSocket message, so the token
/// never straddles messages. Placeholder and real token have the same length, so byte offsets
/// do not shift.
struct TokenSubstituter {
    placeholder: Vec<u8>,
    real_token: Vec<u8>,
    window: usize,
    seen: usize,
}

impl TokenSubstituter {
    fn new(placeholder: &str, real_token: &str, window: usize) -> Self {
        Self {
            placeholder: placeholder.as_bytes().to_vec(),
            real_token: real_token.as_bytes().to_vec(),
            window,
            seen: 0,
        }
    }

    fn enabled(&self) -> bool {
        !self.placeholder.is_empty()
            && self.placeholder.len() == self.real_token.len()
            && self.placeholder != self.real_token
    }

    /// One client message in, the same message (same length) out.
    fn feed(&mut self, msg: &[u8]) -> Vec<u8> {
        let start = self.seen;
        self.seen = self.seen.saturating_add(msg.len());
        if !self.enabled() || start >= self.window {
            return msg.to_vec();
        }
        let in_win = (self.window - start).min(msg.len());
        let pat_len = self.placeholder.len();
        let mut out = msg.to_vec();
        let mut i = 0;
        while i < in_win && i + pat_len <= msg.len() {
            if msg[i..i + pat_len] == self.placeholder[..] {
                out[i..i + pat_len].copy_from_slice(&self.real_token);
                i += pat_len;
            } else {
                i += 1;
            }
        }
        out
    }
}

/// One relayed OSE connection: the client's WebSocket on one side, a loopback TCP connection to
/// the OSE of `target` on the other, until either side ends or both are idle for the target's
/// idle timeout. The relay permit in `target` is released when this returns.
pub async fn pump_ose_relay(socket: WebSocket, target: RelayTarget) {
    let tcp = match tokio::time::timeout(RELAY_CONNECT_TIMEOUT, tokio::net::TcpStream::connect(target.address)).await {
        Ok(Ok(s)) => {
            let _ = s.set_nodelay(true);
            s
        }
        _ => {
            let mut socket = socket;
            let _ = socket
                .send(Message::Close(Some(CloseFrame {
                    code: 1011,
                    reason: "cannot connect to OSE server".into(),
                })))
                .await;
            return;
        }
    };
    let (mut tcp_read, mut tcp_write) = tcp.into_split();
    let (mut ws_out, mut ws_in) = socket.split();
    let mut sub = TokenSubstituter::new(&target.placeholder, &target.real_token, RELAY_SUBSTITUTION_WINDOW);
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
                    let out = sub.feed(&bytes);
                    if !out.is_empty() && tcp_write.write_all(&out).await.is_err() {
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
    /// Start or reuse the OSE of `folder` on the node (`POST /ide/ose`). The node's refusal is
    /// [`ClientError::Api`] with its status: 403 outside the roots, 404 for a missing folder, 503
    /// without an OSE binary.
    pub async fn ide_ose(&self, folder: impl AsRef<FsPath>) -> Result<OseLaunchResponse, ClientError> {
        self.post("/ide/ose", &OseLaunchRequest { folder: folder.as_ref().into() }).await
    }

    /// Open the relay of a launch. Send the editor's bytes as Binary frames; the OSE's bytes
    /// arrive as Binary frames. An unknown launch is [`ClientError::Api`] 404, a full relay 429.
    pub async fn ide_ose_relay(&self, launch_id: &str) -> Result<OseRelayStream, ClientError> {
        self.websocket(&format!("/ide/ose/{launch_id}/relay")).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: &str = "pppppppppppppppp";
    const R: &str = "rrrrrrrrrrrrrrrr";

    /// FR-W4b: a placeholder whole inside one message in the window is replaced; the length stays.
    #[test]
    fn fr_w4b_substituter_replaces_inside_one_message_in_the_window() {
        let mut s = TokenSubstituter::new(P, R, 64);
        let msg = format!("a={P}&b={P}");
        assert_eq!(s.feed(msg.as_bytes()), format!("a={R}&b={R}").into_bytes());
    }

    /// FR-W4b (#776): a placeholder split over two messages passes unchanged, each message whole.
    #[test]
    fn fr_w4b_substituter_leaves_a_split_placeholder_and_message_boundaries() {
        let mut s = TokenSubstituter::new(P, R, 64);
        let (head, tail) = P.split_at(5);
        assert_eq!(s.feed(head.as_bytes()), head.as_bytes());
        assert_eq!(s.feed(tail.as_bytes()), tail.as_bytes());
    }

    /// FR-W4b: past the window nothing is replaced; with equal or unequal-length tokens neither.
    #[test]
    fn fr_w4b_substituter_stops_after_the_window() {
        let mut s = TokenSubstituter::new(P, R, 8);
        assert_eq!(s.feed(b"12345678"), b"12345678");
        assert_eq!(s.feed(P.as_bytes()), P.as_bytes());
        let mut same = TokenSubstituter::new(P, P, 64);
        assert_eq!(same.feed(P.as_bytes()), P.as_bytes());
        let mut uneven = TokenSubstituter::new(P, "short", 64);
        assert_eq!(uneven.feed(P.as_bytes()), P.as_bytes());
    }
}
