//! Agent Client Protocol (ACP) adapter: drives any ACP agent over stdio (SPEC FR-A1, FR-A2).
//!
//! # Protocol
//!
//! ACP protocol version **1** (`protocolVersion: 1`), as specified by
//! <https://agentclientprotocol.com/protocol/v1> and the schema crate
//! `agent-client-protocol-schema` at tag `schema-v1.24.1` (2026-09-30) of
//! <https://github.com/agentclientprotocol/agent-client-protocol>. JSON-RPC 2.0, one JSON object
//! per line on the agent's stdin/stdout ("stdio" transport); the agent's stderr is logged.
//!
//! Ember is the ACP **client**. Methods it calls on the agent:
//! - `initialize` with `clientCapabilities {fs: {readTextFile, writeTextFile}, terminal: true}`;
//!   an agent answering another `protocolVersion` is refused.
//! - `session/new {cwd, mcpServers}` → `sessionId`, the native session id (FR-S2).
//! - Native resume: `session/resume` when the agent advertises
//!   `agentCapabilities.sessionCapabilities.resume` (no history replay), otherwise `session/load`
//!   when it advertises `loadSession`. `session/load` replays the whole conversation as
//!   `session/update` notifications before it answers; Ember already stored that history, so the
//!   replay is dropped. An agent with neither starts a new native session, with a notice.
//! - `session/prompt` per user message. The response's `stopReason` ends the turn
//!   (`cancelled` → interrupted). Messages sent mid-turn are queued and prompted after the turn
//!   (ACP has no steering; some agents, OMP among them, cancel the running turn on a second
//!   prompt).
//! - `session/cancel` (notification) to interrupt; pending permission requests are then answered
//!   with the `cancelled` outcome, as the spec requires.
//!
//! Agent → client notifications: `session/update`, mapped to [`AgentEvent`]s:
//! - `agent_message_chunk` → `AssistantDelta`, and `AssistantMessage` once the message is over
//!   (the `messageId` changes, a tool call or plan starts, or the turn ends);
//! - `tool_call` / `tool_call_update` → `ToolCall`, then `ToolResult` once the status is
//!   `completed` or `failed`;
//! - `plan` → a `ToolCall` named `Plan` with an immediate `ToolResult` (the way Claude Code's
//!   `TodoWrite` already appears), since the event model has no plan event;
//! - `user_message_chunk`, `agent_thought_chunk`, `available_commands_update`,
//!   `current_mode_update`, `config_option_update`, `session_info_update`, `usage_update` and
//!   unknown kinds are ignored.
//!
//! Token usage comes from `PromptResponse.usage {inputTokens, outputTokens}` when the agent sends
//! it (an unstable field in the schema, `unstable_end_turn_token_usage`; OMP sends it).
//!
//! Agent → client requests, answered by Ember:
//! - `session/request_permission` → `ApprovalRequested`; [`AgentRun::answer`] selects an option by
//!   its `kind`: allow once → `allow_once`, always allow → `allow_always`, deny → `reject_once`
//!   (each falling back to the other option of the same polarity). No matching option → the
//!   `cancelled` outcome.
//! - `fs/read_text_file`, `fs/write_text_file`: served on this server, or on the session's
//!   computer through its node API when [`StartRequest::computer`] is set.
//! - `terminal/create`, `terminal/output`, `terminal/wait_for_exit`, `terminal/kill`,
//!   `terminal/release`: commands run directly (argv, no shell), on this server or through the
//!   node's `/exec`.
//! - Anything else (e.g. `elicitation/create`) gets JSON-RPC error `-32601`.
//!
//! # Configuration
//!
//! Each ACP agent is an [`AcpConfig`]: a name (the [`AgentKind::Acp`] name sessions are created
//! with), a command and its arguments, plus argument templates for a model (`{model}`) and for
//! extra instructions (`{instructions}`), since ACP itself has neither. Agents without an
//! instructions template get the instructions in front of the first prompt of each process.
//! [`configs_from_env`] reads `EMBER_ACP_AGENTS` (JSON) or `EMBER_ACP_AGENTS_FILE`:
//!
//! ```json
//! ["omp", {"name": "gemini", "command": "/opt/gemini", "env": {"GEMINI_CLI_HOME": "/x"}}]
//! ```
//!
//! A string is a preset name; an object whose name is a preset overrides the preset's fields.
//! Presets are opt-in: an agent is offered only when its name is listed here (`omp`, `gemini`).
//!
//! # Typed turn errors (FR-A10b, FR-A10c)
//!
//! A JSON-RPC error from `session/new` or `session/prompt` is classified by [`classify_rpc_error`]
//! (a heuristic on the code and on substrings of the message; the real wording of any agent is
//! unverified): authentication problems become `AgentEvent::TurnError {code: "auth_required"}`,
//! quota and rate limits `{code: "quota_exceeded"}`, each followed by a failed `TurnEnded`. The
//! session stays usable. A `session/new` that fails with an authentication error is retried at
//! the next prompt, so signing in does not need a new session.
//!
//! # Gemini CLI preset
//!
//! `gemini --acp` (see "Gemini CLI facts" in `docs/SPEC.md`). The ACP flag was
//! `--experimental-acp` in older versions: when the first start fails within 2 seconds and a
//! one-off run of the same command exits non-zero with an unknown-option message
//! ([`rejects_acp_flag`]), the start is retried once with `--experimental-acp`. Only for the
//! preset's own arguments; an override of `args` is never retried.
//!
//! # OMP (oh-my-pi) preset
//!
//! `omp acp`: the `acp` subcommand of `omp` runs the ACP server over stdio
//! (`packages/coding-agent/src/commands/acp.ts` and `src/modes/acp/acp-agent.ts` in
//! <https://github.com/can1357/oh-my-pi>, release v18.5.0, 2026-10-03; README section "ACP",
//! speak to editors). Read from source, not run (OMP is not installed here): it answers
//! `protocolVersion` 1 with `loadSession: true` and `sessionCapabilities {list, fork, resume,
//! close}`, offers permission options `allow_once` / `allow_always` / `reject_once` /
//! `reject_always`, routes `bash` through `terminal/create` (argv `[<bash>, "-l", "-c", cmd]`),
//! `read` / `write` through `fs/*`, and returns `usage` on the prompt response. The subcommand
//! parses the root CLI flags, so `--model <m>` and `--append-system-prompt <text>` are passed
//! after `acp` (flag table `src/cli/flag-tables.ts`); that they take effect in ACP mode is
//! unverified. `EMBER_OMP_BIN` picks the binary.
//!
//! # Another computer
//!
//! With [`StartRequest::computer`] set, `fs/*` and `terminal/*` go to that computer's node. The
//! agent process itself still runs here, so an agent's tools that do not use the ACP client
//! methods read this server's filesystem; for those the project mount (`computers::mount`) is
//! acquired for ACP sessions as for Claude Code.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use async_trait::async_trait;
use ember_node::client::NodeClient;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, watch, Notify};

use super::proc::{spawn_agent, AgentCommand, ProcHandle};
use super::{AcpName, AgentAdapter, AgentKind, AgentRun, Detected, ForkFrom, McpServer, NativeFork, StartRequest};
use crate::events::{AgentEvent, ApprovalDecision, TurnOutcome};
use crate::session_shim::{apply_env, ENV_CLEAR};

/// The Gemini CLI's ACP flag, and the older one it replaced (FR-A10, SPEC "Gemini CLI facts").
const GEMINI_ACP_FLAG: &str = "--acp";
const GEMINI_ACP_FALLBACK_FLAG: &str = "--experimental-acp";
/// A start that fails within this long may have been a rejected flag.
const FLAG_PROBE_WINDOW: Duration = Duration::from_secs(2);
/// Extra spawn tries when the probe command is "text file busy" (ETXTBSY), and the pause between.
const BUSY_RETRIES: u32 = 3;
const BUSY_PAUSE: Duration = Duration::from_millis(20);

/// What the one-off flag probe saw. Only `StillRunning` means the start flag was accepted
/// (FR-A10); the other three say nothing about the flag.
#[derive(Debug)]
enum ProbeOutcome {
    /// The command exited within the window: success flag and stderr plus stdout.
    Exited { ok: bool, text: String },
    /// The command was still running after [`FLAG_PROBE_WINDOW`]: the flag is accepted.
    StillRunning,
    /// The command could not be started: the io error kind and its redacted text.
    SpawnFailed(std::io::ErrorKind, String),
    /// Waiting for the command failed, or its pipes were missing: redacted text.
    WaitFailed(String),
}

/// True for ETXTBSY ("text file busy"): an executable written a moment ago while another thread
/// forks. Short-lived, so the spawn is worth retrying.
fn is_busy(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::ExecutableFileBusy
}

/// The ACP major version this client speaks.
pub const PROTOCOL_VERSION: u64 = 1;
/// How long to wait for a setup response (`initialize`, `session/new`, …).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// `session/load` replays the whole conversation before answering.
const LOAD_TIMEOUT: Duration = Duration::from_secs(600);
/// How long `shutdown` waits for the agent to exit after stdin closes.
const EXIT_GRACE: Duration = Duration::from_secs(5);
/// Output kept per terminal when the agent sets no `outputByteLimit`.
const DEFAULT_OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
/// Released terminals whose final output is kept for tool results that reference them.
const RELEASED_KEEP: usize = 64;

/// JSON-RPC error codes (JSON-RPC 2.0 and ACP's `ErrorCode`).
const INVALID_PARAMS: i64 = -32602;
const METHOD_NOT_FOUND: i64 = -32601;
const INTERNAL_ERROR: i64 = -32603;
const RESOURCE_NOT_FOUND: i64 = -32002;

// ---------------------------------------------------------------------------------------------
// Configuration

/// Variables the `gemini` child may inherit from the server (FR-A10). Everything else in the
/// server's environment (tokens, other agents' keys, Ember's own settings) stays out.
///
/// - Process basics: `HOME` (the CLI reads `~/.gemini`), `PATH` (it is a Node script that finds
///   `node` and tools), `LANG`, `LC_ALL` (text encoding), `TMPDIR`, `USER`, `TERM`.
/// - XDG directories: `XDG_CONFIG_HOME`, `XDG_CACHE_HOME`, `XDG_DATA_HOME` (where it keeps config
///   and caches when they are set).
/// - The CLI's own credentials and mode: `GEMINI_API_KEY`, `GOOGLE_API_KEY`, `GOOGLE_CLOUD_PROJECT`,
///   `GOOGLE_CLOUD_LOCATION`, `GOOGLE_GENAI_USE_VERTEXAI`, `GOOGLE_APPLICATION_CREDENTIALS`. Ember
///   never reads their values; they are passed through as they are.
/// - Network: `HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY` (both cases) and the CA bundle variables
///   `SSL_CERT_FILE`, `NODE_EXTRA_CA_CERTS`, so a proxied or inspected connection still works.
pub const GEMINI_ENV_ALLOW: &[&str] = &[
    "HOME", "PATH", "LANG", "LC_ALL", "TMPDIR", "USER", "TERM",
    "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME",
    "GEMINI_API_KEY", "GOOGLE_API_KEY", "GOOGLE_CLOUD_PROJECT", "GOOGLE_CLOUD_LOCATION",
    "GOOGLE_GENAI_USE_VERTEXAI", "GOOGLE_APPLICATION_CREDENTIALS",
    "HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY", "http_proxy", "https_proxy", "no_proxy",
    "SSL_CERT_FILE", "NODE_EXTRA_CA_CERTS",
];

/// Which part of the server's environment an agent process inherits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvPolicy {
    /// All of it (`omp`, and any configured agent).
    Inherit,
    /// Only the listed variables (those the server has), then the config's and the request's own.
    Allowlist(&'static [&'static str]),
}

/// One configured ACP agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpConfig {
    /// The agent kind name sessions use (`"omp"`). See [`AcpName::valid`].
    pub name: String,
    pub command: PathBuf,
    /// Arguments that put the CLI into ACP mode (`["acp"]`).
    pub args: Vec<String>,
    /// Appended when the session picked a model; `{model}` is replaced.
    pub model_args: Vec<String>,
    /// Appended when the session has instructions; `{instructions}` is replaced. Empty = the
    /// instructions go in front of the first prompt of each agent process.
    pub instructions_args: Vec<String>,
    /// Arguments that print the version, for detection (FR-A6).
    pub version_args: Vec<String>,
    /// Extra environment for the agent process.
    pub env: Vec<(String, String)>,
    /// How much of the server's environment the process inherits (`Allowlist` for `gemini`).
    pub env_policy: EnvPolicy,
}

impl AcpConfig {
    /// A preset by name: `omp`, `gemini`. `bin` overrides the preset's command.
    pub fn preset(name: &str, bin: Option<PathBuf>) -> Option<AcpConfig> {
        match name {
            "omp" => Some(AcpConfig {
                name: "omp".into(),
                command: bin.unwrap_or_else(|| "omp".into()),
                args: vec!["acp".into()],
                model_args: vec!["--model".into(), "{model}".into()],
                instructions_args: vec!["--append-system-prompt".into(), "{instructions}".into()],
                version_args: vec!["--version".into()],
                env: Vec::new(),
                env_policy: EnvPolicy::Inherit,
            }),
            // Gemini CLI (FR-A10): `-m <model>` for the model; no instructions argument, so the
            // instructions go in front of the first prompt and the user's GEMINI.md is untouched.
            "gemini" => Some(AcpConfig {
                name: "gemini".into(),
                command: bin.unwrap_or_else(|| "gemini".into()),
                args: vec![GEMINI_ACP_FLAG.into()],
                model_args: vec!["-m".into(), "{model}".into()],
                instructions_args: Vec::new(),
                version_args: vec!["--version".into()],
                env: Vec::new(),
                env_policy: EnvPolicy::Allowlist(GEMINI_ENV_ALLOW),
            }),
            _ => None,
        }
    }

    /// The environment entries for a process of this agent: for an allow-list, the
    /// [`ENV_CLEAR`] marker and the allowed variables the server has; then the config's `env`.
    pub fn child_env(&self) -> Vec<(String, String)> {
        let mut env = Vec::new();
        if let EnvPolicy::Allowlist(names) = self.env_policy {
            env.push((ENV_CLEAR.to_string(), "1".to_string()));
            for name in names {
                if let Ok(v) = std::env::var(name) {
                    env.push((name.to_string(), v));
                }
            }
        }
        env.extend(self.env.iter().cloned());
        env
    }

    /// The arguments of the one retry of a start that failed fast, when the failure may be the
    /// agent not knowing the ACP flag (Gemini's `--acp` against `--experimental-acp`).
    fn fallback_args(&self) -> Option<Vec<String>> {
        (self.name == "gemini" && self.args == [GEMINI_ACP_FLAG]).then(|| vec![GEMINI_ACP_FALLBACK_FLAG.into()])
    }

    /// The full argument list for a process serving `req`.
    pub fn process_args(&self, req: &StartRequest) -> Vec<String> {
        self.process_args_with(&self.args, req)
    }

    fn process_args_with(&self, base: &[String], req: &StartRequest) -> Vec<String> {
        let mut args = base.to_vec();
        if let Some(model) = &req.model {
            args.extend(self.model_args.iter().map(|a| a.replace("{model}", model)));
        }
        if let (Some(text), false) = (&req.instructions, self.instructions_args.is_empty()) {
            args.extend(self.instructions_args.iter().map(|a| a.replace("{instructions}", text)));
        }
        args
    }
}

/// An entry of `EMBER_ACP_AGENTS`: a preset name or a full (or overriding) definition.
#[derive(Deserialize)]
#[serde(untagged)]
enum ConfigEntry {
    Preset(String),
    Spec(ConfigSpec),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigSpec {
    name: String,
    command: Option<PathBuf>,
    args: Option<Vec<String>>,
    model_args: Option<Vec<String>>,
    instructions_args: Option<Vec<String>>,
    version_args: Option<Vec<String>>,
    env: Option<BTreeMap<String, String>>,
}

/// Parse an `EMBER_ACP_AGENTS` value. `preset_bin(name)` gives a preset's binary override
/// (`EMBER_OMP_BIN` for `omp`).
pub fn parse_configs(
    json_text: &str,
    preset_bin: impl Fn(&str) -> Option<PathBuf>,
) -> anyhow::Result<Vec<AcpConfig>> {
    let entries: Vec<ConfigEntry> =
        serde_json::from_str(json_text).context("ACP agents: expected a JSON list")?;
    let mut out: Vec<AcpConfig> = Vec::new();
    for entry in entries {
        let config = match entry {
            ConfigEntry::Preset(name) => AcpConfig::preset(&name, preset_bin(&name))
                .ok_or_else(|| anyhow!("ACP agents: unknown preset {name:?}"))?,
            ConfigEntry::Spec(spec) => {
                let base = AcpConfig::preset(&spec.name, preset_bin(&spec.name));
                let command = match (spec.command, &base) {
                    (Some(c), _) => c,
                    (None, Some(b)) => b.command.clone(),
                    (None, None) => bail!("ACP agent {:?}: \"command\" is required", spec.name),
                };
                let base = base.unwrap_or(AcpConfig {
                    name: spec.name.clone(),
                    command: command.clone(),
                    args: Vec::new(),
                    model_args: Vec::new(),
                    instructions_args: Vec::new(),
                    version_args: vec!["--version".into()],
                    env: Vec::new(),
                    env_policy: EnvPolicy::Inherit,
                });
                AcpConfig {
                    name: spec.name,
                    command,
                    args: spec.args.unwrap_or(base.args),
                    model_args: spec.model_args.unwrap_or(base.model_args),
                    instructions_args: spec.instructions_args.unwrap_or(base.instructions_args),
                    version_args: spec.version_args.unwrap_or(base.version_args),
                    env: match spec.env {
                        Some(env) => env.into_iter().collect(),
                        None => base.env,
                    },
                    env_policy: base.env_policy,
                }
            }
        };
        if !AcpName::valid(&config.name) {
            bail!(
                "ACP agent name {:?} is not allowed (lowercase letters, digits, '-', '_'; not a \
                 built-in agent's name)",
                config.name
            );
        }
        if out.iter().any(|c| c.name == config.name) {
            bail!("ACP agent {:?} is configured twice", config.name);
        }
        out.push(config);
    }
    Ok(out)
}

/// The configured ACP agents: `EMBER_ACP_AGENTS` (JSON), else the JSON file named by
/// `EMBER_ACP_AGENTS_FILE`, else none.
pub fn configs_from_env() -> anyhow::Result<Vec<AcpConfig>> {
    let text = match (std::env::var("EMBER_ACP_AGENTS"), std::env::var_os("EMBER_ACP_AGENTS_FILE")) {
        (Ok(text), _) if !text.trim().is_empty() => text,
        (_, Some(path)) => std::fs::read_to_string(&path)
            .with_context(|| format!("reading EMBER_ACP_AGENTS_FILE {}", Path::new(&path).display()))?,
        _ => return Ok(Vec::new()),
    };
    parse_configs(&text, |name| match name {
        "omp" => std::env::var_os("EMBER_OMP_BIN").map(PathBuf::from),
        "gemini" => std::env::var_os("EMBER_GEMINI_BIN").map(PathBuf::from),
        _ => None,
    })
}

// ---------------------------------------------------------------------------------------------
// Adapter

pub struct AcpAdapter {
    config: AcpConfig,
    kind: AgentKind,
}

impl AcpAdapter {
    /// Registers the config's name as an agent kind.
    pub fn new(config: AcpConfig) -> anyhow::Result<AcpAdapter> {
        let name = AcpName::register(&config.name)
            .ok_or_else(|| anyhow!("ACP agent name {:?} is not allowed", config.name))?;
        Ok(AcpAdapter { config, kind: AgentKind::Acp(name) })
    }

    pub fn config(&self) -> &AcpConfig {
        &self.config
    }
}

#[async_trait]
impl AgentAdapter for AcpAdapter {
    fn kind(&self) -> AgentKind {
        self.kind
    }

    /// Whether the agent has `session/fork` is only known after `initialize`; a start that finds
    /// it missing fails and the session layer falls back to the conversation copy (FR-S5). The
    /// request carries no turn, so only the last one can be forked.
    fn native_fork(&self) -> NativeFork {
        NativeFork::Latest
    }

    /// A turn in flight when the server died is read back from the surviving agent (FR-S8). The
    /// agent itself cannot be rejoined: its `initialize` and `session/*` handshake belongs to the
    /// dead connection, and ACP has no method to attach another client to a running turn
    /// (`session/load` and `session/resume` open a session on a connection, they do not rejoin an
    /// in-flight `session/prompt`). So the output is drained, the agent stopped, and the session
    /// resumes natively on its next message.
    fn drains_shim(&self) -> bool {
        true
    }

    fn drain_turn_end(&self, line: &str) -> bool {
        drain_ends_turn(line)
    }

    fn translate_drained(&self, lines: &[String]) -> Vec<AgentEvent> {
        translate_drained_lines(lines)
    }

    fn binary(&self) -> Option<PathBuf> {
        Some(self.config.command.clone())
    }

    async fn detect(&self) -> Detected {
        let mut cmd = Command::new(&self.config.command);
        apply_env(&mut cmd, &self.config.child_env());
        let out = tokio::time::timeout(
            Duration::from_secs(10),
            cmd.args(&self.config.version_args).stdin(Stdio::null()).kill_on_drop(true).output(),
        )
        .await;
        match out {
            Ok(Ok(out)) if out.status.success() => Detected {
                kind: self.kind,
                installed: true,
                version: parse_version(&String::from_utf8_lossy(&out.stdout)),
            },
            _ => Detected { kind: self.kind, installed: false, version: None },
        }
    }

    async fn start(
        &self,
        req: StartRequest,
        events: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<Box<dyn AgentRun>> {
        let began = std::time::Instant::now();
        match self.start_with(&self.config.args, req.clone(), events.clone()).await {
            Ok(run) => Ok(run),
            Err(first) => {
                let Some(fallback) = self.config.fallback_args() else { return Err(first) };
                if began.elapsed() > FLAG_PROBE_WINDOW {
                    return Err(first);
                }
                match self.probe_start_args().await {
                    // Still running after the window: the flag is accepted, the first error stands.
                    ProbeOutcome::StillRunning => return Err(first),
                    ProbeOutcome::Exited { ok, text } => {
                        if ok || !rejects_acp_flag(&text) {
                            return Err(first);
                        }
                    }
                    // The probe itself failed: nothing is known about the flag, so no retry.
                    outcome @ (ProbeOutcome::SpawnFailed(..) | ProbeOutcome::WaitFailed(_)) => {
                        let (name, kind) = match &outcome {
                            ProbeOutcome::SpawnFailed(kind, _) => ("spawn_failed", format!("{kind:?}")),
                            _ => ("wait_failed", "n/a".to_string()),
                        };
                        tracing::warn!(
                            target: "acp", agent = %self.config.name, outcome = name, io_error_kind = %kind,
                            "the flag probe could not run; not retrying with {GEMINI_ACP_FALLBACK_FLAG}"
                        );
                        return Err(first);
                    }
                }
                tracing::warn!(
                    target: "acp", agent = %self.config.name,
                    "the CLI rejected {GEMINI_ACP_FLAG}; retrying once with {GEMINI_ACP_FALLBACK_FLAG}"
                );
                self.start_with(&fallback, req, events).await.map_err(|second| {
                    // The first error can carry the agent's own words: redacted (FR-A10).
                    let first = redact_upstream(&format!("{first:#}"));
                    second.context(format!("{GEMINI_ACP_FLAG} was rejected by the CLI ({first})"))
                })
            }
        }
    }
}

impl AcpAdapter {
    /// Run the command once with its start arguments and no stdin, to see why a fast start failed.
    /// Only [`ProbeOutcome::StillRunning`] means the flag was accepted; see [`ProbeOutcome`].
    async fn probe_start_args(&self) -> ProbeOutcome {
        let mut child = None;
        for attempt in 0..=BUSY_RETRIES {
            let mut cmd = Command::new(&self.config.command);
            apply_env(&mut cmd, &self.config.child_env());
            cmd.args(&self.config.args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            match cmd.spawn() {
                Ok(c) => {
                    child = Some(c);
                    break;
                }
                // A script written a moment ago can be "text file busy" for a few ms while
                // another thread forks: wait and try again, a few times only.
                Err(e) if is_busy(&e) && attempt < BUSY_RETRIES => {
                    tracing::debug!(
                        target: "acp", agent = %self.config.name, attempt = attempt + 1,
                        "probe spawn hit ETXTBSY; retrying in {BUSY_PAUSE:?}"
                    );
                    tokio::time::sleep(BUSY_PAUSE).await;
                }
                Err(e) => return ProbeOutcome::SpawnFailed(e.kind(), redact_upstream(&e.to_string())),
            }
        }
        let Some(mut child) = child else {
            return ProbeOutcome::SpawnFailed(std::io::ErrorKind::ExecutableFileBusy, "spawn kept failing".into());
        };
        let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return ProbeOutcome::WaitFailed("the probe child had no stdout or stderr pipe".into());
        };
        let finished = tokio::time::timeout(FLAG_PROBE_WINDOW, async {
            let (mut o, mut e) = (Vec::new(), Vec::new());
            let _ = tokio::join!(stdout.read_to_end(&mut o), stderr.read_to_end(&mut e));
            let status = child.wait().await;
            (status, o, e)
        })
        .await;
        // Explicit cleanup, whatever happened: the probe never outlives this call. Errors (the
        // child already exited and was reaped) are of no interest and nothing is logged.
        let _ = child.kill().await;
        let _ = child.wait().await;
        let Ok((status, out, err)) = finished else { return ProbeOutcome::StillRunning };
        let status = match status {
            Ok(s) => s,
            Err(e) => return ProbeOutcome::WaitFailed(redact_upstream(&e.to_string())),
        };
        let mut text = String::from_utf8_lossy(&err).into_owned();
        text.push('\n');
        text.push_str(&String::from_utf8_lossy(&out));
        ProbeOutcome::Exited { ok: status.success(), text }
    }

    async fn start_with(
        &self,
        base_args: &[String],
        req: StartRequest,
        events: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<Box<dyn AgentRun>> {
        // On another computer the project path need not exist here (without a mount).
        let spawn_dir = if req.cwd.is_dir() { req.cwd.clone() } else { std::env::temp_dir() };
        let mut env = self.config.child_env();
        env.extend(req.env.iter().cloned());
        let proc = spawn_agent(AgentCommand {
            program: self.config.command.clone(),
            args: self.config.process_args_with(base_args, &req),
            env,
            cwd: spawn_dir,
            // `initialize` and `session/*` have to run on a fresh agent, so a surviving one is
            // restarted through the agent's native resume instead of rejoined.
            reattachable: false,
            mem_limit_mb: req.mem_limit_mb,
            os_sandbox: true,
        })
        .await
        .with_context(|| format!("starting {} (ACP agent {})", self.config.command.display(), self.config.name))?;
        let (stdin, stdout, stderr, child) = (proc.stdin, proc.stdout, proc.stderr, proc.handle);
        let name = self.config.name.clone();
        if let Some(stderr) = stderr {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    // Raw agent text: debug only, debug logs are not for sharing.
                    tracing::debug!(target: "acp", agent = %name, "{line}");
                }
            });
        }
        let prepend_instructions =
            if self.config.instructions_args.is_empty() { req.instructions.clone() } else { None };
        let setup = Setup {
            cwd: req.cwd.clone(),
            resume_native_id: req.resume_native_id.clone(),
            fork_from: req.fork_from.clone(),
            mcp_servers: req.mcp_servers.clone(),
            computer: req.computer.clone(),
            prepend_instructions,
            agent: self.config.name.clone(),
        };
        let run = connect(stdout, stdin, Some(child), setup, events).await?;
        Ok(Box::new(run))
    }
}

/// Whether `line` of output drained from a surviving ACP agent ends the draining: the response to
/// the in-flight `session/prompt` (the only response the agent can send, since the dead client's
/// other requests were answered by it), or a request to the client (a permission prompt, `fs/*`,
/// `terminal/*`) that nobody can answer any more.
pub(crate) fn drain_ends_turn(line: &str) -> bool {
    let Ok(msg) = serde_json::from_str::<Value>(line) else { return false };
    match (msg.get("method").and_then(Value::as_str), msg.get("id")) {
        (Some(_), Some(_)) => true,
        (None, Some(_)) => msg.get("result").is_some() || msg.get("error").is_some(),
        _ => false,
    }
}

/// The events behind lines drained from a surviving ACP agent: `session/update` notifications go
/// through the normal [`Mapper`]; the `session/prompt` response ends the turn like a live one
/// (usage, stop reason). Without an end line the message in progress is still flushed, and the
/// session layer ends the unfinished turn as failed.
pub(crate) fn translate_drained_lines(lines: &[String]) -> Vec<AgentEvent> {
    // Terminals belonged to the dead connection: `terminal` content falls back to a placeholder.
    let host = Host::new(PathBuf::new(), None);
    let mut mapper = Mapper::default();
    let mut out = Vec::new();
    for line in lines {
        let Ok(msg) = serde_json::from_str::<Value>(line) else { continue };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match (msg.get("method").and_then(Value::as_str), msg.get("id")) {
            (Some("session/update"), None) => {
                let update = params.get("update").cloned().unwrap_or(Value::Null);
                out.extend(mapper.on_update(&update, &host));
            }
            // A request to the client: nobody answers it; the turn cannot continue.
            (Some(_), Some(_)) => break,
            (None, Some(_)) => {
                out.extend(mapper.end_turn());
                match msg.get("error") {
                    Some(err) => {
                        let why = err.get("message").and_then(Value::as_str).unwrap_or("unknown error");
                        out.push(AgentEvent::Error { message: format!("ACP session/prompt failed: {why}") });
                        out.push(AgentEvent::TurnEnded { outcome: TurnOutcome::Failed });
                    }
                    None => {
                        out.extend(turn_end_events(msg.get("result").unwrap_or(&Value::Null), false))
                    }
                }
                return out;
            }
            _ => {}
        }
    }
    out.extend(mapper.end_turn());
    out
}

/// Whether the output of a command that exited non-zero says it does not know the `--acp` flag.
/// Used for the one `--experimental-acp` retry. Both parts are required: the output names the
/// flag (`--acp`, or the word `acp` as yargs prints "Unknown argument: acp"), and it says an
/// option, flag or argument is unknown, unrecognized, invalid or bad ("unknown option",
/// "unrecognized argument"...). "Invalid API key option" alone does not retry. The Gemini CLI's
/// own wording is unverified.
pub(crate) fn rejects_acp_flag(output: &str) -> bool {
    let t = output.to_ascii_lowercase();
    // The flag we passed is `--acp`; yargs prints it as the bare word `acp` ("Unknown argument: acp").
    // Split on whitespace and strip quotes and punctuation, so `--experimental-acp-foo` is not `acp`.
    let names_flag = t.contains("--acp")
        || t.split_whitespace().any(|w| w.trim_matches(|c: char| "'\"`,:;()[].".contains(c)) == "acp");
    names_flag
        && ["unknown", "unrecognized", "unrecognised", "invalid", "bad"].iter().any(|w| {
            ["option", "flag", "argument"].iter().any(|n| t.contains(&format!("{w} {n}")))
        })
}

/// `omp 18.5.0` / `18.5.0` → `18.5.0`: the last word of the first non-empty line.
fn parse_version(stdout: &str) -> Option<String> {
    stdout.lines().find(|l| !l.trim().is_empty())?.split_whitespace().last().and_then(super::version_token)
}

/// What [`connect`] needs besides the byte streams.
pub(crate) struct Setup {
    pub cwd: PathBuf,
    pub resume_native_id: Option<String>,
    /// Fork this native session with `session/fork` (FR-S5).
    pub fork_from: Option<ForkFrom>,
    pub mcp_servers: Vec<McpServer>,
    pub computer: Option<NodeClient>,
    /// Put in front of the first prompt (agents without an instructions argument).
    pub prepend_instructions: Option<String>,
    /// The agent's name (`AcpConfig::name`), for agent-specific error texts.
    pub agent: String,
}

/// Speak ACP over `reader`/`writer` (the agent's stdout/stdin): initialize, open the session,
/// and return the running agent. Separate from process spawning so tests can use an in-process
/// agent.
pub(crate) async fn connect<R, W>(
    reader: R,
    writer: W,
    child: Option<ProcHandle>,
    setup: Setup,
    events: mpsc::Sender<AgentEvent>,
) -> anyhow::Result<AcpRun>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let cwd = std::path::absolute(&setup.cwd).unwrap_or_else(|_| setup.cwd.clone());
    let writer: Box<dyn AsyncWrite + Send + Unpin> = Box::new(writer);
    let conn = Arc::new(Conn {
        writer: tokio::sync::Mutex::new(Some(writer)),
        pending: Mutex::new(HashMap::new()),
        next_id: AtomicI64::new(0),
        mapper: Mutex::new(Mapper::default()),
        approvals: Mutex::new(HashMap::new()),
        next_approval: AtomicU64::new(1),
        host: Arc::new(Host::new(cwd.clone(), setup.computer)),
        replaying: AtomicBool::new(false),
        closing: AtomicBool::new(false),
        turn_active: AtomicBool::new(false),
        cancel_requested: AtomicBool::new(false),
        events: events.clone(),
    });
    tokio::spawn(read_loop(conn.clone(), reader));

    let init = conn
        .request("initialize", initialize_params(), Some(REQUEST_TIMEOUT))
        .await
        .context("ACP initialize")?;
    let version = init.get("protocolVersion").and_then(Value::as_u64);
    if version != Some(PROTOCOL_VERSION) {
        bail!("the agent speaks ACP protocol version {version:?}; Ember supports {PROTOCOL_VERSION}");
    }
    let caps = init.get("agentCapabilities").cloned().unwrap_or(Value::Null);
    let can_resume = caps.pointer("/sessionCapabilities/resume").is_some_and(|v| !v.is_null());
    let can_load = caps.get("loadSession").and_then(Value::as_bool).unwrap_or(false);
    let mcp = mcp_servers_param(&setup.mcp_servers);
    let cwd_str = cwd.to_string_lossy().into_owned();

    let auth_method = auth_method_for(&setup.agent, &init);
    let can_fork = caps.pointer("/sessionCapabilities/fork").is_some_and(|v| !v.is_null());
    let session_id = match setup.resume_native_id {
        None if setup.fork_from.is_some() => {
            // ACP's `session/fork` is unstable and takes no turn: the whole session is forked.
            let from = setup.fork_from.as_ref().expect("checked");
            if !can_fork {
                bail!("the agent does not advertise session/fork");
            }
            let resp = conn
                .request(
                    "session/fork",
                    json!({ "sessionId": from.native_id, "cwd": cwd_str, "mcpServers": mcp }),
                    Some(REQUEST_TIMEOUT),
                )
                .await
                .context("ACP session/fork")?;
            Some(
                resp.get("sessionId")
                    .and_then(Value::as_str)
                    .context("session/fork response without sessionId")?
                    .to_string(),
            )
        }
        Some(id) if can_resume => {
            conn.request(
                "session/resume",
                json!({ "sessionId": id, "cwd": cwd_str, "mcpServers": mcp }),
                Some(REQUEST_TIMEOUT),
            )
            .await
            .context("ACP session/resume")?;
            Some(id)
        }
        Some(id) if can_load => {
            // The agent replays the conversation before answering; Ember has it already.
            conn.replaying.store(true, Ordering::SeqCst);
            let res = conn
                .request(
                    "session/load",
                    json!({ "sessionId": id, "cwd": cwd_str, "mcpServers": mcp }),
                    Some(LOAD_TIMEOUT),
                )
                .await;
            conn.replaying.store(false, Ordering::SeqCst);
            *conn.mapper.lock().unwrap() = Mapper::default();
            res.context("ACP session/load")?;
            Some(id)
        }
        resume => {
            if let Some(id) = resume {
                events
                    .send(AgentEvent::Notice {
                        message: format!(
                            "This agent cannot resume ACP session {id}; a new session was started \
                             without the earlier conversation."
                        ),
                    })
                    .await?;
            }
            match open_new_session(&conn, &cwd_str, &mcp, auth_method.as_deref()).await {
                Ok(id) => Some(id),
                // Authentication is needed: the session opens at the first prompt that finds it
                // done, and until then each turn ends with `auth_required` (FR-A10b).
                Err(e) if classify_rpc_error(&e) == Some(ErrorClass::AuthRequired) => {
                    tracing::warn!(target: "acp", agent = %setup.agent, "session/new needs authentication: {}", redact_upstream(&e.message));
                    tracing::debug!(target: "acp", agent = %setup.agent, code = e.code, data = ?e.data, "raw session/new error (not for sharing): {}", e.message);
                    None
                }
                Err(rpc) => {
                    return Err(anyhow::Error::new(AcpRequestError { method: "session/new".into(), rpc })
                        .context("ACP session/new"))
                }
            }
        }
    };
    if let Some(id) = &session_id {
        events.send(AgentEvent::NativeSession { native_id: id.clone() }).await?;
    }
    let session_id = Arc::new(Mutex::new(session_id));

    let (prompts, prompt_rx) = mpsc::unbounded_channel();
    tokio::spawn(prompt_loop(
        conn.clone(),
        setup.agent.clone(),
        session_id.clone(),
        Reopen { cwd: cwd_str.clone(), mcp: mcp.clone(), auth_method },
        prompt_rx,
    ));
    Ok(AcpRun {
        conn,
        child,
        session_id,
        prompts,
        prepend_instructions: setup.prepend_instructions,
    })
}

/// The `authenticate` method to offer when `session/new` asks for authentication: for Gemini, the
/// API key method if its `initialize` answer lists it. The id `gemini-api-key` is UNVERIFIED
/// (FR-A10b); with another or no listed id Ember sends no `authenticate` and the turn ends with
/// `auth_required`.
fn auth_method_for(agent: &str, init: &Value) -> Option<String> {
    if agent != "gemini" {
        return None;
    }
    init.get("authMethods")?
        .as_array()?
        .iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .find(|id| *id == "gemini-api-key")
        .map(str::to_string)
}

fn initialize_params() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "clientCapabilities": {
            "fs": { "readTextFile": true, "writeTextFile": true },
            "terminal": true,
        },
        "clientInfo": { "name": "ember", "title": "Ember", "version": env!("CARGO_PKG_VERSION") },
    })
}

/// ACP `McpServerStdio` entries (`{name, command, args, env: [{name, value}]}`). The environment
/// (registry secrets, FR-A7) travels over the agent's stdin, never on a command line.
fn mcp_servers_param(servers: &[McpServer]) -> Value {
    Value::Array(
        servers
            .iter()
            .map(|m| {
                let env: Vec<Value> = m.env.iter().map(|(k, v)| json!({ "name": k, "value": v })).collect();
                json!({ "name": m.name, "command": m.command, "args": m.args, "env": env })
            })
            .collect(),
    )
}

// ---------------------------------------------------------------------------------------------
// Connection

type Reply = Result<Value, RpcError>;

#[derive(Clone, PartialEq)]
pub(crate) struct RpcError {
    code: i64,
    message: String,
    /// The error's `data`, as text, for [`classify_rpc_error`].
    data: Option<String>,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> RpcError {
        RpcError { code, message: message.into(), data: None }
    }

    fn invalid(message: impl Into<String>) -> RpcError {
        RpcError::new(INVALID_PARAMS, message)
    }

    fn internal(message: impl Into<String>) -> RpcError {
        RpcError::new(INTERNAL_ERROR, message)
    }

    fn io(e: &std::io::Error, what: &str) -> RpcError {
        let code = if e.kind() == std::io::ErrorKind::NotFound { RESOURCE_NOT_FOUND } else { INTERNAL_ERROR };
        RpcError::new(code, format!("{what}: {e}"))
    }

    fn node(e: &ember_node::client::ClientError, what: &str) -> RpcError {
        let code = if e.errno() == Some("ENOENT") { RESOURCE_NOT_FOUND } else { INTERNAL_ERROR };
        RpcError::new(code, format!("{what}: {e}"))
    }
}

/// `Debug` shows the redacted message and no `data`, so no `{:?}` of an error leaks upstream text.
impl std::fmt::Debug for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RpcError").field("code", &self.code).field("message", &redact_upstream(&self.message)).finish()
    }
}

impl std::error::Error for RpcError {}

/// What a failed request returns in an `anyhow::Error`, so callers can look at the JSON-RPC error.
#[derive(Debug)]
struct AcpRequestError {
    method: String,
    rpc: RpcError,
}

impl std::fmt::Display for AcpRequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ACP {} failed: {}", self.method, self.rpc)
    }
}

impl std::error::Error for AcpRequestError {}

/// The JSON-RPC error code ACP uses for "authentication required".
const AUTH_REQUIRED: i64 = -32000;

/// Typed failures of a turn (FR-A10b, FR-A10c).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ErrorClass {
    AuthRequired,
    QuotaExceeded,
}

/// Classify a JSON-RPC error from an agent. A **heuristic**: the code `-32000` (ACP's
/// authentication required) or substrings of the message and data. Quota wins over auth ("API key
/// quota exceeded" is a quota problem). The real wording of the Gemini CLI is unverified.
pub(crate) fn classify_rpc_error(e: &RpcError) -> Option<ErrorClass> {
    let mut text = e.message.to_ascii_lowercase();
    if let Some(d) = &e.data {
        text.push(' ');
        text.push_str(&d.to_ascii_lowercase());
    }
    let word_429 = text.split(|c: char| !c.is_ascii_alphanumeric()).any(|w| w == "429");
    let quota = ["resource_exhausted", "resource exhausted", "quota", "rate limit", "rate-limit", "ratelimit", "too many requests"];
    if word_429 || quota.iter().any(|q| text.contains(q)) {
        return Some(ErrorClass::QuotaExceeded);
    }
    let auth = [
        "authenticat", "unauthorized", "unauthorised", "auth_required", "auth required", "api key", "api_key",
        "sign in", "sign-in", "log in", "login", "credentials",
    ];
    let word_401 = text.split(|c: char| !c.is_ascii_alphanumeric()).any(|w| w == "401");
    if e.code == AUTH_REQUIRED || word_401 || auth.iter().any(|a| text.contains(a)) {
        return Some(ErrorClass::AuthRequired);
    }
    None
}

/// The text of an `auth_required` error for `agent` (FR-A10b).
fn auth_message(agent: &str) -> String {
    if agent == "gemini" {
        "sign in needed: set GEMINI_API_KEY (individual Google accounts are no longer served by the Gemini CLI, use Antigravity)"
            .into()
    } else {
        format!("sign in needed: the {agent} agent reported that authentication is missing or failed")
    }
}

const REDACTED: &str = "[redacted]";
/// The longest upstream text shown or stored, in characters, ellipsis included.
const UPSTREAM_CAP: usize = 300;

fn is_secret_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Text from an agent or its provider (an error message) made safe to show, store or log
/// (FR-A10): whitespace and control characters collapse to single spaces; Google API keys
/// (`AIza` + 35 characters), `Bearer` and `Authorization` values, `sk-` / `ghp_` / `ya29.` style
/// tokens, e-mail addresses and any run of 32 or more `[A-Za-z0-9_-]` characters holding both a
/// letter and a digit become `[redacted]`; the result is capped at 300 characters (the last three
/// are `...` when cut). A heuristic: a secret that looks like an ordinary word, or a long run of
/// letters only, passes. Raw text may be logged only at `debug`, which is not for sharing.
pub(crate) fn redact_upstream(text: &str) -> String {
    let out = redact_upstream_uncapped(text);
    if out.chars().count() > UPSTREAM_CAP {
        let mut cut: String = out.chars().take(UPSTREAM_CAP - 3).collect();
        cut.push_str("...");
        cut
    } else {
        out
    }
}

/// [`redact_upstream`] without the 300-character cap, for one line of the shim's agent stderr log
/// (#544): a debugging log keeps the whole line, only the secrets are cut out.
pub(crate) fn redact_upstream_uncapped(text: &str) -> String {
    let flat = text
        .split(|c: char| c.is_whitespace() || c.is_control() || c == '\u{2028}' || c == '\u{2029}')
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let out = redact_auth_values(&flat);
    let out = redact_prefixed_tokens(&out);
    let out = redact_google_keys(&out);
    let out = redact_emails(&out);
    redact_long_secrets(&out)
}

/// The value after `Authorization:` (and a scheme word such as `Bearer`), and the token after a
/// bare `Bearer`.
fn redact_auth_values(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let lower: Vec<char> = c.iter().map(|x| x.to_ascii_lowercase()).collect();
    let starts = |i: usize, w: &str| lower.get(i..i + w.len()).is_some_and(|x| x.iter().copied().eq(w.chars()));
    let value_char = |x: char| !x.is_whitespace() && !matches!(x, '"' | '\'' | ',' | ';');
    let token_end = |mut i: usize| {
        while i < c.len() && value_char(c[i]) {
            i += 1;
        }
        i
    };
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        let boundary = i == 0 || !c[i - 1].is_ascii_alphanumeric();
        if boundary && starts(i, "authorization") {
            let mut j = i + "authorization".len();
            while j < c.len() && (c[j].is_whitespace() || matches!(c[j], '"' | '\'')) {
                j += 1;
            }
            if j < c.len() && matches!(c[j], ':' | '=') {
                j += 1;
                while j < c.len() && (c[j].is_whitespace() || matches!(c[j], '"' | '\'')) {
                    j += 1;
                }
                let mut end = token_end(j);
                let scheme: String = lower[j..end].iter().collect();
                if ["bearer", "basic", "token", "digest", "negotiate"].contains(&scheme.as_str()) {
                    let mut k = end;
                    while k < c.len() && c[k] == ' ' {
                        k += 1;
                    }
                    end = token_end(k).max(end);
                }
                if end > j {
                    out.extend(&c[i..j]);
                    out.push_str(REDACTED);
                    i = end;
                    continue;
                }
            }
        }
        if boundary && starts(i, "bearer") && c.get(i + 6) == Some(&' ') {
            let mut k = i + 6;
            while k < c.len() && c[k] == ' ' {
                k += 1;
            }
            let end = token_end(k);
            if end > k {
                out.push_str(REDACTED);
                i = end;
                continue;
            }
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

/// `sk-`, `ghp_` (and the other GitHub prefixes), `github_pat_` and `ya29.` tokens at a word
/// start, with at least 8 more token characters.
fn redact_prefixed_tokens(s: &str) -> String {
    const PREFIXES: &[&str] = &["sk-", "ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_", "ya29."];
    let c: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        if i == 0 || !c[i - 1].is_ascii_alphanumeric() {
            let hit = PREFIXES.iter().find(|p| c.get(i..i + p.len()).is_some_and(|x| x.iter().copied().eq(p.chars())));
            if let Some(p) = hit {
                let mut end = i + p.len();
                while end < c.len() && (is_secret_char(c[end]) || c[end] == '.') {
                    end += 1;
                }
                if end - i - p.len() >= 8 {
                    out.push_str(REDACTED);
                    i = end;
                    continue;
                }
            }
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

/// `AIza` followed by 35 key characters (and any further key characters glued to it).
fn redact_google_keys(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        if c.get(i..i + 4).is_some_and(|x| x == ['A', 'I', 'z', 'a'])
            && c.get(i + 4..i + 39).is_some_and(|x| x.iter().all(|&k| is_secret_char(k)))
        {
            let mut end = i + 39;
            while end < c.len() && is_secret_char(c[end]) {
                end += 1;
            }
            out.push_str(REDACTED);
            i = end;
            continue;
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

/// `local@domain.tld`.
fn redact_emails(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let local = |x: char| x.is_ascii_alphanumeric() || matches!(x, '.' | '_' | '%' | '+' | '-');
    let domain = |x: char| x.is_ascii_alphanumeric() || matches!(x, '.' | '-');
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let floor_of = |ranges: &Vec<(usize, usize)>| ranges.last().map_or(0, |r| r.1);
    for (k, &ch) in c.iter().enumerate() {
        if ch != '@' || k < floor_of(&ranges) {
            continue;
        }
        let mut l = k;
        while l > floor_of(&ranges) && local(c[l - 1]) {
            l -= 1;
        }
        let mut r = k + 1;
        while r < c.len() && domain(c[r]) {
            r += 1;
        }
        while r > k + 1 && matches!(c[r - 1], '.' | '-') {
            r -= 1;
        }
        let dom = &c[k + 1..r];
        if l < k && dom.first().is_some_and(|x| x.is_ascii_alphanumeric()) && dom.contains(&'.') {
            ranges.push((l, r));
        }
    }
    let mut out = String::new();
    let mut i = 0;
    for (l, r) in ranges {
        out.extend(&c[i..l]);
        out.push_str(REDACTED);
        i = r;
    }
    out.extend(&c[i..]);
    out
}

/// A run of 32 or more key characters with a letter and a digit in it.
fn redact_long_secrets(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        if !is_secret_char(c[i]) {
            out.push(c[i]);
            i += 1;
            continue;
        }
        let mut end = i;
        while end < c.len() && is_secret_char(c[end]) {
            end += 1;
        }
        let run = &c[i..end];
        if run.len() >= 32 && run.iter().any(|x| x.is_ascii_alphabetic()) && run.iter().any(|x| x.is_ascii_digit()) {
            out.push_str(REDACTED);
        } else {
            out.extend(run);
        }
        i = end;
    }
    out
}

/// The events of a turn that failed with `e`: a typed error for the classes above, else the plain
/// error; always a failed turn.
fn turn_failure_events(agent: &str, e: &RpcError) -> Vec<AgentEvent> {
    let event = match classify_rpc_error(e) {
        Some(ErrorClass::AuthRequired) => {
            AgentEvent::TurnError { code: "auth_required".into(), message: auth_message(agent) }
        }
        Some(ErrorClass::QuotaExceeded) => {
            tracing::debug!(target: "acp", code = e.code, data = ?e.data, "raw quota error (not for sharing): {}", e.message);
            let message = redact_upstream(&e.message);
            let message = if message.is_empty() { "quota or rate limit reached".to_string() } else { message };
            AgentEvent::TurnError { code: "quota_exceeded".into(), message }
        }
        None => {
            tracing::debug!(target: "acp", code = e.code, data = ?e.data, "raw prompt error (not for sharing): {}", e.message);
            // `Display` of the error redacts its message.
            AgentEvent::Error { message: format!("ACP session/prompt failed: {e}") }
        }
    };
    vec![event, AgentEvent::TurnEnded { outcome: TurnOutcome::Failed }]
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", redact_upstream(&self.message), self.code)
    }
}

/// A permission request waiting for the user.
struct PendingPermission {
    rpc_id: Value,
    options: Vec<Value>,
}

/// One agent connection.
struct Conn {
    writer: tokio::sync::Mutex<Option<Box<dyn AsyncWrite + Send + Unpin>>>,
    pending: Mutex<HashMap<i64, oneshot::Sender<Reply>>>,
    next_id: AtomicI64,
    mapper: Mutex<Mapper>,
    /// Ember approval id → the agent's request.
    approvals: Mutex<HashMap<String, PendingPermission>>,
    next_approval: AtomicU64,
    host: Arc<Host>,
    /// Set during `session/load`: replayed history is dropped.
    replaying: AtomicBool,
    /// Set by `shutdown`, so the reader does not report the exit as an error.
    closing: AtomicBool,
    /// A `session/prompt` is in flight.
    turn_active: AtomicBool,
    /// `session/cancel` was sent for the current turn.
    cancel_requested: AtomicBool,
    events: mpsc::Sender<AgentEvent>,
}

impl Conn {
    async fn write(&self, msg: &Value) -> anyhow::Result<()> {
        let mut line = serde_json::to_vec(msg)?;
        line.push(b'\n');
        let mut writer = self.writer.lock().await;
        let writer = writer.as_mut().ok_or_else(|| anyhow!("the ACP agent is shut down"))?;
        writer.write_all(&line).await?;
        writer.flush().await?;
        Ok(())
    }

    /// Call `method`; `timeout: None` waits as long as the agent lives (prompt turns).
    async fn request(&self, method: &str, params: Value, timeout: Option<Duration>) -> anyhow::Result<Value> {
        self.request_raw(method, params, timeout)
            .await
            .map_err(|rpc| anyhow::Error::new(AcpRequestError { method: method.to_string(), rpc }))
    }

    async fn request_raw(&self, method: &str, params: Value, timeout: Option<Duration>) -> Reply {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if let Err(e) = self.write(&msg).await {
            self.pending.lock().unwrap().remove(&id);
            return Err(RpcError::internal(format!("{e:#}")));
        }
        let reply = match timeout {
            Some(t) => match tokio::time::timeout(t, rx).await {
                Ok(r) => r,
                Err(_) => {
                    self.pending.lock().unwrap().remove(&id);
                    return Err(RpcError::internal("timed out"));
                }
            },
            None => rx.await,
        };
        reply.unwrap_or_else(|_| Err(RpcError::internal("the agent exited")))
    }

    async fn notify(&self, method: &str, params: Value) -> anyhow::Result<()> {
        self.write(&json!({ "jsonrpc": "2.0", "method": method, "params": params })).await
    }

    async fn respond(&self, id: Value, reply: Reply) {
        let msg = match reply {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": e.code, "message": e.message } }),
        };
        if let Err(e) = self.write(&msg).await {
            tracing::warn!(target: "acp", "failed to answer the agent: {e:#}");
        }
    }

    async fn emit(&self, events: Vec<AgentEvent>) {
        for ev in events {
            if self.events.send(ev).await.is_err() {
                return;
            }
        }
    }

    /// Answer every pending permission request with the `cancelled` outcome.
    async fn cancel_permissions(&self) {
        let pending: Vec<PendingPermission> =
            self.approvals.lock().unwrap().drain().map(|(_, p)| p).collect();
        for p in pending {
            self.respond(p.rpc_id, Ok(json!({ "outcome": { "outcome": "cancelled" } }))).await;
        }
    }
}

async fn read_loop<R: AsyncRead + Unpin + Send + 'static>(conn: Arc<Conn>, reader: R) {
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            tracing::warn!(target: "acp", "ignoring a non-JSON line from the agent ({} bytes)", line.len());
            tracing::debug!(target: "acp", "raw non-JSON line (debug logs are not for sharing): {line}");
            continue;
        };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match (msg.get("method").and_then(Value::as_str), msg.get("id")) {
            (Some(method), Some(id)) => on_agent_request(&conn, id.clone(), method, params).await,
            (Some("session/update"), None) => {
                if conn.replaying.load(Ordering::SeqCst) {
                    continue;
                }
                let update = params.get("update").cloned().unwrap_or(Value::Null);
                let out = conn.mapper.lock().unwrap().on_update(&update, &conn.host);
                conn.emit(out).await;
            }
            (Some(method), None) => {
                tracing::debug!(target: "acp", "ignoring notification {method}");
            }
            (None, Some(id)) => {
                let waiter = id.as_i64().and_then(|id| conn.pending.lock().unwrap().remove(&id));
                if let Some(waiter) = waiter {
                    if let Some(err) = msg.get("error") {
                        // Raw upstream text: debug only, debug logs are not for sharing.
                        tracing::debug!(target: "acp", "raw error reply (not for sharing): {err}");
                    }
                    let reply = match msg.get("error") {
                        Some(err) => Err(RpcError {
                            data: err.get("data").map(|d| d.as_str().map(str::to_string).unwrap_or_else(|| d.to_string())),
                            ..RpcError::new(
                                err.get("code").and_then(Value::as_i64).unwrap_or(INTERNAL_ERROR),
                                err.get("message").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| err.to_string()),
                            )
                        }),
                        None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = waiter.send(reply);
                }
            }
            (None, None) => {}
        }
    }
    // EOF: fail outstanding requests (a running turn then fails in the prompt loop).
    conn.pending.lock().unwrap().clear();
    conn.approvals.lock().unwrap().clear();
    conn.host.kill_all();
    if !conn.closing.load(Ordering::SeqCst) && !conn.turn_active.load(Ordering::SeqCst) {
        let _ = conn.events.send(AgentEvent::Error { message: "the ACP agent exited".into() }).await;
    }
}

/// A request from the agent. Permission requests become approvals; fs and terminal requests are
/// served in their own task so a long `terminal/wait_for_exit` never blocks the connection.
async fn on_agent_request(conn: &Arc<Conn>, id: Value, method: &str, params: Value) {
    match method {
        "session/request_permission" => {
            let approval_id = format!("acp-{}", conn.next_approval.fetch_add(1, Ordering::Relaxed));
            let tool_call = params.get("toolCall").cloned().unwrap_or(Value::Null);
            let options = params.get("options").and_then(Value::as_array).cloned().unwrap_or_default();
            let out = conn.mapper.lock().unwrap().on_permission(&approval_id, &tool_call);
            conn.approvals.lock().unwrap().insert(approval_id, PendingPermission { rpc_id: id, options });
            conn.emit(out).await;
        }
        "fs/read_text_file" | "fs/write_text_file" | "terminal/create" | "terminal/output"
        | "terminal/wait_for_exit" | "terminal/kill" | "terminal/release" => {
            let conn = conn.clone();
            let method = method.to_string();
            tokio::spawn(async move {
                let reply = conn.host.handle(&method, &params).await;
                conn.respond(id, reply).await;
            });
        }
        other => {
            tracing::warn!(target: "acp", "unsupported agent request {other}");
            conn.respond(id, Err(RpcError::new(METHOD_NOT_FOUND, format!("Ember does not support {other}")))).await;
        }
    }
}

/// Where a deferred `session/new` happens (see [`prompt_loop`]).
struct Reopen {
    cwd: String,
    mcp: Value,
    /// An `authenticate` method to try when `session/new` says authentication is required.
    auth_method: Option<String>,
}

type SharedSession = Arc<Mutex<Option<String>>>;

/// `session/new`, with one `authenticate` round when the agent asks for it and offered `method`
/// in its `initialize` answer. Only the method id is sent: Ember never reads credentials.
async fn open_new_session(conn: &Conn, cwd: &str, mcp: &Value, method: Option<&str>) -> Result<String, RpcError> {
    let params = || json!({ "cwd": cwd, "mcpServers": mcp });
    let mut resp = conn.request_raw("session/new", params(), Some(REQUEST_TIMEOUT)).await;
    if let (Err(e), Some(method)) = (&resp, method) {
        if classify_rpc_error(e) == Some(ErrorClass::AuthRequired) {
            conn.request_raw("authenticate", json!({ "methodId": method }), Some(REQUEST_TIMEOUT)).await?;
            resp = conn.request_raw("session/new", params(), Some(REQUEST_TIMEOUT)).await;
        }
    }
    resp?
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| RpcError::internal("session/new response without sessionId"))
}

/// Prompts one queued message at a time; each `session/prompt` response ends a turn. When the
/// session could not be opened at start (authentication needed), each prompt tries `session/new`
/// again first, and a failure ends that turn with a typed error.
async fn prompt_loop(
    conn: Arc<Conn>,
    agent: String,
    session: SharedSession,
    reopen: Reopen,
    mut prompts: mpsc::UnboundedReceiver<String>,
) {
    while let Some(text) = prompts.recv().await {
        conn.cancel_requested.store(false, Ordering::SeqCst);
        conn.turn_active.store(true, Ordering::SeqCst);
        let current = session.lock().unwrap().clone();
        let opened = match current {
            Some(id) => Ok(id),
            None => match open_new_session(&conn, &reopen.cwd, &reopen.mcp, reopen.auth_method.as_deref()).await {
                Ok(id) => {
                    *session.lock().unwrap() = Some(id.clone());
                    conn.emit(vec![AgentEvent::NativeSession { native_id: id.clone() }]).await;
                    Ok(id)
                }
                Err(e) => Err(e),
            },
        };
        let reply = match opened {
            Ok(session_id) => {
                let params = json!({ "sessionId": session_id, "prompt": [{ "type": "text", "text": text }] });
                conn.request_raw("session/prompt", params, None).await
            }
            Err(e) => Err(e),
        };
        // Permission requests the agent left open belong to the finished turn.
        conn.cancel_permissions().await;
        let cancelled = conn.cancel_requested.load(Ordering::SeqCst);
        let closing = conn.closing.load(Ordering::SeqCst);
        let mut out = conn.mapper.lock().unwrap().end_turn();
        match reply {
            Ok(result) => out.extend(turn_end_events(&result, cancelled)),
            Err(_) if closing => {}
            Err(_) if cancelled => out.push(AgentEvent::TurnEnded { outcome: TurnOutcome::Interrupted }),
            Err(e) => out.extend(turn_failure_events(&agent, &e)),
        }
        conn.emit(out).await;
        conn.turn_active.store(false, Ordering::SeqCst);
        if closing {
            return;
        }
    }
}

/// Usage and `TurnEnded` from a `PromptResponse`.
fn turn_end_events(result: &Value, cancelled: bool) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    if let Some(usage) = result.get("usage").filter(|u| u.is_object()) {
        let input = usage.get("inputTokens").and_then(Value::as_u64).unwrap_or(0);
        let output = usage.get("outputTokens").and_then(Value::as_u64).unwrap_or(0);
        if input > 0 || output > 0 {
            out.push(AgentEvent::Usage { input_tokens: input, output_tokens: output });
        }
    }
    let outcome = match result.get("stopReason").and_then(Value::as_str) {
        Some("cancelled") => TurnOutcome::Interrupted,
        _ if cancelled => TurnOutcome::Interrupted,
        Some("end_turn") | None => TurnOutcome::Completed,
        Some(reason) => {
            let why = match reason {
                "max_tokens" => "the agent hit its token limit",
                "max_turn_requests" => "the agent hit its limit of model requests in one turn",
                "refusal" => "the agent refused to continue",
                _ => "the agent stopped",
            };
            out.push(AgentEvent::Notice { message: format!("{why} (ACP stop reason {reason})") });
            TurnOutcome::Completed
        }
    };
    out.push(AgentEvent::TurnEnded { outcome });
    out
}

// ---------------------------------------------------------------------------------------------
// Event mapping

#[derive(Default)]
struct ToolState {
    title: Option<String>,
    kind: Option<String>,
    name: Option<String>,
    raw_input: Option<Value>,
    locations: Option<Value>,
    content: Option<Value>,
    raw_output: Option<Value>,
    done: bool,
}

impl ToolState {
    /// Apply the fields present in a `ToolCall` / `ToolCallUpdate` (absent and null keep).
    fn apply(&mut self, u: &Value) {
        let s = |k: &str| u.get(k).and_then(Value::as_str).map(str::to_string);
        let v = |k: &str| u.get(k).filter(|v| !v.is_null()).cloned();
        if let Some(t) = s("title") {
            self.title = Some(t);
        }
        if let Some(k) = s("kind") {
            self.kind = Some(k);
        }
        if let Some(n) = s("name") {
            self.name = Some(n);
        }
        if let Some(i) = v("rawInput") {
            self.raw_input = Some(i);
        }
        if let Some(l) = v("locations") {
            self.locations = Some(l);
        }
        if let Some(c) = v("content") {
            self.content = Some(c);
        }
        if let Some(o) = v("rawOutput") {
            self.raw_output = Some(o);
        }
    }

    /// The tool name shown and used for "always allow": the agent's `name`, else its `kind`.
    fn tool_name(&self) -> String {
        self.name.clone().or_else(|| self.kind.clone()).unwrap_or_else(|| "other".into())
    }

    /// `{title, kind, input, locations}`, nulls left out.
    fn input(&self) -> Value {
        let mut m = Map::new();
        if let Some(t) = &self.title {
            m.insert("title".into(), json!(t));
        }
        if let Some(k) = &self.kind {
            m.insert("kind".into(), json!(k));
        }
        if let Some(i) = &self.raw_input {
            m.insert("input".into(), i.clone());
        }
        if let Some(l) = &self.locations {
            m.insert("locations".into(), l.clone());
        }
        Value::Object(m)
    }

    fn output(&self, host: &Host) -> String {
        let mut parts = Vec::new();
        for c in self.content.as_ref().and_then(Value::as_array).into_iter().flatten() {
            match c.get("type").and_then(Value::as_str) {
                Some("content") => {
                    if let Some(t) = c.pointer("/content/text").and_then(Value::as_str) {
                        parts.push(t.to_string());
                    }
                }
                Some("diff") => parts.push(render_diff(c)),
                Some("terminal") => {
                    let id = c.get("terminalId").and_then(Value::as_str).unwrap_or_default();
                    parts.push(host.output_snapshot(id).unwrap_or_else(|| format!("[terminal {id}]")));
                }
                _ => {}
            }
        }
        if parts.is_empty() {
            match &self.raw_output {
                Some(Value::String(s)) => parts.push(s.clone()),
                Some(v) => parts.push(v.to_string()),
                None => {}
            }
        }
        parts.join("\n")
    }
}

/// A `{type: "diff", path, oldText, newText}` as removed and added lines (not a minimal diff).
fn render_diff(c: &Value) -> String {
    let path = c.get("path").and_then(Value::as_str).unwrap_or_default();
    let old = c.get("oldText").and_then(Value::as_str);
    let new = c.get("newText").and_then(Value::as_str).unwrap_or_default();
    let mut out = format!("--- {}\n+++ {path}\n", if old.is_some() { path } else { "/dev/null" });
    for l in old.unwrap_or_default().lines() {
        out.push_str(&format!("-{l}\n"));
    }
    for l in new.lines() {
        out.push_str(&format!("+{l}\n"));
    }
    out
}

/// Translates `session/update` notifications into [`AgentEvent`]s.
#[derive(Default)]
struct Mapper {
    /// The assistant message being streamed.
    message: String,
    message_id: Option<String>,
    tools: HashMap<String, ToolState>,
    plans: u64,
}

impl Mapper {
    fn flush_message(&mut self, out: &mut Vec<AgentEvent>) {
        if !self.message.is_empty() {
            out.push(AgentEvent::AssistantMessage { text: std::mem::take(&mut self.message) });
        }
        self.message_id = None;
    }

    fn on_update(&mut self, u: &Value, host: &Host) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        match u.get("sessionUpdate").and_then(Value::as_str) {
            Some("agent_message_chunk") => {
                let id = u.get("messageId").and_then(Value::as_str).map(str::to_string);
                if id.is_some() && self.message_id.is_some() && id != self.message_id {
                    self.flush_message(&mut out);
                }
                if id.is_some() {
                    self.message_id = id;
                }
                if let Some(text) = u.pointer("/content/text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        self.message.push_str(text);
                        out.push(AgentEvent::AssistantDelta { text: text.to_string() });
                    }
                }
            }
            Some("tool_call") | Some("tool_call_update") => {
                let Some(id) = u.get("toolCallId").and_then(Value::as_str) else { return out };
                self.announce(id, u, &mut out);
                let state = self.tools.get_mut(id).expect("announced");
                let status = u.get("status").and_then(Value::as_str);
                if matches!(status, Some("completed" | "failed")) && !state.done {
                    state.done = true;
                    out.push(AgentEvent::ToolResult {
                        call_id: id.to_string(),
                        output: state.output(host),
                        is_error: status == Some("failed"),
                    });
                }
            }
            Some("plan") => {
                self.flush_message(&mut out);
                self.plans += 1;
                let entries = u.get("entries").cloned().unwrap_or_else(|| json!([]));
                let text = entries
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|e| {
                        format!(
                            "[{}] {}",
                            e.get("status").and_then(Value::as_str).unwrap_or("pending"),
                            e.get("content").and_then(Value::as_str).unwrap_or_default()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let call_id = format!("acp-plan-{}", self.plans);
                out.push(AgentEvent::ToolCall {
                    call_id: call_id.clone(),
                    name: "Plan".into(),
                    input: json!({ "entries": entries }),
                });
                out.push(AgentEvent::ToolResult { call_id, output: text, is_error: false });
            }
            _ => {}
        }
        out
    }

    /// Record a tool call's fields, emitting `ToolCall` the first time its id is seen (which
    /// also ends the assistant message streamed before it).
    fn announce(&mut self, id: &str, fields: &Value, out: &mut Vec<AgentEvent>) {
        let known = self.tools.contains_key(id);
        if !known {
            self.flush_message(out);
        }
        let state = self.tools.entry(id.to_string()).or_default();
        state.apply(fields);
        if !known {
            out.push(AgentEvent::ToolCall { call_id: id.to_string(), name: state.tool_name(), input: state.input() });
        }
    }

    fn on_permission(&mut self, approval_id: &str, tool_call: &Value) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        let (tool, input) = match tool_call.get("toolCallId").and_then(Value::as_str) {
            Some(id) => {
                self.announce(id, tool_call, &mut out);
                let state = &self.tools[id];
                (state.tool_name(), state.input())
            }
            None => {
                let mut state = ToolState::default();
                state.apply(tool_call);
                (state.tool_name(), state.input())
            }
        };
        out.push(AgentEvent::ApprovalRequested { approval_id: approval_id.to_string(), tool, input, needs_person: false });
        out
    }

    /// The turn is over: finish the streamed message and forget the turn's tool calls.
    fn end_turn(&mut self) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        self.flush_message(&mut out);
        self.tools.clear();
        out
    }
}

/// The `optionId` for `decision` among ACP `PermissionOption`s, by option kind.
fn choose_option(options: &[Value], decision: ApprovalDecision) -> Option<String> {
    let kinds: &[&str] = match decision {
        ApprovalDecision::AllowOnce => &["allow_once", "allow_always"],
        ApprovalDecision::AllowAlways => &["allow_always", "allow_once"],
        ApprovalDecision::Deny => &["reject_once", "reject_always"],
    };
    kinds.iter().find_map(|kind| {
        options
            .iter()
            .find(|o| o.get("kind").and_then(Value::as_str) == Some(kind))
            .and_then(|o| o.get("optionId").and_then(Value::as_str))
            .map(str::to_string)
    })
}

// ---------------------------------------------------------------------------------------------
// Client methods: file system and terminals

/// Where the agent's `fs/*` and `terminal/*` requests run: this server, or a node.
struct Host {
    /// Default working directory for terminals.
    cwd: PathBuf,
    node: Option<NodeClient>,
    terminals: Mutex<HashMap<String, Arc<Term>>>,
    /// Final output of released terminals, newest last.
    released: Mutex<VecDeque<(String, String)>>,
    next_terminal: AtomicU64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExitStatus {
    code: Option<i32>,
    signal: Option<String>,
}

impl ExitStatus {
    fn to_json(&self) -> Value {
        json!({ "exitCode": self.code, "signal": self.signal })
    }
}

/// A command started by `terminal/create`.
struct Term {
    output: Mutex<OutputBuf>,
    exit: watch::Sender<Option<ExitStatus>>,
    kill: Notify,
}

/// Output kept within the byte limit, truncated from the front.
struct OutputBuf {
    bytes: Vec<u8>,
    limit: usize,
    truncated: bool,
}

impl OutputBuf {
    fn new(limit: usize) -> OutputBuf {
        OutputBuf { bytes: Vec::new(), limit, truncated: false }
    }

    fn push(&mut self, data: &[u8]) {
        self.bytes.extend_from_slice(data);
        if self.bytes.len() > self.limit {
            let cut = self.bytes.len() - self.limit;
            self.bytes.drain(..cut);
            self.truncated = true;
        }
    }

    /// The kept output as text, starting at a character boundary.
    fn text(&self) -> String {
        let mut start = 0;
        if self.truncated {
            // Skip UTF-8 continuation bytes left at the front by truncation.
            while start < self.bytes.len() && start < 4 && (self.bytes[start] & 0b1100_0000) == 0b1000_0000 {
                start += 1;
            }
        }
        String::from_utf8_lossy(&self.bytes[start..]).into_owned()
    }
}

fn str_param<'a>(params: &'a Value, key: &str) -> Result<&'a str, RpcError> {
    params.get(key).and_then(Value::as_str).ok_or_else(|| RpcError::invalid(format!("missing \"{key}\"")))
}

fn abs_path_param(params: &Value) -> Result<PathBuf, RpcError> {
    let path = PathBuf::from(str_param(params, "path")?);
    if !path.is_absolute() {
        return Err(RpcError::invalid(format!("path {} is not absolute", path.display())));
    }
    Ok(path)
}

/// Refuse a path inside the agent sandbox's directories (FR-A11): Ember serves Gemini's and omp's
/// file tools itself, so this is where their file access to the data dir is denied.
fn deny_data_dir(path: &Path) -> Result<(), RpcError> {
    if crate::sandbox::is_denied(path) {
        return Err(RpcError::invalid(crate::sandbox::denied_message(path)));
    }
    Ok(())
}

/// Lines `line` (1-based) .. `line + limit` of `text`, keeping line endings.
fn slice_lines(text: &str, line: Option<u64>, limit: Option<u64>) -> String {
    if line.is_none() && limit.is_none() {
        return text.to_string();
    }
    let skip = line.unwrap_or(1).saturating_sub(1) as usize;
    let take = limit.map(|l| l as usize).unwrap_or(usize::MAX);
    text.split_inclusive('\n').skip(skip).take(take).collect()
}

/// POSIX signal number → name (numbers common to Linux and macOS).
fn signal_name(sig: i32) -> String {
    match sig {
        1 => "SIGHUP".into(),
        2 => "SIGINT".into(),
        3 => "SIGQUIT".into(),
        6 => "SIGABRT".into(),
        9 => "SIGKILL".into(),
        11 => "SIGSEGV".into(),
        13 => "SIGPIPE".into(),
        15 => "SIGTERM".into(),
        n => format!("SIG{n}"),
    }
}

impl Host {
    fn new(cwd: PathBuf, node: Option<NodeClient>) -> Host {
        Host {
            cwd,
            node,
            terminals: Mutex::new(HashMap::new()),
            released: Mutex::new(VecDeque::new()),
            next_terminal: AtomicU64::new(1),
        }
    }

    async fn handle(&self, method: &str, params: &Value) -> Reply {
        match method {
            "fs/read_text_file" => self.read_text_file(params).await,
            "fs/write_text_file" => self.write_text_file(params).await,
            "terminal/create" => self.create_terminal(params).await,
            "terminal/output" => {
                let term = self.terminal(params)?;
                let buf = term.output.lock().unwrap();
                let mut result = json!({ "output": buf.text(), "truncated": buf.truncated });
                if let Some(status) = term.exit.borrow().as_ref() {
                    result["exitStatus"] = status.to_json();
                }
                Ok(result)
            }
            "terminal/wait_for_exit" => {
                let term = self.terminal(params)?;
                let mut rx = term.exit.subscribe();
                let waited = rx.wait_for(|s| s.is_some()).await;
                let status: Option<ExitStatus> = match waited {
                    Ok(r) => (*r).clone(),
                    Err(_) => None,
                };
                status.map(|s| s.to_json()).ok_or_else(|| RpcError::internal("terminal went away"))
            }
            "terminal/kill" => {
                self.terminal(params)?.kill.notify_one();
                Ok(json!({}))
            }
            "terminal/release" => {
                let id = str_param(params, "terminalId")?.to_string();
                let term = self
                    .terminals
                    .lock()
                    .unwrap()
                    .remove(&id)
                    .ok_or_else(|| RpcError::invalid(format!("unknown terminal {id}")))?;
                term.kill.notify_one();
                let text = term.output.lock().unwrap().text();
                let mut released = self.released.lock().unwrap();
                released.push_back((id, text));
                while released.len() > RELEASED_KEEP {
                    released.pop_front();
                }
                Ok(json!({}))
            }
            other => Err(RpcError::new(METHOD_NOT_FOUND, other.to_string())),
        }
    }

    fn terminal(&self, params: &Value) -> Result<Arc<Term>, RpcError> {
        let id = str_param(params, "terminalId")?;
        self.terminals.lock().unwrap().get(id).cloned().ok_or_else(|| RpcError::invalid(format!("unknown terminal {id}")))
    }

    /// Current output of a live or released terminal (tool results that embed it).
    fn output_snapshot(&self, id: &str) -> Option<String> {
        if let Some(t) = self.terminals.lock().unwrap().get(id) {
            return Some(t.output.lock().unwrap().text());
        }
        self.released.lock().unwrap().iter().rev().find(|(i, _)| i == id).map(|(_, t)| t.clone())
    }

    fn kill_all(&self) {
        for t in self.terminals.lock().unwrap().values() {
            t.kill.notify_one();
        }
    }

    async fn read_text_file(&self, params: &Value) -> Reply {
        let path = abs_path_param(params)?;
        deny_data_dir(&path)?;
        let text = match &self.node {
            Some(node) => {
                let r = node.read_file(&path).await.map_err(|e| RpcError::node(&e, &path.display().to_string()))?;
                String::from_utf8_lossy(&r.data).into_owned()
            }
            None => {
                let bytes = tokio::fs::read(&path).await.map_err(|e| RpcError::io(&e, &path.display().to_string()))?;
                String::from_utf8_lossy(&bytes).into_owned()
            }
        };
        let line = params.get("line").and_then(Value::as_u64);
        let limit = params.get("limit").and_then(Value::as_u64);
        Ok(json!({ "content": slice_lines(&text, line, limit) }))
    }

    async fn write_text_file(&self, params: &Value) -> Reply {
        let path = abs_path_param(params)?;
        deny_data_dir(&path)?;
        let content = str_param(params, "content")?;
        match &self.node {
            Some(node) => {
                let req = ember_node::proto::WriteRequest {
                    path: path.clone(),
                    data: content.as_bytes().to_vec(),
                    expect: None,
                    create_parents: true,
                };
                node.write(&req).await.map_err(|e| RpcError::node(&e, &path.display().to_string()))?;
            }
            None => {
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await.map_err(|e| RpcError::io(&e, &parent.display().to_string()))?;
                }
                tokio::fs::write(&path, content).await.map_err(|e| RpcError::io(&e, &path.display().to_string()))?;
            }
        }
        Ok(json!({}))
    }

    async fn create_terminal(&self, params: &Value) -> Reply {
        let command = str_param(params, "command")?.to_string();
        let args: Vec<String> = params
            .get("args")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        let env: Vec<(String, String)> = params
            .get("env")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|e| Some((e.get("name")?.as_str()?.to_string(), e.get("value")?.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let cwd = params.get("cwd").and_then(Value::as_str).map(PathBuf::from).unwrap_or_else(|| self.cwd.clone());
        let limit = params
            .get("outputByteLimit")
            .and_then(Value::as_u64)
            .map(|l| l as usize)
            .unwrap_or(DEFAULT_OUTPUT_LIMIT);
        let term = Arc::new(Term {
            output: Mutex::new(OutputBuf::new(limit)),
            exit: watch::Sender::new(None),
            kill: Notify::new(),
        });
        match &self.node {
            Some(node) => {
                let mut argv = vec![command];
                argv.extend(args);
                let spec = ember_node::proto::CommandSpec {
                    program: ember_node::proto::Program::Argv(argv),
                    cwd,
                    env: env.into_iter().collect(),
                    env_clear: false,
                };
                let session = node
                    .exec(&ember_node::proto::ExecRequest { command: spec, pty: None })
                    .await
                    .map_err(|e| RpcError::node(&e, "terminal/create"))?;
                tokio::spawn(drive_node_terminal(term.clone(), session));
            }
            None => {
                // The agent asked Ember to run this, so it gets the agent's limits (FR-A11): no
                // inherited Ember secret, and the OS sandbox where one is installed.
                let (program, args) = crate::sandbox::wrap(true, &env, Path::new(&command), &args);
                let mut cmd = Command::new(&program);
                crate::sandbox::scrub_env(&mut cmd);
                let mut child = cmd
                    .args(&args)
                    .envs(env)
                    .current_dir(&cwd)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|e| RpcError::io(&e, &format!("running {command}")))?;
                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                tokio::spawn(drive_local_terminal(term.clone(), child, stdout, stderr));
            }
        }
        let id = format!("term-{}", self.next_terminal.fetch_add(1, Ordering::Relaxed));
        self.terminals.lock().unwrap().insert(id.clone(), term);
        Ok(json!({ "terminalId": id }))
    }
}

async fn pump_output<R: AsyncRead + Unpin>(term: Arc<Term>, mut r: R) {
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => term.output.lock().unwrap().push(&buf[..n]),
        }
    }
}

async fn drive_local_terminal(
    term: Arc<Term>,
    mut child: Child,
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
) {
    let mut readers = Vec::new();
    if let Some(s) = stdout {
        readers.push(tokio::spawn(pump_output(term.clone(), s)));
    }
    if let Some(s) = stderr {
        readers.push(tokio::spawn(pump_output(term.clone(), s)));
    }
    let status = loop {
        tokio::select! {
            status = child.wait() => break status,
            _ = term.kill.notified() => {
                let _ = child.start_kill();
            }
        }
    };
    // Let the readers drain what the process wrote (a grandchild may hold the pipes open).
    for r in readers {
        let _ = tokio::time::timeout(Duration::from_secs(1), r).await;
    }
    let exit = match status {
        Ok(s) => {
            #[cfg(unix)]
            let signal = std::os::unix::process::ExitStatusExt::signal(&s).map(signal_name);
            #[cfg(not(unix))]
            let signal = None;
            ExitStatus { code: s.code(), signal }
        }
        Err(_) => ExitStatus { code: None, signal: None },
    };
    term.exit.send_replace(Some(exit));
}

async fn drive_node_terminal(term: Arc<Term>, session: ember_node::client::ExecSession) {
    use ember_node::proto::{ExecEvent, ExecInput};
    let (mut tx, mut rx) = session.into_split();
    let _ = tx.send(ExecInput::CloseStdin).await;
    let exit = loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(Some(ExecEvent::Stdout { data } | ExecEvent::Stderr { data })) => {
                    term.output.lock().unwrap().push(&data);
                }
                Ok(Some(ExecEvent::Exit { code, signal })) => {
                    break ExitStatus { code, signal: signal.map(signal_name) };
                }
                Ok(Some(ExecEvent::Error { message })) => {
                    term.output.lock().unwrap().push(format!("\n{message}\n").as_bytes());
                    break ExitStatus { code: None, signal: None };
                }
                Ok(Some(ExecEvent::Started { .. })) => {}
                Ok(None) | Err(_) => break ExitStatus { code: None, signal: None },
            },
            _ = term.kill.notified() => {
                let _ = tx.send(ExecInput::Kill { signal: None }).await;
            }
        }
    };
    term.exit.send_replace(Some(exit));
}

// ---------------------------------------------------------------------------------------------
// Run

pub(crate) struct AcpRun {
    conn: Arc<Conn>,
    child: Option<ProcHandle>,
    session_id: SharedSession,
    prompts: mpsc::UnboundedSender<String>,
    /// Instructions not passed on the command line, for the first prompt only.
    prepend_instructions: Option<String>,
}

#[async_trait]
impl AgentRun for AcpRun {
    async fn send(&mut self, text: &str) -> anyhow::Result<()> {
        let text = match self.prepend_instructions.take() {
            Some(instructions) => format!("{instructions}\n\n{text}"),
            None => text.to_string(),
        };
        self.prompts.send(text).map_err(|_| anyhow!("the ACP agent has exited"))
    }

    async fn answer(&mut self, approval_id: &str, decision: ApprovalDecision) -> anyhow::Result<()> {
        let pending = self
            .conn
            .approvals
            .lock()
            .unwrap()
            .remove(approval_id)
            .ok_or_else(|| anyhow!("unknown approval {approval_id}"))?;
        let outcome = match choose_option(&pending.options, decision) {
            Some(option_id) => json!({ "outcome": "selected", "optionId": option_id }),
            // No option of that polarity: the only safe answer left is cancelling the request.
            None => json!({ "outcome": "cancelled" }),
        };
        self.conn.respond(pending.rpc_id, Ok(json!({ "outcome": outcome }))).await;
        Ok(())
    }

    async fn interrupt(&mut self) -> anyhow::Result<()> {
        if !self.conn.turn_active.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.conn.cancel_requested.store(true, Ordering::SeqCst);
        let session_id = self.session_id.lock().unwrap().clone();
        if let Some(session_id) = session_id {
            self.conn.notify("session/cancel", json!({ "sessionId": session_id })).await?;
        }
        self.conn.cancel_permissions().await;
        Ok(())
    }

    async fn shutdown(&mut self) -> anyhow::Result<()> {
        self.conn.closing.store(true, Ordering::SeqCst);
        self.conn.host.kill_all();
        // Closing stdin ends a stdio agent; then give it a moment before killing it.
        if let Some(mut w) = self.conn.writer.lock().await.take() {
            let _ = w.shutdown().await;
        }
        if let Some(handle) = self.child.take() {
            let _ = handle.shutdown(EXIT_GRACE).await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod fake_agent;

#[cfg(test)]
mod tests;
