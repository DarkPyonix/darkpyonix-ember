//! The main server's wire types, as the client sees them (PR-1, `ember/server/src/{events,store}.rs`).
//!
//! Mirrored rather than shared so the client never links the server. Decoding is tolerant where
//! the server may grow: unknown event kinds and statuses decode to `Unknown` instead of failing,
//! so an additive server change never breaks an older client. Incompatible changes bump
//! [`PUSH_VERSION`] and are reported (see [`crate::push`]).

use serde::{Deserialize, Serialize};

/// The push envelope version this client understands.
pub const PUSH_VERSION: u32 = 1;

/// One thing that happened in a session (server `AgentEvent`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    NativeSession { native_id: String },
    UserMessage { text: String },
    AssistantDelta { text: String },
    AssistantMessage { text: String },
    ToolCall { call_id: String, name: String, input: serde_json::Value },
    ToolResult { call_id: String, output: String, is_error: bool },
    ApprovalRequested { approval_id: String, tool: String, input: serde_json::Value },
    ApprovalResolved { approval_id: String, decision: ApprovalDecision },
    Usage { input_tokens: u64, output_tokens: u64 },
    TurnEnded { outcome: TurnOutcome },
    Error { message: String },
    /// Something Ember shows in the session, not part of the agent's conversation (an A2A
    /// message refused by loop protection, the outcome of a mention, a teammate ended).
    Notice { message: String },
    /// An event kind this client does not know yet. Ignored by the reducer.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    AllowOnce,
    AllowAlways,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    Completed,
    Interrupted,
    Failed,
}

/// Session status as stored on the server. The launcher's status adds "finished-unread" on top
/// (see [`crate::state::LauncherStatus`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Idle,
    Running,
    WaitingForApproval,
    Finished,
    Failed,
    #[serde(other)]
    Unknown,
}

/// A session as the server lists it (server `SessionRecord`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub project: String,
    /// Agent kind (`claude-code`, `codex`, …). A string so new agents need no client change.
    pub agent: String,
    pub cwd: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub native_id: Option<String>,
    pub status: SessionStatus,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_seq: i64,
    /// The account the session runs under (FR-U2); `None` = the server's own agent login.
    #[serde(default)]
    pub account_id: Option<String>,
    /// Why that account was chosen (FR-U3).
    #[serde(default)]
    pub account_reason: Option<String>,
    /// FR-L9. Absent from servers older than the metadata API: `false`.
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub archived: bool,
}

impl SessionRecord {
    /// Take the user-editable metadata (title, pin, archive) from `other`, keeping this
    /// record's activity fields. Returns whether anything changed.
    pub fn take_meta(&mut self, other: &SessionRecord) -> bool {
        let changed = self.title != other.title || self.pinned != other.pinned || self.archived != other.archived;
        self.title.clone_from(&other.title);
        self.pinned = other.pinned;
        self.archived = other.archived;
        changed
    }
}

/// A project and the computers assigned to it (server `projects::Project`, FR-L4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    #[serde(default)]
    pub created_at: i64,
    /// Assigned computer ids (`local` = the main server itself), sorted.
    #[serde(default)]
    pub computers: Vec<String>,
}

/// `PATCH /sessions/{id}` body (FR-L9); `None` fields are left alone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SessionPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
}

/// Start of a matched term in [`SearchHit::snippet`].
pub const SNIPPET_OPEN: char = '\u{ab}';
/// End of a matched term in [`SearchHit::snippet`].
pub const SNIPPET_CLOSE: char = '\u{bb}';

/// `GET /search` hit (FR-S4): one message, by session and sequence number.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SearchHit {
    pub session_id: String,
    pub seq: i64,
    /// `user_message` or `assistant_message`.
    #[serde(default)]
    pub kind: String,
    /// Text around the match; matched terms between [`SNIPPET_OPEN`] and [`SNIPPET_CLOSE`].
    pub snippet: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub archived: bool,
}

impl SearchHit {
    /// The snippet without match markers.
    pub fn plain_snippet(&self) -> String {
        self.snippet.chars().filter(|c| *c != SNIPPET_OPEN && *c != SNIPPET_CLOSE).collect()
    }
}

/// One stored event with its per-session sequence number (server `StoredEvent`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredEvent {
    pub session_id: String,
    pub seq: i64,
    pub at: i64,
    pub event: AgentEvent,
}

/// `GET /sessions/{id}` response.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SessionDetail {
    pub session: SessionRecord,
    pub live: bool,
    /// Whether the fork action is offered (FR-S5); disabled, not failing, when `false`.
    #[serde(default)]
    pub can_fork: bool,
}

/// `GET /agents` entry.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DetectedAgent {
    pub kind: String,
    pub installed: bool,
    #[serde(default)]
    pub version: Option<String>,
}

/// `POST /sessions` body.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NewSession {
    pub project: String,
    pub agent: String,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// A decoded push message (server `Push`, plus the `lagged` notice the push loop sends itself).
#[derive(Debug, Clone, PartialEq)]
pub enum Push {
    SessionCreated { session: SessionRecord },
    Event { status: SessionStatus, event: StoredEvent },
    /// The server dropped `missed` messages for this client; it must resync from the store.
    Lagged { missed: u64 },
    /// A session's title, pin or archive mark changed (FR-L9). Only those fields are taken
    /// from `session`; events own the activity fields.
    SessionUpdated { session: SessionRecord },
    /// A project was created or its computer assignment changed (FR-L4).
    ProjectUpdated { project: Project },
    /// A team's members or tasks changed (FR-T7): the whole team.
    TeamUpdated { team: TeamView },
}

// ---- teams and mentions (FR-T6, FR-T7; server `a2a::team`, `a2a::mention`) -------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamRole {
    Leader,
    Teammate,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Open,
    InProgress,
    Blocked,
    Done,
    Cancelled,
    #[serde(other)]
    Unknown,
}

impl TaskStatus {
    pub fn label(self) -> &'static str {
        match self {
            TaskStatus::Open => "open",
            TaskStatus::InProgress => "in progress",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Done => "done",
            TaskStatus::Cancelled => "cancelled",
            TaskStatus::Unknown => "unknown",
        }
    }
}

/// A team member. `title`, `agent` and `status` are a snapshot from when the team last
/// changed; a UI takes live status from the session record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamMember {
    pub session_id: String,
    pub name: String,
    pub role: TeamRole,
    #[serde(default)]
    pub joined_at: i64,
    /// Set once the teammate was ended.
    #[serde(default)]
    pub ended_at: Option<i64>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub status: Option<SessionStatus>,
}

impl TeamMember {
    pub fn active(&self) -> bool {
        self.ended_at.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamTask {
    pub id: String,
    #[serde(default)]
    pub team_id: String,
    /// `#n` within the team.
    pub number: i64,
    pub title: String,
    #[serde(default)]
    pub detail: String,
    pub status: TaskStatus,
    /// Assignee session id.
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub assignee_name: Option<String>,
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

/// A team: leader first among `members`, then teammates (ended ones included); tasks by number.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamView {
    pub id: String,
    #[serde(default)]
    pub project: String,
    /// The leader's session id.
    pub leader: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub members: Vec<TeamMember>,
    #[serde(default)]
    pub tasks: Vec<TeamTask>,
}

impl TeamView {
    pub fn member(&self, session_id: &str) -> Option<&TeamMember> {
        self.members.iter().find(|m| m.session_id == session_id)
    }
}

/// `GET /teams/{id}/mail` entry.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TeamMail {
    pub id: i64,
    #[serde(default)]
    pub team_id: String,
    pub from_session: String,
    #[serde(default)]
    pub from_name: Option<String>,
    /// `None` = the whole team.
    #[serde(default)]
    pub to_session: Option<String>,
    #[serde(default)]
    pub to_name: Option<String>,
    pub text: String,
    #[serde(default)]
    pub created_at: i64,
}

/// `GET /sessions/{id}/mentions` entry: a session the composer may mention as `@@...`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MentionCandidate {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub agent: String,
    pub status: SessionStatus,
}
