//! Extension webviews, the session side (SPEC FR-E8, INTENT D44, `docs/design/EDITOR-WEBVIEWS.md`).
//!
//! [`Webviews`] keeps one [`WebviewRecord`] per webview handle (panels, webview views, custom
//! editors), turns the extension host's calls ([`WebviewCall`]) into [`WebviewUpdate`]s for the
//! window and the window's [`WebviewEvent`]s into calls back to the extension host. It does no I/O:
//! [`crate::SessionCore`] executes what it returns ([`Out`]), like every other `MainThread*` feature.
//!
//! Rules kept here, each as Code-OSS keeps it:
//!
//! - **Queue until ready.** A `$postMessage` to a page that has not reported ready (just created,
//!   after `$setHtml`, or after a hidden page without `retainContextWhenHidden` was dropped) waits in
//!   the record's queue and is sent, in order, when the page is ready (`webviewElement.ts` keeps
//!   pending messages until the frame is ready). `$postMessage` answers `true` for a known handle.
//! - **Local resources.** A request URL is answered with a file only under the record's
//!   `localResourceRoots` ([`resource::check`]); everything else is refused.
//! - **State.** `setState` in the page is kept as JSON text and given back when the page is made
//!   again (`$deserializeWebviewPanel`, `$resolveWebviewView`, and the init script after a reload).
//! - **View states.** A panel's `active`, `visible` and `position` go to the extension host for every
//!   panel at once (`$onDidChangeWebviewPanelViewStates`), as `updateWebviewViewStates` does.
//!
//! The window draws the webview with a host behind the [`host::WebviewHost`] trait; until the
//! compose-rust host node exists it shows [`host::WebviewPanes`] placeholders.

pub mod host;
pub mod resource;
pub mod shim;
pub mod theme;

use std::collections::{HashMap, VecDeque};

use ember_editor_conn::exthost::Call;
use ember_editor_conn::rpc::Reply;
use ember_editor_conn::uri::UriComponents;
use ember_editor_conn::webview::{
    self as wire, ContentOptions, CustomEditorCapabilities, IconPath, PanelOptions, PanelViewState, ShowOptions, ViewBadge, WebviewCall,
    WebviewHandle, WebviewMessage, SIDE_GROUP,
};
use serde_json::Value;

pub use resource::ResourceResponse;

/// What kind of place a webview has in the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebviewKind {
    /// `vscode.window.createWebviewPanel`: an editor tab.
    Panel { view_type: String },
    /// A `WebviewViewProvider`'s view: a view in a tool window.
    View { view_type: String },
    /// A custom editor of `resource`: an editor tab. `text` for a `CustomTextEditorProvider`.
    CustomEditor { view_type: String, resource: UriComponents, text: bool },
}

impl WebviewKind {
    pub fn view_type(&self) -> &str {
        match self {
            Self::Panel { view_type } | Self::View { view_type } | Self::CustomEditor { view_type, .. } => view_type,
        }
    }

    /// Panels and custom editors are editor tabs; their view states go to `ExtHostWebviewPanels`.
    pub fn is_editor_tab(&self) -> bool {
        !matches!(self, Self::View { .. })
    }
}

/// One webview, by handle.
#[derive(Debug, Clone, PartialEq)]
pub struct WebviewRecord {
    pub handle: WebviewHandle,
    pub kind: WebviewKind,
    pub extension_id: String,
    /// `vscode-webview://<uuid>`: unique per webview, so pages cannot read each other.
    pub origin: String,
    pub title: String,
    pub description: Option<String>,
    pub badge: Option<ViewBadge>,
    pub icon: Option<IconPath>,
    /// The last `$setHtml`; `None` until the extension sets one.
    pub html: Option<String>,
    pub content: ContentOptions,
    pub panel: PanelOptions,
    pub serialize_buffers: bool,
    /// The JSON text of the page's last `setState`.
    pub state: Option<String>,
    pub visible: bool,
    pub active: bool,
    pub position: i64,
    pub ready: bool,
    pub focused: bool,
    queue: VecDeque<WebviewMessage>,
}

impl WebviewRecord {
    /// Messages waiting for the page to be ready.
    pub fn queued(&self) -> impl Iterator<Item = &WebviewMessage> {
        self.queue.iter()
    }
}

/// A registered webview view provider (`registerWebviewViewProvider`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewProvider {
    pub view_type: String,
    pub extension_id: String,
    pub retain_context_when_hidden: bool,
    pub serialize_buffers: bool,
}

/// A registered custom editor provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorProvider {
    pub view_type: String,
    pub extension_id: String,
    pub options: PanelOptions,
    pub capabilities: CustomEditorCapabilities,
    /// `registerCustomTextEditorProvider` (the document is a text document).
    pub text: bool,
    pub multiple_editors_per_document: bool,
    pub serialize_buffers: bool,
}

/// From the session to the window.
#[derive(Debug, Clone, PartialEq)]
pub enum WebviewUpdate {
    /// Make a webview (and its tab or view). `reveal` is `Some` for panels shown at creation.
    Created {
        handle: WebviewHandle,
        kind: WebviewKind,
        title: String,
        origin: String,
        content: ContentOptions,
        panel: PanelOptions,
        state: Option<String>,
        reveal: Option<ShowOptions>,
    },
    /// Load this HTML (the page reloads; `state` is what its init script starts with).
    Html { handle: WebviewHandle, html: String, state: Option<String> },
    /// New content options; the page reloads with them if it has HTML.
    Options { handle: WebviewHandle, content: ContentOptions },
    /// Deliver a message to the ready page.
    Post { handle: WebviewHandle, message: WebviewMessage },
    /// The page reported ready (queued messages follow as `Post`).
    Ready { handle: WebviewHandle },
    Title { handle: WebviewHandle, title: String },
    Description { handle: WebviewHandle, description: Option<String> },
    Badge { handle: WebviewHandle, badge: Option<ViewBadge> },
    Icon { handle: WebviewHandle, icon: Option<IconPath> },
    /// Show the tab or view; take focus unless `preserve_focus`.
    Reveal { handle: WebviewHandle, preserve_focus: bool, view_column: Option<i64> },
    /// The answer to a resource request of the page.
    Resource { handle: WebviewHandle, request: u64, response: ResourceResponse },
    /// A link the page clicked that the host opens (http, https, mailto, vscode).
    OpenLink { uri: String },
    /// The webview is gone.
    Disposed { handle: WebviewHandle },
    /// Opening a custom editor or reviving a panel failed (no provider, no serializer).
    OpenFailed { view_type: String, resource: Option<UriComponents>, reason: String },
    /// The webview view and custom editor providers changed (for the window's view list and "Open With").
    Providers { views: Vec<ViewProvider>, editors: Vec<EditorProvider> },
}

/// From the window (and its host) to the session.
#[derive(Debug, Clone, PartialEq)]
pub enum WebviewEvent {
    /// The page finished loading and can receive messages.
    Ready { handle: WebviewHandle },
    /// The page called `vscode.postMessage`.
    Message { handle: WebviewHandle, message: WebviewMessage },
    /// The page called `vscode.setState` (JSON text).
    State { handle: WebviewHandle, state: String },
    /// A panel or custom editor tab changed: shown in its group, the active editor, its column.
    ViewState { handle: WebviewHandle, visible: bool, active: bool, position: i64 },
    /// A webview view was shown or hidden in its tool window.
    ViewVisible { handle: WebviewHandle, visible: bool },
    Focus { handle: WebviewHandle, focused: bool },
    /// The user closed the tab or the view.
    Closed { handle: WebviewHandle },
    /// The window shows webview view `view_type` for the first time (or again after it was closed).
    ResolveView { view_type: String, title: Option<String> },
    /// Open `resource` with custom editor `view_type`.
    OpenCustomEditor { resource: UriComponents, view_type: String },
    /// Revive a panel the window restored (needs a serializer for `view_type`).
    RestorePanel { view_type: String, title: String, state: Option<String>, content: ContentOptions, panel: PanelOptions, position: i64 },
    /// The page asked for `url` (the host's scheme handler); answered with [`WebviewUpdate::Resource`].
    ResourceRequested { handle: WebviewHandle, request: u64, url: String },
    LinkClicked { handle: WebviewHandle, uri: String },
    /// The page has no Content-Security-Policy `<meta>`.
    MissingCsp { handle: WebviewHandle },
}

/// A call whose reply the model waits for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pending {
    /// `$createCustomDocument` before `$resolveCustomEditor` of `handle`.
    CustomDocument { handle: WebviewHandle },
    /// `$resolveCustomEditor`, `$resolveWebviewView` or `$deserializeWebviewPanel` of `handle`.
    Resolve { handle: WebviewHandle },
}

/// What [`Webviews`] asks the session to do, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    Rpc(Call),
    /// A call whose reply comes back through [`Webviews::on_reply`].
    RpcWith(Call, Pending),
    /// Answer extension-host request `req`.
    Respond { req: u32, reply: Reply },
    Ui(WebviewUpdate),
    /// Read a local resource for `(handle, request)`; the bytes come back through [`Webviews::on_resource_read`].
    ReadFile { uri: UriComponents, handle: WebviewHandle, request: u64 },
    /// Run a `command:` link the page clicked (allowed by `enableCommandUris`).
    Command { id: String, arguments: Vec<Value> },
}

/// Code-OSS's `getWebviewResolvedFailedContent`, in Korean.
pub fn failed_content(view_type: &str) -> String {
    let escaped = view_type.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
    format!(
        "<!DOCTYPE html><html><head><meta http-equiv=\"Content-type\" content=\"text/html;charset=UTF-8\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none';\"></head>\
         <body>보기를 불러오는 중 오류가 발생했습니다: {escaped}</body></html>"
    )
}

/// The link schemes a page may open (`MainThreadWebviews.standardSupportedLinkSchemes`).
const LINK_SCHEMES: [&str; 5] = ["http", "https", "mailto", "vscode", "vscode-insider"];

fn column_of(show: &ShowOptions) -> i64 {
    match show.view_column {
        Some(c) if c >= 0 => c,
        Some(SIDE_GROUP) => 1,
        // ACTIVE_GROUP, or no column: the active group, column 0 while there is one group.
        _ => 0,
    }
}

/// Every webview of one session.
#[derive(Debug, Default)]
pub struct Webviews {
    records: HashMap<WebviewHandle, WebviewRecord>,
    /// Creation order, for a stable view-state map and listing.
    order: Vec<WebviewHandle>,
    views: Vec<ViewProvider>,
    editors: Vec<EditorProvider>,
    serializers: HashMap<String, bool>,
    /// The state a closed webview view had, given back when it is resolved again.
    view_states: HashMap<String, String>,
    next_handle: u64,
    out: Vec<Out>,
}

impl Webviews {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, handle: &str) -> Option<&WebviewRecord> {
        self.records.get(handle)
    }

    /// Every webview, oldest first.
    pub fn records(&self) -> impl Iterator<Item = &WebviewRecord> {
        self.order.iter().filter_map(|h| self.records.get(h))
    }

    pub fn view_providers(&self) -> &[ViewProvider] {
        &self.views
    }

    pub fn editor_providers(&self) -> &[EditorProvider] {
        &self.editors
    }

    /// The custom editor providers whose `viewType` matches; the window matches file patterns from
    /// `contributes.customEditors`.
    pub fn editor_provider(&self, view_type: &str) -> Option<&EditorProvider> {
        self.editors.iter().find(|p| p.view_type == view_type)
    }

    fn new_handle(&mut self) -> WebviewHandle {
        self.next_handle += 1;
        format!("ember-webview-{}", self.next_handle)
    }

    fn new_origin() -> String {
        format!("vscode-webview://{}", uuid::Uuid::new_v4())
    }

    fn take(&mut self) -> Vec<Out> {
        std::mem::take(&mut self.out)
    }

    fn ui(&mut self, u: WebviewUpdate) {
        self.out.push(Out::Ui(u));
    }

    fn providers_changed(&mut self) {
        let u = WebviewUpdate::Providers { views: self.views.clone(), editors: self.editors.clone() };
        self.ui(u);
    }

    #[allow(clippy::too_many_arguments)]
    fn insert(
        &mut self,
        handle: WebviewHandle,
        kind: WebviewKind,
        extension_id: String,
        title: String,
        content: ContentOptions,
        panel: PanelOptions,
        serialize_buffers: bool,
        state: Option<String>,
        visible: bool,
        active: bool,
        position: i64,
        reveal: Option<ShowOptions>,
    ) {
        let origin = Self::new_origin();
        let record = WebviewRecord {
            handle: handle.clone(),
            kind: kind.clone(),
            extension_id,
            origin: origin.clone(),
            title: title.clone(),
            description: None,
            badge: None,
            icon: None,
            html: None,
            content: content.clone(),
            panel,
            serialize_buffers,
            state: state.clone(),
            visible,
            active,
            position,
            ready: false,
            focused: false,
            queue: VecDeque::new(),
        };
        if !self.records.contains_key(&handle) {
            self.order.push(handle.clone());
        }
        self.records.insert(handle.clone(), record);
        self.ui(WebviewUpdate::Created { handle, kind, title, origin, content, panel, state, reveal });
    }

    fn remove(&mut self, handle: &str) -> Option<WebviewRecord> {
        self.order.retain(|h| h != handle);
        self.records.remove(handle)
    }

    /// Handle one extension-host request of the webview actors (`req` is answered).
    pub fn on_call(&mut self, req: u32, call: WebviewCall) -> Vec<Out> {
        let mut reply = Reply::Empty;
        match call {
            WebviewCall::SetHtml { handle, html } => {
                if let Some(r) = self.records.get_mut(&handle) {
                    r.html = Some(html.clone());
                    r.ready = false;
                    let state = r.state.clone();
                    self.ui(WebviewUpdate::Html { handle, html, state });
                }
            }
            WebviewCall::SetOptions { handle, options } => {
                if let Some(r) = self.records.get_mut(&handle) {
                    let reload = r.html.is_some() && r.content != options;
                    r.content = options.clone();
                    if reload {
                        r.ready = false;
                    }
                    self.ui(WebviewUpdate::Options { handle, content: options });
                }
            }
            WebviewCall::PostMessage { handle, message } => {
                reply = Reply::Json(Value::Bool(self.records.contains_key(&handle)));
                self.post(&handle, message);
            }
            WebviewCall::CreatePanel { extension, handle, view_type, title, content_options, panel_options, serialize_buffers, show } => {
                let position = column_of(&show);
                self.insert(
                    handle,
                    WebviewKind::Panel { view_type },
                    extension.id,
                    title,
                    content_options,
                    panel_options,
                    serialize_buffers,
                    None,
                    true,
                    !show.preserve_focus,
                    position,
                    Some(show),
                );
            }
            WebviewCall::DisposePanel { handle } => {
                if self.remove(&handle).is_some() {
                    // Code-OSS answers its own dispose with `$onDidDisposeWebviewPanel` too.
                    self.out.push(Out::Rpc(wire::on_did_dispose_panel(&handle)));
                    self.ui(WebviewUpdate::Disposed { handle });
                }
            }
            WebviewCall::Reveal { handle, show } => {
                if self.records.contains_key(&handle) {
                    self.ui(WebviewUpdate::Reveal { handle, preserve_focus: show.preserve_focus, view_column: show.view_column });
                }
            }
            WebviewCall::SetTitle { handle, title } => {
                if let Some(r) = self.records.get_mut(&handle) {
                    r.title = title.clone();
                    self.ui(WebviewUpdate::Title { handle, title });
                }
            }
            WebviewCall::SetIconPath { handle, icon } => {
                if let Some(r) = self.records.get_mut(&handle) {
                    r.icon = icon.clone();
                    self.ui(WebviewUpdate::Icon { handle, icon });
                }
            }
            WebviewCall::RegisterSerializer { view_type, serialize_buffers } => {
                self.serializers.insert(view_type, serialize_buffers);
            }
            WebviewCall::UnregisterSerializer { view_type } => {
                self.serializers.remove(&view_type);
            }
            WebviewCall::RegisterTextEditorProvider { extension, view_type, options, capabilities, serialize_buffers } => {
                self.editors.retain(|p| p.view_type != view_type);
                self.editors.push(EditorProvider {
                    view_type,
                    extension_id: extension.id,
                    options,
                    capabilities,
                    text: true,
                    multiple_editors_per_document: true,
                    serialize_buffers,
                });
                self.providers_changed();
            }
            WebviewCall::RegisterCustomEditorProvider { extension, view_type, options, capabilities, multiple_editors_per_document, serialize_buffers } => {
                self.editors.retain(|p| p.view_type != view_type);
                self.editors.push(EditorProvider {
                    view_type,
                    extension_id: extension.id,
                    options,
                    capabilities,
                    text: false,
                    multiple_editors_per_document,
                    serialize_buffers,
                });
                self.providers_changed();
            }
            WebviewCall::UnregisterEditorProvider { view_type } => {
                self.editors.retain(|p| p.view_type != view_type);
                self.providers_changed();
            }
            // Dirty state and undo of custom documents are not modelled yet (the tab's dirty mark
            // waits for the custom editor tab in the window).
            WebviewCall::CustomEditorEdit { .. } | WebviewCall::CustomEditorContentChange { .. } => {}
            WebviewCall::RegisterViewProvider { extension, view_type, retain_context_when_hidden, serialize_buffers } => {
                self.views.retain(|p| p.view_type != view_type);
                self.views.push(ViewProvider { view_type, extension_id: extension.id, retain_context_when_hidden, serialize_buffers });
                self.providers_changed();
            }
            WebviewCall::UnregisterViewProvider { view_type } => {
                self.views.retain(|p| p.view_type != view_type);
                self.providers_changed();
            }
            WebviewCall::SetViewTitle { handle, title } => {
                if let Some(r) = self.records.get_mut(&handle) {
                    r.title = title.clone().unwrap_or_default();
                    let title = r.title.clone();
                    self.ui(WebviewUpdate::Title { handle, title });
                }
            }
            WebviewCall::SetViewDescription { handle, description } => {
                if let Some(r) = self.records.get_mut(&handle) {
                    r.description = description.clone();
                    self.ui(WebviewUpdate::Description { handle, description });
                }
            }
            WebviewCall::SetViewBadge { handle, badge } => {
                if let Some(r) = self.records.get_mut(&handle) {
                    r.badge = badge.clone();
                    self.ui(WebviewUpdate::Badge { handle, badge });
                }
            }
            WebviewCall::ShowView { handle, preserve_focus } => {
                if self.records.contains_key(&handle) {
                    self.ui(WebviewUpdate::Reveal { handle, preserve_focus, view_column: None });
                }
            }
        }
        self.out.push(Out::Respond { req, reply });
        self.take()
    }

    /// The queue rule: deliver now if the page is ready, else keep it in order.
    fn post(&mut self, handle: &str, message: WebviewMessage) {
        let Some(r) = self.records.get_mut(handle) else { return };
        if r.ready && r.queue.is_empty() {
            self.out.push(Out::Ui(WebviewUpdate::Post { handle: handle.to_owned(), message }));
        } else {
            r.queue.push_back(message);
        }
    }

    /// Handle one event of the window or its host.
    pub fn on_event(&mut self, event: WebviewEvent) -> Vec<Out> {
        match event {
            WebviewEvent::Ready { handle } => {
                if let Some(r) = self.records.get_mut(&handle) {
                    r.ready = true;
                    let queued: Vec<WebviewMessage> = r.queue.drain(..).collect();
                    self.ui(WebviewUpdate::Ready { handle: handle.clone() });
                    for message in queued {
                        self.ui(WebviewUpdate::Post { handle: handle.clone(), message });
                    }
                }
            }
            WebviewEvent::Message { handle, message } => {
                if self.records.contains_key(&handle) {
                    self.out.push(Out::Rpc(wire::on_message(&handle, &message)));
                }
            }
            WebviewEvent::State { handle, state } => {
                if let Some(r) = self.records.get_mut(&handle) {
                    r.state = Some(state);
                }
            }
            WebviewEvent::ViewState { handle, visible, active, position } => {
                let Some(r) = self.records.get_mut(&handle) else { return self.take() };
                if !r.kind.is_editor_tab() {
                    return self.take();
                }
                let changed = (r.visible, r.active, r.position) != (visible, active, position);
                if !visible && !r.panel.retain_context_when_hidden {
                    // The host drops a hidden page that does not retain its context; it reloads on show.
                    r.ready = false;
                }
                r.visible = visible;
                r.active = active;
                r.position = position;
                if active {
                    for (h, other) in self.records.iter_mut() {
                        if *h != handle && other.kind.is_editor_tab() {
                            other.active = false;
                        }
                    }
                }
                if changed {
                    self.send_view_states();
                }
            }
            WebviewEvent::ViewVisible { handle, visible } => {
                if let Some(r) = self.records.get_mut(&handle) {
                    if r.visible != visible {
                        r.visible = visible;
                        if !visible && !r.panel.retain_context_when_hidden {
                            r.ready = false;
                        }
                        self.out.push(Out::Rpc(wire::on_did_change_view_visibility(&handle, visible)));
                    }
                }
            }
            WebviewEvent::Focus { handle, focused } => {
                if let Some(r) = self.records.get_mut(&handle) {
                    r.focused = focused;
                }
            }
            WebviewEvent::Closed { handle } => {
                if let Some(r) = self.remove(&handle) {
                    match &r.kind {
                        WebviewKind::View { view_type } => {
                            if let Some(state) = &r.state {
                                self.view_states.insert(view_type.clone(), state.clone());
                            }
                            self.out.push(Out::Rpc(wire::dispose_webview_view(&handle)));
                        }
                        WebviewKind::Panel { .. } => self.out.push(Out::Rpc(wire::on_did_dispose_panel(&handle))),
                        WebviewKind::CustomEditor { view_type, resource, text } => {
                            self.out.push(Out::Rpc(wire::on_did_dispose_panel(&handle)));
                            let still_open = self.records.values().any(|o| {
                                matches!(&o.kind, WebviewKind::CustomEditor { view_type: v, resource: res, .. } if v == view_type && res == resource)
                            });
                            if !*text && !still_open {
                                self.out.push(Out::Rpc(wire::dispose_custom_document(resource, view_type)));
                            }
                        }
                    }
                    self.ui(WebviewUpdate::Disposed { handle });
                }
            }
            WebviewEvent::ResolveView { view_type, title } => {
                let Some(p) = self.views.iter().find(|p| p.view_type == view_type).cloned() else {
                    self.ui(WebviewUpdate::OpenFailed { view_type, resource: None, reason: "웹뷰 보기를 제공하는 확장이 없습니다".into() });
                    return self.take();
                };
                let handle = self.new_handle();
                let state = self.view_states.remove(&view_type);
                let panel = PanelOptions { enable_find_widget: false, retain_context_when_hidden: p.retain_context_when_hidden };
                self.insert(
                    handle.clone(),
                    WebviewKind::View { view_type: view_type.clone() },
                    p.extension_id.clone(),
                    title.clone().unwrap_or_default(),
                    ContentOptions::default(),
                    panel,
                    p.serialize_buffers,
                    state.clone(),
                    true,
                    false,
                    0,
                    None,
                );
                let state_value = state.as_deref().and_then(|s| serde_json::from_str::<Value>(s).ok());
                let call = wire::resolve_webview_view(&handle, &view_type, title.as_deref(), state_value.as_ref());
                self.out.push(Out::RpcWith(call, Pending::Resolve { handle }));
            }
            WebviewEvent::OpenCustomEditor { resource, view_type } => {
                let Some(p) = self.editor_provider(&view_type).cloned() else {
                    self.ui(WebviewUpdate::OpenFailed {
                        view_type,
                        resource: Some(resource),
                        reason: "이 편집기를 제공하는 확장이 없습니다".into(),
                    });
                    return self.take();
                };
                let doc_open = self.records.values().any(|o| {
                    matches!(&o.kind, WebviewKind::CustomEditor { view_type: v, resource: res, .. } if *v == view_type && *res == resource)
                });
                let handle = self.new_handle();
                let title = resource.path.rsplit('/').next().unwrap_or_default().to_owned();
                self.insert(
                    handle.clone(),
                    WebviewKind::CustomEditor { view_type: view_type.clone(), resource: resource.clone(), text: p.text },
                    p.extension_id.clone(),
                    title,
                    ContentOptions::default(),
                    p.options,
                    p.serialize_buffers,
                    None,
                    true,
                    true,
                    0,
                    Some(ShowOptions::default()),
                );
                if p.text || doc_open {
                    self.resolve_custom_editor(&handle);
                } else {
                    let call = wire::create_custom_document(&resource, &view_type);
                    self.out.push(Out::RpcWith(call, Pending::CustomDocument { handle }));
                }
            }
            WebviewEvent::RestorePanel { view_type, title, state, content, panel, position } => {
                let Some(serialize_buffers) = self.serializers.get(&view_type).copied() else {
                    self.ui(WebviewUpdate::OpenFailed { view_type, resource: None, reason: "이 패널을 복원할 확장이 없습니다".into() });
                    return self.take();
                };
                let handle = self.new_handle();
                self.insert(
                    handle.clone(),
                    WebviewKind::Panel { view_type: view_type.clone() },
                    String::new(),
                    title.clone(),
                    content.clone(),
                    panel,
                    serialize_buffers,
                    state.clone(),
                    true,
                    false,
                    position,
                    None,
                );
                let state_value = state.as_deref().and_then(|s| serde_json::from_str::<Value>(s).ok());
                let call = wire::deserialize_webview_panel(&handle, &view_type, &title, state_value.as_ref(), &content, &panel, false, position);
                self.out.push(Out::RpcWith(call, Pending::Resolve { handle }));
            }
            WebviewEvent::ResourceRequested { handle, request, url } => {
                let decision = match self.records.get(&handle) {
                    Some(r) => resource::check(&url, r.content.roots()),
                    None => resource::Decision::Denied,
                };
                match decision {
                    resource::Decision::Allowed(uri) => self.out.push(Out::ReadFile { uri, handle, request }),
                    resource::Decision::Denied => self.ui(WebviewUpdate::Resource { handle, request, response: ResourceResponse::Denied }),
                }
            }
            WebviewEvent::LinkClicked { handle, uri } => {
                let Some(r) = self.records.get(&handle) else { return self.take() };
                let scheme = uri.split_once(':').map(|(s, _)| s.to_ascii_lowercase()).unwrap_or_default();
                if LINK_SCHEMES.contains(&scheme.as_str()) {
                    self.ui(WebviewUpdate::OpenLink { uri });
                } else if scheme == "command" {
                    let rest = &uri["command:".len()..];
                    let (id, query) = rest.split_once('?').unwrap_or((rest, ""));
                    if r.content.enable_command_uris.allows(id) {
                        let arguments = command_arguments(query);
                        self.out.push(Out::Command { id: id.to_owned(), arguments });
                    }
                }
            }
            WebviewEvent::MissingCsp { handle } => {
                if let Some(r) = self.records.get(&handle) {
                    let call = wire::on_missing_csp(&handle, &r.extension_id);
                    self.out.push(Out::Rpc(call));
                }
            }
        }
        self.take()
    }

    fn resolve_custom_editor(&mut self, handle: &str) {
        let Some(r) = self.records.get(handle) else { return };
        let WebviewKind::CustomEditor { view_type, resource, .. } = &r.kind else { return };
        let call = wire::resolve_custom_editor(resource, handle, view_type, &r.title, &r.content, &r.panel, r.active, r.position);
        self.out.push(Out::RpcWith(call, Pending::Resolve { handle: handle.to_owned() }));
    }

    fn send_view_states(&mut self) {
        let states: Vec<(WebviewHandle, PanelViewState)> = self
            .order
            .iter()
            .filter_map(|h| self.records.get(h))
            .filter(|r| r.kind.is_editor_tab())
            .map(|r| (r.handle.clone(), PanelViewState { active: r.active, visible: r.visible, position: r.position }))
            .collect();
        if !states.is_empty() {
            self.out.push(Out::Rpc(wire::on_did_change_view_states(&states)));
        }
    }

    /// The reply to an [`Out::RpcWith`] call.
    pub fn on_reply(&mut self, pending: Pending, result: Result<Value, String>) -> Vec<Out> {
        match pending {
            Pending::CustomDocument { handle } => match result {
                Ok(_) => self.resolve_custom_editor(&handle),
                Err(_) => self.fail(&handle),
            },
            Pending::Resolve { handle } => {
                if result.is_err() {
                    self.fail(&handle);
                }
            }
        }
        self.take()
    }

    /// Code-OSS shows an error page in the webview when resolving it fails.
    fn fail(&mut self, handle: &str) {
        let Some(r) = self.records.get_mut(handle) else { return };
        let html = failed_content(r.kind.view_type());
        r.html = Some(html.clone());
        r.ready = false;
        let state = r.state.clone();
        self.ui(WebviewUpdate::Html { handle: handle.to_owned(), html, state });
    }

    /// The bytes of an [`Out::ReadFile`].
    pub fn on_resource_read(&mut self, handle: WebviewHandle, request: u64, uri: &UriComponents, result: Result<Vec<u8>, String>) -> Vec<Out> {
        let response = match result {
            Ok(bytes) => ResourceResponse::Ok { bytes, mime: resource::mime_type(&uri.path).to_owned() },
            Err(_) => ResourceResponse::Failed,
        };
        if self.records.contains_key(&handle) {
            self.ui(WebviewUpdate::Resource { handle, request, response });
        }
        self.take()
    }
}

/// The arguments of a `command:` link: the query is URI-encoded JSON, an array or one value.
pub fn command_arguments(query: &str) -> Vec<Value> {
    if query.is_empty() {
        return Vec::new();
    }
    let decoded = resource::percent_decode(query).unwrap_or_default();
    match serde_json::from_str::<Value>(&decoded) {
        Ok(Value::Array(a)) => a,
        Ok(v) => vec![v],
        Err(_) => Vec::new(),
    }
}
