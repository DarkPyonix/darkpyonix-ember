//! FR-E8: the extension host's webview calls at the pinned Code-OSS commit, and the calls Ember makes
//! back. Shapes from `extHost.protocol.ts` L1013-1239 (Code-OSS 1.139.1).

use ember_editor_conn::rpc::{Arg, IncomingRequest};
use ember_editor_conn::rpc_ids::id_of;
use ember_editor_conn::uri::UriComponents;
use ember_editor_conn::webview::{
    self as wv, array_buffer_refs, CommandUris, ContentOptions, CustomEditorCapabilities, IconPath, PanelOptions, PanelViewState,
    ShowOptions, WebviewCall, WebviewMessage,
};
use serde_json::{json, Value};

fn req(proxy: &'static str, method: &str, args: Vec<Arg>) -> IncomingRequest {
    IncomingRequest { req: 3, rpc_id: id_of(proxy).unwrap(), proxy: Some(proxy), method: method.into(), args, cancellable: false }
}

fn j(args: Value) -> Vec<Arg> {
    args.as_array().unwrap().iter().cloned().map(Arg::Json).collect()
}

fn parse(proxy: &'static str, method: &str, args: Vec<Arg>) -> WebviewCall {
    WebviewCall::parse(&req(proxy, method, args)).unwrap().unwrap_or_else(|| panic!("{proxy}.{method} not decoded"))
}

fn ext() -> Value {
    json!({"id": {"value": "pub.ext", "_lower": "pub.ext"}, "location": {"$mid": 1, "scheme": "vscode-remote", "authority": "h", "path": "/x/ext"}})
}

#[test]
fn fr_e8_actor_ids_are_the_pinned_numbering() {
    assert_eq!(id_of(wv::proxy::MAIN_THREAD_WEBVIEWS), Some(44));
    assert_eq!(id_of(wv::proxy::MAIN_THREAD_WEBVIEW_PANELS), Some(45));
    assert_eq!(id_of(wv::proxy::MAIN_THREAD_WEBVIEW_VIEWS), Some(46));
    assert_eq!(id_of(wv::proxy::MAIN_THREAD_CUSTOM_EDITORS), Some(47));
    assert_eq!(id_of(wv::proxy::EXT_HOST_WEBVIEWS), Some(120));
    assert_eq!(id_of(wv::proxy::EXT_HOST_WEBVIEW_PANELS), Some(121));
    assert_eq!(id_of(wv::proxy::EXT_HOST_CUSTOM_EDITORS), Some(122));
    assert_eq!(id_of(wv::proxy::EXT_HOST_WEBVIEW_VIEWS), Some(123));
}

#[test]
fn fr_e8_create_webview_panel_decodes_init_data_and_show_options() {
    let call = parse(
        wv::proxy::MAIN_THREAD_WEBVIEW_PANELS,
        "$createWebviewPanel",
        j(json!([
            ext(),
            "h-1",
            "catCoding",
            {"title": "Cat Coding", "serializeBuffersForPostMessage": true,
             "webviewOptions": {"enableScripts": true, "enableCommandUris": ["a.b"],
                 "localResourceRoots": [{"$mid": 1, "scheme": "vscode-remote", "authority": "h", "path": "/x/ext/media"}],
                 "portMapping": [{"webviewPort": 3000, "extensionHostPort": 3001}]},
             "panelOptions": {"enableFindWidget": true, "retainContextWhenHidden": true}},
            {"viewColumn": -2, "preserveFocus": true}
        ])),
    );
    match call {
        WebviewCall::CreatePanel { extension, handle, view_type, title, content_options, panel_options, serialize_buffers, show } => {
            assert_eq!(extension.id, "pub.ext");
            assert_eq!(extension.location, UriComponents::remote("h", "/x/ext"));
            assert_eq!((handle.as_str(), view_type.as_str(), title.as_str()), ("h-1", "catCoding", "Cat Coding"));
            assert!(content_options.enable_scripts && !content_options.enable_forms);
            assert_eq!(content_options.enable_command_uris, CommandUris::Only(vec!["a.b".into()]));
            assert_eq!(content_options.roots(), &[UriComponents::remote("h", "/x/ext/media")]);
            assert_eq!(content_options.port_mapping[0].extension_host_port, 3001);
            assert_eq!(panel_options, PanelOptions { enable_find_widget: true, retain_context_when_hidden: true });
            assert!(serialize_buffers);
            assert_eq!(show, ShowOptions { view_column: Some(wv::SIDE_GROUP), preserve_focus: true });
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn fr_e8_webviews_set_html_options_and_post_message_with_buffers() {
    assert_eq!(
        parse(wv::proxy::MAIN_THREAD_WEBVIEWS, "$setHtml", j(json!(["h-1", "<p>안녕</p>"]))),
        WebviewCall::SetHtml { handle: "h-1".into(), html: "<p>안녕</p>".into() }
    );
    match parse(wv::proxy::MAIN_THREAD_WEBVIEWS, "$setOptions", j(json!(["h-1", {"enableScripts": false, "enableCommandUris": true}]))) {
        WebviewCall::SetOptions { handle, options } => {
            assert_eq!(handle, "h-1");
            assert!(!options.enable_scripts);
            assert!(options.enable_command_uris.allows("anything"));
            assert!(options.roots().is_empty());
        }
        other => panic!("{other:?}"),
    }
    let msg = r#"{"type":"bytes","data":{"$$vscode_array_buffer_reference$$":true,"index":0,"view":{"type":2,"byteLength":3,"byteOffset":0}}}"#;
    let call = parse(
        wv::proxy::MAIN_THREAD_WEBVIEWS,
        "$postMessage",
        vec![Arg::Json("h-1".into()), Arg::Json(msg.into()), Arg::Buffer(vec![1, 2, 3]), Arg::Buffer(vec![9])],
    );
    match call {
        WebviewCall::PostMessage { handle, message } => {
            assert_eq!(handle, "h-1");
            assert_eq!(message.json, msg);
            assert_eq!(message.buffers, vec![vec![1, 2, 3], vec![9]]);
            assert_eq!(array_buffer_refs(&message.value().unwrap()), vec![(0, Some((2, 0, 3)))]);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn fr_e8_panel_dispose_reveal_title_icon_and_serializers() {
    let p = wv::proxy::MAIN_THREAD_WEBVIEW_PANELS;
    assert_eq!(parse(p, "$disposeWebview", j(json!(["h-1"]))), WebviewCall::DisposePanel { handle: "h-1".into() });
    assert_eq!(
        parse(p, "$reveal", j(json!(["h-1", {"viewColumn": 1}]))),
        WebviewCall::Reveal { handle: "h-1".into(), show: ShowOptions { view_column: Some(1), preserve_focus: false } }
    );
    assert_eq!(parse(p, "$setTitle", j(json!(["h-1", "새 제목"]))), WebviewCall::SetTitle { handle: "h-1".into(), title: "새 제목".into() });
    assert_eq!(
        parse(p, "$setIconPath", j(json!(["h-1", {"id": "flame"}]))),
        WebviewCall::SetIconPath { handle: "h-1".into(), icon: Some(IconPath::ThemeIcon { id: "flame".into() }) }
    );
    match parse(p, "$setIconPath", j(json!(["h-1", {"light": {"$mid": 1, "scheme": "file", "path": "/l.svg"}, "dark": {"$mid": 1, "scheme": "file", "path": "/d.svg"}}]))) {
        WebviewCall::SetIconPath { icon: Some(IconPath::Images { light, dark }), .. } => {
            assert_eq!((light.path.as_str(), dark.path.as_str()), ("/l.svg", "/d.svg"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        parse(p, "$setIconPath", vec![Arg::Json("h-1".into()), Arg::Undefined]),
        WebviewCall::SetIconPath { handle: "h-1".into(), icon: None }
    );
    assert_eq!(
        parse(p, "$registerSerializer", j(json!(["catCoding", {"serializeBuffersForPostMessage": true}]))),
        WebviewCall::RegisterSerializer { view_type: "catCoding".into(), serialize_buffers: true }
    );
    assert_eq!(parse(p, "$unregisterSerializer", j(json!(["catCoding"]))), WebviewCall::UnregisterSerializer { view_type: "catCoding".into() });
}

#[test]
fn fr_e8_webview_view_provider_calls() {
    let p = wv::proxy::MAIN_THREAD_WEBVIEW_VIEWS;
    match parse(p, "$registerWebviewViewProvider", j(json!([ext(), "calicoColors.colorsView", {"retainContextWhenHidden": true, "serializeBuffersForPostMessage": false}]))) {
        WebviewCall::RegisterViewProvider { extension, view_type, retain_context_when_hidden, serialize_buffers } => {
            assert_eq!(extension.id, "pub.ext");
            assert_eq!(view_type, "calicoColors.colorsView");
            assert!(retain_context_when_hidden && !serialize_buffers);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(parse(p, "$unregisterWebviewViewProvider", j(json!(["v"]))), WebviewCall::UnregisterViewProvider { view_type: "v".into() });
    assert_eq!(parse(p, "$setWebviewViewTitle", j(json!(["h", "색"]))), WebviewCall::SetViewTitle { handle: "h".into(), title: Some("색".into()) });
    assert_eq!(
        parse(p, "$setWebviewViewTitle", vec![Arg::Json("h".into()), Arg::Undefined]),
        WebviewCall::SetViewTitle { handle: "h".into(), title: None }
    );
    assert_eq!(
        parse(p, "$setWebviewViewDescription", j(json!(["h", "설명"]))),
        WebviewCall::SetViewDescription { handle: "h".into(), description: Some("설명".into()) }
    );
    match parse(p, "$setWebviewViewBadge", j(json!(["h", {"tooltip": "2개", "value": 2}]))) {
        WebviewCall::SetViewBadge { badge: Some(b), .. } => assert_eq!((b.tooltip.as_str(), b.value), ("2개", 2)),
        other => panic!("{other:?}"),
    }
    assert_eq!(parse(p, "$show", j(json!(["h", true]))), WebviewCall::ShowView { handle: "h".into(), preserve_focus: true });
}

#[test]
fn fr_e8_custom_editor_provider_calls() {
    let p = wv::proxy::MAIN_THREAD_CUSTOM_EDITORS;
    match parse(p, "$registerTextEditorProvider", j(json!([ext(), "catCustoms.catScratch", {"retainContextWhenHidden": false}, {"supportsMove": true}, false]))) {
        WebviewCall::RegisterTextEditorProvider { view_type, capabilities, serialize_buffers, .. } => {
            assert_eq!(view_type, "catCustoms.catScratch");
            assert_eq!(capabilities, CustomEditorCapabilities { supports_move: true, ..Default::default() });
            assert!(!serialize_buffers);
        }
        other => panic!("{other:?}"),
    }
    match parse(p, "$registerCustomEditorProvider", j(json!([ext(), "catCustoms.pawDraw", {}, {}, true, true]))) {
        WebviewCall::RegisterCustomEditorProvider { view_type, multiple_editors_per_document, serialize_buffers, options, .. } => {
            assert_eq!(view_type, "catCustoms.pawDraw");
            assert!(multiple_editors_per_document && serialize_buffers);
            assert_eq!(options, PanelOptions::default());
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(parse(p, "$unregisterEditorProvider", j(json!(["v"]))), WebviewCall::UnregisterEditorProvider { view_type: "v".into() });
    match parse(p, "$onDidEdit", j(json!([{"$mid": 1, "scheme": "vscode-remote", "authority": "h", "path": "/w/a.pawdraw"}, "catCustoms.pawDraw", 4, "Stroke"]))) {
        WebviewCall::CustomEditorEdit { resource, edit_id, label, .. } => {
            assert_eq!(resource.path, "/w/a.pawdraw");
            assert_eq!((edit_id, label.as_deref()), (4, Some("Stroke")));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn fr_e8_other_actors_and_unknown_methods_are_not_webview_calls() {
    let r = req("MainThreadCommands", "$registerCommand", j(json!(["x"])));
    assert!(!WebviewCall::is_webview_proxy(r.proxy));
    assert_eq!(WebviewCall::parse(&r).unwrap(), None);
    let r = req(wv::proxy::MAIN_THREAD_WEBVIEW_PANELS, "$somethingNew", j(json!([])));
    assert!(WebviewCall::is_webview_proxy(r.proxy));
    assert_eq!(WebviewCall::parse(&r).unwrap(), None);
    // A malformed handle is an error, not a silent default.
    assert!(WebviewCall::parse(&req(wv::proxy::MAIN_THREAD_WEBVIEWS, "$setHtml", j(json!([1, "x"])))).is_err());
}

#[test]
fn fr_e8_reverse_calls_have_the_ext_host_shapes() {
    let m = WebviewMessage { json: r#"{"n":1}"#.into(), buffers: vec![vec![5, 6]] };
    let c = wv::on_message("h-1", &m);
    assert_eq!((c.proxy, c.method, c.cancellable), ("ExtHostWebviews", "$onMessage", false));
    assert_eq!(c.args[0], Arg::Json("h-1".into()));
    assert_eq!(c.args[1], Arg::Json(r#"{"n":1}"#.into()));
    assert_eq!(c.args[2], Arg::JsonWithBuffers { value: json!([{"$$ref$$": 0}]), buffers: vec![vec![5, 6]] });

    let c = wv::on_did_change_view_states(&[
        ("a".into(), PanelViewState { active: true, visible: true, position: 0 }),
        ("b".into(), PanelViewState { active: false, visible: false, position: 1 }),
    ]);
    assert_eq!((c.proxy, c.method), ("ExtHostWebviewPanels", "$onDidChangeWebviewPanelViewStates"));
    assert_eq!(
        c.args[0],
        Arg::Json(json!({"a": {"active": true, "visible": true, "position": 0}, "b": {"active": false, "visible": false, "position": 1}}))
    );

    let c = wv::on_did_dispose_panel("a");
    assert_eq!((c.proxy, c.method), ("ExtHostWebviewPanels", "$onDidDisposeWebviewPanel"));

    let c = wv::resolve_webview_view("v-1", "calicoColors.colorsView", None, Some(&json!({"colors": ["#fff"]})));
    assert_eq!((c.proxy, c.method, c.cancellable), ("ExtHostWebviewViews", "$resolveWebviewView", true));
    assert_eq!(c.args[2], Arg::Undefined);
    assert_eq!(c.args[3], Arg::Json(json!({"colors": ["#fff"]})));
    assert_eq!(wv::on_did_change_view_visibility("v-1", false).args[1], Arg::Json(false.into()));
    assert_eq!(wv::dispose_webview_view("v-1").method, "$disposeWebviewView");

    let res = UriComponents::remote("h", "/w/a.pawdraw");
    let c = wv::create_custom_document(&res, "catCustoms.pawDraw");
    assert_eq!((c.proxy, c.method, c.cancellable), ("ExtHostCustomEditors", "$createCustomDocument", true));
    assert_eq!(c.args.len(), 4);
    let c = wv::resolve_custom_editor(&res, "e-1", "catCustoms.pawDraw", "a.pawdraw", &ContentOptions::default(), &PanelOptions::default(), true, 0);
    assert_eq!((c.proxy, c.method, c.cancellable), ("ExtHostCustomEditors", "$resolveCustomEditor", true));
    assert_eq!(c.args[1], Arg::Json("e-1".into()));
    assert_eq!(c.args[3].as_json().unwrap()["title"], "a.pawdraw");
    assert_eq!(c.args[3].as_json().unwrap()["active"], true);
    assert_eq!(c.args[4], Arg::Json(0.into()));

    let c = wv::deserialize_webview_panel("p-1", "catCoding", "Cat", Some(&json!({"count": 3})), &ContentOptions::default(), &PanelOptions::default(), false, 1);
    assert_eq!((c.proxy, c.method), ("ExtHostWebviewPanels", "$deserializeWebviewPanel"));
    assert_eq!(c.args[2].as_json().unwrap()["state"], json!({"count": 3}));
    assert_eq!(c.args[3], Arg::Json(1.into()));

    for name in [c.proxy, wv::on_missing_csp("a", "pub.ext").proxy] {
        assert!(id_of(name).is_some());
    }
}

#[test]
fn fr_e8_content_options_round_trip_keeps_code_oss_names() {
    let o = ContentOptions {
        enable_scripts: true,
        local_resource_roots: Some(vec![UriComponents::remote("h", "/w")]),
        ..Default::default()
    };
    let v = serde_json::to_value(&o).unwrap();
    assert_eq!(v["enableScripts"], true);
    assert_eq!(v["enableCommandUris"], false);
    assert_eq!(v["localResourceRoots"][0]["path"], "/w");
    let back: ContentOptions = serde_json::from_value(v).unwrap();
    assert_eq!(back, o);
}
