//! Ember's native client (SPEC §L): the launcher and conversation UI on dioxus-compose.
//!
//! No webview anywhere (E1, NFR-L2): the UI is drawn by dioxus-compose's Compose renderer, and
//! the only "browser" this app ever involves is the user's own, which it asks the OS to open
//! for VS Code Web. `tests/no_webview.rs` and `.github/scripts/checks/check-no-webview.sh` enforce it.
//!
//! - [`config`]: server URL and data paths.
//! - [`device_link`]: the app as a device of a server over the transport (FR-N9, #856).
//! - [`demo`]: the data served inside the app when no server is given (demo only, #199).
//! - [`services`]: tokio runtime, `ember_client::Client`, extra server API, outbox.
//! - [`bridge`]: client changes → UI signals.
//! - [`model`]: pure view models (tested).
//! - [`owner_claim`]: the owner-presence claim at start (FR-S12, #842).
//! - [`ui`]: the screens.

pub mod android;
pub mod bridge;
pub mod config;
pub mod demo;
pub mod device_link;
pub mod export;
pub mod editor_host;
pub mod editor_reach;
pub mod ide;
pub mod model;
pub mod owner_claim;
pub mod prefs;
pub mod server;
pub mod services;
pub mod terminal_input;
pub mod terminal_reconnect;
pub mod terminal_list;
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
    prepare();
    open_window();
    shutdown();
}

/// Everything before a window: logging, configuration, the runtime, the client (cache, demo
/// data or the server connection), prefs and the installed services. Every entry point runs
/// it once: the desktop `main` through [`launch`], and the Android entry (`android`, FR-L36).
pub(crate) fn prepare() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("EMBER_LOG").unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("warn,ember_app=info,ember_client=info")
            }),
        )
        .try_init();

    let config = AppConfig::from_env();
    // Prefs first: the server address saved in 설정 decides the reach (FR-N9).
    let initial_prefs = config
        .prefs_path()
        .map(|p| prefs::load(&p))
        .unwrap_or_default();
    let config = match server_choice_from_env(&initial_prefs) {
        Ok(peer) => config.with_peer(peer),
        Err(e) => {
            eprintln!("ember-app: {e}. Give the server's peer id, <peer id>@<ip>:<port>, or the address JSON it prints at start");
            std::process::exit(2);
        }
    };
    tracing::info!(server = %config.server_url, data = ?config.data_dir, "starting ember-app");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("ember-io")
        .enable_all()
        .build()
        .expect("tokio runtime");

    // Over the transport (FR-N9): bind with this device's key; the client and the app's own
    // `ServerApi` both call through the one `Api` (it keeps the transport alive).
    let link = config.server_peer.as_ref().map(|setting| {
        let Some(dir) = config.data_dir.as_deref() else {
            eprintln!("ember-app: no data directory for the device key; set EMBER_DATA_DIR");
            std::process::exit(2);
        };
        let key = match device_link::load_key(dir) {
            Ok(k) => k,
            Err(e) => {
                eprintln!("ember-app: device key: {e}");
                std::process::exit(2);
            }
        };
        match rt.block_on(device_link::DeviceLink::bind(key, setting)) {
            Ok(l) => {
                tracing::info!(device = %l.peer_id(), server = %setting.addr.peer, "reaching the server over the transport");
                l
            }
            Err(e) => {
                eprintln!("ember-app: cannot bind the transport: {e}");
                std::process::exit(2);
            }
        }
    });

    // Loads the cached snapshot (FR-L1): the first frame shows cached projects and sessions
    // before any network answer.
    let client = match &link {
        Some(l) => rt.block_on(Client::with_api(config.client_config(), l.api())),
        None => match rt.block_on(Client::new(config.client_config())) {
            Ok(c) => c,
            Err(e) => {
                // Only a malformed EMBER_SERVER_URL gets here.
                eprintln!(
                    "ember-app: {e}. Set EMBER_SERVER_URL to the main server, e.g. {}",
                    config::DEFAULT_SERVER_URL
                );
                std::process::exit(2);
            }
        },
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

    let shared_ui = ui::coexist::SharedUi::new(initial_prefs.clone());
    let server = match &link {
        Some(l) => ServerApi::over_transport(l.api()),
        None => ServerApi::new(&config.server_url),
    };
    let server = match server {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ember-app: {e}");
            std::process::exit(2);
        }
    };
    let check = link.as_ref().filter(|_| demo.is_none()).map(|l| l.api());
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
    // A device the server refuses, or a server out of reach, reads as one Korean sentence at
    // start (FR-N9); 설정 shows it again.
    if let Some(api) = check {
        services().rt.spawn(async move {
            if let Err(text) = device_link::check(&api).await {
                tracing::warn!("server check over the transport failed: {text}");
                ui::cr::notice::post(text);
            }
        });
    }
}

/// [`config::server_choice`] from the environment and the saved prefs (FR-N9).
fn server_choice_from_env(prefs: &prefs::Prefs) -> Result<Option<device_link::PeerSetting>, String> {
    let set = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    config::server_choice(
        set(device_link::SERVER_PEER_ENV).as_deref(),
        set("EMBER_SERVER_URL").is_some(),
        set(ember_transport::RELAY_URL_ENV).is_some(),
        prefs.server_peer.as_deref(),
        prefs.server_relay.as_deref(),
    )
}

/// The desktop windows: the standalone editor, the component gallery or the shell.
fn open_window() {
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
        start_owner_claim();
        let pref = ui::cr::shell::requested().unwrap_or_else(|| services().initial_prefs.theme);
        ui::cr::shell::launch(pref);
    }
}

/// Claims the owner key for a real server on this computer (FR-S12, #842) and keeps the claim
/// for the life of the app. Nothing in demo mode, over the transport, or for a server
/// elsewhere. The separate editor window (`--editor`) does not claim.
fn start_owner_claim() {
    let s = services();
    let api = s.client.api();
    let endpoint = ember_client::owner::OwnerEndpoint::from_env();
    if !owner_claim::applicable(s.demo.is_some(), api.base_url(), api.peer().is_some(), endpoint.as_ref()) {
        return;
    }
    #[cfg(any(unix, windows))]
    if let Some(endpoint) = endpoint {
        let hooks = Arc::new(AppClaimHooks { api: api.clone(), server: s.server.clone() });
        owner_claim::install(owner_claim::spawn(s.rt.handle(), endpoint, hooks, owner_claim::Backoff::default()));
    }
}

/// The claim's key goes to both HTTP clients of the app; its state goes to the screens.
struct AppClaimHooks {
    api: ember_client::api::Api,
    server: ServerApi,
}

impl owner_claim::ClaimHooks for AppClaimHooks {
    fn set_key(&self, key: Option<&ember_client::owner::OwnerKey>) -> bool {
        let client = self.api.set_owner_key(key);
        let server = self.server.set_owner_key(key);
        client && server
    }

    fn view(&self, view: owner_claim::ClaimView) {
        ui::coexist::shared_ui().owner_claim.set(view);
    }

    fn notice(&self, text: String) {
        ui::cr::notice::post(text);
    }
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
    // Used by the separate editor window only (FR-L31).
    LaunchBuilder::new()
        // The toolkit's own widgets follow the saved System / Light / Dark choice (FR-L28).
        .with_theme(
            // Inter, then Pretendard, for prose, JetBrains Mono for Mono, and the code panel
            // in `surface` (design review 2026-11-13 A1).
            ui::fonts::editor_theme(services().initial_prefs.theme.color_scheme()),
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
