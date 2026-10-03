//! Bridge to Codex's experimental exec-server (`docs/design/INTERCEPTION.md`, option (c)).
//!
//! Codex can run its tools (shell, unified exec, PTY, `apply_patch`'s file operations) through a
//! remote *exec-server* that its app-server attaches with `environment/add {environmentId,
//! execServerUrl}`. This module puts that executor on the node:
//!
//! - `ember-node exec-server` (see `main.rs`) replaces itself with
//!   `codex exec-server --listen stdio`. It is the one place that decides
//!   which codex binary and flags run, so the daemon and a manual test use the same command.
//! - `GET /v1/exec-server` (WebSocket, bearer-authenticated like every other route) starts one
//!   such codex child per connection and relays **raw bytes**: every Binary (or
//!   Text) frame from the client is written to the child's stdin unchanged, and whatever the child
//!   writes to stdout comes back as Binary frames, chunked arbitrarily. The relay does not parse
//!   the protocol, so the framing on stdio (newline-delimited JSON-RPC, **[U]**) is the client's
//!   business: ember server's relay re-frames it for codex app-server.
//! - Closing the socket closes the child's stdin; the stdio transport ends and codex exits
//!   and is killed after [`EXIT_GRACE`] if it does not.
//!
//! Known limits: the executor is not confined by the node's path policy (codex enforces its own
//! sandbox, if any); the node needs a `codex` binary (`EMBER_NODE_CODEX_BIN`), which is outside
//! FR-X5's "ember node only" and is accepted for Codex sessions until an ember-native executor
//! exists.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::api::Node;
use crate::client::{ClientError, NodeClient};

/// Codex binary on the node (default `codex` on `PATH`).
pub const CODEX_BIN_ENV: &str = "EMBER_NODE_CODEX_BIN";
/// How long codex may take to exit after its stdin closed before it is killed.
pub const EXIT_GRACE: Duration = Duration::from_secs(3);

pub fn codex_bin() -> PathBuf {
    std::env::var_os(CODEX_BIN_ENV).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("codex"))
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
    use std::os::unix::process::CommandExt;
    let bin = codex_bin();
    let err = std::process::Command::new(&bin).args(codex_args()).exec();
    anyhow::anyhow!("could not run {} {}: {err}", bin.display(), codex_args().join(" "))
}

/// Route handler for `GET /v1/exec-server`.
pub async fn ws(State(node): State<Node>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| bridge(node, socket))
}

async fn bridge(node: Node, ws: WebSocket) {
    // Same binary and arguments as `ember-node exec-server`, spawned directly so the daemon does
    // not depend on finding its own executable.
    let mut cmd = Command::new(codex_bin());
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
            tracing::warn!("exec-server: spawn failed: {e}");
            let (mut sink, _) = ws.split();
            let _ = sink.close().await;
            return;
        }
    };
    let mut stdin = child.stdin.take().expect("piped");
    let mut stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(target: "ember_node::exec_server", "{line}");
        }
    });

    let (mut sink, mut stream) = ws.split();
    // Two independent directions, so a full pipe one way never stalls the other.
    let up = async move {
        while let Some(msg) = stream.next().await {
            let bytes: Vec<u8> = match msg {
                Ok(Message::Binary(b)) => b.to_vec(),
                Ok(Message::Text(t)) => t.as_str().as_bytes().to_vec(),
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => continue,
            };
            if stdin.write_all(&bytes).await.is_err() || stdin.flush().await.is_err() {
                break;
            }
        }
        // Dropping stdin here ends codex's stdio transport, and codex exits.
    };
    let down = async move {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match stdout.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if sink.send(Message::Binary(buf[..n].to_vec().into())).await.is_err() {
                        break;
                    }
                }
            }
        }
        let _ = sink.close().await;
    };
    tokio::select! {
        _ = up => {}
        _ = down => {}
    }
    if tokio::time::timeout(EXIT_GRACE, child.wait()).await.is_err() {
        let _ = child.kill().await;
    }
}

/// Client side of `/v1/exec-server`: a raw byte stream to a fresh codex exec-server on the node.
/// Over TCP or the transport alike ([`crate::client::NodeIo`]).
pub type ExecServerStream = tokio_tungstenite::WebSocketStream<crate::client::NodeIo>;

impl NodeClient {
    /// Open `/v1/exec-server`. Send stdin bytes as Binary frames; stdout arrives as Binary frames.
    pub async fn exec_server(&self) -> Result<ExecServerStream, ClientError> {
        self.websocket("/v1/exec-server").await
    }
}
