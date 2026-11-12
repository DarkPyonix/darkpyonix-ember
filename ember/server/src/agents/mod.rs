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
pub mod antigravity;
pub mod claude_code;
pub mod codex;
pub mod proc;
pub mod scripted;

/// Which agent CLI a session runs.
///
/// Serialised (JSON, and the `agent` column of `sessions` / `accounts`) as one kebab-case string:
/// `"claude-code"`, `"codex"`, `"antigravity"`, `"scripted"`, or the configured name of an Agent
/// Client Protocol agent (`"omp"`, …, see [`acp`]). The built-in strings are unchanged from before
/// ACP agents existed, so stored rows and clients keep working.
///
/// ACP agents are configured at startup, so their names are not known at compile time. They are
/// interned once into a process-wide registry ([`AcpName`]), which keeps `AgentKind` `Copy`,
/// hashable and `as_str() -> &'static str` like the built-ins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentKind {
    ClaudeCode,
    Codex,
    /// Google Antigravity's `agy` CLI.
    Antigravity,
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
const BUILTIN_NAMES: [&str; 4] = ["claude-code", "codex", "antigravity", "scripted"];

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
            AgentKind::Antigravity => "antigravity",
            AgentKind::Scripted => "scripted",
            AgentKind::Acp(name) => name.as_str(),
        }
    }

    /// A built-in agent, or an ACP agent configured on this server (API input).
    pub fn parse(s: &str) -> Option<AgentKind> {
        match s {
            "claude-code" => Some(AgentKind::ClaudeCode),
            "codex" => Some(AgentKind::Codex),
            "antigravity" => Some(AgentKind::Antigravity),
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
    /// as the thread's `developerInstructions`, Antigravity as a rules file in its session root.
    pub instructions: Option<String>,
    /// Run the agent's tools on another computer through Codex's exec-server protocol. Only the
    /// Codex adapter uses it; other adapters are redirected through `env` (shell shim).
    pub remote: Option<RemoteExec>,
    /// Extra stdio MCP servers for this agent process (e.g. the project's browser, FR-R3), added
    /// to the agent's own configuration: Claude Code via `--mcp-config`, Codex via `-c
    /// mcp_servers.<name>.…` overrides, Antigravity via `mcp_config.json` in its session root.
    /// The user's own MCP servers stay.
    pub mcp_servers: Vec<McpServer>,
    /// The node API of the session's current computer, when it is not this server. Set for
    /// adapters that perform the agent's tool I/O themselves: the ACP adapter serves the agent's
    /// `fs/*` and `terminal/*` requests through it ([`acp`]). `None` = this server.
    pub computer: Option<ember_node::client::NodeClient>,
    /// Start as a native fork of another session instead of a new or resumed one (FR-S5). Set only
    /// for a fork whose parent the agent can fork. `resume_native_id` is `None` then. An adapter
    /// that fails to fork returns an error; the session layer falls back to the conversation copy.
    pub fork_from: Option<ForkFrom>,
    /// Memory limit of the agent's process tree in MiB (FR-S8, FR-L20). Handed to the shim, which
    /// kills only this session's agent above it. `None` = the server default
    /// (`EMBER_SESSION_MEM_LIMIT_MB`), else no limit. Set by `Sessions::ensure_live` from
    /// `Sessions::set_mem_limit_mb`; a start-config hook may override it.
    pub mem_limit_mb: Option<u64>,
    /// Tools the harness may call without asking, each one exact tool name (never a pattern).
    /// Claude Code only, passed as `--allowedTools`: without the rule a call to an MCP tool is
    /// denied in `-p` mode (measured, FR-T12). Other adapters ignore it.
    pub allowed_tools: Vec<String>,
}

/// The native session a fork starts from (FR-S5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkFrom {
    /// The parent's native session id.
    pub native_id: String,
    /// Completed turns of the parent to keep, oldest first. `None` = the whole session (the
    /// parent's last completed turn is its end).
    pub keep_turns: Option<usize>,
}

/// What an agent's own fork can do (FR-S5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeFork {
    /// No native fork: the fork copies the conversation.
    None,
    /// Forks the whole native session: only from the parent's last completed turn.
    Latest,
    /// Forks through a chosen completed turn.
    AtTurn,
}

/// A stdio MCP server handed to an agent through its documented CLI configuration (E2).
#[derive(Clone, PartialEq, Eq, Default)]
pub struct McpServer {
    /// Name the agent sees (a TOML bare key for Codex: letters, digits, `-`, `_`).
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    /// Seconds the agent should allow for the server to start (`npx` may download it first).
    pub startup_timeout_secs: Option<u32>,
    /// Environment for the MCP server process (central registry, FR-A7). Values may be secrets:
    /// they are never put on a command line. Claude Code reads them from a private (`0600`)
    /// `--mcp-config` file, Codex from its own process environment through
    /// `mcp_servers.<name>.env_vars`, and `Debug` prints only the keys.
    pub env: Vec<(String, String)>,
}

impl std::fmt::Debug for McpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServer")
            .field("name", &self.name)
            .field("command", &self.command)
            .field("args", &self.args)
            .field("startup_timeout_secs", &self.startup_timeout_secs)
            .field("env_keys", &self.env.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>())
            .finish()
    }
}

/// A remote executor for an agent's tools (`docs/design/INTERCEPTION.md`, option (c)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteExec {
    /// Stable id for the environment inside the agent (one per computer).
    pub environment_id: String,
    /// WebSocket URL of an exec-server speaking Codex's protocol (ember server's local relay to
    /// the node's `/exec-server`).
    pub exec_server_url: String,
}

/// Result of probing for an installed agent (FR-A6).
#[derive(Debug, Clone, Serialize)]
pub struct Detected {
    pub kind: AgentKind,
    pub installed: bool,
    pub version: Option<String>,
}

/// A detection with where it found the CLI and when (FR-A6.1).
#[derive(Debug, Clone, Serialize)]
pub struct Probe {
    pub kind: AgentKind,
    pub installed: bool,
    pub version: Option<String>,
    /// The resolved executable, `None` when it is not found.
    pub path: Option<String>,
    /// Epoch milliseconds when the probe ran.
    pub detected_at: i64,
}

/// A version token is accepted only when it starts with a digit (after an optional `v`), so
/// that garbage output is never shown as a version (FR-A6.1).
pub(crate) fn version_token(tok: &str) -> Option<String> {
    let t = tok.trim();
    let digits = t.strip_prefix('v').unwrap_or(t);
    digits.chars().next().filter(char::is_ascii_digit).map(|_| t.to_string())
}

/// Resolve a command the way the OS would: a path with a separator is taken as it is when it is
/// a file; a bare name is searched in `PATH`. `None` when nothing executable is found.
pub fn resolve_executable(bin: &std::path::Path) -> Option<std::path::PathBuf> {
    #[cfg(unix)]
    let ok = |p: &std::path::Path| {
        use std::os::unix::fs::PermissionsExt;
        p.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    };
    #[cfg(not(unix))]
    let ok = |p: &std::path::Path| p.metadata().map(|m| m.is_file()).unwrap_or(false);
    if bin.components().count() > 1 || bin.is_absolute() {
        return ok(bin).then(|| bin.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    for d in std::env::split_paths(&path) {
        let c = d.join(bin);
        if ok(&c) {
            return Some(c);
        }
        #[cfg(windows)]
        if let Some(name) = bin.to_str() {
            for ext in [".exe", ".cmd", ".bat"] {
                let with_ext = d.join(format!("{name}{ext}"));
                if ok(&with_ext) {
                    return Some(with_ext);
                }
            }
        }
    }
    None
}

#[async_trait]
pub trait AgentAdapter: Send + Sync {
    fn kind(&self) -> AgentKind;

    /// The command `detect` runs, to report where the CLI was found (FR-A6.1). Default: none.
    fn binary(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// Is the agent installed on this server, and which version? Missing is not an error.
    async fn detect(&self) -> Detected;

    /// What the agent's own session fork can do (FR-S5). Default: nothing, so a fork copies the
    /// conversation.
    fn native_fork(&self) -> NativeFork {
        NativeFork::None
    }

    /// Whether an agent that outlived the server under its shim can be rejoined as it runs
    /// (FR-S8). Default: no, the session resumes natively on its next message.
    fn rejoins_shim(&self) -> bool {
        false
    }

    /// Whether the output of an agent that outlived the server mid-turn can be read back without
    /// rejoining it (FR-S8): [`AgentAdapter::drain_turn_end`] and
    /// [`AgentAdapter::translate_drained`] are then used before the old process is stopped.
    /// Default: no.
    fn drains_shim(&self) -> bool {
        false
    }

    /// Whether `line` of drained output ends the draining (the turn finished, or the agent asks
    /// something nobody can answer any more).
    fn drain_turn_end(&self, _line: &str) -> bool {
        false
    }

    /// The events behind output lines drained from a surviving agent (FR-S8).
    fn translate_drained(&self, _lines: &[String]) -> Vec<AgentEvent> {
        Vec::new()
    }

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

    /// Prompts that were pending when the previous server died, found in the store after this
    /// run rejoined its surviving agent (FR-S8). Each is `(approval_id, tool, input, needs_person)`. An
    /// adapter that holds its pending approvals in memory rebuilds them here. Default: nothing.
    async fn restore_pending_approvals(&mut self, _pending: Vec<(String, String, serde_json::Value, bool)>) {}

    /// Questions that were pending when the previous server died, found in the store after this
    /// run rejoined its surviving agent: `(question id, questions)`. Default: nothing.
    async fn restore_pending_questions(&mut self, _pending: Vec<(String, Vec<crate::events::Question>)>) {}

    /// Answer a pending question with the person's answers (already validated against the
    /// stored question by the session). Default: the agent has no questions.
    async fn answer_question(
        &mut self,
        _question_id: &str,
        _answers: &[crate::events::QuestionAnswer],
    ) -> anyhow::Result<()> {
        anyhow::bail!("this agent does not support questions")
    }

    /// The person skips a pending question: the agent is told that nobody answered. Default: the
    /// agent has no questions.
    async fn decline_question(&mut self, _question_id: &str) -> anyhow::Result<()> {
        anyhow::bail!("this agent does not support questions")
    }

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
            (AgentKind::Antigravity, "antigravity"),
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
        for bad in ["codex", "claude-code", "antigravity", "scripted", "", "Omp", "-x", "a b", "a/b"] {
            assert_eq!(AcpName::register(bad), None, "{bad:?}");
        }
        assert!(AcpName::valid("omp"));
        assert!(AcpName::valid("gemini-acp"));
    }
}
