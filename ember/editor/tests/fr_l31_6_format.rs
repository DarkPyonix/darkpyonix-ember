//! FR-L31.6 (#453): line ending, encoding and language mode of an open document through the pure
//! session core. No server: the test feeds back what a server would answer.

use std::time::Instant;

use ember_editor::engine::ReadPurpose;
use ember_editor::languages::LanguageRegistry;
use ember_editor::{
    CoreOptions, DocumentFormat, Effect, Encoding, EncodingMode, Eol, Input, SessionCore, SessionUpdate, WidgetEvent,
};
use ember_editor_conn::exthost::Call;
use ember_editor_conn::rpc::{Arg, IncomingRequest, Reply};
use ember_editor_conn::uri::UriComponents;
use serde_json::{json, Value};

fn core(t0: Instant) -> SessionCore {
    let langs = LanguageRegistry::from_extensions(&[json!({"contributes": {"languages": [
        {"id": "json", "aliases": ["JSON"], "extensions": [".json"]},
        {"id": "markdown", "aliases": ["Markdown"], "extensions": [".md"]}
    ]}})]);
    SessionCore::new(langs, CoreOptions::default(), t0)
}

fn uri(path: &str) -> UriComponents {
    UriComponents::remote("127.0.0.1:9", path)
}

fn calls(fx: &[Effect]) -> Vec<&Call> {
    fx.iter().filter_map(|e| if let Effect::Rpc { call, .. } = e { Some(call) } else { None }).collect()
}

fn call<'a>(fx: &'a [Effect], method: &str) -> &'a Call {
    calls(fx).into_iter().find(|c| c.method == method).unwrap_or_else(|| panic!("no {method} in {fx:#?}"))
}

fn arg(c: &Call, i: usize) -> Value {
    c.args[i].as_json().cloned().unwrap_or(Value::Null)
}

fn updates(fx: &[Effect]) -> Vec<&SessionUpdate> {
    fx.iter().filter_map(|e| if let Effect::Ui(u) = e { Some(u) } else { None }).collect()
}

fn format(fx: &[Effect]) -> DocumentFormat {
    updates(fx)
        .into_iter()
        .rev()
        .find_map(|u| if let SessionUpdate::Format { format, .. } = u { Some(format.clone()) } else { None })
        .unwrap_or_else(|| panic!("no Format in {fx:#?}"))
}

fn open(c: &mut SessionCore, t0: Instant, path: &str, bytes: &[u8]) -> (Vec<Effect>, u64) {
    let u = uri(path);
    c.handle(t0, Input::Open { uri: u.clone() });
    let fx = c.handle(t0, Input::FileRead { uri: u, purpose: ReadPurpose::Open, result: Ok(bytes.to_vec()) });
    let generation = updates(&fx)
        .into_iter()
        .find_map(|u| if let SessionUpdate::Opened { generation, .. } = u { Some(*generation) } else { None })
        .expect("Opened");
    (fx, generation)
}

fn save(c: &mut SessionCore, t0: Instant, u: &UriComponents, generation: u64) -> Vec<u8> {
    let fx = c.handle(t0, Input::Widget { uri: u.clone(), generation, event: WidgetEvent::CodeSaveRequested { version: 0 } });
    match &fx[..] {
        [Effect::WriteFile { bytes, .. }] => bytes.clone(),
        other => panic!("{other:#?}"),
    }
}

#[test]
fn fr_l31_6_open_reports_eol_encoding_and_language_name() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let (fx, _) = open(&mut c, t0, "/w/a.json", b"{\r\n}\n");
    assert_eq!(
        format(&fx),
        DocumentFormat { eol: Eol::Crlf, encoding: Encoding::Utf8, language_id: "json".into(), language_name: "JSON".into() }
    );
    let (fx, _) = open(&mut c, t0, "/w/b.txt", b"\xef\xbb\xbfx");
    let f = format(&fx);
    assert_eq!((f.eol, f.encoding, f.language_name.as_str()), (Eol::Lf, Encoding::Utf8Bom, "Plain Text"));
}

#[test]
fn fr_l31_6_utf16_files_open_and_tell_the_extension_host_their_encoding() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let (fx, g) = open(&mut c, t0, "/w/u.json", b"\xff\xfe{\x00}\x00");
    assert!(updates(&fx).iter().any(|u| matches!(u, SessionUpdate::Opened { text, .. } if text == "{}")));
    assert_eq!(arg(call(&fx, "$acceptEncodingChanged"), 1), json!("utf16le"));
    assert_eq!(format(&fx).encoding, Encoding::Utf16Le);
    assert_eq!(save(&mut c, t0, &uri("/w/u.json"), g), b"\xff\xfe{\x00}\x00".to_vec());
    // Not in any supported encoding: refused, as before.
    let u = uri("/w/latin1.txt");
    c.handle(t0, Input::Open { uri: u.clone() });
    let fx = c.handle(t0, Input::FileRead { uri: u, purpose: ReadPurpose::Open, result: Ok(b"caf\xe9".to_vec()) });
    assert!(updates(&fx).iter().any(|u| matches!(u, SessionUpdate::OpenFailed { .. })));
}

#[test]
fn fr_l31_6_changing_the_eol_is_an_edit_that_makes_the_document_dirty() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let u = uri("/w/a.json");
    let (_, g) = open(&mut c, t0, "/w/a.json", b"{\n}\n");
    let fx = c.handle(t0, Input::SetEol { uri: u.clone(), eol: Eol::Crlf });
    let mc = call(&fx, "$acceptModelChanged");
    assert_eq!(arg(mc, 1)["isEolChange"], true);
    assert_eq!(arg(mc, 1)["eol"], "\r\n");
    assert_eq!(arg(mc, 1)["versionId"], 2);
    assert_eq!(arg(mc, 2), json!(true));
    assert!(updates(&fx).contains(&&SessionUpdate::Dirty { uri: u.clone(), dirty: true }));
    assert_eq!(format(&fx).eol, Eol::Crlf);
    assert!(calls(&fx).iter().any(|c| c.method == "$acceptEditorTabModel"), "the tab shows dirty to extensions");
    // No widget command: the widget's text has `\n` either way.
    assert!(!updates(&fx).iter().any(|u| matches!(u, SessionUpdate::Command { .. })));
    // The same EOL again does nothing.
    assert!(c.handle(t0, Input::SetEol { uri: u.clone(), eol: Eol::Crlf }).is_empty());
    // The widget's next keystroke is the next extension-host version.
    let fx = c.handle(
        t0,
        Input::Widget {
            uri: u.clone(),
            generation: g,
            event: WidgetEvent::CodeChanged { version: 1, range: ember_editor::WidgetRange::empty(ember_editor::WidgetPos::new(0, 1)), text: " ".into() },
        },
    );
    assert_eq!(arg(call(&fx, "$acceptModelChanged"), 1)["versionId"], 3);
    assert_eq!(save(&mut c, t0, &u, g), b"{ \r\n}\r\n".to_vec());
}

#[test]
fn fr_l31_6_save_with_encoding_writes_in_it() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let u = uri("/w/a.json");
    open(&mut c, t0, "/w/a.json", b"hi");
    let fx = c.handle(t0, Input::SetEncoding { uri: u.clone(), encoding: Encoding::Utf16Be, mode: EncodingMode::Save });
    assert_eq!(arg(call(&fx, "$acceptEncodingChanged"), 1), json!("utf16be"));
    assert_eq!(format(&fx).encoding, Encoding::Utf16Be);
    let written = fx.iter().find_map(|e| if let Effect::WriteFile { bytes, .. } = e { Some(bytes.clone()) } else { None });
    assert_eq!(written, Some(b"\xfe\xff\x00h\x00i".to_vec()));
}

#[test]
fn fr_l31_6_reopen_with_encoding_reads_the_file_again_in_it() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let u = uri("/w/a.json");
    open(&mut c, t0, "/w/a.json", b"h\x00i\x00");
    // The same encoding: nothing to do (Code-OSS `isNewEncoding`).
    assert!(c.handle(t0, Input::SetEncoding { uri: u.clone(), encoding: Encoding::Utf8, mode: EncodingMode::Reopen }).is_empty());
    let fx = c.handle(t0, Input::SetEncoding { uri: u.clone(), encoding: Encoding::Utf16Le, mode: EncodingMode::Reopen });
    assert_eq!(fx, vec![Effect::ReadFile { uri: u.clone(), purpose: ReadPurpose::ForceReload }]);
    let fx = c.handle(t0, Input::FileRead { uri: u.clone(), purpose: ReadPurpose::ForceReload, result: Ok(b"h\x00i\x00".to_vec()) });
    assert!(updates(&fx).iter().any(|x| matches!(x, SessionUpdate::Command { command: ember_editor::WidgetCommand::SetText { text, .. }, .. } if text == "hi")));
    let delta = calls(&fx).into_iter().filter(|c| c.method == "$acceptDocumentsAndEditorsDelta").map(|c| arg(c, 0)).last().unwrap();
    assert_eq!(delta["addedDocuments"][0]["encoding"], "utf16le");
    assert_eq!(format(&fx).encoding, Encoding::Utf16Le);
    assert!(updates(&fx).contains(&&SessionUpdate::Dirty { uri: u.clone(), dirty: false }));

    // Bytes that do not fit the chosen encoding: the document stays as it is.
    let fx = c.handle(t0, Input::SetEncoding { uri: u.clone(), encoding: Encoding::Utf16Be, mode: EncodingMode::Reopen });
    assert_eq!(fx.len(), 1);
    let fx = c.handle(t0, Input::FileRead { uri: u.clone(), purpose: ReadPurpose::ForceReload, result: Ok(b"odd".to_vec()) });
    assert!(matches!(updates(&fx)[..], [SessionUpdate::FormatFailed { .. }]), "{fx:#?}");
    assert!(calls(&fx).is_empty());
}

#[test]
fn fr_l31_6_language_mode_change_reannounces_the_language() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let u = uri("/w/notes.json");
    open(&mut c, t0, "/w/notes.json", b"# hi");
    let fx = c.handle(t0, Input::SetLanguage { uri: u.clone(), language_id: Some("markdown".into()) });
    assert_eq!(arg(call(&fx, "$activateByEvent"), 0), json!("onLanguage:markdown"));
    let lc = call(&fx, "$acceptModelLanguageChanged");
    assert_eq!(lc.proxy, "ExtHostDocuments");
    assert_eq!(arg(lc, 0)["path"], "/w/notes.json");
    assert_eq!(arg(lc, 1), json!("markdown"));
    let f = format(&fx);
    assert_eq!((f.language_id.as_str(), f.language_name.as_str()), ("markdown", "Markdown"));
    // Auto detect: back to what the path says.
    let fx = c.handle(t0, Input::SetLanguage { uri: u.clone(), language_id: None });
    assert_eq!(arg(call(&fx, "$acceptModelLanguageChanged"), 1), json!("json"));
    assert!(c.handle(t0, Input::SetLanguage { uri: u, language_id: Some("json".into()) }).iter().all(|e| !matches!(e, Effect::Rpc { .. })));
    assert_eq!(c.language_list().iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["JSON", "Markdown", "Plain Text"]);
}

#[test]
fn fr_l31_6_extensions_set_the_language_with_change_language() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let u = uri("/w/a.json");
    open(&mut c, t0, "/w/a.json", b"{}");
    let req = |req: u32, id: &str| {
        Input::ExtHostRequest(IncomingRequest {
            req,
            rpc_id: ember_editor_conn::rpc_ids::id_of("MainThreadLanguages").unwrap(),
            proxy: Some("MainThreadLanguages"),
            method: "$changeLanguage".into(),
            args: vec![Arg::Json(serde_json::to_value(&u).unwrap()), Arg::Json(json!(id))],
            cancellable: false,
        })
    };
    let fx = c.handle(t0, req(1, "markdown"));
    assert_eq!(arg(call(&fx, "$acceptModelLanguageChanged"), 1), json!("markdown"));
    assert!(fx.contains(&Effect::Respond { req: 1, result: Ok(Reply::Empty) }));
    let fx = c.handle(t0, req(2, "cobol"));
    assert!(fx.iter().any(|e| matches!(e, Effect::Respond { req: 2, result: Err(Some(_)) })), "unknown id refused");
}
