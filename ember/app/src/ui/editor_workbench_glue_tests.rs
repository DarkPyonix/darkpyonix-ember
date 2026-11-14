//! The workbench page's glue (#618, follow-up of #616), rendered: the page runs in an `HtmlDom`
//! with a host that keeps the commands sent to it ([`EditorHost::fake`]), the test feeds it
//! session updates the way the real host queues them, and the page is driven through its own
//! handlers (a click on a tree row, the save chord on the page, leaving the new-name field).
//! No renderer is attached: nothing is laid out, and the code editor stays an empty widget box.

use std::cell::RefCell;

use dioxus_compose::html::prelude::dioxus_core::ScopeId;
use dioxus_compose::html::prelude::dioxus_elements::{Code, Key, Modifiers};
use dioxus_compose::html::prelude::*;
use dioxus_compose::html::{HtmlDom, NodeId};
use ember_editor::{SessionUpdate, WidgetEvent};
use ember_editor_conn::uri::UriComponents;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::editor_host::{EditorHost, HostCommand};
use crate::ui::editor::{file_failure_reason, EditorTarget};
use crate::ui::keymap::{Action, Chord, Os};

const ROOT: &str = "/w";
const FILE: &str = "/w/a.rs";
const NOT_FOUND: &str = "remote error: EntryNotFound (FileSystemError)";

thread_local! {
    /// The host the page under test talks to. `init` builds a real one in its hook; the test app
    /// puts this one in its place on every render.
    static FAKE: RefCell<Option<EditorHost>> = const { RefCell::new(None) };
    /// The folder the page under test is opened on ([`Page::open_at`]).
    static FOLDER: RefCell<String> = RefCell::new(ROOT.to_string());
}

fn target() -> EditorTarget {
    EditorTarget {
        project: String::new(),
        folder: FOLDER.with(|f| f.borrow().clone()),
        endpoint: String::new(),
        token: String::new(),
        server_url: String::new(),
        computer: String::new(),
    }
}

fn app() -> Element {
    super::init(target());
    let fake = FAKE.with(|f| f.borrow().clone()).expect("fake host installed");
    super::SHARED.with(|s| s.borrow_mut().as_mut().expect("workbench initialized").host = fake);
    super::page::workbench_page()
}

fn uri() -> UriComponents {
    UriComponents::remote("test", FILE)
}

/// A command as text, for assertions (`HostCommand` has no `Debug`).
fn describe(command: &HostCommand) -> String {
    match command {
        HostCommand::Open(path) => format!("open {path}"),
        HostCommand::Close(path) => format!("close {path}"),
        HostCommand::Focus(path) => format!("focus {path}"),
        HostCommand::List(path) => format!("list {path}"),
        HostCommand::Widget(uri, generation, WidgetEvent::CodeSaveRequested { version }) => {
            format!("save {} generation {generation} version {version}", uri.path)
        }
        HostCommand::Widget(uri, _, _) => format!("widget event {}", uri.path),
        HostCommand::CreateFile { parent, name } => format!("create file {parent} {name}"),
        HostCommand::Webview(event) => match event {
            ember_editor::webview::WebviewEvent::Ready { handle } => format!("webview ready {handle}"),
            ember_editor::webview::WebviewEvent::Message { handle, message } => format!("webview message {handle} {}", message.json),
            ember_editor::webview::WebviewEvent::State { handle, state } => format!("webview state {handle} {state}"),
            ember_editor::webview::WebviewEvent::ViewState { handle, visible, active, position } => {
                format!("webview view_state {handle} visible={visible} active={active} pos={position}")
            }
            ember_editor::webview::WebviewEvent::Closed { handle } => format!("webview closed {handle}"),
            ember_editor::webview::WebviewEvent::Focus { handle, focused } => format!("webview focus {handle} {focused}"),
            _ => "webview other".to_string(),
        },
        _ => "other".to_string(),
    }
}

struct Page {
    dom: HtmlDom,
    commands: UnboundedReceiver<HostCommand>,
}

impl Page {
    /// The page with one file, `a.rs`, listed under the project folder.
    fn open() -> Page {
        Page::open_at(ROOT)
    }

    /// The page opened on `folder`, with one file, `a.rs`, listed under it.
    fn open_at(folder: &str) -> Page {
        FOLDER.with(|f| *f.borrow_mut() = folder.to_string());
        let (host, commands) = EditorHost::fake();
        {
            let mut state = host.state.lock().unwrap();
            state.connected = true;
            state.listings.insert(
                folder.to_string(),
                vec![("a.rs".to_string(), ember_editor_conn::remote_fs::file_type::FILE)],
            );
        }
        FAKE.with(|f| *f.borrow_mut() = Some(host));
        let mut dom = HtmlDom::new(app);
        dom.render();
        Page { dom, commands }
    }

    fn host(&self) -> EditorHost {
        FAKE.with(|f| f.borrow().clone()).expect("fake host installed")
    }

    /// Renders again after the host's state changed, as a revision bump does in the window.
    fn redraw(&mut self) {
        self.dom.virtual_dom_mut().mark_dirty(ScopeId::APP);
        self.dom.render();
    }

    /// Queues `update` as the host does for a session update, and renders.
    fn deliver(&mut self, update: SessionUpdate) {
        self.host().state.lock().unwrap().updates.push(update);
        self.redraw();
    }

    /// The commands sent since the last call, as text.
    fn sent(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(command) = self.commands.try_recv() {
            out.push(describe(&command));
        }
        out
    }

    fn attr(&self, node: NodeId, name: &str) -> Option<String> {
        let node = self.dom.document().get_node(node)?;
        node.attrs()?.iter().find(|a| &*a.name.local == name).map(|a| a.value.clone())
    }

    /// The nodes in the page. The writer keeps every rsx template as a detached subtree that it
    /// clones for each use, with the template's static attributes (`"aria-label": "새 파일"`), so a
    /// node counts only when its parents lead to the document's root element.
    fn connected(&self) -> Vec<NodeId> {
        let doc = self.dom.document();
        let root = doc.root_element().id;
        let ids: Vec<NodeId> = doc.tree().iter().map(|(id, _)| id).collect();
        ids.into_iter()
            .filter(|&id| {
                let mut at = Some(id);
                while let Some(node) = at {
                    if node == root {
                        return true;
                    }
                    at = doc.get_node(node).and_then(|n| n.parent);
                }
                false
            })
            .collect()
    }

    /// The one element in the page that `matches`; panics when more than one does, so a test
    /// never acts on whichever came first.
    fn find_where(&self, what: &str, matches: impl Fn(NodeId) -> bool) -> Option<NodeId> {
        let found: Vec<NodeId> = self.connected().into_iter().filter(|&id| matches(id)).collect();
        assert!(found.len() <= 1, "{} elements match {what}: {found:?}", found.len());
        found.first().copied()
    }

    /// The one element whose attribute `name` is `value`.
    fn find(&self, name: &str, value: &str) -> Option<NodeId> {
        self.find_where(&format!("{name}={value:?}"), |id| self.attr(id, name).as_deref() == Some(value))
    }

    fn has_attr(&self, name: &str) -> bool {
        self.connected().into_iter().any(|id| self.attr(id, name).is_some())
    }

    fn text(&self) -> String {
        self.dom.document().root_element().text_content()
    }

    fn click(&mut self, name: &str, value: &str) {
        let node = self.find(name, value).unwrap_or_else(|| panic!("no element with {name}={value:?}"));
        self.dom.click_node(node);
        self.dom.render();
    }

    fn click_row(&mut self) {
        let row = self
            .find_where("the a.rs tree row", |id| {
                self.attr(id, "role").as_deref() == Some("treeitem") && self.attr(id, "aria-label").as_deref() == Some("a.rs")
            })
            .expect("the a.rs row in the project tree");
        self.dom.click_node(row);
        self.dom.render();
    }

    /// Presses the active keymap's save chord on the page itself (not in the code editor).
    fn press_save(&mut self) {
        let keymap = crate::services::services().ui.prefs.get_untracked().keymap;
        let Some(Chord::Keys { ctrl, shift, alt, meta, key }) = keymap.chord(Action::Save, Os::current()) else {
            panic!("{keymap:?} binds save to a key chord");
        };
        let mut modifiers = Modifiers::empty();
        if ctrl { modifiers |= Modifiers::CONTROL; }
        if shift { modifiers |= Modifiers::SHIFT; }
        if alt { modifiers |= Modifiers::ALT; }
        if meta { modifiers |= Modifiers::META; }
        let page = self.find("class", "ed").expect("the page's key listener (.ed)");
        self.dom.key_down(page, Key::Character(key.to_lowercase()), Code::KeyS, modifiers);
        self.dom.render();
    }
}

fn opened(generation: u64) -> SessionUpdate {
    SessionUpdate::Opened { uri: uri(), generation, text: "fn main() {}\n".into(), language_id: "rust".into(), tab_width: 4 }
}

// FR-L31 (#618, item 1): Cmd/Ctrl+S that reaches the page saves the document in the active tab, the
// same request the code editor sends for its own Cmd+S; with nothing open it says so and sends nothing.
#[test]
fn fr_l31_the_save_chord_on_the_page_asks_the_session_to_save_the_active_document() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open();

    page.press_save();
    assert!(page.sent().iter().all(|c| !c.starts_with("save")), "nothing to save yet");
    let notice = crate::ui::coexist::shared_ui().notice.get_untracked().map(|(_, text)| text);
    assert_eq!(notice.as_deref(), Some("저장할 파일이 열려 있지 않습니다"));

    page.click_row();
    page.deliver(opened(7));
    page.sent();
    page.press_save();
    let sent = page.sent();
    assert!(sent.contains(&format!("save {FILE} generation 7 version 0")), "{sent:?}");
}

// FR-L31 (#618, item 2): a file that could not be opened shows why in the editor area; a click on its
// row again clears the reason and asks again; Opened clears it and shows the editor.
#[test]
fn fr_l31_an_open_failure_shows_its_reason_a_new_click_retries_and_opened_clears_it() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open();
    let headline = "a.rs 파일을 열 수 없습니다";

    page.click_row();
    assert!(page.sent().contains(&format!("open {FILE}")));
    page.deliver(SessionUpdate::OpenFailed { uri: uri(), error: NOT_FOUND.into() });
    let text = page.text();
    assert!(text.contains(headline), "{text}");
    assert!(text.contains(file_failure_reason(NOT_FOUND)), "{text}");
    assert!(!text.contains(NOT_FOUND), "the raw error is not shown: {text}");

    page.click_row();
    assert!(page.sent().contains(&format!("open {FILE}")), "a new click asks again");
    assert!(!page.text().contains(headline), "the retry clears the old reason");

    page.deliver(SessionUpdate::OpenFailed { uri: uri(), error: NOT_FOUND.into() });
    assert!(page.text().contains(headline));
    assert!(!page.has_attr("data-compose-widget"));
    page.deliver(opened(3));
    assert!(!page.text().contains(headline), "Opened clears the reason");
    assert!(page.has_attr("data-compose-widget"), "the code editor takes the area");
}

// FR-L31 (#618, item 3): leaving the new-name field with nothing typed closes the prompt; a typed
// name stays, and nothing is created either way.
#[test]
fn fr_l31_leaving_an_empty_new_name_field_closes_it_and_a_typed_name_stays() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open();
    let field_label = "새 파일 이름";

    // 새 파일 is in the header "+" menu (owner placement rule 2026-11-13, rule 1).
    crate::ui::editor_tree_menu::close();
    page.click("aria-label", "추가");
    page.click("aria-label", "새 파일");
    let field = page.find("aria-label", field_label).expect("the new-name field");
    page.dom.blur(field);
    page.dom.render();
    assert!(page.find("aria-label", field_label).is_none(), "an empty field closes on blur");

    page.click("aria-label", "추가");
    page.click("aria-label", "새 파일");
    let field = page.find("aria-label", field_label).expect("the new-name field again");
    page.dom.input(field, "notes.md");
    page.dom.render();
    let field = page.find("aria-label", field_label).expect("the field after typing");
    page.dom.blur(field);
    page.dom.render();
    assert!(page.find("aria-label", field_label).is_some(), "a typed name stays after blur");
    assert!(page.sent().iter().all(|c| !c.starts_with("create")), "blur creates nothing");
}

// FR-L31 (#832, #827 rule 2): Shift+F10 or the context-menu key on a focused tree row opens the
// tree's context menu with the row's items, as in Code-OSS. This pins Ember's side of the key
// path; on the v0.0.6 pin the renderer sends only Enter (compose-rust#100), so a real run does not
// reach this handler yet.
#[test]
fn fr_l31_shift_f10_and_the_context_menu_key_on_a_tree_row_open_its_context_menu() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open();
    crate::ui::editor_tree_menu::close();
    page.click_row();
    let row_of = |page: &Page| {
        page.find_where("the a.rs tree row", |id| {
            page.attr(id, "role").as_deref() == Some("treeitem") && page.attr(id, "aria-label").as_deref() == Some("a.rs")
        })
        .expect("the a.rs row in the project tree")
    };

    for (key, code, modifiers) in [
        (Key::F10, Code::F10, Modifiers::SHIFT),
        (Key::ContextMenu, Code::ContextMenu, Modifiers::empty()),
    ] {
        crate::ui::editor_tree_menu::close();
        page.redraw();
        assert!(page.find("aria-label", "프로젝트 메뉴").is_none(), "no menu before {key:?}");
        let row = row_of(&page);
        page.dom.key_down(row, key.clone(), code, modifiers);
        page.dom.render();
        assert_eq!(
            crate::ui::editor_tree_menu::open_menu(),
            Some(crate::ui::editor_tree_menu::TreeMenuAt::Keyboard),
            "{key:?} opens the keyboard context menu"
        );
        assert!(page.find("aria-label", "프로젝트 메뉴").is_some(), "{key:?} shows the menu");
        let rename = page.find("aria-label", "이름 바꾸기").expect("the row's rename item");
        assert_eq!(page.attr(rename, "aria-disabled").as_deref(), Some("false"), "the row's items are enabled");
    }

    // F10 alone is not the menu key.
    crate::ui::editor_tree_menu::close();
    page.redraw();
    let row = row_of(&page);
    page.dom.key_down(row, Key::F10, Code::F10, Modifiers::empty());
    page.dom.render();
    assert_eq!(crate::ui::editor_tree_menu::open_menu(), None);
}

// FR-L31.9 (#781): 파일 열기 on a change row of the 커밋 window. The window's status and repository
// root are put in the source control state directly (the page has no project, so nothing is read from
// a server), and the window is shown with its stripe button.

fn change(path: &str) -> ember_client::api::GitFile {
    ember_client::api::GitFile { path: path.into(), old_path: None, index: '.', worktree: 'M' }
}

impl Page {
    /// Shows the 커밋 window with `files` as the work tree changes of the repository at `toplevel`,
    /// the folder being `prefix` below it.
    fn show_changes(&mut self, toplevel: &str, prefix: &str, files: Vec<ember_client::api::GitFile>) {
        {
            let s = super::shared();
            let mut st = s.scm.state.lock().unwrap();
            st.toplevel = Some(crate::ui::editor_scm_repo::Toplevel { toplevel: toplevel.into(), prefix: prefix.into() });
            st.load = crate::ui::editor_scm::ScmLoad::Ready(ember_client::api::GitStatus {
                branch: "main".into(),
                files,
                ..ember_client::api::GitStatus::default()
            });
        }
        let stripe = self
            .find_where("the 커밋 stripe button", |id| {
                self.attr(id, "aria-pressed").is_some() && self.attr(id, "title").is_some_and(|t| t.starts_with("커밋"))
            })
            .expect("the 커밋 stripe button");
        self.dom.click_node(stripe);
        self.dom.render();
        assert!(self.find("aria-label", "변경된 파일").is_some(), "the 커밋 window lists the changes");
    }

    /// The 파일 열기 icon of the change row of `name`.
    fn open_icon(&self, name: &str) -> NodeId {
        self.find("aria-label", &format!("파일 열기: {name}")).unwrap_or_else(|| panic!("the 파일 열기 icon of {name}"))
    }

    /// Whether the editor's tab strip has `path` as its current tab.
    fn current_tab_is(&self, path: &str) -> bool {
        self.find_where(&format!("the tab of {path}"), |id| {
            self.attr(id, "role").as_deref() == Some("tab") && self.attr(id, "title").as_deref() == Some(path)
        })
        .is_some_and(|tab| self.attr(tab, "aria-selected").as_deref() == Some("true"))
    }

    /// The elements around `node`, nearest first, up to the 변경된 파일 list (included).
    fn around_in_list(&self, node: NodeId) -> Vec<NodeId> {
        let doc = self.dom.document();
        let mut out = Vec::new();
        let mut at = doc.get_node(node).and_then(|n| n.parent);
        while let Some(id) = at {
            out.push(id);
            if self.attr(id, "aria-label").as_deref() == Some("변경된 파일") {
                break;
            }
            at = doc.get_node(id).and_then(|n| n.parent);
        }
        out
    }

    fn notice(&self) -> Option<String> {
        crate::ui::coexist::shared_ui().notice.get_untracked().map(|(_, text)| text)
    }

    fn diff_open(&self) -> bool {
        super::shared().scm.snapshot().diff.is_some()
    }
}

/// Clicks 파일 열기 on `name` and checks that `path` opens in a tab and nothing else happens.
fn assert_icon_opens(page: &mut Page, name: &str, path: &str) {
    page.sent();
    let icon = page.open_icon(name);
    page.dom.click_node(icon);
    page.dom.render();
    let sent = page.sent();
    assert!(sent.contains(&format!("open {path}")), "{name}: {sent:?}");
    assert!(sent.contains(&format!("focus {path}")), "{name}: {sent:?}");
    assert!(page.current_tab_is(path), "{name} is the current tab, {path}");
    assert!(!page.diff_open(), "{name}: the icon opens the file, not the diff");
}

// The icon is a click target of its own: no element around it, up to the list, takes the click, so
// the renderer has one box under the pointer and the row's diff click cannot take it instead or as
// well. The row's file icon and name still open the diff (a target beside the icon, not around it).
#[test]
fn fr_l31_9_the_open_icon_is_not_inside_the_rows_click_target() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open_at("/w/r/app");
    page.show_changes("/w/r", "app", vec![change("app/src/main.rs"), change("docs/GUIDE.md")]);

    for name in ["main.rs", "GUIDE.md"] {
        let icon = page.open_icon(name);
        assert!(page.dom.listeners(icon).contains(&"click"), "{name}: the icon listens for clicks");
        let around: Vec<NodeId> = page.around_in_list(icon).into_iter().filter(|&id| page.dom.listeners(id).contains(&"click")).collect();
        assert!(around.is_empty(), "{name}: an element around the 파일 열기 icon takes clicks: {around:?}");

        let label = page
            .find_where(&format!("the name {name}"), |id| {
                page.dom.document().get_node(id).is_some_and(|n| n.is_element() && n.text_content() == name)
                    && page.attr(id, "aria-label").is_none()
            })
            .unwrap_or_else(|| panic!("the name {name} in its row"));
        let (target, _) = page.dom.listener_target(label, "click").unwrap_or_else(|| panic!("{name}: a click on the name has a target"));
        assert!(!page.around_in_list(icon).contains(&target), "{name}: the name's diff target is beside the icon");
    }
}

// Inside the folder: the folder path, the same tab the explorer opens.
#[test]
fn fr_l31_9_the_open_icon_opens_a_change_inside_the_folder() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open_at("/w/r/app");
    page.show_changes("/w/r", "app", vec![change("app/src/main.rs"), change("docs/GUIDE.md")]);
    assert_icon_opens(&mut page, "main.rs", "/w/r/app/src/main.rs");
}

// Outside the folder: the absolute path below the repository root.
#[test]
fn fr_l31_9_the_open_icon_opens_a_change_outside_the_folder() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open_at("/w/r/app");
    page.show_changes("/w/r", "app", vec![change("app/src/main.rs"), change("docs/GUIDE.md")]);
    assert_icon_opens(&mut page, "GUIDE.md", "/w/r/docs/GUIDE.md");
}

// The folder is the repository root.
#[test]
fn fr_l31_9_the_open_icon_opens_a_change_when_the_folder_is_the_root() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open_at("/w/r");
    page.show_changes("/w/r", "", vec![change("src/main.rs"), change("README.md")]);
    assert_icon_opens(&mut page, "main.rs", "/w/r/src/main.rs");
    assert_icon_opens(&mut page, "README.md", "/w/r/README.md");
}

// A file that cannot be opened is named in a notice with the reason: the session's open failed, or
// the connection is lost (the host drops commands then, so nothing is asked).
#[test]
fn fr_l31_9_a_failed_open_from_the_icon_says_so_in_a_notice() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open_at("/w/r/app");
    page.show_changes("/w/r", "app", vec![change("app/src/main.rs"), change("docs/GUIDE.md")]);

    assert_icon_opens(&mut page, "GUIDE.md", "/w/r/docs/GUIDE.md");
    page.deliver(SessionUpdate::OpenFailed { uri: UriComponents::remote("test", "/w/r/docs/GUIDE.md"), error: NOT_FOUND.into() });
    let notice = page.notice().unwrap_or_default();
    assert_eq!(notice, crate::ui::editor_scm::open_failed_text("GUIDE.md", file_failure_reason(NOT_FOUND)));

    page.host().state.lock().unwrap().lost = Some("connection closed".into());
    page.sent();
    let icon = page.open_icon("main.rs");
    page.dom.click_node(icon);
    page.dom.render();
    assert!(page.sent().iter().all(|c| !c.starts_with("open")), "nothing is asked while the connection is lost");
    assert_eq!(page.notice().unwrap_or_default(), crate::ui::editor_scm::open_offline_text("main.rs"));
}

#[test]
fn fr_e8_extension_webview_panel_lifecycle_and_messaging() {
    use ember_editor::webview::{ContentOptions, PanelOptions, ShowOptions, WebviewKind, WebviewUpdate};
    use ember_editor_conn::webview::WebviewMessage;

    let mut page = Page::open();
    let _ = page.sent(); // Clear initial commands

    // 1. Deliver Created for a markdown preview webview panel
    page.deliver(SessionUpdate::Webview(WebviewUpdate::Created {
        handle: "wv-md".into(),
        kind: WebviewKind::Panel { view_type: "markdown.preview".into() },
        title: "Markdown Preview".into(),
        content: ContentOptions::default(),
        panel: PanelOptions::default(),
        state: None,
        reveal: Some(ShowOptions { preserve_focus: false, view_column: Some(1) }),
    }));

    // Check that ViewState was sent to host on reveal
    let sent = page.sent();
    assert!(sent.iter().any(|s| s == "webview view_state wv-md visible=true active=true pos=1"));

    // Check that tab was opened
    let tab = page.find_where("the webview tab", |id| {
        page.attr(id, "role").as_deref() == Some("tab") && page.attr(id, "title").as_deref() == Some("ember-webview:wv-md")
    }).expect("webview tab present in tab strip");
    assert_eq!(page.attr(tab, "aria-selected").as_deref(), Some("true"));

    // Check that webview pane placeholder renders with title and WAITING_TEXT
    let region = page.find_where("the webview region", |id| {
        page.attr(id, "role").as_deref() == Some("region") && page.attr(id, "aria-label").as_deref() == Some("Markdown Preview")
    }).expect("webview region rendered in editor pane");
    assert!(page.text().contains("Markdown Preview"));
    assert!(page.text().contains(ember_editor::webview::WAITING_TEXT));

    // 2. Deliver Ready: placeholder updates (WAITING_TEXT is cleared)
    page.deliver(SessionUpdate::Webview(WebviewUpdate::Ready { handle: "wv-md".into() }));
    assert!(page.find_where("the webview region after ready", |id| {
        page.attr(id, "role").as_deref() == Some("region") && page.attr(id, "aria-label").as_deref() == Some("Markdown Preview")
    }).is_some());
    assert!(!page.text().contains(ember_editor::webview::WAITING_TEXT));

    // 3. Post message from webview to session
    let s = super::shared();
    super::post_webview_message(&s, "wv-md", WebviewMessage {
        json: r#"{"command":"click","id":42}"#.into(),
        buffers: Vec::new(),
    });
    let sent = page.sent();
    assert!(sent.iter().any(|s| s == "webview message wv-md {\"command\":\"click\",\"id\":42}"));

    // 4. Set state from webview
    super::set_webview_state(&s, "wv-md", r#"{"scrollPos":350}"#);
    let sent = page.sent();
    assert!(sent.iter().any(|s| s == "webview state wv-md {\"scrollPos\":350}"));

    // 5. Close webview tab
    super::page::finish_close(&s, &["ember-webview:wv-md".to_string()], None);
    let sent = page.sent();
    assert!(sent.iter().any(|s| s == "webview closed wv-md"));
    page.redraw();
    assert!(page.find_where("the webview tab after close", |id| {
        page.attr(id, "title").as_deref() == Some("ember-webview:wv-md")
    }).is_none(), "webview tab removed after close");
}
