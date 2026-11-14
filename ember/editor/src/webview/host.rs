//! The window's side of a webview: the [`WebviewHost`] trait the toolkit node will implement, the
//! [`WebviewDriver`] that applies [`WebviewUpdate`]s to a host and turns its callbacks into
//! [`WebviewEvent`]s, the [`WebviewPanes`] placeholder model the window shows until the node exists,
//! and a [`FakeHost`] for tests.
//!
//! The node itself is compose-rust's (p1A, `docs/design/EDITOR-WEBVIEWS.md` "What Ember needs from the
//! host node"); this module fixes what Ember calls on it. Placement (rect, clip, z-order under menus
//! and sheets) belongs to the node's layout and is not part of the trait: the window puts the node
//! where the placeholder is now.

use std::collections::HashMap;
use std::sync::mpsc;

use ember_editor_conn::webview::{ContentOptions, PanelOptions, WebviewHandle, WebviewMessage};

use super::resource::{self, ResourceBase, ResourceResponse};
use super::shim;
use super::theme::{self, Fonts, ThemeKind};
use super::{WebviewEvent, WebviewKind, WebviewUpdate};

/// What the host needs to make one webview.
#[derive(Debug, Clone, PartialEq)]
pub struct HostWebview {
    pub handle: WebviewHandle,
    /// The page's own secure origin, as the host gave it ([`WebviewHost::origin`]).
    pub origin: String,
    /// The HTML with resource URLs and the CSP source already in the host's form.
    pub html: Option<String>,
    /// The name of the channel global the host injects ([`shim::IPC_GLOBAL`]).
    pub ipc_global: String,
    /// Runs before the page's scripts ([`shim::init_script`]).
    pub init_script: String,
    /// Also applied before the first paint, so the page never shows the wrong theme.
    pub css_vars: Vec<(String, String)>,
    pub enable_scripts: bool,
    pub enable_forms: bool,
    pub enable_find_widget: bool,
    pub retain_context_when_hidden: bool,
    pub visible: bool,
}

/// A callback of the host.
#[derive(Debug, Clone, PartialEq)]
pub enum HostEvent {
    /// The page loaded (after `create` or `set_html`).
    Ready { handle: WebviewHandle },
    /// `vscode.postMessage` in the page.
    Message { handle: WebviewHandle, message: WebviewMessage },
    /// `vscode.setState` in the page (JSON text).
    State { handle: WebviewHandle, state: String },
    Focus { handle: WebviewHandle, focused: bool },
    /// The scheme handler got a request; answer with [`WebviewHost::answer_resource`] (later is fine).
    Resource { handle: WebviewHandle, request: u64, url: String },
    /// The page tried to navigate or opened a link; the host never navigates by itself.
    Link { handle: WebviewHandle, uri: String },
    MissingCsp { handle: WebviewHandle },
    /// Find in page: `count` matches, `active` the 0-based current one (`None` without matches).
    Find { handle: WebviewHandle, count: u32, active: Option<u32> },
    /// A key chord the page did not handle (`defaultPrevented` false), for the app's shortcuts, in
    /// Ember's keymap notation (for example `"Cmd+P"`). Editing keys and text input never come here.
    Key { handle: WebviewHandle, chord: String },
}

/// Options of [`WebviewHost::find`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FindOptions {
    pub match_case: bool,
    pub whole_word: bool,
}

/// What the host reports that is for the window, not the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowEvent {
    /// The find widget's "N/M" counter.
    Find { handle: WebviewHandle, count: u32, active: Option<u32> },
    /// Run the app shortcut `chord` (the page left it alone).
    Key { handle: WebviewHandle, chord: String },
}

/// Where the host sends its callbacks; it may call it from any thread.
pub type HostSink = Box<dyn FnMut(HostEvent) + Send>;

/// A platform webview host: one page per handle, several at once.
pub trait WebviewHost {
    /// Install the callback sink (once, before any `create`).
    fn set_sink(&mut self, sink: HostSink);
    /// The secure origin the host gives the page of `handle`, asked once before `create`: a custom
    /// scheme (`vscode-webview://<uuid>`) on macOS and Linux, `https://<uuid>...` on Windows and Android.
    fn origin(&mut self, handle: &str) -> String;
    /// The URL form the host serves local resources in for `handle` (see [`resource`]).
    fn resource_base(&self, handle: &str) -> ResourceBase;
    fn create(&mut self, webview: HostWebview);
    /// Replace the page's HTML: the page reloads, the webview and its channel stay.
    fn set_html(&mut self, handle: &str, html: &str, init_script: &str);
    /// Deliver a message to the page, in call order (the session only posts to a ready page).
    fn post(&mut self, handle: &str, message: &WebviewMessage);
    /// Hide without destroying, or show again.
    fn set_visible(&mut self, handle: &str, visible: bool);
    /// Replace the theme on the live page (`update_script` is [`shim::theme_update_script`]).
    fn set_css_vars(&mut self, handle: &str, vars: &[(String, String)], update_script: &str);
    /// Give the page keyboard focus.
    fn focus(&mut self, handle: &str);
    fn dispose(&mut self, handle: &str);
    fn answer_resource(&mut self, handle: &str, request: u64, response: &ResourceResponse);
    /// Find in page; results come back as [`HostEvent::Find`].
    fn find(&mut self, handle: &str, query: &str, options: FindOptions);
    fn find_next(&mut self, handle: &str);
    fn find_prev(&mut self, handle: &str);
    fn find_stop(&mut self, handle: &str);
    /// "Developer: Open Webview Developer Tools" (debug builds at least).
    fn open_devtools(&mut self, handle: &str);
}

#[derive(Debug, Clone)]
struct Page {
    kind: WebviewKind,
    origin: String,
    base: ResourceBase,
    html: Option<String>,
    state: Option<String>,
    content: ContentOptions,
    panel: PanelOptions,
    visible: bool,
    /// Whether the host has the page now (a hidden page without `retainContextWhenHidden` is dropped).
    alive: bool,
}

/// Applies the session's updates to a host and collects the host's callbacks.
pub struct WebviewDriver<H: WebviewHost> {
    host: H,
    events: mpsc::Receiver<HostEvent>,
    kind: ThemeKind,
    fonts: Fonts,
    vars: Vec<(String, String)>,
    pages: HashMap<WebviewHandle, Page>,
    window_events: Vec<WindowEvent>,
}

impl<H: WebviewHost> WebviewDriver<H> {
    pub fn new(mut host: H, kind: ThemeKind, fonts: Fonts) -> Self {
        let (tx, events) = mpsc::channel();
        host.set_sink(Box::new(move |e| {
            let _ = tx.send(e);
        }));
        let vars = theme::css_vars(kind, &fonts);
        Self { host, events, kind, fonts, vars, pages: HashMap::new(), window_events: Vec::new() }
    }

    pub fn host(&self) -> &H {
        &self.host
    }

    pub fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }

    pub fn css_vars(&self) -> &[(String, String)] {
        &self.vars
    }

    fn create_page(&mut self, handle: &str) {
        let Some(p) = self.pages.get_mut(handle) else { return };
        p.alive = true;
        let webview = HostWebview {
            handle: handle.to_owned(),
            origin: p.origin.clone(),
            html: p.html.as_deref().map(|h| resource::rewrite_for_host(h, &p.base)),
            ipc_global: shim::IPC_GLOBAL.to_owned(),
            init_script: shim::init_script(p.state.as_deref(), &self.vars, self.kind, shim::IPC_GLOBAL),
            css_vars: self.vars.clone(),
            enable_scripts: p.content.enable_scripts,
            enable_forms: p.content.enable_forms,
            enable_find_widget: p.panel.enable_find_widget,
            retain_context_when_hidden: p.panel.retain_context_when_hidden,
            visible: p.visible,
        };
        self.host.create(webview);
    }

    /// Apply one session update; updates for the window only (titles, links, providers) are ignored here.
    pub fn apply(&mut self, update: &WebviewUpdate) {
        match update {
            WebviewUpdate::Created { handle, kind, content, panel, state, .. } => {
                let origin = self.host.origin(handle);
                let base = self.host.resource_base(handle);
                self.pages.insert(
                    handle.clone(),
                    Page {
                        kind: kind.clone(),
                        origin,
                        base,
                        html: None,
                        state: state.clone(),
                        content: content.clone(),
                        panel: *panel,
                        visible: true,
                        alive: false,
                    },
                );
                self.create_page(handle);
            }
            WebviewUpdate::Html { handle, html, state } => {
                let Some(p) = self.pages.get_mut(handle) else { return };
                p.html = Some(html.clone());
                if state.is_some() {
                    p.state = state.clone();
                }
                if p.alive {
                    let script = shim::init_script(p.state.as_deref(), &self.vars, self.kind, shim::IPC_GLOBAL);
                    let html = resource::rewrite_for_host(html, &p.base);
                    self.host.set_html(handle, &html, &script);
                }
            }
            WebviewUpdate::Options { handle, content } => {
                let Some(p) = self.pages.get_mut(handle) else { return };
                let changed = p.content != *content;
                p.content = content.clone();
                // Scripts and forms are fixed when a platform webview is made: make it again.
                if changed && p.alive && p.html.is_some() {
                    self.host.dispose(handle);
                    self.create_page(handle);
                }
            }
            WebviewUpdate::Post { handle, message } => {
                let Some(p) = self.pages.get(handle).filter(|p| p.alive) else { return };
                // Extensions also send `asWebviewUri` results in messages.
                let message = WebviewMessage { json: resource::rewrite_for_host(&message.json, &p.base), buffers: message.buffers.clone() };
                self.host.post(handle, &message);
            }
            WebviewUpdate::Reveal { handle, preserve_focus, .. } => {
                if !*preserve_focus && self.pages.get(handle).is_some_and(|p| p.alive) {
                    self.host.focus(handle);
                }
            }
            WebviewUpdate::Resource { handle, request, response } => self.host.answer_resource(handle, *request, response),
            WebviewUpdate::Disposed { handle } => {
                if let Some(p) = self.pages.remove(handle) {
                    if p.alive {
                        self.host.dispose(handle);
                    }
                }
            }
            WebviewUpdate::Ready { .. }
            | WebviewUpdate::Title { .. }
            | WebviewUpdate::Description { .. }
            | WebviewUpdate::Badge { .. }
            | WebviewUpdate::Icon { .. }
            | WebviewUpdate::OpenLink { .. }
            | WebviewUpdate::OpenFailed { .. }
            | WebviewUpdate::Providers { .. } => {}
        }
    }

    /// The window showed or hid the webview's tab or view. Returns the event for the session.
    pub fn set_view_state(&mut self, handle: &str, visible: bool, active: bool, position: i64) -> Option<WebviewEvent> {
        let p = self.pages.get_mut(handle)?;
        let was = p.visible;
        p.visible = visible;
        let (retain, alive, kind) = (p.panel.retain_context_when_hidden, p.alive, p.kind.clone());
        if visible && !alive {
            self.create_page(handle);
        } else if visible != was || !visible {
            if !visible && !retain && alive {
                self.host.dispose(handle);
                if let Some(p) = self.pages.get_mut(handle) {
                    p.alive = false;
                }
            } else if alive {
                self.host.set_visible(handle, visible);
            }
        }
        Some(match kind {
            WebviewKind::View { .. } => WebviewEvent::ViewVisible { handle: handle.to_owned(), visible },
            _ => WebviewEvent::ViewState { handle: handle.to_owned(), visible, active, position },
        })
    }

    /// The window's theme changed: every live page gets the new variables.
    pub fn set_theme(&mut self, kind: ThemeKind) {
        self.kind = kind;
        self.vars = theme::css_vars(kind, &self.fonts);
        let script = shim::theme_update_script(&self.vars, kind);
        let alive: Vec<WebviewHandle> = self.pages.iter().filter(|(_, p)| p.alive).map(|(h, _)| h.clone()).collect();
        for h in alive {
            self.host.set_css_vars(&h, &self.vars, &script);
        }
    }

    /// Focus the page (the window clicked into its panel).
    pub fn focus(&mut self, handle: &str) {
        if self.pages.get(handle).is_some_and(|p| p.alive) {
            self.host.focus(handle);
        }
    }

    /// The page's origin, as the host gave it.
    pub fn origin(&self, handle: &str) -> Option<&str> {
        self.pages.get(handle).map(|p| p.origin.as_str())
    }

    /// `webview.asWebviewUri` and `webview.cspSource` as this host serves them.
    pub fn resource_base(&self, handle: &str) -> Option<&ResourceBase> {
        self.pages.get(handle).map(|p| &p.base)
    }

    fn alive(&self, handle: &str) -> bool {
        self.pages.get(handle).is_some_and(|p| p.alive)
    }

    /// Find in the page (the find widget over a webview with `enableFindWidget`).
    pub fn find(&mut self, handle: &str, query: &str, options: FindOptions) {
        if self.alive(handle) {
            self.host.find(handle, query, options);
        }
    }

    pub fn find_next(&mut self, handle: &str) {
        if self.alive(handle) {
            self.host.find_next(handle);
        }
    }

    pub fn find_prev(&mut self, handle: &str) {
        if self.alive(handle) {
            self.host.find_prev(handle);
        }
    }

    pub fn find_stop(&mut self, handle: &str) {
        if self.alive(handle) {
            self.host.find_stop(handle);
        }
    }

    pub fn open_devtools(&mut self, handle: &str) {
        if self.alive(handle) {
            self.host.open_devtools(handle);
        }
    }

    /// Find results and app chords the host reported, since the last call (filled by [`Self::poll`]).
    pub fn take_window_events(&mut self) -> Vec<WindowEvent> {
        std::mem::take(&mut self.window_events)
    }

    /// The host's callbacks since the last call, as session events. Find results and key chords go
    /// to [`Self::take_window_events`]; a resource URL is turned back into Code-OSS's form.
    pub fn poll(&mut self) -> Vec<WebviewEvent> {
        let mut out = Vec::new();
        while let Ok(e) = self.events.try_recv() {
            out.push(match e {
                HostEvent::Find { handle, count, active } => {
                    self.window_events.push(WindowEvent::Find { handle, count, active });
                    continue;
                }
                HostEvent::Key { handle, chord } => {
                    self.window_events.push(WindowEvent::Key { handle, chord });
                    continue;
                }
                HostEvent::Ready { handle } => WebviewEvent::Ready { handle },
                HostEvent::Message { handle, message } => WebviewEvent::Message { handle, message },
                HostEvent::State { handle, state } => {
                    if let Some(p) = self.pages.get_mut(&handle) {
                        p.state = Some(state.clone());
                    }
                    WebviewEvent::State { handle, state }
                }
                HostEvent::Focus { handle, focused } => WebviewEvent::Focus { handle, focused },
                HostEvent::Resource { handle, request, url } => {
                    // A URL not in the host's form stays as it is, and the session refuses it.
                    let url = self.pages.get(&handle).and_then(|p| p.base.to_canonical(&url)).unwrap_or(url);
                    WebviewEvent::ResourceRequested { handle, request, url }
                }
                HostEvent::Link { handle, uri } => WebviewEvent::LinkClicked { handle, uri },
                HostEvent::MissingCsp { handle } => WebviewEvent::MissingCsp { handle },
            });
        }
        out
    }
}

// ---- placeholders ------------------------------------------------------------------------------

/// What the placeholder says while there is no host node.
pub const WAITING_TEXT: &str = "웹뷰를 표시하는 중입니다";

/// The state a pane shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneStatus {
    /// Made, the page is not shown yet (always, until the host node exists).
    Waiting,
    /// The page reported ready.
    Shown,
}

impl PaneStatus {
    pub fn text(&self) -> &'static str {
        match self {
            Self::Waiting => WAITING_TEXT,
            Self::Shown => "",
        }
    }
}

/// One webview the window has a place for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebviewPane {
    pub handle: WebviewHandle,
    pub kind: WebviewKind,
    pub title: String,
    pub description: Option<String>,
    pub status: PaneStatus,
}

/// The window's list of webview places, from the session's updates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebviewPanes {
    panes: Vec<WebviewPane>,
    /// The last failure to open a custom editor or view (for a notice), Korean.
    pub last_failure: Option<String>,
}

impl WebviewPanes {
    pub fn panes(&self) -> &[WebviewPane] {
        &self.panes
    }

    pub fn get(&self, handle: &str) -> Option<&WebviewPane> {
        self.panes.iter().find(|p| p.handle == handle)
    }

    pub fn apply(&mut self, update: &WebviewUpdate) {
        match update {
            WebviewUpdate::Created { handle, kind, title, .. } => {
                self.panes.retain(|p| p.handle != *handle);
                self.panes.push(WebviewPane {
                    handle: handle.clone(),
                    kind: kind.clone(),
                    title: title.clone(),
                    description: None,
                    status: PaneStatus::Waiting,
                });
            }
            WebviewUpdate::Html { handle, .. } | WebviewUpdate::Options { handle, .. } => {
                if let Some(p) = self.panes.iter_mut().find(|p| p.handle == *handle) {
                    p.status = PaneStatus::Waiting;
                }
            }
            WebviewUpdate::Ready { handle } => {
                if let Some(p) = self.panes.iter_mut().find(|p| p.handle == *handle) {
                    p.status = PaneStatus::Shown;
                }
            }
            WebviewUpdate::Title { handle, title } => {
                if let Some(p) = self.panes.iter_mut().find(|p| p.handle == *handle) {
                    p.title = title.clone();
                }
            }
            WebviewUpdate::Description { handle, description } => {
                if let Some(p) = self.panes.iter_mut().find(|p| p.handle == *handle) {
                    p.description = description.clone();
                }
            }
            WebviewUpdate::Disposed { handle } => self.panes.retain(|p| p.handle != *handle),
            WebviewUpdate::OpenFailed { reason, .. } => self.last_failure = Some(reason.clone()),
            _ => {}
        }
    }
}

// ---- fake host ---------------------------------------------------------------------------------

/// One call the [`FakeHost`] received.
#[derive(Debug, Clone, PartialEq)]
pub enum HostCall {
    Create(HostWebview),
    SetHtml { handle: WebviewHandle, html: String, init_script: String },
    Post { handle: WebviewHandle, message: WebviewMessage },
    SetVisible { handle: WebviewHandle, visible: bool },
    SetCssVars { handle: WebviewHandle, vars: Vec<(String, String)> },
    Focus { handle: WebviewHandle },
    Dispose { handle: WebviewHandle },
    AnswerResource { handle: WebviewHandle, request: u64, response: ResourceResponse },
    Find { handle: WebviewHandle, query: String, options: FindOptions },
    FindNext { handle: WebviewHandle },
    FindPrev { handle: WebviewHandle },
    FindStop { handle: WebviewHandle },
    OpenDevtools { handle: WebviewHandle },
}

/// A page of the [`FakeHost`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FakePage {
    pub origin: String,
    pub html: Option<String>,
    pub init_script: String,
    pub visible: bool,
    pub css_vars: Vec<(String, String)>,
    /// Messages delivered to the page, in order.
    pub received: Vec<WebviewMessage>,
}

/// A host with no webview: it records calls and lets a test play the page.
pub struct FakeHost {
    pub calls: Vec<HostCall>,
    pub pages: HashMap<WebviewHandle, FakePage>,
    /// The resource form it reports (custom scheme like WKWebView, or https like WebView2).
    pub base: ResourceBase,
    sink: Option<HostSink>,
    next_origin: u64,
}

impl Default for FakeHost {
    fn default() -> Self {
        Self::custom_scheme()
    }
}

impl FakeHost {
    /// Like the macOS and Linux hosts: `vscode-webview://...` origins, `vscode-resource://` resources.
    pub fn custom_scheme() -> Self {
        Self { calls: Vec::new(), pages: HashMap::new(), base: ResourceBase::custom_scheme(), sink: None, next_origin: 0 }
    }

    /// Like the Windows and Android hosts: `https://` origins and resources.
    pub fn https() -> Self {
        Self { base: ResourceBase::code_oss(), ..Self::custom_scheme() }
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Play a page callback.
    pub fn emit(&mut self, event: HostEvent) {
        if let Some(sink) = self.sink.as_mut() {
            sink(event);
        }
    }

    pub fn page(&self, handle: &str) -> Option<&FakePage> {
        self.pages.get(handle)
    }
}

impl WebviewHost for FakeHost {
    fn set_sink(&mut self, sink: HostSink) {
        self.sink = Some(sink);
    }

    fn origin(&mut self, _handle: &str) -> String {
        self.next_origin += 1;
        match self.base {
            ResourceBase::Scheme { .. } => format!("vscode-webview://fake-{}", self.next_origin),
            ResourceBase::Https { .. } => format!("https://fake-{}.vscode-webview.net", self.next_origin),
        }
    }

    fn resource_base(&self, _handle: &str) -> ResourceBase {
        self.base.clone()
    }

    fn create(&mut self, webview: HostWebview) {
        self.pages.insert(
            webview.handle.clone(),
            FakePage {
                origin: webview.origin.clone(),
                html: webview.html.clone(),
                init_script: webview.init_script.clone(),
                visible: webview.visible,
                css_vars: webview.css_vars.clone(),
                received: Vec::new(),
            },
        );
        self.calls.push(HostCall::Create(webview));
    }

    fn set_html(&mut self, handle: &str, html: &str, init_script: &str) {
        if let Some(p) = self.pages.get_mut(handle) {
            p.html = Some(html.to_owned());
            p.init_script = init_script.to_owned();
        }
        self.calls.push(HostCall::SetHtml { handle: handle.to_owned(), html: html.to_owned(), init_script: init_script.to_owned() });
    }

    fn post(&mut self, handle: &str, message: &WebviewMessage) {
        if let Some(p) = self.pages.get_mut(handle) {
            p.received.push(message.clone());
        }
        self.calls.push(HostCall::Post { handle: handle.to_owned(), message: message.clone() });
    }

    fn set_visible(&mut self, handle: &str, visible: bool) {
        if let Some(p) = self.pages.get_mut(handle) {
            p.visible = visible;
        }
        self.calls.push(HostCall::SetVisible { handle: handle.to_owned(), visible });
    }

    fn set_css_vars(&mut self, handle: &str, vars: &[(String, String)], _update_script: &str) {
        if let Some(p) = self.pages.get_mut(handle) {
            p.css_vars = vars.to_vec();
        }
        self.calls.push(HostCall::SetCssVars { handle: handle.to_owned(), vars: vars.to_vec() });
    }

    fn focus(&mut self, handle: &str) {
        self.calls.push(HostCall::Focus { handle: handle.to_owned() });
    }

    fn dispose(&mut self, handle: &str) {
        self.pages.remove(handle);
        self.calls.push(HostCall::Dispose { handle: handle.to_owned() });
    }

    fn answer_resource(&mut self, handle: &str, request: u64, response: &ResourceResponse) {
        self.calls.push(HostCall::AnswerResource { handle: handle.to_owned(), request, response: response.clone() });
    }

    fn find(&mut self, handle: &str, query: &str, options: FindOptions) {
        self.calls.push(HostCall::Find { handle: handle.to_owned(), query: query.to_owned(), options });
    }

    fn find_next(&mut self, handle: &str) {
        self.calls.push(HostCall::FindNext { handle: handle.to_owned() });
    }

    fn find_prev(&mut self, handle: &str) {
        self.calls.push(HostCall::FindPrev { handle: handle.to_owned() });
    }

    fn find_stop(&mut self, handle: &str) {
        self.calls.push(HostCall::FindStop { handle: handle.to_owned() });
    }

    fn open_devtools(&mut self, handle: &str) {
        self.calls.push(HostCall::OpenDevtools { handle: handle.to_owned() });
    }
}
