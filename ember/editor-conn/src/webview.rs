//! Extension webviews on the wire (SPEC FR-E8, INTENT D44, `docs/design/EDITOR-WEBVIEWS.md`).
//!
//! The extension host drives webviews through four main-thread actors and expects four of its own
//! to be called back. The shapes below are `extHost.protocol.ts` at [`crate::PINNED_COMMIT`]
//! (Code-OSS 1.139.1), lines 1013-1239:
//!
//! | actor | methods decoded here |
//! | ----- | -------------------- |
//! | `MainThreadWebviews` (L1100) | `$setHtml`, `$setOptions`, `$postMessage(handle, json, ...buffers)` |
//! | `MainThreadWebviewPanels` (L1118) | `$createWebviewPanel`, `$disposeWebview`, `$reveal`, `$setTitle`, `$setIconPath`, `$registerSerializer`, `$unregisterSerializer` |
//! | `MainThreadCustomEditors` (L1135) | `$registerTextEditorProvider`, `$registerCustomEditorProvider`, `$unregisterEditorProvider`, `$onDidEdit`, `$onContentChange` |
//! | `MainThreadWebviewViews` (L1144) | `$registerWebviewViewProvider`, `$unregisterWebviewViewProvider`, `$setWebviewViewTitle`, `$setWebviewViewDescription`, `$setWebviewViewBadge`, `$show` |
//!
//! and the calls Ember makes back ([`on_message`], [`on_did_change_view_states`],
//! [`on_did_dispose_panel`], [`resolve_webview_view`], [`resolve_custom_editor`], ...).
//!
//! A webview message travels as a JSON **string** plus side buffers: `$postMessage` carries the
//! buffers as rest `VSBuffer` arguments, `$onMessage` as one `SerializableObjectWithBuffers<VSBuffer[]>`.
//! Inside the string an `ArrayBuffer` or typed array is `{"$$vscode_array_buffer_reference$$": true,
//! "index": i, "view"?: {type, byteLength, byteOffset}}` (`extHostWebviewMessaging.ts`); Ember keeps
//! the string and the buffers as they are and lets the page shim revive them.
//!
//! Note: the panel's dispose is `$disposeWebview` at the pinned commit (not `$disposeWebviewPanel`).

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::exthost::{arg_json, Call};
use crate::rpc::{Arg, IncomingRequest};
use crate::uri::UriComponents;
use crate::{Error, Result};

/// Proxy identifier names of the webview actors (numbered in [`crate::rpc_ids`]).
pub mod proxy {
    pub const MAIN_THREAD_WEBVIEWS: &str = "MainThreadWebviews";
    pub const MAIN_THREAD_WEBVIEW_PANELS: &str = "MainThreadWebviewPanels";
    pub const MAIN_THREAD_WEBVIEW_VIEWS: &str = "MainThreadWebviewViews";
    pub const MAIN_THREAD_CUSTOM_EDITORS: &str = "MainThreadCustomEditors";
    pub const EXT_HOST_WEBVIEWS: &str = "ExtHostWebviews";
    pub const EXT_HOST_WEBVIEW_PANELS: &str = "ExtHostWebviewPanels";
    pub const EXT_HOST_CUSTOM_EDITORS: &str = "ExtHostCustomEditors";
    pub const EXT_HOST_WEBVIEW_VIEWS: &str = "ExtHostWebviewViews";
}

/// `WebviewHandle`: a string the extension host picks for panels, and the main thread picks for
/// views, custom editors and revived panels.
pub type WebviewHandle = String;

/// `EditorGroupColumn`: `-1` the active group, `-2` beside it, `0..` a column.
pub const ACTIVE_GROUP: i64 = -1;
pub const SIDE_GROUP: i64 = -2;

/// `enableCommandUris`: `true`, `false`, or the command ids allowed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CommandUris {
    All(bool),
    Only(Vec<String>),
}

impl Default for CommandUris {
    fn default() -> Self {
        Self::All(false)
    }
}

impl CommandUris {
    /// Whether a `command:<id>` link may run (`MainThreadWebviews.isSupportedLink`).
    pub fn allows(&self, id: &str) -> bool {
        match self {
            Self::All(b) => *b,
            Self::Only(ids) => ids.iter().any(|i| i == id),
        }
    }
}

/// `IWebviewPortMapping`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortMapping {
    pub webview_port: u16,
    pub extension_host_port: u16,
}

/// `IWebviewContentOptions`. `local_resource_roots: None` is what the extension host never sends
/// (it fills in the workspace folders and the extension's location, `getDefaultLocalResourceRoots`);
/// Ember treats `None` like an empty list, so nothing is served.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentOptions {
    #[serde(default)]
    pub enable_scripts: bool,
    #[serde(default)]
    pub enable_forms: bool,
    #[serde(default)]
    pub enable_command_uris: CommandUris,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_resource_roots: Option<Vec<UriComponents>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub port_mapping: Vec<PortMapping>,
}

impl ContentOptions {
    pub fn roots(&self) -> &[UriComponents] {
        self.local_resource_roots.as_deref().unwrap_or(&[])
    }
}

/// `IWebviewPanelOptions`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelOptions {
    #[serde(default)]
    pub enable_find_widget: bool,
    #[serde(default)]
    pub retain_context_when_hidden: bool,
}

/// `WebviewPanelShowOptions`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShowOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_column: Option<i64>,
    #[serde(default)]
    pub preserve_focus: bool,
}

/// `WebviewExtensionDescription`. `ExtensionIdentifier` is a class on the wire
/// (`{"value": "pub.name", "_lower": "pub.name"}`); a bare string is accepted too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionDescription {
    pub id: String,
    pub location: UriComponents,
}

impl ExtensionDescription {
    pub fn from_json(v: &Value) -> Result<Self> {
        let id = match v.get("id") {
            Some(Value::String(s)) => s.clone(),
            Some(o) => o
                .get("value")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Malformed("extension id has no value".into()))?
                .to_owned(),
            None => return Err(Error::Malformed("extension description without id".into())),
        };
        let location = serde_json::from_value(v.get("location").cloned().unwrap_or(Value::Null))?;
        Ok(Self { id, location })
    }
}

/// `IWebviewIconPath`: a theme icon, or one image per theme kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IconPath {
    ThemeIcon { id: String },
    Images { light: UriComponents, dark: UriComponents },
}

impl IconPath {
    pub fn from_json(v: &Value) -> Option<Self> {
        if let Some(id) = v.get("id").and_then(Value::as_str) {
            return Some(Self::ThemeIcon { id: id.to_owned() });
        }
        let light = serde_json::from_value(v.get("light")?.clone()).ok()?;
        let dark = serde_json::from_value(v.get("dark")?.clone()).ok()?;
        Some(Self::Images { light, dark })
    }
}

/// `IViewBadge`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ViewBadge {
    #[serde(default)]
    pub tooltip: String,
    pub value: i64,
}

/// `CustomEditorProviderCapabilities`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomEditorCapabilities {
    #[serde(default)]
    pub supports_move: bool,
    #[serde(default)]
    pub supports_inline_diff: bool,
    #[serde(default)]
    pub supports_side_by_side_diff: bool,
}

/// One webview message: the JSON text and its side buffers, in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebviewMessage {
    pub json: String,
    pub buffers: Vec<Vec<u8>>,
}

impl WebviewMessage {
    pub fn text(json: impl Into<String>) -> Self {
        Self { json: json.into(), buffers: Vec::new() }
    }

    /// The parsed message (the array buffer references stay as their marker objects).
    pub fn value(&self) -> Option<Value> {
        serde_json::from_str(&self.json).ok()
    }
}

/// The `{"$$vscode_array_buffer_reference$$": true, ...}` markers in a message, in document order:
/// `(index, view)` where `view` is `(type, byteOffset, byteLength)` for a typed array.
pub fn array_buffer_refs(message: &Value) -> Vec<(usize, Option<(u8, u64, u64)>)> {
    fn walk(v: &Value, out: &mut Vec<(usize, Option<(u8, u64, u64)>)>) {
        match v {
            Value::Object(m) => {
                if m.get("$$vscode_array_buffer_reference$$").and_then(Value::as_bool) == Some(true) {
                    if let Some(i) = m.get("index").and_then(Value::as_u64) {
                        let view = m.get("view").and_then(|w| {
                            Some((
                                w.get("type")?.as_u64()? as u8,
                                w.get("byteOffset")?.as_u64()?,
                                w.get("byteLength")?.as_u64()?,
                            ))
                        });
                        out.push((i as usize, view));
                        return;
                    }
                }
                for x in m.values() {
                    walk(x, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(message, &mut out);
    out
}

/// A decoded call of the four webview actors.
#[derive(Debug, Clone, PartialEq)]
pub enum WebviewCall {
    /// `MainThreadWebviews.$setHtml(handle, value)`.
    SetHtml { handle: WebviewHandle, html: String },
    /// `MainThreadWebviews.$setOptions(handle, options)`.
    SetOptions { handle: WebviewHandle, options: ContentOptions },
    /// `MainThreadWebviews.$postMessage(handle, jsonMessage, ...buffers)`: answers `boolean`.
    PostMessage { handle: WebviewHandle, message: WebviewMessage },
    /// `MainThreadWebviewPanels.$createWebviewPanel(extension, handle, viewType, initData, showOptions)`.
    CreatePanel {
        extension: ExtensionDescription,
        handle: WebviewHandle,
        view_type: String,
        title: String,
        content_options: ContentOptions,
        panel_options: PanelOptions,
        serialize_buffers: bool,
        show: ShowOptions,
    },
    /// `MainThreadWebviewPanels.$disposeWebview(handle)`.
    DisposePanel { handle: WebviewHandle },
    /// `MainThreadWebviewPanels.$reveal(handle, showOptions)`.
    Reveal { handle: WebviewHandle, show: ShowOptions },
    /// `MainThreadWebviewPanels.$setTitle(handle, value)`.
    SetTitle { handle: WebviewHandle, title: String },
    /// `MainThreadWebviewPanels.$setIconPath(handle, value | undefined)`.
    SetIconPath { handle: WebviewHandle, icon: Option<IconPath> },
    /// `MainThreadWebviewPanels.$registerSerializer(viewType, { serializeBuffersForPostMessage })`.
    RegisterSerializer { view_type: String, serialize_buffers: bool },
    /// `MainThreadWebviewPanels.$unregisterSerializer(viewType)`.
    UnregisterSerializer { view_type: String },
    /// `MainThreadCustomEditors.$registerTextEditorProvider(extension, viewType, options, capabilities, serializeBuffers)`.
    RegisterTextEditorProvider {
        extension: ExtensionDescription,
        view_type: String,
        options: PanelOptions,
        capabilities: CustomEditorCapabilities,
        serialize_buffers: bool,
    },
    /// `MainThreadCustomEditors.$registerCustomEditorProvider(extension, viewType, options, capabilities,
    /// supportsMultipleEditorsPerDocument, serializeBuffers)`.
    RegisterCustomEditorProvider {
        extension: ExtensionDescription,
        view_type: String,
        options: PanelOptions,
        capabilities: CustomEditorCapabilities,
        multiple_editors_per_document: bool,
        serialize_buffers: bool,
    },
    /// `MainThreadCustomEditors.$unregisterEditorProvider(viewType)`.
    UnregisterEditorProvider { view_type: String },
    /// `MainThreadCustomEditors.$onDidEdit(resource, viewType, editId, label)`.
    CustomEditorEdit { resource: UriComponents, view_type: String, edit_id: i64, label: Option<String> },
    /// `MainThreadCustomEditors.$onContentChange(resource, viewType)`.
    CustomEditorContentChange { resource: UriComponents, view_type: String },
    /// `MainThreadWebviewViews.$registerWebviewViewProvider(extension, viewType, { retainContextWhenHidden, serializeBuffersForPostMessage })`.
    RegisterViewProvider { extension: ExtensionDescription, view_type: String, retain_context_when_hidden: bool, serialize_buffers: bool },
    /// `MainThreadWebviewViews.$unregisterWebviewViewProvider(viewType)`.
    UnregisterViewProvider { view_type: String },
    /// `MainThreadWebviewViews.$setWebviewViewTitle(handle, value | undefined)`.
    SetViewTitle { handle: WebviewHandle, title: Option<String> },
    /// `MainThreadWebviewViews.$setWebviewViewDescription(handle, value | undefined)`.
    SetViewDescription { handle: WebviewHandle, description: Option<String> },
    /// `MainThreadWebviewViews.$setWebviewViewBadge(handle, badge | undefined)`.
    SetViewBadge { handle: WebviewHandle, badge: Option<ViewBadge> },
    /// `MainThreadWebviewViews.$show(handle, preserveFocus)`.
    ShowView { handle: WebviewHandle, preserve_focus: bool },
}

fn s(a: &[Arg], i: usize) -> Result<String> {
    arg_json(a, i)
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::Malformed(format!("webview arg {i} is not a string")))
}

fn opt_s(a: &[Arg], i: usize) -> Option<String> {
    arg_json(a, i).as_str().map(str::to_owned)
}

fn b(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn de<T: serde::de::DeserializeOwned + Default>(v: Value) -> Result<T> {
    if v.is_null() {
        return Ok(T::default());
    }
    Ok(serde_json::from_value(v)?)
}

impl WebviewCall {
    /// Whether `proxy` is one of the four main-thread webview actors.
    pub fn is_webview_proxy(proxy: Option<&str>) -> bool {
        matches!(
            proxy,
            Some(proxy::MAIN_THREAD_WEBVIEWS | proxy::MAIN_THREAD_WEBVIEW_PANELS | proxy::MAIN_THREAD_WEBVIEW_VIEWS | proxy::MAIN_THREAD_CUSTOM_EDITORS)
        )
    }

    /// Decode a request. `Ok(None)` for another actor or a webview method Ember does not model.
    pub fn parse(r: &IncomingRequest) -> Result<Option<Self>> {
        let a = &r.args;
        let call = match (r.proxy, r.method.as_str()) {
            (Some(proxy::MAIN_THREAD_WEBVIEWS), "$setHtml") => Self::SetHtml { handle: s(a, 0)?, html: s(a, 1)? },
            (Some(proxy::MAIN_THREAD_WEBVIEWS), "$setOptions") => {
                Self::SetOptions { handle: s(a, 0)?, options: de(arg_json(a, 1))? }
            }
            (Some(proxy::MAIN_THREAD_WEBVIEWS), "$postMessage") => {
                let buffers = a
                    .iter()
                    .skip(2)
                    .filter_map(|x| match x {
                        Arg::Buffer(b) => Some(b.clone()),
                        _ => None,
                    })
                    .collect();
                Self::PostMessage { handle: s(a, 0)?, message: WebviewMessage { json: s(a, 1)?, buffers } }
            }
            (Some(proxy::MAIN_THREAD_WEBVIEW_PANELS), "$createWebviewPanel") => {
                let init = arg_json(a, 3);
                Self::CreatePanel {
                    extension: ExtensionDescription::from_json(&arg_json(a, 0))?,
                    handle: s(a, 1)?,
                    view_type: s(a, 2)?,
                    title: init.get("title").and_then(Value::as_str).unwrap_or_default().to_owned(),
                    content_options: de(init.get("webviewOptions").cloned().unwrap_or(Value::Null))?,
                    panel_options: de(init.get("panelOptions").cloned().unwrap_or(Value::Null))?,
                    serialize_buffers: b(&init, "serializeBuffersForPostMessage"),
                    show: de(arg_json(a, 4))?,
                }
            }
            (Some(proxy::MAIN_THREAD_WEBVIEW_PANELS), "$disposeWebview") => Self::DisposePanel { handle: s(a, 0)? },
            (Some(proxy::MAIN_THREAD_WEBVIEW_PANELS), "$reveal") => Self::Reveal { handle: s(a, 0)?, show: de(arg_json(a, 1))? },
            (Some(proxy::MAIN_THREAD_WEBVIEW_PANELS), "$setTitle") => Self::SetTitle { handle: s(a, 0)?, title: s(a, 1)? },
            (Some(proxy::MAIN_THREAD_WEBVIEW_PANELS), "$setIconPath") => {
                Self::SetIconPath { handle: s(a, 0)?, icon: IconPath::from_json(&arg_json(a, 1)) }
            }
            (Some(proxy::MAIN_THREAD_WEBVIEW_PANELS), "$registerSerializer") => Self::RegisterSerializer {
                view_type: s(a, 0)?,
                serialize_buffers: b(&arg_json(a, 1), "serializeBuffersForPostMessage"),
            },
            (Some(proxy::MAIN_THREAD_WEBVIEW_PANELS), "$unregisterSerializer") => Self::UnregisterSerializer { view_type: s(a, 0)? },
            (Some(proxy::MAIN_THREAD_CUSTOM_EDITORS), "$registerTextEditorProvider") => Self::RegisterTextEditorProvider {
                extension: ExtensionDescription::from_json(&arg_json(a, 0))?,
                view_type: s(a, 1)?,
                options: de(arg_json(a, 2))?,
                capabilities: de(arg_json(a, 3))?,
                serialize_buffers: arg_json(a, 4).as_bool().unwrap_or(false),
            },
            (Some(proxy::MAIN_THREAD_CUSTOM_EDITORS), "$registerCustomEditorProvider") => Self::RegisterCustomEditorProvider {
                extension: ExtensionDescription::from_json(&arg_json(a, 0))?,
                view_type: s(a, 1)?,
                options: de(arg_json(a, 2))?,
                capabilities: de(arg_json(a, 3))?,
                multiple_editors_per_document: arg_json(a, 4).as_bool().unwrap_or(false),
                serialize_buffers: arg_json(a, 5).as_bool().unwrap_or(false),
            },
            (Some(proxy::MAIN_THREAD_CUSTOM_EDITORS), "$unregisterEditorProvider") => Self::UnregisterEditorProvider { view_type: s(a, 0)? },
            (Some(proxy::MAIN_THREAD_CUSTOM_EDITORS), "$onDidEdit") => Self::CustomEditorEdit {
                resource: serde_json::from_value(arg_json(a, 0))?,
                view_type: s(a, 1)?,
                edit_id: arg_json(a, 2).as_i64().ok_or_else(|| Error::Malformed("editId is not an integer".into()))?,
                label: opt_s(a, 3),
            },
            (Some(proxy::MAIN_THREAD_CUSTOM_EDITORS), "$onContentChange") => Self::CustomEditorContentChange {
                resource: serde_json::from_value(arg_json(a, 0))?,
                view_type: s(a, 1)?,
            },
            (Some(proxy::MAIN_THREAD_WEBVIEW_VIEWS), "$registerWebviewViewProvider") => {
                let o = arg_json(a, 2);
                Self::RegisterViewProvider {
                    extension: ExtensionDescription::from_json(&arg_json(a, 0))?,
                    view_type: s(a, 1)?,
                    retain_context_when_hidden: b(&o, "retainContextWhenHidden"),
                    serialize_buffers: b(&o, "serializeBuffersForPostMessage"),
                }
            }
            (Some(proxy::MAIN_THREAD_WEBVIEW_VIEWS), "$unregisterWebviewViewProvider") => Self::UnregisterViewProvider { view_type: s(a, 0)? },
            (Some(proxy::MAIN_THREAD_WEBVIEW_VIEWS), "$setWebviewViewTitle") => Self::SetViewTitle { handle: s(a, 0)?, title: opt_s(a, 1) },
            (Some(proxy::MAIN_THREAD_WEBVIEW_VIEWS), "$setWebviewViewDescription") => {
                Self::SetViewDescription { handle: s(a, 0)?, description: opt_s(a, 1) }
            }
            (Some(proxy::MAIN_THREAD_WEBVIEW_VIEWS), "$setWebviewViewBadge") => {
                Self::SetViewBadge { handle: s(a, 0)?, badge: serde_json::from_value(arg_json(a, 1)).ok() }
            }
            (Some(proxy::MAIN_THREAD_WEBVIEW_VIEWS), "$show") => {
                Self::ShowView { handle: s(a, 0)?, preserve_focus: arg_json(a, 1).as_bool().unwrap_or(false) }
            }
            _ => return Ok(None),
        };
        Ok(Some(call))
    }
}

// ---- Ember -> extension host -------------------------------------------------------------------

/// `ExtHostWebviews.$onMessage(handle, jsonSerializedMessage, SerializableObjectWithBuffers<VSBuffer[]>)`:
/// a message from the page (`vscode.postMessage`). The buffers travel as `{"$$ref$$": i}` nodes of one
/// array.
pub fn on_message(handle: &str, message: &WebviewMessage) -> Call {
    let refs: Vec<Value> = (0..message.buffers.len()).map(|i| json!({ "$$ref$$": i })).collect();
    Call::new(
        proxy::EXT_HOST_WEBVIEWS,
        "$onMessage",
        vec![
            Arg::Json(handle.into()),
            Arg::Json(Value::String(message.json.clone())),
            Arg::JsonWithBuffers { value: Value::Array(refs), buffers: message.buffers.clone() },
        ],
    )
}

/// `ExtHostWebviews.$onMissingCsp(handle, extensionId)`: the page has no CSP `<meta>`.
pub fn on_missing_csp(handle: &str, extension_id: &str) -> Call {
    Call::new(proxy::EXT_HOST_WEBVIEWS, "$onMissingCsp", vec![Arg::Json(handle.into()), Arg::Json(extension_id.into())])
}

/// One entry of `WebviewPanelViewStateData`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelViewState {
    pub active: bool,
    pub visible: bool,
    pub position: i64,
}

/// `ExtHostWebviewPanels.$onDidChangeWebviewPanelViewStates({ [handle]: { active, visible, position } })`.
pub fn on_did_change_view_states(states: &[(WebviewHandle, PanelViewState)]) -> Call {
    let mut m = Map::new();
    for (h, st) in states {
        m.insert(h.clone(), json!({ "active": st.active, "visible": st.visible, "position": st.position }));
    }
    Call::new(proxy::EXT_HOST_WEBVIEW_PANELS, "$onDidChangeWebviewPanelViewStates", vec![Arg::Json(Value::Object(m))])
}

/// `ExtHostWebviewPanels.$onDidDisposeWebviewPanel(handle)`: the user closed the panel (or custom editor).
pub fn on_did_dispose_panel(handle: &str) -> Call {
    Call::new(proxy::EXT_HOST_WEBVIEW_PANELS, "$onDidDisposeWebviewPanel", vec![Arg::Json(handle.into())])
}

/// `ExtHostWebviewPanels.$deserializeWebviewPanel(newHandle, viewType, { title, state, webviewOptions,
/// panelOptions, active }, position)`: revive a panel restored with the window.
#[allow(clippy::too_many_arguments)]
pub fn deserialize_webview_panel(
    handle: &str,
    view_type: &str,
    title: &str,
    state: Option<&Value>,
    content: &ContentOptions,
    panel: &PanelOptions,
    active: bool,
    position: i64,
) -> Call {
    let init = json!({
        "title": title,
        "state": state.cloned().unwrap_or(Value::Null),
        "webviewOptions": content,
        "panelOptions": panel,
        "active": active,
    });
    Call::new(
        proxy::EXT_HOST_WEBVIEW_PANELS,
        "$deserializeWebviewPanel",
        vec![Arg::Json(handle.into()), Arg::Json(view_type.into()), Arg::Json(init), Arg::Json(position.into())],
    )
}

/// `ExtHostWebviewViews.$resolveWebviewView(handle, viewType, title, state, cancellation)`.
pub fn resolve_webview_view(handle: &str, view_type: &str, title: Option<&str>, state: Option<&Value>) -> Call {
    Call::new(
        proxy::EXT_HOST_WEBVIEW_VIEWS,
        "$resolveWebviewView",
        vec![
            Arg::Json(handle.into()),
            Arg::Json(view_type.into()),
            title.map(|t| Arg::Json(t.into())).unwrap_or(Arg::Undefined),
            state.map(|s| Arg::Json(s.clone())).unwrap_or(Arg::Undefined),
        ],
    )
    .cancellable()
}

/// `ExtHostWebviewViews.$onDidChangeWebviewViewVisibility(handle, visible)`.
pub fn on_did_change_view_visibility(handle: &str, visible: bool) -> Call {
    Call::new(proxy::EXT_HOST_WEBVIEW_VIEWS, "$onDidChangeWebviewViewVisibility", vec![Arg::Json(handle.into()), Arg::Json(visible.into())])
}

/// `ExtHostWebviewViews.$disposeWebviewView(handle)`.
pub fn dispose_webview_view(handle: &str) -> Call {
    Call::new(proxy::EXT_HOST_WEBVIEW_VIEWS, "$disposeWebviewView", vec![Arg::Json(handle.into())])
}

/// `ExtHostCustomEditors.$createCustomDocument(resource, viewType, backupId, untitledDocumentData, cancellation)`:
/// answers `{ editable }`. Only for `$registerCustomEditorProvider` providers.
pub fn create_custom_document(resource: &UriComponents, view_type: &str) -> Call {
    Call::new(
        proxy::EXT_HOST_CUSTOM_EDITORS,
        "$createCustomDocument",
        vec![crate::exthost::j(resource), Arg::Json(view_type.into()), Arg::Undefined, Arg::Undefined],
    )
    .cancellable()
}

/// `ExtHostCustomEditors.$disposeCustomDocument(resource, viewType)`.
pub fn dispose_custom_document(resource: &UriComponents, view_type: &str) -> Call {
    Call::new(proxy::EXT_HOST_CUSTOM_EDITORS, "$disposeCustomDocument", vec![crate::exthost::j(resource), Arg::Json(view_type.into())])
}

/// `ExtHostCustomEditors.$resolveCustomEditor(resource, newHandle, viewType, { title, contentOptions,
/// options, active }, position, cancellation)`.
#[allow(clippy::too_many_arguments)]
pub fn resolve_custom_editor(
    resource: &UriComponents,
    handle: &str,
    view_type: &str,
    title: &str,
    content: &ContentOptions,
    panel: &PanelOptions,
    active: bool,
    position: i64,
) -> Call {
    let init = json!({ "title": title, "contentOptions": content, "options": panel, "active": active });
    Call::new(
        proxy::EXT_HOST_CUSTOM_EDITORS,
        "$resolveCustomEditor",
        vec![
            crate::exthost::j(resource),
            Arg::Json(handle.into()),
            Arg::Json(view_type.into()),
            Arg::Json(init),
            Arg::Json(position.into()),
        ],
    )
    .cancellable()
}
