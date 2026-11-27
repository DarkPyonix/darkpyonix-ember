//! FR-L31.42 (#445): the problem matchers of a task, against Code-OSS's `problemMatcher.ts` and
//! `problemCollectors.ts`. The lines are what each tool prints; the expected markers are what
//! Code-OSS gives the marker service for them.

use std::collections::{BTreeMap, BTreeSet};

use ember_editor::problem_matcher::{
    ApplyTo, CollectorEvent, FileLocation, FilePrefix, LocationKind, PatternSet, ProblemCollector, ProblemMatcher, Registry,
    TaskSeverity,
};
use ember_editor_conn::exthost::MarkerData;
use serde_json::{json, Value};

fn builtin(name: &str) -> ProblemMatcher {
    let mut m = Registry::builtin().matcher(name).unwrap_or_else(|| panic!("no ${name}")).clone();
    m.resolve_prefix(|p| p.replace("${workspaceFolder}", "/w"));
    m
}

/// The markers the collector reports for `lines`, after the task ended, per (owner, path).
fn run(matchers: Vec<ProblemMatcher>, lines: &[&str]) -> Vec<(String, String, Vec<MarkerData>)> {
    let mut c = ProblemCollector::new(matchers, false, BTreeMap::new());
    for l in lines {
        c.process_line(l);
    }
    c.done();
    c.take_events()
        .into_iter()
        .filter_map(|e| match e {
            CollectorEvent::Markers { owner, path, markers } => Some((owner, path, markers)),
            _ => None,
        })
        .collect()
}

fn one(matcher: &str, line: &str) -> (String, MarkerData) {
    let found = run(vec![builtin(matcher)], &[line]);
    assert_eq!(found.len(), 1, "${matcher} on {line:?}: {found:?}");
    assert_eq!(found[0].2.len(), 1);
    (found[0].1.clone(), found[0].2[0].clone())
}

fn at(m: &MarkerData) -> (u32, u32, u32, u32) {
    (m.start_line_number, m.start_column, m.end_line_number, m.end_column)
}

const WHOLE_LINE: u32 = i32::MAX as u32;

// Every matcher Code-OSS registers itself, with its owner, source, applyTo and file location.
#[test]
fn fr_l31_42_the_built_in_matchers_are_code_oss() {
    let r = Registry::builtin();
    let mut names = r.names();
    names.sort_unstable();
    assert_eq!(names, ["eslint-compact", "eslint-stylish", "go", "gulp-tsc", "jshint", "jshint-stylish", "lessCompile", "msCompile"]);
    let row = |n: &str| {
        let m = r.matcher(n).unwrap();
        (m.owner.as_str(), m.source.as_deref(), m.apply_to, m.file_location)
    };
    assert_eq!(row("msCompile"), ("msCompile", Some("cpp"), ApplyTo::AllDocuments, FileLocation::Absolute));
    assert_eq!(row("lessCompile"), ("lessCompile", Some("less"), ApplyTo::AllDocuments, FileLocation::Absolute));
    assert_eq!(row("gulp-tsc"), ("typescript", Some("ts"), ApplyTo::ClosedDocuments, FileLocation::Relative));
    assert_eq!(row("jshint"), ("jshint", Some("jshint"), ApplyTo::AllDocuments, FileLocation::Absolute));
    assert_eq!(row("jshint-stylish"), ("jshint", Some("jshint"), ApplyTo::AllDocuments, FileLocation::Absolute));
    assert_eq!(row("eslint-compact"), ("eslint", Some("eslint"), ApplyTo::AllDocuments, FileLocation::Absolute));
    assert_eq!(row("eslint-stylish"), ("eslint", Some("eslint"), ApplyTo::AllDocuments, FileLocation::Absolute));
    assert_eq!(row("go"), ("go", Some("go"), ApplyTo::AllDocuments, FileLocation::Relative));
    assert!(r.matcher("lessCompile").unwrap().deprecated);
    assert_eq!(r.matcher("lessCompile").unwrap().severity, Some(TaskSeverity::Error));
    assert_eq!(r.matcher("go").unwrap().file_prefix, FilePrefix::Path("${workspaceFolder}".into()));
    // The patterns Code-OSS names but no built-in matcher uses.
    for p in ["cpp", "csc", "vb"] {
        assert!(matches!(r.pattern(p), Some(PatternSet::Single(_))), "{p}");
    }
    assert!(matches!(r.pattern("eslint-stylish"), Some(PatternSet::Multi(v)) if v.len() == 2 && v[1].loops));
}

#[test]
fn fr_l31_42_ms_compile_reads_location_severity_and_code() {
    let (path, m) = one("msCompile", r"  12>C:\src\a.cpp(10,5): error C2065: 'x': undeclared identifier");
    assert_eq!(path, "/C:/src/a.cpp");
    assert_eq!(m.severity, 8);
    assert_eq!(m.code, Some(json!("C2065")));
    assert_eq!(m.message, "'x': undeclared identifier");
    assert_eq!(m.source.as_deref(), Some("cpp"));
    assert_eq!(at(&m), (10, 5, 10, 5));
    // `fatal error` is no severity name: the matcher's default, an error.
    let (_, m) = one("msCompile", "/src/a.cpp(3): fatal error C1083: Cannot open include file");
    assert_eq!(m.severity, 8);
    assert_eq!(at(&m), (3, 1, 3, WHOLE_LINE));
    let (_, m) = one("msCompile", "/src/a.cpp(1,2,3,4): warning C4100: unreferenced");
    assert_eq!((m.severity, at(&m)), (4, (1, 2, 3, 4)));
}

#[test]
fn fr_l31_42_less_and_gulp_tsc_and_go_resolve_their_files() {
    let (path, m) = one("lessCompile", "unrecognised input in file /w/a.less line no. 7");
    assert_eq!((path.as_str(), m.message.as_str(), at(&m)), ("/w/a.less", "unrecognised input", (7, 1, 7, WHOLE_LINE)));
    let (path, m) = one("gulp-tsc", "src/app.ts(4,9): 2304 Cannot find name 'foo'.");
    assert_eq!(path, "/w/src/app.ts", "relative to ${{workspaceFolder}}");
    assert_eq!((m.code, at(&m)), (Some(json!("2304")), (4, 9, 4, 9)));
    let (path, m) = one("go", "./main.go:12:3: undefined: fmt.Printx");
    assert_eq!(path, "/w/main.go");
    assert_eq!((m.message.as_str(), at(&m)), ("undefined: fmt.Printx", (12, 3, 12, 3)));
    assert!(run(vec![builtin("go")], &["# example", "ok  example 0.01s"]).is_empty());
}

#[test]
fn fr_l31_42_jshint_and_eslint_compact_read_single_lines() {
    let (path, m) = one("jshint", "/w/a.js: line 3, col 7, Missing semicolon. (W033)");
    assert_eq!(path, "/w/a.js");
    assert_eq!((m.severity, m.code.clone(), m.message.as_str(), at(&m)), (4, Some(json!("033")), "Missing semicolon.", (3, 7, 3, 7)));
    let (_, m) = one("eslint-compact", "/w/b.js: line 1, col 2, Error - Unexpected var (no-var)");
    assert_eq!((m.severity, m.code, m.message.as_str()), (8, Some(json!("no-var")), "Unexpected var"));
}

// A multi-line matcher: the file on one line, then every problem below it while the loop matches.
#[test]
fn fr_l31_42_eslint_stylish_and_jshint_stylish_loop_over_a_file() {
    let found = run(
        vec![builtin("eslint-stylish")],
        &[
            "/w/src/a.js",
            "  1:10  error    'x' is defined but never used  no-unused-vars",
            "  2:1   warning  Unexpected console statement   no-console",
            "",
            "/w/src/b.js",
            "  5:3  error  Missing semicolon  semi",
            "✖ 3 problems",
        ],
    );
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].1, "/w/src/a.js");
    let a = &found[0].2;
    assert_eq!(a.len(), 2);
    assert_eq!((a[0].severity, a[0].message.as_str(), a[0].code.clone(), at(&a[0])), (8, "'x' is defined but never used", Some(json!("no-unused-vars")), (1, 10, 1, 10)));
    assert_eq!((a[1].severity, a[1].code.clone()), (4, Some(json!("no-console"))));
    assert_eq!(found[1].1, "/w/src/b.js");
    assert_eq!(found[1].2[0].message, "Missing semicolon");

    let found = run(vec![builtin("jshint-stylish")], &["/w/c.js", "  line 4  col 2  Missing semicolon. (W033)"]);
    assert_eq!(found[0].2[0].severity, 4);
    assert_eq!(found[0].2[0].code, Some(json!("033")));
}

// `$tsc` and `$tsc-watch` come from the TypeScript extension's package.json in Code-OSS, so they
// are read from the running extensions; `$gcc` likewise comes with the C/C++ extension.
#[test]
fn fr_l31_42_contributed_matchers_and_patterns_are_read_from_extensions() {
    let ts = json!({
        "name": "typescript-language-features",
        "contributes": {
            "problemPatterns": [{
                "name": "tsc",
                "regexp": "^([^\\s].*)[\\(:](\\d+)[,:](\\d+)(?:\\):\\s+|\\s+-\\s+)(error|warning|info)\\s+TS(\\d+)\\s*:\\s*(.*)$",
                "file": 1, "line": 2, "column": 3, "severity": 4, "code": 5, "message": 6
            }],
            "problemMatchers": [
                { "name": "tsc", "label": "%typescript.problemMatchers.tsc.label%", "owner": "typescript", "source": "ts",
                  "applyTo": "closedDocuments", "fileLocation": ["relative", "${cwd}"], "pattern": "$tsc" },
                { "name": "tsc-watch", "owner": "typescript", "source": "ts", "applyTo": "closedDocuments",
                  "fileLocation": ["relative", "${cwd}"], "pattern": "$tsc",
                  "background": { "activeOnStart": true,
                      "beginsPattern": { "regexp": "^\\s*(?:message TS6032:|\\[?\\D*.{1,2}[:.].{1,2}[:.].{1,2}\\D*(├\\D*\\d{1,2}\\D+┤)?(?:\\]| -)) (Starting compilation in watch mode|File change detected\\. Starting incremental compilation)\\.\\.\\." },
                      "endsPattern": { "regexp": "^\\s*(?:message TS6042:|\\[?\\D*.{1,2}[:.].{1,2}[:.].{1,2}\\D*(├\\D*\\d{1,2}\\D+┤)?(?:\\]| -)) (?:Compilation complete\\.|Found \\d+ errors?\\.) Watching for file changes\\." } } }
            ]
        }
    });
    let gcc = json!({ "contributes": { "problemMatchers": [{
        "name": "gcc", "owner": "cpptools", "source": "gcc", "fileLocation": ["autoDetect", "${cwd}"],
        "pattern": { "regexp": "^(.*?):(\\d+):(\\d*):?\\s+(?:fatal\\s+)?(warning|error):\\s+(.*)$", "file": 1, "line": 2, "column": 3, "severity": 4, "message": 5 }
    }] } });
    let r = Registry::with_extensions(&[ts, gcc]);
    let tsc = r.matcher("tsc").expect("$tsc");
    assert_eq!((tsc.owner.as_str(), tsc.apply_to, tsc.file_prefix.clone()), ("typescript", ApplyTo::ClosedDocuments, FilePrefix::Path("${cwd}".into())));
    let watch = r.matcher("tsc-watch").expect("$tsc-watch");
    assert!(watch.watching.as_ref().is_some_and(|w| w.active_on_start && w.begins.file == Some(1)));
    assert!(r.matcher("msCompile").is_some(), "the built-ins stay");

    let mut tsc = tsc.clone();
    tsc.resolve_prefix(|p| p.replace("${cwd}", "/w/app"));
    let found = run(vec![tsc], &["src/index.ts(3,7): error TS2322: Type 'string' is not assignable to type 'number'."]);
    assert_eq!(found[0].1, "/w/app/src/index.ts");
    assert_eq!((found[0].2[0].code.clone(), at(&found[0].2[0])), (Some(json!("2322")), (3, 7, 3, 7)));

    let mut gcc = r.matcher("gcc").unwrap().clone();
    gcc.resolve_prefix(|p| p.replace("${cwd}", "/w"));
    let found = run(vec![gcc], &["main.c:4:12: error: expected ';' before 'return'", "/usr/include/x.h:1:1: warning: odd"]);
    assert_eq!(found[0].1, "/w/main.c", "autoDetect joins a relative name to the prefix");
    assert_eq!(found[1].1, "/usr/include/x.h", "and keeps an absolute one");
    assert_eq!(found[1].2[0].severity, 4);
}

// An inline definition of tasks.json gets Code-OSS's defaults: the groups 1, 2, 3 and 0, kind
// `location`, `fileLocation` relative to the workspace folder, `applyTo` all documents.
#[test]
fn fr_l31_42_an_inline_matcher_gets_code_oss_defaults() {
    let r = Registry::builtin();
    let mut errors = Vec::new();
    let found = r.resolve(Some(&json!({ "owner": "mine", "pattern": { "regexp": "^(.*):(\\d+):(\\d+) (.*)$", "message": 4 } })), &mut errors);
    assert!(errors.is_empty(), "{errors:?}");
    let m = &found[0];
    assert_eq!((m.owner.as_str(), m.apply_to, m.file_location), ("mine", ApplyTo::AllDocuments, FileLocation::Relative));
    assert_eq!(m.file_prefix, FilePrefix::Path("${workspaceFolder}".into()));
    let PatternSet::Single(p) = &m.pattern else { panic!() };
    assert_eq!((p.file, p.line, p.character, p.message, p.kind), (Some(1), Some(2), Some(3), Some(4), Some(LocationKind::Location)));

    // No owner: a generated one, as Code-OSS's parser gives.
    let found = r.resolve(Some(&json!({ "pattern": { "regexp": "^(.*)$", "kind": "file", "message": 1 } })), &mut errors);
    assert!(!found[0].owner.is_empty());
    let PatternSet::Single(p) = &found[0].pattern else { panic!() };
    assert_eq!((p.file, p.line), (Some(1), None), "a file pattern has no line default");

    // A list, a `$name`, a `base` with overrides, and the forms that fail with Code-OSS's message.
    let mut errors = Vec::new();
    let found = r.resolve(
        Some(&json!([
            "$go",
            { "base": "$msCompile", "owner": "cpp-build", "fileLocation": ["relative", "/w/build"], "severity": "warning" },
            "$nope",
            "msCompile",
            { "pattern": "$missing" },
        ])),
        &mut errors,
    );
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].owner, "go");
    assert_eq!((found[1].owner.as_str(), found[1].file_location, found[1].severity), ("cpp-build", FileLocation::Relative, Some(TaskSeverity::Warning)));
    assert_eq!(found[1].file_prefix, FilePrefix::Path("/w/build".into()));
    assert!(errors.iter().any(|e| e == "Error: Invalid problemMatcher reference: $nope\n"), "{errors:?}");
    assert!(errors.iter().any(|e| e == "Error: Invalid problemMatcher reference: msCompile\n"), "{errors:?}");
    assert!(errors.iter().any(|e| e.contains("the pattern with the identifier $missing doesn't exist")), "{errors:?}");
    assert!(r.resolve(None, &mut errors).is_empty());
    assert!(r.resolve(Some(&json!([])), &mut errors).is_empty());

    // A multi-line pattern without a message, or a pattern that does not compile, is refused.
    let mut errors = Vec::new();
    assert!(r.resolve(Some(&json!({ "pattern": [{ "regexp": "^(.*)$", "file": 1 }, { "regexp": "^(\\d+)$", "line": 1 }] })), &mut errors).is_empty());
    assert!(errors.iter().any(|e| e.contains("at least have a file and a message")), "{errors:?}");
    let mut errors = Vec::new();
    assert!(r.resolve(Some(&json!({ "pattern": { "regexp": "(" } })), &mut errors).is_empty());
    assert!(errors.iter().any(|e| e.contains("is not a valid regular expression")), "{errors:?}");
}

#[test]
fn fr_l31_42_file_locations_follow_get_resource() {
    let r = Registry::builtin();
    let mut errors = Vec::new();
    let matcher = |loc: Value| {
        let mut m = r.resolve(Some(&json!({ "owner": "o", "fileLocation": loc, "pattern": { "regexp": "^(.*):(\\d+): (.*)$", "message": 3 } })), &mut Vec::new()).remove(0);
        m.resolve_prefix(|p| p.replace("${workspaceFolder}", "/w"));
        m
    };
    assert_eq!(matcher(json!("absolute")).resource("/a/./b/../c.rs").as_deref(), Some("/a/c.rs"));
    assert_eq!(matcher(json!("relative")).resource("src//lib.rs").as_deref(), Some("/w/src/lib.rs"));
    assert_eq!(matcher(json!(["relative", "/w/sub"])).resource("../x.rs").as_deref(), Some("/w/x.rs"));
    assert_eq!(matcher(json!("autoDetect")).resource("x.rs").as_deref(), Some("/w/x.rs"));
    assert_eq!(matcher(json!("autoDetect")).resource("/abs/x.rs").as_deref(), Some("/abs/x.rs"));
    assert_eq!(matcher(json!(["absolute"])).resource(r"C:\x\y.rs").as_deref(), Some("/C:/x/y.rs"));
    let search = matcher(json!(["search", { "include": ["${workspaceFolder}/src"] }]));
    assert_eq!(search.file_location, FileLocation::Search);
    assert_eq!(search.file_prefix, FilePrefix::Search { include: vec!["/w/src".into()], exclude: vec![] });
    assert_eq!(search.resource("/w/src/a.rs").as_deref(), Some("/w/src/a.rs"), "no search: the name is taken as absolute");
    // `["relative"]` alone is no file location, so the definition is refused.
    assert!(r.resolve(Some(&json!({ "fileLocation": ["relative"], "pattern": { "regexp": "^(.*):(\\d+): (.*)$" } })), &mut errors).is_empty());
}

// Severity: a severity group's word, `E`/`W`/`I`, `hint` and `note`; else the matcher's
// `severity`; else an error.
#[test]
fn fr_l31_42_severity_follows_get_severity() {
    let r = Registry::builtin();
    let m = |severity: Option<&str>| {
        let mut def = json!({ "owner": "o", "fileLocation": "absolute", "pattern": { "regexp": "^(\\S+) (\\S*) (.*)$", "line": null, "kind": "file", "severity": 2, "message": 3 } });
        if let Some(s) = severity {
            def["severity"] = json!(s);
        }
        r.resolve(Some(&def), &mut Vec::new()).remove(0)
    };
    let sev = |matcher: ProblemMatcher, line: &str| run(vec![matcher], &[line])[0].2[0].severity;
    assert_eq!(sev(m(None), "/a WARN msg"), 4);
    assert_eq!(sev(m(None), "/a W msg"), 4);
    assert_eq!(sev(m(None), "/a I msg"), 2);
    assert_eq!(sev(m(None), "/a note msg"), 2);
    assert_eq!(sev(m(None), "/a Hint msg"), 2);
    assert_eq!(sev(m(None), "/a odd msg"), 8);
    assert_eq!(sev(m(Some("info")), "/a odd msg"), 2);
    assert_eq!(sev(m(Some("bogus")), "/a odd msg"), 8, "an unknown severity is an error");
    // A file pattern's marker covers the first line, as the marker service makes line 0.
    let found = run(vec![m(None)], &["/a error broken"]);
    assert_eq!(at(&found[0].2[0]), (1, 1, 1, 1));
}

// The start-stop collector cleans, at the end, the files its owner reported before and this run
// did not report again; a file reported again keeps its new markers. Two identical problems are one.
#[test]
fn fr_l31_42_a_run_cleans_what_its_owner_reported_before() {
    let mut known = BTreeMap::new();
    known.insert("go".to_owned(), BTreeSet::from(["/w/old.go".to_owned(), "/w/main.go".to_owned()]));
    let mut c = ProblemCollector::new(vec![builtin("go")], false, known);
    c.process_line("main.go:1:1: first");
    c.process_line("main.go:1:1: first");
    c.process_line("main.go:2:1: second");
    c.done();
    let events = c.take_events();
    assert_eq!(events.len(), 2, "{events:?}");
    match &events[0] {
        CollectorEvent::Markers { owner, path, markers } => {
            assert_eq!((owner.as_str(), path.as_str(), markers.len()), ("go", "/w/main.go", 2));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(events[1], CollectorEvent::Remove { owner: "go".into(), paths: vec!["/w/old.go".into()] });
    assert_eq!(c.number_of_matches(), 3);
    assert_eq!(c.max_severity(), Some(8));
}

// `applyTo`: `closedDocuments` skips an open file; `openDocuments` only takes open ones.
#[test]
fn fr_l31_42_apply_to_follows_the_open_documents() {
    let mut c = ProblemCollector::new(vec![builtin("gulp-tsc")], false, BTreeMap::new());
    c.set_open_documents(["/w/open.ts".to_owned()]);
    c.process_line("open.ts(1,1): 1 shown by the language server instead");
    c.process_line("closed.ts(1,1): 2 reported");
    c.done();
    let paths: Vec<String> = c
        .take_events()
        .into_iter()
        .filter_map(|e| match e {
            CollectorEvent::Markers { path, .. } => Some(path),
            _ => None,
        })
        .collect();
    assert_eq!(paths, ["/w/closed.ts"]);
}

fn watch_matcher(active_on_start: bool) -> ProblemMatcher {
    let def = json!({
        "owner": "watch", "fileLocation": "absolute",
        "pattern": { "regexp": "^(\\S+):(\\d+): (.*)$", "message": 3, "column": null },
        "background": { "activeOnStart": active_on_start, "beginsPattern": "^build started", "endsPattern": { "regexp": "^build done" } }
    });
    Registry::builtin().resolve(Some(&def), &mut Vec::new()).remove(0)
}

// A background task: `beginsPattern` starts a pass (the task is active), `endsPattern` ends it
// (inactive, the point a debug start waits for), and a pass reports and cleans its own problems.
#[test]
fn fr_l31_42_background_begin_and_end_patterns_mark_the_passes() {
    let mut c = ProblemCollector::new(vec![watch_matcher(false)], true, BTreeMap::new());
    assert!(c.is_watching());
    c.about_to_start();
    assert!(c.take_events().is_empty(), "not active on start");
    c.process_line("starting the watcher");
    c.process_line("/w/a.rs:1: before any pass still counts");
    c.take_events();
    c.process_line("build started");
    assert_eq!(c.take_events(), [CollectorEvent::Begins]);
    c.process_line("/w/a.rs:3: broken");
    c.process_line("/w/b.rs:4: broken too");
    c.process_line("build done");
    let events = c.take_events();
    assert!(matches!(&events[0], CollectorEvent::Markers { path, .. } if path == "/w/a.rs"), "{events:?}");
    assert!(matches!(&events[1], CollectorEvent::Markers { path, .. } if path == "/w/b.rs"), "{events:?}");
    assert_eq!(events.last(), Some(&CollectorEvent::Ends));
    assert!(!events.iter().any(|e| matches!(e, CollectorEvent::Remove { .. })), "nothing to clean in the first pass");

    // The next pass: b.rs is fixed, so it is cleaned when the pass ends.
    c.process_line("build started");
    c.process_line("/w/a.rs:3: broken");
    c.process_line("build done");
    let events = c.take_events();
    // The end is announced first, then the owner's stale files are cleaned, as `tryFinish` does.
    assert_eq!(events.first(), Some(&CollectorEvent::Begins));
    let ends = events.iter().position(|e| *e == CollectorEvent::Ends).expect("the pass ended");
    let removed = events
        .iter()
        .position(|e| *e == CollectorEvent::Remove { owner: "watch".into(), paths: vec!["/w/b.rs".into()] })
        .expect("b.rs is cleaned");
    assert!(ends < removed, "{events:?}");

    // `activeOnStart`: the first pass begins at the start; a begin while active is no new pass.
    let mut c = ProblemCollector::new(vec![watch_matcher(true)], true, BTreeMap::new());
    c.about_to_start();
    assert_eq!(c.take_events(), [CollectorEvent::Begins]);
    c.process_line("build started");
    assert!(c.take_events().is_empty());
    c.process_line("build done");
    assert_eq!(c.take_events(), [CollectorEvent::Ends]);
}

// A background task whose matcher has no `background` patterns never begins a pass, so the debug
// start cannot tell it is ready (Code-OSS then gives the untracked-task error).
#[test]
fn fr_l31_42_a_matcher_without_background_patterns_is_not_watching() {
    let mut c = ProblemCollector::new(vec![builtin("go")], true, BTreeMap::new());
    assert!(!c.is_watching());
    c.about_to_start();
    c.process_line("main.go:1:1: x");
    assert!(!c.take_events().iter().any(|e| matches!(e, CollectorEvent::Begins | CollectorEvent::Ends)));
    let mut c = ProblemCollector::new(Vec::new(), true, BTreeMap::new());
    assert!(!c.is_watching());

    // The older `watchedTaskBeginsRegExp` and `watchedTaskEndsRegExp` still make a watcher.
    let def = json!({ "owner": "o", "pattern": "$go", "watchedTaskBeginsRegExp": "^begin", "watchedTaskEndsRegExp": "^end" });
    let m = Registry::builtin().resolve(Some(&def), &mut Vec::new()).remove(0);
    assert!(m.watching.as_ref().is_some_and(|w| !w.active_on_start && w.begins.file.is_none()));
    // One pattern alone is an error and no watcher.
    let mut errors = Vec::new();
    let def = json!({ "owner": "o", "pattern": "$go", "background": { "beginsPattern": "^begin" } });
    let m = Registry::builtin().resolve(Some(&def), &mut errors).remove(0);
    assert!(m.watching.is_none());
    assert!(errors.iter().any(|e| e.contains("both a begin pattern and an end pattern")));
    c.done();
}

// When the background task's process ends, every problem of its owner is removed, as the watching
// collector's `done` cleans all it recorded.
#[test]
fn fr_l31_42_a_background_task_that_ends_removes_its_problems() {
    let mut c = ProblemCollector::new(vec![watch_matcher(true)], true, BTreeMap::new());
    c.about_to_start();
    c.process_line("/w/a.rs:1: broken");
    c.process_line("build done");
    c.take_events();
    c.done();
    assert_eq!(c.take_events(), [CollectorEvent::Remove { owner: "watch".into(), paths: vec!["/w/a.rs".into()] }]);
}
