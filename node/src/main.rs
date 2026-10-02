use ember_node::api::{self, Node};
use ember_node::config::{self, NodeConfig};

/// Configuration from the environment:
/// - `EMBER_NODE_TOKEN` (required): bearer token ember server presents
/// - `EMBER_NODE_LISTEN`: listen address (default `127.0.0.1:8741`)
/// - `EMBER_NODE_ROOTS`: colon-separated allowed roots (default `$HOME`)
#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
