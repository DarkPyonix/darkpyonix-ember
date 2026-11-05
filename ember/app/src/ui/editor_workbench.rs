//! Remote editor workbench.
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use dioxus_compose::prelude::*;
use ember_editor::{BridgeAction, WidgetBridge, WidgetPos, WidgetRange};
use ember_editor::widget::HoverPhase as EditorHoverPhase;
use crate::editor_host::{EditorHost, HostCommand};
use super::{editor::{EditorTarget, EditorTabs, Stripes, base_name, position_text}, keymap::Keymap, workbench};

#[derive(Clone)]
struct Shared {
    host: EditorHost,
    bridge: Rc<RefCell<WidgetBridge>>,
    revision: SyncSignal<u64>,
    tabs: Signal<EditorTabs>,
    directory: Signal<String>,
    position: Signal<(usize, usize)>,
    stripes: Signal<Stripes>,
    menu_open: Signal<bool>,
    keymap: Signal<Keymap>,
    folder: String,
    project: String,
    project_name: String,
    uris: Rc<RefCell<HashMap<String, ember_editor_conn::uri::UriComponents>>>,
}

// WidgetView accepts a function without props and owns a separate tree. Share signals between
// the trees until WidgetView supports props and flex stretch sizing (dioxus-compose #160).
thread_local! { static SHARED: RefCell<Option<Shared>> = const { RefCell::new(None) }; }
fn shared() -> Shared { SHARED.with(|s| s.borrow().as_ref().expect("workbench initialized").clone()) }

/// Sets up the shared state for the page. Called first by the root page.
fn init(target: EditorTarget) {
    // Signals live in the root scope: the page and the editor pane are separate trees that read them.
    let root = dioxus_compose::html::prelude::dioxus_core::ScopeId::ROOT;
    let revision = use_hook(|| SyncSignal::<u64>::new_maybe_sync_in_scope(0, root));
    let host = use_hook(|| EditorHost::start(target.clone(), revision));
    let tabs = use_hook(|| Signal::new_in_scope(EditorTabs::default(), root));
    let directory = use_hook(|| Signal::new_in_scope(target.folder.clone(), root));
    let position = use_hook(|| Signal::new_in_scope((1, 1), root));
    let stripes = use_hook(|| Signal::new_in_scope(Stripes::default(), root));
    let menu_open = use_hook(|| Signal::new_in_scope(false, root));
    let keymap = use_hook(|| Signal::new_in_scope(crate::services::services().ui.prefs.get_untracked().keymap, root));
    let bridge = use_hook(|| Rc::new(RefCell::new(WidgetBridge::new())));
    let uris = use_hook(|| Rc::new(RefCell::new(HashMap::new())));
    SHARED.with(|s| *s.borrow_mut() = Some(Shared { host, bridge, revision, tabs, directory, position, stripes, menu_open, keymap, project_name: crate::ui::project::display_name(&target.project), project: target.project, folder: target.folder, uris }));
}

fn join_path(dir: &str, name: &str) -> String {
    if dir == "/" { format!("/{name}") } else if dir.is_empty() { name.to_string() } else { format!("{}/{name}", dir.trim_end_matches('/')) }
}

// Fixed pane size until WidgetView can fill a flex-assigned box (dioxus-compose
// issue 160); a CodeEditor measured with unbounded height aborts the renderer.
// TODO(dioxus-compose#160): fixed size until WidgetView fills a flex-assigned box.
const EDITOR_W: f32 = 980.0;
const EDITOR_H: f32 = 700.0;

fn pos(p: compose_rust::code::Position) -> WidgetPos { WidgetPos::new(p.line, p.column) }
fn send(s: &Shared, event: Option<(ember_editor_conn::uri::UriComponents, u64, ember_editor::WidgetEvent)>) {
    if let Some((uri, generation, event)) = event { s.host.send(HostCommand::Widget(uri, generation, event)); }
}
fn editor_pane() -> Element {
    let s = shared();
    let _ = *s.revision.read();
    let _ = s.tabs.read();
    let doc = s.bridge.borrow().active_document().cloned();
    let Some(doc) = doc else { return rsx! {}; };
    let generation = doc.generation;
    let uri = doc.uri.clone();
    let change_s = s.clone(); let change_uri = uri.clone();
    let save_s = s.clone(); let save_uri = uri.clone();
    let hover_s = s.clone(); let hover_uri = uri.clone();
    let click_s = s.clone(); let click_uri = uri.clone();
    let selection_s = s.clone(); let selection_uri = uri;
    rsx! { CodeEditor {
        key: "{generation}", width: Some(EDITOR_W), height: Some(EDITOR_H),
        text: doc.text, decorations: compose_rust::code::Decorations::from_bytes(doc.decoration_bytes), tab_width: Some(doc.tab_width),
        on_change: move |e: compose_rust::code::CodeChange| { let event = change_s.bridge.borrow_mut().on_code_changed(&change_uri, e.version, WidgetRange::new(pos(e.range.start), pos(e.range.end)), e.text); send(&change_s, event); },
        on_save: move |e: compose_rust::code::SaveRequest| { let event = save_s.bridge.borrow().on_save_requested(&save_uri, e.version); send(&save_s, event); },
        on_hover: move |e: compose_rust::code::CodeHover| { let phase = match e.phase { compose_rust::schema::HoverPhase::Rest => EditorHoverPhase::Rest, _ => EditorHoverPhase::Leave }; let event = hover_s.bridge.borrow().on_code_hovered(&hover_uri, e.decoration.unwrap_or(0), pos(e.position), phase); send(&hover_s, event); },
        on_decoration_click: move |id| { let event = click_s.bridge.borrow().on_decoration_activated(&click_uri, id); send(&click_s, event); },
        on_selection_change: move |e: compose_rust::code::SelectionChange| { let mut position = selection_s.position; position.set((e.active.line as usize + 1, e.active.column as usize + 1)); let event = selection_s.bridge.borrow().on_selection_changed(&selection_uri, e.version, pos(e.anchor), pos(e.active)); send(&selection_s, event); },
    } }
}

pub mod page {
    use dioxus_compose::html::prelude::dioxus_elements::point_interaction::ModifiersInteraction;
    use dioxus_compose::html::prelude::dioxus_elements::{Key, Modifiers};
    use dioxus_compose::html::prelude::dioxus_signals::{ReadableExt, WritableExt};
    use dioxus_compose::html::prelude::*;
    use super::{editor_pane, join_path, shared, workbench, base_name, position_text, BridgeAction, HostCommand, Shared};
    use crate::ui::editor::{
        agent_status_text, apply_action, breadcrumb_text, chord_from_key, tooltip_text, Side, Stripes, ToolWindow,
    };
    use crate::ui::keymap::{Action, Keymap, Os};
    use crate::ui::project::{project_boxes, status_pill, SOON};

    pub fn app() -> Element {
        let target = crate::services::services().ui.editor.get_untracked().expect("editor target");
        super::init(target);
        workbench_page()
    }

    /// The key's name as the keymap table writes it.
    pub(super) fn key_name(key: &Key) -> String {
        match key {
            Key::Character(c) => c.clone(),
            Key::Escape => "Esc".to_string(),
            other => format!("{other:?}"),
        }
    }

    fn notice(text: impl Into<String>) {
        crate::ui::cr::notice::post(text);
    }

    /// Do a keymap action. Closing a tab also tells the host; an action the editor cannot do
    /// says it is not ready.
    fn run_action(s: &Shared, action: Action) {
        let mut tabs = s.tabs;
        let mut stripes = s.stripes;
        let closing = if action == Action::Close { tabs.read().current_path().map(str::to_owned) } else { None };
        let handled = {
            let mut t = tabs.write();
            let mut st = stripes.write();
            apply_action(action, &mut t, &mut st)
        };
        if let Some(path) = closing.filter(|_| handled) {
            s.host.send(HostCommand::Close(path));
            let next = tabs.read().current_path().map(str::to_owned);
            if let Some(next) = next { s.host.send(HostCommand::Focus(next)); }
        }
        if !handled {
            notice(format!("{}: {SOON}", action.label()));
        }
    }

    /// The agent state and the branch of the project, from the client's rows.
    fn project_status(project: &str) -> (String, String) {
        crate::services::services().client.read(|st| {
            let found = project_boxes(st).into_iter().find(|b| b.id == project);
            let state = found
                .as_ref()
                .and_then(|b| b.top)
                .map(|t| status_pill(t).0.to_string())
                .unwrap_or_else(|| "팀원 없음".to_string());
            let branch = found.and_then(|b| b.branch).unwrap_or_else(|| "브랜치 미설정".to_string());
            (state, branch)
        })
    }

    fn glyph(w: ToolWindow) -> &'static str {
        match w {
            ToolWindow::Project => "\u{25A4}",
            ToolWindow::Commit => "\u{2713}",
            ToolWindow::Git => "\u{2442}",
            ToolWindow::Agent => "\u{2726}",
            ToolWindow::Notifications => "\u{25CE}",
            ToolWindow::Terminal => "\u{203A}_",
            ToolWindow::Run => "\u{25B6}",
            ToolWindow::Problems => "\u{26A0}",
        }
    }

    fn workbench_page() -> Element {
        let s = shared();
        let _ = *s.revision.read();
        let (entries, error, updates) = {
            let mut state = s.host.state.lock().unwrap();
            (state.entries.clone(), state.error.clone(), std::mem::take(&mut state.updates))
        };
        for update in updates {
            if let ember_editor::SessionUpdate::Opened { uri, .. } = &update { s.uris.borrow_mut().insert(uri.path.clone(), uri.clone()); }
            if let ember_editor::SessionUpdate::Closed { uri } = &update { s.uris.borrow_mut().remove(&uri.path); }
            let action = s.bridge.borrow_mut().apply(update);
            if let BridgeAction::Resync(uri) = action { s.host.send(HostCommand::Resync(uri)); }
        }
        let tabs = s.tabs;
        let current = tabs.read().current_path().map(str::to_owned);
        let paths: Vec<String> = tabs.read().paths().into_iter().map(str::to_owned).collect();
        let bridge = s.bridge.borrow();
        let uris = s.uris.borrow();
        if let Some(uri) = current.as_ref().and_then(|p| uris.get(p)) { drop(bridge); s.bridge.borrow_mut().set_active(uri); } else { drop(bridge); }
        let bridge = s.bridge.borrow();
        let active = current.as_ref().and_then(|p| uris.get(p)).and_then(|u| bridge.document(u)).cloned();
        let dirty: Vec<bool> = paths.iter().map(|p| uris.get(p).and_then(|u| bridge.document(u)).is_some_and(|d| d.dirty)).collect();
        drop(uris);
        drop(bridge);
        let (line, col) = *s.position.read();
        let dir = s.directory.read().clone();
        let sel = *s.stripes.read();
        let keymap = *s.keymap.read();
        let (agent_state, branch) = project_status(&s.project);
        let key_s = s.clone();
        rsx! {
            div { class: "monaco-workbench", tabindex: "0", style: "width:100vw; height:calc(100vh - 76px); display:flex; flex-direction:column; background:#1e1e1e; color:#cccccc; font-size:13px;",
                onkeydown: move |event| {
                    let m = event.modifiers();
                    let chord = chord_from_key(
                        &key_name(&event.key()),
                        m.contains(Modifiers::CONTROL),
                        m.contains(Modifiers::SHIFT),
                        m.contains(Modifiers::ALT),
                        m.contains(Modifiers::META),
                    );
                    let keymap = *key_s.keymap.read();
                    if let Some(action) = keymap.action_for(&chord, Os::current()) {
                        run_action(&key_s, action);
                    }
                },
                if let Some(error) = error {
                    div { style: "padding:20px;", "{error}" button { onclick: move |_| crate::services::services().ui.close_editor(), "닫기" } }
                } else {
                    {top_bar(&s, &branch, keymap)}
                    div { style: "display:flex; flex:1; min-height:0;",
                        {stripe_column(&s, sel, keymap, true)}
                        if let Some(w) = sel.selected(Side::Left) {
                            if w == ToolWindow::Project {
                                {project_tree(&s, entries, &dir)}
                            } else {
                                {side_note(w)}
                            }
                        }
                        div { style: "display:flex; flex:1; min-width:0; flex-direction:column;",
                            {tab_strip(&s, &paths, &dirty, current.as_deref())}
                            if let Some(path) = current.as_deref() {
                                {breadcrumbs(path, &s.folder)}
                            }
                            div { style: "flex:1; min-height:0; overflow:hidden;",
                                if active.is_some() { WidgetView { app: editor_pane, style: "width:980px; height:700px; flex-shrink:0;" } }
                                else { div { style: "padding:20px;", "열린 파일이 없습니다" } }
                            }
                            if let Some(w) = sel.selected(Side::Bottom) {
                                {bottom_window(&s, w)}
                            }
                        }
                        if let Some(w) = sel.selected(Side::Right) {
                            {side_note(w)}
                        }
                        {stripe_column(&s, sel, keymap, false)}
                    }
                    {status_bar(&s, &agent_state, &branch, active.is_some(), line, col)}
                }
            }
        }
    }

    fn top_bar(s: &Shared, branch: &str, keymap: Keymap) -> Element {
        let mut menu_open = s.menu_open;
        let name = s.project_name.clone();
        let branch = branch.to_string();
        let find = tooltip_text(keymap.find_label(), &keymap.hint(Action::Find, Os::current()));
        let find_note = format!("{}: {SOON}", keymap.find_label());
        let btn = "height:32px; padding:0 12px; display:flex; align-items:center; border-radius:16px; border:1px solid #333333; background:#1e1e1e; color:#cccccc; cursor:pointer;";
        let open = *menu_open.read();
        rsx! {
            div { style: "position:relative; height:48px; flex-shrink:0; display:flex; align-items:center; gap:8px; padding:0 8px; background:#252526; border-bottom:1px solid #333333;",
                div { style: "{btn}", onclick: move |_| crate::services::services().ui.close_editor(), "닫기" }
                span { style: "font-weight:600;", "{name}" }
                span { style: "color:#9d9d9d;", "{branch}" }
                div { style: "flex:1;" }
                div { style: "{btn}", title: "{find}", onclick: move |_| notice(find_note.clone()), "\u{2315} {find}" }
                div { style: "{btn}", onclick: move |_| notice(format!("실행: {SOON}")), "\u{25B6} 실행" }
                div { style: "{btn}", onclick: move |_| menu_open.set(!open), "\u{22EF}" }
                if open { {menu(s, keymap)} }
            }
        }
    }

    fn menu(s: &Shared, keymap: Keymap) -> Element {
        let os = Os::current();
        let item = "padding:8px 12px; cursor:pointer;";
        rsx! {
            div { style: "position:absolute; right:8px; top:46px; width:280px; z-index:10; background:#252526; border:1px solid #333333; border-radius:8px; padding:4px 0;",
                for a in [Action::Save, Action::Recent, Action::Format, Action::GotoLine, Action::Palette] {
                    {
                        let s = s.clone();
                        let label = if a == Action::Palette { keymap.palette_label() } else { a.label() };
                        let text = tooltip_text(label, &keymap.hint(a, os));
                        let mut menu_open = s.menu_open;
                        rsx! { div { key: "{a.label()}", style: "{item}", onclick: move |_| { menu_open.set(false); run_action(&s, a); }, "{text}" } }
                    }
                }
                div { style: "padding:8px 12px; color:#858585; font-size:11px;", "단축키 체계" }
                for k in Keymap::ALL {
                    {
                        let mut menu_open = s.menu_open;
                        let mut keymap_sig = s.keymap;
                        let mark = if k == keymap { "\u{2713}" } else { "\u{2003}" };
                        let text = format!("{mark} {}  {}", k.label(), k.hint(Action::Find, os));
                        rsx! { div { key: "{k.label()}", style: "{item}", onclick: move |_| {
                            menu_open.set(false);
                            keymap_sig.set(k);
                            crate::services::services().ui.update_prefs(|p| p.keymap = k);
                        }, "{text}" } }
                    }
                }
            }
        }
    }

    fn stripe_button(s: &Shared, w: ToolWindow, selected: bool, keymap: Keymap) -> Element {
        let mut stripes = s.stripes;
        let hint = w.action().map(|a| keymap.hint(a, Os::current())).unwrap_or_default();
        let tip = tooltip_text(w.label(), &hint);
        let look = if selected { "background:#37373d; color:#ffffff;" } else { "color:#9d9d9d;" };
        rsx! {
            div { key: "{w.label()}", title: "{tip}", style: "width:40px; height:40px; display:flex; align-items:center; justify-content:center; cursor:pointer; border-radius:4px; font-size:18px; {look}",
                onclick: move |_| stripes.write().toggle(w), "{glyph(w)}" }
        }
    }

    fn stripe_column(s: &Shared, sel: Stripes, keymap: Keymap, left: bool) -> Element {
        rsx! {
            div { style: "width:40px; flex-shrink:0; display:flex; flex-direction:column; align-items:center; gap:2px; padding-top:4px; background:#252526;",
                if left {
                    for w in ToolWindow::LEFT { {stripe_button(s, w, sel.is_selected(w), keymap)} }
                    div { style: "flex:1;" }
                    for w in ToolWindow::BOTTOM { {stripe_button(s, w, sel.is_selected(w), keymap)} }
                } else {
                    for w in ToolWindow::RIGHT { {stripe_button(s, w, sel.is_selected(w), keymap)} }
                }
            }
        }
    }

    fn side_note(w: ToolWindow) -> Element {
        rsx! {
            div { style: "width:260px; flex-shrink:0; padding:12px; background:#252526;",
                div { style: "font-weight:600;", "{w.label()}" }
                div { style: "color:#9d9d9d; padding-top:6px;", "{SOON}" }
                div { style: "color:#858585; padding-top:6px; font-size:11px;", "서버가 알려 주면 이곳에 표시됩니다." }
            }
        }
    }

    fn project_tree(s: &Shared, entries: Vec<(String, u32)>, dir: &str) -> Element {
        let mut tabs = s.tabs;
        let mut directory = s.directory;
        rsx! {
            div { class: "customview-tree", style: "width:260px; flex-shrink:0; overflow:auto; background:#252526;",
                div { style: "padding:10px;", "탐색기" }
                div { style: "padding:8px;", "{dir}" }
                for (name, kind) in entries {
                    {
                        let path = join_path(dir, &name);
                        let host = s.host.clone();
                        let is_dir = kind & ember_editor_conn::remote_fs::file_type::DIRECTORY != 0;
                        rsx! { div { class: workbench::tree_row_classes(false, false), style: "height:28px; padding-left:12px; cursor:pointer;", onclick: move |_| {
                            if is_dir { directory.set(path.clone()); host.send(HostCommand::List(path.clone())); }
                            else { tabs.write().open(&path); host.send(HostCommand::Open(path.clone())); }
                        }, if is_dir { "▸ " } "{name}" } }
                    }
                }
            }
        }
    }

    fn tab_strip(s: &Shared, paths: &[String], dirty: &[bool], current: Option<&str>) -> Element {
        let mut tabs = s.tabs;
        rsx! {
            div { style: "display:flex; height:36px; flex-shrink:0; background:#252526;",
                for (i, path) in paths.iter().enumerate() {
                    {
                        let path = path.clone(); let close_path = path.clone();
                        let host = s.host.clone(); let close_host = s.host.clone();
                        let selected = current == Some(path.as_str());
                        let label = base_name(&path).to_string();
                        let mark = if dirty[i] { " ●" } else { "" };
                        let style = if selected { "padding:8px; background:#1e1e1e; cursor:pointer;" } else { "padding:8px; cursor:pointer;" };
                        rsx! { div { style, onclick: move |_| {
                            tabs.write().activate(&path);
                            host.send(HostCommand::Focus(path.clone()));
                        }, "{label}{mark}" span { style: "padding-left:10px;", onclick: move |event| { event.stop_propagation(); tabs.write().close(&close_path); close_host.send(HostCommand::Close(close_path.clone())); if let Some(next) = tabs.read().current_path() { close_host.send(HostCommand::Focus(next.to_string())); } }, "×" } } }
                    }
                }
            }
        }
    }

    fn breadcrumbs(path: &str, folder: &str) -> Element {
        let text = breadcrumb_text(path, folder);
        rsx! { div { style: "height:24px; flex-shrink:0; padding:0 12px; display:flex; align-items:center; color:#9d9d9d; font-size:12px; background:#1e1e1e;", "{text}" } }
    }

    fn bottom_window(s: &Shared, w: ToolWindow) -> Element {
        let mut stripes = s.stripes;
        let body = match w {
            ToolWindow::Terminal => "노드의 터미널에 연결되어 있지 않습니다. 연결되면 이곳에 터미널이 표시됩니다.",
            ToolWindow::Run => "실행 구성이 없습니다. 구성이 연결되면 이곳에 출력이 표시됩니다.",
            _ => "진단 정보가 아직 연결되지 않았습니다. 연결되면 이곳에 문제가 표시됩니다.",
        };
        rsx! {
            div { style: "height:200px; flex-shrink:0; display:flex; flex-direction:column; background:#252526; border-top:1px solid #333333;",
                div { style: "display:flex; align-items:center; gap:4px; padding:4px 8px;",
                    for b in ToolWindow::BOTTOM {
                        {
                            let look = if b == w { "background:#cccccc; color:#1e1e1e;" } else { "color:#9d9d9d;" };
                            rsx! { div { key: "{b.label()}", style: "height:28px; padding:0 14px; display:flex; align-items:center; border-radius:14px; cursor:pointer; {look}",
                                onclick: move |_| { if !stripes.read().is_selected(b) { stripes.write().toggle(b); } }, "{b.label()}" } }
                        }
                    }
                    div { style: "flex:1;" }
                    div { style: "width:28px; height:28px; display:flex; align-items:center; justify-content:center; cursor:pointer;",
                        title: "숨기기", onclick: move |_| stripes.write().hide(Side::Bottom), "\u{2304}" }
                }
                div { style: "padding:12px; color:#9d9d9d; overflow:auto;", "{body}" }
            }
        }
    }

    fn status_bar(s: &Shared, agent_state: &str, branch: &str, has_file: bool, line: usize, col: usize) -> Element {
        let folder = s.folder.clone();
        let agent = agent_status_text(agent_state);
        let position = position_text(line, col);
        let branch = branch.to_string();
        rsx! {
            div { style: "height:24px; flex-shrink:0; display:flex; align-items:center; gap:16px; padding:0 8px; background:#007acc; color:white;",
                span { "{folder}" }
                div { style: "flex:1;" }
                span { "{agent}" }
                if has_file {
                    span { "{position}" }
                    span { "LF" }
                    span { "UTF-8" }
                }
                span { "{branch}" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn fr_e1_tabs_activate_and_close() { let mut t = EditorTabs::default(); t.open("a"); t.open("b"); t.activate("a"); t.close("a"); assert_eq!(t.current_path(), Some("b")); }
    #[test] fn fr_l31_join_paths() { assert_eq!(join_path("/", "a"), "/a"); assert_eq!(join_path("/x/", "a"), "/x/a"); assert_eq!(join_path("", "a"), "a"); }
    #[test] fn fr_l31_breadcrumbs_strip_the_folder_and_join_with_a_quote() {
        use crate::ui::editor::{breadcrumb_segments, breadcrumb_text};
        assert_eq!(breadcrumb_segments("/w/p/src/lib.rs", "/w/p"), vec!["src", "lib.rs"]);
        assert_eq!(breadcrumb_segments("src/lib.rs", ""), vec!["src", "lib.rs"]);
        assert_eq!(breadcrumb_segments("/w/pp/a.rs", "/w/p"), vec!["w", "pp", "a.rs"]);
        assert_eq!(breadcrumb_text("/w/p/src/lib.rs", "/w/p/"), "src \u{203A} lib.rs");
    }
    #[test] fn fr_l31_status_text_and_chords() {
        use crate::ui::editor::{agent_status_text, chord_from_key};
        use crate::ui::keymap::{Action, Keymap, Os};
        assert_eq!(agent_status_text("대기"), "에이전트 대기");
        let chord = chord_from_key("s", true, false, false, false);
        assert_eq!(Keymap::VsCode.action_for(&chord, Os::Other), Some(Action::Save));
        assert_eq!(page::key_name(&dioxus_compose::html::prelude::dioxus_elements::Key::Escape), "Esc");
    }
    #[test] fn fr_l31_status_position() { assert_eq!(position_text(2, 3), "2행 3열"); }
}
