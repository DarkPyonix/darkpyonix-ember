use ember_node::api::{self, Node};
use ember_node::config::{self, LocalEndpoint, NodeConfig};
use ember_node::term::pty;

/// Configuration from the environment:
/// - `EMBER_NODE_TOKEN` (required): bearer token ember server presents
/// - `EMBER_NODE_LISTEN`: listen address (default `127.0.0.1:8741`)
/// - `EMBER_NODE_ROOTS`: colon-separated allowed roots (default `$HOME`)
/// - `EMBER_NODE_STATE_DIR`: persistent terminal metadata and `local.json` (default
///   `$HOME/.ember/node`)
/// - `EMBER_NODE_KEEP_PTY=0`: do not start PTY keepers (terminal sessions end with the daemon)
/// - `EMBER_NODE_CODEX_BIN`: codex binary for `/v1/exec-server` (default `codex` on `PATH`)
///
/// `ember-node __keep-pty …` is the internal PTY keeper (see `ember_node::term::pty`).
/// `ember-node exec-server` instead becomes `codex exec-server --listen stdio` on this process's
/// stdio — the same command the daemon starts for each `/v1/exec-server` connection.
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some(pty::KEEPER_SUBCOMMAND) {
        // No runtime, no logging: the keeper is a few hundred kilobytes that hold one fd.
        std::process::exit(pty::keeper_main(&args[2..]));
    }
    if args.get(1).map(String::as_str) == Some("exec-server") {
        return Err(ember_node::exec_server::run_stdio());
    }
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(serve())
}

async fn serve() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = NodeConfig::from_env()?;
    let state_dir = cfg.state_dir.clone();
    let token = cfg.token.clone();
    let node = Node::new(cfg)?;
    let addr = config::listen_addr()?;
    // Plain TCP for now; the transport is undecided (INTENT.md Q7). `api::serve` takes any
    // `axum::serve::Listener`, so another stream transport plugs in here.
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    tracing::info!(roots = ?node.policy().roots(), "ember node listening on http://{local}");
    if let Some(dir) = &state_dir {
        // How ember-term and the VS Code companion on this computer find us.
        if let Err(e) = LocalEndpoint::for_addr(local, &token).write(dir) {
            tracing::warn!("could not write {}: {e}", dir.join(config::LOCAL_ENDPOINT_FILE).display());
        }
    }

    let terms = node.terms().clone();
    tokio::select! {
        r = api::serve(listener, node) => r?,
        _ = shutdown_signal() => {
            // Graceful stop: save every running terminal's screen so a re-adopting node (the
            // PTY keepers hold the processes) restores scrollback and screen.
            tracing::info!("shutting down; saving terminal snapshots");
            tokio::task::spawn_blocking(move || terms.shutdown()).await?;
        }
    }
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}
