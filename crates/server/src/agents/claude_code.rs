//! Claude Code adapter (SPEC FR-A1, FR-A2, FR-A5).
//!
//! Drives the unmodified `claude` CLI through its own headless protocol, one long-lived process
//! per session:
//!
//! ```text
//! claude --print --input-format stream-json --output-format stream-json --verbose \
//!        --include-partial-messages --permission-prompt-tool stdio [--model M] [--resume ID]
//! ```
//!
//! # Protocol sources
//!
//! Verified against Claude Code 2.1.288 (`claude --help`, plus a recorded real session in
//! `tests/fixtures/claude/`) and the Claude Agent SDK for Python
//! (`claude_agent_sdk/_internal/query.py` and `transport/subprocess_cli.py`), which drives the
//! same CLI the same way. `--permission-prompt-tool` is hidden from `--help` but referenced by
//! `--permission-prompts`; the SDK passes `stdio` to receive permission prompts as control
//! requests on stdout.
//!
//! * stdin, user message (SDK `client.py`, confirmed by the recorded run):
//!   `{"type":"user","message":{"role":"user","content":"…"},"parent_tool_use_id":null,"session_id":""}`
//! * stdout, permission prompt (recorded):
//!   `{"type":"control_request","request_id":"<uuid>","request":{"subtype":"can_use_tool",
//!   "tool_name":"Write","input":{…},"permission_suggestions":[…],"tool_use_id":"toolu_…"}}`
//! * stdin, answer (SDK `query.py`, confirmed by the recorded run):
//!   `{"type":"control_response","response":{"subtype":"success","request_id":"<id>",
//!   "response":{"behavior":"allow","updatedInput":{…}}}}` or
//!   `…"response":{"behavior":"deny","message":"…"}`; unsupported requests get
//!   `{"type":"control_response","response":{"subtype":"error","request_id":"<id>","error":"…"}}`.
//! * stdin, interrupt (SDK `query.py`):
//!   `{"type":"control_request","request_id":"<ours>","request":{"subtype":"interrupt"}}`; the CLI
//!   answers with a `control_response` (`{"subtype":"success","response":{"still_queued":[]}}`
//!   observed with 2.1.288) and ends the turn with a `result` of subtype `error_during_execution`,
//!   which the adapter reports as `TurnEnded { Interrupted }` without an `Error`.
//! * stdout, `control_cancel_request` `{"request_id": …}` withdraws a pending permission prompt
//!   (SDK `query.py`).
//!
//! # Approvals
//!
//! Every `can_use_tool` request becomes `ApprovalRequested` with `approval_id` = the CLI's
//! `request_id`. `AllowAlways` remembers the tool name for the lifetime of this process; later
//! requests for that tool are answered `allow` by the adapter immediately and emit **no** event
//! (the tool call itself is still visible as `ToolCall`/`ToolResult`). Requests that were already
//! pending when `AllowAlways` was given stay pending until answered.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, Mutex};

use super::{AgentAdapter, AgentKind, AgentRun, Detected, StartRequest};
use crate::events::{AgentEvent, ApprovalDecision, TurnOutcome};

/// How long to wait for a `result` after the interrupt control request before sending SIGINT.
const INTERRUPT_GRACE: Duration = Duration::from_secs(10);
/// How long to wait for the process to exit after stdin is closed before killing it.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

pub struct ClaudeCodeAdapter {
    bin: PathBuf,
}

impl ClaudeCodeAdapter {
    pub fn new(bin: impl Into<PathBuf>) -> Self {
        ClaudeCodeAdapter { bin: bin.into() }
    }

    /// The binary from `EMBER_CLAUDE_BIN`, else `claude` on `PATH`.
    pub fn from_env() -> Self {
        Self::new(std::env::var_os("EMBER_CLAUDE_BIN").unwrap_or_else(|| "claude".into()))
    }

    #[cfg(test)]
    fn args(req: &StartRequest) -> Vec<String> {
        Self::args_with(req, None)
    }

    /// The CLI arguments; `mcp_file` is a private file holding the `--mcp-config` JSON, used
    /// instead of the inline JSON when an MCP server has environment (possibly secrets, which
    /// must not appear on a command line other local users can read).
    fn args_with(req: &StartRequest, mcp_file: Option<&std::path::Path>) -> Vec<String> {
        let mut args: Vec<String> = [
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--permission-prompt-tool",
            "stdio",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        if let Some(model) = &req.model {
            args.push("--model".into());
            args.push(model.clone());
        }
        if let Some(text) = &req.instructions {
            // Equals form, so instructions starting with "-" are never read as a flag.
            args.push(format!("--append-system-prompt={text}"));
        }
        if let Some(id) = &req.resume_native_id {
            // Equals form, as the SDK does, so an id can never be read as a flag.
            args.push(format!("--resume={id}"));
        }
        // `--mcp-config <configs...>` takes JSON files or strings (claude 2.1.288 --help) and is
        // variadic: the equals form keeps it to exactly this one value. Without
        // `--strict-mcp-config` the user's own MCP servers stay.
        if let Some(path) = mcp_file {
            args.push(format!("--mcp-config={}", path.display()));
        } else if let Some(config) = mcp_config(&req.mcp_servers) {
            args.push(format!("--mcp-config={config}"));
        }
        args
    }
}

/// Whether any MCP server in `req` carries environment (which goes through a private file).
fn mcp_needs_file(req: &StartRequest) -> bool {
    req.mcp_servers.iter().any(|s| !s.env.is_empty())
}

/// A `--mcp-config` file readable only by this user, removed when dropped (with the run).
pub(crate) struct PrivateConfigFile(PathBuf);

impl PrivateConfigFile {
    /// Write `contents` to a new `0600` file in `dir`.
    pub(crate) fn create(dir: &std::path::Path, contents: &str) -> anyhow::Result<PrivateConfigFile> {
        use std::io::Write;
        let path = dir.join(format!("ember-mcp-{}.json", uuid::Uuid::new_v4().simple()));
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&path)
            .map_err(|e| anyhow::anyhow!("creating MCP config file {}: {e}", path.display()))?;
        let file = PrivateConfigFile(path);
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
        Ok(file)
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for PrivateConfigFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// `MCP_TIMEOUT` (milliseconds, Claude Code's MCP server start timeout) from the longest
/// `startup_timeout_secs`, unless the server's environment already sets it. Overridden by the
/// request's own `env`.
fn mcp_timeout_env(req: &StartRequest) -> Option<(String, String)> {
    let secs = req.mcp_servers.iter().filter_map(|s| s.startup_timeout_secs).max()?;
    if std::env::var_os("MCP_TIMEOUT").is_some() {
        return None;
    }
    Some(("MCP_TIMEOUT".into(), (u64::from(secs) * 1000).to_string()))
}

/// The `--mcp-config` JSON for `servers` (`{"mcpServers": {name: {type, command, args, env?}}}`),
/// or `None` when there are none. With `env` it may hold secrets: write it to a
/// [`PrivateConfigFile`], never onto the command line.
pub fn mcp_config(servers: &[super::McpServer]) -> Option<String> {
    if servers.is_empty() {
        return None;
    }
    let map: serde_json::Map<String, Value> = servers
        .iter()
        .map(|s| {
            let mut server =
                serde_json::json!({ "type": "stdio", "command": s.command, "args": s.args });
            if !s.env.is_empty() {
                let env: serde_json::Map<String, Value> =
                    s.env.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
                server["env"] = Value::Object(env);
            }
            (s.name.clone(), server)
        })
        .collect();
    Some(serde_json::json!({ "mcpServers": map }).to_string())
}

#[async_trait]
impl AgentAdapter for ClaudeCodeAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::ClaudeCode
    }

    async fn detect(&self) -> Detected {
        let out = Command::new(&self.bin)
            .arg("--version")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output();
        let out = tokio::time::timeout(Duration::from_secs(15), out).await;
        match out {
            Ok(Ok(o)) if o.status.success() => Detected {
                kind: AgentKind::ClaudeCode,
                installed: true,
                // "2.1.288 (Claude Code)" -> "2.1.288"
                version: String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .next()
                    .map(String::from),
            },
            _ => Detected { kind: AgentKind::ClaudeCode, installed: false, version: None },
        }
    }

    async fn start(
        &self,
        req: StartRequest,
        events: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<Box<dyn AgentRun>> {
        // On another computer, Bash runs there through the `ember-exec` shim and Read/Edit/
        // Write/Glob/Grep use this server's disk, where the project mount
        // (`crate::computers::mount`) has put the node's directory at the same path when a
        // mount mechanism is enabled; otherwise the path must exist here as well.
        anyhow::ensure!(
            req.cwd.is_dir(),
            "working directory {} does not exist on the ember server (on another computer it is \
             mounted here when the project mount is enabled; see EMBER_MOUNT)",
            req.cwd.display()
        );
        // MCP servers with environment (FR-A7): the config goes into a private file that lives
        // as long as the run.
        let mcp_file = match (mcp_needs_file(&req), mcp_config(&req.mcp_servers)) {
            (true, Some(config)) => Some(PrivateConfigFile::create(&std::env::temp_dir(), &config)?),
            _ => None,
        };
        let mut child = Command::new(&self.bin)
            .envs(mcp_timeout_env(&req))
            .envs(req.env.iter().map(|(k, v)| (k, v)))
            .args(Self::args_with(&req, mcp_file.as_ref().map(PrivateConfigFile::path)))
            .current_dir(&req.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| anyhow::anyhow!("failed to start {}: {e}", self.bin.display()))?;
        let pid = child.id();
        let stdin = Arc::new(Mutex::new(child.stdin.take()));
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let state = Arc::new(StdMutex::new(RunState::default()));

        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!(target: "ember::claude", pid, "stderr: {line}");
            }
        });

        tokio::spawn(read_stdout(stdout, stdin.clone(), state.clone(), events));

        Ok(Box::new(ClaudeRun {
            child: Arc::new(Mutex::new(Some(child))),
            pid,
            stdin,
            state,
            request_counter: 0,
            _mcp_file: mcp_file,
        }))
    }
}

/// State shared between the run handle and its stdout reader.
#[derive(Default)]
struct RunState {
    /// Permission prompts waiting on the user: request_id -> (tool name, original input).
    pending: HashMap<String, (String, Value)>,
    /// Tools the user allowed for the rest of this session.
    always_allowed: HashSet<String>,
    /// A user message was sent and its `result` has not arrived yet.
    turn_active: bool,
    /// Bumped on every user message, so a stale interrupt watchdog does nothing.
    turn_seq: u64,
    interrupt_requested: bool,
    shutting_down: bool,
}

type SharedStdin = Arc<Mutex<Option<ChildStdin>>>;

async fn write_line(stdin: &SharedStdin, msg: &Value) -> anyhow::Result<()> {
    let mut guard = stdin.lock().await;
    let pipe = guard.as_mut().ok_or_else(|| anyhow::anyhow!("claude stdin is closed"))?;
    let mut line = serde_json::to_string(msg)?;
    line.push('\n');
    pipe.write_all(line.as_bytes()).await?;
    pipe.flush().await?;
    Ok(())
}

fn allow_response(request_id: &str, input: &Value) -> Value {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": { "behavior": "allow", "updatedInput": input },
        },
    })
}

fn deny_response(request_id: &str, message: &str) -> Value {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": { "behavior": "deny", "message": message },
        },
    })
}

fn error_response(request_id: &str, error: &str) -> Value {
    json!({
        "type": "control_response",
        "response": { "subtype": "error", "request_id": request_id, "error": error },
    })
}

/// Read stdout until the process closes it, translating each line.
async fn read_stdout(
    stdout: tokio::process::ChildStdout,
    stdin: SharedStdin,
    state: Arc<StdMutex<RunState>>,
    events: mpsc::Sender<AgentEvent>,
) {
    let mut parser = LineParser::default();
    let mut lines = BufReader::new(stdout).lines();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(target: "ember::claude", "reading stdout failed: {e}");
                break;
            }
        };
        tracing::trace!(target: "ember::claude", "stdout: {line}");
        for out in parser.parse(&line) {
            let event = match out {
                Parsed::Event(e) => e,
                Parsed::PermissionRequest { request_id, tool, input } => {
                    let auto = {
                        let mut st = state.lock().unwrap();
                        if st.always_allowed.contains(&tool) {
                            true
                        } else {
                            st.pending.insert(request_id.clone(), (tool.clone(), input.clone()));
                            false
                        }
                    };
                    if auto {
                        if let Err(e) =
                            write_line(&stdin, &allow_response(&request_id, &input)).await
                        {
                            tracing::warn!(target: "ember::claude", "auto-allow failed: {e:#}");
                        }
                        continue;
                    }
                    AgentEvent::ApprovalRequested { approval_id: request_id, tool, input }
                }
                Parsed::CancelRequest { request_id } => {
                    state.lock().unwrap().pending.remove(&request_id);
                    continue;
                }
                Parsed::UnsupportedRequest { request_id, subtype } => {
                    tracing::warn!(target: "ember::claude", "unsupported control request {subtype}");
                    let msg = error_response(&request_id, &format!("unsupported: {subtype}"));
                    let _ = write_line(&stdin, &msg).await;
                    continue;
                }
                Parsed::TurnEnd { error } => {
                    let interrupted = {
                        let mut st = state.lock().unwrap();
                        let interrupted = st.interrupt_requested;
                        st.interrupt_requested = false;
                        st.turn_active = false;
                        st.pending.clear();
                        interrupted
                    };
                    // An interrupted turn ends with an error `result`
                    // (`error_during_execution`, observed with 2.1.288); that is not a failure.
                    let outcome = match error {
                        _ if interrupted => TurnOutcome::Interrupted,
                        Some(message) => {
                            if events.send(AgentEvent::Error { message }).await.is_err() {
                                return;
                            }
                            TurnOutcome::Failed
                        }
                        None => TurnOutcome::Completed,
                    };
                    AgentEvent::TurnEnded { outcome }
                }
            };
            if events.send(event).await.is_err() {
                return;
            }
        }
    }
    // The process closed stdout. A turn cut short by anything but our own shutdown is reported.
    let (active, interrupted, shutting_down) = {
        let st = state.lock().unwrap();
        (st.turn_active, st.interrupt_requested, st.shutting_down)
    };
    if active && !shutting_down {
        let outcome = if interrupted {
            TurnOutcome::Interrupted
        } else {
            let _ = events
                .send(AgentEvent::Error { message: "claude exited during a turn".into() })
                .await;
            TurnOutcome::Failed
        };
        let _ = events.send(AgentEvent::TurnEnded { outcome }).await;
    }
}

struct ClaudeRun {
    child: Arc<Mutex<Option<Child>>>,
    pid: Option<u32>,
    stdin: SharedStdin,
    state: Arc<StdMutex<RunState>>,
    request_counter: u64,
    /// The private `--mcp-config` file, removed with the run.
    _mcp_file: Option<PrivateConfigFile>,
}

#[async_trait]
impl AgentRun for ClaudeRun {
    async fn send(&mut self, text: &str) -> anyhow::Result<()> {
        {
            let mut st = self.state.lock().unwrap();
            st.turn_active = true;
            st.turn_seq += 1;
            st.interrupt_requested = false;
        }
        let msg = json!({
            "type": "user",
            "message": { "role": "user", "content": text },
            "parent_tool_use_id": null,
            "session_id": "",
        });
        write_line(&self.stdin, &msg).await
    }

    async fn answer(
        &mut self,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> anyhow::Result<()> {
        let (tool, input) = {
            let mut st = self.state.lock().unwrap();
            let (tool, input) = st
                .pending
                .remove(approval_id)
                .ok_or_else(|| anyhow::anyhow!("unknown approval {approval_id}"))?;
            if decision == ApprovalDecision::AllowAlways {
                st.always_allowed.insert(tool.clone());
            }
            (tool, input)
        };
        let msg = match decision {
            ApprovalDecision::AllowOnce | ApprovalDecision::AllowAlways => {
                allow_response(approval_id, &input)
            }
            ApprovalDecision::Deny => {
                deny_response(approval_id, &format!("The user denied {tool} in Ember."))
            }
        };
        write_line(&self.stdin, &msg).await
    }

    async fn interrupt(&mut self) -> anyhow::Result<()> {
        let seq = {
            let mut st = self.state.lock().unwrap();
            if !st.turn_active {
                return Ok(());
            }
            st.interrupt_requested = true;
            st.turn_seq
        };
        self.request_counter += 1;
        let msg = json!({
            "type": "control_request",
            "request_id": format!("ember_{}_{}", self.request_counter, uuid::Uuid::new_v4().simple()),
            "request": { "subtype": "interrupt" },
        });
        let sent = write_line(&self.stdin, &msg).await;
        // The reader emits TurnEnded{Interrupted} when the CLI's `result` arrives. If it never
        // does (or the request could not be written), fall back to SIGINT.
        let state = self.state.clone();
        let pid = self.pid;
        let delay = if sent.is_ok() { INTERRUPT_GRACE } else { Duration::ZERO };
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let still_running = {
                let st = state.lock().unwrap();
                st.turn_active && st.turn_seq == seq && st.interrupt_requested
            };
            if still_running {
                if let Some(pid) = pid {
                    tracing::warn!(target: "ember::claude", pid, "interrupt not acknowledged; SIGINT");
                    sigint(pid);
                }
            }
        });
        Ok(())
    }

    async fn shutdown(&mut self) -> anyhow::Result<()> {
        self.state.lock().unwrap().shutting_down = true;
        // Closing stdin ends the CLI's input stream; it exits after the current work.
        self.stdin.lock().await.take();
        if let Some(mut child) = self.child.lock().await.take() {
            match tokio::time::timeout(SHUTDOWN_GRACE, child.wait()).await {
                Ok(status) => {
                    status?;
                }
                Err(_) => {
                    tracing::warn!(target: "ember::claude", "claude did not exit; killing");
                    child.kill().await?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
fn sigint(pid: u32) {
    // SAFETY: plain kill(2) on a pid we spawned.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGINT);
    }
}

#[cfg(not(unix))]
fn sigint(_pid: u32) {}

/// One translated stdout line.
#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Event(AgentEvent),
    /// `control_request` / `can_use_tool`.
    PermissionRequest {
        request_id: String,
        tool: String,
        input: Value,
    },
    /// `control_cancel_request`: a pending permission prompt was withdrawn.
    CancelRequest {
        request_id: String,
    },
    /// A control request this adapter does not implement; answered with an error.
    UnsupportedRequest {
        request_id: String,
        subtype: String,
    },
    /// `result`: the turn ended, with the CLI's error message if it reported one.
    TurnEnd {
        error: Option<String>,
    },
}

/// Translates Claude Code stream-json stdout lines into [`Parsed`] items. Unknown or malformed
/// lines yield nothing.
#[derive(Debug, Default)]
pub struct LineParser {
    native_id: Option<String>,
}

impl LineParser {
    pub fn parse(&mut self, line: &str) -> Vec<Parsed> {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            if !line.trim().is_empty() {
                tracing::debug!(target: "ember::claude", "ignoring non-JSON line: {line}");
            }
            return Vec::new();
        };
        let mut out = Vec::new();
        match v["type"].as_str().unwrap_or_default() {
            "system" if v["subtype"] == "init" => {
                if let Some(id) = v["session_id"].as_str() {
                    if self.native_id.as_deref() != Some(id) {
                        self.native_id = Some(id.to_string());
                        out.push(Parsed::Event(AgentEvent::NativeSession {
                            native_id: id.to_string(),
                        }));
                    }
                }
            }
            "stream_event" => {
                let ev = &v["event"];
                if ev["type"] == "content_block_delta" && ev["delta"]["type"] == "text_delta" {
                    if let Some(text) = ev["delta"]["text"].as_str() {
                        if !text.is_empty() {
                            out.push(Parsed::Event(AgentEvent::AssistantDelta {
                                text: text.to_string(),
                            }));
                        }
                    }
                }
            }
            "assistant" => {
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    match block["type"].as_str() {
                        Some("text") => {
                            let text = block["text"].as_str().unwrap_or_default();
                            if !text.is_empty() {
                                out.push(Parsed::Event(AgentEvent::AssistantMessage {
                                    text: text.to_string(),
                                }));
                            }
                        }
                        Some("tool_use") => out.push(Parsed::Event(AgentEvent::ToolCall {
                            call_id: block["id"].as_str().unwrap_or_default().to_string(),
                            name: block["name"].as_str().unwrap_or_default().to_string(),
                            input: block["input"].clone(),
                        })),
                        _ => {}
                    }
                }
            }
            "user" => {
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] == "tool_result" {
                        out.push(Parsed::Event(AgentEvent::ToolResult {
                            call_id: block["tool_use_id"].as_str().unwrap_or_default().to_string(),
                            output: tool_result_text(&block["content"]),
                            is_error: block["is_error"].as_bool().unwrap_or(false),
                        }));
                    }
                }
            }
            "result" => {
                tracing::debug!(
                    target: "ember::claude",
                    "result: subtype={} is_error={}",
                    v["subtype"],
                    v["is_error"]
                );
                let u = &v["usage"];
                let n = |k: &str| u[k].as_u64().unwrap_or(0);
                if u.is_object() {
                    out.push(Parsed::Event(AgentEvent::Usage {
                        // Total prompt tokens the turn processed, cached or not.
                        input_tokens: n("input_tokens")
                            + n("cache_creation_input_tokens")
                            + n("cache_read_input_tokens"),
                        output_tokens: n("output_tokens"),
                    }));
                }
                let failed = v["is_error"].as_bool().unwrap_or(false)
                    || v["subtype"].as_str().is_some_and(|s| s.starts_with("error"));
                let error = failed.then(|| {
                    let errors: Vec<&str> = v["errors"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect();
                    match v["result"].as_str() {
                        Some(r) if !r.is_empty() => r.to_string(),
                        _ if !errors.is_empty() => errors.join("\n"),
                        _ => format!(
                            "claude turn failed ({})",
                            v["subtype"].as_str().unwrap_or("unknown")
                        ),
                    }
                });
                out.push(Parsed::TurnEnd { error });
            }
            "control_request" => {
                let request_id = v["request_id"].as_str().unwrap_or_default().to_string();
                let req = &v["request"];
                match req["subtype"].as_str().unwrap_or_default() {
                    "can_use_tool" => out.push(Parsed::PermissionRequest {
                        request_id,
                        tool: req["tool_name"].as_str().unwrap_or_default().to_string(),
                        input: req["input"].clone(),
                    }),
                    other => out.push(Parsed::UnsupportedRequest {
                        request_id,
                        subtype: other.to_string(),
                    }),
                }
            }
            // `{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":<unix s>,
            // "rateLimitType":"five_hour",…}}` (recorded with 2.1.288). `status` is `allowed`,
            // `allowed_warning` or `rejected` (Agent SDK `SDKRateLimitEvent`); only a rejection
            // means the account is out of quota (FR-U3).
            "rate_limit_event" => {
                let info = &v["rate_limit_info"];
                if info["status"] == "rejected" {
                    let kind = info["rateLimitType"].as_str().unwrap_or("usage");
                    out.push(Parsed::Event(AgentEvent::RateLimited {
                        resets_at: info["resetsAt"].as_i64().map(|s| s * 1000),
                        message: format!("Claude {kind} limit reached"),
                    }));
                }
            }
            "control_response" => {
                tracing::debug!(target: "ember::claude", "control_response: {}", v["response"]);
            }
            "control_cancel_request" => {
                if let Some(id) = v["request_id"].as_str() {
                    out.push(Parsed::CancelRequest { request_id: id.to_string() });
                }
            }
            _ => {}
        }
        out
    }
}

/// `tool_result.content` is either a string or a list of content blocks.
fn tool_result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => {
            blocks.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n")
        }
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORDED: &str = include_str!("../../tests/fixtures/claude/write_then_bash.jsonl");

    fn parse_all(text: &str) -> Vec<Parsed> {
        let mut p = LineParser::default();
        text.lines().flat_map(|l| p.parse(l)).collect()
    }

    #[test]
    fn recorded_turn_translates_in_order() {
        let out = parse_all(RECORDED);
        // Everything but deltas, in order.
        let shape: Vec<String> = out
            .iter()
            .filter(|p| !matches!(p, Parsed::Event(AgentEvent::AssistantDelta { .. })))
            .map(|p| match p {
                Parsed::Event(AgentEvent::NativeSession { .. }) => "native".into(),
                Parsed::Event(AgentEvent::ToolCall { name, .. }) => format!("call:{name}"),
                Parsed::Event(AgentEvent::ToolResult { output, is_error, .. }) => {
                    format!("result:{}:{is_error}", output.len().min(2))
                }
                Parsed::PermissionRequest { tool, .. } => format!("perm:{tool}"),
                Parsed::Event(AgentEvent::AssistantMessage { .. }) => "message".into(),
                Parsed::Event(AgentEvent::Usage { .. }) => "usage".into(),
                Parsed::TurnEnd { error } => format!("end:{}", error.is_some()),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            shape,
            [
                "native",
                "call:Write",
                "perm:Write",
                "result:2:false",
                "call:Bash",
                "result:2:false",
                "message",
                "usage",
                "end:false"
            ]
        );
        let deltas: String = out
            .iter()
            .filter_map(|p| match p {
                Parsed::Event(AgentEvent::AssistantDelta { text }) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let message = out.iter().find_map(|p| match p {
            Parsed::Event(AgentEvent::AssistantMessage { text }) => Some(text.clone()),
            _ => None,
        });
        assert_eq!(Some(deltas), message, "deltas add up to the final message");
    }

    #[test]
    fn recorded_details() {
        let out = parse_all(RECORDED);
        assert!(out.contains(&Parsed::Event(AgentEvent::NativeSession {
            native_id: "64224439-15e9-4763-942c-be8d6e8a9723".into()
        })));
        let perm = out.iter().find_map(|p| match p {
            Parsed::PermissionRequest { request_id, tool, input } => {
                Some((request_id.clone(), tool.clone(), input.clone()))
            }
            _ => None,
        });
        let (id, tool, input) = perm.unwrap();
        assert_eq!(id, "376da9ed-f58a-4d20-b4b3-5fd7a994d06d");
        assert_eq!(tool, "Write");
        assert_eq!(input["content"], "hi");
        assert!(out.contains(&Parsed::Event(AgentEvent::ToolResult {
            call_id: "toolu_01SRMcmwcLNpvYY359wUViKS".into(),
            output: "hi".into(),
            is_error: false,
        })));
        assert!(out.contains(&Parsed::Event(AgentEvent::Usage {
            input_tokens: 18 + 12378 + 38098,
            output_tokens: 297,
        })));
    }

    #[test]
    fn unknown_and_malformed_lines_are_ignored() {
        let mut p = LineParser::default();
        assert!(p.parse("").is_empty());
        assert!(p.parse("not json").is_empty());
        assert!(p.parse(r#"{"type":"rate_limit_event"}"#).is_empty());
        assert!(p.parse(r#"{"type":"system","subtype":"status"}"#).is_empty());
        assert!(p.parse(r#"{"no_type":1}"#).is_empty());
    }

    // The following lines are hand-written (not recorded) in the shapes documented above.

    #[test]
    fn rejected_rate_limit_event_reports_the_reset() {
        let mut p = LineParser::default();
        let allowed = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1790982600,"rateLimitType":"five_hour"}}"#;
        assert!(p.parse(allowed).is_empty());
        let rejected = allowed.replace("\"allowed\"", "\"rejected\"");
        assert_eq!(
            p.parse(&rejected),
            [Parsed::Event(AgentEvent::RateLimited {
                resets_at: Some(1_790_982_600_000),
                message: "Claude five_hour limit reached".into(),
            })]
        );
    }

    #[test]
    fn error_result_fails_the_turn() {
        let mut p = LineParser::default();
        let out = p.parse(
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"usage":{"input_tokens":1,"output_tokens":2}}"#,
        );
        assert_eq!(out[0], Parsed::Event(AgentEvent::Usage { input_tokens: 1, output_tokens: 2 }));
        assert_eq!(
            out[1],
            Parsed::TurnEnd { error: Some("claude turn failed (error_during_execution)".into()) }
        );
        // Shape observed after an interrupt with 2.1.288.
        let out = p.parse(
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["boom"]}"#,
        );
        assert_eq!(out, [Parsed::TurnEnd { error: Some("boom".into()) }]);
    }

    #[test]
    fn native_id_announced_once_per_id() {
        let mut p = LineParser::default();
        let init = r#"{"type":"system","subtype":"init","session_id":"abc"}"#;
        assert_eq!(p.parse(init).len(), 1);
        assert!(p.parse(init).is_empty());
    }

    #[test]
    fn control_lines() {
        let mut p = LineParser::default();
        assert_eq!(
            p.parse(r#"{"type":"control_cancel_request","request_id":"r1"}"#),
            [Parsed::CancelRequest { request_id: "r1".into() }]
        );
        assert_eq!(
            p.parse(r#"{"type":"control_request","request_id":"r2","request":{"subtype":"mcp_message"}}"#),
            [Parsed::UnsupportedRequest { request_id: "r2".into(), subtype: "mcp_message".into() }]
        );
        assert!(p
            .parse(
                r#"{"type":"control_response","response":{"subtype":"success","request_id":"x"}}"#
            )
            .is_empty());
    }

    #[test]
    fn tool_result_block_list_is_joined() {
        let mut p = LineParser::default();
        let out = p.parse(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":[{"type":"text","text":"a"},{"type":"text","text":"b"}],"is_error":true}]}}"#,
        );
        assert_eq!(
            out,
            [Parsed::Event(AgentEvent::ToolResult {
                call_id: "t".into(),
                output: "a\nb".into(),
                is_error: true
            })]
        );
    }

    #[test]
    fn args_cover_model_and_resume() {
        let req = StartRequest {
            cwd: ".".into(),
            resume_native_id: Some("abc".into()),
            model: Some("haiku".into()),
            env: Vec::new(),
            instructions: Some("use ember-a2a".into()),
            remote: None,
            mcp_servers: Vec::new(),
            computer: None,
        };
        let args = ClaudeCodeAdapter::args(&req);
        assert!(!args.iter().any(|a| a.starts_with("--mcp-config")));
        assert!(args.contains(&"--append-system-prompt=use ember-a2a".to_string()));
        assert!(args.windows(2).any(|w| w == ["--permission-prompt-tool", "stdio"]));
        assert!(args.windows(2).any(|w| w == ["--model", "haiku"]));
        assert!(args.contains(&"--resume=abc".to_string()));
    }

    #[test]
    fn args_include_the_browser_mcp_config_when_enabled() {
        let req = StartRequest {
            cwd: ".".into(),
            mcp_servers: vec![crate::agents::McpServer {
                name: "ember-browser".into(),
                command: "/usr/local/bin/npx".into(),
                args: vec!["-y".into(), "chrome-devtools-mcp@latest".into(), "--wsEndpoint=ws://x/cdp".into()],
                startup_timeout_secs: Some(60),
                env: Vec::new(),
            }],
            ..Default::default()
        };
        let args = ClaudeCodeAdapter::args(&req);
        let flag = args.iter().find_map(|a| a.strip_prefix("--mcp-config=")).expect("--mcp-config");
        let v: Value = serde_json::from_str(flag).unwrap();
        let server = &v["mcpServers"]["ember-browser"];
        assert_eq!(server["type"], "stdio");
        assert_eq!(server["command"], "/usr/local/bin/npx");
        assert_eq!(server["args"][2], "--wsEndpoint=ws://x/cdp");
        assert!(!args.contains(&"--strict-mcp-config".to_string()), "the user's servers stay");
    }

    /// FR-A7: registry servers with secret environment go through a private file, never argv.
    #[test]
    fn mcp_env_goes_through_a_private_file_not_the_command_line() {
        let req = StartRequest {
            cwd: ".".into(),
            mcp_servers: vec![crate::agents::McpServer {
                name: "github".into(),
                command: "/usr/local/bin/gh-mcp".into(),
                args: vec!["--stdio".into()],
                startup_timeout_secs: None,
                env: vec![("GITHUB_TOKEN".into(), "ghp_SECRET123".into())],
            }],
            ..Default::default()
        };
        assert!(mcp_needs_file(&req));
        let config = mcp_config(&req.mcp_servers).unwrap();
        let v: Value = serde_json::from_str(&config).unwrap();
        assert_eq!(v["mcpServers"]["github"]["env"]["GITHUB_TOKEN"], "ghp_SECRET123");

        let dir = tempfile::tempdir().unwrap();
        let file = PrivateConfigFile::create(dir.path(), &config).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(file.path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let args = ClaudeCodeAdapter::args_with(&req, Some(file.path()));
        let joined = args.join(" ");
        assert!(!joined.contains("ghp_SECRET123"), "{joined}");
        assert!(args.contains(&format!("--mcp-config={}", file.path().display())));
        assert!(!format!("{req:?}").contains("ghp_SECRET123"), "Debug redacts MCP env");
        let path = file.path().to_path_buf();
        drop(file);
        assert!(!path.exists(), "removed with the run");
    }
}
