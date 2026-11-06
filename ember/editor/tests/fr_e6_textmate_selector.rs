//! FR-E6: scope selectors (`injectionSelector`, the keys of `injections`), after vscode-textmate's matcher.

use std::sync::Arc;

use ember_editor::textmate::{Matcher, Scope, Selector, MAX_SELECTOR_CHARS, MAX_SELECTOR_DEPTH, MAX_SELECTOR_TOKENS};

fn scopes(list: &[&str]) -> Vec<Scope> {
    list.iter().map(|s| Arc::<str>::from(*s)).collect()
}

fn holds(selector: &str, stack: &[&str]) -> bool {
    let parsed = Selector::parse(selector).unwrap_or_else(|| panic!("{selector:?} should parse"));
    parsed.alternatives.iter().any(|(m, _)| m.matches(&scopes(stack)))
}

#[test]
fn fr_e6_a_name_matches_a_scope_equal_to_it_or_below_it_at_a_dot() {
    assert!(holds("comment.block", &["source.ts", "comment.block"]));
    assert!(holds("comment.block", &["source.ts", "comment.block.documentation"]));
    assert!(!holds("comment.block", &["source.ts", "comment.blockish"]));
    assert!(!holds("comment.block", &["source.ts", "comment"]));
}

#[test]
fn fr_e6_names_separated_by_spaces_are_a_path_in_stack_order() {
    assert!(holds("source.host string", &["source.host", "meta.x", "string.quoted"]));
    assert!(!holds("string source.host", &["source.host", "string.quoted"]), "string comes after source.host in the stack");
    assert!(!holds("a b c", &["a", "b"]), "a stack shorter than the path cannot match");
    assert!(holds("a a", &["a", "a.b"]), "each name needs its own scope");
    assert!(!holds("a a", &["a"]));
}

#[test]
fn fr_e6_a_minus_excludes_and_parts_side_by_side_must_all_hold() {
    assert!(holds("text.html - comment", &["text.html", "meta"]));
    assert!(!holds("text.html - comment", &["text.html", "comment.block"]));
    assert!(holds("a b -c", &["a", "b"]));
    assert!(!holds("a b -c", &["a", "b", "c"]));
}

#[test]
fn fr_e6_a_bar_or_a_comma_means_either_and_parentheses_group() {
    assert!(holds("(string | comment) -invalid", &["comment.block"]));
    assert!(!holds("(string | comment) -invalid", &["comment.block", "invalid.x"]));
    assert!(holds("string, comment", &["comment.line"]));
    assert!(holds("a (b | c) d", &["a", "c", "d"]));
    assert!(!holds("a (b | c) d", &["a", "d"]));
}

#[test]
fn fr_e6_l_and_r_set_the_priority_of_each_comma_separated_alternative() {
    let s = Selector::parse("L:a.b, R:c, d").unwrap();
    let priorities: Vec<i8> = s.alternatives.iter().map(|(_, p)| *p).collect();
    assert_eq!(priorities, vec![-1, 1, 0]);
    assert_eq!(s.alternatives[0].0, Matcher::And(vec![Matcher::Path(vec!["a.b".to_owned()])]));
}

#[test]
fn fr_e6_a_selector_over_a_limit_is_rejected_whole() {
    assert!(Selector::parse(&"a ".repeat(MAX_SELECTOR_CHARS)).is_none(), "too long");
    assert!(Selector::parse(&"a,".repeat(MAX_SELECTOR_TOKENS)).is_none(), "too many tokens");
    let deep = format!("{}a{}", "(".repeat(MAX_SELECTOR_DEPTH + 4), ")".repeat(MAX_SELECTOR_DEPTH + 4));
    assert!(Selector::parse(&deep).is_none(), "too deep");
    let ok = format!("{}a{}", "(".repeat(MAX_SELECTOR_DEPTH - 2), ")".repeat(MAX_SELECTOR_DEPTH - 2));
    assert!(Selector::parse(&ok).is_some(), "a sane nesting parses");
    let minuses = "-".repeat(MAX_SELECTOR_DEPTH + 4);
    assert!(Selector::parse(&format!("{minuses}a")).is_none(), "a long run of negations counts as depth");
}

#[test]
fn fr_e6_odd_input_parses_without_panicking_and_an_empty_selector_matches_everything() {
    for src in ["", "   ", ",", "|", "-", "(", ")", "((", "))", "L:", "R:R:L:", "a--b", "a | | b", "\u{0}\u{1b}x", "한글 scope", "a.b-c.d"] {
        let _ = Selector::parse(src);
    }
    assert!(holds("", &["anything"]), "no condition: holds");
    assert!(!holds("-", &["anything"]), "a negation with nothing to negate holds for nothing");
}

/// A small deterministic generator, so a failure repeats.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() as usize) % n.max(1)
    }
}

#[test]
fn fr_e6_random_selectors_parse_and_match_without_panicking_and_the_limits_hold() {
    let atoms = ["a", "b.c", "source.ts", "L:", "R:", "-", "(", ")", "|", ",", " ", "  ", "meta-x", ".", ":", "한", "\n", "\u{0}", "x.y.z-w", "((", "))", "--", "||", ",,"];
    let stack = scopes(&["source.ts", "a.b", "meta-x.q", "c"]);
    let mut rng = Rng(42);
    let (mut parsed, mut rejected) = (0, 0);
    for round in 0..5000 {
        let len = 1 + rng.below(if round % 50 == 0 { 700 } else { 24 });
        let src: String = (0..len).map(|_| atoms[rng.below(atoms.len())]).collect();
        match Selector::parse(&src) {
            Some(sel) => {
                parsed += 1;
                assert!(src.chars().count() <= MAX_SELECTOR_CHARS);
                for (m, _) in &sel.alternatives {
                    let _ = m.matches(&stack);
                    let _ = m.matches(&[]);
                }
            }
            None => rejected += 1,
        }
    }
    assert!(parsed > 1000, "most short inputs parse: {parsed}");
    assert!(rejected > 0, "some inputs hit a limit: {rejected}");
}

#[test]
fn fr_e6_long_mixed_input_is_rejected_quickly() {
    let started = std::time::Instant::now();
    for n in [2_000, 20_000, 200_000] {
        let src: String = "(a | -b, L:c ".repeat(n / 10);
        assert!(Selector::parse(&src).is_none(), "{n} characters");
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
}
