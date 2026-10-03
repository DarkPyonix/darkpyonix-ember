//! Antigravity adapter (SPEC FR-A1, FR-A2, FR-A5).
//!
//! Drives the unmodified `agy` CLI in its headless print mode, **one process per turn** (FR-A2):
//!
//! ```text
//! agy -p <message> --output-format stream-json --add-dir <session root> \
//!     --dangerously-skip-permissions [--conversation=<id>] [--model <M>]
//! ```
//!
//! # Protocol sources
//!
//! Verified against agy 1.2.10 (`agy --help`, `agy mcp add --help`, the documentation and release
//! notes embedded in the binary) and two recorded real turns in `tests/fixtures/agy/` (the second
//! one resumed with `--conversation`; local paths scrubbed):
//!
//! * `agy --version` prints `1.2.10`.
//! * stdout, one JSON object per line (recorded):
//!   - `{"event":"init","conversation_id":"<uuid>","init":{"model","cwd","tools":[…],
//!     "permission_mode":"request-review"}}` — the conversation id is the native session id.
//!   - `{"event":"step_update","step_update":{"conversation_id","step_index":N,"state":"ACTIVE"|
//!     "DONE","step_type":"user_input"|"system_message"|"agent_response"|"tool",
//!     "duration_seconds","usage":{"input_tokens","output_tokens","thinking_tokens",
//!     "cache_read_tokens","total_tokens"},"tool_name","tool_info":{"name","parameters":{…}}}}`.
//!     A tool step is announced `ACTIVE` and again `DONE`; the stream carries **no tool output**.
//!     `usage` is on `agent_response` steps and covers that model call only.
//!   - `{"event":"result","result":{"conversation_id","status":"SUCCESS","response":"<final
//!     text>","duration_seconds","num_turns","usage":{…},"denied_actions":[{"action":"command",
//!     "display_name":"RunCommand"}]}}`. `result.usage` and `num_turns` cover the **whole
//!     conversation** (the resumed recording reports both turns), so per-turn usage is summed from
//!     the steps instead.
//! * `--conversation <id>` continues that conversation (recorded: same id, step indices continue,
//!   the model remembered the first turn's request).
//! * `step_update.text_delta` (assistant text deltas) is **not recorded**: the field name comes from
//!   the binary's JSON tags and the release note "streamed text … in `--output-format stream-json`
//!   text deltas". Both recorded turns ended before the model wrote any text.
//! * stderr: `AGY_ERROR: {…}` on an agent or model API failure (exit code 3) and lines with an
//!   `error:` marker (release notes; not recorded). A soft-denied tool prints a `jetski: …` notice
//!   (recorded, `tests/fixtures/agy/turn1_stderr.txt`).
//!
//! # Approvals (FR-A5)
//!
//! Print mode cannot ask for a permission: a tool that needs one is **soft-denied** and listed in
//! `denied_actions` (recorded). A `PreToolUse` hook answering `{"decision":"allow"}`, even with
//! `permissionOverrides: ["command(<the command>)"]`, did **not** lift that denial (recorded with
//! 1.2.10, `turn2_resumed.jsonl`). So the adapter runs agy with `--dangerously-skip-permissions`
//! and makes Ember's own `PreToolUse` hook the only gate: agy's hook contract says `"deny"`
//! "hard-blocks the execution immediately", and hooks run synchronously before every tool.
//!
//! The hook is configured through agy's own customization mechanism, not by patching agy: every
//! session gets a private directory (the *session root*, under `<data dir>/agy/sessions/`) that is
//! passed with `--add-dir`. A workspace's `.agents/` directory is a customization root (embedded
//! docs: "Customization Roots", `hooks.json`); recorded: `.agents/hooks.json` in an `--add-dir`
//! directory is loaded in print mode without the folder being trusted, the hook's working
//! directory is that `.agents/` directory, its stdin is the documented camelCase payload
//! (`tests/fixtures/agy/pre_tool_use.json`) and it inherits agy's environment (plus
//! `ANTIGRAVITY_CONVERSATION_ID`).
//!
//! The session root holds:
//! - `.agents/hooks.json`: one named hook, [`HOOK_NAME`], `PreToolUse` with matcher `*`, running
//!   `.agents/ember-hook.sh` with a [`HOOK_TIMEOUT_SECS`] timeout;
//! - `.agents/ember-hook.sh` (mode 0700): `curl` POSTs the payload to [`HOOK_ROUTE`] on this
//!   server with this run's secret (`Authorization: Bearer`) and prints the reply; if the server
//!   cannot be reached it prints `deny` (fail closed). The secret lives in this file, not in the
//!   environment, so the agent's own shell commands do not inherit it;
//! - `.agents/rules/ember.md`: [`StartRequest::instructions`] (A2A, the session's computer, …)
//!   plus a note that the session root is not the project. Rules are Markdown files under
//!   `rules/` of a customization root (embedded docs);
//! - `.agents/mcp_config.json`: [`StartRequest::mcp_servers`] as `{"mcpServers": {name:
//!   {"command","args"}}}` (embedded docs, "Configuration Schema"). The user's own servers in
//!   `~/.gemini/config/mcp_config.json` stay.
//!
//! The route ([`router`], served by [`HookBroker`]) blocks until the user answers or
//! [`APPROVAL_TIMEOUT`] passes (then `deny`). Read-only tools ([`READ_ONLY_TOOLS`]) are allowed
//! without asking, as agy itself does for workspace reads in its default mode; every other tool,
//! including tools this list has never heard of, asks. `AllowAlways` allows that tool name for
//! the rest of the process. `approval_id` and `ToolCall.call_id` are both
//! `<conversation id>:<step index>`.
//!
//! Before its first turn a run checks that agy actually loaded the hook (`agy -p /hooks`, a
//! read-only print-mode slash command that starts no turn, per the release notes) and refuses to
//! run with permissions skipped if [`HOOK_NAME`] is not listed. `EMBER_AGY_HOOK_CHECK=0` skips
//! the check.
//!
//! The session root appears to the model as an extra workspace, listed first (recorded:
//! `workspacePaths` puts `--add-dir` folders before the cwd, and the model picked it as the
//! command's `Cwd`), which is why the rules file tells the agent to stay out of it. Roots are
//! reused per conversation (`<data dir>/agy/conversations/<id>` names the root), because a
//! resumed conversation keeps every folder ever added (recorded).
//!
//! # Not supported yet
//!
//! Accounts (agy has no configuration-directory variable: `~/.gemini` is fixed), sessions on
//! another computer, tool output (not in the stream; the conversation's `transcript_full.jsonl`
//! has it), and steering a running turn.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};

use super::{AgentAdapter, AgentKind, AgentRun, Detected, McpServer, StartRequest};
use crate::events::{AgentEvent, ApprovalDecision, TurnOutcome};

/// The route agy's `PreToolUse` hook calls (local listener only).
pub const HOOK_ROUTE: &str = "/api/v1/agents/antigravity/hook";
/// Name of Ember's hook in `hooks.json`; also what the pre-flight check looks for.
pub const HOOK_NAME: &str = "ember-approvals";
/// agy's own timeout for the hook command, in seconds (agy's default is 30).
pub const HOOK_TIMEOUT_SECS: u64 = 1800;
/// How long the hook route waits for the user before answering `deny`. Shorter than the hook's
/// timeouts, so agy always gets an answer from Ember rather than a killed hook.
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(HOOK_TIMEOUT_SECS - 60);
/// After SIGINT, how long an interrupted or shut-down turn may take before SIGKILL.
const INTERRUPT_GRACE: Duration = Duration::from_secs(5);
/// How long a new turn waits for the previous process to exit (agy waits for background tasks
/// after its final answer) before killing it.
const PREVIOUS_EXIT_GRACE: Duration = Duration::from_secs(30);

/// Tools that only read, allowed without asking. Names are agy's (the `init` event's `tools`,
/// recorded with 1.2.10); anything else asks.
pub const READ_ONLY_TOOLS: &[&str] = &[
    "view_file",
    "list_dir",
    "find_by_name",
    "grep_search",
    "command_status",
    "list_resources",
    "read_resource",
    "list_permissions",
    "list_browser_pages",
    "finish",
    "wait",
    "wait_5_seconds",
];

pub struct AntigravityAdapter {
    bin: PathBuf,
    /// `<data dir>/agy`: session roots and the conversation → root index.
    home: PathBuf,
    hooks: Arc<HookBroker>,
}

impl AntigravityAdapter {
    pub fn new(bin: impl Into<PathBuf>, home: impl Into<PathBuf>, hooks: Arc<HookBroker>) -> Self {
        AntigravityAdapter { bin: bin.into(), home: home.into(), hooks }
    }

    /// The binary from `EMBER_AGY_BIN`, else `agy` on `PATH`; session roots under
    /// `<data_dir>/agy`.
    pub fn from_env(data_dir: &Path, hooks: Arc<HookBroker>) -> Self {
        Self::new(
            std::env::var_os("EMBER_AGY_BIN").unwrap_or_else(|| "agy".into()),
            data_dir.join("agy"),
            hooks,
        )
    }

    /// Arguments for one turn.
    pub fn turn_args(
        prompt: &str,
        conversation: Option<&str>,
        model: Option<&str>,
        root: &Path,
    ) -> Vec<String> {
        // `-p <prompt>` is the recorded form. A leading "-" could be read as a flag.
        let prompt = if prompt.starts_with('-') { format!(" {prompt}") } else { prompt.to_string() };
        let mut args: Vec<String> = vec![
            "-p".into(),
            prompt,
            "--output-format".into(),
            "stream-json".into(),
            "--add-dir".into(),
            root.to_string_lossy().into_owned(),
            // Ember's PreToolUse hook is the gate; see the module docs.
            "--dangerously-skip-permissions".into(),
        ];
        if let Some(id) = conversation {
            // Equals form, so an id can never be read as a flag.
            args.push(format!("--conversation={id}"));
        }
        if let Some(model) = model {
            args.push("--model".into());
            args.push(model.to_string());
        }
        args
    }
}

#[async_trait]
impl AgentAdapter for AntigravityAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Antigravity
    }

    async fn detect(&self) -> Detected {
        let out = Command::new(&self.bin)
            .arg("--version")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output();
        match tokio::time::timeout(Duration::from_secs(15), out).await {
            Ok(Ok(o)) if o.status.success() => Detected {
                kind: AgentKind::Antigravity,
                installed: true,
                // "1.2.10"
                version: String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .next()
                    .map(String::from),
            },
            _ => Detected { kind: AgentKind::Antigravity, installed: false, version: None },
        }
    }

    async fn start(
        &self,
        req: StartRequest,
        events: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<Box<dyn AgentRun>> {
        anyhow::ensure!(
            req.remote.is_none(),
            "Antigravity sessions cannot run on another computer yet"
        );
        anyhow::ensure!(
            req.cwd.is_dir(),
            "working directory {} does not exist on the ember server",
            req.cwd.display()
        );
        let hook_url = self.hooks.hook_url().ok_or_else(|| {
            anyhow::anyhow!("the Antigravity approval hook has no server URL (HookBroker::set_base_url)")
        })?;
        let (root, new_root) = session_root(&self.home, req.resume_native_id.as_deref());
        let token = uuid::Uuid::new_v4().simple().to_string();
        write_session_root(&root, &hook_url, &token, &req)?;
        let approvals = Arc::new(RunApprovals::new(events.clone(), APPROVAL_TIMEOUT));
        self.hooks.register(&token, approvals.clone());
        Ok(Box::new(AgyRun {
            bin: self.bin.clone(),
            home: self.home.clone(),
            root,
            new_root,
            req,
            events,
            hooks: self.hooks.clone(),
            token,
            approvals,
            state: Arc::new(StdMutex::new(TurnState::default())),
            turn_task: None,
            hook_checked: false,
        }))
    }
}

// ---------------------------------------------------------------------------------------------
// Session root

/// Ids used as file names: agy's conversation ids are UUIDs.
fn safe_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The session root for a run: the resumed conversation's own root when known, else a new one.
/// Returns whether the root is new (its conversation id is recorded once known).
fn session_root(home: &Path, resume: Option<&str>) -> (PathBuf, bool) {
    if let Some(id) = resume.filter(|id| safe_name(id)) {
        if let Ok(name) = std::fs::read_to_string(home.join("conversations").join(id)) {
            let name = name.trim();
            if safe_name(name) {
                return (home.join("sessions").join(name), false);
            }
        }
    }
    (home.join("sessions").join(uuid::Uuid::new_v4().to_string()), true)
}

/// Record that `conversation` uses `root`, so a resume adds the same folder again.
fn remember_root(home: &Path, conversation: &str, root: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(safe_name(conversation), "unexpected conversation id {conversation:?}");
    let name = root.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let dir = home.join("conversations");
    make_private_dir(&dir)?;
    std::fs::write(dir.join(conversation), name)?;
    Ok(())
}

fn make_private_dir(dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Single-quote `s` for `sh`.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `hooks.json`: one named hook, `PreToolUse` on every tool, running `script`.
pub fn hooks_json(script: &Path) -> Value {
    json!({
        HOOK_NAME: {
            "PreToolUse": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": format!("sh {}", shell_quote(&script.to_string_lossy())),
                    "timeout": HOOK_TIMEOUT_SECS,
                }],
            }],
        },
    })
}

/// The hook script: forwards the payload to Ember and prints its answer; denies when Ember
/// cannot be reached.
pub fn hook_script(url: &str, token: &str) -> String {
    let deny = json!({
        "decision": "deny",
        "reason": "Ember could not be reached to approve this tool call.",
    })
    .to_string();
    format!(
        "#!/bin/sh\n\
         # Written by Ember for one Antigravity session (SPEC FR-A5): agy's PreToolUse hook.\n\
         # Asks the Ember server, which waits for the user's answer; denies if it cannot.\n\
         curl -sS --fail --max-time {max} -X POST \\\n  \
         -H 'Content-Type: application/json' \\\n  \
         -H {auth} \\\n  \
         --data-binary @- {url} 2>/dev/null || printf '%s\\n' {deny}\n",
        max = HOOK_TIMEOUT_SECS - 10,
        auth = shell_quote(&format!("Authorization: Bearer {token}")),
        url = shell_quote(url),
        deny = shell_quote(&deny),
    )
}

/// `mcp_config.json` for `servers`, or `None` when there are none.
pub fn mcp_config(servers: &[McpServer]) -> Option<Value> {
    if servers.is_empty() {
        return None;
    }
    let map: serde_json::Map<String, Value> = servers
        .iter()
        .map(|s| (s.name.clone(), json!({ "command": s.command, "args": s.args })))
        .collect();
    Some(json!({ "mcpServers": map }))
}

/// The rules file: where the project is, and the session's instructions.
pub fn rules_text(root: &Path, cwd: &Path, instructions: Option<&str>) -> String {
    let mut text = format!(
        "# Ember\n\n\
         This conversation runs in Ember. The project is `{cwd}`: work there. The folder `{root}` \
         only holds Ember's configuration for this conversation; never read, write or run \
         commands in it, and never use it as a working directory.\n",
        cwd = cwd.display(),
        root = root.display(),
    );
    if let Some(extra) = instructions.filter(|s| !s.trim().is_empty()) {
        text.push('\n');
        text.push_str(extra.trim_end());
        text.push('\n');
    }
    text
}

/// (Re)write everything in the session root for this run.
fn write_session_root(root: &Path, hook_url: &str, token: &str, req: &StartRequest) -> anyhow::Result<()> {
    let agents = root.join(".agents");
    make_private_dir(root)?;
    make_private_dir(&agents)?;
    make_private_dir(&agents.join("rules"))?;
    let script = agents.join("ember-hook.sh");
    std::fs::write(&script, hook_script(hook_url, token))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;
    }
    std::fs::write(agents.join("hooks.json"), serde_json::to_vec_pretty(&hooks_json(&script))?)?;
    std::fs::write(
        agents.join("rules").join("ember.md"),
        rules_text(root, &req.cwd, req.instructions.as_deref()),
    )?;
    let mcp = agents.join("mcp_config.json");
    match mcp_config(&req.mcp_servers) {
        Some(cfg) => std::fs::write(&mcp, serde_json::to_vec_pretty(&cfg)?)?,
        None => {
            let _ = std::fs::remove_file(&mcp);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Approvals: the hook route

/// Routes hook calls to the run that owns their secret. Shared by the adapter and [`router`].
#[derive(Default)]
pub struct HookBroker {
    url: OnceLock<String>,
    runs: StdMutex<HashMap<String, Arc<RunApprovals>>>,
}

impl HookBroker {
    pub fn new() -> Arc<HookBroker> {
        Arc::new(HookBroker::default())
    }

    /// The server's own base URL as seen from this machine (e.g. `http://127.0.0.1:8740`). Set
    /// once, before any Antigravity session starts.
    pub fn set_base_url(&self, base: &str) {
        let _ = self.url.set(format!("{}{HOOK_ROUTE}", base.trim_end_matches('/')));
    }

    pub fn hook_url(&self) -> Option<String> {
        self.url.get().cloned()
    }

    fn register(&self, token: &str, run: Arc<RunApprovals>) {
        self.runs.lock().unwrap().insert(token.to_string(), run);
    }

    fn unregister(&self, token: &str) {
        if let Some(run) = self.runs.lock().unwrap().remove(token) {
            run.cancel_all();
        }
    }

    /// Answer one hook call. `None` when `token` belongs to no running session.
    pub async fn handle(&self, token: &str, payload: &Value) -> Option<Value> {
        let run = self.runs.lock().unwrap().get(token).cloned()?;
        Some(run.decide(payload).await)
    }
}

/// `POST HOOK_ROUTE` with `Authorization: Bearer <secret>` and agy's hook payload; replies with
/// agy's hook output (`{"decision": "allow" | "deny", "reason"?}`). Serve it on the local
/// listener only: agy runs on this machine.
pub fn router(broker: Arc<HookBroker>) -> Router {
    Router::new().route(HOOK_ROUTE, axum::routing::post(hook_call)).with_state(broker)
}

async fn hook_call(
    State(broker): State<Arc<HookBroker>>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    match broker.handle(token, &payload).await {
        Some(reply) => Json(reply).into_response(),
        None => (StatusCode::UNAUTHORIZED, Json(json!({ "error": "unknown session", "code": "unauthorized" })))
            .into_response(),
    }
}

/// One tool call as agy's hook reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct HookCall {
    pub approval_id: String,
    pub tool: String,
    pub input: Value,
}

impl HookCall {
    /// From the `PreToolUse` payload: `{conversationId, stepIdx, toolCall: {name, args}, …}`.
    pub fn from_payload(v: &Value) -> HookCall {
        let conv = v["conversationId"].as_str().unwrap_or_default();
        let approval_id = match v["stepIdx"].as_u64() {
            Some(step) if !conv.is_empty() => call_id(conv, step),
            _ => format!("agy-{}", uuid::Uuid::new_v4().simple()),
        };
        HookCall {
            approval_id,
            tool: v["toolCall"]["name"].as_str().unwrap_or_default().to_string(),
            input: v["toolCall"]["args"].clone(),
        }
    }
}

/// `<conversation id>:<step index>`: both the tool call id and the approval id.
pub fn call_id(conversation: &str, step: u64) -> String {
    format!("{conversation}:{step}")
}

pub fn allow_reply() -> Value {
    json!({ "decision": "allow" })
}

pub fn deny_reply(reason: &str) -> Value {
    json!({ "decision": "deny", "reason": reason })
}

/// Pending approvals of one run.
struct RunApprovals {
    events: mpsc::Sender<AgentEvent>,
    timeout: Duration,
    state: StdMutex<ApprovalState>,
}

#[derive(Default)]
struct ApprovalState {
    /// approval_id -> (tool, answer channel)
    pending: HashMap<String, (String, oneshot::Sender<ApprovalDecision>)>,
    always_allowed: HashSet<String>,
}

impl RunApprovals {
    fn new(events: mpsc::Sender<AgentEvent>, timeout: Duration) -> RunApprovals {
        RunApprovals { events, timeout, state: StdMutex::new(ApprovalState::default()) }
    }

    async fn decide(&self, payload: &Value) -> Value {
        let call = HookCall::from_payload(payload);
        if READ_ONLY_TOOLS.contains(&call.tool.as_str())
            || self.state.lock().unwrap().always_allowed.contains(&call.tool)
        {
            return allow_reply();
        }
        let (tx, rx) = oneshot::channel();
        self.state.lock().unwrap().pending.insert(call.approval_id.clone(), (call.tool.clone(), tx));
        let asked = self
            .events
            .send(AgentEvent::ApprovalRequested {
                approval_id: call.approval_id.clone(),
                tool: call.tool.clone(),
                input: call.input.clone(),
            })
            .await;
        if asked.is_err() {
            self.state.lock().unwrap().pending.remove(&call.approval_id);
            return deny_reply("The Ember session is gone.");
        }
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(ApprovalDecision::AllowOnce | ApprovalDecision::AllowAlways)) => allow_reply(),
            Ok(Ok(ApprovalDecision::Deny)) => {
                deny_reply(&format!("The user denied {} in Ember.", call.tool))
            }
            // Withdrawn: the turn was interrupted or the run shut down.
            Ok(Err(_)) => deny_reply("The turn was stopped in Ember."),
            Err(_) => {
                self.state.lock().unwrap().pending.remove(&call.approval_id);
                let _ = self
                    .events
                    .send(AgentEvent::ApprovalResolved {
                        approval_id: call.approval_id,
                        decision: ApprovalDecision::Deny,
                    })
                    .await;
                deny_reply("Nobody answered in Ember in time.")
            }
        }
    }

    fn answer(&self, approval_id: &str, decision: ApprovalDecision) -> anyhow::Result<()> {
        let mut st = self.state.lock().unwrap();
        let (tool, tx) = st
            .pending
            .remove(approval_id)
            .ok_or_else(|| anyhow::anyhow!("unknown approval {approval_id}"))?;
        if decision == ApprovalDecision::AllowAlways {
            st.always_allowed.insert(tool);
        }
        // The hook may have given up already; nothing to do then.
        let _ = tx.send(decision);
        Ok(())
    }

    /// Withdraw every pending approval (each hook answers `deny`).
    fn cancel_all(&self) {
        self.state.lock().unwrap().pending.clear();
    }
}

// ---------------------------------------------------------------------------------------------
// Running turns

#[derive(Default)]
struct TurnState {
    /// A turn's process is running and its `TurnEnded` has not been sent.
    active: bool,
    /// Bumped per turn, so a stale kill timer does nothing.
    seq: u64,
    pid: Option<u32>,
    interrupted: bool,
    shutting_down: bool,
    /// The conversation id, once agy announced it (or the resumed one).
    native_id: Option<String>,
}

struct AgyRun {
    bin: PathBuf,
    home: PathBuf,
    root: PathBuf,
    new_root: bool,
    req: StartRequest,
    events: mpsc::Sender<AgentEvent>,
    hooks: Arc<HookBroker>,
    token: String,
    approvals: Arc<RunApprovals>,
    state: Arc<StdMutex<TurnState>>,
    /// The last turn's task; it ends when that process has exited.
    turn_task: Option<tokio::task::JoinHandle<()>>,
    hook_checked: bool,
}

impl AgyRun {
    /// End a turn that could not start: `Error` then `TurnEnded { Failed }`.
    async fn fail(&self, message: String) -> anyhow::Result<()> {
        self.events.send(AgentEvent::Error { message }).await?;
        self.events.send(AgentEvent::TurnEnded { outcome: TurnOutcome::Failed }).await?;
        Ok(())
    }

    /// Does agy list Ember's hook for this cwd and session root? (`agy -p /hooks`)
    async fn check_hook(&self) -> anyhow::Result<()> {
        if std::env::var("EMBER_AGY_HOOK_CHECK").as_deref() == Ok("0") {
            return Ok(());
        }
        let out = Command::new(&self.bin)
            .args(["-p", "/hooks", "--add-dir"])
            .arg(&self.root)
            .envs(self.req.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&self.req.cwd)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output();
        let out = tokio::time::timeout(Duration::from_secs(60), out)
            .await
            .map_err(|_| anyhow::anyhow!("`agy -p /hooks` timed out"))??;
        let listing = String::from_utf8_lossy(&out.stdout);
        anyhow::ensure!(
            listing.contains(HOOK_NAME),
            "Antigravity did not load Ember's approval hook ({HOOK_NAME} in {}), so its tools \
             cannot be gated; refusing to run. `agy -p /hooks` printed: {}",
            self.root.join(".agents/hooks.json").display(),
            listing.trim()
        );
        Ok(())
    }
}

#[async_trait]
impl AgentRun for AgyRun {
    async fn send(&mut self, text: &str) -> anyhow::Result<()> {
        // The previous turn's process may still be finishing background work.
        if let Some(mut prev) = self.turn_task.take() {
            if tokio::time::timeout(PREVIOUS_EXIT_GRACE, &mut prev).await.is_err() {
                let pid = self.state.lock().unwrap().pid;
                if let Some(pid) = pid {
                    tracing::warn!(target: "ember::agy", pid, "previous turn did not exit; killing");
                    signal(pid, Signal::Kill);
                }
                let _ = prev.await;
            }
        }
        anyhow::ensure!(!self.state.lock().unwrap().active, "a turn is already running");
        if !self.hook_checked {
            if let Err(e) = self.check_hook().await {
                return self.fail(format!("{e:#}")).await;
            }
            self.hook_checked = true;
        }
        let conversation = {
            let st = self.state.lock().unwrap();
            st.native_id.clone().or_else(|| self.req.resume_native_id.clone())
        };
        let args = AntigravityAdapter::turn_args(
            text,
            conversation.as_deref(),
            self.req.model.as_deref(),
            &self.root,
        );
        let spawned = Command::new(&self.bin)
            .envs(self.req.env.iter().map(|(k, v)| (k, v)))
            .args(&args)
            .current_dir(&self.req.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => return self.fail(format!("failed to start {}: {e}", self.bin.display())).await,
        };
        let seq = {
            let mut st = self.state.lock().unwrap();
            st.active = true;
            st.seq += 1;
            st.interrupted = false;
            st.pid = child.id();
            st.seq
        };
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let turn = Turn {
            events: self.events.clone(),
            state: self.state.clone(),
            approvals: self.approvals.clone(),
            home: self.home.clone(),
            root: self.new_root.then(|| self.root.clone()),
            seq,
        };
        self.turn_task = Some(tokio::spawn(turn.run(child, stdout, stderr)));
        Ok(())
    }

    async fn answer(
        &mut self,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> anyhow::Result<()> {
        self.approvals.answer(approval_id, decision)
    }

    async fn interrupt(&mut self) -> anyhow::Result<()> {
        let (pid, seq) = {
            let mut st = self.state.lock().unwrap();
            if !st.active {
                return Ok(());
            }
            st.interrupted = true;
            (st.pid, st.seq)
        };
        // A hook waiting on the user would hold agy up; it now answers `deny`.
        self.approvals.cancel_all();
        if let Some(pid) = pid {
            signal(pid, Signal::Int);
            kill_later(self.state.clone(), pid, seq);
        }
        Ok(())
    }

    async fn shutdown(&mut self) -> anyhow::Result<()> {
        let pid = {
            let mut st = self.state.lock().unwrap();
            st.shutting_down = true;
            st.pid
        };
        self.hooks.unregister(&self.token);
        if let Some(mut task) = self.turn_task.take() {
            if let Some(pid) = pid {
                signal(pid, Signal::Int);
            }
            if tokio::time::timeout(INTERRUPT_GRACE, &mut task).await.is_err() {
                if let Some(pid) = pid {
                    tracing::warn!(target: "ember::agy", pid, "agy did not exit; killing");
                    signal(pid, Signal::Kill);
                }
                let _ = task.await;
            }
        }
        Ok(())
    }
}

impl Drop for AgyRun {
    fn drop(&mut self) {
        self.hooks.unregister(&self.token);
    }
}

/// One turn's process: translate its output, then report how the turn ended.
struct Turn {
    events: mpsc::Sender<AgentEvent>,
    state: Arc<StdMutex<TurnState>>,
    approvals: Arc<RunApprovals>,
    home: PathBuf,
    /// The session root, while its conversation id still has to be recorded.
    root: Option<PathBuf>,
    seq: u64,
}

impl Turn {
    async fn run(
        mut self,
        mut child: tokio::process::Child,
        stdout: tokio::process::ChildStdout,
        stderr: tokio::process::ChildStderr,
    ) {
        let pid = child.id();
        let stderr_task = tokio::spawn(async move {
            let mut errors = StderrErrors::default();
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!(target: "ember::agy", pid, "stderr: {line}");
                errors.push(&line);
            }
            errors
        });

        let mut parser = LineParser::default();
        let mut end: Option<Option<String>> = None;
        let mut lines = BufReader::new(stdout).lines();
        loop {
            let line = match lines.next_line().await {
                Ok(Some(l)) => l,
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!(target: "ember::agy", "reading stdout failed: {e}");
                    break;
                }
            };
            tracing::trace!(target: "ember::agy", "stdout: {line}");
            for out in parser.parse(&line) {
                match out {
                    Parsed::Event(event) => {
                        if let AgentEvent::NativeSession { native_id } = &event {
                            self.state.lock().unwrap().native_id = Some(native_id.clone());
                            if let Some(root) = self.root.take() {
                                if let Err(e) = remember_root(&self.home, native_id, &root) {
                                    tracing::warn!(target: "ember::agy", "recording the session root failed: {e:#}");
                                }
                            }
                        }
                        if self.events.send(event).await.is_err() {
                            return;
                        }
                    }
                    Parsed::TurnEnd { error } => {
                        // Reported now: agy may keep running for its background tasks.
                        self.finish(Some(error.clone()), None).await;
                        end = Some(error);
                    }
                }
            }
        }
        let errors = stderr_task.await.unwrap_or_default();
        let status = child.wait().await;
        if end.is_none() {
            let reason = errors.message().unwrap_or_else(|| match &status {
                Ok(s) => format!("agy exited ({s}) without a result"),
                Err(e) => format!("agy failed: {e}"),
            });
            if let Some(limit) = errors.rate_limit() {
                let _ = self.events.send(limit).await;
            }
            self.finish(None, Some(reason)).await;
        }
        let mut st = self.state.lock().unwrap();
        if st.seq == self.seq {
            st.pid = None;
        }
    }

    /// Send the turn's end once. `result` is the parsed `result` (its error, if any); `exit` the
    /// reason when the process ended without one.
    async fn finish(&self, result: Option<Option<String>>, exit: Option<String>) {
        let (interrupted, shutting_down) = {
            let mut st = self.state.lock().unwrap();
            if !st.active || st.seq != self.seq {
                return;
            }
            st.active = false;
            (st.interrupted, st.shutting_down)
        };
        self.approvals.cancel_all();
        if shutting_down && !interrupted {
            return;
        }
        let error = match (result, exit) {
            (Some(error), _) => error,
            (None, reason) => reason,
        };
        let outcome = match error {
            _ if interrupted => TurnOutcome::Interrupted,
            Some(message) => {
                let _ = self.events.send(AgentEvent::Error { message }).await;
                TurnOutcome::Failed
            }
            None => TurnOutcome::Completed,
        };
        let _ = self.events.send(AgentEvent::TurnEnded { outcome }).await;
    }
}

#[derive(Clone, Copy)]
enum Signal {
    Int,
    Kill,
}

#[cfg(unix)]
fn signal(pid: u32, sig: Signal) {
    let sig = match sig {
        Signal::Int => libc::SIGINT,
        Signal::Kill => libc::SIGKILL,
    };
    // SAFETY: plain kill(2) on a pid we spawned.
    unsafe {
        libc::kill(pid as libc::pid_t, sig);
    }
}

#[cfg(not(unix))]
fn signal(_pid: u32, _sig: Signal) {}

/// SIGKILL `pid` if the turn `seq` is still running after the grace period.
fn kill_later(state: Arc<StdMutex<TurnState>>, pid: u32, seq: u64) {
    tokio::spawn(async move {
        tokio::time::sleep(INTERRUPT_GRACE).await;
        let running = {
            let st = state.lock().unwrap();
            st.seq == seq && st.pid == Some(pid)
        };
        if running {
            tracing::warn!(target: "ember::agy", pid, "agy ignored SIGINT; killing");
            signal(pid, Signal::Kill);
        }
    });
}

// ---------------------------------------------------------------------------------------------
// Parsing

/// One translated stdout line.
#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Event(AgentEvent),
    /// `result`: the turn ended, with an error unless agy reported `SUCCESS`.
    TurnEnd { error: Option<String> },
}

/// Translates one turn's `--output-format stream-json` lines. Unknown or malformed lines yield
/// nothing. Use a new parser per process (per turn).
#[derive(Debug, Default)]
pub struct LineParser {
    native_id: Option<String>,
    tools_called: HashSet<u64>,
    tools_done: HashSet<u64>,
    /// step index -> (input tokens, output + thinking tokens), last report wins.
    usage: HashMap<u64, (u64, u64)>,
}

/// Step states that mean the step is still going.
fn running(state: &str) -> bool {
    matches!(state, "" | "ACTIVE" | "PENDING" | "RUNNING" | "WAITING" | "GENERATING")
}

impl LineParser {
    pub fn parse(&mut self, line: &str) -> Vec<Parsed> {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            if !line.trim().is_empty() {
                tracing::debug!(target: "ember::agy", "ignoring non-JSON line: {line}");
            }
            return Vec::new();
        };
        let mut out = Vec::new();
        match v["event"].as_str().unwrap_or_default() {
            "init" => {
                if let Some(id) = v["conversation_id"].as_str().filter(|s| !s.is_empty()) {
                    if self.native_id.as_deref() != Some(id) {
                        self.native_id = Some(id.to_string());
                        out.push(Parsed::Event(AgentEvent::NativeSession { native_id: id.to_string() }));
                    }
                }
            }
            "step_update" => self.step(&v["step_update"], &mut out),
            "result" => {
                let r = &v["result"];
                if !self.usage.is_empty() {
                    let (input_tokens, output_tokens) =
                        self.usage.values().fold((0u64, 0u64), |(i, o), &(si, so)| (i + si, o + so));
                    out.push(Parsed::Event(AgentEvent::Usage { input_tokens, output_tokens }));
                }
                if let Some(text) = r["response"].as_str().filter(|s| !s.is_empty()) {
                    out.push(Parsed::Event(AgentEvent::AssistantMessage { text: text.to_string() }));
                }
                let denied: Vec<String> = r["denied_actions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|a| {
                        let name = a["display_name"].as_str().or(a["action"].as_str()).unwrap_or("?");
                        match a["action"].as_str() {
                            Some(kind) if kind != name => format!("{name} ({kind})"),
                            _ => name.to_string(),
                        }
                    })
                    .collect();
                if !denied.is_empty() {
                    out.push(Parsed::Event(AgentEvent::Notice {
                        message: format!("Antigravity refused: {}", denied.join(", ")),
                    }));
                }
                let status = r["status"].as_str().unwrap_or_default();
                let error = (status != "SUCCESS").then(|| {
                    r["error"]
                        .as_str()
                        .or(r["error_message"].as_str())
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .unwrap_or_else(|| {
                            format!("Antigravity turn ended with status {}", if status.is_empty() { "unknown" } else { status })
                        })
                });
                out.push(Parsed::TurnEnd { error });
            }
            _ => {}
        }
        out
    }

    fn step(&mut self, s: &Value, out: &mut Vec<Parsed>) {
        let Some(idx) = s["step_index"].as_u64() else { return };
        let conv = s["conversation_id"]
            .as_str()
            .map(String::from)
            .or_else(|| self.native_id.clone())
            .unwrap_or_default();
        let state = s["state"].as_str().unwrap_or_default();
        if let Some(text) = s["text_delta"].as_str().filter(|t| !t.is_empty()) {
            out.push(Parsed::Event(AgentEvent::AssistantDelta { text: text.to_string() }));
        }
        let u = &s["usage"];
        if u.is_object() {
            let n = |k: &str| u[k].as_u64().unwrap_or(0);
            self.usage.insert(idx, (n("input_tokens"), n("output_tokens") + n("thinking_tokens")));
        }
        if s["step_type"] != "tool" {
            return;
        }
        let id = call_id(&conv, idx);
        if self.tools_called.insert(idx) {
            let info = &s["tool_info"];
            let name = s["tool_name"].as_str().or(info["name"].as_str()).unwrap_or_default();
            out.push(Parsed::Event(AgentEvent::ToolCall {
                call_id: id.clone(),
                name: name.to_string(),
                input: info["parameters"].clone(),
            }));
        }
        if !running(state) && self.tools_done.insert(idx) {
            let error = s["error"].as_str().filter(|e| !e.is_empty());
            // Not in the recorded stream; taken if a later version adds it.
            let output = s["output"].as_str().or(error).unwrap_or_default();
            out.push(Parsed::Event(AgentEvent::ToolResult {
                call_id: id,
                output: output.to_string(),
                is_error: state != "DONE" || error.is_some(),
            }));
        }
    }
}

/// What agy reported on stderr.
#[derive(Debug, Default)]
pub struct StderrErrors {
    /// `AGY_ERROR: {…}` payloads.
    structured: Vec<Value>,
    /// Lines with an `error:` marker.
    plain: Vec<String>,
}

impl StderrErrors {
    pub fn push(&mut self, line: &str) {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("AGY_ERROR:") {
            match serde_json::from_str::<Value>(rest.trim()) {
                Ok(v) => self.structured.push(v),
                Err(_) => self.plain.push(rest.trim().to_string()),
            }
        } else if line.starts_with("error:") || line.contains(" error: ") {
            self.plain.push(line.to_string());
        }
    }

    /// The best error message, if agy printed one.
    pub fn message(&self) -> Option<String> {
        if let Some(v) = self.structured.last() {
            for key in ["message", "short_error", "error", "detail"] {
                if let Some(s) = v[key].as_str().filter(|s| !s.is_empty()) {
                    return Some(s.to_string());
                }
            }
            return Some(v.to_string());
        }
        self.plain.last().cloned()
    }

    /// `RateLimited` when the structured error looks like a quota error (`RESOURCE_EXHAUSTED` or
    /// HTTP 429). The exact field names are not recorded, so the whole payload is searched.
    pub fn rate_limit(&self) -> Option<AgentEvent> {
        let v = self.structured.iter().rev().find(|v| {
            let s = v.to_string();
            s.contains("RESOURCE_EXHAUSTED") || s.contains(":429") || s.contains("\"429\"")
        })?;
        Some(AgentEvent::RateLimited {
            resets_at: None,
            message: self.message().unwrap_or_else(|| v.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TURN1: &str = include_str!("../../tests/fixtures/agy/turn1_denied.jsonl");
    const TURN2: &str = include_str!("../../tests/fixtures/agy/turn2_resumed.jsonl");
    const HOOK_PAYLOAD: &str = include_str!("../../tests/fixtures/agy/pre_tool_use.json");
    const CONV: &str = "04de067f-43aa-4026-b999-2fc0974a2069";

    fn parse_all(text: &str) -> Vec<Parsed> {
        let mut p = LineParser::default();
        text.lines().flat_map(|l| p.parse(l)).collect()
    }

    fn shape(out: &[Parsed]) -> Vec<String> {
        out.iter()
            .map(|p| match p {
                Parsed::Event(AgentEvent::NativeSession { .. }) => "native".into(),
                Parsed::Event(AgentEvent::ToolCall { name, .. }) => format!("call:{name}"),
                Parsed::Event(AgentEvent::ToolResult { is_error, .. }) => format!("result:{is_error}"),
                Parsed::Event(AgentEvent::Usage { .. }) => "usage".into(),
                Parsed::Event(AgentEvent::AssistantMessage { .. }) => "message".into(),
                Parsed::Event(AgentEvent::Notice { .. }) => "notice".into(),
                Parsed::TurnEnd { error } => format!("end:{}", error.is_some()),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn recorded_first_turn_translates_in_order() {
        let out = parse_all(TURN1);
        assert_eq!(
            shape(&out),
            ["native", "call:run_command", "result:false", "usage", "notice", "end:false"]
        );
        assert_eq!(out[0], Parsed::Event(AgentEvent::NativeSession { native_id: CONV.into() }));
        assert_eq!(
            out[1],
            Parsed::Event(AgentEvent::ToolCall {
                call_id: format!("{CONV}:2"),
                name: "run_command".into(),
                input: json!({ "CommandLine": "echo hello-ember" }),
            })
        );
        assert_eq!(out[3], Parsed::Event(AgentEvent::Usage { input_tokens: 13150, output_tokens: 86 }));
        assert_eq!(
            out[4],
            Parsed::Event(AgentEvent::Notice { message: "Antigravity refused: RunCommand (command)".into() })
        );
    }

    #[test]
    fn recorded_resumed_turn_counts_only_its_own_usage() {
        let out = parse_all(TURN2);
        assert_eq!(
            shape(&out),
            ["native", "call:run_command", "result:false", "usage", "notice", "end:false"]
        );
        // `result.usage` says 26752/172 for the whole conversation; this turn's step said this:
        assert_eq!(out[3], Parsed::Event(AgentEvent::Usage { input_tokens: 13602, output_tokens: 86 }));
        assert!(matches!(&out[1], Parsed::Event(AgentEvent::ToolCall { call_id, .. }) if *call_id == format!("{CONV}:6")));
    }

    #[test]
    fn recorded_hook_payload() {
        let v: Value = serde_json::from_str(HOOK_PAYLOAD).unwrap();
        let call = HookCall::from_payload(&v);
        assert_eq!(call.approval_id, format!("{CONV}:2"), "same id as the tool call");
        assert_eq!(call.tool, "run_command");
        assert_eq!(call.input["CommandLine"], "echo hello-ember");
    }

    // The following lines are hand-written (not recorded).

    #[test]
    fn text_deltas_errors_and_unknown_lines() {
        let mut p = LineParser::default();
        assert!(p.parse("").is_empty());
        assert!(p.parse("not json").is_empty());
        assert!(p.parse(r#"{"event":"something_new"}"#).is_empty());
        assert_eq!(
            p.parse(r#"{"event":"step_update","step_update":{"conversation_id":"c","step_index":1,"state":"ACTIVE","step_type":"agent_response","text_delta":"Hi"}}"#),
            [Parsed::Event(AgentEvent::AssistantDelta { text: "Hi".into() })]
        );
        let out = p.parse(r#"{"event":"result","result":{"conversation_id":"c","status":"ERROR","response":""}}"#);
        assert_eq!(out, [Parsed::TurnEnd { error: Some("Antigravity turn ended with status ERROR".into()) }]);
    }

    #[test]
    fn tool_step_seen_only_when_done_still_gets_a_call() {
        let mut p = LineParser::default();
        let out = p.parse(r#"{"event":"step_update","step_update":{"conversation_id":"c","step_index":4,"state":"ERROR","step_type":"tool","tool_name":"write_to_file","tool_info":{"name":"write_to_file","parameters":{"TargetFile":"a"}},"error":"boom"}}"#);
        assert_eq!(
            out,
            [
                Parsed::Event(AgentEvent::ToolCall {
                    call_id: "c:4".into(),
                    name: "write_to_file".into(),
                    input: json!({ "TargetFile": "a" }),
                }),
                Parsed::Event(AgentEvent::ToolResult { call_id: "c:4".into(), output: "boom".into(), is_error: true }),
            ]
        );
    }

    #[test]
    fn stderr_errors() {
        let mut e = StderrErrors::default();
        e.push(include_str!("../../tests/fixtures/agy/turn1_stderr.txt"));
        assert_eq!(e.message(), None, "the soft-deny notice is not an error");
        e.push("error: model call failed");
        assert_eq!(e.message().as_deref(), Some("error: model call failed"));
        e.push(r#"AGY_ERROR: {"status":"RESOURCE_EXHAUSTED","code":429,"message":"quota exceeded"}"#);
        assert_eq!(e.message().as_deref(), Some("quota exceeded"));
        assert!(matches!(e.rate_limit(), Some(AgentEvent::RateLimited { .. })));
        let mut e = StderrErrors::default();
        e.push(r#"AGY_ERROR: {"short_error":"bad"}"#);
        assert_eq!(e.message().as_deref(), Some("bad"));
        assert!(e.rate_limit().is_none());
    }

    #[test]
    fn args_cover_resume_model_and_the_session_root() {
        let args = AntigravityAdapter::turn_args("hi", Some("abc"), Some("gemini-3.8-flash-low"), Path::new("/r"));
        assert_eq!(&args[..2], ["-p", "hi"]);
        assert!(args.windows(2).any(|w| w == ["--output-format", "stream-json"]));
        assert!(args.windows(2).any(|w| w == ["--add-dir", "/r"]));
        assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
        assert!(args.contains(&"--conversation=abc".to_string()));
        assert!(args.windows(2).any(|w| w == ["--model", "gemini-3.8-flash-low"]));
        let args = AntigravityAdapter::turn_args("-x", None, None, Path::new("/r"));
        assert_eq!(args[1], " -x");
        assert!(!args.iter().any(|a| a.starts_with("--conversation") || a == "--model"));
    }

    #[test]
    fn session_root_files() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("agy");
        let (root, new) = session_root(&home, None);
        assert!(new);
        let req = StartRequest {
            cwd: dir.path().to_path_buf(),
            instructions: Some("Use ember-a2a.".into()),
            mcp_servers: vec![McpServer {
                name: "ember-browser".into(),
                command: "npx".into(),
                args: vec!["-y".into(), "x".into()],
                startup_timeout_secs: Some(60),
            }],
            ..Default::default()
        };
        write_session_root(&root, "http://127.0.0.1:1/h", "tok'en", &req).unwrap();
        let agents = root.join(".agents");
        let hooks: Value = serde_json::from_slice(&std::fs::read(agents.join("hooks.json")).unwrap()).unwrap();
        let h = &hooks[HOOK_NAME]["PreToolUse"][0];
        assert_eq!(h["matcher"], "*");
        assert_eq!(h["hooks"][0]["timeout"], HOOK_TIMEOUT_SECS);
        assert!(h["hooks"][0]["command"].as_str().unwrap().ends_with("ember-hook.sh'"));
        let script = std::fs::read_to_string(agents.join("ember-hook.sh")).unwrap();
        assert!(script.contains(r"'Authorization: Bearer tok'\''en'"), "{script}");
        assert!(script.contains("'http://127.0.0.1:1/h'"));
        assert!(script.contains(r#""decision":"deny""#), "fails closed: {script}");
        let rules = std::fs::read_to_string(agents.join("rules/ember.md")).unwrap();
        assert!(rules.contains("Use ember-a2a.") && rules.contains(&root.display().to_string()));
        let mcp: Value = serde_json::from_slice(&std::fs::read(agents.join("mcp_config.json")).unwrap()).unwrap();
        assert_eq!(mcp["mcpServers"]["ember-browser"]["command"], "npx");
        assert_eq!(mcp["mcpServers"]["ember-browser"]["args"][1], "x");

        // Without MCP servers the file goes; a resume finds the same root.
        write_session_root(&root, "u", "t", &StartRequest { cwd: dir.path().into(), ..Default::default() }).unwrap();
        assert!(!agents.join("mcp_config.json").exists());
        remember_root(&home, "conv-1", &root).unwrap();
        assert_eq!(session_root(&home, Some("conv-1")), (root.clone(), false));
        assert!(session_root(&home, Some("other")).1);
        assert!(session_root(&home, Some("../x")).1);
        assert!(remember_root(&home, "../x", &root).is_err());
    }

    #[tokio::test]
    async fn broker_waits_for_the_answer() {
        let (tx, mut rx) = mpsc::channel(16);
        let broker = HookBroker::new();
        let run = Arc::new(RunApprovals::new(tx, Duration::from_secs(30)));
        broker.register("t", run.clone());
        let payload: Value = serde_json::from_str(HOOK_PAYLOAD).unwrap();

        assert!(broker.handle("wrong", &payload).await.is_none());
        let read = json!({"conversationId": "c", "stepIdx": 1, "toolCall": {"name": "view_file", "args": {}}});
        assert_eq!(broker.handle("t", &read).await, Some(allow_reply()));

        let b = broker.clone();
        let p = payload.clone();
        let waiting = tokio::spawn(async move { b.handle("t", &p).await });
        let Some(AgentEvent::ApprovalRequested { approval_id, tool, .. }) = rx.recv().await else {
            panic!("expected an approval request");
        };
        assert_eq!((approval_id.as_str(), tool.as_str()), (format!("{CONV}:2").as_str(), "run_command"));
        assert!(run.answer("nope", ApprovalDecision::AllowOnce).is_err());
        run.answer(&approval_id, ApprovalDecision::AllowAlways).unwrap();
        assert_eq!(waiting.await.unwrap(), Some(allow_reply()));
        // Allowed for the rest of the run, without asking.
        assert_eq!(broker.handle("t", &payload).await, Some(allow_reply()));
        assert!(rx.try_recv().is_err());

        // Another tool is denied; an unregistered run withdraws what is pending.
        let write = json!({"conversationId": "c", "stepIdx": 9, "toolCall": {"name": "write_to_file", "args": {}}});
        let b = broker.clone();
        let w = write.clone();
        let waiting = tokio::spawn(async move { b.handle("t", &w).await });
        let Some(AgentEvent::ApprovalRequested { approval_id, .. }) = rx.recv().await else { panic!() };
        run.answer(&approval_id, ApprovalDecision::Deny).unwrap();
        assert_eq!(waiting.await.unwrap().unwrap()["decision"], "deny");
        let b = broker.clone();
        let waiting = tokio::spawn(async move { b.handle("t", &write).await });
        let _ = rx.recv().await;
        broker.unregister("t");
        assert_eq!(waiting.await.unwrap().unwrap()["decision"], "deny");
    }

    #[tokio::test]
    async fn unanswered_approval_times_out_as_deny() {
        let (tx, mut rx) = mpsc::channel(16);
        let run = RunApprovals::new(tx, Duration::from_millis(50));
        let payload: Value = serde_json::from_str(HOOK_PAYLOAD).unwrap();
        let reply = run.decide(&payload).await;
        assert_eq!(reply["decision"], "deny");
        assert!(matches!(rx.recv().await, Some(AgentEvent::ApprovalRequested { .. })));
        assert!(matches!(
            rx.recv().await,
            Some(AgentEvent::ApprovalResolved { decision: ApprovalDecision::Deny, .. })
        ));
    }
}
