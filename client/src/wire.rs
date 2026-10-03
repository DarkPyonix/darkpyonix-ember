//! The main server's wire types, as the client sees them (PR-1, `server/src/{events,store}.rs`).
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
}
