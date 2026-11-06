//! Find and replace in the open file (D38, #440): the pure model. Columns are UTF-16 code units, as
//! the widget counts them. No renderer.

use ember_app::ui::find::{count_text, find_matches, cached_matches, find_matches_capped, next_match, replace_edits, FindOptions, Match, MAX_MATCHES};

fn opts() -> FindOptions {
    FindOptions { case_sensitive: false, whole_word: false }
}

fn m(line: u32, start: u32, end: u32) -> Match {
    Match { line, start, end }
}

#[test]
fn fr_l31_find_lists_every_occurrence_with_its_line_and_columns() {
    let text = "let a = 1;\nlet b = a + a;\n";
    assert_eq!(find_matches(text, "a", opts()), vec![m(0, 4, 5), m(1, 8, 9), m(1, 12, 13)]);
    assert_eq!(find_matches(text, "let", opts()), vec![m(0, 0, 3), m(1, 0, 3)]);
}

#[test]
fn fr_l31_an_empty_query_or_a_query_with_a_line_break_matches_nothing() {
    assert!(find_matches("abc", "", opts()).is_empty());
    assert!(find_matches("a\nb", "a\nb", opts()).is_empty(), "a match does not span lines");
}

#[test]
fn fr_l31_find_ignores_case_unless_asked_not_to() {
    let text = "Foo foo FOO";
    assert_eq!(find_matches(text, "foo", opts()).len(), 3);
    assert_eq!(find_matches(text, "foo", FindOptions { case_sensitive: true, whole_word: false }), vec![m(0, 4, 7)]);
    assert_eq!(find_matches(text, "FOO", FindOptions { case_sensitive: true, whole_word: false }), vec![m(0, 8, 11)]);
}

#[test]
fn fr_l31_whole_word_skips_matches_inside_longer_words() {
    let text = "cat concat cat_x cat.";
    let whole = FindOptions { case_sensitive: false, whole_word: true };
    assert_eq!(find_matches(text, "cat", whole), vec![m(0, 0, 3), m(0, 17, 20)]);
    assert_eq!(find_matches(text, "cat", opts()).len(), 4);
}

#[test]
fn fr_l31_matches_do_not_overlap() {
    assert_eq!(find_matches("aaaa", "aa", opts()), vec![m(0, 0, 2), m(0, 2, 4)]);
}

#[test]
fn fr_l31_columns_count_utf16_code_units() {
    // The emoji is two UTF-16 units, the Hangul syllables and the spaces one each: 2 + 1 + 2 + 1 = 6.
    let text = "😀 가나 target";
    assert_eq!(find_matches(text, "target", opts()), vec![m(0, 6, 12)]);
    assert_eq!(find_matches(text, "나", opts()), vec![m(0, 4, 5)]);
}

#[test]
fn fr_l31_next_match_goes_forward_and_wraps() {
    let matches = vec![m(0, 4, 5), m(1, 8, 9), m(1, 12, 13)];
    assert_eq!(next_match(&matches, (0, 0), true), Some(0));
    assert_eq!(next_match(&matches, (0, 5), true), Some(1), "from the end of the current match");
    assert_eq!(next_match(&matches, (1, 13), true), Some(0), "past the last one it wraps");
    assert_eq!(next_match(&[], (0, 0), true), None);
}

#[test]
fn fr_l31_previous_match_goes_backward_and_wraps() {
    let matches = vec![m(0, 4, 5), m(1, 8, 9), m(1, 12, 13)];
    assert_eq!(next_match(&matches, (1, 12), false), Some(1));
    assert_eq!(next_match(&matches, (1, 8), false), Some(0));
    assert_eq!(next_match(&matches, (0, 4), false), Some(2), "before the first one it wraps");
}

#[test]
fn fr_l31_the_counter_says_where_the_current_match_is() {
    assert_eq!(count_text(Some(2), 12, "x"), "3/12");
    assert_eq!(count_text(None, 12, "x"), "12개");
    assert_eq!(count_text(None, 0, "x"), "결과 없음");
    assert_eq!(count_text(None, 0, ""), "", "nothing typed yet");
}

#[test]
fn fr_l31_find_stops_at_the_match_cap_and_says_so() {
    let text = "a".repeat(MAX_MATCHES + 10);
    let (matches, truncated) = find_matches_capped(&text, "a", opts());
    assert_eq!(matches.len(), MAX_MATCHES);
    assert!(truncated);
    let (matches, truncated) = find_matches_capped(&"a".repeat(MAX_MATCHES), "a", opts());
    assert_eq!(matches.len(), MAX_MATCHES);
    assert!(!truncated, "exactly the cap is not past it");
}

#[test]
fn fr_l31_replace_one_edits_only_the_chosen_match_and_none_when_out_of_range() {
    let matches = vec![m(0, 0, 3), m(1, 4, 7)];
    assert_eq!(replace_edits(&matches, Some(1), "x"), vec![(m(1, 4, 7), "x".to_string())]);
    assert!(replace_edits(&matches, Some(2), "x").is_empty());
    assert!(replace_edits(&[], Some(0), "x").is_empty());
}

#[test]
fn fr_l31_replace_all_is_last_first_so_no_edit_moves_another() {
    let matches = vec![m(0, 0, 1), m(0, 4, 5), m(2, 0, 1)];
    let edits = replace_edits(&matches, None, "한글😀");
    assert_eq!(edits.iter().map(|(x, _)| *x).collect::<Vec<_>>(), vec![m(2, 0, 1), m(0, 4, 5), m(0, 0, 1)]);
    assert!(edits.iter().all(|(_, t)| t == "한글😀"), "a multi-byte replacement is passed through whole");
}

#[test]
fn fr_l31_replace_edits_applied_last_first_give_the_replaced_text() {
    // Apply the edits to a line the way the widget would (UTF-16 columns, each against the same text).
    let text = "a-😀-a-a";
    let matches = find_matches(text, "a", opts());
    let mut units: Vec<u16> = text.encode_utf16().collect();
    for (mt, t) in replace_edits(&matches, None, "XY") {
        units.splice(mt.start as usize..mt.end as usize, t.encode_utf16());
    }
    assert_eq!(String::from_utf16(&units).unwrap(), "XY-😀-XY-XY");
}

#[test]
fn fr_l31_cached_matches_follow_the_text_query_and_options() {
    let o = opts();
    assert_eq!(cached_matches("a b a", "a", o).0.len(), 2);
    assert_eq!(cached_matches("a b a", "a", o).0.len(), 2, "same inputs, same answer");
    assert_eq!(cached_matches("a b a a", "a", o).0.len(), 3, "a changed text is searched again");
    assert_eq!(cached_matches("a b a a", "b", o).0.len(), 1, "a changed query is searched again");
    assert_eq!(cached_matches("A a", "a", FindOptions { case_sensitive: true, whole_word: false }).0.len(), 1, "changed options too");
}
