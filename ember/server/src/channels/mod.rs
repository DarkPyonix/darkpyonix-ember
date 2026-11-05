//! Channels (SPEC FR-T10, FR-T16, FR-L12; INTENT D22).
//!
//! A channel belongs to exactly one project. Every project has one default channel whose agent
//! members are all the project's sessions (computed when read, so a new session joins it); more
//! channels are created with a chosen subset of the project's sessions. The signed-in person is
//! a member of every channel as the person `user`. Messages are an append-only log per channel
//! in the main store (SQLite), and human messages are also appended to a per-channel JSONL file
//! ([`jsonl`], FR-T16); mentions are parsed and gated ([`mention`], FR-T12, FR-T13) and delivered to
//! the mentioned agents through A2A ([`api`]); invites (FR-T14) have their store and owner routes ([`invites`]); accepting one and the guest routes are in [`guest`] (not served by any listener yet); guest mention delivery (FR-T15) and allowances (FR-T17) are not built. Read tracking is one
//! cursor per channel for the person `user` ([`READS_MIGRATION`]). Tables: [`MIGRATION`]. The
//! agent feed (FR-T11) is derived from session events: [`feed`]. Routes: [`api`].

pub mod api;
pub mod feed;
pub mod guest;
pub mod invites;
pub mod jsonl;
pub mod mention;

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

/// Store migration (appended after [`MIGRATION`]): the read cursor per channel and reader.
/// `reader` is `user` for the one local person; per-person readers wait on FR-T14.
pub const READS_MIGRATION: &str = "
CREATE TABLE channel_reads (
    channel_id  TEXT NOT NULL REFERENCES channels(id),
    reader      TEXT NOT NULL,
    last_seq    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (channel_id, reader)
);
";

/// Store migration (appended last): the session ids a message mentions, as a JSON array
/// (FR-T12), so delivery can read them later. Empty for human conversation.
pub const MENTIONS_MIGRATION: &str = "
ALTER TABLE channel_messages ADD COLUMN mentions TEXT NOT NULL DEFAULT '[]';
";

/// The person every channel includes.
pub const USER: &str = "user";
/// The name the person `user` has in a channel, also used in the header an agent sees (FR-T12).
pub const USER_LABEL: &str = "나";
/// The author of a message the server writes into a channel, such as a delivery failure
/// (FR-T13). Not a member, not an agent session id, and never written to the JSONL (FR-T16).
pub const SYSTEM: &str = "system";
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
    /// A mention names no member agent (FR-T13); carries the names.
    #[error("not a member of this channel: {}", .0.join(", "))]
    NotAMember(Vec<String>),
    /// A refusal with its own machine code: `invalid_name` (400), `rate_limited` and
    /// `quota_exceeded` (429).
    #[error("{1}")]
    Coded(&'static str, String),
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

/// One log entry. `at` is Unix seconds; `unread` is 1 when the message is after the
/// `user` read cursor and was not written by `user`, else 0.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Message {
    /// The log sequence number as text.
    pub id: String,
    pub seq: i64,
    pub from: String,
    pub text: String,
    pub at: i64,
    pub unread: u32,
    /// Session ids of the member agents this message mentions (FR-T12); empty for human
    /// conversation.
    pub mentions: Vec<String>,
    /// Who wrote it, for display (FR-T15): `나` for `user`, the member name for an agent, the
    /// guest's display name for `guest:<id>`, `system` for `system`. Derived at read time (a
    /// rename shows at once); `from` stays the stable id. Additive.
    pub author_name: String,
    /// `person`, `agent`, `guest` or `system`. Additive.
    pub author_kind: String,
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

fn msg(seq: i64, author: String, body: String, at_ms: i64, last_seq: i64, mentions: &str) -> Message {
    let unread = u32::from(seq > last_seq && author != USER);
    // Names of agents and guests need the channel's members and guests; `Store::annotate` fills them.
    let (author_name, author_kind) = if author == USER {
        (USER_LABEL, "person")
    } else if author == SYSTEM {
        (SYSTEM, "system")
    } else if author.starts_with(guest::AUTHOR_PREFIX) {
        ("guest", "guest")
    } else {
        ("agent", "agent")
    };
    Message {
        id: seq.to_string(),
        seq,
        from: author,
        text: body,
        at: at_ms / 1000,
        unread,
        mentions: serde_json::from_str(mentions).unwrap_or_default(),
        author_name: author_name.to_string(),
        author_kind: author_kind.to_string(),
    }
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
        let mut out = vec![Member { id: USER.into(), name: USER_LABEL.into(), is_agent: false, harness: String::new() }];
        out.extend(agents.into_iter().map(|(id, harness, title)| Member {
            name: if title.trim().is_empty() { harness.clone() } else { title },
            id,
            is_agent: true,
            harness,
        }));
        Ok(out)
    }

    /// The `user` read cursor of a channel (0 when never marked).
    fn read_cursor(&self, channel: &str) -> Result<i64, ChannelError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT last_seq FROM channel_reads WHERE channel_id = ?1 AND reader = ?2",
                params![channel, USER],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    /// Messages after the cursor that `user` did not write.
    fn unread_count(&self, channel: &str) -> Result<u32, ChannelError> {
        let last = self.read_cursor(channel)?;
        let n: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM channel_messages WHERE channel_id = ?1 AND seq > ?2 AND author != ?3",
            params![channel, last, USER],
            |r| r.get(0),
        )?;
        Ok(n as u32)
    }

    /// Mark messages up to `seq` (default the latest) as read by `user`. The cursor never moves
    /// back and never passes the latest message. Returns the channel without its log.
    pub fn mark_channel_read(&self, channel: &str, seq: Option<i64>) -> Result<Channel, ChannelError> {
        let row = self.channel_row(channel)?;
        {
            let conn = self.conn();
            let latest: i64 = conn.query_row(
                "SELECT COALESCE(MAX(seq), 0) FROM channel_messages WHERE channel_id = ?1",
                params![channel],
                |r| r.get(0),
            )?;
            let to = seq.unwrap_or(latest).clamp(0, latest);
            conn.execute(
                "INSERT INTO channel_reads (channel_id, reader, last_seq) VALUES (?1, ?2, ?3)
                 ON CONFLICT(channel_id, reader) DO UPDATE SET last_seq = MAX(last_seq, excluded.last_seq)",
                params![channel, USER, to],
            )?;
        }
        self.build(row, false)
    }

    fn last_message(&self, channel: &str) -> Result<Option<Message>, ChannelError> {
        let last = self.read_cursor(channel)?;
        let found = self
            .conn()
            .query_row(
                "SELECT seq, author, body, at, mentions FROM channel_messages WHERE channel_id = ?1 ORDER BY seq DESC LIMIT 1",
                params![channel],
                |r| Ok(msg(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, last, &r.get::<_, String>(4)?)),
            )
            .optional()?;
        let mut found: Vec<Message> = found.into_iter().collect();
        self.annotate(channel, &mut found)?;
        Ok(found.pop())
    }

    /// Fill `author_name` of agent and guest messages (FR-T15): the member list is read once and
    /// the guest names in one query, and only when the page holds a guest message. Must not be
    /// called with the connection held.
    pub(crate) fn annotate(&self, channel: &str, messages: &mut [Message]) -> Result<(), ChannelError> {
        if !messages.iter().any(|m| m.author_kind == "agent" || m.author_kind == "guest") {
            return Ok(());
        }
        let members = if messages.iter().any(|m| m.author_kind == "agent") {
            self.members_of(&self.channel_row(channel)?)?
        } else {
            Vec::new()
        };
        let mut guests: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        if messages.iter().any(|m| m.author_kind == "guest") {
            let conn = self.conn();
            let mut stmt = conn.prepare("SELECT id, name FROM channel_guests WHERE channel_id = ?1")?;
            let rows = stmt.query_map(params![channel], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            for row in rows {
                let (id, name) = row?;
                guests.insert(id, name);
            }
        }
        for m in messages.iter_mut() {
            match m.author_kind.as_str() {
                "agent" => {
                    if let Some(x) = members.iter().find(|x| x.id == m.from) {
                        m.author_name = x.name.clone();
                    }
                }
                "guest" => {
                    if let Some(name) = m.from.strip_prefix(guest::AUTHOR_PREFIX).and_then(|id| guests.get(id)) {
                        m.author_name = name.clone();
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn build(&self, row: Row, with_messages: bool) -> Result<Channel, ChannelError> {
        let members = self.members_of(&row)?;
        let last_message = self.last_message(&row.id)?;
        let unread = self.unread_count(&row.id)?;
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
            unread,
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
        // Only the person's `@name` is gated and resolved for now (FR-T12, FR-T13); an agent's
        // message is stored as written.
        let mentions: Vec<String> = if from == USER {
            let found = mention::parse_mentions(text, &self.members_of(&row)?);
            if !found.unknown.is_empty() {
                return Err(ChannelError::NotAMember(found.unknown));
            }
            found.mentioned.into_iter().map(|m| m.id).collect()
        } else {
            Vec::new()
        };
        let mentions = serde_json::to_string(&mentions).map_err(anyhow::Error::from)?;
        let at = now_ms();
        let last = self.read_cursor(channel)?;
        let message = {
            let conn = self.conn();
            conn.execute(
                "INSERT INTO channel_messages (channel_id, author, body, at, mentions) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![channel, from, text, at, mentions],
            )?;
            msg(conn.last_insert_rowid(), from.to_string(), text.to_string(), at, last, &mentions)
        };
        let mut one = [message];
        self.annotate(channel, &mut one)?;
        let [message] = one;
        Ok(message)
    }

    /// Append a message from [`SYSTEM`], e.g. "not reached" (FR-T13). It skips the member check
    /// and mention parsing, and the caller writes no JSONL line for it.
    pub fn post_channel_system_message(&self, channel: &str, text: &str) -> Result<Message, ChannelError> {
        self.channel_row(channel)?;
        let at = now_ms();
        let last = self.read_cursor(channel)?;
        let conn = self.conn();
        conn.execute(
            "INSERT INTO channel_messages (channel_id, author, body, at) VALUES (?1, ?2, ?3, ?4)",
            params![channel, SYSTEM, text, at],
        )?;
        Ok(msg(conn.last_insert_rowid(), SYSTEM.to_string(), text.to_string(), at, last, "[]"))
    }

    /// The members of a channel (404 when it does not exist).
    pub fn channel_members(&self, channel: &str) -> Result<Vec<Member>, ChannelError> {
        let row = self.channel_row(channel)?;
        self.members_of(&row)
    }

    /// The name of a channel, if it exists.
    pub fn channel_name(&self, channel: &str) -> Option<String> {
        self.channel_row(channel).ok().map(|r| r.name)
    }

    /// Remove one message row. Used only to undo a post whose JSONL append failed (FR-T16).
    pub fn delete_channel_message(&self, seq: i64) -> Result<(), ChannelError> {
        self.conn().execute("DELETE FROM channel_messages WHERE seq = ?1", params![seq])?;
        Ok(())
    }

    /// Messages with `seq > after`, oldest first (or the newest `limit` when `newest`).
    pub fn channel_messages_after(
        &self,
        channel: &str,
        after: i64,
        limit: Option<i64>,
        newest: bool,
    ) -> Result<Vec<Message>, ChannelError> {
        let last = self.read_cursor(channel)?;
        let mut v: Vec<Message> = {
        let conn = self.conn();
        let sql = format!(
            "SELECT seq, author, body, at, mentions FROM channel_messages WHERE channel_id = ?1 AND seq > ?2 ORDER BY seq {} LIMIT ?3",
            if newest { "DESC" } else { "ASC" }
        );
        let mut stmt = conn.prepare(&sql)?;
        let v = stmt
            .query_map(params![channel, after, limit.unwrap_or(-1)], |r| {
                Ok(msg(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, last, &r.get::<_, String>(4)?))
            })?
            .collect::<Result<_, _>>()?;
        v
        };
        self.annotate(channel, &mut v)?;
        Ok(v)
    }

    /// Messages after `after` for a known channel (404 otherwise).
    pub fn read_channel_messages(&self, channel: &str, after: i64) -> Result<Vec<Message>, ChannelError> {
        self.channel_row(channel)?;
        self.channel_messages_after(channel, after, None, false)
    }

    /// Which agents `text` would reach and which names match none (FR-T12). Stores nothing.
    pub fn preview_channel_mentions(&self, channel: &str, text: &str) -> Result<mention::Mentions, ChannelError> {
        let row = self.channel_row(channel)?;
        Ok(mention::parse_mentions(text, &self.members_of(&row)?))
    }
}
