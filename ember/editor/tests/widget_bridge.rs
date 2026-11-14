//! End-to-end FR-38 `CodeEditor` widget decoration & event bridge tests (`ember_editor::bridge`,
//! `Decoration::encode_records` / `decode_records`).

use std::time::{Duration, Instant};

use ember_editor::engine::{ReadPurpose, Token};
use ember_editor::languages::LanguageRegistry;
use ember_editor::{
    apply_text_change, BridgeAction, CoreOptions, Decoration, DecorationId, DecorationKind, Effect, HoverPhase, Input,
    SessionCore, Severity, WidgetBridge, WidgetCommand, WidgetEvent, WidgetPos, WidgetRange,
    DECORATION_RECORD_LEN,
};
use ember_editor_conn::exthost::Call;
use ember_editor_conn::rpc::{Arg, IncomingRequest};
use ember_editor_conn::uri::UriComponents;
use serde_json::{json, Value};

const AUTH: &str = "127.0.0.1:9";

fn p(line: u32, col: u32) -> WidgetPos {
    WidgetPos::new(line, col)
}

fn r(sl: u32, sc: u32, el: u32, ec: u32) -> WidgetRange {
    WidgetRange::new(p(sl, sc), p(el, ec))
}

fn uri(path: &str) -> UriComponents {
    UriComponents::remote(AUTH, path)
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

fn call<'a>(effects: &'a [Effect], method: &str) -> (Option<Token>, &'a Call) {
    calls(effects)
        .into_iter()
        .find(|(_, c)| c.method == method)
        .unwrap_or_else(|| panic!("no {method} in {effects:#?}"))
}

fn arg(c: &Call, i: usize) -> Value {
    c.args[i].as_json().cloned().unwrap_or(Value::Null)
}

fn apply_all(bridge: &mut WidgetBridge, effects: &[Effect]) -> Vec<BridgeAction> {
    let mut actions = Vec::new();
    for e in effects {
        if let Effect::Ui(u) = e {
            let act = bridge.apply(u.clone());
            if act != BridgeAction::None {
                actions.push(act);
            }
        }
    }
    actions
}

#[test]
fn decoration_record_44_byte_wire_round_trip_and_byte_layout() {
    let list = vec![
        Decoration {
            id: DecorationId(0x0100_0000_0000_0001),
            version: 7,
            kind: DecorationKind::Underline,
            severity: Some(Severity::Error),
            color: 3,
            range: r(1, 2, 1, 8),
            text: String::new(),
        },
        Decoration {
            id: DecorationId(0x0100_0000_0000_0002),
            version: 7,
            kind: DecorationKind::Underline,
            severity: Some(Severity::Warning),
            color: 0,
            range: r(2, 0, 2, 4),
            text: String::new(),
        },
        Decoration {
            id: DecorationId(0x0100_0000_0000_0003),
            version: 7,
            kind: DecorationKind::Underline,
            severity: Some(Severity::Information),
            color: 0,
            range: r(3, 1, 3, 5),
            text: String::new(),
        },
        Decoration {
            id: DecorationId(0x0100_0000_0000_0004),
            version: 7,
            kind: DecorationKind::Underline,
            severity: Some(Severity::Hint),
            color: 0,
            range: r(4, 0, 4, 1),
            text: String::new(),
        },
        Decoration {
            id: DecorationId(0x0200_0000_0000_000a),
            version: 7,
            kind: DecorationKind::CodeLens,
            severity: None,
            color: 0,
            range: r(0, 0, 0, 0),
            text: "▶ Run test (2 참조)".into(),
        },
        Decoration {
            id: DecorationId(0x0300_0000_0000_000b),
            version: 7,
            kind: DecorationKind::HoverAnchor,
            severity: None,
            color: 0,
            range: r(1, 2, 1, 8),
            text: String::new(),
        },
        Decoration {
            id: DecorationId(0x0400_0000_0000_000c),
            version: 7,
            kind: DecorationKind::GhostText,
            severity: None,
            color: 0,
            range: r(5, 4, 5, 4),
            text: "println!(\"안녕\");".into(),
        },
    ];

    let bytes = Decoration::encode_records(&list);
    let records_len = list.len() * DECORATION_RECORD_LEN;
    let lens_bytes = list[4].text.as_bytes();
    let ghost_bytes = list[6].text.as_bytes();
    assert_eq!(bytes.len(), records_len + lens_bytes.len() + ghost_bytes.len());

    // Inspect exact 44-byte offsets of record 0 (Underline Error).
    let rec0 = &bytes[0..DECORATION_RECORD_LEN];
    assert_eq!(u32::from_le_bytes(rec0[0..4].try_into().unwrap()), 7);
    assert_eq!(u16::from_le_bytes(rec0[4..6].try_into().unwrap()), 1); // Underline
    assert_eq!(u16::from_le_bytes(rec0[6..8].try_into().unwrap()), 1); // Severity::Error
    assert_eq!(u16::from_le_bytes(rec0[8..10].try_into().unwrap()), 3); // color
    assert_eq!(u16::from_le_bytes(rec0[10..12].try_into().unwrap()), 0); // reserved 0
    assert_eq!(u32::from_le_bytes(rec0[12..16].try_into().unwrap()), 1); // start.line
    assert_eq!(u32::from_le_bytes(rec0[16..20].try_into().unwrap()), 2); // start.col
    assert_eq!(u32::from_le_bytes(rec0[20..24].try_into().unwrap()), 1); // end.line
    assert_eq!(u32::from_le_bytes(rec0[24..28].try_into().unwrap()), 8); // end.col
    assert_eq!(u64::from_le_bytes(rec0[28..36].try_into().unwrap()), 0x0100_0000_0000_0001);
    assert_eq!(u32::from_le_bytes(rec0[36..40].try_into().unwrap()), records_len as u32);
    assert_eq!(u32::from_le_bytes(rec0[40..44].try_into().unwrap()), 0);

    // Inspect record 4 (CodeLens with UTF-8 title) and record 5 (HoverAnchor after non-empty text).
    let rec4 = &bytes[4 * DECORATION_RECORD_LEN..5 * DECORATION_RECORD_LEN];
    assert_eq!(u16::from_le_bytes(rec4[4..6].try_into().unwrap()), 2); // CodeLens
    assert_eq!(u16::from_le_bytes(rec4[6..8].try_into().unwrap()), 0); // no severity
    assert_eq!(u32::from_le_bytes(rec4[36..40].try_into().unwrap()), records_len as u32);
    assert_eq!(u32::from_le_bytes(rec4[40..44].try_into().unwrap()), lens_bytes.len() as u32);

    let rec5 = &bytes[5 * DECORATION_RECORD_LEN..6 * DECORATION_RECORD_LEN];
    assert_eq!(u16::from_le_bytes(rec5[4..6].try_into().unwrap()), 3); // HoverAnchor
    assert_eq!(
        u32::from_le_bytes(rec5[36..40].try_into().unwrap()),
        (records_len + lens_bytes.len()) as u32,
        "empty text points into the text region at records_len + texts.len()"
    );
    assert_eq!(u32::from_le_bytes(rec5[40..44].try_into().unwrap()), 0);

    let rec6 = &bytes[6 * DECORATION_RECORD_LEN..7 * DECORATION_RECORD_LEN];
    assert_eq!(u16::from_le_bytes(rec6[4..6].try_into().unwrap()), 4); // GhostText
    assert_eq!(
        u32::from_le_bytes(rec6[36..40].try_into().unwrap()),
        (records_len + lens_bytes.len()) as u32
    );
    assert_eq!(u32::from_le_bytes(rec6[40..44].try_into().unwrap()), ghost_bytes.len() as u32);

    // UTF-8 tail matches concatenated texts.
    assert_eq!(&bytes[records_len..records_len + lens_bytes.len()], lens_bytes);
    assert_eq!(&bytes[records_len + lens_bytes.len()..], ghost_bytes);

    let decoded = Decoration::decode_records(&bytes);
    assert_eq!(decoded, list);
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
fn end_to_end_session_core_and_widget_bridge_flow() {
    let t0 = Instant::now();
    let ms = |n: u64| t0 + Duration::from_millis(n);
    let langs = LanguageRegistry::from_extensions(&[json!({
        "contributes": {"languages": [{"id": "rust", "extensions": [".rs"]}]}
    })]);
    // Cap at 1 so viewport prioritization is exercised.
    let opts = CoreOptions { codelens_resolve_cap: 1, ..Default::default() };
    let mut core = SessionCore::new(langs, opts, t0);
    let mut bridge = WidgetBridge::new();
    let u = uri("/w/main.rs");

    // 1. Open document -> WidgetBridge records generation, text, tab_width.
    let fx = core.handle(t0, Input::Open { uri: u.clone() });
    assert_eq!(fx, vec![Effect::ReadFile { uri: u.clone(), purpose: ReadPurpose::Open }]);
    let initial_text = "fn one() {}\nfn two() {\n    let x = fo\n}\n";
    let fx = core.handle(
        t0,
        Input::FileRead {
            uri: u.clone(),
            purpose: ReadPurpose::Open,
            result: Ok(initial_text.as_bytes().to_vec()),
        },
    );
    assert!(apply_all(&mut bridge, &fx).is_empty());
    let doc = bridge.document(&u).expect("document registered in bridge");
    let g1 = doc.generation;
    assert_eq!(doc.text, initial_text);
    assert_eq!(doc.language_id, "rust");
    assert_eq!(doc.tab_width, 4);
    assert!(doc.decorations.is_empty());
    assert!(doc.decoration_bytes.is_empty());

    // 2. Extension host sends Diagnostics ($changeMany) -> WidgetBridge holds Underline
    //    decorations and their 44-byte decoration_bytes decode back to the exact Underline records.
    let marker = json!({
        "severity": 8,
        "message": "cannot find value `fo` in this scope",
        "source": "rustc",
        "startLineNumber": 3,
        "startColumn": 13,
        "endLineNumber": 3,
        "endColumn": 15
    });
    let fx = core.handle(
        t0,
        request(1, "MainThreadDiagnostics", "$changeMany", vec![json!("rustc"), json!([[u, [marker]]])]),
    );
    apply_all(&mut bridge, &fx);
    let doc = bridge.document(&u).unwrap();
    assert_eq!(doc.decorations.len(), 1);
    assert_eq!(doc.decorations[0].kind, DecorationKind::Underline);
    assert_eq!(doc.decorations[0].severity, Some(Severity::Error));
    assert_eq!(doc.decorations[0].range, r(2, 12, 2, 14));
    assert_eq!(doc.decoration_bytes.len(), DECORATION_RECORD_LEN);
    assert_eq!(Decoration::decode_records(&doc.decoration_bytes), doc.decorations);

    // 3. Extension host registers CodeLens provider + returns resolved & unresolved lenses;
    //    VisibleRangeChanged prioritizes visible lenses -> WidgetBridge decoration_bytes contain
    //    both Underline and CodeLens records; activating the CodeLens decoration dispatches
    //    $executeContributedCommand.
    let (u_ev, gen_ev, vis_ev) = bridge.on_visible_range_changed(&u, 0, 1, 3).unwrap();
    core.handle(t0, Input::Widget { uri: u_ev, generation: gen_ev, event: vis_ev });

    core.handle(
        t0,
        request(
            2,
            "MainThreadLanguageFeatures",
            "$registerCodeLensSupport",
            vec![json!(7), json!([{"language": "rust"}]), json!(8)],
        ),
    );
    let fx = core.handle(ms(250), Input::Tick);
    let (lens_tok, _) = call(&fx, "$provideCodeLenses");
    let lens_range = |l: u32| json!({"startLineNumber": l, "startColumn": 1, "endLineNumber": l, "endColumn": 7});
    // Line 1 (0-based 0) is outside visible range 1..=3; Line 2 (0-based 1) is inside visible range 1..=3!
    // With codelens_resolve_cap = 1, the unresolved lens at line 2 must be resolved first.
    let fx = core.handle(
        ms(250),
        Input::RpcReply {
            token: lens_tok.unwrap(),
            result: Ok(json!({
                "cacheId": 10,
                "lenses": [
                    {"cacheId": [10, 0], "range": lens_range(1)},
                    {"cacheId": [10, 1], "range": lens_range(2)},
                    {"cacheId": [10, 2], "range": lens_range(2), "command": {"id": "__vscRun", "title": "Run", "arguments": ["run /1"]}}
                ]
            })),
        },
    );
    apply_all(&mut bridge, &fx);
    let (res_tok, res_call) = call(&fx, "$resolveCodeLens");
    assert_eq!(
        arg(res_call, 1)["cacheId"],
        json!([10, 1]),
        "unresolved lens inside the visible range (line 2 -> 0-based 1) is prioritized over line 1"
    );
    let fx = core.handle(
        ms(250),
        Input::RpcReply {
            token: res_tok.unwrap(),
            result: Ok(json!({
                "range": lens_range(2),
                "command": {"id": "__vscRefs", "title": "3 references", "arguments": ["refs /2"]}
            })),
        },
    );
    apply_all(&mut bridge, &fx);
    let doc = bridge.document(&u).unwrap();
    let decoded = Decoration::decode_records(&doc.decoration_bytes);
    assert_eq!(decoded, doc.decorations);
    assert!(decoded.iter().any(|d| d.kind == DecorationKind::Underline));
    let refs_lens = decoded
        .iter()
        .find(|d| d.kind == DecorationKind::CodeLens && d.text == "3 references")
        .expect("resolved visible CodeLens");
    let refs_lens_id = refs_lens.id.get();

    // Scrolling to bring line 0 into view resolves the remaining unresolved lens at index 0.
    let (u_ev, gen_ev, vis_ev) = bridge.on_visible_range_changed(&u, 0, 0, 1).unwrap();
    let fx = core.handle(ms(255), Input::Widget { uri: u_ev, generation: gen_ev, event: vis_ev });
    let (_, res_call_0) = call(&fx, "$resolveCodeLens");
    assert_eq!(arg(res_call_0, 1)["cacheId"], json!([10, 0]));

    // Clicking the CodeLens dispatches $executeContributedCommand.
    core.handle(t0, request(3, "MainThreadCommands", "$registerCommand", vec![json!("__vscRefs")]));
    let (u_ev, gen_ev, act_ev) = bridge.on_decoration_activated(&u, refs_lens_id).unwrap();
    let fx = core.handle(ms(260), Input::Widget { uri: u_ev, generation: gen_ev, event: act_ev });
    let (_, exec_call) = call(&fx, "$executeContributedCommand");
    assert_eq!((arg(exec_call, 0), arg(exec_call, 1)), (json!("__vscRefs"), json!("refs /2")));

    // 4. Pointer hovers (on_code_hovered with HoverPhase::Rest) -> extension host returns hover
    //    Markdown with a range -> WidgetBridge receives HoverPopup AND HoverAnchor decoration in
    //    decoration_bytes; HoverPhase::Leave clears both hover and the HoverAnchor decoration.
    core.handle(
        ms(260),
        request(4, "MainThreadLanguageFeatures", "$registerHoverProvider", vec![json!(12), json!([{"language": "rust"}])]),
    );
    let (u_ev, gen_ev, hov_ev) = bridge.on_code_hovered(&u, 0, p(2, 12), HoverPhase::Rest).unwrap();
    let fx = core.handle(ms(270), Input::Widget { uri: u_ev, generation: gen_ev, event: hov_ev });
    apply_all(&mut bridge, &fx);
    // Initial diagnostic hover popup is already present before provider replies.
    assert!(bridge.document(&u).unwrap().hover.is_some());
    let (hov_tok, _) = call(&fx, "$provideHover");
    let fx = core.handle(
        ms(275),
        Input::RpcReply {
            token: hov_tok.unwrap(),
            result: Ok(json!({
                "id": 1,
                "contents": [{"value": "`fo`: unresolved identifier"}],
                "range": {"startLineNumber": 3, "startColumn": 13, "endLineNumber": 3, "endColumn": 15}
            })),
        },
    );
    apply_all(&mut bridge, &fx);
    let doc = bridge.document(&u).unwrap();
    let popup = doc.hover.as_ref().expect("hover popup present");
    assert!(popup.complete);
    assert_eq!(popup.range, Some(r(2, 12, 2, 14)));
    let decoded = Decoration::decode_records(&doc.decoration_bytes);
    assert_eq!(decoded, doc.decorations);
    let anchor = decoded
        .iter()
        .find(|d| d.kind == DecorationKind::HoverAnchor)
        .expect("HoverAnchor decoration emitted when hover has range");
    assert_eq!(anchor.range, r(2, 12, 2, 14));

    // HoverPhase::Leave clears both hover and the HoverAnchor decoration.
    let (u_ev, gen_ev, leave_ev) = bridge.on_code_hovered(&u, anchor.id.get(), p(2, 12), HoverPhase::Leave).unwrap();
    let fx = core.handle(ms(280), Input::Widget { uri: u_ev, generation: gen_ev, event: leave_ev });
    apply_all(&mut bridge, &fx);
    let doc = bridge.document(&u).unwrap();
    assert!(doc.hover.is_none());
    let decoded = Decoration::decode_records(&doc.decoration_bytes);
    assert!(!decoded.iter().any(|d| d.kind == DecorationKind::HoverAnchor));

    // 5. User moves caret (on_selection_changed) and types (on_code_changed) -> extension host
    //    returns inline completion -> WidgetBridge decoration_bytes contain GhostText record ->
    //    user types through prefix -> presses Tab (on_code_changed + on_decoration_activated) ->
    //    ghost text accepted and cleared.
    core.handle(
        ms(280),
        request(
            5,
            "MainThreadLanguageFeatures",
            "$registerInlineCompletionsSupport",
            inline_registration(20, 50),
        ),
    );
    let (u_ev, gen_ev, sel_ev) = bridge.on_selection_changed(&u, 0, p(2, 14), p(2, 14)).unwrap();
    core.handle(ms(285), Input::Widget { uri: u_ev, generation: gen_ev, event: sel_ev });

    let (u_ev, gen_ev, chg_ev) = bridge.on_code_changed(&u, 1, WidgetRange::empty(p(2, 14)), "o".into()).unwrap();
    assert!(bridge.document(&u).unwrap().text.contains("let x = foo"));
    let fx = core.handle(ms(290), Input::Widget { uri: u_ev, generation: gen_ev, event: chg_ev });
    apply_all(&mut bridge, &fx);
    assert!(bridge.document(&u).unwrap().dirty);

    let fx = core.handle(ms(340), Input::Tick);
    let (inl_tok, _) = call(&fx, "$provideInlineCompletions");
    let fx = core.handle(
        ms(345),
        Input::RpcReply {
            token: inl_tok.unwrap(),
            result: Ok(json!({
                "pid": 99,
                "languageId": "rust",
                "items": [{
                    "insertText": "foo_bar();",
                    "idx": 0,
                    "range": {"startLineNumber": 3, "startColumn": 13, "endLineNumber": 3, "endColumn": 16}
                }]
            })),
        },
    );
    apply_all(&mut bridge, &fx);
    let doc = bridge.document(&u).unwrap();
    let decoded = Decoration::decode_records(&doc.decoration_bytes);
    assert_eq!(decoded, doc.decorations);
    let ghost = decoded
        .iter()
        .find(|d| d.kind == DecorationKind::GhostText)
        .expect("GhostText decoration present");
    assert_eq!((ghost.range, ghost.text.as_str()), (WidgetRange::empty(p(2, 15)), "_bar();"));
    let ghost_id = ghost.id.get();

    // User types through prefix "_b".
    let (u_ev, gen_ev, chg_ev) = bridge.on_code_changed(&u, 2, WidgetRange::empty(p(2, 15)), "_b".into()).unwrap();
    let fx = core.handle(ms(350), Input::Widget { uri: u_ev, generation: gen_ev, event: chg_ev });
    apply_all(&mut bridge, &fx);
    let decoded = Decoration::decode_records(&bridge.document(&u).unwrap().decoration_bytes);
    let ghost2 = decoded.iter().find(|d| d.kind == DecorationKind::GhostText).unwrap();
    assert_eq!((ghost2.id.get(), ghost2.range, ghost2.text.as_str()), (ghost_id, WidgetRange::empty(p(2, 17)), "ar();"));

    // User presses Tab: widget inserts "ar();" (CodeChanged) and sends DecorationActivated(ghost_id).
    let (u_ev, gen_ev, chg_ev) = bridge.on_code_changed(&u, 3, WidgetRange::empty(p(2, 17)), "ar();".into()).unwrap();
    let fx = core.handle(ms(360), Input::Widget { uri: u_ev, generation: gen_ev, event: chg_ev });
    apply_all(&mut bridge, &fx);
    let (u_ev, gen_ev, act_ev) = bridge.on_decoration_activated(&u, ghost_id).unwrap();
    let fx = core.handle(ms(360), Input::Widget { uri: u_ev, generation: gen_ev, event: act_ev });
    apply_all(&mut bridge, &fx);
    let end_call = call(&fx, "$handleInlineCompletionEndOfLifetime").1;
    assert_eq!(arg(end_call, 3)["kind"], 0);
    let decoded = Decoration::decode_records(&bridge.document(&u).unwrap().decoration_bytes);
    assert!(!decoded.iter().any(|d| d.kind == DecorationKind::GhostText));
    assert_eq!(bridge.document(&u).unwrap().text, "fn one() {}\nfn two() {\n    let x = foo_bar();\n}\n");

    // 6. Desync -> BridgeAction::Resync(uri) -> core.resync emits SetText with new generation ->
    //    WidgetBridge updates generation, text, and clears stale decorations.
    let (u_ev, gen_ev, bad_ev) = bridge.on_code_changed(&u, 99, WidgetRange::empty(p(0, 0)), "!".into()).unwrap();
    let fx = core.handle(ms(400), Input::Widget { uri: u_ev, generation: gen_ev, event: bad_ev });
    let actions = apply_all(&mut bridge, &fx);
    assert_eq!(actions, vec![BridgeAction::Resync(u.clone())]);

    let fx = core.resync(ms(405), &u);
    apply_all(&mut bridge, &fx);
    let doc = bridge.document(&u).unwrap();
    assert!(doc.generation > g1);
    assert_eq!(doc.text, "fn one() {}\nfn two() {\n    let x = foo_bar();\n}\n");
    assert_eq!(Decoration::decode_records(&doc.decoration_bytes), doc.decorations);
    // CodeLenses were cleared on reset (awaiting next provider reply), while diagnostics remain.
    assert!(!doc.decorations.iter().any(|d| d.kind == DecorationKind::CodeLens));
    assert!(doc.decorations.iter().any(|d| d.kind == DecorationKind::Underline));
    let _ = WidgetCommand::SetText { generation: doc.generation, text: doc.text.clone() };
    let _ = WidgetEvent::CodeSaveRequested { version: 0 };
}

#[test]
fn remount_or_reopen_preserves_document_version_in_widget_bridge() {
    let mut bridge = WidgetBridge::new();
    let u = uri("/src/main.rs");
    let fx = vec![Effect::Ui(ember_editor::SessionUpdate::Opened {
        uri: u.clone(),
        generation: 42,
        text: "hello world".to_string(),
        language_id: "rust".to_string(),
        tab_width: 4,
    })];
    apply_all(&mut bridge, &fx);

    let doc = bridge.document(&u).expect("document opened");
    assert_eq!(doc.generation, 42);
    assert_eq!(doc.version, 0, "initial version is 0");

    // First edit updates version to 1.
    bridge
        .on_code_changed(&u, 1, WidgetRange::empty(p(0, 5)), ",".to_string())
        .expect("on_code_changed event");
    assert_eq!(bridge.document(&u).unwrap().version, 1);

    // Later edit updates version to 5.
    bridge
        .on_code_changed(&u, 5, WidgetRange::empty(p(0, 12)), "!".to_string())
        .expect("on_code_changed event");
    assert_eq!(bridge.document(&u).unwrap().version, 5);

    // Simulated remount or tab switch: re-reading document preserves version 5.
    let remounted_doc = bridge.document(&u).expect("document exists");
    assert_eq!(remounted_doc.version, 5, "remount sees current version, not 0");

    // Syntax update with version 9 advances version.
    let fx_syntax = vec![Effect::Ui(ember_editor::SessionUpdate::Syntax {
        uri: u.clone(),
        generation: 42,
        version: 9,
        window: (0, 0),
        runs: Vec::new(),
    })];
    apply_all(&mut bridge, &fx_syntax);
    assert_eq!(bridge.document(&u).unwrap().version, 9);
}

#[test]
fn replace_all_multi_edit_with_hangul_and_emoji_preserves_utf8_boundaries() {
    let text = "안녕 😀 세상 🚀 안녕 😀 모두 🚀\n한글 🦀 테스트 🦀 한글\n";
    // Target matches for "안녕 😀" on line 0:
    // "안"(1) + "녕"(1) + " "(1) + "😀"(2) = 5 UTF-16 units -> cols 0..5
    // " 세상 "(4) + "🚀"(2) + " "(1) = 7 units -> cols 12..17
    let r1 = r(0, 0, 0, 5);
    let r2 = r(0, 12, 0, 17);

    // Apply last first (r2 then r1)
    let step1 = apply_text_change(text, r2, "반가워요 ✨").expect("valid UTF-16 edit 2");
    assert!(std::str::from_utf8(step1.as_bytes()).is_ok(), "step 1 produces valid UTF-8");
    let step2 = apply_text_change(&step1, r1, "반가워요 ✨").expect("valid UTF-16 edit 1");
    assert!(std::str::from_utf8(step2.as_bytes()).is_ok(), "step 2 produces valid UTF-8");

    assert_eq!(
        step2,
        "반가워요 ✨ 세상 🚀 반가워요 ✨ 모두 🚀\n한글 🦀 테스트 🦀 한글\n"
    );

    // Target matches for "🦀" on line 1:
    // "한"(1) + "글"(1) + " "(1) = 3 units -> cols 3..5
    // " 테스트 "(5) = 5 units -> cols 10..12
    let r3 = r(1, 3, 1, 5);
    let r4 = r(1, 10, 1, 12);

    let step3 = apply_text_change(&step2, r4, "🌟").expect("valid UTF-16 edit 4");
    let step4 = apply_text_change(&step3, r3, "🌟").expect("valid UTF-16 edit 3");
    assert_eq!(
        step4,
        "반가워요 ✨ 세상 🚀 반가워요 ✨ 모두 🚀\n한글 🌟 테스트 🌟 한글\n"
    );

    // Round-trip back
    let r_back1 = r(1, 3, 1, 5);
    let r_back2 = r(1, 10, 1, 12);
    let back1 = apply_text_change(&step4, r_back2, "🦀").unwrap();
    let back2 = apply_text_change(&back1, r_back1, "🦀").unwrap();
    assert_eq!(
        back2,
        "반가워요 ✨ 세상 🚀 반가워요 ✨ 모두 🚀\n한글 🦀 테스트 🦀 한글\n"
    );
}

#[test]
fn bridge_handles_output_channels_and_extension_notifications() {
    let mut bridge = WidgetBridge::new();
    assert!(bridge.output_channels().is_empty());

    // Output channels update
    let ch1 = ember_editor::OutputChannel::new("c1", "Git");
    let ch2 = ember_editor::OutputChannel::new("c2", "Tasks");
    bridge.apply(ember_editor::SessionUpdate::OutputChannels {
        channels: vec![ch1, ch2],
        active_id: Some("c1".into()),
    });
    assert_eq!(bridge.output_channels().len(), 2);
    assert_eq!(bridge.active_output_channel(), Some("c1"));

    // Clear output channel
    bridge.clear_output_channel("c1");

    // Extension message update: forwarded whole, the workbench keeps the notifications (FR-L31.16).
    let message = ember_editor::ExtensionMessage {
        id: 1,
        severity: ember_editor::MessageSeverity::Warning,
        message: "Extension warning".into(),
        source: Some("Rust Analyzer".into()),
        actions: vec![ember_editor::MessageAction { title: "Retry".into(), handle: 0, is_close_affordance: false }],
        modal: false,
        detail: None,
    };
    let action = bridge.apply(ember_editor::SessionUpdate::ExtensionMessage(message.clone()));
    assert_eq!(action, BridgeAction::ExtensionNotice(message));
}

#[test]
fn bridge_closed_clears_markers_for_that_file() {
    let mut bridge = WidgetBridge::new();
    let u1 = uri("/w/a.rs");
    let u2 = uri("/w/b.rs");
    bridge.apply(ember_editor::SessionUpdate::Opened {
        uri: u1.clone(),
        generation: 1,
        text: "fn main() {}".into(),
        language_id: "rust".into(),
        tab_width: 4,
    });
    bridge.apply(ember_editor::SessionUpdate::Opened {
        uri: u2.clone(),
        generation: 2,
        text: "fn test() {}".into(),
        language_id: "rust".into(),
        tab_width: 4,
    });

    // Populate problems for both files
    bridge.apply(ember_editor::SessionUpdate::Problems {
        problems: vec![
            ember_editor::diagnostics::Problem {
                uri: u1.clone(),
                owner: "rustc".into(),
                severity: Severity::Error,
                message: "error in a".into(),
                source: Some("rustc".into()),
                code: None,
                range: r(0, 0, 0, 2),
            },
            ember_editor::diagnostics::Problem {
                uri: u2.clone(),
                owner: "rustc".into(),
                severity: Severity::Warning,
                message: "warn in b".into(),
                source: Some("rustc".into()),
                code: None,
                range: r(0, 0, 0, 2),
            },
        ],
    });
    assert_eq!(bridge.problems().len(), 2);

    // Close u1: markers for u1 are cleared, u2 remains
    bridge.apply(ember_editor::SessionUpdate::Closed { uri: u1 });
    assert_eq!(bridge.problems().len(), 1);
    assert_eq!(bridge.problems()[0].uri.path, "/w/b.rs");
}
