//! File transfer to a computer, relayed to its node's receiver (SPEC `FR-F14`, `FR-F6`, #773).
//!
//! | Method | Path | Node route |
//! | ------ | ---- | ---------- |
//! | GET    | `/api/computers/{id}/transfers` | `GET /transfers` |
//! | POST   | `/api/computers/{id}/transfers/offers` | `POST /transfers/offers` (the server sets `owner`) |
//! | POST   | `/api/computers/{id}/transfers/{tid}/chunks` | `POST /transfers/{tid}/chunks` |
//! | GET    | `/api/computers/{id}/transfers/{tid}/resume-offset/{file}` | `GET /transfers/{tid}/resume-offset/{file}` |
//! | POST   | `/api/computers/{id}/transfers/{tid}/{complete,pause,resume,cancel}` | the same |
//! | GET, DELETE | `/api/computers/{id}/transfers/history` | the same |
//!
//! The routes sit beside the other `/api/computers/{id}/...` relays (D42: one authenticated door,
//! the client never holds a node credential) and not on `/api/transfers` with a target field,
//! because each receiver keeps its own offers, transfers and history: the client reads one
//! receiver's snapshot under one prefix, and the server's own receiver stays as it is.
//!
//! Each call is forwarded with the node's status and body unchanged. A chunk is held in memory
//! only while it is forwarded; nothing is written on this server's disk (`FR-F6`). Only a
//! computer reached over the transport is a target: one registered by plain HTTP answers 403
//! `InsecureTarget` before anything is sent to it, because the bytes would cross the network in
//! clear. The server's own computer is 400 (its receiver is `/api/transfers`), an unknown computer
//! 404, an unreachable node 502.
//!
//! Every caller of this router is the owner (the owner API, `INTENT.md` D37), and every
//! registered computer is one the owner enrolled, so a relayed offer is marked `owner: true` and
//! the node accepts it on its own (`FR-F15`). Accepting or declining an offer for a computer is
//! not relayed (403): answering for a node from another device is later work.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use ember_node::client::NodeClient;
use serde_json::{json, Value};

use super::{Computers, LOCAL};

/// Largest request body: a 256 KiB chunk as base64 is about 350 KiB.
pub const MAX_BODY: usize = 4 * 1024 * 1024;

/// Per-call deadline toward the node.
const DEADLINE: Duration = Duration::from_secs(60);

pub fn router(computers: Arc<Computers>) -> Router {
    Router::new()
        .route("/api/computers/{id}/transfers", get(snapshot))
        .route("/api/computers/{id}/transfers/offers", post(offer))
        .route("/api/computers/{id}/transfers/offers/{oid}/accept", post(answer_refused))
        .route("/api/computers/{id}/transfers/offers/{oid}/decline", post(answer_refused))
        .route("/api/computers/{id}/transfers/history", get(history).delete(clear_history))
        .route("/api/computers/{id}/transfers/{tid}/chunks", post(chunk))
        .route("/api/computers/{id}/transfers/{tid}/resume-offset/{file}", get(resume_offset))
        .route("/api/computers/{id}/transfers/{tid}/{op}", post(control))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(computers)
}

fn plain(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, Json(json!({ "error": msg.into() }))).into_response()
}

/// The node client for a computer that may receive files, or the refusal.
fn target(c: &Computers, id: &str) -> Result<NodeClient, Response> {
    if id == LOCAL {
        return Err(plain(StatusCode::BAD_REQUEST, "the server's own computer receives on /api/transfers"));
    }
    let computer = match c.registry().get(id) {
        Ok(Some(computer)) => computer,
        Ok(None) => return Err(plain(StatusCode::NOT_FOUND, format!("unknown computer: {id}"))),
        Err(e) => return Err(plain(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))),
    };
    if computer.peer.is_none() {
        return Err(plain(
            StatusCode::FORBIDDEN,
            format!(
                "InsecureTarget: computer {} is reached over plain HTTP; files are sent only to computers on the \
                 encrypted transport",
                computer.name
            ),
        ));
    }
    match c.client(id) {
        Ok(n) => Ok(n.with_deadline(DEADLINE)),
        Err(e) => Err(plain(StatusCode::BAD_GATEWAY, format!("{e:#}"))),
    }
}

/// A transfer id as the node makes them (`tr-` and hex); anything else never reaches a node path.
fn check_tid(tid: &str) -> Result<(), Response> {
    if !tid.is_empty() && tid.len() <= 64 && tid.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_') {
        Ok(())
    } else {
        Err(plain(StatusCode::BAD_REQUEST, "bad transfer id"))
    }
}

async fn forward(c: &Computers, id: &str, method: Method, path: &str, body: Option<&Value>) -> Response {
    let node = match target(c, id) {
        Ok(n) => n,
        Err(r) => return r,
    };
    match node.request_raw(method, path, body).await {
        Ok((status, bytes)) => {
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
            (code, [(axum::http::header::CONTENT_TYPE, "application/json")], bytes).into_response()
        }
        Err(e) => plain(StatusCode::BAD_GATEWAY, format!("node unreachable: {e}")),
    }
}

async fn snapshot(State(c): State<Arc<Computers>>, Path(id): Path<String>) -> Response {
    forward(&c, &id, Method::GET, "/transfers", None).await
}

async fn offer(State(c): State<Arc<Computers>>, Path(id): Path<String>, Json(v): Json<Value>) -> Response {
    // The body is rebuilt so only the server says who started the offer (`FR-F15`).
    let (Some(from), Some(files)) = (v.get("from").and_then(Value::as_str), v.get("files")) else {
        return plain(StatusCode::BAD_REQUEST, "an offer needs from and files");
    };
    let body = json!({
        "from": from,
        "peer_id": v.get("peer_id").cloned().unwrap_or(Value::Null),
        "files": files,
        "owner": true,
    });
    forward(&c, &id, Method::POST, "/transfers/offers", Some(&body)).await
}

async fn answer_refused(Path((_id, _oid)): Path<(String, String)>) -> Response {
    plain(
        StatusCode::FORBIDDEN,
        "answering an offer for a computer from another device is not built yet (#773)",
    )
}

async fn chunk(State(c): State<Arc<Computers>>, Path((id, tid)): Path<(String, String)>, Json(v): Json<Value>) -> Response {
    if let Err(r) = check_tid(&tid) {
        return r;
    }
    forward(&c, &id, Method::POST, &format!("/transfers/{tid}/chunks"), Some(&v)).await
}

async fn resume_offset(State(c): State<Arc<Computers>>, Path((id, tid, file)): Path<(String, String, usize)>) -> Response {
    if let Err(r) = check_tid(&tid) {
        return r;
    }
    forward(&c, &id, Method::GET, &format!("/transfers/{tid}/resume-offset/{file}"), None).await
}

async fn control(State(c): State<Arc<Computers>>, Path((id, tid, op)): Path<(String, String, String)>) -> Response {
    if !matches!(op.as_str(), "complete" | "pause" | "resume" | "cancel") {
        return plain(StatusCode::NOT_FOUND, format!("no such transfer operation: {op}"));
    }
    if let Err(r) = check_tid(&tid) {
        return r;
    }
    forward(&c, &id, Method::POST, &format!("/transfers/{tid}/{op}"), Some(&json!({}))).await
}

async fn history(State(c): State<Arc<Computers>>, Path(id): Path<String>) -> Response {
    forward(&c, &id, Method::GET, "/transfers/history", None).await
}

async fn clear_history(State(c): State<Arc<Computers>>, Path(id): Path<String>) -> Response {
    forward(&c, &id, Method::DELETE, "/transfers/history", None).await
}
