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
//! A guest posts as author `guest:<guest_id>` in the log, forced by the server: the body is `{text}` and a
//! `from` in it is ignored. `@name` mentions go through the member gate (an unknown name is 400)
//! and are stored. Text is 1..=8192 bytes after trimming for the emptiness check.
//!
//! Delivery (FR-T15, FR-T17): a guest's mention reaches a member agent only while the owner's
//! allowance is in force, checked against the database at delivery time ([`deliver_guest_mentions_at`]):
//! the guest is not revoked and `allow_until > now`, so expiry needs no owner action. Without an
//! allowance nothing is sent, no `a2a_messages` row is made, and one `system` note is posted for
//! the message (`guest @x's mention to @a, @b was not delivered (no active allowance)`; stored
//! and pushed, not in the JSONL). With one, each mentioned agent gets the text once through
//! [`crate::a2a::A2a::send_from_guest`], inside an untrusted block ([`crate::a2a::channel::guest_message_text`]).
//! A router built without A2A ([`guest_router`], [`guest_router_with_dir`]) delivers nothing and
//! posts no note.
//!
//! A guest sees the channel's whole history, including what was said before it joined (the
//! Discord and Slack model) [provisional]. Other guests appear as `author: "guest"` with their
//! display name, never their id; the caller's own messages carry `you: true`.
//!
//! Limits [provisional]: 10 posts per minute per guest (429 `rate_limited`, in memory) and 500
//! messages per guest per channel (429 `quota_exceeded`). The accept attempt limit per remote
//! address belongs to the guest listener (PR 5), which knows the address.
//!
//! A guest sees names only: the channel name, member and agent names, and per message a kind, the
//! author's display name and the mentioned names. Session ids, harnesses and the project's
//! working directory never appear.
//!
//! A guest's post is also appended to the channel's JSONL file when the router is built with a data
//! dir ([`guest_router_with_dir`], FR-T16): `from` is `guest:<id>` and the line gains `name`.
//!
//! [`guest_router`] is built apart from the owner router. The only socket that serves it is the
//! guest door (`EMBER_GUEST_LISTEN`, [`crate::guest_door`], off by default), wrapped by the
//! door's Host and Origin guard and never by `auth::protect`.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;

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
use crate::a2a::channel::sanitize_label;
use super::{mention, ChannelError, SYSTEM, USER, USER_LABEL};
use crate::session::Sessions;
use crate::store::{now_ms, Store};

/// Every (method, path pattern) the guest router serves. The guest door serves these and
/// `GET /join`, nothing else; `fr_t14_guest_door_serves_exactly_the_route_table` probes the real
/// router against this table, so a route added to [`build`] without a row here fails that test.
pub const GUEST_ROUTES: &[(&str, &str)] = &[
    ("POST", "/api/guest/accept"),
    ("GET", "/api/guest/channels/{id}"),
    ("GET", "/api/guest/channels/{id}/messages"),
    ("POST", "/api/guest/channels/{id}/messages"),
];

/// Longest message text a guest may post, in bytes.
pub const MAX_TEXT_BYTES: usize = 8192;
/// Request body cap on the guest routes (a little above the text cap, for the JSON frame).
const MAX_BODY_BYTES: usize = 16 * 1024;
/// Messages returned by one read.
const READ_LIMIT: i64 = 500;
/// Author prefix of a guest's messages.
pub const AUTHOR_PREFIX: &str = "guest:";

/// Posts one guest may make per minute (FR-T14) [provisional].
pub const POSTS_PER_MINUTE: usize = 10;
/// Messages one guest may have in one channel (FR-T14) [provisional].
pub const MESSAGE_CAP: i64 = 500;
const RATE_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// A message as a guest sees it. `author` is `user`, `system`, `agent` or `guest`: an id is never
/// shown, neither an agent's nor another guest's; `name` is the display name and `you` is true
/// for the caller's own messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GuestMessage {
    pub id: String,
    pub seq: i64,
    pub author: String,
    pub name: String,
    /// True when the caller wrote it.
    pub you: bool,
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
        raw.into_iter().map(|m| self.guest_view(guest, &members, m)).collect()
    }

    fn guest_view(&self, viewer: &Guest, members: &[super::Member], m: super::Message) -> Result<GuestMessage, ChannelError> {
        let name_of = |id: &str| members.iter().find(|x| x.id == id).map(|x| x.name.clone());
        let mentions = m.mentions.iter().filter_map(|id| name_of(id)).collect();
        let mut you = false;
        let (author, name, kind) = if m.from == USER {
            (USER.to_string(), USER_LABEL.to_string(), "person")
        } else if m.from == SYSTEM {
            (SYSTEM.to_string(), SYSTEM.to_string(), "system")
        } else if let Some(gid) = m.from.strip_prefix(AUTHOR_PREFIX) {
            let name: Option<String> = self
                .conn()
                .query_row(
                    "SELECT name FROM channel_guests WHERE id = ?1 AND channel_id = ?2",
                    params![gid, viewer.channel_id],
                    |r| r.get(0),
                )
                .ok();
            you = gid == viewer.id;
            ("guest".to_string(), name.unwrap_or_else(|| "guest".into()), "guest")
        } else {
            ("agent".to_string(), name_of(&m.from).unwrap_or_else(|| "agent".into()), "agent")
        };
        Ok(GuestMessage { id: m.id, seq: m.seq, author, name, you, kind: kind.into(), text: m.text, at: m.at, mentions })
    }

    /// How many messages `guest` has in its channel.
    pub fn guest_message_count(&self, guest: &Guest) -> Result<i64, ChannelError> {
        Ok(self.conn().query_row(
            "SELECT COUNT(*) FROM channel_messages WHERE channel_id = ?1 AND author = ?2",
            params![guest.channel_id, format!("{AUTHOR_PREFIX}{}", guest.id)],
            |r| r.get(0),
        )?)
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
        let mut message = super::msg(conn.last_insert_rowid(), from, text.to_string(), at, 0, &mentions);
        message.author_name = guest.name.clone();
        Ok(message)
    }
}

#[derive(Clone)]
struct Ctx {
    store: Arc<Store>,
    sessions: Arc<Sessions>,
    /// Recent post times per guest id, for the rate limit (in memory, like the A2A channel post).
    posts: Arc<std::sync::Mutex<HashMap<String, VecDeque<Instant>>>>,
    /// The human-message JSONL file (FR-T16); a guest is a human, so its posts are written too.
    jsonl: Option<Arc<super::jsonl::JsonlLog>>,
    /// Delivers a guest's mentions under an allowance (FR-T17); `None` delivers nothing.
    a2a: Option<Arc<crate::a2a::A2a>>,
}

impl Ctx {
    /// Record a post attempt by `guest`; an error when it is the 11th within a minute.
    fn rate_check(&self, guest: &str) -> Result<(), ChannelError> {
        let now = Instant::now();
        let mut all = self.posts.lock().unwrap_or_else(|e| e.into_inner());
        all.retain(|_, q| q.back().is_some_and(|t| now.duration_since(*t) < RATE_WINDOW));
        let q = all.entry(guest.to_string()).or_default();
        while q.front().is_some_and(|t| now.duration_since(*t) >= RATE_WINDOW) {
            q.pop_front();
        }
        if q.len() >= POSTS_PER_MINUTE {
            return Err(ChannelError::Coded("rate_limited", format!("at most {POSTS_PER_MINUTE} posts per minute")));
        }
        q.push_back(now);
        Ok(())
    }
}

/// The guest router: `accept` without a credential, the channel routes behind the guest check.
/// Separate from the owner router; the caller decides where it is served.
pub fn guest_router(store: Arc<Store>, sessions: Arc<Sessions>) -> Router {
    build(store, sessions, None, None)
}

/// As [`guest_router`], and a guest's posts are also appended under `<data_dir>/channels/`
/// (FR-T16), with the same insert-append-rollback as the owner route.
pub fn guest_router_with_dir(store: Arc<Store>, sessions: Arc<Sessions>, data_dir: &std::path::Path) -> Router {
    build(store, sessions, Some(Arc::new(super::jsonl::JsonlLog::new(data_dir))), None)
}

/// As [`guest_router_with_dir`], and a guest's `@name` mentions are delivered to the mentioned
/// agents through `a2a` while the owner's allowance is in force (FR-T15, FR-T17).
pub fn guest_router_with_a2a(
    store: Arc<Store>,
    sessions: Arc<Sessions>,
    data_dir: &std::path::Path,
    a2a: Arc<crate::a2a::A2a>,
) -> Router {
    build(store, sessions, Some(Arc::new(super::jsonl::JsonlLog::new(data_dir))), Some(a2a))
}

fn build(
    store: Arc<Store>,
    sessions: Arc<Sessions>,
    jsonl: Option<Arc<super::jsonl::JsonlLog>>,
    a2a: Option<Arc<crate::a2a::A2a>>,
) -> Router {
    let ctx = Ctx { store: store.clone(), sessions, posts: Arc::default(), jsonl, a2a };
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
    if let Err(e) = c.rate_check(&guest.id) {
        return fail(e);
    }
    match c.store.guest_message_count(&guest) {
        Ok(n) if n >= MESSAGE_CAP => {
            return fail(ChannelError::Coded(
                "quota_exceeded",
                format!("a guest may post at most {MESSAGE_CAP} messages in a channel"),
            ))
        }
        Ok(_) => {}
        Err(e) => return fail(e),
    }
    // Row, JSONL line and push as for the owner route (FR-T16, FR-L12); a failed append rolls the
    // row back and is 500. Delivery to agents runs after, only under an allowance (FR-T17).
    let message = match super::api::persist_and_publish(&c.store, &c.sessions, c.jsonl.as_deref(), &guest.channel_id, || {
        c.store.post_guest_message(&guest, &b.text)
    }) {
        Ok(m) => m,
        Err(e) => return fail(e),
    };
    if let Some(a2a) = c.a2a.clone().filter(|_| !message.mentions.is_empty()) {
        let (store, sessions, guest_id, msg) = (c.store.clone(), c.sessions.clone(), guest.id.clone(), message.clone());
        tokio::spawn(async move { deliver_guest_mentions(&store, &sessions, &a2a, &guest_id, &msg).await });
    }
    let view = c
        .store
        .channel_members(&guest.channel_id)
        .and_then(|m| c.store.guest_view(&guest, &m, message));
    match view {
        Ok(v) => (StatusCode::CREATED, Json(v)).into_response(),
        Err(e) => fail(e),
    }
}

/// Deliver a guest's stored `message` to each mentioned agent, if the guest's allowance is in
/// force now (FR-T17). See [`deliver_guest_mentions_at`].
pub async fn deliver_guest_mentions(
    store: &Store,
    sessions: &Sessions,
    a2a: &Arc<crate::a2a::A2a>,
    guest_id: &str,
    message: &super::Message,
) {
    deliver_guest_mentions_at(store, sessions, a2a, guest_id, message, now_ms()).await
}

/// As [`deliver_guest_mentions`] with the clock given. The allowance is read from the database
/// here, at delivery time, so an expiry or a revocation after the post counts. With none in
/// force nothing is stored in the A2A log and one `system` note names the mentioned agents. With
/// one, each mentioned agent gets the text once, through the same switches as a person's
/// mention; a refusal is posted as `@<agent> not reached: <reason>`. A mention of a session that
/// is not a member agent of the channel is skipped (the post route already refuses one; this
/// keeps delivery safe on its own).
///
/// Limits (FR-T17): at most [`MAX_MENTION_TARGETS`] distinct agents per message (one note says
/// `delivered to the first 3 of N mentions`), and at most [`DELIVERIES_PER_HOUR`] deliveries per
/// guest per hour (one note per message for the rest). The post itself is not affected.
pub async fn deliver_guest_mentions_at(
    store: &Store,
    sessions: &Sessions,
    a2a: &Arc<crate::a2a::A2a>,
    guest_id: &str,
    message: &super::Message,
    now: i64,
) {
    deliver_guest_mentions_hooked(store, sessions, a2a, guest_id, message, now, &|_| {}).await
}

/// As [`deliver_guest_mentions_at`], calling `before_send(i)` just before the allowance is
/// re-read for the `i`-th send (0-based). A test uses it to revoke between two sends. The
/// allowance, the revocation and the expiry are re-checked immediately before EACH send, with the
/// clock `now` advanced by the time spent, so a revoke or an expiry between two sends stops the
/// later ones (one note says so).
pub async fn deliver_guest_mentions_hooked(
    store: &Store,
    sessions: &Sessions,
    a2a: &Arc<crate::a2a::A2a>,
    guest_id: &str,
    message: &super::Message,
    now: i64,
    before_send: &(dyn Fn(usize) + Sync),
) {
    if message.mentions.is_empty() || message.from != format!("{AUTHOR_PREFIX}{guest_id}") {
        return;
    }
    let Some(channel) = store.guest_channel_of(guest_id) else { return };
    let members = store.channel_members(&channel).unwrap_or_default();
    let mut targets: Vec<(String, String)> = Vec::new();
    for id in &message.mentions {
        if let Some(m) = members.iter().find(|m| m.is_agent && &m.id == id) {
            if !targets.iter().any(|(t, _)| t == &m.id) {
                targets.push((m.id.clone(), m.name.clone()));
            }
        }
    }
    if targets.is_empty() {
        return;
    }
    let guest_name = sanitize_label(&store.guest_name(guest_id).unwrap_or_else(|| "guest".into()));
    let handle = |n: &str| format!("@{}", sanitize_label(n));
    if store.guest_allowance_at(guest_id, now).is_none() {
        let names: Vec<String> = targets.iter().map(|(_, n)| handle(n)).collect();
        let note = format!(
            "guest @{guest_name}'s mention to {} was not delivered (no active allowance)",
            names.join(", ")
        );
        post_note(store, sessions, &channel, &note);
        return;
    }
    let total = targets.len();
    if total > MAX_MENTION_TARGETS {
        targets.truncate(MAX_MENTION_TARGETS);
        post_note(
            store,
            sessions,
            &channel,
            &format!("guest @{guest_name}'s message was delivered to the first {MAX_MENTION_TARGETS} of {total} mentions"),
        );
    }
    let started = Instant::now();
    for (i, (session_id, name)) in targets.iter().enumerate() {
        before_send(i);
        // Re-read the allowance now: a revoke or an expiry since the last send ends delivery.
        let at = now + started.elapsed().as_millis() as i64;
        let Some(until) = store.guest_allowance_at(guest_id, at) else {
            let rest: Vec<String> = targets[i..].iter().map(|(_, n)| handle(n)).collect();
            post_note(
                store,
                sessions,
                &channel,
                &format!("guest @{guest_name}'s mention to {} was not delivered (the allowance ended)", rest.join(", ")),
            );
            return;
        };
        if !delivery_slot(guest_id, at) {
            let rest: Vec<String> = targets[i..].iter().map(|(_, n)| handle(n)).collect();
            post_note(
                store,
                sessions,
                &channel,
                &format!(
                    "guest @{guest_name}'s mention to {} was not delivered (at most {DELIVERIES_PER_HOUR} deliveries an hour per guest)",
                    rest.join(", ")
                ),
            );
            return;
        }
        let sent = a2a.send_from_guest(&channel, &guest_name, until, session_id, &message.text).await;
        if let Err(e) = sent {
            post_note(store, sessions, &channel, &format!("{} not reached: {e}", handle(name)));
        }
    }
}

/// Most distinct agents one guest message is delivered to.
pub const MAX_MENTION_TARGETS: usize = 3;
/// Most deliveries to agents per guest in any hour (in memory, lost on restart like the post limiter).
pub const DELIVERIES_PER_HOUR: usize = 20;
const DELIVERY_WINDOW_MS: i64 = 3_600_000;

static DELIVERIES: std::sync::Mutex<Option<HashMap<String, VecDeque<i64>>>> = std::sync::Mutex::new(None);

/// Take one delivery slot for `guest` at `now` (Unix ms); false when the guest already had
/// [`DELIVERIES_PER_HOUR`] in the last hour.
fn delivery_slot(guest: &str, now: i64) -> bool {
    let mut g = DELIVERIES.lock().unwrap_or_else(|e| e.into_inner());
    let all = g.get_or_insert_with(HashMap::new);
    all.retain(|_, q| q.back().is_some_and(|t| now - *t < DELIVERY_WINDOW_MS));
    let q = all.entry(guest.to_string()).or_default();
    while q.front().is_some_and(|t| now - *t >= DELIVERY_WINDOW_MS) {
        q.pop_front();
    }
    if q.len() >= DELIVERIES_PER_HOUR {
        return false;
    }
    q.push_back(now);
    true
}

/// Post a `system` note into the channel and push it (not written to the JSONL, FR-T16).
fn post_note(store: &Store, sessions: &Sessions, channel: &str, note: &str) {
    match store.post_channel_system_message(channel, note) {
        Ok(m) => sessions.publish(crate::session::Push::ChannelMessage { channel_id: channel.to_string(), message: m }),
        Err(err) => tracing::warn!(channel = %channel, "recording a guest delivery note failed: {err:#}"),
    }
}

impl Store {
    /// The channel a guest belongs to.
    fn guest_channel_of(&self, guest_id: &str) -> Option<String> {
        self.conn()
            .query_row("SELECT channel_id FROM channel_guests WHERE id = ?1", params![guest_id], |r| r.get(0))
            .ok()
    }

    /// A guest's display name.
    fn guest_name(&self, guest_id: &str) -> Option<String> {
        self.conn()
            .query_row("SELECT name FROM channel_guests WHERE id = ?1", params![guest_id], |r| r.get(0))
            .ok()
    }
}
