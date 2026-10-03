//! HTTP API and push channel for clients (SPEC §L via FR-S*, PR-1).
//!
//! All routes live under `/api/v1`. The push channel is a WebSocket at `/api/v1/push`; on connect
//! a client sends nothing and receives every [`Push`] from then on. To catch up after a gap, it
//! reads `/sessions/{id}/events?after=<seq>` and then relies on the stream.
//!
//! "Open IDE" launch targets (`FR-L7`) are in [`ide`], with their own router merged in
//! `main.rs`, since they need the IDE configuration as well as the sessions.
//!
//! Projects, session metadata, search and export (FR-L4, FR-L9, FR-S4, FR-S5):
//!
//! | Method | Path | Body → Response |
//! | ------ | ---- | --------------- |
//! | GET    | `/api/v1/projects` | → `[Project]` (`{name, created_at, computers}`) |
//! | POST   | `/api/v1/projects` | `{name}` → 201 `Project` (200 when it existed) |
//! | GET    | `/api/v1/projects/{name}` | → `Project` |
//! | PUT    | `/api/v1/projects/{name}/computers/{computer_id}` | → `Project` (assign; idempotent) |
//! | DELETE | `/api/v1/projects/{name}/computers/{computer_id}` | → `Project` (unassign; idempotent) |
//! | PATCH  | `/api/v1/sessions/{id}` | `{title?, pinned?, archived?}` → `SessionRecord` |
//! | GET    | `/api/v1/search?q=&limit=` | → `[SearchHit]` (`{session_id, seq, kind, snippet, title, project, archived}`) |
//! | GET    | `/api/v1/sessions/{id}/export` | → transcript file (JSON, `Content-Disposition: attachment`) |
//! | POST   | `/api/v1/sessions/{id}/fork` | → 501 `{error, reason}`: no agent supports forking yet |
//!
//! Project names are path segments: clients percent-encode them. Assignment and metadata
//! changes are pushed (`project_updated`, `session_updated`).

pub mod ide;

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::http::header;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;

use crate::agents::AgentKind;
use crate::events::ApprovalDecision;
use crate::projects::ProjectError;
use crate::session::{NewSession, Push, SessionError, Sessions, PUSH_VERSION};
use crate::store::{now_ms, SessionPatch};

pub fn router(sessions: Arc<Sessions>) -> Router {
    Router::new()
        .route("/api/v1/health", get(|| async { Json(json!({ "ok": true, "push_version": PUSH_VERSION })) }))
        .route("/api/v1/agents", get(agents))
        .route("/api/v1/sessions", get(list_sessions).post(create_session))
        .route("/api/v1/sessions/{id}", get(get_session).patch(patch_session))
        .route("/api/v1/sessions/{id}/export", get(export_session))
        .route("/api/v1/sessions/{id}/fork", post(fork_session))
        .route("/api/v1/search", get(search))
        .route("/api/v1/projects", get(list_projects).post(create_project))
        .route("/api/v1/projects/{name}", get(get_project))
        .route("/api/v1/projects/{name}/computers/{computer_id}", put(assign).delete(unassign))
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
            SessionError::Account(_) => StatusCode::CONFLICT,
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

impl From<ProjectError> for ApiError {
    fn from(e: ProjectError) -> Self {
        let code = match &e {
            ProjectError::NotFound(_) | ProjectError::ComputerNotFound(_) => StatusCode::NOT_FOUND,
            ProjectError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ProjectError::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        ApiError(code, format!("{e:#}"))
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
    /// Account id (FR-U2); omitted = the router chooses.
    account: Option<String>,
}

async fn create_session(
    State(s): State<Arc<Sessions>>,
    Json(b): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    let agent = AgentKind::parse(&b.agent)
        .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, format!("unknown agent {}", b.agent)))?;
    let rec = s.create_with_account(
        NewSession {
            project: b.project,
            agent,
            cwd: b.cwd,
            model: b.model,
            title: b.title.unwrap_or_else(|| "New conversation".into()),
        },
        b.account.as_deref(),
    )?;
    Ok((StatusCode::CREATED, Json(rec)))
}

async fn get_session(
    State(s): State<Arc<Sessions>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let rec = s.store().session(&id)?.ok_or_else(|| SessionError::NotFound(id.clone()))?;
    // `can_fork`: whether the fork action is offered (FR-S5: disabled, not failing).
    Ok(Json(json!({ "session": rec, "live": s.is_live(&id).await, "can_fork": false })))
}

/// FR-L9: rename, pin, archive.
async fn patch_session(
    State(s): State<Arc<Sessions>>,
    Path(id): Path<String>,
    Json(patch): Json<SessionPatch>,
) -> ApiResult<impl IntoResponse> {
    if patch.title.as_deref().is_some_and(|t| t.trim().is_empty()) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "title must not be empty".into()));
    }
    Ok(Json(s.update_meta(&id, &patch)?))
}

/// Version of the export file (`format: "ember-transcript"`).
pub const EXPORT_VERSION: u32 = 1;

/// FR-L9: the session as one self-contained JSON file: its record and every stored event in
/// order (the normalised transcript, FR-S2).
/// Streaming deltas are included so the file replays exactly as clients saw the session.
async fn export_session(
    State(s): State<Arc<Sessions>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let rec = s.store().session(&id)?.ok_or_else(|| SessionError::NotFound(id.clone()))?;
    let events = s.store().events_after(&id, 0)?;
    let body = json!({
        "format": "ember-transcript",
        "version": EXPORT_VERSION,
        "exported_at": now_ms(),
        "session": rec,
        "events": events,
    });
    let disposition = format!("attachment; filename=\"ember-session-{}.json\"", file_safe(&id));
    Ok(([(header::CONTENT_DISPOSITION, disposition)], Json(body)))
}

/// Keep only characters safe in a quoted header filename.
fn file_safe(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// FR-S5. No wrapped agent forks yet: Claude Code has no fork-from-turn in its headless mode
/// and Codex's `thread/fork` is not wired into the adapter (it forks a whole thread, not from a
/// chosen turn, and needs a second app-server thread per session). Clients read `can_fork`
/// from `GET /sessions/{id}` and disable the action; this answers 501 for anyone who calls it.
async fn fork_session(
    State(s): State<Arc<Sessions>>,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let rec = s.store().session(&id)?.ok_or_else(|| SessionError::NotFound(id.clone()))?;
    let reason = format!("forking is not supported for {} sessions yet", rec.agent.as_str());
    Ok((StatusCode::NOT_IMPLEMENTED, Json(json!({ "error": reason, "reason": reason }))).into_response())
}

#[derive(Deserialize)]
struct SearchQuery {
    #[serde(default)]
    q: String,
    limit: Option<usize>,
}

/// Default and maximum number of search hits.
pub const SEARCH_LIMIT: usize = 50;
pub const SEARCH_LIMIT_MAX: usize = 500;

/// FR-S4: indexed full-text search across every session's messages.
async fn search(State(s): State<Arc<Sessions>>, Query(q): Query<SearchQuery>) -> ApiResult<impl IntoResponse> {
    let limit = q.limit.unwrap_or(SEARCH_LIMIT).clamp(1, SEARCH_LIMIT_MAX);
    Ok(Json(s.store().search(&q.q, limit)?))
}

async fn list_projects(State(s): State<Arc<Sessions>>) -> ApiResult<impl IntoResponse> {
    Ok(Json(s.store().projects()?))
}

#[derive(Deserialize)]
struct ProjectBody {
    name: String,
}

async fn create_project(
    State(s): State<Arc<Sessions>>,
    Json(b): Json<ProjectBody>,
) -> ApiResult<impl IntoResponse> {
    let (project, created) = s.store().ensure_project(&b.name)?;
    if created {
        s.publish(Push::ProjectUpdated { v: PUSH_VERSION, project: project.clone() });
    }
    Ok((if created { StatusCode::CREATED } else { StatusCode::OK }, Json(project)))
}

async fn get_project(State(s): State<Arc<Sessions>>, Path(name): Path<String>) -> ApiResult<impl IntoResponse> {
    Ok(Json(s.store().project(&name)?.ok_or(ProjectError::NotFound(name))?))
}

/// FR-L4: assign a computer to a project.
async fn assign(
    State(s): State<Arc<Sessions>>,
    Path((name, computer_id)): Path<(String, String)>,
) -> ApiResult<impl IntoResponse> {
    let project = s.store().assign_computer(&name, &computer_id)?;
    s.publish(Push::ProjectUpdated { v: PUSH_VERSION, project: project.clone() });
    Ok(Json(project))
}

/// FR-L4: unassign a computer from a project.
async fn unassign(
    State(s): State<Arc<Sessions>>,
    Path((name, computer_id)): Path<(String, String)>,
) -> ApiResult<impl IntoResponse> {
    let project = s.store().unassign_computer(&name, &computer_id)?;
    s.publish(Push::ProjectUpdated { v: PUSH_VERSION, project: project.clone() });
    Ok(Json(project))
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
    // Subscribe before the upgrade response goes out, so an event stored in between is not lost.
    let rx = s.subscribe();
    ws.on_upgrade(move |socket| push_loop(socket, rx))
}

async fn push_loop(mut socket: WebSocket, mut rx: tokio::sync::broadcast::Receiver<Push>) {
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
