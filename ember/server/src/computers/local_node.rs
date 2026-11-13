//! The in-process node of this machine, behind a loopback listener (FR-W4, #455).
//!
//! Terminal attach is a WebSocket through [`NodeClient`], which needs a real URL, so the
//! in-process `oneshot` of `api/git.rs` cannot carry it. This starts ONE node for the server's own
//! machine on `127.0.0.1:0`, the first time the local computer's terminals are used (a server that
//! never opens one binds nothing). Dropping the value aborts the accept task and closes the port.
//!
//! Security: the listener is loopback only. Its Bearer token is 32 bytes from the OS random source
//! (`getrandom`, 256 bits, 64 hex characters), held in memory and never logged. With a state dir
it is also written to `<state dir>/local.json` (mode 0600, the node's own endpoint file format),
so `ember-term` in an OSE terminal finds this node (#26, [provisional]); that folder is inside the
server data dir, which the node itself refuses ([`denied_paths`]). The client
//! ignores proxy variables ([`NodeClient::new_local`]), so the token never goes to a proxy named
//! in the server's environment. Another OS user on the machine can
//! connect to the port but cannot authenticate. The node serves file and terminal operations as
//! the server user within its roots (`$HOME`), so the token equals the server token in power. It
//! is reachable only through the relay routes, which stay behind `auth::protect`.
//!
//! Server restarts (FR-P6, #26): with [`LocalNodeOptions`] carrying a state dir and a keeper
//! executable (what `main.rs` sets: `<data dir>/local-node` and the server binary itself, which
//! answers `__keep-pty`), every terminal gets a PTY keeper and a record, as on a standalone node.
//! A restarted server's local node reads the records, re-adopts the live keepers and lists the
//! sessions again; a graceful stop ([`LocalNode::shutdown`]) saves each screen first. Without
//! options (tests, `EMBER_NODE_KEEP_PTY=0`) the node has no state dir, starts no keepers and its
//! terminals are children of the server and end with it (#474). Dropping [`LocalNode`] aborts the
//! accept task, which closes the listener; connections already accepted (an attach WebSocket in
//! flight) are separate tasks and keep running until they end. At most [`MAX_LOCAL_TERMS`]
//! terminals run at once.

use std::net::SocketAddr;

use anyhow::Context;
use ember_node::api::{self, Node};
use ember_node::client::NodeClient;
use ember_node::config::{LocalEndpoint, NodeConfig};

/// The paths the local node refuses: the server data dir (`EMBER_DATA_DIR`, else `<home>/.ember`,
/// where `ember.db`, `secret.key`, the server token and `accounts/` live) and the OSE data root
/// (`EMBER_OSE_DATA_DIR`, else `<data dir>-runtime/ose-data`, holding connection token files), when
/// it is not inside the data dir. Kept pure so tests need no environment.
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

/// Where the local node keeps terminal records and which executable runs its PTY keepers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalNodeOptions {
    pub state_dir: Option<std::path::PathBuf>,
    pub keeper: Option<std::path::PathBuf>,
}

impl LocalNodeOptions {
    /// Pure choice from the inputs: state dir `EMBER_LOCAL_NODE_STATE_DIR` (`state_override`), else
    /// `<data_dir>/local-node`. Its own folder, not `~/.ember/node`, because a standalone ember node
    /// on the same machine locks and owns that one. `keep_pty` is `EMBER_NODE_KEEP_PTY`: `0`,
    /// `false` or `no` turns keepers (and with them survival) off; `exe` is the server binary.
    pub fn choose(
        data_dir: &std::path::Path,
        state_override: Option<std::path::PathBuf>,
        keep_pty: Option<&str>,
        exe: Option<std::path::PathBuf>,
    ) -> LocalNodeOptions {
        if matches!(keep_pty, Some("0") | Some("false") | Some("no")) {
            return LocalNodeOptions::default();
        }
        let state_dir = state_override.filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| data_dir.join("local-node"));
        LocalNodeOptions { state_dir: Some(state_dir), keeper: exe }
    }

    /// [`LocalNodeOptions::choose`] from the environment and `std::env::current_exe`.
    pub fn from_env(data_dir: &std::path::Path) -> LocalNodeOptions {
        let keep = std::env::var("EMBER_NODE_KEEP_PTY").ok();
        Self::choose(
            data_dir,
            std::env::var_os("EMBER_LOCAL_NODE_STATE_DIR").map(std::path::PathBuf::from),
            keep.as_deref(),
            std::env::current_exe().ok(),
        )
    }

    /// True when the state dir holds terminal records of a previous run, so the server starts
    /// the local node at once and the keepers' programs are read again (a PTY nobody reads
    /// blocks its program once the kernel buffer is full).
    pub fn has_records(&self) -> bool {
        let Some(dir) = &self.state_dir else { return false };
        std::fs::read_dir(dir.join("terms"))
            .map(|rd| rd.flatten().any(|e| e.path().extension().is_some_and(|x| x == "json")))
            .unwrap_or(false)
    }
}

pub struct LocalNode {
    addr: SocketAddr,
    token: String,
    terms: std::sync::Arc<ember_node::term::Terms>,
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
        Self::start_with(&LocalNodeOptions::default())
    }

    /// [`LocalNode::start`] with a state dir and keeper ([`LocalNodeOptions`], FR-P6).
    pub fn start_with(opts: &LocalNodeOptions) -> anyhow::Result<LocalNode> {
        let handle = tokio::runtime::Handle::try_current().context("the local node needs a Tokio runtime")?;
        let token = new_token()?;
        let roots = std::env::var_os("HOME").map(std::path::PathBuf::from).map(|p| absolute("HOME", p)).into_iter().collect();
        let mut config = NodeConfig::new(token.clone(), roots).with_denied(denied_from_env());
        config.state_dir = opts.state_dir.clone();
        config.pty_keeper = opts.keeper.clone();
        let node = Node::new(config)?;
        node.terms().set_max_running(MAX_LOCAL_TERMS);
        let terms = node.terms().clone();
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        std_listener.set_nonblocking(true)?;
        let addr = std_listener.local_addr()?;
        let listener = {
            let _guard = handle.enter();
            tokio::net::TcpListener::from_std(std_listener)?
        };
        if let Some(dir) = &opts.state_dir {
            // ember-term (VS Code tasks in an OSE window) finds the node through this file, the
            // way it finds a standalone daemon (EMBER_NODE_STATE_DIR is set on the OSE child).
            if let Err(e) = LocalEndpoint::for_addr(addr, &token).write(dir) {
                tracing::warn!("local node: cannot write {}: {e}", dir.join(ember_node::config::LOCAL_ENDPOINT_FILE).display());
            }
        }
        let task = handle.spawn(async move {
            if let Err(e) = api::serve(listener, node).await {
                tracing::warn!("local node stopped: {e}");
            }
        });
        Ok(LocalNode { addr, token, terms, task })
    }

    /// Graceful stop: save each running terminal's screen next to its record, so the next
    /// server's local node restores scrollback and screen (FR-P2, FR-P6). No-op without a state dir.
    pub fn shutdown(&self) {
        self.terms.shutdown();
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn client(&self) -> anyhow::Result<NodeClient> {
        Ok(NodeClient::new_local(format!("http://{}", self.addr), self.token.clone())?)
    }
}
