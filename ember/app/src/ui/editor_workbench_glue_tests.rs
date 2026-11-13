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
}

fn target() -> EditorTarget {
    EditorTarget {
        project: String::new(),
        folder: ROOT.to_string(),
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
        let (host, commands) = EditorHost::fake();
        {
            let mut state = host.state.lock().unwrap();
            state.connected = true;
            state.listings.insert(
                ROOT.to_string(),
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
