//! The screens. Root component: [`app`].
//!
//! Layout adapts per window size class through dioxus-compose's `Navigation`, which the
//! renderer draws as a bottom bar (compact), a rail (medium) or a sidebar (expanded) from one
//! declaration; the screens themselves branch on `use_window_size()` only where their content
//! changes shape (two panes side by side vs. stacked).

pub mod compat;
pub mod conversation;
pub mod launcher;
pub mod new_session;
pub mod search;

use std::collections::HashMap;

use dioxus_compose::prelude::*;

use crate::bridge::{use_bridge, Live};
use crate::model::{self, RowContext, SessionRow};
use crate::prefs::Prefs;
use crate::services::{save_prefs, services};

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
            let project = services().client.read(|s| s.session(id).map(|v| v.record.project.clone()));
            if let Some(p) = project {
                let mut sel = self.selected_project;
                sel.set(Some(p));
            }
        }
        route.set(to);
    }

    pub fn open_conversation(&self, id: &str) {
        self.go(Route::Conversation(id.to_string()));
    }

    /// Change prefs and persist them.
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
    pub fn with_rows_no_previews<R>(&self, f: impl FnOnce(&ember_client::State, &RowContext<'_>) -> R) -> R {
        let _ = *self.live.launcher.read();
        let prefs = self.prefs.read();
        let session_accounts = self.live.session_accounts.read();
        let labels = model::account_labels(&self.live.accounts.read());
        let cx = RowContext { prefs: &prefs, session_accounts: &session_accounts, account_labels: &labels };
        services().client.read(|s| f(s, &cx))
    }
}

pub fn use_ui() -> Ui {
    use_context::<Ui>()
}

/// How many conversations stand in the navigation (a bottom bar holds about five).
const NAV_CONVERSATIONS: usize = 5;

pub fn app() -> Element {
    let live = Live::use_live();
    use_bridge(live);
    let route = use_signal(|| Route::Projects);
    let prefs = use_signal(|| services().initial_prefs.clone());
    let selected_project = use_signal(|| services().initial_prefs.last_project.clone());
    let new_session = use_signal(|| None::<String>);
    let show_archived = use_signal(|| false);
    let ui = use_context_provider(|| Ui { live, route, prefs, selected_project, new_session, show_archived });

    let current = route();
    let mut nav: Vec<SessionRow> = ui.with_rows_no_previews(|s, cx| model::recent_sessions(s, cx, NAV_CONVERSATIONS));
    // The open conversation is always a destination, even if it is not among the recent ones
    // (opened from search, or archived), so the navigation can mark it.
    if let Route::Conversation(id) = &current {
        if !nav.iter().any(|r| &r.id == id) {
            if let Some(row) = ui.with_rows_no_previews(|s, cx| model::session_row(s, id, cx)) {
                nav.insert(0, row);
            }
        }
    }
    let selected = match &current {
        Route::Projects => 0,
        Route::Search => 1,
        Route::Conversation(id) => nav.iter().position(|r| &r.id == id).map_or(0, |i| i + 2),
    };
    let unread: HashMap<String, bool> = nav
        .iter()
        .map(|r| (r.id.clone(), r.status == ember_client::LauncherStatus::FinishedUnread))
        .collect();

    rsx! {
        Scaffold {
            top_bar: rsx! {
                TopAppBar {
                    fill_max_width: true,
                    Text { text: "Ember", type_role: TypeRole::Title }
                    Spacer { weight: 1.0 }
                    Button {
                        text: "",
                        icon: IconRole::Search,
                        variant: ButtonVariant::Text,
                        on_click: move |_| ui.go(Route::Search),
                    }
                    Button {
                        text: "New session",
                        icon: IconRole::Compose,
                        variant: ButtonVariant::Tonal,
                        on_click: move |_| {
                            let project = ui.selected_project.peek().clone();
                            ui.open_new_session(project);
                        },
                    }
                }
            },
            bottom_bar: rsx! {
                Navigation {
                    selected_index: selected,
                    NavigationItem {
                        text: "Projects",
                        icon: IconRole::Home,
                        on_click: move |()| ui.go(Route::Projects),
                    }
                    NavigationItem {
                        text: "Search",
                        icon: IconRole::Search,
                        on_click: move |()| ui.go(Route::Search),
                    }
                    for row in nav.iter().cloned() {
                        NavigationItem {
                            key: "{row.id}",
                            // TODO(dioxus-compose M9, ~10-08): a badge on the destination
                            // instead of the bullet for "finished, unread".
                            text: if unread.get(&row.id).copied().unwrap_or(false) {
                                format!("\u{2022} {}", row.title)
                            } else {
                                row.title.clone()
                            },
                            icon: if model::is_busy(row.status) { IconRole::History } else { IconRole::Inbox },
                            section: "Conversations",
                            on_click: {
                                let id = row.id.clone();
                                move |()| ui.open_conversation(&id)
                            },
                        }
                    }
                }
            },

            match current {
                Route::Projects => rsx! { launcher::Launcher {} },
                Route::Search => rsx! { search::SearchScreen {} },
                Route::Conversation(id) => rsx! { conversation::ConversationView { key: "{id}", id: id.clone() } },
            }

            new_session::NewSessionDialog {}
        }
    }
}
