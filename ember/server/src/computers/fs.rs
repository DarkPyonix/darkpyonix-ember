//! File operations of a computer, relayed from its node (FR-W4, FR-L7, #456).
//!
//! | Method | Path | Node route |
//! | ------ | ---- | ---------- |
//! | POST   | `/api/computers/{id}/fs/stat`     | `POST /fs/stat` |
//! | POST   | `/api/computers/{id}/fs/lstat`    | `POST /fs/lstat` |
//! | POST   | `/api/computers/{id}/fs/read`     | `POST /fs/read` |
//! | POST   | `/api/computers/{id}/fs/list`     | `POST /fs/list` |
//! | POST   | `/api/computers/{id}/fs/readlink` | `POST /fs/readlink` |
//! | POST   | `/api/computers/{id}/fs/glob`     | `POST /fs/glob` |
//! | POST   | `/api/computers/{id}/fs/grep`     | `POST /fs/grep` |
//! | POST   | `/api/computers/{id}/fs/write`    | `POST /fs/write` (412 on a failed `expect`) |
//! | POST   | `/api/computers/{id}/fs/pwrite`   | `POST /fs/pwrite` |
//! | POST   | `/api/computers/{id}/fs/setattr`  | `POST /fs/setattr` |
//! | POST   | `/api/computers/{id}/fs/mkdir`    | `POST /fs/mkdir` |
//! | POST   | `/api/computers/{id}/fs/remove`   | `POST /fs/remove` (204) |
//! | POST   | `/api/computers/{id}/fs/rename`   | `POST /fs/rename` (204) |
//! | POST   | `/api/computers/{id}/fs/symlink`  | `POST /fs/symlink` |
//!
//! The request body is the node's (`ember_node::proto`: `PathRequest`, `ReadRequest`,
//! `GlobRequest`, `GrepRequest`, `WriteRequest`, `MkdirRequest`, `RemoveRequest`,
//! `RenameRequest`, `SymlinkRequest`, ...) and the response is the node's, unchanged (`remove`
//! and `rename` answer 204 with no body, as the node does). A node error keeps
//! its status and body; an unreachable node is 502; an unknown computer is 404; the local
//! computer (this server) has no node client, so it answers 400.
//!
//! The write relay carries the content operations `write`, `pwrite` and `setattr` and the
//! namespace operations `mkdir`, `remove`, `rename` and `symlink`. On Unix the node opens the
//! file, or the parent directory of a namespace operation, first and checks the handle against
//! its roots before acting relative to it, and refuses a symlink whose target is outside the
//! roots (INTENT D48, #456). Any other `{op}` is 404. **[provisional]** A failed `expect`
//! precondition is the node's 412 with its `actual_sha256`, passed unchanged.
//!
//! The server adds no path check. The node resolves every path (symlinks and `..` included)
//! against its allowed roots and refuses the rest, so a second check here could only disagree
//! with it. A read-only request body is capped at 1 MiB ([`MAX_BODY`]); a write request body at
//! 24 MiB ([`MAX_WRITE_BODY`], room for 16 MiB of data in base64 and the JSON around it; the
//! node itself accepts 64 MiB). A larger body is 413 and never reaches the node. A read response is bounded by the node
//! (`MAX_READ`, 16 MiB per call), not by the server.
//!
//! Limits added by the server (#467) **[provisional]**:
//! - `glob` and `grep` `limit` is clamped to [`MAX_SEARCH_LIMIT`] (1000, the node's own default)
//!   before the request is sent, so a caller cannot ask the node for an unbounded result list.
//!   The node still collects every glob match before sorting and truncating; bounding that walk
//!   is node work (`ember/node/src/fs.rs`, owned by another change) and is recorded in SPEC.
//! - At most [`FsLimits::concurrent`] relayed operations run at once across all computers (a
//!   global semaphore, default 8, so at most about 8 x 21 MB of read responses are buffered).
//!   A request that cannot get a permit within half the timeout answers 503.
//! - Every relayed operation has a deadline ([`FsLimits::timeout`], default 60 s). Past it the
//!   server answers 504 and drops the node request. When the client disconnects, axum drops the
//!   handler future, which drops the node request (its connection closes); the node may finish
//!   work it already started, the server no longer waits for it.
//! - Error text is not scrubbed: an unreachable node's message names its URL and a node error
//!   body may contain absolute node paths. These routes are owner-only (D37), so the owner sees
//!   their own addresses and paths. Scrubbing is required before any scoped access (#366).
//!
//! The routes sit in the main router, so on the local listener `auth::protect` (Host, Origin and
//! the server token) wraps them, and the guest door does not serve them.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use ember_node::client::ClientError;
use ember_node::proto::{
    GlobRequest, GrepRequest, MkdirRequest, PathRequest, PwriteRequest, ReadRequest, RemoveRequest, RenameRequest,
    SetAttrRequest, SymlinkRequest, WriteRequest,
};
use http_body_util::BodyExt;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use tokio::sync::Semaphore;

use super::{ComputerError, Computers};

/// Largest request body the read-only routes accept.
pub const MAX_BODY: usize = 1024 * 1024;

/// Largest request body the content write routes (`write`, `pwrite`, `setattr`) accept. The
/// namespace routes carry only paths and keep [`MAX_BODY`].
pub const MAX_WRITE_BODY: usize = 24 * 1024 * 1024;

/// Most additional bytes drained from the request stream after [`MAX_BODY`] / [`MAX_WRITE_BODY`]
/// is exceeded, so a client sending an oversized body on a single HTTP/1.1 connection receives the
/// 413 response cleanly instead of racing a TCP `RST` while still writing.
const OVER_LIMIT_DRAIN_CAP: usize = 64 * 1024 * 1024;
const OVER_LIMIT_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// The operations relayed with [`MAX_BODY`].
const READ_OPS: &[&str] = &["stat", "lstat", "read", "list", "readlink", "glob", "grep"];
/// The operations relayed with [`MAX_WRITE_BODY`] (content writes, INTENT D48).
const WRITE_OPS: &[&str] = &["write", "pwrite", "setattr"];
/// The namespace operations, relayed on their own routes with [`MAX_BODY`] (INTENT D48, #456).
const NAMESPACE_OPS: &[&str] = &["mkdir", "remove", "rename", "symlink"];

/// Largest `limit` a relayed `glob` or `grep` may ask the node for.
pub const MAX_SEARCH_LIMIT: usize = 1000;

/// Concurrency and deadline of the relayed operations.
#[derive(Debug, Clone, Copy)]
pub struct FsLimits {
    /// Relayed operations in flight at once, across all computers.
    pub concurrent: usize,
    /// Deadline of one operation, waiting for a permit (at most half of it) included.
    pub timeout: Duration,
}

impl Default for FsLimits {
    fn default() -> Self {
        Self { concurrent: 8, timeout: Duration::from_secs(60) }
    }
}

#[derive(Clone)]
struct Relay {
    computers: Arc<Computers>,
    permits: Arc<Semaphore>,
    timeout: Duration,
}

pub fn router(computers: Arc<Computers>) -> Router {
    router_with(computers, FsLimits::default())
}

pub fn router_with(computers: Arc<Computers>, limits: FsLimits) -> Router {
    let state = Relay {
        computers,
        permits: Arc::new(Semaphore::new(limits.concurrent.max(1))),
        timeout: limits.timeout,
    };
    // The static write paths win over `{op}` in axum's router, so each body limit applies only
    // to its own operations. `DefaultBodyLimit::disable()` lets `read_json_capped` read up to each
    // route's limit and drain the remainder of a bounded oversized upload before answering 413, so
    // hyper does not drop the unread request body mid-write and reset the client's socket.
    let writes = Router::new()
        .route("/api/computers/{id}/fs/write", post(write_op))
        .route("/api/computers/{id}/fs/pwrite", post(pwrite_op))
        .route("/api/computers/{id}/fs/setattr", post(setattr_op))
        .layer(DefaultBodyLimit::disable())
        .with_state(state.clone());
    let namespace = Router::new()
        .route("/api/computers/{id}/fs/mkdir", post(mkdir_op))
        .route("/api/computers/{id}/fs/remove", post(remove_op))
        .route("/api/computers/{id}/fs/rename", post(rename_op))
        .route("/api/computers/{id}/fs/symlink", post(symlink_op))
        .layer(DefaultBodyLimit::disable())
        .with_state(state.clone());
    Router::new()
        .route("/api/computers/{id}/fs/{op}", post(relay))
        .layer(DefaultBodyLimit::disable())
        .with_state(state)
        .merge(writes)
        .merge(namespace)
}

/// The `limit` actually sent to the node.
pub fn clamp_limit(limit: Option<usize>) -> Option<usize> {
    Some(limit.unwrap_or(MAX_SEARCH_LIMIT).min(MAX_SEARCH_LIMIT))
}

fn plain(code: StatusCode, msg: String) -> Response {
    (code, Json(json!({ "error": msg }))).into_response()
}

fn computer_error(e: ComputerError) -> Response {
    let code = match &e {
        ComputerError::NotFound(_) | ComputerError::SessionNotFound(_) => StatusCode::NOT_FOUND,
        ComputerError::BadRequest(_) => StatusCode::BAD_REQUEST,
        ComputerError::Unreachable(..) => StatusCode::BAD_GATEWAY,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    plain(code, format!("{e:#}"))
}

fn client_error(e: ClientError) -> Response {
    match e {
        ClientError::Api { status, body } => {
            (StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY), Json(body)).into_response()
        }
        other => plain(StatusCode::BAD_GATEWAY, format!("node unreachable: {other}")),
    }
}

fn body<T: DeserializeOwned>(v: Value) -> Result<T, Response> {
    serde_json::from_value(v).map_err(|e| plain(StatusCode::BAD_REQUEST, format!("bad request body: {e}")))
}

fn out<T: serde::Serialize>(r: Result<T, ClientError>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => client_error(e),
    }
}

/// A node answer with no body (204), passed as 204.
fn no_content(r: Result<(), ClientError>) -> Response {
    match r {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => client_error(e),
    }
}

async fn drain_discard(mut body: Body, max_drain: usize) {
    let mut drained = 0usize;
    let _ = tokio::time::timeout(OVER_LIMIT_DRAIN_TIMEOUT, async {
        while let Some(Ok(frame)) = body.frame().await {
            if let Ok(data) = frame.into_data() {
                drained = drained.saturating_add(data.len());
                if drained >= max_drain {
                    break;
                }
            }
        }
    })
    .await;
}

/// Read and parse a JSON request body bounded by `max_bytes`. When the body exceeds `max_bytes`,
/// drain and discard the remaining frames (up to [`OVER_LIMIT_DRAIN_CAP`]) without buffering them
/// before answering `413 Payload Too Large` with `Connection: close`, so an HTTP/1.1 client that
/// is still writing the request body reads `413` cleanly instead of seeing `ECONNRESET`.
async fn read_json_capped(req: Request, max_bytes: usize) -> Result<Value, Response> {
    let declared_len = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    let (_, mut body) = req.into_parts();
    if let Some(len) = declared_len {
        if len > max_bytes as u64 {
            if len <= (max_bytes + OVER_LIMIT_DRAIN_CAP) as u64 {
                drain_discard(body, max_bytes + OVER_LIMIT_DRAIN_CAP).await;
            }
            let mut res = plain(StatusCode::PAYLOAD_TOO_LARGE, "request body too large".into());
            res.headers_mut().insert(header::CONNECTION, header::HeaderValue::from_static("close"));
            return Err(res);
        }
    }

    let mut buf = Vec::with_capacity(declared_len.unwrap_or(0).min(max_bytes as u64) as usize);
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|e| plain(StatusCode::BAD_REQUEST, format!("bad request body: {e}")))?;
        if let Ok(data) = frame.into_data() {
            if buf.len().saturating_add(data.len()) > max_bytes {
                drop(buf);
                drain_discard(body, OVER_LIMIT_DRAIN_CAP).await;
                let mut res = plain(StatusCode::PAYLOAD_TOO_LARGE, "request body too large".into());
                res.headers_mut().insert(header::CONNECTION, header::HeaderValue::from_static("close"));
                return Err(res);
            }
            buf.extend_from_slice(&data);
        }
    }
    serde_json::from_slice(&buf).map_err(|e| plain(StatusCode::BAD_REQUEST, format!("bad request body: {e}")))
}

async fn write_op(State(r): State<Relay>, Path(id): Path<String>, req: Request) -> Response {
    match read_json_capped(req, MAX_WRITE_BODY).await {
        Ok(v) => run(r, id, "write", v).await,
        Err(res) => res,
    }
}

async fn pwrite_op(State(r): State<Relay>, Path(id): Path<String>, req: Request) -> Response {
    match read_json_capped(req, MAX_WRITE_BODY).await {
        Ok(v) => run(r, id, "pwrite", v).await,
        Err(res) => res,
    }
}

async fn setattr_op(State(r): State<Relay>, Path(id): Path<String>, req: Request) -> Response {
    match read_json_capped(req, MAX_WRITE_BODY).await {
        Ok(v) => run(r, id, "setattr", v).await,
        Err(res) => res,
    }
}

async fn mkdir_op(State(r): State<Relay>, Path(id): Path<String>, req: Request) -> Response {
    match read_json_capped(req, MAX_BODY).await {
        Ok(v) => run(r, id, "mkdir", v).await,
        Err(res) => res,
    }
}

async fn remove_op(State(r): State<Relay>, Path(id): Path<String>, req: Request) -> Response {
    match read_json_capped(req, MAX_BODY).await {
        Ok(v) => run(r, id, "remove", v).await,
        Err(res) => res,
    }
}

async fn rename_op(State(r): State<Relay>, Path(id): Path<String>, req: Request) -> Response {
    match read_json_capped(req, MAX_BODY).await {
        Ok(v) => run(r, id, "rename", v).await,
        Err(res) => res,
    }
}

async fn symlink_op(State(r): State<Relay>, Path(id): Path<String>, req: Request) -> Response {
    match read_json_capped(req, MAX_BODY).await {
        Ok(v) => run(r, id, "symlink", v).await,
        Err(res) => res,
    }
}

async fn relay(
    State(r): State<Relay>,
    Path((id, op)): Path<(String, String)>,
    req: Request,
) -> Response {
    let v = match read_json_capped(req, MAX_BODY).await {
        Ok(v) => v,
        Err(res) => return res,
    };
    // Only the read-only operations go through `{op}`; check the name before touching the
    // computer. A write or namespace op reaching here (it cannot, its static route wins) is
    // refused too.
    if !READ_OPS.contains(&op.as_str()) {
        return plain(StatusCode::NOT_FOUND, format!("no such file operation: {op}"));
    }
    run(r, id, &op, v).await
}

async fn run(r: Relay, id: String, op: &str, v: Value) -> Response {
    debug_assert!(READ_OPS.contains(&op) || WRITE_OPS.contains(&op) || NAMESPACE_OPS.contains(&op));
    let node = match r.computers.client(&id) {
        Ok(n) => n,
        Err(e) => return computer_error(e),
    };
    let deadline = tokio::time::Instant::now() + r.timeout;
    // Waiting for a permit may use at most half the deadline, so a request that does get one
    // still has time to run instead of timing out at once.
    let wait = tokio::time::Instant::now() + r.timeout / 2;
    let _permit = match tokio::time::timeout_at(wait, r.permits.clone().acquire_owned()).await {
        Ok(Ok(p)) => p,
        _ => return plain(StatusCode::SERVICE_UNAVAILABLE, "too many file operations in flight".into()),
    };
    match tokio::time::timeout_at(deadline, call(&node, op, v)).await {
        Ok(resp) => resp,
        Err(_) => plain(StatusCode::GATEWAY_TIMEOUT, format!("file operation {op} timed out")),
    }
}

/// Sends one operation to the node. Dropping this future drops the node request.
async fn call(node: &ember_node::client::NodeClient, op: &str, v: Value) -> Response {
    match op {
        "stat" => match body::<PathRequest>(v) {
            Ok(r) => out(node.stat(&r.path).await),
            Err(e) => e,
        },
        "lstat" => match body::<PathRequest>(v) {
            Ok(r) => out(node.lstat(&r.path).await),
            Err(e) => e,
        },
        "list" => match body::<PathRequest>(v) {
            Ok(r) => out(node.list(&r.path).await),
            Err(e) => e,
        },
        "readlink" => match body::<PathRequest>(v) {
            Ok(r) => out(node.readlink(&r.path).await),
            Err(e) => e,
        },
        "read" => match body::<ReadRequest>(v) {
            Ok(r) => out(node.read(&r).await),
            Err(e) => e,
        },
        "glob" => match body::<GlobRequest>(v) {
            Ok(mut r) => {
                r.limit = clamp_limit(r.limit);
                out(node.glob(&r).await)
            }
            Err(e) => e,
        },
        "grep" => match body::<GrepRequest>(v) {
            Ok(mut r) => {
                r.limit = clamp_limit(r.limit);
                out(node.grep(&r).await)
            }
            Err(e) => e,
        },
        "write" => match body::<WriteRequest>(v) {
            Ok(r) => out(node.write(&r).await),
            Err(e) => e,
        },
        "pwrite" => match body::<PwriteRequest>(v) {
            Ok(r) => out(node.pwrite(&r.path, r.offset, r.data).await),
            Err(e) => e,
        },
        "setattr" => match body::<SetAttrRequest>(v) {
            Ok(r) => out(node.setattr(&r).await),
            Err(e) => e,
        },
        "mkdir" => match body::<MkdirRequest>(v) {
            Ok(r) => out(node.mkdir(&r).await),
            Err(e) => e,
        },
        "remove" => match body::<RemoveRequest>(v) {
            Ok(r) => no_content(node.remove(&r.path, r.recursive).await),
            Err(e) => e,
        },
        "rename" => match body::<RenameRequest>(v) {
            Ok(r) => no_content(node.rename(&r).await),
            Err(e) => e,
        },
        "symlink" => match body::<SymlinkRequest>(v) {
            Ok(r) => out(node.symlink(&r.path, &r.target).await),
            Err(e) => e,
        },
        _ => unreachable!("checked above"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_w4_fs_relay_limit_is_clamped() {
        assert_eq!(clamp_limit(None), Some(MAX_SEARCH_LIMIT));
        assert_eq!(clamp_limit(Some(5)), Some(5));
        assert_eq!(clamp_limit(Some(usize::MAX)), Some(MAX_SEARCH_LIMIT));
        assert_eq!(clamp_limit(Some(0)), Some(0));
    }
}
