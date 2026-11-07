//! The Ember editor screen (FR-L31) and its keymap choice (FR-L30): the pure parts, no renderer.

use std::collections::HashMap;

use ember_app::prefs::{self, Prefs};
use ember_app::ui::editor::{position_text, DoubleShift, EditorTabs, Side, Stripes, ToolWindow};
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

// FR-L30: JetBrains' search everywhere is Shift pressed twice, so two Shift presses close together make
// that chord and nothing else does.
#[test]
fn fr_l30_two_shift_presses_close_together_are_double_shift() {
    let mut d = DoubleShift::default();
    assert!(!d.press("Shift", false, false, false, false, 1_000));
    assert!(d.press("Shift", false, false, false, false, 1_300));
    // The pair is used up: a third press starts a new one.
    assert!(!d.press("Shift", false, false, false, false, 1_400));
    assert_eq!(
        Keymap::JetBrains.action_for(&Chord::DoubleShift, Os::Mac),
        Some(Action::Find)
    );
    assert_eq!(Keymap::VsCode.action_for(&Chord::DoubleShift, Os::Mac), None);
}

#[test]
fn fr_l30_double_shift_needs_two_bare_shift_presses_within_the_window() {
    let mut d = DoubleShift::default();
    // Too slow.
    assert!(!d.press("Shift", false, false, false, false, 0));
    assert!(!d.press("Shift", false, false, false, false, 900));
    // Another key in between breaks the pair, as typing a capital letter would.
    let mut d = DoubleShift::default();
    assert!(!d.press("Shift", false, false, false, false, 0));
    assert!(!d.press("P", false, false, false, false, 100));
    assert!(!d.press("Shift", false, false, false, false, 200));
    // Holding Shift repeats the key down event; that is one press.
    let mut d = DoubleShift::default();
    assert!(!d.press("Shift", false, false, false, false, 0));
    assert!(!d.press("Shift", true, false, false, false, 100));
    // Shift with another modifier is not a bare Shift.
    let mut d = DoubleShift::default();
    assert!(!d.press("Shift", false, true, false, false, 0));
    assert!(!d.press("Shift", false, true, false, false, 100));
}

// FR-L31 (D38): the explorer is a tree of the folder, directories first, children indented,
// a directory's children shown only while it is expanded and once they have been listed.
mod tree {
    use super::*;
    use ember_app::ui::editor::{tree_rows, TreeRow};
    use std::collections::HashSet;

    const DIR: u32 = ember_editor_conn::remote_fs::file_type::DIRECTORY;
    const FILE: u32 = ember_editor_conn::remote_fs::file_type::FILE;

    fn listings() -> HashMap<String, Vec<(String, u32)>> {
        HashMap::from([
            ("/p".to_string(), vec![("b.rs".to_string(), FILE), ("src".to_string(), DIR), ("A.md".to_string(), FILE)]),
            ("/p/src".to_string(), vec![("lib.rs".to_string(), FILE), ("deep".to_string(), DIR)]),
        ])
    }

    fn names(rows: &[TreeRow]) -> Vec<(&str, u32)> {
        rows.iter().map(|r| (r.name.as_str(), r.depth)).collect()
    }

    #[test]
    fn fr_l31_collapsed_tree_lists_directories_first_then_files_by_name() {
        let rows = tree_rows("/p", &listings(), &HashSet::new());
        assert_eq!(names(&rows), vec![("src", 0), ("A.md", 0), ("b.rs", 0)]);
        assert!(rows[0].is_dir && !rows[0].open);
        assert_eq!(rows[0].path, "/p/src");
    }

    #[test]
    fn fr_l31_expanded_directory_shows_its_children_indented() {
        let open = HashSet::from(["/p/src".to_string()]);
        let rows = tree_rows("/p", &listings(), &open);
        assert_eq!(names(&rows), vec![("src", 0), ("deep", 1), ("lib.rs", 1), ("A.md", 0), ("b.rs", 0)]);
        assert!(rows[0].open);
    }

    #[test]
    fn fr_l31_expanded_directory_not_listed_yet_shows_no_children() {
        let open = HashSet::from(["/p/src".to_string(), "/p/src/deep".to_string()]);
        let rows = tree_rows("/p", &listings(), &open);
        let deep = rows.iter().find(|r| r.name == "deep").unwrap();
        assert!(deep.open);
        assert_eq!(rows.len(), 5);
    }
}

mod selection {
    use super::*;
    use ember_app::ui::editor::{prune_redundant_selection, update_tree_selection, TreeRow};

    fn make_rows() -> Vec<TreeRow> {
        vec![
            TreeRow { path: "/p/a".into(), name: "a".into(), depth: 0, is_dir: true, open: false },
            TreeRow { path: "/p/b".into(), name: "b".into(), depth: 0, is_dir: false, open: false },
            TreeRow { path: "/p/c".into(), name: "c".into(), depth: 0, is_dir: false, open: false },
            TreeRow { path: "/p/d".into(), name: "d".into(), depth: 0, is_dir: true, open: false },
            TreeRow { path: "/p/e".into(), name: "e".into(), depth: 0, is_dir: false, open: false },
        ]
    }

    #[test]
    fn single_click_selects_just_clicked_item() {
        let rows = make_rows();
        let sel = update_tree_selection(&[("/p/a".into(), true)], ("/p/b".into(), false), &rows, false, false);
        assert_eq!(sel, vec![("/p/b".to_string(), false)]);
    }

    #[test]
    fn cmd_ctrl_toggle_adds_new_item_and_removes_existing_item() {
        let rows = make_rows();
        // Toggle non-existing item: append
        let sel1 = update_tree_selection(&[("/p/a".into(), true)], ("/p/b".into(), false), &rows, false, true);
        assert_eq!(sel1, vec![("/p/a".to_string(), true), ("/p/b".to_string(), false)]);

        // Toggle existing item: remove
        let sel2 = update_tree_selection(&sel1, ("/p/a".into(), true), &rows, false, true);
        assert_eq!(sel2, vec![("/p/b".to_string(), false)]);

        // Toggle remaining item: empty
        let sel3 = update_tree_selection(&sel2, ("/p/b".into(), false), &rows, false, true);
        assert!(sel3.is_empty());
    }

    #[test]
    fn shift_selects_contiguous_range_between_anchor_and_clicked() {
        let rows = make_rows();
        // Downward range: from /p/b (idx 1) to /p/d (idx 3)
        let sel = update_tree_selection(&[("/p/b".into(), false)], ("/p/d".into(), true), &rows, true, false);
        assert_eq!(
            sel,
            vec![
                ("/p/b".to_string(), false),
                ("/p/c".to_string(), false),
                ("/p/d".to_string(), true),
            ]
        );

        // Upward range: from /p/d (idx 3) to /p/a (idx 0)
        let sel_up = update_tree_selection(&[("/p/d".into(), true)], ("/p/a".into(), true), &rows, true, false);
        assert_eq!(
            sel_up,
            vec![
                ("/p/a".to_string(), true),
                ("/p/b".to_string(), false),
                ("/p/c".to_string(), false),
                ("/p/d".to_string(), true),
            ]
        );

        // Shift with empty current: defaults to clicked item
        let sel_empty = update_tree_selection(&[], ("/p/c".into(), false), &rows, true, false);
        assert_eq!(sel_empty, vec![("/p/c".to_string(), false)]);
    }

    #[test]
    fn prune_redundant_selection_filters_out_items_below_selected_directory() {
        let items = vec![
            ("/p/src".to_string(), true),
            ("/p/src/main.rs".to_string(), false),
            ("/p/src/nested/deep.rs".to_string(), false),
            ("/p/README.md".to_string(), false),
        ];
        let pruned = prune_redundant_selection(&items);
        assert_eq!(
            pruned,
            vec![
                ("/p/src".to_string(), true),
                ("/p/README.md".to_string(), false),
            ]
        );
    }

    #[test]
    fn prune_redundant_selection_filters_nested_directories_and_keeps_siblings() {
        let items = vec![
            ("/p/dir".to_string(), true),
            ("/p/dir/child".to_string(), true),
            ("/p/dir2".to_string(), true),
        ];
        let pruned = prune_redundant_selection(&items);
        assert_eq!(
            pruned,
            vec![
                ("/p/dir".to_string(), true),
                ("/p/dir2".to_string(), true),
            ]
        );
    }
}

// FR-L31 (D38): the page grid is the mockup's; a closed tool window leaves a 0 wide column or a
// 0 high row, and the stripes decide which.
mod grid {
    use super::*;
    use ember_app::ui::editor::editor_grid_style;

    #[test]
    fn fr_l31_open_left_and_bottom_leave_the_tracks_to_the_stylesheet() {
        // The stylesheet's tracks carry the width breakpoints; an inline track would override them.
        assert_eq!(editor_grid_style(true, true), "");
    }

    #[test]
    fn fr_l31_closed_left_column_is_zero_wide() {
        let g = editor_grid_style(false, true);
        assert!(g.contains("grid-template-columns:var(--stw) 0 minmax(0,1fr) var(--stw)"), "{g}");
    }

    #[test]
    fn fr_l31_closed_bottom_row_is_zero_high() {
        let g = editor_grid_style(true, false);
        assert!(g.contains("grid-template-rows:52px minmax(0,1fr) 0 var(--sbh)"), "{g}");
    }

    #[test]
    fn fr_l31_stripe_toggles_decide_the_closed_tracks() {
        let mut s = Stripes::default();
        let grid = |s: &Stripes| editor_grid_style(s.selected(Side::Left).is_some(), s.selected(Side::Bottom).is_some());
        assert_eq!(grid(&s), editor_grid_style(true, true));
        s.toggle(ToolWindow::Project);
        assert_eq!(grid(&s), editor_grid_style(false, true));
        s.toggle(ToolWindow::Terminal);
        assert_eq!(grid(&s), editor_grid_style(false, false));
        s.toggle(ToolWindow::Git);
        assert_eq!(grid(&s), editor_grid_style(true, false));
    }
}

// FR-L31 (D38): the maximized bottom window takes the editor's row, like the mockup's `.ed.max`.
#[test]
fn fr_l31_maximized_bottom_window_collapses_the_editor_row() {
    let g = ember_app::ui::editor::editor_grid_style_maximized();
    assert!(g.contains("grid-template-rows:52px 0 minmax(0,1fr) var(--sbh)"), "{g}");
    assert!(!g.contains("grid-template-columns"), "{g}");
}

// FR-L31 (D38): the status bar's agent dot and path follow the mockup.
mod status {
    use ember_app::ui::editor::{agent_dot_class, status_crumbs};

    #[test]
    fn fr_l31_agent_dot_colours_follow_the_state() {
        assert_eq!(agent_dot_class("승인 대기"), "dot wait");
        assert_eq!(agent_dot_class("실행 중"), "dot run");
        assert_eq!(agent_dot_class("완료"), "dot ok");
        assert_eq!(agent_dot_class("팀원 없음"), "dot ok");
    }

    #[test]
    fn fr_l31_status_path_is_the_folder_name_then_the_file_directories() {
        assert_eq!(status_crumbs("/w/p/", None), "p");
        assert_eq!(status_crumbs("/w/p", Some("/w/p/ember/proxy/src/shim.rs")), "p \u{203A} ember \u{203A} proxy \u{203A} src");
        assert_eq!(status_crumbs("/w/p", Some("/w/p/README.md")), "p");
    }
}

// FR-L31 (D38): the window opens FullSize on macOS, the top bar is the title strip.
mod title_strip {
    use ember_app::ui::editor::{editor_page_height, editor_strip_padding};

    #[test]
    fn fr_l31_full_size_page_is_the_window_height_and_other_chrome_keeps_its_allowance() {
        assert_eq!(editor_page_height(true), "100vh");
        assert_eq!(editor_page_height(false), "calc(100vh - 76px)");
    }

    #[test]
    fn fr_l31_top_bar_clears_the_traffic_lights_only_under_full_size() {
        assert_eq!(editor_strip_padding(true, 98.0), "padding-left:98px;");
        assert_eq!(editor_strip_padding(false, 98.0), "");
    }

    #[test]
    fn fr_l31_top_bar_is_a_drag_region_and_its_controls_are_cut_out_in_the_stylesheets() {
        let css = ember_app::ui::editor_workbench::page::config().stylesheets.join("\n");
        let compact: String = css.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains(".ed-top{-webkit-app-region:drag}"), "no drag rule for the top bar");
        assert!(
            compact.contains(".ed-topbutton,.ed-top.ed-menu{-webkit-app-region:no-drag}"),
            "the top bar's buttons and menu are not cut out of the drag region"
        );
    }

    #[test]
    fn fr_l31_editor_window_opens_full_size_on_macos() {
        use compose_rust::schema::{Chrome, Window};
        // The chrome the editor window is given is the shell's: FullSize on macOS, untouched elsewhere.
        let given = ember_app::ui::cr::shell::window_chrome(Window::new()).chrome;
        let plain = Window::new().chrome;
        assert_eq!(given, if cfg!(target_os = "macos") { Chrome::FullSize } else { plain });
    }
}

// FR-E2 (D38 problems list): the tab carries the count, a row says where the problem is.
mod problems {
    use ember_app::ui::editor::{problem_location, problems_tab_label};

    #[test]
    fn fr_e2_problems_tab_shows_the_count_only_when_there_are_problems() {
        assert_eq!(problems_tab_label(0), "문제");
        assert_eq!(problems_tab_label(3), "문제 3");
    }

    #[test]
    fn fr_e2_problem_row_location_is_the_project_relative_path_and_one_based_line() {
        assert_eq!(problem_location("/w/p/src/lib.rs", "/w/p/", 0), "src/lib.rs:1");
        assert_eq!(problem_location("/w/p/package.json", "/w/p", 11), "package.json:12");
    }
}

// FR-L31 (D38, #438): explorer edits: where a new item goes, the delete wording, which tabs go.
mod explorer_edit {
    use ember_app::ui::editor::{delete_click_allowed, delete_confirmation, edit_failure_text, is_at_or_below, new_item_parent, EditorTabs};

    #[test]
    fn fr_l31_delete_confirmation_warns_about_unsaved_changes_of_open_files() {
        let text = delete_confirmation("/w/p/src", "/w/p", true, &["/w/p/src/a.rs".to_string(), "/w/p/src/deep/b.rs".to_string()]);
        assert!(text.contains("저장하지 않은 변경이 있는 파일: src/a.rs, src/deep/b.rs"), "{text}");
        assert!(text.contains("편집도 사라집니다"), "{text}");
        assert!(!delete_confirmation("/w/p/a.rs", "/w/p", false, &[]).contains("저장하지 않은"));
    }

    #[test]
    fn fr_l31_the_delete_button_ignores_clicks_in_the_first_600_ms() {
        assert!(!delete_click_allowed(1_000, 1_000));
        assert!(!delete_click_allowed(1_000, 1_599));
        assert!(delete_click_allowed(1_000, 1_600));
        assert!(!delete_click_allowed(5_000, 4_000), "a clock that went backwards does not allow it");
    }

    #[test]
    fn fr_l31_failures_are_shown_in_korean_without_the_servers_english_text() {
        let text = edit_failure_text("새 파일", "remote error: EntryExists (FileSystemError): File already exists");
        assert_eq!(text, "새 파일 실패: 같은 이름이 이미 있습니다");
        assert!(!edit_failure_text("삭제", "remote error: something odd happened").contains("odd"));
        assert_eq!(edit_failure_text("이름 변경", "EACCES: permission denied"), "이름 변경 실패: 권한이 없습니다");
    }

    #[test]
    fn fr_l31_renaming_a_directory_moves_the_tabs_below_it_in_place_with_their_dirty_marks() {
        let mut tabs = EditorTabs::default();
        tabs.open("/w/p/src/a.rs");
        tabs.open("/w/p/other.rs");
        tabs.open("/w/p/src/deep/b.rs");
        tabs.set_dirty("/w/p/src/deep/b.rs", true);
        tabs.activate("/w/p/src/a.rs");
        let moved = tabs.rename_prefix("/w/p/src", "/w/p/lib");
        assert_eq!(moved, vec![
            ("/w/p/src/a.rs".to_string(), "/w/p/lib/a.rs".to_string()),
            ("/w/p/src/deep/b.rs".to_string(), "/w/p/lib/deep/b.rs".to_string()),
        ]);
        assert_eq!(tabs.paths(), vec!["/w/p/lib/a.rs", "/w/p/other.rs", "/w/p/lib/deep/b.rs"]);
        assert!(tabs.is_dirty("/w/p/lib/deep/b.rs"));
        assert_eq!(tabs.current_path(), Some("/w/p/lib/a.rs"));
        assert!(tabs.rename_prefix("/w/p/src2", "/w/p/x").is_empty(), "a sibling that shares a prefix is not touched");
    }

    #[test]
    fn fr_l31_new_item_goes_in_the_selected_directory_or_beside_the_selected_file() {
        assert_eq!(new_item_parent(Some(("/w/p/src", true)), "/w/p"), "/w/p/src");
        assert_eq!(new_item_parent(Some(("/w/p/src/a.rs", false)), "/w/p"), "/w/p/src");
        assert_eq!(new_item_parent(Some(("/a.rs", false)), "/w/p"), "/");
        assert_eq!(new_item_parent(None, "/w/p"), "/w/p");
    }

    #[test]
    fn fr_l31_delete_confirmation_says_it_cannot_be_undone_and_names_the_contents_of_a_folder() {
        let file = delete_confirmation("/w/p/src/a.rs", "/w/p", false, &[]);
        assert!(file.contains("src/a.rs") && file.contains("되돌릴 수 없습니다"), "{file}");
        let dir = delete_confirmation("/w/p/src", "/w/p", true, &[]);
        assert!(dir.contains("그 안의 모든 파일") && dir.contains("되돌릴 수 없습니다"), "{dir}");
    }

    #[test]
    fn fr_l31_a_deleted_path_closes_its_own_tab_and_the_tabs_below_it_only() {
        assert!(is_at_or_below("/w/p/src/a.rs", "/w/p/src/a.rs"));
        assert!(is_at_or_below("/w/p/src/a.rs", "/w/p/src"));
        assert!(!is_at_or_below("/w/p/src2/a.rs", "/w/p/src"));
        assert!(!is_at_or_below("/w/p/a.rs", "/w/p/src"));
    }
}

// #302 (D38): the names the accessibility tree shows for tabs, tree rows and problems.
mod aria {
    use ember_app::ui::editor::{problem_aria_label, tab_aria_label, tree_item_aria_label};

    #[test]
    fn fr_l31_a_tab_is_named_by_its_file_and_says_when_it_is_not_saved() {
        assert_eq!(tab_aria_label("lib.rs", false), "lib.rs");
        assert_eq!(tab_aria_label("lib.rs", true), "lib.rs (저장 안 됨)");
    }

    #[test]
    fn fr_l31_a_tree_row_is_named_by_its_name_and_says_when_it_is_a_folder() {
        assert_eq!(tree_item_aria_label("src", true), "src 폴더");
        assert_eq!(tree_item_aria_label("main.rs", false), "main.rs");
    }

    #[test]
    fn fr_l31_a_problem_row_says_what_where_and_how_bad() {
        assert_eq!(problem_aria_label("오류", "Value expected", "package.json:2"), "오류: Value expected (package.json:2)");
    }
}

// #302: Keyboard navigation and roving tabindex in the editor window (tree and tab strip).
mod keyboard_navigation {
    use ember_app::ui::editor::{
        editor_focus_target, tab_strip_key, tree_key_action, TabKeyOutcome, TreeKeyOutcome, TreeRow,
    };

    #[test]
    fn fr_l31_tab_strip_arrow_keys_navigate_and_wrap() {
        let paths = vec![
            "/w/p/a.rs".to_string(),
            "/w/p/b.rs".to_string(),
            "/w/p/c.rs".to_string(),
        ];

        // ArrowRight from first tab moves to second.
        assert_eq!(
            tab_strip_key(&paths, Some("/w/p/a.rs"), "ArrowRight"),
            Some(TabKeyOutcome::Activate("/w/p/b.rs".to_string()))
        );

        // ArrowRight from middle tab moves to third.
        assert_eq!(
            tab_strip_key(&paths, Some("/w/p/b.rs"), "ArrowRight"),
            Some(TabKeyOutcome::Activate("/w/p/c.rs".to_string()))
        );

        // ArrowRight from last tab wraps around to first.
        assert_eq!(
            tab_strip_key(&paths, Some("/w/p/c.rs"), "ArrowRight"),
            Some(TabKeyOutcome::Activate("/w/p/a.rs".to_string()))
        );

        // ArrowRight with unknown current tab defaults to first tab.
        assert_eq!(
            tab_strip_key(&paths, None, "ArrowRight"),
            Some(TabKeyOutcome::Activate("/w/p/a.rs".to_string()))
        );
        assert_eq!(
            tab_strip_key(&paths, Some("/unknown.rs"), "ArrowRight"),
            Some(TabKeyOutcome::Activate("/w/p/a.rs".to_string()))
        );

        // ArrowLeft from middle tab moves to first.
        assert_eq!(
            tab_strip_key(&paths, Some("/w/p/b.rs"), "ArrowLeft"),
            Some(TabKeyOutcome::Activate("/w/p/a.rs".to_string()))
        );

        // ArrowLeft from first tab wraps around to last.
        assert_eq!(
            tab_strip_key(&paths, Some("/w/p/a.rs"), "ArrowLeft"),
            Some(TabKeyOutcome::Activate("/w/p/c.rs".to_string()))
        );

        // ArrowLeft with unknown current tab defaults to last tab.
        assert_eq!(
            tab_strip_key(&paths, None, "ArrowLeft"),
            Some(TabKeyOutcome::Activate("/w/p/c.rs".to_string()))
        );
    }

    #[test]
    fn fr_l31_tab_strip_home_end_delete_and_activate() {
        let paths = vec![
            "/w/p/a.rs".to_string(),
            "/w/p/b.rs".to_string(),
            "/w/p/c.rs".to_string(),
        ];

        // Home activates first tab.
        assert_eq!(
            tab_strip_key(&paths, Some("/w/p/c.rs"), "Home"),
            Some(TabKeyOutcome::Activate("/w/p/a.rs".to_string()))
        );

        // End activates last tab.
        assert_eq!(
            tab_strip_key(&paths, Some("/w/p/a.rs"), "End"),
            Some(TabKeyOutcome::Activate("/w/p/c.rs".to_string()))
        );

        // Enter and Space activate the current tab.
        assert_eq!(
            tab_strip_key(&paths, Some("/w/p/b.rs"), "Enter"),
            Some(TabKeyOutcome::Activate("/w/p/b.rs".to_string()))
        );
        assert_eq!(
            tab_strip_key(&paths, Some("/w/p/b.rs"), " "),
            Some(TabKeyOutcome::Activate("/w/p/b.rs".to_string()))
        );
        assert_eq!(
            tab_strip_key(&paths, Some("/w/p/b.rs"), "Space"),
            Some(TabKeyOutcome::Activate("/w/p/b.rs".to_string()))
        );

        // Delete and Backspace close the current tab.
        assert_eq!(
            tab_strip_key(&paths, Some("/w/p/b.rs"), "Delete"),
            Some(TabKeyOutcome::Close("/w/p/b.rs".to_string()))
        );
        assert_eq!(
            tab_strip_key(&paths, Some("/w/p/c.rs"), "Backspace"),
            Some(TabKeyOutcome::Close("/w/p/c.rs".to_string()))
        );

        // Delete with no current tab returns None.
        assert_eq!(tab_strip_key(&paths, None, "Delete"), None);

        // Unrelated key returns None.
        assert_eq!(tab_strip_key(&paths, Some("/w/p/a.rs"), "Escape"), None);
        assert_eq!(tab_strip_key(&paths, Some("/w/p/a.rs"), "x"), None);

        // Empty paths always returns None.
        assert_eq!(tab_strip_key(&[], Some("/w/p/a.rs"), "ArrowRight"), None);
        assert_eq!(tab_strip_key(&[], None, "Home"), None);
    }

    fn sample_tree() -> Vec<TreeRow> {
        vec![
            TreeRow {
                path: "/p/src".to_string(),
                name: "src".to_string(),
                depth: 0,
                is_dir: true,
                open: true,
            },
            TreeRow {
                path: "/p/src/deep".to_string(),
                name: "deep".to_string(),
                depth: 1,
                is_dir: true,
                open: false,
            },
            TreeRow {
                path: "/p/src/lib.rs".to_string(),
                name: "lib.rs".to_string(),
                depth: 1,
                is_dir: false,
                open: false,
            },
            TreeRow {
                path: "/p/README.md".to_string(),
                name: "README.md".to_string(),
                depth: 0,
                is_dir: false,
                open: false,
            },
        ]
    }

    #[test]
    fn fr_l31_tree_arrow_down_up_home_end() {
        let rows = sample_tree();

        // ArrowDown with no selection selects first row.
        assert_eq!(
            tree_key_action(&rows, None, "/p", "ArrowDown"),
            Some(TreeKeyOutcome::Select("/p/src".to_string(), true))
        );

        // ArrowDown from row 0 selects row 1.
        assert_eq!(
            tree_key_action(&rows, Some(("/p/src", true)), "/p", "ArrowDown"),
            Some(TreeKeyOutcome::Select("/p/src/deep".to_string(), true))
        );

        // ArrowDown from row 2 selects row 3.
        assert_eq!(
            tree_key_action(&rows, Some(("/p/src/lib.rs", false)), "/p", "ArrowDown"),
            Some(TreeKeyOutcome::Select("/p/README.md".to_string(), false))
        );

        // ArrowDown from last row stays on last row.
        assert_eq!(
            tree_key_action(&rows, Some(("/p/README.md", false)), "/p", "ArrowDown"),
            Some(TreeKeyOutcome::Select("/p/README.md".to_string(), false))
        );

        // ArrowUp from last row selects row 2.
        assert_eq!(
            tree_key_action(&rows, Some(("/p/README.md", false)), "/p", "ArrowUp"),
            Some(TreeKeyOutcome::Select("/p/src/lib.rs".to_string(), false))
        );

        // ArrowUp from first row stays on first row.
        assert_eq!(
            tree_key_action(&rows, Some(("/p/src", true)), "/p", "ArrowUp"),
            Some(TreeKeyOutcome::Select("/p/src".to_string(), true))
        );

        // ArrowUp with no selection selects first row.
        assert_eq!(
            tree_key_action(&rows, None, "/p", "ArrowUp"),
            Some(TreeKeyOutcome::Select("/p/src".to_string(), true))
        );

        // Home selects first row.
        assert_eq!(
            tree_key_action(&rows, Some(("/p/README.md", false)), "/p", "Home"),
            Some(TreeKeyOutcome::Select("/p/src".to_string(), true))
        );

        // End selects last row.
        assert_eq!(
            tree_key_action(&rows, Some(("/p/src", true)), "/p", "End"),
            Some(TreeKeyOutcome::Select("/p/README.md".to_string(), false))
        );

        // Empty rows returns None.
        assert_eq!(tree_key_action(&[], None, "/p", "ArrowDown"), None);
    }

    #[test]
    fn fr_l31_tree_arrow_right_expands_or_descends() {
        let rows = sample_tree();

        // ArrowRight on closed directory returns Expand.
        assert_eq!(
            tree_key_action(&rows, Some(("/p/src/deep", true)), "/p", "ArrowRight"),
            Some(TreeKeyOutcome::Expand("/p/src/deep".to_string()))
        );

        // ArrowRight on open directory moves to its first child.
        assert_eq!(
            tree_key_action(&rows, Some(("/p/src", true)), "/p", "ArrowRight"),
            Some(TreeKeyOutcome::Select("/p/src/deep".to_string(), true))
        );

        // ArrowRight on file does nothing.
        assert_eq!(
            tree_key_action(&rows, Some(("/p/src/lib.rs", false)), "/p", "ArrowRight"),
            None
        );

        // ArrowRight with no selection does nothing.
        assert_eq!(tree_key_action(&rows, None, "/p", "ArrowRight"), None);
    }

    #[test]
    fn fr_l31_tree_arrow_left_collapses_or_ascends() {
        let rows = sample_tree();

        // ArrowLeft on open directory returns Collapse.
        assert_eq!(
            tree_key_action(&rows, Some(("/p/src", true)), "/p", "ArrowLeft"),
            Some(TreeKeyOutcome::Collapse("/p/src".to_string()))
        );

        // ArrowLeft on child row (file lib.rs at depth 1) moves to parent directory.
        assert_eq!(
            tree_key_action(&rows, Some(("/p/src/lib.rs", false)), "/p", "ArrowLeft"),
            Some(TreeKeyOutcome::Select("/p/src".to_string(), true))
        );

        // ArrowLeft on closed child directory (deep at depth 1) moves to parent directory.
        assert_eq!(
            tree_key_action(&rows, Some(("/p/src/deep", true)), "/p", "ArrowLeft"),
            Some(TreeKeyOutcome::Select("/p/src".to_string(), true))
        );

        // ArrowLeft on root-level file (depth 0) does nothing.
        assert_eq!(
            tree_key_action(&rows, Some(("/p/README.md", false)), "/p", "ArrowLeft"),
            None
        );

        // ArrowLeft with no selection does nothing.
        assert_eq!(tree_key_action(&rows, None, "/p", "ArrowLeft"), None);
    }

    #[test]
    fn fr_l31_tree_enter_and_space_open_file_or_toggle_dir() {
        let rows = sample_tree();

        // Enter on file opens the file.
        assert_eq!(
            tree_key_action(&rows, Some(("/p/src/lib.rs", false)), "/p", "Enter"),
            Some(TreeKeyOutcome::OpenFile("/p/src/lib.rs".to_string()))
        );

        // Space on file opens the file.
        assert_eq!(
            tree_key_action(&rows, Some(("/p/src/lib.rs", false)), "/p", " "),
            Some(TreeKeyOutcome::OpenFile("/p/src/lib.rs".to_string()))
        );

        // Enter on closed directory expands it.
        assert_eq!(
            tree_key_action(&rows, Some(("/p/src/deep", true)), "/p", "Enter"),
            Some(TreeKeyOutcome::Expand("/p/src/deep".to_string()))
        );

        // Enter on open directory collapses it.
        assert_eq!(
            tree_key_action(&rows, Some(("/p/src", true)), "/p", "Enter"),
            Some(TreeKeyOutcome::Collapse("/p/src".to_string()))
        );

        // Space on open directory collapses it.
        assert_eq!(
            tree_key_action(&rows, Some(("/p/src", true)), "/p", "Space"),
            Some(TreeKeyOutcome::Collapse("/p/src".to_string()))
        );

        // Unrelated key returns None.
        assert_eq!(
            tree_key_action(&rows, Some(("/p/src", true)), "/p", "Tab"),
            None
        );
    }

    #[test]
    fn fr_l31_editor_focus_target_returns_current_path() {
        assert_eq!(
            editor_focus_target(Some("/w/p/src/lib.rs")),
            Some("/w/p/src/lib.rs".to_string())
        );
        assert_eq!(editor_focus_target(None), None);
    }

    #[test]
    fn fr_l31_tree_ignores_keys_while_prompt_is_open() {
        use ember_app::ui::editor::{tree_handles_key, TreePrompt};
        assert!(tree_handles_key(None));
        for p in [
            TreePrompt::NewFile { parent: "/p".into() },
            TreePrompt::NewDir { parent: "/p".into() },
            TreePrompt::Rename { path: "/p/a.rs".into() },
            TreePrompt::Delete { path: "/p/a.rs".into(), is_dir: false },
            TreePrompt::DeleteMany { items: vec![("/p/a.rs".into(), false), ("/p/b.rs".into(), false)] },
        ] {
            assert!(!tree_handles_key(Some(&p)), "prompt {p:?} must block tree key handling");
        }
    }
}

// FR-L31.2: the editor's source control rules against the demo git source, which takes the same op
// names and bodies the project relay (FR-G4) does: grouping, stage and unstage per file (a rename
// with its old path), the commit gate and the commit body.
#[test]
fn fr_l31_2_stage_unstage_and_commit_against_the_demo_repository() {
    use ember_app::demo::DemoSource;
    use ember_app::ui::editor_scm::{commit_gate, groups, op_paths, Group, ScmLoad, NEED_MESSAGE};
    use ember_app::ui::vcs::{commit_body, git_result};
    use ember_client::api::{GitCommitted, GitStatus};
    use serde_json::json;

    let scratch = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let dir = tempfile::tempdir_in(scratch).unwrap();
    let d = DemoSource::new(0, dir.path().to_path_buf());
    let status = |d: &DemoSource| -> GitStatus { git_result(d.git("demo-project", "status", &json!({}))).unwrap() };

    let g = groups(&status(&d));
    // A file with staged and unstaged hunks is listed in both groups.
    assert!(g.staged.iter().any(|e| e.path == "ember/app/src/ui/vcs.rs" && e.letter == 'M'));
    assert!(g.changes.iter().any(|e| e.path == "ember/app/src/ui/vcs.rs" && e.letter == 'M'));
    let docs = g.changes.iter().find(|e| e.path == "docs/GIT.md").unwrap().clone();
    assert_eq!(docs.letter, 'U');

    d.git("demo-project", "stage", &json!({ "paths": op_paths(&[docs]) })).unwrap();
    let g = groups(&status(&d));
    assert!(g.staged.iter().any(|e| e.path == "docs/GIT.md" && e.letter == 'A' && e.group == Group::Staged));
    assert!(!g.changes.iter().any(|e| e.path == "docs/GIT.md"));

    // A rename staged, then unstaged with its old path.
    let renamed = g.changes.iter().find(|e| e.old_path.is_some()).unwrap().clone();
    d.git("demo-project", "stage", &json!({ "paths": op_paths(&[renamed.clone()]) })).unwrap();
    let g = groups(&status(&d));
    let staged_rename = g.staged.iter().find(|e| e.path == renamed.path).unwrap().clone();
    assert_eq!(op_paths(&[staged_rename.clone()]).len(), 2);
    d.git("demo-project", "unstage", &json!({ "paths": op_paths(&[staged_rename]) })).unwrap();
    assert!(groups(&status(&d)).changes.iter().any(|e| e.path == renamed.path));

    // Commit what is staged; the committed file leaves the staged group and the branch is one ahead more.
    let load = ScmLoad::Ready(status(&d));
    let ahead = load.status().unwrap().ahead;
    let req = commit_gate(&load, "Docs: Add GIT.md\n\nWhy.", false).unwrap();
    let done: GitCommitted = git_result(d.git("demo-project", "commit", &commit_body(&req))).unwrap();
    assert_eq!(done.sha.len(), 40);
    let after = ScmLoad::Ready(status(&d));
    assert!(!groups(after.status().unwrap()).staged.iter().any(|e| e.path == "docs/GIT.md"));
    assert_eq!(after.status().unwrap().ahead, ahead + 1);
    assert_eq!(commit_gate(&after, "\n  \n", false).unwrap_err(), NEED_MESSAGE);
}
