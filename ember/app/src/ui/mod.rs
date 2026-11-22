//! The screens. Root component: [`app`].
//!
//! The shell ([`shell`]) draws the navigation: a sidebar, a rail or floating bottom tabs per
//! window size class (FR-L10, FR-L28). The screens branch on `use_window_size()` only where
//! their content changes shape (two panes side by side vs. stacked). Tokens are in [`theme`],
//! shared components in [`components`].

pub mod characters;
pub mod coexist;
pub mod cr;
pub mod compat;
pub mod add_computer;
pub mod agents;
pub mod back;
pub mod channel_owner;
pub mod channels;
pub mod components;
pub mod conversation;
pub mod editor;
pub mod editor_actions;
pub mod editor_agent;
pub(crate) mod editor_debug;
pub(crate) mod editor_debug_view;
pub mod enrol_flow;
pub mod device_scope;
pub mod editor_icons;
pub mod editor_blame;
pub mod editor_terminals;
pub mod editor_notifications;
pub mod editor_extensions;
pub mod editor_scm;
pub mod editor_status;
pub mod editor_scm_repo;
pub mod editor_menu;
pub mod editor_tree_menu;
pub mod editor_states;
pub mod editor_workbench;
pub mod find;
pub mod palette;
pub mod files;
pub mod files_service;
pub mod fonts;
pub mod editor_language;
pub mod editor_search;
pub mod editor_tasks;
pub mod editor_settings;
pub mod home;
pub mod keymap;
pub mod monitor;
pub mod project;
pub mod question;
pub mod routines;
pub mod safe_area;
pub mod session;
pub mod shell;
pub mod term_fit;
pub mod term_pane;
pub mod theme;
pub mod tool_card;
pub mod usage;
pub mod vcs;
pub mod vcs_watch;
pub mod window_size;
pub mod workbench;
pub mod workspace_trust;

use crate::ui::shell::NavMenu;

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

/// The menu a cold start lands on: 홈 is the first menu (D26).
pub fn initial_menu() -> NavMenu {
    NavMenu::Home
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
