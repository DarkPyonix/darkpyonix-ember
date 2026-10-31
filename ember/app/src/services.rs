//! Everything below the UI that the app owns: the tokio runtime, the [`Client`], the extra
//! server API, and the message outbox. Built once in [`crate::launch`] before the window
//! opens, then reachable from any component through [`services`].
//!
//! Threads (dioxus-compose PR-3): the VirtualDom runs on the renderer's UI thread, which must
//! never do I/O. All network and file work runs on this runtime's worker threads. Results come
//! back to the UI in one of two ways, both the patterns dioxus-compose's samples use:
//!
//! - **Sync signals** (`use_signal_sync`) written from a worker: the write marks the reading
//!   scopes dirty and the host requests a frame (the chat sample's streaming reply). The
//!   [`crate::bridge`] does this for client state changes.
//! - **A UI task awaiting the worker** ([`run`]): `dioxus_core::spawn` on the UI thread awaits
//!   the tokio `JoinHandle` and applies the result there (the notepad sample's file worker).
//!   Used for user actions whose answer the UI shows (create session, open IDE, …).

use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};

use dioxus_compose::prelude::*;
use tokio::runtime::Runtime;

use ember_client::Client;

use crate::config::AppConfig;
use crate::model::Outbox;
use crate::server::ServerApi;

pub struct Services {
    pub config: AppConfig,
    pub rt: Runtime,
    pub client: Client,
    pub server: ServerApi,
    /// Demo only (#199): `Some` when the app serves itself (no server). The one place the
    /// app decides between the demo source and the server.
    pub demo: Option<crate::demo::DemoSource>,
    pub outbox: Arc<Mutex<Outbox>>,
    /// Prefs as read from disk at start-up; the UI owns them from then on.
    pub initial_prefs: crate::prefs::Prefs,
    /// Shared UI and live state for `compose-rust` composables (#220).
    pub ui: crate::ui::coexist::SharedUi,
    /// The UI's sync signals, set when the root component mounts (see [`crate::bridge`]).
    pub live: OnceLock<crate::bridge::Live>,
}

static SERVICES: OnceLock<Services> = OnceLock::new();

/// Install the services. Called once, before the window opens.
pub fn install(s: Services) -> &'static Services {
    if SERVICES.set(s).is_err() {
        panic!("ember-app services installed twice");
    }
    services()
}

pub fn services() -> &'static Services {
    SERVICES.get().expect("ember-app services are installed before the UI starts")
}

/// Returns `Some` when [`install`] has run, or `None` in isolated unit tests.
pub fn try_services() -> Option<&'static Services> {
    SERVICES.get()
}

impl Services {
    /// Fire-and-forget work on the runtime.
    pub fn spawn<F>(&self, f: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.rt.spawn(f);
    }

    /// Run `f` inside the runtime's context: needed by client calls that `tokio::spawn`
    /// internally (`Client::open_session` starts its lease renewal task).
    pub fn enter<R>(&self, f: impl FnOnce() -> R) -> R {
        let _guard = self.rt.enter();
        f()
    }

    /// Open a conversation in the client (load transcript, mark seen, renew lease).
    pub fn open_session(&self, id: &str) {
        if let Some(d) = &self.demo {
            d.open(&self.client, id);
            return;
        }
        self.enter(|| self.client.open_session(id));
    }

    pub fn close_session(&self, id: &str) {
        if self.demo.is_some() {
            self.client.apply(ember_client::Input::Closed(id.to_string()));
            return;
        }
        self.enter(|| self.client.close_session(id));
    }

    /// Send whatever the outbox releases for `id` now (FR-L6). Safe to call from any thread
    /// and as often as wanted: the outbox decides whether anything goes out.
    pub fn drive_outbox(&'static self, id: &str) {
        let (busy, last_seq) = self.client.read(|s| match s.session(id) {
            Some(v) => (crate::model::is_busy(v.status), v.record.last_seq),
            None => (false, 0),
        });
        let next = self.outbox.lock().unwrap().next(id, busy, last_seq);
        if next.is_some() {
            self.bump_outbox();
        }
        if let Some(text) = next {
            let sid = id.to_string();
            self.spawn(async move {
                let s = services();
                if let Some(d) = &s.demo {
                    d.send_message(&s.client, &sid, &text);
                    return;
                }
                if let Err(e) = s.client.send_message(&sid, &text).await {
                    tracing::warn!(session = %sid, "send failed: {e}");
                    s.outbox.lock().unwrap().failed(&sid, text, e.to_string());
                    s.bump_outbox();
                }
            });
        }
    }
}

impl Services {
    /// Answer an approval: on the server, or in the demo state (#199).
    pub async fn answer(
        &self,
        sid: &str,
        approval_id: &str,
        decision: ember_client::wire::ApprovalDecision,
    ) -> Result<(), String> {
        if let Some(d) = &self.demo {
            d.answer(&self.client, sid, approval_id, decision);
            return Ok(());
        }
        self.client.answer(sid, approval_id, decision).await.map_err(|e| e.to_string())
    }

    /// Create a session: on the server, or in the demo state (#199).
    pub async fn create_session(&self, body: &crate::server::CreateSession) -> Result<ember_client::wire::SessionRecord, String> {
        if let Some(d) = &self.demo {
            return Ok(d.create_session(&self.client, body));
        }
        let rec = self.server.create_session(body).await.map_err(|e| e.to_string())?;
        self.client.apply(ember_client::Input::SessionLoaded(rec.clone()));
        Ok(rec)
    }

    /// Rename, pin or archive: on the server, or in the demo state (#199).
    pub async fn patch_session(&self, id: &str, patch: &ember_client::wire::SessionPatch) -> Result<(), String> {
        if let Some(d) = &self.demo {
            d.patch_session(&self.client, id, patch);
            return Ok(());
        }
        self.client.patch_session(id, patch).await.map(|_| ()).map_err(|e| e.to_string())
    }

    /// Switch approve-everything: on the server, or in the demo state (#199).
    pub async fn set_approve_all(&self, id: &str, enabled: bool) -> Result<(), String> {
        if self.demo.is_some() {
            self.client.apply(ember_client::Input::ApproveAllLoaded { session_id: id.to_string(), enabled });
            return Ok(());
        }
        self.client.set_approve_all(id, enabled).await.map_err(|e| e.to_string())
    }

    /// In the demo, say the action is not available and return true; the caller then returns
    /// without calling the server (#202).
    pub fn demo_blocked(&self) -> bool {
        match crate::demo::unavailable(self.demo.is_some()) {
            Some(m) => {
                show_message(m);
                true
            }
            None => false,
        }
    }

    /// Tell the UI the outbox changed (queued-message indicator).
    pub fn bump_outbox(&self) {
        crate::ui::coexist::SharedLive::bump(&self.ui.live.outbox);
        if let Some(live) = self.live.get() {
            let mut sig = live.outbox;
            *sig.write() += 1;
        }
    }
}

/// Run `work` on the runtime, then `then` with its result on the UI thread. Call from a
/// component or an event handler (it uses the current Dioxus scope to own the UI task).
pub fn run<T, F>(work: F, then: impl FnOnce(T) + 'static)
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
{
    let handle = services().rt.spawn(work);
    dioxus_core::spawn(async move {
        match handle.await {
            Ok(v) => then(v),
            Err(e) => show_message(format!("Background task failed: {e}")),
        }
    });
}

/// Like [`run`], for `compose-rust` composables and handlers that do not sit inside a Dioxus
/// scope (#220). `then` runs on the tokio worker and may write [`compose_rust::MutableState`]s
/// or [`crate::ui::coexist::SharedUi`] fields, which automatically request a frame.
pub fn run_shared<T, F>(work: F, then: impl FnOnce(T) + Send + 'static)
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
{
    services().rt.spawn(async move {
        then(work.await);
    });
}

/// Persist prefs off the UI thread.
pub fn save_prefs(prefs: crate::prefs::Prefs) {
    let Some(path) = services().config.prefs_path() else { return };
    services().rt.spawn_blocking(move || {
        if let Err(e) = crate::prefs::save(&path, &prefs) {
            tracing::warn!("could not save prefs to {}: {e}", path.display());
        }
    });
}

/// Services for unit tests that compose screens: demo data, no server contacted, data under
/// `.scratch/`. Installed once per test process; tests that change shared UI state take
/// [`test_guard`] first so they do not see each other's.
#[cfg(test)]
pub fn test_services() -> &'static Services {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/test-data");
        let config = crate::config::AppConfig::from_values(
            Some("http://127.0.0.1:9".to_string()),
            Some(data),
            None,
        );
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("tokio runtime");
        let client = rt.block_on(ember_client::Client::new(config.client_config())).expect("client");
        let now = crate::demo::now_ms();
        let source = crate::demo::DemoSource::new(now);
        for input in source.inputs(now) {
            client.apply(input);
        }
        let initial_prefs = crate::prefs::Prefs::default();
        let server = crate::server::ServerApi::new(&config.server_url);
        install(Services {
            config,
            rt,
            client,
            server,
            demo: Some(source),
            outbox: Arc::new(Mutex::new(crate::model::Outbox::default())),
            ui: crate::ui::coexist::SharedUi::new(initial_prefs.clone()),
            initial_prefs,
            live: OnceLock::new(),
        });
    });
    services()
}

/// Serialises tests that touch the shared UI state and resets what they change.
#[cfg(test)]
pub fn test_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let ui = &test_services().ui;
    ui.new_session.set(None);
    ui.notice.set(None);
    ui.route.set(crate::ui::Route::Projects);
    guard
}
