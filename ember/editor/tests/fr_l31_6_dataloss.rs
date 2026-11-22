//! FR-L31.6: paths that lost or overwrote the user's text. Each test drives the session core and the
//! widget bridge by hand, as the window does, with no server.

use std::time::Instant;

use ember_editor::engine::{ReadPurpose, Token};
use ember_editor::languages::LanguageRegistry;
use ember_editor::{
    BridgeAction, CoreOptions, Effect, ExternalChange, FileNoticeKind, Input, SessionCore, SessionUpdate, WidgetBridge, WidgetCommand,
    WidgetEvent, WidgetPos, WidgetRange,
};
use ember_editor_conn::remote_fs::{FileChange, FileChangeType};
use ember_editor_conn::rpc::{Arg, IncomingRequest, Reply};
use ember_editor_conn::uri::UriComponents;
use serde_json::{json, Value};

fn core(t0: Instant) -> SessionCore {
    let langs = LanguageRegistry::from_extensions(&[json!({"contributes": {"languages": [{"id": "json", "extensions": [".json"]}]}})]);
    SessionCore::new(langs, CoreOptions::default(), t0)
}

fn uri(path: &str) -> UriComponents {
    UriComponents::remote("127.0.0.1:9", path)
}

fn updates(effects: &[Effect]) -> Vec<&SessionUpdate> {
    effects.iter().filter_map(|e| if let Effect::Ui(u) = e { Some(u) } else { None }).collect()
}

fn apply(bridge: &mut WidgetBridge, effects: &[Effect]) -> Vec<BridgeAction> {
    updates(effects).into_iter().map(|u| bridge.apply(u.clone())).collect()
}

fn has_resync(actions: &[BridgeAction]) -> bool {
    actions.iter().any(|a| matches!(a, BridgeAction::Resync(_)))
}

fn open(c: &mut SessionCore, bridge: &mut WidgetBridge, t0: Instant, u: &UriComponents, text: &str) -> u64 {
    c.handle(t0, Input::Open { uri: u.clone() });
    let fx = c.handle(t0, Input::FileRead { uri: u.clone(), purpose: ReadPurpose::Open, result: Ok(text.as_bytes().to_vec()) });
    apply(bridge, &fx);
    bridge.document(u).expect("opened").generation
}

/// Type `text` at the start of the document, as the widget's change callback does, and feed the core.
fn type_text(c: &mut SessionCore, bridge: &mut WidgetBridge, t0: Instant, u: &UriComponents, version: u32, text: &str) -> Vec<BridgeAction> {
    let range = WidgetRange::empty(WidgetPos::new(0, 0));
    let (uri, generation, event) = bridge.on_code_changed(u, version, range, text.into()).expect("open in the bridge");
    let fx = c.handle(t0, Input::Widget { uri, generation, event });
    apply(bridge, &fx)
}

fn save_effects(c: &mut SessionCore, t0: Instant, u: &UriComponents, generation: u64) -> Vec<Effect> {
    c.handle(t0, Input::Widget { uri: u.clone(), generation, event: WidgetEvent::CodeSaveRequested { version: 0 } })
}

fn writes(fx: &[Effect]) -> Vec<(Vec<u8>, Token, Option<ember_editor::DiskBase>)> {
    fx.iter()
        .filter_map(|e| if let Effect::WriteFile { bytes, token, expect, .. } = e { Some((bytes.clone(), *token, expect.clone())) } else { None })
        .collect()
}

fn changed_on_disk(u: &UriComponents) -> Input {
    Input::FilesChanged(vec![FileChange { kind: FileChangeType::Updated, resource: u.clone(), correlation_id: None }])
}

// ---- A1, B4: the bridge knows an edit is unsaved before the session says so ---------------------

#[test]
fn fr_l31_6_a1_an_edit_while_no_session_answers_leaves_the_document_dirty() {
    let mut bridge = WidgetBridge::new();
    let u = uri("/w/a.json");
    bridge.apply(SessionUpdate::Opened { uri: u.clone(), generation: 3, text: "{}\n".into(), language_id: "json".into(), tab_width: 4 });
    assert!(!bridge.document(&u).unwrap().dirty);
    // The connection is lost: the change reaches the bridge and no session ever answers.
    bridge.on_code_changed(&u, 1, WidgetRange::empty(WidgetPos::new(0, 0)), "x".into()).unwrap();
    assert!(bridge.document(&u).unwrap().dirty, "the tab must show the edit as unsaved and ask before it closes");
    assert_eq!(bridge.document(&u).unwrap().text, "x{}\n");
}

#[test]
fn fr_l31_6_b4_a_close_right_after_the_first_keystroke_sees_the_edit() {
    let t0 = Instant::now();
    let (mut c, mut bridge) = (core(t0), WidgetBridge::new());
    let u = uri("/w/a.json");
    let g = open(&mut c, &mut bridge, t0, &u, "{}\n");
    // The keystroke is in the bridge; the session's Dirty has not come back yet.
    let (_, _, event) = bridge.on_code_changed(&u, 1, WidgetRange::empty(WidgetPos::new(0, 0)), "x".into()).unwrap();
    assert!(bridge.document(&u).unwrap().dirty);

    // A later Dirty{false} after a save still wins.
    let fx = c.handle(t0, Input::Widget { uri: u.clone(), generation: g, event });
    apply(&mut bridge, &fx);
    let fx = save_effects(&mut c, t0, &u, g);
    let (_, token, _) = writes(&fx).remove(0);
    let fx = c.handle(t0, Input::FileWritten { uri: u.clone(), token, result: Ok(()) });
    apply(&mut bridge, &fx);
    assert!(!bridge.document(&u).unwrap().dirty);
}

// ---- A3: the widget's versions after a remount --------------------------------------------------

#[test]
fn fr_l31_6_a3_typing_after_a_remount_reaches_the_buffer() {
    let t0 = Instant::now();
    let (mut c, mut bridge) = (core(t0), WidgetBridge::new());
    let u = uri("/w/a.json");
    let g = open(&mut c, &mut bridge, t0, &u, "abc\n");
    assert!(!has_resync(&type_text(&mut c, &mut bridge, t0, &u, 1, "x")));
    assert!(!has_resync(&type_text(&mut c, &mut bridge, t0, &u, 2, "y")));
    assert_eq!(c.widget_text(&u).as_deref(), Some("yxabc\n"));

    // Another tab was in front, now this one is shown again: a new widget, counting from 0.
    bridge.on_widget_mounted(&u, g);
    let actions = type_text(&mut c, &mut bridge, t0, &u, 1, "z");
    assert!(!has_resync(&actions), "{actions:?}");
    assert_eq!(c.widget_text(&u).as_deref(), Some("zyxabc\n"), "the typed character reaches the core buffer");

    // And on typing, the version keeps counting from there.
    assert!(!has_resync(&type_text(&mut c, &mut bridge, t0, &u, 2, "w")));
    assert_eq!(c.widget_text(&u).as_deref(), Some("wzyxabc\n"));
}

#[test]
fn fr_l31_6_a3_a_version_that_goes_backwards_is_a_remount_even_before_the_window_says_so() {
    let t0 = Instant::now();
    let (mut c, mut bridge) = (core(t0), WidgetBridge::new());
    let u = uri("/w/a.json");
    open(&mut c, &mut bridge, t0, &u, "abc\n");
    type_text(&mut c, &mut bridge, t0, &u, 1, "x");
    type_text(&mut c, &mut bridge, t0, &u, 2, "y");
    // No on_widget_mounted call: the next change of the new widget is version 1 again.
    let actions = type_text(&mut c, &mut bridge, t0, &u, 1, "z");
    assert!(!has_resync(&actions), "{actions:?}");
    assert_eq!(c.widget_text(&u).as_deref(), Some("zyxabc\n"));
}

#[test]
fn fr_l31_6_a3_an_edit_queued_before_a_remount_is_applied_not_dropped() {
    let t0 = Instant::now();
    let (mut c, mut bridge) = (core(t0), WidgetBridge::new());
    let u = uri("/w/a.json");
    let g = open(&mut c, &mut bridge, t0, &u, "abc\n");
    type_text(&mut c, &mut bridge, t0, &u, 1, "x");
    type_text(&mut c, &mut bridge, t0, &u, 2, "y");
    // The session wrote the edit against its own version 2, for a widget that is not mounted.
    bridge.apply(SessionUpdate::Command {
        uri: u.clone(),
        command: WidgetCommand::EditCode { request_id: 1, base_version: 2, range: WidgetRange::empty(WidgetPos::new(0, 0)), text: "q".into() },
    });
    bridge.on_widget_mounted(&u, g);
    let edits = bridge.take_pending_edits(&u);
    match &edits[..] {
        [WidgetCommand::EditCode { base_version, .. }] => assert_eq!(*base_version, 0, "the new widget is at version 0"),
        other => panic!("{other:?}"),
    }
    // The widget applies it and reports it as its version 1.
    let actions = type_text(&mut c, &mut bridge, t0, &u, 1, "q");
    assert!(!has_resync(&actions));
    assert_eq!(c.widget_text(&u).as_deref(), Some("qyxabc\n"));
}

#[test]
fn fr_l31_6_a3_generations_are_unique_across_sessions() {
    let t0 = Instant::now();
    let u = uri("/w/a.json");
    let (mut first, mut bridge_a) = (core(t0), WidgetBridge::new());
    let ga = open(&mut first, &mut bridge_a, t0, &u, "same\n");
    // A reconnect makes a new session; its first document must not reuse the old widget's key.
    let (mut second, mut bridge_b) = (core(t0), WidgetBridge::new());
    let gb = open(&mut second, &mut bridge_b, t0, &u, "same\n");
    assert_ne!(ga, gb);
}

// ---- B1: a resync keeps what the buffer was based on --------------------------------------------

#[test]
fn fr_l31_6_b1_resync_keeps_a_dirty_document_dirty_and_a_disk_change_raises_the_conflict() {
    let t0 = Instant::now();
    let (mut c, mut bridge) = (core(t0), WidgetBridge::new());
    let u = uri("/w/a.json");
    open(&mut c, &mut bridge, t0, &u, "{}\n");
    type_text(&mut c, &mut bridge, t0, &u, 1, "x");
    assert!(c.is_any_dirty_at_or_below("/w"));

    let fx = c.resync(t0, &u);
    assert!(updates(&fx).contains(&&SessionUpdate::Dirty { uri: u.clone(), dirty: true }), "{fx:#?}");
    apply(&mut bridge, &fx);
    assert!(bridge.document(&u).unwrap().dirty);
    assert!(c.is_any_dirty_at_or_below("/w"), "a rename or move of the file must still be refused");

    // The file changes on the server: the buffer is not replaced.
    let fx = c.handle(t0, changed_on_disk(&u));
    assert_eq!(fx, vec![Effect::ReadFile { uri: u.clone(), purpose: ReadPurpose::Reload }]);
    let fx = c.handle(t0, Input::FileRead { uri: u.clone(), purpose: ReadPurpose::Reload, result: Ok(b"[2]\n".to_vec()) });
    assert!(updates(&fx).contains(&&SessionUpdate::ExternalChange { uri: u.clone(), change: ExternalChange::ChangedWhileDirty }), "{fx:#?}");
    assert!(!updates(&fx).iter().any(|u| matches!(u, SessionUpdate::Command { command: WidgetCommand::SetText { .. }, .. })));
    assert_eq!(c.widget_text(&u).as_deref(), Some("x{}\n"));
}

#[test]
fn fr_l31_6_b1_a_resync_shows_the_conflict_bar_again() {
    let t0 = Instant::now();
    let (mut c, mut bridge) = (core(t0), WidgetBridge::new());
    let u = uri("/w/a.json");
    open(&mut c, &mut bridge, t0, &u, "{}\n");
    type_text(&mut c, &mut bridge, t0, &u, 1, "x");
    c.handle(t0, changed_on_disk(&u));
    let fx = c.handle(t0, Input::FileRead { uri: u.clone(), purpose: ReadPurpose::Reload, result: Ok(b"[2]\n".to_vec()) });
    apply(&mut bridge, &fx);
    assert_eq!(bridge.document(&u).unwrap().external_change, Some(ExternalChange::ChangedWhileDirty));

    let fx = c.resync(t0, &u);
    apply(&mut bridge, &fx);
    assert_eq!(bridge.document(&u).unwrap().external_change, Some(ExternalChange::ChangedWhileDirty), "the new widget node must not drop the bar");
}

// ---- B2: one write per document at a time -------------------------------------------------------

#[test]
fn fr_l31_6_b2_a_save_asked_while_one_is_in_flight_runs_after_it_with_the_latest_text() {
    let t0 = Instant::now();
    let (mut c, mut bridge) = (core(t0), WidgetBridge::new());
    let u = uri("/w/a.json");
    let g = open(&mut c, &mut bridge, t0, &u, "{}\n");
    type_text(&mut c, &mut bridge, t0, &u, 1, "x");
    let first = writes(&save_effects(&mut c, t0, &u, g));
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].0, b"x{}\n".to_vec());

    // More typing, a second save while the first write is on the wire: nothing is written yet.
    type_text(&mut c, &mut bridge, t0, &u, 2, "y");
    let second = save_effects(&mut c, t0, &u, g);
    assert!(writes(&second).is_empty(), "the older write could land last: {second:#?}");

    // The first write ends: the document has moved on, so it is still unsaved, and the second write starts.
    let fx = c.handle(t0, Input::FileWritten { uri: u.clone(), token: first[0].1, result: Ok(()) });
    apply(&mut bridge, &fx);
    assert!(bridge.document(&u).unwrap().dirty);
    let next = writes(&fx);
    assert_eq!(next.len(), 1, "{fx:#?}");
    assert_eq!(next[0].0, b"yx{}\n".to_vec(), "the latest bytes");

    let fx = c.handle(t0, Input::FileWritten { uri: u.clone(), token: next[0].1, result: Ok(()) });
    apply(&mut bridge, &fx);
    assert!(!bridge.document(&u).unwrap().dirty);
    assert!(writes(&fx).is_empty());
}

#[test]
fn fr_l31_6_b2_a_failed_write_still_lets_the_queued_save_run() {
    let t0 = Instant::now();
    let (mut c, mut bridge) = (core(t0), WidgetBridge::new());
    let u = uri("/w/a.json");
    let g = open(&mut c, &mut bridge, t0, &u, "{}\n");
    type_text(&mut c, &mut bridge, t0, &u, 1, "x");
    let first = writes(&save_effects(&mut c, t0, &u, g));
    assert!(writes(&save_effects(&mut c, t0, &u, g)).is_empty());
    let fx = c.handle(t0, Input::FileWritten { uri: u.clone(), token: first[0].1, result: Err("EACCES".into()) });
    assert!(updates(&fx).iter().any(|u| matches!(u, SessionUpdate::SaveFailed { .. })));
    assert_eq!(writes(&fx).len(), 1, "the user asked again");
}

// ---- B3: a save checks the disk first -----------------------------------------------------------

#[test]
fn fr_l31_6_b3_a_save_expects_the_text_the_buffer_was_based_on() {
    let t0 = Instant::now();
    let (mut c, mut bridge) = (core(t0), WidgetBridge::new());
    let u = uri("/w/a.json");
    let g = open(&mut c, &mut bridge, t0, &u, "{}\r\n");
    type_text(&mut c, &mut bridge, t0, &u, 1, "x");
    let (_, _, expect) = writes(&save_effects(&mut c, t0, &u, g)).remove(0);
    let expect = expect.expect("a save checks the file before it writes");
    assert!(expect.holds(b"{}\r\n"));
    assert!(!expect.holds(b"[1]\r\n"), "the file changed behind the session");
    assert!(!expect.holds(&[0x80, 0x81]), "a file that no longer decodes is a change too");
}

#[test]
fn fr_l31_6_b3_a_refused_save_raises_the_conflict_and_the_next_save_is_the_overwrite() {
    let t0 = Instant::now();
    let (mut c, mut bridge) = (core(t0), WidgetBridge::new());
    let u = uri("/w/a.json");
    let g = open(&mut c, &mut bridge, t0, &u, "{}\n");
    type_text(&mut c, &mut bridge, t0, &u, 1, "x");
    let (_, token, _) = writes(&save_effects(&mut c, t0, &u, g)).remove(0);
    // Another save is queued behind it; it must not overwrite what the first one refused to.
    assert!(writes(&save_effects(&mut c, t0, &u, g)).is_empty());

    let fx = c.handle(t0, Input::SaveStale { uri: u.clone(), token });
    assert!(writes(&fx).is_empty(), "{fx:#?}");
    let actions = apply(&mut bridge, &fx);
    assert!(actions.iter().any(|a| matches!(a, BridgeAction::SaveFailed { .. })), "{actions:?}");
    let doc = bridge.document(&u).unwrap();
    assert_eq!(doc.external_change, Some(ExternalChange::ChangedWhileDirty));
    assert!(doc.dirty);

    // The user chose to overwrite: the save writes without a check.
    let again = writes(&save_effects(&mut c, t0, &u, g));
    assert_eq!(again.len(), 1);
    assert!(again[0].2.is_none());
    let fx = c.handle(t0, Input::FileWritten { uri: u.clone(), token: again[0].1, result: Ok(()) });
    apply(&mut bridge, &fx);
    assert_eq!(bridge.document(&u).unwrap().external_change, None);
    // The conflict is over: the next save checks again.
    type_text(&mut c, &mut bridge, t0, &u, 2, "y");
    assert!(writes(&save_effects(&mut c, t0, &u, g))[0].2.is_some());
}

#[test]
fn fr_l31_6_b3_a_failed_reload_is_an_update_the_window_can_show() {
    let t0 = Instant::now();
    let (mut c, mut bridge) = (core(t0), WidgetBridge::new());
    let u = uri("/w/a.json");
    open(&mut c, &mut bridge, t0, &u, "{}\n");

    c.handle(t0, changed_on_disk(&u));
    let fx = c.handle(t0, Input::FileRead { uri: u.clone(), purpose: ReadPurpose::Reload, result: Err("EIO: read failed".into()) });
    let notice = updates(&fx).into_iter().find_map(|u| match u {
        SessionUpdate::FileNotice { uri, kind, detail } => Some((uri.clone(), *kind, detail.clone())),
        _ => None,
    });
    let (notice_uri, kind, detail) = notice.expect("a FileNotice");
    assert_eq!((notice_uri, kind), (Some(u.clone()), FileNoticeKind::ReloadFailed));
    assert!(detail.contains("EIO"), "{detail}");
    let actions = apply(&mut bridge, &fx);
    assert!(actions.iter().any(|a| matches!(a, BridgeAction::FileNotice { kind: FileNoticeKind::ReloadFailed, .. })), "{actions:?}");

    // Bytes that no longer decode are the same.
    c.handle(t0, changed_on_disk(&u));
    let fx = c.handle(t0, Input::FileRead { uri: u.clone(), purpose: ReadPurpose::Reload, result: Ok(vec![0x80, 0x81]) });
    assert!(updates(&fx).iter().any(|u| matches!(u, SessionUpdate::FileNotice { kind: FileNoticeKind::ReloadFailed, .. })), "{fx:#?}");
}

#[test]
fn fr_l31_6_b3_an_edit_the_widget_did_not_apply_is_an_update_the_window_can_show() {
    let t0 = Instant::now();
    let (mut c, mut bridge) = (core(t0), WidgetBridge::new());
    let u = uri("/w/a.json");
    let g = open(&mut c, &mut bridge, t0, &u, "{}\n");
    let (uri_ev, generation, event) =
        bridge.on_edit_rejected(&u, 4, 0, 1, WidgetRange::empty(WidgetPos::new(0, 0))).expect("open in the bridge");
    assert_eq!(generation, g);
    let fx = c.handle(t0, Input::Widget { uri: uri_ev, generation, event });
    assert!(updates(&fx).iter().any(|u| matches!(u, SessionUpdate::FileNotice { kind: FileNoticeKind::EditRejected, .. })), "{fx:#?}");
}

// ---- B6: edits for a document that is not the one shown -----------------------------------------

fn try_apply_workspace_edit(c: &mut SessionCore, now: Instant, dto: Value) -> Vec<Effect> {
    c.handle(
        now,
        Input::ExtHostRequest(IncomingRequest {
            req: 77,
            rpc_id: ember_editor_conn::rpc_ids::id_of("MainThreadBulkEdits").unwrap(),
            proxy: Some("MainThreadBulkEdits"),
            method: "$tryApplyWorkspaceEdit".into(),
            args: vec![Arg::Json(dto), Arg::Undefined, Arg::Json(json!(false))],
            cancellable: false,
        }),
    )
}

#[test]
fn fr_l31_6_b6_a_workspace_edit_for_a_background_document_goes_into_the_core_buffer() {
    let t0 = Instant::now();
    let (mut c, mut bridge) = (core(t0), WidgetBridge::new());
    let (a, b) = (uri("/w/a.json"), uri("/w/b.json"));
    open(&mut c, &mut bridge, t0, &a, "{}\n");
    open(&mut c, &mut bridge, t0, &b, "[]\n");
    // `b` is the one shown; `a` is a background tab with no widget.
    let range = json!({"startLineNumber": 1, "startColumn": 2, "endLineNumber": 1, "endColumn": 2});
    let dto = json!({"edits": [{"resource": serde_json::to_value(&a).unwrap(), "textEdit": {"range": range, "text": "\"k\": 1"}}]});
    let fx = try_apply_workspace_edit(&mut c, t0, dto);

    assert_eq!(c.widget_text(&a).as_deref(), Some("{\"k\": 1}\n"), "the core is the source of truth");
    assert!(
        !updates(&fx).iter().any(|u| matches!(u, SessionUpdate::Command { command: WidgetCommand::EditCode { .. }, .. })),
        "there is no widget to queue the edit for: {fx:#?}"
    );
    assert!(fx.contains(&Effect::Respond { req: 77, result: Ok(Reply::Json(json!(true))) }), "{fx:#?}");
    assert!(updates(&fx).contains(&&SessionUpdate::Dirty { uri: a.clone(), dirty: true }));
    apply(&mut bridge, &fx);

    // Closing the tab now is a close of an unsaved document, and the widget gets the text when it mounts.
    let doc = bridge.document(&a).unwrap();
    assert!(doc.dirty);
    assert_eq!(doc.text, "{\"k\": 1}\n");
    assert!(doc.pending_edits.is_empty());
    assert!(c.is_any_dirty_at_or_below("/w/a.json"));
    // The shown document is untouched.
    assert!(!bridge.document(&b).unwrap().dirty);
}

#[test]
fn fr_l31_6_b6_the_shown_document_still_gets_its_edit_through_the_widget() {
    let t0 = Instant::now();
    let (mut c, mut bridge) = (core(t0), WidgetBridge::new());
    let (a, b) = (uri("/w/a.json"), uri("/w/b.json"));
    open(&mut c, &mut bridge, t0, &a, "{}\n");
    open(&mut c, &mut bridge, t0, &b, "[]\n");
    let range = json!({"startLineNumber": 1, "startColumn": 2, "endLineNumber": 1, "endColumn": 2});
    let dto = json!({"edits": [{"resource": serde_json::to_value(&b).unwrap(), "textEdit": {"range": range, "text": "1"}}]});
    let fx = try_apply_workspace_edit(&mut c, t0, dto);
    assert!(updates(&fx).iter().any(|u| matches!(u, SessionUpdate::Command { command: WidgetCommand::EditCode { .. }, .. })), "{fx:#?}");
}

// ---- A2 (core half): restore with the text the buffer was based on ------------------------------

fn restore(c: &mut SessionCore, t0: Instant, u: &UriComponents, text: &str, base: Option<&str>) -> Vec<Effect> {
    c.handle(t0, Input::Restore { uri: u.clone(), text: text.into(), base: base.map(str::to_owned) })
}

#[test]
fn fr_l31_6_a2_a_buffer_with_edits_over_a_file_that_changed_raises_the_conflict() {
    let t0 = Instant::now();
    let (mut c, mut bridge) = (core(t0), WidgetBridge::new());
    let u = uri("/w/a.json");
    // The window's tab was based on `{"old": 1}`; while it was away the file became `{"new": 2}`.
    let fx = restore(&mut c, t0, &u, "{\"old\": 1, \"mine\": 3}\n", Some("{\"old\": 1}\n"));
    assert_eq!(fx, vec![Effect::ReadFile { uri: u.clone(), purpose: ReadPurpose::Open }]);
    let fx = c.handle(t0, Input::FileRead { uri: u.clone(), purpose: ReadPurpose::Open, result: Ok(b"{\"new\": 2}\n".to_vec()) });
    apply(&mut bridge, &fx);
    let doc = bridge.document(&u).unwrap();
    assert!(doc.dirty);
    assert_eq!(doc.text, "{\"old\": 1, \"mine\": 3}\n", "the user's edits are kept");
    assert_eq!(doc.external_change, Some(ExternalChange::ChangedWhileDirty));
}

#[test]
fn fr_l31_6_a2_a_buffer_without_edits_yields_to_the_newer_file() {
    let t0 = Instant::now();
    let (mut c, mut bridge) = (core(t0), WidgetBridge::new());
    let u = uri("/w/a.json");
    let fx = restore(&mut c, t0, &u, "{\"old\": 1}\n", Some("{\"old\": 1}\n"));
    assert_eq!(fx.len(), 1);
    let fx = c.handle(t0, Input::FileRead { uri: u.clone(), purpose: ReadPurpose::Open, result: Ok(b"{\"new\": 2}\n".to_vec()) });
    apply(&mut bridge, &fx);
    let doc = bridge.document(&u).unwrap();
    assert!(!doc.dirty, "nothing was edited, so there is nothing to protect");
    assert_eq!(doc.text, "{\"new\": 2}\n", "the tab shows the file as it is now");
    assert_eq!(doc.external_change, None);
}

#[test]
fn fr_l31_6_a2_a_file_that_still_holds_the_base_restores_the_buffer_as_before() {
    let t0 = Instant::now();
    let (mut c, mut bridge) = (core(t0), WidgetBridge::new());
    let u = uri("/w/a.json");
    let g = open(&mut c, &mut bridge, t0, &u, "{}\n");
    let fx = restore(&mut c, t0, &u, "{\"mine\": 1}\n", Some("{}\n"));
    apply(&mut bridge, &fx);
    let doc = bridge.document(&u).unwrap();
    assert!(doc.dirty);
    assert_ne!(doc.generation, g);
    assert_eq!(doc.external_change, None);
}

#[test]
fn fr_l31_6_a2_the_bridge_keeps_the_text_the_buffer_is_based_on() {
    let mut bridge = WidgetBridge::new();
    let u = uri("/w/a.json");
    bridge.apply(SessionUpdate::Opened { uri: u.clone(), generation: 3, text: "{}\n".into(), language_id: "json".into(), tab_width: 4 });
    bridge.on_code_changed(&u, 1, WidgetRange::empty(WidgetPos::new(0, 0)), "x".into()).unwrap();
    assert_eq!(bridge.document(&u).unwrap().disk_base.as_deref(), Some("{}\n"), "typing does not change what is on disk");
    bridge.apply(SessionUpdate::Dirty { uri: u.clone(), dirty: false });
    assert_eq!(bridge.document(&u).unwrap().disk_base.as_deref(), Some("x{}\n"), "after a save the buffer is the disk text");
}
