use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use ember_node::api::{self, Node};
use ember_node::config::{self, LocalEndpoint, NodeConfig};
use ember_node::term::pty;
use ember_node::hub;
use ember_node::transport;
use ember_transport::PeerGate;

/// Configuration from the environment:
/// - `EMBER_NODE_TOKEN` (required): bearer token ember server presents
/// - `EMBER_NODE_LISTEN`: listen address (default `127.0.0.1:8741`)
/// - `EMBER_NODE_ROOTS`: colon-separated allowed roots (default `$HOME`)
/// - `EMBER_NODE_STATE_DIR`: persistent terminal metadata and `local.json` (default
///   `$HOME/.ember/node`)
/// - `EMBER_NODE_KEEP_PTY=0`: do not start PTY keepers (terminal sessions end with the daemon)
/// - `EMBER_NODE_CODEX_BIN`: codex binary for `/exec-server` (default `codex` on `PATH`)
/// - `EMBER_NODE_TRANSPORT`: `1` also serves the API over the peer-to-peer transport (service
///   `ember-node`, key in `<state dir>/transport.key`), `only` serves it there instead of TCP;
///   the peer id and address are printed at start
/// - `EMBER_NODE_ALLOWED_PEERS` and `<state dir>/allowed-peers`: peer ids (ember servers) allowed
///   to connect over the transport (FR-N3); SIGHUP reloads the file and disconnects removed peers
/// - `EMBER_RELAY_URL`: relay server(s) for the transport (overrides the hub's relay)
/// - `EMBER_HUB_URL`: the darkpyonix.dev hub (default `https://darkpyonix.dev`, `off` disables);
///   `ember-node hub register` joins this computer to the user's GitHub account there (the
///   registration is kept in `<state dir>/hub.json`); a registered node publishes its address
///   through the hub and uses its relay (FR-N2). `EMBER_NODE_HUB_ALLOW_SERVERS=1` also admits
///   the account's main servers over the transport
/// - `EMBER_NODE_EGRESS=off`: refuse `/egress` (the remote browser's SOCKS5 exit, FR-R1)
/// - `EMBER_NODE_EGRESS_DENY`: denied egress destinations (`private`, `link-local`, `loopback`,
///   CIDRs; default none)
/// - `EMBER_NODE_SOCKS_LISTEN`: also serve plain SOCKS5 on this address (loopback clients need no
///   authentication; others use the token as RFC 1929 password)
///
/// `ember-node __keep-pty …` is the internal PTY keeper (see `ember_node::term::pty`).
/// `ember-node hub <register [--name N] | status | forget>` manages the hub registration.
/// `ember-node exec-server` instead becomes `codex exec-server --listen stdio` on this process's
/// stdio, the same command the daemon starts for each `/exec-server` connection.
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some(pty::KEEPER_SUBCOMMAND) {
        // No runtime, no logging: the keeper is a few hundred kilobytes that hold one fd.
        std::process::exit(pty::keeper_main(&args[2..]));
    }
    if args.get(1).map(String::as_str) == Some("exec-server") {
        return Err(ember_node::exec_server::run_stdio());
    }
    if args.get(1).map(String::as_str) == Some("hub") {
        return tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(ember_node::hub::cli(&args[2..]));
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
    let mode = transport::TransportMode::from_env()?;
    let node = Node::new(cfg)?;
    if let Some(socks) = std::env::var("EMBER_NODE_SOCKS_LISTEN").ok().filter(|s| !s.is_empty()) {
        let listener = tokio::net::TcpListener::bind(socks.parse::<std::net::SocketAddr>()?).await?;
        tracing::info!("SOCKS5 egress listening on {}", listener.local_addr()?);
        tokio::spawn(ember_node::egress::serve_listener(listener, token.clone(), node.egress_policy().clone()));
    }

    // Every listener serves the same router (`api::serve` takes any `axum::serve::Listener`).
    let mut servers: Vec<Pin<Box<dyn Future<Output = std::io::Result<()>> + Send>>> = Vec::new();

    if mode.tcp() {
        let addr = config::listen_addr()?;
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let local = listener.local_addr()?;
        tracing::info!(roots = ?node.policy().roots(), "ember node listening on http://{local}");
        if let Some(dir) = &state_dir {
            // How ember-term and the VS Code companion on this computer find us.
            if let Err(e) = LocalEndpoint::for_addr(local, &token).write(dir) {
                tracing::warn!("could not write {}: {e}", dir.join(config::LOCAL_ENDPOINT_FILE).display());
            }
        }
        servers.push(Box::pin(api::serve(listener, node.clone())));
    }

    let mut bound = None;
    if mode.transport() {
        let dir = state_dir.clone().ok_or_else(|| {
            anyhow::anyhow!("{} needs a state dir for its key (EMBER_NODE_STATE_DIR or HOME)", transport::ENABLE_ENV)
        })?;
        let t = transport::bind(&dir).await?;
        let allowed = transport::allowed_peers(Some(&dir))?;
        if allowed.is_empty() {
            tracing::warn!(
                "no allowed peers: every transport connection will be refused. Add the ember server's                  peer id to {} or {}",
                transport::allowed_peers_file(&dir).display(),
                transport::ALLOWED_PEERS_ENV
            );
        }
        let gate = PeerGate::allow_list(allowed.iter().copied());
        let allow_servers = hub::allow_servers_from_env();
        if let Some(reg) = hub::active_registration(&dir)? {
            tracing::info!(hub = %reg.hub_url, name = %reg.device.name, "registered with the hub");
        } else if hub::registration_file(&dir).load()?.is_some() {
            tracing::error!("the hub removed this node; it is not published there (ember-node hub status)");
        }
        // Give the endpoint a moment to reach its relay, so the printed address includes it.
        t.wait_online(Duration::from_secs(5)).await;
        let addr = t.local_addr();
        // Printed (not only logged) so it can be pasted into the server's computer registration.
        println!("ember node peer id: {}", t.peer_id());
        println!("ember node peer addr: {}", serde_json::to_string(&addr)?);
        tracing::info!(
            peer = %t.peer_id(),
            service = transport::NODE_SERVICE,
            allowed = allowed.len(),
            "ember node serving over the transport"
        );
        // The hub registration: noticing removal, and (opt-in) admitting the account's servers.
        let mut watch = Some(hub::spawn_watch(&dir, t.clone(), gate.clone(), allowed.clone(), allow_servers, hub::WATCH_PERIOD));
        {
            // SIGHUP re-reads the allow-list; peers no longer listed are disconnected (FR-N3).
            // It also picks up a registration made since start (`ember-node hub register`).
            let gate = gate.clone();
            let dir = dir.clone();
            let t = t.clone();
            tokio::spawn(async move {
                use tokio::signal::unix::{signal, SignalKind};
                let Ok(mut hup) = signal(SignalKind::hangup()) else { return };
                while hup.recv().await.is_some() {
                    match transport::allowed_peers(Some(&dir)) {
                        Ok(list) => {
                            let n = list.len();
                            let closed = gate.set_allowed(list.iter().copied());
                            tracing::info!(allowed = n, closed, "reloaded allowed peers");
                            if let Ok(Some(reg)) = hub::active_registration(&dir) {
                                if let Err(e) = hub::apply_registration(&t, &dir, &reg).await {
                                    tracing::warn!("could not apply the hub registration: {e:#}");
                                }
                                if let Some(old) = watch.take() {
                                    old.abort();
                                }
                                watch = Some(hub::spawn_watch(&dir, t.clone(), gate.clone(), list, allow_servers, hub::WATCH_PERIOD));
                            }
                        }
                        Err(e) => tracing::warn!("could not reload allowed peers: {e:#}"),
                    }
                }
            });
        }
        servers.push(Box::pin(transport::serve(&t, node.clone(), gate)?));
        bound = Some(t);
    }

    let terms = node.terms().clone();
    tokio::select! {
        (r, _, _) = futures::future::select_all(servers) => r?,
        _ = shutdown_signal() => {
            // Graceful stop: save every running terminal's screen so a re-adopting node (the
            // PTY keepers hold the processes) restores scrollback and screen.
            tracing::info!("shutting down; saving terminal snapshots");
            tokio::task::spawn_blocking(move || terms.shutdown()).await?;
        }
    }
    if let Some(t) = bound {
        t.close().await;
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
