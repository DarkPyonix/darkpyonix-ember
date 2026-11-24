//! The workbench page's glue (#618, follow-up of #616), rendered: the page runs in an `HtmlDom`
//! with a host that keeps the commands sent to it ([`EditorHost::fake`]), the test feeds it
//! session updates the way the real host queues them, and the page is driven through its own
//! handlers (a click on a tree row, the save chord on the page, leaving the new-name field).
//! No renderer is attached: nothing is laid out, and the code editor stays an empty widget box.

use std::cell::RefCell;

use dioxus_compose::html::prelude::dioxus_core::ScopeId;
use dioxus_compose::html::prelude::dioxus_elements::{Code, Key, Modifiers};
use dioxus_compose::html::prelude::*;
use dioxus_compose::html::{HtmlDom, NodeId, PointerInput};
use dioxus_compose::prelude::ReadableExt;
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
        relay: String::new(),
        vscode_url: String::new(),
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
        HostCommand::Restore { path, text } => format!("restore {path} text {text:?}"),
        HostCommand::RestoreWithBase { path, text, base } => format!("restore {path} text {text:?} base {base:?}"),
        HostCommand::SetIndentation { path, action } => format!("set indentation {path} {action:?}"),
        HostCommand::CreateFile { parent, name } => format!("create file {parent} {name}"),
        HostCommand::Rename { path, new_name } => format!("rename {path} {new_name}"),
        HostCommand::MovePaths { sources, dest_dir } => format!("move {} -> {dest_dir}", sources.join(",")),
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
        Page::open_with(folder, None)
    }

    /// The page with the editor's own stylesheets (`page::config`), so it can be laid out.
    fn open_styled() -> Page {
        Page::open_with(ROOT, Some(super::page::config()))
    }

    fn open_with(folder: &str, config: Option<dioxus_compose::html::HtmlConfig>) -> Page {
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
        let mut dom = match config {
            Some(config) => HtmlDom::with_config(app, config),
            None => HtmlDom::new(app),
        };
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
// tree's context menu with the row's items, as in Code-OSS, beside right-click (compose-rust#417).
// This pins Ember's side of the key path; compose-rust#100 and #717 are in the v0.0.11 pin, and a
// real run reaching this handler is not yet checked on a display.
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

// FR-L31.5 (compose-rust#417, #673): the tree row and the tab keep a preview on the real
// `ondoubleclick` and open their menus on `oncontextmenu`; two `onclick`s within 500 ms no longer
// stand in for a double-click.
#[test]
fn fr_l31_5_double_click_and_right_click_are_the_real_events() {
    let page = include_str!("editor_workbench.rs");
    let production = &page[..page.find("#[cfg(test)]").unwrap_or(page.len())];
    assert!(!production.contains(concat!("DOUBLE", "_CLICK")), "the 500 ms stand-in is gone");
    let editor = include_str!("editor.rs");
    assert!(!editor.contains(concat!("pub struct Double", "Click")) && !editor.contains(concat!("DOUBLE_CLICK", "_MS")));
    let tree = &production[production.find(r#"role: "treeitem", "aria-label": "{row_label}""#).expect("tree row")..];
    let tree = &tree[..tree.find("chev-sp").expect("row content")];
    assert!(tree.contains("ondoubleclick:") && tree.contains("open_tab_file(&keep_s, &keep_path)"), "a double-click keeps the preview");
    assert!(tree.contains("oncontextmenu:") && tree.contains("TreeMenuAt::Pointer"), "a right-click opens the context menu");
    let tab = &production[production.find(r#"role: "tab", "aria-selected": "{selected}""#).expect("editor tab")..];
    let tab = &tab[..tab.find("onkeydown:").expect("tab keys")];
    assert!(tab.contains("ondoubleclick:") && tab.contains("oncontextmenu:") && tab.contains("tab_menu.set(Some("));
    assert!(!tab[..tab.find("ondoubleclick:").unwrap()].contains("keep_open"), "a single click does not keep the tab");
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

// FR-L31.26 (#874): the status bar is --sbh (32 px) high and its bottom edge is the page's bottom
// edge, as in the mockup (`editor.html`: the bar's top line at y 868 of a 900 px window). On
// v0.0.10 the window showed 16 px of page under the bar. This lays the page out with its own
// stylesheets and no renderer, so it tells the two possible causes apart: when it fails, the
// grid itself leaves room under the bar (the message lists the page's grid items and their
// boxes); when it passes and the window still shows the band, the page is laid out in a viewport
// 16 px shorter than the window, which is the renderer's or dioxus-compose's island size.
#[test]
fn fr_l31_26_the_status_bar_is_32_high_and_flush_with_the_page_bottom() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open_styled();
    page.click_row();
    page.deliver(opened(3));
    let ed = page.find("class", "ed").expect("the page grid (.ed) with the tree and the bottom window open");
    let bar = page.find("aria-label", "상태 표시줄").expect("the status bar");
    let _ = page.dom.layout(1440.0, 900.0, 1.0);
    let list = page.dom.display_list().expect("laid out");
    let ed_rect = list.get(ed).expect("the page draws its background").rect;
    let bar_rect = list.get(bar).expect("the status bar draws its line").rect;
    let doc = page.dom.document();
    let items: Vec<String> = doc
        .get_node(ed)
        .map(|n| n.children.clone())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|id| {
            let node = doc.get_node(id)?;
            let rect = list.get(id).map(|e| format!("{:?}", e.rect)).unwrap_or_else(|| "no box".into());
            // A text or comment child is a grid item too (an anonymous one), so it is listed.
            let tag = match node.element_data() {
                Some(e) if node.is_anonymous() => format!("anonymous {}", e.name.local),
                Some(e) => format!("{}.{}", e.name.local, page.attr(id, "class").unwrap_or_default()),
                None if node.is_text_node() => format!("text {:?}", node.text_data().map(|t| t.content.clone()).unwrap_or_default()),
                None => "comment or other".to_string(),
            };
            Some(format!("{tag} {rect}"))
        })
        .collect();
    // The boxes the grid places (comments are not among them; anonymous blocks are).
    let laid: Vec<String> = doc
        .get_node(ed)
        .and_then(|n| n.layout_children.borrow().clone())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|id| {
            let node = doc.get_node(id)?;
            let name = node.element_data().map(|e| format!("{}{}.{}", if node.is_anonymous() { "anonymous " } else { "" }, e.name.local, page.attr(id, "class").unwrap_or_default()));
            let l = node.final_layout;
            Some(format!("{} at {},{} {}x{}", name.unwrap_or_else(|| "text".into()), l.location.x, l.location.y, l.size.width, l.size.height))
        })
        .collect();
    let items = format!("{items:#?}; grid items {laid:#?}");
    let close = |a: f32, b: f32| (a - b).abs() < 0.5;
    assert!(close(bar_rect.height, 32.0), "bar {bar_rect:?}; items {items}");
    assert!(
        close(bar_rect.y + bar_rect.height, ed_rect.y + ed_rect.height),
        "the bar ends at {} but the page at {}; page {ed_rect:?}; items {items}",
        bar_rect.y + bar_rect.height,
        ed_rect.y + ed_rect.height,
    );
    assert!(close(ed_rect.y + ed_rect.height, 900.0), "the page fills the viewport: {ed_rect:?}");
    let page_css = include_str!("../../assets/editor/page.css");
    let wb = include_str!("editor_workbench.rs");
    assert!(!page_css.contains(&[".ed", "-none"].concat()), "dioxus-compose#266: .ed-none stand-in is gone");
    assert!(!page_css.contains(concat!("TODO(dioxus-compose#", "266")));
    assert!(!wb.contains(&["\"ed", "-none\""].concat()), "dioxus-compose#266: no ed-none div in the workbench");
}

/// A node's laid-out box in the page, as `(x, y, width, height)`: its layout's location added up
/// through its parents, so a node that draws nothing (a pane, a widget's placeholder) has one too.
fn page_box(page: &Page, node: NodeId) -> (f32, f32, f32, f32) {
    let doc = page.dom.document();
    let n = doc.get_node(node).expect("the node");
    let size = n.final_layout.size;
    let (mut x, mut y, mut at) = (0.0, 0.0, Some(node));
    while let Some(id) = at {
        let Some(node) = doc.get_node(id) else { break };
        x += node.final_layout.location.x;
        y += node.final_layout.location.y;
        at = node.parent;
    }
    (x, y, size.width, size.height)
}

// FR-L31.40 (#301): the code editor's box is the editor pane under the breadcrumb row less the
// 8 px above the first line (FR-L31.27), and ends where the pane does, above the bottom panel: the
// box `use_widget_box` hands the editor is the area the reader sees, so a line the editor counts
// as on screen is drawn there. The window measured the same (pane 1054x456 at 337,135 and box
// 1054x448 at 1440x900); this keeps the stylesheet and the pane's inline sizes from drifting.
#[test]
fn fr_l31_40_the_code_editor_box_is_the_pane_under_the_breadcrumb_less_the_top_room() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open_styled();
    page.click_row();
    page.deliver(opened(3));
    let widget = page.find_where("the code editor", |id| page.attr(id, "data-compose-widget").is_some()).expect("the code editor's box");
    let pane = page.find("data-ed", "code-pane").expect("the editor pane");
    let crumbs = page.find("class", "ed-crumbs").expect("the breadcrumb row");
    let bottom = page.find("aria-label", "아래 도구 창").expect("the bottom tool window");
    for (width, height) in [(1440.0, 900.0), (1280.0, 820.0), (1000.0, 620.0)] {
        let _ = page.dom.layout(width, height, 1.0);
        let (w, p, c, b) = (page_box(&page, widget), page_box(&page, pane), page_box(&page, crumbs), page_box(&page, bottom));
        let close = |a: f32, b: f32| (a - b).abs() < 0.5;
        let at = format!("at {width}x{height}: box {w:?}, pane {p:?}, crumbs {c:?}, bottom panel {b:?}");
        assert!(p.3 > 0.0 && w.3 > 0.0, "laid out {at}");
        assert!(close(p.1, c.1 + c.3), "the pane starts under the breadcrumb row {at}");
        assert!(close(w.0, p.0) && close(w.2, p.2), "the box is the pane's width {at}");
        assert!(close(w.1, p.1 + 8.0), "8 px above the box {at}");
        assert!(close(w.1 + w.3, p.1 + p.3), "the box ends where the pane does {at}");
        assert!(w.1 + w.3 <= b.1 + 0.5, "the box ends above the bottom panel {at}");
    }
}

/// The page grid's and the status bar's boxes, as `(x, y, width, height)`, with the page laid out
/// in a `width` by `height` viewport.
fn page_and_bar_boxes(page: &mut Page, ed: NodeId, bar: NodeId, width: f32, height: f32) -> ((f32, f32, f32, f32), (f32, f32, f32, f32)) {
    let _ = page.dom.layout(width, height, 1.0);
    let list = page.dom.display_list().expect("laid out");
    let e = list.get(ed).expect("the page draws its background").rect;
    let b = list.get(bar).expect("the status bar draws its line").rect;
    ((e.x, e.y, e.width, e.height), (b.x, b.y, b.width, b.height))
}

// #301, #320: the page is the viewport, so it follows the window when it grows and when it
// shrinks. The window's sizes are those of #320 (1280x820 opened, 1500x900 grown, 1000x620
// shrunk); at each one the page grid is exactly the viewport and the status bar ends at its
// bottom edge and spans its width. No renderer is attached: this is the page's own layout, given
// the viewport the HTML runtime takes from the window's content box.
#[test]
fn fr_l31_37_the_page_follows_the_viewport_when_it_grows_and_when_it_shrinks() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open_styled();
    page.click_row();
    page.deliver(opened(3));
    let ed = page.find("class", "ed").expect("the page grid (.ed) with the tree and the bottom window open");
    let bar = page.find("aria-label", "상태 표시줄").expect("the status bar");
    let close = |a: f32, b: f32| (a - b).abs() < 0.5;
    for (width, height) in [(1280.0_f32, 820.0_f32), (1500.0, 900.0), (1000.0, 620.0), (1280.0, 820.0)] {
        let (e, b) = page_and_bar_boxes(&mut page, ed, bar, width, height);
        assert!(close(e.0, 0.0) && close(e.1, 0.0), "at {width}x{height} the page starts at {},{}", e.0, e.1);
        assert!(close(e.2, width), "at {width}x{height} the page is {} wide", e.2);
        assert!(close(e.3, height), "at {width}x{height} the page is {} high", e.3);
        assert!(close(b.1 + b.3, height), "at {width}x{height} the status bar ends at {}", b.1 + b.3);
        assert!(close(b.0, 0.0) && close(b.2, width), "at {width}x{height} the status bar spans {} from {}", b.2, b.0);
    }
}

impl Page {
    /// The border boxes (x, y, width, height) of `nodes` with the page laid out at 1440x900.
    fn boxes(&mut self, nodes: &[(&str, NodeId)]) -> Vec<(f32, f32, f32, f32)> {
        let _ = self.dom.layout(1440.0, 900.0, 1.0);
        let list = self.dom.display_list().expect("laid out");
        nodes
            .iter()
            .map(|(what, id)| {
                let r = list.get(*id).unwrap_or_else(|| panic!("{what} has no box")).rect;
                (r.x, r.y, r.width, r.height)
            })
            .collect()
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.dom.document().get_node(node).and_then(|n| n.parent)
    }

    fn has_class(&self, node: NodeId, class: &str) -> bool {
        self.attr(node, "class").is_some_and(|c| c.split_whitespace().any(|c| c == class))
    }
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.75
}

// FR-L31.29 (#895, design review E4): quick open hangs under the find pill, ending at the pill's
// right edge, and its query row is a row of the popover, not a second framed field.
#[test]
fn fr_l31_29_quick_open_hangs_under_the_find_pill_in_one_frame() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open_styled();
    let pill = page.find("class", "ed-find").expect("the find pill");
    page.dom.click_node(pill);
    page.dom.render();
    let list = page
        .find_where("the quick open list", |id| page.attr(id, "role").as_deref() == Some("dialog"))
        .expect("the find pill opens quick open");
    // `find` panics on two matches, so this also says the popover holds no framed `.ed-find` field.
    let pill = page.find("class", "ed-find").expect("the find pill, the only framed field");
    let query = page.find("class", "ed-palq").expect("the query row");
    assert_eq!(page.parent(query), Some(list), "the query row is a row of the popover");
    assert_eq!(page.parent(list), page.parent(pill), "the list is placed against the pill's box");
    let b = page.boxes(&[("the find pill", pill), ("the quick open list", list), ("the query row", query)]);
    let (pill, list, query) = (b[0], b[1], b[2]);
    assert!(near(pill.2, 300.0), "the pill keeps its 300px: {pill:?}");
    assert!(near(list.0 + list.2, pill.0 + pill.2), "the list ends at the pill's right edge: list {list:?}, pill {pill:?}");
    let gap = list.1 - (pill.1 + pill.3);
    assert!((0.0..=8.0).contains(&gap), "the list starts right under the pill: list {list:?}, pill {pill:?}");
    assert!(near(list.2, 560.0), "the list is 560px wide at 1440: {list:?}");
    assert!(near(query.2, list.2 - 2.0), "the query row spans the popover inside its border: query {query:?}, list {list:?}");
}

impl Page {
    /// Every element in the page that `matches`, in document order.
    fn all_where(&self, matches: impl Fn(NodeId) -> bool) -> Vec<NodeId> {
        let mut found: Vec<NodeId> = self.connected().into_iter().filter(|&id| matches(id)).collect();
        found.sort();
        found
    }
}

fn file_change(path: &str, index: char, worktree: char) -> ember_client::api::GitFile {
    ember_client::api::GitFile { path: path.into(), old_path: None, index, worktree }
}

// FR-L31.32 (#895, design review E7): a row of the 커밋 window is as wide as the window, the status
// letter is a 20px badge at the row's right end, and the action icons keep their columns whether a
// row has one, two or three of them. The group's action sits over the rows' stage column.
#[test]
fn fr_l31_32_commit_rows_fill_the_window_and_their_columns_line_up() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open_styled();
    page.show_changes(
        ROOT,
        "",
        vec![
            // Modified: open, discard, stage. Deleted: no open. Untracked: no discard.
            file_change("ember/server/src/api/vcs.rs", '.', 'M'),
            file_change("ember/server/src/legacy.rs", '.', 'D'),
            file_change("docs/GIT.md", '?', '?'),
        ],
    );
    let list = page.find("aria-label", "변경된 파일").expect("the list");
    let rows = page.all_where(|id| page.has_class(id, "ed-scm-row"));
    let badges = page.all_where(|id| page.has_class(id, "ed-scm-st"));
    let stages = page.all_where(|id| page.attr(id, "aria-label").is_some_and(|l| l.starts_with("스테이지: ")));
    let opens = page.all_where(|id| page.attr(id, "aria-label").is_some_and(|l| l.starts_with("파일 열기: ")));
    let discards = page.all_where(|id| page.attr(id, "aria-label").is_some_and(|l| l.starts_with("변경 사항 취소: ")));
    let all = page.find("aria-label", "모두 스테이지").expect("the group's action");
    assert_eq!((rows.len(), badges.len(), stages.len(), opens.len(), discards.len()), (3, 3, 3, 2, 2));

    let named = |what: &'static str, ids: &[NodeId]| -> Vec<(&'static str, NodeId)> { ids.iter().map(|&id| (what, id)).collect() };
    let list_box = page.boxes(&[("the list", list)])[0];
    let row_boxes = page.boxes(&named("a row", &rows));
    let badge_boxes = page.boxes(&named("a badge", &badges));
    let stage_boxes = page.boxes(&named("a stage button", &stages));
    let open_boxes = page.boxes(&named("an open button", &opens));
    let discard_boxes = page.boxes(&named("a discard button", &discards));
    let all_box = page.boxes(&[("the group's action", all)])[0];

    // `.ed-tree` pads its rows by `--sp2` (8px) on each side.
    for row in &row_boxes {
        assert!(near(row.0, list_box.0 + 8.0) && near(row.2, list_box.2 - 16.0), "a row fills the list: row {row:?}, list {list_box:?}");
    }
    let right = row_boxes[0].0 + row_boxes[0].2;
    for badge in &badge_boxes {
        assert!(near(badge.2, 20.0) && near(badge.3, 20.0), "a 20px badge: {badge:?}");
        assert!(near(badge.0 + badge.2, right - 8.0), "the badge ends 8px inside the row: badge {badge:?}, row right {right}");
    }
    for (what, column) in [("stage", &stage_boxes), ("open", &open_boxes), ("discard", &discard_boxes)] {
        assert!(column.iter().all(|b| near(b.0, column[0].0) && near(b.2, 20.0)), "the {what} icons share one column: {column:?}");
    }
    // From the right: badge, stage, discard, open.
    assert!(stage_boxes[0].0 < badge_boxes[0].0 && discard_boxes[0].0 < stage_boxes[0].0 && open_boxes[0].0 < discard_boxes[0].0);
    assert!(near(all_box.0, stage_boxes[0].0), "the group's action is over the stage column: {all_box:?}, {stage_boxes:?}");
}

// FR-L31.32: a badge's tone is the soft pair of the letter's colour, and each tone has its rule.
#[test]
fn fr_l31_32_status_badges_take_the_soft_pair_of_the_letters_colour() {
    use crate::ui::editor_scm::{letter_color, letter_tone};
    let css = include_str!("../../assets/editor/page.css");
    for letter in ['M', 'A', 'D', 'R', 'C', 'T', 'U', '!'] {
        let tone = letter_tone(letter);
        if tone == "grey" {
            assert_eq!(letter_color(letter), "var(--muted)");
            assert!(css.contains(".ed-scm-st.grey{background:var(--c-grey-soft);color:var(--text-2)}"));
        } else {
            assert_eq!(letter_color(letter), format!("var(--c-{tone})"), "{letter}");
            assert!(css.contains(&format!(".ed-scm-st.{tone}{{background:var(--c-{tone}-soft);color:var(--c-{tone})}}")), "{letter}");
        }
    }
    // The amend check is the mockup's square (`vcs.css:68-69`), not a native control.
    assert!(css.contains(".ed-scm-cb{width:18px;height:18px;flex:none;border:1.5px solid var(--c-grey);border-radius:5px;"));
    assert!(css.contains(".ed-scm-cb.on{background:var(--ink);border-color:var(--ink)}"));
    let scm = include_str!("editor_scm.rs");
    assert!(scm.contains(r#"role: "checkbox", "aria-checked": "{st.amend}", "aria-label": "이전 커밋 수정","#));
    assert!(!scm.contains(r#"r#type: "checkbox""#));
}

// FR-L31.39: every letter takes the mockup's pair (`vcs.js:19` `VCS_ST`): M orange, A green,
// D red, R blue; untracked (U, git's `?`) joins A, C joins R, a conflict joins M.
#[test]
fn fr_l31_39_status_badges_follow_the_mockups_map() {
    use crate::ui::editor_scm::{letter_color, letter_tone};
    for (letter, tone) in [('M', "orange"), ('A', "green"), ('D', "red"), ('R', "blue"), ('C', "blue"), ('U', "green"), ('!', "orange")] {
        assert_eq!(letter_tone(letter), tone, "{letter}");
        assert_eq!(letter_color(letter), format!("var(--c-{tone})"), "{letter}");
    }
}

// FR-L31.33 (#895, design review E8): the commit button is the mockup's: at least 40px high across
// the window, 14/500 with the branch in mono, and its icon in the fill's text colour.
#[test]
fn fr_l31_33_the_commit_button_is_40_high_and_names_the_branch() {
    use crate::ui::editor_scm::commit_button_label;
    let status = |branch: &str| ember_client::api::GitStatus { branch: branch.into(), ..ember_client::api::GitStatus::default() };
    assert_eq!(commit_button_label(false, false, Some(&status("feat/git-api"))), (Some("feat/git-api".to_string()), "에 커밋"));
    assert_eq!(commit_button_label(false, false, Some(&status("(detached)"))), (None, "커밋"));
    assert_eq!(commit_button_label(false, false, None), (None, "커밋"));
    assert_eq!(commit_button_label(false, true, Some(&status("main"))), (None, "이전 커밋 수정"));
    assert_eq!(commit_button_label(true, true, Some(&status("main"))), (None, "작업하는 중…"));

    let css = include_str!("../../assets/editor/page.css");
    assert!(css.contains("min-height:var(--h-btn);padding:8px 16px;background:var(--ink);color:var(--ink-text);font-size:var(--fs-md);font-weight:var(--fw-medium);"));
    assert!(css.contains(".ed-scm-commit b{font-family:var(--mono);font-size:var(--fs-sm);font-weight:var(--fw-medium)}"));
    assert!(include_str!("../../assets/editor/style.css").contains("--h-btn:40px"));

    let _guard = crate::services::test_guard();
    let mut page = Page::open_styled();
    page.show_changes(ROOT, "", vec![change("a.rs")]);
    let button = page
        .find_where("the commit button", |id| page.has_class(id, "ed-scm-commit"))
        .expect("the commit button");
    assert!(page.attr(button, "aria-label").is_some_and(|l| l.starts_with("커밋 (")));
    assert!(page.dom.document().get_node(button).is_some_and(|n| n.text_content() == "main에 커밋"), "the branch, then the words");
    let icon = page.dom.document().get_node(button).map(|n| n.children.clone()).unwrap_or_default().into_iter().find_map(|id| page.attr(id, "src"));
    assert!(icon.is_some_and(|src| src.starts_with("icon:commit-w")), "the icon is drawn in the fill's text colour");
    let message = page.find("aria-label", "커밋 메시지").expect("the message box");
    let b = page.boxes(&[("the commit button", button), ("the message box", message)]);
    assert!(near(b[0].3, 40.0), "40px high on one line: {:?}", b[0]);
    assert!(near(b[0].0, b[1].0) && near(b[0].2, b[1].2), "as wide as the message box: {b:?}");
}

impl Page {
    /// Clicks the bottom tool window's tab whose label starts with `label`.
    fn show_bottom_tab(&mut self, label: &str) {
        let tab = self
            .find_where(&format!("the bottom tab {label}"), |id| {
                self.attr(id, "role").as_deref() == Some("tab")
                    && self.dom.document().get_node(id).is_some_and(|n| n.text_content().starts_with(label))
                    && self.parent(id).is_some_and(|p| self.has_class(p, "ed-btabs"))
            })
            .unwrap_or_else(|| panic!("the bottom tab {label}"));
        self.dom.click_node(tab);
        self.dom.render();
    }

    /// The text of the one notice (`.ed-note`) in the bottom tool window.
    fn bottom_note(&self) -> String {
        let note = self
            .find_where("the bottom window's notice", |id| self.has_class(id, "ed-note"))
            .expect("a notice in the bottom window");
        assert!(!self.has_class(note, "ed-term"), "a notice is not terminal text");
        self.dom.document().get_node(note).map(|n| n.text_content()).unwrap_or_default()
    }
}

// FR-L31.34 (#895, design review E9): the notices of the bottom tool window are prose in the UI
// face (`.ed-note`), all four the same; the mono terminal body is for output only.
#[test]
fn fr_l31_34_tool_window_notices_are_prose_not_terminal_text() {
    let css = include_str!("../../assets/editor/page.css");
    assert!(css.contains(".ed-note{flex:1;overflow:auto;min-height:0;padding:var(--sp3) var(--sp4);font:var(--fw-regular) var(--fs-sm)/var(--lh-sm) var(--font);color:var(--muted)}"));
    assert!(include_str!("../../assets/editor/editor.css").contains(".ed-term{flex:1;overflow:auto;padding:var(--sp3) var(--sp4);font:var(--code-fs)/var(--code-lh) var(--mono);"));
    for (file, source) in [("editor_workbench.rs", include_str!("editor_workbench.rs")), ("editor_tasks.rs", include_str!("editor_tasks.rs"))] {
        assert!(!source.contains(r#"class: "ed-term", style: "color:var(--muted);""#), "{file}: a muted note in the terminal class");
        assert!(!source.contains(r#"class: "ed-term", style: "flex:1; color:var(--muted);""#), "{file}: a muted note in the terminal class");
        assert!(!source.contains("font-family:var(--font-mono)"), "{file}: a font token that does not exist");
    }

    let _guard = crate::services::test_guard();
    let mut page = Page::open();
    // No server address in this window: the terminal says so.
    assert!(page.bottom_note().starts_with("터미널은 서버에 연결했을 때만"));
    page.show_bottom_tab("문제");
    assert_eq!(page.bottom_note(), "문제가 없습니다.");
    page.show_bottom_tab("출력");
    assert_eq!(page.bottom_note(), "출력이 없습니다.");
}

// FR-L31.35 (#895, design review E10): 출력's channels are 30px pills, the shown one filled, by the
// mockup's bottom-tab rule.
#[test]
fn fr_l31_35_output_channels_are_30px_pills() {
    let css = include_str!("../../assets/editor/editor.css");
    assert!(css.contains(".ed-btabs button{height:30px;padding:0 var(--sp3);border:0;border-radius:var(--pill);background:none;"));
    assert!(css.contains(".ed-btabs button.on{background:var(--ink);color:var(--ink-text)}"));

    let _guard = crate::services::test_guard();
    let mut page = Page::open_styled();
    page.show_bottom_tab("출력");
    let group = page.find("aria-label", "출력 채널").expect("the channel switch");
    assert!(page.has_class(group, "ed-btabs"), "the channels take the bottom tabs' rule");
    let channels = page.all_where(|id| page.parent(id) == Some(group) && page.attr(id, "aria-pressed").is_some());
    assert_eq!(channels.len(), 1, "워크벤치 alone until an extension opens a channel");
    let workbench = channels[0];
    assert_eq!(page.attr(workbench, "aria-pressed").as_deref(), Some("true"));
    assert!(page.has_class(workbench, "on") && !page.has_class(workbench, "ed-pj"));
    let _ = page.dom.layout(1440.0, 900.0, 1.0);
    let list = page.dom.display_list().expect("laid out");
    let entry = list.get(workbench).expect("the channel's box");
    assert!(near(entry.rect.height, 30.0), "30px high: {:?}", entry.rect);
    assert!(entry.background.is_some() && entry.radii.is_some(), "the shown channel is a filled pill");
}

// FR-L31.36 (#895, design review E11): with no tab open the editor panel has no tab strip (and no
// ⋯ button or divider), and the empty state starts at the panel's top; the strip is back with the
// first tab.
#[test]
fn fr_l31_36_the_editor_has_no_tab_strip_while_no_tab_is_open() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open_styled();
    // A trusted folder, so no restricted-mode banner sits above the empty state.
    page.host().state.lock().unwrap().trust = crate::ui::workspace_trust::TrustState { trusted: true, prompt: false, banner_dismissed: false };
    page.redraw();
    assert!(page.find("aria-label", "열린 파일").is_none(), "no tab strip");
    assert!(page.find("title", "탭 메뉴").is_none(), "no ⋯ button");
    assert!(page.text().contains("열린 파일이 없습니다"));
    let main = page.find("aria-label", "편집기").expect("the editor panel");
    let empty = page.find("class", "ed-empty").expect("the empty state");
    let b = page.boxes(&[("the editor panel", main), ("the empty state", empty)]);
    // The panel has a 1px border; the empty state fills what is inside it.
    assert!(near(b[1].1, b[0].1 + 1.0), "the empty state starts at the panel's top: {b:?}");
    assert!(near(b[1].3, b[0].3 - 2.0), "and fills the panel: {b:?}");

    page.click_row();
    page.deliver(opened(1));
    let strip = page.find("aria-label", "열린 파일").expect("the tab strip, with a tab open");
    assert_eq!(page.attr(strip, "role").as_deref(), Some("tablist"));
    assert!(page.find("title", "탭 메뉴").is_some(), "the ⋯ button is back");
    assert!(page.current_tab_is(FILE));
}

#[compose_rust::runtime::Composable]
fn toolkit_probe() {
    { use ::compose_rust::ui::Text; Text!("probe".to_string()) };
}

// FR-L33.1 (#875): the window's safe area reaches the toolkit after the page's first render. The
// report renders the page again, so the top bar takes the traffic lights' inset, also while the
// workspace trust prompt is up and nothing else renders the page.
#[test]
fn fr_l33_1_the_safe_area_arriving_after_the_first_render_moves_the_top_bar() {
    use compose_rust::protocol::HostEvent;
    use compose_rust::{EventPayload, WindowHeightClass, WindowSizeClass};
    use crate::ui::safe_area::SafeArea;
    let _guard = crate::services::test_guard();
    // A toolkit Host resets the window's measurement when it is made, so it comes first.
    let mut toolkit = compose_rust::Host::new(|| Box::new(compose_rust::Recomposer::new(toolkit_probe)));
    toolkit.rebuild().expect("rebuild");
    let mut page = Page::open();
    page.host().state.lock().unwrap().trust = crate::ui::workspace_trust::TrustState { trusted: false, prompt: true, banner_dismissed: false };
    page.redraw();
    assert!(page.find("aria-label", crate::ui::workspace_trust::PROMPT_TITLE).is_some(), "the trust prompt is up");
    let top = page.find("class", "ed-top").expect("the top bar");
    let before = page.attr(top, "style").unwrap_or_default();
    assert_eq!(before, crate::ui::editor::editor_strip_padding(0.0, 0.0), "nothing reported yet");

    let lights = SafeArea { top: 28.0, leading: 78.0, trailing: 0.0 };
    toolkit
        .dispatch(HostEvent::new(0, 0, EventPayload::WindowSizeChanged {
                width_dp: 1440.0,
                height_dp: 900.0,
                class: WindowSizeClass::from_width_dp(1440.0),
                height_class: WindowHeightClass::from_height_dp(900.0),
                safe_area_top: lights.top,
                safe_area_leading: lights.leading,
                safe_area_trailing: lights.trailing,
                safe_area_bottom: 0.0,
            }))
        .expect("dispatch");
    // The report woke the page; nothing else asks it to render.
    page.dom.virtual_dom_mut().process_events();
    page.dom.render();
    let top = page.find("class", "ed-top").expect("the top bar");
    let after = page.attr(top, "style").unwrap_or_default();
    let expected = crate::ui::editor::editor_strip_padding(lights.leading, lights.trailing);
    assert_eq!(after, expected, "the top bar follows the reported safe area with the prompt up");
    assert!(page.find("aria-label", crate::ui::workspace_trust::PROMPT_TITLE).is_some(), "the prompt is still up");
    compose_rust::window::reset_window_size();
}

// SPEC FR-E3 (#32 phase A step 7): a hover update for the active document draws the hover card over
// the editor once the editor has said where the hovered word is; a hover for another place, or
// none, draws nothing.
#[test]
fn fr_e3_a_hover_draws_the_card_where_the_editor_says_the_word_is() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open();
    page.click_row();
    page.deliver(opened(7));
    let word = ember_editor::WidgetRange::new(ember_editor::WidgetPos::new(0, 3), ember_editor::WidgetPos::new(0, 7));
    let popup = ember_editor::hover::HoverPopup {
        doc: uri().key(),
        generation: 7,
        pos: ember_editor::WidgetPos::new(0, 5),
        range: Some(word),
        diagnostics: Vec::new(),
        contents: vec![ember_editor::hover::MarkdownString { value: "fn main()".into(), is_trusted: false, support_theme_icons: false, support_html: false, raw: serde_json::Value::Null }],
        complete: true,
    };
    page.deliver(SessionUpdate::Hover(popup));
    assert!(page.find("data-ed", "hover-card").is_none(), "no card before the editor answers");
    super::set_hover_rect((7, 0, 9), Some(compose_rust::code::CodeRect::new(40.0, 60.0, 2.0, 18.0)));
    page.redraw();
    assert!(page.find("data-ed", "hover-card").is_none(), "an answer for another position draws nothing");
    super::set_hover_rect((7, 0, 3), Some(compose_rust::code::CodeRect::new(40.0, 60.0, 2.0, 18.0)));
    page.redraw();
    let card = page.find("data-ed", "hover-card").expect("the card is drawn");
    assert!(page.attr(card, "style").unwrap_or_default().contains("top:80px;"));
    assert!(page.text().contains("fn main()"));
    page.deliver(SessionUpdate::HoverHidden { uri: uri() });
    assert!(page.find("data-ed", "hover-card").is_none(), "the card goes with the hover");
    super::set_hover_rect((0, 0, 0), None);
}

impl Page {
    /// The page with `a.rs` and `b.rs` listed under the project folder.
    fn open_two_files() -> Page {
        let mut page = Page::open();
        page.host().state.lock().unwrap().listings.insert(
            ROOT.to_string(),
            vec![
                ("a.rs".to_string(), ember_editor_conn::remote_fs::file_type::FILE),
                ("b.rs".to_string(), ember_editor_conn::remote_fs::file_type::FILE),
            ],
        );
        page.redraw();
        page
    }

    fn tree_row(&self, name: &str) -> NodeId {
        self.find_where(&format!("the {name} tree row"), |id| {
            self.attr(id, "role").as_deref() == Some("treeitem") && self.attr(id, "aria-label").as_deref() == Some(name)
        })
        .unwrap_or_else(|| panic!("the {name} row in the project tree"))
    }

    fn tab_of(&self, path: &str) -> NodeId {
        self.find_where(&format!("the tab of {path}"), |id| {
            self.attr(id, "role").as_deref() == Some("tab") && self.attr(id, "title").as_deref() == Some(path)
        })
        .unwrap_or_else(|| panic!("the tab of {path}"))
    }

    fn press(&mut self, node: Option<NodeId>, key: Key, code: Code) {
        let input = dioxus_compose::html::KeyInput::new(key, code, Modifiers::empty());
        self.dom.key_down_input(node, &input);
        self.dom.render();
    }
}

// FR-L31.5 (#302): Down from a.rs, then Enter, opens b.rs, the row the arrow key selected, and never
// a.rs, the row that had focus before it.
#[test]
fn fr_l31_tree_enter_opens_the_row_the_arrow_keys_selected() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open_two_files();
    page.click_row();
    page.sent();

    let a = page.tree_row("a.rs");
    page.press(Some(a), Key::ArrowDown, Code::ArrowDown);
    assert_eq!(page.attr(page.tree_row("b.rs"), "aria-selected").as_deref(), Some("true"));
    page.sent();

    // The key goes to whichever row holds focus, as the window delivers it.
    page.press(None, Key::Enter, Code::Enter);
    let sent = page.sent();
    assert!(sent.contains(&format!("open {ROOT}/b.rs")), "{sent:?}");
    assert!(!sent.contains(&format!("open {ROOT}/a.rs")), "{sent:?}");
}

// FR-L31.5 (#302): an arrow key moves keyboard focus to the row it selected, as Code-OSS does, so
// the row with `tabindex="0"`, the selected row and the focused row are one.
#[test]
fn fr_l31_tree_focus_follows_the_arrow_selection() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open_two_files();
    page.click_row();

    let a = page.tree_row("a.rs");
    page.press(Some(a), Key::ArrowDown, Code::ArrowDown);
    assert_eq!(page.dom.focused(), Some(page.tree_row("b.rs")), "Down moves focus to b.rs");
    assert_eq!(page.attr(page.tree_row("b.rs"), "tabindex").as_deref(), Some("0"));

    let b = page.tree_row("b.rs");
    page.press(Some(b), Key::ArrowUp, Code::ArrowUp);
    assert_eq!(page.dom.focused(), Some(page.tree_row("a.rs")), "Up moves focus back to a.rs");
}

// FR-L31.5 (#302): the arrow keys in the tab strip move focus to the tab they activated, as they do
// in the tree.
#[test]
fn fr_l31_tabs_arrow_keys_move_focus_with_the_selection() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open_two_files();
    {
        use dioxus_compose::html::prelude::dioxus_signals::WritableExt;
        let mut tabs = super::shared().tabs;
        let mut tabs = tabs.write();
        tabs.open(&format!("{ROOT}/a.rs"));
        tabs.open(&format!("{ROOT}/b.rs"));
        tabs.activate(&format!("{ROOT}/a.rs"));
    }
    page.redraw();

    let a = page.tab_of(&format!("{ROOT}/a.rs"));
    page.press(Some(a), Key::ArrowRight, Code::ArrowRight);
    assert!(page.current_tab_is(&format!("{ROOT}/b.rs")));
    assert_eq!(page.dom.focused(), Some(page.tab_of(&format!("{ROOT}/b.rs"))), "Right moves focus to b.rs");

    let b = page.tab_of(&format!("{ROOT}/b.rs"));
    page.press(Some(b), Key::ArrowLeft, Code::ArrowLeft);
    assert_eq!(page.dom.focused(), Some(page.tab_of(&format!("{ROOT}/a.rs"))), "Left moves focus back to a.rs");
}

// FR-L31.7 (#302): the tab width is a button with a name, as the line ending, the encoding and the
// language are; it opens the indentation pick.
#[test]
fn fr_l31_status_tab_width_is_a_button_with_a_name() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open();
    page.click_row();
    page.deliver(opened(1));

    let item = page.find("aria-label", "들여쓰기 선택: 4칸").expect("the tab width item is named");
    let tag = page.dom.document().get_node(item).and_then(|n| n.element_data().map(|e| e.name.local.to_string()));
    assert_eq!(tag.as_deref(), Some("button"), "the tab width is a button, not static text");
    assert_eq!(page.attr(item, "title").as_deref(), Some("들여쓰기 선택"));

    page.click("aria-label", "들여쓰기 선택: 4칸");
    assert!(page.find("aria-label", "들여쓰기 작업 검색").is_some(), "the indentation pick is open");
    assert!(page.find("aria-label", "설정 검색").is_none(), "the settings view is not");
}

impl Page {
    /// The titles of the rows of the open pick, in order.
    fn pick_rows(&self) -> Vec<String> {
        // Document order: node ids follow creation order, and a row the list drew again has a new id.
        let doc = self.dom.document();
        let mut rows = Vec::new();
        let mut stack = vec![doc.root_element().id];
        while let Some(id) = stack.pop() {
            if self.attr(id, "role").as_deref() == Some("option") {
                rows.push(doc.get_node(id).map(|n| n.text_content()).unwrap_or_default());
            }
            if let Some(node) = doc.get_node(id) {
                stack.extend(node.children.iter().rev());
            }
        }
        rows
    }

    /// Clicks the row of the open pick whose text is `text`.
    fn click_pick_row(&mut self, text: &str) {
        let row = self
            .find_where(&format!("the pick row {text:?}"), |id| {
                self.attr(id, "role").as_deref() == Some("option")
                    && self.dom.document().get_node(id).is_some_and(|n| n.text_content() == text)
            })
            .unwrap_or_else(|| panic!("no pick row {text:?} in {:?}", self.pick_rows()));
        self.dom.click_node(row);
        self.dom.render();
    }

    /// The indentation commands sent since the last call (closing a pick also cancels the file index).
    fn sent_indentation(&mut self) -> Vec<String> {
        self.sent().into_iter().filter(|c| c.starts_with("set indentation ")).collect()
    }

    /// A page with `a.rs` open and the indentation pick showing.
    fn open_indentation_pick() -> Page {
        let mut page = Page::open();
        page.click_row();
        page.deliver(opened(1));
        let _ = page.sent();
        page.click("aria-label", "들여쓰기 선택: 4칸");
        page
    }
}

// FR-L31.7 (#302): the status bar's indentation item opens Code-OSS's "Select Indentation" pick: the
// rows of `showIndentationPicker` in its order, as Code-OSS's Korean pack words them.
#[test]
fn fr_l31_status_indentation_opens_the_select_indentation_pick() {
    let _guard = crate::services::test_guard();
    let page = Page::open_indentation_pick();
    assert_eq!(
        page.pick_rows(),
        [
            "공백을 사용한 들여쓰기",
            "탭을 사용한 들여쓰기",
            "탭 표시 크기 변경",
            "콘텐츠에서 들여쓰기 감지",
            "들여쓰기를 공백으로 변환",
            "들여쓰기를 탭으로 변환",
            "후행 공백 자르기",
        ]
    );
}

// FR-L31.7 (#302): spaces, then a size: the list of sizes marks the current one, the choice goes to
// the session as that document's options, and the setting is not touched.
#[test]
fn fr_l31_indent_using_spaces_then_size_sets_the_document_options() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open_indentation_pick();
    page.click_pick_row("공백을 사용한 들여쓰기");
    assert!(page.find("aria-label", "탭 크기 검색").is_some(), "the size list is open");
    let rows = page.pick_rows();
    assert_eq!(rows.len(), 8);
    assert_eq!(rows[3], "4구성된 탭 크기", "the settings' size is marked");
    assert_eq!(rows[1], "2");
    page.click_pick_row("2");
    let sent = page.sent_indentation();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].starts_with("set indentation ") && sent[0].ends_with("IndentUsing { insert_spaces: true, size: 2 }"), "{sent:?}");

    // The session answers with the document's options: the item shows them.
    page.deliver(SessionUpdate::Indentation { uri: uri(), indentation: ember_editor::Indentation::new(2, true) });
    assert!(page.find("aria-label", "들여쓰기 선택: 2칸").is_some());
    page.deliver(SessionUpdate::Indentation { uri: uri(), indentation: ember_editor::Indentation::new(4, false) });
    assert!(page.find("aria-label", "들여쓰기 선택: 탭 크기: 4").is_some());
    // The user setting is what it was: the document's choice is not a setting.
    assert_eq!(page.host().state.lock().unwrap().settings.editor(None).tab_size, 4);
}

#[test]
fn fr_l31_change_tab_display_size_keeps_insert_spaces() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open_indentation_pick();
    page.click_pick_row("탭 표시 크기 변경");
    page.click_pick_row("8");
    let sent = page.sent_indentation();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].ends_with("DisplaySize(8)"), "only the tab's width changes: {sent:?}");
    page.deliver(SessionUpdate::Indentation {
        uri: uri(),
        indentation: ember_editor::Indentation { tab_size: 8, indent_size: 4, insert_spaces: true },
    });
    assert!(page.find("aria-label", "들여쓰기 선택: 4칸 (탭 크기: 8)").is_some());
}

#[test]
fn fr_l31_detect_and_convert_indentation_go_to_the_session() {
    let _guard = crate::services::test_guard();
    sends_indentation_action("콘텐츠에서 들여쓰기 감지", "Detect");
}

#[test]
fn fr_l31_convert_indentation_to_spaces_goes_to_the_session() {
    let _guard = crate::services::test_guard();
    sends_indentation_action("들여쓰기를 공백으로 변환", "Convert { to_spaces: true }");
}

#[test]
fn fr_l31_convert_indentation_to_tabs_goes_to_the_session() {
    let _guard = crate::services::test_guard();
    sends_indentation_action("들여쓰기를 탭으로 변환", "Convert { to_spaces: false }");
}

#[test]
fn fr_l31_trim_trailing_whitespace_goes_to_the_session() {
    let _guard = crate::services::test_guard();
    sends_indentation_action("후행 공백 자르기", "TrimTrailingWhitespace");
}

/// One page per test: the page's UI signals live in the first page's scope, so a second page in the
/// same test reads dropped signals.
fn sends_indentation_action(row: &str, ends_with: &str) {
    let mut page = Page::open_indentation_pick();
    page.click_pick_row(row);
    let sent = page.sent_indentation();
    assert_eq!(sent.len(), 1, "{row}: {sent:?}");
    assert!(sent[0].ends_with(ends_with), "{row}: {sent:?}");
}

#[test]
fn double_click_pins_preview_tab_from_tree_and_tab_strip() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open_styled();
    page.host().state.lock().unwrap().listings.insert(
        ROOT.to_string(),
        vec![
            ("a.rs".to_string(), ember_editor_conn::remote_fs::file_type::FILE),
            ("b.rs".to_string(), ember_editor_conn::remote_fs::file_type::FILE),
            ("c.rs".to_string(), ember_editor_conn::remote_fs::file_type::FILE),
            ("d.rs".to_string(), ember_editor_conn::remote_fs::file_type::FILE),
        ],
    );
    page.redraw();
    let _ = page.dom.layout(1280.0, 820.0, 1.0);

    let row_of = |p: &Page, name: &str| {
        p.find_where(&format!("tree row {name}"), |id| {
            p.attr(id, "role").as_deref() == Some("treeitem") && p.attr(id, "aria-label").as_deref() == Some(name)
        })
        .unwrap_or_else(|| panic!("missing tree row {name}"))
    };
    let tab_of = |p: &Page, path: &str| {
        p.find_where(&format!("tab {path}"), |id| {
            p.attr(id, "role").as_deref() == Some("tab") && p.attr(id, "title").as_deref() == Some(path)
        })
    };

    // Single-click opens a.rs as a preview tab.
    let row_a = row_of(&page, "a.rs");
    page.dom.click_node(row_a);
    page.dom.render();
    let _ = page.dom.layout(1280.0, 820.0, 1.0);
    assert!(super::shared().tabs.read().is_preview("/w/a.rs"));

    // Double-click on the tree row keeps a.rs permanent.
    let row_a = row_of(&page, "a.rs");
    page.dom.pointer_event(row_a, &PointerInput::press(10.0, 10.0, 1, 2));
    page.dom.pointer_event(row_a, &PointerInput::release(10.0, 10.0, 1, 2));
    page.dom.render();
    let _ = page.dom.layout(1280.0, 820.0, 1.0);
    assert!(!super::shared().tabs.read().is_preview("/w/a.rs"), "tree double-click keeps a.rs permanent");

    // Single-click b.rs opens b.rs as preview without replacing a.rs.
    let row_b = row_of(&page, "b.rs");
    page.dom.click_node(row_b);
    page.dom.render();
    let _ = page.dom.layout(1280.0, 820.0, 1.0);
    assert!(tab_of(&page, "/w/a.rs").is_some(), "a.rs stays open");
    assert!(super::shared().tabs.read().is_preview("/w/b.rs"), "b.rs opens as preview");

    // Double-click on the b.rs tab in the tab strip keeps b.rs permanent.
    let tab_b = tab_of(&page, "/w/b.rs").expect("b.rs tab");
    page.dom.pointer_event(tab_b, &PointerInput::press(10.0, 10.0, 1, 2));
    page.dom.pointer_event(tab_b, &PointerInput::release(10.0, 10.0, 1, 2));
    page.dom.render();
    let _ = page.dom.layout(1280.0, 820.0, 1.0);
    assert!(!super::shared().tabs.read().is_preview("/w/b.rs"), "tab double-click keeps b.rs permanent");

    // Single-click c.rs opens c.rs as preview alongside both permanent tabs.
    let row_c = row_of(&page, "c.rs");
    page.dom.click_node(row_c);
    page.dom.render();
    let _ = page.dom.layout(1280.0, 820.0, 1.0);
    assert!(tab_of(&page, "/w/a.rs").is_some());
    assert!(tab_of(&page, "/w/b.rs").is_some());
    assert!(tab_of(&page, "/w/c.rs").is_some());

    // Double-clicking an unopened tree row directly (before onclick) opens it as permanent.
    let row_d = row_of(&page, "d.rs");
    page.dom.pointer_event(row_d, &PointerInput::press(10.0, 10.0, 1, 2));
    page.dom.pointer_event(row_d, &PointerInput::release(10.0, 10.0, 1, 2));
    page.dom.render();
    assert!(!super::shared().tabs.read().is_preview("/w/d.rs"), "direct double-click opens d.rs permanent");
    assert!(tab_of(&page, "/w/d.rs").is_some());
}

#[test]
fn ctrl_tab_and_ctrl_shift_tab_switch_open_tabs() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open();
    page.host().state.lock().unwrap().listings.insert(
        ROOT.to_string(),
        vec![
            ("a.rs".to_string(), ember_editor_conn::remote_fs::file_type::FILE),
            ("b.rs".to_string(), ember_editor_conn::remote_fs::file_type::FILE),
            ("c.rs".to_string(), ember_editor_conn::remote_fs::file_type::FILE),
        ],
    );
    super::page::open_tab_file(&super::shared(), "/w/a.rs");
    super::page::open_tab_file(&super::shared(), "/w/b.rs");
    super::page::open_tab_file(&super::shared(), "/w/c.rs");
    page.redraw();
    let _ = page.sent();
    assert!(page.current_tab_is("/w/c.rs"));

    let root = page.find("class", "ed").expect("workbench root");
    page.dom.key_down(root, Key::Tab, Code::Tab, Modifiers::CONTROL);
    page.dom.render();
    assert!(page.current_tab_is("/w/a.rs"), "Ctrl+Tab wraps from last tab to first");
    assert!(page.sent().iter().any(|c| c == "focus /w/a.rs"));

    let root = page.find("class", "ed").expect("workbench root");
    page.dom.key_down(root, Key::Tab, Code::Tab, Modifiers::CONTROL);
    page.dom.render();
    assert!(page.current_tab_is("/w/b.rs"), "Ctrl+Tab advances to second tab");
    assert!(page.sent().iter().any(|c| c == "focus /w/b.rs"));

    let root = page.find("class", "ed").expect("workbench root");
    page.dom.key_down(root, Key::Tab, Code::Tab, Modifiers::CONTROL | Modifiers::SHIFT);
    page.dom.render();
    assert!(page.current_tab_is("/w/a.rs"), "Ctrl+Shift+Tab moves back to first tab");
    assert!(page.sent().iter().any(|c| c == "focus /w/a.rs"));
}

#[test]
fn tree_self_drop_and_unchanged_rename_do_not_send_noop_commands() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open_styled();
    page.host().state.lock().unwrap().listings.insert(
        ROOT.to_string(),
        vec![
            ("a.rs".to_string(), ember_editor_conn::remote_fs::file_type::FILE),
            ("b.rs".to_string(), ember_editor_conn::remote_fs::file_type::FILE),
        ],
    );
    page.redraw();
    let _ = page.dom.layout(1280.0, 820.0, 1.0);

    let row_a = page
        .find_where("a.rs tree row", |id| {
            page.attr(id, "role").as_deref() == Some("treeitem") && page.attr(id, "aria-label").as_deref() == Some("a.rs")
        })
        .expect("a.rs row");
    page.dom.click_node(row_a);
    page.dom.render();
    let _ = page.dom.layout(1280.0, 820.0, 1.0);
    let _ = page.sent();

    // Drag a.rs slightly and drop it on itself: same parent directory, so no MovePaths is sent.
    let row_a = page
        .find_where("a.rs tree row", |id| {
            page.attr(id, "role").as_deref() == Some("treeitem") && page.attr(id, "aria-label").as_deref() == Some("a.rs")
        })
        .expect("a.rs row");
    page.dom.pointer_event(row_a, &PointerInput::press(10.0, 10.0, 1, 1));
    let mut move_ev = PointerInput::press(20.0, 10.0, 1, 0);
    move_ev.kind = dioxus_compose::html::PointerKind::Move;
    move_ev.buttons = 1;
    page.dom.pointer_event(row_a, &move_ev);
    page.dom.pointer_event(row_a, &PointerInput::release(20.0, 10.0, 1, 1));
    page.dom.render();
    let sent = page.sent();
    assert!(
        !sent.iter().any(|c| c.starts_with("move ")),
        "dropping a file onto itself must not send MovePaths: {sent:?}"
    );

    // F2 starts rename; pressing Enter without editing closes the prompt without sending Rename.
    let tree = page.find("role", "tree").expect("project tree");
    page.dom.key_down(tree, Key::F2, Code::F2, Modifiers::empty());
    page.dom.render();
    let input = page.find("aria-label", "새 이름").expect("inline rename input");
    page.dom.key_down(input, Key::Enter, Code::Enter, Modifiers::empty());
    page.dom.render();
    assert!(page.find("aria-label", "새 이름").is_none(), "rename input closes on unchanged Enter");
    let sent = page.sent();
    assert!(
        !sent.iter().any(|c| c.starts_with("rename ")),
        "unchanged rename must not send Rename: {sent:?}"
    );

    // F2 + new name + Enter still sends Rename.
    let tree = page.find("role", "tree").expect("project tree");
    page.dom.key_down(tree, Key::F2, Code::F2, Modifiers::empty());
    page.dom.render();
    let input = page.find("aria-label", "새 이름").expect("inline rename input");
    page.dom.input(input, "renamed.rs");
    page.dom.render();
    let input = page.find("aria-label", "새 이름").expect("inline rename input");
    page.dom.key_down(input, Key::Enter, Code::Enter, Modifiers::empty());
    page.dom.render();
    let sent = page.sent();
    assert!(
        sent.iter().any(|c| c == "rename /w/a.rs renamed.rs"),
        "changed rename sends Rename: {sent:?}"
    );
}

// FR-L31.6 (A3, window half): the editor pane tells the bridge about every mount of its code editor,
// once per mount. A mount of a document the reader already typed in puts the widget's count back to 0,
// so an edit the session queued for the widget is written against the widget's own numbering.
#[test]
fn fr_l31_6_a3_window_a_remount_is_reported_once_and_queued_edits_use_the_widget_numbering() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open();
    page.click_row();
    page.deliver(opened(7));
    let shared = super::shared();
    let other = UriComponents::remote("test", "/w/b.rs");
    super::forget_mount();

    // First mount, then one character typed: the widget is at version 1, and so is the session.
    assert!(super::report_mount(&shared, &uri(), 7), "the first mount is reported");
    let typed = shared.bridge.borrow_mut().on_code_changed(&uri(), 1, ember_editor::WidgetRange::empty(ember_editor::WidgetPos::new(0, 0)), "x".into());
    assert!(matches!(typed, Some((_, 7, WidgetEvent::CodeChanged { version: 1, .. }))));
    // A render without a remount does not report again.
    assert!(!super::report_mount(&shared, &uri(), 7), "a re-render is not a mount");

    // To b.rs and back: the editor is built again and counts from 0.
    assert!(super::report_mount(&shared, &other, 8));
    assert!(super::report_mount(&shared, &uri(), 7), "coming back is a new mount");
    assert_eq!(shared.bridge.borrow().document(&uri()).unwrap().widget_version_of(1), 0);

    // An edit the session wrote against its version 1 reaches the widget as version 0.
    page.deliver(SessionUpdate::Command {
        uri: uri(),
        command: ember_editor::WidgetCommand::EditCode { request_id: 9, base_version: 1, range: ember_editor::WidgetRange::empty(ember_editor::WidgetPos::new(0, 0)), text: "y".into() },
    });
    let queued = shared.bridge.borrow_mut().take_pending_edits(&uri());
    assert!(matches!(queued.as_slice(), [ember_editor::WidgetCommand::EditCode { base_version: 0, .. }]), "{queued:?}");

    // The next character typed in the widget (its version 1) is the session's version 2: no gap.
    let typed = shared.bridge.borrow_mut().on_code_changed(&uri(), 1, ember_editor::WidgetRange::empty(ember_editor::WidgetPos::new(0, 1)), "z".into());
    assert!(matches!(typed, Some((_, 7, WidgetEvent::CodeChanged { version: 2, .. }))));
    assert_eq!(shared.bridge.borrow().document(&uri()).unwrap().text, "xzfn main() {}\n");
    super::forget_mount();
}

// FR-L31.6 (A3, compare view): the compare view's editable side is a code editor of its own over the same
// document. Opening it is a mount; going back to the tab's pane is another; and the compare view going
// away while the pane is on screen does not make the pane's next render count as a mount, which would
// reset the version count under a widget that kept its own.
#[test]
fn fr_l31_6_a3_window_the_compare_view_editor_is_its_own_mount() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open();
    page.click_row();
    page.deliver(opened(7));
    let shared = super::shared();
    super::forget_mount();
    super::forget_view_mount("diff");

    assert!(super::report_mount(&shared, &uri(), 7), "the pane's first mount is reported");
    let typed = shared.bridge.borrow_mut().on_code_changed(&uri(), 1, ember_editor::WidgetRange::empty(ember_editor::WidgetPos::new(0, 0)), "x".into());
    assert!(matches!(typed, Some((_, 7, WidgetEvent::CodeChanged { version: 1, .. }))));

    // The compare view opens over the same document and generation: a new widget, counted from 0.
    assert!(super::report_view_mount(&shared, &uri(), 7, "diff"), "the compare view's editor is a mount");
    assert!(!super::report_view_mount(&shared, &uri(), 7, "diff"), "a re-render of it is not");
    assert_eq!(shared.bridge.borrow().document(&uri()).unwrap().widget_version_of(1), 0);
    let typed = shared.bridge.borrow_mut().on_code_changed(&uri(), 1, ember_editor::WidgetRange::empty(ember_editor::WidgetPos::new(0, 1)), "y".into());
    assert!(matches!(typed, Some((_, 7, WidgetEvent::CodeChanged { version: 2, .. }))), "{typed:?}");

    // Back to the tab: the pane's editor is built again, so that is a mount too.
    assert!(super::report_mount(&shared, &uri(), 7), "the pane after the compare view is a mount");
    // The compare view's scope ends after the pane drew: the pane's record stays.
    drop(super::MountGuard("diff"));
    assert!(!super::report_mount(&shared, &uri(), 7), "the pane's re-render is still not a mount");
    assert_eq!(shared.bridge.borrow().document(&uri()).unwrap().text, "xyfn main() {}\n");
    super::forget_mount();
}

const OTHER: &str = "/w/b.rs";

fn other_opened(generation: u64, text: &str) -> SessionUpdate {
    SessionUpdate::Opened { uri: UriComponents::remote("test", OTHER), generation, text: text.into(), language_id: "rust".into(), tab_width: 4 }
}

impl Page {
    /// a.rs (with one character typed, so it has unsaved edits) and b.rs (untouched) are open in tabs.
    fn open_dirty_a_clean_b() -> Page {
        let mut page = Page::open_two_files();
        let shared = super::shared();
        { let mut tabs = shared.tabs; let mut tabs = dioxus_compose::prelude::WritableExt::write(&mut tabs); tabs.open(FILE); tabs.open(OTHER); tabs.activate(FILE); }
        page.deliver(opened(7));
        page.deliver(other_opened(8, "fn other() {}\n"));
        shared.bridge.borrow_mut().on_code_changed(&uri(), 1, ember_editor::WidgetRange::empty(ember_editor::WidgetPos::new(0, 0)), "x".into()).expect("a.rs is open");
        page.sent();
        page
    }

    /// The window's connection came back: the host counts one more reconnect.
    fn reconnect(&mut self) {
        self.host().state.lock().unwrap().reconnects += 1;
        self.redraw();
    }
}

// FR-L31.6 (A2, window half): after a reconnect the tab with unsaved edits is put back with the file
// text it was based on, and the tab without is only opened again, so it shows the file as it is now.
#[test]
fn fr_l31_6_a2_window_a_reconnect_restores_only_the_dirty_tab_and_passes_its_disk_base() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open_dirty_a_clean_b();
    page.reconnect();
    let sent = page.sent();
    assert!(sent.contains(&format!("restore {FILE} text \"xfn main() {{}}\\n\" base Some(\"fn main() {{}}\\n\")")), "{sent:?}");
    assert!(sent.contains(&format!("open {OTHER}")), "{sent:?}");
    assert!(sent.iter().all(|c| !c.starts_with(&format!("restore {OTHER}"))), "a clean tab is not restored: {sent:?}");
}

// FR-L31.6 (A2, window half): a dirty tab over a file that changed while the connection was lost shows
// the conflict bar, and a clean tab shows the new file text.
#[test]
fn fr_l31_6_a2_window_after_a_reconnect_a_dirty_tab_shows_the_conflict_and_a_clean_tab_the_new_disk_text() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open_dirty_a_clean_b();
    page.reconnect();
    // The new session answers: b.rs as the file is now, and a.rs raised as changed under its edits.
    page.deliver(other_opened(9, "fn other_new() {}\n"));
    page.deliver(SessionUpdate::ExternalChange { uri: uri(), change: ember_editor::ExternalChange::ChangedWhileDirty });
    let shared = super::shared();
    let b = shared.bridge.borrow().document(&UriComponents::remote("test", OTHER)).cloned().expect("b.rs is open");
    assert_eq!(b.text, "fn other_new() {}\n");
    assert!(!b.dirty);
    assert!(page.text().contains("a.rs 파일이 디스크에서 바뀌었습니다"), "{}", page.text());
}

fn posted_notice() -> Option<String> {
    crate::ui::coexist::shared_ui().notice.get_untracked().map(|(_, text)| text)
}

// FR-L31.6 (B3, window half): a file notice from the session (a reload that failed, a watch the server
// refused, a watcher error, an edit the editor did not apply) is shown, in Korean, naming the file.
#[test]
fn fr_l31_6_b3_window_each_file_notice_kind_is_shown_with_the_file_name() {
    use ember_editor::FileNoticeKind::{EditRejected, ReloadFailed, WatchRefused, WatcherError};
    let _guard = crate::services::test_guard();
    let mut page = Page::open();
    page.click_row();
    page.deliver(opened(7));
    let mut seen = Vec::new();
    for (kind, uri_of, expect) in [
        (ReloadFailed, Some(uri()), "다시 읽지 못했습니다"),
        (WatchRefused, Some(uri()), "감시"),
        (WatcherError, None, "파일 감시"),
        (EditRejected, Some(uri()), "적용하지 못했습니다"),
    ] {
        page.deliver(SessionUpdate::FileNotice { uri: uri_of.clone(), kind, detail: "EIO: raw server text at /secret".into() });
        let text = posted_notice().unwrap_or_else(|| panic!("{kind:?} shows nothing"));
        assert!(text.contains(expect), "{kind:?}: {text}");
        assert_eq!(text.contains("a.rs"), uri_of.is_some(), "{kind:?} names the file when it has one: {text}");
        assert!(!text.contains("secret") && !text.contains("EIO"), "the server's words are not shown: {text}");
        assert!(!seen.contains(&text), "each kind says its own thing");
        seen.push(text);
    }
}

// FR-L31.6 (B3, window half): a save the session refused because the file changed on disk says so in
// Korean, not in the session's English.
#[test]
fn fr_l31_6_b3_window_a_stale_save_is_shown_in_korean() {
    let _guard = crate::services::test_guard();
    let mut page = Page::open();
    page.click_row();
    page.deliver(opened(7));
    page.deliver(SessionUpdate::ExternalChange { uri: uri(), change: ember_editor::ExternalChange::ChangedWhileDirty });
    page.deliver(SessionUpdate::SaveFailed {
        uri: uri(),
        error: "the file changed on disk after it was opened or last saved, so nothing was written".into(),
    });
    let text = posted_notice().expect("the failed save is shown");
    assert!(text.contains("a.rs") && text.contains("디스크에서 바뀌어"), "{text}");
    assert!(!text.contains("nothing was written") && !text.contains("알 수 없는 오류"), "{text}");
}
