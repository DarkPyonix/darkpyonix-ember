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

// FR-L31, #448: Problems capping and navigation, and output channels.
mod problems_and_output {
    use ember_app::ui::editor::{
        paginate_problems, push_workbench_log, MAX_VISIBLE_PROBLEMS, MAX_WORKBENCH_LOGS,
    };
    use ember_app::ui::editor_workbench::PendingGoto;

    #[test]
    fn fr_l31_problems_list_cap_and_overflow_count() {
        assert_eq!(MAX_VISIBLE_PROBLEMS, 200);

        // Under cap: all visible, no overflow
        let short_list: Vec<usize> = (0..50).collect();
        let (visible, overflow) = paginate_problems(&short_list, MAX_VISIBLE_PROBLEMS);
        assert_eq!(visible.len(), 50);
        assert_eq!(overflow, 0);

        // Over cap: capped at limit, remainder reported as overflow
        let long_list: Vec<usize> = (0..250).collect();
        let (visible, overflow) = paginate_problems(&long_list, MAX_VISIBLE_PROBLEMS);
        assert_eq!(visible.len(), 200);
        assert_eq!(overflow, 50);

        // Load-more increment renders subsequent slice
        let (visible_more, overflow_more) = paginate_problems(&long_list, 400);
        assert_eq!(visible_more.len(), 250);
        assert_eq!(overflow_more, 0);
    }

    #[test]
    fn fr_l31_pending_goto_records_line_and_col() {
        let goto = PendingGoto {
            id: 42,
            path: "/w/src/main.rs".into(),
            line: 15,
            col: 8,
        };
        assert_eq!(goto.id, 42);
        assert_eq!(goto.path, "/w/src/main.rs");
        assert_eq!(goto.line, 15);
        assert_eq!(goto.col, 8);
    }

    #[test]
    fn fr_l31_output_channels_and_message_severity() {
        use ember_editor::output::{MessageSeverity, OutputChannel, MAX_OUTPUT_CHANNEL_BYTES};

        let mut ch = OutputChannel::new("ember-output-1", "Git");
        assert_eq!(ch.label, "Git");
        ch.append("line 1\n");
        ch.append("line 2\n");
        assert_eq!(ch.text, "line 1\nline 2\n");

        ch.clear();
        assert_eq!(ch.text, "");

        // Huge append is capped at 64 KiB
        ch.append(&"x".repeat(MAX_OUTPUT_CHANNEL_BYTES + 500));
        assert_eq!(ch.text.len(), MAX_OUTPUT_CHANNEL_BYTES);

        assert_eq!(MessageSeverity::from_u8(1).label(), "정보");
        assert_eq!(MessageSeverity::from_u8(2).label(), "경고");
        assert_eq!(MessageSeverity::from_u8(3).label(), "오류");
    }

    #[test]
    fn fr_l31_workbench_logs_capped_at_max_lines() {
        let mut logs = Vec::new();
        for i in 0..(MAX_WORKBENCH_LOGS + 25) {
            push_workbench_log(&mut logs, format!("line {i}"));
        }
        assert_eq!(logs.len(), MAX_WORKBENCH_LOGS);
        assert_eq!(logs.first().map(String::as_str), Some("line 25"));
        assert_eq!(
            logs.last().map(String::as_str),
            Some(format!("line {}", MAX_WORKBENCH_LOGS + 24).as_str())
        );
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
    fn fr_l31_4_tree_f2_renames_selected_row_but_not_root() {
        let rows = sample_tree();
        assert_eq!(
            tree_key_action(&rows, Some(("/p/src/lib.rs", false)), "/p", "F2"),
            Some(TreeKeyOutcome::Rename("/p/src/lib.rs".to_string()))
        );
        assert_eq!(
            tree_key_action(&rows, Some(("/p/src", true)), "/p", "F2"),
            Some(TreeKeyOutcome::Rename("/p/src".to_string()))
        );
        assert_eq!(tree_key_action(&rows, None, "/p", "F2"), None);
        let mut with_root = rows.clone();
        with_root.insert(0, TreeRow { path: "/p".to_string(), name: "p".to_string(), depth: 0, is_dir: true, open: true });
        assert_eq!(tree_key_action(&with_root, Some(("/p", true)), "/p", "F2"), None);
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

    #[test]
    fn fr_l31_4_select_all_cut_selection_and_clipboard_badge() {
        use ember_app::ui::editor::{
            clipboard_badge_text, cut_selection, select_all_tree_rows, ClipboardKind, TreeClipboard,
        };
        let rows = sample_tree();
        let (all, last) = select_all_tree_rows(&rows);
        assert_eq!(all.len(), rows.len());
        assert_eq!(all[0], ("/p/src".to_string(), true));
        assert_eq!(last, Some(("/p/README.md".to_string(), false)));

        let (empty_all, empty_last) = select_all_tree_rows(&[]);
        assert!(empty_all.is_empty());
        assert_eq!(empty_last, None);

        // Cutting when row_path is part of multi_sel cuts all non-redundant selected items.
        let clip = cut_selection(&all, "/p/README.md", false).expect("non-empty cut");
        assert_eq!(clip.kind, ClipboardKind::Cut);
        // `/p/src/deep` and `/p/src/lib.rs` are pruned because `/p/src` is selected.
        assert_eq!(
            clip.paths,
            vec![
                ("/p/src".to_string(), true),
                ("/p/README.md".to_string(), false),
            ]
        );
        assert_eq!(clipboard_badge_text(Some(&clip)).as_deref(), Some("잘라냄 2개"));

        // Cutting a row outside multi_sel cuts only that row.
        let single = cut_selection(&[("/p/Cargo.toml".to_string(), false)], "/p/README.md", false)
            .expect("single cut");
        assert_eq!(single.paths, vec![("/p/README.md".to_string(), false)]);
        assert_eq!(clipboard_badge_text(Some(&single)).as_deref(), Some("잘라냄 1개"));

        // Empty clipboard handling.
        assert_eq!(
            clipboard_badge_text(Some(&TreeClipboard { kind: ClipboardKind::Cut, paths: Vec::new() })),
            None
        );
        assert_eq!(clipboard_badge_text(None), None);
    }

    #[test]
    fn fr_l31_roving_tabindex_avoids_duplicate_tab_stops_in_tree_and_tabs() {
        use ember_app::ui::editor::{tab_strip_roving_tabindex, tree_roving_tabindex};
        let rows = sample_tree();

        // When no tree row is selected, only the root folder row has tabindex "0" and the container is "-1".
        let (container_ti, root_ti, row_tis) = tree_roving_tabindex(&rows, None, &[]);
        assert_eq!(container_ti, "-1");
        assert_eq!(root_ti, "0");
        assert!(row_tis.iter().all(|&t| t == "-1"));
        let stops: usize = std::iter::once(container_ti)
            .chain(std::iter::once(root_ti))
            .chain(row_tis.iter().copied())
            .filter(|&t| t == "0")
            .count();
        assert_eq!(stops, 1, "tree with no selection must have exactly 1 tab stop");

        // When a single row is selected, only that row has tabindex "0"; container and root are "-1".
        let (container_ti, root_ti, row_tis) =
            tree_roving_tabindex(&rows, Some(("/p/src/lib.rs", false)), &[]);
        assert_eq!(container_ti, "-1");
        assert_eq!(root_ti, "-1");
        assert_eq!(row_tis, vec!["-1", "-1", "0", "-1"]);

        // When multiple rows are selected, only the primary (or first selected) row has tabindex "0" so Tab does not stop on every selected item.
        let multi = vec![
            ("/p/src".to_string(), true),
            ("/p/src/lib.rs".to_string(), false),
            ("/p/README.md".to_string(), false),
        ];
        let (container_ti, root_ti, row_tis) =
            tree_roving_tabindex(&rows, Some(("/p/README.md", false)), &multi);
        assert_eq!(container_ti, "-1");
        assert_eq!(root_ti, "-1");
        assert_eq!(row_tis, vec!["-1", "-1", "-1", "0"]);

        // Tab strip: when empty, container is "-1" and there are no tab stops.
        let (strip_ti, tab_tis) = tab_strip_roving_tabindex(&[], None);
        assert_eq!(strip_ti, "-1");
        assert!(tab_tis.is_empty());

        // Tab strip with tabs: container is "-1" and only the active tab (or first tab if none matches) has "0".
        let paths = vec!["/p/a.rs".to_string(), "/p/b.rs".to_string(), "/p/c.rs".to_string()];
        let (strip_ti, tab_tis) = tab_strip_roving_tabindex(&paths, Some("/p/b.rs"));
        assert_eq!(strip_ti, "-1");
        assert_eq!(tab_tis, vec!["-1", "0", "-1"]);

        let (strip_ti_fallback, tab_tis_fallback) = tab_strip_roving_tabindex(&paths, None);
        assert_eq!(strip_ti_fallback, "-1");
        assert_eq!(tab_tis_fallback, vec!["0", "-1", "-1"]);
    }

    #[test]
    fn fr_l31_tab_strip_delete_refuses_dirty_tab_with_polite_notice() {
        use ember_app::ui::editor::{dirty_tab_close_notice, tab_strip_key_with_dirty, TabKeyOutcome};
        let paths = vec![
            "/w/p/clean.rs".to_string(),
            "/w/p/dirty.rs".to_string(),
        ];
        let dirty = vec![false, true];

        // Clean tab closes on Delete or Backspace.
        assert_eq!(
            tab_strip_key_with_dirty(&paths, &dirty, Some("/w/p/clean.rs"), "Delete"),
            Some(TabKeyOutcome::Close("/w/p/clean.rs".to_string()))
        );
        assert_eq!(
            tab_strip_key_with_dirty(&paths, &dirty, Some("/w/p/clean.rs"), "Backspace"),
            Some(TabKeyOutcome::Close("/w/p/clean.rs".to_string()))
        );

        // Dirty tab refuses to close on Delete or Backspace.
        assert_eq!(
            tab_strip_key_with_dirty(&paths, &dirty, Some("/w/p/dirty.rs"), "Delete"),
            Some(TabKeyOutcome::RefuseDirty("/w/p/dirty.rs".to_string()))
        );
        assert_eq!(
            tab_strip_key_with_dirty(&paths, &dirty, Some("/w/p/dirty.rs"), "Backspace"),
            Some(TabKeyOutcome::RefuseDirty("/w/p/dirty.rs".to_string()))
        );

        let notice = dirty_tab_close_notice("/w/p/dirty.rs");
        assert!(notice.contains("dirty.rs"), "{notice}");
        assert!(notice.ends_with("니다.") || notice.ends_with("세요."), "{notice}");
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

// FR-L31.2 and #444: Diff gutter and editor source control tests.
#[test]
fn fr_l31_2_diff_gutter_and_scm_helpers() {
    use ember_app::ui::editor::{compute_line_diffs, diff_gutter_decorations, DiffLineKind, DiffLineMarker};
    use ember_app::ui::editor_scm::{
        commit_gate_with_amend, discard_patch, discard_tap, markers_from_file_diff, reconstruct_base_text,
        side_by_side_rows, ScmLoad,
    };
    use ember_app::ui::vcs::{
        parse_unified, ConflictChoice, ConflictFile, ConflictSegment, DeleteTap, FileDiff, LineKind,
    };
    use ember_client::api::{GitFile, GitStatus};

    // 1. Line diff markers
    let old_text = "alpha\nbeta\ngamma\ndelta";
    let new_text = "alpha\nbeta_mod\ngamma\ndelta\nepsilon";
    let markers = compute_line_diffs(old_text, new_text);
    assert_eq!(markers, vec![
        DiffLineMarker { line: 1, kind: DiffLineKind::Modified },
        DiffLineMarker { line: 4, kind: DiffLineKind::Added },
    ]);

    // 2. Diff gutter decorations
    let decs = diff_gutter_decorations(1, &markers);
    assert_eq!(decs.decorations().len(), 2);

    // 3. Reconstruct base text
    let patch = "diff --git a/test.rs b/test.rs\n--- a/test.rs\n+++ b/test.rs\n@@ -1,4 +1,5 @@\n alpha\n-beta\n+beta_mod\n gamma\n delta\n+epsilon\n";
    let diff = parse_unified(patch).into_iter().next().unwrap();
    let base = reconstruct_base_text(new_text, &diff);
    assert_eq!(base, old_text);

    // 4. Markers from file diff
    let m_fd = markers_from_file_diff(&diff, 5);
    assert_eq!(m_fd, vec![
        DiffLineMarker { line: 1, kind: DiffLineKind::Modified },
        DiffLineMarker { line: 4, kind: DiffLineKind::Added },
    ]);

    // 5. Side by side rows
    let sbs = side_by_side_rows(&diff.hunks[0]);
    assert_eq!(sbs.len(), 5);
    // Context alpha
    assert_eq!(sbs[0].0.as_ref().unwrap().1, LineKind::Context);
    assert_eq!(sbs[0].1.as_ref().unwrap().1, LineKind::Context);
    // beta (del on left, mod on right)
    assert_eq!(sbs[1].0.as_ref().unwrap().1, LineKind::Del);
    assert_eq!(sbs[1].1.as_ref().unwrap().1, LineKind::Add);
    // Context gamma
    assert_eq!(sbs[2].0.as_ref().unwrap().1, LineKind::Context);
    // Context delta
    assert_eq!(sbs[3].0.as_ref().unwrap().1, LineKind::Context);
    // epsilon (None on left, Add on right)
    assert!(sbs[4].0.is_none());
    assert_eq!(sbs[4].1.as_ref().unwrap().1, LineKind::Add);

    // 6. Amend commit gate
    let status = GitStatus {
        branch: "main".into(),
        files: vec![GitFile { path: "a.rs".into(), old_path: None, index: '.', worktree: 'M' }],
        ..GitStatus::default()
    };
    let ready = ScmLoad::Ready(status);
    let req = commit_gate_with_amend(&ready, "Fix: amend previous\n\nDetails", true, false).unwrap();
    assert!(req.amend);

    // 7. Discard patch and confirmation tap
    let dp = discard_patch(&diff).unwrap();
    assert!(dp.contains("+beta_mod"));
    assert_eq!(discard_tap(None, "test.rs", 100), DeleteTap::Arm);
    assert_eq!(discard_tap(Some(("test.rs", 100)), "test.rs", 300), DeleteTap::Wait);
    assert_eq!(discard_tap(Some(("test.rs", 100)), "test.rs", 800), DeleteTap::Confirm);
    assert_eq!(discard_tap(Some(("test.rs", 100)), "test.rs", 4200), DeleteTap::Expired);

    // 8. Conflict file resolution
    let cf = ConflictFile {
        path: "conf.rs".into(),
        segments: vec![
            ConflictSegment::Common("fn main() {\n".into()),
            ConflictSegment::Block {
                ours_label: "HEAD".into(),
                theirs_label: "feature".into(),
                ours: "    println!(\"ours\");\n".into(),
                theirs: "    println!(\"theirs\");\n".into(),
                choice: ConflictChoice::Unresolved,
            },
            ConflictSegment::Common("}\n".into()),
        ],
    };
    assert!(!cf.is_resolved());
    let cf_ours = cf.with_choice(0, ConflictChoice::Ours);
    assert!(cf_ours.is_resolved());
    assert_eq!(cf_ours.resolved_content().unwrap(), "fn main() {\n    println!(\"ours\");\n}\n");
    let cf_both = cf.with_choice(0, ConflictChoice::Both);
    assert_eq!(cf_both.resolved_content().unwrap(), "fn main() {\n    println!(\"ours\");\n    println!(\"theirs\");\n}\n");
}


// #477: Remaining mockup differences (FR-L31): close_others, locate ancestors, modified path set from real SCM status, empty state hints, tab_width text, tree/tab classes.
#[test]
fn fr_l31_close_others_keeps_only_active_tab_and_returns_closed_paths() {
    use ember_app::ui::editor::EditorTabs;
    let mut tabs = EditorTabs::default();
    assert!(tabs.close_others().is_empty());

    tabs.open("/w/a.rs");
    tabs.open("/w/b.rs");
    tabs.open("/w/c.rs");
    tabs.set_dirty("/w/b.rs", true);
    tabs.activate("/w/b.rs");

    let closed = tabs.close_others();
    assert_eq!(closed, vec!["/w/a.rs".to_string(), "/w/c.rs".to_string()]);
    assert_eq!(tabs.paths(), vec!["/w/b.rs"]);
    assert_eq!(tabs.current_path(), Some("/w/b.rs"));
    assert!(tabs.is_dirty("/w/b.rs"));
}

#[test]
fn fr_l31_ancestor_dirs_to_expand_reveals_active_file_below_root() {
    use ember_app::ui::editor::ancestor_dirs_to_expand;
    assert_eq!(
        ancestor_dirs_to_expand("/w/p", "/w/p/ember/proxy/src/shim.rs"),
        vec![
            "/w/p".to_string(),
            "/w/p/ember".to_string(),
            "/w/p/ember/proxy".to_string(),
            "/w/p/ember/proxy/src".to_string(),
        ]
    );
    assert_eq!(
        ancestor_dirs_to_expand("/w/p/", "/w/p/README.md"),
        vec!["/w/p".to_string()]
    );
    assert!(ancestor_dirs_to_expand("/w/p", "/other/README.md").is_empty());
}

#[test]
fn fr_l31_modified_paths_set_reflects_only_real_scm_status() {
    use ember_app::ui::editor_scm::{modified_paths_set, ScmLoad};
    use ember_client::api::{GitFile, GitStatus};

    // Without a ready SCM status, no file is marked modified.
    assert!(modified_paths_set("/w/p", &ScmLoad::NoProject).is_empty());
    assert!(modified_paths_set("/w/p", &ScmLoad::Loading).is_empty());
    assert!(modified_paths_set("/w/p", &ScmLoad::NotRepo).is_empty());
    assert!(modified_paths_set("/w/p", &ScmLoad::Unavailable("offline".into())).is_empty());

    // With a ready status, modified/added/conflict files are included as absolute paths, while clean/ignored or deleted-only are not.
    let status = GitStatus {
        branch: "develop".into(),
        files: vec![
            GitFile { path: "src/shim.rs".into(), old_path: None, index: '.', worktree: 'M' },
            GitFile { path: "src/router.rs".into(), old_path: None, index: 'M', worktree: '.' },
            GitFile { path: "docs/SPEC.md".into(), old_path: None, index: '?', worktree: '?' },
            GitFile { path: "src/deleted.rs".into(), old_path: None, index: '.', worktree: 'D' },
        ],
        ..GitStatus::default()
    };
    let set = modified_paths_set("/w/p", &ScmLoad::Ready(status));
    assert!(set.contains("/w/p/src/shim.rs"));
    assert!(set.contains("/w/p/src/router.rs"));
    assert!(set.contains("/w/p/docs/SPEC.md"));
    assert!(!set.contains("/w/p/src/deleted.rs"));
    assert!(!set.contains("/w/p/Cargo.toml"));
}

#[test]
fn fr_l31_empty_state_hints_and_row_tab_classes_and_tab_width_text() {
    use ember_app::ui::editor::{empty_state_hints, tab_class, tab_width_text, tree_row_class};
    use ember_app::ui::keymap::{Keymap, Os};

    let ((find_hint, find_label), (proj_hint, proj_label)) = empty_state_hints(Keymap::VsCode, Os::Mac);
    assert_eq!(find_hint, "\u{2318}P");
    assert_eq!(find_label, "파일로 이동");
    assert_eq!(proj_hint, "\u{21E7}\u{2318}E");
    assert_eq!(proj_label, "프로젝트 창");

    let ((jb_find_hint, jb_find_label), (jb_proj_hint, jb_proj_label)) = empty_state_hints(Keymap::JetBrains, Os::Mac);
    assert_eq!(jb_find_hint, "\u{21E7}\u{21E7}");
    assert_eq!(jb_find_label, "모든 곳에서 찾기");
    assert_eq!(jb_proj_hint, "\u{2318}1");
    assert_eq!(jb_proj_label, "프로젝트 창");

    assert_eq!(tree_row_class(false, false), "ed-tn");
    assert_eq!(tree_row_class(true, false), "ed-tn sel");
    assert_eq!(tree_row_class(false, true), "ed-tn mod");
    assert_eq!(tree_row_class(true, true), "ed-tn sel mod");

    assert_eq!(tab_class(false, false), "ed-tab");
    assert_eq!(tab_class(true, false), "ed-tab on");
    assert_eq!(tab_class(false, true), "ed-tab vc-mod");
    assert_eq!(tab_class(true, true), "ed-tab on vc-mod");

    assert_eq!(tab_width_text(4), "4칸");
    assert_eq!(tab_width_text(2), "2칸");
    assert_eq!(tab_width_text(0), "4칸");
}

#[test]
fn fr_e6_folds_prop_encodes_fold_regions_and_bridge_toggles_fold() {
    let region = ember_editor::FoldRegion {
        version: 1,
        start_line: 1,
        end_line: 4,
        collapsed: true,
    };
    let encoded = ember_app::ui::editor::folds(&[region]);
    let regions = encoded.folds();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].version, 1);
    assert_eq!(regions[0].start_line, 1);
    assert_eq!(regions[0].end_line, 4);
    assert!(regions[0].collapsed);

    let mut bridge = ember_editor::bridge::WidgetBridge::new();
    let uri = ember_editor_conn::uri::UriComponents::remote("127.0.0.1:9", "/w/a.ts");
    bridge.apply(ember_editor::SessionUpdate::Opened {
        uri: uri.clone(),
        generation: 1,
        text: "line0\n  line1\n  line2\n  line3\nline4\n".into(),
        language_id: "typescript".into(),
        tab_width: 4,
    });
    bridge.apply(ember_editor::SessionUpdate::Folds {
        uri: uri.clone(),
        generation: 1,
        version: 0,
        folds: vec![ember_editor::FoldRegion {
            version: 0,
            start_line: 1,
            end_line: 4,
            collapsed: false,
        }],
    });

    let res = bridge.on_fold_changed(&uri, 1, true).expect("event from bridge");
    assert_eq!(res.0, uri);
    assert_eq!(res.1, 1);
    assert_eq!(
        res.2,
        ember_editor::WidgetEvent::FoldChanged {
            start_line: 1,
            collapsed: true,
        }
    );

    let doc = bridge.document(&uri).expect("document in bridge");
    assert_eq!(doc.folds.len(), 1);
    assert_eq!(doc.folds[0].start_line, 1);
    assert_eq!(doc.folds[0].end_line, 4);
    assert!(doc.folds[0].collapsed);
}

// FR-L31.5 (#439): tabs pin, preview, reorder, close others / right / all and the unsaved confirm, as Code-OSS.
mod fr_l31_tabs {
    use ember_app::ui::editor::{tab_class_with, tab_menu_items, tab_move_key, CloseResolution, DirtyChoice, EditorTabs, SaveOutcome, TabMenuItem};

    const SAVED_AT_7: SaveOutcome = SaveOutcome { ok: true, version: Some(7), dirty: false };

    fn tabs(paths: &[&str]) -> EditorTabs {
        let mut t = EditorTabs::default();
        for p in paths {
            t.open(p);
        }
        t
    }

    #[test]
    fn fr_l31_tabs_pin_moves_to_the_end_of_the_pinned_group_and_unpin_to_the_first_unpinned_place() {
        let mut t = tabs(&["a", "b", "c", "d"]);
        t.activate("b");
        t.pin("c");
        assert_eq!(t.paths(), vec!["c", "a", "b", "d"]);
        assert!(t.is_pinned("c"));
        assert_eq!(t.current_path(), Some("b"), "pinning another tab keeps the current one");
        t.pin("d");
        assert_eq!(t.paths(), vec!["c", "d", "a", "b"]);
        t.pin("d"); // already pinned: no change
        assert_eq!(t.paths(), vec!["c", "d", "a", "b"]);
        t.unpin("c");
        assert_eq!(t.paths(), vec!["d", "c", "a", "b"]);
        assert!(!t.is_pinned("c"));
        t.toggle_pin("a");
        assert_eq!(t.paths(), vec!["d", "a", "c", "b"]);
        assert!(t.is_pinned("a"));
        t.toggle_pin("d");
        assert_eq!(t.paths(), vec!["a", "d", "c", "b"], "unpinned d goes right after the pinned a");
        t.toggle_pin("d");
        assert_eq!(t.paths(), vec!["a", "d", "c", "b"]);
        // A newly opened tab goes after the pinned ones.
        t.open("e");
        assert_eq!(t.paths(), vec!["a", "d", "c", "b", "e"]);
    }

    #[test]
    fn fr_l31_tabs_preview_is_replaced_by_the_next_preview_and_returns_the_replaced_path() {
        let mut t = tabs(&["a"]);
        assert_eq!(t.open_preview("p1"), None);
        assert!(t.is_preview("p1"));
        assert_eq!(t.current_path(), Some("p1"));
        t.open("b");
        // The next preview takes the old preview's place, not the end.
        assert_eq!(t.open_preview("p2"), Some("p1".to_string()));
        assert_eq!(t.paths(), vec!["a", "p2", "b"]);
        assert_eq!(t.current_path(), Some("p2"));
        // Previewing a file already open only activates it.
        assert_eq!(t.open_preview("a"), None);
        assert_eq!(t.current_path(), Some("a"));
        assert!(!t.is_preview("a"));
        assert_eq!(t.paths(), vec!["a", "p2", "b"]);
    }

    #[test]
    fn fr_l31_tabs_preview_becomes_permanent_by_edit_double_click_open_or_pin() {
        let mut t = EditorTabs::default();
        t.open_preview("p1");
        t.set_dirty("p1", true);
        assert!(!t.is_preview("p1"), "an edit keeps the tab");
        assert_eq!(t.open_preview("p2"), None, "a permanent tab is not replaced");
        assert_eq!(t.paths(), vec!["p1", "p2"]);
        t.keep_open("p2");
        assert!(!t.is_preview("p2"));
        t.open_preview("p3");
        t.open("p3");
        assert!(!t.is_preview("p3"), "an explicit open keeps the tab");
        t.open_preview("p4");
        t.pin("p4");
        assert!(!t.is_preview("p4") && t.is_pinned("p4"));
        assert_eq!(t.open_preview("p5"), None);
        assert_eq!(t.paths(), vec!["p4", "p1", "p2", "p3", "p5"]);
    }

    #[test]
    fn fr_l31_tabs_reorder_moves_within_its_group_and_keeps_the_current_tab() {
        let mut t = tabs(&["a", "b", "c", "d"]);
        t.pin("a");
        t.activate("c");
        assert!(t.move_left("c"));
        assert_eq!(t.paths(), vec!["a", "c", "b", "d"]);
        assert_eq!(t.current_path(), Some("c"));
        assert!(!t.move_left("c"), "an unpinned tab does not cross into the pinned group");
        assert!(t.move_right("c"));
        assert!(t.move_right("c"));
        assert_eq!(t.paths(), vec!["a", "b", "d", "c"]);
        assert!(!t.move_right("c"), "the last tab does not move right");
        assert!(!t.move_right("a"), "a pinned tab does not cross into the unpinned group");
        assert!(t.move_to("c", 1));
        assert_eq!(t.paths(), vec!["a", "c", "b", "d"]);
        assert!(!t.move_to("c", 0), "move_to clamps to the group");
        assert!(!t.move_left("missing"));
        assert_eq!(t.current_path(), Some("c"));
    }

    #[test]
    fn fr_l31_tabs_close_others_right_and_all_skip_pinned_tabs() {
        let mut t = tabs(&["a", "b", "c", "d", "e"]);
        t.pin("d");
        // Order: d (pinned), a, b, c, e.
        assert_eq!(t.paths(), vec!["d", "a", "b", "c", "e"]);
        let others = t.close_others_request("b");
        assert_eq!(others.paths, vec!["a", "c", "e"]);
        assert_eq!(others.anchor.as_deref(), Some("b"));
        let right = t.close_right_request("a");
        assert_eq!(right.paths, vec!["b", "c", "e"]);
        let right_of_pinned = t.close_right_request("d");
        assert_eq!(right_of_pinned.paths, vec!["a", "b", "c", "e"]);
        let all = t.close_all_request();
        assert_eq!(all.paths, vec!["a", "b", "c", "e"]);
        assert!(!all.needs_confirm());
        t.close_many(&right.paths, right.anchor.as_deref());
        assert_eq!(t.paths(), vec!["d", "a"]);
        assert_eq!(t.current_path(), Some("a"));
        // The old ⋯ menu entry keeps working and keeps pinned tabs.
        t.open("f");
        t.activate("f");
        assert_eq!(t.close_others(), vec!["a".to_string()]);
        assert_eq!(t.paths(), vec!["d", "f"]);
        assert_eq!(t.current_path(), Some("f"));
    }

    #[test]
    fn fr_l31_tabs_closing_a_dirty_tab_returns_a_confirm_request_with_save_dont_save_and_cancel() {
        let mut t = tabs(&["/w/a.rs", "/w/b.rs", "/w/c.rs"]);
        let clean = t.close_request("/w/a.rs");
        assert!(!clean.needs_confirm());
        assert_eq!(clean.paths, vec!["/w/a.rs"]);
        assert!(t.close_request("/w/missing.rs").is_empty());

        t.set_dirty("/w/b.rs", true);
        let one = t.close_request("/w/b.rs");
        assert!(one.needs_confirm());
        assert_eq!(one.dirty, vec!["/w/b.rs"]);
        assert_eq!(one.confirm_title(), "b.rs의 변경 내용을 저장하시겠습니까?");
        assert_eq!(one.confirm_detail(), "저장하지 않으면 변경 내용이 손실됩니다.");
        assert_eq!(one.save_label(), "저장");
        assert!(one.dirty_names().is_empty());
        assert_eq!(one.resolve(DirtyChoice::Save), CloseResolution { save: vec!["/w/b.rs".into()], close: vec!["/w/b.rs".into()] });
        assert_eq!(one.resolve(DirtyChoice::DontSave), CloseResolution { save: vec![], close: vec!["/w/b.rs".into()] });
        assert_eq!(one.resolve(DirtyChoice::Cancel), CloseResolution::default());

        t.set_dirty("/w/c.rs", true);
        let all = t.close_all_request();
        assert_eq!(all.paths, vec!["/w/a.rs", "/w/b.rs", "/w/c.rs"]);
        assert_eq!(all.dirty, vec!["/w/b.rs", "/w/c.rs"]);
        assert_eq!(all.confirm_title(), "다음 2개 파일의 변경 내용을 저장하시겠습니까?");
        assert_eq!(all.save_label(), "모두 저장");
        assert_eq!(all.dirty_names(), vec!["b.rs", "c.rs"]);
        let saved = all.resolve(DirtyChoice::Save);
        assert_eq!(saved.save, vec!["/w/b.rs", "/w/c.rs"]);
        assert_eq!(saved.close, vec!["/w/a.rs", "/w/b.rs", "/w/c.rs"]);
        // Cancel closes nothing, not even the clean tab, as in Code-OSS.
        assert!(all.resolve(DirtyChoice::Cancel).close.is_empty());
    }

    #[test]
    fn fr_l31_tabs_context_menu_items_follow_code_oss_and_disable_what_cannot_run() {
        let mut t = tabs(&["a", "b", "c"]);
        let items = tab_menu_items(&t, "c");
        let labels: Vec<&str> = items.iter().map(|(i, _)| i.label()).collect();
        assert_eq!(labels, vec!["닫기", "다른 항목 닫기", "오른쪽에 있는 항목 닫기", "모두 닫기", "고정", "왼쪽으로 이동", "오른쪽으로 이동"]);
        assert!(items.contains(&(TabMenuItem::CloseRight, false)), "nothing is right of the last tab");
        assert!(items.contains(&(TabMenuItem::MoveRight, false)));
        assert!(items.contains(&(TabMenuItem::MoveLeft, true)));
        t.pin("a");
        let pinned = tab_menu_items(&t, "a");
        assert!(pinned.contains(&(TabMenuItem::Unpin, true)));
        assert!(pinned.contains(&(TabMenuItem::MoveRight, false)), "the only pinned tab cannot move");
        t.open_preview("p");
        assert!(tab_menu_items(&t, "p").contains(&(TabMenuItem::KeepOpen, true)));
        assert!(tab_menu_items(&t, "missing").is_empty());
    }

    #[test]
    fn fr_l31_tabs_class_marks_pinned_and_preview_and_move_keys_are_the_same_on_every_os() {
        assert_eq!(tab_class_with(true, false, true, false), "ed-tab on pinned");
        assert_eq!(tab_class_with(false, true, false, true), "ed-tab vc-mod preview");
        assert_eq!(tab_class_with(false, false, false, false), "ed-tab");
        assert_eq!(tab_move_key("PageUp", true, true, false, false), Some(false));
        assert_eq!(tab_move_key("PageDown", true, true, false, false), Some(true));
        assert_eq!(tab_move_key("PageDown", true, false, false, false), None);
        assert_eq!(tab_move_key("PageDown", true, true, false, true), None);
        assert_eq!(tab_move_key("ArrowLeft", true, true, false, false), None);
    }

    #[test]
    fn fr_l31_tabs_save_in_the_confirm_closes_a_dirty_tab_only_after_its_save_succeeds() {
        let mut t = tabs(&["/w/a.rs", "/w/b.rs", "/w/c.rs"]);
        t.set_dirty("/w/b.rs", true);
        t.set_dirty("/w/c.rs", true);
        let request = t.close_all_request();
        // 저장: the clean tab closes now, the dirty ones wait for their saves.
        let now = t.begin_close(&request, DirtyChoice::Save, |_| Some(7));
        assert_eq!(now, vec!["/w/a.rs"]);
        t.close_many(&now, None);
        assert!(t.is_closing("/w/b.rs") && t.is_closing("/w/c.rs"));
        assert_eq!(t.paths(), vec!["/w/b.rs", "/w/c.rs"]);
        // Success closes the tab.
        t.set_dirty("/w/b.rs", false);
        assert!(t.save_finished("/w/b.rs", SAVED_AT_7));
        t.close("/w/b.rs");
        assert!(!t.is_closing("/w/b.rs"));
        // Failure keeps the tab open and dirty.
        assert!(!t.save_finished("/w/c.rs", SaveOutcome { ok: false, ..SAVED_AT_7 }));
        assert_eq!(t.paths(), vec!["/w/c.rs"]);
        assert!(t.is_dirty("/w/c.rs"));
        assert!(!t.is_closing("/w/c.rs"));
        // A later save of that tab (Cmd+S) no longer closes it.
        assert!(!t.save_finished("/w/c.rs", SAVED_AT_7));
        assert_eq!(t.paths(), vec!["/w/c.rs"]);
    }

    #[test]
    fn fr_l31_tabs_an_edit_typed_while_the_save_is_in_flight_keeps_the_tab_open_and_dirty() {
        let mut t = tabs(&["/w/a.rs", "/w/b.rs"]);
        t.set_dirty("/w/a.rs", true);
        t.set_dirty("/w/b.rs", true);
        let request = t.close_all_request();
        assert!(t.begin_close(&request, DirtyChoice::Save, |_| Some(7)).is_empty());
        // The document moved past the saved version: the save succeeded but the tab stays.
        assert!(!t.save_finished("/w/a.rs", SaveOutcome { version: Some(8), ..SAVED_AT_7 }));
        assert!(t.is_dirty("/w/a.rs") && !t.is_closing("/w/a.rs"));
        // The session still reports it dirty after the save: the tab stays.
        assert!(!t.save_finished("/w/b.rs", SaveOutcome { dirty: true, ..SAVED_AT_7 }));
        assert!(t.is_dirty("/w/b.rs") && !t.is_closing("/w/b.rs"));
        assert_eq!(t.paths(), vec!["/w/a.rs", "/w/b.rs"]);
    }

    #[test]
    fn fr_l31_tabs_dont_save_closes_all_at_once_and_cancel_closes_none() {
        let mut t = tabs(&["/w/a.rs", "/w/b.rs"]);
        t.set_dirty("/w/b.rs", true);
        let request = t.close_all_request();
        assert!(t.begin_close(&request, DirtyChoice::Cancel, |_| None).is_empty());
        assert!(!t.is_closing("/w/b.rs"));
        assert_eq!(t.begin_close(&request, DirtyChoice::DontSave, |_| None), vec!["/w/a.rs", "/w/b.rs"]);
        assert!(!t.is_closing("/w/b.rs"));
    }

    #[test]
    fn fr_l31_tabs_two_clicks_on_one_target_within_500_ms_are_a_double_click() {
        use ember_app::ui::editor::{DoubleClick, DOUBLE_CLICK_MS};
        assert_eq!(DOUBLE_CLICK_MS, 500);
        let mut d = DoubleClick::default();
        assert!(!d.click("tab:a", 1_000));
        assert!(d.click("tab:a", 1_500), "the second click at 500 ms is a double-click");
        assert!(!d.click("tab:a", 1_600), "the pair is used up: a third click starts over");
        assert!(!d.click("tab:a", 2_101), "501 ms later is too slow");
        assert!(!d.click("tab:b", 2_200), "another target is not a double-click");
        assert!(!d.click("tab:a", 2_300));
        assert!(d.click("tab:a", 2_400));
    }

    #[test]
    fn fr_l31_tabs_shift_f10_and_the_context_menu_key_open_the_tab_menu() {
        use ember_app::ui::editor::tab_menu_key;
        assert!(tab_menu_key("F10", false, true, false, false));
        assert!(tab_menu_key("ContextMenu", false, false, false, false));
        assert!(!tab_menu_key("F10", false, false, false, false), "F10 alone is not the menu key");
        assert!(!tab_menu_key("F10", true, true, false, false));
        assert!(!tab_menu_key("ContextMenu", false, true, false, false));
        assert!(!tab_menu_key("Enter", false, true, false, false));
    }
}

// Design review 2026-11-13 (A6, A7, A11, A12, A13, A14) and the owner placement rule of the same day
// (rules 4 and 5): the editor chrome as the mockup draws it.
mod design_chrome {
    use ember_app::ui::editor::{ToolWindow, FIND_PILL_LABEL};

    fn compact_css() -> String {
        let css = ember_app::ui::editor_workbench::page::config().stylesheets.join("\n");
        css.chars().filter(|c| !c.is_whitespace()).collect()
    }

    const PAGE: &str = include_str!("../src/ui/editor_workbench.rs");

    #[test]
    fn design_a13_stripes_are_exactly_the_mockups() {
        assert_eq!(ToolWindow::LEFT_STRIPE, [ToolWindow::Project, ToolWindow::Commit, ToolWindow::Git]);
        assert_eq!(ToolWindow::BOTTOM_STRIPE, [ToolWindow::Terminal, ToolWindow::Run, ToolWindow::Problems]);
        // 검색 and 출력 keep their windows: search on the left, output as a bottom tab.
        assert_eq!(ToolWindow::Search.side(), ember_app::ui::editor::Side::Left);
        assert!(ToolWindow::BOTTOM.contains(&ToolWindow::Output));
        assert!(PAGE.contains("&ToolWindow::LEFT_STRIPE") && PAGE.contains("&ToolWindow::BOTTOM_STRIPE"));
    }

    #[test]
    fn design_a13_the_line_above_the_bottom_group_is_drawn_as_an_element() {
        let css = compact_css();
        assert!(css.contains(".ed-stripe-sep{flex:none;align-self:center;width:28px;height:1px;background:var(--line)}"));
        assert!(PAGE.contains("div { class: \"ed-stripe-sep\", role: \"separator\" }"));
    }

    #[test]
    fn design_a7_bottom_tab_labels_are_centred() {
        assert!(compact_css().contains(".ed-btabsbutton{display:inline-flex;align-items:center;justify-content:center;flex:none}"));
    }

    #[test]
    fn design_a11_status_bar_is_sbh_high_on_one_centre_line() {
        let css = compact_css();
        assert!(css.contains(".ed-status{height:var(--sbh);align-self:end;box-sizing:border-box}"));
        assert!(css.contains(".ed-sbi.crumbs-s{align-self:center}"));
    }

    #[test]
    fn design_a12_find_pill_is_300_wide_with_an_8px_gap_and_the_mockup_label() {
        assert_eq!(FIND_PILL_LABEL, "파일, 동작, 기호 찾기");
        let css = compact_css();
        assert!(css.contains("@media(min-width:1101px){.ed-top.ed-find{flex:none;width:300px}}"));
        assert!(css.contains(".ed-find>span.hide-md{margin-left:var(--sp2)}"));
        assert!(css.contains("@media(max-width:560px){.ed-top.ed-find{display:none}}"));
        assert!(PAGE.contains("span { class: \"hide-md\", \"{crate::ui::editor::FIND_PILL_LABEL}\" }"));
    }

    #[test]
    fn design_a6_palette_overlay_takes_no_grid_row() {
        assert!(PAGE.contains("style: \"position:fixed; grid-row:1; grid-column:1; left:0; top:0; right:0; bottom:0; z-index:39;\""));
        assert!(PAGE.contains("style: \"position:fixed; grid-row:1; grid-column:1; z-index:40; top:60px;"));
        assert!(ember_app::ui::editor_menu::menu_style().contains("grid-row:1; grid-column:1;"));
    }

    #[test]
    fn design_a14_top_bar_actions_follow_the_mockup_order_without_a_menu_button() {
        let top = &PAGE[PAGE.find("fn top_bar(").unwrap()..PAGE.find("fn menu(s: &Shared").unwrap()];
        let at = |label: &str| top.find(&format!("title: \"{label}\"")).unwrap_or_else(|| panic!("{label} missing"));
        let runcfg = at("실행 구성");
        let run = at("실행");
        let stop = at("중지");
        let ask = at("에이전트에게 묻기");
        let theme = at("{theme_title}");
        assert!(runcfg < run && run < stop && stop < ask && ask < theme, "order is not run config, run, stop, ask, theme");
        assert!(!top.contains("icon(\"more\")"), "the top bar still has a ⋯ button");
    }
}
