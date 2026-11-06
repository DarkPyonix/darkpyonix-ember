//! Find and replace in the open file (D38, #440): the pure model. Columns are UTF-16 code units, as
//! the widget counts them. No renderer.

use ember_app::ui::find::{count_text, find_matches, next_match, FindOptions, Match};

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
