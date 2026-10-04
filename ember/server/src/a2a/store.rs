//! A2A tables (SPEC FR-T2, FR-T4–FR-T6): runtime tokens, the durable message queue and the
//! on/off switches.
//!
//! They live in the same SQLite file as the session store but on their own connection, so the
//! session schema is untouched. Message rows double as the sliding-window history for loop
//! protection, so the limits also survive a restart.

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
CREATE TABLE IF NOT EXISTS a2a_tokens (
    session_id  TEXT PRIMARY KEY,
    token_hash  TEXT NOT NULL UNIQUE,
    created_at  INTEGER NOT NULL
);
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
CREATE TABLE IF NOT EXISTS a2a_switches (
    scope    TEXT PRIMARY KEY,
    enabled  INTEGER NOT NULL
);
";

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
}

pub struct A2aStore {
    conn: Mutex<Connection>,
}

impl A2aStore {
    /// Open the A2A tables in the server's database file (`ember.db`).
    pub fn open(path: &Path) -> anyhow::Result<A2aStore> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        Ok(A2aStore {
            conn: Mutex::new(conn),
        })
    }

    pub fn open_in_memory() -> anyhow::Result<A2aStore> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(A2aStore {
            conn: Mutex::new(conn),
        })
    }

    /// Issue a fresh runtime token for `session_id`, replacing any earlier one. Only its hash is
    /// stored.
    pub fn issue_token(&self, session_id: &str) -> anyhow::Result<String> {
        let token = format!(
            "ember_rt_{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        self.conn.lock().unwrap().execute(
            "INSERT INTO a2a_tokens (session_id, token_hash, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (session_id) DO UPDATE SET token_hash = ?2, created_at = ?3",
            params![session_id, hash_token(&token), now_ms()],
        )?;
        Ok(token)
    }

    /// The session a runtime token belongs to.
    pub fn session_for_token(&self, token: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT session_id FROM a2a_tokens WHERE token_hash = ?1",
                params![hash_token(token)],
                |r| r.get(0),
            )
            .optional()?)
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
        };
        self.conn.lock().unwrap().execute(
            "INSERT INTO a2a_messages (id, from_session, to_session, reply_to, text, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                msg.id,
                msg.from_session,
                msg.to_session,
                msg.reply_to,
                msg.text,
                msg.created_at
            ],
        )?;
        Ok(msg)
    }

    pub fn message(&self, id: &str) -> anyhow::Result<Option<A2aMessage>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT id, from_session, to_session, reply_to, text, created_at, delivered_at
                 FROM a2a_messages WHERE id = ?1",
                params![id],
                row_to_message,
            )
            .optional()?)
    }

    /// Undelivered messages to `to`, oldest first.
    pub fn pending_for(&self, to: &str) -> anyhow::Result<Vec<A2aMessage>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, from_session, to_session, reply_to, text, created_at, delivered_at
             FROM a2a_messages WHERE to_session = ?1 AND delivered_at IS NULL
             ORDER BY created_at, rowid",
        )?;
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
