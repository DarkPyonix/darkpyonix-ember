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
    pub outbox: Arc<Mutex<Outbox>>,
    /// Prefs as read from disk at start-up; the UI owns them from then on.
    pub initial_prefs: crate::prefs::Prefs,
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
        self.enter(|| self.client.open_session(id));
    }

    pub fn close_session(&self, id: &str) {
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
    /// Tell the UI the outbox changed (queued-message indicator).
    pub fn bump_outbox(&self) {
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

/// Persist prefs off the UI thread.
pub fn save_prefs(prefs: crate::prefs::Prefs) {
    let Some(path) = services().config.prefs_path() else { return };
    services().rt.spawn_blocking(move || {
        if let Err(e) = crate::prefs::save(&path, &prefs) {
            tracing::warn!("could not save prefs to {}: {e}", path.display());
        }
    });
}
