use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use ember_server::accounts::Accounts;
use ember_server::agents::claude_code::ClaudeCodeAdapter;
use ember_server::agents::codex::CodexAdapter;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::a2a::{A2a, A2aConfig, A2aStore};
use ember_server::agents::AgentAdapter;
use ember_server::browser::{agent as browser_agent, BrowserConfig, BrowserManager};
use ember_server::computers::{self, Computers, Registry};
use ember_server::devices::{self, Devices};
use ember_server::transport::{self as server_transport, ServerTransport};
use ember_server::session::Sessions;
use ember_server::store::Store;

/// Configuration from the environment.
/// - `EMBER_DATA_DIR`: where `ember.db`, `secret.key` and `accounts/` live (default `~/.ember`)
/// - `EMBER_LISTEN`: listen address (default `127.0.0.1:8740`)
/// - `EMBER_IDLE_SECS`: release an agent process after this long without activity (default 300)
/// - `EMBER_CLAUDE_BIN`: the Claude Code CLI (default `claude` on `PATH`)
/// - `EMBER_CODEX_BIN`: the Codex CLI (default `codex` on `PATH`)
/// - `EMBER_SCRIPTED_AGENT=1`: also offer the test agent (development only)
/// - `EMBER_CHROME_BIN`: the browser for remote browsing (default: found on the machine)
/// - `EMBER_BROWSER_MCP`: `chrome-devtools`, `playwright` or `off` (default): give agents the
///   project's browser as an MCP server, run with `npx`/`bunx` (`EMBER_BROWSER_MCP_RUNNER`)
/// - `EMBER_IDE_COMPUTERS`, `EMBER_IDE_URL`, `EMBER_PUBLIC_URL`: "Open IDE" targets per
///   computer, until ember node reports them (see `api::ide::IdeConfig`)
/// - `EMBER_AGENT_URL`: the server URL given to agents for A2A (default derived from
///   `EMBER_LISTEN`, with an unspecified address replaced by loopback)
/// - `EMBER_A2A=0`: agent-to-agent messaging starts off until a user turns it on
/// - `EMBER_EXEC_BIN`: the `ember-exec` shim for Claude Code on other computers (default: next
///   to this executable)
/// - `EMBER_HOSTED=1`: this server is a hosted service, not self-hosted; Sign in with ChatGPT
///   (FR-U4) is then off, since OpenAI allows plan usage only for locally hosted apps
/// - `EMBER_CHATGPT_REDIRECT_PORT`: port of the `http://127.0.0.1:<port>/auth/callback` sign-in
///   redirect (default: the `EMBER_LISTEN` port)
/// - `EMBER_TRANSPORT=1`: bind the peer-to-peer transport (key in `<data dir>/transport.key`;
///   peer id and address printed at start). Computers can then be registered by peer, and the
///   API is also served on transport service `ember-server/1` to allowed devices (FR-N3,
///   `/api/v1/devices`, managed on the TCP listener only)
/// - `EMBER_RELAY_URL`: relay server(s) for the transport
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let data_dir = std::env::var_os("EMBER_DATA_DIR").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".ember")
    });
    std::fs::create_dir_all(&data_dir)?;
    let store = Arc::new(Store::open(&data_dir.join("ember.db"))?);
    let reset = store.reset_live_statuses()?;
    if reset > 0 {
        tracing::info!("{reset} session(s) were mid-turn at shutdown; marked idle for resume");
    }

    // Always offered; detection reports a missing binary as not installed (FR-A6).
    let mut adapters: Vec<Arc<dyn AgentAdapter>> =
        vec![Arc::new(ClaudeCodeAdapter::from_env()), Arc::new(CodexAdapter::from_env())];
    if std::env::var("EMBER_SCRIPTED_AGENT").as_deref() == Ok("1") {
        adapters.push(Arc::new(ScriptedAdapter));
    }
    let accounts = Accounts::open(store.clone(), &data_dir)?;
    // Devices allowed over the transport (FR-N3); the table feeds the transport's gate.
    let devices = Devices::open(store.clone())?;
    // The peer-to-peer transport (FR-N1), opt-in.
    let net = if server_transport::enabled_from_env() {
        let t = server_transport::bind(&data_dir).await?;
        t.wait_online(std::time::Duration::from_secs(5)).await;
        // Printed (not only logged): the peer id goes into each node's allowed peers.
        println!("ember server peer id: {}", t.peer_id());
        println!("ember server peer addr: {}", serde_json::to_string(&t.local_addr())?);
        Some(ServerTransport::new(t))
    } else {
        None
    };
    let dialer = net.as_ref().map(|n| n.dialer.clone());
    // Computers and switching (FR-X1–FR-X3, FR-S7 v0); tables live in the main store.
    let computer_registry = Registry::new(store.clone());
    let browser_egress_store: Arc<dyn ember_server::browser::EgressStore> = store.clone();
    let sessions = Sessions::new(store, adapters);
    accounts.install(&sessions);
    let shim = computers::find_shim();
    if shim.is_none() {
        tracing::warn!("ember-exec not found; Claude Code sessions cannot run on other computers");
    }
    let computers = Computers::with_transport(
        computer_registry,
        computers::connector(dialer.clone()),
        shim,
        dialer,
    );
    computers.install(&sessions);

    let addr: SocketAddr = std::env::var("EMBER_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:8740".into())
        .parse()?;
    let agent_url = std::env::var("EMBER_AGENT_URL").unwrap_or_else(|_| {
        let mut local = addr;
        if local.ip().is_unspecified() {
            local.set_ip(if addr.is_ipv4() {
                std::net::Ipv4Addr::LOCALHOST.into()
            } else {
                std::net::Ipv6Addr::LOCALHOST.into()
            });
        }
        format!("http://{local}")
    });
    let mut a2a_config = A2aConfig::new(agent_url);
    a2a_config.cli_path = A2aConfig::cli_next_to_current_exe();
    a2a_config.enabled_by_default =
        !matches!(std::env::var("EMBER_A2A").as_deref(), Ok("0" | "off"));
    if a2a_config.cli_path.is_none() {
        tracing::warn!("ember-a2a not found next to the server binary; agents cannot use A2A");
    }
    let a2a = A2a::new(sessions.clone(), A2aStore::open(&data_dir.join("ember.db"))?, a2a_config);

    let idle = std::time::Duration::from_secs(
        std::env::var("EMBER_IDLE_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(300),
    );
    {
        let sessions = sessions.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                tick.tick().await;
                for id in sessions.reap_idle(idle).await {
                    tracing::info!(session = %id, "released idle agent");
                }
            }
        });
    }

    // Remote browser (SPEC §R). Agents run on this machine, so they reach the CDP relay on loopback.
    // Each project's egress choice is persisted; a computer egress goes through that computer's
    // node via a loopback SOCKS5 listener (FR-R1).
    let browsers = BrowserManager::with_egress(
        BrowserConfig::from_env(&data_dir),
        Some(browser_egress_store),
        Some(computers.clone() as Arc<dyn ember_server::browser::EgressResolver>),
    );
    match &browsers.config().chrome {
        Some(p) => tracing::info!("remote browser: {}", p.display()),
        None => tracing::warn!("remote browser unavailable: no Chrome/Chromium found (EMBER_CHROME_BIN)"),
    }
    match browser_agent::from_env() {
        Some((mcp, runner)) => {
            tracing::info!("agents get the project browser via {mcp:?} MCP ({runner:?})");
            sessions.add_start_config_hook(browser_agent::start_config_hook(
                format!("http://127.0.0.1:{}", addr.port()),
                mcp,
                runner,
            ));
        }
        None => tracing::info!("agents get no browser MCP server (EMBER_BROWSER_MCP is off)"),
    }

    // Sign in with ChatGPT (FR-U4): self-hosted only; the loopback callback is served below.
    let chatgpt = ember_server::chatgpt::ChatGpt::new(
        accounts.clone(),
        ember_server::chatgpt::ChatGptConfig::from_env(addr.port()),
    )?;
    if !chatgpt.config().enabled {
        tracing::info!("Sign in with ChatGPT is off: EMBER_HOSTED is set");
    } else if !addr.ip().is_loopback() && !addr.ip().is_unspecified() {
        tracing::warn!(
            "ChatGPT sign-in redirects to {}, but the server listens on {addr}; paste the final \
             URL into POST /api/v1/chatgpt/signin/complete",
            chatgpt.config().redirect_uri()
        );
    }

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("ember server listening on http://{addr}");
    // After binding, so agents woken by queued messages can reach the API.
    a2a.install();
    // TODO(computers): "Open IDE" still takes its per-computer targets from
    // EMBER_IDE_COMPUTERS; derive them from the computers registry (and the session's current
    // computer) instead of a second, hand-maintained list.
    let ide = Arc::new(ember_server::api::ide::IdeConfig::from_env()?);
    let app = ember_server::api::router(sessions.clone())
        .merge(computers::api::router(computers.clone(), sessions.clone()))
        .merge(ember_server::api::ide::router(sessions, ide))
        .merge(ember_server::a2a::api::router(a2a))
        .merge(ember_server::accounts::api::router(accounts))
        .merge(ember_server::chatgpt::api::router(chatgpt))
        .merge(ember_server::browser::api::router(browsers.clone()));
    // Over the transport: the same API to allowed devices, without device management.
    let transport_task = match &net {
        Some(n) => {
            let serve = server_transport::serve(&n.transport, app.clone(), devices.gate().clone())?;
            tracing::info!(
                peer = %n.transport.peer_id(),
                service = server_transport::SERVER_SERVICE,
                devices = devices.list()?.len(),
                "ember server serving over the transport"
            );
            Some(tokio::spawn(async move {
                if let Err(e) = serve.await {
                    tracing::error!("transport listener failed: {e}");
                }
            }))
        }
        None => None,
    };
    // Device management is local only (TCP).
    let local_app = app.merge(devices::api::router(devices.clone()));
    axum::serve(listener, local_app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    if let Some(task) = transport_task {
        task.abort();
    }
    if let Some(n) = &net {
        n.transport.close().await;
    }
    // Close browsers gracefully so their profiles (cookies, storage) are flushed (FR-R2).
    browsers.shutdown().await;
    Ok(())
}
