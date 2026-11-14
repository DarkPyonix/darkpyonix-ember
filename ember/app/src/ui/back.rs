//! The app's back (FR-L36): what the system back gesture or button does.
//!
//! The order is the same on every screen size: an open sheet or overlay held in the shared
//! state closes first, then a detail view returns to its list, then any menu returns to 홈,
//! and on 홈 with nothing open the press is not handled, so the system leaves the app.
//!
//! TODO(compose-rust#302): the toolkit has no BackRequested event yet. Until it has one, the
//! Android Activity (`android/MainActivity.kt`) calls [`back`] itself, and a sheet a screen
//! keeps in its own composable state (settings, project settings) is not closed by back.

use crate::ui::coexist::SharedUi;
use crate::ui::shell::NavMenu;

/// What one back press does. Pure, so the order is tested without a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackStep {
    /// Close the 새 세션 sheet.
    CloseNewSession,
    /// Close the team character screen (it replaces the content with its own back button).
    CloseCharacters,
    /// A phone screen shows a detail view (an open room, a routine): show its list again.
    CloseDetail,
    /// Any menu other than 홈, an open conversation included: go to 홈.
    Home,
    /// 홈 with nothing open: not handled, the system leaves the app.
    Leave,
}

/// The step for the current state.
pub fn back_step(new_session_open: bool, characters_open: bool, detail_open: bool, menu: NavMenu) -> BackStep {
    if new_session_open {
        BackStep::CloseNewSession
    } else if characters_open {
        BackStep::CloseCharacters
    } else if detail_open {
        BackStep::CloseDetail
    } else if menu != NavMenu::Home {
        BackStep::Home
    } else {
        BackStep::Leave
    }
}

/// Take one back step. Returns `false` when the press was not handled.
pub fn back(ui: &SharedUi) -> bool {
    let step = back_step(
        ui.new_session.get_untracked().is_some(),
        ui.characters.get_untracked().is_some(),
        ui.detail_open.get_untracked(),
        ui.menu.get_untracked(),
    );
    match step {
        BackStep::CloseNewSession => ui.close_new_session(),
        BackStep::CloseCharacters => ui.close_characters(),
        BackStep::CloseDetail => ui.detail_open.set(false),
        BackStep::Home => crate::ui::cr::shell::choose(ui, NavMenu::Home),
        BackStep::Leave => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prefs::Prefs;
    use crate::ui::Route;

    #[test]
    fn fr_l36_back_order_is_sheet_then_detail_then_home_then_leave() {
        assert_eq!(back_step(true, true, true, NavMenu::Sessions), BackStep::CloseNewSession);
        assert_eq!(back_step(false, true, true, NavMenu::Projects), BackStep::CloseCharacters);
        assert_eq!(back_step(false, false, true, NavMenu::Channels), BackStep::CloseDetail);
        assert_eq!(back_step(false, false, false, NavMenu::Sessions), BackStep::Home);
        assert_eq!(back_step(false, false, false, NavMenu::Home), BackStep::Leave);
    }

    #[test]
    fn fr_l36_back_walks_from_a_sheet_over_a_conversation_to_leaving_the_app() {
        let ui = SharedUi::new(Prefs::default());
        ui.open_conversation("s1");
        ui.open_new_session(None);
        assert!(back(&ui));
        assert_eq!(ui.new_session.get_untracked(), None);
        assert_eq!(ui.route.get_untracked(), Route::Conversation("s1".into()));
        assert!(back(&ui));
        assert_eq!(ui.menu.get_untracked(), NavMenu::Home);
        assert_eq!(ui.route.get_untracked(), Route::Projects);
        assert!(!back(&ui), "홈 with nothing open leaves the app");
    }

    #[test]
    fn fr_l36_back_closes_the_character_screen_and_a_detail_view_before_the_menu() {
        let ui = SharedUi::new(Prefs::default());
        ui.set_menu(NavMenu::Projects);
        ui.open_characters("ember");
        assert!(back(&ui));
        assert_eq!(ui.characters.get_untracked(), None);
        assert_eq!(ui.menu.get_untracked(), NavMenu::Projects);
        ui.set_menu(NavMenu::Channels);
        ui.detail_open.set(true);
        assert!(back(&ui));
        assert!(!ui.detail_open.get_untracked());
        assert_eq!(ui.menu.get_untracked(), NavMenu::Channels);
        assert!(back(&ui));
        assert_eq!(ui.menu.get_untracked(), NavMenu::Home);
    }
}
