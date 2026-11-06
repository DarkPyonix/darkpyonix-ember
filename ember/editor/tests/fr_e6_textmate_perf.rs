//! FR-E6 acceptance (4): what tokenizing costs on a large file. Ignored by default (it prints numbers and
//! is only meaningful in a release build):
//!
//! ```sh
//! cargo test --release -p ember-editor --test fr_e6_textmate_perf -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use ember_editor::coords::{WidgetPos, WidgetRange};
use ember_editor::textmate::{parse_grammar, DocumentTokens, Grammar, GrammarSet};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/textmate")
}

fn grammar(scope: &str) -> Arc<Grammar> {
    let mut set = GrammarSet::new();
    for entry in std::fs::read_dir(fixtures().join("grammars")).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json") {
            set.add(parse_grammar(&path.to_string_lossy(), &std::fs::read_to_string(&path).unwrap()).unwrap());
        }
    }
    Arc::new(Grammar::new(&set, scope).unwrap())
}

fn range(sl: u32, sc: u32, el: u32, ec: u32) -> WidgetRange {
    WidgetRange::new(WidgetPos::new(sl, sc), WidgetPos::new(el, ec))
}

fn micros(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1e6
}

#[test]
#[ignore = "prints timings; run with --release --ignored --nocapture"]
fn fr_e6_tokenizing_cost_on_a_ten_thousand_line_file() {
    for (name, scope, ext) in [("TypeScript", "source.ts", "ts"), ("Rust", "source.rust", "rs"), ("Python", "source.python", "py"), ("Markdown", "text.html.markdown", "md")] {
        let sample = std::fs::read_to_string(fixtures().join("samples").join(format!("sample.{ext}"))).unwrap();
        let lines = sample.lines().count();
        let repeat = 10_000 / lines + 1;
        let text = sample.repeat(repeat);
        let total_lines = text.lines().count();
        let g = grammar(scope);

        let started = Instant::now();
        let mut doc = DocumentTokens::new(g.clone(), &text);
        let full_ms = micros(started) / 1000.0;

        // Typing a character in the middle of a line.
        let mid = (total_lines / 2) as u32;
        let mut typing = Vec::new();
        for i in 0..200u32 {
            let started = Instant::now();
            doc.apply_edit(range(mid, 0, mid, 0), "x").unwrap();
            typing.push(micros(started));
            doc.apply_edit(range(mid, 0, mid, 1), "").unwrap();
            let _ = i;
        }
        typing.sort_by(|a, b| a.partial_cmp(b).unwrap());

        // The worst edit: opening a block comment on the first line when nothing closes it before the end.
        let started = Instant::now();
        let changed = doc.apply_edit(range(0, 0, 0, 0), if ext == "py" { "\"\"\"" } else if ext == "md" { "<!--" } else { "/*" }).unwrap();
        let open_ms = micros(started) / 1000.0;

        println!(
            "[perf] {name}: {total_lines} lines, first tokenize {full_ms:.1} ms; typing a character: median {:.0} us, p95 {:.0} us, max {:.0} us; opening a comment at the top: {open_ms:.1} ms ({} lines re-tokenized)",
            typing[typing.len() / 2],
            typing[typing.len() * 95 / 100],
            typing[typing.len() - 1],
            changed.tokenized
        );
    }
}
