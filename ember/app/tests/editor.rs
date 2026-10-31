//! The Ember editor screen (FR-L31) and its keymap choice (FR-L30): the pure parts, no renderer.

use std::collections::HashMap;

use ember_app::prefs::{self, Prefs};
use ember_app::ui::editor::{position_text, EditorTabs, Side, Stripes, ToolWindow};
use ember_app::ui::keymap::{Action, Chord, Keymap, Os};

const OSES: [Os; 2] = [Os::Mac, Os::Other];

// FR-L30: with VS Code, quick open and the terminal; with JetBrains, search everywhere and the terminal.
#[test]
fn fr_l30_the_documented_defaults() {
    assert_eq!(Keymap::VsCode.hint(Action::Find, Os::Mac), "\u{2318}P");
    assert_eq!(Keymap::VsCode.hint(Action::Find, Os::Other), "Ctrl+P");
    assert_eq!(Keymap::VsCode.hint(Action::Terminal, Os::Mac), "\u{2303}`");
    assert_eq!(Keymap::VsCode.hint(Action::Terminal, Os::Other), "Ctrl+`");
    assert_eq!(
        Keymap::JetBrains.hint(Action::Find, Os::Mac),
        "\u{21E7}\u{21E7}"
    );
    assert_eq!(
        Keymap::JetBrains.hint(Action::Find, Os::Other),
        "Shift Shift"
    );
    assert_eq!(
        Keymap::JetBrains.hint(Action::Terminal, Os::Mac),
        "\u{2325}F12"
    );
    assert_eq!(
        Keymap::JetBrains.hint(Action::Terminal, Os::Other),
        "Alt+F12"
    );
    assert_eq!(Keymap::VsCode.hint(Action::Save, Os::Mac), "\u{2318}S");
    assert_eq!(Keymap::VsCode.hint(Action::Save, Os::Other), "Ctrl+S");
}

#[test]
fn fr_l30_no_chord_is_bound_to_two_actions() {
    for keymap in Keymap::ALL {
        for os in OSES {
            let mut seen: HashMap<String, Action> = HashMap::new();
            for action in Action::ALL {
                let Some(chord) = keymap.chord(action, os) else {
                    continue;
                };
                let shown = chord.display(os);
                if let Some(other) = seen.insert(shown.clone(), action) {
                    panic!("{keymap:?}/{os:?}: {shown} is bound to {other:?} and {action:?}");
                }
            }
        }
    }
}

#[test]
fn fr_l30_every_action_has_a_chord_somewhere_and_dispatch_follows_the_table() {
    for keymap in Keymap::ALL {
        let bound = Action::ALL
            .into_iter()
            .filter(|a| OSES.iter().any(|os| keymap.chord(*a, *os).is_some()))
            .count();
        assert!(bound >= 15, "{keymap:?} binds only {bound} actions");
        for os in OSES {
            for action in Action::ALL {
                if let Some(chord) = keymap.chord(action, os) {
                    assert_eq!(keymap.action_for(&chord, os), Some(action));
                    assert_eq!(keymap.hint(action, os), chord.display(os));
                } else {
                    assert_eq!(keymap.hint(action, os), "");
                }
            }
        }
    }
    // A chord of the other keymap is not handled: JetBrains' terminal key does nothing in VS Code.
    let alt_f12 = Chord::parse(Os::Other, "Alt+F12").unwrap();
    assert_eq!(
        Keymap::JetBrains.action_for(&alt_f12, Os::Other),
        Some(Action::Terminal)
    );
    assert_eq!(Keymap::VsCode.action_for(&alt_f12, Os::Other), None);
}

#[test]
fn fr_l30_the_modifier_is_command_on_mac_and_ctrl_elsewhere() {
    for action in [Action::Save, Action::Find] {
        assert!(Keymap::VsCode.hint(action, Os::Mac).contains('\u{2318}'));
        assert!(Keymap::VsCode.hint(action, Os::Other).starts_with("Ctrl+"));
    }
}

#[test]
fn chord_formatting_orders_modifiers_and_round_trips() {
    for (os, text) in [
        (Os::Mac, "\u{21E7}\u{2318}P"),
        (Os::Mac, "\u{21E7}\u{2325}F"),
        (Os::Mac, "\u{2303}\u{2318}I"),
        (Os::Mac, "\u{21E7}\u{238B}"),
        (Os::Other, "Ctrl+Shift+P"),
        (Os::Other, "Shift+Alt+F"),
        (Os::Other, "Ctrl+Alt+L"),
        (Os::Other, "Shift+Esc"),
        (Os::Other, "Shift Shift"),
    ] {
        let chord = Chord::parse(os, text).unwrap_or_else(|| panic!("parse {text}"));
        assert_eq!(chord.display(os), text);
    }
    assert_eq!(Chord::parse(Os::Other, ""), None);
    // Modifier order in the input does not matter.
    assert_eq!(
        Chord::parse(Os::Other, "Shift+Ctrl+P")
            .unwrap()
            .display(Os::Other),
        "Ctrl+Shift+P"
    );
}

#[test]
fn keymap_labels_follow_the_choice() {
    assert_eq!(Keymap::VsCode.label(), "VS Code");
    assert_eq!(Keymap::JetBrains.label(), "JetBrains");
    assert_eq!(Keymap::VsCode.find_label(), "파일로 이동");
    assert_eq!(Keymap::JetBrains.find_label(), "모든 곳에서 찾기");
    assert_eq!(Keymap::VsCode.palette_label(), "명령 팔레트");
    assert_eq!(Keymap::JetBrains.palette_label(), "동작 찾기");
}

#[test]
fn keymap_default_and_prefs_round_trip() {
    assert_eq!(Keymap::default(), Keymap::VsCode);
    assert_eq!(Prefs::default().keymap, Keymap::VsCode);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prefs.json");
    // An older file has no keymap: it reads as VS Code.
    std::fs::write(&path, br#"{"format":1}"#).unwrap();
    assert_eq!(prefs::load(&path).keymap, Keymap::VsCode);
    let p = Prefs {
        keymap: Keymap::JetBrains,
        ..Prefs::default()
    };
    prefs::save(&path, &p).unwrap();
    assert_eq!(prefs::load(&path).keymap, Keymap::JetBrains);
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("jetbrains"));
}

#[test]
fn status_bar_position_text() {
    assert_eq!(position_text(15, 22), "15행 22열");
    assert_eq!(position_text(1, 1), "1행 1열");
}

#[test]
fn tabs_open_activate_close_and_track_unsaved() {
    let mut t = EditorTabs::default();
    assert_eq!(t.current_path(), None);
    assert!(!t.has_unsaved());
    t.open("a.rs");
    t.open("b.rs");
    t.open("a.rs"); // already open: activated, not duplicated
    assert_eq!(t.paths(), vec!["a.rs", "b.rs"]);
    assert_eq!(t.current_path(), Some("a.rs"));
    t.set_dirty("b.rs", true);
    assert!(t.is_dirty("b.rs"));
    assert!(!t.is_dirty("a.rs"));
    assert!(t.has_unsaved());
    // Closing the current tab activates its neighbour.
    t.close("a.rs");
    assert_eq!(t.paths(), vec!["b.rs"]);
    assert_eq!(t.current_path(), Some("b.rs"));
    // Closing a tab with unsaved changes drops its dirty mark with it.
    t.close("b.rs");
    assert_eq!(t.current_path(), None);
    assert!(!t.has_unsaved());
    t.close("missing.rs"); // no panic
    t.set_dirty("missing.rs", true);
    assert!(!t.has_unsaved());
}

#[test]
fn closing_a_background_tab_keeps_the_current_one() {
    let mut t = EditorTabs::default();
    for p in ["a", "b", "c"] {
        t.open(p);
    }
    t.activate("b");
    t.close("c");
    assert_eq!(t.current_path(), Some("b"));
    t.close("a");
    assert_eq!(t.current_path(), Some("b"));
    assert_eq!(t.paths(), vec!["b"]);
}

#[test]
fn stripes_select_toggle_and_stay_on_their_side() {
    assert_eq!(ToolWindow::Project.side(), Side::Left);
    assert_eq!(ToolWindow::Agent.side(), Side::Right);
    assert_eq!(ToolWindow::Terminal.side(), Side::Bottom);
    let mut s = Stripes::default();
    assert!(s.is_selected(ToolWindow::Project));
    assert!(s.is_selected(ToolWindow::Terminal));
    assert!(!s.is_selected(ToolWindow::Agent));
    // Another left window replaces the project tree; the bottom one is untouched.
    s.toggle(ToolWindow::Git);
    assert!(s.is_selected(ToolWindow::Git));
    assert!(!s.is_selected(ToolWindow::Project));
    assert!(s.is_selected(ToolWindow::Terminal));
    // The selected one again hides its window.
    s.toggle(ToolWindow::Git);
    assert_eq!(s.selected(Side::Left), None);
    s.toggle(ToolWindow::Problems);
    assert_eq!(s.selected(Side::Bottom), Some(ToolWindow::Problems));
    s.toggle(ToolWindow::Notifications);
    assert_eq!(s.selected(Side::Right), Some(ToolWindow::Notifications));
}

#[test]
fn every_tool_window_has_a_name_and_every_action_a_label() {
    for w in ToolWindow::ALL {
        assert!(!w.label().is_empty());
    }
    for a in Action::ALL {
        assert!(!a.label().is_empty());
    }
}

// FR-L31 (#186): the session view IDE menu opens the editor on the launch project and folder.
#[test]
fn fr_l31_editor_target_is_empty_until_the_launch_arrives() {
    use ember_app::server::IdeLaunch;
    let none = (String::new(), String::new());
    assert_eq!(IdeLaunch::editor_target(None), none);
    let err = Err("down".to_string());
    assert_eq!(IdeLaunch::editor_target(Some(&err)), none);
}
