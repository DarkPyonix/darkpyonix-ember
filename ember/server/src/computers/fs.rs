//! Read-only file operations of a computer, relayed from its node (FR-W4, FR-L7, #456).
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
//!
//! The request body is the node's (`ember_node::proto`: `PathRequest`, `ReadRequest`,
//! `GlobRequest`, `GrepRequest`) and the response is the node's, unchanged. A node error keeps
//! its status and body; an unreachable node is 502; an unknown computer is 404; the local
//! computer (this server) has no node client, so it answers 400.
//!
//! Only the read-only operations are relayed. The write operations (`write`, `remove`, `rename`,
//! `mkdir`, `symlink`, `setattr`, `pwrite`) are not routed: any other `{op}` is 404. They need
//! their own review. **[provisional]**
//!
//! The server adds no path check. The node resolves every path (symlinks and `..` included)
//! against its allowed roots and refuses the rest, so a second check here could only disagree
//! with it. The request body is capped at 1 MiB. A read response is bounded by the node
//! (`MAX_READ`, 16 MiB per call), not by the server.
//!
//! Limits added by the server (#467) **[provisional]**:
//! - `glob` and `grep` `limit` is clamped to [`MAX_SEARCH_LIMIT`] (1000, the node's own default)
//!   before the request is sent, so a caller cannot ask the node for an unbounded result list.
//!   The node still collects every glob match before sorting and truncating; bounding that walk
//!   is node work (`ember/node/src/fs.rs`, owned by another change) and is recorded in SPEC.
//! - At most [`FsLimits::concurrent`] relayed operations run at once across all computers (a
//!   global semaphore, default 8, so at most about 8 x 21 MB of read responses are buffered).
//!   A request that cannot get a permit within the timeout answers 503.
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

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use ember_node::client::ClientError;
use ember_node::proto::{GlobRequest, GrepRequest, PathRequest, ReadRequest};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use tokio::sync::Semaphore;

use super::{ComputerError, Computers};

/// Largest request body these routes accept.
pub const MAX_BODY: usize = 1024 * 1024;

/// Largest `limit` a relayed `glob` or `grep` may ask the node for.
pub const MAX_SEARCH_LIMIT: usize = 1000;

/// Concurrency and deadline of the relayed operations.
#[derive(Debug, Clone, Copy)]
pub struct FsLimits {
    /// Relayed operations in flight at once, across all computers.
    pub concurrent: usize,
    /// Deadline of one operation, waiting for a permit included.
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
    Router::new()
        .route("/api/computers/{id}/fs/{op}", post(relay))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(state)
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

async fn relay(
    State(r): State<Relay>,
    Path((id, op)): Path<(String, String)>,
    Json(v): Json<Value>,
) -> Response {
    // Only the read-only operations exist here; check the name before touching the computer.
    if !matches!(op.as_str(), "stat" | "lstat" | "read" | "list" | "readlink" | "glob" | "grep") {
        return plain(StatusCode::NOT_FOUND, format!("no such file operation: {op}"));
    }
    let node = match r.computers.client(&id) {
        Ok(n) => n,
        Err(e) => return computer_error(e),
    };
    let deadline = tokio::time::Instant::now() + r.timeout;
    let _permit = match tokio::time::timeout_at(deadline, r.permits.clone().acquire_owned()).await {
        Ok(Ok(p)) => p,
        _ => return plain(StatusCode::SERVICE_UNAVAILABLE, "too many file operations in flight".into()),
    };
    match tokio::time::timeout_at(deadline, call(&node, &op, v)).await {
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
