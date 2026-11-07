//! FR-E7: search across files and replace, through the pure session core (no server). The test plays the
//! driver and the extension host: it answers the core's calls the way the OSE's search provider does.

use std::time::Instant;

use ember_editor::engine::{ReadPurpose, Token};
use ember_editor::languages::LanguageRegistry;
use ember_editor::search::{ReplaceRequest, SearchEnd, SearchOptions, SearchRequest, Target};
use ember_editor::{CoreOptions, Effect, Input, SessionCore, SessionUpdate, WidgetCommand, WidgetEvent, WidgetPos, WidgetRange};
use ember_editor_conn::exthost::Call;
use ember_editor_conn::rpc::{Arg, IncomingRequest, Reply};
use ember_editor_conn::uri::UriComponents;
use serde_json::{json, Value};

const AUTH: &str = "127.0.0.1:9";

fn core(t0: Instant) -> SessionCore {
    let langs = LanguageRegistry::from_extensions(&[json!({"contributes": {"languages": [{"id": "rust", "extensions": [".rs"]}]}})]);
    SessionCore::new(langs, CoreOptions::default(), t0)
}

fn uri(path: &str) -> UriComponents {
    UriComponents::remote(AUTH, path)
}

fn request(req: u32, method: &str, args: Vec<Value>) -> Input {
    Input::ExtHostRequest(IncomingRequest {
        req,
        rpc_id: ember_editor_conn::rpc_ids::id_of("MainThreadSearch").unwrap(),
        proxy: Some("MainThreadSearch"),
        method: method.into(),
        args: args.into_iter().map(Arg::Json).collect(),
        cancellable: false,
    })
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

fn search_call(effects: &[Effect]) -> (Token, &Call) {
    effects
        .iter()
        .find_map(|e| match e {
            Effect::Rpc { token: Some(t), call } if call.method == "$provideTextSearchResults" => Some((*t, call)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no search call in {effects:#?}"))
}

fn arg(c: &Call, i: usize) -> Value {
    c.args[i].as_json().cloned().unwrap_or(Value::Null)
}

fn req(pattern: &str) -> SearchRequest {
    SearchRequest {
        options: SearchOptions { pattern: pattern.into(), regex: false, case_sensitive: true, whole_word: false },
        include: String::new(),
        exclude: String::new(),
        use_ignore_files: true,
    }
}

fn file_match(path: &str, line: u32, start: u32, end: u32, preview: &str) -> Value {
    json!({
        "resource": {"$mid": 1, "scheme": "vscode-remote", "authority": AUTH, "path": path},
        "results": [{"previewText": preview, "rangeLocations": [{
            "source": {"startLineNumber": line, "startColumn": start, "endLineNumber": line, "endColumn": end},
            "preview": {"startLineNumber": 0, "startColumn": start, "endLineNumber": 0, "endColumn": end}
        }]}]
    })
}

/// A core whose extension host registered its providers like the real one: `vscode-userdata` first, then
/// the workspace scheme.
fn with_provider(t0: Instant) -> SessionCore {
    let mut c = core(t0);
    let fx = c.handle(t0, request(1, "$registerTextSearchProvider", vec![json!(0), json!("vscode-userdata")]));
    assert!(fx.contains(&Effect::Respond { req: 1, result: Ok(Reply::Empty) }));
    c.handle(t0, request(2, "$registerTextSearchProvider", vec![json!(1), json!("vscode-remote")]));
    c
}

fn open(core: &mut SessionCore, now: Instant, path: &str, text: &str) -> u64 {
    let u = uri(path);
    core.handle(now, Input::Open { uri: u.clone() });
    let fx = core.handle(now, Input::FileRead { uri: u, purpose: ReadPurpose::Open, result: Ok(text.as_bytes().to_vec()) });
    updates(&fx)
        .into_iter()
        .find_map(|u| match u {
            SessionUpdate::Opened { generation, .. } => Some(*generation),
            _ => None,
        })
        .expect("Opened")
}

#[test]
fn fr_e7_search_before_the_provider_registers_says_unavailable() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let fx = c.handle(t0, Input::Search { id: 1, request: req("foo"), folder: uri("/w") });
    assert_eq!(updates(&fx), vec![&SessionUpdate::SearchDone { id: 1, end: SearchEnd::Unavailable }]);
}

#[test]
fn fr_e7_search_streams_matches_and_ends_with_the_limit_flag() {
    let t0 = Instant::now();
    let mut c = with_provider(t0);
    let fx = c.handle(t0, Input::Search { id: 7, request: req("foo"), folder: uri("/w") });
    let (token, call) = search_call(&fx);
    assert!(call.cancellable);
    assert_eq!(arg(call, 0), json!(1), "the remote provider's handle");
    let session = arg(call, 1);
    assert_eq!(arg(call, 2)["contentPattern"]["pattern"], "foo");
    assert_eq!(arg(call, 2)["folderQueries"][0]["folder"]["path"], "/w");
    assert_eq!(arg(call, 2)["maxResults"], 2000);

    let fx = c.handle(t0, request(10, "$handleTextMatch", vec![json!(1), session.clone(), json!([file_match("/w/a.rs", 2, 4, 7, "let foo;")])]));
    assert!(fx.contains(&Effect::Respond { req: 10, result: Ok(Reply::Empty) }));
    match updates(&fx).as_slice() {
        [SessionUpdate::SearchHits { id: 7, files }] => {
            assert_eq!(files[0].uri.path, "/w/a.rs");
            assert_eq!((files[0].hits[0].line, files[0].hits[0].start, files[0].hits[0].end), (2, 4, 7));
        }
        other => panic!("{other:?}"),
    }
    // Matches of another provider session are not this search's.
    let fx = c.handle(t0, request(11, "$handleTextMatch", vec![json!(1), json!(999), json!([file_match("/w/b.rs", 0, 0, 3, "foo")])]));
    assert!(updates(&fx).is_empty());

    let fx = c.handle(t0, Input::RpcReply { token, result: Ok(json!({"limitHit": true, "messages": []})) });
    assert_eq!(updates(&fx), vec![&SessionUpdate::SearchDone { id: 7, end: SearchEnd::Complete { limit_hit: true } }]);
}

#[test]
fn fr_e7_cancel_stops_the_call_and_drops_later_matches() {
    let t0 = Instant::now();
    let mut c = with_provider(t0);
    let fx = c.handle(t0, Input::Search { id: 3, request: req("foo"), folder: uri("/w") });
    let (token, call) = search_call(&fx);
    let session = arg(call, 1);
    let fx = c.handle(t0, Input::CancelSearch { id: 3 });
    assert!(fx.contains(&Effect::Cancel(token)));
    assert_eq!(updates(&fx), vec![&SessionUpdate::SearchDone { id: 3, end: SearchEnd::Cancelled }]);
    let fx = c.handle(t0, request(12, "$handleTextMatch", vec![json!(1), session, json!([file_match("/w/a.rs", 0, 0, 3, "foo")])]));
    assert!(updates(&fx).is_empty());
}

#[test]
fn fr_e7_a_new_search_cancels_the_running_one() {
    let t0 = Instant::now();
    let mut c = with_provider(t0);
    let fx = c.handle(t0, Input::Search { id: 1, request: req("foo"), folder: uri("/w") });
    let (first, _) = search_call(&fx);
    let fx = c.handle(t0, Input::Search { id: 2, request: req("bar"), folder: uri("/w") });
    assert!(fx.contains(&Effect::Cancel(first)));
    assert!(updates(&fx).contains(&&SessionUpdate::SearchDone { id: 1, end: SearchEnd::Cancelled }));
    let (_, call) = search_call(&fx);
    assert_eq!(arg(call, 2)["contentPattern"]["pattern"], "bar");
}

#[test]
fn fr_e7_a_provider_failure_is_reported() {
    let t0 = Instant::now();
    let mut c = with_provider(t0);
    let fx = c.handle(t0, Input::Search { id: 4, request: req("("), folder: uri("/w") });
    let (token, _) = search_call(&fx);
    let fx = c.handle(t0, Input::RpcReply { token, result: Err("remote error: regex parse error".into()) });
    assert_eq!(updates(&fx), vec![&SessionUpdate::SearchDone { id: 4, end: SearchEnd::Failed("remote error: regex parse error".into()) }]);
}

#[test]
fn fr_e7_unsaved_buffers_are_searched_instead_of_the_disk() {
    let t0 = Instant::now();
    let mut c = with_provider(t0);
    let g = open(&mut c, t0, "/w/a.rs", "let x = 1;\n");
    let u = uri("/w/a.rs");
    // Type "foo" at the start: the buffer now has a match the disk does not.
    c.handle(t0, Input::Widget { uri: u.clone(), generation: g, event: WidgetEvent::CodeChanged { version: 1, range: WidgetRange::empty(WidgetPos::new(0, 0)), text: "foo ".into() } });
    let fx = c.handle(t0, Input::Search { id: 5, request: req("foo"), folder: uri("/w") });
    let local = updates(&fx)
        .into_iter()
        .find_map(|u| match u {
            SessionUpdate::SearchHits { id: 5, files } => Some(files.clone()),
            _ => None,
        })
        .expect("hits from the buffer");
    assert_eq!(local[0].uri, u);
    assert_eq!((local[0].hits[0].line, local[0].hits[0].start, local[0].hits[0].end), (0, 0, 3));
    let (_, call) = search_call(&fx);
    let session = arg(call, 1);
    // The provider's match for the same file is the disk's: dropped. Another file's is kept.
    let fx = c.handle(
        t0,
        request(13, "$handleTextMatch", vec![json!(1), session, json!([file_match("/w/a.rs", 9, 0, 3, "foo"), file_match("/w/b.rs", 1, 0, 3, "foo")])]),
    );
    match updates(&fx).as_slice() {
        [SessionUpdate::SearchHits { files, .. }] => {
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].uri.path, "/w/b.rs");
        }
        other => panic!("{other:?}"),
    }
}

fn replace_req(path: &str, pattern: &str, replacement: &str, targets: Vec<Target>) -> ReplaceRequest {
    ReplaceRequest {
        options: SearchOptions { pattern: pattern.into(), regex: false, case_sensitive: true, whole_word: false },
        replacement: replacement.into(),
        files: vec![(uri(path), targets)],
    }
}

fn write_of(effects: &[Effect]) -> (Token, Vec<u8>) {
    effects
        .iter()
        .find_map(|e| match e {
            Effect::WriteFile { bytes, token, .. } => Some((*token, bytes.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no write in {effects:#?}"))
}

#[test]
fn fr_e7_replace_in_a_closed_file_keeps_bom_and_line_endings() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let u = uri("/w/a.rs");
    let fx = c.handle(t0, Input::Replace { id: 1, request: replace_req("/w/a.rs", "foo", "bar", vec![Target { line: 1, start: 0, end: 3 }]) });
    assert_eq!(fx, vec![Effect::ReadFile { uri: u.clone(), purpose: ReadPurpose::Replace { id: 1 } }]);
    let fx = c.handle(t0, Input::FileRead { uri: u.clone(), purpose: ReadPurpose::Replace { id: 1 }, result: Ok("\u{feff}x\r\nfoo\r\n".as_bytes().to_vec()) });
    let (token, bytes) = write_of(&fx);
    assert_eq!(bytes, "\u{feff}x\r\nbar\r\n".as_bytes());
    let fx = c.handle(t0, Input::FileWritten { uri: u, token, result: Ok(()) });
    match updates(&fx).as_slice() {
        [SessionUpdate::ReplaceDone { id: 1, report }] => {
            assert_eq!((report.replaced, report.skipped, report.files), (1, 0, 1));
            assert!(report.failures.is_empty());
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn fr_e7_replace_skips_a_match_that_moved_since_the_search() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let u = uri("/w/a.rs");
    c.handle(t0, Input::Replace { id: 2, request: replace_req("/w/a.rs", "foo", "bar", vec![Target { line: 0, start: 0, end: 3 }]) });
    let fx = c.handle(t0, Input::FileRead { uri: u, purpose: ReadPurpose::Replace { id: 2 }, result: Ok(b"  foo\n".to_vec()) });
    assert!(!fx.iter().any(|e| matches!(e, Effect::WriteFile { .. })), "nothing to write");
    match updates(&fx).as_slice() {
        [SessionUpdate::ReplaceDone { id: 2, report }] => assert_eq!((report.replaced, report.skipped, report.files), (0, 1, 0)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn fr_e7_replace_failure_names_the_file() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let u = uri("/w/a.rs");
    c.handle(t0, Input::Replace { id: 3, request: replace_req("/w/a.rs", "foo", "bar", vec![Target { line: 0, start: 0, end: 3 }]) });
    let fx = c.handle(t0, Input::FileRead { uri: u.clone(), purpose: ReadPurpose::Replace { id: 3 }, result: Ok(b"foo\n".to_vec()) });
    let (token, _) = write_of(&fx);
    let fx = c.handle(t0, Input::FileWritten { uri: u, token, result: Err("NoPermissions".into()) });
    match updates(&fx).as_slice() {
        [SessionUpdate::ReplaceDone { id: 3, report }] => {
            assert_eq!((report.replaced, report.skipped, report.files), (0, 1, 0));
            assert_eq!(report.failures, vec![("/w/a.rs".to_string(), "NoPermissions".to_string())]);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn fr_e7_an_open_document_takes_the_new_text_only_after_the_write() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let _ = open(&mut c, t0, "/w/a.rs", "foo\n");
    let u = uri("/w/a.rs");
    let fx = c.handle(t0, Input::Replace { id: 4, request: replace_req("/w/a.rs", "foo", "bar", vec![Target { line: 0, start: 0, end: 3 }]) });
    let (token, bytes) = write_of(&fx);
    assert_eq!(bytes, b"bar\n");
    assert!(!updates(&fx).iter().any(|x| matches!(x, SessionUpdate::Command { .. })), "the buffer is untouched until the write succeeds");
    assert_eq!(c.widget_text(&u).as_deref(), Some("foo\n"));
    let fx = c.handle(t0, Input::FileWritten { uri: u.clone(), token, result: Ok(()) });
    assert!(updates(&fx).iter().any(|x| matches!(x, SessionUpdate::Command { command: WidgetCommand::SetText { text, .. }, .. } if text == "bar\n")));
    assert!(updates(&fx).contains(&&SessionUpdate::Dirty { uri: u.clone(), dirty: false }));
    assert!(updates(&fx).iter().any(|x| matches!(x, SessionUpdate::ReplaceDone { id: 4, report } if report.replaced == 1)));
    assert_eq!(c.widget_text(&u).as_deref(), Some("bar\n"));
}
