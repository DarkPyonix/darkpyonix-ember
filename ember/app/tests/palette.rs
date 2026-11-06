//! The command palette's model (D38, #447): fuzzy ranking of commands and files, and the list of
//! the editor's own actions it starts with. No renderer.

use ember_app::ui::keymap::{Action, Keymap, Os};
use ember_app::ui::palette::{action_items, highlight_segments, move_selection, rank, Item, Payload};

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
