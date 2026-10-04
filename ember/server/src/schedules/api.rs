//! Schedule HTTP routes (SPEC FR-A8).
//!
//! User routes:
//!
//! | Method | Path | Body → Response |
//! | ------ | ---- | --------------- |
//! | GET    | `/api/v1/schedules?project=` | → `[Schedule]` |
//! | POST   | `/api/v1/schedules` | `NewSchedule` → 201 `Schedule` |
//! | GET    | `/api/v1/schedules/{id}` | → `Schedule` |
//! | PATCH  | `/api/v1/schedules/{id}` | `{prompt?, kind?, catch_up?}` → `Schedule` |
//! | DELETE | `/api/v1/schedules/{id}` | → 204 |
//! | POST   | `/api/v1/schedules/{id}/pause` | → `Schedule` |
//! | POST   | `/api/v1/schedules/{id}/resume` | → `Schedule` |
//! | POST   | `/api/v1/schedules/{id}/run` | → 202 `ScheduleRun` (run now) |
//! | GET    | `/api/v1/schedules/{id}/runs?limit=` | → `[ScheduleRun]`, newest first |
//!
//! `NewSchedule` is `{project, agent?, account?, computer?, cwd?, prompt, kind, target,
//! catch_up?, paused?}` with `kind` one of `{"type":"cron","expr":"0 9 * * 1-5","tz":"Asia/Seoul"}`,
//! `{"type":"interval","seconds":3600}`, `{"type":"once","at":"2026-10-04T09:00:00+09:00"}` (or Unix
//! ms) and `target` one of `{"type":"continue","session_id":"…"}`, `{"type":"new","title":"…"}`.
//! `agent` and `cwd` are required for `new` targets.
//!
//! Agent routes (`ember-a2a schedule …`), authenticated with `Authorization: Bearer
//! <EMBER_RUNTIME_TOKEN>` and confined to the calling session's project:
//!
//! | Method | Path | Body → Response |
//! | ------ | ---- | --------------- |
//! | GET    | `/api/v1/a2a/schedules` | → `[Schedule]` of the caller's project |
//! | POST   | `/api/v1/a2a/schedules` | `{prompt, kind, target?, agent?, cwd?, catch_up?}` → 201 `Schedule` |
//! | DELETE | `/api/v1/a2a/schedules/{id}` | → 204 |
//!
//! For agents, `target` defaults to continuing the calling session, `agent` and `cwd` to the
//! caller's, and new sessions use the caller's account. Changes are pushed
//! (`schedule_created`, `schedule_updated`, `schedule_deleted`, `schedule_run`). Errors are
//! `{error, code}` with `code` one of `not_found`, `bad_request`, `unauthorized`, `internal`.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use super::{NewSchedule, ScheduleError, ScheduleKind, SchedulePatch, ScheduleTarget, Scheduler};
use crate::a2a::A2a;

pub fn router(scheduler: Arc<Scheduler>) -> Router {
    Router::new()
        .route("/api/v1/schedules", get(list).post(create))
        .route("/api/v1/schedules/{id}", get(one).patch(update).delete(remove))
        .route("/api/v1/schedules/{id}/pause", post(pause))
        .route("/api/v1/schedules/{id}/resume", post(resume))
        .route("/api/v1/schedules/{id}/run", post(run_now))
        .route("/api/v1/schedules/{id}/runs", get(runs))
        .with_state(scheduler)
}

/// The agent-side routes; `a2a` authenticates runtime tokens.
pub fn agent_router(scheduler: Arc<Scheduler>, a2a: Arc<A2a>) -> Router {
    Router::new()
        .route("/api/v1/a2a/schedules", get(agent_list).post(agent_create))
        .route("/api/v1/a2a/schedules/{id}", delete(agent_remove))
        .with_state(AgentState { scheduler, a2a })
}

impl IntoResponse for ScheduleError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            ScheduleError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            ScheduleError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            ScheduleError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            ScheduleError::Other(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };
        (status, Json(json!({ "error": format!("{self:#}"), "code": code }))).into_response()
    }
}

type ApiResult<T> = Result<T, ScheduleError>;

#[derive(Deserialize)]
struct ListQuery {
    project: Option<String>,
}

async fn list(State(s): State<Arc<Scheduler>>, Query(q): Query<ListQuery>) -> ApiResult<impl IntoResponse> {
    Ok(Json(s.list(q.project.as_deref())?))
}

async fn create(State(s): State<Arc<Scheduler>>, Json(b): Json<NewSchedule>) -> ApiResult<impl IntoResponse> {
    Ok((StatusCode::CREATED, Json(s.create(b, None)?)))
}

async fn one(State(s): State<Arc<Scheduler>>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    Ok(Json(s.get(&id)?))
}

async fn update(
    State(s): State<Arc<Scheduler>>,
    Path(id): Path<String>,
    Json(b): Json<SchedulePatch>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(s.update(&id, b)?))
}

async fn remove(State(s): State<Arc<Scheduler>>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    s.delete(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn pause(State(s): State<Arc<Scheduler>>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    Ok(Json(s.pause(&id)?))
}

async fn resume(State(s): State<Arc<Scheduler>>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    Ok(Json(s.resume(&id)?))
}

async fn run_now(State(s): State<Arc<Scheduler>>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    Ok((StatusCode::ACCEPTED, Json(s.run_now(&id).await?)))
}

#[derive(Deserialize)]
struct RunsQuery {
    limit: Option<usize>,
}

async fn runs(
    State(s): State<Arc<Scheduler>>,
    Path(id): Path<String>,
    Query(q): Query<RunsQuery>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(s.runs(&id, q.limit.unwrap_or(50).clamp(1, 500))?))
}

// ---------------------------------------------------------------------------------------------
// Agent routes

#[derive(Clone)]
struct AgentState {
    scheduler: Arc<Scheduler>,
    a2a: Arc<A2a>,
}

/// The calling session's record, from its runtime token.
fn caller(st: &AgentState, headers: &HeaderMap) -> ApiResult<crate::store::SessionRecord> {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or(ScheduleError::Unauthorized)?;
    let id = st.a2a.authenticate(token).map_err(|_| ScheduleError::Unauthorized)?;
    st.scheduler.sessions().store().session(&id)?.ok_or(ScheduleError::Unauthorized)
}

async fn agent_list(State(st): State<AgentState>, headers: HeaderMap) -> ApiResult<impl IntoResponse> {
    let me = caller(&st, &headers)?;
    Ok(Json(st.scheduler.list(Some(&me.project))?))
}

#[derive(Deserialize)]
struct AgentNew {
    prompt: String,
    kind: ScheduleKind,
    #[serde(default)]
    target: Option<ScheduleTarget>,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    catch_up: bool,
}

async fn agent_create(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Json(b): Json<AgentNew>,
) -> ApiResult<impl IntoResponse> {
    let me = caller(&st, &headers)?;
    let target = b.target.unwrap_or(ScheduleTarget::Continue { session_id: me.id.clone() });
    let new = match &target {
        ScheduleTarget::Continue { .. } => NewSchedule {
            project: me.project.clone(),
            agent: b.agent,
            account: None,
            computer: None,
            cwd: None,
            prompt: b.prompt,
            kind: b.kind,
            target,
            catch_up: b.catch_up,
            paused: false,
        },
        ScheduleTarget::New { .. } => NewSchedule {
            project: me.project.clone(),
            agent: Some(b.agent.unwrap_or_else(|| me.agent.as_str().to_string())),
            account: me.account_id.clone(),
            computer: None,
            cwd: Some(b.cwd.unwrap_or_else(|| me.cwd.clone())),
            prompt: b.prompt,
            kind: b.kind,
            target,
            catch_up: b.catch_up,
            paused: false,
        },
    };
    Ok((StatusCode::CREATED, Json(st.scheduler.create(new, Some(&me.id))?)))
}

async fn agent_remove(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let me = caller(&st, &headers)?;
    let s = st.scheduler.get(&id)?;
    if s.project != me.project {
        // Not visible from this project: same answer as a missing one.
        return Err(ScheduleError::NotFound(id));
    }
    st.scheduler.delete(&id)?;
    Ok(StatusCode::NO_CONTENT)
}
