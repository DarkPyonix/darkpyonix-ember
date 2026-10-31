//! MCP registry HTTP routes (SPEC FR-A7).
//!
//! | Method | Path | Body → Response |
//! | ------ | ---- | --------------- |
//! | GET    | `/api/mcp` | → `[McpEntry]` |
//! | POST   | `/api/mcp` | `{name, command, args?, env?: {NAME: value}, enabled?, scope?}` → 201 `McpEntry` |
//! | GET    | `/api/mcp/{id}` | → `McpEntry` |
//! | PATCH  | `/api/mcp/{id}` | `{name?, command?, args?, env?: {NAME: value or null}, enabled?, scope?}` → `McpEntry` |
//! | DELETE | `/api/mcp/{id}` | → 204 |
//!
//! `McpEntry` is `{id, name, command, args, env_keys, enabled, scope, created_at, updated_at}`:
//! environment values are write-only. `scope` is `all` (default) or `project:<name>`. Changes
//! apply from each agent process's next start. Errors are `{error, code}` with `code` one of
//! `not_found`, `bad_request`, `conflict`, `internal`.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use super::{McpError, McpPatch, McpRegistry, NewMcp};

pub fn router(registry: Arc<McpRegistry>) -> Router {
    Router::new()
        .route("/api/mcp", get(list).post(add))
        .route("/api/mcp/{id}", get(one).patch(update).delete(remove))
        .with_state(registry)
}

impl IntoResponse for McpError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            McpError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            McpError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            McpError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            McpError::Other(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };
        (status, Json(json!({ "error": format!("{self:#}"), "code": code }))).into_response()
    }
}

type ApiResult<T> = Result<T, McpError>;

async fn list(State(r): State<Arc<McpRegistry>>) -> ApiResult<impl IntoResponse> {
    Ok(Json(r.list()?))
}

async fn add(State(r): State<Arc<McpRegistry>>, Json(b): Json<NewMcp>) -> ApiResult<impl IntoResponse> {
    Ok((StatusCode::CREATED, Json(r.add(b)?)))
}

async fn one(State(r): State<Arc<McpRegistry>>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    Ok(Json(r.get(&id)?))
}

async fn update(
    State(r): State<Arc<McpRegistry>>,
    Path(id): Path<String>,
    Json(b): Json<McpPatch>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(r.update(&id, b)?))
}

async fn remove(State(r): State<Arc<McpRegistry>>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    r.delete(&id)?;
    Ok(StatusCode::NO_CONTENT)
}
