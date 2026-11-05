//! Remote editor workbench.
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use dioxus_compose::prelude::*;
use ember_editor::{BridgeAction, WidgetBridge, WidgetPos, WidgetRange};
use ember_editor::widget::HoverPhase as EditorHoverPhase;
use crate::editor_host::{EditorHost, HostCommand};
use super::{editor::{EditorTarget, EditorTabs, base_name, position_text}, use_ui, workbench};

#[derive(Clone)]
struct Shared {
    host: EditorHost,
    bridge: Rc<RefCell<WidgetBridge>>,
    revision: SyncSignal<u64>,
    tabs: Signal<EditorTabs>,
    directory: Signal<String>,
    position: Signal<(usize, usize)>,
    folder: String,
    ui: super::Ui,
    uris: Rc<RefCell<HashMap<String, ember_editor_conn::uri::UriComponents>>>,
}

// WidgetView accepts a function without props and owns a separate tree. Share signals between
// the trees until WidgetView supports props and flex stretch sizing (dioxus-compose #160).
thread_local! { static SHARED: RefCell<Option<Shared>> = const { RefCell::new(None) }; }
fn shared() -> Shared { SHARED.with(|s| s.borrow().as_ref().expect("workbench initialized").clone()) }

pub fn screen(target: EditorTarget) -> Element {
    let ui = use_ui();
    let revision = use_signal_sync(|| 0_u64);
    let host = use_hook(|| EditorHost::start(target.clone(), revision));
    let tabs = use_signal(EditorTabs::default);
    let directory = use_signal(|| target.folder.clone());
    let position = use_signal(|| (1, 1));
    let bridge = use_hook(|| Rc::new(RefCell::new(WidgetBridge::new())));
    let uris = use_hook(|| Rc::new(RefCell::new(HashMap::new())));
    SHARED.with(|s| *s.borrow_mut() = Some(Shared { host, bridge, revision, tabs, directory, position, folder: target.folder, ui, uris }));
    rsx! { HtmlView { app: page::workbench_page, config: Some(workbench::config as fn() -> dioxus_compose::html::HtmlConfig), fill_max_width: true, fill_max_height: true } }
}

fn join_path(dir: &str, name: &str) -> String {
    if dir == "/" { format!("/{name}") } else if dir.is_empty() { name.to_string() } else { format!("{}/{name}", dir.trim_end_matches('/')) }
}

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
        key: "{generation}", fill_max_width: true, fill_max_height: true,
        text: doc.text, decorations: compose_rust::code::Decorations::from_bytes(doc.decoration_bytes), tab_width: Some(doc.tab_width),
        on_change: move |e: compose_rust::code::CodeChange| { let event = change_s.bridge.borrow_mut().on_code_changed(&change_uri, e.version, WidgetRange::new(pos(e.range.start), pos(e.range.end)), e.text); send(&change_s, event); },
        on_save: move |e: compose_rust::code::SaveRequest| { let event = save_s.bridge.borrow().on_save_requested(&save_uri, e.version); send(&save_s, event); },
        on_hover: move |e: compose_rust::code::CodeHover| { let phase = match e.phase { compose_rust::schema::HoverPhase::Rest => EditorHoverPhase::Rest, _ => EditorHoverPhase::Leave }; let event = hover_s.bridge.borrow().on_code_hovered(&hover_uri, e.decoration.unwrap_or(0), pos(e.position), phase); send(&hover_s, event); },
        on_decoration_click: move |id| { let event = click_s.bridge.borrow().on_decoration_activated(&click_uri, id); send(&click_s, event); },
        on_selection_change: move |e: compose_rust::code::SelectionChange| { let mut position = selection_s.position; position.set((e.active.line as usize + 1, e.active.column as usize + 1)); let event = selection_s.bridge.borrow().on_selection_changed(&selection_uri, e.version, pos(e.anchor), pos(e.active)); send(&selection_s, event); },
    } }
}

mod page {
    use dioxus_compose::html::prelude::dioxus_signals::{ReadableExt, WritableExt};
    use dioxus_compose::html::prelude::*;
    use super::{editor_pane, join_path, shared, workbench, base_name, position_text, BridgeAction, HostCommand};

pub fn workbench_page() -> Element {
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
    let mut tabs = s.tabs;
    let mut directory = s.directory;
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
    let ui = s.ui;
    let status = position_text(line, col);
    let folder = s.folder.clone();
    let dir = directory.read().clone();
    rsx! {
        div { class: "monaco-workbench", style: "display:flex; flex-direction:column; width:100%; height:100%; background:#1e1e1e; color:#cccccc;",
            if let Some(error) = error {
                div { style: "padding:20px;", "{error}" button { onclick: move |_| ui.close_editor(), "닫기" } }
            } else {
                div { style: "display:flex; flex:1; min-height:0;",
                    div { class: "customview-tree", style: "width:260px; flex-shrink:0; overflow:auto; background:#252526;",
                        div { style: "padding:10px;", "탐색기" }
                        div { style: "padding:8px;", "{dir}" }
                        for (name, kind) in entries {
                            {
                                let path = join_path(&dir, &name);
                                let host = s.host.clone();
                                let is_dir = kind & ember_editor_conn::remote_fs::file_type::DIRECTORY != 0;
                                rsx! { div { class: workbench::tree_row_classes(false, false), style: "height:28px; padding-left:12px; cursor:pointer;", onclick: move |_| {
                                    if is_dir { directory.set(path.clone()); host.send(HostCommand::List(path.clone())); }
                                    else { tabs.write().open(&path); host.send(HostCommand::Open(path.clone())); }
                                }, if is_dir { "▸ " } "{name}" } }
                            }
                        }
                    }
                    div { style: "display:flex; flex:1; min-width:0; flex-direction:column;",
                        div { style: "display:flex; height:36px; background:#252526;",
                            for (i, path) in paths.iter().enumerate() {
                                {
                                    let path = path.clone(); let close_path = path.clone();
                                    let host = s.host.clone(); let close_host = s.host.clone();
                                    let selected = current.as_deref() == Some(path.as_str());
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
                        if active.is_some() { WidgetView { app: editor_pane, style: "flex:1; min-width:0; height:100%" } }
                        else { div { style: "padding:20px;", "열린 파일이 없습니다" } }
                    }
                }
                div { style: "height:24px; display:flex; justify-content:space-between; background:#007acc; color:white;", span { "{folder}" } span { "{status}" } }
            }
        }
    }
}

}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn fr_e1_tabs_activate_and_close() { let mut t = EditorTabs::default(); t.open("a"); t.open("b"); t.activate("a"); t.close("a"); assert_eq!(t.current_path(), Some("b")); }
    #[test] fn fr_l31_join_paths() { assert_eq!(join_path("/", "a"), "/a"); assert_eq!(join_path("/x/", "a"), "/x/a"); assert_eq!(join_path("", "a"), "a"); }
    #[test] fn fr_l31_status_position() { assert_eq!(position_text(2, 3), "2행 3열"); }
}
