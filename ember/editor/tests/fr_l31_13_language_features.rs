//! FR-L31.13: language features through the pure session core (no server). Each test plays the
//! window and the extension host: it registers providers, asks for a feature, checks the call that
//! goes out, and feeds back the reply a provider would give.

use std::time::Instant;

use ember_editor::engine::{ReadPurpose, Token};
use ember_editor::language::{Feature, LanguageRequest, LanguageUpdate};
use ember_editor::languages::LanguageRegistry;
use ember_editor::{CoreOptions, Effect, Input, SessionCore, SessionUpdate, WidgetCommand, WidgetPos, WidgetRange};
use ember_editor_conn::exthost::Call;
use ember_editor_conn::rpc::{Arg, IncomingRequest, Reply};
use ember_editor_conn::uri::UriComponents;
use serde_json::{json, Value};

const AUTH: &str = "127.0.0.1:9";
const FEATURES: &str = "MainThreadLanguageFeatures";
const DOC: &str = "/w/a.json";

fn uri(path: &str) -> UriComponents {
    UriComponents::remote(AUTH, path)
}

fn uri_json(path: &str) -> Value {
    serde_json::to_value(uri(path)).unwrap()
}

fn p(line: u32, col: u32) -> WidgetPos {
    WidgetPos::new(line, col)
}

fn range(sl: u32, sc: u32, el: u32, ec: u32) -> WidgetRange {
    WidgetRange::new(p(sl, sc), p(el, ec))
}

fn irange(sl: u32, sc: u32, el: u32, ec: u32) -> Value {
    json!({"startLineNumber": sl, "startColumn": sc, "endLineNumber": el, "endColumn": ec})
}

fn selector() -> Value {
    json!([{"language": "json"}])
}

/// A core with `DOC` open holding `text`.
fn core_with(text: &str) -> (SessionCore, Instant) {
    let now = Instant::now();
    let langs = LanguageRegistry::from_extensions(&[json!({"contributes": {"languages": [{"id": "json", "extensions": [".json"]}]}})]);
    let mut core = SessionCore::new(langs, CoreOptions::default(), now);
    core.handle(now, Input::Open { uri: uri(DOC) });
    let fx = core.handle(now, Input::FileRead { uri: uri(DOC), purpose: ReadPurpose::Open, result: Ok(text.as_bytes().to_vec()) });
    assert!(fx.iter().any(|e| matches!(e, Effect::Ui(SessionUpdate::Opened { .. }))), "{fx:#?}");
    (core, now)
}

/// The extension host registers a provider: `method(handle, selector, ...rest)`.
fn register(core: &mut SessionCore, now: Instant, method: &str, handle: i64, rest: Vec<Value>) {
    let mut args = vec![json!(handle), selector()];
    args.extend(rest);
    let fx = core.handle(
        now,
        Input::ExtHostRequest(IncomingRequest {
            req: handle as u32,
            rpc_id: ember_editor_conn::rpc_ids::id_of(FEATURES).unwrap(),
            proxy: Some(FEATURES),
            method: method.into(),
            args: args.into_iter().map(Arg::Json).collect(),
            cancellable: false,
        }),
    );
    assert!(fx.iter().any(|e| matches!(e, Effect::Respond { result: Ok(Reply::Empty), .. })), "{method}: {fx:#?}");
}

fn ask(core: &mut SessionCore, now: Instant, request: LanguageRequest) -> Vec<Effect> {
    core.handle(now, Input::Language(request))
}

fn reply(core: &mut SessionCore, now: Instant, token: Token, value: Value) -> Vec<Effect> {
    core.handle(now, Input::RpcReply { token, result: Ok(value) })
}

fn calls<'a>(effects: &'a [Effect], method: &str) -> Vec<(Option<Token>, &'a Call)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Rpc { token, call } if call.method == method => Some((*token, call)),
            _ => None,
        })
        .collect()
}

/// The one call of `method`, with its reply token.
fn call<'a>(effects: &'a [Effect], method: &str) -> (Token, &'a Call) {
    let found = calls(effects, method);
    assert_eq!(found.len(), 1, "expected one {method} in {effects:#?}");
    (found[0].0.unwrap_or_else(|| panic!("{method} has no token")), found[0].1)
}

fn arg(c: &Call, i: usize) -> Value {
    c.args[i].as_json().cloned().unwrap_or(Value::Null)
}

fn language_updates(effects: &[Effect]) -> Vec<&LanguageUpdate> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Ui(SessionUpdate::Language(u)) => Some(u),
            _ => None,
        })
        .collect()
}

/// The `EditCode` commands for the widget, in the order sent: (base version, range, text).
fn widget_edits(effects: &[Effect]) -> Vec<(u32, WidgetRange, String)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Ui(SessionUpdate::Command { command: WidgetCommand::EditCode { base_version, range, text, .. }, .. }) => {
                Some((*base_version, *range, text.clone()))
            }
            _ => None,
        })
        .collect()
}

fn cancelled(effects: &[Effect]) -> Vec<Token> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Cancel(t) => Some(*t),
            _ => None,
        })
        .collect()
}

fn unavailable(effects: &[Effect], feature: Feature) -> bool {
    language_updates(effects).iter().any(|u| matches!(u, LanguageUpdate::Unavailable { feature: f, .. } if *f == feature))
}

// ---- completion ---------------------------------------------------------------------------------

fn completion_core() -> (SessionCore, Instant) {
    let (mut core, now) = core_with("al\n");
    register(&mut core, now, "$registerCompletionsProvider", 5, vec![json!(["."]), json!(true), json!({"value": "pub.ext"})]);
    (core, now)
}

fn suggest_reply() -> Value {
    json!({
        "a": {"insert": irange(1, 1, 1, 3), "replace": irange(1, 1, 1, 3)},
        "b": [{"a": "alpha", "x": [11, 0]}, {"a": "beta", "h": "beta($1)", "i": 4, "x": [11, 1]}],
        "d": 1,
        "x": 11
    })
}

#[test]
fn fr_l31_13_completion_asks_lists_resolves_inserts_and_releases() {
    let (mut core, now) = completion_core();
    let fx = ask(&mut core, now, LanguageRequest::Complete { uri: uri(DOC), pos: Some(p(0, 2)), trigger: None });
    let (token, c) = call(&fx, "$provideCompletionItems");
    assert!(c.cancellable);
    assert_eq!(arg(c, 0), json!(5));
    assert_eq!(arg(c, 1), uri_json(DOC));
    assert_eq!(arg(c, 2), json!({"lineNumber": 1, "column": 3}));
    assert_eq!(arg(c, 3), json!({"triggerKind": 0}));

    let fx = reply(&mut core, now, token, suggest_reply());
    match language_updates(&fx)[..] {
        [LanguageUpdate::Completions { version, pos, items, incomplete, complete, .. }] => {
            assert_eq!((*version, *pos, *incomplete, *complete), (0, p(0, 2), false, true));
            assert_eq!(items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(), ["alpha", "beta"]);
            assert_eq!(items[1].insert_range, range(0, 0, 0, 2));
        }
        ref other => panic!("{other:#?}"),
    }

    // The provider resolves details, so accepting asks for them first.
    let fx = ask(&mut core, now, LanguageRequest::AcceptCompletion { uri: uri(DOC), index: 1 });
    assert!(widget_edits(&fx).is_empty());
    let (token, c) = call(&fx, "$resolveCompletionItem");
    assert_eq!((arg(c, 0), arg(c, 1)), (json!(5), json!([11, 1])));

    let resolved = json!({"a": "beta", "h": "changed", "l": [{"range": irange(1, 1, 1, 1), "text": "import beta\n"}], "o": "ext.afterInsert", "p": [1]});
    let fx = reply(&mut core, now, token, resolved);
    // The snippet as plain text over the typed word (the insert text first given, not the resolved
    // one), then the additional edit; both against the version the list was made for.
    assert_eq!(widget_edits(&fx), vec![(0, range(0, 0, 0, 2), "beta()".to_string()), (0, range(0, 0, 0, 0), "import beta\n".to_string())]);
    let (_, c) = calls(&fx, "$releaseCompletionItems")[0];
    assert_eq!((arg(c, 0), arg(c, 1)), (json!(5), json!(11)));
    assert!(language_updates(&fx).iter().any(|u| matches!(u, LanguageUpdate::CompletionApplied { .. })));
    // The item's command is not registered yet: its extension is activated first.
    let (_, c) = calls(&fx, "$activateByEvent")[0];
    assert_eq!(arg(c, 0), json!("onCommand:ext.afterInsert"));

    // The session is over: a second accept does nothing.
    assert!(ask(&mut core, now, LanguageRequest::AcceptCompletion { uri: uri(DOC), index: 0 }).is_empty());
}

#[test]
fn fr_l31_13_completion_without_resolve_inserts_at_once() {
    let (mut core, now) = core_with("al\n");
    register(&mut core, now, "$registerCompletionsProvider", 5, vec![json!([]), json!(false), json!({"value": "pub.ext"})]);
    let fx = ask(&mut core, now, LanguageRequest::Complete { uri: uri(DOC), pos: Some(p(0, 2)), trigger: None });
    let (token, _) = call(&fx, "$provideCompletionItems");
    reply(&mut core, now, token, suggest_reply());
    let fx = ask(&mut core, now, LanguageRequest::AcceptCompletion { uri: uri(DOC), index: 0 });
    assert!(calls(&fx, "$resolveCompletionItem").is_empty());
    assert_eq!(widget_edits(&fx), vec![(0, range(0, 0, 0, 2), "alpha".to_string())]);
    assert_eq!(calls(&fx, "$releaseCompletionItems").len(), 1);
}

#[test]
fn fr_l31_13_completion_trigger_character_new_request_and_cancel() {
    let (mut core, now) = completion_core();
    // "(" is not a trigger character of the provider: nobody is asked.
    let fx = ask(&mut core, now, LanguageRequest::Complete { uri: uri(DOC), pos: Some(p(0, 2)), trigger: Some("(".into()) });
    assert!(calls(&fx, "$provideCompletionItems").is_empty());
    assert!(unavailable(&fx, Feature::Completion));

    let fx = ask(&mut core, now, LanguageRequest::Complete { uri: uri(DOC), pos: Some(p(0, 2)), trigger: Some(".".into()) });
    let (first, c) = call(&fx, "$provideCompletionItems");
    assert_eq!(arg(c, 3), json!({"triggerKind": 1, "triggerCharacter": "."}));

    // A new request cancels the one still outstanding.
    let fx = ask(&mut core, now, LanguageRequest::Complete { uri: uri(DOC), pos: Some(p(0, 1)), trigger: None });
    assert_eq!(cancelled(&fx), vec![first]);
    let (second, _) = call(&fx, "$provideCompletionItems");
    // The cancelled call's reply is not delivered; if one slips through it changes nothing.
    assert!(reply(&mut core, now, first, suggest_reply()).is_empty());

    reply(&mut core, now, second, suggest_reply());
    let fx = ask(&mut core, now, LanguageRequest::CancelCompletion { uri: uri(DOC) });
    let (_, c) = calls(&fx, "$releaseCompletionItems")[0];
    assert_eq!(arg(c, 1), json!(11));
    assert!(language_updates(&fx).is_empty());
}

#[test]
fn fr_l31_13_completion_merges_providers_and_reports_progress() {
    let (mut core, now) = completion_core();
    register(&mut core, now, "$registerCompletionsProvider", 6, vec![json!([]), json!(false), json!({"value": "pub.other"})]);
    let fx = ask(&mut core, now, LanguageRequest::Complete { uri: uri(DOC), pos: None, trigger: None });
    let asked = calls(&fx, "$provideCompletionItems");
    assert_eq!(asked.len(), 2);
    // No position given and no caret known yet: the start of the document.
    assert_eq!(arg(asked[0].1, 2), json!({"lineNumber": 1, "column": 1}));
    let tokens: Vec<Token> = asked.iter().map(|(t, _)| t.unwrap()).collect();

    let fx = reply(&mut core, now, tokens[0], json!({"b": [{"a": "one"}], "c": true}));
    assert!(matches!(language_updates(&fx)[..], [LanguageUpdate::Completions { complete: false, incomplete: true, items, .. }] if items.len() == 1));
    // A provider that fails still ends the wait.
    let fx = core.handle(now, Input::RpcReply { token: tokens[1], result: Err("boom".into()) });
    assert!(matches!(language_updates(&fx)[..], [LanguageUpdate::Completions { complete: true, items, .. }] if items.len() == 1));
}

// ---- definition and references ------------------------------------------------------------------

#[test]
fn fr_l31_13_definition_merges_providers_in_order_with_previews() {
    let (mut core, now) = core_with("{\n  \"name\": 1\n}\n");
    assert!(unavailable(&ask(&mut core, now, LanguageRequest::Definition { uri: uri(DOC), pos: Some(p(1, 3)) }), Feature::Definition));

    register(&mut core, now, "$registerDefinitionSupport", 1, vec![]);
    register(&mut core, now, "$registerDefinitionSupport", 2, vec![]);
    let fx = ask(&mut core, now, LanguageRequest::Definition { uri: uri(DOC), pos: Some(p(1, 3)) });
    let asked = calls(&fx, "$provideDefinition");
    // Equal scores: the provider registered last is asked first.
    assert_eq!(asked.iter().map(|(_, c)| arg(c, 0)).collect::<Vec<_>>(), [json!(2), json!(1)]);
    assert_eq!(arg(asked[0].1, 2), json!({"lineNumber": 2, "column": 4}));
    assert!(asked[0].1.cancellable);

    let here = json!({"uri": uri_json(DOC), "range": irange(2, 3, 2, 13), "targetSelectionRange": irange(2, 4, 2, 8)});
    let there = json!({"uri": uri_json("/w/other.json"), "range": irange(7, 1, 7, 5)});
    // The second provider answers first; the list still follows provider order.
    let fx = reply(&mut core, now, asked[1].0.unwrap(), json!([there, here]));
    assert!(language_updates(&fx).is_empty());
    let fx = reply(&mut core, now, asked[0].0.unwrap(), json!([here]));
    match language_updates(&fx)[..] {
        [LanguageUpdate::Locations { feature, origin, locations, .. }] => {
            assert_eq!((*feature, *origin), (Feature::Definition, p(1, 3)));
            assert_eq!(locations.iter().map(|l| l.uri.path.as_str()).collect::<Vec<_>>(), [DOC, "/w/other.json"]);
            assert_eq!(locations[0].target(), range(1, 3, 1, 7));
            // The open document gives its line; a file that is not open gives none.
            assert_eq!(locations[0].preview.as_deref(), Some("\"name\": 1"));
            assert_eq!(locations[1].preview, None);
        }
        ref other => panic!("{other:#?}"),
    }
}

#[test]
fn fr_l31_13_references_include_the_declaration_and_an_empty_answer_is_an_empty_list() {
    let (mut core, now) = core_with("{}\n");
    register(&mut core, now, "$registerReferenceSupport", 3, vec![]);
    let fx = ask(&mut core, now, LanguageRequest::References { uri: uri(DOC), pos: Some(p(0, 1)) });
    let (token, c) = call(&fx, "$provideReferences");
    assert_eq!(arg(c, 3), json!({"includeDeclaration": true}));
    let fx = reply(&mut core, now, token, Value::Null);
    assert!(matches!(language_updates(&fx)[..], [LanguageUpdate::Locations { feature: Feature::References, locations, .. }] if locations.is_empty()));
}

// ---- formatting ---------------------------------------------------------------------------------

#[test]
fn fr_l31_13_format_document_sends_the_edits_last_first() {
    let (mut core, now) = core_with("{\"a\":1,\n\"b\":2}\n");
    assert!(unavailable(&ask(&mut core, now, LanguageRequest::Format { uri: uri(DOC), range: None }), Feature::Format));

    register(&mut core, now, "$registerDocumentFormattingSupport", 4, vec![json!({"value": "pub.ext"}), json!("Formatter")]);
    let fx = ask(&mut core, now, LanguageRequest::Format { uri: uri(DOC), range: None });
    let (token, c) = call(&fx, "$provideDocumentFormattingEdits");
    assert_eq!((arg(c, 0), arg(c, 2)), (json!(4), json!({"tabSize": 4, "insertSpaces": true})));

    let edits = json!([{"range": irange(1, 6, 1, 6), "text": " "}, {"range": irange(2, 1, 2, 1), "text": "    "}]);
    let fx = reply(&mut core, now, token, edits);
    assert_eq!(widget_edits(&fx), vec![(0, range(1, 0, 1, 0), "    ".to_string()), (0, range(0, 5, 0, 5), " ".to_string())]);
    assert!(matches!(language_updates(&fx)[..], [LanguageUpdate::Formatted { edits: 2, .. }]));

    // Already formatted: no edits, and the window is told so.
    let fx = ask(&mut core, now, LanguageRequest::Format { uri: uri(DOC), range: None });
    let fx = reply(&mut core, now, call(&fx, "$provideDocumentFormattingEdits").0, Value::Null);
    assert!(widget_edits(&fx).is_empty());
    assert!(matches!(language_updates(&fx)[..], [LanguageUpdate::Formatted { edits: 0, .. }]));
}

#[test]
fn fr_l31_13_format_falls_back_to_the_range_formatter_and_formats_a_selection() {
    let (mut core, now) = core_with("ab\ncde");
    register(&mut core, now, "$registerRangeFormattingSupport", 4, vec![json!({"value": "pub.ext"}), json!("Formatter"), json!(false)]);
    // Only a range formatter: the whole document is one range.
    let fx = ask(&mut core, now, LanguageRequest::Format { uri: uri(DOC), range: None });
    let (_, c) = call(&fx, "$provideDocumentRangeFormattingEdits");
    assert_eq!(arg(c, 2), irange(1, 1, 2, 4));
    assert_eq!(arg(c, 3), json!({"tabSize": 4, "insertSpaces": true}));

    // A second request replaces the first, which is cancelled.
    let first = call(&fx, "$provideDocumentRangeFormattingEdits").0;
    let fx = ask(&mut core, now, LanguageRequest::Format { uri: uri(DOC), range: Some(range(1, 0, 1, 2)) });
    assert_eq!(cancelled(&fx), vec![first]);
    assert_eq!(arg(call(&fx, "$provideDocumentRangeFormattingEdits").1, 2), irange(2, 1, 2, 3));
}

// ---- rename -------------------------------------------------------------------------------------

#[test]
fn fr_l31_13_rename_prepares_with_the_provider_or_the_word() {
    let (mut core, now) = core_with("let total = 1;\n");
    assert!(unavailable(&ask(&mut core, now, LanguageRequest::PrepareRename { uri: uri(DOC), pos: Some(p(0, 5)) }), Feature::Rename));

    // A provider that cannot resolve the location: the word under the caret is offered.
    register(&mut core, now, "$registerRenameSupport", 7, vec![json!(false)]);
    let fx = ask(&mut core, now, LanguageRequest::PrepareRename { uri: uri(DOC), pos: Some(p(0, 5)) });
    assert!(calls(&fx, "$resolveRenameLocation").is_empty());
    assert!(matches!(language_updates(&fx)[..], [LanguageUpdate::RenameReady { range: r, placeholder, .. }] if *r == range(0, 4, 0, 9) && placeholder == "total"));
    // No word there: nothing to rename.
    let fx = ask(&mut core, now, LanguageRequest::PrepareRename { uri: uri(DOC), pos: Some(p(0, 11)) });
    assert!(matches!(language_updates(&fx)[..], [LanguageUpdate::RenameRejected { reason: None, .. }]));

    // A provider that resolves is asked; it is newer, so it is the one used.
    register(&mut core, now, "$registerRenameSupport", 8, vec![json!(true)]);
    let fx = ask(&mut core, now, LanguageRequest::PrepareRename { uri: uri(DOC), pos: Some(p(0, 5)) });
    let (token, c) = call(&fx, "$resolveRenameLocation");
    assert_eq!((arg(c, 0), arg(c, 2)), (json!(8), json!({"lineNumber": 1, "column": 6})));
    let fx = reply(&mut core, now, token, json!({"range": irange(1, 5, 1, 10), "text": "total"}));
    assert!(matches!(language_updates(&fx)[..], [LanguageUpdate::RenameReady { pos, range: r, placeholder, .. }] if *pos == p(0, 5) && *r == range(0, 4, 0, 9) && placeholder == "total"));

    let fx = ask(&mut core, now, LanguageRequest::PrepareRename { uri: uri(DOC), pos: Some(p(0, 1)) });
    let fx = reply(&mut core, now, call(&fx, "$resolveRenameLocation").0, json!({"rejectReason": "You cannot rename a keyword."}));
    assert!(matches!(language_updates(&fx)[..], [LanguageUpdate::RenameRejected { reason: Some(reason), .. }] if reason == "You cannot rename a keyword."));

    // No opinion from the provider: the word again.
    let fx = ask(&mut core, now, LanguageRequest::PrepareRename { uri: uri(DOC), pos: Some(p(0, 5)) });
    let fx = reply(&mut core, now, call(&fx, "$resolveRenameLocation").0, Value::Null);
    assert!(matches!(language_updates(&fx)[..], [LanguageUpdate::RenameReady { placeholder, .. }] if placeholder == "total"));
}

#[test]
fn fr_l31_13_rename_edits_the_open_document_and_writes_the_files_that_are_not_open() {
    let (mut core, now) = core_with("let total = 1;\n");
    register(&mut core, now, "$registerRenameSupport", 7, vec![json!(false)]);
    let fx = ask(&mut core, now, LanguageRequest::Rename { uri: uri(DOC), pos: p(0, 5), new_name: "sum".into() });
    let (token, c) = call(&fx, "$provideRenameEdits");
    assert_eq!((arg(c, 2), arg(c, 3)), (json!({"lineNumber": 1, "column": 6}), json!("sum")));

    let other = "/w/other.json";
    let edit = json!({"edits": [
        {"resource": uri_json(DOC), "textEdit": {"range": irange(1, 5, 1, 10), "text": "sum"}, "versionId": 1},
        {"resource": uri_json(other), "textEdit": {"range": irange(1, 7, 1, 12), "text": "sum"}},
        {"oldResource": uri_json(other), "newResource": uri_json("/w/renamed.json")}
    ]});
    let fx = reply(&mut core, now, token, edit);
    assert_eq!(widget_edits(&fx), vec![(0, range(0, 4, 0, 9), "sum".to_string())]);
    // The other file is not open: it is read, and nothing is reported until it is written.
    let id = fx
        .iter()
        .find_map(|e| match e {
            Effect::ReadFile { uri: u, purpose: ReadPurpose::WorkspaceEdit { id } } if u.path == other => Some(*id),
            _ => None,
        })
        .expect("a read of the other file");
    assert!(language_updates(&fx).is_empty());

    let fx = core.handle(now, Input::FileRead { uri: uri(other), purpose: ReadPurpose::WorkspaceEdit { id }, result: Ok(b"print(total)\r\n".to_vec()) });
    let (bytes, write) = fx
        .iter()
        .find_map(|e| match e {
            Effect::WriteFile { uri: u, bytes, token } if u.path == other => Some((bytes.clone(), *token)),
            _ => None,
        })
        .expect("a write of the other file");
    assert_eq!(bytes, b"print(sum)\r\n".to_vec());

    let fx = core.handle(now, Input::FileWritten { uri: uri(other), token: write, result: Ok(()) });
    match language_updates(&fx)[..] {
        [LanguageUpdate::WorkspaceEditApplied { feature, summary }] => {
            assert_eq!(*feature, Feature::Rename);
            assert_eq!((summary.files, summary.edits, summary.unsupported), (2, 2, 1));
            assert!(summary.failures.is_empty());
        }
        ref other => panic!("{other:#?}"),
    }
}

#[test]
fn fr_l31_13_rename_reports_rejection_and_files_that_fail() {
    let (mut core, now) = core_with("let total = 1;\n");
    register(&mut core, now, "$registerRenameSupport", 7, vec![json!(false)]);
    let fx = ask(&mut core, now, LanguageRequest::Rename { uri: uri(DOC), pos: p(0, 5), new_name: "1x".into() });
    let fx = reply(&mut core, now, call(&fx, "$provideRenameEdits").0, json!({"edits": [], "rejectReason": "Not a valid identifier."}));
    assert!(widget_edits(&fx).is_empty());
    assert!(matches!(language_updates(&fx)[..], [LanguageUpdate::RenameRejected { reason: Some(reason), .. }] if reason == "Not a valid identifier."));

    let fx = ask(&mut core, now, LanguageRequest::Rename { uri: uri(DOC), pos: p(0, 5), new_name: "sum".into() });
    let edit = json!({"edits": [{"resource": uri_json("/w/gone.json"), "textEdit": {"range": irange(1, 1, 1, 2), "text": "x"}}]});
    let fx = reply(&mut core, now, call(&fx, "$provideRenameEdits").0, edit);
    let id = fx.iter().find_map(|e| match e { Effect::ReadFile { purpose: ReadPurpose::WorkspaceEdit { id }, .. } => Some(*id), _ => None }).unwrap();
    let fx = core.handle(now, Input::FileRead { uri: uri("/w/gone.json"), purpose: ReadPurpose::WorkspaceEdit { id }, result: Err("ENOENT".into()) });
    match language_updates(&fx)[..] {
        [LanguageUpdate::WorkspaceEditApplied { summary, .. }] => {
            assert_eq!((summary.files, summary.edits), (0, 0));
            assert_eq!(summary.failures, vec![("/w/gone.json".to_string(), "ENOENT".to_string())]);
        }
        ref other => panic!("{other:#?}"),
    }
}

// ---- code actions -------------------------------------------------------------------------------

fn action_list() -> Value {
    json!({"cacheId": 21, "actions": [
        {"cacheId": [21, 0], "title": "Add missing comma", "kind": "quickfix", "isPreferred": true,
         "edit": {"edits": [{"resource": uri_json(DOC), "textEdit": {"range": irange(1, 3, 1, 3), "text": ","}}]}},
        {"cacheId": [21, 1], "title": "Sort keys", "kind": "source.sort"},
        {"cacheId": [21, 2], "title": "Extract", "kind": "refactor.extract", "disabled": "Select a value"}
    ]})
}

#[test]
fn fr_l31_13_code_actions_list_apply_an_edit_and_release() {
    let (mut core, now) = core_with("{}\n");
    assert!(unavailable(&ask(&mut core, now, LanguageRequest::CodeActions { uri: uri(DOC), range: None }), Feature::CodeActions));

    register(&mut core, now, "$registerCodeActionSupport", 9, vec![json!({"providedKinds": ["quickfix"]}), json!("Ext"), json!("pub.ext"), json!(true)]);
    let fx = ask(&mut core, now, LanguageRequest::CodeActions { uri: uri(DOC), range: Some(range(0, 1, 0, 2)) });
    let (token, c) = call(&fx, "$provideCodeActions");
    assert_eq!((arg(c, 0), arg(c, 2), arg(c, 3)), (json!(9), irange(1, 2, 1, 3), json!({"trigger": 1})));

    let fx = reply(&mut core, now, token, action_list());
    match language_updates(&fx)[..] {
        [LanguageUpdate::CodeActions { actions, complete, .. }] => {
            assert!(*complete);
            assert_eq!(actions.iter().map(|a| a.title.as_str()).collect::<Vec<_>>(), ["Add missing comma", "Sort keys", "Extract"]);
            assert!(actions[0].preferred && actions[2].disabled.is_some());
        }
        ref other => panic!("{other:#?}"),
    }

    // A disabled action is not run and the list stays.
    assert!(ask(&mut core, now, LanguageRequest::ApplyCodeAction { uri: uri(DOC), index: 2 }).is_empty());

    // An action that came with its edit is applied without resolving.
    let fx = ask(&mut core, now, LanguageRequest::ApplyCodeAction { uri: uri(DOC), index: 0 });
    assert!(calls(&fx, "$resolveCodeAction").is_empty());
    assert_eq!(widget_edits(&fx), vec![(0, range(0, 2, 0, 2), ",".to_string())]);
    let (_, c) = calls(&fx, "$releaseCodeActions")[0];
    assert_eq!((arg(c, 0), arg(c, 1)), (json!(9), json!(21)));
    assert!(matches!(language_updates(&fx)[..], [LanguageUpdate::WorkspaceEditApplied { feature: Feature::CodeActions, summary }] if summary.files == 1 && summary.edits == 1));
}

#[test]
fn fr_l31_13_code_action_without_an_edit_is_resolved_then_its_command_runs() {
    let (mut core, now) = core_with("{}\n");
    register(&mut core, now, "$registerCodeActionSupport", 9, vec![json!({}), json!("Ext"), json!("pub.ext"), json!(true)]);
    let fx = ask(&mut core, now, LanguageRequest::CodeActions { uri: uri(DOC), range: None });
    // No range given: the caret, as an empty range.
    assert_eq!(arg(call(&fx, "$provideCodeActions").1, 2), irange(1, 1, 1, 1));
    reply(&mut core, now, call(&fx, "$provideCodeActions").0, action_list());

    let fx = ask(&mut core, now, LanguageRequest::ApplyCodeAction { uri: uri(DOC), index: 1 });
    let (token, c) = call(&fx, "$resolveCodeAction");
    assert_eq!((arg(c, 0), arg(c, 1)), (json!(9), json!([21, 1])));
    assert!(calls(&fx, "$releaseCodeActions").is_empty());

    let resolved = json!({
        "edit": {"edits": [{"resource": uri_json(DOC), "textEdit": {"range": irange(1, 1, 1, 3), "text": "{ }"}}]},
        "command": {"id": "ext.sorted", "title": "Sorted", "arguments": ["a"]}
    });
    let fx = reply(&mut core, now, token, resolved);
    assert_eq!(widget_edits(&fx), vec![(0, range(0, 0, 0, 2), "{ }".to_string())]);
    assert_eq!(calls(&fx, "$releaseCodeActions").len(), 1);
    assert_eq!(arg(calls(&fx, "$activateByEvent")[0].1, 0), json!("onCommand:ext.sorted"));

    // Dismissing a list releases it too.
    let fx = ask(&mut core, now, LanguageRequest::CodeActions { uri: uri(DOC), range: None });
    reply(&mut core, now, call(&fx, "$provideCodeActions").0, action_list());
    let fx = ask(&mut core, now, LanguageRequest::DismissCodeActions { uri: uri(DOC) });
    assert_eq!(calls(&fx, "$releaseCodeActions").len(), 1);
}

#[test]
fn fr_l31_13_an_extension_apply_edit_is_applied_and_answered_true() {
    let (mut core, now) = core_with("{}\n");
    let dto = json!({"edits": [{"resource": uri_json(DOC), "textEdit": {"range": irange(1, 2, 1, 2), "text": "\"a\": 1"}}]});
    let fx = core.handle(
        now,
        Input::ExtHostRequest(IncomingRequest {
            req: 77,
            rpc_id: ember_editor_conn::rpc_ids::id_of("MainThreadBulkEdits").unwrap(),
            proxy: Some("MainThreadBulkEdits"),
            method: "$tryApplyWorkspaceEdit".into(),
            args: vec![Arg::Json(dto), Arg::Undefined, Arg::Json(json!(false))],
            cancellable: false,
        }),
    );
    assert_eq!(widget_edits(&fx), vec![(0, range(0, 1, 0, 1), "\"a\": 1".to_string())]);
    assert!(fx.contains(&Effect::Respond { req: 77, result: Ok(Reply::Json(json!(true))) }), "{fx:#?}");
}

// ---- signature help -----------------------------------------------------------------------------

#[test]
fn fr_l31_13_signature_help_takes_the_first_provider_that_answers_and_releases_the_old_hint() {
    let (mut core, now) = core_with("f(1, 2)\n");
    assert!(unavailable(&ask(&mut core, now, LanguageRequest::SignatureHelp { uri: uri(DOC), pos: Some(p(0, 2)), trigger: None }), Feature::SignatureHelp));

    let meta = json!({"triggerCharacters": ["(", ","], "retriggerCharacters": [")"]});
    register(&mut core, now, "$registerSignatureHelpProvider", 1, vec![meta.clone()]);
    register(&mut core, now, "$registerSignatureHelpProvider", 2, vec![meta]);
    let fx = ask(&mut core, now, LanguageRequest::SignatureHelp { uri: uri(DOC), pos: Some(p(0, 2)), trigger: Some("(".into()) });
    let (token, c) = call(&fx, "$provideSignatureHelp");
    assert_eq!((arg(c, 0), arg(c, 2)), (json!(2), json!({"lineNumber": 1, "column": 3})));
    assert_eq!(arg(c, 3), json!({"triggerKind": 2, "triggerCharacter": "(", "isRetrigger": false}));

    // The first provider has nothing: the next one is asked, and nothing is shown yet.
    let fx = reply(&mut core, now, token, Value::Null);
    assert!(language_updates(&fx).is_empty());
    let (token, c) = call(&fx, "$provideSignatureHelp");
    assert_eq!(arg(c, 0), json!(1));

    let help = json!({"id": 31, "activeSignature": 0, "activeParameter": 0, "signatures": [{"label": "f(a, b)", "parameters": [{"label": "a"}, {"label": "b"}]}]});
    let fx = reply(&mut core, now, token, help.clone());
    match language_updates(&fx)[..] {
        [LanguageUpdate::SignatureHelp { help: Some(shown), .. }] => {
            assert_eq!(shown.active().unwrap().0.label, "f(a, b)");
            assert_eq!(shown.active().unwrap().1, Some(0));
        }
        ref other => panic!("{other:#?}"),
    }

    // Asking again while a hint is shown is a retrigger that carries it.
    let fx = ask(&mut core, now, LanguageRequest::SignatureHelp { uri: uri(DOC), pos: Some(p(0, 5)), trigger: Some(",".into()) });
    let (token, c) = call(&fx, "$provideSignatureHelp");
    assert_eq!(arg(c, 3), json!({"triggerKind": 2, "triggerCharacter": ",", "isRetrigger": true, "activeSignatureHelp": help}));
    // Provider 2 answers this time: the hint of provider 1 is released.
    let next = json!({"id": 32, "activeSignature": 0, "activeParameter": 1, "signatures": [{"label": "f(a, b)", "parameters": [{"label": "a"}, {"label": "b"}]}]});
    let fx = reply(&mut core, now, token, next);
    let (_, c) = calls(&fx, "$releaseSignatureHelp")[0];
    assert_eq!((arg(c, 0), arg(c, 1)), (json!(1), json!(31)));

    let fx = ask(&mut core, now, LanguageRequest::DismissSignatureHelp { uri: uri(DOC) });
    let (_, c) = calls(&fx, "$releaseSignatureHelp")[0];
    assert_eq!((arg(c, 0), arg(c, 1)), (json!(2), json!(32)));
    assert!(language_updates(&fx).is_empty());
}

#[test]
fn fr_l31_13_signature_help_hides_when_no_provider_answers() {
    let (mut core, now) = core_with("f()\n");
    register(&mut core, now, "$registerSignatureHelpProvider", 1, vec![json!({"triggerCharacters": ["("], "retriggerCharacters": []})]);
    let fx = ask(&mut core, now, LanguageRequest::SignatureHelp { uri: uri(DOC), pos: Some(p(0, 2)), trigger: None });
    let (token, c) = call(&fx, "$provideSignatureHelp");
    assert_eq!(arg(c, 3), json!({"triggerKind": 1, "isRetrigger": false}));
    let fx = reply(&mut core, now, token, Value::Null);
    assert!(matches!(language_updates(&fx)[..], [LanguageUpdate::SignatureHelp { help: None, .. }]));
}

// ---- document symbols ---------------------------------------------------------------------------

#[test]
fn fr_l31_13_document_symbols_come_back_with_the_version_asked_for() {
    let (mut core, now) = core_with("{\n  \"a\": {\n    \"b\": 1\n  }\n}\n");
    assert!(unavailable(&ask(&mut core, now, LanguageRequest::DocumentSymbols { uri: uri(DOC) }), Feature::DocumentSymbols));

    register(&mut core, now, "$registerDocumentSymbolProvider", 12, vec![json!("JSON")]);
    let fx = ask(&mut core, now, LanguageRequest::DocumentSymbols { uri: uri(DOC) });
    let (token, c) = call(&fx, "$provideDocumentSymbols");
    assert_eq!((arg(c, 0), arg(c, 1), c.args.len()), (json!(12), uri_json(DOC), 2));

    let symbols = json!([{"name": "a", "detail": "", "kind": 18, "tags": [], "range": irange(2, 3, 4, 4), "selectionRange": irange(2, 3, 2, 6),
        "children": [{"name": "b", "detail": "1", "kind": 15, "tags": [], "range": irange(3, 5, 3, 11), "selectionRange": irange(3, 5, 3, 8)}]}]);
    let fx = reply(&mut core, now, token, symbols);
    match language_updates(&fx)[..] {
        [LanguageUpdate::DocumentSymbols { version, symbols, .. }] => {
            assert_eq!(*version, 0);
            let path = ember_editor::language::navigation::symbol_path(symbols, p(2, 6));
            assert_eq!(path.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        }
        ref other => panic!("{other:#?}"),
    }
}

// ---- lifetime -----------------------------------------------------------------------------------

#[test]
fn fr_l31_13_closing_the_document_cancels_and_releases_what_was_asked_for_it() {
    let (mut core, now) = completion_core();
    register(&mut core, now, "$registerDefinitionSupport", 1, vec![]);
    let fx = ask(&mut core, now, LanguageRequest::Complete { uri: uri(DOC), pos: Some(p(0, 2)), trigger: None });
    reply(&mut core, now, call(&fx, "$provideCompletionItems").0, suggest_reply());
    let fx = ask(&mut core, now, LanguageRequest::Definition { uri: uri(DOC), pos: Some(p(0, 0)) });
    let definition = call(&fx, "$provideDefinition").0;

    let fx = core.handle(now, Input::Close { uri: uri(DOC) });
    assert_eq!(calls(&fx, "$releaseCompletionItems").len(), 1);
    assert!(cancelled(&fx).contains(&definition));
    // A request for a document that is not open is dropped.
    assert!(ask(&mut core, now, LanguageRequest::Definition { uri: uri(DOC), pos: None }).is_empty());
}
