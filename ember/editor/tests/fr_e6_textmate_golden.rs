//! FR-E6 acceptance (2): the interpreter against the real VS Code tokenizer. The files in
//! `tests/fixtures/textmate/golden/` hold what vscode-textmate produces for each sample with the vendored
//! grammars (regenerate with `generate-golden.cjs`); this test tokenizes the same samples with the
//! interpreter and requires the same tokens: the same ranges (UTF-16 columns) and the same scopes.
//! Neighbouring tokens with equal scopes are merged on both sides, because where an engine splits a run
//! is not observable.

use std::path::{Path, PathBuf};

use ember_editor::textmate::{parse_grammar, Grammar, GrammarSet, StateStack};
use serde_json::Value;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/textmate")
}

fn grammar_set() -> GrammarSet {
    let mut set = GrammarSet::new();
    let dir = fixtures().join("grammars");
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json" || e == "tmLanguage") {
            let text = std::fs::read_to_string(&path).unwrap();
            set.add(parse_grammar(&path.to_string_lossy(), &text).unwrap_or_else(|e| panic!("{}: {e}", path.display())));
        }
    }
    // What the extensions' `injectTo` says: the JSDoc grammars are injected into TypeScript and JavaScript.
    set.add_injector("source.ts", "documentation.injection.ts");
    set.add_injector("source.js", "documentation.injection.js.jsx");
    set.add_injector("source.host", "source.guest");
    set
}

type Tok = (usize, usize, Vec<String>);

/// The interpreter's tokens for a sample, as UTF-16 ranges, merged.
fn interpret(grammar: &Grammar, text: &str) -> Vec<Vec<Tok>> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let mut state: Option<StateStack> = None;
    let mut out = Vec::new();
    for line in lines {
        let result = grammar.tokenize_line(line, state.as_ref());
        state = Some(result.end_state);
        let utf16 = |byte: usize| line[..byte].encode_utf16().count();
        let mut merged: Vec<Tok> = Vec::new();
        for t in result.tokens {
            let (start, end) = (utf16(t.start), utf16(t.end));
            let scopes: Vec<String> = t.scopes.iter().map(|s| s.to_string()).collect();
            match merged.last_mut() {
                Some(last) if last.1 == start && last.2 == scopes => last.1 = end,
                _ => merged.push((start, end, scopes)),
            }
        }
        out.push(merged);
    }
    out
}

fn golden_lines(name: &str) -> (String, Vec<Vec<Tok>>) {
    let text = std::fs::read_to_string(fixtures().join("golden").join(format!("{name}.json"))).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    let lines = v["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| {
            line.as_array()
                .unwrap()
                .iter()
                .map(|t| {
                    (
                        t[0].as_u64().unwrap() as usize,
                        t[1].as_u64().unwrap() as usize,
                        t[2].as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_owned()).collect(),
                    )
                })
                .collect()
        })
        .collect();
    (v["grammar"].as_str().unwrap().to_owned(), lines)
}

fn check(ext: &str) {
    let set = grammar_set();
    let (scope, golden) = golden_lines(ext);
    let grammar = Grammar::new(&set, &scope).unwrap_or_else(|| panic!("no grammar {scope}"));
    assert!(grammar.problems().is_empty(), "{scope} has patterns the interpreter could not compile: {:?}", grammar.problems());
    let text = std::fs::read_to_string(fixtures().join("samples").join(format!("sample.{ext}"))).unwrap();
    let got = interpret(&grammar, &text);
    assert_eq!(got.len(), golden.len(), "line count");
    let mut bad = Vec::new();
    for (i, (g, e)) in got.iter().zip(&golden).enumerate() {
        if g != e {
            bad.push(i);
        }
    }
    if !bad.is_empty() {
        let lines: Vec<&str> = text.split('\n').collect();
        let mut report = String::new();
        for &i in bad.iter().take(4) {
            report.push_str(&format!("\nline {} {:?}\n  vscode-textmate:\n", i + 1, lines[i]));
            for t in &golden[i] {
                report.push_str(&format!("    {}..{} {}\n", t.0, t.1, t.2.join(" ")));
            }
            report.push_str("  interpreter:\n");
            for t in &got[i] {
                report.push_str(&format!("    {}..{} {}\n", t.0, t.1, t.2.join(" ")));
            }
        }
        panic!("{} of {} lines differ from vscode-textmate (first {} shown):{report}", bad.len(), golden.len(), bad.len().min(4));
    }
}

#[test]
fn fr_e6_golden_typescript_matches_vscode_textmate() {
    check("ts");
}

#[test]
fn fr_e6_golden_javascript_matches_vscode_textmate() {
    check("js");
}

#[test]
fn fr_e6_golden_rust_matches_vscode_textmate() {
    check("rs");
}

#[test]
fn fr_e6_golden_python_matches_vscode_textmate() {
    check("py");
}

#[test]
fn fr_e6_golden_json_matches_vscode_textmate() {
    check("json");
}

#[test]
fn fr_e6_golden_markdown_matches_vscode_textmate() {
    check("md");
}

#[test]
fn fr_e6_golden_injections_match_vscode_textmate() {
    // A small host grammar with injections of every kind (L: and R: priority, a path, `-`, `,`, `|`, grouping)
    // and a grammar injected into it through `injectTo`/`injectionSelector`.
    check("host");
}
