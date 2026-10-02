//! A2A HTTP routes (SPEC §T).
//!
//! Agent routes authenticate with `Authorization: Bearer <EMBER_RUNTIME_TOKEN>`; the token
//! identifies the calling session:
//! - `GET  /api/v1/a2a/targets` → `[Target]`
//! - `POST /api/v1/a2a/messages` `{to, text, reply_to?}` → `202 {id, to, status}` where status is
//!   `delivered` or `queued`
//! - `GET  /api/v1/a2a/messages/{id}` → the message, if the caller sent or received it
//!
//! User routes (switches, FR-T6):
//! - `GET|PUT /api/v1/a2a/settings` `{enabled}`
//! - `GET|PUT /api/v1/sessions/{id}/a2a` `{enabled}`
//!
//! Errors are `{error, code}` with `code` one of `unauthorized`, `disabled`, `rate_limited`,
//! `not_found`, `bad_request`, `internal`.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use super::{A2a, A2aError};

pub fn router(a2a: Arc<A2a>) -> Router {
    Router::new()
        .route("/api/v1/a2a/targets", get(targets))
        .route("/api/v1/a2a/messages", axum::routing::post(send))
        .route("/api/v1/a2a/messages/{id}", get(message))
        .route("/api/v1/a2a/settings", get(get_settings).put(put_settings))
        .route(
            "/api/v1/sessions/{id}/a2a",
            get(get_session_switch).put(put_session_switch),
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
