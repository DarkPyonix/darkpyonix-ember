//! A2A tables (SPEC FR-T2, FR-T4–FR-T6): runtime tokens, the durable message queue and the
//! on/off switches.
//!
//! They live in the same SQLite file as the session store but on their own connection, so the
//! session schema is untouched. Message rows double as the sliding-window history for loop
//! protection, so the limits also survive a restart.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
CREATE TABLE IF NOT EXISTS a2a_messages (
    id            TEXT PRIMARY KEY,
    from_session  TEXT NOT NULL,
    to_session    TEXT NOT NULL,
    reply_to      TEXT,
    text          TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    delivered_at  INTEGER
);
CREATE INDEX IF NOT EXISTS a2a_messages_pending ON a2a_messages (to_session, delivered_at);
CREATE INDEX IF NOT EXISTS a2a_messages_from ON a2a_messages (from_session, created_at);
CREATE TABLE IF NOT EXISTS a2a_terminal_grants (
    reader      TEXT NOT NULL,
    target      TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (reader, target)
);
CREATE TABLE IF NOT EXISTS a2a_switches (
    scope    TEXT PRIMARY KEY,
    enabled  INTEGER NOT NULL
);
";

/// Runtime tokens (FR-T2, FR-T2a). Keyed by the token's hash, so a session may hold more than one:
/// an agent rejoined after a server restart keeps the token in its environment while the rejoining
/// start mints one for the run's later processes. `last_seen_at` is the last authenticated use or
/// liveness refresh (idle lifetime); `expires_at` is the absolute end, pushed back only while the
/// session's agent run is mid-turn ([`A2aStore::hold_session_tokens`]).
const TOKENS_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS a2a_tokens (
    token_hash    TEXT PRIMARY KEY,
    session_id    TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    last_seen_at  INTEGER NOT NULL,
    expires_at    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS a2a_tokens_session ON a2a_tokens (session_id);
";

/// How long a runtime token lives (FR-T2a, #102). Server config: [`TokenTimes::from_env`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenTimes {
    /// A token unused for this long is refused, unless its session's agent run is alive: the
    /// liveness check refreshes it every [`TokenTimes::check`]. Default 24 h
    /// (`EMBER_A2A_TOKEN_IDLE_SECS`).
    pub idle: Duration,
    /// Absolute lifetime from minting. A run past it is released at its next idle moment, so the
    /// next start mints a fresh token; a run mid-turn keeps its token until the turn ends.
    /// Default 7 days (`EMBER_A2A_TOKEN_MAX_SECS`).
    pub max: Duration,
    /// How often live runs refresh their tokens and the absolute lifetime is applied. Default
    /// 60 s (`EMBER_A2A_TOKEN_CHECK_SECS`), never more than a third of `idle`.
    pub check: Duration,
}

impl Default for TokenTimes {
    fn default() -> TokenTimes {
        TokenTimes {
            idle: Duration::from_secs(24 * 60 * 60),
            max: Duration::from_secs(7 * 24 * 60 * 60),
            check: Duration::from_secs(60),
        }
    }
}

impl TokenTimes {
    /// The defaults with the `EMBER_A2A_TOKEN_*_SECS` overrides applied. A value that is not a
    /// positive integer is ignored.
    pub fn from_env() -> TokenTimes {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    pub(crate) fn from_lookup(get: impl Fn(&str) -> Option<String>) -> TokenTimes {
        let secs = |name: &str| {
            get(name).and_then(|v| v.trim().parse::<u64>().ok()).filter(|s| *s > 0).map(Duration::from_secs)
        };
        let d = TokenTimes::default();
        TokenTimes {
            idle: secs("EMBER_A2A_TOKEN_IDLE_SECS").unwrap_or(d.idle),
            max: secs("EMBER_A2A_TOKEN_MAX_SECS").unwrap_or(d.max),
            check: secs("EMBER_A2A_TOKEN_CHECK_SECS").unwrap_or(d.check),
        }
        .settled()
    }

    /// `check` at most a third of `idle` (and at least 10 ms), so a live run is refreshed several
    /// times within one idle lifetime even when one check is late.
    pub fn settled(mut self) -> TokenTimes {
        self.check = self.check.min(self.idle / 3).max(Duration::from_millis(10));
        self
    }

    fn idle_ms(&self) -> i64 {
        self.idle.as_millis().min(i64::MAX as u128) as i64
    }

    fn max_ms(&self) -> i64 {
        self.max.as_millis().min(i64::MAX as u128) as i64
    }
}

/// A session's tokens as the liveness check sees them: the oldest mint time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTokens {
    pub session_id: String,
    pub oldest_created_at: i64,
}

/// Columns added after the first release, as `(name, SQL type)`. Applied by [`migrate`] to a
/// database that predates them. `origin_channel` and `origin_label` mark a message that a person
/// typed in a channel (FR-T12): `from_session` then holds `channel:<channel id>`, which is never a
/// session id.
///
/// `origin_guest_until` marks the message as a guest's (FR-T17): the guest's allowance end in Unix
/// milliseconds, with `origin_label` holding the guest's display name.
///
/// `kind` is `report` for an FR-T8 report the server built (#995); `NULL` for every other message.
const ADDED_COLUMNS: &[(&str, &str)] = &[
    ("origin_channel", "TEXT"),
    ("origin_label", "TEXT"),
    ("origin_guest_until", "INTEGER"),
    ("kind", "TEXT"),
];

/// [`A2aMessage::kind`] of an FR-T8 report.
pub const KIND_REPORT: &str = "report";

/// Prefix of `from_session` for a message from a person in a channel.
pub const CHANNEL_SENDER_PREFIX: &str = "channel:";

const MESSAGE_COLUMNS: &str = "id, from_session, to_session, reply_to, text, created_at, delivered_at, \
     origin_channel, origin_label, origin_guest_until, kind";

/// Idempotent: rebuilds an `a2a_tokens` table of the one-token-per-session shape into
/// [`TOKENS_SCHEMA`], keeping its tokens (FR-T2a). A kept token counts as just seen and gets the
/// default absolute lifetime from now.
fn migrate_tokens(conn: &mut Connection) -> anyhow::Result<()> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let cols: Vec<String> = {
        let mut stmt = tx.prepare("PRAGMA table_info(a2a_tokens)")?;
        let names = stmt.query_map([], |r| r.get::<_, String>(1))?;
        names.collect::<rusqlite::Result<_>>()?
    };
    let old = !cols.is_empty() && !cols.iter().any(|c| c == "expires_at");
    if old {
        tx.execute_batch("ALTER TABLE a2a_tokens RENAME TO a2a_tokens_v1;")?;
    }
    tx.execute_batch(TOKENS_SCHEMA)?;
    if old {
        let now = now_ms();
        tx.execute(
            "INSERT OR IGNORE INTO a2a_tokens (token_hash, session_id, created_at, last_seen_at, expires_at)
             SELECT token_hash, session_id, created_at, ?1, ?2 FROM a2a_tokens_v1",
            params![now, now.saturating_add(TokenTimes::default().max_ms())],
        )?;
        tx.execute_batch("DROP TABLE a2a_tokens_v1;")?;
    }
    tx.commit()?;
    Ok(())
}

/// Idempotent: adds each missing column of [`ADDED_COLUMNS`] to `a2a_messages`.
fn migrate(conn: &Connection) -> anyhow::Result<()> {
    let mut have = Vec::new();
    {
        let mut stmt = conn.prepare("PRAGMA table_info(a2a_messages)")?;
        let names = stmt.query_map([], |r| r.get::<_, String>(1))?;
        for n in names {
            have.push(n?);
        }
    }
    for (name, ty) in ADDED_COLUMNS {
        if have.iter().any(|h| h == name) {
            continue;
        }
        if let Err(e) = conn.execute(&format!("ALTER TABLE a2a_messages ADD COLUMN {name} {ty}"), []) {
            // Another connection to the same file may have added it between check and ALTER.
            if !e.to_string().contains("duplicate column") {
                return Err(e.into());
            }
        }
    }
    Ok(())
}

const GLOBAL_SCOPE: &str = "global";

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn hash_token(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn session_scope(id: &str) -> String {
    format!("session:{id}")
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct A2aMessage {
    pub id: String,
    pub from_session: String,
    pub to_session: String,
    pub reply_to: Option<String>,
    pub text: String,
    pub created_at: i64,
    pub delivered_at: Option<i64>,
    /// The channel a person typed this message in; `None` for a message from a session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_channel: Option<String>,
    /// The person's name at send time (e.g. `나`); set with `origin_channel`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_label: Option<String>,
    /// Set for a message from a guest (FR-T17): the end of the allowance it was delivered under,
    /// Unix milliseconds. `origin_label` is then the guest's display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_guest_until: Option<i64>,
    /// `Some("report")` for an FR-T8 report built by the server (#995); `None` otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

impl A2aMessage {
    pub fn is_report(&self) -> bool {
        self.kind.as_deref() == Some(KIND_REPORT)
    }
}

pub struct A2aStore {
    conn: Mutex<Connection>,
    /// Token lifetimes (FR-T2a); [`TokenTimes::default`] until [`A2aStore::set_token_times`].
    times: Mutex<TokenTimes>,
}

impl A2aStore {
    /// Open the A2A tables in the server's database file (`ember.db`).
    pub fn open(path: &Path) -> anyhow::Result<A2aStore> {
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        migrate_tokens(&mut conn)?;
        Ok(A2aStore {
            conn: Mutex::new(conn),
            times: Mutex::new(TokenTimes::default()),
        })
    }

    pub fn open_in_memory() -> anyhow::Result<A2aStore> {
        let mut conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        migrate_tokens(&mut conn)?;
        Ok(A2aStore {
            conn: Mutex::new(conn),
            times: Mutex::new(TokenTimes::default()),
        })
    }

    /// Set the token lifetimes (FR-T2a). Applies to tokens minted afterwards (absolute) and to
    /// every check from now on (idle).
    pub fn set_token_times(&self, times: TokenTimes) {
        *self.times.lock().unwrap() = times.settled();
    }

    pub fn token_times(&self) -> TokenTimes {
        *self.times.lock().unwrap()
    }

    /// Issue a fresh runtime token for `session_id`, replacing (revoking) any earlier one. Only
    /// its hash is stored.
    pub fn issue_token(&self, session_id: &str) -> anyhow::Result<String> {
        self.issue_token_at(session_id, now_ms())
    }

    /// [`A2aStore::issue_token`] at the clock time `now` (Unix ms).
    pub fn issue_token_at(&self, session_id: &str, now: i64) -> anyhow::Result<String> {
        self.mint(session_id, now, true)
    }

    /// Issue a fresh token for `session_id` and keep its earlier ones (FR-T2a): for a start that
    /// may rejoin an agent which outlived the server and still holds an earlier token.
    pub fn issue_token_beside(&self, session_id: &str) -> anyhow::Result<String> {
        self.mint(session_id, now_ms(), false)
    }

    fn mint(&self, session_id: &str, now: i64, replace: bool) -> anyhow::Result<String> {
        let token = format!(
            "ember_rt_{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let expires_at = now.saturating_add(self.token_times().max_ms());
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        if replace {
            tx.execute("DELETE FROM a2a_tokens WHERE session_id = ?1", params![session_id])?;
        }
        tx.execute(
            "INSERT INTO a2a_tokens (token_hash, session_id, created_at, last_seen_at, expires_at)
             VALUES (?1, ?2, ?3, ?3, ?4)",
            params![hash_token(&token), session_id, now, expires_at],
        )?;
        tx.commit()?;
        Ok(token)
    }

    /// The session a runtime token belongs to, if the token is valid now. A use refreshes its
    /// idle lifetime.
    pub fn session_for_token(&self, token: &str) -> anyhow::Result<Option<String>> {
        self.session_for_token_at(token, now_ms())
    }

    /// [`A2aStore::session_for_token`] at the clock time `now` (Unix ms). A token past its
    /// absolute end, or unused for longer than the idle lifetime, is deleted and `None`.
    pub fn session_for_token_at(&self, token: &str, now: i64) -> anyhow::Result<Option<String>> {
        let idle = self.token_times().idle_ms();
        let hash = hash_token(token);
        let conn = self.conn.lock().unwrap();
        let row: Option<(String, i64, i64)> = conn
            .query_row(
                "SELECT session_id, last_seen_at, expires_at FROM a2a_tokens WHERE token_hash = ?1",
                params![hash],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((session, last_seen, expires_at)) = row else { return Ok(None) };
        if now > expires_at || now.saturating_sub(last_seen) > idle {
            conn.execute("DELETE FROM a2a_tokens WHERE token_hash = ?1", params![hash])?;
            return Ok(None);
        }
        if now > last_seen {
            conn.execute("UPDATE a2a_tokens SET last_seen_at = ?2 WHERE token_hash = ?1", params![hash, now])?;
        }
        Ok(Some(session))
    }

    /// Revoke every token of `session_id` (its agent run ended, or the session did). Returns how
    /// many were removed.
    pub fn revoke_session_tokens(&self, session_id: &str) -> anyhow::Result<usize> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .execute("DELETE FROM a2a_tokens WHERE session_id = ?1", params![session_id])?)
    }

    /// Revoke the tokens of every session not in `keep` (server start: only agents that
    /// outlived the previous server can hold one). Returns how many were removed.
    pub fn revoke_tokens_except(&self, keep: &[String]) -> anyhow::Result<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let mut sessions: Vec<String> = Vec::new();
        {
            let mut stmt = tx.prepare("SELECT DISTINCT session_id FROM a2a_tokens")?;
            for id in stmt.query_map([], |r| r.get::<_, String>(0))? {
                sessions.push(id?);
            }
        }
        let mut removed = 0;
        for id in sessions.iter().filter(|id| !keep.contains(id)) {
            removed += tx.execute("DELETE FROM a2a_tokens WHERE session_id = ?1", params![id])?;
        }
        tx.commit()?;
        Ok(removed)
    }

    /// Mark every token of `session_id` as seen at `now`: its agent run is alive (FR-T2a).
    pub fn touch_session_tokens(&self, session_id: &str, now: i64) -> anyhow::Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE a2a_tokens SET last_seen_at = ?2 WHERE session_id = ?1 AND last_seen_at < ?2",
            params![session_id, now],
        )?;
        Ok(())
    }

    /// Keep every token of `session_id` valid until at least `until` (Unix ms): its run is
    /// mid-turn past the absolute lifetime, and a turn is never cut off (FR-T2a).
    pub fn hold_session_tokens(&self, session_id: &str, until: i64) -> anyhow::Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE a2a_tokens SET expires_at = ?2 WHERE session_id = ?1 AND expires_at < ?2",
            params![session_id, until],
        )?;
        Ok(())
    }

    /// Every session that holds a token, with its oldest mint time.
    pub fn token_sessions(&self) -> anyhow::Result<Vec<SessionTokens>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT session_id, MIN(created_at) FROM a2a_tokens GROUP BY session_id")?;
        let rows = stmt.query_map([], |r| Ok(SessionTokens { session_id: r.get(0)?, oldest_created_at: r.get(1)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Delete the tokens that can no longer pass [`A2aStore::session_for_token_at`] at `now`.
    pub fn sweep_expired_tokens(&self, now: i64) -> anyhow::Result<usize> {
        let idle = self.token_times().idle_ms();
        Ok(self.conn.lock().unwrap().execute(
            "DELETE FROM a2a_tokens WHERE expires_at < ?1 OR last_seen_at < ?1 - ?2",
            params![now, idle],
        )?)
    }

    pub fn insert_message(
        &self,
        from: &str,
        to: &str,
        reply_to: Option<&str>,
        text: &str,
    ) -> anyhow::Result<A2aMessage> {
        let msg = A2aMessage {
            id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
            from_session: from.into(),
            to_session: to.into(),
            reply_to: reply_to.map(String::from),
            text: text.into(),
            created_at: now_ms(),
            delivered_at: None,
            origin_channel: None,
            origin_label: None,
            origin_guest_until: None,
            kind: None,
        };
        self.insert(&msg)?;
        Ok(msg)
    }

    /// Store an FR-T8 report from session `from` to its delegator `to` (#995).
    pub fn insert_report(&self, from: &str, to: &str, text: &str) -> anyhow::Result<A2aMessage> {
        let msg = A2aMessage {
            id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
            from_session: from.into(),
            to_session: to.into(),
            reply_to: None,
            text: text.into(),
            created_at: now_ms(),
            delivered_at: None,
            origin_channel: None,
            origin_label: None,
            origin_guest_until: None,
            kind: Some(KIND_REPORT.into()),
        };
        self.insert(&msg)?;
        Ok(msg)
    }

    /// Put delivered messages back in the queue (an FR-T8 report whose receiving turn failed).
    pub fn mark_undelivered(&self, ids: &[String]) -> anyhow::Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        for id in ids {
            tx.execute("UPDATE a2a_messages SET delivered_at = NULL WHERE id = ?1", params![id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Store a message a person typed in `channel_id` (FR-T12). `from_session` becomes
    /// `channel:<id>`, so it never collides with a session id in the loop-limit counts.
    pub fn insert_channel_message(
        &self,
        channel_id: &str,
        label: &str,
        to: &str,
        text: &str,
    ) -> anyhow::Result<A2aMessage> {
        self.insert_origin(channel_id, label, None, to, text)
    }

    /// Store a message a guest typed in `channel_id` (FR-T17), delivered under an allowance that
    /// ends at `allow_until` (Unix ms). Same sender form as a person's message.
    pub fn insert_guest_message(
        &self,
        channel_id: &str,
        guest_name: &str,
        allow_until: i64,
        to: &str,
        text: &str,
    ) -> anyhow::Result<A2aMessage> {
        self.insert_origin(channel_id, guest_name, Some(allow_until), to, text)
    }

    fn insert_origin(
        &self,
        channel_id: &str,
        label: &str,
        guest_until: Option<i64>,
        to: &str,
        text: &str,
    ) -> anyhow::Result<A2aMessage> {
        let msg = A2aMessage {
            id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
            from_session: format!("{CHANNEL_SENDER_PREFIX}{channel_id}"),
            to_session: to.into(),
            reply_to: None,
            text: text.into(),
            created_at: now_ms(),
            delivered_at: None,
            origin_channel: Some(channel_id.into()),
            origin_label: Some(label.into()),
            origin_guest_until: guest_until,
            kind: None,
        };
        self.insert(&msg)?;
        Ok(msg)
    }

    fn insert(&self, msg: &A2aMessage) -> anyhow::Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO a2a_messages
                 (id, from_session, to_session, reply_to, text, created_at, origin_channel, origin_label,
                  origin_guest_until, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                msg.id,
                msg.from_session,
                msg.to_session,
                msg.reply_to,
                msg.text,
                msg.created_at,
                msg.origin_channel,
                msg.origin_label,
                msg.origin_guest_until,
                msg.kind
            ],
        )?;
        Ok(())
    }

    pub fn message(&self, id: &str) -> anyhow::Result<Option<A2aMessage>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                &format!("SELECT {MESSAGE_COLUMNS} FROM a2a_messages WHERE id = ?1"),
                params![id],
                row_to_message,
            )
            .optional()?)
    }

    /// Undelivered messages to `to`, oldest first.
    pub fn pending_for(&self, to: &str) -> anyhow::Result<Vec<A2aMessage>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM a2a_messages WHERE to_session = ?1 AND delivered_at IS NULL
             ORDER BY created_at, rowid"
        ))?;
        let rows = stmt.query_map(params![to], row_to_message)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Sessions with at least one undelivered message.
    pub fn sessions_with_pending(&self) -> anyhow::Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT DISTINCT to_session FROM a2a_messages WHERE delivered_at IS NULL")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn mark_delivered(&self, ids: &[String]) -> anyhow::Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let now = now_ms();
        for id in ids {
            tx.execute(
                "UPDATE a2a_messages SET delivered_at = ?2 WHERE id = ?1",
                params![id, now],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Messages sent by `from` since `since_ms`.
    pub fn sent_since(&self, from: &str, since_ms: i64) -> anyhow::Result<u32> {
        Ok(self.conn.lock().unwrap().query_row(
            "SELECT COUNT(*) FROM a2a_messages WHERE from_session = ?1 AND created_at >= ?2",
            params![from, since_ms],
            |r| r.get(0),
        )?)
    }

    /// True when a person's mention of `to` in `channel` was stored at or after `since_ms`.
    pub fn channel_mention_since(&self, to: &str, channel: &str, since_ms: i64) -> anyhow::Result<bool> {
        let n: i64 = self.conn.lock().unwrap().query_row(
            "SELECT COUNT(*) FROM a2a_messages
             WHERE to_session = ?1 AND origin_channel = ?2 AND created_at >= ?3",
            params![to, channel, since_ms],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// The time of the latest mention of `to` in `channel` (a person's or a guest's), if any
    /// (FR-T12, the open mention thread).
    pub fn latest_channel_mention(&self, to: &str, channel: &str) -> anyhow::Result<Option<i64>> {
        Ok(self.conn.lock().unwrap().query_row(
            "SELECT MAX(created_at) FROM a2a_messages WHERE to_session = ?1 AND origin_channel = ?2",
            params![to, channel],
            |r| r.get(0),
        )?)
    }

    /// Messages between `a` and `b`, in either direction, since `since_ms`.
    pub fn pair_since(&self, a: &str, b: &str, since_ms: i64) -> anyhow::Result<u32> {
        Ok(self.conn.lock().unwrap().query_row(
            "SELECT COUNT(*) FROM a2a_messages
             WHERE ((from_session = ?1 AND to_session = ?2) OR (from_session = ?2 AND to_session = ?1))
               AND created_at >= ?3",
            params![a, b, since_ms],
            |r| r.get(0),
        )?)
    }

    fn switch(&self, scope: &str) -> anyhow::Result<Option<bool>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT enabled FROM a2a_switches WHERE scope = ?1",
                params![scope],
                |r| r.get::<_, bool>(0),
            )
            .optional()?)
    }

    fn set_switch(&self, scope: &str, enabled: bool) -> anyhow::Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO a2a_switches (scope, enabled) VALUES (?1, ?2)
             ON CONFLICT (scope) DO UPDATE SET enabled = ?2",
            params![scope, enabled],
        )?;
        Ok(())
    }

    /// The server-wide switch; `None` when never set.
    pub fn global_enabled(&self) -> anyhow::Result<Option<bool>> {
        self.switch(GLOBAL_SCOPE)
    }

    pub fn set_global_enabled(&self, enabled: bool) -> anyhow::Result<()> {
        self.set_switch(GLOBAL_SCOPE, enabled)
    }

    /// A session's switch; sessions are on unless turned off.
    pub fn session_enabled(&self, id: &str) -> anyhow::Result<bool> {
        Ok(self.switch(&session_scope(id))?.unwrap_or(true))
    }

    pub fn set_session_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<()> {
        self.set_switch(&session_scope(id), enabled)
    }

    /// Whether the user granted `reader` terminal access to `target` (FR-T9).
    pub fn terminal_granted(&self, reader: &str, target: &str) -> anyhow::Result<bool> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT 1 FROM a2a_terminal_grants WHERE reader = ?1 AND target = ?2",
                params![reader, target],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Grant or revoke (FR-T9).
    pub fn set_terminal_grant(&self, reader: &str, target: &str, allowed: bool) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        if allowed {
            conn.execute(
                "INSERT OR IGNORE INTO a2a_terminal_grants (reader, target, created_at) VALUES (?1, ?2, ?3)",
                params![reader, target, now_ms()],
            )?;
        } else {
            conn.execute(
                "DELETE FROM a2a_terminal_grants WHERE reader = ?1 AND target = ?2",
                params![reader, target],
            )?;
        }
        Ok(())
    }

    /// Targets `reader` was granted, sorted.
    pub fn terminal_grants(&self, reader: &str) -> anyhow::Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT target FROM a2a_terminal_grants WHERE reader = ?1 ORDER BY target")?;
        let rows = stmt.query_map(params![reader], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

fn row_to_message(r: &rusqlite::Row<'_>) -> rusqlite::Result<A2aMessage> {
    Ok(A2aMessage {
        id: r.get(0)?,
        from_session: r.get(1)?,
        to_session: r.get(2)?,
        reply_to: r.get(3)?,
        text: r.get(4)?,
        created_at: r.get(5)?,
        delivered_at: r.get(6)?,
        origin_channel: r.get(7)?,
        origin_label: r.get(8)?,
        origin_guest_until: r.get(9)?,
        kind: r.get(10)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_stored_hashed_and_replaced_on_reissue() {
        let s = A2aStore::open_in_memory().unwrap();
        let t1 = s.issue_token("s1").unwrap();
        assert_eq!(s.session_for_token(&t1).unwrap().as_deref(), Some("s1"));
        let stored: String = s
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT token_hash FROM a2a_tokens", [], |r| r.get(0))
            .unwrap();
        assert_ne!(stored, t1);
        let t2 = s.issue_token("s1").unwrap();
        assert_ne!(t1, t2);
        assert_eq!(s.session_for_token(&t1).unwrap(), None);
        assert_eq!(s.session_for_token(&t2).unwrap().as_deref(), Some("s1"));
    }

    #[test]
    fn queue_and_counts() {
        let s = A2aStore::open_in_memory().unwrap();
        let m1 = s.insert_message("a", "b", None, "one").unwrap();
        let m2 = s.insert_message("b", "a", Some(&m1.id), "two").unwrap();
        s.insert_message("a", "c", None, "three").unwrap();
        assert_eq!(s.pending_for("b").unwrap(), vec![m1.clone()]);
        assert_eq!(s.sent_since("a", 0).unwrap(), 2);
        assert_eq!(s.pair_since("a", "b", 0).unwrap(), 2);
        assert_eq!(s.pair_since("b", "a", 0).unwrap(), 2);
        s.mark_delivered(std::slice::from_ref(&m1.id)).unwrap();
        assert!(s.pending_for("b").unwrap().is_empty());
        assert_eq!(s.message(&m2.id).unwrap().unwrap().reply_to, Some(m1.id));
        let mut pending = s.sessions_with_pending().unwrap();
        pending.sort();
        assert_eq!(pending, vec!["a".to_string(), "c".to_string()]);
    }

    #[test]
    fn migration_adds_the_origin_columns_to_an_old_database_and_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("ember-a2a-mig-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ember.db");
        {
            let old = Connection::open(&path).unwrap();
            old.execute_batch(
                "CREATE TABLE a2a_messages (id TEXT PRIMARY KEY, from_session TEXT NOT NULL,
                 to_session TEXT NOT NULL, reply_to TEXT, text TEXT NOT NULL,
                 created_at INTEGER NOT NULL, delivered_at INTEGER);
                 INSERT INTO a2a_messages VALUES ('m0','a','b',NULL,'old',1,NULL);",
            )
            .unwrap();
        }
        let s = A2aStore::open(&path).unwrap();
        let again = A2aStore::open(&path).unwrap();
        let old = s.message("m0").unwrap().unwrap();
        assert_eq!(old.origin_channel, None);
        let m = again.insert_channel_message("c1", "나", "b", "hi").unwrap();
        assert_eq!(m.from_session, "channel:c1");
        let got = s.message(&m.id).unwrap().unwrap();
        assert_eq!(got.origin_channel.as_deref(), Some("c1"));
        assert_eq!(got.origin_label.as_deref(), Some("나"));
        assert_eq!(s.sent_since("a", 0).unwrap(), 1, "a channel sender never counts as a session");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// FR-T2a (#102): a database from before token lifetimes (one row per session, keyed by the
    /// session) is rebuilt so a session may hold more than one token; its tokens keep working.
    #[test]
    fn fr_t2a_the_one_token_per_session_table_is_migrated_and_keeps_its_tokens() {
        let dir = std::env::temp_dir().join(format!("ember-a2a-tok-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ember.db");
        let token = "ember_rt_old";
        {
            let old = Connection::open(&path).unwrap();
            old.execute_batch(
                "CREATE TABLE a2a_tokens (session_id TEXT PRIMARY KEY, token_hash TEXT NOT NULL UNIQUE,
                 created_at INTEGER NOT NULL);",
            )
            .unwrap();
            old.execute(
                "INSERT INTO a2a_tokens VALUES ('s1', ?1, ?2)",
                params![hash_token(token), now_ms()],
            )
            .unwrap();
        }
        let s = A2aStore::open(&path).unwrap();
        let again = A2aStore::open(&path).unwrap();
        assert_eq!(s.session_for_token(token).unwrap().as_deref(), Some("s1"));
        // Two tokens of one session side by side (a rejoined agent and its run's next process).
        let beside = again.issue_token_beside("s1").unwrap();
        assert_eq!(s.session_for_token(token).unwrap().as_deref(), Some("s1"));
        assert_eq!(s.session_for_token(&beside).unwrap().as_deref(), Some("s1"));
        assert_eq!(s.revoke_session_tokens("s1").unwrap(), 2);
        assert_eq!(s.session_for_token(token).unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn switches_default_on() {
        let s = A2aStore::open_in_memory().unwrap();
        assert_eq!(s.global_enabled().unwrap(), None);
        assert!(s.session_enabled("x").unwrap());
        s.set_session_enabled("x", false).unwrap();
        assert!(!s.session_enabled("x").unwrap());
        s.set_global_enabled(false).unwrap();
        assert_eq!(s.global_enabled().unwrap(), Some(false));
    }
}
