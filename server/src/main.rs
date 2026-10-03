use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use ember_server::accounts::Accounts;
use ember_server::agents::claude_code::ClaudeCodeAdapter;
use ember_server::agents::codex::CodexAdapter;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::a2a::{A2a, A2aConfig, A2aStore};
use ember_server::agents::AgentAdapter;
use ember_server::session::Sessions;
use ember_server::store::Store;

/// Configuration from the environment.
/// - `EMBER_DATA_DIR`: where `ember.db`, `secret.key` and `accounts/` live (default `~/.ember`)
/// - `EMBER_LISTEN`: listen address (default `127.0.0.1:8740`)
/// - `EMBER_IDLE_SECS`: release an agent process after this long without activity (default 300)
/// - `EMBER_CLAUDE_BIN`: the Claude Code CLI (default `claude` on `PATH`)
/// - `EMBER_CODEX_BIN`: the Codex CLI (default `codex` on `PATH`)
/// - `EMBER_SCRIPTED_AGENT=1`: also offer the test agent (development only)
/// - `EMBER_IDE_COMPUTERS`, `EMBER_IDE_URL`, `EMBER_PUBLIC_URL`: "Open IDE" targets per
///   computer, until ember node reports them (see `api::ide::IdeConfig`)
/// - `EMBER_AGENT_URL`: the server URL given to agents for A2A (default derived from
///   `EMBER_LISTEN`, with an unspecified address replaced by loopback)
/// - `EMBER_A2A=0`: agent-to-agent messaging starts off until a user turns it on
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
    let sessions = Sessions::new(store, adapters);
    accounts.install(&sessions);

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

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("ember server listening on http://{addr}");
    // After binding, so agents woken by queued messages can reach the API.
    a2a.install();
    let ide = Arc::new(ember_server::api::ide::IdeConfig::from_env()?);
    let app = ember_server::api::router(sessions.clone())
        .merge(ember_server::api::ide::router(sessions, ide))
        .merge(ember_server::a2a::api::router(a2a))
        .merge(ember_server::accounts::api::router(accounts));
    axum::serve(listener, app).await?;
    Ok(())
}
