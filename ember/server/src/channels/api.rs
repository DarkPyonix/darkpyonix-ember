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
//! | POST   | `/api/channels/{id}/read` | `{seq?}` → `Channel` (no `messages`) with `unread` recomputed |
//!
//! `read` marks everything up to `seq` (default the latest) as read by `user`; the cursor only
//! moves forward. `unread` on a channel counts messages after the cursor not written by `user`;
//! on a message it is 1 or 0 the same way.
//!
//! | POST   | `/api/channels/{id}/mentions/preview` | `{text}` → `{reaches: [{id, name}], unknown: [name]}` |
//!
//! A message from `user` with `@name` of a name that is not a member agent is refused 400 with
//! `code`/`reason` `not_a_member` and `unknown: [name]`; nothing is stored or pushed (FR-T13).
//! Resolved mentions are stored and returned as `mentions: [session_id]` (FR-T12).
//!
//! A posted message is also pushed as `channel_message` on `/api/push` (FR-L12).
//!
//! `from` defaults to `user`; otherwise it is a member agent's session id. Errors are
//! `{error, code}` with `code` one of `not_found`, `bad_request`, `forbidden`, `internal`.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use super::{ChannelError, NewChannel, USER};
use crate::session::{Push, Sessions};
use crate::store::Store;

/// Router state: the store for the log, and the sessions for the push broadcaster (FR-L12) and,
/// later, delivery to agents (FR-T12).
#[derive(Clone)]
pub struct ChannelsCtx {
    pub store: Arc<Store>,
    pub sessions: Arc<Sessions>,
}

pub fn router(store: Arc<Store>, sessions: Arc<Sessions>) -> Router {
    Router::new()
        .route("/api/channels", get(list).post(create))
        .route("/api/channels/{id}", get(one))
        .route("/api/channels/{id}/messages", get(messages).post(post_message))
        .route("/api/channels/{id}/read", post(mark_read))
        .route("/api/channels/{id}/mentions/preview", post(preview))
        .with_state(ChannelsCtx { store, sessions })
}

impl IntoResponse for ChannelError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            ChannelError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            ChannelError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            ChannelError::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            ChannelError::NotAMember(names) => {
                let body = json!({ "error": format!("{self:#}"), "code": "not_a_member", "reason": "not_a_member", "unknown": names });
                return (StatusCode::BAD_REQUEST, Json(body)).into_response();
            }
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

async fn list(State(c): State<ChannelsCtx>, Query(q): Query<ListQuery>) -> ApiResult<impl IntoResponse> {
    Ok(Json(c.store.channels(q.project.as_deref())?))
}

async fn create(State(c): State<ChannelsCtx>, Json(b): Json<NewChannel>) -> ApiResult<impl IntoResponse> {
    Ok((StatusCode::CREATED, Json(c.store.create_channel(b)?)))
}

async fn one(State(c): State<ChannelsCtx>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    Ok(Json(c.store.channel(&id)?))
}

#[derive(Deserialize)]
struct AfterQuery {
    after: Option<i64>,
}

async fn messages(
    State(c): State<ChannelsCtx>,
    Path(id): Path<String>,
    Query(q): Query<AfterQuery>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(c.store.read_channel_messages(&id, q.after.unwrap_or(0))?))
}

#[derive(Deserialize)]
struct NewMessage {
    text: String,
    #[serde(default)]
    from: Option<String>,
}

async fn post_message(
    State(c): State<ChannelsCtx>,
    Path(id): Path<String>,
    Json(b): Json<NewMessage>,
) -> ApiResult<impl IntoResponse> {
    let from = b.from.unwrap_or_else(|| USER.to_string());
    let message = c.store.post_channel_message(&id, &from, &b.text)?;
    // Every attached client hears of it (FR-L12); no receiver is not an error.
    c.sessions.publish(Push::ChannelMessage { channel_id: id, message: message.clone() });
    Ok((StatusCode::CREATED, Json(message)))
}

#[derive(Deserialize, Default)]
struct ReadBody {
    #[serde(default)]
    seq: Option<i64>,
}

async fn mark_read(
    State(c): State<ChannelsCtx>,
    Path(id): Path<String>,
    body: Option<Json<ReadBody>>,
) -> ApiResult<impl IntoResponse> {
    let seq = body.and_then(|Json(b)| b.seq);
    Ok(Json(c.store.mark_channel_read(&id, seq)?))
}

#[derive(Deserialize)]
struct PreviewBody {
    text: String,
}

async fn preview(
    State(c): State<ChannelsCtx>,
    Path(id): Path<String>,
    Json(b): Json<PreviewBody>,
) -> ApiResult<impl IntoResponse> {
    let m = c.store.preview_channel_mentions(&id, &b.text)?;
    let reaches: Vec<_> = m.mentioned.iter().map(|a| json!({ "id": a.id, "name": a.name })).collect();
    Ok(Json(json!({ "reaches": reaches, "unknown": m.unknown })))
}
