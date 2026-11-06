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
//! The routes sit in the main router, so on the local listener `auth::protect` (Host, Origin and
//! the server token) wraps them, and the guest door does not serve them.

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use ember_node::client::ClientError;
use ember_node::proto::{GlobRequest, GrepRequest, PathRequest, ReadRequest};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use super::{ComputerError, Computers};

/// Largest request body these routes accept.
pub const MAX_BODY: usize = 1024 * 1024;

pub fn router(computers: Arc<Computers>) -> Router {
    Router::new()
        .route("/api/computers/{id}/fs/{op}", post(relay))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(computers)
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
    State(c): State<Arc<Computers>>,
    Path((id, op)): Path<(String, String)>,
    Json(v): Json<Value>,
) -> Response {
    // Only the read-only operations exist here; check the name before touching the computer.
    if !matches!(op.as_str(), "stat" | "lstat" | "read" | "list" | "readlink" | "glob" | "grep") {
        return plain(StatusCode::NOT_FOUND, format!("no such file operation: {op}"));
    }
    let node = match c.client(&id) {
        Ok(n) => n,
        Err(e) => return computer_error(e),
    };
    match op.as_str() {
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
            Ok(r) => out(node.glob(&r).await),
            Err(e) => e,
        },
        "grep" => match body::<GrepRequest>(v) {
            Ok(r) => out(node.grep(&r).await),
            Err(e) => e,
        },
        _ => unreachable!("checked above"),
    }
}
