//! FR-E6: colour roles from scopes, and a document whose runs follow its edits. The property that matters
//! is that incremental updating gives exactly what tokenizing the edited text from scratch gives.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ember_editor::coords::{WidgetPos, WidgetRange};
use ember_editor::textmate::{parse_grammar, role_for_scopes, DocumentTokens, EditError, Grammar, GrammarSet, SyntaxRole};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/textmate")
}

fn grammar(scope: &str) -> Arc<Grammar> {
    let mut set = GrammarSet::new();
    for entry in std::fs::read_dir(fixtures().join("grammars")).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json") {
            let text = std::fs::read_to_string(&path).unwrap();
            set.add(parse_grammar(&path.to_string_lossy(), &text).unwrap());
        }
    }
    Arc::new(Grammar::new(&set, scope).unwrap())
}

fn sample(ext: &str) -> String {
    std::fs::read_to_string(fixtures().join("samples").join(format!("sample.{ext}"))).unwrap()
}

fn range(sl: u32, sc: u32, el: u32, ec: u32) -> WidgetRange {
    WidgetRange::new(WidgetPos::new(sl, sc), WidgetPos::new(el, ec))
}

// ---- roles ------------------------------------------------------------------------------------------

#[test]
fn fr_e6_the_innermost_scope_with_a_role_decides_the_colour() {
    assert_eq!(role_for_scopes(&["source.ts", "string.quoted.double.ts"]), Some(SyntaxRole::String));
    assert_eq!(role_for_scopes(&["source.ts", "string.quoted.double.ts", "constant.character.escape.ts"]), Some(SyntaxRole::Escape));
    assert_eq!(role_for_scopes(&["source.ts", "comment.line.double-slash.ts", "punctuation.definition.comment.ts"]), Some(SyntaxRole::Comment));
    assert_eq!(role_for_scopes(&["source.ts", "meta.function.ts", "entity.name.function.ts"]), Some(SyntaxRole::Function));
    assert_eq!(role_for_scopes(&["source.ts", "meta.var.expr.ts"]), None, "meta scopes carry no colour");
    assert_eq!(role_for_scopes::<&str>(&[]), None);
}

#[test]
fn fr_e6_prefixes_match_whole_scope_segments_only() {
    assert_eq!(role_for_scopes(&["keyword.operator.assignment.ts"]), Some(SyntaxRole::Operator));
    assert_eq!(role_for_scopes(&["keyword.control.flow.ts"]), Some(SyntaxRole::Keyword));
    assert_eq!(role_for_scopes(&["keywordish.thing"]), None, "keywordish is not keyword");
    assert_eq!(role_for_scopes(&["constant.numeric.decimal.ts"]), Some(SyntaxRole::Number));
    assert_eq!(role_for_scopes(&["support.type.property-name.json"]), Some(SyntaxRole::Property));
    assert_eq!(role_for_scopes(&["entity.name.tag.html"]), Some(SyntaxRole::Tag));
}

// ---- a document -------------------------------------------------------------------------------------

#[test]
fn fr_e6_a_document_has_runs_in_utf16_columns_with_the_roles_of_its_tokens() {
    let doc = DocumentTokens::new(grammar("source.ts"), "const a = \"한😀\"; // x");
    let runs = doc.line_runs(0);
    let find = |role| runs.iter().find(|r| r.role == role).copied();
    let string = find(SyntaxRole::String).unwrap();
    // `"한😀"` is 1 + 1 + 2 + 1 = 5 UTF-16 units starting at column 10.
    assert_eq!((string.start, string.end), (10, 15));
    assert!(find(SyntaxRole::Keyword).is_some() && find(SyntaxRole::Comment).is_some());
    assert!(runs.windows(2).all(|w| w[0].end <= w[1].start), "runs do not overlap");
}

#[test]
fn fr_e6_a_bad_edit_range_is_an_error_and_changes_nothing() {
    let mut doc = DocumentTokens::new(grammar("source.ts"), "a\n😀b");
    assert_eq!(doc.apply_edit(range(5, 0, 5, 0), "x"), Err(EditError::OutOfRange));
    assert_eq!(doc.apply_edit(range(1, 1, 1, 1), "x"), Err(EditError::OutOfRange), "inside a surrogate pair");
    assert_eq!(doc.text(), "a\n😀b");
}

#[test]
fn fr_e6_typing_inside_a_line_tokenizes_that_line_only() {
    let mut doc = DocumentTokens::new(grammar("source.ts"), &sample("ts"));
    let line = doc.text().lines().position(|l| l.starts_with("const pattern")).unwrap() as u32;
    let changed = doc.apply_edit(range(line, 6, line, 6), "x").unwrap();
    assert_eq!(changed.tokenized, 1);
    assert_eq!((changed.first_line, changed.last_line), (line, line));
}

#[test]
fn fr_e6_opening_a_block_comment_changes_the_lines_up_to_where_it_closes_and_closing_it_restores_them() {
    let text = "let a = 1;\nlet b = 2;\nlet c = 3;\n/* note */\nlet d = 4;";
    let g = grammar("source.ts");
    let before = DocumentTokens::new(g.clone(), text).runs();
    let mut doc = DocumentTokens::new(g.clone(), text);
    let opened = doc.apply_edit(range(1, 0, 1, 0), "/*").unwrap();
    assert!(opened.last_line >= 3, "the comment now runs to the `*/` on line 3: {opened:?}");
    assert_eq!(doc.runs(), DocumentTokens::new(g.clone(), &doc.text()).runs());
    doc.apply_edit(range(1, 0, 1, 2), "").unwrap();
    assert_eq!(doc.runs(), before, "removing the opening restores every run");
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

fn random_edit(doc: &DocumentTokens, rng: &mut Rng, pool: &[&str]) -> (WidgetRange, String) {
    let text = doc.text();
    let lines: Vec<&str> = text.split('\n').collect();
    let pick = |rng: &mut Rng| -> WidgetPos {
        let line = rng.below(lines.len());
        let chars: Vec<char> = lines[line].chars().collect();
        let n = rng.below(chars.len() + 1);
        let col: u32 = chars[..n].iter().map(|c| c.len_utf16() as u32).sum();
        WidgetPos::new(line as u32, col)
    };
    let (a, b) = (pick(rng), pick(rng));
    let (start, end) = if (a.line, a.col) <= (b.line, b.col) { (a, b) } else { (b, a) };
    // Mostly small edits: keep a deletion short so the document survives many rounds.
    let end = if end.line > start.line + 2 { WidgetPos::new(start.line + 2, 0) } else { end };
    let end = if end.line == start.line && end.col > start.col + 12 { WidgetPos::new(start.line, start.col + 12) } else { end };
    // A document that has shrunk is grown again, so the run keeps editing a document of realistic size.
    if lines.len() < 40 {
        let at = WidgetRange::empty(start);
        return (at, format!("{}\n", pool[rng.below(pool.len())]));
    }
    let insert = match rng.below(3) {
        0 => String::new(),
        _ => pool[rng.below(pool.len())].to_owned(),
    };
    (WidgetRange::new(start, end), insert)
}

fn incremental_equals_fresh(scope: &str, ext: &str, rounds: usize, seed: u64, pool: &[&str]) {
    let g = grammar(scope);
    let mut doc = DocumentTokens::new(g.clone(), &sample(ext));
    let mut rng = Rng(seed);
    let mut tokenized = 0u64;
    let mut all = 0u64;
    for round in 0..rounds {
        let (r, insert) = random_edit(&doc, &mut rng, pool);
        match doc.apply_edit(r, &insert) {
            Ok(changed) => {
                tokenized += changed.tokenized as u64;
                all += doc.line_count() as u64;
            }
            Err(e) => panic!("round {round}: {e:?} for {r:?} {insert:?}"),
        }
        let fresh = DocumentTokens::new(g.clone(), &doc.text());
        assert_eq!(doc.line_count(), fresh.line_count(), "round {round} {r:?} {insert:?}");
        for line in 0..doc.line_count() {
            assert_eq!(doc.line_runs(line), fresh.line_runs(line), "round {round}, line {line} after {r:?} {insert:?}");
        }
    }
    // The point of doing it incrementally: far fewer lines than tokenizing all of them each time.
    assert!(tokenized * 3 < all, "tokenized {tokenized} lines over {rounds} edits, a full pass each time is {all}");
}

#[test]
fn fr_e6_incremental_runs_equal_a_fresh_pass_after_random_typescript_edits() {
    incremental_equals_fresh("source.ts", "ts", 300, 7, &["/*", "*/", "`", "\"", "'", "{", "}", "//", "\n", "const x = 1;", "😀", "${", "(", ")", "x", " "]);
}

#[test]
fn fr_e6_incremental_runs_equal_a_fresh_pass_after_random_markdown_edits() {
    incremental_equals_fresh("text.html.markdown", "md", 200, 11, &["```", "> ", "\n", "# ", "*", "`", "- ", "x", "---", "\n\n", "<!--", "-->"]);
}

#[test]
fn fr_e6_incremental_runs_equal_a_fresh_pass_after_random_python_edits() {
    incremental_equals_fresh("source.python", "py", 200, 13, &["\"\"\"", "'", "\"", "#", "\n", "def f():", "    ", "\\", "(", ")", "x", "f\""]);
}

#[test]
fn fr_e6_incremental_runs_equal_a_fresh_pass_after_random_rust_edits() {
    incremental_equals_fresh("source.rust", "rs", 200, 17, &["/*", "*/", "\"", "r#\"", "\"#", "'a", "{", "}", "//", "\n", "fn x() {}", "😀"]);
}
