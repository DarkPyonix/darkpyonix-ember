//! Agent adapters (SPEC §A).
//!
//! An adapter starts one agent CLI, unmodified, through that agent's own non-interactive protocol
//! (FR-A2), and translates its output into [`AgentEvent`]s (FR-A3). Adding an agent means adding
//! an adapter here and nothing else.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::events::{AgentEvent, ApprovalDecision};

pub mod claude_code;
pub mod scripted;

/// Which agent CLI a session runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentKind {
    ClaudeCode,
    Codex,
    /// In-process fake used by tests; never offered to users.
    Scripted,
}

impl AgentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "claude-code",
            AgentKind::Codex => "codex",
            AgentKind::Scripted => "scripted",
        }
    }

    pub fn parse(s: &str) -> Option<AgentKind> {
        match s {
            "claude-code" => Some(AgentKind::ClaudeCode),
            "codex" => Some(AgentKind::Codex),
            "scripted" => Some(AgentKind::Scripted),
            _ => None,
        }
    }
}

/// Everything an adapter needs to start or resume an agent process.
#[derive(Debug, Clone)]
pub struct StartRequest {
    /// Working directory the agent sees.
    pub cwd: PathBuf,
    /// The agent's own session id, when resuming (FR-S2). `None` starts a new native session.
    pub resume_native_id: Option<String>,
    /// Model override, if the user picked one.
    pub model: Option<String>,
}

/// Result of probing for an installed agent (FR-A6).
#[derive(Debug, Clone, Serialize)]
pub struct Detected {
    pub kind: AgentKind,
    pub installed: bool,
    pub version: Option<String>,
}

#[async_trait]
pub trait AgentAdapter: Send + Sync {
    fn kind(&self) -> AgentKind;

    /// Is the agent installed on this server, and which version? Missing is not an error.
    async fn detect(&self) -> Detected;

    /// Start (or resume) the agent. Events are sent on `events` until the process exits.
    async fn start(
        &self,
        req: StartRequest,
        events: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<Box<dyn AgentRun>>;
}

/// A running agent process.
#[async_trait]
pub trait AgentRun: Send {
    /// Deliver a user (or A2A) message. The adapter must not emit `UserMessage` itself; the
    /// session records it.
    async fn send(&mut self, text: &str) -> anyhow::Result<()>;

    /// Answer a pending approval.
    async fn answer(&mut self, approval_id: &str, decision: ApprovalDecision)
        -> anyhow::Result<()>;

    /// Interrupt the current turn; the adapter emits `TurnEnded { Interrupted }`.
    async fn interrupt(&mut self) -> anyhow::Result<()>;

    /// Stop the process. Native session state stays on disk for a later resume.
    async fn shutdown(&mut self) -> anyhow::Result<()>;
}
