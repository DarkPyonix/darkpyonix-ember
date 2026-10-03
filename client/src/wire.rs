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
}
