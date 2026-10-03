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

pub mod acp;
pub mod claude_code;
pub mod codex;
pub mod scripted;

/// Which agent CLI a session runs.
///
/// Serialised (JSON, and the `agent` column of `sessions` / `accounts`) as one kebab-case string:
/// `"claude-code"`, `"codex"`, `"scripted"`, or the configured name of an Agent Client Protocol
/// agent (`"omp"`, …, see [`acp`]). The built-in strings are unchanged from before ACP agents
/// existed, so stored rows and clients keep working.
///
/// ACP agents are configured at startup, so their names are not known at compile time. They are
/// interned once into a process-wide registry ([`AcpName`]), which keeps `AgentKind` `Copy`,
/// hashable and `as_str() -> &'static str` like the built-ins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentKind {
    ClaudeCode,
    Codex,
    /// In-process fake used by tests; never offered to users.
    Scripted,
    /// An agent driven through the Agent Client Protocol, by its configured name (FR-A2).
    Acp(AcpName),
}

/// The configured name of an ACP agent, interned for the life of the process.
///
/// Names are lowercase ASCII letters, digits, `-` and `_`, start with a letter or digit, and never
/// equal a built-in agent's name. The set of names is bounded by the configuration and by the
/// names stored in the database, so interning (leaking) them is bounded too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AcpName(&'static str);

/// Built-in agent names an ACP agent may not take.
const BUILTIN_NAMES: [&str; 3] = ["claude-code", "codex", "scripted"];

/// Every interned ACP name, and whether it is configured (registered) on this server.
fn acp_names() -> &'static std::sync::Mutex<std::collections::HashMap<&'static str, bool>> {
    static NAMES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<&'static str, bool>>,
    > = std::sync::OnceLock::new();
    NAMES.get_or_init(Default::default)
}

impl AcpName {
    /// Is `name` acceptable as an ACP agent name?
    pub fn valid(name: &str) -> bool {
        let mut chars = name.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
            && name.len() <= 64
            && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
            && !BUILTIN_NAMES.contains(&name)
    }

    /// Intern `name` without registering it (a stored session of an agent that is no longer
    /// configured keeps its name, and is reported unavailable rather than becoming another agent).
    fn intern(name: &str, register: bool) -> Option<AcpName> {
        if !AcpName::valid(name) {
            return None;
        }
        let mut names = acp_names().lock().unwrap();
        let existing: Option<(&'static str, bool)> = names.get_key_value(name).map(|(k, r)| (*k, *r));
        if let Some((k, registered)) = existing {
            if register && !registered {
                names.insert(k, true);
            }
            return Some(AcpName(k));
        }
        let k: &'static str = Box::leak(name.to_string().into_boxed_str());
        names.insert(k, register);
        Some(AcpName(k))
    }

    /// Register `name` as a configured ACP agent; `None` if the name is not valid.
    pub fn register(name: &str) -> Option<AcpName> {
        AcpName::intern(name, true)
    }

    /// The registered ACP agent called `name`, if any.
    #[allow(clippy::let_and_return)] // the guard must outlive the borrow
    pub fn lookup(name: &str) -> Option<AcpName> {
        let names = acp_names().lock().unwrap();
        let found = names.get_key_value(name).filter(|(_, r)| **r).map(|(k, _)| AcpName(*k));
        found
    }

    pub fn as_str(self) -> &'static str {
        self.0
    }
}

impl AgentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "claude-code",
            AgentKind::Codex => "codex",
            AgentKind::Scripted => "scripted",
            AgentKind::Acp(name) => name.as_str(),
        }
    }

    /// A built-in agent, or an ACP agent configured on this server (API input).
    pub fn parse(s: &str) -> Option<AgentKind> {
        match s {
            "claude-code" => Some(AgentKind::ClaudeCode),
            "codex" => Some(AgentKind::Codex),
            "scripted" => Some(AgentKind::Scripted),
            _ => AcpName::lookup(s).map(AgentKind::Acp),
        }
    }

    /// Like [`AgentKind::parse`], but a valid ACP name that is not configured (any more) is kept
    /// as that ACP agent instead of being rejected. For rows read back from the database.
    pub fn from_stored(s: &str) -> Option<AgentKind> {
        AgentKind::parse(s).or_else(|| AcpName::intern(s, false).map(AgentKind::Acp))
    }

    /// Is this an Agent Client Protocol agent?
    pub fn is_acp(self) -> bool {
        matches!(self, AgentKind::Acp(_))
    }
}

impl Serialize for AgentKind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AgentKind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<AgentKind, D::Error> {
        let s = String::deserialize(d)?;
        AgentKind::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("unknown agent {s}")))
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
    /// Extra stdio MCP servers for this agent process (e.g. the project's browser, FR-R3), added
    /// to the agent's own configuration: Claude Code via `--mcp-config`, Codex via `-c
    /// mcp_servers.<name>.…` overrides. The user's own MCP servers stay.
    pub mcp_servers: Vec<McpServer>,
    /// The node API of the session's current computer, when it is not this server. Set for
    /// adapters that perform the agent's tool I/O themselves: the ACP adapter serves the agent's
    /// `fs/*` and `terminal/*` requests through it ([`acp`]). `None` = this server.
    pub computer: Option<ember_node::client::NodeClient>,
}

/// A stdio MCP server handed to an agent through its documented CLI configuration (E2).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct McpServer {
    /// Name the agent sees (a TOML bare key for Codex: letters, digits, `-`, `_`).
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    /// Seconds the agent should allow for the server to start (`npx` may download it first).
    pub startup_timeout_secs: Option<u32>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_kinds_serialise_as_before() {
        for (kind, s) in [
            (AgentKind::ClaudeCode, "claude-code"),
            (AgentKind::Codex, "codex"),
            (AgentKind::Scripted, "scripted"),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), serde_json::json!(s));
            assert_eq!(serde_json::from_value::<AgentKind>(serde_json::json!(s)).unwrap(), kind);
            assert_eq!(AgentKind::parse(s), Some(kind));
        }
    }

    #[test]
    fn acp_kinds_are_strings_and_need_registration() {
        assert_eq!(AgentKind::parse("acp-test-unregistered"), None);
        assert!(serde_json::from_value::<AgentKind>(serde_json::json!("acp-test-unregistered")).is_err());
        // A stored row keeps its name even when the agent is not configured.
        let stored = AgentKind::from_stored("acp-test-unregistered").unwrap();
        assert_eq!(stored.as_str(), "acp-test-unregistered");
        assert!(stored.is_acp());

        let name = AcpName::register("acp-test-kind").unwrap();
        let kind = AgentKind::Acp(name);
        assert_eq!(serde_json::to_value(kind).unwrap(), serde_json::json!("acp-test-kind"));
        assert_eq!(serde_json::from_value::<AgentKind>(serde_json::json!("acp-test-kind")).unwrap(), kind);
        assert_eq!(AgentKind::parse("acp-test-kind"), Some(kind));
        assert_eq!(AgentKind::from_stored("acp-test-kind"), Some(kind));
        // Registering again returns the same interned name.
        assert_eq!(AcpName::register("acp-test-kind"), Some(name));
    }

    #[test]
    fn acp_names_cannot_shadow_builtins_or_be_odd() {
        for bad in ["codex", "claude-code", "scripted", "", "Omp", "-x", "a b", "a/b"] {
            assert_eq!(AcpName::register(bad), None, "{bad:?}");
        }
        assert!(AcpName::valid("omp"));
        assert!(AcpName::valid("gemini-acp"));
    }
}
