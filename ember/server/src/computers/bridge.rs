//! Loopback HTTP bridge to a node reached over the transport (Claude Code path of FR-X1/FR-X2
//! with `FR-N1`).
//!
//! The `ember-exec` shim runs as its own process per Bash call and speaks plain HTTP/WebSocket
//! to `EMBER_EXEC_NODE_URL`. It has no transport identity of its own (and must not: the node
//! admits only the server's peer id, `FR-N3`). For a peer-addressed node the server therefore
//! gives the shim `http://127.0.0.1:<port>` of this bridge, which carries each accepted TCP
//! connection as raw bytes over one new transport stream to the node's `ember-node` service.
//! Because the transport maps one stream to one HTTP/1.1 connection, this is a complete HTTP
//! and WebSocket proxy with no parsing.
//!
//! ```text
//! ember-exec ──http://127.0.0.1:<port>──▶ bridge ──transport stream (ember-node)──▶ node
//! ```
//!
//! The listener is loopback-only; the node's bearer token is still required on every request,
//! so a local process that finds the port gains nothing without the token.

use anyhow::Context;
use ember_transport::{Dialer, PeerAddr};

use ember_node::client::NODE_SERVICE;

/// A running bridge for one computer. Dropping it stops accepting connections.
pub struct NodeBridge {
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for NodeBridge {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl NodeBridge {
    /// Bind a loopback listener and start bridging to `peer`. Callable from synchronous code
    /// that runs inside a Tokio runtime (the session start hook).
    pub fn start(dialer: Dialer, peer: PeerAddr) -> anyhow::Result<NodeBridge> {
        let handle = tokio::runtime::Handle::try_current()
            .context("the node bridge needs a Tokio runtime")?;
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        std_listener.set_nonblocking(true)?;
        let addr = std_listener.local_addr()?;
        let listener = {
            let _guard = handle.enter();
            tokio::net::TcpListener::from_std(std_listener)?
        };
        let task = handle.spawn(async move {
            loop {
                let mut tcp = match listener.accept().await {
                    Ok((tcp, _)) => tcp,
                    Err(e) => {
                        tracing::warn!("node bridge accept failed: {e}");
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let dialer = dialer.clone();
                let peer = peer.clone();
                tokio::spawn(async move {
                    let _ = tcp.set_nodelay(true);
                    let mut stream = match dialer.open_bi(&peer, NODE_SERVICE).await {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(peer = %peer.peer.fmt_short(), "node bridge: {e}");
                            return;
                        }
                    };
                    if let Err(e) = tokio::io::copy_bidirectional(&mut tcp, &mut stream).await {
                        tracing::debug!("node bridge connection ended: {e}");
                    }
                });
            }
        });
        Ok(NodeBridge { url: format!("http://{addr}"), task })
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}
