//! Channel HTTP routes (SPEC FR-T10, FR-T16, FR-L12).
//!
//! | Method | Path | Body → Response |
//! | ------ | ---- | --------------- |
//! | GET    | `/api/channels?project=` | → `[Channel]` (no `messages`) |
//! | POST   | `/api/channels` | `{project, name, agents?}` → 201 `Channel` |
//! | GET    | `/api/channels/{id}` | → `Channel` with the latest messages |
//! | GET    | `/api/channels/{id}/messages?after=` | → `[Message]`, oldest first, `seq > after` |
//! | POST   | `/api/channels/{id}/messages` | `{text, from?}` → 201 `Message` |
//!
//! `from` defaults to `user`; otherwise it is a member agent's session id. Errors are
//! `{error, code}` with `code` one of `not_found`, `bad_request`, `forbidden`, `internal`.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use super::{ChannelError, NewChannel, USER};
use crate::store::Store;

pub fn router(store: Arc<Store>) -> Router {
    Router::new()
        .route("/api/channels", get(list).post(create))
        .route("/api/channels/{id}", get(one))
        .route("/api/channels/{id}/messages", get(messages).post(post_message))
        .with_state(store)
}

impl IntoResponse for ChannelError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            ChannelError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            ChannelError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            ChannelError::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            ChannelError::Other(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };
        (status, Json(json!({ "error": format!("{self:#}"), "code": code }))).into_response()
    }
}

type ApiResult<T> = Result<T, ChannelError>;

#[derive(Deserialize)]
struct ListQuery {
    project: Option<String>,
}

async fn list(State(s): State<Arc<Store>>, Query(q): Query<ListQuery>) -> ApiResult<impl IntoResponse> {
    Ok(Json(s.channels(q.project.as_deref())?))
}

async fn create(State(s): State<Arc<Store>>, Json(b): Json<NewChannel>) -> ApiResult<impl IntoResponse> {
    Ok((StatusCode::CREATED, Json(s.create_channel(b)?)))
}

async fn one(State(s): State<Arc<Store>>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    Ok(Json(s.channel(&id)?))
}

#[derive(Deserialize)]
struct AfterQuery {
    after: Option<i64>,
}

async fn messages(
    State(s): State<Arc<Store>>,
    Path(id): Path<String>,
    Query(q): Query<AfterQuery>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(s.read_channel_messages(&id, q.after.unwrap_or(0))?))
}

#[derive(Deserialize)]
struct NewMessage {
    text: String,
    #[serde(default)]
    from: Option<String>,
}

async fn post_message(
    State(s): State<Arc<Store>>,
    Path(id): Path<String>,
    Json(b): Json<NewMessage>,
) -> ApiResult<impl IntoResponse> {
    let from = b.from.unwrap_or_else(|| USER.to_string());
    Ok((StatusCode::CREATED, Json(s.post_channel_message(&id, &from, &b.text)?)))
}
