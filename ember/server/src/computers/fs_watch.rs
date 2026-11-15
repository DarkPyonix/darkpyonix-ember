//! File watching of a computer, relayed from its node (SPEC `FR-W4d`, #460).
//!
//! `GET /api/computers/{id}/fs/watch` is a WebSocket carried frame by frame to the node's
//! `GET /fs/watch` (protocol in `ember_node::fs_watch`); the local computer goes to the
//! in-process node (`Computers::local_node_client`, #455). The server keeps no watch state: the
//! node checks every path against its roots and deny list, coalesces and bounds the events.
//!
//! Gates, the same as the other fs relay routes: the route is in the main router, so the local
//! listener's `auth::protect` (Host, Origin, server token) wraps it and the guest door does not
//! serve it; on the transport only a `full` device passes (`computers/*/fs` is in no narrow
//! scope, `devices::scope`). The `Origin` check of the terminal relay runs before the upgrade.
//!
//! The node socket is opened before the upgrade, so an unknown computer is 404 and an
//! unreachable node 502 (or the node's own status). Only text, ping and pong frames go up; a
//! binary frame from the client closes both sockets with 1003. A client message larger than
//! [`MAX_CLIENT_MESSAGE`] is refused by the socket. At most [`MAX_SOCKETS`] watch sockets are
//! relayed at once (503 beyond). Both sockets end together: when the client goes (a revoked
//! device's stream is closed by the transport within one heartbeat, `FR-N3`), the node socket is
//! closed and the node drops every watch of it.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use ember_node::client::{ClientError, FsWatchSocket};
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::{ComputerError, Computers};

/// Watch sockets relayed at once by this server.
pub const MAX_SOCKETS: usize = 64;
/// Largest message a client may send on the watch socket.
pub const MAX_CLIENT_MESSAGE: usize = 64 * 1024;
/// How long opening the node socket may take.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the closing handshake may take once one side ended.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
struct Relay {
    computers: Arc<Computers>,
    permits: Arc<Semaphore>,
}

pub fn router(computers: Arc<Computers>) -> Router {
    router_with(computers, MAX_SOCKETS)
}

pub fn router_with(computers: Arc<Computers>, max_sockets: usize) -> Router {
    Router::new()
        .route("/api/computers/{id}/fs/watch", get(watch))
        .with_state(Relay { computers, permits: Arc::new(Semaphore::new(max_sockets.max(1))) })
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

async fn watch(State(r): State<Relay>, Path(id): Path<String>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    if let Err(code) = crate::api::origin::check_origin(&headers) {
        return plain(code, "origin not allowed".into());
    }
    let node = if id == super::LOCAL { r.computers.local_node_client() } else { r.computers.client(&id) };
    let node = match node {
        Ok(n) => n,
        Err(e) => return computer_error(e),
    };
    let Ok(permit) = r.permits.clone().try_acquire_owned() else {
        return plain(StatusCode::SERVICE_UNAVAILABLE, "too many file watch sockets".into());
    };
    let upstream = match tokio::time::timeout(OPEN_TIMEOUT, node.fs_watch_socket()).await {
        Ok(Ok(s)) => s,
        Ok(Err(ClientError::Api { status, body })) => {
            return (StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY), Json(body)).into_response()
        }
        Ok(Err(e)) => return plain(StatusCode::BAD_GATEWAY, format!("node unreachable: {e}")),
        Err(_) => return plain(StatusCode::GATEWAY_TIMEOUT, "node did not open the watch socket in time".into()),
    };
    ws.max_message_size(MAX_CLIENT_MESSAGE).on_upgrade(move |socket| bridge(socket, upstream, permit))
}

enum End {
    ClientBinary,
    NodeClosed(Option<(u16, String)>),
    Other,
}

/// Carry frames both ways until one side ends, then close the other.
async fn bridge(socket: WebSocket, upstream: FsWatchSocket, _permit: OwnedSemaphorePermit) {
    use tokio_tungstenite::tungstenite::Message as NodeMessage;

    let (mut client_tx, mut client_rx) = socket.split();
    let (mut node_tx, mut node_rx) = upstream.split();
    let end = {
        let up = async {
            loop {
                let out = match client_rx.next().await {
                    Some(Ok(Message::Text(t))) => NodeMessage::Text(t.to_string().into()),
                    Some(Ok(Message::Ping(p))) => NodeMessage::Ping(p),
                    Some(Ok(Message::Pong(p))) => NodeMessage::Pong(p),
                    Some(Ok(Message::Binary(_))) => return End::ClientBinary,
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return End::Other,
                };
                if node_tx.send(out).await.is_err() {
                    return End::Other;
                }
            }
        };
        let down = async {
            loop {
                let out = match node_rx.next().await {
                    Some(Ok(NodeMessage::Text(t))) => Message::Text(t.to_string().into()),
                    Some(Ok(NodeMessage::Ping(p))) => Message::Ping(p.into()),
                    Some(Ok(NodeMessage::Close(frame))) => {
                        return End::NodeClosed(frame.map(|f| (u16::from(f.code), f.reason.as_str().to_string())))
                    }
                    Some(Ok(_)) => continue,
                    Some(Err(_)) | None => return End::NodeClosed(None),
                };
                if client_tx.send(out).await.is_err() {
                    return End::Other;
                }
            }
        };
        tokio::select! {
            e = up => e,
            e = down => e,
        }
    };
    let frame = match end {
        End::ClientBinary => Some(CloseFrame { code: 1003, reason: "the file watch socket takes text frames".into() }),
        End::NodeClosed(Some((code, reason))) => Some(CloseFrame { code, reason: reason.as_str().into() }),
        End::NodeClosed(None) | End::Other => None,
    };
    let close = async {
        let _ = tokio::join!(node_tx.close(), client_tx.send(Message::Close(frame)));
    };
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, close).await;
}
