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
    crate::mcp::schema::MIGRATION,
    crate::schedules::schema::MIGRATION,
];

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
        let status = parse_status(&status).after(event);
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
        agent: AgentKind::parse(&agent).unwrap_or(AgentKind::Scripted),
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
}
