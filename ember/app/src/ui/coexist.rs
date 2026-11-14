//! Shared UI and live state for the `compose-rust` UI migration (#220, INTENT D9).
//!
//! Base screens move from `dioxus-compose` to `compose-rust` one PR at a time. The two toolkits
//! run in separate windows (option C, decided after compose-rust FR-53), so they never share a
//! runtime. What they share is state: [`SharedUi`] and [`SharedLive`] hold the route, prefs,
//! theme and live revisions in `compose-rust` `MutableState`s outside either runtime.
//!
//! TRANSITIONAL: the Dioxus `SyncSignal` mirroring ([`DioxusMirrors`], [`use_shared_mirrors`])
//! only keeps the still-Dioxus screens in step with the shared store. Delete it once every
//! screen has moved (tracked in #226).

use std::sync::{Arc, OnceLock};

use compose_rust::runtime::{mutable_state_of, MutableState};
use dioxus_compose::prelude::*;
use ember_client::api::UsageSummary;
use ember_client::wire::{DailyUsage, DetectedAgent};

use crate::bridge::Live;
use crate::model::{self, RowContext};
use crate::prefs::Prefs;
use crate::server::{Account, ChatGptAccount};
use crate::services::{save_prefs, services, try_services};
use crate::ui::editor::EditorTarget;
use crate::ui::shell::NavMenu;
use crate::ui::theme::ThemePref;
use crate::ui::{initial_menu, Route};

// ---------------------------------------------------------------------------------------------
// Shared live and UI state (`compose-rust` MutableState + Dioxus SyncSignal bridge)
// ---------------------------------------------------------------------------------------------

/// Live client and server state observed by `compose-rust` composables (#220).
///
/// Every field is a [`MutableState`], so it is `Clone + Send + Sync + 'static` and can be
/// written from `tokio` worker threads in [`crate::bridge`] or read inside any
/// `#[Composable]` function.
#[derive(Clone)]
pub struct SharedLive {
    /// Projects, session records/status, computers, connection.
    pub launcher: MutableState<u64>,
    /// Any transcript.
    pub transcripts: MutableState<u64>,
    /// The message outbox (queued-message indicator).
    pub outbox: MutableState<u64>,
    /// Any team (members, tasks: FR-T7).
    pub teams: MutableState<u64>,
    pub accounts: MutableState<Vec<Account>>,
    /// ChatGPT plan accounts (FR-U4); empty when sign-in is off.
    pub chatgpt: MutableState<Vec<ChatGptAccount>>,
    pub agents: MutableState<Vec<DetectedAgent>>,
    /// The 다시 찾기 state of the 에이전트 screen (FR-L21).
    pub agents_rescan: MutableState<crate::ui::agents::Rescan>,
    /// When the agent detection last answered (ms since epoch); `None` before the first answer.
    pub agents_checked_ms: MutableState<Option<u64>>,
    /// Global new-member defaults (`GET /api/agents/defaults`, FR-L20, #481, #670).
    pub agent_defaults: MutableState<crate::server::AgentDefaults>,
    /// Revision counter incremented when server-stored agent defaults or settings change.
    pub agent_store_rev: MutableState<u64>,
    /// Tokens per account per UTC day, last 14 days (FR-U7); empty until loaded.
    pub usage: MutableState<Vec<DailyUsage>>,
    /// Tokens per UTC day over the last 14 days, all harnesses (FR-U7, #332).
    pub usage_summary: MutableState<crate::ui::usage::Load<UsageSummary>>,
    /// The Home GitHub card's last answer (FR-L35).
    pub github: MutableState<crate::ui::home::GithubFetch>,
    /// Today's tokens per project, keyed by project name (the Session status tile, FR-L18).
    pub project_day: MutableState<std::collections::HashMap<String, crate::ui::usage::Load<UsageSummary>>>,
}

impl Default for SharedLive {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedLive {
    pub fn new() -> Self {
        Self {
            launcher: mutable_state_of(0),
            transcripts: mutable_state_of(0),
            outbox: mutable_state_of(0),
            teams: mutable_state_of(0),
            accounts: mutable_state_of(Vec::new()),
            chatgpt: mutable_state_of(Vec::new()),
            agents: mutable_state_of(Vec::new()),
            agents_rescan: mutable_state_of(Default::default()),
            agents_checked_ms: mutable_state_of(None),
            agent_defaults: mutable_state_of(Default::default()),
            agent_store_rev: mutable_state_of(0),
            usage: mutable_state_of(Vec::new()),
            usage_summary: mutable_state_of(Default::default()),
            github: mutable_state_of(Default::default()),
            project_day: mutable_state_of(Default::default()),
        }
    }

    pub fn bump(state: &MutableState<u64>) {
        state.update(|v| *v += 1);
    }

    pub fn bump_all(&self) {
        Self::bump(&self.launcher);
        Self::bump(&self.transcripts);
        Self::bump(&self.outbox);
        Self::bump(&self.teams);
    }
}

/// TRANSITIONAL (delete with #226). Dioxus `SyncSignal` mirrors registered when a Dioxus `VirtualDom` mounts [`crate::ui::app`],
/// so mutations made through [`SharedUi`] from a `compose-rust` composable immediately wake
/// any mounted Dioxus screen as well.
#[derive(Clone, Copy)]
pub struct DioxusMirrors {
    pub route: SyncSignal<Route>,
    pub prefs: SyncSignal<Prefs>,
    pub selected_project: SyncSignal<Option<String>>,
    pub new_session: SyncSignal<Option<String>>,
    pub show_archived: SyncSignal<bool>,
    pub menu: SyncSignal<NavMenu>,
    pub editor: SyncSignal<Option<EditorTarget>>,
    pub theme_pref: SyncSignal<ThemePref>,
}

/// UI-wide navigation, preference and dialog state observed by `compose-rust` composables
/// and synchronized with Dioxus [`crate::ui::Ui`] (#220).
#[derive(Clone)]
pub struct SharedUi {
    pub live: SharedLive,
    pub route: MutableState<Route>,
    pub prefs: MutableState<Prefs>,
    /// The project selected on the main screen.
    pub selected_project: MutableState<Option<String>>,
    /// `Some(project)` while the new-session dialog is open (empty: no preset).
    pub new_session: MutableState<Option<String>>,
    pub show_archived: MutableState<bool>,
    /// The menu chosen in the shell (FR-L10). A cold start lands on Home (D26).
    pub menu: MutableState<NavMenu>,
    /// `Some` while the Ember 에디터 screen is open (FR-L31); it replaces the shell.
    pub editor: MutableState<Option<EditorTarget>>,
    /// System / Light / Dark choice (FR-L28).
    pub theme_pref: MutableState<ThemePref>,
    /// The latest message for the user, posted from any thread (see `cr::notice`).
    pub notice: MutableState<Option<(u64, String)>>,
    /// A phone screen shows a detail view (an open room): the shell hides its top-right menu
    /// there (`body.view-detail .phonemenu { display: none }`).
    pub detail_open: MutableState<bool>,
    /// `Some(project)` while the team character screen is open (FR-L22); empty means the
    /// screen picks the project. Not a menu: it opens from the project settings sheet and
    /// replaces the content with a back button (INTENT D26, #503).
    pub characters: MutableState<Option<String>>,
    /// `true` when this process was launched directly with `--editor <project> <folder>`, so
    /// closing the editor exits the process instead of leaving a blank window.
    pub standalone_editor: MutableState<bool>,
    /// The owner claim (FR-S12, #842), written by the claim driver from a worker. Holds no key.
    pub owner_claim: MutableState<crate::owner_claim::ClaimView>,
    /// The claim prompt the user dismissed (`owner_claim::prompt_key`); a new attempt, a new
    /// pending claim or a 403 `owner_required` opens the sheet again.
    pub owner_claim_dismissed: MutableState<Option<String>>,
    /// The update check (NFR-R2, #880), written by `self_update` from a worker.
    pub self_update: MutableState<crate::self_update::UpdateState>,
    mirrors: Arc<OnceLock<DioxusMirrors>>,
}

impl SharedUi {
    pub fn new(initial_prefs: Prefs) -> Self {
        let last_project = initial_prefs.last_project.clone();
        let theme = initial_prefs.theme;
        Self {
            live: SharedLive::new(),
            route: mutable_state_of(Route::Projects),
            prefs: mutable_state_of(initial_prefs),
            selected_project: mutable_state_of(last_project),
            new_session: mutable_state_of(None),
            show_archived: mutable_state_of(false),
            menu: mutable_state_of(initial_menu()),
            editor: mutable_state_of(None),
            theme_pref: mutable_state_of(theme),
            notice: mutable_state_of(None),
            detail_open: mutable_state_of(false),
            characters: mutable_state_of(None),
            standalone_editor: mutable_state_of(false),
            owner_claim: mutable_state_of(Default::default()),
            owner_claim_dismissed: mutable_state_of(None),
            self_update: mutable_state_of(crate::self_update::UpdateState::NotInstalled),
            mirrors: Arc::new(OnceLock::new()),
        }
    }

    /// Register the Dioxus `SyncSignal` mirrors from the root Dioxus component.
    pub fn register_mirrors(&self, mirrors: DioxusMirrors) {
        let _ = self.mirrors.set(mirrors);
    }

    pub fn mirrors(&self) -> Option<DioxusMirrors> {
        self.mirrors.get().copied()
    }

    /// Navigate, opening/closing the conversation in the client as the route enters/leaves it
    /// (FR-S6, FR-L2), and keeping both `MutableState`s and Dioxus signals in step.
    pub fn go(&self, to: Route) {
        let from = self.route.get_untracked();
        if from == to {
            return;
        }
        if let Some(s) = try_services() {
            if let Route::Conversation(old) = &from {
                s.close_session(old);
            }
            if let Route::Conversation(id) = &to {
                s.open_session(id);
                let project = s
                    .client
                    .read(|st| st.session(id).map(|v| v.record.project.clone()));
                if let Some(p) = project {
                    self.set_selected_project(Some(p));
                }
            }
        }
        let next_menu = match &to {
            Route::Projects | Route::Search => NavMenu::Projects,
            Route::Conversation(_) => NavMenu::Sessions,
        };
        self.set_menu(next_menu);
        self.route.set(to.clone());
        if let Some(mut m) = self.mirrors() {
            m.route.set(to);
        }
    }

    pub fn open_conversation(&self, id: &str) {
        self.go(Route::Conversation(id.to_string()));
    }

    pub fn set_menu(&self, menu: NavMenu) {
        let prev = self.menu.get_untracked();
        self.menu.set(menu);
        if let Some(mut m) = self.mirrors() {
            m.menu.set(menu);
        }
        if menu == NavMenu::Home && prev != NavMenu::Home && try_services().is_some() {
            crate::bridge::request_github_summary();
        }
    }

    pub fn set_selected_project(&self, project: Option<String>) {
        self.selected_project.set(project.clone());
        if let Some(mut m) = self.mirrors() {
            m.selected_project.set(project);
        }
    }

    pub fn set_show_archived(&self, show: bool) {
        self.show_archived.set(show);
        if let Some(mut m) = self.mirrors() {
            m.show_archived.set(show);
        }
    }

    pub fn set_theme_pref(&self, pref: ThemePref) {
        self.theme_pref.set(pref);
        if let Some(mut m) = self.mirrors() {
            m.theme_pref.set(pref);
        }
        self.update_prefs(|p| p.theme = pref);
    }

    /// Change per-device prefs and persist them when `Services` is installed.
    pub fn update_prefs(&self, f: impl FnOnce(&mut Prefs)) {
        let mut snapshot = self.prefs.get_untracked();
        f(&mut snapshot);
        self.prefs.set(snapshot.clone());
        if let Some(mut m) = self.mirrors() {
            m.prefs.set(snapshot.clone());
        }
        if try_services().is_some() {
            save_prefs(snapshot);
        }
    }

    /// Open the Ember 에디터 for a project in its own window (FR-L7, FR-L31).
    /// TODO(dioxus-compose#152): once the `window` element on a shared host lands, open the
    /// editor window in the same process instead of spawning `--editor <project> <folder>`.
    pub fn open_editor(&self, project: String, folder: String) {
        self.open_editor_connection(project, folder, None, None, None, None);
    }

    /// Open an editor with the remote connection when one is available.
    pub fn open_editor_connection(
        &self,
        project: String,
        folder: String,
        endpoint: Option<String>,
        token: Option<String>,
        server_url: Option<String>,
        computer: Option<String>,
    ) {
        let target = EditorTarget {
            project: project.clone(),
            folder: folder.clone(),
            endpoint: endpoint.unwrap_or_default(),
            token: token.unwrap_or_default(),
            server_url: server_url.unwrap_or_default(),
            computer: computer.unwrap_or_default(),
        };
        #[cfg(not(test))]
        let spawn_target = target.clone();
        self.editor.set(Some(target.clone()));
        if let Some(mut mirrors) = self.mirrors() {
            mirrors.editor.set(Some(target));
        }
        if crate::services::try_services().is_some() {
            #[cfg(not(test))]
            let notice = crate::ide::spawn_editor_notice(&project, &folder, move |_, _| {
                let exe = std::env::current_exe()?;
                crate::ide::spawn_editor_target(&exe, &spawn_target).map(|_| ())
            });
            #[cfg(test)]
            let notice = crate::ide::spawn_editor_notice(&project, &folder, |_, _| Ok(()));
            crate::ui::cr::notice::post(notice);
        }
    }

    /// Testable core of [`SharedUi::open_editor`]: records the [`EditorTarget`] and, when
    /// `Services` is installed, runs `spawner(&project, &folder)` and posts the corresponding
    /// user notice.
    pub fn open_editor_with(
        &self,
        project: String,
        folder: String,
        spawner: impl FnOnce(&str, &str) -> std::io::Result<()>,
    ) -> Option<String> {
        let target = Some(EditorTarget {
            project: project.clone(),
            folder: folder.clone(),
            ..Default::default()
        });
        self.editor.set(target.clone());
        if let Some(mut m) = self.mirrors() {
            m.editor.set(target);
        }
        if crate::services::try_services().is_some() {
            let notice = crate::ide::spawn_editor_notice(&project, &folder, spawner);
            crate::ui::cr::notice::post(notice.clone());
            // The text that was posted: the shared notice signal is written by worker threads too, so a
            // reader of the signal can see someone else's message instead.
            return Some(notice);
        }
        None
    }

    pub fn close_editor(&self) {
        self.close_editor_with(|| std::process::exit(crate::shutdown()));
    }

    /// Close the Ember 에디터 screen; when this process was launched directly with
    /// `--editor <project> <folder>` (`standalone_editor` is true), closing the editor exits the
    /// window process instead of leaving a blank window behind.
    pub fn close_editor_with(&self, on_exit_standalone: impl FnOnce()) {
        let standalone = self.standalone_editor.get_untracked();
        self.editor.set(None);
        if let Some(mut m) = self.mirrors() {
            m.editor.set(None);
        }
        if standalone {
            on_exit_standalone();
        }
    }

    pub fn open_characters(&self, project: &str) {
        self.characters.set(Some(project.to_string()));
    }

    pub fn close_characters(&self) {
        self.characters.set(None);
    }

    pub fn open_new_session(&self, project: Option<String>) {
        let val = Some(project.unwrap_or_default());
        self.new_session.set(val.clone());
        if let Some(mut m) = self.mirrors() {
            m.new_session.set(val);
        }
    }

    pub fn close_new_session(&self) {
        self.new_session.set(None);
        if let Some(mut m) = self.mirrors() {
            m.new_session.set(None);
        }
    }

    /// Read the live data a session row needs (subscribing the calling composable to it,
    /// previews included) and build rows with `f`.
    pub fn with_rows<R>(&self, f: impl FnOnce(&ember_client::State, &RowContext<'_>) -> R) -> R {
        let _ = self.live.transcripts.get();
        self.with_rows_no_previews(f)
    }

    /// Like [`SharedUi::with_rows`], without redrawing on transcript changes.
    pub fn with_rows_no_previews<R>(
        &self,
        f: impl FnOnce(&ember_client::State, &RowContext<'_>) -> R,
    ) -> R {
        let _ = self.live.launcher.get();
        let accounts = self.live.accounts.get();
        let labels = model::account_labels(&accounts);
        let cx = RowContext {
            account_labels: &labels,
        };
        services().client.read(|s| f(s, &cx))
    }
}

/// Reach the shared UI state from any `compose-rust` composable or handler.
pub fn shared_ui() -> &'static SharedUi {
    &services().ui
}

/// Synchronize Dioxus signals into [`SharedUi`] and wire [`Live`] when [`crate::ui::app`] runs.
pub fn use_shared_mirrors(
    live: Live,
    route: SyncSignal<Route>,
    prefs: SyncSignal<Prefs>,
    selected_project: SyncSignal<Option<String>>,
    new_session: SyncSignal<Option<String>>,
    show_archived: SyncSignal<bool>,
    menu: SyncSignal<NavMenu>,
    editor: SyncSignal<Option<EditorTarget>>,
    theme_pref: SyncSignal<ThemePref>,
) {
    use_hook(move || {
        let Some(s) = try_services() else { return };
        s.ui.register_mirrors(DioxusMirrors {
            route,
            prefs,
            selected_project,
            new_session,
            show_archived,
            menu,
            editor,
            theme_pref,
        });
        let _ = live;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use compose_rust::protocol::{decode_batch, Mutation};
    use compose_rust::schema::PropertyKind;
    use compose_rust::ui as cu;
    use compose_rust::{Host, Recomposer};

    #[test]
    fn editor_connection_is_retained_in_target() {
        let ui = SharedUi::new(Prefs::default());
        ui.open_editor_connection(
            "project".into(),
            "/work".into(),
            Some("localhost:8000".into()),
            Some("secret".into()),
            Some("http://srv:1".into()),
            Some("node-a".into()),
        );
        let target = ui.editor.get_untracked().unwrap();
        assert_eq!((target.server_url.as_str(), target.computer.as_str()), ("http://srv:1", "node-a"));
        assert_eq!(target.endpoint, "localhost:8000");
        assert_eq!(target.token, "secret");
    }

    #[test]
    fn shared_ui_tracks_route_editor_and_theme_transitions() {
        let ui = SharedUi::new(Prefs::default());
        assert_eq!(ui.menu.get_untracked(), NavMenu::Home);
        assert_eq!(ui.route.get_untracked(), Route::Projects);
        assert_eq!(ui.editor.get_untracked(), None);
        assert_eq!(ui.theme_pref.get_untracked(), ThemePref::System);

        ui.open_conversation("sess-42");
        assert_eq!(
            ui.route.get_untracked(),
            Route::Conversation("sess-42".to_string())
        );
        assert_eq!(ui.menu.get_untracked(), NavMenu::Sessions);

        ui.go(Route::Search);
        assert_eq!(ui.route.get_untracked(), Route::Search);
        assert_eq!(ui.menu.get_untracked(), NavMenu::Projects);

        ui.open_editor("ember".to_string(), "/workspace/ember".to_string());
        assert_eq!(
            ui.editor.get_untracked(),
            Some(EditorTarget {
                project: "ember".to_string(),
                folder: "/workspace/ember".to_string(),
                ..Default::default()
            })
        );
        ui.close_editor();
        assert_eq!(ui.editor.get_untracked(), None);

        ui.set_theme_pref(ThemePref::Dark);
        assert_eq!(ui.theme_pref.get_untracked(), ThemePref::Dark);
        assert_eq!(ui.prefs.get_untracked().theme, ThemePref::Dark);

        // Standalone `--editor` mode exits when `close_editor_with` is called; normal mode does not.
        let mut exited = false;
        ui.close_editor_with(|| exited = true);
        assert!(!exited);
        ui.standalone_editor.set(true);
        ui.close_editor_with(|| exited = true);
        assert!(exited);
        ui.standalone_editor.set(false);
    }

    #[test]
    fn fr_l31_open_editor_with_spawns_and_posts_success_or_error_notice() {
        let _guard = crate::services::test_guard();
        let ui = shared_ui();
        let mut spawned: Option<(String, String)> = None;
        let posted = ui.open_editor_with("demo-project".into(), "/work/demo".into(), |p, f| {
            spawned = Some((p.to_string(), f.to_string()));
            Ok(())
        });
        assert_eq!(
            spawned,
            Some(("demo-project".to_string(), "/work/demo".to_string()))
        );
        assert_eq!(posted, Some("Ember 에디터: 새 창에서 여는 중입니다".to_string()));

        let posted = ui
            .open_editor_with("demo-project".into(), "/work/demo".into(), |_, _| {
                Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no binary"))
            })
            .unwrap_or_default();
        assert!(
            posted.starts_with("Ember 에디터: 열지 못했습니다:"),
            "unexpected error notice: {posted}"
        );
        ui.close_editor();
    }

    fn has_text(mutations: &[Mutation<'_>], expected: &str) -> bool {
        mutations.iter().any(|m| {
            matches!(
                m,
                Mutation::SetProp {
                    property: PropertyKind::Text,
                    value: compose_rust::protocol::PropertyValue::String(s),
                    ..
                } if *s == expected
            ) || matches!(m, Mutation::SetText { text, .. } if *text == expected)
                || matches!(m, Mutation::AppendText { text, .. } if *text == expected)
        })
    }

    #[test]
    fn shared_live_bumps_recompose_compose_rust_tree() {
        let live = SharedLive::new();
        let launcher = live.launcher.clone();

        let mut host = Host::new(move || {
            let launcher = launcher.clone();
            Box::new(Recomposer::new(move || {
                let rev = launcher.get();
                cu::Text(format!("rev:{rev}"));
            }))
        });

        let initial = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        assert!(has_text(&initial, "rev:0"), "initial batch: {initial:?}");

        SharedLive::bump(&live.launcher);
        let updated = decode_batch(host.render_frame(16_000_000).expect("render")).expect("decode");
        assert!(
            has_text(&updated, "rev:1") || has_text(&updated, "1"),
            "updated batch: {updated:?}"
        );
    }
}
