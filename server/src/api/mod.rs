//! HTTP API and push channel for clients (SPEC §L via FR-S*, PR-1).
//!
//! All routes live under `/api/v1`. The push channel is a WebSocket at `/api/v1/push`; on connect
//! a client sends nothing and receives every [`Push`] from then on. To catch up after a gap, it
//! reads `/sessions/{id}/events?after=<seq>` and then relies on the stream.
//!
//! "Open IDE" launch targets (`FR-L7`) are in [`ide`], with their own router merged in
//! `main.rs`, since they need the IDE configuration as well as the sessions.

pub mod ide;

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;

use crate::agents::AgentKind;
use crate::events::ApprovalDecision;
use crate::session::{NewSession, Push, SessionError, Sessions, PUSH_VERSION};

pub fn router(sessions: Arc<Sessions>) -> Router {
    Router::new()
        .route("/api/v1/health", get(|| async { Json(json!({ "ok": true, "push_version": PUSH_VERSION })) }))
        .route("/api/v1/agents", get(agents))
        .route("/api/v1/sessions", get(list_sessions).post(create_session))
        .route("/api/v1/sessions/{id}", get(get_session))
        .route("/api/v1/sessions/{id}/events", get(events))
        .route("/api/v1/sessions/{id}/messages", post(send_message))
        .route("/api/v1/sessions/{id}/approvals/{approval_id}", post(answer))
        .route("/api/v1/sessions/{id}/interrupt", post(interrupt))
        .route("/api/v1/sessions/{id}/lease", post(lease))
        .route("/api/v1/push", get(push))
        .with_state(sessions)
}

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<SessionError> for ApiError {
    fn from(e: SessionError) -> Self {
        let code = match &e {
            SessionError::NotFound(_) => StatusCode::NOT_FOUND,
            SessionError::AgentUnavailable(_) => StatusCode::BAD_REQUEST,
            SessionError::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        ApiError(code, format!("{e:#}"))
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}

type ApiResult<T> = Result<T, ApiError>;

async fn agents(State(s): State<Arc<Sessions>>) -> impl IntoResponse {
    Json(s.detect_agents().await)
}

#[derive(Deserialize)]
struct ListQuery {
    project: Option<String>,
}

async fn list_sessions(
    State(s): State<Arc<Sessions>>,
    Query(q): Query<ListQuery>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(s.store().sessions(q.project.as_deref())?))
}

#[derive(Deserialize)]
struct CreateBody {
    project: String,
    agent: String,
    cwd: PathBuf,
    model: Option<String>,
    title: Option<String>,
}

async fn create_session(
    State(s): State<Arc<Sessions>>,
    Json(b): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    let agent = AgentKind::parse(&b.agent)
        .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, format!("unknown agent {}", b.agent)))?;
    let rec = s.create(NewSession {
        project: b.project,
        agent,
        cwd: b.cwd,
        model: b.model,
        title: b.title.unwrap_or_else(|| "New conversation".into()),
    })?;
    Ok((StatusCode::CREATED, Json(rec)))
}

async fn get_session(
    State(s): State<Arc<Sessions>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let rec = s.store().session(&id)?.ok_or_else(|| SessionError::NotFound(id.clone()))?;
    Ok(Json(json!({ "session": rec, "live": s.is_live(&id).await })))
}

#[derive(Deserialize)]
struct EventsQuery {
    #[serde(default)]
    after: i64,
}

async fn events(
    State(s): State<Arc<Sessions>>,
    Path(id): Path<String>,
    Query(q): Query<EventsQuery>,
) -> ApiResult<impl IntoResponse> {
    s.store().session(&id)?.ok_or_else(|| SessionError::NotFound(id.clone()))?;
    Ok(Json(s.store().events_after(&id, q.after)?))
}

#[derive(Deserialize)]
struct MessageBody {
    text: String,
}

async fn send_message(
    State(s): State<Arc<Sessions>>,
    Path(id): Path<String>,
    Json(b): Json<MessageBody>,
) -> ApiResult<impl IntoResponse> {
    s.send(&id, &b.text).await?;
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
struct AnswerBody {
    decision: ApprovalDecision,
}

async fn answer(
    State(s): State<Arc<Sessions>>,
    Path((id, approval_id)): Path<(String, String)>,
    Json(b): Json<AnswerBody>,
) -> ApiResult<impl IntoResponse> {
    s.answer(&id, &approval_id, b.decision).await?;
    Ok(StatusCode::ACCEPTED)
}

async fn interrupt(
    State(s): State<Arc<Sessions>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    s.interrupt(&id).await?;
    Ok(StatusCode::ACCEPTED)
}

/// How long one lease keeps a viewed session's agent alive; clients renew well before it ends.
pub const LEASE_TTL: std::time::Duration = std::time::Duration::from_secs(90);

async fn lease(
    State(s): State<Arc<Sessions>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    s.lease(&id, LEASE_TTL)?;
    Ok(Json(json!({ "ttl_secs": LEASE_TTL.as_secs() })))
}

async fn push(State(s): State<Arc<Sessions>>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| push_loop(socket, s))
}

async fn push_loop(mut socket: WebSocket, s: Arc<Sessions>) {
    let mut rx = s.subscribe();
    loop {
        let msg = match rx.recv().await {
            Ok(p) => p,
            // A slow client missed messages: tell it to resync from the events endpoint.
            Err(RecvError::Lagged(n)) => {
                let lag = json!({ "type": "lagged", "v": PUSH_VERSION, "missed": n }).to_string();
                if socket.send(Message::Text(lag.into())).await.is_err() {
                    return;
                }
                continue;
            }
            Err(RecvError::Closed) => return,
        };
        let text = match serde_json::to_string::<Push>(&msg) {
            Ok(t) => t,
            Err(e) => {
                tracing::error!("push encode failed: {e}");
                continue;
            }
        };
        if socket.send(Message::Text(text.into())).await.is_err() {
            return;
        }
    }
}
