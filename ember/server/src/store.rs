//! Session store on the main server (SPEC FR-S1, FR-S2).
//!
//! SQLite, one file. Every event is appended with a per-session sequence number before it is
//! pushed to clients, so a restart loses no completed turn and a late client can replay from any
//! point.

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::agents::AgentKind;
use crate::events::{AgentEvent, SessionStatus};

#[derive(Debug, Clone, Serialize)]
pub struct SessionRecord {
    pub id: String,
    pub project: String,
    pub agent: AgentKind,
    pub cwd: String,
    pub model: Option<String>,
    pub native_id: Option<String>,
    pub status: SessionStatus,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// Sequence number of the last stored event (0 when none).
    pub last_seq: i64,
    /// The account this session runs under (FR-U2); `None` uses the server's own agent login.
    /// Fixed at creation and never changed.
    pub account_id: Option<String>,
    /// Why that account was chosen (user choice or the router's reason, FR-U3).
    pub account_reason: Option<String>,
    /// Shown first in its project (FR-L9). Changed with [`Store::update_session_meta`].
    pub pinned: bool,
    /// Out of the way in its project's list, still searchable (FR-L9).
    pub archived: bool,
}

/// A change to a session's user-editable metadata (FR-L9); `None` leaves a field alone.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct SessionPatch {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub pinned: Option<bool>,
    #[serde(default)]
    pub archived: Option<bool>,
}

/// One full-text search match (FR-S4): a message in a session.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchHit {
    pub session_id: String,
    /// The matching event's sequence number, so a client can scroll to it.
    pub seq: i64,
    /// `user_message` or `assistant_message`.
    pub kind: String,
    /// Text around the match, matched terms wrapped in [`SNIPPET_OPEN`] / [`SNIPPET_CLOSE`].
    pub snippet: String,
    pub title: String,
    pub project: String,
    pub archived: bool,
}

/// Marks the start of a matched term in [`SearchHit::snippet`].
pub const SNIPPET_OPEN: &str = "\u{00ab}";
/// Marks the end of a matched term in [`SearchHit::snippet`].
pub const SNIPPET_CLOSE: &str = "\u{00bb}";

#[derive(Debug, Clone, Serialize)]
pub struct StoredEvent {
    pub session_id: String,
    pub seq: i64,
    pub at: i64,
    pub event: AgentEvent,
}

pub struct Store {
    conn: Mutex<Connection>,
}

/// Schema version 1: the original tables. Databases created before migrations existed are at
/// `user_version` 0 with these tables already present, hence `IF NOT EXISTS`.
const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS sessions (
    id          TEXT PRIMARY KEY,
    project     TEXT NOT NULL,
    agent       TEXT NOT NULL,
    cwd         TEXT NOT NULL,
    model       TEXT,
    native_id   TEXT,
    status      TEXT NOT NULL,
    title       TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    last_seq    INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS events (
    session_id  TEXT NOT NULL REFERENCES sessions(id),
    seq         INTEGER NOT NULL,
    at          INTEGER NOT NULL,
    event       TEXT NOT NULL,
    PRIMARY KEY (session_id, seq)
);
";

/// Acknowledged output offset per session shim (SPEC FR-S8): where a new server resumes the
/// agent's output after a restart.
const SHIM_OFFSETS_MIGRATION: &str = "
CREATE TABLE IF NOT EXISTS shim_offsets (
    session_id TEXT PRIMARY KEY,
    offset     INTEGER NOT NULL
);
";

/// Per-harness settings stored on the main server (FR-L20, FR-S8, #311). One row per harness and
/// key; the only key today is `mem_limit_mb` (decimal MiB). No row means unset.
const HARNESS_SETTINGS_MIGRATION: &str = "
CREATE TABLE IF NOT EXISTS harness_settings (
    harness    TEXT NOT NULL,
    key        TEXT NOT NULL,
    value      TEXT NOT NULL,
    updated_ms INTEGER NOT NULL,
    PRIMARY KEY (harness, key)
);
";

/// Partial index over prompt and turn-end events per session (`idx_events_prompt_turn`), so
/// resolving a prompt or reading pending prompts walks only the current turn's prompt rows instead
/// of scanning the session's full event history (#909).
pub(crate) const PROMPT_EVENTS_INDEX_MIGRATION: &str = "
CREATE INDEX IF NOT EXISTS idx_events_prompt_turn
ON events (session_id, seq DESC)
WHERE json_extract(event, '$.kind') IN (
    'turn_ended',
    'approval_requested',
    'approval_resolved',
    'question_asked',
    'question_answered',
    'prompt_withdrawn'
);
";

/// Query returning only prompt and turn-end events for a session, newest first (`idx_events_prompt_turn`).
const CURRENT_TURN_PROMPT_EVENTS_SQL: &str = "
SELECT event FROM events
WHERE session_id = ?1
  AND json_extract(event, '$.kind') IN (
      'turn_ended',
      'approval_requested',
      'approval_resolved',
      'question_asked',
      'question_answered',
      'prompt_withdrawn'
  )
ORDER BY seq DESC
";

/// Migrations, in order: entry `i` brings `PRAGMA user_version` from `i` to `i + 1`. Each runs in
/// its own transaction together with the version bump. Append only; never edit a shipped entry.
const MIGRATIONS: &[&str] = &[
    SCHEMA_V1,
    crate::accounts::schema::MIGRATION,
    crate::computers::schema::MIGRATION,
    crate::chatgpt::schema::MIGRATION,
    crate::browser::egress::MIGRATION,
    crate::computers::schema::PEER_MIGRATION,
    crate::devices::schema::MIGRATION,
    crate::projects::schema::MIGRATION,
    crate::projects::schema::META_FTS_MIGRATION,
    crate::hub::schema::MIGRATION,
    crate::hub::schema::RESOLVE_MIGRATION,
    crate::mcp::schema::MIGRATION,
    crate::schedules::schema::MIGRATION,
    crate::a2a::team::MIGRATION,
    crate::fork::MIGRATION,
    crate::fork::NATIVE_MIGRATION,
    crate::computers::schema::DESIGNATED_MIGRATION,
    crate::members::MIGRATION,
    SHIM_OFFSETS_MIGRATION,
    crate::channels::MIGRATION,
    crate::channels::READS_MIGRATION,
    crate::channels::MENTIONS_MIGRATION,
    crate::projects::schema::SETTINGS_MIGRATION,
    crate::channels::invites::MIGRATION,
    crate::projects::schema::PROFILE_OVERRIDES_MIGRATION,
    crate::computers::enrol::MIGRATION,
    crate::computers::enrol::CONFIRM_MIGRATION,
    HARNESS_SETTINGS_MIGRATION,
    crate::computers::schema::PEER_ID_UNIQUE_MIGRATION,
    crate::projects::schema::CHARACTER_OVERRIDES_MIGRATION,
    crate::schedules::schema::EVENTS_MIGRATION,
    crate::devices::schema::SCOPE_MIGRATION,
    PROMPT_EVENTS_INDEX_MIGRATION,
];

/// The position of a registered migration (tests locate migrations by position, never by length).
#[cfg(test)]
pub(crate) fn migration_position(sql: &str) -> Option<usize> {
    MIGRATIONS.iter().position(|m| *m == sql)
}

/// Apply the first `count` migrations to `conn` (test helper for migration backfill tests).
#[cfg(test)]
pub(crate) fn migrate_up_to(conn: &mut Connection, count: usize) -> anyhow::Result<()> {
    for (i, sql) in MIGRATIONS.iter().enumerate().take(count) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        if *sql == crate::computers::schema::PEER_ID_UNIQUE_MIGRATION {
            crate::computers::backfill_peer_ids(&tx)?;
        }
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

/// Bring `conn` up to the latest schema version.
fn migrate(conn: &mut Connection) -> anyhow::Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    anyhow::ensure!(
        current as usize <= MIGRATIONS.len(),
        "database schema version {current} is newer than this server ({})",
        MIGRATIONS.len()
    );
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        if *sql == crate::computers::schema::PEER_ID_UNIQUE_MIGRATION {
            crate::computers::backfill_peer_ids(&tx)?;
        }
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

const SESSION_COLUMNS: &str = "id, project, agent, cwd, model, native_id, status, title, created_at, updated_at, last_seq, account_id, account_reason, pinned, archived";

/// Current Unix time in milliseconds.
pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn status_str(s: SessionStatus) -> &'static str {
    match s {
        SessionStatus::Idle => "idle",
        SessionStatus::Running => "running",
        SessionStatus::WaitingForApproval => "waiting_for_approval",
        SessionStatus::Finished => "finished",
        SessionStatus::Failed => "failed",
    }
}

fn parse_status(s: &str) -> SessionStatus {
    match s {
        "running" => SessionStatus::Running,
        "waiting_for_approval" => SessionStatus::WaitingForApproval,
        "finished" => SessionStatus::Finished,
        "failed" => SessionStatus::Failed,
        _ => SessionStatus::Idle,
    }
}

/// Prompt events (`ApprovalRequested`, `ApprovalResolved`, `QuestionAsked`, `QuestionAnswered`,
/// `PromptWithdrawn`) of `session_id` since the latest `TurnEnded`, in chronological order (#909).
/// Also returns the number of database rows fetched before stopping at `TurnEnded`.
fn current_turn_prompt_events_conn(
    conn: &Connection,
    session_id: &str,
) -> anyhow::Result<(Vec<AgentEvent>, usize)> {
    let mut stmt = conn.prepare(CURRENT_TURN_PROMPT_EVENTS_SQL)?;
    let mut rows = stmt.query(params![session_id])?;
    let mut events_rev = Vec::new();
    let mut rows_read = 0usize;
    while let Some(row) = rows.next()? {
        rows_read += 1;
        let json: String = row.get(0)?;
        let ev: AgentEvent = serde_json::from_str(&json)?;
        if matches!(ev, AgentEvent::TurnEnded { .. }) {
            break;
        }
        events_rev.push(ev);
    }
    events_rev.reverse();
    Ok((events_rev, rows_read))
}

impl Store {
    pub fn open(path: &Path) -> anyhow::Result<Store> {
        let mut conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode = WAL;")?;
        migrate(&mut conn)?;
        Ok(Store { conn: Mutex::new(conn) })
    }

    pub fn open_in_memory() -> anyhow::Result<Store> {
        let mut conn = Connection::open_in_memory()?;
        migrate(&mut conn)?;
        Ok(Store { conn: Mutex::new(conn) })
    }

    /// The connection, for modules that own their own tables (accounts, …).
    pub(crate) fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap()
    }

    /// `PRAGMA user_version` of the open database.
    pub fn schema_version(&self) -> anyhow::Result<i64> {
        Ok(self.conn().query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    /// One stored setting of a harness (`harness` is the agent's wire name), if set.
    pub fn harness_setting(&self, harness: &str, key: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT value FROM harness_settings WHERE harness = ?1 AND key = ?2",
                params![harness, key],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Store (or replace) a setting of a harness. Validation belongs to the caller.
    pub fn set_harness_setting(&self, harness: &str, key: &str, value: &str) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO harness_settings (harness, key, value, updated_ms) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (harness, key) DO UPDATE SET value = excluded.value, updated_ms = excluded.updated_ms",
            params![harness, key, value, now_ms()],
        )?;
        Ok(())
    }

    /// Remove a setting of a harness; removing one that is not set is not an error.
    pub fn clear_harness_setting(&self, harness: &str, key: &str) -> anyhow::Result<()> {
        self.conn().execute(
            "DELETE FROM harness_settings WHERE harness = ?1 AND key = ?2",
            params![harness, key],
        )?;
        Ok(())
    }

    /// Every `(harness, value)` stored under `key`, ordered by harness name.
    pub fn harness_settings_with_key(&self, key: &str) -> anyhow::Result<Vec<(String, String)>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT harness, value FROM harness_settings WHERE key = ?1 ORDER BY harness")?;
        let rows = stmt
            .query_map(params![key], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn create_session(
        &self,
        project: &str,
        agent: AgentKind,
        cwd: &str,
        model: Option<&str>,
        title: &str,
    ) -> anyhow::Result<SessionRecord> {
        self.create_session_with_account(project, agent, cwd, model, title, None)
    }

    /// Create a session under `account` = `(account id, reason)` (FR-U2).
    pub fn create_session_with_account(
        &self,
        project: &str,
        agent: AgentKind,
        cwd: &str,
        model: Option<&str>,
        title: &str,
        account: Option<(&str, &str)>,
    ) -> anyhow::Result<SessionRecord> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_ms();
        let (account_id, reason) = account.unzip();
        {
            let mut conn = self.conn.lock().unwrap();
            let tx = conn.transaction()?;
            // The project exists from its first session on (FR-L1, FR-L4).
            tx.execute(
                "INSERT OR IGNORE INTO projects (name, created_at) VALUES (?1, ?2)",
                params![project, now],
            )?;
            tx.execute(
                "INSERT INTO sessions (id, project, agent, cwd, model, status, title, created_at, updated_at, account_id, account_reason)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9, ?10)",
                params![id, project, agent.as_str(), cwd, model, status_str(SessionStatus::Idle), title, now, account_id, reason],
            )?;
            tx.commit()?;
        }
        Ok(self.session(&id)?.expect("just inserted"))
    }

    pub fn session(&self, id: &str) -> anyhow::Result<Option<SessionRecord>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                &format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE id = ?1"),
                params![id],
                row_to_session,
            )
            .optional()?)
    }

    pub fn sessions(&self, project: Option<&str>) -> anyhow::Result<Vec<SessionRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {SESSION_COLUMNS} FROM sessions WHERE (?1 IS NULL OR project = ?1) ORDER BY updated_at DESC"
        ))?;
        let rows = stmt.query_map(params![project], row_to_session)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Append `event`, updating the session's status and native id in the same transaction.
    pub fn append(&self, session_id: &str, event: &AgentEvent) -> anyhow::Result<(StoredEvent, SessionStatus)> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let (last_seq, status): (i64, String) = tx.query_row(
            "SELECT last_seq, status FROM sessions WHERE id = ?1",
            params![session_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let seq = last_seq + 1;
        let at = now_ms();
        let still_pending = if matches!(
            event,
            AgentEvent::ApprovalResolved { .. }
                | AgentEvent::QuestionAnswered { .. }
                | AgentEvent::PromptWithdrawn { .. }
        ) {
            let (mut prior, _) = current_turn_prompt_events_conn(&tx, session_id)?;
            prior.push(event.clone());
            crate::events::has_pending_prompts(&prior)
        } else {
            false
        };
        let status = parse_status(&status).after_with_pending(event, still_pending);
        tx.execute(
            "INSERT INTO events (session_id, seq, at, event) VALUES (?1, ?2, ?3, ?4)",
            params![session_id, seq, at, serde_json::to_string(event)?],
        )?;
        tx.execute(
            "UPDATE sessions SET last_seq = ?2, status = ?3, updated_at = ?4 WHERE id = ?1",
            params![session_id, seq, status_str(status), at],
        )?;
        if let AgentEvent::NativeSession { native_id } = event {
            tx.execute(
                "UPDATE sessions SET native_id = ?2 WHERE id = ?1",
                params![session_id, native_id],
            )?;
        }
        tx.commit()?;
        Ok((StoredEvent { session_id: session_id.to_string(), seq, at, event: event.clone() }, status))
    }

    /// Prompt events of `session_id` since the latest `TurnEnded`, oldest first (#909). Uses
    /// `idx_events_prompt_turn` (`ORDER BY seq DESC`), stopping at the first `TurnEnded` row so
    /// resolving a question or reading pending prompts never scans the session's full event history.
    pub fn current_turn_prompt_events(&self, session_id: &str) -> anyhow::Result<Vec<AgentEvent>> {
        let conn = self.conn.lock().unwrap();
        Ok(current_turn_prompt_events_conn(&conn, session_id)?.0)
    }

    /// Events with `seq > after`, oldest first.
    pub fn events_after(&self, session_id: &str, after: i64) -> anyhow::Result<Vec<StoredEvent>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT seq, at, event FROM events WHERE session_id = ?1 AND seq > ?2 ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![session_id, after], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, at, json) = row?;
            out.push(StoredEvent {
                session_id: session_id.to_string(),
                seq,
                at,
                event: serde_json::from_str(&json)?,
            });
        }
        Ok(out)
    }

    /// Change a session's title, pin or archive mark (FR-L9). Does not touch `updated_at`, which
    /// is activity. Returns the updated record, or `None` when the session does not exist.
    /// An empty title is refused.
    pub fn update_session_meta(&self, id: &str, patch: &SessionPatch) -> anyhow::Result<Option<SessionRecord>> {
        let title = patch.title.as_deref().map(str::trim);
        anyhow::ensure!(title != Some(""), "title must not be empty");
        let changed = self.conn.lock().unwrap().execute(
            "UPDATE sessions SET
                 title = COALESCE(?2, title),
                 pinned = COALESCE(?3, pinned),
                 archived = COALESCE(?4, archived)
             WHERE id = ?1",
            params![id, title, patch.pinned, patch.archived],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        self.session(id)
    }

    /// Full-text search over every session's user and assistant messages (FR-S4), best match
    /// first, at most `limit` hits. Uses the FTS5 index `messages_fts` (never a scan of
    /// `events`). Each whitespace-separated word must match as a word prefix, so `오류` finds
    /// `오류가` and `serv` finds `server`; FTS5 syntax in the query is taken literally.
    pub fn search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<SearchHit>> {
        let Some(fts) = fts_query(query) else { return Ok(Vec::new()) };
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT messages_fts.session_id, messages_fts.seq, messages_fts.kind,
                    snippet(messages_fts, 0, ?2, ?3, '\u{2026}', 16),
                    sessions.title, sessions.project, sessions.archived
             FROM messages_fts JOIN sessions ON sessions.id = messages_fts.session_id
             WHERE messages_fts MATCH ?1
             ORDER BY rank
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(params![fts, SNIPPET_OPEN, SNIPPET_CLOSE, limit as i64], |r| {
            Ok(SearchHit {
                session_id: r.get(0)?,
                seq: r.get(1)?,
                kind: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                snippet: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                title: r.get(4)?,
                project: r.get(5)?,
                archived: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Record how much of a session's shim output is stored (FR-S8).
    pub fn set_shim_offset(&self, session_id: &str, offset: u64) -> anyhow::Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO shim_offsets (session_id, offset) VALUES (?1, ?2)
             ON CONFLICT(session_id) DO UPDATE SET offset = excluded.offset",
            params![session_id, offset as i64],
        )?;
        Ok(())
    }

    /// The stored shim output offset of a session, 0 when none.
    pub fn shim_offset(&self, session_id: &str) -> anyhow::Result<u64> {
        let v: Option<i64> = self
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT offset FROM shim_offsets WHERE session_id = ?1", [session_id], |r| r.get(0))
            .optional()?;
        Ok(v.unwrap_or(0).max(0) as u64)
    }

    /// All stored shim offsets, for `reattach_all` at startup.
    pub fn shim_offsets(&self) -> anyhow::Result<std::collections::HashMap<String, u64>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT session_id, offset FROM shim_offsets")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as u64)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Forget a session's shim offset (its agent ended).
    pub fn clear_shim_offset(&self, session_id: &str) -> anyhow::Result<()> {
        self.conn.lock().unwrap().execute("DELETE FROM shim_offsets WHERE session_id = ?1", [session_id])?;
        Ok(())
    }

    /// After a restart, sessions that were mid-turn can no longer be: mark them idle so the next
    /// message resumes them natively.
    pub fn reset_live_statuses(&self) -> anyhow::Result<usize> {
        Ok(self.conn.lock().unwrap().execute(
            "UPDATE sessions SET status = 'idle' WHERE status IN ('running', 'waiting_for_approval')",
            [],
        )?)
    }
}

fn row_to_session(r: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRecord> {
    let agent: String = r.get(2)?;
    let status: String = r.get(6)?;
    Ok(SessionRecord {
        id: r.get(0)?,
        project: r.get(1)?,
        agent: AgentKind::from_stored(&agent).unwrap_or(AgentKind::Scripted),
        cwd: r.get(3)?,
        model: r.get(4)?,
        native_id: r.get(5)?,
        status: parse_status(&status),
        title: r.get(7)?,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
        last_seq: r.get(10)?,
        account_id: r.get(11)?,
        account_reason: r.get(12)?,
        pinned: r.get(13)?,
        archived: r.get(14)?,
    })
}

/// An FTS5 query for `input`: every word quoted (so `"`, `*`, `AND`, `-`… are literal) and
/// made a prefix query, all of them required. Words without a letter or digit would tokenize
/// to nothing and are dropped. `None` when no words are left.
fn fts_query(input: &str) -> Option<String> {
    let terms: Vec<String> = input
        .split_whitespace()
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .map(|w| format!("\"{}\"*", w.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::TurnOutcome;

    #[test]
    fn append_sequences_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ember.db");
        let id;
        {
            let store = Store::open(&path).unwrap();
            let s = store.create_session("p", AgentKind::Scripted, "/tmp", None, "t").unwrap();
            id = s.id;
            store.append(&id, &AgentEvent::NativeSession { native_id: "n1".into() }).unwrap();
            store.append(&id, &AgentEvent::UserMessage { text: "hi".into() }).unwrap();
            let (e, st) = store
                .append(&id, &AgentEvent::TurnEnded { outcome: TurnOutcome::Completed })
                .unwrap();
            assert_eq!(e.seq, 3);
            assert_eq!(st, SessionStatus::Finished);
        }
        let store = Store::open(&path).unwrap();
        let s = store.session(&id).unwrap().unwrap();
        assert_eq!(s.native_id.as_deref(), Some("n1"));
        assert_eq!(s.last_seq, 3);
        assert_eq!(s.status, SessionStatus::Finished);
        let tail = store.events_after(&id, 1).unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].event, AgentEvent::UserMessage { text: "hi".into() });
    }

    #[test]
    fn pre_migration_database_is_migrated_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ember.db");
        {
            // A database written by the server before migrations existed (user_version 0).
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.execute(
                "INSERT INTO sessions (id, project, agent, cwd, model, native_id, status, title, created_at, updated_at, last_seq)
                 VALUES ('old', 'p', 'codex', '/tmp', NULL, 'thr-1', 'finished', 't', 1, 2, 0)",
                [],
            )
            .unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), MIGRATIONS.len() as i64);
        let old = store.session("old").unwrap().unwrap();
        assert_eq!(old.native_id.as_deref(), Some("thr-1"));
        assert_eq!(old.account_id, None, "existing sessions keep the server's own login");
        let new = store
            .create_session_with_account("p", AgentKind::Codex, "/tmp", None, "t", Some(("a1", "chosen")))
            .unwrap();
        assert_eq!(new.account_id.as_deref(), Some("a1"));
        drop(store);
        // Reopening is a no-op.
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), MIGRATIONS.len() as i64);
        assert_eq!(store.sessions(None).unwrap().len(), 2);
    }

    /// FR-L26: a database from before the profile override column gains it as NULL and keeps its
    /// projects; an override can then be stored.
    #[test]
    fn fr_l26_old_database_gains_profile_overrides_column() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ember.db");
        let before =
            MIGRATIONS.iter().position(|m| *m == crate::projects::schema::PROFILE_OVERRIDES_MIGRATION).unwrap();
        {
            let mut conn = Connection::open(&path).unwrap();
            for (i, sql) in MIGRATIONS.iter().enumerate().take(before) {
                let tx = conn.transaction().unwrap();
                tx.execute_batch(sql).unwrap();
                tx.pragma_update(None, "user_version", (i + 1) as i64).unwrap();
                tx.commit().unwrap();
            }
            conn.execute_batch("INSERT INTO projects (name, created_at) VALUES ('alpha', 5);").unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), MIGRATIONS.len() as i64);
        let alpha = store.project("alpha").unwrap().unwrap();
        assert_eq!(alpha.created_at, 5);
        assert!(alpha.profile_overrides.is_empty());
        let stored: Option<String> = store
            .conn()
            .query_row("SELECT profile_overrides FROM projects WHERE name = 'alpha'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, None, "existing rows hold NULL");
    }

    /// FR-T14: a database from before the invite tables gains them in place. The old database
    /// stops just before the invite migration, found by position, so a migration appended after it
    /// does not change what this test means (a test that drops the tables and rewinds
    /// `user_version` by one breaks as soon as another migration is appended).
    #[test]
    fn fr_t14_the_migration_applies_to_an_old_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ember.db");
        let before = MIGRATIONS.iter().position(|m| *m == crate::channels::invites::MIGRATION).unwrap();
        {
            let mut conn = Connection::open(&path).unwrap();
            for (i, sql) in MIGRATIONS.iter().enumerate().take(before) {
                let tx = conn.transaction().unwrap();
                tx.execute_batch(sql).unwrap();
                tx.pragma_update(None, "user_version", (i + 1) as i64).unwrap();
                tx.commit().unwrap();
            }
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), MIGRATIONS.len() as i64);
        // A project's default channel exists once it has a session.
        store.create_session("core", crate::agents::AgentKind::Scripted, "/tmp/x", None, "alpha").unwrap();
        let ch = store.channels(None).unwrap().remove(0).id;
        assert!(store.create_invite(&ch, "user", None).is_ok());
        assert_eq!(store.list_invites(&ch).unwrap().len(), 1);
    }

    /// FR-L26: a database from before the project settings columns gains them in place, with
    /// empty values, and keeps its projects.
    #[test]
    fn fr_l26_old_database_gains_project_settings_columns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ember.db");
        // The database stops just before the settings migration, wherever it sits in the list.
        let before = MIGRATIONS.iter().position(|m| *m == crate::projects::schema::SETTINGS_MIGRATION).unwrap();
        {
            let mut conn = Connection::open(&path).unwrap();
            for (i, sql) in MIGRATIONS.iter().enumerate().take(before) {
                let tx = conn.transaction().unwrap();
                tx.execute_batch(sql).unwrap();
                tx.pragma_update(None, "user_version", (i + 1) as i64).unwrap();
                tx.commit().unwrap();
            }
            conn.execute_batch("INSERT INTO projects (name, created_at) VALUES ('alpha', 5), ('기본 프로젝트', 6);")
                .unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), MIGRATIONS.len() as i64);
        let projects = store.projects().unwrap();
        assert_eq!(projects.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["alpha", "기본 프로젝트"]);
        for p in &projects {
            assert_eq!((p.repository.as_deref(), p.default_branch.as_deref()), (None, None));
            assert!(p.member_defaults == Default::default() && p.character_style.is_none() && !p.archived);
        }
        assert_eq!(projects[0].created_at, 5);
    }

    /// A database at schema 7 (before projects, session metadata and search) gains them in
    /// place: projects from its sessions, unpinned/unarchived sessions, and an index of the
    /// messages it already holds.
    #[test]
    fn version_7_database_gains_projects_meta_and_search_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ember.db");
        {
            let mut conn = Connection::open(&path).unwrap();
            for (i, sql) in MIGRATIONS.iter().enumerate().take(7) {
                let tx = conn.transaction().unwrap();
                tx.execute_batch(sql).unwrap();
                tx.pragma_update(None, "user_version", (i + 1) as i64).unwrap();
                tx.commit().unwrap();
            }
            conn.execute_batch(
                "INSERT INTO sessions (id, project, agent, cwd, status, title, created_at, updated_at, last_seq)
                 VALUES ('a', 'alpha', 'codex', '/w', 'finished', 'A', 5, 9, 3),
                        ('b', 'alpha', 'codex', '/w', 'finished', 'B', 3, 4, 0),
                        ('c', 'beta', 'claude-code', '/w', 'idle', 'C', 7, 7, 0);
                 INSERT INTO events (session_id, seq, at, event) VALUES
                    ('a', 1, 5, '{\"kind\":\"user_message\",\"text\":\"배포 서버에서 오류가 났어요\"}'),
                    ('a', 2, 6, '{\"kind\":\"assistant_delta\",\"text\":\"오류\"}'),
                    ('a', 3, 7, '{\"kind\":\"assistant_message\",\"text\":\"The deploy server log shows an error\"}');",
            )
            .unwrap();
            let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
            assert_eq!(v, 7);
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), MIGRATIONS.len() as i64);

        let projects = store.projects().unwrap();
        assert_eq!(projects.len(), 2);
        assert_eq!((projects[0].name.as_str(), projects[0].created_at), ("alpha", 3), "earliest session");
        assert_eq!((projects[1].name.as_str(), projects[1].created_at), ("beta", 7));

        let a = store.session("a").unwrap().unwrap();
        assert!(!a.pinned && !a.archived);

        let hits = store.search("오류", 10).unwrap();
        assert_eq!(hits.len(), 1, "deltas are not indexed: {hits:?}");
        assert_eq!((hits[0].session_id.as_str(), hits[0].seq), ("a", 1));
        assert!(hits[0].snippet.contains("\u{00ab}오류가\u{00bb}"), "{}", hits[0].snippet);
        assert_eq!(store.search("deploy error", 10).unwrap()[0].seq, 3);

        // New events are indexed by the trigger.
        store.append("c", &AgentEvent::UserMessage { text: "세션 검색 테스트".into() }).unwrap();
        let hits = store.search("검색", 10).unwrap();
        assert_eq!((hits[0].session_id.as_str(), hits[0].seq, hits[0].project.as_str()), ("c", 1, "beta"));
    }

    #[test]
    fn session_meta_patch_and_search_query_escaping() {
        let store = Store::open_in_memory().unwrap();
        let s = store.create_session("p", AgentKind::Scripted, "/tmp", None, "t").unwrap();
        store.append(&s.id, &AgentEvent::UserMessage { text: "say \"hello\" AND -world".into() }).unwrap();
        let before = store.session(&s.id).unwrap().unwrap();
        let after = store
            .update_session_meta(&s.id, &SessionPatch { title: Some("  Renamed ".into()), pinned: Some(true), archived: None })
            .unwrap()
            .unwrap();
        assert_eq!((after.title.as_str(), after.pinned, after.archived), ("Renamed", true, false));
        assert_eq!(after.updated_at, before.updated_at, "metadata is not activity");
        let after = store.update_session_meta(&s.id, &SessionPatch { archived: Some(true), ..Default::default() }).unwrap().unwrap();
        assert!(after.pinned && after.archived && after.title == "Renamed");
        assert!(store.update_session_meta(&s.id, &SessionPatch { title: Some(" ".into()), ..Default::default() }).is_err());
        assert!(store.update_session_meta("nope", &SessionPatch::default()).unwrap().is_none());

        // FTS5 operators in the query are literal text, never syntax errors.
        assert_eq!(store.search("\"hello\"", 5).unwrap().len(), 1);
        assert_eq!(store.search("AND -world", 5).unwrap().len(), 1);
        assert!(store.search("   ", 5).unwrap().is_empty());
        assert!(store.search("( - *", 5).unwrap().is_empty());
        assert!(store.search("absent", 5).unwrap().is_empty());
        assert_eq!(fts_query("a \"b"), Some("\"a\"* \"\"\"b\"*".to_string()));
    }

    #[test]
    fn fr_l20_harness_setting_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.harness_setting("codex", "mem_limit_mb").unwrap(), None);
        store.set_harness_setting("codex", "mem_limit_mb", "2048").unwrap();
        assert_eq!(store.harness_setting("codex", "mem_limit_mb").unwrap().as_deref(), Some("2048"));
        store.set_harness_setting("codex", "mem_limit_mb", "4096").unwrap();
        assert_eq!(store.harness_setting("codex", "mem_limit_mb").unwrap().as_deref(), Some("4096"));
        assert_eq!(store.harness_setting("claude-code", "mem_limit_mb").unwrap(), None, "per harness");
        store.clear_harness_setting("codex", "mem_limit_mb").unwrap();
        assert_eq!(store.harness_setting("codex", "mem_limit_mb").unwrap(), None);
        store.clear_harness_setting("codex", "mem_limit_mb").unwrap();
    }

    #[test]
    fn fr_l20_harness_setting_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ember.db");
        Store::open(&path).unwrap().set_harness_setting("antigravity", "mem_limit_mb", "1024").unwrap();
        let store = Store::open(&path).unwrap();
        assert_eq!(store.harness_setting("antigravity", "mem_limit_mb").unwrap().as_deref(), Some("1024"));
        assert_eq!(store.schema_version().unwrap(), MIGRATIONS.len() as i64);
    }

    #[test]
    fn fr_l20_harness_settings_with_key_lists_only_that_key() {
        let store = Store::open_in_memory().unwrap();
        store.set_harness_setting("codex", "mem_limit_mb", "2048").unwrap();
        store.set_harness_setting("claude-code", "mem_limit_mb", "1024").unwrap();
        store.set_harness_setting("codex", "other", "x").unwrap();
        assert_eq!(
            store.harness_settings_with_key("mem_limit_mb").unwrap(),
            vec![("claude-code".to_string(), "1024".to_string()), ("codex".to_string(), "2048".to_string())]
        );
        assert!(store.harness_settings_with_key("none").unwrap().is_empty());
    }

    #[test]
    fn newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ember.db");
        Connection::open(&path).unwrap().pragma_update(None, "user_version", 99).unwrap();
        assert!(Store::open(&path).is_err());
    }

    #[test]
    fn reset_live_statuses_after_restart() {
        let store = Store::open_in_memory().unwrap();
        let s = store.create_session("p", AgentKind::Scripted, "/tmp", None, "t").unwrap();
        store.append(&s.id, &AgentEvent::UserMessage { text: "hi".into() }).unwrap();
        assert_eq!(store.reset_live_statuses().unwrap(), 1);
        assert_eq!(store.session(&s.id).unwrap().unwrap().status, SessionStatus::Idle);
    }

    /// FR-A5 (#909): resolving a question or checking pending prompts uses `idx_events_prompt_turn`
    /// (`ORDER BY seq DESC` stopping at the first `TurnEnded`), reading only the current turn's
    /// prompt rows plus at most one `TurnEnded` row instead of scanning the session's full history.
    #[test]
    fn fr_a5_current_turn_prompt_events_uses_partial_index_and_reads_only_the_current_turn() {
        use crate::events::{ApprovalDecision, Question, QuestionOption, TurnOutcome};

        let store = Store::open_in_memory().unwrap();
        let s = store.create_session("p", AgentKind::Scripted, "/tmp", None, "long").unwrap();
        let id = s.id.clone();

        // Confirm SQLite's query planner uses the partial index `idx_events_prompt_turn`.
        {
            let conn = store.conn();
            let mut stmt = conn
                .prepare(&format!("EXPLAIN QUERY PLAN {CURRENT_TURN_PROMPT_EVENTS_SQL}"))
                .unwrap();
            let details: Vec<String> = stmt
                .query_map(params![id.as_str()], |r| r.get::<_, String>(3))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert!(
                details.iter().any(|d| d.contains("idx_events_prompt_turn")),
                "expected idx_events_prompt_turn in query plan, got {details:?}"
            );
        }

        // First turn with no `TurnEnded` yet: all prompt events in the turn are returned in order.
        let q1 = AgentEvent::QuestionAsked {
            question_id: "q-early".into(),
            questions: vec![Question {
                id: "q0".into(),
                header: Some("Pick".into()),
                text: "Which?".into(),
                options: vec![QuestionOption { label: "A".into(), description: None }],
                multi_select: false,
                allow_other: false,
                secret: false,
            }],
        };
        store.append(&id, &AgentEvent::UserMessage { text: "start".into() }).unwrap();
        store.append(&id, &q1).unwrap();
        let (early_events, early_rows) = {
            let conn = store.conn();
            current_turn_prompt_events_conn(&conn, &id).unwrap()
        };
        assert_eq!(early_events, vec![q1]);
        assert_eq!(early_rows, 1, "user_message is not in the partial index");

        // Now populate 10,000 events across 50 completed turns (1 KiB payload per delta).
        let payload = "x".repeat(1024);
        {
            let mut conn = store.conn();
            let tx = conn.transaction().unwrap();
            tx.execute("DELETE FROM events WHERE session_id = ?1", params![id]).unwrap();
            for seq in 1..=10_000i64 {
                let ev = if seq % 200 == 0 {
                    serde_json::to_string(&AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }).unwrap()
                } else if seq % 200 == 50 {
                    serde_json::to_string(&AgentEvent::ApprovalRequested {
                        approval_id: format!("old-{seq}"),
                        tool: "Bash".into(),
                        input: serde_json::json!({}),
                        needs_person: false,
                    })
                    .unwrap()
                } else {
                    serde_json::to_string(&AgentEvent::AssistantDelta { text: payload.clone() }).unwrap()
                };
                tx.execute(
                    "INSERT INTO events (session_id, seq, at, event) VALUES (?1, ?2, ?3, ?4)",
                    params![id, seq, seq, ev],
                )
                .unwrap();
            }
            tx.execute("UPDATE sessions SET last_seq = 10000, status = 'finished' WHERE id = ?1", params![id])
                .unwrap();
            tx.commit().unwrap();
        }

        // Start turn 51: 1 user message, 100 deltas, 1 question, 1 approval.
        store.append(&id, &AgentEvent::UserMessage { text: "turn 51".into() }).unwrap();
        for _ in 0..100 {
            store.append(&id, &AgentEvent::AssistantDelta { text: payload.clone() }).unwrap();
        }
        let q_current = AgentEvent::QuestionAsked {
            question_id: "q1".into(),
            questions: vec![Question {
                id: "q0".into(),
                header: Some("Pick".into()),
                text: "Which?".into(),
                options: vec![QuestionOption { label: "A".into(), description: None }],
                multi_select: false,
                allow_other: false,
                secret: false,
            }],
        };
        let a_current = AgentEvent::ApprovalRequested {
            approval_id: "a1".into(),
            tool: "Bash".into(),
            input: serde_json::json!({}),
            needs_person: false,
        };
        store.append(&id, &q_current).unwrap();
        let (_, st) = store.append(&id, &a_current).unwrap();
        assert_eq!(st, SessionStatus::WaitingForApproval);

        let t0 = std::time::Instant::now();
        let (prompt_events, rows_read) = {
            let conn = store.conn();
            current_turn_prompt_events_conn(&conn, &id).unwrap()
        };
        let lookup_us = t0.elapsed().as_micros();
        assert_eq!(prompt_events, vec![q_current, a_current]);
        assert_eq!(
            rows_read, 3,
            "must read only the 2 current-turn prompts + 1 previous TurnEnded row (not 10,103 rows)"
        );

        let t1 = std::time::Instant::now();
        let (_, st) = store
            .append(&id, &AgentEvent::QuestionAnswered { question_id: "q1".into(), answers: vec![] })
            .unwrap();
        let append_q_us = t1.elapsed().as_micros();
        assert_eq!(st, SessionStatus::WaitingForApproval, "a1 is still pending");

        let (_, st) = store
            .append(
                &id,
                &AgentEvent::ApprovalResolved { approval_id: "a1".into(), decision: ApprovalDecision::AllowOnce },
            )
            .unwrap();
        assert_eq!(st, SessionStatus::Running, "all prompts in turn 51 resolved");
        eprintln!(
            "AFTER_FIX: prompt_events_rows_read={} lookup_us={} append_question_answered_us={}",
            rows_read, lookup_us, append_q_us
        );
    }

    /// FR-A5 (#909): an existing database from before `PROMPT_EVENTS_INDEX_MIGRATION` gains
    /// `idx_events_prompt_turn` on upgrade and indexes its existing prompt and `turn_ended` rows.
    #[test]
    fn fr_a5_old_database_gains_prompt_events_partial_index() {
        use crate::events::TurnOutcome;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ember.db");
        let before = migration_position(PROMPT_EVENTS_INDEX_MIGRATION).unwrap();
        {
            let mut conn = Connection::open(&path).unwrap();
            migrate_up_to(&mut conn, before).unwrap();
            conn.execute_batch(
                "INSERT INTO projects (name, created_at) VALUES ('p', 1);
                 INSERT INTO sessions (id, project, agent, cwd, status, title, created_at, updated_at, last_seq)
                 VALUES ('s1', 'p', 'claude-code', '/tmp', 'waiting_for_approval', 'T', 1, 3, 3);
                 INSERT INTO events (session_id, seq, at, event) VALUES
                    ('s1', 1, 1, '{\"kind\":\"approval_requested\",\"approval_id\":\"old\",\"tool\":\"Bash\",\"input\":{}}'),
                    ('s1', 2, 2, '{\"kind\":\"turn_ended\",\"outcome\":\"completed\"}'),
                    ('s1', 3, 3, '{\"kind\":\"question_asked\",\"question_id\":\"q1\",\"questions\":[]}');",
            )
            .unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), MIGRATIONS.len() as i64);
        let events = store.current_turn_prompt_events("s1").unwrap();
        assert_eq!(
            events,
            vec![AgentEvent::QuestionAsked { question_id: "q1".into(), questions: vec![] }]
        );
        let (_, st) = store
            .append("s1", &AgentEvent::QuestionAnswered { question_id: "q1".into(), answers: vec![] })
            .unwrap();
        assert_eq!(st, SessionStatus::Running);
        let (_, st) = store
            .append("s1", &AgentEvent::TurnEnded { outcome: TurnOutcome::Completed })
            .unwrap();
        assert_eq!(st, SessionStatus::Finished);
    }
}
