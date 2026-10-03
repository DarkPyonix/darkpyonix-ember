//! Loopback relay from codex app-server to a node's exec-server (Codex path of FR-X1/FR-X2).
//!
//! codex app-server attaches a remote executor with `environment/add {environmentId,
//! execServerUrl}` and connects to that URL as a WebSocket client, one JSON-RPC message per Text
//! frame **[U]** (the exec-server's own `--listen ws://` transport). The node exposes the
//! executor as a raw byte stream (`/v1/exec-server`, bearer token, codex on stdio). This relay
//! sits between them, inside ember server:
//!
//! ```text
//! codex app-server ──ws://127.0.0.1:<port>/<secret>──▶ relay ──/v1/exec-server (+token)──▶ node
//!    Text frame (one message)  ─── + "\n" ───────────▶ Binary bytes ──▶ codex exec-server stdin
//!    Text frame per line       ◀── split on "\n" ──── Binary bytes ◀── codex exec-server stdout
//! ```
//!
//! Each accepted connection opens its own node stream (so its own codex exec-server process).
//! The listener is bound to loopback and the path carries a random secret, because codex cannot
//! present the node's bearer token. Whether codex keeps a URL path when it connects is **[U]**.

use anyhow::Context;
use ember_node::client::NodeClient;
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::Message;

/// A running relay for one computer. Dropping it stops accepting connections.
pub struct ExecServerRelay {
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for ExecServerRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ExecServerRelay {
    /// Bind a loopback listener and start relaying to `node`. Callable from synchronous code
    /// that runs inside a Tokio runtime (the session start hook).
    pub fn start(node: NodeClient) -> anyhow::Result<ExecServerRelay> {
        let handle = tokio::runtime::Handle::try_current()
            .context("the exec-server relay needs a Tokio runtime")?;
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        std_listener.set_nonblocking(true)?;
        let addr = std_listener.local_addr()?;
        let secret = uuid::Uuid::new_v4().simple().to_string();
        let path = format!("/{secret}");
        let url = format!("ws://{addr}{path}");
        let listener = {
            let _guard = handle.enter();
            tokio::net::TcpListener::from_std(std_listener)?
        };
        let task = handle.spawn(async move {
            loop {
                let stream = match listener.accept().await {
                    Ok((stream, _)) => stream,
                    Err(e) => {
                        tracing::warn!("exec-server relay accept failed: {e}");
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let path = path.clone();
                let node = node.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve(stream, path, node).await {
                        tracing::debug!("exec-server relay connection ended: {e:#}");
                    }
                });
            }
        });
        Ok(ExecServerRelay { url, task })
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}

// The handshake callback's signature is fixed by tungstenite, error type included.
#[allow(clippy::result_large_err)]
async fn serve(stream: TcpStream, path: String, node: NodeClient) -> anyhow::Result<()> {
    let check = move |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
        if req.uri().path() == path {
            Ok(resp)
        } else {
            let mut err = ErrorResponse::new(Some("not found".into()));
            *err.status_mut() = StatusCode::NOT_FOUND;
            Err(err)
        }
    };
    let codex = tokio_tungstenite::accept_hdr_async(stream, check).await?;
    let upstream = node.exec_server().await.context("opening the node's /v1/exec-server")?;
    let (mut codex_tx, mut codex_rx) = codex.split();
    let (mut node_tx, mut node_rx) = upstream.split();

    let up = async move {
        while let Some(msg) = codex_rx.next().await {
            let mut bytes = match msg? {
                Message::Text(t) => t.as_str().as_bytes().to_vec(),
                Message::Binary(b) => b.to_vec(),
                Message::Close(_) => break,
                _ => continue,
            };
            bytes.push(b'\n');
            node_tx.send(Message::binary(bytes)).await?;
        }
        let _ = node_tx.close().await;
        anyhow::Ok(())
    };
    let down = async move {
        let mut framer = LineFramer::default();
        while let Some(msg) = node_rx.next().await {
            match msg? {
                Message::Binary(b) => {
                    for line in framer.push(&b) {
                        codex_tx.send(Message::text(line)).await?;
                    }
                }
                Message::Text(t) => {
                    for line in framer.push(t.as_str().as_bytes()) {
                        codex_tx.send(Message::text(line)).await?;
                    }
                }
                Message::Close(_) => break,
                _ => continue,
            }
        }
        let _ = codex_tx.close().await;
        anyhow::Ok(())
    };
    tokio::select! {
        r = up => r,
        r = down => r,
    }
}

/// Splits a byte stream into newline-terminated messages (`\r\n` tolerated, blank lines
/// skipped). A partial last line waits for the next chunk.
#[derive(Debug, Default)]
pub struct LineFramer {
    buf: Vec<u8>,
}

impl LineFramer {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(i) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=i).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if !line.is_empty() {
                out.push(String::from_utf8_lossy(&line).into_owned());
            }
        }
        out
    }

    /// Bytes of an unterminated line still buffered.
    pub fn pending(&self) -> usize {
        self.buf.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framer_splits_across_chunks() {
        let mut f = LineFramer::default();
        assert!(f.push(b"{\"id\":1,").is_empty());
        assert_eq!(f.pending(), 8);
        assert_eq!(f.push(b"\"result\":{}}\n{\"method\":\"x\"}\r\n\n{\"id\""), [
            "{\"id\":1,\"result\":{}}".to_string(),
            "{\"method\":\"x\"}".to_string()
        ]);
        assert_eq!(f.push(b":2}\n"), ["{\"id\":2}".to_string()]);
        assert_eq!(f.pending(), 0);
    }

    #[tokio::test]
    async fn relay_listens_on_loopback_with_a_secret_path() {
        let node = NodeClient::new("http://127.0.0.1:9", "t").unwrap();
        let relay = ExecServerRelay::start(node).unwrap();
        let url = relay.url().to_string();
        assert!(url.starts_with("ws://127.0.0.1:"), "{url}");
        let path = url.splitn(4, '/').nth(3).unwrap();
        assert_eq!(path.len(), 32);
        // A wrong path is refused during the handshake.
        let wrong = url.replace(path, "nope");
        let Err(err) = tokio_tungstenite::connect_async(wrong).await else { panic!("refused") };
        assert!(
            matches!(&err, tokio_tungstenite::tungstenite::Error::Http(r) if r.status() == 404),
            "{err:?}"
        );
    }
}
