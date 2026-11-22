//! End-to-end flows through the pure session core (no server): widget events in, RPC calls / file
//! effects / UI updates out. Each test plays the part of the driver: it executes nothing, it only
//! feeds back the replies a server would give.

use std::time::{Duration, Instant};

use ember_editor::engine::{ReadPurpose, Token};
use ember_editor::{
    CoreOptions, DecorationKind, Effect, ExternalChange, HoverPhase, Input, SessionCore, SessionUpdate, Severity, WidgetCommand,
    WidgetEvent, WidgetPos, WidgetRange,
};
use ember_editor::languages::LanguageRegistry;
use ember_editor_conn::exthost::Call;
use ember_editor_conn::remote_fs::{FileChange, FileChangeType};
use ember_editor_conn::rpc::{Arg, IncomingRequest, Reply};
use ember_editor_conn::uri::UriComponents;
use serde_json::{json, Value};

const AUTH: &str = "127.0.0.1:9";

fn core(t0: Instant) -> SessionCore {
    let langs = LanguageRegistry::from_extensions(&[json!({"contributes": {"languages": [{"id": "json", "extensions": [".json"]}]}})]);
    SessionCore::new(langs, CoreOptions::default(), t0)
}

fn uri(path: &str) -> UriComponents {
    UriComponents::remote(AUTH, path)
}

fn p(line: u32, col: u32) -> WidgetPos {
    WidgetPos::new(line, col)
}

fn request(req: u32, proxy: &'static str, method: &str, args: Vec<Value>) -> Input {
    Input::ExtHostRequest(IncomingRequest {
        req,
        rpc_id: ember_editor_conn::rpc_ids::id_of(proxy).unwrap(),
        proxy: Some(proxy),
        method: method.into(),
        args: args.into_iter().map(Arg::Json).collect(),
        cancellable: false,
    })
}

fn calls(effects: &[Effect]) -> Vec<(Option<Token>, &Call)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Rpc { token, call } => Some((*token, call)),
            _ => None,
        })
        .collect()
}

fn methods(effects: &[Effect]) -> Vec<&str> {
    calls(effects).iter().map(|(_, c)| c.method).collect()
}

fn call<'a>(effects: &'a [Effect], method: &str) -> (Option<Token>, &'a Call) {
    calls(effects).into_iter().find(|(_, c)| c.method == method).unwrap_or_else(|| panic!("no {method} in {effects:#?}"))
}

fn arg(c: &Call, i: usize) -> Value {
    c.args[i].as_json().cloned().unwrap_or(Value::Null)
}

fn updates(effects: &[Effect]) -> Vec<&SessionUpdate> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Ui(u) => Some(u),
            _ => None,
        })
        .collect()
}

fn decorations(effects: &[Effect]) -> Vec<ember_editor::Decoration> {
    updates(effects)
        .into_iter()
        .rev()
        .find_map(|u| match u {
            SessionUpdate::Decorations { decorations, .. } => Some(decorations.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no Decorations in {effects:#?}"))
}

fn changed(version: u32, at: WidgetPos, text: &str) -> WidgetEvent {
    WidgetEvent::CodeChanged { version, range: WidgetRange::empty(at), text: text.into() }
}

/// Open `path` with `text`; returns the generation.
fn open(core: &mut SessionCore, now: Instant, path: &str, text: &str) -> (Vec<Effect>, u64) {
    let u = uri(path);
    let fx = core.handle(now, Input::Open { uri: u.clone() });
    assert_eq!(fx, vec![Effect::ReadFile { uri: u.clone(), purpose: ReadPurpose::Open }]);
    let fx = core.handle(now, Input::FileRead { uri: u, purpose: ReadPurpose::Open, result: Ok(text.as_bytes().to_vec()) });
    let generation = updates(&fx)
        .into_iter()
        .find_map(|u| match u {
            SessionUpdate::Opened { generation, .. } => Some(*generation),
            _ => None,
        })
        .expect("Opened");
    (fx, generation)
}

#[test]
fn open_edit_save_reload_keeps_exthost_versions_monotonic() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let u = uri("/w/data.json");
    let (fx, g1) = open(&mut c, t0, "/w/data.json", "{\r\n  \"ok\": true\r\n}\r\n");
    assert_eq!(methods(&fx), vec!["$activateByEvent", "$acceptDocumentsAndEditorsDelta", "$acceptEditorTabModel"]);
    assert_eq!(arg(call(&fx, "$activateByEvent").1, 0), json!("onLanguage:json"));
    let delta = arg(call(&fx, "$acceptDocumentsAndEditorsDelta").1, 0);
    assert_eq!(delta["addedDocuments"][0]["versionId"], 1);
    assert_eq!(delta["addedDocuments"][0]["EOL"], "\r\n");
    assert!(fx.contains(&Effect::Watch { uri: u.clone() }));
    // The widget gets `\n` text.
    assert!(updates(&fx).iter().any(|x| matches!(x, SessionUpdate::Opened { text, language_id, tab_width: 4, .. }
        if text == "{\n  \"ok\": true\n}\n" && language_id == "json")));

    // Two keystrokes → ext-host versions 2, 3; the first one makes the document dirty.
    let fx = c.handle(t0, Input::Widget { uri: u.clone(), generation: g1, event: changed(1, p(1, 12), ",") });
    let mc = call(&fx, "$acceptModelChanged").1;
    assert_eq!(arg(mc, 1)["versionId"], 2);
    assert_eq!(arg(mc, 1)["changes"][0]["text"], ",");
    assert!(updates(&fx).contains(&&SessionUpdate::Dirty { uri: u.clone(), dirty: true }));
    let fx = c.handle(t0, Input::Widget { uri: u.clone(), generation: g1, event: changed(2, p(1, 13), "\n") });
    let mc = call(&fx, "$acceptModelChanged").1;
    assert_eq!(arg(mc, 1)["versionId"], 3);
    assert_eq!(arg(mc, 1)["changes"][0]["text"], "\r\n", "inserted line breaks use the file's EOL");

    // A lost event is reported, not applied.
    let fx = c.handle(t0, Input::Widget { uri: u.clone(), generation: g1, event: changed(9, p(0, 0), "x") });
    assert!(methods(&fx).is_empty());
    assert!(matches!(updates(&fx)[0], SessionUpdate::Desync { .. }));

    // External change while dirty → ask the UI, change nothing.
    let fx = c.handle(t0, Input::FilesChanged(vec![FileChange { kind: FileChangeType::Updated, resource: u.clone(), correlation_id: None }]));
    assert_eq!(fx, vec![Effect::ReadFile { uri: u.clone(), purpose: ReadPurpose::Reload }]);
    let fx = c.handle(t0, Input::FileRead { uri: u.clone(), purpose: ReadPurpose::Reload, result: Ok(b"{}".to_vec()) });
    assert_eq!(updates(&fx), vec![&SessionUpdate::ExternalChange { uri: u.clone(), change: ExternalChange::ChangedWhileDirty }]);

    // Save: write the file with its own EOL, then $acceptModelSaved, clean.
    let fx = c.handle(t0, Input::Widget { uri: u.clone(), generation: g1, event: WidgetEvent::CodeSaveRequested { version: 2 } });
    let (bytes, token) = match &fx[..] {
        [Effect::WriteFile { bytes, token, .. }] => (bytes.clone(), *token),
        other => panic!("{other:#?}"),
    };
    assert_eq!(String::from_utf8(bytes).unwrap(), "{\r\n  \"ok\": true,\r\n\r\n}\r\n");
    let fx = c.handle(t0, Input::FileWritten { uri: u.clone(), token, result: Ok(()) });
    assert!(methods(&fx).contains(&"$acceptModelSaved"));
    assert!(updates(&fx).contains(&&SessionUpdate::Saved { uri: u.clone() }));
    assert!(updates(&fx).contains(&&SessionUpdate::Dirty { uri: u.clone(), dirty: false }));

    // Our own save echoing through the watcher is ignored.
    let fx = c.handle(
        t0,
        Input::FileRead { uri: u.clone(), purpose: ReadPurpose::Reload, result: Ok(b"{\r\n  \"ok\": true,\r\n\r\n}\r\n".to_vec()) },
    );
    assert!(fx.is_empty(), "{fx:#?}");

    // A real external change on a clean buffer → reset: remove + re-add at version 4, new generation.
    let fx = c.handle(t0, Input::FileRead { uri: u.clone(), purpose: ReadPurpose::Reload, result: Ok(b"[1]\n".to_vec()) });
    let deltas: Vec<Value> = calls(&fx)
        .iter()
        .filter(|(_, c)| c.method == "$acceptDocumentsAndEditorsDelta")
        .map(|(_, c)| arg(c, 0))
        .collect();
    assert_eq!(deltas[0]["removedDocuments"][0]["path"], "/w/data.json");
    assert_eq!(deltas[1]["addedDocuments"][0]["versionId"], 4);
    let g2 = updates(&fx)
        .iter()
        .find_map(|x| match x {
            SessionUpdate::Command { command: WidgetCommand::SetText { generation, text }, .. } => {
                assert_eq!(text, "[1]\n");
                Some(*generation)
            }
            _ => None,
        })
        .expect("SetText");
    assert_ne!(g2, g1);

    // Late events from the old node are dropped; the new node restarts at version 1 → ext 5.
    let fx = c.handle(t0, Input::Widget { uri: u.clone(), generation: g1, event: changed(3, p(0, 0), "x") });
    assert!(fx.is_empty());
    let fx = c.handle(t0, Input::Widget { uri: u.clone(), generation: g2, event: changed(1, p(0, 1), "0,") });
    assert_eq!(arg(call(&fx, "$acceptModelChanged").1, 1)["versionId"], 5);

    // Close: remove from the extension host, stop watching.
    let fx = c.handle(t0, Input::Close { uri: u.clone() });
    assert_eq!(arg(call(&fx, "$acceptDocumentsAndEditorsDelta").1, 0)["removedDocuments"][0]["path"], "/w/data.json");
    assert!(fx.contains(&Effect::Unwatch { uri: u.clone() }));
    assert!(updates(&fx).contains(&&SessionUpdate::Closed { uri: u }));
}

fn marker(sev: u8, msg: &str, l: u32, c1: u32, c2: u32) -> Value {
    json!({"severity": sev, "message": msg, "source": "json", "startLineNumber": l, "startColumn": c1, "endLineNumber": l, "endColumn": c2})
}

#[test]
fn diagnostics_become_underlines_that_follow_edits() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let u = uri("/w/data.json");
    let (_, g) = open(&mut c, t0, "/w/data.json", "{\n  \"ok\": true,,\n}\n");
    let fx = c.handle(t0, request(1, "MainThreadDiagnostics", "$changeMany", vec![json!("json"), json!([[u, [marker(8, "Value expected", 2, 14, 15)]]])]));
    assert!(fx.contains(&Effect::Respond { req: 1, result: Ok(Reply::Empty) }));
    let d = decorations(&fx);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].kind, DecorationKind::Underline);
    assert_eq!(d[0].severity, Some(Severity::Error));
    assert_eq!(d[0].range, WidgetRange::new(p(1, 13), p(1, 14)));
    let id = d[0].id;

    // Insert a line at the top: the squiggle moves down with the same id, at the new version.
    let fx = c.handle(t0, Input::Widget { uri: u.clone(), generation: g, event: changed(1, p(0, 0), "\n") });
    let d = decorations(&fx);
    assert_eq!((d[0].id, d[0].version, d[0].range), (id, 1, WidgetRange::new(p(2, 13), p(2, 14))));

    // $clear removes it.
    let fx = c.handle(t0, request(2, "MainThreadDiagnostics", "$clear", vec![json!("json")]));
    assert!(decorations(&fx).is_empty());
}

#[test]
fn hover_merges_diagnostics_and_provider_markdown_and_cancels() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let u = uri("/w/data.json");
    let (_, g) = open(&mut c, t0, "/w/data.json", "{\n  \"ok\": true,,\n}\n");
    c.handle(t0, request(1, "MainThreadLanguageFeatures", "$registerHoverProvider", vec![json!(4), json!([{"language": "json"}])]));
    c.handle(t0, request(2, "MainThreadLanguageFeatures", "$registerHoverProvider", vec![json!(5), json!([{"language": "rust"}])]));
    c.handle(t0, request(3, "MainThreadDiagnostics", "$changeMany", vec![json!("json"), json!([[u, [marker(8, "Value expected", 2, 14, 15)]]])]));

    let rest = |pos| Input::Widget { uri: u.clone(), generation: g, event: WidgetEvent::CodeHovered { decoration: 0, pos, phase: HoverPhase::Rest } };
    let fx = c.handle(t0, rest(p(1, 13)));
    let hovers = calls(&fx);
    assert_eq!(hovers.len(), 1, "only the json provider matches");
    let (t1, hc) = (hovers[0].0.unwrap(), hovers[0].1);
    assert_eq!(hc.method, "$provideHover");
    assert_eq!(arg(hc, 0), json!(4));
    assert_eq!(arg(hc, 2), json!({"lineNumber": 2, "column": 14}));
    assert!(hc.cancellable);
    match updates(&fx)[0] {
        SessionUpdate::Hover(h) => {
            assert_eq!(h.diagnostics[0].message, "Value expected");
            assert!(!h.complete);
        }
        other => panic!("{other:?}"),
    }
    let fx = c.handle(t0, Input::RpcReply { token: t1, result: Ok(json!({"id": 1, "contents": [{"value": "**ok**: boolean"}]})) });
    match updates(&fx)[0] {
        SessionUpdate::Hover(h) => {
            assert!(h.complete);
            assert_eq!(h.contents[0].value, "**ok**: boolean");
        }
        other => panic!("{other:?}"),
    }

    // A new rest cancels the outstanding request; its late reply is ignored.
    let fx = c.handle(t0, rest(p(0, 0)));
    let t2 = calls(&fx)[0].0.unwrap();
    let fx = c.handle(t0, rest(p(2, 0)));
    assert!(fx.contains(&Effect::Cancel(t2)));
    assert!(c.handle(t0, Input::RpcReply { token: t2, result: Ok(json!({"contents": [{"value": "late"}]})) }).is_empty());

    // Typing hides the hover.
    let t3 = calls(&fx)[0].0.unwrap();
    c.handle(t0, Input::RpcReply { token: t3, result: Ok(json!({"contents": [{"value": "x"}]})) });
    let fx = c.handle(t0, Input::Widget { uri: u.clone(), generation: g, event: changed(1, p(0, 1), " ") });
    assert!(updates(&fx).contains(&&SessionUpdate::HoverHidden { uri: u }));
}

#[test]
fn selection_and_visible_range_events_drive_ghost_cancellation_and_codelens_priority() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let u = uri("/w/data.json");
    let (_, g) = open(&mut c, t0, "/w/data.json", "fo\nbar\nbaz\n");
    c.handle(t0, request(1, "MainThreadLanguageFeatures", "$registerInlineCompletionsSupport", inline_registration(9, 0)));
    let w = |e| Input::Widget { uri: u.clone(), generation: g, event: e };

    // Typing triggers ghost text at (0, 3).
    let fx = c.handle(t0, w(changed(1, p(0, 2), "o")));
    let (t, _) = call(&fx, "$provideInlineCompletions");
    let fx = c.handle(t0, Input::RpcReply { token: t.unwrap(), result: Ok(json!({"pid": 1, "items": [{"insertText": "()", "idx": 0}]})) });
    assert!(decorations(&fx).iter().any(|d| d.kind == DecorationKind::GhostText));

    // Moving selection away from (0, 3) ends the ghost text as Ignored(userTypingDisagreed: false).
    let fx = c.handle(t0, w(WidgetEvent::SelectionChanged { version: 1, anchor: p(1, 0), active: p(1, 0) }));
    assert!(!decorations(&fx).iter().any(|d| d.kind == DecorationKind::GhostText));
    let end = call(&fx, "$handleInlineCompletionEndOfLifetime").1;
    assert_eq!(arg(end, 3), json!({"kind": 2, "userTypingDisagreed": false}));
}

#[test]
fn code_lens_is_requested_resolved_drawn_and_clicked() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let u = uri("/w/data.json");
    let (_, g) = open(&mut c, t0, "/w/data.json", "a\nb\nc\n");
    c.handle(t0, request(1, "MainThreadLanguageFeatures", "$registerCodeLensSupport", vec![json!(7), json!([{"language": "json"}]), json!(8)]));
    assert_eq!(c.next_wake(), Some(t0 + Duration::from_millis(250)));
    let fx = c.handle(t0 + Duration::from_millis(250), Input::Tick);
    let (t, lc) = call(&fx, "$provideCodeLenses");
    assert_eq!(arg(lc, 1)["path"], "/w/data.json");
    let range = |l: u32| json!({"startLineNumber": l, "startColumn": 1, "endLineNumber": l, "endColumn": 2});
    let fx = c.handle(
        t0,
        Input::RpcReply {
            token: t.unwrap(),
            result: Ok(json!({"cacheId": 11, "lenses": [
                {"cacheId": [11, 0], "range": range(1), "command": {"id": "__vsc1", "title": "1 reference", "arguments": ["refs /1"]}},
                {"cacheId": [11, 1], "range": range(3)}
            ]})),
        },
    );
    let (rt, rc) = call(&fx, "$resolveCodeLens");
    assert_eq!(arg(rc, 1)["cacheId"], json!([11, 1]));
    let d = decorations(&fx);
    assert_eq!(d.len(), 1);
    assert_eq!((d[0].kind, d[0].text.as_str(), d[0].range.start), (DecorationKind::CodeLens, "1 reference", p(0, 0)));
    let lens_id = d[0].id;

    let fx = c.handle(t0, Input::RpcReply { token: rt.unwrap(), result: Ok(json!({"range": range(3), "command": {"id": "workbench.action.x", "title": "Run"}})) });
    let d = decorations(&fx);
    assert_eq!(d.len(), 2);
    assert_eq!(d[0].id, lens_id, "resolving another lens keeps ids");

    // Clicking a lens whose command the extension host registered → $executeContributedCommand.
    c.handle(t0, request(2, "MainThreadCommands", "$registerCommand", vec![json!("__vsc1")]));
    let fx = c.handle(t0, Input::Widget { uri: u.clone(), generation: g, event: WidgetEvent::DecorationActivated { decoration: lens_id.get() } });
    let ec = call(&fx, "$executeContributedCommand").1;
    assert_eq!((arg(ec, 0), arg(ec, 1)), (json!("__vsc1"), json!("refs /1")));

    // A command nobody registered: activate onCommand:, then hand it to the UI.
    let fx = c.handle(t0, Input::Widget { uri: u.clone(), generation: g, event: WidgetEvent::DecorationActivated { decoration: d[1].id.get() } });
    let (at, ac) = call(&fx, "$activateByEvent");
    assert_eq!(arg(ac, 0), json!("onCommand:workbench.action.x"));
    let fx = c.handle(t0, Input::RpcReply { token: at.unwrap(), result: Ok(Value::Null) });
    assert!(matches!(updates(&fx)[0], SessionUpdate::RunCommand { command } if command.id == "workbench.action.x"));

    // Executing a command through Input::ExecuteCommand dispatches $executeContributedCommand.
    let fx = c.handle(t0, Input::ExecuteCommand { id: "__vsc1".into(), arguments: vec![json!("hello")] });
    let ec = call(&fx, "$executeContributedCommand").1;
    assert_eq!((arg(ec, 0), arg(ec, 1)), (json!("__vsc1"), json!("hello")));

    // $emitCodeLensEvent re-requests; the replaced list is released.
    c.handle(t0, request(3, "MainThreadLanguageFeatures", "$emitCodeLensEvent", vec![json!(8)]));
    let fx = c.handle(t0 + Duration::from_secs(1), Input::Tick);
    let (t, _) = call(&fx, "$provideCodeLenses");
    let fx = c.handle(t0, Input::RpcReply { token: t.unwrap(), result: Ok(json!({"cacheId": 12, "lenses": []})) });
    assert_eq!(arg(call(&fx, "$releaseCodeLenses").1, 1), json!(11));
}

fn inline_registration(handle: i64, debounce_ms: u64) -> Vec<Value> {
    vec![
        json!(handle),
        json!([{"language": "*"}]),
        json!(true),
        json!("test.ext"),
        json!("1.0.0"),
        Value::Null,
        json!([]),
        json!("Test"),
        json!(debounce_ms),
        json!([]),
        json!(false),
        json!(false),
        Value::Null,
        json!(false),
        json!(false),
        Value::Null,
        json!(false),
    ]
}

#[test]
fn inline_completion_debounce_cancel_show_type_through_and_accept() {
    let t0 = Instant::now();
    let ms = |n: u64| t0 + Duration::from_millis(n);
    let mut c = core(t0);
    let u = uri("/w/a.json");
    let (_, g) = open(&mut c, t0, "/w/a.json", "fo");
    c.handle(t0, request(1, "MainThreadLanguageFeatures", "$registerInlineCompletionsSupport", inline_registration(9, 100)));
    let w = |e| Input::Widget { uri: u.clone(), generation: g, event: e };

    // Typing restarts the provider's 100 ms debounce.
    let fx = c.handle(ms(0), w(changed(1, p(0, 2), "o")));
    assert!(!methods(&fx).contains(&"$provideInlineCompletions"));
    assert_eq!(c.next_wake(), Some(ms(100)));
    c.handle(ms(50), w(changed(2, p(0, 3), "(")));
    assert_eq!(c.next_wake(), Some(ms(150)));
    assert!(calls(&c.handle(ms(120), Input::Tick)).is_empty());

    let fx = c.handle(ms(150), Input::Tick);
    let (ta, rc) = call(&fx, "$provideInlineCompletions");
    assert_eq!(arg(rc, 2), json!({"lineNumber": 1, "column": 5}));
    assert_eq!(arg(rc, 3)["triggerKind"], 0);
    assert!(rc.cancellable);

    // More typing cancels the outstanding request; its late reply is ignored.
    let fx = c.handle(ms(160), w(changed(3, p(0, 4), "x")));
    assert!(fx.contains(&Effect::Cancel(ta.unwrap())));
    assert!(c.handle(ms(170), Input::RpcReply { token: ta.unwrap(), result: Ok(json!({"pid": 1, "items": []})) }).is_empty());

    let fx = c.handle(ms(260), Input::Tick);
    let (tb, _) = call(&fx, "$provideInlineCompletions");
    let fx = c.handle(
        ms(270),
        Input::RpcReply {
            token: tb.unwrap(),
            result: Ok(json!({"pid": 7, "languageId": "json", "items": [
                {"insertText": "bar", "idx": 0, "range": {"startLineNumber": 1, "startColumn": 1, "endLineNumber": 1, "endColumn": 6}},
                {"insertText": "foo(x, y)", "idx": 1, "range": {"startLineNumber": 1, "startColumn": 1, "endLineNumber": 1, "endColumn": 6},
                 "command": {"id": "__vscAccept", "title": "", "arguments": ["acc /1"]}}
            ]})),
        },
    );
    let shown = call(&fx, "$handleInlineCompletionDidShow").1;
    assert_eq!((arg(shown, 1), arg(shown, 2), arg(shown, 3)), (json!(7), json!(1), json!("foo(x, y)")));
    let ghost = decorations(&fx).into_iter().find(|d| d.kind == DecorationKind::GhostText).expect("ghost");
    assert_eq!((ghost.range, ghost.text.as_str()), (WidgetRange::empty(p(0, 5)), ", y)"));

    // Typing the next characters shrinks it, keeps its id, and does not re-request.
    let fx = c.handle(ms(300), w(changed(4, p(0, 5), ",")));
    let g2 = decorations(&fx).into_iter().find(|d| d.kind == DecorationKind::GhostText).unwrap();
    assert_eq!((g2.id, g2.range, g2.text.as_str()), (ghost.id, WidgetRange::empty(p(0, 6)), " y)"));
    assert!(!methods(&fx).contains(&"$provideInlineCompletions"));
    assert_eq!(c.next_wake(), Some(ms(550)), "only the CodeLens refresh is pending: typing through does not re-request");

    // Tab: the widget inserts the rest (CodeChanged) and then reports the activation.
    let fx = c.handle(ms(400), w(changed(5, p(0, 6), " y)")));
    assert!(decorations(&fx).iter().all(|d| d.kind != DecorationKind::GhostText));
    let fx = c.handle(ms(400), w(WidgetEvent::DecorationActivated { decoration: ghost.id.get() }));
    let end = call(&fx, "$handleInlineCompletionEndOfLifetime").1;
    assert_eq!(arg(end, 3)["kind"], 0);
    assert_eq!(arg(call(&fx, "$freeInlineCompletionsList").1, 1), json!(7));
    // No EditCode: the widget already inserted the text. The item's command runs next.
    assert!(!updates(&fx).iter().any(|u| matches!(u, SessionUpdate::Command { .. })));
    assert_eq!(arg(call(&fx, "$activateByEvent").1, 0), json!("onCommand:__vscAccept"));
}

#[test]
fn inline_ghost_ends_when_typing_disagrees_and_widget_without_insert_gets_edit_code() {
    let t0 = Instant::now();
    let ms = |n: u64| t0 + Duration::from_millis(n);
    let mut c = core(t0);
    let u = uri("/w/a.json");
    let (_, g) = open(&mut c, t0, "/w/a.json", "");
    // debounceDelayMs 0: the request goes out in the same step as the change.
    c.handle(t0, request(1, "MainThreadLanguageFeatures", "$registerInlineCompletionsSupport", inline_registration(9, 0)));
    let w = |e| Input::Widget { uri: u.clone(), generation: g, event: e };
    let reply_ghost = |c: &mut SessionCore, at: Instant, fx: &[Effect], pid: i64| {
        let (t, _) = call(fx, "$provideInlineCompletions");
        let fx = c.handle(at, Input::RpcReply { token: t.unwrap(), result: Ok(json!({"pid": pid, "items": [{"insertText": "abc", "idx": 0}]})) });
        decorations(&fx).into_iter().find(|d| d.kind == DecorationKind::GhostText).unwrap()
    };

    let fx = c.handle(ms(0), w(changed(1, p(0, 0), "x")));
    let ghost = reply_ghost(&mut c, ms(0), &fx, 1);
    assert_eq!((ghost.range, ghost.text.as_str()), (WidgetRange::empty(p(0, 1)), "abc"));
    // Typing something else ends it as Ignored(userTypingDisagreed) and frees the list.
    let fx = c.handle(ms(10), w(changed(2, p(0, 1), "z")));
    let end = call(&fx, "$handleInlineCompletionEndOfLifetime").1;
    assert_eq!(arg(end, 3), json!({"kind": 2, "userTypingDisagreed": true}));
    assert_eq!(arg(call(&fx, "$freeInlineCompletionsList").1, 2), json!({"kind": "other"}));

    // Activation without a preceding insertion → the session inserts with EditCode.
    let ghost = reply_ghost(&mut c, ms(20), &fx, 2);
    let fx = c.handle(ms(30), w(WidgetEvent::DecorationActivated { decoration: ghost.id.get() }));
    assert!(updates(&fx).iter().any(|u| matches!(u, SessionUpdate::Command { command: WidgetCommand::EditCode { base_version: 2, range, text, .. }, .. }
        if *range == WidgetRange::empty(p(0, 2)) && text == "abc")));
    assert_eq!(arg(call(&fx, "$handleInlineCompletionEndOfLifetime").1, 3)["kind"], 0);
}

#[test]
fn exthost_open_document_and_other_requests() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let other = uri("/w/other.json");
    let fx = c.handle(t0, request(5, "MainThreadDocuments", "$tryOpenDocument", vec![json!(other)]));
    assert_eq!(fx, vec![Effect::ReadFile { uri: other.clone(), purpose: ReadPurpose::ExtHostOpen { req: 5 } }]);
    let fx = c.handle(t0, Input::FileRead { uri: other.clone(), purpose: ReadPurpose::ExtHostOpen { req: 5 }, result: Ok(b"{}".to_vec()) });
    let delta = arg(call(&fx, "$acceptDocumentsAndEditorsDelta").1, 0);
    assert_eq!(delta["addedDocuments"][0]["languageId"], "json");
    assert!(delta.get("addedEditors").is_none());
    assert!(fx.iter().any(|e| matches!(e, Effect::Respond { req: 5, result: Ok(Reply::Json(v)) } if v["path"] == "/w/other.json")));

    // Opening it in the editor later re-announces it with a higher version.
    let (fx, _) = open(&mut c, t0, "/w/other.json", "{}");
    let deltas: Vec<Value> = calls(&fx).iter().filter(|(_, c)| c.method == "$acceptDocumentsAndEditorsDelta").map(|(_, c)| arg(c, 0)).collect();
    assert_eq!(deltas[0]["removedDocuments"][0]["path"], "/w/other.json");
    assert_eq!(deltas[1]["addedDocuments"][0]["versionId"], 2);

    let fx = c.handle(t0, request(6, "MainThreadLanguages", "$getLanguages", vec![]));
    assert_eq!(fx, vec![Effect::Respond { req: 6, result: Ok(Reply::Json(json!(["json", "plaintext"]))) }]);
    let fx = c.handle(t0, request(7, "MainThreadStorage", "$initializeExtensionStorage", vec![json!(false), json!("x")]));
    assert_eq!(fx, vec![Effect::Respond { req: 7, result: Ok(Reply::Empty) }]);
    let fx = c.handle(t0, request(8, "MainThreadCommands", "$executeCommand", vec![json!("setContext"), json!(["k", true]), json!(false)]));
    assert_eq!(fx, vec![Effect::Respond { req: 8, result: Ok(Reply::Empty) }]);
    // The extension host at 1.139.1 sends setContext as `_setContext` (mhutchie.git-graph, #84):
    // also a no-op, never a RunCommand to the UI.
    let fx = c.handle(t0, request(9, "MainThreadCommands", "$executeCommand", vec![json!("_setContext"), json!(["git-graph:codiconsSupported", true]), json!(false)]));
    assert_eq!(fx, vec![Effect::Respond { req: 9, result: Ok(Reply::Empty) }]);
}

fn problems(effects: &[Effect]) -> Option<&Vec<ember_editor::diagnostics::Problem>> {
    updates(effects).into_iter().rev().find_map(|u| match u {
        SessionUpdate::Problems { problems } => Some(problems),
        _ => None,
    })
}

// FR-E2 (D38 problems list): the markers of every file the extension host reports are listed as
// problems, errors first, then by file and position, and the list follows `$changeMany` and `$clear`.
#[test]
fn problems_list_every_reported_marker_errors_first_and_follows_clear() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let a = uri("/w/a.json");
    let b = uri("/w/b.json");
    open(&mut c, t0, "/w/a.json", "{\n  \"ok\": true,,\n}\n");
    let fx = c.handle(
        t0,
        request(
            1,
            "MainThreadDiagnostics",
            "$changeMany",
            vec![json!("json"), json!([[a, [marker(4, "Trailing comma", 2, 14, 15), marker(8, "Value expected", 2, 14, 15)]], [b, [marker(8, "Unexpected end", 1, 1, 2)]]])],
        ),
    );
    let list = problems(&fx).expect("a Problems update");
    let seen: Vec<(&str, Severity, &str, u32)> =
        list.iter().map(|p| (p.uri.path.as_str(), p.severity, p.message.as_str(), p.range.start.line)).collect();
    assert_eq!(
        seen,
        vec![
            ("/w/a.json", Severity::Error, "Value expected", 1),
            ("/w/b.json", Severity::Error, "Unexpected end", 0),
            ("/w/a.json", Severity::Warning, "Trailing comma", 1),
        ]
    );
    assert_eq!(list[0].source.as_deref(), Some("json"));

    let fx = c.handle(t0, request(2, "MainThreadDiagnostics", "$clear", vec![json!("json")]));
    assert_eq!(problems(&fx), Some(&Vec::new()));
}

#[test]
fn file_close_clears_markers_for_that_file() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let a = uri("/w/a.json");
    let b = uri("/w/b.json");
    open(&mut c, t0, "/w/a.json", "{\n  \"ok\": true,,\n}\n");
    open(&mut c, t0, "/w/b.json", "{\n}\n");

    let fx = c.handle(
        t0,
        request(
            1,
            "MainThreadDiagnostics",
            "$changeMany",
            vec![json!("json"), json!([[a, [marker(8, "Error in a", 1, 1, 2)]], [b, [marker(4, "Warn in b", 1, 1, 2)]]])],
        ),
    );
    assert_eq!(problems(&fx).expect("problems").len(), 2);

    // Close a: problems update is emitted with only b's markers
    let fx = c.handle(t0, Input::Close { uri: a });
    let remaining = problems(&fx).expect("problems update after close");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].uri.path, "/w/b.json");
}

fn log_uri() -> Value {
    json!({"$mid": 1, "scheme": "vscode-remote", "authority": AUTH, "path": "/logs/git.log"})
}

/// `$register` for the channel `label`, whose request id is `req`; returns the effects.
fn register_channel(c: &mut SessionCore, t0: Instant, req: u32, label: &str) -> Vec<Effect> {
    c.handle(t0, request(req, "MainThreadOutputService", "$register", vec![json!(label), log_uri(), json!("git"), json!("vscode.git")]))
}

fn update_channel(c: &mut SessionCore, t0: Instant, req: u32, id: &str, mode: u8, till: Option<u64>) -> Vec<Effect> {
    let mut args = vec![json!(id), json!(mode)];
    args.extend(till.map(|t| json!(t)));
    c.handle(t0, request(req, "MainThreadOutputService", "$update", args))
}

/// The log-file reads among `fx`.
fn channel_reads(fx: &[Effect]) -> Vec<(UriComponents, ReadPurpose)> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::ReadFile { uri, purpose: purpose @ ReadPurpose::OutputChannel { .. } } => Some((uri.clone(), purpose.clone())),
            _ => None,
        })
        .collect()
}

/// Serve one log-file read with `text` and return what the channel list says afterwards.
fn serve_channel_read(c: &mut SessionCore, t0: Instant, read: (UriComponents, ReadPurpose), text: &str) -> (Vec<Effect>, Option<Vec<ember_editor::OutputChannel>>) {
    let fx = c.handle(t0, Input::FileRead { uri: read.0, purpose: read.1, result: Ok(text.as_bytes().to_vec()) });
    let channels = updates(&fx).into_iter().find_map(|u| match u {
        SessionUpdate::OutputChannels { channels, .. } => Some(channels.clone()),
        _ => None,
    });
    (fx, channels)
}

#[test]
fn output_service_and_message_service_flow() {
    let t0 = Instant::now();
    let mut c = core(t0);

    // Register an output channel
    let fx = register_channel(&mut c, t0, 1, "Git");
    assert!(fx.contains(&Effect::Respond {
        req: 1,
        result: Ok(Reply::Json(json!("ember-output-1"))),
    }));
    let up = updates(&fx).into_iter().find_map(|u| match u {
        SessionUpdate::OutputChannels { channels, active_id } => Some((channels, active_id)),
        _ => None,
    }).expect("OutputChannels update");
    assert_eq!(up.0.len(), 1);
    assert_eq!(up.0[0].label, "Git");
    assert_eq!(up.0[0].file, Some(serde_json::from_value(log_uri()).unwrap()));
    assert_eq!(up.1.as_deref(), Some("ember-output-1"));

    // Append to the channel: the extension host wrote the text to the file and says so.
    let fx = update_channel(&mut c, t0, 2, "ember-output-1", 1, None);
    assert!(fx.contains(&Effect::Respond { req: 2, result: Ok(Reply::Empty) }));
    let reads = channel_reads(&fx);
    assert_eq!(reads.len(), 1, "{fx:#?}");
    assert_eq!(reads[0].0, serde_json::from_value(log_uri()).unwrap());
    let (_, up) = serve_channel_read(&mut c, t0, reads[0].clone(), "git status output\n");
    assert_eq!(up.expect("OutputChannels update on append")[0].text, "git status output\n");

    // Clear the channel: the text now starts where the file ends.
    let fx = update_channel(&mut c, t0, 3, "ember-output-1", 3, Some(18));
    let up = updates(&fx).into_iter().find_map(|u| match u {
        SessionUpdate::OutputChannels { channels, .. } => Some(channels),
        _ => None,
    }).expect("OutputChannels update on clear");
    assert_eq!(up[0].text, "");
    let reads = channel_reads(&fx);
    assert_eq!(reads.len(), 1);
    let (_, up) = serve_channel_read(&mut c, t0, reads[0].clone(), "git status output\n");
    assert_eq!(up.expect("OutputChannels update after the clear")[0].text, "");

    // Reveal makes it the active channel.
    let fx = c.handle(t0, request(6, "MainThreadOutputService", "$reveal", vec![json!("ember-output-1"), json!(false)]));
    let active = updates(&fx).into_iter().find_map(|u| match u {
        SessionUpdate::OutputChannels { active_id, .. } => Some(active_id.clone()),
        _ => None,
    }).expect("OutputChannels update on reveal");
    assert_eq!(active.as_deref(), Some("ember-output-1"));

    // Dispose removes it, and a late read of its file is dropped.
    let fx = update_channel(&mut c, t0, 7, "ember-output-1", 1, None);
    let late = channel_reads(&fx).remove(0);
    let fx = c.handle(t0, request(8, "MainThreadOutputService", "$dispose", vec![json!("ember-output-1")]));
    let up = updates(&fx).into_iter().find_map(|u| match u {
        SessionUpdate::OutputChannels { channels, .. } => Some(channels),
        _ => None,
    }).expect("OutputChannels update on dispose");
    assert!(up.is_empty());
    let (_, up) = serve_channel_read(&mut c, t0, late, "after dispose\n");
    assert!(up.is_none());

    // Show message: no answer until the notification closes (FR-L31.16).
    let fx = c.handle(
        t0,
        request(
            4,
            "MainThreadMessageService",
            "$showMessage",
            vec![
                json!(3),
                json!("Rust analyzer crashed"),
                json!({"source": {"identifier": {"value": "rust-lang.rust-analyzer"}, "label": "rust-analyzer"}}),
                json!([{"title": "Restart", "isCloseAffordance": false, "handle": 0}]),
            ],
        ),
    );
    assert!(!fx.iter().any(|e| matches!(e, Effect::Respond { req: 4, .. })), "answered before the user chose");
    let msg = updates(&fx).into_iter().find_map(|u| match u {
        SessionUpdate::ExtensionMessage(m) => Some(m.clone()),
        _ => None,
    }).expect("ExtensionMessage update");
    assert_eq!(msg.severity, ember_editor::MessageSeverity::Error);
    assert_eq!(msg.message, "Rust analyzer crashed");
    assert_eq!(msg.source.as_deref(), Some("rust-analyzer"));
    assert_eq!(msg.actions.len(), 1);

    let fx = c.handle(t0, Input::AnswerMessage { id: msg.id, handle: Some(0) });
    assert!(fx.contains(&Effect::Respond { req: 4, result: Ok(Reply::Json(json!(0))) }));
    // A second answer for the same message is dropped.
    let fx = c.handle(t0, Input::AnswerMessage { id: msg.id, handle: None });
    assert!(!fx.iter().any(|e| matches!(e, Effect::Respond { req: 4, .. })));

    // Closed without a button: `undefined`.
    let fx = c.handle(t0, request(5, "MainThreadMessageService", "$showMessage", vec![json!(1), json!("Hi"), json!({}), json!([])]));
    let id = updates(&fx).into_iter().find_map(|u| match u { SessionUpdate::ExtensionMessage(m) => Some(m.id), _ => None }).unwrap();
    let fx = c.handle(t0, Input::AnswerMessage { id, handle: None });
    assert!(fx.contains(&Effect::Respond { req: 5, result: Ok(Reply::Empty) }));
}

#[test]
fn fr_l31_16_output_channel_shows_the_file_after_update() {
    let t0 = Instant::now();
    let mut c = core(t0);
    register_channel(&mut c, t0, 1, "Git");

    let fx = update_channel(&mut c, t0, 2, "ember-output-1", 1, None);
    let read = channel_reads(&fx).remove(0);
    let (_, up) = serve_channel_read(&mut c, t0, read, "one\n");
    assert_eq!(up.unwrap()[0].text, "one\n");

    // The second read returns the whole file, so both lines show and the first is not doubled.
    let fx = update_channel(&mut c, t0, 3, "ember-output-1", 1, None);
    let read = channel_reads(&fx).remove(0);
    let (_, up) = serve_channel_read(&mut c, t0, read, "one\ntwo\n");
    assert_eq!(up.unwrap()[0].text, "one\ntwo\n");

    // A file that cannot be read leaves the text as it was.
    let fx = update_channel(&mut c, t0, 4, "ember-output-1", 1, None);
    let (uri, purpose) = channel_reads(&fx).remove(0);
    let fx = c.handle(t0, Input::FileRead { uri, purpose, result: Err("gone".into()) });
    assert!(updates(&fx).iter().all(|u| !matches!(u, SessionUpdate::OutputChannels { .. })));
    let fx = update_channel(&mut c, t0, 5, "ember-output-1", 1, None);
    let read = channel_reads(&fx).remove(0);
    let (_, up) = serve_channel_read(&mut c, t0, read, "one\ntwo\nthree\n");
    assert_eq!(up.unwrap()[0].text, "one\ntwo\nthree\n");

    // Bytes that are not UTF-8 are replaced, not refused.
    let fx = update_channel(&mut c, t0, 6, "ember-output-1", 1, None);
    let (uri, purpose) = channel_reads(&fx).remove(0);
    let fx = c.handle(t0, Input::FileRead { uri, purpose, result: Ok(vec![b'a', 0xFF, b'b']) });
    let text = updates(&fx).into_iter().find_map(|u| match u {
        SessionUpdate::OutputChannels { channels, .. } => Some(channels[0].text.clone()),
        _ => None,
    });
    assert_eq!(text.as_deref(), Some("a\u{FFFD}b"));
}

#[test]
fn fr_l31_16_output_channel_clear_and_replace_drop_text_before_till() {
    let t0 = Instant::now();
    let mut c = core(t0);
    register_channel(&mut c, t0, 1, "Git");

    let fx = update_channel(&mut c, t0, 2, "ember-output-1", 1, None);
    let read = channel_reads(&fx).remove(0);
    serve_channel_read(&mut c, t0, read, "old1\nold2\n");

    // Replace(till = 10) was written after the old text: the shown text is the file from byte 10.
    let fx = update_channel(&mut c, t0, 3, "ember-output-1", 2, Some(10));
    let read = channel_reads(&fx).remove(0);
    let (_, up) = serve_channel_read(&mut c, t0, read, "old1\nold2\nnew\n");
    assert_eq!(up.unwrap()[0].text, "new\n");

    // An append after it keeps counting from the same start.
    let fx = update_channel(&mut c, t0, 4, "ember-output-1", 1, None);
    let read = channel_reads(&fx).remove(0);
    let (_, up) = serve_channel_read(&mut c, t0, read, "old1\nold2\nnew\nmore\n");
    assert_eq!(up.unwrap()[0].text, "new\nmore\n");

    // Clear(till = 19) empties the channel at once and what follows starts at byte 19.
    let fx = update_channel(&mut c, t0, 5, "ember-output-1", 3, Some(19));
    let cleared = updates(&fx).into_iter().find_map(|u| match u {
        SessionUpdate::OutputChannels { channels, .. } => Some(channels[0].text.clone()),
        _ => None,
    });
    assert_eq!(cleared.as_deref(), Some(""));
    let read = channel_reads(&fx).remove(0);
    let (_, up) = serve_channel_read(&mut c, t0, read, "old1\nold2\nnew\nmore\n");
    assert_eq!(up.unwrap()[0].text, "");
    let fx = update_channel(&mut c, t0, 6, "ember-output-1", 1, None);
    let read = channel_reads(&fx).remove(0);
    let (_, up) = serve_channel_read(&mut c, t0, read, "old1\nold2\nnew\nmore\nfresh\n");
    assert_eq!(up.unwrap()[0].text, "fresh\n");

    // A replace or clear without `till` is ignored: no read, no change.
    let fx = update_channel(&mut c, t0, 7, "ember-output-1", 3, None);
    assert!(fx.contains(&Effect::Respond { req: 7, result: Ok(Reply::Empty) }));
    assert!(channel_reads(&fx).is_empty());
    assert!(updates(&fx).is_empty());
}

#[test]
fn fr_l31_16_output_channel_reads_coalesce_while_one_is_in_flight() {
    let t0 = Instant::now();
    let mut c = core(t0);
    register_channel(&mut c, t0, 1, "Git");

    let fx = update_channel(&mut c, t0, 2, "ember-output-1", 1, None);
    let first = channel_reads(&fx).remove(0);
    // Every update answers at once, and none starts a second read while the first is out.
    for req in 3..8 {
        let fx = update_channel(&mut c, t0, req, "ember-output-1", 1, None);
        assert!(fx.contains(&Effect::Respond { req, result: Ok(Reply::Empty) }));
        assert!(channel_reads(&fx).is_empty(), "a read is already in flight: {fx:#?}");
    }
    // When it lands, exactly one more read follows for all of them.
    let (fx, up) = serve_channel_read(&mut c, t0, first, "a\n");
    assert_eq!(up.unwrap()[0].text, "a\n");
    let again = channel_reads(&fx);
    assert_eq!(again.len(), 1, "{fx:#?}");
    let (fx, up) = serve_channel_read(&mut c, t0, again[0].clone(), "a\nb\n");
    assert_eq!(up.unwrap()[0].text, "a\nb\n");
    assert!(channel_reads(&fx).is_empty());

    // With nothing in flight, the next update reads again.
    let fx = update_channel(&mut c, t0, 9, "ember-output-1", 1, None);
    assert_eq!(channel_reads(&fx).len(), 1);
}

#[test]
fn engine_is_any_dirty_at_or_below_tracks_dirty_documents() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let u = uri("/w/src/main.rs");
    let fx = c.handle(t0, Input::Open { uri: u.clone() });
    let req = fx.iter().find_map(|e| match e {
        Effect::ReadFile { uri, purpose } => Some((uri.clone(), purpose.clone())),
        _ => None,
    }).unwrap();
    let initial_bytes = b"fn main() {}\n".to_vec();
    let fx = c.handle(t0, Input::FileRead { uri: req.0, purpose: req.1, result: Ok(initial_bytes) });
    let generation = updates(&fx).into_iter().find_map(|u| match u {
        SessionUpdate::Opened { generation, .. } => Some(*generation),
        _ => None,
    }).unwrap();

    // Initially clean
    assert!(!c.is_any_dirty_at_or_below("/w/src"));
    assert!(!c.is_any_dirty_at_or_below("/w/src/main.rs"));

    // Modify document: becomes dirty
    c.handle(
        t0,
        Input::Widget {
            uri: u.clone(),
            generation,
            event: WidgetEvent::CodeChanged {
                version: 1,
                range: WidgetRange::empty(WidgetPos::new(0, 11)),
                text: " println!();".into(),
            },
        },
    );

    assert!(c.is_any_dirty_at_or_below("/w/src"));
    assert!(c.is_any_dirty_at_or_below("/w/src/main.rs"));
    assert!(c.is_any_dirty_at_or_below("/w"));
    assert!(!c.is_any_dirty_at_or_below("/w/other"));
    assert!(!c.is_any_dirty_at_or_below("/w/src2"));

    // Save requested: FileWritten Ok -> clean again
    let fx = c.handle(t0, Input::Widget { uri: u.clone(), generation, event: WidgetEvent::CodeSaveRequested { version: 1 } });
    let token = fx.iter().find_map(|e| match e {
        Effect::WriteFile { token, .. } => Some(*token),
        _ => None,
    }).unwrap();
    c.handle(t0, Input::FileWritten { uri: u.clone(), token, result: Ok(()) });

    assert!(!c.is_any_dirty_at_or_below("/w/src"));
}

// Requests for actors or methods no trait implements get the fallback reply and are counted, so
// the census says which actor needs behaviour next; implemented calls are not counted, and a
// malformed implemented call falls back without a panic.
#[test]
fn fr_e1_unimplemented_main_thread_calls_fall_back_and_are_counted() {
    let t0 = Instant::now();
    let mut c = core(t0);
    assert!(c.unimplemented_calls().is_empty());

    let fx = c.handle(t0, request(1, "MainThreadClipboard", "$readText", vec![json!("text/plain")]));
    assert_eq!(fx, vec![Effect::Respond { req: 1, result: Ok(Reply::Empty) }]);
    let key = ("MainThreadClipboard".to_string(), "$readText".to_string());
    assert_eq!(c.unimplemented_calls().get(&key), Some(&1));
    let fx = c.handle(t0, request(2, "MainThreadClipboard", "$readText", vec![json!("text/plain")]));
    assert_eq!(fx, vec![Effect::Respond { req: 2, result: Ok(Reply::Empty) }]);
    assert_eq!(c.unimplemented_calls().get(&key), Some(&2));

    // A method of an implemented actor that no trait method covers is counted too.
    let fx = c.handle(t0, request(3, "MainThreadCommands", "$fireCommandActivationEvent", vec![json!("x")]));
    assert_eq!(fx, vec![Effect::Respond { req: 3, result: Ok(Reply::Empty) }]);
    assert_eq!(c.unimplemented_calls().get(&("MainThreadCommands".to_string(), "$fireCommandActivationEvent".to_string())), Some(&1));

    // An implemented call is answered by its trait and is not in the census.
    let fx = c.handle(t0, request(4, "MainThreadDiagnostics", "$clear", vec![json!("ts")]));
    assert!(fx.contains(&Effect::Respond { req: 4, result: Ok(Reply::Empty) }));
    assert!(!c.unimplemented_calls().keys().any(|(p, _)| p == "MainThreadDiagnostics"));

    // An implemented call whose arguments do not decode gets the fallback reply.
    let before = c.unimplemented_calls().clone();
    let fx = c.handle(t0, request(5, "MainThreadDiagnostics", "$clear", vec![]));
    assert_eq!(fx, vec![Effect::Respond { req: 5, result: Ok(Reply::Empty) }]);
    assert_eq!(c.unimplemented_calls(), &before);
}

// ---- FR-L31.16: log output channels arrive as registered loggers (#1019) ----------------------

fn logger_uri(name: &str) -> Value {
    json!({"$mid": 1, "scheme": "vscode-remote", "authority": AUTH, "path": format!("/logs/{name}.log")})
}

fn register_logger(c: &mut SessionCore, t0: Instant, req: u32, id: &str, name: &str, hidden: bool) -> Vec<Effect> {
    let logger = json!({"resource": logger_uri(id), "id": id, "name": name, "hidden": hidden, "extensionId": "vscode.json-language-features"});
    c.handle(t0, request(req, "MainThreadLogger", "$registerLogger", vec![logger]))
}

fn channel_list(fx: &[Effect]) -> Option<Vec<ember_editor::OutputChannel>> {
    updates(fx).into_iter().rev().find_map(|u| match u {
        SessionUpdate::OutputChannels { channels, .. } => Some(channels.clone()),
        _ => None,
    })
}

#[test]
fn fr_l31_16_a_registered_logger_becomes_an_output_channel_showing_its_file() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let fx = register_logger(&mut c, t0, 1, "json.log", "JSON Language Server", false);
    assert!(fx.contains(&Effect::Respond { req: 1, result: Ok(Reply::Empty) }), "{fx:#?}");
    let list = channel_list(&fx).expect("the channel list changes");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].label, "JSON Language Server");
    assert_eq!(list[0].file, Some(serde_json::from_value(logger_uri("json.log")).unwrap()));
    // The file is read right away and its text is the channel's.
    let read = channel_reads(&fx).remove(0);
    assert_eq!(read.0, serde_json::from_value(logger_uri("json.log")).unwrap());
    let (_, up) = serve_channel_read(&mut c, t0, read, "[info] started\n");
    assert_eq!(up.unwrap()[0].text, "[info] started\n");

    // Registering the same logger again updates the channel instead of adding a second one.
    let fx = register_logger(&mut c, t0, 2, "json.log", "JSON Language Server", false);
    assert_eq!(channel_list(&fx).unwrap().len(), 1);
}

#[test]
fn fr_l31_16_a_log_channel_follows_its_file() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let fx = register_logger(&mut c, t0, 1, "json.log", "JSON Language Server", false);
    let read = channel_reads(&fx).remove(0);
    serve_channel_read(&mut c, t0, read, "one\n");
    assert!(c.next_wake().is_some(), "a visible log channel schedules its next read");

    // Nothing is read before the second is up.
    let fx = c.handle(t0 + Duration::from_millis(500), Input::Tick);
    assert!(channel_reads(&fx).is_empty());

    // A second after the registration the shown log channel is read again and shows the growth.
    let t1 = t0 + Duration::from_millis(1100);
    let fx = c.handle(t1, Input::Tick);
    let read = channel_reads(&fx).remove(0);
    let (_, up) = serve_channel_read(&mut c, t1, read, "one\ntwo\n");
    assert_eq!(up.unwrap()[0].text, "one\ntwo\n");

    // A log channel the window does not show is not read.
    let fx = register_logger(&mut c, t1, 2, "other.log", "Other", false);
    let read = channel_reads(&fx).remove(0);
    serve_channel_read(&mut c, t1, read, "");
    c.handle(t1, Input::ActiveOutputChannel { id: Some("ember-logger-other.log".into()) });
    let fx = c.handle(t1 + Duration::from_millis(1100), Input::Tick);
    let reads = channel_reads(&fx);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].0, serde_json::from_value(logger_uri("other.log")).unwrap());
}

#[test]
fn fr_l31_16_deregister_and_visibility_of_a_logger() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let fx = register_logger(&mut c, t0, 1, "json.log", "JSON Language Server", false);
    let read = channel_reads(&fx).remove(0);
    serve_channel_read(&mut c, t0, read, "text\n");

    // Hidden: gone from the list, text kept.
    let fx = c.handle(t0, request(2, "MainThreadLogger", "$setVisibility", vec![logger_uri("json.log"), json!(false)]));
    assert!(fx.contains(&Effect::Respond { req: 2, result: Ok(Reply::Empty) }));
    assert!(channel_list(&fx).unwrap().is_empty());

    // Shown again: back in the list with its text, and the file is read again.
    let fx = c.handle(t0, request(3, "MainThreadLogger", "$setVisibility", vec![logger_uri("json.log"), json!(true)]));
    let list = channel_list(&fx).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].text, "text\n");
    assert_eq!(channel_reads(&fx).len(), 1);

    // Deregistered: removed.
    let fx = c.handle(t0, request(4, "MainThreadLogger", "$deregisterLogger", vec![logger_uri("json.log")]));
    assert!(fx.contains(&Effect::Respond { req: 4, result: Ok(Reply::Empty) }));
    assert!(channel_list(&fx).unwrap().is_empty());
}

#[test]
fn fr_l31_16_a_hidden_logger_is_not_a_channel() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let fx = register_logger(&mut c, t0, 1, "quiet.log", "Quiet", true);
    assert!(fx.contains(&Effect::Respond { req: 1, result: Ok(Reply::Empty) }));
    assert!(channel_list(&fx).map_or(true, |l| l.is_empty()));
    assert!(channel_reads(&fx).is_empty());
    assert!(c.next_wake().is_none());
}
