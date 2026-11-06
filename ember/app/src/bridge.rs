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
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use dioxus_compose::prelude::*;
use tokio::sync::{broadcast::error::RecvError, Notify};

use ember_client::state::Input;
use ember_client::wire::{DailyUsage, DetectedAgent};
use ember_client::{Changes, LauncherStatus};

use crate::model::merge_computers;
use crate::server::{Account, ChatGptAccount};
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
    /// Any team (members, tasks: FR-T7).
    pub teams: SyncSignal<u64>,
    pub accounts: SyncSignal<Vec<Account>>,
    /// ChatGPT plan accounts (FR-U4); empty when sign-in is off.
    pub chatgpt: SyncSignal<Vec<ChatGptAccount>>,
    pub agents: SyncSignal<Vec<DetectedAgent>>,
    /// Tokens per account per UTC day, last 14 days (FR-U7); empty until loaded.
    pub usage: SyncSignal<Vec<DailyUsage>>,
}

impl Live {
    /// Create the signals. Call once, in the root component.
    pub fn use_live() -> Live {
        Live {
            launcher: use_signal_sync(|| 0),
            transcripts: use_signal_sync(|| 0),
            outbox: use_signal_sync(|| 0),
            teams: use_signal_sync(|| 0),
            accounts: use_signal_sync(Vec::new),
            chatgpt: use_signal_sync(Vec::new),
            agents: use_signal_sync(Vec::new),
            usage: use_signal_sync(Vec::new),
        }
    }

    fn bump(mut sig: SyncSignal<u64>) {
        *sig.write() += 1;
    }

    fn bump_all(&self) {
        Self::bump(self.launcher);
        Self::bump(self.transcripts);
        Self::bump(self.outbox);
        Self::bump(self.teams);
        if let Some(s) = crate::services::try_services() {
            s.ui.live.bump_all();
        }
    }
}

static BRIDGE_STARTED: OnceLock<()> = OnceLock::new();

/// Start the runtime bridge tasks once for [`crate::ui::coexist::SharedLive`] (#220), without
/// requiring a Dioxus component tree.
pub fn start_shared() {
    if BRIDGE_STARTED.set(()).is_err() {
        return;
    }
    let s = services();
    let refresh = Arc::new(Notify::new());
    s.spawn(changes_task(s, None, refresh.clone()));
    s.spawn(refresh_task(s, None, refresh));
}

/// Start the bridge tasks once, from the root component.
pub fn use_bridge(live: Live) {
    use_hook(move || {
        let s = services();
        let _ = s.live.set(live);
        if BRIDGE_STARTED.set(()).is_err() {
            // Already running; the first set of tasks stays.
            return;
        }
        let refresh = Arc::new(Notify::new());
        s.spawn(changes_task(s, Some(live), refresh.clone()));
        s.spawn(refresh_task(s, Some(live), refresh));
    });
}

fn bump_all(s: &'static Services, live: Option<Live>) {
    s.ui.live.bump_all();
    if let Some(live) = live.or_else(|| s.live.get().copied()) {
        Live::bump(live.launcher);
        Live::bump(live.transcripts);
        Live::bump(live.outbox);
        Live::bump(live.teams);
    }
}

async fn changes_task(s: &'static Services, live: Option<Live>, refresh: Arc<Notify>) {
    let mut changes = s.client.subscribe();
    let mut revision = s.client.watch_revision();
    // Anything that changed before this subscription existed.
    bump_all(s, live);
    let mut lagged = false;
    let mut prints = Fingerprints::default();
    loop {
        tokio::select! {
            r = changes.recv() => match r {
                Ok(ch) => on_changes(s, live, &ch, &mut prints),
                Err(RecvError::Lagged(n)) => {
                    tracing::debug!("ui bridge lagged by {n} changes; redrawing everything");
                    lagged = true;
                    bump_all(s, live);
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
                    bump_all(s, live);
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

fn on_changes(s: &'static Services, live: Option<Live>, ch: &Changes, prints: &mut Fingerprints) {
    let live = live.or_else(|| s.live.get().copied());
    let sessions_changed = prints.changed(s, &ch.sessions);
    // A project change without session changes is the project list itself (a project created
    // or its assignment changed, FR-L4); with session changes the fingerprints decide.
    let projects_changed = ch.projects && ch.sessions.is_empty();
    if sessions_changed || projects_changed || ch.computers || ch.connection {
        if let Some(live) = live {
            Live::bump(live.launcher);
        }
        crate::ui::coexist::SharedLive::bump(&s.ui.live.launcher);
    }
    if !ch.transcripts.is_empty() {
        if let Some(live) = live {
            Live::bump(live.transcripts);
        }
        crate::ui::coexist::SharedLive::bump(&s.ui.live.transcripts);
    }
    if !ch.teams.is_empty() {
        if let Some(live) = live {
            Live::bump(live.teams);
        }
        crate::ui::coexist::SharedLive::bump(&s.ui.live.teams);
    }
    // Turn boundaries show up as session status changes.
    for id in &ch.sessions {
        s.drive_outbox(id);
    }
}

async fn refresh_task(s: &'static Services, live: Option<Live>, refresh: Arc<Notify>) {
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
async fn refresh_once(s: &'static Services, live: Option<Live>, first: bool, full: bool) {
    if full {
        refresh_slow(s, live, first).await;
    } else if s.demo.is_some() {
        // Demo only (#199): nothing to reload.
    } else if let Err(e) = s.client.load_projects().await {
        tracing::debug!("projects refresh failed: {e}");
    }
}

// ---------------------------------------------------------------------------------------------
// Token usage (FR-U7, #332)
// ---------------------------------------------------------------------------------------------

use std::sync::Mutex;

use crate::ui::usage::Load;
use ember_client::api::{UsagePeriod, UsageSummary};

/// Projects whose day tile was asked for; the slow timer refreshes them.
static WATCHED_PROJECTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Ask for the 14-day summary now (a screen opened). Cheap to call once per mount; the slow
/// timer keeps it current after that.
pub fn request_usage_summary() {
    let s = services();
    s.spawn(async move { fetch_usage_summary(services()).await });
}

/// Minimum gap between two timer reads of the GitHub card (the server caches for 90 s).
const GITHUB_MIN_GAP: Duration = Duration::from_secs(90);
static GITHUB_LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);

/// Whether the timer may read the GitHub card now (FR-G13): only while Home is shown, never in
/// the demo, and not before the server cache time has passed since the last read.
pub fn should_poll_github(route_is_home: bool, last_fetch_age: Option<Duration>, min_gap: Duration, demo: bool) -> bool {
    route_is_home && !demo && last_fetch_age.is_none_or(|a| a >= min_gap)
}

/// Ask for the GitHub card now (Home opened, FR-L35). Not in the demo, which shows a sample.
pub fn request_github_summary() {
    let s = services();
    if s.demo.is_some() {
        return;
    }
    s.spawn(async move { fetch_github(services(), true).await });
}

/// Read `GET /api/integrations/github`. The timer passes `force = false` and is held to the
/// server cache time. A failed call keeps the last answer.
async fn fetch_github(s: &'static Services, force: bool) {
    if s.demo.is_some() {
        return;
    }
    {
        let mut last = GITHUB_LAST.lock().unwrap_or_else(|e| e.into_inner());
        let home = s.ui.menu.get_untracked() == crate::ui::shell::NavMenu::Home;
        if !force && !should_poll_github(home, last.map(|t| t.elapsed()), GITHUB_MIN_GAP, false) {
            return;
        }
        *last = Some(std::time::Instant::now());
    }
    let got = s.client.api().github_summary().await.map_err(|e| e.to_string());
    let old = s.ui.live.github.get_untracked();
    let new = crate::ui::home::GithubFetch::settle(&old, got.map(|g| crate::ui::home::github_state_from(&g)));
    if new != old {
        s.ui.live.github.set(new);
    }
}

/// Run the harness detection again (`POST /api/computers/local/harnesses/detect`, FR-A6.1) and
/// say how it ended in `agents_rescan` (FR-L21). Nothing is reported before the answer arrived.
/// The demo re-reads its canned list and marks the sentence 예시. The sentence returns to
/// the idle button label after a few seconds.
pub fn rescan_agents() {
    use crate::ui::agents::{self as ag, Rescan};
    let s = services();
    if matches!(s.ui.live.agents_rescan.get_untracked(), Rescan::Running) {
        return;
    }
    s.ui.live.agents_rescan.set(Rescan::Running);
    s.spawn(async move {
        let s = services();
        let before = s.ui.live.agents.get_untracked();
        let demo = s.demo.is_some();
        let got: Result<Vec<_>, ag::RescanFailure> = if demo {
            Ok(crate::demo::agents())
        } else {
            match s.client.api().detect_harnesses("local").await {
                Ok(v) => Ok(ag::detect_to_agents(&v)),
                Err(e) => {
                    tracing::debug!("harness detect failed: {e}");
                    Err(ag::rescan_failure(e.status(), e.code()))
                }
            }
        };
        let mut sentence = ag::rescan_sentence(&before, got.as_deref().map_err(|e| *e));
        if demo {
            sentence.push_str(" (예시)");
        }
        if let Ok(fresh) = got {
            let at = if demo { Some(crate::demo::now_ms().max(0) as u64) } else { ag::latest_detected_at(&fresh) };
            if at.is_some() {
                s.ui.live.agents_checked_ms.set(at);
            }
            s.ui.live.agents.set(ag::merge_detected(&before, &fresh));
        }
        s.ui.live.agents_rescan.set(Rescan::Done(sentence.clone()));
        tokio::time::sleep(Duration::from_secs(6)).await;
        if s.ui.live.agents_rescan.get_untracked() == Rescan::Done(sentence) {
            s.ui.live.agents_rescan.set(Rescan::Idle);
        }
    });
}

/// Ask for today's tokens of `project` (the Session status tile). Only the first call per
/// project starts a request; the slow timer refreshes it.
pub fn request_project_day(project: &str) {
    {
        let mut w = WATCHED_PROJECTS.lock().unwrap();
        if w.iter().any(|p| p == project) {
            return;
        }
        w.push(project.to_string());
    }
    let project = project.to_string();
    services().spawn(async move { fetch_project_day(services(), &project).await });
}

/// What to show after a fetch: the new data, or the failure unless older data is held (a
/// refresh that fails keeps the last good figures and logs).
fn settle(old: &Load<UsageSummary>, got: Result<UsageSummary, String>) -> Load<UsageSummary> {
    match got {
        Ok(u) => Load::Ready(u),
        Err(e) if matches!(old, Load::Ready(_)) => {
            tracing::debug!("usage refresh failed: {e}");
            old.clone()
        }
        Err(e) => Load::Failed(e),
    }
}

async fn fetch_usage_summary(s: &'static Services) {
    let got = match &s.demo {
        Some(_) => Ok(crate::demo::usage_summary(crate::demo::now_ms())),
        None => s.client.api().usage_summary(&UsagePeriod::Days(14), None).await.map_err(|e| e.to_string()),
    };
    let old = s.ui.live.usage_summary.get_untracked();
    let new = settle(&old, got);
    if new != old {
        s.ui.live.usage_summary.set(new);
    }
}

async fn fetch_project_day(s: &'static Services, project: &str) {
    let today = crate::ui::home::today_utc();
    let got = match &s.demo {
        Some(_) => {
            let mut u = crate::demo::usage_summary(crate::demo::now_ms());
            u.days.retain(|d| d.day == today);
            Ok(u)
        }
        None => s
            .client
            .api()
            .usage_summary(&UsagePeriod::Day(today), Some(project))
            .await
            .map_err(|e| e.to_string()),
    };
    let old = s.ui.live.project_day.get_untracked().get(project).cloned().unwrap_or_default();
    let new = settle(&old, got);
    if new != old {
        s.ui.live.project_day.update(|m| {
            m.insert(project.to_string(), new);
        });
    }
}

async fn fetch_project_days(s: &'static Services) {
    let projects = WATCHED_PROJECTS.lock().unwrap().clone();
    for p in projects {
        fetch_project_day(s, &p).await;
    }
}

async fn refresh_slow(s: &'static Services, live: Option<Live>, first: bool) {
    let live = live.or_else(|| s.live.get().copied());
    // Demo only (#199): the same signals, filled from the demo source; no request is made.
    if s.demo.is_some() {
        let now = crate::demo::now_ms();
        let acc = crate::demo::accounts();
        let gpt = crate::demo::chatgpt_accounts(now);
        let usg = crate::demo::usage(now);
        let agt = crate::demo::agents();
        s.ui.live.accounts.set(acc.clone());
        s.ui.live.chatgpt.set(gpt.clone());
        s.ui.live.usage.set(usg.clone());
        s.ui.live.agents.set(agt.clone());
        fetch_usage_summary(s).await;
        fetch_project_days(s).await;
        if let Some(mut live) = live {
            live.accounts.set(acc);
            live.chatgpt.set(gpt);
            live.usage.set(usg);
            live.agents.set(agt);
        }
        return;
    }
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
            s.ui.live.accounts.set(a.clone());
            if let Some(mut live) = live {
                if *live.accounts.peek() != a {
                    live.accounts.set(a);
                }
            }
        }
        Err(e) => tracing::debug!("accounts refresh failed: {e}"),
    }
    let g = s.server.chatgpt_accounts().await.unwrap_or_default();
    s.ui.live.chatgpt.set(g.clone());
    if let Some(mut live) = live {
        if *live.chatgpt.peek() != g {
            live.chatgpt.set(g);
        }
    }
    match s.client.api().usage(Some(14)).await {
        Ok(u) => {
            s.ui.live.usage.set(u.clone());
            if let Some(mut live) = live {
                if *live.usage.peek() != u {
                    live.usage.set(u);
                }
            }
        }
        Err(e) => tracing::debug!("usage refresh failed: {e}"),
    }
    fetch_usage_summary(s).await;
    if !first {
        fetch_github(s, false).await;
    }
    fetch_project_days(s).await;
    match s.client.agents().await {
        Ok(a) => {
            s.ui.live.agents_checked_ms
                .set(Some(crate::ui::agents::latest_detected_at(&a).unwrap_or_else(|| crate::demo::now_ms().max(0) as u64)));
            s.ui.live.agents.set(a.clone());
            if let Some(mut live) = live {
                if *live.agents.peek() != a {
                    live.agents.set(a);
                }
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

#[cfg(test)]
mod github_poll_tests {
    use super::*;

    #[test]
    fn fr_g13_github_poll_only_on_home() {
        let gap = Duration::from_secs(90);
        assert!(should_poll_github(true, None, gap, false));
        assert!(should_poll_github(true, Some(Duration::from_secs(100)), gap, false));
        assert!(!should_poll_github(false, Some(Duration::from_secs(100)), gap, false));
        assert!(!should_poll_github(true, Some(Duration::from_secs(100)), gap, true));
        assert!(!should_poll_github(true, Some(Duration::from_secs(10)), gap, false));
    }
}
