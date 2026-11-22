//! FR-L31.7 (#302): the indentation options of an open document and the edits of the status bar's
//! indentation pick, through the pure session core. No server: the test reads the effects.

use std::time::Instant;

use ember_editor::engine::ReadPurpose;
use ember_editor::languages::LanguageRegistry;
use ember_editor::{
    CoreOptions, Effect, Indentation, IndentationAction, Input, SessionCore, SessionUpdate, WidgetCommand, WidgetEvent,
    WidgetPos, WidgetRange,
};
use ember_editor_conn::exthost::Call;
use ember_editor_conn::uri::UriComponents;
use serde_json::{json, Value};

fn core(t0: Instant) -> SessionCore {
    let langs = LanguageRegistry::from_extensions(&[json!({"contributes": {"languages": [
        {"id": "json", "aliases": ["JSON"], "extensions": [".json"]}
    ]}})]);
    SessionCore::new(langs, CoreOptions::default(), t0)
}

fn uri(path: &str) -> UriComponents {
    UriComponents::remote("127.0.0.1:9", path)
}

fn open(c: &mut SessionCore, t0: Instant, path: &str, text: &str) -> Vec<Effect> {
    let u = uri(path);
    c.handle(t0, Input::Open { uri: u.clone() });
    c.handle(t0, Input::FileRead { uri: u, purpose: ReadPurpose::Open, result: Ok(text.as_bytes().to_vec()) })
}

fn updates(fx: &[Effect]) -> Vec<&SessionUpdate> {
    fx.iter().filter_map(|e| if let Effect::Ui(u) = e { Some(u) } else { None }).collect()
}

fn indentation(fx: &[Effect]) -> Indentation {
    updates(fx)
        .into_iter()
        .rev()
        .find_map(|u| if let SessionUpdate::Indentation { indentation, .. } = u { Some(*indentation) } else { None })
        .unwrap_or_else(|| panic!("no Indentation in {fx:#?}"))
}

fn options_call(fx: &[Effect]) -> &Call {
    fx.iter()
        .find_map(|e| match e {
            Effect::Rpc { call, .. } if call.method == "$acceptEditorPropertiesChanged" => Some(call),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no $acceptEditorPropertiesChanged in {fx:#?}"))
}

fn set(c: &mut SessionCore, t0: Instant, path: &str, action: IndentationAction) -> Vec<Effect> {
    c.handle(t0, Input::SetIndentation { uri: uri(path), action })
}

fn edits(fx: &[Effect]) -> Vec<(WidgetRange, String)> {
    updates(fx)
        .into_iter()
        .filter_map(|u| match u {
            SessionUpdate::Command { command: WidgetCommand::EditCode { range, text, .. }, .. } => Some((*range, text.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn fr_l31_a_document_opens_with_the_settings_indentation() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let fx = open(&mut c, t0, "/w/a.json", "{\n}\n");
    assert_eq!(indentation(&fx), Indentation { tab_size: 4, indent_size: 4, insert_spaces: true });
}

#[test]
fn fr_l31_indent_using_spaces_then_size_sets_the_document_options() {
    let t0 = Instant::now();
    let mut c = core(t0);
    open(&mut c, t0, "/w/a.json", "{\n}\n");
    open(&mut c, t0, "/w/b.json", "{\n}\n");
    let fx = set(&mut c, t0, "/w/a.json", IndentationAction::IndentUsing { insert_spaces: true, size: 2 });
    assert_eq!(indentation(&fx), Indentation { tab_size: 2, indent_size: 2, insert_spaces: true });
    let options = options_call(&fx).args[1].as_json().cloned().unwrap_or(Value::Null)["options"].clone();
    assert_eq!((options["tabSize"].clone(), options["indentSize"].clone(), options["insertSpaces"].clone()), (json!(2), json!(2), json!(true)));
    // Only the options change: no edit, and the document is not dirty.
    assert!(edits(&fx).is_empty());
    assert!(!updates(&fx).iter().any(|u| matches!(u, SessionUpdate::Dirty { .. })));
    // The same choice again changes nothing.
    assert!(set(&mut c, t0, "/w/a.json", IndentationAction::IndentUsing { insert_spaces: true, size: 2 }).is_empty());
    // Another document keeps the settings' indentation: the choice is for this file only.
    let fx = c.handle(t0, Input::Open { uri: uri("/w/b.json") });
    assert_eq!(indentation(&fx), Indentation { tab_size: 4, indent_size: 4, insert_spaces: true });
    // Opening the changed document again reports its own options.
    let fx = c.handle(t0, Input::Open { uri: uri("/w/a.json") });
    assert_eq!(indentation(&fx).tab_size, 2);
    assert!(updates(&fx).iter().any(|u| matches!(u, SessionUpdate::Opened { tab_width: 2, .. })));
}

#[test]
fn fr_l31_change_tab_display_size_keeps_insert_spaces() {
    let t0 = Instant::now();
    let mut c = core(t0);
    open(&mut c, t0, "/w/a.json", "{\n}\n");
    set(&mut c, t0, "/w/a.json", IndentationAction::IndentUsing { insert_spaces: true, size: 2 });
    let fx = set(&mut c, t0, "/w/a.json", IndentationAction::DisplaySize(8));
    // The tab is 8 wide; one indent level and the Tab key's spaces stay as they were.
    assert_eq!(indentation(&fx), Indentation { tab_size: 8, indent_size: 2, insert_spaces: true });
    assert!(updates(&fx).iter().any(|u| matches!(u, SessionUpdate::Folds { .. })), "folds are measured with the tab width");
}

#[test]
fn fr_l31_detect_indentation_from_content() {
    let t0 = Instant::now();
    let mut c = core(t0);
    open(&mut c, t0, "/w/two.json", "a\n  b\n    c\n  d\ne\n");
    let fx = set(&mut c, t0, "/w/two.json", IndentationAction::Detect);
    assert_eq!(indentation(&fx), Indentation { tab_size: 2, indent_size: 2, insert_spaces: true });
    open(&mut c, t0, "/w/tabs.json", "a\n\tb\n\t\tc\n\td\n");
    let fx = set(&mut c, t0, "/w/tabs.json", IndentationAction::Detect);
    assert_eq!(indentation(&fx), Indentation { tab_size: 4, indent_size: 4, insert_spaces: false });
    // Nothing to learn: the settings' own indentation stands, and no call is made.
    open(&mut c, t0, "/w/flat.json", "a\nb\n");
    assert!(set(&mut c, t0, "/w/flat.json", IndentationAction::Detect).is_empty());
}

#[test]
fn fr_l31_convert_indentation_edits_the_leading_whitespace_and_sets_insert_spaces() {
    let t0 = Instant::now();
    let mut c = core(t0);
    open(&mut c, t0, "/w/a.json", "a\n\tb\n\t\tc\n");
    let fx = set(&mut c, t0, "/w/a.json", IndentationAction::Convert { to_spaces: true });
    let at = |line, col| WidgetPos::new(line, col);
    assert_eq!(
        edits(&fx),
        vec![
            (WidgetRange::new(at(2, 0), at(2, 2)), " ".repeat(8)),
            (WidgetRange::new(at(1, 0), at(1, 1)), " ".repeat(4)),
        ],
        "last line first, one edit per changed line"
    );
    assert!(!updates(&fx).iter().any(|u| matches!(u, SessionUpdate::Indentation { .. })), "the document already inserts spaces");

    // Back to tabs: the same document, now written with spaces by the widget (the test sets it up).
    open(&mut c, t0, "/w/b.json", "a\n    b\n      c\n");
    let fx = set(&mut c, t0, "/w/b.json", IndentationAction::Convert { to_spaces: false });
    assert_eq!(
        edits(&fx),
        vec![
            (WidgetRange::new(at(2, 0), at(2, 6)), "\t  ".to_string()),
            (WidgetRange::new(at(1, 0), at(1, 4)), "\t".to_string()),
        ]
    );
    // insert_spaces was already true for b.json, so the options now change to tabs.
    assert!(!indentation(&fx).insert_spaces);
}

#[test]
fn fr_l31_trim_trailing_whitespace_edits_the_line_ends() {
    let t0 = Instant::now();
    let mut c = core(t0);
    open(&mut c, t0, "/w/a.json", "a  \nb\n");
    let fx = set(&mut c, t0, "/w/a.json", IndentationAction::TrimTrailingWhitespace);
    assert_eq!(edits(&fx), vec![(WidgetRange::new(WidgetPos::new(0, 1), WidgetPos::new(0, 3)), String::new())]);
    assert!(!fx.iter().any(|e| matches!(e, Effect::Rpc { call, .. } if call.method == "$acceptEditorPropertiesChanged")));
}

#[test]
fn fr_l31_convert_indentation_through_the_widget_makes_the_document_dirty_with_the_converted_text() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let u = uri("/w/a.json");
    let fx = open(&mut c, t0, "/w/a.json", "a\n\tb\n\t\tc\n");
    let generation = updates(&fx)
        .into_iter()
        .find_map(|u| if let SessionUpdate::Opened { generation, .. } = u { Some(*generation) } else { None })
        .expect("Opened");
    let fx = set(&mut c, t0, "/w/a.json", IndentationAction::Convert { to_spaces: true });
    // The widget applies each edit and answers with the change it made.
    let mut dirty = false;
    for (n, (range, text)) in edits(&fx).into_iter().enumerate() {
        let fx = c.handle(t0, Input::Widget { uri: u.clone(), generation, event: WidgetEvent::CodeChanged { version: n as u32 + 1, range, text } });
        dirty |= updates(&fx).iter().any(|u| matches!(u, SessionUpdate::Dirty { dirty: true, .. }));
    }
    assert!(dirty, "the conversion is an edit: the document is dirty");
    let fx = c.handle(t0, Input::Widget { uri: u, generation, event: WidgetEvent::CodeSaveRequested { version: 2 } });
    match &fx[..] {
        [Effect::WriteFile { bytes, .. }] => assert_eq!(bytes, b"a\n    b\n        c\n"),
        other => panic!("{other:#?}"),
    }
}
