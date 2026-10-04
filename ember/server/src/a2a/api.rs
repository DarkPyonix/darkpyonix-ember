//! A2A HTTP routes (SPEC §T).
//!
//! Agent routes authenticate with `Authorization: Bearer <EMBER_RUNTIME_TOKEN>`; the token
//! identifies the calling session:
//! - `GET  /api/a2a/targets` → `[Target]`
//! - `POST /api/a2a/messages` `{to, text, reply_to?}` → `202 {id, to, status}` where status is
//!   `delivered` or `queued`
//! - `GET  /api/a2a/messages/{id}` → the message, if the caller sent or received it
//!
//!
//! Team routes for agents (FR-T7, token = the caller; see [`super::team`]):
//! - `GET    /api/a2a/team` → `TeamView` or `null`
//! - `POST   /api/a2a/team/members` `{name, prompt, agent?, account?, computer?, title?}` →
//!   `201 {team_id, member, message}` (leader only; the first spawn makes the caller leader)
//! - `DELETE /api/a2a/team/members/{name-or-session}` → `Member` (leader only)
//! - `GET    /api/a2a/team/tasks` → `[Task]`
//! - `POST   /api/a2a/team/tasks` `{title, detail?, assignee?}` → `201 Task`
//! - `PATCH  /api/a2a/team/tasks/{number}` `{title?, detail?, status?, assignee?}` → `Task`
//!   (`assignee: ""` or `"none"` unassigns)
//! - `GET    /api/a2a/team/mail?after=&limit=` → `[Mail]`, oldest first
//! - `POST   /api/a2a/team/mail` `{to?, text}` → `202 {mail, deliveries}` (`to` omitted or
//!   `all` = the whole team)
//!
//! User routes (switches and mentions, FR-T6; teams, FR-T7):
//! - `GET|PUT /api/a2a/settings` `{enabled}`
//! - `GET|PUT /api/sessions/{id}/a2a` `{enabled}`
//! - `GET /api/sessions/{id}/mentions?q=&limit=` → `[Target]`: sessions a message typed in
//!   `{id}` may mention. Mentions themselves are resolved when the message is posted to
//!   `POST /api/sessions/{id}/messages` (see [`super::mention`]).
//! - `GET /api/sessions/{id}/team` → the `TeamView` `{id}` leads or belongs to, or `null`
//! - `GET /api/teams/{team_id}` → `TeamView`
//! - `GET /api/teams/{team_id}/mail?after=&limit=` → `[Mail]` (all of the team's mail)
//! - `POST /api/teams/{team_id}/members/{session_id}/end` → `Member`
//!
//! Team changes are pushed as `{"type": "team_updated", "team": TeamView}`.
//!
//! Errors are `{error, code}` with `code` one of `unauthorized`, `disabled`, `rate_limited`,
//! `not_found`, `bad_request`, `forbidden`, `internal`.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use super::team::{NewTask, SpawnRequest, TaskPatch};
use super::{A2a, A2aError};

pub fn router(a2a: Arc<A2a>) -> Router {
    Router::new()
        .route("/api/a2a/targets", get(targets))
        .route("/api/a2a/messages", post(send))
        .route("/api/a2a/messages/{id}", get(message))
        .route("/api/a2a/settings", get(get_settings).put(put_settings))
        .route(
            "/api/sessions/{id}/a2a",
            get(get_session_switch).put(put_session_switch),
        )
        .route("/api/a2a/team", get(my_team))
        .route("/api/a2a/team/members", post(spawn))
        .route("/api/a2a/team/members/{member}", delete(end_member))
        .route("/api/a2a/team/tasks", get(list_tasks).post(add_task))
        .route("/api/a2a/team/tasks/{number}", patch(update_task))
        .route("/api/a2a/team/mail", get(read_mail).post(send_mail))
        .route("/api/sessions/{id}/mentions", get(mentions))
        .route("/api/sessions/{id}/team", get(session_team))
        .route("/api/teams/{team_id}", get(team))
        .route("/api/teams/{team_id}/mail", get(team_mail))
        .route(
            "/api/teams/{team_id}/members/{session_id}/end",
            post(user_end_member),
        )
        .with_state(a2a)
}

impl IntoResponse for A2aError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            A2aError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            A2aError::Disabled(_) => (StatusCode::FORBIDDEN, "disabled"),
            A2aError::RateLimited(_) => (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
            A2aError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            A2aError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            A2aError::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            A2aError::Other(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };
        (
            status,
            Json(json!({ "error": format!("{self:#}"), "code": code })),
        )
            .into_response()
    }
}

type ApiResult<T> = Result<T, A2aError>;

fn caller(a2a: &A2a, headers: &HeaderMap) -> ApiResult<String> {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or(A2aError::Unauthorized)?;
    a2a.authenticate(token)
}

async fn targets(State(a): State<Arc<A2a>>, headers: HeaderMap) -> ApiResult<impl IntoResponse> {
    let me = caller(&a, &headers)?;
    Ok(Json(a.targets(&me)?))
}

#[derive(Deserialize)]
struct SendBody {
    to: String,
    text: String,
    reply_to: Option<String>,
}

async fn send(
    State(a): State<Arc<A2a>>,
    headers: HeaderMap,
    Json(b): Json<SendBody>,
) -> ApiResult<impl IntoResponse> {
    let me = caller(&a, &headers)?;
    let receipt = a.send(&me, &b.to, &b.text, b.reply_to.as_deref()).await?;
    Ok((StatusCode::ACCEPTED, Json(receipt)))
}

async fn message(
    State(a): State<Arc<A2a>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let me = caller(&a, &headers)?;
    match a.store().message(&id)? {
        Some(m) if m.from_session == me || m.to_session == me => Ok(Json(m)),
        _ => Err(A2aError::NotFound(format!("message {id} not found"))),
    }
}

#[derive(Deserialize)]
struct Switch {
    enabled: bool,
}

async fn get_settings(State(a): State<Arc<A2a>>) -> ApiResult<impl IntoResponse> {
    Ok(Json(json!({ "enabled": a.enabled()? })))
}

async fn put_settings(
    State(a): State<Arc<A2a>>,
    Json(b): Json<Switch>,
) -> ApiResult<impl IntoResponse> {
    a.set_enabled(b.enabled)?;
    Ok(Json(json!({ "enabled": b.enabled })))
}

async fn get_session_switch(
    State(a): State<Arc<A2a>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(json!({ "enabled": a.session_enabled(&id)? })))
}

async fn put_session_switch(
    State(a): State<Arc<A2a>>,
    Path(id): Path<String>,
    Json(b): Json<Switch>,
) -> ApiResult<impl IntoResponse> {
    a.set_session_enabled(&id, b.enabled)?;
    Ok(Json(json!({ "enabled": b.enabled })))
}

// ---- teams (FR-T7) ------------------------------------------------------------------------------

async fn my_team(State(a): State<Arc<A2a>>, headers: HeaderMap) -> ApiResult<impl IntoResponse> {
    let me = caller(&a, &headers)?;
    Ok(Json(a.team_of(&me)?))
}

async fn spawn(
    State(a): State<Arc<A2a>>,
    headers: HeaderMap,
    Json(b): Json<SpawnRequest>,
) -> ApiResult<impl IntoResponse> {
    let me = caller(&a, &headers)?;
    let out = a.spawn_teammate(&me, b).await?;
    Ok((StatusCode::CREATED, Json(out)))
}

async fn end_member(
    State(a): State<Arc<A2a>>,
    headers: HeaderMap,
    Path(member): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let me = caller(&a, &headers)?;
    Ok(Json(a.end_teammate(&me, &member).await?))
}

async fn list_tasks(State(a): State<Arc<A2a>>, headers: HeaderMap) -> ApiResult<impl IntoResponse> {
    let me = caller(&a, &headers)?;
    Ok(Json(a.tasks(&me)?))
}

async fn add_task(
    State(a): State<Arc<A2a>>,
    headers: HeaderMap,
    Json(b): Json<NewTask>,
) -> ApiResult<impl IntoResponse> {
    let me = caller(&a, &headers)?;
    let task = a.add_task(&me, b).await?;
    Ok((StatusCode::CREATED, Json(task)))
}

async fn update_task(
    State(a): State<Arc<A2a>>,
    headers: HeaderMap,
    Path(number): Path<String>,
    Json(b): Json<TaskPatch>,
) -> ApiResult<impl IntoResponse> {
    let me = caller(&a, &headers)?;
    let number: i64 = number
        .trim_start_matches('#')
        .parse()
        .map_err(|_| A2aError::BadRequest(format!("bad task number {number}")))?;
    Ok(Json(a.update_task(&me, number, b).await?))
}

#[derive(Deserialize)]
struct MailQuery {
    #[serde(default)]
    after: i64,
    limit: Option<usize>,
}

async fn read_mail(
    State(a): State<Arc<A2a>>,
    headers: HeaderMap,
    Query(q): Query<MailQuery>,
) -> ApiResult<impl IntoResponse> {
    let me = caller(&a, &headers)?;
    Ok(Json(a.read_mail(&me, q.after, q.limit)?))
}

#[derive(Deserialize)]
struct MailBody {
    #[serde(default)]
    to: Option<String>,
    text: String,
}

async fn send_mail(
    State(a): State<Arc<A2a>>,
    headers: HeaderMap,
    Json(b): Json<MailBody>,
) -> ApiResult<impl IntoResponse> {
    let me = caller(&a, &headers)?;
    let receipt = a.send_mail(&me, b.to.as_deref(), &b.text).await?;
    Ok((StatusCode::ACCEPTED, Json(receipt)))
}

async fn session_team(
    State(a): State<Arc<A2a>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(a.team_of(&id)?))
}

async fn team(State(a): State<Arc<A2a>>, Path(team_id): Path<String>) -> ApiResult<impl IntoResponse> {
    match a.team_view(&team_id)? {
        Some(t) => Ok(Json(t)),
        None => Err(A2aError::NotFound(format!("team {team_id} not found"))),
    }
}

async fn team_mail(
    State(a): State<Arc<A2a>>,
    Path(team_id): Path<String>,
    Query(q): Query<MailQuery>,
) -> ApiResult<impl IntoResponse> {
    if a.team_view(&team_id)?.is_none() {
        return Err(A2aError::NotFound(format!("team {team_id} not found")));
    }
    Ok(Json(a.mail_of(&team_id, None, q.after, q.limit)?))
}

async fn user_end_member(
    State(a): State<Arc<A2a>>,
    Path((team_id, session_id)): Path<(String, String)>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(a.end_teammate_by_user(&team_id, &session_id).await?))
}

// ---- mentions (FR-T6) ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct MentionQuery {
    #[serde(default)]
    q: String,
    limit: Option<usize>,
}

async fn mentions(
    State(a): State<Arc<A2a>>,
    Path(id): Path<String>,
    Query(q): Query<MentionQuery>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(a.mention_candidates(&id, &q.q, q.limit)?))
}
