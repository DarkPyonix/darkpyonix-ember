//! HTTP API and push channel for clients (SPEC §L via FR-S*, PR-1).
//!
//! All routes live under `/api`. The push channel is a WebSocket at `/api/push`; on connect
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
//! | GET    | `/api/projects` | → `[Project]` (`{name, created_at, computers, repository, default_branch, member_defaults, character_style, archived}`) |
//! | POST   | `/api/projects` | `{name}` → 201 `Project` (200 when it existed) |
//! | GET    | `/api/projects/{name}` | → `Project` |
//! | PATCH  | `/api/projects/{name}` | `{name?, repository?, default_branch?, member_defaults?, character_style?}` → `Project` (FR-L26); errors `{error, code}` |
//! | PUT    | `/api/projects/{name}/computers/{computer_id}` | → `Project` (assign; idempotent) |
//! | DELETE | `/api/projects/{name}/computers/{computer_id}` | → `Project` (unassign; idempotent) |
//! | PATCH  | `/api/sessions/{id}` | `{title?, pinned?, archived?}` → `SessionRecord` |
//! | GET    | `/api/search?q=&limit=` | → `[SearchHit]` (`{session_id, seq, kind, snippet, title, project, archived}`) |
//! | GET    | `/api/sessions/{id}/export` | → transcript file (JSON, `Content-Disposition: attachment`) |
//! | PUT    | `/api/sessions/{id}/approve-all` | `{enabled}` → `{approve_all}` (FR-A5; `GET /sessions/{id}` carries `approve_all`; push `approve_all`) |
//! | POST   | `/api/sessions/{id}/fork` | `{turn_seq?, title?}` → 201 new session; 409 without a completed turn |
//!
//! Tokens (FR-U7; cost is always `null`, the response says why in `cost_note`):
//!
//! | Method | Path | Response |
//! | ------ | ---- | -------- |
//! | GET    | `/api/usage/summary?days=7&project=` or `?day=YYYY-MM-DD` | `{from, to, days:[{day, input_tokens, output_tokens, total_tokens, turns, cost_usd, sessions, by_agent:[{agent, ...}]}], input_tokens, output_tokens, total_tokens, turns, cost_usd, by_agent, cost_note}` |
//! | GET    | `/api/sessions/{id}/usage` | `{session_id, input_tokens, output_tokens, total_tokens, turns, cost_usd, reported, cost_note}`; also `usage` in `GET /sessions/{id}` |
//!
//! (`GET /api/usage` is per account, in `crate::accounts::api`.)
//!
//! Git per project (FR-G4) is in [`git`]: `POST /api/projects/{name}/git/<op>`.
//!
//! Project names are path segments: clients percent-encode them. Assignment and metadata
//! changes are pushed (`project_updated`, `session_updated`).

pub mod git;
pub mod ide;
pub mod ide_extensions;
pub mod origin;

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
use crate::session::{NewSession, Push, SessionError, Sessions};
use crate::store::{now_ms, SessionPatch};

pub fn router(sessions: Arc<Sessions>) -> Router {
    Router::new()
        .route("/api/health", get(|| async { Json(json!({ "ok": true })) }))
        .route("/api/agents", get(agents))
        .route("/api/sessions", get(list_sessions).post(create_session))
        .route("/api/sessions/{id}", get(get_session).patch(patch_session))
        .route("/api/sessions/{id}/export", get(export_session))
        .route("/api/sessions/{id}/fork", post(fork_session))
        .route("/api/search", get(search))
        .route("/api/usage/summary", get(usage_summary))
        .route("/api/sessions/{id}/usage", get(session_usage))
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/projects/{name}", get(get_project).patch(patch_project))
        .route("/api/projects/{name}/computers/{computer_id}", put(assign).delete(unassign))
        .route("/api/sessions/{id}/events", get(events))
        .route("/api/sessions/{id}/messages", post(send_message))
        .route("/api/sessions/{id}/approvals/{approval_id}", post(answer))
        .route("/api/sessions/{id}/approve-all", put(set_approve_all))
        .route("/api/sessions/{id}/interrupt", post(interrupt))
        .route("/api/sessions/{id}/lease", post(lease))
        .route("/api/push", get(push))
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
            ProjectError::Protected(_) => StatusCode::FORBIDDEN,
            ProjectError::NameTaken(_) => StatusCode::CONFLICT,
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
    // `can_fork`: whether the fork action is offered (FR-S5: disabled, not failing). It needs a
    // completed turn; `fork_turns` lists the sequence numbers a fork may start after.
    let turns = crate::fork::completed_turns(&s.store().events_after(&id, 0)?);
    Ok(Json(json!({
        "session": rec,
        "live": s.is_live(&id).await,
        "can_fork": !turns.is_empty(),
        "fork_turns": turns,
        "fork_of": s.store().fork_origin(&id)?,
        "approve_all": s.approve_all(&id),
        "usage": s.store().session_usage(&id)?,
    })))
}

/// FR-U7: tokens one session's harness reported.
async fn session_usage(
    State(s): State<Arc<Sessions>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let u = s.store().session_usage(&id)?.ok_or_else(|| SessionError::NotFound(id.clone()))?;
    Ok(Json(u))
}

#[derive(Deserialize)]
struct UsageQuery {
    /// One UTC day, `YYYY-MM-DD`; wins over `days`.
    day: Option<String>,
    /// The last N UTC days including today (default 7, 1..=366).
    days: Option<i64>,
    project: Option<String>,
}

/// FR-U7: tokens per UTC day (and per harness), from the usage events the agents reported.
async fn usage_summary(
    State(s): State<Arc<Sessions>>,
    Query(q): Query<UsageQuery>,
) -> ApiResult<impl IntoResponse> {
    let (now, days) = match &q.day {
        Some(day) => {
            let start = s.store().day_start_ms(day)?.ok_or_else(|| {
                ApiError(StatusCode::BAD_REQUEST, format!("day must be a date like 2026-11-06, got {day:?}"))
            })?;
            (start, 1)
        }
        None => (now_ms(), q.days.unwrap_or(7)),
    };
    Ok(Json(s.store().usage_summary(now, days, q.project.as_deref())?))
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

#[derive(Deserialize, Default)]
struct ForkBody {
    /// Sequence number of a completed turn's `turn_ended`; the last completed turn when absent.
    #[serde(default)]
    turn_seq: Option<i64>,
    #[serde(default)]
    title: Option<String>,
}

/// FR-S5: a new session that starts after a completed turn of this one. 409 when the session has
/// no such turn (the client reads `can_fork` and does not offer the action).
async fn fork_session(
    State(s): State<Arc<Sessions>>,
    Path(id): Path<String>,
    body: Option<Json<ForkBody>>,
) -> ApiResult<Response> {
    let b = body.map(|Json(b)| b).unwrap_or_default();
    match s.fork(&id, b.turn_seq, b.title.as_deref()) {
        Ok(rec) => Ok((StatusCode::CREATED, Json(rec)).into_response()),
        Err(crate::fork::ForkError::NotFound(_)) => Err(SessionError::NotFound(id).into()),
        Err(crate::fork::ForkError::BadTurn(m)) => Err(ApiError(StatusCode::CONFLICT, m)),
        Err(crate::fork::ForkError::Other(e)) => Err(e.into()),
    }
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
        s.publish(Push::ProjectUpdated { project: project.clone() });
    }
    Ok((if created { StatusCode::CREATED } else { StatusCode::OK }, Json(project)))
}

async fn get_project(State(s): State<Arc<Sessions>>, Path(name): Path<String>) -> ApiResult<impl IntoResponse> {
    Ok(Json(s.store().project(&name)?.ok_or(ProjectError::NotFound(name))?))
}

/// A failed project change as `{error, code}` (FR-L26).
fn coded(status: StatusCode, code: &str, error: impl std::fmt::Display) -> Response {
    (status, Json(json!({ "error": error.to_string(), "code": code }))).into_response()
}

fn project_error(e: ProjectError) -> Response {
    let code = match &e {
        ProjectError::NotFound(_) | ProjectError::ComputerNotFound(_) => "not_found",
        ProjectError::BadRequest(_) => "invalid_field",
        ProjectError::Protected(_) => "protected_project",
        ProjectError::NameTaken(_) => "name_taken",
        ProjectError::Other(_) => "internal",
    };
    let message = format!("{e:#}");
    coded(ApiError::from(e).0, code, message)
}

/// Fields of the settings sheet whose server side is not built yet. They are refused by name so
/// the client sees a clean error and not a silent no-op (leader: blocked on an owner decision,
/// archive: #384 PR 4).
const NOT_YET_PROJECT_FIELDS: [&str; 2] = ["leader_session", "archived"];

/// FR-L26: change project settings. Body fields are optional; unknown fields are 400
/// `unknown_field`, unbuilt ones 400 `not_yet`. Pushes `project_updated` with the new state.
async fn patch_project(
    State(s): State<Arc<Sessions>>,
    Path(name): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return coded(StatusCode::BAD_REQUEST, "invalid_body", "the body must be a JSON object");
    };
    if let Some(field) = NOT_YET_PROJECT_FIELDS.iter().find(|f| map.contains_key(**f)) {
        return coded(StatusCode::BAD_REQUEST, "not_yet", format!("{field} cannot be changed yet"));
    }
    let patch: crate::projects::ProjectPatch = match serde_json::from_value(serde_json::Value::Object(map)) {
        Ok(p) => p,
        Err(e) => {
            let msg = e.to_string();
            let code = if msg.contains("unknown field") { "unknown_field" } else { "invalid_field" };
            return coded(StatusCode::BAD_REQUEST, code, msg);
        }
    };
    match s.store().patch_project(&name, &patch) {
        Ok(project) => {
            s.publish(Push::ProjectUpdated { project: project.clone() });
            Json(project).into_response()
        }
        Err(e) => project_error(e),
    }
}

/// FR-L4: assign a computer to a project.
async fn assign(
    State(s): State<Arc<Sessions>>,
    Path((name, computer_id)): Path<(String, String)>,
) -> ApiResult<impl IntoResponse> {
    let project = s.store().assign_computer(&name, &computer_id)?;
    s.publish(Push::ProjectUpdated { project: project.clone() });
    Ok(Json(project))
}

/// FR-L4: unassign a computer from a project.
async fn unassign(
    State(s): State<Arc<Sessions>>,
    Path((name, computer_id)): Path<(String, String)>,
) -> ApiResult<impl IntoResponse> {
    let project = s.store().unassign_computer(&name, &computer_id)?;
    s.publish(Push::ProjectUpdated { project: project.clone() });
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
    // Mentions of other sessions are delivered by the A2A hook (FR-T6).
    s.send_from_user(&id, &b.text).await?;
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

#[derive(Deserialize)]
struct ApproveAllBody {
    enabled: bool,
}

/// FR-A5: switch a session's approve-everything mode.
async fn set_approve_all(
    State(s): State<Arc<Sessions>>,
    Path(id): Path<String>,
    Json(b): Json<ApproveAllBody>,
) -> ApiResult<impl IntoResponse> {
    s.set_approve_all(&id, b.enabled).await?;
    Ok(Json(json!({ "approve_all": b.enabled })))
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

async fn push(
    State(s): State<Arc<Sessions>>,
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, StatusCode> {
    origin::check_origin(&headers)?;
    // Subscribe before the upgrade response goes out, so an event stored in between is not lost.
    let rx = s.subscribe();
    Ok(ws.on_upgrade(move |socket| push_loop(socket, rx)))
}

async fn push_loop(mut socket: WebSocket, mut rx: tokio::sync::broadcast::Receiver<Push>) {
    loop {
        let msg = match rx.recv().await {
            Ok(p) => p,
            // A slow client missed messages: tell it to resync from the events endpoint.
            Err(RecvError::Lagged(n)) => {
                let lag = json!({ "type": "lagged", "missed": n }).to_string();
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
