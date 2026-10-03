//! Client state → UI reactivity.
//!
//! The UI never copies the client's [`State`](ember_client::State) into signals. Screens read
//! it directly with `Client::read` while rendering, and subscribe to a few **revision
//! signals** that say *which part* changed. Those are sync signals (`use_signal_sync`), written
//! from tokio worker threads; a write marks the reading scopes dirty and dioxus-compose's host
//! requests a frame (the same mechanism the chat sample uses for its streamed reply).
//!
//! - [`Client::subscribe`] drives targeted invalidation from each [`Changes`]: the launcher
//!   (projects, session records/status, computers, connection) and transcripts are separate,
//!   so a streaming reply does not redraw the project grid.
//! - [`Client::watch_revision`] is the catch-all: it never lags, so whenever the broadcast
//!   receiver reports `Lagged` (changes were dropped), the next revision redraws everything.
//!   It also covers the window between `Client::start` (before the window opened) and the
//!   first subscription here.
//!
//! The same tasks refresh what the client crate does not sync yet (accounts, agents, computer
//! reachability) and deliver queued messages at turn boundaries. Session metadata, accounts
//! per session and project assignments arrive with the client's own sync (records and push).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use dioxus_compose::prelude::*;
use tokio::sync::{broadcast::error::RecvError, Notify};

use ember_client::state::Input;
use ember_client::wire::DetectedAgent;
use ember_client::{Changes, LauncherStatus};

use crate::model::merge_computers;
use crate::server::Account;
use crate::services::{services, Services};

/// How often accounts, agents and computer reachability are refreshed.
pub const REFRESH_EVERY: Duration = Duration::from_secs(30);

/// The UI's view of live data. All fields are sync signals, so the struct is `Copy` and can
/// be written from worker threads.
#[derive(Clone, Copy)]
pub struct Live {
    /// Projects, session records/status, computers, connection.
    pub launcher: SyncSignal<u64>,
    /// Any transcript.
    pub transcripts: SyncSignal<u64>,
    /// The message outbox (queued-message indicator).
    pub outbox: SyncSignal<u64>,
    pub accounts: SyncSignal<Vec<Account>>,
    pub agents: SyncSignal<Vec<DetectedAgent>>,
}

impl Live {
    /// Create the signals. Call once, in the root component.
    pub fn use_live() -> Live {
        Live {
            launcher: use_signal_sync(|| 0),
            transcripts: use_signal_sync(|| 0),
            outbox: use_signal_sync(|| 0),
            accounts: use_signal_sync(Vec::new),
            agents: use_signal_sync(Vec::new),
        }
    }

    fn bump(mut sig: SyncSignal<u64>) {
        *sig.write() += 1;
    }

    fn bump_all(&self) {
        Self::bump(self.launcher);
        Self::bump(self.transcripts);
        Self::bump(self.outbox);
    }
}

/// Start the bridge tasks once, from the root component.
pub fn use_bridge(live: Live) {
    use_hook(move || {
        let s = services();
        if s.live.set(live).is_err() {
            // Already running (a second root mount); the first set of tasks stays.
            return;
        }
        let refresh = Arc::new(Notify::new());
        s.spawn(changes_task(s, live, refresh.clone()));
        s.spawn(refresh_task(s, live, refresh));
    });
}

async fn changes_task(s: &'static Services, live: Live, refresh: Arc<Notify>) {
    let mut changes = s.client.subscribe();
    let mut revision = s.client.watch_revision();
    // Anything that changed before this subscription existed.
    live.bump_all();
    let mut lagged = false;
    let mut prints = Fingerprints::default();
    loop {
        tokio::select! {
            r = changes.recv() => match r {
                Ok(ch) => on_changes(s, live, &ch, &mut prints),
                Err(RecvError::Lagged(n)) => {
                    tracing::debug!("ui bridge lagged by {n} changes; redrawing everything");
                    lagged = true;
                    live.bump_all();
                }
                Err(RecvError::Closed) => return,
            },
            r = revision.changed() => {
                if r.is_err() {
                    return;
                }
                if lagged {
                    // Changes were dropped: the revision is the only reliable signal now.
                    lagged = false;
                    live.bump_all();
                    refresh.notify_one();
                    let open: Vec<String> = s.client.read(|st| st.open_sessions().cloned().collect());
                    for id in open {
                        s.drive_outbox(&id);
                    }
                }
            }
        }
    }
}

/// What the launcher shows of each session, coarsely. Every streamed delta changes a
/// session's `last_seq` and `updated_at`, which would redraw the launcher and the navigation
/// many times a second; they only need to redraw when this changes.
#[derive(Default)]
struct Fingerprints(HashMap<String, Fingerprint>);

/// Status, title, project, pinned, archived, coarse activity time.
type Fingerprint = (LauncherStatus, String, String, bool, bool, i64);

/// Activity time granularity for the launcher (ordering and "5m ago").
const LAUNCHER_TIME_STEP_MS: i64 = 30_000;

impl Fingerprints {
    /// Whether any of `ids` looks different to the launcher now.
    fn changed<'a>(&mut self, s: &Services, ids: impl IntoIterator<Item = &'a String>) -> bool {
        let mut changed = false;
        s.client.read(|st| {
            for id in ids {
                let print = st.session(id).map(|v| {
                    let r = v.record;
                    (v.status, r.title.clone(), r.project.clone(), r.pinned, r.archived, r.updated_at / LAUNCHER_TIME_STEP_MS)
                });
                match print {
                    Some(p) => {
                        if self.0.get(id) != Some(&p) {
                            self.0.insert(id.clone(), p);
                            changed = true;
                        }
                    }
                    None => changed |= self.0.remove(id).is_some(),
                }
            }
        });
        changed
    }
}

fn on_changes(s: &'static Services, live: Live, ch: &Changes, prints: &mut Fingerprints) {
    let sessions_changed = prints.changed(s, &ch.sessions);
    // A project change without session changes is the project list itself (a project created
    // or its assignment changed, FR-L4); with session changes the fingerprints decide.
    let projects_changed = ch.projects && ch.sessions.is_empty();
    if sessions_changed || projects_changed || ch.computers || ch.connection {
        Live::bump(live.launcher);
    }
    if !ch.transcripts.is_empty() {
        Live::bump(live.transcripts);
    }
    // Turn boundaries show up as session status changes.
    for id in &ch.sessions {
        s.drive_outbox(id);
    }
}

async fn refresh_task(s: &'static Services, live: Live, refresh: Arc<Notify>) {
    let mut first = true;
    let mut full = true;
    loop {
        refresh_once(s, live, first, full).await;
        first = false;
        full = tokio::select! {
            _ = tokio::time::sleep(REFRESH_EVERY) => true,
            _ = refresh.notified() => {
                // Coalesce bursts (a session list reload notifies many times).
                tokio::time::sleep(Duration::from_millis(300)).await;
                false
            }
        };
    }
}

/// `full`: probe computers and re-read accounts and agents; otherwise (after the bridge
/// lagged) only reload the projects and their assignments, which push keeps current
/// otherwise.
async fn refresh_once(s: &'static Services, live: Live, first: bool, full: bool) {
    if full {
        refresh_slow(s, live, first).await;
    } else if let Err(e) = s.client.load_projects().await {
        tracing::debug!("projects refresh failed: {e}");
    }
}

async fn refresh_slow(s: &'static Services, mut live: Live, first: bool) {
    // Computers: list at once without probing so the list renders before reachability is
    // known (FR-L3), then probe and update in place.
    if first {
        if let Ok(list) = s.server.computers(false).await {
            let merged = s.client.read(|st| merge_computers(st.computers(), &list));
            s.client.apply(Input::ComputersLoaded(merged));
        }
    }
    match s.server.accounts().await {
        Ok(a) => {
            if *live.accounts.peek() != a {
                live.accounts.set(a);
            }
        }
        Err(e) => tracing::debug!("accounts refresh failed: {e}"),
    }
    match s.client.agents().await {
        Ok(a) => {
            if *live.agents.peek() != a {
                live.agents.set(a);
            }
        }
        Err(e) => tracing::debug!("agents refresh failed: {e}"),
    }
    // Probing asks every node and can take a while; last.
    match s.server.computers(true).await {
        Ok(list) => {
            let merged = s.client.read(|st| merge_computers(st.computers(), &list));
            s.client.apply(Input::ComputersLoaded(merged));
        }
        Err(e) => tracing::debug!("computers refresh failed: {e}"),
    }
}
