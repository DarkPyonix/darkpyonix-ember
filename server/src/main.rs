use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use ember_server::agents::claude_code::ClaudeCodeAdapter;
use ember_server::agents::codex::CodexAdapter;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::AgentAdapter;
use ember_server::browser::{agent as browser_agent, BrowserConfig, BrowserManager};
use ember_server::session::Sessions;
use ember_server::store::Store;

/// Configuration from the environment.
/// - `EMBER_DATA_DIR`: where `ember.db` lives (default `~/.ember`)
/// - `EMBER_LISTEN`: listen address (default `127.0.0.1:8740`)
/// - `EMBER_IDLE_SECS`: release an agent process after this long without activity (default 300)
/// - `EMBER_CLAUDE_BIN`: the Claude Code CLI (default `claude` on `PATH`)
/// - `EMBER_CODEX_BIN`: the Codex CLI (default `codex` on `PATH`)
/// - `EMBER_SCRIPTED_AGENT=1`: also offer the test agent (development only)
/// - `EMBER_CHROME_BIN`: the browser for remote browsing (default: found on the machine)
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
    let sessions = Sessions::new(store, adapters);

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

    let addr: SocketAddr = std::env::var("EMBER_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:8740".into())
        .parse()?;

    // Remote browser (SPEC §R). Agents run on this machine, so they reach the CDP relay on loopback.
    let browsers = BrowserManager::new(BrowserConfig::from_env(&data_dir));
    match &browsers.config().chrome {
        Some(p) => tracing::info!("remote browser: {}", p.display()),
        None => tracing::warn!("remote browser unavailable: no Chrome/Chromium found (EMBER_CHROME_BIN)"),
    }
    sessions.add_start_hook(browser_agent::start_hook(
        format!("http://127.0.0.1:{}", addr.port()),
        browser_agent::BrowserMcp::ChromeDevtools,
    ));

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("ember server listening on http://{addr}");
    let app = ember_server::api::router(sessions)
        .merge(ember_server::browser::api::router(browsers.clone()));
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    // Close browsers gracefully so their profiles (cookies, storage) are flushed (FR-R2).
    browsers.shutdown().await;
    Ok(())
}
