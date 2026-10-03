use ember_node::api::{self, Node};
use ember_node::config::{self, NodeConfig};

/// Configuration from the environment:
/// - `EMBER_NODE_TOKEN` (required): bearer token ember server presents
/// - `EMBER_NODE_LISTEN`: listen address (default `127.0.0.1:8741`)
/// - `EMBER_NODE_ROOTS`: colon-separated allowed roots (default `$HOME`)
/// - `EMBER_NODE_CODEX_BIN`: codex binary for `/v1/exec-server` (default `codex` on `PATH`)
///
/// `ember-node exec-server` instead becomes `codex exec-server --listen stdio` on this process's
/// stdio — the same command the daemon starts for each `/v1/exec-server` connection.
fn main() -> anyhow::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("exec-server") {
        return Err(ember_node::exec_server::run_stdio());
    }
    daemon()
}

#[tokio::main]
async fn daemon() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = NodeConfig::from_env()?;
    let node = Node::new(cfg)?;
    let addr = config::listen_addr()?;
    // Plain TCP for now; the transport is undecided (INTENT.md Q7). `api::serve` takes any
    // `axum::serve::Listener`, so another stream transport plugs in here.
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(
        roots = ?node.policy().roots(),
        "ember node listening on http://{}",
        listener.local_addr()?
    );
    api::serve(listener, node).await?;
    Ok(())
}
