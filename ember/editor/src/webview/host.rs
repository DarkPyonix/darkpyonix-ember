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

use super::resource::ResourceResponse;
use super::shim;
use super::theme::{self, Fonts, ThemeKind};
use super::{WebviewEvent, WebviewKind, WebviewUpdate};

/// What the host needs to make one webview.
#[derive(Debug, Clone, PartialEq)]
pub struct HostWebview {
    pub handle: WebviewHandle,
    /// The page's own secure origin (`vscode-webview://<uuid>`).
    pub origin: String,
    pub html: Option<String>,
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
}

/// Where the host sends its callbacks; it may call it from any thread.
pub type HostSink = Box<dyn FnMut(HostEvent) + Send>;

/// A platform webview host: one page per handle, several at once.
pub trait WebviewHost {
    /// Install the callback sink (once, before any `create`).
    fn set_sink(&mut self, sink: HostSink);
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
}

#[derive(Debug, Clone)]
struct Page {
    kind: WebviewKind,
    origin: String,
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
}

impl<H: WebviewHost> WebviewDriver<H> {
    pub fn new(mut host: H, kind: ThemeKind, fonts: Fonts) -> Self {
        let (tx, events) = mpsc::channel();
        host.set_sink(Box::new(move |e| {
            let _ = tx.send(e);
        }));
        let vars = theme::css_vars(kind, &fonts);
        Self { host, events, kind, fonts, vars, pages: HashMap::new() }
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
            html: p.html.clone(),
            init_script: shim::init_script(p.state.as_deref(), &self.vars, self.kind),
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
            WebviewUpdate::Created { handle, kind, origin, content, panel, state, .. } => {
                self.pages.insert(
                    handle.clone(),
                    Page {
                        kind: kind.clone(),
                        origin: origin.clone(),
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
                    let script = shim::init_script(p.state.as_deref(), &self.vars, self.kind);
                    self.host.set_html(handle, html, &script);
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
                if self.pages.get(handle).is_some_and(|p| p.alive) {
                    self.host.post(handle, message);
                }
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

    /// The host's callbacks since the last call, as session events.
    pub fn poll(&mut self) -> Vec<WebviewEvent> {
        let mut out = Vec::new();
        while let Ok(e) = self.events.try_recv() {
            out.push(match e {
                HostEvent::Ready { handle } => WebviewEvent::Ready { handle },
                HostEvent::Message { handle, message } => WebviewEvent::Message { handle, message },
                HostEvent::State { handle, state } => {
                    if let Some(p) = self.pages.get_mut(&handle) {
                        p.state = Some(state.clone());
                    }
                    WebviewEvent::State { handle, state }
                }
                HostEvent::Focus { handle, focused } => WebviewEvent::Focus { handle, focused },
                HostEvent::Resource { handle, request, url } => WebviewEvent::ResourceRequested { handle, request, url },
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
#[derive(Default)]
pub struct FakeHost {
    pub calls: Vec<HostCall>,
    pub pages: HashMap<WebviewHandle, FakePage>,
    sink: Option<HostSink>,
}

impl FakeHost {
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
}
