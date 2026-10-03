//! The normalised event model every agent adapter emits (SPEC FR-A3).
//!
//! Each agent speaks its own protocol (Claude Code stream-json, Codex app-server JSON-RPC, ACP…).
//! Adapters translate that into these events; storage, the push channel, the UI and A2A only ever
//! see this model, so adding an agent never touches them.

use serde::{Deserialize, Serialize};

/// One thing that happened in a session, in the order it happened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    /// The agent's own session identifier became known (used for native resume, FR-S2).
    NativeSession { native_id: String },
    /// A message the user (or an A2A sender) sent into the session.
    UserMessage { text: String },
    /// A note from Ember itself, delivered **to the agent** in front of the next user message
    /// (e.g. "the session's computer changed", FR-S7). Stored so the transcript shows what the
    /// agent was told. Unlike [`AgentEvent::Notice`], it is part of the agent's conversation.
    SystemNotice { text: String },
    /// Incremental assistant text, for live display. Not needed to reconstruct the transcript.
    AssistantDelta { text: String },
    /// A complete assistant message.
    AssistantMessage { text: String },
    /// The agent invoked a tool.
    ToolCall { call_id: String, name: String, input: serde_json::Value },
    /// A tool finished.
    ToolResult { call_id: String, output: String, is_error: bool },
    /// The agent is waiting for a tool approval (FR-A5).
    ApprovalRequested { approval_id: String, tool: String, input: serde_json::Value },
    /// An approval was answered.
    ApprovalResolved { approval_id: String, decision: ApprovalDecision },
    /// Token usage reported by the agent (FR-U3).
    Usage { input_tokens: u64, output_tokens: u64 },
    /// The agent's account hit a rate or usage limit (FR-U3). `resets_at` is a Unix time in
    /// milliseconds when the agent reported one. Usually followed by an `Error` and a failed turn.
    RateLimited { resets_at: Option<i64>, message: String },
    /// The current turn is over.
    TurnEnded { outcome: TurnOutcome },
    /// The agent process reported or hit an error.
    Error { message: String },
    /// Something Ember itself shows in this session, e.g. an A2A message refused by loop
    /// protection (FR-T5). Not part of the agent's conversation; leaves the status alone.
    Notice { message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    AllowOnce,
    /// Allow this tool kind for the rest of the session.
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

/// Session status shown in the launcher (FR-L2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Idle,
    Running,
    WaitingForApproval,
    Finished,
    Failed,
}

impl SessionStatus {
    /// Status after `event`, given the status before it. Approval outranks running (FR-L2).
    pub fn after(self, event: &AgentEvent) -> SessionStatus {
        match event {
            AgentEvent::UserMessage { .. } => SessionStatus::Running,
            AgentEvent::ApprovalRequested { .. } => SessionStatus::WaitingForApproval,
            AgentEvent::ApprovalResolved { .. } => SessionStatus::Running,
            AgentEvent::TurnEnded { outcome: TurnOutcome::Failed } => SessionStatus::Failed,
            AgentEvent::TurnEnded { .. } => SessionStatus::Finished,
            _ => self,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_outranks_running_until_resolved() {
        let s = SessionStatus::Idle.after(&AgentEvent::UserMessage { text: "hi".into() });
        assert_eq!(s, SessionStatus::Running);
        let s = s.after(&AgentEvent::AssistantDelta { text: "x".into() });
        assert_eq!(s, SessionStatus::Running);
        let s = s.after(&AgentEvent::ApprovalRequested {
            approval_id: "a".into(),
            tool: "Bash".into(),
            input: serde_json::json!({}),
        });
        assert_eq!(s, SessionStatus::WaitingForApproval);
        let s = s.after(&AgentEvent::ToolCall {
            call_id: "c".into(),
            name: "Read".into(),
            input: serde_json::json!({}),
        });
        assert_eq!(s, SessionStatus::WaitingForApproval);
        let s = s.after(&AgentEvent::ApprovalResolved {
            approval_id: "a".into(),
            decision: ApprovalDecision::AllowOnce,
        });
        assert_eq!(s, SessionStatus::Running);
        let s = s.after(&AgentEvent::TurnEnded { outcome: TurnOutcome::Completed });
        assert_eq!(s, SessionStatus::Finished);
    }

    #[test]
    fn events_serialise_with_kind_tag() {
        let v = serde_json::to_value(AgentEvent::TurnEnded { outcome: TurnOutcome::Interrupted })
            .unwrap();
        assert_eq!(v, serde_json::json!({"kind": "turn_ended", "outcome": "interrupted"}));
    }
}
