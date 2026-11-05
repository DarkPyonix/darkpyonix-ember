//! Channels (SPEC FR-T10, FR-T16, FR-L12; INTENT D22).
//!
//! A channel belongs to exactly one project. Every project has one default channel whose agent
//! members are all the project's sessions (computed when read, so a new session joins it); more
//! channels are created with a chosen subset of the project's sessions. The signed-in person is
//! a member of every channel as the person `user`. Messages are an append-only log per channel
//! in the main store (SQLite); the per-channel JSONL file of FR-T16, mentions (FR-T12, FR-T13),
//! the agent feed (FR-T11), invites (FR-T14, FR-T15, FR-T17), read tracking and push are not
//! built. Tables: [`MIGRATION`]. Routes: [`api`].

pub mod api;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::store::{now_ms, Store};

/// Store migration (appended to `store::MIGRATIONS`): channels, their agent members and the
/// message log. `seq` is the log order, unique across channels, so `?after=` is a plain
/// comparison.
pub const MIGRATION: &str = "
CREATE TABLE channels (
    id          TEXT PRIMARY KEY,
    project     TEXT NOT NULL REFERENCES projects(name),
    name        TEXT NOT NULL,
    is_default  INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL
);
CREATE UNIQUE INDEX channels_one_default ON channels(project) WHERE is_default = 1;
CREATE TABLE channel_agents (
    channel_id  TEXT NOT NULL REFERENCES channels(id),
    session_id  TEXT NOT NULL,
    PRIMARY KEY (channel_id, session_id)
);
CREATE TABLE channel_messages (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id  TEXT NOT NULL REFERENCES channels(id),
    author      TEXT NOT NULL,
    body        TEXT NOT NULL,
    at          INTEGER NOT NULL
);
CREATE INDEX channel_messages_by_channel ON channel_messages(channel_id, seq);
";

/// The person every channel includes.
pub const USER: &str = "user";
/// The name a default channel gets.
pub const DEFAULT_NAME: &str = "전체";
/// Messages in a channel detail (the latest ones); older ones come from `?after=`.
const DETAIL_LIMIT: i64 = 200;

#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    #[error("{0} not found")]
    NotFound(String),
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Forbidden(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<rusqlite::Error> for ChannelError {
    fn from(e: rusqlite::Error) -> Self {
        ChannelError::Other(e.into())
    }
}

/// A person or an agent session in a channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Member {
    /// `user` or an agent session id.
    pub id: String,
    pub name: String,
    pub is_agent: bool,
    /// The agent harness (`claude-code`, `codex`, ...); empty for a person.
    pub harness: String,
}

/// One log entry. `at` is Unix seconds; `unread` is 0 until read tracking exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Message {
    /// The log sequence number as text.
    pub id: String,
    pub seq: i64,
    pub from: String,
    pub text: String,
    pub at: i64,
    pub unread: u32,
}

/// A channel as the list and detail routes return it. `messages` is empty in a list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Channel {
    pub id: String,
    pub name: String,
    pub project: String,
    pub default: bool,
    pub members: Vec<Member>,
    pub last_message: Option<Message>,
    pub messages: Vec<Message>,
    pub unread: u32,
}

/// What a client sends to create a channel.
#[derive(Debug, Clone, Deserialize)]
pub struct NewChannel {
    pub project: String,
    pub name: String,
    /// Session ids of the project to include. None other becomes a member.
    #[serde(default)]
    pub agents: Vec<String>,
}

struct Row {
    id: String,
    name: String,
    project: String,
    default: bool,
}

fn msg(seq: i64, author: String, body: String, at_ms: i64) -> Message {
    Message { id: seq.to_string(), seq, from: author, text: body, at: at_ms / 1000, unread: 0 }
}

impl Store {
    /// Make sure every project has its default channel (FR-T10).
    fn ensure_default_channels(&self) -> Result<(), ChannelError> {
        self.conn().execute(
            "INSERT INTO channels (id, project, name, is_default, created_at)
             SELECT 'default-' || name, name, ?1, 1, ?2 FROM projects
             WHERE name NOT IN (SELECT project FROM channels WHERE is_default = 1)",
            params![DEFAULT_NAME, now_ms()],
        )?;
        Ok(())
    }

    fn channel_row(&self, id: &str) -> Result<Row, ChannelError> {
        self.ensure_default_channels()?;
        self.conn()
            .query_row("SELECT id, name, project, is_default FROM channels WHERE id = ?1", params![id], |r| {
                Ok(Row { id: r.get(0)?, name: r.get(1)?, project: r.get(2)?, default: r.get::<_, i64>(3)? != 0 })
            })
            .optional()?
            .ok_or_else(|| ChannelError::NotFound(format!("channel {id}")))
    }

    fn members_of(&self, row: &Row) -> Result<Vec<Member>, ChannelError> {
        let conn = self.conn();
        let sql = if row.default {
            "SELECT id, agent, title FROM sessions WHERE project = ?1 ORDER BY created_at, id"
        } else {
            "SELECT s.id, s.agent, s.title FROM channel_agents a JOIN sessions s ON s.id = a.session_id
             WHERE a.channel_id = ?1 ORDER BY s.created_at, s.id"
        };
        let key = if row.default { &row.project } else { &row.id };
        let mut stmt = conn.prepare(sql)?;
        let agents: Vec<(String, String, String)> =
            stmt.query_map(params![key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
        let mut out = vec![Member { id: USER.into(), name: "나".into(), is_agent: false, harness: String::new() }];
        out.extend(agents.into_iter().map(|(id, harness, title)| Member {
            name: if title.trim().is_empty() { harness.clone() } else { title },
            id,
            is_agent: true,
            harness,
        }));
        Ok(out)
    }

    fn last_message(&self, channel: &str) -> Result<Option<Message>, ChannelError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT seq, author, body, at FROM channel_messages WHERE channel_id = ?1 ORDER BY seq DESC LIMIT 1",
                params![channel],
                |r| Ok(msg(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?)
    }

    fn build(&self, row: Row, with_messages: bool) -> Result<Channel, ChannelError> {
        let members = self.members_of(&row)?;
        let last_message = self.last_message(&row.id)?;
        let messages = if with_messages {
            let mut m = self.channel_messages_after(&row.id, 0, Some(DETAIL_LIMIT), true)?;
            m.reverse();
            m
        } else {
            Vec::new()
        };
        Ok(Channel {
            id: row.id,
            name: row.name,
            project: row.project,
            default: row.default,
            members,
            last_message,
            messages,
            unread: 0,
        })
    }

    /// Every channel, grouped by project order then creation; default channels first.
    pub fn channels(&self, project: Option<&str>) -> Result<Vec<Channel>, ChannelError> {
        self.ensure_default_channels()?;
        let rows: Vec<Row> = {
            let conn = self.conn();
            let mut stmt = conn.prepare(
                "SELECT id, name, project, is_default FROM channels
                 WHERE (?1 IS NULL OR project = ?1) ORDER BY project, is_default DESC, created_at, id",
            )?;
            let v = stmt
                .query_map(params![project], |r| {
                    Ok(Row { id: r.get(0)?, name: r.get(1)?, project: r.get(2)?, default: r.get::<_, i64>(3)? != 0 })
                })?
                .collect::<Result<_, _>>()?;
            v
        };
        rows.into_iter().map(|r| self.build(r, false)).collect()
    }

    /// One channel with its latest messages.
    pub fn channel(&self, id: &str) -> Result<Channel, ChannelError> {
        let row = self.channel_row(id)?;
        self.build(row, true)
    }

    /// Create a non-default channel with `agents` (session ids of the project) and no others.
    pub fn create_channel(&self, new: NewChannel) -> Result<Channel, ChannelError> {
        let name = new.name.trim();
        if name.is_empty() {
            return Err(ChannelError::BadRequest("channel name must not be empty".into()));
        }
        if self.project(&new.project)?.is_none() {
            return Err(ChannelError::NotFound(format!("project {:?}", new.project)));
        }
        let id = uuid::Uuid::new_v4().to_string();
        {
            let mut conn = self.conn();
            let tx = conn.transaction()?;
            for sid in &new.agents {
                let project: Option<String> = tx
                    .query_row("SELECT project FROM sessions WHERE id = ?1", params![sid], |r| r.get(0))
                    .optional()?;
                match project {
                    None => return Err(ChannelError::NotFound(format!("session {sid}"))),
                    Some(p) if p != new.project => {
                        return Err(ChannelError::BadRequest(format!(
                            "session {sid} belongs to project {p:?}, not {:?}",
                            new.project
                        )))
                    }
                    Some(_) => {}
                }
            }
            tx.execute(
                "INSERT INTO channels (id, project, name, is_default, created_at) VALUES (?1, ?2, ?3, 0, ?4)",
                params![id, new.project, name, now_ms()],
            )?;
            for sid in &new.agents {
                tx.execute(
                    "INSERT OR IGNORE INTO channel_agents (channel_id, session_id) VALUES (?1, ?2)",
                    params![id, sid],
                )?;
            }
            tx.commit()?;
        }
        self.channel(&id)
    }

    /// Append a message. `from` is `user` or a member agent's session id.
    pub fn post_channel_message(&self, channel: &str, from: &str, text: &str) -> Result<Message, ChannelError> {
        if text.trim().is_empty() {
            return Err(ChannelError::BadRequest("message text must not be empty".into()));
        }
        let row = self.channel_row(channel)?;
        if !self.members_of(&row)?.iter().any(|m| m.id == from) {
            return Err(ChannelError::Forbidden(format!("{from} is not a member of channel {channel}")));
        }
        let at = now_ms();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO channel_messages (channel_id, author, body, at) VALUES (?1, ?2, ?3, ?4)",
            params![channel, from, text, at],
        )?;
        Ok(msg(conn.last_insert_rowid(), from.to_string(), text.to_string(), at))
    }

    /// Messages with `seq > after`, oldest first (or the newest `limit` when `newest`).
    pub fn channel_messages_after(
        &self,
        channel: &str,
        after: i64,
        limit: Option<i64>,
        newest: bool,
    ) -> Result<Vec<Message>, ChannelError> {
        let conn = self.conn();
        let sql = format!(
            "SELECT seq, author, body, at FROM channel_messages WHERE channel_id = ?1 AND seq > ?2 ORDER BY seq {} LIMIT ?3",
            if newest { "DESC" } else { "ASC" }
        );
        let mut stmt = conn.prepare(&sql)?;
        let v = stmt
            .query_map(params![channel, after, limit.unwrap_or(-1)], |r| {
                Ok(msg(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }

    /// Messages after `after` for a known channel (404 otherwise).
    pub fn read_channel_messages(&self, channel: &str, after: i64) -> Result<Vec<Message>, ChannelError> {
        self.channel_row(channel)?;
        self.channel_messages_after(channel, after, None, false)
    }
}
