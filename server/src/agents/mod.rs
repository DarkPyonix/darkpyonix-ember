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
pub mod codex;
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
#[derive(Debug, Clone, Default)]
pub struct StartRequest {
    /// Working directory the agent sees.
    pub cwd: PathBuf,
    /// The agent's own session id, when resuming (FR-S2). `None` starts a new native session.
    pub resume_native_id: Option<String>,
    /// Model override, if the user picked one.
    pub model: Option<String>,
    /// Extra environment for the agent process, from the session's start hooks (account
    /// isolation, A2A runtime credentials, …). Applied on top of the server's environment.
    pub env: Vec<(String, String)>,
    /// Extra system-level instructions for the agent, from the session's instruction hooks
    /// (e.g. how to use the A2A tool). Claude Code gets them via `--append-system-prompt`, Codex
    /// as the thread's `developerInstructions`.
    pub instructions: Option<String>,
    /// Run the agent's tools on another computer through Codex's exec-server protocol. Only the
    /// Codex adapter uses it; other adapters are redirected through `env` (shell shim).
    pub remote: Option<RemoteExec>,
}

/// A remote executor for an agent's tools (`docs/design/INTERCEPTION.md`, option (c)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteExec {
    /// Stable id for the environment inside the agent (one per computer).
    pub environment_id: String,
    /// WebSocket URL of an exec-server speaking Codex's protocol (ember server's local relay to
    /// the node's `/v1/exec-server`).
    pub exec_server_url: String,
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
