//! HTTP API for computers and a session's current computer (FR-X3). Merged into the main
//! router by `main.rs`; all routes are under `/api`.
//!
//! | Method | Path | Body → Response |
//! | ------ | ---- | --------------- |
//! | GET    | `/api/computers?probe=<bool>` | → `[ComputerStatus]` (`local` first; probe default true) |
//! | POST   | `/api/computers` | `{name, url, token}` or `{name, peer, token}` → 201 `Computer` (token never returned) |
//! | GET    | `/api/computers/{id}` | → `ComputerStatus` with `env` |
//! | DELETE | `/api/computers/{id}` | → 204 (409 while a session is on it or a browser egresses through it); its project assignments are removed |
//! | GET    | `/api/sessions/{id}/computer` | → `CurrentComputer` |
//! | PUT    | `/api/sessions/{id}/computer` | `{computer_id}` → `SwitchOutcome` (409 mid-turn, 502 unreachable) |
//!
//! `peer` registers a node reached over the transport (`FR-N1`): either its peer id (64 hex
//! chars) or a full `PeerAddr` JSON object `{peer, relays, direct}` as the node prints at start.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use ember_transport::{PeerAddr, PeerId};
use serde::Deserialize;
use serde_json::json;

use super::{ComputerError, Computers};
use crate::session::{Push, Sessions};

#[derive(Clone)]
struct AppState {
    computers: Arc<Computers>,
    sessions: Arc<Sessions>,
}

pub fn router(computers: Arc<Computers>, sessions: Arc<Sessions>) -> Router {
    Router::new()
        .route("/api/computers", get(list).post(register))
        .route("/api/computers/{id}", get(status).delete(remove))
        .route("/api/sessions/{id}/computer", get(current).put(switch))
        .with_state(AppState { computers, sessions })
}

struct ApiError(ComputerError);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let code = match &self.0 {
            ComputerError::NotFound(_) | ComputerError::SessionNotFound(_) => StatusCode::NOT_FOUND,
            ComputerError::Busy(_) | ComputerError::InUse(..) | ComputerError::EgressInUse(..) => {
                StatusCode::CONFLICT
            }
            ComputerError::Unreachable(..) => StatusCode::BAD_GATEWAY,
            ComputerError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ComputerError::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (code, Json(json!({ "error": format!("{:#}", self.0) }))).into_response()
    }
}

impl From<ComputerError> for ApiError {
    fn from(e: ComputerError) -> Self {
        ApiError(e)
    }
}

type ApiResult<T> = Result<T, ApiError>;

#[derive(Deserialize)]
struct ListQuery {
    probe: Option<bool>,
}

async fn list(State(s): State<AppState>, Query(q): Query<ListQuery>) -> ApiResult<impl IntoResponse> {
    Ok(Json(s.computers.list(q.probe.unwrap_or(true)).await?))
}

#[derive(Deserialize)]
struct RegisterBody {
    name: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    peer: Option<PeerSpec>,
    token: String,
}

/// A node's transport address: a bare peer id or a full address.
#[derive(Deserialize)]
#[serde(untagged)]
enum PeerSpec {
    Id(PeerId),
    Addr(PeerAddr),
}

async fn register(State(s): State<AppState>, Json(b): Json<RegisterBody>) -> ApiResult<impl IntoResponse> {
    let c = match (b.url.filter(|u| !u.is_empty()), b.peer) {
        (Some(url), None) => s.computers.register(&b.name, &url, &b.token)?,
        (None, Some(peer)) => {
            let addr = match peer {
                PeerSpec::Id(id) => PeerAddr::new(id),
                PeerSpec::Addr(a) => a,
            };
            s.computers.register_peer(&b.name, &addr, &b.token)?
        }
        _ => return Err(ComputerError::BadRequest("give exactly one of url or peer".into()).into()),
    };
    Ok((StatusCode::CREATED, Json(c)))
}

async fn status(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    Ok(Json(s.computers.status(&id).await?))
}

async fn remove(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    // Removal unassigns the computer from its projects (FR-L4): tell clients.
    let assigned = s.sessions.store().projects_of_computer(&id).map_err(ComputerError::Other)?;
    s.computers.remove(&id)?;
    for name in assigned {
        if let Ok(Some(project)) = s.sessions.store().project(&name) {
            s.sessions.publish(Push::ProjectUpdated { project });
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn current(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    Ok(Json(s.computers.current(&s.sessions, &id)?))
}

#[derive(Deserialize)]
struct SwitchBody {
    computer_id: String,
}

async fn switch(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<SwitchBody>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(s.computers.switch(&s.sessions, &id, &b.computer_id).await?))
}
