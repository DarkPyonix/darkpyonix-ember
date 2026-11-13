//! FR-E8: the session side of extension webviews. One record per handle, the panel lifecycle, the
//! message queue and its order, the local resource rule, the state round trip, the theme variables,
//! and the flow through a fake webview host. No server and no webview: the model is pure and the host
//! is [`FakeHost`].

use std::time::Instant;

use ember_editor::engine::ReadPurpose;
use ember_editor::languages::LanguageRegistry;
use ember_editor::webview::host::{FakeHost, HostCall, HostEvent, PaneStatus, WebviewDriver, WebviewPanes, WAITING_TEXT};
use ember_editor::webview::resource::{self, Decision, ResourceResponse};
use ember_editor::webview::theme::{self, Fonts, ThemeKind};
use ember_editor::webview::{shim, Out, Pending, WebviewEvent, WebviewKind, WebviewUpdate, Webviews};
use ember_editor::{BridgeAction, CoreOptions, Effect, Input, SessionCore, SessionUpdate, WidgetBridge};
use ember_editor_conn::exthost::Call;
use ember_editor_conn::rpc::{Arg, IncomingRequest, Reply};
use ember_editor_conn::uri::UriComponents;
use ember_editor_conn::webview::{
    CommandUris, ContentOptions, ExtensionDescription, PanelOptions, ShowOptions, WebviewCall, WebviewMessage,
};
use serde_json::{json, Value};

fn ext() -> ExtensionDescription {
    ExtensionDescription { id: "pub.cat".into(), location: UriComponents::remote("h", "/x/cat") }
}

fn roots() -> ContentOptions {
    ContentOptions {
        enable_scripts: true,
        local_resource_roots: Some(vec![UriComponents::remote("h", "/x/cat/media"), UriComponents::remote("h", "/w")]),
        ..Default::default()
    }
}

fn create(handle: &str, retain: bool) -> WebviewCall {
    WebviewCall::CreatePanel {
        extension: ext(),
        handle: handle.into(),
        view_type: "catCoding".into(),
        title: "Cat Coding".into(),
        content_options: roots(),
        panel_options: PanelOptions { enable_find_widget: false, retain_context_when_hidden: retain },
        serialize_buffers: true,
        show: ShowOptions { view_column: None, preserve_focus: false },
    }
}

fn msg(s: &str) -> WebviewMessage {
    WebviewMessage::text(s)
}

fn updates(out: &[Out]) -> Vec<&WebviewUpdate> {
    out.iter().filter_map(|o| if let Out::Ui(u) = o { Some(u) } else { None }).collect()
}

fn rpcs(out: &[Out]) -> Vec<&Call> {
    out.iter()
        .filter_map(|o| match o {
            Out::Rpc(c) | Out::RpcWith(c, _) => Some(c),
            _ => None,
        })
        .collect()
}

fn posts(out: &[Out]) -> Vec<String> {
    updates(out)
        .into_iter()
        .filter_map(|u| if let WebviewUpdate::Post { message, .. } = u { Some(message.json.clone()) } else { None })
        .collect()
}

fn respond(out: &[Out]) -> Option<(u32, Reply)> {
    out.iter().find_map(|o| if let Out::Respond { req, reply } = o { Some((*req, reply.clone())) } else { None })
}

#[test]
fn fr_e8_panel_lifecycle_create_title_reveal_dispose() {
    let mut w = Webviews::new();
    let out = w.on_call(1, create("p-1", false));
    assert_eq!(respond(&out), Some((1, Reply::Empty)));
    match updates(&out)[..] {
        [WebviewUpdate::Created { handle, kind, title, content, reveal, .. }] => {
            assert_eq!(handle, "p-1");
            assert_eq!(kind, &WebviewKind::Panel { view_type: "catCoding".into() });
            assert_eq!(title, "Cat Coding");
            assert!(content.enable_scripts);
            assert!(reveal.is_some());
        }
        ref other => panic!("{other:?}"),
    }
    let r = w.get("p-1").unwrap();
    assert!(r.visible && r.active && !r.ready);

    let out = w.on_call(2, WebviewCall::SetHtml { handle: "p-1".into(), html: "<h1>cat</h1>".into() });
    assert!(matches!(updates(&out)[..], [WebviewUpdate::Html { handle, html, state: None }] if handle == "p-1" && html == "<h1>cat</h1>"));
    let out = w.on_call(3, WebviewCall::SetTitle { handle: "p-1".into(), title: "고양이".into() });
    assert!(matches!(updates(&out)[..], [WebviewUpdate::Title { title, .. }] if title == "고양이"));
    assert_eq!(w.get("p-1").unwrap().title, "고양이");
    let out = w.on_call(4, WebviewCall::Reveal { handle: "p-1".into(), show: ShowOptions { view_column: Some(1), preserve_focus: true } });
    assert!(matches!(updates(&out)[..], [WebviewUpdate::Reveal { preserve_focus: true, view_column: Some(1), .. }]));

    w.on_call(5, create("p-2", false));

    let out = w.on_call(6, WebviewCall::DisposePanel { handle: "p-1".into() });
    assert!(w.get("p-1").is_none());
    assert_eq!(rpcs(&out).iter().map(|c| c.method).collect::<Vec<_>>(), vec!["$onDidDisposeWebviewPanel"]);
    assert!(matches!(updates(&out)[..], [WebviewUpdate::Disposed { handle }] if handle == "p-1"));
    // Calls for a gone handle change nothing and are still answered.
    let out = w.on_call(7, WebviewCall::SetHtml { handle: "p-1".into(), html: "x".into() });
    assert!(updates(&out).is_empty());
    assert_eq!(respond(&out), Some((7, Reply::Empty)));
}

#[test]
fn fr_e8_user_closing_a_panel_tells_the_extension_host() {
    let mut w = Webviews::new();
    w.on_call(1, create("p-1", false));
    let out = w.on_event(WebviewEvent::Closed { handle: "p-1".into() });
    assert_eq!(rpcs(&out)[0].method, "$onDidDisposeWebviewPanel");
    assert_eq!(rpcs(&out)[0].args[0], Arg::Json("p-1".into()));
    assert!(matches!(updates(&out)[..], [WebviewUpdate::Disposed { .. }]));
    assert!(w.records().next().is_none());
}

#[test]
fn fr_e8_messages_queue_until_ready_and_keep_their_order() {
    let mut w = Webviews::new();
    w.on_call(1, create("p-1", false));
    w.on_call(2, WebviewCall::SetHtml { handle: "p-1".into(), html: "<p>".into() });
    for (i, m) in ["1", "2", "3"].iter().enumerate() {
        let out = w.on_call(10 + i as u32, WebviewCall::PostMessage { handle: "p-1".into(), message: msg(m) });
        assert!(posts(&out).is_empty(), "posted before ready");
        assert_eq!(respond(&out).unwrap().1, Reply::Json(Value::Bool(true)));
    }
    assert_eq!(w.get("p-1").unwrap().queued().count(), 3);
    let out = w.on_event(WebviewEvent::Ready { handle: "p-1".into() });
    assert!(matches!(updates(&out)[0], WebviewUpdate::Ready { .. }));
    assert_eq!(posts(&out), vec!["1", "2", "3"]);
    assert_eq!(w.get("p-1").unwrap().queued().count(), 0);
    // Ready: straight through.
    let out = w.on_call(20, WebviewCall::PostMessage { handle: "p-1".into(), message: msg("4") });
    assert_eq!(posts(&out), vec!["4"]);
    // `$setHtml` reloads the page: queue again until it is ready.
    w.on_call(21, WebviewCall::SetHtml { handle: "p-1".into(), html: "<p>2".into() });
    let out = w.on_call(22, WebviewCall::PostMessage { handle: "p-1".into(), message: msg("5") });
    assert!(posts(&out).is_empty());
    let out = w.on_event(WebviewEvent::Ready { handle: "p-1".into() });
    assert_eq!(posts(&out), vec!["5"]);
    // An unknown handle answers false.
    let out = w.on_call(23, WebviewCall::PostMessage { handle: "nope".into(), message: msg("x") });
    assert_eq!(respond(&out).unwrap().1, Reply::Json(Value::Bool(false)));
}

#[test]
fn fr_e8_hidden_page_without_retain_queues_until_shown_and_ready() {
    let mut w = Webviews::new();
    w.on_call(1, create("keep", true));
    w.on_call(2, create("drop", false));
    w.on_event(WebviewEvent::Ready { handle: "keep".into() });
    w.on_event(WebviewEvent::Ready { handle: "drop".into() });
    w.on_event(WebviewEvent::ViewState { handle: "keep".into(), visible: false, active: false, position: 0 });
    w.on_event(WebviewEvent::ViewState { handle: "drop".into(), visible: false, active: false, position: 0 });
    let out = w.on_call(3, WebviewCall::PostMessage { handle: "keep".into(), message: msg("k") });
    assert_eq!(posts(&out), vec!["k"], "a retained page keeps receiving while hidden");
    let out = w.on_call(4, WebviewCall::PostMessage { handle: "drop".into(), message: msg("d") });
    assert!(posts(&out).is_empty(), "a dropped page waits");
    let out = w.on_event(WebviewEvent::Ready { handle: "drop".into() });
    assert_eq!(posts(&out), vec!["d"]);
}

#[test]
fn fr_e8_page_messages_go_to_the_extension_host_in_order() {
    let mut w = Webviews::new();
    w.on_call(1, create("p-1", false));
    let mut methods = Vec::new();
    for i in 0..3 {
        let m = WebviewMessage { json: format!("{{\"i\":{i}}}"), buffers: vec![vec![i as u8]] };
        let out = w.on_event(WebviewEvent::Message { handle: "p-1".into(), message: m });
        let c = rpcs(&out)[0].clone();
        assert_eq!((c.proxy, c.method), ("ExtHostWebviews", "$onMessage"));
        assert_eq!(c.args[2], Arg::JsonWithBuffers { value: json!([{"$$ref$$": 0}]), buffers: vec![vec![i as u8]] });
        methods.push(c.args[1].clone());
    }
    assert_eq!(methods, vec![Arg::Json(json!("{\"i\":0}")), Arg::Json(json!("{\"i\":1}")), Arg::Json(json!("{\"i\":2}"))]);
}

#[test]
fn fr_e8_view_states_go_to_the_extension_host_for_every_panel() {
    let mut w = Webviews::new();
    w.on_call(1, create("a", false));
    w.on_call(2, create("b", false));
    let out = w.on_event(WebviewEvent::ViewState { handle: "b".into(), visible: true, active: true, position: 1 });
    let c = rpcs(&out)[0];
    assert_eq!(c.method, "$onDidChangeWebviewPanelViewStates");
    let states = c.args[0].as_json().unwrap();
    assert_eq!(states["a"], json!({"active": false, "visible": true, "position": 0}));
    assert_eq!(states["b"], json!({"active": true, "visible": true, "position": 1}));
    // Nothing changed: nothing sent.
    let out = w.on_event(WebviewEvent::ViewState { handle: "b".into(), visible: true, active: true, position: 1 });
    assert!(rpcs(&out).is_empty());
}

#[test]
fn fr_e8_resource_rule_serves_only_under_local_resource_roots() {
    let roots = roots();
    let r = roots.roots();
    let url = resource::as_webview_uri(&UriComponents::remote("127.0.0.1:9", "/x/cat/media/cat.gif"));
    assert_eq!(url, "https://vscode-remote+127-002e0-002e0-002e1-003a9.vscode-resource.vscode-cdn.net/x/cat/media/cat.gif");
    let parsed = resource::parse_resource_url(&url).unwrap();
    assert_eq!(parsed, UriComponents::remote("127.0.0.1:9", "/x/cat/media/cat.gif"));

    let base = "https://vscode-remote+h.vscode-resource.vscode-cdn.net";
    assert_eq!(resource::check(&format!("{base}/x/cat/media/cat.gif?v=2"), r), Decision::Allowed(UriComponents::remote("h", "/x/cat/media/cat.gif")));
    assert_eq!(resource::check(&format!("{base}/w/src/%EA%B0%80.png"), r), Decision::Allowed(UriComponents::remote("h", "/w/src/가.png")));
    // Outside, a root itself, a climb out with `..`, a prefix that is not a parent, another authority or scheme.
    assert_eq!(resource::check(&format!("{base}/x/cat/package.json"), r), Decision::Denied);
    assert_eq!(resource::check(&format!("{base}/w"), r), Decision::Denied);
    assert_eq!(resource::check(&format!("{base}/x/cat/media/../secret.txt"), r), Decision::Denied);
    assert_eq!(resource::check(&format!("{base}/w/%2e%2e/etc/passwd"), r), Decision::Denied);
    assert_eq!(resource::check(&format!("{base}/../../etc/passwd"), r), Decision::Denied);
    assert_eq!(resource::check(&format!("{base}/wx/a.png"), r), Decision::Denied);
    assert_eq!(resource::check("https://vscode-remote+other.vscode-resource.vscode-cdn.net/w/a.png", r), Decision::Denied);
    assert_eq!(resource::check("https://file+.vscode-resource.vscode-cdn.net/w/a.png", r), Decision::Denied);
    assert_eq!(resource::check("https://example.com/w/a.png", r), Decision::Denied);
    assert_eq!(resource::check(&format!("{base}/w/a.png"), &[]), Decision::Denied);
    // `.` and backslashes resolve inside the root.
    assert_eq!(resource::check(&format!("{base}/w/./sub\\a.css"), r), Decision::Allowed(UriComponents::remote("h", "/w/sub/a.css")));
    assert_eq!(resource::mime_type("/w/a.css"), "text/css");
    assert_eq!(resource::mime_type("/w/a.JS"), "text/javascript");
    assert_eq!(resource::mime_type("/w/a.bin"), "application/octet-stream");
}

#[test]
fn fr_e8_resource_requests_read_the_file_or_are_refused() {
    let mut w = Webviews::new();
    w.on_call(1, create("p-1", false));
    let out = w.on_event(WebviewEvent::ResourceRequested {
        handle: "p-1".into(),
        request: 7,
        url: "https://vscode-remote+h.vscode-resource.vscode-cdn.net/x/cat/media/cat.svg".into(),
    });
    let uri = match &out[..] {
        [Out::ReadFile { uri, handle, request: 7 }] if handle == "p-1" => uri.clone(),
        other => panic!("{other:?}"),
    };
    let out = w.on_resource_read("p-1".into(), 7, &uri, Ok(b"<svg/>".to_vec()));
    assert!(matches!(updates(&out)[..], [WebviewUpdate::Resource { request: 7, response: ResourceResponse::Ok { bytes, mime }, .. }] if bytes == b"<svg/>" && mime == "image/svg+xml"));
    let out = w.on_resource_read("p-1".into(), 8, &uri, Err("gone".into()));
    assert!(matches!(updates(&out)[..], [WebviewUpdate::Resource { request: 8, response: ResourceResponse::Failed, .. }]));

    let out = w.on_event(WebviewEvent::ResourceRequested {
        handle: "p-1".into(),
        request: 9,
        url: "https://vscode-remote+h.vscode-resource.vscode-cdn.net/etc/passwd".into(),
    });
    assert!(matches!(updates(&out)[..], [WebviewUpdate::Resource { request: 9, response: ResourceResponse::Denied, .. }]));
    assert_eq!(ResourceResponse::Denied.status(), 401);

    // `$setOptions` changes the roots that apply.
    w.on_call(2, WebviewCall::SetOptions { handle: "p-1".into(), options: ContentOptions::default() });
    let out = w.on_event(WebviewEvent::ResourceRequested {
        handle: "p-1".into(),
        request: 10,
        url: "https://vscode-remote+h.vscode-resource.vscode-cdn.net/x/cat/media/cat.svg".into(),
    });
    assert!(matches!(updates(&out)[..], [WebviewUpdate::Resource { response: ResourceResponse::Denied, .. }]));
}

#[test]
fn fr_e8_state_round_trips_through_reload_restore_and_views() {
    let mut w = Webviews::new();
    w.on_call(1, create("p-1", false));
    w.on_event(WebviewEvent::State { handle: "p-1".into(), state: r#"{"count":3}"#.into() });
    let out = w.on_call(2, WebviewCall::SetHtml { handle: "p-1".into(), html: "<p>".into() });
    assert!(matches!(updates(&out)[..], [WebviewUpdate::Html { state: Some(s), .. }] if s == r#"{"count":3}"#));
    // The init script hands the same state to `getState()`.
    let script = shim::init_script(Some(r#"{"count":3}"#), &[], ThemeKind::Light, shim::IPC_GLOBAL);
    assert!(script.contains(r#"JSON.parse("{\"count\":3}")"#), "{script}");
    assert!(script.contains("acquireVsCodeApi") && script.contains("getState") && script.contains("setState"));

    // A restored panel goes to its serializer with the state as a value.
    w.on_call(3, WebviewCall::RegisterSerializer { view_type: "catCoding".into(), serialize_buffers: false });
    let out = w.on_event(WebviewEvent::RestorePanel {
        view_type: "catCoding".into(),
        title: "Cat".into(),
        state: Some(r#"{"count":3}"#.into()),
        content: roots(),
        panel: PanelOptions::default(),
        position: 0,
    });
    let (call, pending) = out.iter().find_map(|o| if let Out::RpcWith(c, p) = o { Some((c, p)) } else { None }).unwrap();
    assert_eq!(call.method, "$deserializeWebviewPanel");
    assert_eq!(call.args[2].as_json().unwrap()["state"], json!({"count": 3}));
    assert!(matches!(pending, Pending::Resolve { .. }));

    // A webview view keeps its state across close and resolve.
    w.on_call(4, WebviewCall::RegisterViewProvider {
        extension: ext(),
        view_type: "cat.view".into(),
        retain_context_when_hidden: false,
        serialize_buffers: false,
    });
    let out = w.on_event(WebviewEvent::ResolveView { view_type: "cat.view".into(), title: Some("고양이".into()) });
    let handle = match updates(&out)[..] {
        [WebviewUpdate::Created { handle, kind: WebviewKind::View { .. }, .. }] => handle.clone(),
        ref other => panic!("{other:?}"),
    };
    assert_eq!(rpcs(&out)[0].method, "$resolveWebviewView");
    assert_eq!(rpcs(&out)[0].args[3], Arg::Undefined);
    w.on_event(WebviewEvent::State { handle: handle.clone(), state: r#"{"c":"blue"}"#.into() });
    let out = w.on_event(WebviewEvent::Closed { handle });
    assert_eq!(rpcs(&out)[0].method, "$disposeWebviewView");
    let out = w.on_event(WebviewEvent::ResolveView { view_type: "cat.view".into(), title: None });
    assert_eq!(rpcs(&out)[0].args[3], Arg::Json(json!({"c": "blue"})));
    assert!(matches!(updates(&out)[..], [WebviewUpdate::Created { state: Some(s), .. }] if s == r#"{"c":"blue"}"#));
}

#[test]
fn fr_e8_custom_editor_creates_the_document_then_resolves() {
    let mut w = Webviews::new();
    w.on_call(1, WebviewCall::RegisterCustomEditorProvider {
        extension: ext(),
        view_type: "cat.paw".into(),
        options: PanelOptions::default(),
        capabilities: Default::default(),
        multiple_editors_per_document: true,
        serialize_buffers: false,
    });
    let res = UriComponents::remote("h", "/w/a.pawdraw");
    let out = w.on_event(WebviewEvent::OpenCustomEditor { resource: res.clone(), view_type: "cat.paw".into() });
    let handle = match updates(&out)[..] {
        [WebviewUpdate::Created { handle, title, .. }] => {
            assert_eq!(title, "a.pawdraw");
            handle.clone()
        }
        ref other => panic!("{other:?}"),
    };
    let pending = match &out[..] {
        [_, Out::RpcWith(c, p)] => {
            assert_eq!(c.method, "$createCustomDocument");
            p.clone()
        }
        other => panic!("{other:?}"),
    };
    let out = w.on_reply(pending, Ok(json!({"editable": true})));
    assert_eq!(rpcs(&out)[0].method, "$resolveCustomEditor");
    assert_eq!(rpcs(&out)[0].args[1], Arg::Json(handle.clone().into()));
    // A failed resolve shows Code-OSS's error page, in Korean.
    let out = w.on_reply(Pending::Resolve { handle: handle.clone() }, Err("boom".into()));
    assert!(matches!(updates(&out)[..], [WebviewUpdate::Html { html, .. }] if html.contains("보기를 불러오는 중 오류가 발생했습니다: cat.paw")));
    // Closing the last editor of the document disposes the document.
    let out = w.on_event(WebviewEvent::Closed { handle });
    let methods: Vec<_> = rpcs(&out).iter().map(|c| c.method).collect();
    assert_eq!(methods, vec!["$onDidDisposeWebviewPanel", "$disposeCustomDocument"]);
    // No provider: the window says why.
    let out = w.on_event(WebviewEvent::OpenCustomEditor { resource: res, view_type: "none".into() });
    assert!(matches!(updates(&out)[..], [WebviewUpdate::OpenFailed { reason, .. }] if reason == "이 편집기를 제공하는 확장이 없습니다"));
}

#[test]
fn fr_e8_links_open_through_the_host_and_command_links_need_permission() {
    let mut w = Webviews::new();
    w.on_call(1, create("p-1", false));
    let out = w.on_event(WebviewEvent::LinkClicked { handle: "p-1".into(), uri: "https://code.visualstudio.com".into() });
    assert!(matches!(updates(&out)[..], [WebviewUpdate::OpenLink { uri }] if uri == "https://code.visualstudio.com"));
    let out = w.on_event(WebviewEvent::LinkClicked { handle: "p-1".into(), uri: "file:///etc/passwd".into() });
    assert!(out.is_empty());
    let out = w.on_event(WebviewEvent::LinkClicked { handle: "p-1".into(), uri: "command:cat.pet".into() });
    assert!(out.is_empty(), "enableCommandUris is off");
    w.on_call(2, WebviewCall::SetOptions {
        handle: "p-1".into(),
        options: ContentOptions { enable_command_uris: CommandUris::Only(vec!["cat.pet".into()]), ..roots() },
    });
    let out = w.on_event(WebviewEvent::LinkClicked { handle: "p-1".into(), uri: "command:cat.pet?%5B1%2C%22a%22%5D".into() });
    assert_eq!(out, vec![Out::Command { id: "cat.pet".into(), arguments: vec![json!(1), json!("a")] }]);
    let out = w.on_event(WebviewEvent::LinkClicked { handle: "p-1".into(), uri: "command:cat.other".into() });
    assert!(out.is_empty());
}

#[test]
fn fr_e8_theme_vars_cover_the_webview_default_stylesheet() {
    for kind in [ThemeKind::Light, ThemeKind::Dark] {
        let vars = theme::css_vars(kind, &Fonts::default());
        for name in theme::DEFAULT_STYLE_VARS {
            assert!(vars.iter().any(|(k, v)| k == name && !v.is_empty()), "{name} missing in {kind:?}");
        }
        let mut names: Vec<&String> = vars.iter().map(|(k, _)| k).collect();
        let n = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), n, "duplicate variable");
        assert!(vars.iter().all(|(k, _)| k.starts_with("--vscode-") || k == "--text-link-decoration"));
    }
    let light = theme::css_vars(ThemeKind::Light, &Fonts::default());
    let dark = theme::css_vars(ThemeKind::Dark, &Fonts::default());
    let get = |v: &[(String, String)], k: &str| v.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap();
    // Ember tokens (DESIGN.md §2): surface, text, c-blue.
    assert_eq!(get(&light, "--vscode-editor-background"), "#ffffff");
    assert_eq!(get(&dark, "--vscode-editor-background"), "#1f1f1d");
    assert_eq!(get(&light, "--vscode-editor-foreground"), "#1c1b19");
    assert_eq!(get(&dark, "--vscode-textLink-foreground"), "#6b95e0");
    assert_eq!(get(&light, "--vscode-font-size"), "13px");
    assert_eq!(ThemeKind::Dark.body_class(), "vscode-dark");
    let script = shim::init_script(None, &light, ThemeKind::Light, shim::IPC_GLOBAL);
    assert!(script.contains(r##"root.setProperty("--vscode-editor-background", "#ffffff");"##));
    assert!(script.contains("let state = undefined;"));
    assert!(script.contains(r#""vscode-light""#));
    assert!(theme::css_text(&light[..1]).starts_with(":root { --vscode-font-family: "));
}

#[test]
fn fr_e8_fake_host_flow_from_create_to_dispose() {
    let mut w = Webviews::new();
    let mut d = WebviewDriver::new(FakeHost::new(), ThemeKind::Light, Fonts::default());
    let mut panes = WebviewPanes::default();
    let run = |out: Vec<Out>, d: &mut WebviewDriver<FakeHost>, panes: &mut WebviewPanes| {
        for o in out {
            if let Out::Ui(u) = o {
                panes.apply(&u);
                d.apply(&u);
            }
        }
    };

    run(w.on_call(1, create("p-1", false)), &mut d, &mut panes);
    // The placeholder shows the title and the waiting text.
    let pane = panes.get("p-1").unwrap();
    assert_eq!((pane.title.as_str(), pane.status.text()), ("Cat Coding", WAITING_TEXT));
    assert_eq!(WAITING_TEXT, "웹뷰를 표시하는 중입니다");
    match &d.host().calls[..] {
        [HostCall::Create(c)] => {
            assert_eq!(c.handle, "p-1");
            assert!(c.origin.starts_with("vscode-webview://"), "the origin is the host's");
            assert_eq!(c.ipc_global, "__emberWebviewHost");
            assert!(c.init_script.contains(r#"globalThis["__emberWebviewHost"]"#));
            assert!(c.enable_scripts);
            assert!(c.init_script.contains("acquireVsCodeApi"));
            assert!(c.css_vars.iter().any(|(k, _)| k == "--vscode-editor-background"));
        }
        other => panic!("{other:?}"),
    }

    run(w.on_call(2, WebviewCall::SetHtml { handle: "p-1".into(), html: "<h1>cat</h1>".into() }), &mut d, &mut panes);
    run(w.on_call(3, WebviewCall::PostMessage { handle: "p-1".into(), message: msg("early") }), &mut d, &mut panes);
    assert!(d.host().page("p-1").unwrap().received.is_empty());

    // The page loads and posts back.
    d.host_mut().emit(HostEvent::Ready { handle: "p-1".into() });
    d.host_mut().emit(HostEvent::Message { handle: "p-1".into(), message: msg(r#"{"command":"alert"}"#) });
    d.host_mut().emit(HostEvent::State { handle: "p-1".into(), state: "7".into() });
    let events = d.poll();
    assert_eq!(events.len(), 3);
    let mut to_exthost = Vec::new();
    for e in events {
        let out = w.on_event(e);
        to_exthost.extend(rpcs(&out).into_iter().map(|c| c.method));
        run(out, &mut d, &mut panes);
    }
    assert_eq!(to_exthost, vec!["$onMessage"]);
    assert_eq!(d.host().page("p-1").unwrap().received, vec![msg("early")]);
    assert_eq!(panes.get("p-1").unwrap().status, PaneStatus::Shown);
    assert_eq!(w.get("p-1").unwrap().state.as_deref(), Some("7"));

    // Hidden without retain: the host drops the page, and makes it again with the state on show.
    let e = d.set_view_state("p-1", false, false, 0).unwrap();
    run(w.on_event(e), &mut d, &mut panes);
    assert!(d.host().page("p-1").is_none());
    let e = d.set_view_state("p-1", true, true, 0).unwrap();
    run(w.on_event(e), &mut d, &mut panes);
    let page = d.host().page("p-1").unwrap();
    assert_eq!(page.html.as_deref(), Some("<h1>cat</h1>"));
    assert!(page.init_script.contains(r#"JSON.parse("7")"#));

    // Theme change reaches the live page.
    d.set_theme(ThemeKind::Dark);
    let bg = d.host().page("p-1").unwrap().css_vars.iter().find(|(k, _)| k == "--vscode-editor-background").unwrap().1.clone();
    assert_eq!(bg, "#1f1f1d");

    // A resource request is answered through the host.
    d.host_mut().emit(HostEvent::Resource {
        handle: "p-1".into(),
        request: 1,
        url: "vscode-resource://vscode-remote+h/secret".into(),
    });
    for e in d.poll() {
        run(w.on_event(e), &mut d, &mut panes);
    }
    assert!(d.host().calls.iter().any(|c| matches!(c, HostCall::AnswerResource { request: 1, response: ResourceResponse::Denied, .. })));

    run(w.on_call(9, WebviewCall::DisposePanel { handle: "p-1".into() }), &mut d, &mut panes);
    assert!(matches!(d.host().calls.last(), Some(HostCall::Dispose { handle }) if handle == "p-1"));
    assert!(panes.panes().is_empty());
}

fn request(req: u32, proxy: &'static str, method: &str, args: Vec<Arg>) -> Input {
    Input::ExtHostRequest(IncomingRequest {
        req,
        rpc_id: ember_editor_conn::rpc_ids::id_of(proxy).unwrap(),
        proxy: Some(proxy),
        method: method.into(),
        args,
        cancellable: false,
    })
}

#[test]
fn fr_e8_session_core_routes_webview_calls_and_the_bridge_keeps_panes() {
    let t0 = Instant::now();
    let mut core = SessionCore::new(LanguageRegistry::from_extensions(&[]), CoreOptions::default(), t0);
    let ext = json!({"id": {"value": "pub.cat", "_lower": "pub.cat"}, "location": {"$mid": 1, "scheme": "vscode-remote", "authority": "h", "path": "/x/cat"}});
    let init = json!({"title": "Cat Coding", "serializeBuffersForPostMessage": false,
        "webviewOptions": {"enableScripts": true, "localResourceRoots": [{"$mid": 1, "scheme": "vscode-remote", "authority": "h", "path": "/x/cat/media"}]},
        "panelOptions": {}});
    let fx = core.handle(
        t0,
        request(
            1,
            "MainThreadWebviewPanels",
            "$createWebviewPanel",
            [ext, json!("p-1"), json!("catCoding"), init, json!({})].into_iter().map(Arg::Json).collect(),
        ),
    );
    assert!(fx.iter().any(|e| matches!(e, Effect::Respond { req: 1, result: Ok(Reply::Empty) })));
    let created = fx.iter().find_map(|e| if let Effect::Ui(u @ SessionUpdate::Webview(WebviewUpdate::Created { .. })) = e { Some(u.clone()) } else { None });
    let mut bridge = WidgetBridge::new();
    match bridge.apply(created.expect("Created")) {
        BridgeAction::Webview(WebviewUpdate::Created { handle, .. }) => assert_eq!(handle, "p-1"),
        other => panic!("{other:?}"),
    }
    assert_eq!(bridge.webview_panes().panes()[0].status.text(), WAITING_TEXT);
    assert!(core.webviews().get("p-1").is_some());

    let fx = core.handle(
        t0,
        request(2, "MainThreadWebviews", "$postMessage", vec![Arg::Json("p-1".into()), Arg::Json("{}".into())]),
    );
    assert!(fx.iter().any(|e| matches!(e, Effect::Respond { req: 2, result: Ok(Reply::Json(Value::Bool(true))) })));
    assert!(!fx.iter().any(|e| matches!(e, Effect::Ui(SessionUpdate::Webview(WebviewUpdate::Post { .. })))));

    let fx = core.handle(t0, Input::Webview(WebviewEvent::Ready { handle: "p-1".into() }));
    assert!(fx.iter().any(|e| matches!(e, Effect::Ui(SessionUpdate::Webview(WebviewUpdate::Post { .. })))));

    // A resource read goes through the file effect and comes back as the host's answer.
    let fx = core.handle(
        t0,
        Input::Webview(WebviewEvent::ResourceRequested {
            handle: "p-1".into(),
            request: 4,
            url: "https://vscode-remote+h.vscode-resource.vscode-cdn.net/x/cat/media/a.png".into(),
        }),
    );
    let (uri, purpose) = fx
        .iter()
        .find_map(|e| if let Effect::ReadFile { uri, purpose } = e { Some((uri.clone(), purpose.clone())) } else { None })
        .expect("ReadFile");
    assert_eq!(purpose, ReadPurpose::WebviewResource { handle: "p-1".into(), request: 4 });
    let fx = core.handle(t0, Input::FileRead { uri, purpose, result: Ok(vec![0x89, b'P']) });
    assert!(fx.iter().any(|e| matches!(e,
        Effect::Ui(SessionUpdate::Webview(WebviewUpdate::Resource { request: 4, response: ResourceResponse::Ok { mime, .. }, .. })) if mime == "image/png")));
}

#[test]
fn fr_e8_resource_base_comes_from_the_host_in_both_forms() {
    let file = UriComponents::remote("127.0.0.1:9", "/x/cat/media/cat.gif");
    let scheme = resource::ResourceBase::custom_scheme();
    let https = resource::ResourceBase::code_oss();
    assert_eq!(scheme.as_webview_uri(&file), "vscode-resource://vscode-remote+127-002e0-002e0-002e1-003a9/x/cat/media/cat.gif");
    assert_eq!(https.as_webview_uri(&file), resource::as_webview_uri(&file));
    assert_eq!(scheme.csp_source(), "'self' vscode-resource:");
    assert_eq!(https.csp_source(), "'self' https://*.vscode-resource.vscode-cdn.net");
    for base in [&scheme, &https] {
        let url = base.as_webview_uri(&file);
        assert_eq!(base.parse(&url), Some(file.clone()));
        // Back to Code-OSS's form, the path kept byte for byte, so the session's rule applies unchanged.
        let canonical = base.to_canonical(&format!("{url}?v=1")).unwrap();
        assert_eq!(canonical, format!("{}?v=1", resource::as_webview_uri(&file)));
        assert_eq!(resource::parse_resource_url(&canonical).unwrap().path, "/x/cat/media/cat.gif");
    }
    assert_eq!(scheme.parse("https://vscode-remote+h.vscode-resource.vscode-cdn.net/a"), None);
    assert_eq!(https.parse("vscode-resource://vscode-remote+h/a"), None);
    assert_eq!(scheme.to_canonical("vscode-resource://nolabel/a"), None);

    // The extension host writes Code-OSS's form; the page gets the host's.
    let html = r#"<meta http-equiv="Content-Security-Policy" content="img-src 'self' https://*.vscode-cdn.net; script-src 'nonce-1'"><img src="https://vscode-remote+h.vscode-resource.vscode-cdn.net/x/cat/media/cat.gif"><a href="https://example.com/">x</a>"#;
    let out = resource::rewrite_for_host(html, &scheme);
    assert!(out.contains("img-src 'self' vscode-resource:;"), "{out}");
    assert!(out.contains(r#"src="vscode-resource://vscode-remote+h/x/cat/media/cat.gif""#), "{out}");
    assert!(out.contains(r#"href="https://example.com/""#));
    assert_eq!(resource::rewrite_for_host(html, &https), html);
}

#[test]
fn fr_e8_driver_uses_the_hosts_origin_and_resource_form() {
    for (host, prefix) in [(FakeHost::custom_scheme(), "vscode-webview://"), (FakeHost::https(), "https://")] {
        let https = matches!(host.base, resource::ResourceBase::Https { .. });
        let mut w = Webviews::new();
        let mut d = WebviewDriver::new(host, ThemeKind::Light, Fonts::default());
        for o in w.on_call(1, create("p-1", false)).into_iter().chain(w.on_call(2, create("p-2", false))) {
            if let Out::Ui(u) = o {
                d.apply(&u);
            }
        }
        let (o1, o2) = (d.origin("p-1").unwrap().to_owned(), d.origin("p-2").unwrap().to_owned());
        assert!(o1.starts_with(prefix) && o2.starts_with(prefix));
        assert_ne!(o1, o2, "one origin per webview");

        let src = "https://vscode-remote+h.vscode-resource.vscode-cdn.net/x/cat/media/cat.gif";
        let html = format!(r#"<img src="{src}">"#);
        for o in w.on_call(3, WebviewCall::SetHtml { handle: "p-1".into(), html }) {
            if let Out::Ui(u) = o {
                d.apply(&u);
            }
        }
        let page_html = d.host().page("p-1").unwrap().html.clone().unwrap();
        let expected = if https { src.to_owned() } else { "vscode-resource://vscode-remote+h/x/cat/media/cat.gif".to_owned() };
        assert!(page_html.contains(&expected), "{page_html}");

        // The page requests what it was given; the session sees Code-OSS's form and allows it.
        d.host_mut().emit(HostEvent::Resource { handle: "p-1".into(), request: 1, url: expected.clone() });
        let events = d.poll();
        assert!(matches!(&events[..], [WebviewEvent::ResourceRequested { url, .. }] if url == src));
        let out = w.on_event(events.into_iter().next().unwrap());
        assert!(matches!(&out[..], [Out::ReadFile { uri, .. }] if uri.path == "/x/cat/media/cat.gif"));

        // Messages carry the host's form too.
        w.on_event(WebviewEvent::Ready { handle: "p-1".into() });
        let json = format!(r#"{{"src":"{src}"}}"#);
        for o in w.on_call(4, WebviewCall::PostMessage { handle: "p-1".into(), message: msg(&json) }) {
            if let Out::Ui(u) = o {
                d.apply(&u);
            }
        }
        let got = &d.host().page("p-1").unwrap().received[0].json;
        assert!(got.contains(&expected), "{got}");
        assert!(serde_json::from_str::<Value>(got).is_ok());
    }
}

#[test]
fn fr_e8_find_devtools_and_app_chords_go_through_the_host() {
    use ember_editor::webview::host::{FindOptions, WindowEvent};
    let mut w = Webviews::new();
    let mut d = WebviewDriver::new(FakeHost::new(), ThemeKind::Light, Fonts::default());
    for o in w.on_call(1, create("p-1", false)) {
        if let Out::Ui(u) = o {
            d.apply(&u);
        }
    }
    let opts = FindOptions { match_case: true, whole_word: false };
    d.find("p-1", "cat", opts);
    d.find_next("p-1");
    d.find_prev("p-1");
    d.find_stop("p-1");
    d.open_devtools("p-1");
    d.find("gone", "cat", opts);
    let calls: Vec<&HostCall> = d.host().calls.iter().skip(1).collect();
    assert_eq!(
        calls,
        vec![
            &HostCall::Find { handle: "p-1".into(), query: "cat".into(), options: opts },
            &HostCall::FindNext { handle: "p-1".into() },
            &HostCall::FindPrev { handle: "p-1".into() },
            &HostCall::FindStop { handle: "p-1".into() },
            &HostCall::OpenDevtools { handle: "p-1".into() },
        ]
    );
    d.host_mut().emit(HostEvent::Find { handle: "p-1".into(), count: 4, active: Some(1) });
    d.host_mut().emit(HostEvent::Key { handle: "p-1".into(), chord: "Cmd+P".into() });
    d.host_mut().emit(HostEvent::Ready { handle: "p-1".into() });
    // Only the ready event is the session's.
    assert_eq!(d.poll(), vec![WebviewEvent::Ready { handle: "p-1".into() }]);
    assert_eq!(
        d.take_window_events(),
        vec![
            WindowEvent::Find { handle: "p-1".into(), count: 4, active: Some(1) },
            WindowEvent::Key { handle: "p-1".into(), chord: "Cmd+P".into() },
        ]
    );
    assert!(d.take_window_events().is_empty());
}
