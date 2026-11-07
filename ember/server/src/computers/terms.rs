//! Persistent terminal sessions of a computer, relayed through the server (FR-P3, FR-P6).
//!
//! The editor holds a server session and a computer entry, not a node token. These routes
//! forward the node's `/terms` routes to the registered computer (HTTP or transport) with the
//! node token the server holds, and relay the attach WebSocket frame by frame. The server owns
//! no terminal state: the sessions live on the node, so a server restart or a dropped client
//! connection never ends one. **[provisional]**
//!
//! | Method | Path | Node route |
//! | ------ | ---- | ---------- |
//! | GET    | `/api/computers/{id}/terms?project=&origin=&running=` | `GET /terms` |
//! | POST   | `/api/computers/{id}/terms` | `POST /terms` (201 created, 200 key matched) |
//! | GET    | `/api/computers/{id}/terms/{tid}` | `GET /terms/{tid}` |
//! | GET    | `/api/computers/{id}/terms/{tid}/snapshot` | `GET /terms/{tid}/snapshot` |
//! | POST   | `/api/computers/{id}/terms/{tid}/control` | `POST /terms/{tid}/control` |
//! | POST   | `/api/computers/{id}/terms/{tid}/kill` | `POST /terms/{tid}/kill` |
//! | DELETE | `/api/computers/{id}/terms/{tid}` | `DELETE /terms/{tid}` |
//! | GET (WS) | `/api/computers/{id}/terms/{tid}/attach` | `GET /terms/{tid}/attach` |
//!
//! Bodies and events are the node's (`ember_node::proto`), unchanged. A node error keeps its
//! status and body; an unreachable node is 502; an unknown computer is 404. The local
//! computer (this server) is served by an in-process node on a loopback listener with a random
//! in-memory token, started on first use ([`Computers::local_node_client`], FR-W4, #455), so
//! its terminal routes behave like any node's.
//!
//! Attach: the client sends a `TermHello` as its first text frame, then `TermInput`s. The
//! server opens the node attachment with that hello and answers with the node's `TermEvent`s
//! (first an `attached` event, rebuilt from the node's attach result). If the node refuses or
//! is unreachable, the client gets one `error` event and the socket closes. When either side
//! ends, the other is detached; the session keeps running.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use ember_node::client::{ClientError, NodeClient};
use ember_node::proto::*;
use futures::{SinkExt, StreamExt};
use serde_json::json;

use super::{ComputerError, Computers};

pub fn router(computers: Arc<Computers>) -> Router {
    Router::new()
        .route("/api/computers/{id}/terms", get(list).post(create))
        .route("/api/computers/{id}/terms/{tid}", get(one).delete(remove))
        .route("/api/computers/{id}/terms/{tid}/snapshot", get(snapshot))
        .route("/api/computers/{id}/terms/{tid}/control", post(control))
        .route("/api/computers/{id}/terms/{tid}/kill", post(kill))
        .route("/api/computers/{id}/terms/{tid}/attach", get(attach))
        .with_state(computers)
}

type S = State<Arc<Computers>>;

/// An error answered as `{ "error": .. }` (or the node's own body).
struct Failure(Response);

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        self.0
    }
}

fn plain(code: StatusCode, msg: String) -> Failure {
    Failure((code, Json(json!({ "error": msg }))).into_response())
}

impl From<ComputerError> for Failure {
    fn from(e: ComputerError) -> Self {
        let code = match &e {
            ComputerError::NotFound(_) | ComputerError::SessionNotFound(_) => StatusCode::NOT_FOUND,
            ComputerError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ComputerError::Unreachable(..) => StatusCode::BAD_GATEWAY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        plain(code, format!("{e:#}"))
    }
}

impl From<ClientError> for Failure {
    fn from(e: ClientError) -> Self {
        match e {
            ClientError::Api { status, body } => {
                Failure((StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY), Json(body)).into_response())
            }
            other => plain(StatusCode::BAD_GATEWAY, format!("node unreachable: {other}")),
        }
    }
}

type R<T> = Result<T, Failure>;

fn node(c: &Computers, id: &str) -> R<NodeClient> {
    if id == super::LOCAL {
        return Ok(c.local_node_client()?);
    }
    Ok(c.client(id)?)
}

async fn list(State(c): S, Path(id): Path<String>, Query(q): Query<TermListQuery>) -> R<Json<Vec<TermInfo>>> {
    Ok(Json(node(&c, &id)?.terms(&q).await?))
}

async fn create(State(c): S, Path(id): Path<String>, Json(r): Json<TermCreateRequest>) -> R<Response> {
    let res = node(&c, &id)?.term_create(&r).await?;
    let status = if res.created { StatusCode::CREATED } else { StatusCode::OK };
    Ok((status, Json(res)).into_response())
}

async fn one(State(c): S, Path((id, tid)): Path<(String, String)>) -> R<Json<TermInfo>> {
    Ok(Json(node(&c, &id)?.term(&tid).await?))
}

async fn snapshot(State(c): S, Path((id, tid)): Path<(String, String)>) -> R<Json<TermSnapshot>> {
    Ok(Json(node(&c, &id)?.term_snapshot(&tid).await?))
}

async fn control(
    State(c): S,
    Path((id, tid)): Path<(String, String)>,
    Json(r): Json<TermControlRequest>,
) -> R<StatusCode> {
    node(&c, &id)?.term_control(&tid, r.client, r.take).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn kill(
    State(c): S,
    Path((id, tid)): Path<(String, String)>,
    body: Option<Json<KillRequest>>,
) -> R<StatusCode> {
    node(&c, &id)?.term_kill(&tid, body.and_then(|b| b.0.signal)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove(State(c): S, Path((id, tid)): Path<(String, String)>) -> R<StatusCode> {
    node(&c, &id)?.term_remove(&tid).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn attach(
    State(c): S,
    Path((id, tid)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
) -> R<impl IntoResponse> {
    // Refuse a cross-site browser page before anything reaches the node (FR-P3).
    crate::api::origin::check_origin(&headers).map_err(|c| plain(c, "origin not allowed".into()))?;
    // Resolve the computer before upgrading, so an unknown one is a plain 404.
    let node = node(&c, &id)?;
    Ok(ws.on_upgrade(move |socket| relay(socket, node, tid)))
}

fn event(ev: &TermEvent) -> Message {
    Message::Text(serde_json::to_string(ev).expect("serialisable").into())
}

async fn relay(socket: WebSocket, node: NodeClient, tid: String) {
    let (mut out, mut inp) = socket.split();
    // The client's first frame is its hello.
    let hello = loop {
        match inp.next().await {
            Some(Ok(Message::Text(t))) => match serde_json::from_str::<TermHello>(t.as_str()) {
                Ok(h) => break h,
                Err(e) => {
                    let _ = out.send(event(&TermEvent::Error { message: format!("bad hello: {e}") })).await;
                    return;
                }
            },
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            _ => return,
        }
    };
    let att = match node.term_attach(&tid, &hello).await {
        Ok(a) => a,
        Err(e) => {
            let message = match &e {
                ClientError::Api { body, .. } => body.error.clone(),
                other => format!("node unreachable: {other}"),
            };
            let _ = out.send(event(&TermEvent::Error { message })).await;
            let _ = out.close().await;
            return;
        }
    };
    if out.send(event(&TermEvent::Attached { client: att.client, term: att.term.clone() })).await.is_err() {
        return;
    }
    let (mut node_tx, mut node_rx) = att.into_split();
    let up = async {
        while let Some(Ok(msg)) = inp.next().await {
            match msg {
                Message::Text(t) => match serde_json::from_str::<TermInput>(t.as_str()) {
                    Ok(TermInput::Detach) => break,
                    Ok(i) => {
                        if node_tx.send(i).await.is_err() {
                            return;
                        }
                    }
                    // Unparseable input is dropped, as the node would.
                    Err(_) => {}
                },
                Message::Close(_) => break,
                _ => {}
            }
        }
        let _ = node_tx.detach().await;
    };
    let down = async {
        loop {
            match node_rx.recv().await {
                Ok(Some(ev)) => {
                    let last = matches!(ev, TermEvent::Exit { .. } | TermEvent::Error { .. });
                    if out.send(event(&ev)).await.is_err() || last {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    let _ = out.send(event(&TermEvent::Error { message: format!("node link lost: {e}") })).await;
                    break;
                }
            }
        }
        let _ = out.close().await;
    };
    tokio::select! {
        _ = up => {}
        _ = down => {}
    }
}
