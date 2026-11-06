//! FR-E6: syntax highlighting inside the session. The core asks for the grammar files a document needs,
//! hands the tokenizing to a worker, and reports the coloured runs; these tests play the driver: they
//! read the fixture grammars for it and run the worker themselves.

use std::path::{Path, PathBuf};
use std::time::Instant;

use ember_editor::engine::ReadPurpose;
use ember_editor::highlight::{HighlightJob, HighlightOutput, Problem, Worker};
use ember_editor::languages::LanguageRegistry;
use ember_editor::textmate::{DocRun, GrammarCatalog, SyntaxRole};
use ember_editor::{CoreOptions, Effect, Input, SessionCore, SessionUpdate, WidgetEvent, WidgetPos, WidgetRange};
use ember_editor_conn::uri::UriComponents;
use serde_json::{json, Value};

const AUTH: &str = "127.0.0.1:9";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/textmate")
}

/// What `scanExtensions` says for two extensions that live in the fixture folder.
fn descriptions() -> Vec<Value> {
    let dir = fixtures().join("grammars").to_string_lossy().to_string();
    vec![
        json!({
            "identifier": {"value": "vscode.typescript-basics"},
            "extensionLocation": {"path": dir},
            "contributes": {
                "languages": [{"id": "typescript", "extensions": [".ts"]}, {"id": "python", "extensions": [".py"]}],
                "grammars": [
                    {"language": "typescript", "scopeName": "source.ts", "path": "./TypeScript.tmLanguage.json"},
                    {"language": "python", "scopeName": "source.python", "path": "./MagicPython.tmLanguage.json"},
                    {"scopeName": "source.regexp.python", "path": "./MagicRegExp.tmLanguage.json"},
                    {"scopeName": "documentation.injection.ts", "path": "./jsdoc.ts.injection.tmLanguage.json", "injectTo": ["source.ts", "source.tsx"]},
                ]
            }
        }),
    ]
}

fn core(t0: Instant) -> SessionCore {
    let d = descriptions();
    let mut core = SessionCore::new(LanguageRegistry::from_extensions(&d), CoreOptions::default(), t0);
    core.set_grammar_catalog(GrammarCatalog::from_extensions(&d));
    core
}

fn uri(path: &str) -> UriComponents {
    UriComponents::remote(AUTH, path)
}

fn p(line: u32, col: u32) -> WidgetPos {
    WidgetPos::new(line, col)
}

/// A `Syntax` update as the test keeps it: generation, version, window, runs.
type SyntaxUpdate = (u64, u32, (u32, u32), Vec<DocRun>);

/// Plays the driver for the effects that matter here: reads grammar files from disk, collects jobs and updates.
struct Driver {
    core: SessionCore,
    worker: Worker,
    t: Instant,
    syntax: Vec<SyntaxUpdate>,
    jobs: Vec<HighlightJob>,
    reads: Vec<String>,
    open_generation: u64,
}

impl Driver {
    fn new() -> Self {
        let t = Instant::now();
        Self { core: core(t), worker: Worker::new(), t, syntax: Vec::new(), jobs: Vec::new(), reads: Vec::new(), open_generation: 0 }
    }

    /// Feeds an input and everything it leads to (grammar reads answered from disk, jobs run on the worker).
    fn feed(&mut self, input: Input) {
        let mut queue = vec![input];
        let mut pending_jobs: Vec<HighlightJob> = Vec::new();
        while let Some(input) = queue.pop() {
            for effect in self.core.handle(self.t, input) {
                match effect {
                    Effect::ReadFile { uri, purpose: ReadPurpose::Grammar { scope } } => {
                        self.reads.push(scope.clone());
                        let result = std::fs::read(&uri.path).map_err(|e| e.to_string());
                        queue.insert(0, Input::FileRead { uri, purpose: ReadPurpose::Grammar { scope }, result });
                    }
                    Effect::Highlight(job) => {
                        self.jobs.push(job.clone());
                        pending_jobs.push(job);
                    }
                    Effect::Ui(SessionUpdate::Syntax { generation, version, window, runs, .. }) => self.syntax.push((generation, version, window, runs)),
                    Effect::Ui(SessionUpdate::Opened { generation, .. }) => self.open_generation = generation,
                    _ => {}
                }
            }
            if queue.is_empty() && !pending_jobs.is_empty() {
                for output in self.worker.run(std::mem::take(&mut pending_jobs)) {
                    queue.push(Input::Highlighted(output));
                }
            }
        }
    }

    fn open(&mut self, path: &str, text: &str) {
        self.feed(Input::FileRead { uri: uri(path), purpose: ReadPurpose::Open, result: Ok(text.as_bytes().to_vec()) });
    }

    fn last_runs(&self) -> &[DocRun] {
        &self.syntax.last().expect("a Syntax update").3
    }
}

fn role_at(runs: &[DocRun], line: u32, col: u32) -> Option<SyntaxRole> {
    runs.iter().find(|r| r.line == line && r.start <= col && col < r.end).map(|r| r.role)
}

#[test]
fn fr_e6_opening_a_file_reads_its_grammar_and_what_it_includes_then_colours_it() {
    let mut d = Driver::new();
    d.open("/w/a.ts", "const x = \"s\"; // c\n");
    assert!(d.reads.contains(&"source.ts".to_owned()), "reads: {:?}", d.reads);
    assert_eq!(d.reads.iter().filter(|s| *s == "source.ts").count(), 1, "each grammar is read once");
    let runs = d.last_runs();
    assert_eq!(role_at(runs, 0, 0), Some(SyntaxRole::Keyword), "const");
    assert_eq!(role_at(runs, 0, 10), Some(SyntaxRole::String));
    assert_eq!(role_at(runs, 0, 17), Some(SyntaxRole::Comment));
    let (generation, version, _, _) = d.syntax.last().unwrap();
    assert_eq!((*generation, *version), (d.open_generation, 0));
}

#[test]
fn fr_e6_a_grammar_that_a_second_file_of_the_language_needs_is_not_read_again() {
    let mut d = Driver::new();
    d.open("/w/a.ts", "let a;\n");
    let reads_after_first = d.reads.len();
    d.open("/w/b.ts", "let b;\n");
    assert_eq!(d.reads.len(), reads_after_first, "the second file reuses the loaded grammars");
    assert_eq!(role_at(d.last_runs(), 0, 0), Some(SyntaxRole::Keyword));
}

#[test]
fn fr_e6_a_file_whose_language_has_no_grammar_gets_no_jobs() {
    let mut d = Driver::new();
    d.open("/w/notes.txt", "const x = 1\n");
    assert!(d.jobs.is_empty() && d.reads.is_empty() && d.syntax.is_empty());
}

#[test]
fn fr_e6_an_edit_updates_the_colours_against_the_new_version() {
    let mut d = Driver::new();
    d.open("/w/a.ts", "let a = 1;\nlet b = 2;\n");
    let generation = d.open_generation;
    d.feed(Input::Widget {
        uri: uri("/w/a.ts"),
        generation,
        event: WidgetEvent::CodeChanged { version: 1, range: WidgetRange::new(p(1, 0), p(1, 0)), text: "/*".to_owned() },
    });
    let (_, version, _, runs) = d.syntax.last().unwrap();
    assert_eq!(*version, 1);
    assert_eq!(role_at(runs, 1, 0), Some(SyntaxRole::Comment), "the second line is now inside a comment");
    assert_eq!(role_at(runs, 0, 0), Some(SyntaxRole::Keyword), "the first is unchanged");
}

#[test]
fn fr_e6_the_window_of_coloured_lines_follows_the_visible_lines() {
    let mut d = Driver::new();
    let text: String = (0..1000).map(|i| format!("let v{i} = {i};\n")).collect();
    d.open("/w/big.ts", &text);
    let (_, _, first_window, runs) = d.syntax.last().unwrap().clone();
    assert_eq!(first_window.0, 0);
    assert!(role_at(&runs, 10, 0).is_some() && role_at(&runs, 800, 0).is_none(), "only the first lines carry colours at first");
    let generation = d.open_generation;
    d.feed(Input::Widget { uri: uri("/w/big.ts"), generation, event: WidgetEvent::VisibleRangeChanged { version: 0, first_line: 790, last_line: 830 } });
    let (_, _, window, runs) = d.syntax.last().unwrap();
    assert!(window.0 <= 790 && window.1 >= 830, "{window:?}");
    assert_eq!(role_at(runs, 800, 0), Some(SyntaxRole::Keyword));
}

#[test]
fn fr_e6_closing_a_file_drops_its_state_and_a_late_answer_is_ignored() {
    let mut d = Driver::new();
    d.open("/w/a.ts", "let a;\n");
    let before = d.syntax.len();
    d.feed(Input::Close { uri: uri("/w/a.ts") });
    assert!(d.jobs.iter().any(|j| matches!(j, HighlightJob::Drop { .. })));
    // An answer for the closed document arrives after the close.
    let late = HighlightOutput { key: uri("/w/a.ts").key(), seq: 1, version: 5, window: (0, 10), runs: Vec::new(), problem: None };
    d.feed(Input::Highlighted(late));
    assert_eq!(d.syntax.len(), before, "no update for a closed document");
}

// ---- the worker ---------------------------------------------------------------------------------------

fn build_job(key: &str, seq: u64, text: &str) -> HighlightJob {
    let dir = fixtures().join("grammars");
    let read = |name: &str| -> std::sync::Arc<str> { std::fs::read_to_string(dir.join(name)).unwrap().into() };
    HighlightJob::Build {
        key: key.to_owned(),
        seq,
        root_scope: "source.ts".to_owned(),
        grammars: vec![("source.ts".to_owned(), "/g/TypeScript.tmLanguage.json".to_owned(), read("TypeScript.tmLanguage.json"))],
        injectors: Vec::new(),
        text: text.to_owned(),
        version: 0,
        window: (0, 300),
    }
}

fn edit(key: &str, seq: u64, at: (u32, u32), text: &str, version: u32) -> HighlightJob {
    HighlightJob::Edit { key: key.to_owned(), seq, range: WidgetRange::new(p(at.0, at.1), p(at.0, at.1)), text: text.to_owned(), version }
}

#[test]
fn fr_e6_edits_that_queued_up_give_one_answer_for_the_last_version() {
    let mut w = Worker::new();
    let out = w.run(vec![build_job("k", 1, "let a;"), edit("k", 1, (0, 6), "1", 1), edit("k", 1, (0, 7), "2", 2), edit("k", 1, (0, 8), "3", 3)]);
    assert_eq!(out.len(), 1, "one answer for the batch");
    assert_eq!(out[0].version, 3);
    assert_eq!(out[0].seq, 1);
    assert!(out[0].problem.is_none());
}

#[test]
fn fr_e6_an_edit_the_worker_cannot_place_asks_for_a_rebuild_and_a_stale_seq_is_ignored() {
    let mut w = Worker::new();
    w.run(vec![build_job("k", 1, "let a;")]);
    let out = w.run(vec![edit("k", 1, (9, 0), "x", 1)]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].problem, Some(Problem::Rebuild));
    // After a rebuild with a new seq, an edit still carrying the old seq changes nothing.
    w.run(vec![build_job("k", 2, "let a;")]);
    assert!(w.run(vec![edit("k", 1, (0, 0), "x", 1)]).is_empty());
}

#[test]
fn fr_e6_a_grammar_that_is_missing_turns_highlighting_off_without_a_crash() {
    let mut w = Worker::new();
    let job = HighlightJob::Build {
        key: "k".into(),
        seq: 1,
        root_scope: "source.none".into(),
        grammars: Vec::new(),
        injectors: Vec::new(),
        text: "x".into(),
        version: 0,
        window: (0, 10),
    };
    let out = w.run(vec![job]);
    assert!(matches!(out[0].problem, Some(Problem::Off(_))));
}

// ---- a server's grammars are untrusted input -----------------------------------------------------------

#[test]
fn fr_e6_a_grammar_path_that_leaves_the_extension_folder_is_never_requested() {
    let t0 = Instant::now();
    let hostile = vec![json!({
        "identifier": {"value": "evil.ext"},
        "extensionLocation": {"path": "/e/evil"},
        "contributes": {
            "languages": [{"id": "evil", "extensions": [".evil"]}],
            "grammars": [{"language": "evil", "scopeName": "source.evil", "path": "../../etc/passwd"}]
        }
    })];
    let mut core = SessionCore::new(LanguageRegistry::from_extensions(&hostile), CoreOptions::default(), t0);
    core.set_grammar_catalog(GrammarCatalog::from_extensions(&hostile));
    let effects = core.handle(t0, Input::FileRead { uri: uri("/w/a.evil"), purpose: ReadPurpose::Open, result: Ok(b"x".to_vec()) });
    assert!(
        !effects.iter().any(|e| matches!(e, Effect::ReadFile { purpose: ReadPurpose::Grammar { .. }, .. })),
        "no read of a path outside the extension: {effects:?}"
    );
}

#[test]
fn fr_e6_a_grammar_answer_over_the_size_cap_turns_colours_off_for_the_language() {
    let mut d = Driver::new();
    let mut effects = d.core.handle(d.t, Input::FileRead { uri: uri("/w/a.ts"), purpose: ReadPurpose::Open, result: Ok(b"let a;\n".to_vec()) });
    let first = effects
        .drain(..)
        .find_map(|e| match e {
            Effect::ReadFile { uri, purpose: ReadPurpose::Grammar { scope } } => Some((uri, scope)),
            _ => None,
        })
        .expect("a grammar read");
    let huge = vec![b' '; ember_editor::textmate::MAX_GRAMMAR_BYTES as usize + 1];
    let after = d.core.handle(d.t, Input::FileRead { uri: first.0, purpose: ReadPurpose::Grammar { scope: first.1 }, result: Ok(huge) });
    assert!(!after.iter().any(|e| matches!(e, Effect::Highlight(_))), "no build from an oversized grammar: {after:?}");
}

#[test]
fn fr_e6_runs_for_a_version_the_document_has_not_reached_or_is_far_past_are_dropped() {
    let mut d = Driver::new();
    d.open("/w/a.ts", "let a;\n");
    let seq = match d.jobs.iter().find_map(|j| match j { HighlightJob::Build { seq, .. } => Some(*seq), _ => None }) {
        Some(s) => s,
        None => panic!("a build job"),
    };
    let before = d.syntax.len();
    let future = HighlightOutput { key: uri("/w/a.ts").key(), seq, version: 9, window: (0, 10), runs: Vec::new(), problem: None };
    d.feed(Input::Highlighted(future));
    assert_eq!(d.syntax.len(), before, "an answer for version 9 of a document at version 0 is dropped");
}

#[test]
fn fr_e6_a_panic_in_the_worker_is_contained_and_reported_as_colours_off() {
    use ember_editor::highlight::guard;
    let mut worker = Worker::new();
    worker.run(vec![build_job("k", 1, "let a;")]);
    let outputs = guard(&mut worker, vec![edit("k", 1, (0, 0), "x", 1), HighlightJob::Window { key: "other".into(), seq: 4, window: (0, 5) }], |_, _| {
        panic!("a broken grammar")
    });
    let mut keys: Vec<(&str, u64)> = outputs.iter().map(|o| (o.key.as_str(), o.seq)).collect();
    keys.sort();
    assert_eq!(keys, vec![("k", 1), ("other", 4)]);
    assert!(outputs.iter().all(|o| matches!(o.problem, Some(Problem::Off(_)))));
    // The thread survives with a fresh worker: a later job works.
    assert_eq!(guard(&mut worker, vec![build_job("k2", 2, "let b;")], Worker::run).len(), 1);
}

#[test]
fn fr_e6_a_rebuild_answer_makes_the_session_send_a_new_build_with_a_new_seq() {
    let mut d = Driver::new();
    d.open("/w/a.ts", "let a;\n");
    let seqs = |d: &Driver| -> Vec<u64> { d.jobs.iter().filter_map(|j| match j { HighlightJob::Build { seq, .. } => Some(*seq), _ => None }).collect() };
    let first = seqs(&d);
    assert_eq!(first.len(), 1);
    d.feed(Input::Highlighted(HighlightOutput::rebuild(&uri("/w/a.ts").key(), first[0])));
    let all = seqs(&d);
    assert_eq!(all.len(), 2, "one more build");
    assert!(all[1] > all[0], "the new build has a new seq, so answers to the old one are ignored");
    assert_eq!(role_at(d.last_runs(), 0, 0), Some(SyntaxRole::Keyword), "and the file is coloured again");
}

#[test]
fn fr_e6_a_grammar_that_failed_to_read_is_tried_again_when_the_next_file_opens() {
    let t0 = Instant::now();
    let mut core = core(t0);
    let reads = |effects: &[Effect]| -> Vec<(UriComponents, String)> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::ReadFile { uri, purpose: ReadPurpose::Grammar { scope } } => Some((uri.clone(), scope.clone())),
                _ => None,
            })
            .collect()
    };
    let effects = core.handle(t0, Input::FileRead { uri: uri("/w/a.ts"), purpose: ReadPurpose::Open, result: Ok(b"let a;\n".to_vec()) });
    let first = reads(&effects);
    // The language's grammar and the JSDoc grammar injected into it are both asked for.
    let root = first.iter().find(|(_, scope)| scope == "source.ts").expect("the language's own grammar is read").clone();
    assert_eq!(first.len(), 2, "{first:?}");
    // The read of the language's grammar fails.
    core.handle(t0, Input::FileRead { uri: root.0.clone(), purpose: ReadPurpose::Grammar { scope: root.1.clone() }, result: Err("busy".into()) });
    // The next file of the language asks again for what failed.
    let effects = core.handle(t0, Input::FileRead { uri: uri("/w/b.ts"), purpose: ReadPurpose::Open, result: Ok(b"let b;\n".to_vec()) });
    let again = reads(&effects);
    assert!(again.iter().any(|(_, scope)| scope == "source.ts"), "the failed grammar is requested again: {again:?}");
}

#[test]
fn fr_e6_the_grammars_injected_into_a_language_are_read_and_sent_with_the_build() {
    let mut d = Driver::new();
    d.open("/w/a.ts", "/** doc */\nlet a;\n");
    assert!(d.reads.contains(&"documentation.injection.ts".to_owned()), "reads: {:?}", d.reads);
    let injectors = d.jobs.iter().find_map(|j| match j {
        HighlightJob::Build { injectors, .. } => Some(injectors.clone()),
        _ => None,
    });
    assert_eq!(injectors, Some(vec!["documentation.injection.ts".to_owned()]));
    assert_eq!(role_at(d.last_runs(), 1, 0), Some(SyntaxRole::Keyword), "and the file is coloured");
}
