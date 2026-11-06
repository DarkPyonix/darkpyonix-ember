//! FR-E6: the interpreter's rules on small grammars, one behaviour at a time (the golden tests cover the
//! real grammars).

use ember_editor::textmate::{parse_grammar, Grammar, GrammarSet, StateStack, Token, MAX_LINE_BYTES, MAX_STACK_DEPTH};

fn grammar(json: &str) -> Grammar {
    let mut set = GrammarSet::new();
    set.add(parse_grammar("g.json", json).unwrap());
    let scope = serde_json::from_str::<serde_json::Value>(json).unwrap()["scopeName"].as_str().unwrap().to_owned();
    Grammar::new(&set, &scope).unwrap()
}

/// `text|scopes-after-the-root` for each token, neighbours with equal scopes merged (where an engine splits
/// a run is not observable), to compare against readable expectations.
fn show(line: &str, tokens: &[Token]) -> Vec<String> {
    let mut merged: Vec<(usize, usize, Vec<&str>)> = Vec::new();
    for t in tokens {
        let scopes: Vec<&str> = t.scopes.iter().skip(1).map(|s| &**s).collect();
        match merged.last_mut() {
            Some(last) if last.1 == t.start && last.2 == scopes => last.1 = t.end,
            _ => merged.push((t.start, t.end, scopes)),
        }
    }
    merged.iter().map(|(s, e, scopes)| format!("{}|{}", &line[*s..*e], scopes.join(" "))).collect()
}

fn run(g: &Grammar, text: &str) -> Vec<Vec<String>> {
    let mut state: Option<StateStack> = None;
    text.split('\n')
        .map(|line| {
            let r = g.tokenize_line(line, state.as_ref());
            state = Some(r.end_state);
            show(line, &r.tokens)
        })
        .collect()
}

#[test]
fn fr_e6_a_match_rule_scopes_what_it_matches_and_the_rest_keeps_the_root_scope() {
    let g = grammar(r#"{"scopeName":"source.t","patterns":[{"match":"\\b\\d+\\b","name":"constant.numeric"}]}"#);
    assert_eq!(run(&g, "a 12 b"), vec![vec!["a |", "12|constant.numeric", " b|"]]);
}

#[test]
fn fr_e6_the_earliest_match_wins_and_the_first_pattern_wins_a_tie() {
    let g = grammar(r#"{"scopeName":"source.t","patterns":[{"match":"ab","name":"first"},{"match":"abc","name":"second"},{"match":"b","name":"third"}]}"#);
    assert_eq!(run(&g, "abc"), vec![vec!["ab|first", "c|"]]);
}

#[test]
fn fr_e6_captures_scope_their_groups_and_a_capture_can_have_patterns_of_its_own() {
    let g = grammar(
        r##"{"scopeName":"source.t","patterns":[{"match":"(let) (\\w+) = (.*)","captures":{
            "1":{"name":"keyword"},"2":{"name":"variable"},"3":{"patterns":[{"match":"\\d+","name":"number"}]}}}]}"##,
    );
    assert_eq!(
        run(&g, "let x = 5 + 6"),
        vec![vec!["let|keyword", " |", "x|variable", " = |", "5|number", " + |", "6|number"]]
    );
}

#[test]
fn fr_e6_a_capture_name_can_use_the_captured_text() {
    let g = grammar(r#"{"scopeName":"source.t","patterns":[{"match":"@(\\w+)","name":"tag.$1.x","captures":{"1":{"name":"id.${1:/upcase}"}}}]}"#);
    assert_eq!(run(&g, "@Ab"), vec![vec!["@|tag.Ab.x", "Ab|tag.Ab.x id.AB"]]);
}

#[test]
fn fr_e6_a_region_runs_across_lines_and_the_state_says_it_is_open() {
    let g = grammar(r#"{"scopeName":"source.t","patterns":[{"begin":"/\\*","end":"\\*/","name":"comment.block"}]}"#);
    let mut state: Option<StateStack> = None;
    let mut depths = Vec::new();
    let mut shown = Vec::new();
    for line in ["a /* b", "c", "d */ e"] {
        let r = g.tokenize_line(line, state.as_ref());
        depths.push(r.end_state.depth());
        shown.push(show(line, &r.tokens));
        state = Some(r.end_state);
    }
    assert_eq!(shown[0], vec!["a |", "/* b|comment.block"]);
    assert_eq!(shown[1], vec!["c|comment.block"]);
    assert_eq!(shown[2], vec!["d */|comment.block", " e|"]);
    assert_eq!(depths, vec![2, 2, 1]);
}

#[test]
fn fr_e6_equal_states_mean_equal_futures_and_different_states_differ() {
    let g = grammar(r#"{"scopeName":"source.t","patterns":[{"begin":"\"","end":"\"","name":"string"}]}"#);
    let a = g.tokenize_line("x \"open", None).end_state;
    let b = g.tokenize_line("y \"other", None).end_state;
    let closed = g.tokenize_line("plain", None).end_state;
    assert_eq!(a, b, "the same rule open at the line end");
    assert_ne!(a, closed);
}

#[test]
fn fr_e6_begin_and_end_captures_and_content_name_scope_the_parts() {
    let g = grammar(
        r#"{"scopeName":"source.t","patterns":[{"begin":"\\(","end":"\\)","name":"meta.group","contentName":"meta.content",
            "beginCaptures":{"0":{"name":"punct.open"}},"endCaptures":{"0":{"name":"punct.close"}}}]}"#,
    );
    assert_eq!(run(&g, "(ab)"), vec![vec!["(|meta.group punct.open", "ab|meta.group meta.content", ")|meta.group punct.close"]]);
}

#[test]
fn fr_e6_the_end_pattern_can_use_the_text_the_begin_matched() {
    let g = grammar(r##"{"scopeName":"source.t","patterns":[{"begin":"r(#+)\"","end":"\"\\1","name":"string.raw"}]}"##);
    assert_eq!(run(&g, "r##\"a\"# b\"## c"), vec![vec!["r##\"a\"# b\"##|string.raw", " c|"]]);
    let g2 = grammar(r#"{"scopeName":"source.t","patterns":[{"begin":"<<(\\S+)","end":"^\\1$","name":"heredoc"}]}"#);
    let lines = run(&g2, "<<A.B\nx\nA.B\nrest");
    assert_eq!(lines[1], vec!["x|heredoc"]);
    assert_eq!(lines[2], vec!["A.B|heredoc"], "the dot of the captured text is escaped, not a wildcard");
    assert_eq!(lines[3], vec!["rest|"]);
}

#[test]
fn fr_e6_a_while_rule_continues_only_while_each_line_matches() {
    let g = grammar(r#"{"scopeName":"source.t","patterns":[{"begin":"^> ","while":"^> ","name":"quote","contentName":"quote.body"}]}"#);
    let lines = run(&g, "> a\n> b\nc");
    assert_eq!(lines[0], vec!["> |quote", "a|quote quote.body"]);
    assert_eq!(lines[1], vec!["> b|quote quote.body"], "a continuation line is inside the rule, so its `> ` carries the content scope too (as in vscode-textmate)");
    assert_eq!(lines[2], vec!["c|"], "the while pattern failed, the quote ended");
}

#[test]
fn fr_e6_include_resolves_repository_entries_self_and_other_grammars() {
    let mut set = GrammarSet::new();
    set.add(
        parse_grammar(
            "outer.json",
            r##"{"scopeName":"source.outer","patterns":[{"include":"#num"},{"begin":"\\[","end":"\\]","name":"list","patterns":[{"include":"$self"}]},
                {"include":"source.inner"},{"include":"source.inner#word"}],
               "repository":{"num":{"match":"\\d+","name":"number"}}}"##,
        )
        .unwrap(),
    );
    set.add(
        parse_grammar(
            "inner.json",
            r#"{"scopeName":"source.inner","patterns":[{"match":"!","name":"bang"}],"repository":{"word":{"match":"[a-z]+","name":"word"}}}"#,
        )
        .unwrap(),
    );
    let g = Grammar::new(&set, "source.outer").unwrap();
    assert_eq!(
        run(&g, "1 [2 [3]] ! ab"),
        vec![vec!["1|number", " |", "[|list", "2|list number", " |list", "[|list list", "3|list list number", "]|list list", "]|list", " |", "!|bang", " |", "ab|word"]]
    );
}

#[test]
fn fr_e6_apply_end_pattern_last_lets_a_nested_pattern_win_a_tie() {
    let first = grammar(r#"{"scopeName":"source.t","patterns":[{"begin":"<","end":">","name":"r","patterns":[{"match":">","name":"inner"}]}]}"#);
    assert_eq!(run(&first, "<>x>"), vec![vec!["<>|r", "x>|"]], "the end pattern is tried first, so it wins the tie");
    let last = grammar(r#"{"scopeName":"source.t","patterns":[{"begin":"<","end":">","name":"r","applyEndPatternLast":1,"patterns":[{"match":">","name":"inner"}]}]}"#);
    assert_eq!(run(&last, "<>x>"), vec![vec!["<|r", ">|r inner", "x|r", ">|r inner"]], "with the end pattern last the nested pattern wins every tie, so the region stays open");
}

#[test]
fn fr_e6_the_anchor_a_matches_only_at_the_start_of_the_document_and_g_where_the_search_began() {
    let g = grammar(r#"{"scopeName":"source.t","patterns":[{"match":"\\Aab","name":"start"},{"begin":"<","end":">","patterns":[{"match":"\\Gx","name":"first-x"},{"match":"x","name":"x"}]}]}"#);
    let lines = run(&g, "ab\nab\n<xx>");
    assert_eq!(lines[0], vec!["ab|start"]);
    assert_eq!(lines[1], vec!["ab|"], "\\A does not match on the second line");
    assert_eq!(lines[2], vec!["<|", "x|first-x", "x|x", ">|"]);
}

#[test]
fn fr_e6_a_pattern_that_matches_nothing_cannot_loop() {
    let g = grammar(r#"{"scopeName":"source.t","patterns":[{"match":"x*","name":"empty"},{"begin":"","end":"","name":"b"}]}"#);
    let r = g.tokenize_line("abc", None);
    assert!(r.tokens.iter().map(|t| t.end).max().unwrap_or(0) <= 3);
    let g2 = grammar(r#"{"scopeName":"source.t","patterns":[{"begin":"(?=a)","end":"(?=b)","name":"r"}]}"#);
    let r2 = g2.tokenize_line("aab", None);
    assert!(r2.tokens.len() <= 3, "{:?}", r2.tokens);
}

#[test]
fn fr_e6_a_pattern_oniguruma_rejects_is_left_out_and_listed() {
    let mut set = GrammarSet::new();
    set.add(parse_grammar("g.json", r#"{"scopeName":"source.t","patterns":[{"match":"(unclosed","name":"bad"},{"match":"ok","name":"good"}]}"#).unwrap());
    let g = Grammar::new(&set, "source.t").unwrap();
    assert_eq!(g.problems().len(), 1);
    assert!(g.problems()[0].contains("invalid match pattern"));
    assert_eq!(run(&g, "ok"), vec![vec!["ok|good"]]);
}

#[test]
fn fr_e6_offsets_are_bytes_of_the_line_and_the_line_break_is_not_part_of_it() {
    let g = grammar(r#"{"scopeName":"source.t","patterns":[{"match":"나","name":"hangul"}]}"#);
    let r = g.tokenize_line("가나다", None);
    let spans: Vec<(usize, usize)> = r.tokens.iter().map(|t| (t.start, t.end)).collect();
    assert_eq!(spans, vec![(0, 3), (3, 6), (6, 9)]);
    assert!(g.tokenize_line("", None).tokens.is_empty());
}

// ---- limits on hostile or broken grammars and input ----------------------------------------------------

#[test]
fn fr_e6_a_local_repository_entry_that_includes_itself_compiles_and_runs() {
    let g = grammar(
        r##"{"scopeName":"source.t","patterns":[{"begin":"\\(","end":"\\)","name":"p","repository":{"loop":{"patterns":[{"include":"#loop"},{"match":"x","name":"x"}]}},"patterns":[{"include":"#loop"}]}]}"##,
    );
    assert_eq!(run(&g, "(xx)"), vec![vec!["(|p", "xx|p x", ")|p"]]);
}

#[test]
fn fr_e6_repository_entries_that_include_each_other_and_self_do_not_loop() {
    let g = grammar(
        r##"{"scopeName":"source.t","patterns":[{"include":"#a"}],
            "repository":{"a":{"patterns":[{"include":"#b"},{"match":"a","name":"A"}]},"b":{"patterns":[{"include":"#a"},{"include":"$self"},{"match":"b","name":"B"}]}}}"##,
    );
    assert_eq!(run(&g, "ab"), vec![vec!["a|A", "b|B"]]);
}

#[test]
fn fr_e6_rules_that_nest_without_end_stop_at_the_stack_limit() {
    let g = grammar(r#"{"scopeName":"source.t","patterns":[{"begin":"a","end":"z","name":"r","patterns":[{"include":"$self"}]}]}"#);
    let line = "a".repeat(MAX_STACK_DEPTH * 4);
    let r = g.tokenize_line(&line, None);
    assert!(r.end_state.depth() <= MAX_STACK_DEPTH + 2, "depth {}", r.end_state.depth());
    assert!(r.tokens.iter().all(|t| t.end <= line.len()));
}

#[test]
fn fr_e6_a_very_long_line_is_not_tokenized_and_leaves_the_state_alone() {
    let g = grammar(r#"{"scopeName":"source.t","patterns":[{"begin":"/\\*","end":"\\*/","name":"c"}]}"#);
    let open = g.tokenize_line("/* open", None).end_state;
    let line = "x".repeat(MAX_LINE_BYTES + 1);
    let r = g.tokenize_line(&line, Some(&open));
    assert!(r.skipped);
    assert_eq!(r.end_state, open, "the state is as it was");
    assert_eq!(r.tokens.len(), 1);
    assert_eq!((r.tokens[0].start, r.tokens[0].end), (0, line.len()));
    assert_eq!(r.tokens[0].scopes.last().map(|s| &**s), Some("c"), "the scopes in force");
    assert!(!g.tokenize_line(&"x".repeat(MAX_LINE_BYTES), None).skipped, "exactly the limit is tokenized");
}

#[test]
fn fr_e6_a_pattern_with_catastrophic_backtracking_gives_up_instead_of_hanging() {
    let g = grammar(r#"{"scopeName":"source.t","patterns":[{"match":"^(a+)+$","name":"bad"},{"match":"b","name":"b"}]}"#);
    let line = format!("{}b", "a".repeat(60));
    let started = std::time::Instant::now();
    let r = g.tokenize_line(&line, None);
    assert!(started.elapsed() < std::time::Duration::from_secs(20), "took {:?}", started.elapsed());
    assert!(r.tokens.iter().all(|t| t.end <= line.len()));
}
