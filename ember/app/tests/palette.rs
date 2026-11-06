//! The command palette's model (D38, #447): fuzzy ranking of commands and files, and the list of
//! the editor's own actions it starts with. No renderer.

use ember_app::ui::keymap::{Action, Keymap, Os};
use ember_app::ui::palette::{action_items, file_items, highlight_segments, items_for, mode_for, move_selection, placeholder, rank, skipped_directory, Item, Mode, Payload};

fn items(titles: &[&str]) -> Vec<Item> {
    titles
        .iter()
        .map(|t| Item { payload: Payload::File((*t).to_string()), title: (*t).to_string(), hint: String::new(), enabled: true })
        .collect()
}

fn titles(list: &[Item], ranked: &[ember_app::ui::palette::Ranked]) -> Vec<String> {
    ranked.iter().map(|r| list[r.index].title.clone()).collect()
}

#[test]
fn fr_l31_an_empty_query_lists_everything_in_order() {
    let list = items(&["b", "a", "c"]);
    let ranked = rank("", &list);
    assert_eq!(titles(&list, &ranked), vec!["b", "a", "c"]);
    assert!(ranked.iter().all(|r| r.matched.is_empty()));
    assert_eq!(titles(&list, &rank("   ", &list)), vec!["b", "a", "c"], "blanks are not a query");
}

#[test]
fn fr_l31_only_titles_that_hold_the_query_letters_in_order_match() {
    let list = items(&["Save All", "Close Tab", "Save"]);
    assert_eq!(titles(&list, &rank("sv", &list)), vec!["Save", "Save All"]);
    assert!(rank("zz", &list).is_empty());
    assert!(rank("evas", &list).is_empty(), "the letters must come in the query's order");
}

#[test]
fn fr_l31_matching_ignores_case_and_reports_the_matched_characters() {
    let list = items(&["Go To Line"]);
    let ranked = rank("gtl", &list);
    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].matched, vec![0, 3, 6], "the first letter of each word");
}

#[test]
fn fr_l31_word_starts_and_runs_beat_scattered_letters() {
    let list = items(&["Seeing a very long string", "Save File", "Safari"]);
    let ranked = rank("sf", &list);
    assert_eq!(list[ranked[0].index].title, "Save File", "S and F both start a word");
    let list = items(&["Show All Files", "Save"]);
    assert_eq!(list[rank("save", &list)[0].index].title, "Save", "a whole word beats a spread");
}

#[test]
fn fr_l31_a_prefix_beats_the_same_letters_later_in_the_title() {
    let list = items(&["Close Terminal", "Terminal"]);
    assert_eq!(list[rank("term", &list)[0].index].title, "Terminal");
}

#[test]
fn fr_l31_equal_scores_prefer_the_shorter_title_then_keep_the_list_order() {
    let list = items(&["alpha one", "alpha two"]);
    assert_eq!(titles(&list, &rank("alpha", &list)), vec!["alpha one", "alpha two"]);
    let list = items(&["Save All", "Save"]);
    assert_eq!(titles(&list, &rank("save", &list)), vec!["Save", "Save All"]);
}

#[test]
fn fr_l31_korean_titles_match_by_character() {
    let list = items(&["탭 닫기", "저장", "터미널"]);
    assert_eq!(titles(&list, &rank("탭", &list)), vec!["탭 닫기"]);
    assert_eq!(titles(&list, &rank("터널", &list)), vec!["터미널"]);
}

#[test]
fn fr_l31_the_palette_starts_with_the_editors_actions_and_not_itself() {
    let list = action_items(Keymap::VsCode, Os::Mac);
    let actions: Vec<Action> = list.iter().filter_map(|i| if let Payload::Action(a) = &i.payload { Some(*a) } else { None }).collect();
    assert!(!actions.contains(&Action::Palette), "the palette does not list itself");
    assert!(!actions.contains(&Action::Find), "find is the quick open, not a row: choosing it would close the sheet and open it again");
    assert!(actions.contains(&Action::Save) && actions.contains(&Action::GotoLine));
    let save = list.iter().find(|i| i.payload == Payload::Action(Action::Save)).unwrap();
    assert_eq!(save.title, "저장");
    assert_eq!(save.hint, Keymap::VsCode.hint(Action::Save, Os::Mac));
}

#[test]
fn fr_l31_commands_the_editor_cannot_do_yet_are_marked_and_listed_after_the_working_ones() {
    let list = action_items(Keymap::VsCode, Os::Mac);
    let enabled = |a: Action| list.iter().find(|i| i.payload == Payload::Action(a)).unwrap().enabled;
    assert!(enabled(Action::Terminal) && enabled(Action::Close) && enabled(Action::HideTool));
    for a in [Action::Save, Action::Recent, Action::RunConfig, Action::Stop, Action::GotoLine, Action::Format, Action::Ask, Action::NewTerminal, Action::MaximizeTool] {
        assert!(!enabled(a), "{a:?} is not implemented");
    }
    let ranked = rank("", &list);
    let first_disabled = ranked.iter().position(|r| !list[r.index].enabled).unwrap();
    assert!(ranked[..first_disabled].iter().all(|r| list[r.index].enabled));
    assert!(ranked[first_disabled..].iter().all(|r| !list[r.index].enabled), "the working commands come first");
    // A query ranks a working command above a not-ready one with the same score.
    let two = vec![
        Item { payload: Payload::File("a".into()), title: "Run".into(), hint: String::new(), enabled: false },
        Item { payload: Payload::File("b".into()), title: "Run".into(), hint: String::new(), enabled: true },
    ];
    assert_eq!(rank("run", &two)[0].index, 1);
}

#[test]
fn fr_l31_a_title_is_cut_into_matched_and_plain_runs() {
    assert_eq!(
        highlight_segments("Go To Line", &[0, 3, 6]),
        vec![("G".to_string(), true), ("o ".to_string(), false), ("T".to_string(), true), ("o ".to_string(), false), ("L".to_string(), true), ("ine".to_string(), false)]
    );
    assert_eq!(highlight_segments("저장", &[]), vec![("저장".to_string(), false)]);
    assert_eq!(highlight_segments("ab", &[0, 1]), vec![("ab".to_string(), true)]);
}

#[test]
fn fr_l31_the_arrow_keys_move_the_highlight_and_stop_at_the_ends() {
    assert_eq!(move_selection(0, 3, true), 1);
    assert_eq!(move_selection(2, 3, true), 2);
    assert_eq!(move_selection(0, 3, false), 0);
    assert_eq!(move_selection(2, 3, false), 1);
    assert_eq!(move_selection(0, 0, true), 0);
}

#[test]
fn fr_l31_find_opens_files_in_vs_code_and_everything_in_jetbrains_and_the_palette_opens_commands() {
    assert_eq!(mode_for(Action::Palette, Keymap::VsCode), Some(Mode::Commands));
    assert_eq!(mode_for(Action::Palette, Keymap::JetBrains), Some(Mode::Commands));
    assert_eq!(mode_for(Action::Find, Keymap::VsCode), Some(Mode::Files));
    assert_eq!(mode_for(Action::Find, Keymap::JetBrains), Some(Mode::Everything));
    assert_eq!(mode_for(Action::Save, Keymap::VsCode), None);
}

#[test]
fn fr_l31_the_index_skips_version_control_dependencies_and_build_output() {
    for name in [".git", "node_modules", "target", ".hg", "__pycache__"] {
        assert!(skipped_directory(name), "{name}");
    }
    for name in ["src", ".github", ".vscode", "docs", "build"] {
        assert!(!skipped_directory(name), "{name}");
    }
}

#[test]
fn fr_l31_files_are_listed_by_their_path_below_the_project_folder_sorted_by_name() {
    let paths = vec!["/w/p/src/b.rs".to_string(), "/w/p/README.md".to_string(), "/w/p/src/A.rs".to_string()];
    let items = file_items("/w/p/", &paths);
    let titles: Vec<&str> = items.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(titles, vec!["README.md", "src/A.rs", "src/b.rs"]);
    assert_eq!(items[1].payload, Payload::File("/w/p/src/A.rs".to_string()), "the payload is the full path");
}

#[test]
fn fr_l31_a_file_is_found_by_the_letters_of_its_path() {
    let paths = vec!["/w/p/src/main.rs".to_string(), "/w/p/src/lib.rs".to_string(), "/w/p/README.md".to_string()];
    let list = file_items("/w/p", &paths);
    let ranked = rank("smr", &list);
    assert_eq!(list[ranked[0].index].title, "src/main.rs");
    let list = file_items("/w/p", &paths);
    assert_eq!(list[rank("read", &list)[0].index].title, "README.md");
}

#[test]
fn fr_l31_everything_lists_the_actions_then_the_files() {
    let files = vec!["/w/p/a.rs".to_string()];
    let all = items_for(Mode::Everything, Keymap::JetBrains, Os::Mac, "/w/p", &files);
    let commands = items_for(Mode::Commands, Keymap::JetBrains, Os::Mac, "/w/p", &files);
    assert_eq!(all.len(), commands.len() + 1);
    assert_eq!(all.last().unwrap().payload, Payload::File("/w/p/a.rs".to_string()));
    assert!(items_for(Mode::Files, Keymap::VsCode, Os::Mac, "/w/p", &files).iter().all(|i| matches!(i.payload, Payload::File(_))));
}

#[test]
fn fr_l31_the_field_names_what_opened_the_palette_in_the_keymaps_words() {
    assert_eq!(placeholder(Mode::Commands, Keymap::VsCode), "명령 팔레트");
    assert_eq!(placeholder(Mode::Commands, Keymap::JetBrains), "동작 찾기");
    assert_eq!(placeholder(Mode::Files, Keymap::VsCode), "파일로 이동");
    assert_eq!(placeholder(Mode::Everything, Keymap::JetBrains), "모든 곳에서 찾기");
}
