//! Loopback SOCKS5 listener per computer: the remote browser's egress through that computer's
//! ember node (SPEC FR-R1).
//!
//! ```text
//! Chrome --proxy-server=socks5://127.0.0.1:<port>
//!    └─TCP─▶ EgressListener ──/v1/egress (WebSocket, + token)──▶ node: SOCKS5 server ──TCP─▶ target
//! ```
//!
//! The listener does not parse SOCKS5: each accepted connection gets its own `/v1/egress` stream
//! and bytes are copied unchanged ([`ember_node::egress::bridge`]), so Chrome's SOCKS5 client
//! talks to the node's SOCKS5 server end to end and DNS is resolved on the node. Chrome cannot
//! authenticate to a SOCKS5 proxy, so the listener is bound to loopback without authentication:
//! any local process on the ember server can use it while it runs (the same trust boundary as the
//! browser's unauthenticated DevTools port).

use std::net::SocketAddr;

use anyhow::Context;
use ember_node::client::NodeClient;

/// A running listener for one computer. Dropping it stops accepting connections; connections
/// already open finish on their own.
pub struct EgressListener {
    addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for EgressListener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl EgressListener {
    /// Bind `127.0.0.1:0` and start forwarding to `node`. Callable from synchronous code inside a
    /// Tokio runtime.
    pub fn start(node: NodeClient) -> anyhow::Result<EgressListener> {
        let handle = tokio::runtime::Handle::try_current()
            .context("the egress listener needs a Tokio runtime")?;
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        std_listener.set_nonblocking(true)?;
        let addr = std_listener.local_addr()?;
        let listener = {
            let _guard = handle.enter();
            tokio::net::TcpListener::from_std(std_listener)?
        };
        let task = handle.spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(x) => x,
                    Err(e) => {
                        tracing::warn!("egress listener accept failed: {e}");
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let _ = stream.set_nodelay(true);
                let node = node.clone();
                tokio::spawn(async move {
                    let ws = match node.egress().await {
                        Ok(ws) => ws,
                        Err(e) => {
                            // Chrome sees the connection close during the handshake and reports
                            // ERR_PROXY_CONNECTION_FAILED / ERR_SOCKS_CONNECTION_FAILED.
                            tracing::warn!("egress: opening the node's /v1/egress failed: {e}");
                            return;
                        }
                    };
                    if let Err(e) = ember_node::egress::bridge(stream, ws).await {
                        tracing::debug!("egress connection ended: {e:#}");
                    }
                });
            }
        });
        Ok(EgressListener { addr, task })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The proxy URL for Chrome's `--proxy-server`.
    pub fn proxy_url(&self) -> String {
        format!("socks5://{}", self.addr)
    }
}
