//! Find and replace in the open file (D38, #440, #628): the pure model. Columns are UTF-16 code units, as
//! the widget counts them. No renderer.

use ember_app::ui::find::{
    cached_doc_matches, cached_matches, count_text, find_matches, find_matches_capped,
    find_nav_match, next_match, replace_edits, FindBarState, FindOptions, Match,
    MAX_FIND_MATCHES, MAX_MATCHES,
};

fn opts() -> FindOptions {
    FindOptions {
        case_sensitive: false,
        whole_word: false,
        regex: false,
    }
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
    assert_eq!(
        find_matches(text, "foo", FindOptions { case_sensitive: true, whole_word: false, regex: false }),
        vec![m(0, 4, 7)]
    );
    assert_eq!(
        find_matches(text, "FOO", FindOptions { case_sensitive: true, whole_word: false, regex: false }),
        vec![m(0, 8, 11)]
    );
}

#[test]
fn fr_l31_whole_word_skips_matches_inside_longer_words() {
    let text = "cat concat cat_x cat.";
    let whole = FindOptions { case_sensitive: false, whole_word: true, regex: false };
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
    assert_eq!(MAX_FIND_MATCHES, MAX_MATCHES);
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
    assert_eq!(
        cached_matches("A a", "a", FindOptions { case_sensitive: true, whole_word: false, regex: false }).0.len(),
        1,
        "changed options too"
    );
}

#[test]
fn fr_l31_cached_doc_matches_skips_text_read_and_recompute_when_version_unchanged() {
    use std::cell::Cell;
    let text = "foo bar foo baz foo";
    let reads = Cell::new(0usize);
    let get_text = || {
        reads.set(reads.get() + 1);
        text
    };

    let o = opts();
    // 1. Initial query: computes and caches.
    let (m1, _) = cached_doc_matches("file:///a.rs", 1, 0, "foo", o, get_text);
    assert_eq!(m1.len(), 3);
    assert_eq!(reads.get(), 1);

    // 2. Second request with same doc, generation, version, query, options:
    //    get_text closure is NOT called, no clone, no hash.
    let (m2, _) = cached_doc_matches("file:///a.rs", 1, 0, "foo", o, get_text);
    assert_eq!(m2, m1);
    assert_eq!(reads.get(), 1, "closure was not evaluated on cache hit");

    // 3. Document version bumped (e.g. user typed edit): re-evaluates.
    let (m3, _) = cached_doc_matches("file:///a.rs", 1, 1, "foo", o, get_text);
    assert_eq!(m3.len(), 3);
    assert_eq!(reads.get(), 2, "version change re-evaluates");

    // 4. Query changed: re-evaluates.
    let (m4, _) = cached_doc_matches("file:///a.rs", 1, 1, "bar", o, get_text);
    assert_eq!(m4.len(), 1);
    assert_eq!(reads.get(), 3, "query change re-evaluates");

    // 5. Options changed: re-evaluates.
    let o_case = FindOptions { case_sensitive: true, whole_word: false, regex: false };
    let (m5, _) = cached_doc_matches("file:///a.rs", 1, 1, "bar", o_case, get_text);
    assert_eq!(m5.len(), 1);
    assert_eq!(reads.get(), 4, "options change re-evaluates");

    // 6. Same again: cache hit.
    let (m6, _) = cached_doc_matches("file:///a.rs", 1, 1, "bar", o_case, get_text);
    assert_eq!(m6, m5);
    assert_eq!(reads.get(), 4, "cache hits again");
}

#[test]
fn fr_l31_navigation_past_capped_matches_visits_all_occurrences() {
    // 10,005 lines each containing "target".
    let text = "target\n".repeat(10_005);
    let o = opts();
    let (matches, truncated) = find_matches_capped(&text, "target", o);
    assert_eq!(matches.len(), MAX_MATCHES);
    assert!(truncated);

    // Within the capped matches list (index 0..9999):
    let nav1 = find_nav_match(&text, "target", o, &matches, truncated, (0, 0), true);
    assert_eq!(nav1, Some(m(0, 0, 6)));

    let nav2 = find_nav_match(&text, "target", o, &matches, truncated, (9998, 6), true);
    assert_eq!(nav2, Some(m(9999, 0, 6)));

    // Navigating forward past the 10,000 match cap (from the end of match 9999):
    let nav_past1 = find_nav_match(&text, "target", o, &matches, truncated, (9999, 6), true);
    assert_eq!(nav_past1, Some(m(10000, 0, 6)), "visits match 10,000 past the cap");

    let nav_past2 = find_nav_match(&text, "target", o, &matches, truncated, (10000, 6), true);
    assert_eq!(nav_past2, Some(m(10001, 0, 6)), "visits match 10,001");

    let nav_last = find_nav_match(&text, "target", o, &matches, truncated, (10003, 6), true);
    assert_eq!(nav_last, Some(m(10004, 0, 6)), "visits the final match 10,004");

    // Navigating forward past the very last match wraps around to line 0:
    let nav_wrap_forward = find_nav_match(&text, "target", o, &matches, truncated, (10004, 6), true);
    assert_eq!(nav_wrap_forward, Some(m(0, 0, 6)), "wraps forward to first match");

    // Navigating backward before the first match wraps around to the final match:
    let nav_wrap_backward = find_nav_match(&text, "target", o, &matches, truncated, (0, 0), false);
    assert_eq!(nav_wrap_backward, Some(m(10004, 0, 6)), "wraps backward to final match past cap");

    // Navigating backward from match 10,001 visits match 10,000, then match 9,999:
    let nav_back1 = find_nav_match(&text, "target", o, &matches, truncated, (10001, 0), false);
    assert_eq!(nav_back1, Some(m(10000, 0, 6)));

    let nav_back2 = find_nav_match(&text, "target", o, &matches, truncated, (10000, 0), false);
    assert_eq!(nav_back2, Some(m(9999, 0, 6)));
}

#[test]
fn fr_l31_find_bar_state_refocus_increments_seq_when_already_open() {
    let mut state = FindBarState::new();
    assert!(!state.open);
    assert_eq!(state.focus_seq, 0);

    // Initial open sets open = true and increments focus_seq to 1.
    state.open_find_bar();
    assert!(state.open);
    assert_eq!(state.focus_seq, 1);

    // Cmd+F while already open increments focus_seq to request refocusing.
    state.open_find_bar();
    assert!(state.open);
    assert_eq!(state.focus_seq, 2);

    // Close and reopen.
    state.close();
    assert!(!state.open);
    state.open_find_bar();
    assert!(state.open);
    assert_eq!(state.focus_seq, 3);
}

#[test]
fn fr_l31_replace_all_with_hangul_and_emoji_text_round_trips_cleanly() {
    let text = "시작 😀 다람쥐 🦀 헌 쳇바퀴에 🚀 타고파 😀 끝\n줄2: 🦀 다람쥐 🚀\n";
    let matches = find_matches(text, "다람쥐", opts());
    assert_eq!(matches.len(), 2);

    let edits = replace_edits(&matches, None, "푸른청설모 🐿️");
    assert_eq!(edits.len(), 2);
    // Last first ordering.
    assert!(edits[0].0.line >= edits[1].0.line);

    // Apply edits via UTF-16 code unit splice to mirror widget behavior.
    let mut lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
    for (mt, rep) in edits {
        let l = mt.line as usize;
        let mut units: Vec<u16> = lines[l].encode_utf16().collect();
        units.splice(mt.start as usize..mt.end as usize, rep.encode_utf16());
        lines[l] = String::from_utf16(&units).expect("valid UTF-16 splice");
    }
    let replaced = lines.join("\n");
    assert!(replaced.contains("푸른청설모 🐿️"));
    assert!(!replaced.contains("다람쥐"));

    // Round-trip replacement back to original.
    let back_matches = find_matches(&replaced, "푸른청설모 🐿️", opts());
    assert_eq!(back_matches.len(), 2);
    let back_edits = replace_edits(&back_matches, None, "다람쥐");
    let mut back_lines: Vec<String> = replaced.split('\n').map(str::to_owned).collect();
    for (mt, rep) in back_edits {
        let l = mt.line as usize;
        let mut units: Vec<u16> = back_lines[l].encode_utf16().collect();
        units.splice(mt.start as usize..mt.end as usize, rep.encode_utf16());
        back_lines[l] = String::from_utf16(&units).expect("valid UTF-16 splice");
    }
    let restored = back_lines.join("\n");
    assert_eq!(restored, text, "round-trip restored original text identically");
}
