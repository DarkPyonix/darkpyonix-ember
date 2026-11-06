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
//! | GET    | `/api/channels/{id}/feed` | → `[FeedItem]`, newest first (FR-T11) |
//! | POST   | `/api/channels/{id}/feed/{approval_id}/answer` | `{decision}` → 204 |
//! | POST   | `/api/channels/{id}/mentions/preview` | `{text}` → `{reaches: [{id, name}], unknown: [name]}` |
//!
//! The feed holds the pending approvals and the last-turn reports of the channel's member agents
//! (see [`super::feed`]). `decision` is `allow_once`, `allow_always` or `deny`; an approval that
//! is not pending in this channel is 404.
//!
//! A message from `user` with `@name` of a name that is not a member agent is refused 400 with
//! `code`/`reason` `not_a_member` and `unknown: [name]`; nothing is stored or pushed (FR-T13).
//! Resolved mentions are stored and returned as `mentions: [session_id]` (FR-T12).
//!
//! A person's message with `@name` mentions is then delivered to each mentioned agent through
//! A2A, once per agent, behind a header naming the person, channel name and channel id
//! (FR-T12). A refusal (A2A off server-wide or for the agent, FR-T6; an active teammate, FR-T7) is
//! posted back into the channel as a message from `system` (not written to the JSONL, FR-T16).
//! A message authored by an agent never triggers delivery.
//!
//! | POST   | `/api/channels/{id}/post` | `{text}` → 201 `Message` from the calling session (FR-T12) |
//!
//! `post` is how an agent answers a mention (`ember-a2a channel post <id> "text"`). It carries
//! the session's runtime token, not the server token; `auth.rs` exempts exactly this path and
//! the handler checks the token (401), that the session is a member and was mentioned in this
//! channel in the last 30 minutes (403), and at most 6 posts a minute per session and channel
//! (429). The session's own A2A switch is not consulted. The message never parses mentions.
//!
//! | GET    | `/api/channels/{id}/guests` | → `[{id, name, created_at, expires_at, revoked_at, allow_until}]` (FR-T17) |
//! | PUT    | `/api/channels/{id}/guests/{guest_id}/allow` | `{hours}` (whole, 1..=168; else 400 `invalid_hours`) → `{guest_id, allow_until, capped}` |
//! | DELETE | `/api/channels/{id}/guests/{guest_id}/allow` | → 204, the allowance ends at once |
//! | DELETE | `/api/channels/{id}/guests/{guest_id}` | → 204, the guest credential is revoked at once |
//!
//! `allow_until` is never beyond the guest credential's own expiry (`capped` says when it was cut).
//! The channel (`GET /api/channels`, `GET /api/channels/{id}`) lists its present `guests` as
//! `[{id, name, allow_until}]`, `allow_until` null when no allowance is in force.
//!
//! A posted message is also pushed as `channel_message` on `/api/push` (FR-L12).
//!
//! Every message carries `author_name` and `author_kind` besides `from` (FR-T15): `나`/`person`
//! for `user`, the member name/`agent` for a session id, the guest's display name/`guest` for
//! `guest:<id>`, and `system`/`system`. They are derived at read time, so an owner client renders
//! a guest without knowing guest ids.
//!
//! `from` defaults to `user`; otherwise it is a member agent's session id. Errors are
//! `{error, code}` with `code` one of `not_found`, `bad_request`, `forbidden`, `internal`.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
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
    /// The human-message JSONL file (FR-T16); `None` writes no file (routers built without a data dir).
    pub jsonl: Option<Arc<super::jsonl::JsonlLog>>,
    /// Delivers a person's mentions to the mentioned agents (FR-T12); `None` delivers nothing.
    pub a2a: Option<Arc<crate::a2a::A2a>>,
}

pub fn router(store: Arc<Store>, sessions: Arc<Sessions>) -> Router {
    build(ChannelsCtx { store, sessions, jsonl: None, a2a: None })
}

/// As [`router`], and human messages are also appended under `<data_dir>/channels/` (FR-T16).
pub fn router_with_dir(store: Arc<Store>, sessions: Arc<Sessions>, data_dir: &std::path::Path) -> Router {
    build(ChannelsCtx { store, sessions, jsonl: Some(Arc::new(super::jsonl::JsonlLog::new(data_dir))), a2a: None })
}

/// As [`router_with_dir`], and a person's `@name` mentions are delivered to the mentioned agents
/// through `a2a` (FR-T12, FR-T13).
pub fn router_with_a2a(
    store: Arc<Store>,
    sessions: Arc<Sessions>,
    data_dir: &std::path::Path,
    a2a: Arc<crate::a2a::A2a>,
) -> Router {
    build(ChannelsCtx {
        store,
        sessions,
        jsonl: Some(Arc::new(super::jsonl::JsonlLog::new(data_dir))),
        a2a: Some(a2a),
    })
}

fn build(ctx: ChannelsCtx) -> Router {
    Router::new()
        .route("/api/channels", get(list).post(create))
        .route("/api/channels/{id}", get(one))
        .route("/api/channels/{id}/messages", get(messages).post(post_message))
        .route("/api/channels/{id}/post", post(agent_post))
        .route("/api/channels/{id}/read", post(mark_read))
        .route("/api/channels/{id}/mentions/preview", post(preview))
        .route("/api/channels/{id}/feed", get(feed))
        .route("/api/channels/{id}/feed/{approval_id}/answer", post(answer_feed))
        .route("/api/channels/{id}/invites", get(list_invites).post(create_invite))
        .route("/api/channels/{id}/invites/{invite_id}", delete(revoke_invite))
        .route("/api/channels/{id}/guests", get(list_guests))
        .route("/api/channels/{id}/guests/{guest_id}", delete(revoke_guest))
        .route("/api/channels/{id}/guests/{guest_id}/allow", put(set_allowance).delete(clear_allowance))
        .with_state(ctx)
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
            ChannelError::Coded(code, _) => {
                let status = if matches!(*code, "rate_limited" | "quota_exceeded") {
                    StatusCode::TOO_MANY_REQUESTS
                } else {
                    StatusCode::BAD_REQUEST
                };
                (status, *code)
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
    let message = append_and_publish(&c, &id, &from, &b.text)?;
    deliver_mentions(&c, &id, &message);
    Ok((StatusCode::CREATED, Json(message)))
}

/// Store a message, append it to the JSONL file and push it (FR-T16, FR-L12).
fn append_and_publish(c: &ChannelsCtx, id: &str, from: &str, text: &str) -> ApiResult<super::Message> {
    persist_and_publish(&c.store, &c.sessions, c.jsonl.as_deref(), id, || c.store.post_channel_message(id, from, text))
}

/// Run `insert` (which stores the row), append the line, then push (FR-T16, FR-L12). Insert and
/// append happen under the log's lock so lines reach the file in seq order; if the append fails
/// the row is deleted and the caller gets 500. Shared by the owner and the guest routes.
pub(super) fn persist_and_publish(
    store: &Store,
    sessions: &Sessions,
    jsonl: Option<&super::jsonl::JsonlLog>,
    id: &str,
    insert: impl FnOnce() -> ApiResult<super::Message>,
) -> ApiResult<super::Message> {
    let _guard = jsonl.map(|j| j.guard());
    let message = insert()?;
    if let Some(j) = jsonl {
        if let Err(e) = j.append(id, &message) {
            let _ = store.delete_channel_message(message.seq);
            return Err(match e {
                ChannelError::Other(_) => e,
                other => ChannelError::Other(anyhow::anyhow!("{other}")),
            });
        }
    }
    // Every attached client hears of it (FR-L12); no receiver is not an error.
    sessions.publish(Push::ChannelMessage { channel_id: id.to_string(), message: message.clone() });
    Ok(message)
}

#[derive(Deserialize)]
struct AgentPost {
    text: String,
}

/// `POST /api/channels/{id}/post`: an agent answers in a channel (FR-T12). The bearer is the
/// session's runtime token, which `auth.rs` lets through for exactly this path, so the handler
/// is the only check: token (401), member and recent mention (403), rate limit (429). The
/// message is authored by the session, parses no mentions and delivers to no one, so agents
/// cannot start a chain through a channel.
async fn agent_post(
    State(c): State<ChannelsCtx>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let Some(a2a) = c.a2a.clone() else {
        return ChannelError::NotFound("agent posting".into()).into_response();
    };
    let session = match crate::a2a::api::caller(&a2a, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let text = match serde_json::from_slice::<AgentPost>(&body) {
        Ok(b) if !b.text.trim().is_empty() => b.text,
        _ => return ChannelError::BadRequest("body must be {\"text\": \"...\"} with text".into()).into_response(),
    };
    if let Err(e) = a2a.check_channel_post(&session, &id) {
        return e.into_response();
    }
    match append_and_publish(&c, &id, &session, &text) {
        Ok(m) => (StatusCode::CREATED, Json(m)).into_response(),
        Err(e) => e.into_response(),
    }
}

/// Deliver a person's message to each mentioned agent (FR-T12). It runs after the message is
/// stored, logged and pushed, so a failure never un-posts it, and in a task per agent so the 201
/// does not wait for a sleeping agent to start. A message with no mention, or from an agent,
/// reaches no one. A refusal is written into the channel as a `system` message (FR-T13).
fn deliver_mentions(c: &ChannelsCtx, channel: &str, message: &super::Message) {
    let Some(a2a) = &c.a2a else { return };
    if message.from != USER || message.mentions.is_empty() {
        return;
    }
    let members = c.store.channel_members(channel).unwrap_or_default();
    for session_id in &message.mentions {
        let name = members
            .iter()
            .find(|m| &m.id == session_id)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| session_id.clone());
        let (c, a2a) = (c.clone(), a2a.clone());
        let (channel, session_id, text) = (channel.to_string(), session_id.clone(), message.text.clone());
        tokio::spawn(async move {
            let Err(e) = a2a.send_from_person(&channel, super::USER_LABEL, &session_id, &text).await else {
                return;
            };
            let note = format!("@{name} not reached: {e}");
            match c.store.post_channel_system_message(&channel, &note) {
                Ok(m) => c.sessions.publish(Push::ChannelMessage { channel_id: channel, message: m }),
                Err(err) => tracing::warn!(channel = %channel, "recording a delivery failure failed: {err:#}"),
            }
        });
    }
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

async fn feed(State(c): State<ChannelsCtx>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    Ok(Json(c.store.channel_feed(&id)?))
}

#[derive(Deserialize)]
struct FeedAnswer {
    decision: crate::events::ApprovalDecision,
}

/// Answer a member session's pending approval from the feed (FR-T11, FR-A5). The session records
/// the resolution and pushes its event, so the feed and the session clear together.
async fn answer_feed(
    State(c): State<ChannelsCtx>,
    Path((id, approval_id)): Path<(String, String)>,
    Json(b): Json<FeedAnswer>,
) -> ApiResult<impl IntoResponse> {
    let item = c.store.channel_feed_approval(&id, &approval_id)?;
    c.sessions
        .answer(&item.session_id, &approval_id, b.decision)
        .await
        .map_err(|e| ChannelError::Other(anyhow::anyhow!("{e}")))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Default)]
struct InviteBody {
    ttl_hours: Option<i64>,
}

/// FR-T14: the owner creates an invite; the token is in this answer only.
async fn create_invite(
    State(c): State<ChannelsCtx>,
    Path(id): Path<String>,
    body: Option<Json<InviteBody>>,
) -> ApiResult<impl IntoResponse> {
    let ttl = body.and_then(|Json(b)| b.ttl_hours);
    let (invite, token) = c.store.create_invite(&id, USER, ttl)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": invite.id,
            "token": token,
            "link": format!("/join#{token}"),
            "expires_at": invite.expires_at,
        })),
    ))
}

async fn list_invites(State(c): State<ChannelsCtx>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    Ok(Json(c.store.list_invites(&id)?))
}

async fn revoke_invite(
    State(c): State<ChannelsCtx>,
    Path((id, invite_id)): Path<(String, String)>,
) -> ApiResult<impl IntoResponse> {
    c.store.revoke_invite(&id, &invite_id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_guests(State(c): State<ChannelsCtx>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    Ok(Json(c.store.list_guests(&id)?))
}

/// FR-T17: allow one guest's mentions for the next `hours` whole hours. The body is read by hand
/// so that a missing, malformed or non-integer `hours` is one `invalid_hours` error.
async fn set_allowance(
    State(c): State<ChannelsCtx>,
    Path((id, guest_id)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> ApiResult<impl IntoResponse> {
    let parsed: Option<serde_json::Value> = serde_json::from_slice(&body).ok();
    let hours = super::allowance::check_hours(parsed.as_ref().and_then(|v| v.get("hours")))?;
    let a = c.store.set_guest_allowance(&id, &guest_id, hours)?;
    Ok(Json(json!({ "guest_id": guest_id, "allow_until": a.allow_until, "capped": a.capped })))
}

async fn clear_allowance(
    State(c): State<ChannelsCtx>,
    Path((id, guest_id)): Path<(String, String)>,
) -> ApiResult<impl IntoResponse> {
    c.store.clear_guest_allowance(&id, &guest_id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_guest(
    State(c): State<ChannelsCtx>,
    Path((id, guest_id)): Path<(String, String)>,
) -> ApiResult<impl IntoResponse> {
    c.store.revoke_guest(&id, &guest_id)?;
    Ok(StatusCode::NO_CONTENT)
}
