//! Agent teams (SPEC FR-T7): a leader session spawns teammate sessions, and the team shares a
//! task list and a mailbox, all stored in the server database.
//!
//! - **Leader**: the first `team spawn` makes the calling session the leader of a new team.
//!   Only the leader spawns and ends teammates. A session leads at most one team and is never
//!   both a leader and a teammate (no nested teams).
//! - **Teammates** are ordinary sessions in the leader's project, created with the agent,
//!   account and computer the leader chose and nothing else from the leader (no model, no
//!   transcript, no approvals: each teammate keeps its own approvals, answered in its own
//!   session). The first prompt arrives as an A2A message from the leader (FR-T3), so the
//!   teammate can reply with one call.
//! - **Messaging rule**: an active teammate can be messaged only by its own team, and can
//!   message only its own team. Everyone else (including leaders) is unaffected. Ended
//!   teammates are ordinary sessions again.
//! - **Tasks**: any active member adds tasks. The leader assigns and updates any task; a
//!   teammate updates only tasks that are unassigned or its own, and assigns only to itself
//!   (claiming). An assignment to someone else, and a teammate's status change, are announced
//!   to the one concerned through the A2A queue (best effort, under loop protection).
//! - **Mailbox**: `mail send` to one member or to the whole team stores the mail and delivers
//!   it through the A2A queue, so it wakes the recipient and survives a restart (FR-T4). Loop
//!   protection counts one message per recipient (FR-T5).
//!
//! Every change pushes the whole team as [`Push::TeamUpdated`].

use std::collections::HashMap;
use std::sync::Arc;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::{A2a, A2aError, Receipt, CLI_NAME};
use crate::agents::AgentKind;
use crate::events::{AgentEvent, SessionStatus};
use crate::session::{NewSession, Push, SessionError, PUSH_VERSION};
use crate::store::{now_ms, SessionPatch, SessionRecord, Store};

/// Store migration: teams, members, tasks and mail (appended to `store::MIGRATIONS`).
///
/// `team_members.session_id` is the primary key, so a session belongs to at most one team, as
/// leader or teammate, for good (an ended teammate keeps its row for the record). Names are
/// unique among a team's active members only.
pub const MIGRATION: &str = "
CREATE TABLE teams (
    id              TEXT PRIMARY KEY,
    leader_session  TEXT NOT NULL UNIQUE,
    project         TEXT NOT NULL,
    created_at      INTEGER NOT NULL
);
CREATE TABLE team_members (
    session_id  TEXT PRIMARY KEY,
    team_id     TEXT NOT NULL REFERENCES teams(id),
    name        TEXT NOT NULL,
    role        TEXT NOT NULL,
    joined_at   INTEGER NOT NULL,
    ended_at    INTEGER
);
CREATE INDEX team_members_by_team ON team_members (team_id);
CREATE UNIQUE INDEX team_members_active_name ON team_members (team_id, name) WHERE ended_at IS NULL;
CREATE TABLE team_tasks (
    id          TEXT PRIMARY KEY,
    team_id     TEXT NOT NULL REFERENCES teams(id),
    number      INTEGER NOT NULL,
    title       TEXT NOT NULL,
    detail      TEXT NOT NULL DEFAULT '',
    status      TEXT NOT NULL,
    assignee    TEXT,
    created_by  TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    UNIQUE (team_id, number)
);
CREATE TABLE team_mail (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    team_id       TEXT NOT NULL REFERENCES teams(id),
    from_session  TEXT NOT NULL,
    to_session    TEXT,
    text          TEXT NOT NULL,
    created_at    INTEGER NOT NULL
);
CREATE INDEX team_mail_by_team ON team_mail (team_id, id);
";

/// Active teammates per team, leader not counted.
pub const MAX_TEAMMATES: usize = 8;
/// The leader's member name.
pub const LEADER_NAME: &str = "lead";
/// Default and maximum number of mails one read returns.
pub const MAIL_PAGE: usize = 20;
pub const MAIL_PAGE_MAX: usize = 200;
const NAME_MAX: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Leader,
    Teammate,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::Leader => "leader",
            Role::Teammate => "teammate",
        }
    }

    fn parse(s: &str) -> Role {
        if s == "leader" {
            Role::Leader
        } else {
            Role::Teammate
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Open,
    InProgress,
    Blocked,
    Done,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Open => "open",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Done => "done",
            TaskStatus::Cancelled => "cancelled",
        }
    }

    /// Accepts `in_progress`, `in-progress` and `canceled` too.
    pub fn parse(s: &str) -> Option<TaskStatus> {
        match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "open" | "todo" => Some(TaskStatus::Open),
            "in_progress" | "doing" => Some(TaskStatus::InProgress),
            "blocked" => Some(TaskStatus::Blocked),
            "done" => Some(TaskStatus::Done),
            "cancelled" | "canceled" => Some(TaskStatus::Cancelled),
            _ => None,
        }
    }
}

/// A `team_members` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRow {
    pub session_id: String,
    pub team_id: String,
    pub name: String,
    pub role: Role,
    pub joined_at: i64,
    pub ended_at: Option<i64>,
}

impl MemberRow {
    pub fn active(&self) -> bool {
        self.ended_at.is_none()
    }

    pub fn is_active_teammate(&self) -> bool {
        self.active() && self.role == Role::Teammate
    }
}

/// A `teams` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamRow {
    pub id: String,
    pub leader_session: String,
    pub project: String,
    pub created_at: i64,
}

/// A team member as clients and agents see it. `title`, `agent` and `status` are a snapshot of
/// the session record when the view was built; clients take live status from their sessions.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Member {
    pub session_id: String,
    pub name: String,
    pub role: Role,
    pub joined_at: i64,
    pub ended_at: Option<i64>,
    pub title: String,
    pub agent: String,
    pub status: Option<SessionStatus>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Task {
    pub id: String,
    pub team_id: String,
    /// Short number within the team (`#3`), what agents and the CLI use.
    pub number: i64,
    pub title: String,
    pub detail: String,
    pub status: TaskStatus,
    /// Assignee session id.
    pub assignee: Option<String>,
    /// The assignee's member name, when it is (or was) a member.
    pub assignee_name: Option<String>,
    pub created_by: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Mail {
    /// Increasing mail number (server-wide), for `mail read --after`.
    pub id: i64,
    pub team_id: String,
    pub from_session: String,
    pub from_name: Option<String>,
    /// `None` = the whole team.
    pub to_session: Option<String>,
    pub to_name: Option<String>,
    pub text: String,
    pub created_at: i64,
}

/// A team with its members (leader first, then teammates in joining order, ended ones
/// included) and its tasks (by number).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TeamView {
    pub id: String,
    pub project: String,
    /// The leader's session id.
    pub leader: String,
    pub created_at: i64,
    pub members: Vec<Member>,
    pub tasks: Vec<Task>,
}

/// `POST /a2a/team/members`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SpawnRequest {
    pub name: String,
    /// Agent kind (`claude-code`, `codex`); default: the leader's.
    #[serde(default)]
    pub agent: Option<String>,
    /// Account id; default: the account router's choice (FR-U2).
    #[serde(default)]
    pub account: Option<String>,
    /// Computer id; default: the main server (`local`).
    #[serde(default)]
    pub computer: Option<String>,
    /// Session title; default `<name> · <leader title>`.
    #[serde(default)]
    pub title: Option<String>,
    /// The teammate's first message.
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpawnOutcome {
    pub team_id: String,
    pub member: Member,
    /// The first prompt, as an A2A message from the leader.
    pub message: Receipt,
}

/// `POST /a2a/team/tasks`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NewTask {
    pub title: String,
    #[serde(default)]
    pub detail: Option<String>,
    /// Member name or session id.
    #[serde(default)]
    pub assignee: Option<String>,
}

/// `PATCH /a2a/team/tasks/{number}`; `None` leaves a field alone.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TaskPatch {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    /// Member name or session id; `""` or `none` unassigns.
    #[serde(default)]
    pub assignee: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MailReceipt {
    pub mail: Mail,
    /// One per recipient.
    pub deliveries: Vec<Receipt>,
}

// ---- store ------------------------------------------------------------------------------------

const MEMBER_COLUMNS: &str = "session_id, team_id, name, role, joined_at, ended_at";
const TASK_COLUMNS: &str =
    "id, team_id, number, title, detail, status, assignee, created_by, created_at, updated_at";
const MAIL_COLUMNS: &str = "id, team_id, from_session, to_session, text, created_at";

fn row_to_member(r: &rusqlite::Row<'_>) -> rusqlite::Result<MemberRow> {
    Ok(MemberRow {
        session_id: r.get(0)?,
        team_id: r.get(1)?,
        name: r.get(2)?,
        role: Role::parse(&r.get::<_, String>(3)?),
        joined_at: r.get(4)?,
        ended_at: r.get(5)?,
    })
}

fn row_to_task(r: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: r.get(0)?,
        team_id: r.get(1)?,
        number: r.get(2)?,
        title: r.get(3)?,
        detail: r.get(4)?,
        status: TaskStatus::parse(&r.get::<_, String>(5)?).unwrap_or(TaskStatus::Open),
        assignee: r.get(6)?,
        assignee_name: None,
        created_by: r.get(7)?,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
    })
}

fn row_to_mail(r: &rusqlite::Row<'_>) -> rusqlite::Result<Mail> {
    Ok(Mail {
        id: r.get(0)?,
        team_id: r.get(1)?,
        from_session: r.get(2)?,
        from_name: None,
        to_session: r.get(3)?,
        to_name: None,
        text: r.get(4)?,
        created_at: r.get(5)?,
    })
}

impl Store {
    /// `session_id`'s membership, active or ended.
    pub fn team_member(&self, session_id: &str) -> anyhow::Result<Option<MemberRow>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {MEMBER_COLUMNS} FROM team_members WHERE session_id = ?1"),
                params![session_id],
                row_to_member,
            )
            .optional()?)
    }

    pub fn team_row(&self, team_id: &str) -> anyhow::Result<Option<TeamRow>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id, leader_session, project, created_at FROM teams WHERE id = ?1",
                params![team_id],
                |r| {
                    Ok(TeamRow {
                        id: r.get(0)?,
                        leader_session: r.get(1)?,
                        project: r.get(2)?,
                        created_at: r.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    /// Leader first, then teammates in joining order.
    pub fn team_members(&self, team_id: &str) -> anyhow::Result<Vec<MemberRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {MEMBER_COLUMNS} FROM team_members WHERE team_id = ?1
             ORDER BY role = 'teammate', joined_at, rowid"
        ))?;
        let rows = stmt.query_map(params![team_id], row_to_member)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Every active membership (leaders and teammates), by session id.
    pub fn active_team_members(&self) -> anyhow::Result<HashMap<String, MemberRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {MEMBER_COLUMNS} FROM team_members WHERE ended_at IS NULL"
        ))?;
        let rows = stmt.query_map([], row_to_member)?;
        let mut out = HashMap::new();
        for row in rows {
            let m = row?;
            out.insert(m.session_id.clone(), m);
        }
        Ok(out)
    }

    /// Create a team led by `leader`, with the leader as its first member.
    pub fn create_team(&self, leader: &str, project: &str) -> anyhow::Result<TeamRow> {
        let team = TeamRow {
            id: format!("team_{}", uuid::Uuid::new_v4().simple()),
            leader_session: leader.into(),
            project: project.into(),
            created_at: now_ms(),
        };
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO teams (id, leader_session, project, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![team.id, team.leader_session, team.project, team.created_at],
        )?;
        tx.execute(
            "INSERT INTO team_members (session_id, team_id, name, role, joined_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![leader, team.id, LEADER_NAME, Role::Leader.as_str(), team.created_at],
        )?;
        tx.commit()?;
        Ok(team)
    }

    pub fn add_team_member(
        &self,
        team_id: &str,
        session_id: &str,
        name: &str,
    ) -> anyhow::Result<MemberRow> {
        let m = MemberRow {
            session_id: session_id.into(),
            team_id: team_id.into(),
            name: name.into(),
            role: Role::Teammate,
            joined_at: now_ms(),
            ended_at: None,
        };
        self.conn().execute(
            "INSERT INTO team_members (session_id, team_id, name, role, joined_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![m.session_id, m.team_id, m.name, m.role.as_str(), m.joined_at],
        )?;
        Ok(m)
    }

    /// End an active teammate. Returns whether it was one.
    pub fn end_team_member(&self, session_id: &str) -> anyhow::Result<bool> {
        Ok(self.conn().execute(
            "UPDATE team_members SET ended_at = ?2
             WHERE session_id = ?1 AND role = 'teammate' AND ended_at IS NULL",
            params![session_id, now_ms()],
        )? > 0)
    }

    /// Teammates who joined `team_id` since `since_ms` (spawn rate).
    pub fn team_joins_since(&self, team_id: &str, since_ms: i64) -> anyhow::Result<u32> {
        Ok(self.conn().query_row(
            "SELECT COUNT(*) FROM team_members
             WHERE team_id = ?1 AND role = 'teammate' AND joined_at >= ?2",
            params![team_id, since_ms],
            |r| r.get(0),
        )?)
    }

    pub fn insert_team_task(
        &self,
        team_id: &str,
        title: &str,
        detail: &str,
        assignee: Option<&str>,
        created_by: &str,
    ) -> anyhow::Result<Task> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let number: i64 = tx.query_row(
            "SELECT COALESCE(MAX(number), 0) + 1 FROM team_tasks WHERE team_id = ?1",
            params![team_id],
            |r| r.get(0),
        )?;
        let now = now_ms();
        let task = Task {
            id: format!("task_{}", uuid::Uuid::new_v4().simple()),
            team_id: team_id.into(),
            number,
            title: title.into(),
            detail: detail.into(),
            status: TaskStatus::Open,
            assignee: assignee.map(String::from),
            assignee_name: None,
            created_by: created_by.into(),
            created_at: now,
            updated_at: now,
        };
        tx.execute(
            "INSERT INTO team_tasks (id, team_id, number, title, detail, status, assignee, created_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
            params![
                task.id,
                task.team_id,
                task.number,
                task.title,
                task.detail,
                task.status.as_str(),
                task.assignee,
                task.created_by,
                now
            ],
        )?;
        tx.commit()?;
        Ok(task)
    }

    pub fn team_task(&self, team_id: &str, number: i64) -> anyhow::Result<Option<Task>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {TASK_COLUMNS} FROM team_tasks WHERE team_id = ?1 AND number = ?2"),
                params![team_id, number],
                row_to_task,
            )
            .optional()?)
    }

    /// By number.
    pub fn team_tasks(&self, team_id: &str) -> anyhow::Result<Vec<Task>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {TASK_COLUMNS} FROM team_tasks WHERE team_id = ?1 ORDER BY number"
        ))?;
        let rows = stmt.query_map(params![team_id], row_to_task)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// `assignee`: `None` leaves it, `Some(None)` unassigns.
    pub fn update_team_task(
        &self,
        id: &str,
        title: Option<&str>,
        detail: Option<&str>,
        status: Option<TaskStatus>,
        assignee: Option<Option<&str>>,
    ) -> anyhow::Result<()> {
        self.conn().execute(
            "UPDATE team_tasks SET
                 title = COALESCE(?2, title),
                 detail = COALESCE(?3, detail),
                 status = COALESCE(?4, status),
                 assignee = CASE WHEN ?5 THEN ?6 ELSE assignee END,
                 updated_at = ?7
             WHERE id = ?1",
            params![
                id,
                title,
                detail,
                status.map(TaskStatus::as_str),
                assignee.is_some(),
                assignee.flatten(),
                now_ms()
            ],
        )?;
        Ok(())
    }

    pub fn insert_team_mail(
        &self,
        team_id: &str,
        from: &str,
        to: Option<&str>,
        text: &str,
    ) -> anyhow::Result<Mail> {
        let now = now_ms();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO team_mail (team_id, from_session, to_session, text, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![team_id, from, to, text, now],
        )?;
        Ok(Mail {
            id: conn.last_insert_rowid(),
            team_id: team_id.into(),
            from_session: from.into(),
            from_name: None,
            to_session: to.map(String::from),
            to_name: None,
            text: text.into(),
            created_at: now,
        })
    }

    /// Mail of `team_id`, oldest first: with `after > 0` the first `limit` after it, otherwise
    /// the last `limit`. With `viewer`, only team-wide mail and mail to or from the viewer.
    pub fn team_mail(
        &self,
        team_id: &str,
        viewer: Option<&str>,
        after: i64,
        limit: usize,
    ) -> anyhow::Result<Vec<Mail>> {
        let order = if after > 0 { "ASC" } else { "DESC" };
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {MAIL_COLUMNS} FROM team_mail
             WHERE team_id = ?1 AND id > ?2
               AND (?3 IS NULL OR to_session IS NULL OR to_session = ?3 OR from_session = ?3)
             ORDER BY id {order} LIMIT ?4"
        ))?;
        let rows = stmt.query_map(params![team_id, after, viewer, limit as i64], row_to_mail)?;
        let mut out: Vec<Mail> = rows.collect::<Result<_, _>>()?;
        if after <= 0 {
            out.reverse();
        }
        Ok(out)
    }
}

// ---- service ------------------------------------------------------------------------------------

fn valid_name(name: &str) -> Result<(), A2aError> {
    let ok = !name.is_empty()
        && name.len() <= NAME_MAX
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && !name.eq_ignore_ascii_case(LEADER_NAME)
        && !name.eq_ignore_ascii_case("all");
    if ok {
        Ok(())
    } else {
        Err(A2aError::BadRequest(format!(
            "invalid teammate name {name:?}: use 1-{NAME_MAX} letters, digits, '-' or '_' \
             (not \"{LEADER_NAME}\" or \"all\")"
        )))
    }
}

fn session_error(e: SessionError) -> A2aError {
    match e {
        SessionError::NotFound(id) => A2aError::NotFound(format!("session {id} not found")),
        SessionError::AgentUnavailable(_) | SessionError::Account(_) => {
            A2aError::BadRequest(format!("{e:#}"))
        }
        SessionError::Other(e) => A2aError::Other(e),
    }
}

/// Fill `assignee_name` from the members.
fn name_tasks(tasks: &mut [Task], names: &HashMap<String, String>) {
    for t in tasks {
        t.assignee_name = t.assignee.as_ref().and_then(|a| names.get(a).cloned());
    }
}

impl A2a {
    fn member_names(&self, team_id: &str) -> anyhow::Result<HashMap<String, String>> {
        Ok(self
            .sessions
            .store()
            .team_members(team_id)?
            .into_iter()
            .map(|m| (m.session_id, m.name))
            .collect())
    }

    /// The whole team, or `None` when it does not exist.
    pub fn team_view(&self, team_id: &str) -> anyhow::Result<Option<TeamView>> {
        let store = self.sessions.store();
        let Some(team) = store.team_row(team_id)? else {
            return Ok(None);
        };
        let rows = store.team_members(team_id)?;
        let names: HashMap<String, String> =
            rows.iter().map(|m| (m.session_id.clone(), m.name.clone())).collect();
        let mut members = Vec::with_capacity(rows.len());
        for m in rows {
            let rec: Option<SessionRecord> = store.session(&m.session_id)?;
            members.push(Member {
                title: rec.as_ref().map(|r| r.title.clone()).unwrap_or_default(),
                agent: rec.as_ref().map(|r| r.agent.as_str().to_string()).unwrap_or_default(),
                status: rec.as_ref().map(|r| r.status),
                session_id: m.session_id,
                name: m.name,
                role: m.role,
                joined_at: m.joined_at,
                ended_at: m.ended_at,
            });
        }
        let mut tasks = store.team_tasks(team_id)?;
        name_tasks(&mut tasks, &names);
        Ok(Some(TeamView {
            id: team.id,
            project: team.project,
            leader: team.leader_session,
            created_at: team.created_at,
            members,
            tasks,
        }))
    }

    /// The team `session_id` leads or belongs to (also after it was ended from it).
    pub fn team_of(&self, session_id: &str) -> Result<Option<TeamView>, A2aError> {
        self.session(session_id)?;
        match self.sessions.store().team_member(session_id)? {
            Some(m) => Ok(self.team_view(&m.team_id)?),
            None => Ok(None),
        }
    }

    pub(crate) fn publish_team(&self, team_id: &str) {
        match self.team_view(team_id) {
            Ok(Some(team)) => self.sessions.publish(Push::TeamUpdated {
                v: PUSH_VERSION,
                team,
            }),
            Ok(None) => {}
            Err(e) => tracing::warn!(team = %team_id, "building the team view failed: {e:#}"),
        }
    }

    /// The caller's active membership.
    fn membership(&self, caller: &str) -> Result<MemberRow, A2aError> {
        match self.sessions.store().team_member(caller)? {
            Some(m) if m.active() => Ok(m),
            Some(m) => Err(A2aError::Forbidden(format!(
                "this session was ended from team {}; it is no longer a member",
                m.team_id
            ))),
            None => Err(A2aError::BadRequest(format!(
                "this session is not in a team; `{CLI_NAME} team spawn` starts one with this \
                 session as its leader"
            ))),
        }
    }

    fn require_leader(&self, caller: &str) -> Result<MemberRow, A2aError> {
        let m = self.membership(caller)?;
        if m.role != Role::Leader {
            return Err(A2aError::Forbidden(
                "only the team leader can do that; ask the leader with `ember-a2a mail send lead`"
                    .into(),
            ));
        }
        Ok(m)
    }

    /// An active member of `team_id` by name (case-insensitive) or session id (or a unique
    /// prefix of at least 4 characters).
    fn resolve_member(&self, team_id: &str, who: &str) -> Result<MemberRow, A2aError> {
        let who = who.trim();
        let active: Vec<MemberRow> = self
            .sessions
            .store()
            .team_members(team_id)?
            .into_iter()
            .filter(MemberRow::active)
            .collect();
        if let Some(m) = active
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(who) || m.session_id == who)
        {
            return Ok(m.clone());
        }
        if who.len() >= 4 {
            let hits: Vec<&MemberRow> =
                active.iter().filter(|m| m.session_id.starts_with(who)).collect();
            if let [m] = hits.as_slice() {
                return Ok((*m).clone());
            }
        }
        Err(A2aError::NotFound(format!(
            "no active team member {who:?}; `{CLI_NAME} team list` shows the team"
        )))
    }

    /// Spawn a teammate for `caller`, making it a leader on its first spawn.
    pub async fn spawn_teammate(
        self: &Arc<Self>,
        caller: &str,
        req: SpawnRequest,
    ) -> Result<SpawnOutcome, A2aError> {
        self.check_caller(caller)?;
        let name = req.name.trim().to_string();
        valid_name(&name)?;
        let prompt = req.prompt.trim();
        if prompt.is_empty() {
            return Err(A2aError::BadRequest("the teammate's first prompt is empty".into()));
        }
        let store = self.sessions.store();
        let leader = self.session(caller)?;
        let existing = match store.team_member(caller)? {
            Some(m) if m.role == Role::Leader => Some(m.team_id),
            Some(m) if m.active() => {
                return Err(A2aError::Forbidden(format!(
                    "only the team leader spawns teammates; you are teammate {} in team {}",
                    m.name, m.team_id
                )))
            }
            Some(_) => {
                return Err(A2aError::Forbidden(
                    "this session was a teammate in another team and cannot lead one".into(),
                ))
            }
            None => None,
        };
        if let Some(team_id) = &existing {
            let members = store.team_members(team_id)?;
            let active: Vec<&MemberRow> =
                members.iter().filter(|m| m.is_active_teammate()).collect();
            if active.len() >= MAX_TEAMMATES {
                return Err(A2aError::BadRequest(format!(
                    "the team already has {MAX_TEAMMATES} active teammates (limit); end one first"
                )));
            }
            if active.iter().any(|m| m.name.eq_ignore_ascii_case(&name)) {
                return Err(A2aError::BadRequest(format!(
                    "the team already has an active teammate named {name}"
                )));
            }
            let l = self.config.limits;
            let since = now_ms() - l.window.as_millis() as i64;
            if store.team_joins_since(team_id, since)? >= l.per_session {
                let message = format!(
                    "team {team_id} has spawned {} teammates in the last {} min (limit)",
                    l.per_session,
                    l.window.as_secs().div_ceil(60)
                );
                return Err(A2aError::RateLimited(message));
            }
        }
        let agent = match req.agent.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
            Some(a) => AgentKind::parse(a)
                .ok_or_else(|| A2aError::BadRequest(format!("unknown agent {a}")))?,
            None => leader.agent,
        };
        let title = req
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(String::from)
            .unwrap_or_else(|| format!("{name} \u{00b7} {}", leader.title));
        // Same project and directory; nothing else is inherited (model, account, computer and
        // approvals are the teammate's own).
        let rec = self
            .sessions
            .create_with_account(
                NewSession {
                    project: leader.project.clone(),
                    agent,
                    cwd: leader.cwd.clone().into(),
                    model: None,
                    title,
                },
                req.account.as_deref().map(str::trim).filter(|a| !a.is_empty()),
            )
            .map_err(session_error)?;
        let team_id = match existing {
            Some(id) => id,
            None => store.create_team(caller, &leader.project)?.id,
        };
        let row = match store.add_team_member(&team_id, &rec.id, &name) {
            Ok(r) => r,
            Err(e) => {
                self.archive(&rec.id);
                return Err(A2aError::BadRequest(format!("adding teammate {name}: {e:#}")));
            }
        };

        if let Some(computer) = req
            .computer
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty() && *c != crate::computers::LOCAL)
        {
            let setter = self.computer_setter.lock().unwrap().clone();
            let result = match setter {
                Some(set) => set(rec.id.clone(), computer.to_string()).await,
                None => Err("this server cannot move sessions between computers".to_string()),
            };
            if let Err(e) = result {
                store.end_team_member(&rec.id)?;
                self.archive(&rec.id);
                self.publish_team(&team_id);
                return Err(A2aError::BadRequest(format!(
                    "teammate {name} could not be put on computer {computer}: {e}; it was \
                     ended and archived"
                )));
            }
        }
        self.publish_team(&team_id);

        // The first prompt, as a message from the leader: not rate-limited (bounded by
        // MAX_TEAMMATES and the spawn rate above).
        let msg = store_message(self, caller, &rec.id, prompt)?;
        let message = self.deliver(&msg).await?;
        let member = self
            .team_view(&team_id)?
            .and_then(|t| t.members.into_iter().find(|m| m.session_id == rec.id))
            .ok_or_else(|| A2aError::Other(anyhow::anyhow!("teammate {} vanished", row.name)))?;
        Ok(SpawnOutcome {
            team_id,
            member,
            message,
        })
    }

    fn archive(&self, id: &str) {
        let patch = SessionPatch {
            archived: Some(true),
            ..Default::default()
        };
        if let Err(e) = self.sessions.update_meta(id, &patch) {
            tracing::warn!(session = %id, "archiving a failed teammate failed: {e:#}");
        }
    }

    /// The leader ends a teammate: its agent is stopped and it leaves the team.
    pub async fn end_teammate(&self, caller: &str, who: &str) -> Result<Member, A2aError> {
        let leader = self.require_leader(caller)?;
        let target = self.resolve_member(&leader.team_id, who)?;
        if target.role == Role::Leader {
            return Err(A2aError::BadRequest("the leader cannot end itself".into()));
        }
        self.end_member(&leader.team_id, &target, "the team leader").await
    }

    /// The user ends a teammate from a client.
    pub async fn end_teammate_by_user(
        &self,
        team_id: &str,
        session_id: &str,
    ) -> Result<Member, A2aError> {
        let target = match self.sessions.store().team_member(session_id)? {
            Some(m) if m.team_id == team_id && m.is_active_teammate() => m,
            _ => {
                return Err(A2aError::NotFound(format!(
                    "session {session_id} is not an active teammate of team {team_id}"
                )))
            }
        };
        self.end_member(team_id, &target, "the user").await
    }

    async fn end_member(
        &self,
        team_id: &str,
        target: &MemberRow,
        by: &str,
    ) -> Result<Member, A2aError> {
        if !self.sessions.store().end_team_member(&target.session_id)? {
            return Err(A2aError::NotFound(format!(
                "{} is not an active teammate",
                target.name
            )));
        }
        // Stop the agent; its native state stays, so the user can still resume it directly.
        if let Err(e) = self.sessions.interrupt(&target.session_id).await {
            tracing::debug!(session = %target.session_id, "interrupt on team end: {e:#}");
        }
        if let Err(e) = self.sessions.release(&target.session_id).await {
            tracing::warn!(session = %target.session_id, "release on team end: {e:#}");
        }
        let notice = AgentEvent::Notice {
            message: format!(
                "Teammate {} was ended by {by}; it has left team {team_id}.",
                target.name
            ),
        };
        if let Err(e) = self.sessions.record_event(&target.session_id, &notice) {
            tracing::warn!(session = %target.session_id, "recording team notice failed: {e:#}");
        }
        self.publish_team(team_id);
        self.team_view(team_id)?
            .and_then(|t| t.members.into_iter().find(|m| m.session_id == target.session_id))
            .ok_or_else(|| A2aError::NotFound(format!("team {team_id} not found")))
    }

    pub fn tasks(&self, caller: &str) -> Result<Vec<Task>, A2aError> {
        self.check_caller(caller)?;
        let m = self.membership(caller)?;
        let mut tasks = self.sessions.store().team_tasks(&m.team_id)?;
        name_tasks(&mut tasks, &self.member_names(&m.team_id)?);
        Ok(tasks)
    }

    pub async fn add_task(self: &Arc<Self>, caller: &str, new: NewTask) -> Result<Task, A2aError> {
        self.check_caller(caller)?;
        let me = self.membership(caller)?;
        let title = new.title.trim();
        if title.is_empty() {
            return Err(A2aError::BadRequest("the task title is empty".into()));
        }
        let assignee = match new.assignee.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
            Some(who) => Some(self.resolve_member(&me.team_id, who)?),
            None => None,
        };
        if let Some(a) = &assignee {
            if me.role == Role::Teammate && a.session_id != me.session_id {
                return Err(A2aError::Forbidden(
                    "a teammate assigns tasks only to itself; ask the leader to assign others"
                        .into(),
                ));
            }
        }
        let store = self.sessions.store();
        let mut task = store.insert_team_task(
            &me.team_id,
            title,
            new.detail.as_deref().unwrap_or("").trim(),
            assignee.as_ref().map(|a| a.session_id.as_str()),
            caller,
        )?;
        task.assignee_name = assignee.as_ref().map(|a| a.name.clone());
        self.publish_team(&me.team_id);
        if let Some(a) = assignee.filter(|a| a.session_id != caller) {
            self.announce_assignment(&me, &a, &task).await;
        }
        Ok(task)
    }

    pub async fn update_task(
        self: &Arc<Self>,
        caller: &str,
        number: i64,
        patch: TaskPatch,
    ) -> Result<Task, A2aError> {
        self.check_caller(caller)?;
        let me = self.membership(caller)?;
        let store = self.sessions.store();
        let task = store
            .team_task(&me.team_id, number)?
            .ok_or_else(|| A2aError::NotFound(format!("task #{number} not found")))?;
        let status = match patch.status.as_deref() {
            Some(s) => Some(TaskStatus::parse(s).ok_or_else(|| {
                A2aError::BadRequest(format!(
                    "unknown status {s}: use open, in_progress, blocked, done or cancelled"
                ))
            })?),
            None => None,
        };
        // Some(None) = unassign.
        let assignee: Option<Option<MemberRow>> = match patch.assignee.as_deref().map(str::trim) {
            None => None,
            Some("") | Some("none") | Some("-") => Some(None),
            Some(who) => Some(Some(self.resolve_member(&me.team_id, who)?)),
        };
        if me.role == Role::Teammate {
            if task.assignee.as_deref().is_some_and(|a| a != caller) {
                return Err(A2aError::Forbidden(format!(
                    "task #{number} is assigned to someone else; only they or the leader update it"
                )));
            }
            if let Some(Some(a)) = &assignee {
                if a.session_id != caller {
                    return Err(A2aError::Forbidden(
                        "a teammate assigns tasks only to itself; ask the leader to reassign"
                            .into(),
                    ));
                }
            }
        }
        let title = patch.title.as_deref().map(str::trim).filter(|t| !t.is_empty());
        let detail = patch.detail.as_deref().map(str::trim);
        store.update_team_task(
            &task.id,
            title,
            detail,
            status,
            assignee
                .as_ref()
                .map(|a| a.as_ref().map(|m| m.session_id.as_str())),
        )?;
        let mut updated = store
            .team_task(&me.team_id, number)?
            .ok_or_else(|| A2aError::NotFound(format!("task #{number} not found")))?;
        name_tasks(std::slice::from_mut(&mut updated), &self.member_names(&me.team_id)?);
        self.publish_team(&me.team_id);

        if let Some(Some(a)) = &assignee {
            if a.session_id != caller && task.assignee.as_deref() != Some(a.session_id.as_str()) {
                self.announce_assignment(&me, a, &updated).await;
            }
        }
        // A teammate's progress reaches the leader without a separate mail.
        if me.role == Role::Teammate && status.is_some() && status != Some(task.status) {
            if let Some(team) = store.team_row(&me.team_id)? {
                let text = format!(
                    "Task #{} \"{}\" is now {} (updated by teammate {}).",
                    updated.number,
                    updated.title,
                    updated.status.as_str(),
                    me.name
                );
                self.notify(caller, &team.leader_session, &text).await;
            }
        }
        Ok(updated)
    }

    async fn announce_assignment(self: &Arc<Self>, me: &MemberRow, to: &MemberRow, task: &Task) {
        let mut text = format!(
            "Task #{} was assigned to you by {}: {}",
            task.number, me.name, task.title
        );
        if !task.detail.is_empty() {
            text.push_str(&format!("\n\n{}", task.detail));
        }
        text.push_str(&format!(
            "\n\nUpdate it as you go: {CLI_NAME} task update {} --status in_progress|blocked|done",
            task.number
        ));
        self.notify(&me.session_id, &to.session_id, &text).await;
    }

    /// Best-effort A2A notice under loop protection (a refused one leaves its own notice).
    async fn notify(self: &Arc<Self>, from: &str, to: &str, text: &str) {
        let msg = {
            let _guard = self.send_lock.lock().await;
            if self.check_limits(from, &[to.to_string()]).is_err() {
                return;
            }
            match self.store.insert_message(from, to, None, text) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("storing a team notice failed: {e:#}");
                    return;
                }
            }
        };
        if let Err(e) = self.deliver(&msg).await {
            tracing::warn!(to = %to, "delivering a team notice failed: {e:#}");
        }
    }

    /// Send team mail to one member (`to`: name or session id) or the whole team (`None`).
    pub async fn send_mail(
        self: &Arc<Self>,
        caller: &str,
        to: Option<&str>,
        text: &str,
    ) -> Result<MailReceipt, A2aError> {
        self.check_caller(caller)?;
        let me = self.membership(caller)?;
        let text = text.trim();
        if text.is_empty() {
            return Err(A2aError::BadRequest("mail text is empty".into()));
        }
        let to_member = match to.map(str::trim).filter(|t| !t.is_empty() && *t != "all") {
            Some(who) => {
                let m = self.resolve_member(&me.team_id, who)?;
                if m.session_id == caller {
                    return Err(A2aError::BadRequest("a session cannot mail itself".into()));
                }
                Some(m)
            }
            None => None,
        };
        let store = self.sessions.store();
        let recipients: Vec<MemberRow> = match &to_member {
            Some(m) => vec![m.clone()],
            None => store
                .team_members(&me.team_id)?
                .into_iter()
                .filter(|m| m.active() && m.session_id != caller)
                .collect(),
        };
        if recipients.is_empty() {
            return Err(A2aError::BadRequest("the team has no other active members".into()));
        }
        let mut disabled = Vec::new();
        for r in &recipients {
            if !self.store.session_enabled(&r.session_id)? {
                disabled.push(r.name.clone());
            }
        }
        if !disabled.is_empty() {
            return Err(A2aError::Disabled(format!(
                "agent-to-agent messaging is turned off for {}",
                disabled.join(", ")
            )));
        }
        let ids: Vec<String> = recipients.iter().map(|r| r.session_id.clone()).collect();
        let (mut mail, msgs) = {
            let _guard = self.send_lock.lock().await;
            self.check_limits(caller, &ids)?;
            let mail = store.insert_team_mail(
                &me.team_id,
                caller,
                to_member.as_ref().map(|m| m.session_id.as_str()),
                text,
            )?;
            let heading = match &to_member {
                Some(_) => format!("Team mail #{} from {} to you:", mail.id, me.name),
                None => format!("Team mail #{} from {} to the whole team:", mail.id, me.name),
            };
            let body = format!("{heading}\n\n{text}");
            let mut msgs = Vec::new();
            for id in &ids {
                msgs.push(self.store.insert_message(caller, id, None, &body)?);
            }
            (mail, msgs)
        };
        mail.from_name = Some(me.name.clone());
        mail.to_name = to_member.as_ref().map(|m| m.name.clone());
        let mut deliveries = Vec::new();
        for m in &msgs {
            deliveries.push(self.deliver(m).await?);
        }
        Ok(MailReceipt { mail, deliveries })
    }

    /// The caller's mailbox: the leader sees all team mail, a teammate team-wide mail and its
    /// own.
    pub fn read_mail(
        &self,
        caller: &str,
        after: i64,
        limit: Option<usize>,
    ) -> Result<Vec<Mail>, A2aError> {
        self.check_caller(caller)?;
        // Ended teammates may still read what they were sent.
        let me = match self.sessions.store().team_member(caller)? {
            Some(m) => m,
            None => self.membership(caller)?,
        };
        let viewer = (me.role == Role::Teammate).then_some(caller);
        self.mail_of(&me.team_id, viewer, after, limit)
    }

    /// A team's mail for the user (everything).
    pub fn mail_of(
        &self,
        team_id: &str,
        viewer: Option<&str>,
        after: i64,
        limit: Option<usize>,
    ) -> Result<Vec<Mail>, A2aError> {
        let limit = limit.unwrap_or(MAIL_PAGE).clamp(1, MAIL_PAGE_MAX);
        let mut mail = self.sessions.store().team_mail(team_id, viewer, after, limit)?;
        let names = self.member_names(team_id)?;
        for m in &mut mail {
            m.from_name = names.get(&m.from_session).cloned();
            m.to_name = m.to_session.as_ref().and_then(|t| names.get(t).cloned());
        }
        Ok(mail)
    }

    /// The part of the instructions only a teammate gets (stable while it is a member).
    pub(crate) fn teammate_instructions(&self, rec: &SessionRecord) -> Option<String> {
        let store = self.sessions.store();
        let m = store.team_member(&rec.id).ok().flatten()?;
        if !m.is_active_teammate() {
            return None;
        }
        let team = store.team_row(&m.team_id).ok().flatten()?;
        // Ids and names only (no titles, which the user may rename): stable across resumes.
        Some(format!(
            "# Your team\n\
             \n\
             You are teammate `{}` in team {}, led by session {} (member name \
             `{LEADER_NAME}`). Work on the tasks assigned to you (`{CLI_NAME} task list`), set \
             their status as you go (`{CLI_NAME} task update <n> --status \
             in_progress|blocked|done`), and tell the leader when you finish or are stuck \
             (`{CLI_NAME} mail send {LEADER_NAME} \"...\"`). You can message only your team.",
            m.name, team.id, team.leader_session
        ))
    }
}

/// Store an A2A message without loop-protection accounting beyond the row itself.
fn store_message(a: &A2a, from: &str, to: &str, text: &str) -> Result<super::A2aMessage, A2aError> {
    Ok(a.store.insert_message(from, to, None, text)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    #[test]
    fn team_rows_round_trip() {
        let s = store();
        let t = s.create_team("lead-s", "acme").unwrap();
        assert_eq!(s.team_member("lead-s").unwrap().unwrap().role, Role::Leader);
        s.add_team_member(&t.id, "m1", "alice").unwrap();
        // Names are unique among active members only.
        assert!(s.add_team_member(&t.id, "m2", "alice").is_err());
        assert!(s.end_team_member("m1").unwrap());
        assert!(!s.end_team_member("m1").unwrap());
        assert!(!s.end_team_member("lead-s").unwrap(), "the leader is never ended");
        s.add_team_member(&t.id, "m2", "alice").unwrap();
        let members = s.team_members(&t.id).unwrap();
        assert_eq!(
            members.iter().map(|m| m.session_id.as_str()).collect::<Vec<_>>(),
            vec!["lead-s", "m1", "m2"]
        );
        let active = s.active_team_members().unwrap();
        assert!(active.contains_key("lead-s") && active.contains_key("m2") && !active.contains_key("m1"));
        assert_eq!(s.team_joins_since(&t.id, 0).unwrap(), 2);
    }

    #[test]
    fn tasks_number_per_team_and_patch() {
        let s = store();
        let a = s.create_team("la", "p").unwrap();
        let b = s.create_team("lb", "p").unwrap();
        let t1 = s.insert_team_task(&a.id, "one", "", None, "la").unwrap();
        let t2 = s.insert_team_task(&a.id, "two", "d", Some("x"), "la").unwrap();
        let u1 = s.insert_team_task(&b.id, "other", "", None, "lb").unwrap();
        assert_eq!((t1.number, t2.number, u1.number), (1, 2, 1));
        s.update_team_task(&t2.id, None, None, Some(TaskStatus::Done), Some(None)).unwrap();
        let t2 = s.team_task(&a.id, 2).unwrap().unwrap();
        assert_eq!((t2.status, t2.assignee.as_deref(), t2.title.as_str()), (TaskStatus::Done, None, "two"));
        s.update_team_task(&t1.id, Some("uno"), None, None, Some(Some("y"))).unwrap();
        let t1 = s.team_task(&a.id, 1).unwrap().unwrap();
        assert_eq!((t1.title.as_str(), t1.assignee.as_deref(), t1.status), ("uno", Some("y"), TaskStatus::Open));
        assert_eq!(s.team_tasks(&a.id).unwrap().len(), 2);
        assert_eq!(TaskStatus::parse("in-progress"), Some(TaskStatus::InProgress));
        assert_eq!(TaskStatus::parse("nope"), None);
    }

    #[test]
    fn mail_visibility_and_paging() {
        let s = store();
        let t = s.create_team("l", "p").unwrap();
        let m1 = s.insert_team_mail(&t.id, "l", None, "all hands").unwrap();
        s.insert_team_mail(&t.id, "l", Some("b"), "to b").unwrap();
        let m3 = s.insert_team_mail(&t.id, "a", Some("l"), "a to lead").unwrap();
        assert_eq!(s.team_mail(&t.id, None, 0, 10).unwrap().len(), 3);
        let for_a: Vec<String> =
            s.team_mail(&t.id, Some("a"), 0, 10).unwrap().into_iter().map(|m| m.text).collect();
        assert_eq!(for_a, vec!["all hands".to_string(), "a to lead".to_string()]);
        // The last N, oldest first; or the first N after a cursor.
        let last = s.team_mail(&t.id, None, 0, 1).unwrap();
        assert_eq!(last[0].id, m3.id);
        let after = s.team_mail(&t.id, None, m1.id, 1).unwrap();
        assert_eq!(after[0].text, "to b");
    }
}
