use super::*;
use crate::terminal_stream::OutputLines;
use crate::ui::editor_tasks::{TasksLoad, parse_tasks_json};

const TASKS: &str = r#"{
    "version": "2.0.0",
    "tasks": [
        { "label": "build", "type": "shell", "command": "go build ./...", "problemMatcher": "$go" },
        { "label": "lint", "type": "shell", "command": "eslint .", "problemMatcher": ["$eslint-stylish", "$nope"] },
        { "label": "tsc", "type": "shell", "command": "tsc", "options": { "cwd": "${workspaceFolder}/web" }, "problemMatcher": "$tsc" },
        { "label": "watch", "type": "shell", "command": "make watch", "isBackground": true,
          "problemMatcher": { "owner": "make", "fileLocation": ["relative", "${cwd}"],
              "pattern": { "regexp": "^(.*):(\\d+): (.*)$", "message": 3, "column": null },
              "background": { "beginsPattern": "^make: building", "endsPattern": "^make: done" } } },
        { "label": "plain", "type": "shell", "command": "true" },
    ],
}"#;

fn task(label: &str) -> TaskDef {
    parse_tasks_json(TASKS).unwrap().into_iter().find(|t| t.label == label).unwrap()
}

fn ctx() -> VarContext {
    VarContext { folder: "/w".into(), file: None }
}

fn registry() -> Registry {
    let ts = serde_json::json!({ "contributes": {
        "problemPatterns": [{ "name": "tsc", "regexp": "^([^\\s].*)[\\(:](\\d+)[,:](\\d+)(?:\\):\\s+|\\s+-\\s+)(error|warning|info)\\s+TS(\\d+)\\s*:\\s*(.*)$",
            "file": 1, "line": 2, "column": 3, "severity": 4, "code": 5, "message": 6 }],
        "problemMatchers": [{ "name": "tsc", "owner": "typescript", "source": "ts", "applyTo": "closedDocuments",
            "fileLocation": ["relative", "${cwd}"], "pattern": "$tsc" }]
    } });
    Registry::with_extensions(&[ts])
}

// tasks.json's `problemMatcher` is read as written and resolved against the matchers Code-OSS
// ships and the extensions contribute, with `${cwd}` the task's folder and `${workspaceFolder}`
// the project.
#[test]
fn fr_l31_42_a_task_gets_the_matchers_of_its_problem_matcher() {
    assert_eq!(task("build").problem_matcher, Some(serde_json::json!("$go")));
    assert_eq!(task("plain").problem_matcher, None);
    assert!(matches!(TasksLoad::from_read(Ok(TASKS.into())), TasksLoad::Parsed(_)));

    let (m, errors) = matchers_for(&registry(), &task("build"), &ctx(), "/w");
    assert!(errors.is_empty());
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].resource("main.go").as_deref(), Some("/w/main.go"));

    let (m, errors) = matchers_for(&registry(), &task("lint"), &ctx(), "/w");
    assert_eq!(m.len(), 1, "the unknown name is left out");
    assert_eq!(errors, ["Error: Invalid problemMatcher reference: $nope\n"]);

    let (m, _) = matchers_for(&registry(), &task("tsc"), &ctx(), "/w/web");
    assert_eq!(m[0].owner, "typescript");
    assert_eq!(m[0].resource("src/a.ts").as_deref(), Some("/w/web/src/a.ts"), "${{cwd}} is the task's folder");

    let (m, _) = matchers_for(&registry(), &task("watch"), &ctx(), "/w/sub");
    assert!(m[0].watching.is_some());
    assert_eq!(m[0].resource("x.c").as_deref(), Some("/w/sub/x.c"));

    let (m, errors) = matchers_for(&registry(), &task("plain"), &ctx(), "/w");
    assert!(m.is_empty() && errors.is_empty());
}

// A run's lines become the problems of the matcher's owner; when it ends, what it did not report
// again is removed; a background run's passes make it active, then ready.
#[test]
fn fr_l31_42_task_output_feeds_the_problems() {
    let mut known = BTreeMap::new();
    known.insert("go".to_owned(), BTreeSet::from(["/w/stale.go".to_owned()]));
    let (m, _) = matchers_for(&registry(), &task("build"), &ctx(), "/w");
    let mut c = ProblemCollector::new(m, false, known.clone());
    let mut status = TrackStatus::default();
    let changes = feed(&mut c, vec!["# example".into(), "./main.go:3:5: undefined: x".into()], false, Vec::new(), &mut known, &mut status);
    assert!(changes.is_empty(), "a start-stop run reports a file when it moves on or ends: {changes:?}");
    let changes = feed(&mut c, Vec::new(), true, Vec::new(), &mut known, &mut status);
    assert_eq!(changes.len(), 1, "{changes:?}");
    let (owner, entries) = &changes[0];
    assert_eq!(owner, "go");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0, "/w/main.go");
    let markers = entries[0].1.as_ref().unwrap();
    assert_eq!((markers[0].start_line_number, markers[0].start_column, markers[0].severity), (3, 5, 8));
    assert_eq!(entries[1], ("/w/stale.go".to_owned(), None));
    assert_eq!(known["go"], BTreeSet::from(["/w/main.go".to_owned()]));
    assert_eq!(status, TrackStatus::default(), "a task that exits has no passes");

    let (m, _) = matchers_for(&registry(), &task("watch"), &ctx(), "/w");
    let mut known = BTreeMap::new();
    let mut c = ProblemCollector::new(m, true, known.clone());
    assert!(c.is_watching());
    c.about_to_start();
    let mut status = TrackStatus::default();
    feed(&mut c, vec!["make: building".into()], false, Vec::new(), &mut known, &mut status);
    assert_eq!(status, TrackStatus { began: true, ready: false });
    let changes = feed(&mut c, vec!["a.c:2: oops".into(), "make: done".into()], false, Vec::new(), &mut known, &mut status);
    assert_eq!(status, TrackStatus { began: true, ready: true });
    assert_eq!(changes[0].0, "make");
    assert_eq!(changes[0].1[0].0, "/w/a.c");
}

// The terminal's output as lines: escape sequences dropped, CR LF and LF end a line, a bare CR
// starts it again, a last line without a break counts when the process ends.
#[test]
fn fr_l31_42_the_task_output_is_read_line_by_line() {
    let mut lines = OutputLines::default();
    lines.feed(b"\x1b[1;31msrc/a.go:1:2: bad\x1b[0m   \r\nnext");
    assert_eq!(lines.take(), ["src/a.go:1:2: bad"]);
    lines.feed(b" part\n50%\r100%\n\x1b]0;title\x07done\x1b(B ok\x08\x08K");
    assert_eq!(lines.take(), ["next part", "100%"]);
    lines.finish();
    assert_eq!(lines.take(), ["done K"]);
    assert!(lines.fed);
}
