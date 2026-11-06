//! The in-process node of this machine, behind a loopback listener (FR-W4, #455).
//!
//! Terminal attach is a WebSocket through [`NodeClient`], which needs a real URL, so the
//! in-process `oneshot` of `api/git.rs` cannot carry it. This starts ONE node for the server's own
//! machine on `127.0.0.1:0`, the first time the local computer's terminals are used (a server that
//! never opens one binds nothing). Dropping the value aborts the accept task and closes the port.
//!
//! Security: the listener is loopback only. Its Bearer token is two `Uuid::new_v4().simple()`
//! (256 random bits), held in memory, never logged or written. Another OS user on the machine can
//! connect to the port but cannot authenticate. The node serves file and terminal operations as
//! the server user within its roots (`$HOME`), so the token equals the server token in power. It
//! is reachable only through the relay routes, which stay behind `auth::protect`.

use std::net::SocketAddr;

use anyhow::Context;
use ember_node::api::{self, Node};
use ember_node::client::NodeClient;
use ember_node::config::NodeConfig;

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
    /// Bind `127.0.0.1:0` and serve a node rooted at `$HOME`. Needs a Tokio runtime.
    pub fn start() -> anyhow::Result<LocalNode> {
        let handle = tokio::runtime::Handle::try_current().context("the local node needs a Tokio runtime")?;
        let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        let roots = std::env::var_os("HOME").map(std::path::PathBuf::from).into_iter().collect();
        let node = Node::new(NodeConfig::new(token.clone(), roots))?;
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
        Ok(NodeClient::new(format!("http://{}", self.addr), self.token.clone())?)
    }
}
