//! The in-process node of this machine, behind a loopback listener (FR-W4, #455).
//!
//! Terminal attach is a WebSocket through [`NodeClient`], which needs a real URL, so the
//! in-process `oneshot` of `api/git.rs` cannot carry it. This starts ONE node for the server's own
//! machine on `127.0.0.1:0`, the first time the local computer's terminals are used (a server that
//! never opens one binds nothing). Dropping the value aborts the accept task and closes the port.
//!
//! Security: the listener is loopback only. Its Bearer token is 32 bytes from the OS random source
//! (`getrandom`, 256 bits, 64 hex characters), held in memory, never logged or written. The client
//! ignores proxy variables ([`NodeClient::new_local`]), so the token never goes to a proxy named
//! in the server's environment. Another OS user on the machine can
//! connect to the port but cannot authenticate. The node serves file and terminal operations as
//! the server user within its roots (`$HOME`), so the token equals the server token in power. It
//! is reachable only through the relay routes, which stay behind `auth::protect`.
//!
//! Lifetimes (#474): the node has no state dir, so it starts no PTY keepers and writes no
//! terminal records. Its terminals are children of the server process and end with it (the PTY
//! master closes and the shell gets SIGHUP); a restarted server starts a new, empty local node and
//! does not re-attach to anything. Dropping [`LocalNode`] aborts the accept task, which closes the
//! listener; connections already accepted (an attach WebSocket in flight) are separate tasks and
//! keep running until they end. At most [`MAX_LOCAL_TERMS`] terminals run at once.

use std::net::SocketAddr;

use anyhow::Context;
use ember_node::api::{self, Node};
use ember_node::client::NodeClient;
use ember_node::config::NodeConfig;

/// The paths the local node refuses: the server data dir (`EMBER_DATA_DIR`, else `<home>/.ember`,
/// where `ember.db`, `secret.key`, the server token and `accounts/` live) and the OSE data root
/// (`EMBER_OSE_DATA_DIR`, else `<data dir>/ose-data`, holding connection token files), when it is
/// a different path. Kept pure so tests need no environment.
///
/// This narrows the `/fs` API only. It is not a sandbox: terminals and agents run as the server
/// user and can read these folders by shell (D37 full authority is unchanged).
pub fn denied_paths(
    data_dir: Option<std::path::PathBuf>,
    ose_dir: Option<std::path::PathBuf>,
    home: Option<std::path::PathBuf>,
) -> Vec<std::path::PathBuf> {
    let non_empty = |p: Option<std::path::PathBuf>| p.filter(|p| !p.as_os_str().is_empty());
    let (data_dir, home) = (non_empty(data_dir), non_empty(home));
    let mut out = Vec::new();
    match (&data_dir, &home) {
        (Some(d), _) => out.push(d.clone()),
        (None, Some(h)) => out.push(h.join(".ember")),
        (None, None) => {}
    }
    if let Ok(ose) = crate::ose::data_root_from(ose_dir, data_dir, home) {
        if !out.iter().any(|p| ose.starts_with(p)) {
            out.push(ose);
        }
    }
    out
}

/// The most terminals the local node runs at once (#474); creating one more answers 409.
pub const MAX_LOCAL_TERMS: usize = 64;

/// `p` made absolute against the working directory, with a warning, because the server itself
/// uses a relative `EMBER_DATA_DIR` (or `HOME`) relative to its working directory (#534). Dropping
/// it would fail open; the policy refuses relative entries.
fn absolute(var: &str, p: std::path::PathBuf) -> std::path::PathBuf {
    if p.is_absolute() || p.as_os_str().is_empty() {
        return p;
    }
    let abs = std::env::current_dir().map(|d| d.join(&p)).unwrap_or_else(|_| p.clone());
    tracing::warn!(var, given = %p.display(), used = %abs.display(), "relative path made absolute for the local node");
    abs
}

fn denied_from_env() -> Vec<std::path::PathBuf> {
    let var = |k: &str| std::env::var_os(k).map(std::path::PathBuf::from).map(|p| absolute(k, p));
    denied_paths(var("EMBER_DATA_DIR"), var("EMBER_OSE_DATA_DIR"), var("HOME"))
}

/// 32 bytes from the OS random source as 64 lowercase hex characters (#474).
pub fn new_token() -> anyhow::Result<String> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("no OS randomness for the local node token: {e}"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

pub struct LocalNode {
    addr: SocketAddr,
    token: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for LocalNode {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl LocalNode {
    /// Bind `127.0.0.1:0` and serve a node rooted at `$HOME`, refusing the server data dir and the
    /// OSE data root ([`denied_paths`]). Needs a Tokio runtime.
    pub fn start() -> anyhow::Result<LocalNode> {
        let handle = tokio::runtime::Handle::try_current().context("the local node needs a Tokio runtime")?;
        let token = new_token()?;
        let roots = std::env::var_os("HOME").map(std::path::PathBuf::from).map(|p| absolute("HOME", p)).into_iter().collect();
        let node = Node::new(NodeConfig::new(token.clone(), roots).with_denied(denied_from_env()))?;
        node.terms().set_max_running(MAX_LOCAL_TERMS);
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        std_listener.set_nonblocking(true)?;
        let addr = std_listener.local_addr()?;
        let listener = {
            let _guard = handle.enter();
            tokio::net::TcpListener::from_std(std_listener)?
        };
        let task = handle.spawn(async move {
            if let Err(e) = api::serve(listener, node).await {
                tracing::warn!("local node stopped: {e}");
            }
        });
        Ok(LocalNode { addr, token, task })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn client(&self) -> anyhow::Result<NodeClient> {
        Ok(NodeClient::new_local(format!("http://{}", self.addr), self.token.clone())?)
    }
}
