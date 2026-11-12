//! FR-L31.12 (#440): the find bar's 정규식, 선택 영역에서 찾기 and 대소문자 보존 options, as Code-OSS's find
//! widget has them. Pure model, no renderer. Columns are UTF-16 code units.

use ember_app::ui::find::{
    find_matches, find_nav_match, preserve_case, regex_error, replacement_edits, FindOptions, Match, Scope,
};

fn m(line: u32, start: u32, end: u32) -> Match {
    Match { line, start, end }
}

fn regex() -> FindOptions {
    FindOptions { regex: true, ..FindOptions::default() }
}

/// Applies edits that are last first, as the editor does.
fn apply(text: &str, edits: &[(Match, String)]) -> String {
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    for (mm, t) in edits {
        let line = &lines[mm.line as usize];
        let units: Vec<u16> = line.encode_utf16().collect();
        let before = String::from_utf16(&units[..mm.start as usize]).unwrap();
        let after = String::from_utf16(&units[mm.end as usize..]).unwrap();
        lines[mm.line as usize] = format!("{before}{t}{after}");
    }
    lines.join("\n")
}

#[test]
fn fr_l31_12_regex_finds_pattern_matches_per_line() {
    let text = "let a1 = 10;\nlet b22 = a1 + 3;";
    assert_eq!(find_matches(text, r"[a-z]\d+", regex()), vec![m(0, 4, 6), m(1, 4, 7), m(1, 10, 12)]);
    assert_eq!(find_matches(text, r"\d+", FindOptions::default()), vec![]);
}

#[test]
fn fr_l31_12_regex_respects_case_and_whole_word() {
    let text = "Foo foo food";
    assert_eq!(find_matches(text, "fo+", regex()).len(), 3);
    let cased = FindOptions { case_sensitive: true, ..regex() };
    assert_eq!(find_matches(text, "fo+", cased), vec![m(0, 4, 7), m(0, 8, 11)]);
    let word = FindOptions { whole_word: true, ..regex() };
    assert_eq!(find_matches(text, "fo+", word), vec![m(0, 0, 3), m(0, 4, 7)]);
}

#[test]
fn fr_l31_12_regex_columns_count_utf16_units() {
    assert_eq!(find_matches("😀 값=1", r"\d", regex()), vec![m(0, 5, 6)]);
}

#[test]
fn fr_l31_12_an_invalid_regex_matches_nothing_and_says_why() {
    assert_eq!(find_matches("a(b", "a(", regex()), vec![]);
    assert!(regex_error("a(", regex()).is_some());
    assert!(regex_error("a(", FindOptions::default()).is_none());
    assert!(regex_error("a+", regex()).is_none());
}

#[test]
fn fr_l31_12_regex_navigation_scans_past_the_kept_matches() {
    let text = "x1\ny\nx2";
    let found = find_nav_match(text, r"x\d", regex(), &[], true, (0, 2), true);
    assert_eq!(found, Some(m(2, 0, 2)));
}

#[test]
fn fr_l31_12_regex_replacement_expands_groups() {
    let text = "a=1, b=2";
    let opts = regex();
    let matches = find_matches(text, r"(\w)=(\d)", opts);
    let edits = replacement_edits(text, r"(\w)=(\d)", opts, &matches, None, "$2:$1$$");
    assert_eq!(apply(text, &edits), "1:a$, 2:b$");
    let one = replacement_edits(text, r"(\w)=(\d)", opts, &matches, Some(1), "[$&]");
    assert_eq!(apply(text, &one), "a=1, [b=2]");
}

#[test]
fn fr_l31_12_literal_replacement_is_not_expanded() {
    let text = "a=1";
    let opts = FindOptions::default();
    let matches = find_matches(text, "a", opts);
    assert_eq!(replacement_edits(text, "a", opts, &matches, None, "$1"), vec![(m(0, 0, 1), "$1".to_string())]);
}

#[test]
fn fr_l31_12_preserve_case_follows_code_oss() {
    assert_eq!(preserve_case("FOO", "bar"), "BAR");
    assert_eq!(preserve_case("foo", "BAR"), "bar");
    assert_eq!(preserve_case("Foo", "bar"), "Bar");
    assert_eq!(preserve_case("fOO", "Bar"), "bar");
    assert_eq!(preserve_case("foo-Bar", "baz-qux"), "baz-Qux");
    assert_eq!(preserve_case("FOO_bar", "baz_QUX"), "BAZ_qux");
    assert_eq!(preserve_case("", "bar"), "bar");
}

#[test]
fn fr_l31_12_preserve_case_applies_to_each_replacement() {
    let text = "Apple apple APPLE";
    let opts = FindOptions { preserve_case: true, ..FindOptions::default() };
    let matches = find_matches(text, "apple", opts);
    let edits = replacement_edits(text, "apple", opts, &matches, None, "pear");
    assert_eq!(apply(text, &edits), "Pear pear PEAR");
}

#[test]
fn fr_l31_12_scope_from_selection_orders_ends_and_trims_a_trailing_line_start() {
    let text = "one\ntwo\nthree";
    assert_eq!(Scope::from_selection((1, 2), (0, 1), text), Some(Scope { start: (0, 1), end: (1, 2) }));
    assert_eq!(Scope::from_selection((0, 0), (2, 0), text), Some(Scope { start: (0, 0), end: (1, 3) }));
    assert_eq!(Scope::from_selection((1, 1), (1, 1), text), None);
}

#[test]
fn fr_l31_12_find_in_selection_keeps_only_matches_inside_the_scope() {
    let text = "a a\na a\na a";
    let scope = Scope { start: (0, 2), end: (1, 1) };
    let opts = FindOptions { scope: Some(scope), ..FindOptions::default() };
    assert_eq!(find_matches(text, "a", opts), vec![m(0, 2, 3), m(1, 0, 1)]);
    let edits = replacement_edits(text, "a", opts, &find_matches(text, "a", opts), None, "b");
    assert_eq!(apply(text, &edits), "a b\nb a\na a");
}
