//! Bridge to Codex's experimental exec-server (`docs/design/INTERCEPTION.md`, option (c)).
//!
//! Codex can run its tools (shell, unified exec, PTY, `apply_patch`'s file operations) through a
//! remote *exec-server* that its app-server attaches with `environment/add {environmentId,
//! execServerUrl}`. This module puts that executor on the node:
//!
//! - `ember-node exec-server` (see `main.rs`) replaces itself with
//!   `codex exec-server --listen stdio`. It is the one place that decides
//!   which codex binary and flags run, so the daemon and a manual test use the same command.
//! - `GET /exec-server` (WebSocket, bearer-authenticated like every other route) starts one
//!   such codex child per connection. Binary (or Text) frames from the client are a byte stream
//!   of newline-delimited JSON-RPC (codex-rs/exec-server `connection.rs`, `read_line`, at
//!   rust-v0.167.0); each whole line is checked (below) and written to the child's stdin; each
//!   whole line codex writes to stdout comes back as one Binary frame. ember server's relay
//!   re-frames it for codex app-server.
//! - Closing the socket closes the child's stdin; the stdio transport ends and codex exits
//!   and is killed after [`EXIT_GRACE`] if it does not.
//! - **Confinement (#1047).** The relay is line-framed in both directions. Every line from the
//!   client is judged by [`crate::exec_server_guard::check_line`] against the node's allowed
//!   folders before codex sees it: a request for a path outside them is answered here with a
//!   JSON-RPC error and never written to codex. Codex's own output is relayed whole line by whole
//!   line, so a refusal is never spliced into the middle of one of codex's messages.
//! - **Codex must be installed on this computer** (`EMBER_NODE_CODEX_BIN`, else `codex` on
//!   `PATH`). When it is missing, the upgrade is answered with 503 and a message naming the
//!   variable ([`resolve_codex_bin`]), instead of a socket that closes with no reason.
//!
//! Known limits: the commands codex runs (`process/start`) are not confined beyond their working
//! directory (they run as the node's user, like `/exec`); see `crate::exec_server_guard` and SPEC
//! FR-X5 for everything that stays unconfined. `ember-node exec-server` (stdio) is not checked:
//! it is run by someone who already has a shell on this computer.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::api::Node;
use crate::client::{ClientError, NodeClient};
use crate::exec_server_guard::{check_line, Verdict};
use crate::proto::{ErrorBody, ErrorCode};

/// Codex binary on the node (default `codex` on `PATH`).
pub const CODEX_BIN_ENV: &str = "EMBER_NODE_CODEX_BIN";
/// How long codex may take to exit after its stdin closed before it is killed.
pub const EXIT_GRACE: Duration = Duration::from_secs(3);

pub fn codex_bin() -> PathBuf {
    std::env::var_os(CODEX_BIN_ENV).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("codex"))
}

/// The longest line accepted in either direction: codex's own per-message bound on stdio
/// (`MAX_STDIO_JSONRPC_MESSAGE_LEN`, 64 MiB). A longer line ends the connection.
pub const MAX_LINE: usize = 64 * 1024 * 1024;

/// Codex is not installed where the node looks for it (FR-X5).
#[derive(Debug)]
pub struct CodexMissing {
    pub detail: String,
}

impl std::fmt::Display for CodexMissing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "codex is not installed on this computer ({}). A Codex session on this computer runs \
             `codex exec-server` here: install Codex on this computer, or set {CODEX_BIN_ENV} to its path",
            self.detail
        )
    }
}

impl std::error::Error for CodexMissing {}

/// The codex binary `/exec-server` runs: `EMBER_NODE_CODEX_BIN` if set, else `codex` on `PATH`.
pub fn resolve_codex_bin() -> Result<PathBuf, CodexMissing> {
    resolve_codex_bin_from(std::env::var_os(CODEX_BIN_ENV), std::env::var_os("PATH"))
}

/// [`resolve_codex_bin`] with the variable and `PATH` given. A configured value with a directory
/// in it must name an executable file; a bare name (and the default `codex`) is looked up on
/// `path`, as the spawn would.
pub fn resolve_codex_bin_from(configured: Option<OsString>, path: Option<OsString>) -> Result<PathBuf, CodexMissing> {
    let name = configured.clone().unwrap_or_else(|| OsString::from("codex"));
    let given = PathBuf::from(&name);
    if given.components().count() > 1 || given.is_absolute() {
        return if is_executable_file(&given) {
            Ok(given)
        } else {
            Err(CodexMissing { detail: format!("{CODEX_BIN_ENV}={} is not an executable file", given.display()) })
        };
    }
    if let Some(found) = path.as_deref().and_then(|p| search_path(&name, p)) {
        return Ok(found);
    }
    let what = match configured {
        Some(_) => format!("{CODEX_BIN_ENV}={}", given.display()),
        None => "codex".to_string(),
    };
    Err(CodexMissing { detail: format!("{what} was not found on PATH") })
}

fn search_path(name: &OsStr, path: &OsStr) -> Option<PathBuf> {
    for dir in std::env::split_paths(path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let exe = candidate.with_extension("exe");
            if is_executable_file(&exe) {
                return Some(exe);
            }
        }
    }
    None
}

fn is_executable_file(path: &std::path::Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Arguments after the codex binary (codex-cli 0.155.1). Not `--exit-on-stdin-close`: that flag
/// belongs to remote registration and makes codex demand `--environment-id` and `--remote`; with
/// `--listen stdio`, codex already exits when stdin closes (checked: exit 0).
/// `codex exec-server --help`.
pub fn codex_args() -> [&'static str; 3] {
    ["exec-server", "--listen", "stdio"]
}

/// `ember-node exec-server`: become `codex exec-server` on this process's stdio. Returns only on
/// failure.
pub fn run_stdio() -> anyhow::Error {
    let bin = match resolve_codex_bin() {
        Ok(bin) => bin,
        Err(e) => return e.into(),
    };
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&bin).args(codex_args()).exec();
        anyhow::anyhow!("could not run {} {}: {err}", bin.display(), codex_args().join(" "))
    }
    #[cfg(not(unix))]
    {
        match std::process::Command::new(&bin).args(codex_args()).status() {
            Ok(s) => std::process::exit(s.code().unwrap_or(1)),
            Err(err) => anyhow::anyhow!("could not run {} {}: {err}", bin.display(), codex_args().join(" ")),
        }
    }
}

/// Route handler for `GET /exec-server`. A missing codex is answered before the upgrade, with 503
/// and the reason, so the caller sees why instead of a socket that closes at once.
pub async fn ws(State(node): State<Node>, ws: WebSocketUpgrade) -> Response {
    let bin = match resolve_codex_bin() {
        Ok(bin) => bin,
        Err(e) => {
            tracing::warn!("exec-server: {e}");
            let body = ErrorBody { code: ErrorCode::NotFound, error: e.to_string(), actual_sha256: None, errno: None };
            return (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response();
        }
    };
    ws.on_upgrade(move |socket| bridge(node, bin, socket))
}

/// Splits a byte stream into lines (without their `\n`). A partial line waits for more bytes.
#[derive(Debug, Default)]
struct Lines {
    buf: Vec<u8>,
}

impl Lines {
    /// Add bytes; returns the whole lines now complete, or `None` once a line passes [`MAX_LINE`].
    fn push(&mut self, chunk: &[u8]) -> Option<Vec<Vec<u8>>> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        let mut start = 0;
        while let Some(i) = self.buf[start..].iter().position(|b| *b == b'\n') {
            out.push(self.buf[start..start + i].to_vec());
            start += i + 1;
        }
        self.buf.drain(..start);
        if self.buf.len() > MAX_LINE {
            return None;
        }
        Some(out)
    }
}

async fn bridge(node: Node, bin: PathBuf, ws: WebSocket) {
    // Same binary and arguments as `ember-node exec-server`, spawned directly so the daemon does
    // not depend on finding its own executable.
    let mut cmd = Command::new(&bin);
    cmd.args(codex_args())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Start inside the first allowed root, so codex's default cwd is a project directory.
    if let Some(root) = node.policy().roots().first() {
        cmd.current_dir(root);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("exec-server: spawn of {} failed: {e}", bin.display());
            let (mut sink, _) = ws.split();
            let _ = sink.close().await;
            return;
        }
    };
    let mut stdin = child.stdin.take().expect("piped");
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(target: "ember_node::exec_server", "{line}");
        }
    });

    let (mut sink, mut stream) = ws.split();
    // One writer to the client: codex's lines and the node's refusals, each a whole line.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let mut writer = tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if sink.send(Message::Binary(line.into())).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    // Two independent directions, so a full pipe one way never stalls the other.
    let refusals = tx.clone();
    let policy = node.policy().clone();
    let up = async move {
        let mut lines = Lines::default();
        while let Some(msg) = stream.next().await {
            let bytes: Vec<u8> = match msg {
                Ok(Message::Binary(b)) => b.to_vec(),
                Ok(Message::Text(t)) => t.as_str().as_bytes().to_vec(),
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => continue,
            };
            let Some(complete) = lines.push(&bytes) else {
                tracing::warn!("exec-server: a client line passed {MAX_LINE} bytes; closing");
                break;
            };
            for line in complete {
                match check_line(&policy, &line) {
                    Verdict::Forward(out) => {
                        if stdin.write_all(&out).await.is_err() || stdin.flush().await.is_err() {
                            return;
                        }
                    }
                    Verdict::Refuse(answer) => {
                        tracing::info!(target: "ember_node::exec_server", "refused: {}", String::from_utf8_lossy(&answer).trim_end());
                        if refusals.send(answer).await.is_err() {
                            return;
                        }
                    }
                    Verdict::Drop => {}
                }
            }
        }
        // Dropping stdin here ends codex's stdio transport, and codex exits.
    };
    let down = async move {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = Vec::new();
            let limit = (MAX_LINE + 1) as u64;
            match (&mut reader).take(limit).read_until(b'\n', &mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let whole = line.last() == Some(&b'\n');
            if !whole && line.len() > MAX_LINE {
                tracing::warn!("exec-server: a codex line passed {MAX_LINE} bytes; closing");
                break;
            }
            if tx.send(line).await.is_err() {
                break;
            }
            if !whole {
                // End of output in the middle of a line: pass the rest on and stop.
                break;
            }
        }
    };
    tokio::select! {
        _ = up => {}
        _ = down => {}
    }
    // Both senders are gone now, so the writer drains what is queued and closes the socket. A
    // client that stopped reading does not hold the child past the grace period.
    if tokio::time::timeout(EXIT_GRACE, &mut writer).await.is_err() {
        writer.abort();
    }
    if tokio::time::timeout(EXIT_GRACE, child.wait()).await.is_err() {
        let _ = child.kill().await;
    }
}

/// Client side of `/exec-server`: a raw byte stream to a fresh codex exec-server on the node.
/// Over TCP or the transport alike ([`crate::client::NodeIo`]).
pub type ExecServerStream = tokio_tungstenite::WebSocketStream<crate::client::NodeIo>;

impl NodeClient {
    /// Open `/exec-server`. Send stdin bytes as Binary frames; stdout arrives as Binary frames.
    pub async fn exec_server(&self) -> Result<ExecServerStream, ClientError> {
        self.websocket("/exec-server").await
    }
}
