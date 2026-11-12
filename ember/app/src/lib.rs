//! Ember's native client (SPEC §L): the launcher and conversation UI on dioxus-compose.
//!
//! No webview anywhere (E1, NFR-L2): the UI is drawn by dioxus-compose's Compose renderer, and
//! the only "browser" this app ever involves is the user's own, which it asks the OS to open
//! for VS Code Web. `tests/no_webview.rs` and `.github/scripts/checks/check-no-webview.sh` enforce it.
//!
//! - [`config`]: server URL and data paths.
//! - [`demo`]: the data served inside the app when no server is given (demo only, #199).
//! - [`services`]: tokio runtime, `ember_client::Client`, extra server API, outbox.
//! - [`bridge`]: client changes → UI signals.
//! - [`model`]: pure view models (tested).
//! - [`ui`]: the screens.

pub mod bridge;
pub mod config;
pub mod demo;
pub mod export;
pub mod editor_host;
pub mod ide;
pub mod model;
pub mod prefs;
pub mod server;
pub mod services;
pub mod terminal_input;
pub mod terminal_reconnect;
pub mod terminal_stream;
pub mod ui;

use std::sync::{Arc, Mutex, OnceLock};

use dioxus_compose::prelude::*;

use ember_client::Client;

use crate::config::AppConfig;
use crate::model::Outbox;
use crate::server::ServerApi;
use crate::services::{Services, install, services};

/// Build the services, start the client, open the window. Returns when the window closes.
pub fn launch() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("EMBER_LOG").unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("warn,ember_app=info,ember_client=info")
            }),
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
            eprintln!(
                "ember-app: {e}. Set EMBER_SERVER_URL to the main server, e.g. {}",
                config::DEFAULT_SERVER_URL
            );
            std::process::exit(2);
        }
    };
    // Demo only (#199): with EMBER_DEMO_DATA=1 and no EMBER_SERVER_URL the app serves itself;
    // the push connection and sync workers stay off so nothing waits for a server.
    let demo = demo::active_from_env().then(|| {
        let now = demo::now_ms();
        let data_dir = config
            .data_dir
            .clone()
            .unwrap_or_else(|| std::env::temp_dir().join("ember-demo"));
        let source =
            demo::DemoSource::with_data_dir(now, data_dir).expect("데모 폴더를 만들 수 없습니다");
        for input in source.inputs(now) {
            client.apply(input);
        }
        tracing::info!("demo mode: serving demo data, no server contacted");
        source
    });
    if demo.is_none() {
        // Push connection, resync worker and cache writer (they need the runtime's context).
        let _guard = rt.enter();
        client.start();
    }

    let initial_prefs = config
        .prefs_path()
        .map(|p| prefs::load(&p))
        .unwrap_or_default();
    let shared_ui = ui::coexist::SharedUi::new(initial_prefs.clone());
    let server = match ServerApi::new(&config.server_url) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ember-app: {e}");
            std::process::exit(2);
        }
    };
    install(Services {
        config,
        rt,
        client,
        server,
        demo,
        outbox: Arc::new(Mutex::new(Outbox::default())),
        initial_prefs,
        ui: shared_ui,
        live: OnceLock::new(),
    });

    if let Some(target) = ide::parse_editor_args(std::env::args()) {
        // Separate IDE window (FR-L31): `--editor <project> <folder>` opens the editor as its own
        // HTML-mode window in its own process until dioxus-compose#152 (the `window` element on a
        // shared host) lands.
        // `EMBER_CR_SIZE=1100x400` opens a check window of that size beside the owner's.
        let (w, h): (u16, u16) = std::env::var("EMBER_CR_SIZE")
            .ok()
            .and_then(|v| {
                let (w, h) = v.split_once('x')?;
                Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
            })
            .unwrap_or((1280, 820));
        let title: &'static str = Box::leak(format!("{} - Ember", target.project).into_boxed_str());
        services().ui.standalone_editor.set(true);
        services().ui.editor.set(Some(target));
        // TODO(dioxus-compose#161): HtmlView inside a Compose root aborts the real renderer, so the editor runs in HTML mode.
        launch_builder()
            .with_html(ui::editor_workbench::page::config)
            .with_window(ui::cr::shell::window_chrome(Window::new().with_title(title).with_size(w, h)))
            .launch(ui::editor_workbench::page::app);
    } else if let Some(pref) = ui::cr::gallery::requested() {
        // Phase 1 review window (#220): the compose-rust components, no app.
        ui::cr::gallery::launch(pref);
    } else {
        // Default launch (#220 phase 8): start the background bridge and open the compose-rust
        // shell. `EMBER_CR_SHELL` still overrides the initial theme preference when set.
        bridge::start_shared();
        let pref = ui::cr::shell::requested().unwrap_or_else(|| services().initial_prefs.theme);
        ui::cr::shell::launch(pref);
    }

    shutdown();
}

/// Flush the current state before the process exits or the window loop returns.
pub(crate) fn shutdown() -> i32 {
    let s = services();
    let _guard = services::prefs_write_lock();
    let prefs = s.ui.prefs.get_untracked();
    let path = s.config.prefs_path();
    let code = flush_before_exit(path.as_deref(), &prefs, || {
        // Demo data is never written to the cache.
        if s.demo.is_none() {
            if let Err(e) = s.rt.block_on(s.client.flush_cache()) {
                tracing::warn!("final cache write failed: {e}");
            }
        }
        s.client.stop();
    });
    code
}

fn flush_before_exit(
    path: Option<&std::path::Path>,
    prefs: &prefs::Prefs,
    flush_cache: impl FnOnce(),
) -> i32 {
    if let Some(path) = path {
        if let Err(e) = prefs::save(path, prefs) {
            tracing::warn!("could not save prefs to {}: {e}", path.display());
        }
    }
    flush_cache();
    0
}

fn launch_builder() -> LaunchBuilder {
    // Follows the host platform's design system (Material 3 where there is no native one).
    LaunchBuilder::new()
        // The toolkit's own widgets follow the saved System / Light / Dark choice (FR-L28).
        .with_theme(
            // Inter, then Pretendard, then system fonts for every role but Mono.
            ui::fonts::app_theme(services().initial_prefs.theme.color_scheme()),
        )
        .with_window(Window::new().with_title("Ember"))
}

#[cfg(test)]
mod shutdown_tests {
    #[test]
    fn fr_l31_1_editor_close_flushes_prefs_and_cache_before_exit() {
        let scratch = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let dir = tempfile::tempdir_in(scratch).unwrap();
        let path = dir.path().join("prefs.json");
        let mut prefs = crate::prefs::Prefs::default();
        prefs.keymap = crate::ui::keymap::Keymap::JetBrains;
        prefs.ide_target = Some("vscode".into());
        let mut cache_flushed = false;
        let code = super::flush_before_exit(Some(&path), &prefs, || {
            assert_eq!(crate::prefs::load(&path), prefs);
            cache_flushed = true;
        });
        assert_eq!(code, 0);
        assert!(cache_flushed);
        assert_eq!(crate::prefs::load(&path), prefs);
    }
}
