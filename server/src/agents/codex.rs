//! Codex adapter: drives the unmodified `codex` CLI through `codex app-server` (SPEC FR-A2).
//!
//! `codex app-server` speaks JSON-RPC 2.0 over stdio, one JSON object per line, without the
//! `"jsonrpc"` field on the wire. The protocol was read from the CLI itself
//! (`codex app-server generate-ts` / `generate-json-schema`, codex-cli 0.155.1) and checked against
//! a captured real session (`tests/fixtures/codex/session.jsonl`).
//!
//! Methods used (client → server requests unless noted):
//! - `initialize` (+ client notification `initialized`): handshake.
//! - `thread/start` / `thread/resume`: new thread, or native resume by thread id (FR-S2). The
//!   thread id is the native session id.
//! - `turn/start`: send a user message when no turn is running; `turn/steer` adds it to the running
//!   turn (what Codex's own clients do with a follow-up mid-turn).
//! - `turn/interrupt`: interrupt; Codex then sends `turn/completed` with status `interrupted`.
//!
//! Server → client notifications mapped to [`AgentEvent`]s:
//! - `turn/started`, `turn/completed` (→ `TurnEnded`, plus `Error` on failure),
//! - `item/started`, `item/completed` for `agentMessage`, `commandExecution`, `fileChange`,
//!   `mcpToolCall`, `dynamicToolCall` items,
//! - `item/agentMessage/delta` (→ `AssistantDelta`),
//! - `thread/tokenUsage/updated` (→ one `Usage` per turn),
//! - `error` (→ `Error` when Codex will not retry),
//! - `serverRequest/resolved` (forget an approval Codex no longer waits for).
//!
//! Every other notification is ignored.
//!
//! Server → client requests mapped to `ApprovalRequested` and answered by [`AgentRun::answer`]:
//! `item/commandExecution/requestApproval`, `item/fileChange/requestApproval`,
//! `item/permissions/requestApproval`, and the legacy `execCommandApproval` / `applyPatchApproval`.
//! Any other server request gets a JSON-RPC error so Codex never waits on us.
//!
//! Codex's own settings (approval policy, sandbox, model, config.toml) apply as they do natively
//! (FR-A4); the adapter only overrides what the session asks for (model, cwd) or what it was built
//! with ([`CodexAdapter::with_approval_policy`], [`CodexAdapter::with_sandbox`]).

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context};
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot};

use super::{AgentAdapter, AgentKind, AgentRun, Detected, StartRequest};
use crate::events::{AgentEvent, ApprovalDecision, TurnOutcome};

/// How long to wait for a response to one of our requests.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// How long `shutdown` waits for the app-server to exit after stdin closes.
const EXIT_GRACE: Duration = Duration::from_secs(5);

pub struct CodexAdapter {
    bin: PathBuf,
    approval_policy: Option<Value>,
    sandbox: Option<String>,
}

impl CodexAdapter {
    pub fn new(bin: impl Into<PathBuf>) -> CodexAdapter {
        CodexAdapter { bin: bin.into(), approval_policy: None, sandbox: None }
    }

    /// The binary from `EMBER_CODEX_BIN`, default `codex` on `PATH`.
    pub fn from_env() -> CodexAdapter {
        CodexAdapter::new(std::env::var_os("EMBER_CODEX_BIN").unwrap_or_else(|| "codex".into()))
    }

    /// Override Codex's approval policy (`AskForApproval`, e.g. `"untrusted"`, `"on-request"`).
    /// Without it the user's own Codex configuration decides.
    pub fn with_approval_policy(mut self, policy: impl Into<Value>) -> CodexAdapter {
        self.approval_policy = Some(policy.into());
        self
    }

    /// Override Codex's sandbox mode (`"read-only"`, `"workspace-write"`, `"danger-full-access"`).
    pub fn with_sandbox(mut self, sandbox: impl Into<String>) -> CodexAdapter {
        self.sandbox = Some(sandbox.into());
        self
    }

    /// Params shared by `thread/start` and `thread/resume`.
    fn thread_params(&self, req: &StartRequest) -> Map<String, Value> {
        let mut p = Map::new();
        p.insert("cwd".into(), json!(req.cwd.to_string_lossy()));
        if let Some(model) = &req.model {
            p.insert("model".into(), json!(model));
        }
        if let Some(policy) = &self.approval_policy {
            p.insert("approvalPolicy".into(), policy.clone());
        }
        if let Some(sandbox) = &self.sandbox {
            p.insert("sandbox".into(), json!(sandbox));
        }
        p
    }
}

#[async_trait]
impl AgentAdapter for CodexAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    async fn detect(&self) -> Detected {
        let out = tokio::time::timeout(
            Duration::from_secs(10),
            Command::new(&self.bin).arg("--version").stdin(Stdio::null()).output(),
        )
        .await;
        let version = match out {
            Ok(Ok(out)) if out.status.success() => {
                parse_version(&String::from_utf8_lossy(&out.stdout))
            }
            _ => return Detected { kind: AgentKind::Codex, installed: false, version: None },
        };
        Detected { kind: AgentKind::Codex, installed: true, version }
    }

    async fn start(
        &self,
        req: StartRequest,
        events: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<Box<dyn AgentRun>> {
        let mut child = Command::new(&self.bin)
            .arg("app-server")
            .current_dir(&req.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("starting {} app-server", self.bin.display()))?;
        let stdin = child.stdin.take().context("app-server stdin")?;
        let stdout = child.stdout.take().context("app-server stdout")?;
        let stderr = child.stderr.take().context("app-server stderr")?;

        let conn = Arc::new(Conn {
            stdin: tokio::sync::Mutex::new(Some(stdin)),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicI64::new(0),
            mapper: Mutex::new(Mapper::default()),
            closing: AtomicBool::new(false),
        });

        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "codex", "{line}");
            }
        });
        tokio::spawn(read_loop(conn.clone(), stdout, events.clone()));

        conn.request("initialize", initialize_params()).await.context("codex initialize")?;
        conn.notify("initialized", None).await?;

        let mut params = self.thread_params(&req);
        let resp = match &req.resume_native_id {
            Some(id) => {
                params.insert("threadId".into(), json!(id));
                params.insert("excludeTurns".into(), json!(true));
                conn.request("thread/resume", Value::Object(params))
                    .await
                    .context("thread/resume")?
            }
            None => {
                conn.request("thread/start", Value::Object(params)).await.context("thread/start")?
            }
        };
        let thread_id = thread_id_of(&resp).context("thread response without thread.id")?;
        conn.mapper.lock().unwrap().thread_id = Some(thread_id.clone());
        events.send(AgentEvent::NativeSession { native_id: thread_id.clone() }).await?;

        Ok(Box::new(CodexRun { conn, child: Some(child), events, thread_id }))
    }
}

/// `codex-cli 0.155.1` → `0.155.1`.
fn parse_version(stdout: &str) -> Option<String> {
    stdout.lines().next()?.split_whitespace().last().map(str::to_string)
}

fn initialize_params() -> Value {
    json!({
        "clientInfo": { "name": "ember", "title": null, "version": env!("CARGO_PKG_VERSION") },
        "capabilities": null,
    })
}

/// `result.thread.id` of a `thread/start` / `thread/resume` response.
fn thread_id_of(result: &Value) -> Option<String> {
    result.pointer("/thread/id")?.as_str().map(str::to_string)
}

fn text_input(text: &str) -> Value {
    json!([{ "type": "text", "text": text, "text_elements": [] }])
}

// ---------------------------------------------------------------------------------------------
// Connection

type Reply = Result<Value, String>;

/// One app-server process's JSON-RPC connection.
struct Conn {
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    pending: Mutex<HashMap<i64, oneshot::Sender<Reply>>>,
    next_id: AtomicI64,
    mapper: Mutex<Mapper>,
    /// Set by `shutdown`, so the reader does not report the exit as an error.
    closing: AtomicBool,
}

impl Conn {
    async fn write(&self, msg: &Value) -> anyhow::Result<()> {
        let mut line = serde_json::to_vec(msg)?;
        line.push(b'\n');
        let mut stdin = self.stdin.lock().await;
        let stdin = stdin.as_mut().ok_or_else(|| anyhow!("codex app-server is shut down"))?;
        stdin.write_all(&line).await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn request(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if let Err(e) = self.write(&json!({ "id": id, "method": method, "params": params })).await {
            self.pending.lock().unwrap().remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(Ok(result))) => Ok(result),
            Ok(Ok(Err(message))) => Err(anyhow!("codex {method} failed: {message}")),
            Ok(Err(_)) => Err(anyhow!("codex app-server exited during {method}")),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(anyhow!("codex {method} timed out"))
            }
        }
    }

    async fn notify(&self, method: &str, params: Option<Value>) -> anyhow::Result<()> {
        let mut msg = json!({ "method": method });
        if let Some(p) = params {
            msg["params"] = p;
        }
        self.write(&msg).await
    }
}

async fn read_loop(
    conn: Arc<Conn>,
    stdout: tokio::process::ChildStdout,
    events: mpsc::Sender<AgentEvent>,
) {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            tracing::warn!(target: "codex", "ignoring non-JSON line from app-server: {line}");
            continue;
        };
        let null = Value::Null;
        let params = msg.get("params").unwrap_or(&null);
        let out = match (msg.get("method").and_then(Value::as_str), msg.get("id")) {
            (Some(method), Some(id)) => {
                let mapped =
                    conn.mapper.lock().unwrap().on_server_request(id.clone(), method, params);
                match mapped {
                    Ok(evs) => evs,
                    Err(message) => {
                        tracing::warn!(target: "codex", "unsupported server request {method}");
                        let reply =
                            json!({ "id": id, "error": { "code": -32601, "message": message } });
                        if let Err(e) = conn.write(&reply).await {
                            tracing::warn!(target: "codex", "failed to answer {method}: {e:#}");
                        }
                        Vec::new()
                    }
                }
            }
            (Some(method), None) => conn.mapper.lock().unwrap().on_notification(method, params),
            (None, Some(id)) => {
                let waiter = id.as_i64().and_then(|id| conn.pending.lock().unwrap().remove(&id));
                if let Some(waiter) = waiter {
                    let reply = match msg.get("error") {
                        Some(err) => Err(err
                            .get("message")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| err.to_string())),
                        None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = waiter.send(reply);
                }
                Vec::new()
            }
            (None, None) => Vec::new(),
        };
        for ev in out {
            if events.send(ev).await.is_err() {
                return;
            }
        }
    }
    // EOF: fail outstanding requests, and report an unexpected exit.
    conn.pending.lock().unwrap().clear();
    if !conn.closing.load(Ordering::Relaxed) {
        let evs = conn.mapper.lock().unwrap().on_exit();
        for ev in evs {
            let _ = events.send(ev).await;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Run

struct CodexRun {
    conn: Arc<Conn>,
    child: Option<Child>,
    events: mpsc::Sender<AgentEvent>,
    thread_id: String,
}

#[async_trait]
impl AgentRun for CodexRun {
    async fn send(&mut self, text: &str) -> anyhow::Result<()> {
        let active = self.conn.mapper.lock().unwrap().turn_id.clone();
        if let Some(turn_id) = active {
            let steer = self
                .conn
                .request(
                    "turn/steer",
                    json!({ "threadId": self.thread_id, "input": text_input(text), "expectedTurnId": turn_id }),
                )
                .await;
            match steer {
                Ok(_) => return Ok(()),
                // The turn ended between our check and the request: start a new one.
                Err(e) => {
                    tracing::debug!(target: "codex", "turn/steer failed, starting a turn: {e:#}")
                }
            }
        }
        let resp = self
            .conn
            .request("turn/start", json!({ "threadId": self.thread_id, "input": text_input(text) }))
            .await;
        match resp {
            Ok(resp) => {
                if let Some(turn_id) = resp.pointer("/turn/id").and_then(Value::as_str) {
                    self.conn.mapper.lock().unwrap().turn_accepted(turn_id);
                }
                Ok(())
            }
            Err(e) => {
                // The session already recorded the message and is running; end that turn.
                let _ = self.events.send(AgentEvent::Error { message: format!("{e:#}") }).await;
                let _ =
                    self.events.send(AgentEvent::TurnEnded { outcome: TurnOutcome::Failed }).await;
                Err(e)
            }
        }
    }

    async fn answer(
        &mut self,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> anyhow::Result<()> {
        let pending = self
            .conn
            .mapper
            .lock()
            .unwrap()
            .approvals
            .remove(approval_id)
            .ok_or_else(|| anyhow!("unknown approval {approval_id}"))?;
        let reply =
            json!({ "id": pending.rpc_id, "result": approval_response(&pending, decision) });
        self.conn.write(&reply).await
    }

    async fn interrupt(&mut self) -> anyhow::Result<()> {
        let active = self.conn.mapper.lock().unwrap().turn_id.clone();
        if let Some(turn_id) = active {
            self.conn
                .request("turn/interrupt", json!({ "threadId": self.thread_id, "turnId": turn_id }))
                .await?;
        }
        Ok(())
    }

    async fn shutdown(&mut self) -> anyhow::Result<()> {
        self.conn.closing.store(true, Ordering::Relaxed);
        // Closing stdin makes app-server exit; the thread stays on disk for `thread/resume`.
        self.conn.stdin.lock().await.take();
        if let Some(mut child) = self.child.take() {
            if tokio::time::timeout(EXIT_GRACE, child.wait()).await.is_err() {
                child.kill().await?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Mapping (pure; unit-tested against fixtures)

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovalKind {
    /// `item/commandExecution/requestApproval`
    Command,
    /// `item/fileChange/requestApproval`
    FileChange,
    /// `item/permissions/requestApproval`
    Permissions,
    /// Legacy `execCommandApproval` / `applyPatchApproval` (answered with `ReviewDecision`).
    Legacy,
}

#[derive(Debug, Clone)]
struct PendingApproval {
    rpc_id: Value,
    kind: ApprovalKind,
    params: Value,
}

#[derive(Default)]
struct Mapper {
    thread_id: Option<String>,
    /// The running turn, if any.
    turn_id: Option<String>,
    /// The last turn that finished, so a late `turn/start` response does not revive it.
    last_finished_turn: Option<String>,
    /// Tool items seen in `item/started`: item id → (tool name, input).
    tools: HashMap<String, (String, Value)>,
    /// Approvals waiting for `answer`, by approval id.
    approvals: HashMap<String, PendingApproval>,
    /// Token usage summed over this turn's model calls.
    turn_usage: (u64, u64),
    last_total_tokens: Option<u64>,
    /// An `Error` was already emitted for this turn.
    turn_errored: bool,
}

impl Mapper {
    fn turn_accepted(&mut self, turn_id: &str) {
        if self.turn_id.is_none() && self.last_finished_turn.as_deref() != Some(turn_id) {
            self.turn_id = Some(turn_id.to_string());
        }
    }

    fn on_notification(&mut self, method: &str, p: &Value) -> Vec<AgentEvent> {
        match method {
            "turn/started" => {
                if let Some(id) = p.pointer("/turn/id").and_then(Value::as_str) {
                    self.turn_id = Some(id.to_string());
                }
                Vec::new()
            }
            "turn/completed" => self.turn_completed(p),
            "item/agentMessage/delta" => match str_at(p, "/delta") {
                Some(d) if !d.is_empty() => {
                    vec![AgentEvent::AssistantDelta { text: d.to_string() }]
                }
                _ => Vec::new(),
            },
            "item/started" => self.item_started(&p["item"]),
            "item/completed" => self.item_completed(&p["item"]),
            "thread/tokenUsage/updated" => {
                let total = p.pointer("/tokenUsage/total/totalTokens").and_then(Value::as_u64);
                if total.is_none() || total != self.last_total_tokens {
                    self.last_total_tokens = total;
                    self.turn_usage.0 += u64_at(p, "/tokenUsage/last/inputTokens");
                    self.turn_usage.1 += u64_at(p, "/tokenUsage/last/outputTokens");
                }
                Vec::new()
            }
            "error" if p["willRetry"] != json!(true) => {
                self.turn_errored = true;
                let message = str_at(p, "/error/message").unwrap_or("codex error").to_string();
                vec![AgentEvent::Error { message }]
            }
            "serverRequest/resolved" => {
                let id = &p["requestId"];
                self.approvals.retain(|_, a| &a.rpc_id != id);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn turn_completed(&mut self, p: &Value) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        let outcome = match str_at(p, "/turn/status") {
            Some("interrupted") => TurnOutcome::Interrupted,
            Some("failed") => TurnOutcome::Failed,
            _ => TurnOutcome::Completed,
        };
        if outcome == TurnOutcome::Failed && !self.turn_errored {
            let message = str_at(p, "/turn/error/message").unwrap_or("turn failed").to_string();
            out.push(AgentEvent::Error { message });
        }
        let (input_tokens, output_tokens) = std::mem::take(&mut self.turn_usage);
        if input_tokens + output_tokens > 0 {
            out.push(AgentEvent::Usage { input_tokens, output_tokens });
        }
        out.push(AgentEvent::TurnEnded { outcome });
        self.last_finished_turn = str_at(p, "/turn/id").map(str::to_string).or(self.turn_id.take());
        self.turn_id = None;
        self.turn_errored = false;
        self.tools.clear();
        self.approvals.clear();
        out
    }

    fn item_started(&mut self, item: &Value) -> Vec<AgentEvent> {
        let Some((name, input)) = tool_of(item) else {
            return Vec::new();
        };
        let Some(id) = str_at(item, "/id") else {
            return Vec::new();
        };
        self.tools.insert(id.to_string(), (name.clone(), input.clone()));
        vec![AgentEvent::ToolCall { call_id: id.to_string(), name, input }]
    }

    fn item_completed(&mut self, item: &Value) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        if item["type"] == "agentMessage" {
            if let Some(text) = str_at(item, "/text").filter(|t| !t.is_empty()) {
                out.push(AgentEvent::AssistantMessage { text: text.to_string() });
            }
            return out;
        }
        let Some((name, input)) = tool_of(item) else {
            return out;
        };
        let Some(id) = str_at(item, "/id") else {
            return out;
        };
        // A tool item that completed without `item/started` still gets its call.
        if self.tools.remove(id).is_none() {
            out.push(AgentEvent::ToolCall { call_id: id.to_string(), name, input });
        }
        let (output, is_error) = tool_result(item);
        out.push(AgentEvent::ToolResult { call_id: id.to_string(), output, is_error });
        out
    }

    /// Map a server request. `Err` carries the error message to answer unsupported requests with.
    fn on_server_request(
        &mut self,
        rpc_id: Value,
        method: &str,
        p: &Value,
    ) -> Result<Vec<AgentEvent>, String> {
        let (kind, approval_id, tool, input) = match method {
            "item/commandExecution/requestApproval" => {
                let id = str_at(p, "/approvalId").or(str_at(p, "/itemId")).unwrap_or_default();
                let mut input = obj(&[("command", &p["command"]), ("cwd", &p["cwd"])]);
                add_reason(&mut input, p);
                (ApprovalKind::Command, id, "command".to_string(), input)
            }
            "item/fileChange/requestApproval" => {
                let id = str_at(p, "/itemId").unwrap_or_default();
                let (tool, mut input) = self
                    .tools
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| ("file_change".into(), json!({})));
                add_reason(&mut input, p);
                if let Some(root) = p.get("grantRoot").filter(|v| !v.is_null()) {
                    input["grantRoot"] = root.clone();
                }
                (ApprovalKind::FileChange, id, tool, input)
            }
            "item/permissions/requestApproval" => {
                let id = str_at(p, "/itemId").unwrap_or_default();
                let mut input = obj(&[("permissions", &p["permissions"]), ("cwd", &p["cwd"])]);
                add_reason(&mut input, p);
                (ApprovalKind::Permissions, id, "permissions".to_string(), input)
            }
            "execCommandApproval" => {
                let id = str_at(p, "/approvalId").or(str_at(p, "/callId")).unwrap_or_default();
                let command = match &p["command"] {
                    Value::Array(argv) => {
                        json!(argv.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "))
                    }
                    other => other.clone(),
                };
                let mut input = obj(&[("command", &command), ("cwd", &p["cwd"])]);
                add_reason(&mut input, p);
                (ApprovalKind::Legacy, id, "command".to_string(), input)
            }
            "applyPatchApproval" => {
                let id = str_at(p, "/callId").unwrap_or_default();
                let mut input = obj(&[("changes", &p["fileChanges"])]);
                add_reason(&mut input, p);
                (ApprovalKind::Legacy, id, "file_change".to_string(), input)
            }
            other => return Err(format!("ember does not handle {other}")),
        };
        let mut approval_id = approval_id.to_string();
        if approval_id.is_empty() || self.approvals.contains_key(&approval_id) {
            approval_id = format!("{approval_id}#{rpc_id}");
        }
        self.approvals
            .insert(approval_id.clone(), PendingApproval { rpc_id, kind, params: p.clone() });
        Ok(vec![AgentEvent::ApprovalRequested { approval_id, tool, input }])
    }

    /// The app-server exited without being asked to.
    fn on_exit(&mut self) -> Vec<AgentEvent> {
        let mut out = vec![AgentEvent::Error { message: "codex app-server exited".into() }];
        if self.turn_id.take().is_some() {
            out.push(AgentEvent::TurnEnded { outcome: TurnOutcome::Failed });
        }
        self.approvals.clear();
        out
    }
}

/// The result object answering an approval request.
fn approval_response(a: &PendingApproval, decision: ApprovalDecision) -> Value {
    match a.kind {
        ApprovalKind::Command | ApprovalKind::FileChange => {
            // Newer servers list what they accept; respect it when present.
            let offered = |d: &str| match a.params.get("availableDecisions") {
                Some(Value::Array(list)) => list.iter().any(|v| v == d),
                _ => true,
            };
            let decision = match decision {
                ApprovalDecision::AllowOnce => "accept",
                ApprovalDecision::AllowAlways if offered("acceptForSession") => "acceptForSession",
                ApprovalDecision::AllowAlways => "accept",
                ApprovalDecision::Deny if offered("decline") => "decline",
                ApprovalDecision::Deny => "cancel",
            };
            json!({ "decision": decision })
        }
        ApprovalKind::Permissions => match decision {
            ApprovalDecision::Deny => json!({ "permissions": {}, "scope": "turn" }),
            allow => {
                let scope = if allow == ApprovalDecision::AllowAlways { "session" } else { "turn" };
                json!({ "permissions": strip_nulls(&a.params["permissions"]), "scope": scope })
            }
        },
        ApprovalKind::Legacy => match decision {
            ApprovalDecision::AllowOnce => json!({ "decision": "approved" }),
            ApprovalDecision::AllowAlways => json!({ "decision": "approved_for_session" }),
            ApprovalDecision::Deny => {
                json!({ "decision": { "denied": { "rejection": "Denied by the user in Ember." } } })
            }
        },
    }
}

/// Tool name and input for a tool-like `ThreadItem`, or `None` for other items.
fn tool_of(item: &Value) -> Option<(String, Value)> {
    match item["type"].as_str()? {
        "commandExecution" => {
            Some(("command".into(), obj(&[("command", &item["command"]), ("cwd", &item["cwd"])])))
        }
        "fileChange" => Some(("file_change".into(), obj(&[("changes", &item["changes"])]))),
        "mcpToolCall" => Some((
            format!("mcp__{}__{}", str_at(item, "/server")?, str_at(item, "/tool")?),
            item["arguments"].clone(),
        )),
        "dynamicToolCall" => Some((str_at(item, "/tool")?.to_string(), item["arguments"].clone())),
        _ => None,
    }
}

/// Output and error flag of a completed tool item.
fn tool_result(item: &Value) -> (String, bool) {
    let status = str_at(item, "/status").unwrap_or("completed");
    match item["type"].as_str() {
        Some("commandExecution") => {
            let exit_ok = item["exitCode"].as_i64().is_none_or(|c| c == 0);
            let output = match str_at(item, "/aggregatedOutput") {
                Some(o) => o.to_string(),
                None if status == "declined" => "declined".to_string(),
                None => String::new(),
            };
            (output, status != "completed" || !exit_ok)
        }
        Some("fileChange") => {
            let lines: Vec<String> = item["changes"]
                .as_array()
                .map(|cs| {
                    cs.iter()
                        .map(|c| {
                            let kind = str_at(c, "/kind/type").unwrap_or("update");
                            format!("{kind} {}", str_at(c, "/path").unwrap_or("?"))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mut output = lines.join("\n");
            if status != "completed" {
                output = format!("{status}\n{output}").trim_end().to_string();
            }
            (output, status != "completed")
        }
        Some("mcpToolCall") => match str_at(item, "/error/message") {
            Some(message) => (message.to_string(), true),
            None => (content_text(&item["result"]["content"]), status != "completed"),
        },
        Some("dynamicToolCall") => (
            content_text(&item["contentItems"]),
            item["success"] == json!(false) || status == "failed",
        ),
        _ => (String::new(), false),
    }
}

/// Text of MCP / dynamic tool content items; non-text items as JSON.
fn content_text(content: &Value) -> String {
    content
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|c| c["text"].as_str().map(str::to_string).unwrap_or_else(|| c.to_string()))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn str_at<'a>(v: &'a Value, ptr: &str) -> Option<&'a str> {
    v.pointer(ptr).and_then(Value::as_str)
}

fn u64_at(v: &Value, ptr: &str) -> u64 {
    v.pointer(ptr).and_then(Value::as_u64).unwrap_or(0)
}

/// An object of the non-null fields given.
fn obj(fields: &[(&str, &Value)]) -> Value {
    Value::Object(
        fields
            .iter()
            .filter(|(_, v)| !v.is_null())
            .map(|(k, v)| (k.to_string(), (*v).clone()))
            .collect(),
    )
}

fn add_reason(input: &mut Value, p: &Value) {
    if let (Some(reason), Some(map)) = (str_at(p, "/reason"), input.as_object_mut()) {
        map.insert("reason".into(), json!(reason));
    }
}

fn strip_nulls(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k.clone(), strip_nulls(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One fixture line: `{"dir": "in" | "out" | "note", "msg": …}`.
    fn fixture(text: &str) -> Vec<(String, Value)> {
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let v: Value = serde_json::from_str(l).unwrap();
                (v["dir"].as_str().unwrap().to_string(), v["msg"].clone())
            })
            .collect()
    }

    const SESSION: &str = include_str!("../../tests/fixtures/codex/session.jsonl");
    const SYNTHETIC: &str = include_str!("../../tests/fixtures/codex/synthetic.jsonl");

    /// Feed every inbound notification and server request to a mapper, the way `read_loop` does.
    fn replay(m: &mut Mapper, lines: &[(String, Value)]) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        for (dir, msg) in lines {
            if dir != "in" {
                continue;
            }
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            match (msg.get("method").and_then(Value::as_str), msg.get("id")) {
                (Some(method), Some(id)) => {
                    out.extend(m.on_server_request(id.clone(), method, &params).unwrap())
                }
                (Some(method), None) => out.extend(m.on_notification(method, &params)),
                _ => {}
            }
        }
        out
    }

    #[test]
    fn real_session_maps_to_normalised_events() {
        let lines = fixture(SESSION);
        let mut m = Mapper::default();
        let events = replay(&mut m, &lines);

        let deltas: String = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::AssistantDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let rest: Vec<_> =
            events.iter().filter(|e| !matches!(e, AgentEvent::AssistantDelta { .. })).collect();

        let exec = "exec-d6b5efae-7bce-469f-923d-bc77b68f6961";
        let command = "/bin/zsh -lc \"printf 'hi' > hello.txt && cat hello.txt | tee copy.txt\"";
        let input = json!({ "command": command, "cwd": "/tmp/ember-codex-work" });
        let message = "Created `hello.txt` with `hi` and copied it to `copy.txt` using `tee`.";
        assert_eq!(
            rest,
            vec![
                &AgentEvent::ToolCall {
                    call_id: exec.into(),
                    name: "command".into(),
                    input: input.clone()
                },
                &AgentEvent::ApprovalRequested {
                    approval_id: exec.into(),
                    tool: "command".into(),
                    input
                },
                &AgentEvent::ToolResult {
                    call_id: exec.into(),
                    output: "hi".into(),
                    is_error: false
                },
                &AgentEvent::AssistantMessage { text: message.into() },
                &AgentEvent::Usage { input_tokens: 16_394 + 16_464, output_tokens: 46 + 25 },
                &AgentEvent::TurnEnded { outcome: TurnOutcome::Completed },
            ]
        );
        assert_eq!(deltas, message);
        // The approval was resolved by `serverRequest/resolved`, then the turn ended.
        assert!(m.approvals.is_empty());
        assert!(m.turn_id.is_none());
    }

    #[test]
    fn real_session_requests_match_what_the_adapter_sends() {
        let lines = fixture(SESSION);
        let out: Vec<&Value> = lines.iter().filter(|(d, _)| d == "out").map(|(_, m)| m).collect();
        // initialize params (version aside) and the approval answer are what the server accepted.
        let mut init = initialize_params();
        init["clientInfo"]["version"] = json!("0.1");
        assert_eq!(out[0]["method"], "initialize");
        assert_eq!(out[0]["params"], init);
        assert_eq!(*out[1], json!({ "method": "initialized" }));
        assert_eq!(
            out[3]["params"]["input"],
            text_input(out[3]["params"]["input"][0]["text"].as_str().unwrap())
        );

        let request = lines
            .iter()
            .find(|(_, m)| m["method"] == "item/commandExecution/requestApproval")
            .map(|(_, m)| m.clone())
            .unwrap();
        let pending = PendingApproval {
            rpc_id: request["id"].clone(),
            kind: ApprovalKind::Command,
            params: request["params"].clone(),
        };
        let answer = out.iter().find(|m| m.get("result").is_some()).unwrap();
        assert_eq!(answer["result"], approval_response(&pending, ApprovalDecision::AllowOnce));

        // thread/start and thread/resume responses carry the native session id.
        let ids: Vec<String> = lines
            .iter()
            .filter(|(d, m)| d == "in" && m.pointer("/result/thread").is_some())
            .map(|(_, m)| thread_id_of(&m["result"]).unwrap())
            .collect();
        assert_eq!(ids, vec!["01a0feb1-8ab6-7a33-ad47-036d4b6560de"; 2]);
    }

    #[test]
    fn file_change_mcp_interrupt_and_failure() {
        let lines = fixture(SYNTHETIC);
        let mut m = Mapper::default();
        let events = replay(&mut m, &lines);
        let changes = json!([{
            "path": "/tmp/w/hello.txt",
            "kind": { "type": "update", "move_path": null },
            "diff": "-hi\n+hello\n",
        }]);
        assert_eq!(
            events,
            vec![
                AgentEvent::ToolCall {
                    call_id: "call_fc1".into(),
                    name: "file_change".into(),
                    input: json!({ "changes": changes }),
                },
                AgentEvent::ApprovalRequested {
                    approval_id: "call_fc1".into(),
                    tool: "file_change".into(),
                    input: json!({ "changes": changes }),
                },
                AgentEvent::ToolResult {
                    call_id: "call_fc1".into(),
                    output: "update /tmp/w/hello.txt".into(),
                    is_error: false,
                },
                AgentEvent::ToolCall {
                    call_id: "call_m1".into(),
                    name: "mcp__docs__search".into(),
                    input: json!({ "q": "x" }),
                },
                AgentEvent::ToolResult {
                    call_id: "call_m1".into(),
                    output: "found".into(),
                    is_error: false
                },
                AgentEvent::TurnEnded { outcome: TurnOutcome::Interrupted },
                AgentEvent::Error { message: "usage limit reached".into() },
                AgentEvent::TurnEnded { outcome: TurnOutcome::Failed },
            ]
        );
    }

    #[test]
    fn approval_decisions_map_to_codex_responses() {
        let pending = |kind, params: Value| PendingApproval { rpc_id: json!(1), kind, params };
        let cmd = pending(ApprovalKind::Command, json!({}));
        assert_eq!(
            approval_response(&cmd, ApprovalDecision::AllowOnce),
            json!({"decision": "accept"})
        );
        assert_eq!(
            approval_response(&cmd, ApprovalDecision::AllowAlways),
            json!({"decision": "acceptForSession"})
        );
        assert_eq!(approval_response(&cmd, ApprovalDecision::Deny), json!({"decision": "decline"}));

        // As seen live: the server offered only accept / execpolicy amendment / cancel.
        let narrow = pending(
            ApprovalKind::Command,
            json!({ "availableDecisions": ["accept", {"acceptWithExecpolicyAmendment": {}}, "cancel"] }),
        );
        assert_eq!(
            approval_response(&narrow, ApprovalDecision::AllowAlways),
            json!({"decision": "accept"})
        );
        assert_eq!(
            approval_response(&narrow, ApprovalDecision::Deny),
            json!({"decision": "cancel"})
        );

        let file = pending(ApprovalKind::FileChange, json!({}));
        assert_eq!(
            approval_response(&file, ApprovalDecision::AllowAlways),
            json!({"decision": "acceptForSession"})
        );

        let perms = pending(
            ApprovalKind::Permissions,
            json!({ "permissions": { "network": { "enabled": true }, "fileSystem": null } }),
        );
        assert_eq!(
            approval_response(&perms, ApprovalDecision::AllowAlways),
            json!({ "permissions": { "network": { "enabled": true } }, "scope": "session" })
        );
        assert_eq!(
            approval_response(&perms, ApprovalDecision::Deny),
            json!({ "permissions": {}, "scope": "turn" })
        );

        let legacy = pending(ApprovalKind::Legacy, json!({}));
        assert_eq!(
            approval_response(&legacy, ApprovalDecision::AllowOnce),
            json!({"decision": "approved"})
        );
        assert_eq!(
            approval_response(&legacy, ApprovalDecision::AllowAlways),
            json!({"decision": "approved_for_session"})
        );
        assert!(
            approval_response(&legacy, ApprovalDecision::Deny)["decision"]["denied"].is_object()
        );
    }

    #[test]
    fn unknown_traffic_is_ignored_or_refused() {
        let mut m = Mapper::default();
        assert!(m.on_notification("something/new", &json!({"x": 1})).is_empty());
        assert!(m.on_server_request(json!(3), "item/tool/requestUserInput", &json!({})).is_err());
        assert!(m.approvals.is_empty());
    }

    #[test]
    fn late_turn_start_response_does_not_revive_a_finished_turn() {
        let mut m = Mapper::default();
        m.on_notification("turn/started", &json!({ "turn": { "id": "t1" } }));
        m.on_notification(
            "turn/completed",
            &json!({ "turn": { "id": "t1", "status": "completed" } }),
        );
        m.turn_accepted("t1");
        assert!(m.turn_id.is_none());
        m.turn_accepted("t2");
        assert_eq!(m.turn_id.as_deref(), Some("t2"));
    }

    #[test]
    fn exit_mid_turn_fails_the_turn() {
        let mut m = Mapper::default();
        m.on_notification("turn/started", &json!({ "turn": { "id": "t1" } }));
        assert_eq!(
            m.on_exit(),
            vec![
                AgentEvent::Error { message: "codex app-server exited".into() },
                AgentEvent::TurnEnded { outcome: TurnOutcome::Failed },
            ]
        );
    }

    #[test]
    fn version_is_parsed() {
        assert_eq!(parse_version("codex-cli 0.155.1\n").as_deref(), Some("0.155.1"));
        assert_eq!(parse_version(""), None);
    }
}
