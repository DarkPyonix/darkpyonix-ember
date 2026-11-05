//! The guest routes of a channel (SPEC FR-T14, FR-T15) [provisional].
//!
//! | Method | Path | Body → Response |
//! | ------ | ---- | --------------- |
//! | POST   | `/api/guest/accept` | `{token, name}` → 201 `{credential, channel_id, guest_id, expires_at}` |
//! | GET    | `/api/guest/channels/{id}` | → `{id, name, members: [{name, is_agent}], you}` |
//! | GET    | `/api/guest/channels/{id}/messages?after=` | → `[GuestMessage]`, oldest first |
//! | POST   | `/api/guest/channels/{id}/messages` | `{text}` → 201 `GuestMessage` |
//!
//! Authentication: `accept` needs no header (the invite token in the body is the secret, and it
//! works once). Every other route needs `Authorization: Bearer dpg_...`, a guest credential, checked
//! against the store on every request ([`guest_from_headers`]); anything else, including the
//! server token, is 401 `{error, code}`. The credential is bound to one channel: another channel
//! id is 404. There is no route for the feed, approvals, sessions or any other part of the
//! server, so those paths are 404 here by absence.
//!
//! A guest posts as author `guest:<guest_id>`, forced by the server: the body is `{text}` and a
//! `from` in it is ignored. `@name` mentions go through the member gate (an unknown name is 400)
//! and are stored, but no delivery runs here (the allowance, FR-T17, is not built), so a guest's
//! mention reaches no agent. Text is 1..=8192 bytes after trimming for the emptiness check.
//!
//! A guest sees names only: the channel name, member and agent names, and per message a kind, the
//! author's display name and the mentioned names. Session ids, harnesses and the project's
//! working directory never appear.
//!
//! [`guest_router`] is built apart from the owner router and is not merged into any listener yet
//! (the guest listener `EMBER_GUEST_LISTEN` is a later step), so nothing here is reachable.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::invites::{Guest, CREDENTIAL_PREFIX};
use super::{mention, ChannelError, SYSTEM, USER, USER_LABEL};
use crate::session::{Push, Sessions};
use crate::store::{now_ms, Store};

/// Longest message text a guest may post, in bytes.
pub const MAX_TEXT_BYTES: usize = 8192;
/// Request body cap on the guest routes (a little above the text cap, for the JSON frame).
const MAX_BODY_BYTES: usize = 16 * 1024;
/// Messages returned by one read.
const READ_LIMIT: i64 = 500;
/// Author prefix of a guest's messages.
pub const AUTHOR_PREFIX: &str = "guest:";

/// A message as a guest sees it. `author` is `user`, `system`, `agent` (the id is withheld) or
/// `guest:<guest_id>`; `name` is the display name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GuestMessage {
    pub id: String,
    pub seq: i64,
    pub author: String,
    pub name: String,
    /// `person`, `agent`, `guest` or `system`.
    pub kind: String,
    pub text: String,
    pub at: i64,
    /// Names of the agents the message mentions.
    pub mentions: Vec<String>,
}

impl Store {
    /// The channel as a guest sees it: names only.
    pub fn guest_channel_summary(&self, guest: &Guest) -> Result<serde_json::Value, ChannelError> {
        let row = self.channel_row(&guest.channel_id)?;
        let members: Vec<_> = self
            .members_of(&row)?
            .into_iter()
            .map(|m| json!({ "name": m.name, "is_agent": m.is_agent }))
            .collect();
        Ok(json!({ "id": row.id, "name": row.name, "members": members, "you": { "id": guest.id, "name": guest.name } }))
    }

    /// Messages after `after` in the guest's channel, mapped to what a guest may see.
    pub fn guest_messages(&self, guest: &Guest, after: i64) -> Result<Vec<GuestMessage>, ChannelError> {
        let row = self.channel_row(&guest.channel_id)?;
        let members = self.members_of(&row)?;
        let raw = self.channel_messages_after(&guest.channel_id, after, Some(READ_LIMIT), false)?;
        raw.into_iter().map(|m| self.guest_view(&guest.channel_id, &members, m)).collect()
    }

    fn guest_view(&self, channel: &str, members: &[super::Member], m: super::Message) -> Result<GuestMessage, ChannelError> {
        let name_of = |id: &str| members.iter().find(|x| x.id == id).map(|x| x.name.clone());
        let mentions = m.mentions.iter().filter_map(|id| name_of(id)).collect();
        let (author, name, kind) = if m.from == USER {
            (USER.to_string(), USER_LABEL.to_string(), "person")
        } else if m.from == SYSTEM {
            (SYSTEM.to_string(), SYSTEM.to_string(), "system")
        } else if let Some(gid) = m.from.strip_prefix(AUTHOR_PREFIX) {
            let name: Option<String> = self
                .conn()
                .query_row(
                    "SELECT name FROM channel_guests WHERE id = ?1 AND channel_id = ?2",
                    params![gid, channel],
                    |r| r.get(0),
                )
                .ok();
            (m.from.clone(), name.unwrap_or_else(|| "guest".into()), "guest")
        } else {
            ("agent".to_string(), name_of(&m.from).unwrap_or_else(|| "agent".into()), "agent")
        };
        Ok(GuestMessage { id: m.id, seq: m.seq, author, name, kind: kind.into(), text: m.text, at: m.at, mentions })
    }

    /// Store a guest's message under `guest:<id>`. Mentions go through the member gate (FR-T13)
    /// and are stored; nothing is delivered.
    pub fn post_guest_message(&self, guest: &Guest, text: &str) -> Result<super::Message, ChannelError> {
        if text.trim().is_empty() {
            return Err(ChannelError::BadRequest("message text must not be empty".into()));
        }
        if text.len() > MAX_TEXT_BYTES {
            return Err(ChannelError::BadRequest(format!("message text must be at most {MAX_TEXT_BYTES} bytes")));
        }
        let row = self.channel_row(&guest.channel_id)?;
        let found = mention::parse_mentions(text, &self.members_of(&row)?);
        if !found.unknown.is_empty() {
            return Err(ChannelError::NotAMember(found.unknown));
        }
        let mentions: Vec<String> = found.mentioned.into_iter().map(|m| m.id).collect();
        let mentions = serde_json::to_string(&mentions).map_err(anyhow::Error::from)?;
        let from = format!("{AUTHOR_PREFIX}{}", guest.id);
        let at = now_ms();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO channel_messages (channel_id, author, body, at, mentions) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![guest.channel_id, from, text, at, mentions],
        )?;
        Ok(super::msg(conn.last_insert_rowid(), from, text.to_string(), at, 0, &mentions))
    }
}

#[derive(Clone)]
struct Ctx {
    store: Arc<Store>,
    sessions: Arc<Sessions>,
}

/// The guest router: `accept` without a credential, the channel routes behind the guest check.
/// Separate from the owner router; the caller decides where it is served.
pub fn guest_router(store: Arc<Store>, sessions: Arc<Sessions>) -> Router {
    let ctx = Ctx { store: store.clone(), sessions };
    let protected = Router::new()
        .route("/api/guest/channels/{id}", get(summary))
        .route("/api/guest/channels/{id}/messages", get(messages).post(post_message))
        .route_layer(middleware::from_fn_with_state(store, require_guest))
        .with_state(ctx.clone());
    Router::new()
        .route("/api/guest/accept", post(accept))
        .with_state(ctx)
        .merge(protected)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
}

/// The guest a request's `Authorization: Bearer dpg_...` header names, if the credential is valid
/// now. The server token, a missing or malformed header, an unknown, expired or revoked
/// credential, and a revoked invite all give `None`.
pub fn guest_from_headers(headers: &HeaderMap, store: &Store) -> Option<Guest> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let credential = value.strip_prefix("Bearer ")?.trim();
    if !credential.starts_with(CREDENTIAL_PREFIX) {
        return None;
    }
    store.guest_by_credential(credential)
}

fn unauthorized() -> Response {
    let mut r = (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "missing or invalid guest credential", "code": "unauthorized" })),
    )
        .into_response();
    r.headers_mut().insert(header::WWW_AUTHENTICATE, header::HeaderValue::from_static("Bearer"));
    r
}

async fn require_guest(State(store): State<Arc<Store>>, mut req: Request<Body>, next: Next) -> Response {
    match guest_from_headers(req.headers(), &store) {
        Some(guest) => {
            req.extensions_mut().insert(guest);
            next.run(req).await
        }
        None => unauthorized(),
    }
}

/// A channel error as a guest may see it: an internal error carries no detail.
fn fail(e: ChannelError) -> Response {
    match e {
        ChannelError::Other(err) => {
            tracing::warn!("guest route failed: {err:#}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "internal error", "code": "internal" })))
                .into_response()
        }
        other => other.into_response(),
    }
}

/// The guest may act on its own channel only; anything else is 404, never 403.
fn own_channel(guest: &Guest, id: &str) -> Result<(), Response> {
    if guest.channel_id == id {
        Ok(())
    } else {
        Err(fail(ChannelError::NotFound("channel".into())))
    }
}

#[derive(Deserialize)]
struct AcceptBody {
    token: String,
    name: String,
}

async fn accept(State(c): State<Ctx>, Json(b): Json<AcceptBody>) -> Response {
    match c.store.accept_invite(&b.token, &b.name) {
        Ok((guest, credential)) => (
            StatusCode::CREATED,
            Json(json!({
                "credential": credential,
                "channel_id": guest.channel_id,
                "guest_id": guest.id,
                "expires_at": guest.expires_at,
            })),
        )
            .into_response(),
        Err(e) => fail(e),
    }
}

async fn summary(State(c): State<Ctx>, Extension(guest): Extension<Guest>, Path(id): Path<String>) -> Response {
    if let Err(r) = own_channel(&guest, &id) {
        return r;
    }
    match c.store.guest_channel_summary(&guest) {
        Ok(v) => Json(v).into_response(),
        Err(e) => fail(e),
    }
}

#[derive(Deserialize)]
struct AfterQuery {
    after: Option<i64>,
}

async fn messages(
    State(c): State<Ctx>,
    Extension(guest): Extension<Guest>,
    Path(id): Path<String>,
    Query(q): Query<AfterQuery>,
) -> Response {
    if let Err(r) = own_channel(&guest, &id) {
        return r;
    }
    match c.store.guest_messages(&guest, q.after.unwrap_or(0)) {
        Ok(v) => Json(v).into_response(),
        Err(e) => fail(e),
    }
}

/// Only `text` is read; any other field, `from` included, is ignored.
#[derive(Deserialize)]
struct PostBody {
    text: String,
}

async fn post_message(
    State(c): State<Ctx>,
    Extension(guest): Extension<Guest>,
    Path(id): Path<String>,
    Json(b): Json<PostBody>,
) -> Response {
    if let Err(r) = own_channel(&guest, &id) {
        return r;
    }
    let message = match c.store.post_guest_message(&guest, &b.text) {
        Ok(m) => m,
        Err(e) => return fail(e),
    };
    // The owner's clients hear of it (FR-L12). No delivery to agents runs (FR-T15, FR-T17).
    c.sessions.publish(Push::ChannelMessage { channel_id: guest.channel_id.clone(), message: message.clone() });
    let view = c
        .store
        .channel_members(&guest.channel_id)
        .and_then(|m| c.store.guest_view(&guest.channel_id, &m, message));
    match view {
        Ok(v) => (StatusCode::CREATED, Json(v)).into_response(),
        Err(e) => fail(e),
    }
}
