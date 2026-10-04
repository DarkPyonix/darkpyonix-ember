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
//! ["omp", {"name": "gemini", "command": "gemini", "args": ["--experimental-acp"]}]
//! ```
//!
//! A string is a preset name; an object whose name is a preset overrides the preset's fields.
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

use super::{AcpName, AgentAdapter, AgentKind, AgentRun, Detected, McpServer, StartRequest};
use crate::events::{AgentEvent, ApprovalDecision, TurnOutcome};

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
}

impl AcpConfig {
    /// A preset by name: `omp`. `bin` overrides the preset's command.
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
            }),
            _ => None,
        }
    }

    /// The full argument list for a process serving `req`.
    pub fn process_args(&self, req: &StartRequest) -> Vec<String> {
        let mut args = self.args.clone();
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

    async fn detect(&self) -> Detected {
        let out = tokio::time::timeout(
            Duration::from_secs(10),
            Command::new(&self.config.command)
                .args(&self.config.version_args)
                .envs(self.config.env.iter().map(|(k, v)| (k, v)))
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
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
        // On another computer the project path need not exist here (without a mount).
        let spawn_dir = if req.cwd.is_dir() { req.cwd.clone() } else { std::env::temp_dir() };
        let mut child = Command::new(&self.config.command)
            .args(self.config.process_args(&req))
            .envs(self.config.env.iter().map(|(k, v)| (k, v)))
            .envs(req.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&spawn_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("starting {} (ACP agent {})", self.config.command.display(), self.config.name))?;
        let stdin = child.stdin.take().context("ACP agent stdin")?;
        let stdout = child.stdout.take().context("ACP agent stdout")?;
        let stderr = child.stderr.take().context("ACP agent stderr")?;
        let name = self.config.name.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "acp", agent = %name, "{line}");
            }
        });
        let prepend_instructions =
            if self.config.instructions_args.is_empty() { req.instructions.clone() } else { None };
        let setup = Setup {
            cwd: req.cwd.clone(),
            resume_native_id: req.resume_native_id.clone(),
            mcp_servers: req.mcp_servers.clone(),
            computer: req.computer.clone(),
            prepend_instructions,
        };
        let run = connect(stdout, stdin, Some(child), setup, events).await?;
        Ok(Box::new(run))
    }
}

/// `omp 18.5.0` / `18.5.0` → `18.5.0`: the last word of the first non-empty line.
fn parse_version(stdout: &str) -> Option<String> {
    stdout.lines().find(|l| !l.trim().is_empty())?.split_whitespace().last().map(str::to_string)
}

/// What [`connect`] needs besides the byte streams.
pub(crate) struct Setup {
    pub cwd: PathBuf,
    pub resume_native_id: Option<String>,
    pub mcp_servers: Vec<McpServer>,
    pub computer: Option<NodeClient>,
    /// Put in front of the first prompt (agents without an instructions argument).
    pub prepend_instructions: Option<String>,
}

/// Speak ACP over `reader`/`writer` (the agent's stdout/stdin): initialize, open the session,
/// and return the running agent. Separate from process spawning so tests can use an in-process
/// agent.
pub(crate) async fn connect<R, W>(
    reader: R,
    writer: W,
    child: Option<Child>,
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

    let session_id = match setup.resume_native_id {
        Some(id) if can_resume => {
            conn.request(
                "session/resume",
                json!({ "sessionId": id, "cwd": cwd_str, "mcpServers": mcp }),
                Some(REQUEST_TIMEOUT),
            )
            .await
            .context("ACP session/resume")?;
            id
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
            id
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
            let resp = conn
                .request("session/new", json!({ "cwd": cwd_str, "mcpServers": mcp }), Some(REQUEST_TIMEOUT))
                .await
                .context("ACP session/new")?;
            resp.get("sessionId")
                .and_then(Value::as_str)
                .context("session/new response without sessionId")?
                .to_string()
        }
    };
    events.send(AgentEvent::NativeSession { native_id: session_id.clone() }).await?;

    let (prompts, prompt_rx) = mpsc::unbounded_channel();
    tokio::spawn(prompt_loop(conn.clone(), session_id.clone(), prompt_rx));
    Ok(AcpRun {
        conn,
        child,
        session_id,
        prompts,
        prepend_instructions: setup.prepend_instructions,
    })
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

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RpcError {
    code: i64,
    message: String,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> RpcError {
        RpcError { code, message: message.into() }
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

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
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
        self.request_raw(method, params, timeout).await.map_err(|e| anyhow!("ACP {method} failed: {e}"))
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
            tracing::warn!(target: "acp", "ignoring non-JSON line from the agent: {line}");
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
                    let reply = match msg.get("error") {
                        Some(err) => Err(RpcError::new(
                            err.get("code").and_then(Value::as_i64).unwrap_or(INTERNAL_ERROR),
                            err.get("message").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| err.to_string()),
                        )),
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

/// Prompts one queued message at a time; each `session/prompt` response ends a turn.
async fn prompt_loop(conn: Arc<Conn>, session_id: String, mut prompts: mpsc::UnboundedReceiver<String>) {
    while let Some(text) = prompts.recv().await {
        conn.cancel_requested.store(false, Ordering::SeqCst);
        conn.turn_active.store(true, Ordering::SeqCst);
        let params = json!({ "sessionId": session_id, "prompt": [{ "type": "text", "text": text }] });
        let reply = conn.request_raw("session/prompt", params, None).await;
        // Permission requests the agent left open belong to the finished turn.
        conn.cancel_permissions().await;
        let cancelled = conn.cancel_requested.load(Ordering::SeqCst);
        let closing = conn.closing.load(Ordering::SeqCst);
        let mut out = conn.mapper.lock().unwrap().end_turn();
        match reply {
            Ok(result) => out.extend(turn_end_events(&result, cancelled)),
            Err(_) if closing => {}
            Err(_) if cancelled => out.push(AgentEvent::TurnEnded { outcome: TurnOutcome::Interrupted }),
            Err(e) => {
                out.push(AgentEvent::Error { message: format!("ACP session/prompt failed: {e}") });
                out.push(AgentEvent::TurnEnded { outcome: TurnOutcome::Failed });
            }
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
        out.push(AgentEvent::ApprovalRequested { approval_id: approval_id.to_string(), tool, input });
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
                let mut child = Command::new(&command)
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
    child: Option<Child>,
    session_id: String,
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
        self.conn.notify("session/cancel", json!({ "sessionId": self.session_id })).await?;
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
        if let Some(mut child) = self.child.take() {
            if tokio::time::timeout(EXIT_GRACE, child.wait()).await.is_err() {
                let _ = child.kill().await;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod fake_agent;

#[cfg(test)]
mod tests;
