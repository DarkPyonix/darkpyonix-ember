//! Ember's native client (SPEC §L): the launcher and conversation UI on dioxus-compose.
//!
//! No webview anywhere (E1, NFR-L2): the UI is drawn by dioxus-compose's Compose renderer, and
//! the only "browser" this app ever involves is the user's own, which it asks the OS to open
//! for VS Code Web. `tests/no_webview.rs` and `scripts/check-no-webview.sh` enforce it.
//!
//! - [`config`]: server URL and data paths.
//! - [`services`]: tokio runtime, `ember_client::Client`, extra server API, outbox.
//! - [`bridge`]: client changes → UI signals.
//! - [`model`]: pure view models (tested).
//! - [`ui`]: the screens.

pub mod bridge;
pub mod config;
pub mod export;
pub mod ide;
pub mod model;
pub mod prefs;
pub mod server;
pub mod services;
pub mod ui;

use std::sync::{Arc, Mutex, OnceLock};

use dioxus_compose::prelude::*;

use ember_client::Client;

use crate::config::AppConfig;
use crate::model::Outbox;
use crate::server::ServerApi;
use crate::services::{install, services, Services};

/// Build the services, start the client, open the window. Returns when the window closes.
pub fn launch() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("EMBER_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,ember_app=info,ember_client=info")),
        )
        .try_init();

    let config = AppConfig::from_env();
    tracing::info!(server = %config.server_url, data = ?config.data_dir, "starting ember-app");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("ember-io")
        .enable_all()
        .build()
        .expect("tokio runtime");

    // Loads the cached snapshot (FR-L1): the first frame shows cached projects and sessions
    // before any network answer.
    let client = match rt.block_on(Client::new(config.client_config())) {
        Ok(c) => c,
        Err(e) => {
            // Only a malformed EMBER_SERVER_URL gets here.
            eprintln!("ember-app: {e}. Set EMBER_SERVER_URL to the main server, e.g. {}", config::DEFAULT_SERVER_URL);
            std::process::exit(2);
        }
    };
    // Push connection, resync worker and cache writer (they need the runtime's context).
    {
        let _guard = rt.enter();
        client.start();
    }

    let initial_prefs = config.prefs_path().map(|p| prefs::load(&p)).unwrap_or_default();
    let server = ServerApi::new(&config.server_url);
    install(Services {
        config,
        rt,
        client,
        server,
        outbox: Arc::new(Mutex::new(Outbox::default())),
        initial_prefs,
        live: OnceLock::new(),
    });

    launch_builder().launch(ui::app);

    // The window is closed: write the cache once more and stop the background tasks.
    let s = services();
    if let Err(e) = s.rt.block_on(s.client.flush_cache()) {
        tracing::warn!("final cache write failed: {e}");
    }
    s.client.stop();
}

fn launch_builder() -> LaunchBuilder {
    // Follows the host platform's design system (Material 3 where there is no native one).
    LaunchBuilder::new()
        .with_theme(Theme::adaptive(DesignSystem::Material3))
        .with_window(Window::new().with_title("Ember"))
}
