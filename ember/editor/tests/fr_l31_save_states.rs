//! FR-L31.6: the file states the window shows. The session core and the widget bridge keep what
//! Code-OSS shows for a document: a failed save (until a save succeeds), a change on disk while the
//! buffer has unsaved edits (a clean buffer reloads silently), a file deleted on disk, and a buffer
//! restored after a reconnect (dirty unless it equals the disk).

use std::time::Instant;

use ember_editor::engine::{ReadPurpose, Token};
use ember_editor::languages::LanguageRegistry;
use ember_editor::{
    BridgeAction, CoreOptions, Effect, ExternalChange, Input, SessionCore, SessionUpdate, WidgetBridge, WidgetCommand, WidgetEvent,
    WidgetPos, WidgetRange,
};
use ember_editor_conn::remote_fs::{FileChange, FileChangeType};
use ember_editor_conn::uri::UriComponents;
use serde_json::json;

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

/// Feed every UI update to the bridge, as the window does, and return the actions.
fn apply(bridge: &mut WidgetBridge, effects: &[Effect]) -> Vec<BridgeAction> {
    updates(effects).into_iter().map(|u| bridge.apply(u.clone())).collect()
}

fn open(c: &mut SessionCore, bridge: &mut WidgetBridge, t0: Instant, u: &UriComponents, text: &str) -> u64 {
    let fx = c.handle(t0, Input::Open { uri: u.clone() });
    assert_eq!(fx, vec![Effect::ReadFile { uri: u.clone(), purpose: ReadPurpose::Open }]);
    let fx = c.handle(t0, Input::FileRead { uri: u.clone(), purpose: ReadPurpose::Open, result: Ok(text.as_bytes().to_vec()) });
    apply(bridge, &fx);
    bridge.document(u).expect("opened").generation
}

/// Type `text` at the start of the document through the bridge, as the widget's `on_change` does.
fn type_text(c: &mut SessionCore, bridge: &mut WidgetBridge, t0: Instant, u: &UriComponents, version: u32, text: &str) {
    let range = WidgetRange::empty(WidgetPos::new(0, 0));
    let (uri, generation, event) = bridge.on_code_changed(u, version, range, text.into()).expect("open in the bridge");
    let fx = c.handle(t0, Input::Widget { uri, generation, event });
    apply(bridge, &fx);
}

fn save(c: &mut SessionCore, t0: Instant, u: &UriComponents, generation: u64) -> (Vec<u8>, Token) {
    let fx = c.handle(t0, Input::Widget { uri: u.clone(), generation, event: WidgetEvent::CodeSaveRequested { version: 0 } });
    fx.iter()
        .find_map(|e| if let Effect::WriteFile { bytes, token, .. } = e { Some((bytes.clone(), *token)) } else { None })
        .unwrap_or_else(|| panic!("no WriteFile in {fx:#?}"))
}

/// The updates that change the buffer or its dirty state (not overlays).
fn buffer_updates<'a>(effects: &'a [Effect]) -> Vec<&'a SessionUpdate> {
    updates(effects).into_iter().filter(|u| matches!(u, SessionUpdate::Command { .. } | SessionUpdate::Dirty { .. })).collect()
}

fn changed_on_disk(u: &UriComponents) -> Input {
    Input::FilesChanged(vec![FileChange { kind: FileChangeType::Updated, resource: u.clone(), correlation_id: None }])
}

#[test]
fn fr_l31_save_states_a_failed_save_stays_on_the_document_until_a_save_succeeds() {
    let t0 = Instant::now();
    let (mut c, mut bridge) = (core(t0), WidgetBridge::new());
    let u = uri("/w/a.json");
    let g = open(&mut c, &mut bridge, t0, &u, "{}\n");
    type_text(&mut c, &mut bridge, t0, &u, 1, "x");
    let (_, token) = save(&mut c, t0, &u, g);
    let fx = c.handle(t0, Input::FileWritten { uri: u.clone(), token, result: Err("EACCES: permission denied".into()) });
    let actions = apply(&mut bridge, &fx);
    assert!(actions.contains(&BridgeAction::SaveFailed { uri: u.clone(), error: "EACCES: permission denied".into() }));
    let doc = bridge.document(&u).unwrap();
    assert_eq!(doc.save_error.as_deref(), Some("EACCES: permission denied"));
    assert!(doc.dirty, "a failed save keeps the edits");

    // 다시 시도: the same save again, this time it succeeds and the error goes.
    let (_, token) = save(&mut c, t0, &u, g);
    let fx = c.handle(t0, Input::FileWritten { uri: u.clone(), token, result: Ok(()) });
    apply(&mut bridge, &fx);
    let doc = bridge.document(&u).unwrap();
    assert_eq!(doc.save_error, None);
    assert!(!doc.dirty);
}

#[test]
fn fr_l31_save_states_a_clean_buffer_reloads_silently_and_a_dirty_one_gets_the_conflict() {
    let t0 = Instant::now();
    let (mut c, mut bridge) = (core(t0), WidgetBridge::new());
    let u = uri("/w/a.json");
    let g = open(&mut c, &mut bridge, t0, &u, "{}\n");

    // Clean: the new disk text replaces the buffer, nothing is asked.
    let fx = c.handle(t0, changed_on_disk(&u));
    assert_eq!(fx, vec![Effect::ReadFile { uri: u.clone(), purpose: ReadPurpose::Reload }]);
    let fx = c.handle(t0, Input::FileRead { uri: u.clone(), purpose: ReadPurpose::Reload, result: Ok(b"[1]\n".to_vec()) });
    let actions = apply(&mut bridge, &fx);
    assert!(actions.iter().all(|a| *a == BridgeAction::None), "{actions:?}");
    let doc = bridge.document(&u).unwrap();
    assert_eq!(doc.text, "[1]\n");
    assert_eq!(doc.external_change, None);
    let g2 = doc.generation;
    assert_ne!(g2, g);

    // Dirty: the buffer stays, the document carries the conflict for the window.
    type_text(&mut c, &mut bridge, t0, &u, 1, "x");
    c.handle(t0, changed_on_disk(&u));
    let fx = c.handle(t0, Input::FileRead { uri: u.clone(), purpose: ReadPurpose::Reload, result: Ok(b"[2]\n".to_vec()) });
    apply(&mut bridge, &fx);
    let doc = bridge.document(&u).unwrap();
    assert_eq!(doc.external_change, Some(ExternalChange::ChangedWhileDirty));
    assert_eq!(doc.text, "x[1]\n");

    // 디스크 버전으로 되돌리기: a forced reload takes the disk text and clears the conflict.
    let fx = c.handle(t0, Input::Reload { uri: u.clone() });
    assert_eq!(fx, vec![Effect::ReadFile { uri: u.clone(), purpose: ReadPurpose::ForceReload }]);
    let fx = c.handle(t0, Input::FileRead { uri: u.clone(), purpose: ReadPurpose::ForceReload, result: Ok(b"[2]\n".to_vec()) });
    apply(&mut bridge, &fx);
    let doc = bridge.document(&u).unwrap();
    assert_eq!(doc.text, "[2]\n");
    assert_eq!(doc.external_change, None);
    assert!(!doc.dirty);
}

#[test]
fn fr_l31_save_states_overwrite_after_a_conflict_writes_the_buffer_and_clears_it() {
    let t0 = Instant::now();
    let (mut c, mut bridge) = (core(t0), WidgetBridge::new());
    let u = uri("/w/a.json");
    let g = open(&mut c, &mut bridge, t0, &u, "{}\n");
    type_text(&mut c, &mut bridge, t0, &u, 1, "x");
    c.handle(t0, changed_on_disk(&u));
    let fx = c.handle(t0, Input::FileRead { uri: u.clone(), purpose: ReadPurpose::Reload, result: Ok(b"[2]\n".to_vec()) });
    apply(&mut bridge, &fx);
    assert_eq!(bridge.document(&u).unwrap().external_change, Some(ExternalChange::ChangedWhileDirty));

    // 덮어쓰기: the buffer is written over the disk version.
    let (bytes, token) = save(&mut c, t0, &u, g);
    assert_eq!(String::from_utf8(bytes).unwrap(), "x{}\n");
    let fx = c.handle(t0, Input::FileWritten { uri: u.clone(), token, result: Ok(()) });
    apply(&mut bridge, &fx);
    let doc = bridge.document(&u).unwrap();
    assert_eq!(doc.external_change, None);
    assert!(!doc.dirty);
}

#[test]
fn fr_l31_save_states_a_file_deleted_on_disk_is_marked_until_saved_again() {
    let t0 = Instant::now();
    let (mut c, mut bridge) = (core(t0), WidgetBridge::new());
    let u = uri("/w/a.json");
    let g = open(&mut c, &mut bridge, t0, &u, "{}\n");
    let fx = c.handle(t0, Input::FilesChanged(vec![FileChange { kind: FileChangeType::Deleted, resource: u.clone(), correlation_id: None }]));
    apply(&mut bridge, &fx);
    let doc = bridge.document(&u).unwrap();
    assert_eq!(doc.external_change, Some(ExternalChange::Deleted));
    assert_eq!(doc.text, "{}\n", "the buffer stays open, as in Code-OSS");

    // Saving writes the file again and the mark goes.
    let (_, token) = save(&mut c, t0, &u, g);
    let fx = c.handle(t0, Input::FileWritten { uri: u.clone(), token, result: Ok(()) });
    apply(&mut bridge, &fx);
    assert_eq!(bridge.document(&u).unwrap().external_change, None);
}

#[test]
fn fr_l31_save_states_a_restored_buffer_opens_the_file_and_is_dirty_with_the_file_line_ending() {
    let t0 = Instant::now();
    let (mut c, mut bridge) = (core(t0), WidgetBridge::new());
    let u = uri("/w/a.json");
    // After a reconnect the window restores its buffer of a file the new session has not opened.
    let fx = c.handle(t0, Input::Restore { uri: u.clone(), text: "{\n  \"mine\": 1\n}\n".into() });
    assert_eq!(fx, vec![Effect::ReadFile { uri: u.clone(), purpose: ReadPurpose::Open }]);
    let fx = c.handle(t0, Input::FileRead { uri: u.clone(), purpose: ReadPurpose::Open, result: Ok(b"{\r\n}\r\n".to_vec()) });
    let ups = updates(&fx);
    let opened = ups.iter().position(|u| matches!(u, SessionUpdate::Opened { .. })).expect("Opened");
    let set_text = ups
        .iter()
        .position(|u| matches!(u, SessionUpdate::Command { command: WidgetCommand::SetText { text, .. }, .. } if text == "{\n  \"mine\": 1\n}\n"))
        .expect("SetText with the restored text");
    assert!(opened < set_text);
    assert!(ups.contains(&&SessionUpdate::Dirty { uri: u.clone(), dirty: true }));
    apply(&mut bridge, &fx);
    let doc = bridge.document(&u).unwrap();
    assert!(doc.dirty);
    assert_eq!(doc.text, "{\n  \"mine\": 1\n}\n");

    // Saving writes the restored text with the file's own line ending.
    let (bytes, _) = save(&mut c, t0, &u, doc.generation);
    assert_eq!(String::from_utf8(bytes).unwrap(), "{\r\n  \"mine\": 1\r\n}\r\n");
}

#[test]
fn fr_l31_save_states_restoring_the_disk_text_leaves_the_document_clean() {
    let t0 = Instant::now();
    let (mut c, mut bridge) = (core(t0), WidgetBridge::new());
    let u = uri("/w/a.json");
    let g = open(&mut c, &mut bridge, t0, &u, "{}\n");
    // The same text: nothing happens.
    let fx = c.handle(t0, Input::Restore { uri: u.clone(), text: "{}\n".into() });
    assert!(buffer_updates(&fx).is_empty(), "{fx:#?}");
    assert_eq!(bridge.document(&u).unwrap().generation, g);
    // Other text on an open document: a new generation, dirty.
    let fx = c.handle(t0, Input::Restore { uri: u.clone(), text: "[]\n".into() });
    apply(&mut bridge, &fx);
    assert!(bridge.document(&u).unwrap().dirty);
    assert_ne!(bridge.document(&u).unwrap().generation, g);
    // Back to the disk text: clean again.
    let fx = c.handle(t0, Input::Restore { uri: u.clone(), text: "{}\n".into() });
    assert!(updates(&fx).contains(&&SessionUpdate::Dirty { uri: u.clone(), dirty: false }));
    apply(&mut bridge, &fx);
    assert!(!bridge.document(&u).unwrap().dirty);
}

#[test]
fn fr_l31_save_states_a_lost_connection_reaches_the_window() {
    let mut bridge = WidgetBridge::new();
    let action = bridge.apply(SessionUpdate::ConnectionLost { reason: "extension host disconnected".into() });
    assert_eq!(action, BridgeAction::ConnectionLost("extension host disconnected".into()));
}
