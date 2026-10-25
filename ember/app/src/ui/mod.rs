//! The screens. Root component: [`app`].
//!
//! The shell ([`shell`]) draws the navigation: a sidebar, a rail or floating bottom tabs per
//! window size class (FR-L10, FR-L28). The screens branch on `use_window_size()` only where
//! their content changes shape (two panes side by side vs. stacked). Tokens are in [`theme`],
//! shared components in [`components`].

pub mod compat;
pub mod agents;
pub mod components;
pub mod conversation;
pub mod home;
pub mod launcher;
pub mod new_session;
pub mod project;
pub mod search;
pub mod shell;
pub mod theme;

use dioxus_compose::prelude::*;

use crate::bridge::{use_bridge, Live};
use crate::model::{self, RowContext};
use crate::prefs::Prefs;
use crate::services::{save_prefs, services};
use crate::ui::shell::NavMenu;
use crate::ui::theme::ThemeCtx;

/// Where the app is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// The main screen: projects, their sessions, computers (FR-L1–L4).
    Projects,
    /// Search across sessions (FR-L9).
    Search,
    /// One conversation (FR-L5–L7).
    Conversation(String),
}

/// UI-wide state, provided as context by [`app`]. All signals, so it is `Copy`.
#[derive(Clone, Copy)]
pub struct Ui {
    pub live: Live,
    pub route: Signal<Route>,
    pub prefs: Signal<Prefs>,
    /// The project selected on the main screen.
    pub selected_project: Signal<Option<String>>,
    /// `Some(project)` while the new-session dialog is open (empty: no preset).
    pub new_session: Signal<Option<String>>,
    pub show_archived: Signal<bool>,
    /// The menu chosen in the shell (FR-L10). A cold start lands on Projects.
    pub menu: Signal<NavMenu>,
}

impl Ui {
    /// Navigate, opening/closing the conversation in the client as the route enters/leaves it
    /// (the client renews an open session's lease and marks it seen: FR-S6, FR-L2).
    pub fn go(&self, to: Route) {
        let mut route = self.route;
        let from = route.peek().clone();
        if from == to {
            return;
        }
        if let Route::Conversation(old) = &from {
            services().close_session(old);
        }
        if let Route::Conversation(id) = &to {
            services().open_session(id);
            let project = services()
                .client
                .read(|s| s.session(id).map(|v| v.record.project.clone()));
            if let Some(p) = project {
                let mut sel = self.selected_project;
                sel.set(Some(p));
            }
        }
        // Each route belongs to a menu, so the shell marks the right item however it was reached.
        let mut menu = self.menu;
        match &to {
            Route::Projects | Route::Search => menu.set(NavMenu::Projects),
            Route::Conversation(_) => menu.set(NavMenu::Sessions),
        }
        route.set(to);
    }

    pub fn open_conversation(&self, id: &str) {
        self.go(Route::Conversation(id.to_string()));
    }

    /// Change per-device prefs and persist them.
    pub fn update_prefs(&self, f: impl FnOnce(&mut Prefs)) {
        let mut prefs = self.prefs;
        let snapshot = {
            let mut w = prefs.write();
            f(&mut w);
            w.clone()
        };
        save_prefs(snapshot);
    }

    pub fn open_new_session(&self, project: Option<String>) {
        let mut d = self.new_session;
        d.set(Some(project.unwrap_or_default()));
    }

    /// Read the live data a session row needs (subscribing the caller to it, previews
    /// included) and build rows with `f`.
    pub fn with_rows<R>(&self, f: impl FnOnce(&ember_client::State, &RowContext<'_>) -> R) -> R {
        let _ = *self.live.transcripts.read();
        self.with_rows_no_previews(f)
    }

    /// Like [`Ui::with_rows`], without redrawing on transcript changes (a streaming reply
    /// would otherwise redraw the caller on every delta). Previews may be stale.
    pub fn with_rows_no_previews<R>(
        &self,
        f: impl FnOnce(&ember_client::State, &RowContext<'_>) -> R,
    ) -> R {
        let _ = *self.live.launcher.read();
        let labels = model::account_labels(&self.live.accounts.read());
        let cx = RowContext {
            account_labels: &labels,
        };
        services().client.read(|s| f(s, &cx))
    }
}

/// The menu a cold start lands on: 홈 is the first menu (D26).
pub fn initial_menu() -> NavMenu {
    NavMenu::Home
}

pub fn use_ui() -> Ui {
    use_context::<Ui>()
}

pub fn app() -> Element {
    let live = Live::use_live();
    use_bridge(live);
    let route = use_signal(|| Route::Projects);
    let prefs = use_signal(|| services().initial_prefs.clone());
    let selected_project = use_signal(|| services().initial_prefs.last_project.clone());
    let new_session = use_signal(|| None::<String>);
    let show_archived = use_signal(|| false);
    let menu = use_signal(initial_menu);
    let theme_pref = use_signal(|| services().initial_prefs.theme);
    use_context_provider(|| ThemeCtx { pref: theme_pref });
    use_context_provider(|| Ui {
        live,
        route,
        prefs,
        selected_project,
        new_session,
        show_archived,
        menu,
    });

    rsx! { shell::Shell {} }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_l10_cold_start_lands_on_home() {
        assert_eq!(initial_menu(), NavMenu::Home);
        assert_eq!(NavMenu::ALL[0], NavMenu::Home);
    }
}
