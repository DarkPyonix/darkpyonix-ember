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
}
