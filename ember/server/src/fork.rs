//! Session fork (SPEC FR-S5).
//!
//! A fork is a new session that starts from a completed turn of another one. Ember copies the
//! parent's stored events up to and including that turn's `turn_ended` into the new session, and
//! records the parent and the turn in `session_forks`. No wrapped agent forks natively from an
//! arbitrary turn in its headless mode, so the fork starts a fresh agent session and the first
//! message sent to it carries the copied conversation as a one-time system notice (the same
//! channel as FR-S7). The agent's own native id is not copied: the fork never resumes the parent.

use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use crate::events::{AgentEvent, TurnOutcome};
use crate::store::{now_ms, SessionRecord, Store};

/// Migration from `user_version` 14 to 15: where a forked session came from.
/// `seeded` is 1 until the copied conversation has been put in front of the agent.
pub const MIGRATION: &str = "
CREATE TABLE session_forks (
    session_id  TEXT PRIMARY KEY REFERENCES sessions(id),
    parent_id   TEXT NOT NULL,
    parent_seq  INTEGER NOT NULL,
    forked_at   INTEGER NOT NULL,
    seeded      INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX session_forks_by_parent ON session_forks(parent_id);
";

/// Migration from `user_version` 15 to 16: a native fork waiting for the fork's first message.
/// `native_pending` is 1 until the agent has forked (or failed to and the copy took over);
/// `parent_native_id` and `keep_turns` (NULL = the whole session) say what to fork.
pub const NATIVE_MIGRATION: &str = "
ALTER TABLE session_forks ADD COLUMN native_pending INTEGER NOT NULL DEFAULT 0;
ALTER TABLE session_forks ADD COLUMN parent_native_id TEXT;
ALTER TABLE session_forks ADD COLUMN keep_turns INTEGER;
";

/// Upper bound of the transcript put in front of the agent, in characters (the newest are kept).
pub const SEED_MAX_CHARS: usize = 100_000;

/// Where a fork came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForkOrigin {
    pub parent_id: String,
    /// Sequence number, in the parent, of the `turn_ended` event the fork starts after.
    pub turn_seq: i64,
    pub forked_at: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum ForkError {
    #[error("session {0} not found")]
    NotFound(String),
    /// The chosen point is not a completed turn, or the session has none.
    #[error("{0}")]
    BadTurn(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Whether and how a fork at the `ordinal`-th completed turn (1-based) of a parent with `total`
/// completed turns can use the agent's own fork (FR-S5). `trailing` is true when the parent has a
/// user message after that turn, which a whole-session fork would carry along.
pub fn plan_native_fork(
    mode: crate::agents::NativeFork,
    parent_native_id: Option<&str>,
    ordinal: usize,
    total: usize,
    trailing: bool,
) -> Option<crate::agents::ForkFrom> {
    use crate::agents::{ForkFrom, NativeFork};
    let native_id = parent_native_id?.to_string();
    match mode {
        NativeFork::None => None,
        NativeFork::AtTurn => Some(ForkFrom { native_id, keep_turns: Some(ordinal) }),
        NativeFork::Latest if ordinal == total && !trailing => Some(ForkFrom { native_id, keep_turns: None }),
        NativeFork::Latest => None,
    }
}

/// Sequence numbers of the completed turns (`turn_ended`, outcome completed), oldest first.
pub fn completed_turns(events: &[crate::store::StoredEvent]) -> Vec<i64> {
    events
        .iter()
        .filter(|e| matches!(e.event, AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }))
        .map(|e| e.seq)
        .collect()
}

/// The conversation as plain text, user and assistant messages only.
pub fn transcript(events: &[crate::store::StoredEvent]) -> String {
    let mut out = String::new();
    for e in events {
        match &e.event {
            AgentEvent::UserMessage { text } => out.push_str(&format!("User: {text}\n\n")),
            AgentEvent::AssistantMessage { text } => out.push_str(&format!("Assistant: {text}\n\n")),
            _ => {}
        }
    }
    let total = out.chars().count();
    if total > SEED_MAX_CHARS {
        out = out.chars().skip(total - SEED_MAX_CHARS).collect();
    }
    out.trim_end().to_string()
}

impl Store {
    /// Fork `parent` after its completed turn `turn_seq` (the last completed turn when `None`).
    pub fn fork_session(
        &self,
        parent: &str,
        turn_seq: Option<i64>,
        title: Option<&str>,
    ) -> Result<SessionRecord, ForkError> {
        let rec = self.session(parent)?.ok_or_else(|| ForkError::NotFound(parent.to_string()))?;
        let events = self.events_after(parent, 0)?;
        let turns = completed_turns(&events);
        let at = match turn_seq {
            Some(s) if turns.contains(&s) => s,
            Some(s) => return Err(ForkError::BadTurn(format!("seq {s} is not a completed turn of this session"))),
            None => *turns
                .last()
                .ok_or_else(|| ForkError::BadTurn("the session has no completed turn to fork from".into()))?,
        };
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_ms();
        let title = title.map(str::trim).filter(|t| !t.is_empty()).map(str::to_string);
        let title = title.unwrap_or_else(|| format!("Fork of {}", rec.title));
        let copied: Vec<_> = events
            .iter()
            .filter(|e| e.seq <= at && !matches!(e.event, AgentEvent::NativeSession { .. }))
            .collect();
        {
            let mut conn = self.conn();
            let tx = conn.transaction().map_err(anyhow::Error::from)?;
            let run = |tx: &rusqlite::Transaction<'_>| -> rusqlite::Result<()> {
                tx.execute(
                    "INSERT INTO sessions (id, project, agent, cwd, model, status, title, created_at, updated_at, last_seq, account_id, account_reason)
                     SELECT ?2, project, agent, cwd, model, 'finished', ?3, ?4, ?4, ?5, account_id, account_reason
                     FROM sessions WHERE id = ?1",
                    params![parent, id, title, now, copied.len() as i64],
                )?;
                for (i, e) in copied.iter().enumerate() {
                    tx.execute(
                        "INSERT INTO events (session_id, seq, at, event) VALUES (?1, ?2, ?3, ?4)",
                        params![id, (i + 1) as i64, e.at, serde_json::to_string(&e.event).unwrap_or_default()],
                    )?;
                }
                tx.execute(
                    "INSERT INTO session_forks (session_id, parent_id, parent_seq, forked_at) VALUES (?1, ?2, ?3, ?4)",
                    params![id, parent, at, now],
                )?;
                Ok(())
            };
            run(&tx).map_err(anyhow::Error::from)?;
            tx.commit().map_err(anyhow::Error::from)?;
        }
        Ok(self.session(&id)?.expect("just inserted"))
    }

    /// Mark `session` (a fork) as forking natively at its first message: the copied conversation
    /// is not put in front of the agent.
    pub fn set_native_fork(&self, session: &str, from: &crate::agents::ForkFrom) -> anyhow::Result<()> {
        self.conn().execute(
            "UPDATE session_forks SET native_pending = 1, seeded = 0, parent_native_id = ?2, keep_turns = ?3
             WHERE session_id = ?1",
            params![session, from.native_id, from.keep_turns.map(|n| n as i64)],
        )?;
        Ok(())
    }

    /// The native fork `session` still has to make, if any.
    pub fn pending_native_fork(&self, session: &str) -> anyhow::Result<Option<crate::agents::ForkFrom>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT parent_native_id, keep_turns FROM session_forks
                 WHERE session_id = ?1 AND native_pending = 1 AND parent_native_id IS NOT NULL",
                params![session],
                |r| {
                    Ok(crate::agents::ForkFrom {
                        native_id: r.get(0)?,
                        keep_turns: r.get::<_, Option<i64>>(1)?.map(|n| n as usize),
                    })
                },
            )
            .optional()?)
    }

    /// The native fork is done (`ok`), or failed: then the copied conversation is put in front of
    /// the agent after all.
    pub fn settle_native_fork(&self, session: &str, ok: bool) -> anyhow::Result<()> {
        self.conn().execute(
            "UPDATE session_forks SET native_pending = 0, seeded = ?2 WHERE session_id = ?1",
            params![session, if ok { 0 } else { 1 }],
        )?;
        Ok(())
    }

    /// The parent and turn `session` was forked from, if it is a fork.
    pub fn fork_origin(&self, session: &str) -> anyhow::Result<Option<ForkOrigin>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT parent_id, parent_seq, forked_at FROM session_forks WHERE session_id = ?1",
                params![session],
                |r| Ok(ForkOrigin { parent_id: r.get(0)?, turn_seq: r.get(1)?, forked_at: r.get(2)? }),
            )
            .optional()?)
    }

    /// Sessions forked from `parent`, newest first.
    pub fn forks_of(&self, parent: &str) -> anyhow::Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT session_id FROM session_forks WHERE parent_id = ?1 ORDER BY forked_at DESC")?;
        let rows = stmt.query_map(params![parent], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// For a fork whose copied conversation has not reached its agent yet: the note to put in
    /// front of the next message. Returned once; `None` for every later call and for non-forks.
    pub fn take_fork_seed(&self, session: &str) -> anyhow::Result<Option<String>> {
        let taken = self.conn().execute(
            "UPDATE session_forks SET seeded = 0 WHERE session_id = ?1 AND seeded = 1",
            params![session],
        )?;
        if taken == 0 {
            return Ok(None);
        }
        let text = transcript(&self.events_after(session, 0)?);
        if text.is_empty() {
            return Ok(None);
        }
        Ok(Some(format!(
            "This conversation is a fork of an earlier session. The earlier conversation up to the fork point follows; continue from it.\n\n{text}"
        )))
    }
}
