//! Remote editor workbench.
use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc};
use dioxus_compose::prelude::*;
use ember_editor::{BridgeAction, WidgetBridge, WidgetPos, WidgetRange};
use ember_editor::widget::HoverPhase as EditorHoverPhase;
use crate::editor_host::{EditorHost, HostCommand};
use super::{editor::{EditorTarget, EditorTabs, Stripes, base_name, position_text}, keymap::Keymap, workbench};

/// The find bar's state: whether it is open, what is typed, the options, which match the reader is on,
/// and the command the editor pane carries out next (a selection to show, or replacements to make).
#[derive(Clone, Copy)]
pub struct FindState {
    pub open: Signal<bool>,
    pub query: Signal<String>,
    pub replacement: Signal<String>,
    pub case_sensitive: Signal<bool>,
    pub whole_word: Signal<bool>,
    /// 정규식, 대소문자 보존 and 선택 영역에서 찾기 (FR-L31.12).
    pub regex: Signal<bool>,
    pub preserve_case: Signal<bool>,
    pub scope: Signal<Option<crate::ui::find::Scope>>,
    pub in_selection: Signal<bool>,
    /// The editor's last selection, anchor and active end, each (line, UTF-16 column).
    pub selection: Signal<((u32, u32), (u32, u32))>,
    pub current: Signal<Option<usize>>,
    pub cmd: Signal<Option<FindCmd>>,
    pub next_id: Signal<u64>,
    pub focus_seq: Signal<u64>,
}

/// One thing asked of the editor by the find bar. `id` is new for every request, so the pane does each once.
#[derive(Clone, Debug, PartialEq)]
pub struct FindCmd {
    pub id: u64,
    /// The match to select and bring into view.
    pub goto: Option<crate::ui::find::Match>,
    /// Replacements, last first, each written against the same document version.
    pub replacements: Vec<(crate::ui::find::Match, String)>,
}

/// A pending cursor / caret move target requested by the problems list or a goto command.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingGoto {
    pub id: u64,
    pub path: String,
    pub line: u32,
    pub col: u32,
}

#[derive(Clone)]
pub struct Shared {
    pub host: EditorHost,
    pub bridge: Rc<RefCell<WidgetBridge>>,
    pub revision: SyncSignal<u64>,
    pub tabs: Signal<EditorTabs>,
    pub directory: Signal<String>,
    pub expanded: Signal<std::collections::HashSet<String>>,
    pub bottom_max: Signal<bool>,
    pub tree_sel: Signal<Option<(String, bool)>>,
    pub tree_multi_sel: Signal<Vec<(String, bool)>>,
    pub tree_clipboard: Signal<Option<crate::ui::editor::TreeClipboard>>,
    pub tree_drag_src: Signal<Option<String>>,
    pub prompt: Signal<Option<crate::ui::editor::TreePrompt>>,
    pub prompt_text: Signal<String>,
    pub prompt_at: Signal<u64>,
    pub palette_open: Signal<bool>,
    pub find: FindState,
    pub palette_mode: Signal<crate::ui::palette::Mode>,
    pub palette_query: Signal<String>,
    pub palette_sel: Signal<usize>,
    pub recent_files: Signal<Vec<String>>,
    pub theme: Signal<crate::ui::theme::ThemePref>,
    pub position: Signal<(usize, usize)>,
    pub stripes: Signal<Stripes>,
    pub menu_open: Signal<bool>,
    /// The tab whose context menu is open, and where (client x, y) (FR-L31.5).
    pub tab_menu: Signal<Option<(String, f64, f64)>>,
    /// The unsaved-changes question on a close, while it is open (FR-L31.5).
    pub close_confirm: Signal<Option<crate::ui::editor::CloseRequest>>,
    pub keymap: Signal<Keymap>,
    pub folder: String,
    pub server_url: String,
    pub computer: String,
    pub project: String,
    pub project_name: String,
    pub uris: Rc<RefCell<HashMap<String, ember_editor_conn::uri::UriComponents>>>,
    /// Why a file could not be opened, by path: the pane says so instead of looking like nothing is open.
    pub open_errors: Rc<RefCell<HashMap<String, String>>>,
    pub editor_versions: Rc<RefCell<HashMap<u64, u32>>>,
    pub last_find_cmd: Rc<RefCell<u64>>,
    pub pending_goto: Signal<Option<PendingGoto>>,
    pub next_goto_id: Signal<u64>,
    pub last_goto_id: Rc<RefCell<u64>>,
    pub problems_limit: Signal<usize>,
    pub selected_output_channel: Signal<String>,
    pub workbench_logs: Signal<Vec<String>>,
    /// Source control of the 커밋 and Git windows (FR-L31.2).
    pub scm: crate::ui::editor_scm::Scm,
    /// Extension notifications: toasts, the center and modal dialogs (FR-L31.16).
    pub notifications: Rc<RefCell<ember_editor::notifications::Notifications>>,
    /// The 확장 window: search, install, uninstall, enable and disable (FR-L31.14).
    pub extensions: crate::ui::editor_extensions::Extensions,
}

// WidgetView accepts a function without props and owns a separate tree. Share signals between
// the trees until WidgetView supports props and flex stretch sizing (dioxus-compose #160).
thread_local! { static DOUBLE_SHIFT: RefCell<super::editor::DoubleShift> = RefCell::new(super::editor::DoubleShift::default()); }
// v0.0.7: #417 fixed; workaround kept until checked on a display (dioxus-compose has to forward compose-rust's pointer events as oncontextmenu and ondoubleclick; its pin moves separately): tells a double-click from two `onclick`s.
thread_local! { static DOUBLE_CLICK: RefCell<super::editor::DoubleClick> = RefCell::new(super::editor::DoubleClick::default()); }

/// Milliseconds on a clock that only moves forward, for telling two Shift presses apart.
pub(crate) fn now_ms() -> u64 {
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
}

thread_local! { static ICON_DARK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
thread_local! { static MODIFIER_CLICK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
thread_local! { static SHARED: RefCell<Option<Shared>> = const { RefCell::new(None) }; }
pub(crate) fn shared() -> Shared { SHARED.with(|s| s.borrow().as_ref().expect("workbench initialized").clone()) }

/// Sets up the shared state for the page. Called first by the root page.
fn init(target: EditorTarget) {
    // Signals live in the root scope: the page and the editor pane are separate trees that read them.
    let root = dioxus_compose::html::prelude::dioxus_core::ScopeId::ROOT;
    let revision = use_hook(|| SyncSignal::<u64>::new_maybe_sync_in_scope(0, root));
    let host = use_hook(|| EditorHost::start(target.clone(), revision));
    let tabs = use_hook(|| Signal::new_in_scope(EditorTabs::default(), root));
    let directory = use_hook(|| Signal::new_in_scope(target.folder.clone(), root));
    let expanded = use_hook(|| Signal::new_in_scope(std::collections::HashSet::<String>::new(), root));
    let theme = use_hook(|| Signal::new_in_scope(crate::services::services().ui.prefs.get_untracked().theme, root));
    let tree_sel = use_hook(|| Signal::new_in_scope(None::<(String, bool)>, root));
    let tree_multi_sel = use_hook(|| Signal::new_in_scope(Vec::<(String, bool)>::new(), root));
    let tree_clipboard = use_hook(|| Signal::new_in_scope(None::<crate::ui::editor::TreeClipboard>, root));
    let tree_drag_src = use_hook(|| Signal::new_in_scope(None::<String>, root));
    let prompt = use_hook(|| Signal::new_in_scope(None::<crate::ui::editor::TreePrompt>, root));
    let palette_open = use_hook(|| Signal::new_in_scope(false, root));
    let find = use_hook(|| FindState {
        open: Signal::new_in_scope(false, root),
        query: Signal::new_in_scope(String::new(), root),
        replacement: Signal::new_in_scope(String::new(), root),
        case_sensitive: Signal::new_in_scope(false, root),
        whole_word: Signal::new_in_scope(false, root),
        regex: Signal::new_in_scope(false, root),
        preserve_case: Signal::new_in_scope(false, root),
        scope: Signal::new_in_scope(None, root),
        in_selection: Signal::new_in_scope(false, root),
        selection: Signal::new_in_scope(((0, 0), (0, 0)), root),
        current: Signal::new_in_scope(None, root),
        cmd: Signal::new_in_scope(None, root),
        next_id: Signal::new_in_scope(1, root),
        focus_seq: Signal::new_in_scope(0, root),
    });
    let palette_mode = use_hook(|| Signal::new_in_scope(crate::ui::palette::Mode::Commands, root));
    let palette_query = use_hook(|| Signal::new_in_scope(String::new(), root));
    let palette_sel = use_hook(|| Signal::new_in_scope(0usize, root));
    let recent_files = use_hook(|| Signal::new_in_scope(Vec::<String>::new(), root));
    let prompt_at = use_hook(|| Signal::new_in_scope(0u64, root));
    let prompt_text = use_hook(|| Signal::new_in_scope(String::new(), root));
    let bottom_max = use_hook(|| Signal::new_in_scope(false, root));
    let position = use_hook(|| Signal::new_in_scope((1, 1), root));
    let stripes = use_hook(|| {
        let mut initial = Stripes::default();
        // Debug capture hook: start with the 확장 window showing (FR-L31.14).
        if crate::ui::editor_extensions::capture_hook().is_some() {
            initial.toggle(crate::ui::editor::ToolWindow::Extensions);
        }
        Signal::new_in_scope(initial, root)
    });
    let menu_open = use_hook(|| Signal::new_in_scope(false, root));
    let tab_menu = use_hook(|| Signal::new_in_scope(None::<(String, f64, f64)>, root));
    let close_confirm = use_hook(|| Signal::new_in_scope(None::<crate::ui::editor::CloseRequest>, root));
    let keymap = use_hook(|| Signal::new_in_scope(crate::services::services().ui.prefs.get_untracked().keymap, root));
    // The saved zoom (Code-OSS's window.zoomLevel) applies when the window opens (FR-L31.15).
    use_hook(|| compose_rust::zoom::set_zoom_level(crate::services::services().ui.prefs.get_untracked().editor_zoom_level));
    let bridge = use_hook(|| Rc::new(RefCell::new(WidgetBridge::new())));
    let uris = use_hook(|| Rc::new(RefCell::new(HashMap::new())));
    let open_errors = use_hook(|| Rc::new(RefCell::new(HashMap::new())));
    let editor_versions = use_hook(|| Rc::new(RefCell::new(HashMap::new())));
    let last_find_cmd = use_hook(|| Rc::new(RefCell::new(0u64)));
    let pending_goto = use_hook(|| Signal::new_in_scope(None::<PendingGoto>, root));
    let next_goto_id = use_hook(|| Signal::new_in_scope(1u64, root));
    let last_goto_id = use_hook(|| Rc::new(RefCell::new(0u64)));
    let problems_limit = use_hook(|| Signal::new_in_scope(crate::ui::editor::MAX_VISIBLE_PROBLEMS, root));
    let selected_output_channel = use_hook(|| Signal::new_in_scope("ember-workbench".to_string(), root));
    let workbench_logs = use_hook(|| Signal::new_in_scope(Vec::<String>::new(), root));
    // The branch in the bars comes from the first status read, so it starts with the window.
    let scm = use_hook(|| { let scm = crate::ui::editor_scm::Scm::new(&target.project, revision); scm.refresh(); scm });
    let notifications = use_hook(|| {
        let mut n = ember_editor::notifications::Notifications::new();
        // Debug-only capture hook (FR-L31.16): seeds toasts and a modal so a builder can capture them
        // without an extension that calls `$showMessage`. Ids above 1<<40 never match a real call.
        if cfg!(debug_assertions) && std::env::var("EMBER_EDITOR_DEMO_NOTIFICATIONS").is_ok_and(|v| v == "1") {
            use ember_editor::{ExtensionMessage, MessageAction, MessageSeverity};
            let act = |t: &str, h: i64, c: bool| MessageAction { title: t.into(), handle: h, is_close_affordance: c };
            let base = 1u64 << 40;
            let demo = [
                (MessageSeverity::Info, "Git 저장소를 찾았습니다.", vec![], false),
                (MessageSeverity::Warning, "rust-analyzer가 Cargo.toml을 다시 읽어야 합니다.", vec![act("다시 읽기", 0, false), act("나중에", 1, false)], false),
                (MessageSeverity::Error, "언어 서버가 종료되었습니다.", vec![act("다시 시작", 0, false)], false),
            ];
            for (i, (severity, message, actions, modal)) in demo.into_iter().enumerate() {
                n.push(ExtensionMessage { id: base + i as u64, severity, message: message.into(), source: Some("rust-analyzer".into()), actions, modal, detail: None }, now_ms());
            }
            if std::env::var("EMBER_EDITOR_DEMO_MODAL").is_ok_and(|v| v == "1") {
                n.push(ExtensionMessage { id: base + 9, severity: MessageSeverity::Warning, message: "변경 내용을 버리시겠습니까?".into(), source: Some("Git".into()), actions: vec![act("버리기", 0, false), act("유지", 1, true)], modal: true, detail: Some("이 작업은 되돌릴 수 없습니다.".into()) }, now_ms());
            }
        }
        Rc::new(RefCell::new(n))
    });
    let extensions = use_hook(|| crate::ui::editor_extensions::Extensions::new(&target.computer, revision));
    SHARED.with(|s| *s.borrow_mut() = Some(Shared { extensions, host, bridge, revision, tabs, directory, expanded, bottom_max, tree_sel, tree_multi_sel, tree_clipboard, tree_drag_src, prompt, prompt_text, prompt_at, palette_open, find, palette_mode, palette_query, palette_sel, recent_files, theme, position, stripes, menu_open, tab_menu, close_confirm, keymap, project_name: crate::ui::project::display_name(&target.project), project: target.project, folder: target.folder, server_url: target.server_url, computer: target.computer, uris, open_errors, editor_versions, last_find_cmd, pending_goto, next_goto_id, last_goto_id, problems_limit, selected_output_channel, workbench_logs, scm, notifications }));
}

pub(crate) fn join_path(dir: &str, name: &str) -> String {
    if dir == "/" { format!("/{name}") } else if dir.is_empty() { name.to_string() } else { format!("{}/{name}", dir.trim_end_matches('/')) }
}

/// The open files whose edits are not saved.
fn unsaved_open_paths(s: &Shared) -> Vec<String> {
    let bridge = s.bridge.borrow();
    let uris = s.uris.borrow();
    let mut paths: Vec<String> = s.tabs.read().paths().into_iter().map(str::to_owned).collect();
    // A file edited in a diff without a tab of its own counts too (FR-L31.10).
    if let Some(rel) = s.scm.working_diff_path() {
        let abs = join_path(&s.folder, &rel);
        if !paths.contains(&abs) {
            paths.push(abs);
        }
    }
    paths.retain(|p| uris.get(p).and_then(|u| bridge.document(u)).is_some_and(|d| d.dirty));
    paths
}

/// An edit makes a preview tab permanent, as in Code-OSS (FR-L31.5). The tab signal is written only when the
/// tab is a preview, so typing does not redraw the strip on every key.
fn keep_edited_tab(s: &Shared, path: &str) {
    let preview = s.tabs.read().is_preview(path);
    if preview {
        let mut tabs = s.tabs;
        tabs.write().keep_open(path);
    }
}

fn pos(p: compose_rust::code::Position) -> WidgetPos { WidgetPos::new(p.line, p.column) }
fn send(s: &Shared, event: Option<(ember_editor_conn::uri::UriComponents, u64, ember_editor::WidgetEvent)>) {
    if let Some((uri, generation, event)) = event { s.host.send(HostCommand::Widget(uri, generation, event)); }
}
fn editor_pane() -> Element {
    let s = shared();
    // The editor fills the box the page's layout gave this tree. Until the first layout the
    // box is empty, and a CodeEditor measured without a limit would abort the renderer.
    let (box_w, box_h) = dioxus_compose::use_widget_box();
    if box_w <= 0.0 || box_h <= 0.0 {
        return rsx! {};
    }
    let _ = *s.revision.read();
    let _ = s.tabs.read();
    let doc = s.bridge.borrow().active_document().cloned();
    let Some(doc) = doc else { return rsx! {}; };
    let generation = doc.generation;
    let uri = doc.uri.clone();
    let handle = dioxus_compose::use_code_editor();
    let version = {
        let mut map = s.editor_versions.borrow_mut();
        let current = map.entry(generation).or_insert(doc.version);
        *current = (*current).max(doc.version);
        *current
    };
    if let Some(cmd) = s.find.cmd.read().clone() {
        let mut last_cmd = s.last_find_cmd.borrow_mut();
        if *last_cmd != cmd.id {
            *last_cmd = cmd.id;
            use compose_rust::code::{CodeRange, Position};
            if let Some(m) = cmd.goto {
                handle.select(version, Position::new(m.line, m.start), Position::new(m.line, m.end));
                handle.reveal(version, CodeRange::of(m.line, m.start, m.line, m.end), compose_rust::schema::RevealAlign::Center);
            }
            for (i, (m, text)) in cmd.replacements.iter().enumerate() {
                handle.edit(i as u32 + 1, version, CodeRange::of(m.line, m.start, m.line, m.end), text.clone());
            }
        }
    }
    if let Some(goto) = s.pending_goto.read().clone() {
        if goto.path == doc.uri.path {
            let mut last_goto = s.last_goto_id.borrow_mut();
            if *last_goto != goto.id {
                *last_goto = goto.id;
                use compose_rust::code::{CodeRange, Position};
                let pos = Position::new(goto.line, goto.col);
                handle.select(version, pos, pos);
                handle.reveal(version, CodeRange::of(goto.line, goto.col, goto.line, goto.col), compose_rust::schema::RevealAlign::Center);
                let mut pos_sig = s.position;
                pos_sig.set((goto.line as usize + 1, goto.col as usize + 1));
            }
        }
    }
    // Edits the session asks for (an accepted completion, formatting, a rename, a code action) go to
    // the widget, which applies them as the reader's own and reports them back as changes.
    for command in s.bridge.borrow_mut().take_pending_edits(&doc.uri) {
        if let ember_editor::WidgetCommand::EditCode { request_id, base_version, range, text } = command {
            use compose_rust::code::CodeRange;
            handle.edit(request_id, base_version, CodeRange::of(range.start.line, range.start.col, range.end.line, range.end.col), text);
        }
    }
    let gutter_decorations = {
        let current_path = s.tabs.read().current_path().map(str::to_string);
        if let Some(abs_path) = current_path {
            s.scm.gutter_decorations(&s.folder, &abs_path, &doc.text, version)
        } else {
            compose_rust::code::GutterDecorations::default()
        }
    };
    let change_s = s.clone(); let change_uri = uri.clone();
    let save_s = s.clone(); let save_uri = uri.clone();
    let hover_s = s.clone(); let hover_uri = uri.clone();
    let click_s = s.clone(); let click_uri = uri.clone();
    let fold_s = s.clone(); let fold_uri = uri.clone();
    let selection_s = s.clone(); let selection_uri = uri;
    rsx! { CodeEditor {
        key: "{generation}", width: Some(box_w), height: Some(box_h), handle,
        text: doc.text, decorations: compose_rust::code::Decorations::from_bytes(doc.decoration_bytes), syntax_spans: crate::ui::editor::syntax_spans(&doc.syntax), folds: crate::ui::editor::folds(&doc.folds), tab_width: Some(doc.tab_width),
        gutter_decorations,
        on_change: move |e: compose_rust::code::CodeChange| { keep_edited_tab(&change_s, &change_uri.path); change_s.editor_versions.borrow_mut().insert(generation, e.version); let event = change_s.bridge.borrow_mut().on_code_changed(&change_uri, e.version, WidgetRange::new(pos(e.range.start), pos(e.range.end)), e.text); send(&change_s, event); crate::ui::editor_states::view::edited(&change_s, &change_uri.path); },
        on_save: move |e: compose_rust::code::SaveRequest| { if !crate::ui::editor_states::view::manual_save_allowed(&save_s, &save_uri.path) { return; } let event = save_s.bridge.borrow().on_save_requested(&save_uri, e.version); send(&save_s, event); save_s.scm.refresh(); },
        on_hover: move |e: compose_rust::code::CodeHover| { let phase = match e.phase { compose_rust::schema::HoverPhase::Rest => EditorHoverPhase::Rest, _ => EditorHoverPhase::Leave }; let event = hover_s.bridge.borrow().on_code_hovered(&hover_uri, e.decoration.unwrap_or(0), pos(e.position), phase); send(&hover_s, event); },
        on_decoration_click: move |id| { let event = click_s.bridge.borrow().on_decoration_activated(&click_uri, id); send(&click_s, event); },
        on_fold_change: move |e: compose_rust::code::FoldChange| { let event = fold_s.bridge.borrow_mut().on_fold_changed(&fold_uri, e.start_line, e.collapsed); send(&fold_s, event); let mut rev = fold_s.revision; *rev.write() += 1; },
        on_selection_change: move |e: compose_rust::code::SelectionChange| { let mut position = selection_s.position; position.set((e.active.line as usize + 1, e.active.column as usize + 1)); let mut find_sel = selection_s.find.selection; find_sel.set(((e.anchor.line, e.anchor.column), (e.active.line, e.active.column))); let event = selection_s.bridge.borrow().on_selection_changed(&selection_uri, e.version, pos(e.anchor), pos(e.active)); send(&selection_s, event); },
    } }
}

pub mod page {
    use dioxus_compose::html::prelude::dioxus_elements::point_interaction::{InteractionLocation, ModifiersInteraction};
    use dioxus_compose::html::prelude::dioxus_elements::{Key, Modifiers};
    use dioxus_compose::html::prelude::dioxus_signals::{ReadableExt, WritableExt};
    use dioxus_compose::html::prelude::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use crate::terminal_stream::{self, ChangeCallback, TerminalHandle, TerminalState};
    use crate::ui::editor_terminals as terms;
    use super::{DOUBLE_CLICK, DOUBLE_SHIFT, now_ms, send, editor_pane, join_path, shared, workbench, base_name, position_text, BridgeAction, HostCommand, PendingGoto, Shared};
    use crate::ui::editor::{
        action_for_key, agent_status_text, apply_action, tooltip_text, Side, Stripes, ToolWindow,
    };
    use crate::ui::keymap::{Action, Chord, Keymap, Os};
    use crate::ui::editor::DoubleShift;
    use crate::ui::project::{project_boxes, status_pill, SOON};

    /// The mockup's tokens and editor screen styles.
    pub fn config() -> HtmlConfig {
        HtmlConfig {
            stylesheets: vec![
                // The mockup's dark tokens hang on `:root[data-theme=dark]`; here the page's own container
                // carries the attribute, so the selector is the attribute alone.
                include_str!("../../assets/editor/style.css").replace(":root[data-theme=dark]", "[data-theme=dark]"),
                include_str!("../../assets/editor/editor.css").to_string(),
                include_str!("../../assets/editor/page.css").to_string(),
            ],
            images: Some(Box::new(crate::ui::editor_icons::IconResolver::default())),
            ..HtmlConfig::default()
        }
    }

    pub fn app() -> Element {
        let target = crate::services::services().ui.editor.get_untracked().expect("editor target");
        super::init(target);
        workbench_page()
    }

    /// The key's name as the keymap table writes it.
    pub(crate) fn key_name(key: &Key) -> String {
        match key {
            Key::Character(c) => c.clone(),
            Key::Escape => "Esc".to_string(),
            other => format!("{other:?}"),
        }
    }

    fn notice(text: impl Into<String>) {
        crate::ui::cr::notice::post(text);
    }

    /// Open the find bar, or refocus the query field when already open.
    pub fn open_find_bar(s: &Shared) -> bool {
        if s.bridge.borrow().active_document().is_none() {
            notice("찾을 파일이 열려 있지 않습니다");
            return false;
        }
        let mut open = s.find.open;
        open.set(true);
        let mut focus_seq = s.find.focus_seq;
        let next_seq = *focus_seq.read() + 1;
        focus_seq.set(next_seq);
        true
    }

    /// Do a keymap action. Closing a tab also tells the host; an action the editor cannot do
    /// says it is not ready.
    fn run_action(s: &Shared, action: Action) {
        if action == Action::FindInFile {
            open_find_bar(s);
            return;
        }
        if matches!(action, Action::FindInFiles | Action::ReplaceInFiles) {
            crate::ui::editor_search::view::open(s, action == Action::ReplaceInFiles);
            return;
        }
        if action == Action::Save {
            save_current(s);
            return;
        }
        if action == Action::Format {
            crate::ui::editor_language::view::run(s, crate::ui::editor_language::LangAction::Format);
            return;
        }
        // Settings (FR-L31.17): the view is a tab, the two files open as documents.
        match action {
            Action::Settings => return crate::ui::editor_settings::view::open(s),
            Action::SettingsJson => return crate::ui::editor_settings::view::open_file(s, crate::ui::editor_settings::SettingsFile::User),
            Action::KeybindingsJson => return crate::ui::editor_settings::view::open_file(s, crate::ui::editor_settings::SettingsFile::Keybindings),
            _ => {}
        }
        if let Some(mode) = crate::ui::palette::mode_for(action, *s.keymap.read()) {
            open_palette(s, mode);
            return;
        }
        let saved_zoom = crate::services::services().ui.prefs.get_untracked().editor_zoom_level;
        if let Some(level) = crate::ui::editor_actions::zoom_level_after(action, saved_zoom) {
            compose_rust::zoom::set_zoom_level(level);
            crate::services::services().ui.update_prefs(|p| p.editor_zoom_level = level);
            return;
        }
        if action == Action::NewTerminal {
            let mut stripes = s.stripes;
            stripes.write().show(ToolWindow::Terminal);
            return;
        }
        if action == Action::MaximizeTool {
            let mut bottom_max = s.bottom_max;
            let max = *bottom_max.read();
            let mut stripes = s.stripes;
            let next = crate::ui::editor_actions::toggle_maximized_panel(&mut *stripes.write(), max);
            bottom_max.set(next);
            return;
        }
        if action == Action::ReopenClosed {
            let mut tabs = s.tabs;
            let reopened = tabs.write().reopen_closed();
            match reopened {
                Some(path) => {
                    open_tab_file(s, &path);
                }
                None => notice("다시 열 편집기가 없습니다"),
            }
            return;
        }
        if action == Action::Close {
            // Bound first: the read guard must be gone before `sync_dirty` writes the tabs.
            let current = s.tabs.read().current_path().map(str::to_owned);
            if let Some(path) = current {
                sync_dirty(s);
                let request = s.tabs.read().close_request(&path);
                request_close(s, request);
                return;
            }
        }
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
            notice(crate::ui::editor_actions::not_ready_notice(action));
        }
        if handled && matches!(action, Action::Commit | Action::Git) && stripes.read().selected(Side::Left).is_some() {
            s.scm.refresh();
        }
    }

    /// Saves the document in the active tab: the same request the editor widget sends for its own Cmd+S, for
    /// when the key reaches the page instead of the widget. The session saves the text it holds, so the
    /// request carries no version.
    fn save_current(s: &Shared) {
        let current = s.tabs.read().current_path().map(str::to_owned);
        // The working tree of a diff being edited is saved instead, while it covers the editor area (FR-L31.10).
        let editing = s.scm.editable_diff_path(current.as_deref()).map(|rel| join_path(&s.folder, &rel));
        let current = crate::ui::editor_scm::save_target(current.as_deref(), editing);
        let uri = current.and_then(|p| s.uris.borrow().get(&p).cloned());
        match uri {
            Some(uri) if !crate::ui::editor_states::view::manual_save_allowed(s, &uri.path) => {}
            Some(uri) => send(s, s.bridge.borrow().on_save_requested(&uri, 0)),
            None => notice("저장할 파일이 열려 있지 않습니다"),
        }
    }

    /// Copies the unsaved marks of the open documents into the tab model, so a close request knows which
    /// tabs are dirty (the session reports them to the bridge).
    pub(crate) fn sync_dirty(s: &Shared) {
        let flags: Vec<(String, bool)> = {
            let bridge = s.bridge.borrow();
            let uris = s.uris.borrow();
            s.tabs
                .read()
                .paths()
                .into_iter()
                .map(|p| (p.to_owned(), uris.get(p).and_then(|u| bridge.document(u)).is_some_and(|d| d.dirty)))
                .collect()
        };
        let stale = { let tabs = s.tabs.read(); flags.iter().any(|(p, d)| tabs.is_dirty(p) != *d) };
        if stale {
            let mut tabs = s.tabs;
            let mut t = tabs.write();
            for (p, d) in flags {
                t.set_dirty(&p, d);
            }
        }
    }

    /// Closes the tabs of `request` at once, or asks first when one of them has unsaved edits (FR-L31.5).
    pub(crate) fn request_close(s: &Shared, request: crate::ui::editor::CloseRequest) {
        if request.is_empty() {
            return;
        }
        if request.needs_confirm() {
            let mut confirm = s.close_confirm;
            confirm.set(Some(request));
        } else {
            finish_close(s, &request.paths, request.anchor.as_deref());
        }
    }

    /// Closes `paths`, tells the session, and focuses the tab that is current afterwards.
    fn finish_close(s: &Shared, paths: &[String], anchor: Option<&str>) {
        if paths.is_empty() {
            return;
        }
        let mut tabs = s.tabs;
        tabs.write().close_many(paths, anchor);
        for p in paths {
            s.host.send(HostCommand::Close(p.clone()));
        }
        let next = tabs.read().current_path().map(str::to_owned);
        if let Some(next) = next {
            s.host.send(HostCommand::Focus(next));
        }
    }

    /// The answer to the unsaved-changes question. 저장 sends the saves and closes the clean tabs now; each
    /// dirty tab closes when its save succeeds (`save_result`) and stays open and dirty when it fails, as in
    /// Code-OSS. 저장 안 함 closes them all; 취소 none.
    fn answer_close(s: &Shared, choice: crate::ui::editor::DirtyChoice) {
        let mut confirm = s.close_confirm;
        let Some(request) = confirm.read().clone() else { return };
        confirm.set(None);
        // The version each save is asked at: an edit after it keeps the tab open (`save_result`).
        let versions: HashMap<String, u32> = {
            let bridge = s.bridge.borrow();
            let uris = s.uris.borrow();
            request.dirty.iter().filter_map(|p| uris.get(p).and_then(|u| bridge.document(u)).map(|d| (p.clone(), d.version))).collect()
        };
        let now = { let mut tabs = s.tabs; tabs.write().begin_close(&request, choice, |p| versions.get(p).copied()) };
        if choice == crate::ui::editor::DirtyChoice::Save {
            for p in &request.dirty {
                let uri = s.uris.borrow().get(p).cloned();
                let event = uri.and_then(|uri| s.bridge.borrow().on_save_requested(&uri, 0));
                match event {
                    Some(event) => send(s, Some(event)),
                    // No document to save: the tab stays open, as after a failed save.
                    None => { let mut tabs = s.tabs; tabs.write().save_finished(p, crate::ui::editor::SaveOutcome { ok: false, version: None, dirty: true }); }
                }
            }
        }
        finish_close(s, &now, request.anchor.as_deref());
    }

    /// A save finished: a tab that waited on it to close closes on success when its document is still at
    /// the saved version and clean, and stays open otherwise. Called after the session's updates of the
    /// same batch are applied, so the unsaved mark that follows `Saved` is already in the bridge.
    fn save_result(s: &Shared, path: &str, ok: bool) {
        let (version, dirty) = {
            let bridge = s.bridge.borrow();
            let uris = s.uris.borrow();
            match uris.get(path).and_then(|u| bridge.document(u)) {
                Some(d) => (Some(d.version), d.dirty),
                None => (None, true),
            }
        };
        let outcome = crate::ui::editor::SaveOutcome { ok, version, dirty };
        let close = { let mut tabs = s.tabs; tabs.write().save_finished(path, outcome) };
        if close {
            finish_close(s, &[path.to_string()], None);
        }
    }

    /// Opens a file as the preview tab (a single click in the explorer or the search results); the
    /// preview it replaces is closed in the session.
    pub(crate) fn open_preview(s: &Shared, path: &str) {
        let mut tabs = s.tabs;
        let replaced = tabs.write().open_preview(path);
        if let Some(old) = replaced {
            s.host.send(HostCommand::Close(old));
        }
        s.open_errors.borrow_mut().remove(path);
        s.host.send(HostCommand::Open(path.to_string()));
    }

    /// Opens a file in a full tab and focuses it (double click, recent or palette selection).
    pub(crate) fn open_tab_file(s: &Shared, path: &str) {
        let mut tabs = s.tabs;
        tabs.write().open(path);
        s.open_errors.borrow_mut().remove(path);
        s.host.send(HostCommand::Open(path.to_string()));
        s.host.send(HostCommand::Focus(path.to_string()));
        let mut recent = s.recent_files;
        record_recent_file(&mut recent, path);
    }

    fn record_recent_file(recent: &mut super::Signal<Vec<String>>, path: &str) {
        let mut list = recent.read().clone();
        crate::ui::palette::record_recent_path(&mut list, path);
        recent.set(list);
    }

    fn open_palette(s: &Shared, mode: crate::ui::palette::Mode) {
        let (mut open, mut query, mut sel, mut palette_mode) = (s.palette_open, s.palette_query, s.palette_sel, s.palette_mode);
        palette_mode.set(mode);
        if mode != crate::ui::palette::Mode::Commands && mode != crate::ui::palette::Mode::GotoLine {
            s.host.send(HostCommand::IndexFiles);
        }
        if mode == crate::ui::palette::Mode::GotoLine {
            query.set(":".to_string());
        } else {
            query.set(String::new());
        }
        sel.set(0);
        open.set(true);
    }

    fn close_palette(s: &Shared) {
        let mut open = s.palette_open;
        if *open.read() {
            open.set(false);
            s.host.send(HostCommand::CancelIndexFiles);
        }
    }

    /// Does what a chosen palette item says: runs the action, or opens the file in a tab.
    fn choose_palette_item(s: &Shared, payload: crate::ui::palette::Payload) {
        match payload {
            crate::ui::palette::Payload::Action(action) => run_action(s, action),
            crate::ui::palette::Payload::File(path) => {
                open_tab_file(s, &path);
            }
            crate::ui::palette::Payload::FileAt { path, line, column } => {
                open_tab_file(s, &path);
                let line0 = line.saturating_sub(1) as u32;
                let col0 = column.unwrap_or(1).saturating_sub(1) as u32;
                let target = crate::ui::find::Match { line: line0, start: col0, end: col0 };
                if s.bridge.borrow().active_document().is_some_and(|d| d.uri.path == path) {
                    find_send(s, Some(target), Vec::new());
                } else {
                    crate::ui::editor_search::view::schedule_reveal(&path, target);
                }
                let mut pos_sig = s.position;
                pos_sig.set((line, column.unwrap_or(1)));
            }
            crate::ui::palette::Payload::GotoLine { line, column } => {
                let line0 = line.saturating_sub(1) as u32;
                let col0 = column.unwrap_or(1).saturating_sub(1) as u32;
                find_send(s, Some(crate::ui::find::Match { line: line0, start: col0, end: col0 }), Vec::new());
                let mut pos_sig = s.position;
                pos_sig.set((line, column.unwrap_or(1)));
            }
            crate::ui::palette::Payload::ExtensionCommand(id) => {
                s.host.send(HostCommand::ExecuteCommand(id));
            }
            payload @ (crate::ui::palette::Payload::SetEol(_)
            | crate::ui::palette::Payload::EncodingAction(_)
            | crate::ui::palette::Payload::SetEncoding { .. }
            | crate::ui::palette::Payload::SetLanguage(_)
            | crate::ui::palette::Payload::Cancel) => choose_format_item(s, payload),
        }
    }

    /// The active document's path and its status bar state (format, dirty), for the pickers (FR-L31.7).
    fn active_format(s: &Shared) -> Option<(String, Option<ember_editor::DocumentFormat>, bool)> {
        let path = s.tabs.read().current_path().map(str::to_owned)?;
        let uri = s.uris.borrow().get(&path).cloned()?;
        let bridge = s.bridge.borrow();
        let doc = bridge.document(&uri)?;
        Some((path, doc.format.clone(), doc.dirty))
    }

    /// Open one of the status bar's pickers in the palette sheet, on the current item (FR-L31.7).
    fn open_picker(s: &Shared, mode: crate::ui::palette::Mode) {
        if active_format(s).is_none() {
            notice("열린 파일이 없습니다");
            return;
        }
        let (mut open, mut query, mut sel, mut palette_mode) = (s.palette_open, s.palette_query, s.palette_sel, s.palette_mode);
        palette_mode.set(mode);
        query.set(String::new());
        // Follow the current entry until the person moves: the rows may still grow (FR-L31.7).
        sel.set(crate::ui::editor_status::FOLLOW_CURRENT);
        open.set(true);
    }

    /// What a picker row does: the next picker, or the change sent to the session (FR-L31.7).
    fn choose_format_item(s: &Shared, payload: crate::ui::palette::Payload) {
        use crate::ui::palette::{Mode, Payload};
        let Some((path, format, dirty)) = active_format(s) else { return };
        match payload {
            Payload::SetEol(eol) => { s.host.send(HostCommand::SetEol { path, eol }); }
            Payload::EncodingAction(mode) => open_picker(s, Mode::Encoding(mode)),
            Payload::SetEncoding { mode: ember_editor::EncodingMode::Reopen, encoding }
                if *s.palette_mode.read() != Mode::ConfirmReopen(encoding)
                    && crate::ui::editor_status::reopen_needs_confirmation(dirty, format.as_ref(), encoding) =>
            {
                open_picker(s, Mode::ConfirmReopen(encoding));
            }
            Payload::SetEncoding { mode, encoding } => { s.host.send(HostCommand::SetEncoding { path, encoding, mode }); }
            Payload::SetLanguage(language_id) => { s.host.send(HostCommand::SetLanguage { path, language_id }); }
            _ => {}
        }
    }

    /// The palette sheet: a search field over the editor's actions, ranked as the query is typed.
    /// Enter does the highlighted one, Escape or a click outside closes it. The design follows the
    /// app's sheet rules in the mockup's tokens (the mockup has no palette).
    fn palette_sheet(s: &Shared, keymap: Keymap) -> Element {
        use crate::ui::palette::{
            apply_line_suffix, effective_mode, goto_line_item, highlight_segments, items_for_workbench,
            move_selection, parse_path_line_suffix, placeholder, rank, Payload,
        };
        let (mut open, mut query, mut sel) = (s.palette_open, s.palette_query, s.palette_sel);
        let base_mode = *s.palette_mode.read();
        let raw_text = query.read().clone();
        let (mode, effective_query) = effective_mode(base_mode, &raw_text);
        let (files, index_running, index_capped, ext_commands) = {
            let state = s.host.state.lock().unwrap();
            (state.file_items.clone(), state.index_running, state.index_capped, state.extension_commands.clone())
        };
        let recent = s.recent_files.read().clone();
        let (items, ranked, line_suffix) = if mode == crate::ui::palette::Mode::GotoLine {
            let item = goto_line_item(&effective_query);
            let items = vec![item];
            let ranked = vec![crate::ui::palette::Ranked { index: 0, score: 100, matched: Vec::new() }];
            (std::borrow::Cow::Owned(items), ranked, None)
        } else {
            let (file_q, suffix) = parse_path_line_suffix(&effective_query);
            if crate::ui::editor_status::is_picker(mode) {
                let format = active_format(s).and_then(|(_, f, _)| f);
                let languages = s.host.state.lock().unwrap().languages.clone();
                let items = crate::ui::editor_status::picker_items(mode, format.as_ref(), &languages);
                let ranked = rank(&effective_query, &items);
                (std::borrow::Cow::Owned(items), ranked, None)
            } else {
                let items = items_for_workbench(mode, keymap, Os::current(), &s.folder, &files, &ext_commands, &recent);
                let ranked = rank(file_q, &items);
                (items, ranked, suffix)
            }
        };
        let hint_text = placeholder(mode, keymap);
        // A picker lists every row (a language list is longer than 50) and shows the window of rows that
        // holds the highlighted one, so the current language or encoding is in view when it opens (FR-L31.7).
        let picker = crate::ui::editor_status::is_picker(mode);
        let shown: Vec<_> = ranked.iter().take(if picker { usize::MAX } else { 50 }).cloned().collect();
        let chosen = if picker {
            let format = active_format(s).and_then(|(_, f, _)| f);
            let languages = s.host.state.lock().unwrap().languages.clone();
            crate::ui::editor_status::resolve_selection(*sel.read(), mode, format.as_ref(), &languages, shown.len())
        } else {
            (*sel.read()).min(shown.len().saturating_sub(1))
        };
        let (win_start, _) = if picker {
            crate::ui::editor_status::list_window(chosen, shown.len(), crate::ui::editor_status::PICKER_VISIBLE_ROWS)
        } else {
            (0, shown.len())
        };
        // A command the editor cannot do yet is listed but does nothing: Enter on it leaves the sheet open.
        let enter_payload = shown
            .get(chosen)
            .filter(|r| items[r.index].enabled)
            .map(|r| apply_line_suffix(items[r.index].payload.clone(), line_suffix));
        let count = shown.len();
        let backdrop_s = s.clone();
        let key_s = s.clone();
        rsx! {
            // `grid-row` and `grid-column` keep both in the top bar's cell: placed automatically they
            // took an implicit row of the page grid and pushed the page down one gap (design review A6).
            div { style: "position:fixed; grid-row:1; grid-column:1; left:0; top:0; right:0; bottom:0; z-index:39;", onclick: move |_| close_palette(&backdrop_s) }
            div { class: "ed-menu", role: "dialog", "aria-label": "명령 팔레트",
                style: "position:fixed; grid-row:1; grid-column:1; z-index:40; top:60px; left:calc(50vw - min(280px, calc(50vw - 12px))); width:min(560px, calc(100vw - 24px)); right:auto; padding:var(--sp1);",
                div { class: "ed-find", style: "width:auto; margin:var(--sp1);",
                    {icon("search")}
                    input { style: "flex:1; min-width:0; border:0; background:none; outline:none; color:var(--text); font:inherit;",
                        "aria-label": "{crate::ui::palette::search_label(mode)}", placeholder: "{hint_text}", value: "{raw_text}", autofocus: true,
                        oninput: move |event| {
                            // Only a changed query moves the highlight back to the top, so an input event that
                            // repeats the text (focus, mount) keeps a picker on its current row.
                            let value = event.value();
                            if value != *query.read() { query.set(value); sel.set(0); }
                        },
                        onkeydown: move |event| {
                            match event.key() {
                                Key::Escape => { event.stop_propagation(); close_palette(&key_s); }
                                Key::ArrowDown => { event.stop_propagation(); sel.set(move_selection(chosen, count, true)); }
                                Key::ArrowUp => { event.stop_propagation(); sel.set(move_selection(chosen, count, false)); }
                                Key::Enter => {
                                    event.stop_propagation();
                                    if let Some(payload) = enter_payload.clone() {
                                        close_palette(&key_s);
                                        choose_palette_item(&key_s, payload);
                                    }
                                }
                                _ => {}
                            }
                        },
                    }
                }
                if crate::ui::palette::lists_files(mode) && index_running && !shown.is_empty() {
                    div { style: "padding:var(--sp1) var(--sp3); color:var(--muted); font-size:var(--fs-xs);", "파일을 더 찾는 중입니다…" }
                }
                if crate::ui::palette::lists_files(mode) && index_capped {
                    div { style: "padding:var(--sp1) var(--sp3); color:var(--muted); font-size:var(--fs-xs);", "파일이 많아 일부만 표시합니다." }
                }
                div { style: "max-height:320px; overflow:auto;", role: "listbox",
                    if shown.is_empty() { div { style: "padding:var(--sp3); color:var(--muted);", "{crate::ui::palette::empty_text(mode, index_running)}" } }
                    if win_start > 0 {
                        div { class: "ed-tn", role: "option", "aria-label": "위 항목 더 보기", style: "--d:0; height:34px; color:var(--muted);",
                            onclick: move |_| sel.set(win_start - 1),
                            "{crate::ui::editor_status::more_above_text(win_start)}"
                        }
                    }
                    for (row, r) in shown.iter().enumerate().skip(win_start) {
                        {
                            let item = &items[r.index];
                            let segments = highlight_segments(&item.title, &r.matched);
                            let enabled = item.enabled;
                            let hint = if enabled { item.hint.clone() } else if mode == crate::ui::palette::Mode::GotoLine { String::new() } else if let crate::ui::palette::Payload::Action(a) = &item.payload { crate::ui::editor_actions::not_ready_hint(*a) } else { "준비 중".to_string() };
                            let payload = apply_line_suffix(item.payload.clone(), line_suffix);
                            let click_s = s.clone();
                            let class = if row == chosen { "ed-tn sel" } else { "ed-tn" };
                            let row_style = if enabled { "--d:0; height:34px;" } else { "--d:0; height:34px; color:var(--muted);" };
                            rsx! { div { key: "{row}", class, role: "option", "aria-selected": "{row == chosen}", "aria-disabled": "{!enabled}", style: "{row_style}",
                                onclick: move |_| {
                                    if !enabled { return; }
                                    close_palette(&click_s);
                                    choose_palette_item(&click_s, payload.clone());
                                },
                                for (i, (part, hit)) in segments.into_iter().enumerate() {
                                    span { key: "{i}", style: if hit { "font-weight:var(--fw-semibold); color:var(--text);" } else { "" }, "{part}" }
                                }
                                div { class: "spacer" }
                                if !hint.is_empty() { kbd { class: "ed-sc", "{hint}" } }
                            } }
                        }
                    }
                }
            }
        }
    }

    fn find_options(s: &Shared) -> crate::ui::find::FindOptions {
        crate::ui::find::FindOptions {
            case_sensitive: *s.find.case_sensitive.read(),
            whole_word: *s.find.whole_word.read(),
            regex: *s.find.regex.read(),
            preserve_case: *s.find.preserve_case.read(),
            scope: if *s.find.in_selection.read() { *s.find.scope.read() } else { None },
        }
    }

    /// Asks the editor to do `goto` and `replacements`, once.
    fn find_send(s: &Shared, goto: Option<crate::ui::find::Match>, replacements: Vec<(crate::ui::find::Match, String)>) {
        let (mut cmd, mut next_id) = (s.find.cmd, s.find.next_id);
        let id = *next_id.read();
        next_id.set(id + 1);
        cmd.set(Some(super::FindCmd { id, goto, replacements }));
    }

    /// Goes to the next or the previous match: from the match the reader is on, else from the caret.
    fn find_go(s: &Shared, forward: bool) {
        let (matches, truncated, text) = {
            let bridge = s.bridge.borrow();
            let Some(doc) = bridge.active_document() else { return; };
            let (m, tr) = crate::ui::find::cached_doc_matches(&doc.uri.key(), doc.generation, doc.version, &s.find.query.read(), find_options(s), || &doc.text);
            let text = if tr { Some(doc.text.clone()) } else { None };
            (m, tr, text)
        };
        if matches.is_empty() && !truncated { return; }
        let mut current = s.find.current;
        let from = match *current.read() {
            Some(i) if i < matches.len() => {
                let m = matches[i];
                if forward { (m.line, m.end) } else { (m.line, m.start) }
            }
            _ => {
                let (line, col) = *s.position.read();
                (line.saturating_sub(1) as u32, col.saturating_sub(1) as u32)
            }
        };
        let target = if !truncated {
            crate::ui::find::next_match(&matches, from, forward).map(|i| {
                current.set(Some(i));
                matches[i]
            })
        } else {
            let full_text = text.unwrap_or_default();
            crate::ui::find::find_nav_match(&full_text, &s.find.query.read(), find_options(s), &matches, truncated, from, forward).map(|m| {
                current.set(matches.iter().position(|x| *x == m));
                m
            })
        };
        if let Some(target) = target {
            find_send(s, Some(target), Vec::new());
        }
    }

    /// Replaces the match the reader is on (the first one when none is chosen yet).
    fn find_replace_one(s: &Shared) {
        let (matches, text) = {
            let bridge = s.bridge.borrow();
            let Some(doc) = bridge.active_document() else { return; };
            (crate::ui::find::cached_doc_matches(&doc.uri.key(), doc.generation, doc.version, &s.find.query.read(), find_options(s), || &doc.text).0, doc.text.clone())
        };
        let mut current = s.find.current;
        let index = current.read().filter(|i| *i < matches.len()).or(if matches.is_empty() { None } else { Some(0) });
        let Some(index) = index else { return };
        let replacement = s.find.replacement.read().clone();
        find_send(s, None, crate::ui::find::replacement_edits(&text, &s.find.query.read(), find_options(s), &matches, Some(index), &replacement));
        current.set(None);
    }

    fn find_replace_all(s: &Shared) {
        let ((matches, truncated), text) = {
            let bridge = s.bridge.borrow();
            let Some(doc) = bridge.active_document() else { return; };
            (crate::ui::find::cached_doc_matches(&doc.uri.key(), doc.generation, doc.version, &s.find.query.read(), find_options(s), || &doc.text), doc.text.clone())
        };
        if matches.is_empty() { return; }
        if truncated {
            notice(format!("결과가 {}개를 넘어 모두 바꾸기를 쓸 수 없습니다. 찾을 내용을 더 구체적으로 적으세요", crate::ui::find::MAX_MATCHES));
            return;
        }
        let replacement = s.find.replacement.read().clone();
        let mut current = s.find.current;
        find_send(s, None, crate::ui::find::replacement_edits(&text, &s.find.query.read(), find_options(s), &matches, None, &replacement));
        current.set(None);
    }

    /// Turns a find option (FR-L31.12) on or off. Turning 선택 영역에서 찾기 on takes the selection of that
    /// moment as the scope, as Code-OSS does.
    fn toggle_find_option(s: &Shared, option: crate::ui::keymap::FindOption) {
        use crate::ui::keymap::FindOption;
        let mut current = s.find.current;
        match option {
            FindOption::Regex => { let mut r = s.find.regex; let on = *r.read(); r.set(!on); }
            FindOption::PreserveCase => { let mut p = s.find.preserve_case; let on = *p.read(); p.set(!on); }
            FindOption::InSelection => {
                let mut in_sel = s.find.in_selection;
                let on = *in_sel.read();
                if !on {
                    let (anchor, active) = *s.find.selection.read();
                    let text = s.bridge.borrow().active_document().map(|d| d.text.clone()).unwrap_or_default();
                    let mut scope = s.find.scope;
                    scope.set(crate::ui::find::Scope::from_selection(anchor, active, &text));
                }
                in_sel.set(!on);
            }
        }
        current.set(None);
    }

    /// The find option a key pressed in a find bar field toggles, by the physical key so that macOS's
    /// Option letters (⌥R gives `®`) still match.
    fn find_option_for_event(event: &dioxus_elements::KeyboardEvent) -> Option<crate::ui::keymap::FindOption> {
        let m = event.modifiers();
        let code = format!("{:?}", event.code());
        let key = code.strip_prefix("Key").filter(|k| k.len() == 1).map(str::to_string).unwrap_or_else(|| key_name(&event.key()));
        let chord = crate::ui::editor::chord_from_key(&key, m.contains(Modifiers::CONTROL), m.contains(Modifiers::SHIFT), m.contains(Modifiers::ALT), m.contains(Modifiers::META));
        crate::ui::keymap::FindOption::for_chord(&chord, crate::ui::keymap::Os::current())
    }

    /// The find bar over the editor: a field with the case and whole-word options, the counter and the
    /// previous and next buttons, a replacement field with replace and replace all, and close. Enter goes
    /// to the next match, Shift+Enter to the previous, Escape closes.
    fn find_bar(s: &Shared) -> Element {
        let (mut query, mut replacement, mut case_sensitive, mut whole_word, mut current, mut open) =
            (s.find.query, s.find.replacement, s.find.case_sensitive, s.find.whole_word, s.find.current, s.find.open);
        let focus_seq = *s.find.focus_seq.read();
        let q = query.read().clone();
        let (matches, truncated) = {
            let bridge = s.bridge.borrow();
            if let Some(doc) = bridge.active_document() {
                crate::ui::find::cached_doc_matches(&doc.uri.key(), doc.generation, doc.version, &q, find_options(s), || &doc.text)
            } else {
                (Vec::new(), false)
            }
        };
        let regex_bad = crate::ui::find::regex_error(&q, find_options(s)).is_some();
        let counter = if regex_bad { "잘못된 정규식".to_string() } else if truncated { format!("{}개 이상", crate::ui::find::MAX_MATCHES) } else { crate::ui::find::count_text(current.read().filter(|i| *i < matches.len()), matches.len(), &q) };
        let (prev_s, next_s, one_s, all_s, key_s, enter_s) = (s.clone(), s.clone(), s.clone(), s.clone(), s.clone(), s.clone());
        let has_doc = s.bridge.borrow().active_document().is_some();
        let has_matches = has_doc && (!matches.is_empty() || truncated);
        let (case_on, word_on) = (*case_sensitive.read(), *whole_word.read());
        let (regex_on, preserve_on, sel_on) = (*s.find.regex.read(), *s.find.preserve_case.read(), *s.find.in_selection.read());
        let (regex_s, sel_s, preserve_s, opt_q, opt_r) = (s.clone(), s.clone(), s.clone(), s.clone(), s.clone());
        let os = crate::ui::keymap::Os::current();
        use crate::ui::keymap::FindOption;
        let (regex_t, sel_t, preserve_t) = (format!("정규식 사용 ({})", FindOption::Regex.hint(os)), format!("선택 영역에서 찾기 ({})", FindOption::InSelection.hint(os)), format!("대소문자 보존 ({})", FindOption::PreserveCase.hint(os)));
        let on = |flag: bool| if flag { "ed-pj on" } else { "ed-pj" };
        rsx! {
            div { class: "ed-tsub", role: "search", "aria-label": "문서에서 찾기", style: "flex-wrap:wrap;",
                div { class: "ed-find", style: "width:220px; height:28px;",
                    {icon("search")}
                    input {
                        key: "find-query-{focus_seq}",
                        style: "flex:1; min-width:0; border:0; background:none; outline:none; color:var(--text); font:inherit;",
                        "aria-label": "찾을 내용", placeholder: "찾기", value: "{q}", autofocus: true,
                        oninput: move |event| { query.set(event.value()); current.set(None); },
                        onkeydown: move |event| {
                            match event.key() {
                                Key::Enter => { event.stop_propagation(); find_go(&key_s, !event.modifiers().contains(Modifiers::SHIFT)); }
                                Key::Escape => { event.stop_propagation(); open.set(false); }
                                _ => if let Some(option) = find_option_for_event(&event) { event.stop_propagation(); event.prevent_default(); toggle_find_option(&opt_q, option); }
                            }
                        },
                    }
                }
                button { class: on(case_on), title: "대소문자 구분", "aria-label": "대소문자 구분", "aria-pressed": "{case_on}", style: "border:1px solid var(--line);", onclick: move |_| { case_sensitive.set(!case_on); current.set(None); }, "Aa" }
                button { class: on(word_on), title: "단어 단위", "aria-label": "단어 단위", "aria-pressed": "{word_on}", style: "border:1px solid var(--line);", onclick: move |_| { whole_word.set(!word_on); current.set(None); }, "단어" }
                button { class: on(regex_on), title: "{regex_t}", "aria-label": "정규식 사용", "aria-pressed": "{regex_on}", style: "border:1px solid var(--line);", onclick: move |_| toggle_find_option(&regex_s, FindOption::Regex), ".*" }
                button { class: on(sel_on), title: "{sel_t}", "aria-label": "선택 영역에서 찾기", "aria-pressed": "{sel_on}", style: "border:1px solid var(--line);",
                    onclick: move |_| toggle_find_option(&sel_s, FindOption::InSelection), "선택" }
                span { class: "ed-sbi", role: "status", "{counter}" }
                button { class: "ed-ib", title: "이전 결과", "aria-label": "이전 결과", disabled: !has_matches, onclick: move |_| find_go(&prev_s, false), {icon("up")} }
                button { class: "ed-ib", title: "다음 결과", "aria-label": "다음 결과", disabled: !has_matches, onclick: move |_| find_go(&next_s, true), {icon("down")} }
                div { class: "ed-find", style: "width:200px; height:28px;",
                    input { style: "flex:1; min-width:0; border:0; background:none; outline:none; color:var(--text); font:inherit;",
                        "aria-label": "바꿀 내용", placeholder: "바꾸기", value: "{replacement}",
                        oninput: move |event| replacement.set(event.value()),
                        onkeydown: move |event| {
                            match event.key() {
                                Key::Enter => { event.stop_propagation(); find_replace_one(&enter_s); }
                                Key::Escape => { event.stop_propagation(); open.set(false); }
                                _ => if let Some(option) = find_option_for_event(&event) { event.stop_propagation(); event.prevent_default(); toggle_find_option(&opt_r, option); }
                            }
                        },
                    }
                }
                button { class: on(preserve_on), title: "{preserve_t}", "aria-label": "대소문자 보존", "aria-pressed": "{preserve_on}", style: "border:1px solid var(--line);", onclick: move |_| toggle_find_option(&preserve_s, FindOption::PreserveCase), "AB" }
                button { class: "ed-pj", style: "border:1px solid var(--line);", disabled: !has_matches, onclick: move |_| find_replace_one(&one_s), "바꾸기" }
                button { class: "ed-pj", style: "border:1px solid var(--line);", disabled: !has_matches, onclick: move |_| find_replace_all(&all_s), "모두 바꾸기" }
                div { class: "spacer" }
                button { class: "ed-ib", title: "닫기", "aria-label": "닫기", onclick: move |_| open.set(false), {icon("close")} }
            }
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

    pub(super) fn workbench_page() -> Element {
        let s = shared();
        let _ = *s.revision.read();
        let (error, updates, edit_errors, gone, renamed) = {
            let mut state = s.host.state.lock().unwrap();
            (state.error.clone(), std::mem::take(&mut state.updates), std::mem::take(&mut state.edit_errors), std::mem::take(&mut state.gone), std::mem::take(&mut state.renamed))
        };
        for (old, new) in renamed {
            // The tabs below the renamed path follow it (clean ones only: a rename with unsaved
            // changes is refused before it is sent); the session reopens them under the new name.
            let current = s.tabs.read().current_path().map(str::to_owned);
            let moved = { let mut tabs = s.tabs; tabs.write().rename_prefix(&old, &new) };
            for (from, to) in moved {
                s.host.send(HostCommand::Close(from.clone()));
                s.host.send(HostCommand::Open(to.clone()));
                if current.as_deref() == Some(from.as_str()) { s.host.send(HostCommand::Focus(to)); }
            }
            let mut expanded = s.expanded;
            let remapped: Vec<String> = expanded.read().iter().filter(|p| crate::ui::editor::is_at_or_below(p, &old)).cloned().collect();
            for p in remapped {
                expanded.write().remove(&p);
                expanded.write().insert(format!("{}{}", new.trim_end_matches('/'), &p[old.trim_end_matches('/').len()..]));
            }
            let mut tree_sel = s.tree_sel;
            tree_sel.set(None);
        }
        for message in edit_errors { notice(message); }
        for path in gone {
            let doomed: Vec<String> = s.tabs.read().paths().into_iter().filter(|tab| crate::ui::editor::is_at_or_below(tab, &path)).map(str::to_owned).collect();
            for tab in doomed {
                let mut tabs = s.tabs;
                tabs.write().close(&tab);
                s.host.send(HostCommand::Close(tab));
            }
            { let mut tabs = s.tabs; tabs.write().forget_closed_below(&path); }
            let mut expanded = s.expanded;
            expanded.write().retain(|p| !crate::ui::editor::is_at_or_below(p, &path));
            let mut tree_sel = s.tree_sel;
            tree_sel.set(None);
        }
        let saved = updates.iter().any(|u| matches!(u, ember_editor::SessionUpdate::Saved { .. }));
        let mut saved_paths: Vec<String> = Vec::new();
        let widget_edits = updates.iter().any(|u| matches!(u, ember_editor::SessionUpdate::Command { command: ember_editor::WidgetCommand::EditCode { .. }, .. }));
        for update in updates {
            if let ember_editor::SessionUpdate::Opened { uri, .. } = &update { s.uris.borrow_mut().insert(uri.path.clone(), uri.clone()); }
            if let ember_editor::SessionUpdate::Closed { uri } = &update { s.uris.borrow_mut().remove(&uri.path); }
            if let ember_editor::SessionUpdate::Opened { uri, .. } = &update { s.open_errors.borrow_mut().remove(&uri.path); crate::ui::editor_scm::view::open_settled(&uri.path); }
            if let ember_editor::SessionUpdate::Saved { uri } = &update { saved_paths.push(uri.path.clone()); }
            let action = s.bridge.borrow_mut().apply(update);
            match action {
                BridgeAction::Resync(uri) => { s.host.send(HostCommand::Resync(uri)); }
                BridgeAction::OpenFailed { uri, error } => {
                    tracing::warn!(path = %uri.path, %error, "opening a file failed");
                    s.open_errors.borrow_mut().insert(uri.path.clone(), crate::ui::editor::file_failure_reason(&error).to_string());
                    // 파일 열기 in the 커밋 window says so in a notice too (FR-L31.9, #781).
                    crate::ui::editor_scm::view::open_failed(&uri.path, crate::ui::editor::file_failure_reason(&error));
                }
                BridgeAction::SaveFailed { uri, error } => {
                    tracing::warn!(path = %uri.path, %error, "saving a file failed");
                    save_result(&s, &uri.path, false);
                    notice(format!("{}을(를) 저장하지 못했습니다: {}", crate::ui::editor::display_name(base_name(&uri.path)), crate::ui::editor::file_failure_reason(&error)));
                }
                BridgeAction::ExtensionNotice(message) => {
                    let prefix = message.source.clone().unwrap_or_else(|| "알림".to_string());
                    let mut logs = s.workbench_logs;
                    crate::ui::editor::push_workbench_log(&mut logs.write(), format!("[{prefix}] [{}] {}", message.severity.label(), message.message));
                    crate::ui::editor_notifications::view::push(&s, message);
                }
                BridgeAction::CommandFailed { id, error } => {
                    tracing::warn!(%id, %error, "extension command failed");
                    notice(format!("명령 {id}을(를) 실행하지 못했습니다: {error}"));
                }
                BridgeAction::RunCommand(command) => {
                    notice(format!("지원하지 않는 명령입니다: {}", command.id));
                }
                BridgeAction::FormatFailed { uri, error } => {
                    tracing::warn!(path = %uri.path, %error, "reopening a file with an encoding failed");
                    notice(crate::ui::editor_status::reopen_failure_text(&crate::ui::editor::display_name(base_name(&uri.path)), None, &error));
                }
                _ => {}
            }
        }
        // After the whole batch, so the unsaved mark the session sends with `Saved` is in the bridge. If it
        // came in a later batch the mark is still the old dirty one and the tab stays open: the safe side.
        for path in saved_paths { save_result(&s, &path, true); }
        if saved {
            // A save changes the work tree: the changes list and the open diff follow it.
            s.scm.refresh();
        }
        // Reconnect, auto-save and compare follow the updates (FR-L31.6).
        let now_current = s.tabs.read().current_path().map(str::to_owned);
        crate::ui::editor_states::view::after_updates(&s, now_current.as_deref());
        // The document of an open working-tree diff stays open in the session (FR-L31.10).
        crate::ui::editor_scm::view::sync_diff_document(&s);
        if widget_edits {
            // The editor pane is a tree of its own: one more render so it takes the queued edits.
            let mut revision = s.revision;
            *revision.write() += 1;
        }
        crate::ui::editor_language::view::after_updates(&s, saved);
        let tabs = s.tabs;
        let current = tabs.read().current_path().map(str::to_owned);
        let paths: Vec<String> = tabs.read().paths().into_iter().map(str::to_owned).collect();
        let bridge = s.bridge.borrow();
        let uris = s.uris.borrow();
        if let Some(uri) = current.as_ref().and_then(|p| uris.get(p)) { drop(bridge); s.bridge.borrow_mut().set_active(uri); } else { drop(bridge); }
        let bridge = s.bridge.borrow();
        let active = current.as_ref().and_then(|p| uris.get(p)).and_then(|u| bridge.document(u)).cloned();
        let open_error = current.as_ref().and_then(|p| s.open_errors.borrow().get(p).cloned());
        let dirty: Vec<bool> = paths.iter().map(|p| uris.get(p).and_then(|u| bridge.document(u)).is_some_and(|d| d.dirty)).collect();
        drop(uris);
        drop(bridge);
        crate::ui::editor_search::view::apply_pending_reveal(&s);
        crate::ui::editor_settings::view::apply_capture_hook(&s);
        let settings_shown = current.as_deref().is_some_and(crate::ui::editor_settings::is_settings_tab);
        let (line, col) = *s.position.read();
        let sel = *s.stripes.read();
        let keymap = *s.keymap.read();
        // A modal blocks the ⋯ menu (#831): it is not drawn, and an open one closes.
        let modal = modal_up(&s);
        if modal && *s.menu_open.peek() {
            let mut menu_open = s.menu_open;
            menu_open.set(false);
        }
        let em_shown = crate::ui::editor_menu::menu_shown(*s.menu_open.read(), modal);
        let (agent_state, project_branch) = project_status(&s.project);
        // The branch git/status returned; the project row's until the first answer.
        let branch = s.scm.branch_line().unwrap_or(project_branch);
        let conflict_shown = s.scm.conflict_visible();
        let diff_shown = s.scm.diff_visible(current.as_deref());
        let compare_shown = crate::ui::editor_states::view::compare_visible(current.as_deref());
        let key_s = s.clone();
        let has_left = sel.selected(Side::Left).is_some();
        let has_bottom = sel.selected(Side::Bottom).is_some();
        let theme_pref = *s.theme.read();
        let dark = theme_pref.resolve(crate::ui::theme::system_scheme()) == crate::ui::theme::Scheme::Dark;
        if super::ICON_DARK.with(|d| d.replace(dark)) != dark {
            // The code editor is a Compose widget: it takes its colours from the toolkit's theme.
            let scheme = if dark { compose_rust::schema::ColorScheme::Dark } else { compose_rust::schema::ColorScheme::Light };
            compose_rust::theme::set_theme(compose_rust::theme::current_theme().with_color_scheme(scheme));
        }
        let ed_class = format!(
            "ed{}{}",
            if has_left { "" } else { " no-left" },
            if has_bottom { "" } else { " no-bottom" },
        );
        // The grid the mockup's `.ed.no-left` and `.ed.no-bottom` rules give, written inline so a
        // closed column is 0 wide and the areas stay as the stylesheet has them.
        let maximized = has_bottom && *s.bottom_max.read();
        let grid = if maximized { crate::ui::editor::editor_grid_style_maximized() } else { crate::ui::editor::editor_grid_style(has_left, has_bottom) };
        // macOS opens the window FullSize (the page starts at the top and the top bar is the title
        // strip), so the page is the window's height; elsewhere the native chrome stays and the
        // page keeps its 76px allowance. TODO(#320): the height is fixed at launch. v0.0.7: #467
        // fixed (`with_report_size`); workaround kept until checked on a display (the page has to
        // follow the reported height in the dioxus-compose window).
        let height = crate::ui::editor::editor_page_height(cfg!(target_os = "macos"));
        let ed_style = format!("width:100vw; height:{height}; {grid}");
        rsx! {
            // TODO(#320): shrinking the window does not reflow this page; growing does.
            div { class: "{ed_class}", "data-theme": if dark { "dark" } else { "light" }, tabindex: "-1", style: "{ed_style}",
                onkeydown: move |event| {
                    let m = event.modifiers();
                    let keymap = *key_s.keymap.read();
                    let name = key_name(&event.key());
                    if event.key() == Key::Escape && !m.contains(Modifiers::CONTROL) && !m.contains(Modifiers::ALT) && !m.contains(Modifiers::META) && !m.contains(Modifiers::SHIFT) {
                        if let Some(target) = crate::ui::editor::editor_focus_target(key_s.tabs.read().current_path()) {
                            key_s.host.send(HostCommand::Focus(target));
                        }
                        crate::ui::editor_language::view::dismiss_hint(&key_s);
                    }
                    if let Some(action) = crate::ui::editor_language::action_for_key(&name, m.contains(Modifiers::CONTROL), m.contains(Modifiers::SHIFT), m.contains(Modifiers::ALT), m.contains(Modifiers::META)) {
                        crate::ui::editor_language::view::run(&key_s, action);
                    }
                    let double = DOUBLE_SHIFT.with(|d| {
                        d.borrow_mut().press(
                            &name,
                            event.is_auto_repeating(),
                            m.contains(Modifiers::CONTROL),
                            m.contains(Modifiers::ALT),
                            m.contains(Modifiers::META),
                            now_ms(),
                        )
                    });
                    if double {
                        if let Some(action) = keymap.action_for(&Chord::DoubleShift, Os::current()) {
                            run_action(&key_s, action);
                        }
                    }
                    let default_action = action_for_key(
                        keymap,
                        Os::current(),
                        &key_name(&event.key()),
                        m.contains(Modifiers::CONTROL),
                        m.contains(Modifiers::SHIFT),
                        m.contains(Modifiers::ALT),
                        m.contains(Modifiers::META),
                    );
                    // The user's keybindings.json is read on top of the keymap (FR-L31.17).
                    let user_rules = key_s.host.state.lock().unwrap().settings.keybindings.clone();
                    let resolution = if user_rules.is_empty() {
                        crate::ui::editor_settings::KeyResolution::Default
                    } else {
                        let chord = crate::ui::editor::chord_from_key(
                            &key_name(&event.key()),
                            m.contains(Modifiers::CONTROL),
                            m.contains(Modifiers::SHIFT),
                            m.contains(Modifiers::ALT),
                            m.contains(Modifiers::META),
                        );
                        crate::ui::editor_settings::resolve_key(&user_rules, &chord, default_action)
                    };
                    match resolution {
                        crate::ui::editor_settings::KeyResolution::Default => {
                            if let Some(action) = default_action { run_action(&key_s, action); }
                        }
                        crate::ui::editor_settings::KeyResolution::Unbound => {}
                        crate::ui::editor_settings::KeyResolution::Action(action) => run_action(&key_s, action),
                        crate::ui::editor_settings::KeyResolution::Command(id) => { key_s.host.send(HostCommand::ExecuteCommand(id)); }
                    }
                },
                if let Some(error) = error {
                    div { style: "padding:20px; grid-column:1 / -1; padding-top:76px;", "{error}" button { onclick: move |_| crate::services::services().ui.close_editor(), "닫기" } }
                } else {
                    {top_bar(&s, &branch, keymap)}
                    div { class: "ed-rail",
                        {stripe_column(&s, sel, keymap, Strip::Left)}
                        // The line above the bottom group (`.ed-stripe-b` border-top): drawn as its own
                        // element, because the HTML path does not draw a border with side margins (A13).
                        div { class: "ed-stripe-sep", role: "separator" }
                        {stripe_column(&s, sel, keymap, Strip::Bottom)}
                    }
                    if let (Some(w), false) = (sel.selected(Side::Left), maximized) {
                        if w == ToolWindow::Project {
                            {project_tree(&s, current.as_deref())}
                        } else if w == ToolWindow::Search {
                            {crate::ui::editor_search::view::panel(&s)}
                        } else if w == ToolWindow::Commit {
                            {crate::ui::editor_scm::view::commit_window(&s)}
                        } else if w == ToolWindow::Git {
                            {crate::ui::editor_scm::view::git_window(&s)}
                        } else if w == ToolWindow::Extensions {
                            {crate::ui::editor_extensions::view::panel(&s)}
                        } else {
                            {side_note(w, "ed-left")}
                        }
                    }
                    section { class: "ed-main", style: if maximized { "display:none;" } else if has_left { "" } else { "margin-left:calc(var(--gap) * -1);" }, "aria-label": "편집기",
                        onmounted: move |event| crate::ui::editor_language::view::mount_editor_area(event.data()),
                        {crate::ui::workspace_trust::view::banner(&s)}
                        {tab_strip(&s, &paths, &dirty, current.as_deref())}
                        if let Some((menu_path, x, y)) = s.tab_menu.read().clone() { {tab_context_menu(&s, &menu_path, x, y)} }
                        if let Some(path) = current.as_deref().filter(|_| !settings_shown) {
                            {breadcrumbs(&s, path)}
                        }
                        {crate::ui::editor_states::view::banners(&s, current.as_deref())}
                        if *s.find.open.read() && active.is_some() && !diff_shown && !conflict_shown && !compare_shown { {find_bar(&s)} }
                        div { style: "flex:1; min-height:0; min-width:0; display:flex; overflow:hidden;",
                            if conflict_shown { {crate::ui::editor_scm::view::conflict_pane(&s)} }
                            else if diff_shown { {crate::ui::editor_scm::view::diff_pane(&s)} }
                            else if compare_shown { {crate::ui::editor_states::view::compare_pane(&s)} }
                            else if settings_shown { {crate::ui::editor_settings::view::pane(&s)} }
                            else if active.is_some() { WidgetView { app: editor_pane, style: "flex:1; min-width:0; height:100%;" } }
                            else if let Some(error) = current.as_ref().and_then(|p| open_error.clone().map(|e| (p.clone(), e))) {
                                div { class: "ed-empty", role: "alert",
                                    b { "{crate::ui::editor::display_name(base_name(&error.0))} 파일을 열 수 없습니다" }
                                    p { "{error.1}" }
                                }
                            }
                            else {
                                {
                                    let ((find_hint, find_label), (proj_hint, proj_label)) = crate::ui::editor::empty_state_hints(keymap, Os::current());
                                    rsx! {
                                        div { class: "ed-empty",
                                            b { "열린 파일이 없습니다" }
                                            div { class: "muted", style: "display:flex; flex-direction:column; gap:6px; align-items:center;",
                                                div { style: "display:flex; align-items:center; gap:6px;",
                                                    kbd { class: "ed-sc", "{find_hint}" }
                                                    span { "{find_label}" }
                                                }
                                                div { style: "display:flex; align-items:center; gap:6px;",
                                                    kbd { class: "ed-sc", "{proj_hint}" }
                                                    span { "{proj_label}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        {crate::ui::editor_language::view::hint_in_editor(&s)}
                    }
                    {stripe_column(&s, sel, keymap, Strip::Right)}
                    if let Some(w) = sel.selected(Side::Bottom) {
                        {bottom_window(&s, w)}
                    }
                    if em_shown { {menu(&s, keymap)} }
                    if *s.palette_open.read() { {palette_sheet(&s, keymap)} }
                    if let Some(request) = s.close_confirm.read().clone() { {close_confirm_sheet(&s, &request)} }
                    {crate::ui::workspace_trust::view::prompt(&s)}
                    {crate::ui::editor_notifications::view::layer(&s)}
                    {crate::ui::editor_language::view::overlay(&s)}
                    {status_bar(&s, &agent_state, &branch, current.as_deref().filter(|_| active.is_some()), line, col, active.as_ref().map_or(4, |d| d.tab_width))}
                }
            }
        }
    }

    /// A mockup icon as an image: the HTML path draws SVG through `img`, which cannot take
    /// `currentColor`, so the stroke is the text colour of the light palette.
    // TODO(editor-redesign step 4): tint per theme once the dark palette is switched on.
    pub(crate) fn icon(name: &str) -> Element {
        icon_px(name, 16)
    }

    /// A mockup icon at `px` dp (the mockup sizes tree, tab and breadcrumb icons by context).
    pub(crate) fn icon_px(name: &str, px: u32) -> Element {
        let dark = if super::ICON_DARK.with(|d| d.get()) { "@dark" } else { "" };
        rsx! { img { class: "i", src: "icon:{name}{dark}", width: "{px}", height: "{px}" } }
    }

    fn top_bar(s: &Shared, branch: &str, keymap: Keymap) -> Element {
        let mut theme = s.theme;
        let theme_pref = *s.theme.read();
        let theme_title = format!("테마: {}", theme_pref.label());
        let name = s.project_name.clone();
        let branch = branch.to_string();
        let find = tooltip_text(keymap.find_label(), &keymap.hint(Action::Find, Os::current()));
        let find_hint = keymap.hint(Action::Find, Os::current());
        let find_s = s.clone();
        let git_s = s.clone();
        let (runcfg_s, run_s, stop_s) = (s.clone(), s.clone(), s.clone());
        let mut stripes = s.stripes;
        let project_tip = tooltip_text("프로젝트 창", &keymap.hint(Action::Project, Os::current()));
        // The traffic lights float over the top bar's left end under FullSize.
        let strip_pad = crate::ui::editor::editor_strip_padding(cfg!(target_os = "macos"), crate::ui::safe_area::safe_area().leading);
        rsx! {
            header { class: "ed-top", style: "{strip_pad}",
                img { class: "logo logo-img", src: "icon:mark", width: "28", height: "28", alt: "Ember" }
                button { class: "ed-pj", title: "{project_tip}", "aria-label": "{project_tip}", onclick: move |_| stripes.write().toggle(ToolWindow::Project), "{name}" {icon_px("chev-dn", 12)} }
                button { class: "ed-pj", title: "브랜치", "aria-label": "브랜치", onclick: move |_| crate::ui::editor_scm::view::show_git(&git_s), {icon("git")} "{branch}" {icon_px("chev-dn", 12)} }
                div { class: "spacer" }
                button { class: "ed-find", title: "{find}", "aria-label": "{find}", onclick: move |_| run_action(&find_s, Action::Find),
                    {icon("search")}
                    span { class: "hide-md", "{crate::ui::editor::FIND_PILL_LABEL}" }
                    kbd { class: "ed-sc", "{find_hint}" }
                }
                div { class: "ed-runcfg",
                    button { class: "ed-pj", title: "실행 구성", "aria-label": "실행 구성", onclick: move |_| crate::ui::editor_tasks::view::open(&runcfg_s), {icon("play")} "실행 구성" {icon_px("chev-dn", 12)} }
                    button { class: "ed-ib run", title: "실행", "aria-label": "실행", onclick: move |_| { if let Some(blocked) = crate::ui::workspace_trust::view::blocked(&run_s, crate::ui::workspace_trust::Restricted::Run) { notice(blocked); } else { crate::ui::editor_tasks::view::run_default(&run_s); } }, {icon("play")} }
                    button { class: "ed-ib", title: "중지", "aria-label": "중지", onclick: move |_| crate::ui::editor_tasks::view::stop(&stop_s), {icon("pause")} }
                }
                // The mockup's order (`editor.js:75-76`): run configuration, run, stop, ask agent, theme.
                // No ⋯ here: the tab strip's ⋯ opens the menu (owner placement rule 2026-11-13, rule 5).
                button { class: "ed-ib", title: "에이전트에게 묻기", "aria-label": "에이전트에게 묻기", onclick: move |_| notice(crate::ui::editor_actions::not_ready_notice(Action::Ask)), {icon("spark")} }
                button { class: "ed-ib", title: "{theme_title}", "aria-label": "{theme_title}", onclick: move |_| {
                    let next = theme_pref.next();
                    theme.set(next);
                    crate::services::services().ui.update_prefs(|p| p.theme = next);
                }, {icon(if super::ICON_DARK.with(|d| d.get()) { "moon" } else { "sun" })} }
            }
        }
    }

    /// The ⋯ menu (FR-L31, #795): fixed under the top bar at the page level, its groups opened as
    /// submenus beside it (`editor_menu`), so it fits the window without scrolling.
    fn menu(s: &Shared, keymap: Keymap) -> Element {
        use crate::ui::editor_menu::{self as em, Submenu};
        let os = Os::current();
        // The current tab's items (FR-L31.5) head the menu. They stand in for the right-click menu.
        // v0.0.7: #417 fixed; workaround kept until checked on a display (dioxus-compose has to forward compose-rust's pointer events as oncontextmenu and ondoubleclick; its pin moves separately).
        let current_tab = s.tabs.read().current_path().map(str::to_owned);
        let tab_items = current_tab.as_deref().map(|p| crate::ui::editor::tab_menu_items(&s.tabs.read(), p)).unwrap_or_default();
        let tab_label = current_tab.as_deref().map(|p| format!("탭: {}", crate::ui::editor::display_name(base_name(p)))).unwrap_or_default();
        let rows = em::top_rows(current_tab.is_some());
        let open = em::open_submenu().filter(|sub| *sub != Submenu::Tab || current_tab.is_some());
        let group = |sub: Submenu, label: String| {
            let mut revision = s.revision;
            let on = open == Some(sub);
            rsx! {
                button { key: "{label}", role: "menuitem", "aria-haspopup": "menu", "aria-expanded": "{on}",
                    style: if on { "background:var(--hover);" } else { "" },
                    onclick: move |_| { em::toggle_submenu(sub); *revision.write() += 1; },
                    span { class: "spacer", "{label}" }
                    {icon_px("chev", 12)}
                }
            }
        };
        let submenu = match open {
            None => rsx! {},
            Some(sub) => {
                let size = match sub {
                    Submenu::Tab => tab_items.len(),
                    Submenu::Code => 1 + crate::ui::editor_language::MENU.len(),
                    Submenu::AutoSave => crate::ui::editor_states::AutoSave::ALL.len(),
                    Submenu::Keymap => Keymap::ALL.len(),
                };
                let top = em::submenu_top_fitted(&rows, sub, size).unwrap_or(em::TOP);
                let style = em::submenu_style(top);
                let body = match sub {
                    Submenu::Tab => {
                        let tab_path = current_tab.clone().unwrap_or_default();
                        rsx! {
                            for (item, enabled) in tab_items.into_iter() {
                                {
                                    let s = s.clone();
                                    let path = tab_path.clone();
                                    let mut menu_open = s.menu_open;
                                    let aria_disabled = (!enabled).to_string();
                                    rsx! {
                                        button { role: "menuitem", disabled: !enabled, "aria-disabled": "{aria_disabled}",
                                            onclick: move |_| {
                                                menu_open.set(false);
                                                if enabled { run_tab_menu_item(&s, item, &path); }
                                            },
                                            span { class: "spacer", "{item.label()}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Submenu::Code => rsx! {
                        {
                            let s = s.clone();
                            let label = Action::Format.label();
                            let hint = keymap.hint(Action::Format, os);
                            let mut menu_open = s.menu_open;
                            rsx! {
                                button { key: "{label}", role: "menuitem", onclick: move |_| { menu_open.set(false); run_action(&s, Action::Format); },
                                    span { class: "spacer", "{label}" }
                                    if !hint.is_empty() { span { class: "ed-msc", "{hint}" } }
                                }
                            }
                        }
                        // The language features are in the menu too: a key pressed while the code has the focus may not reach the page.
                        // TODO(compose-rust#554): the chords alone are enough once the editor previews keys.
                        for (action, label, hint) in crate::ui::editor_language::MENU {
                            {
                                let s = s.clone();
                                let mut menu_open = s.menu_open;
                                rsx! {
                                    button { key: "{label}", role: "menuitem", onclick: move |_| { menu_open.set(false); crate::ui::editor_language::view::run(&s, action); },
                                        span { class: "spacer", "{label}" }
                                        span { class: "ed-msc", "{hint}" }
                                    }
                                }
                            }
                        }
                    },
                    Submenu::AutoSave => crate::ui::editor_states::view::menu_items(s),
                    Submenu::Keymap => rsx! {
                        for k in Keymap::ALL {
                            {
                                let mut menu_open = s.menu_open;
                                let mut keymap_sig = s.keymap;
                                let selected = k == keymap;
                                let hint = k.hint(Action::Find, os);
                                rsx! {
                                    button { key: "{k.label()}", role: "menuitemradio", "aria-checked": "{selected}", onclick: move |_| {
                                        menu_open.set(false);
                                        keymap_sig.set(k);
                                        crate::services::services().ui.update_prefs(|p| p.keymap = k);
                                    },
                                        span { class: "ed-mchk", if selected { {icon_px("check", 14)} } }
                                        span { class: "spacer", "{k.label()}" }
                                        span { class: "ed-msc", "{hint}" }
                                    }
                                }
                            }
                        }
                    },
                };
                rsx! { div { class: "ed-menu ed-mainmenu", role: "menu", style: "{style}", {body} } }
            }
        };
        let menu_style = em::menu_style();
        // The mockup's items, a separator, then Ember's (owner placement rule 2026-11-13, rule 5).
        let entries = em::entries(current_tab.is_some());
        let item = |top: em::TopItem| -> Element {
            use em::TopItem as T;
            let s = s.clone();
            let mut menu_open = s.menu_open;
            let (label, hint): (String, String) = match top {
                T::Save => (Action::Save.label().to_string(), keymap.hint(Action::Save, os)),
                T::Recent => (Action::Recent.label().to_string(), keymap.hint(Action::Recent, os)),
                T::GotoLine => (Action::GotoLine.label().to_string(), keymap.hint(Action::GotoLine, os)),
                T::CloseOthers => ("다른 탭 모두 닫기".to_string(), String::new()),
                T::Palette => (keymap.palette_label().to_string(), keymap.hint(Action::Palette, os)),
                T::FindInFile => (Action::FindInFile.label().to_string(), keymap.hint(Action::FindInFile, os)),
                T::ReopenClosed => (Action::ReopenClosed.label().to_string(), keymap.hint(Action::ReopenClosed, os)),
                T::Settings => (Action::Settings.label().to_string(), keymap.hint(Action::Settings, os)),
                T::CloseEditor => ("에디터 닫기".to_string(), String::new()),
            };
            let tab_path = current_tab.clone();
            rsx! {
                button { key: "{label}", role: "menuitem", onclick: move |_| {
                    menu_open.set(false);
                    match top {
                        T::Save => run_action(&s, Action::Save),
                        T::Recent => run_action(&s, Action::Recent),
                        T::GotoLine => run_action(&s, Action::GotoLine),
                        T::CloseOthers => if let Some(path) = tab_path.as_deref() { run_tab_menu_item(&s, crate::ui::editor::TabMenuItem::CloseOthers, path); },
                        T::Palette => run_action(&s, Action::Palette),
                        T::FindInFile => run_action(&s, Action::FindInFile),
                        T::ReopenClosed => run_action(&s, Action::ReopenClosed),
                        T::Settings => run_action(&s, Action::Settings),
                        T::CloseEditor => crate::services::services().ui.close_editor(),
                    }
                },
                    span { class: "spacer", "{label}" }
                    if !hint.is_empty() { span { class: "ed-msc", "{hint}" } }
                }
            }
        };
        rsx! {
            div { class: "ed-menu ed-mainmenu", role: "menu", "aria-label": "편집기 메뉴", style: "{menu_style}",
                for entry in entries.into_iter() {
                    {
                        match entry {
                            em::Entry::Item(top) => item(top),
                            em::Entry::Group(Submenu::Code) => group(Submenu::Code, "코드".to_string()),
                            em::Entry::Group(Submenu::Keymap) => group(Submenu::Keymap, "단축키 체계".to_string()),
                            em::Entry::Group(Submenu::AutoSave) => group(Submenu::AutoSave, "자동 저장".to_string()),
                            em::Entry::Group(Submenu::Tab) => group(Submenu::Tab, tab_label.clone()),
                            em::Entry::Sep => rsx! { div { class: "ed-msep" } },
                        }
                    }
                }
            }
            {submenu}
        }
    }

    fn stripe_button(s: &Shared, w: ToolWindow, selected: bool, keymap: Keymap) -> Element {
        let mut stripes = s.stripes;
        let scm_s = s.clone();
        // 알림 opens the notification center, an overlay as in Code-OSS, not a side window (FR-L31.16).
        let selected = if w == ToolWindow::Notifications { s.notifications.borrow().center_open() } else { selected };
        let center_s = s.clone();
        let hint = w.action().map(|a| keymap.hint(a, Os::current())).unwrap_or_default();
        let tip = tooltip_text(w.label(), &hint);
        let class = if selected { "ed-sb on" } else { "ed-sb" };
        rsx! {
            button { key: "{w.label()}", class, title: "{tip}", "aria-label": "{tip}", "aria-pressed": "{selected}", onclick: move |_| {
                if w == ToolWindow::Notifications { crate::ui::editor_notifications::view::toggle_center(&center_s); }
                else if w.side() == Side::Right { notice(crate::ui::editor_actions::tool_window_notice(w)); } else { stripes.write().toggle(w); }
                if matches!(w, ToolWindow::Commit | ToolWindow::Git) && stripes.read().is_selected(w) { scm_s.scm.refresh(); }
            }, {icon(icon_name(w))} }
        }
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Strip { Left, Bottom, Right }

    fn icon_name(w: ToolWindow) -> &'static str {
        match w {
            ToolWindow::Project => "folder",
            ToolWindow::Search => "search",
            ToolWindow::Commit => "commit",
            ToolWindow::Git => "git",
            ToolWindow::Extensions => "blocks",
            ToolWindow::Agent => "spark",
            ToolWindow::Notifications => "bell",
            ToolWindow::Terminal => "code",
            ToolWindow::Run => "play",
            ToolWindow::Problems => "warn",
            ToolWindow::Output => "code",
        }
    }

    fn stripe_column(s: &Shared, sel: Stripes, keymap: Keymap, strip: Strip) -> Element {
        let (class, windows): (&str, &[ToolWindow]) = match strip {
            Strip::Left => ("ed-stripe ed-stripe-l", &ToolWindow::LEFT_STRIPE),
            Strip::Bottom => ("ed-stripe ed-stripe-b", &ToolWindow::BOTTOM_STRIPE),
            Strip::Right => ("ed-stripe ed-stripe-r", &ToolWindow::RIGHT),
        };
        rsx! {
            nav { class, "aria-label": "도구 창",
                for w in windows.iter().copied() { {stripe_button(s, w, sel.is_selected(w), keymap)} }
            }
        }
    }

    fn side_note(w: ToolWindow, area: &str) -> Element {
        let class = format!("ed-tw {area}");
        rsx! {
            section { class, "aria-label": "{w.label()}",
                div { class: "ed-twh", b { class: "ed-twt", "{w.label()}" } }
                div { style: "padding:12px;",
                    div { style: "color:var(--text-2);", "{SOON}" }
                    div { style: "color:var(--muted); padding-top:6px; font-size:var(--fs-2xs);", "서버가 알려 주면 이곳에 표시됩니다." }
                }
            }
        }
    }

    fn project_tree(s: &Shared, current: Option<&str>) -> Element {
        use crate::ui::editor::TreePrompt;
        let mut tabs = s.tabs;
        let mut expanded = s.expanded;
        let mut stripes = s.stripes;
        let mut tree_sel = s.tree_sel;
        let mut tree_multi_sel = s.tree_multi_sel;
        let mut tree_clipboard = s.tree_clipboard;
        let mut tree_drag_src = s.tree_drag_src;
        let mut prompt = s.prompt;
        let mut prompt_text = s.prompt_text;
        let mut prompt_at = s.prompt_at;
        let unsaved = super::unsaved_open_paths(s);
        let root = s.folder.clone();
        let rows = {
            let state = s.host.state.lock().unwrap();
            // `files.exclude` hides rows here, so everything below works on what is shown (FR-L31.17).
            crate::ui::editor_settings::visible_tree_rows(
                crate::ui::editor::tree_rows(&root, &state.listings, &expanded.read()),
                &root,
                &state.settings.exclude_matcher(),
            )
        };
        let root_name = base_name(&root).to_string();
        let selected: Option<(String, bool)> = tree_sel.read().clone();
        let tree_rows_owned = rows.clone();
        let unsaved_for_key = unsaved.clone();
        let tree_root = root.clone();
        let tree_host = s.host.clone();
        let tree_open_errors = s.open_errors.clone();
        let pending = prompt.read().clone();
        let parent_for_new = crate::ui::editor::new_item_parent(selected.as_ref().map(|(p, d)| (p.as_str(), *d)), &root);
        let (new_file_parent, new_dir_parent) = (parent_for_new.clone(), parent_for_new);
        let submit_host = s.host.clone();
        let submit_root = root.clone();
        let mut submit = move |pending: TreePrompt| {
            let name = prompt_text.read().trim().to_string();
            match pending {
                TreePrompt::NewFile { parent } => {
                    expanded.write().insert(parent.clone());
                    submit_host.send(HostCommand::CreateFile { parent, name });
                }
                TreePrompt::NewDir { parent } => {
                    expanded.write().insert(parent.clone());
                    submit_host.send(HostCommand::CreateDir { parent, name });
                }
                TreePrompt::Rename { path } => { submit_host.send(HostCommand::Rename { path, new_name: name }); }
                TreePrompt::Delete { path, is_dir } => { submit_host.send(HostCommand::Delete { path, is_dir }); }
                TreePrompt::DeleteMany { items } => { submit_host.send(HostCommand::DeleteMany { items }); }
            }
            let _ = &submit_root;
            prompt.set(None);
            prompt_text.set(String::new());
        };
        let mut submit_enter = submit.clone();
        let mut submit_ok = submit.clone();
        // A rename edits the renamed row in place (#797); other prompts sit at the top of the tree.
        let rename_at = crate::ui::editor::rename_in_place_row(pending.as_ref(), &rows);
        let mut prompt_inline: Option<Element> = None;
        let prompt_row = match pending.clone() {
            None => rsx! {},
            Some(TreePrompt::Delete { path, is_dir }) => {
                let affected: Vec<String> = unsaved.iter().filter(|p| crate::ui::editor::is_at_or_below(p, &path)).cloned().collect();
                let message = crate::ui::editor::delete_confirmation(&path, &root, is_dir, &affected);
                let confirm = TreePrompt::Delete { path, is_dir };
                // A deletion cannot be undone: the answer that is safe is the one a stray key or an early
                // double click gives. Cancel is the default (Enter and Escape cancel), and a click on
                // the delete button is ignored for the first 600 ms after the question appears.
                rsx! {
                    div { class: "ed-tn", role: "alertdialog", "aria-label": "삭제 확인", tabindex: "0", style: "--d:0; height:auto; flex-wrap:wrap; padding:var(--sp2); white-space:normal; background:var(--c-red-soft); color:var(--c-red);",
                        onkeydown: move |event| { if matches!(event.key(), Key::Enter | Key::Escape) { event.stop_propagation(); prompt.set(None); } },
                        div { style: "flex:1 1 100%;", "{message}" }
                        button { class: "ed-pj", style: "border:1px solid var(--line); background:var(--surface); color:var(--text);", autofocus: true, onclick: move |_| prompt.set(None), "취소" }
                        button { class: "ed-pj", style: "background:var(--c-red); color:var(--ink-text);", onclick: move |_| {
                            if crate::ui::editor::delete_click_allowed(*prompt_at.read(), now_ms()) { submit_ok(confirm.clone()); }
                        }, "삭제" }
                    }
                }
            }
            Some(TreePrompt::DeleteMany { items }) => {
                let affected: Vec<String> = unsaved.iter().filter(|p| items.iter().any(|(ip, _)| crate::ui::editor::is_at_or_below(p, ip))).cloned().collect();
                let message = crate::ui::editor::delete_many_confirmation(&items, &root, &affected);
                let confirm = TreePrompt::DeleteMany { items };
                rsx! {
                    div { class: "ed-tn", role: "alertdialog", "aria-label": "삭제 확인", tabindex: "0", style: "--d:0; height:auto; flex-wrap:wrap; padding:var(--sp2); white-space:normal; background:var(--c-red-soft); color:var(--c-red);",
                        onkeydown: move |event| { if matches!(event.key(), Key::Enter | Key::Escape) { event.stop_propagation(); prompt.set(None); } },
                        div { style: "flex:1 1 100%;", "{message}" }
                        button { class: "ed-pj", style: "border:1px solid var(--line); background:var(--surface); color:var(--text);", autofocus: true, onclick: move |_| prompt.set(None), "취소" }
                        button { class: "ed-pj", style: "background:var(--c-red); color:var(--ink-text);", onclick: move |_| {
                            if crate::ui::editor::delete_click_allowed(*prompt_at.read(), now_ms()) { submit_ok(confirm.clone()); }
                        }, "삭제" }
                    }
                }
            }
            Some(other) => {
                let label = match &other { TreePrompt::NewFile { .. } => "새 파일 이름", TreePrompt::NewDir { .. } => "새 폴더 이름", _ => "새 이름" };
                let enter = other.clone();
                let field = rsx! {
                        input { style: "flex:1; min-width:0; height:22px; border:1px solid var(--line); border-radius:var(--r-xs); background:var(--surface); color:var(--text); padding:0 6px;",
                            "aria-label": "{label}", placeholder: "{label}", value: "{prompt_text}", autofocus: true,
                            // In place on a row, a click in the field must not select, open or toggle the row.
                            onmousedown: move |event| event.stop_propagation(),
                            onclick: move |event| event.stop_propagation(),
                            oninput: move |event| prompt_text.set(event.value()),
                            onblur: move |_| { if crate::ui::editor::blur_closes_prompt(&prompt_text.read()) { prompt.set(None); } },
                            onkeydown: move |event| {
                                match event.key() {
                                    Key::Enter => { event.stop_propagation(); submit_enter(enter.clone()); }
                                    Key::Escape => { event.stop_propagation(); prompt.set(None); }
                                    _ => {}
                                }
                            },
                        }
                };
                if rename_at.is_some() {
                    prompt_inline = Some(field);
                    rsx! {}
                } else {
                    rsx! {
                        div { class: "ed-tn", style: "--d:0;",
                            {icon_px("file", 15)}
                            {field}
                        }
                    }
                }
            }
        };
        // The tree's menus (owner placement rule 2026-11-13, rules 1 and 2): the header "+" menu and the
        // context menu share one item list and one handler.
        use crate::ui::editor_tree_menu::{self as tm, TreeMenuEntry, TreeMenuItem};
        let tree_menu_state = tm::TreeMenuState {
            has_selection: selected.is_some() || !tree_multi_sel.read().is_empty(),
            has_clipboard: tree_clipboard.read().is_some(),
            tab_open: selected.as_ref().is_some_and(|(p, d)| !*d && s.tabs.read().paths().contains(&p.as_str())),
        };
        let run_host = s.host.clone();
        let run_root = root.clone();
        let run_rows = rows.clone();
        let run_unsaved = unsaved.clone();
        let run_s = s.clone();
        let run_selected = selected.clone();
        let run_tree_item = move |item: TreeMenuItem| {
            let items = {
                let sel = tree_multi_sel.read();
                if sel.is_empty() { run_selected.clone().into_iter().collect::<Vec<_>>() } else { sel.clone() }
            };
            let pruned = crate::ui::editor::prune_redundant_selection(&items);
            match item {
                TreeMenuItem::NewFile => { prompt_text.set(String::new()); prompt.set(Some(TreePrompt::NewFile { parent: new_file_parent.clone() })); }
                TreeMenuItem::NewFolder => { prompt_text.set(String::new()); prompt.set(Some(TreePrompt::NewDir { parent: new_dir_parent.clone() })); }
                TreeMenuItem::Cut | TreeMenuItem::Copy => {
                    if !pruned.is_empty() {
                        let n = pruned.len();
                        let cut = item == TreeMenuItem::Cut;
                        let kind = if cut { crate::ui::editor::ClipboardKind::Cut } else { crate::ui::editor::ClipboardKind::Copy };
                        tree_clipboard.set(Some(crate::ui::editor::TreeClipboard { kind, paths: pruned }));
                        notice(if cut { format!("{n}개 항목을 잘라냈습니다") } else { format!("{n}개 항목을 복사했습니다") });
                    }
                }
                TreeMenuItem::Paste => {
                    let clip = tree_clipboard.read().clone();
                    if let Some(clip) = clip {
                        let dest_dir = crate::ui::editor::new_item_parent(run_selected.as_ref().map(|(p, d)| (p.as_str(), *d)), &run_root);
                        let sources: Vec<String> = clip.paths.into_iter().map(|(p, _)| p).collect();
                        expanded.write().insert(dest_dir.clone());
                        match clip.kind {
                            crate::ui::editor::ClipboardKind::Copy => {
                                run_host.send(HostCommand::CopyPaths { sources, dest_dir });
                            }
                            crate::ui::editor::ClipboardKind::Cut => {
                                run_host.send(HostCommand::MovePaths { sources, dest_dir });
                                tree_clipboard.set(None);
                            }
                        }
                    }
                }
                TreeMenuItem::Rename => {
                    if let Some((path, _)) = run_selected.clone() {
                        let blocked: Vec<String> = run_unsaved.iter().filter(|u| crate::ui::editor::is_at_or_below(u, &path)).cloned().collect();
                        if !blocked.is_empty() {
                            notice(format!("저장하지 않은 변경이 있어 이름을 바꿀 수 없습니다. 먼저 저장하세요: {}", blocked.iter().map(|u| base_name(u).to_string()).collect::<Vec<_>>().join(", ")));
                        } else {
                            prompt_text.set(base_name(&path).to_string());
                            prompt.set(Some(TreePrompt::Rename { path }));
                        }
                    }
                }
                TreeMenuItem::Delete => {
                    if !pruned.is_empty() {
                        prompt_at.set(now_ms());
                        if pruned.len() == 1 {
                            prompt.set(Some(TreePrompt::Delete { path: pruned[0].0.clone(), is_dir: pruned[0].1 }));
                        } else {
                            prompt.set(Some(TreePrompt::DeleteMany { items: pruned }));
                        }
                    }
                }
                TreeMenuItem::CloseTab => {
                    if let Some((path, false)) = run_selected.clone() {
                        run_tab_menu_item(&run_s, crate::ui::editor::TabMenuItem::Close, &path);
                    }
                }
                TreeMenuItem::SelectAll => {
                    let (all, last) = crate::ui::editor::select_all_tree_rows(&run_rows);
                    tree_multi_sel.set(all);
                    tree_sel.set(last);
                }
            }
        };
        let menu_at = tm::open_menu();
        let plus_open = menu_at == Some(tm::TreeMenuAt::Plus);
        let mut tree_revision = s.revision;
        let mut key_revision = s.revision;
        let tree_menu = match menu_at {
            None => rsx! {},
            Some(at) => {
                let entries = at.entries(tree_menu_state);
                let style = at.style();
                let mac = matches!(Os::current(), Os::Mac);
                let mut scrim_rev = s.revision;
                let mut scrim_rev2 = s.revision;
                rsx! {
                    div { class: "ed-scrim", style: "z-index:40;",
                        onclick: move |_| { tm::close(); *scrim_rev.write() += 1; },
                        oncontextmenu: move |event| { event.prevent_default(); tm::close(); *scrim_rev2.write() += 1; } }
                    div { class: "ed-menu", role: "menu", "aria-label": "프로젝트 메뉴", style: "{style}",
                        for (i, entry) in entries.into_iter().enumerate() {
                            {
                                match entry {
                                    TreeMenuEntry::Sep => rsx! { div { key: "sep{i}", class: "ed-msep" } },
                                    TreeMenuEntry::Heading(h) => rsx! { div { key: "h{i}", class: "ed-mlab", "{h}" } },
                                    TreeMenuEntry::Item(item, enabled) => {
                                        let mut run = run_tree_item.clone();
                                        let hint = item.hint(mac);
                                        let aria_disabled = (!enabled).to_string();
                                        let mut rev = s.revision;
                                        rsx! {
                                            button { key: "{item.label()}", role: "menuitem", "aria-label": "{item.label()}", disabled: !enabled, "aria-disabled": "{aria_disabled}",
                                                onclick: move |_| { tm::close(); *rev.write() += 1; if enabled { run(item); } },
                                                span { class: "spacer", "{item.label()}" }
                                                if !hint.is_empty() { span { class: "ed-msc", "{hint}" } }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        };
        let paste_selected = selected.clone();
        let paste_host = s.host.clone();
        let paste_root = root.clone();
        let clip_state = tree_clipboard.read().clone();
        let clip_badge = crate::ui::editor::clipboard_badge_text(clip_state.as_ref());
        let key_host = s.host.clone();
        let root_for_key = root.clone();
        let root_for_drop = root.clone();
        let drop_root_host = s.host.clone();
        let (tree_container_ti, tree_root_ti, tree_row_tis) = crate::ui::editor::tree_roving_tabindex(
            &rows,
            selected.as_ref().map(|(p, d)| (p.as_str(), *d)),
            &tree_multi_sel.read(),
        );
        let reveal_current = current.map(str::to_string);
        let reveal_root = root.clone();
        let reveal_host = s.host.clone();
        let reveal_disabled = reveal_current.is_none();
        let modified = s.scm.modified_paths(&root);
        rsx! {
            section { class: "ed-tw ed-left", "aria-label": "프로젝트",
                // The mockup's three actions and one "+" (owner placement rule 2026-11-13, rule 1).
                div { class: "ed-twh",
                    b { class: "ed-twt", "프로젝트" }
                    div { class: "spacer" }
                    button { class: "ed-ib", title: "추가", "aria-label": "추가", "aria-haspopup": "menu", "aria-expanded": "{plus_open}", onclick: move |_| {
                        crate::ui::editor_tree_menu::toggle_plus();
                        *tree_revision.write() += 1;
                    }, {icon("plus")} }
                    button { class: "ed-ib", title: "모두 접기", "aria-label": "모두 접기", onclick: move |_| expanded.write().clear(), {icon_px("chev", 16)} }
                    button { class: "ed-ib", title: "열린 파일 찾기", "aria-label": "열린 파일 찾기", disabled: reveal_disabled, onclick: move |_| {
                        if let Some(cur_path) = reveal_current.clone() {
                            for dir in crate::ui::editor::ancestor_dirs_to_expand(&reveal_root, &cur_path) {
                                expanded.write().insert(dir.clone());
                                reveal_host.send(HostCommand::List(dir));
                            }
                            tree_multi_sel.set(vec![(cur_path.clone(), false)]);
                            tree_sel.set(Some((cur_path, false)));
                        }
                    }, {icon("search")} }
                    button { class: "ed-ib", title: "숨기기", "aria-label": "숨기기", onclick: move |_| stripes.write().hide(Side::Left), {icon("close")} }
                }
                {tree_menu}
                div { class: "ed-tree", role: "tree", "aria-label": "프로젝트 트리", "aria-multiselectable": "true", tabindex: "{tree_container_ti}",
                    onkeydown: move |event| {
                        if !crate::ui::editor::tree_handles_key(prompt.read().as_ref()) {
                            return;
                        }
                        // Shift+F10 stands in for right-click. v0.0.7: #417 fixed; workaround kept until checked on a display (dioxus-compose has to forward compose-rust's pointer events as oncontextmenu and ondoubleclick).
                        // compose-rust#100: v0.0.6 sent only Enter as a key event (#832); recheck on v0.0.7
                        // whether Shift+F10 now reaches this branch.
                        {
                            let m = event.modifiers();
                            if crate::ui::editor::tab_menu_key(&key_name(&event.key()), m.contains(Modifiers::CONTROL), m.contains(Modifiers::SHIFT), m.contains(Modifiers::ALT), m.contains(Modifiers::META)) {
                                event.stop_propagation();
                                event.prevent_default();
                                crate::ui::editor_tree_menu::open(crate::ui::editor_tree_menu::TreeMenuAt::Keyboard);
                                *key_revision.write() += 1;
                                return;
                            }
                        }
                        if event.key() == Key::Escape {
                            event.stop_propagation();
                            if let Some(current_path) = tabs.read().current_path() {
                                tree_host.send(HostCommand::Focus(current_path.to_string()));
                            }
                            return;
                        }
                        let m = event.modifiers();
                        let cmd = m.contains(Modifiers::META) || m.contains(Modifiers::CONTROL);
                        if cmd {
                            match event.key() {
                                Key::Character(ref c) if c.eq_ignore_ascii_case("c") => {
                                    event.stop_propagation();
                                    event.prevent_default();
                                    let sel = tree_multi_sel.read();
                                    let items = if sel.is_empty() {
                                        selected.clone().into_iter().collect::<Vec<_>>()
                                    } else {
                                        sel.clone()
                                    };
                                    let pruned = crate::ui::editor::prune_redundant_selection(&items);
                                    if !pruned.is_empty() {
                                        let n = pruned.len();
                                        tree_clipboard.set(Some(crate::ui::editor::TreeClipboard {
                                            kind: crate::ui::editor::ClipboardKind::Copy,
                                            paths: pruned,
                                        }));
                                        notice(format!("{n}개 항목을 복사했습니다"));
                                    }
                                    return;
                                }
                                Key::Character(ref c) if c.eq_ignore_ascii_case("x") => {
                                    event.stop_propagation();
                                    event.prevent_default();
                                    let sel = tree_multi_sel.read();
                                    let items = if sel.is_empty() {
                                        selected.clone().into_iter().collect::<Vec<_>>()
                                    } else {
                                        sel.clone()
                                    };
                                    let pruned = crate::ui::editor::prune_redundant_selection(&items);
                                    if !pruned.is_empty() {
                                        let n = pruned.len();
                                        tree_clipboard.set(Some(crate::ui::editor::TreeClipboard {
                                            kind: crate::ui::editor::ClipboardKind::Cut,
                                            paths: pruned,
                                        }));
                                        notice(format!("{n}개 항목을 잘라냈습니다"));
                                    }
                                    return;
                                }
                                Key::Character(ref c) if c.eq_ignore_ascii_case("v") => {
                                    event.stop_propagation();
                                    event.prevent_default();
                                    let clip = tree_clipboard.read().clone();
                                    if let Some(clip) = clip {
                                        let dest_dir = crate::ui::editor::new_item_parent(selected.as_ref().map(|(p, d)| (p.as_str(), *d)), &root_for_key);
                                        let sources: Vec<String> = clip.paths.into_iter().map(|(p, _)| p).collect();
                                        expanded.write().insert(dest_dir.clone());
                                        match clip.kind {
                                            crate::ui::editor::ClipboardKind::Copy => {
                                                key_host.send(HostCommand::CopyPaths { sources, dest_dir });
                                            }
                                            crate::ui::editor::ClipboardKind::Cut => {
                                                key_host.send(HostCommand::MovePaths { sources, dest_dir });
                                                tree_clipboard.set(None);
                                            }
                                        }
                                    }
                                    return;
                                }
                                _ => {}
                            }
                        } else if matches!(event.key(), Key::Delete | Key::Backspace) {
                            let sel = tree_multi_sel.read();
                            let items = if sel.is_empty() {
                                selected.clone().into_iter().collect::<Vec<_>>()
                            } else {
                                sel.clone()
                            };
                            let pruned = crate::ui::editor::prune_redundant_selection(&items);
                            if !pruned.is_empty() {
                                event.stop_propagation();
                                event.prevent_default();
                                prompt_at.set(now_ms());
                                if pruned.len() == 1 {
                                    prompt.set(Some(TreePrompt::Delete { path: pruned[0].0.clone(), is_dir: pruned[0].1 }));
                                } else {
                                    prompt.set(Some(TreePrompt::DeleteMany { items: pruned }));
                                }
                            }
                            return;
                        }
                        let key_str = key_name(&event.key());
                        let cur_sel = tree_sel.read().clone();
                        let outcome = crate::ui::editor::tree_key_action(
                            &tree_rows_owned,
                            cur_sel.as_ref().map(|(p, d)| (p.as_str(), *d)),
                            &tree_root,
                            &key_str,
                        );
                        if let Some(action) = outcome {
                            event.stop_propagation();
                            match action {
                                crate::ui::editor::TreeKeyOutcome::Select(p, is_dir) => {
                                    tree_multi_sel.set(vec![(p.clone(), is_dir)]);
                                    tree_sel.set(Some((p, is_dir)));
                                }
                                crate::ui::editor::TreeKeyOutcome::Expand(p) => {
                                    expanded.write().insert(p.clone());
                                    tree_host.send(HostCommand::List(p));
                                }
                                crate::ui::editor::TreeKeyOutcome::Collapse(p) => {
                                    expanded.write().remove(&p);
                                }
                                crate::ui::editor::TreeKeyOutcome::OpenFile(p) => {
                                    tree_open_errors.borrow_mut().remove(&p);
                                    tabs.write().open(&p);
                                    tree_host.send(HostCommand::Open(p.clone()));
                                    tree_host.send(HostCommand::Focus(p));
                                }
                                crate::ui::editor::TreeKeyOutcome::Rename(p) => {
                                    let blocked: Vec<String> = unsaved_for_key.iter().filter(|u| crate::ui::editor::is_at_or_below(u, &p)).cloned().collect();
                                    if !blocked.is_empty() {
                                        notice(format!("저장하지 않은 변경이 있어 이름을 바꿀 수 없습니다. 먼저 저장하세요: {}", blocked.iter().map(|u| base_name(u).to_string()).collect::<Vec<_>>().join(", ")));
                                    } else {
                                        prompt_text.set(base_name(&p).to_string());
                                        prompt.set(Some(TreePrompt::Rename { path: p }));
                                    }
                                }
                            }
                        }
                    },
                    div { class: "ed-tn", role: "treeitem", "aria-expanded": "true", "aria-label": "{root_name} 폴더", tabindex: "{tree_root_ti}", style: "--d:0;",
                        ondragover: move |event| { event.prevent_default(); },
                        ondrop: move |event| {
                            event.prevent_default();
                            let drag_src = tree_drag_src.read().clone();
                            if let Some(src) = drag_src {
                                let dest_dir = root_for_drop.clone();
                                let multi = tree_multi_sel.read().clone();
                                let sources = if multi.iter().any(|(p, _)| p == &src) {
                                    crate::ui::editor::prune_redundant_selection(&multi)
                                        .into_iter()
                                        .map(|(p, _)| p)
                                        .filter(|p| p != &dest_dir)
                                        .collect()
                                } else if src != dest_dir {
                                    vec![src]
                                } else {
                                    Vec::new()
                                };
                                if !sources.is_empty() {
                                    drop_root_host.send(HostCommand::MovePaths { sources, dest_dir });
                                }
                                tree_drag_src.set(None);
                            }
                        },
                        {icon_px("chev-dn", 12)} {icon_px("folder", 15)} span { class: "ellipsis", b { "{root_name}" } }
                    }
                    if let Some(badge) = clip_badge {
                        div { class: "ed-tn", style: "--d:0; padding:0 var(--sp2); background:var(--surface-2);",
                            span { class: "ed-sc", style: "flex:none;", "{badge}" }
                            div { class: "spacer" }
                            button { class: "ed-sc", title: "붙여넣기", "aria-label": "붙여넣기", style: "flex:none; cursor:pointer; color:var(--text);", onclick: move |event| {
                                event.stop_propagation();
                                let clip = tree_clipboard.read().clone();
                                if let Some(clip) = clip {
                                    let dest_dir = crate::ui::editor::new_item_parent(paste_selected.as_ref().map(|(p, d)| (p.as_str(), *d)), &paste_root);
                                    let sources: Vec<String> = clip.paths.into_iter().map(|(p, _)| p).collect();
                                    expanded.write().insert(dest_dir.clone());
                                    match clip.kind {
                                        crate::ui::editor::ClipboardKind::Copy => {
                                            paste_host.send(HostCommand::CopyPaths { sources, dest_dir });
                                        }
                                        crate::ui::editor::ClipboardKind::Cut => {
                                            paste_host.send(HostCommand::MovePaths { sources, dest_dir });
                                            tree_clipboard.set(None);
                                        }
                                    }
                                }
                            }, "붙여넣기" }
                            button { class: "ed-ib", title: "클립보드 비우기", "aria-label": "클립보드 비우기", style: "width:20px; height:20px; flex-shrink:0;", onclick: move |event| {
                                event.stop_propagation();
                                tree_clipboard.set(None);
                            }, {icon_px("close", 12)} }
                        }
                    }
                    {prompt_row}
                    for (row_idx, row) in rows.iter().enumerate() {
                        {
                            let row_ti = tree_row_tis.get(row_idx).copied().unwrap_or("-1");
                            let path = row.path.clone();
                            let host = s.host.clone();
                            let mut recent_files = s.recent_files;
                            let is_dir = row.is_dir;
                            let in_multi = tree_multi_sel.read().iter().any(|(p, _)| p == &row.path);
                            let in_cut = tree_clipboard.read().as_ref().is_some_and(|c| c.kind == crate::ui::editor::ClipboardKind::Cut && c.paths.iter().any(|(p, _)| p == &row.path));
                            let chosen = in_multi || selected.as_ref().map(|(p, _)| p.as_str()) == Some(row.path.as_str());
                            let active_row = chosen || (selected.is_none() && tree_multi_sel.read().is_empty() && current == Some(row.path.as_str()));
                            let is_mod = !is_dir && modified.contains(&row.path);
                            let class = crate::ui::editor::tree_row_class(active_row, is_mod);
                            let depth = row.depth + 1;
                            let row_style = if in_cut { format!("--d:{depth}; opacity:0.55;") } else { format!("--d:{depth};") };
                            let ctx_path = row.path.clone();
                            let mut ctx_revision = s.revision;
                            let row_label = crate::ui::editor::tree_item_aria_label(&row.name, is_dir);
                            let path_for_down = row.path.clone();
                            let rows_for_down = rows.clone();
                            let rows_for_click = rows.clone();
                            let preview_s = s.clone();
                            let keep_s = s.clone();
                            let keep_path = row.path.clone();
                            let drag_path = row.path.clone();
                            let drop_path = row.path.clone();
                            let drop_is_dir = is_dir;
                            let drop_host = s.host.clone();
                            let drop_root = root.clone();
                            rsx! { div { key: "{row.path}", class, role: "treeitem", "aria-label": "{row_label}", "aria-selected": "{chosen}", tabindex: "{row_ti}", style: "{row_style}",
                                "aria-expanded": if is_dir { "{row.open}" } else { "" },
                                draggable: "true",
                                ondragstart: move |_| {
                                    tree_drag_src.set(Some(drag_path.clone()));
                                },
                                ondragover: move |event| {
                                    event.prevent_default();
                                },
                                ondrop: move |event| {
                                    event.prevent_default();
                                    let drag_src = tree_drag_src.read().clone();
                                    if let Some(src) = drag_src {
                                        let dest_dir = if drop_is_dir {
                                            drop_path.clone()
                                        } else {
                                            crate::ui::editor::new_item_parent(Some((drop_path.as_str(), false)), &drop_root)
                                        };
                                        let multi = tree_multi_sel.read().clone();
                                        let sources = if multi.iter().any(|(p, _)| p == &src) {
                                            crate::ui::editor::prune_redundant_selection(&multi)
                                                .into_iter()
                                                .map(|(p, _)| p)
                                                .filter(|p| p != &dest_dir)
                                                .collect()
                                        } else if src != dest_dir {
                                            vec![src]
                                        } else {
                                            Vec::new()
                                        };
                                        if !sources.is_empty() {
                                            expanded.write().insert(dest_dir.clone());
                                            drop_host.send(HostCommand::MovePaths { sources, dest_dir });
                                        }
                                        tree_drag_src.set(None);
                                    }
                                },
                                onmousedown: move |event| {
                                    let close = prompt.read().as_ref().is_some_and(|p| crate::ui::editor::row_click_closes_prompt(p, &prompt_text.read()));
                                    if close { prompt.set(None); }
                                    let m = event.modifiers();
                                    let shift = m.contains(Modifiers::SHIFT);
                                    let toggle = m.contains(Modifiers::META) || m.contains(Modifiers::CONTROL);
                                    super::MODIFIER_CLICK.with(|c| c.set(shift || toggle));
                                    if shift || toggle {
                                        event.stop_propagation();
                                        let new_multi = crate::ui::editor::update_tree_selection(
                                            &tree_multi_sel.read(),
                                            (path_for_down.clone(), is_dir),
                                            &rows_for_down,
                                            shift,
                                            toggle,
                                        );
                                        let new_sel = new_multi.last().cloned().or_else(|| Some((path_for_down.clone(), is_dir)));
                                        tree_multi_sel.set(new_multi);
                                        tree_sel.set(new_sel);
                                    }
                                },
                                onclick: move |_| {
                                    if super::MODIFIER_CLICK.with(|c| c.replace(false)) {
                                        return;
                                    }
                                    let close = prompt.read().as_ref().is_some_and(|p| crate::ui::editor::row_click_closes_prompt(p, &prompt_text.read()));
                                    if close { prompt.set(None); }
                                    let new_multi = crate::ui::editor::update_tree_selection(
                                        &tree_multi_sel.read(),
                                        (path.clone(), is_dir),
                                        &rows_for_click,
                                        false,
                                        false,
                                    );
                                    let new_sel = new_multi.last().cloned().or_else(|| Some((path.clone(), is_dir)));
                                    tree_multi_sel.set(new_multi);
                                    tree_sel.set(new_sel);
                                    if is_dir {
                                        let was_open = expanded.read().contains(&path);
                                        if was_open { expanded.write().remove(&path); }
                                        else { expanded.write().insert(path.clone()); host.send(HostCommand::List(path.clone())); }
                                    } else {
                                        // A single click opens the preview tab; a double-click keeps it (FR-L31.5).
                                        open_preview(&preview_s, &path);
                                        // Two clicks within 500 ms stand in for `ondoubleclick`. v0.0.7: #417 fixed; workaround kept until checked on a display (dioxus-compose has to forward compose-rust's pointer events as oncontextmenu and ondoubleclick; its pin moves separately).
                                        if DOUBLE_CLICK.with(|d| d.borrow_mut().click(&format!("tree:{path}"), now_ms())) {
                                            let mut tabs = preview_s.tabs;
                                            tabs.write().keep_open(&path);
                                        }
                                        record_recent_file(&mut recent_files, &path);
                                    }
                                },
                                ondoubleclick: move |_| {
                                    if !is_dir {
                                        let mut tabs = keep_s.tabs;
                                        tabs.write().keep_open(&keep_path);
                                    }
                                },
                                // Right-click selects the row (keeping a multi-selection it is part of)
                                // and opens the tree's context menu (owner placement rule, rule 2).
                                oncontextmenu: move |event| {
                                    event.prevent_default();
                                    event.stop_propagation();
                                    if !tree_multi_sel.read().iter().any(|(p, _)| p == &ctx_path) {
                                        tree_multi_sel.set(vec![(ctx_path.clone(), is_dir)]);
                                    }
                                    tree_sel.set(Some((ctx_path.clone(), is_dir)));
                                    let at = event.client_coordinates();
                                    crate::ui::editor_tree_menu::open(crate::ui::editor_tree_menu::TreeMenuAt::Pointer { x: at.x, y: at.y });
                                    *ctx_revision.write() += 1;
                                },
                                if is_dir { if row.open { {icon_px("chev-dn", 12)} } else { {icon_px("chev", 12)} } } else { span { class: "chev-sp" } }
                                if is_dir { {icon_px("folder", 15)} } else { {icon_px("file", 15)} }
                                if rename_at == Some(row_idx) {
                                    {prompt_inline.take()}
                                } else {
                                    span { class: "ellipsis", "{row.name}" }
                                }
                            } }
                        }
                    }
                }
            }
        }
    }

    /// Whether a modal is up: the trust prompt, the unsaved-changes sheet or an extension's modal
    /// message (#831). Code-OSS's modal dialogs block the workbench, so the ⋯ menu stays closed.
    fn modal_up(s: &Shared) -> bool {
        crate::ui::workspace_trust::view::prompt_up(s) || s.close_confirm.peek().is_some() || s.notifications.borrow().modal().is_some()
    }

    fn tab_strip(s: &Shared, paths: &[String], dirty: &[bool], current: Option<&str>) -> Element {
        let mut tabs = s.tabs;
        let mut menu_open = s.menu_open;
        let host = s.host.clone();
        let scm = s.scm.clone();
        let strip_s = s.clone();
        let menu_s = s.clone();
        let modified = s.scm.modified_paths(&s.folder);
        let paths_owned: Vec<String> = paths.to_vec();
        let dirty_owned: Vec<bool> = dirty.to_vec();
        let current_owned: Option<String> = current.map(str::to_string);
        let (strip_ti, tab_tis) = crate::ui::editor::tab_strip_roving_tabindex(paths, current);
        let model = s.tabs.read().clone();
        rsx! {
            div { class: "ed-tabs", role: "tablist", "aria-label": "열린 파일", tabindex: "{strip_ti}",
                onkeydown: move |event| {
                    if event.key() == Key::Escape {
                        event.stop_propagation();
                        if let Some(ref current_path) = current_owned {
                            host.send(HostCommand::Focus(current_path.clone()));
                        }
                        return;
                    }
                    let key_str = key_name(&event.key());
                    let m = event.modifiers();
                    // Shift+F10 / ContextMenu opens the ⋯ menu, which holds the current tab's items, while
                    // the right-click menu cannot open. v0.0.7: #417 fixed; workaround kept until checked on a display (dioxus-compose has to forward compose-rust's pointer events as oncontextmenu and ondoubleclick; its pin moves separately).
                    if crate::ui::editor::tab_menu_key(
                        &key_str,
                        m.contains(Modifiers::CONTROL),
                        m.contains(Modifiers::SHIFT),
                        m.contains(Modifiers::ALT),
                        m.contains(Modifiers::META),
                    ) {
                        event.stop_propagation();
                        event.prevent_default();
                        if current_owned.is_some() {
                            let mut tab_menu = strip_s.tab_menu;
                            tab_menu.set(None);
                            menu_open.set(true);
                        }
                        return;
                    }
                    // Move Editor Left / Right (FR-L31.5): the reorder drag cannot do yet.
                    if let Some(right) = crate::ui::editor::tab_move_key(
                        &key_str,
                        m.contains(Modifiers::CONTROL),
                        m.contains(Modifiers::SHIFT),
                        m.contains(Modifiers::ALT),
                        m.contains(Modifiers::META),
                    ) {
                        event.stop_propagation();
                        if let Some(ref current_path) = current_owned {
                            let mut t = tabs.write();
                            if right { t.move_right(current_path); } else { t.move_left(current_path); }
                        }
                        return;
                    }
                    let outcome = crate::ui::editor::tab_strip_key_with_dirty(
                        &paths_owned,
                        &dirty_owned,
                        current_owned.as_deref(),
                        &key_str,
                    );
                    if let Some(action) = outcome {
                        event.stop_propagation();
                        match action {
                            crate::ui::editor::TabKeyOutcome::Activate(path) => {
                                scm.close_diff();
                                tabs.write().activate(&path);
                                host.send(HostCommand::Focus(path));
                            }
                            crate::ui::editor::TabKeyOutcome::Close(path) | crate::ui::editor::TabKeyOutcome::RefuseDirty(path) => {
                                // A dirty tab is not closed at once: the window asks, as Code-OSS does.
                                sync_dirty(&strip_s);
                                let request = strip_s.tabs.read().close_request(&path);
                                request_close(&strip_s, request);
                            }
                        }
                    }
                },
                for (i, path) in paths.iter().enumerate() {
                    {
                        let tab_ti = tab_tis.get(i).copied().unwrap_or("-1");
                        let path = path.clone(); let close_path = path.clone();
                        let close_path_key = path.clone();
                        let host = s.host.clone(); let key_host = s.host.clone();
                        let close_s = s.clone(); let close_key_s = s.clone();
                        let mut recent_files = s.recent_files;
                        let mut key_recent_files = s.recent_files;
                        let selected = current == Some(path.as_str());
                        let label = base_name(&path).to_string();
                        let deleted = s.uris.borrow().get(&path).and_then(|u| s.bridge.borrow().document(u).map(|d| crate::ui::editor_states::is_deleted(d.external_change))).unwrap_or(false);
                        let shown_label = crate::ui::editor_states::tab_label(&label, deleted);
                        let label_style = crate::ui::editor_states::tab_label_style(deleted);
                        let is_mod = modified.contains(&path);
                        let pinned = model.is_pinned(&path);
                        let preview = model.is_preview(&path);
                        let class = crate::ui::editor::tab_class_with(selected, is_mod, pinned, preview);
                        let tab_label = crate::ui::editor::tab_aria_label(&shown_label, dirty[i]);
                        let close_label = if pinned { format!("고정 해제: {label}") } else { format!("닫기: {label}") };
                        let scm = s.scm.clone();
                        let key_scm = s.scm.clone();
                        let tab_path = path.clone();
                        let keep_path = path.clone();
                        let menu_path = path.clone();
                        let mut tab_menu = s.tab_menu;
                        let mut top_menu = s.menu_open;
                        rsx! { div { class, role: "tab", "aria-selected": "{selected}", "aria-label": "{tab_label}", tabindex: "{tab_ti}", title: "{path}",
                            onclick: move |_| {
                                scm.close_diff();
                                tabs.write().activate(&path);
                                // Two clicks within 500 ms stand in for `ondoubleclick`. v0.0.7: #417 fixed; workaround kept until checked on a display (dioxus-compose has to forward compose-rust's pointer events as oncontextmenu and ondoubleclick; its pin moves separately).
                                if DOUBLE_CLICK.with(|d| d.borrow_mut().click(&format!("tab:{path}"), now_ms())) {
                                    tabs.write().keep_open(&path);
                                }
                                record_recent_file(&mut recent_files, &path);
                                host.send(HostCommand::Focus(path.clone()));
                            },
                            ondoubleclick: move |_| {
                                tabs.write().keep_open(&keep_path);
                            },
                            oncontextmenu: move |event| {
                                event.prevent_default();
                                event.stop_propagation();
                                let at = event.client_coordinates();
                                top_menu.set(false);
                                tab_menu.set(Some((menu_path.clone(), at.x, at.y)));
                            },
                            onkeydown: move |event| {
                                if matches!(event.key(), Key::Enter) || matches!(event.key(), Key::Character(ref c) if c == " ") {
                                    event.stop_propagation();
                                    key_scm.close_diff();
                                    tabs.write().activate(&tab_path);
                                    record_recent_file(&mut key_recent_files, &tab_path);
                                    key_host.send(HostCommand::Focus(tab_path.clone()));
                                }
                            },
                            {icon_px(if selected { "file-w" } else { "file" }, 14)}
                            span { class: "ellipsis", style: "{label_style}", "{shown_label}" }
                            if dirty[i] { span { class: "ed-dirty" } }
                            // A pinned tab shows the pin where the close button is; a click on it unpins, as in Code-OSS.
                            button {
                                class: "ed-tx",
                                tabindex: "-1",
                                title: "{close_label}",
                                "aria-label": "{close_label}",
                                onclick: move |event| {
                                    event.stop_propagation();
                                    if pinned {
                                        let mut t = close_s.tabs;
                                        t.write().unpin(&close_path);
                                        return;
                                    }
                                    sync_dirty(&close_s);
                                    let request = close_s.tabs.read().close_request(&close_path);
                                    request_close(&close_s, request);
                                },
                                onkeydown: move |event| {
                                    if matches!(event.key(), Key::Enter) || matches!(event.key(), Key::Character(ref c) if c == " ") {
                                        event.stop_propagation();
                                        if pinned {
                                            let mut t = close_key_s.tabs;
                                            t.write().unpin(&close_path_key);
                                            return;
                                        }
                                        sync_dirty(&close_key_s);
                                        let request = close_key_s.tabs.read().close_request(&close_path_key);
                                        request_close(&close_key_s, request);
                                    }
                                },
                                if pinned {
                                    {icon_px(if selected { "pin-w" } else { "pin" }, 12)}
                                } else {
                                    {icon_px(if selected { "close-w" } else { "close" }, 12)}
                                }
                            }
                        } }
                    }
                }
                div { class: "spacer" }
                button { class: "ed-ib", title: "탭 메뉴", "aria-label": "탭 메뉴", onclick: move |_| { crate::ui::editor_menu::close_submenu(); let open = *menu_open.read(); menu_open.set(crate::ui::editor_menu::toggled(open, modal_up(&menu_s))); }, {icon("more")} }
            }
        }
    }

    /// The context menu of a tab (FR-L31.5): Code-OSS's close, pin and keep-open items, and the move items
    /// that stand in for drag reorder.
    fn tab_context_menu(s: &Shared, path: &str, x: f64, y: f64) -> Element {
        use crate::ui::editor::TabMenuItem;
        let items = crate::ui::editor::tab_menu_items(&s.tabs.read(), path);
        // Code-OSS's groups: the close items | keep open and pin | (Ember) the move items.
        let seps: Vec<bool> = items
            .iter()
            .enumerate()
            .map(|(i, (item, _))| match item {
                TabMenuItem::KeepOpen | TabMenuItem::MoveLeft => true,
                TabMenuItem::Pin | TabMenuItem::Unpin => i == 0 || items[i - 1].0 != TabMenuItem::KeepOpen,
                _ => false,
            })
            .collect();
        let mut tab_menu = s.tab_menu;
        let style = format!("position:fixed; left:{x}px; top:{y}px; right:auto;");
        rsx! {
            div { class: "ed-scrim", onclick: move |_| tab_menu.set(None), oncontextmenu: move |event| { event.prevent_default(); tab_menu.set(None); } }
            div { class: "ed-menu ed-tabmenu", role: "menu", "aria-label": "탭 메뉴", style: "{style}",
                for (i, (item, enabled)) in items.into_iter().enumerate() {
                    {
                        let s = s.clone();
                        let path = path.to_string();
                        let mut tab_menu = s.tab_menu;
                        let sep = seps[i];
                        let aria_disabled = (!enabled).to_string();
                        rsx! {
                            if sep { div { class: "ed-msep" } }
                            button { role: "menuitem", disabled: !enabled, "aria-disabled": "{aria_disabled}",
                                onclick: move |_| {
                                    tab_menu.set(None);
                                    if enabled { run_tab_menu_item(&s, item, &path); }
                                },
                                span { class: "spacer", "{item.label()}" }
                            }
                        }
                    }
                }
            }
        }
    }

    fn run_tab_menu_item(s: &Shared, item: crate::ui::editor::TabMenuItem, path: &str) {
        use crate::ui::editor::TabMenuItem;
        let mut tabs = s.tabs;
        match item {
            TabMenuItem::KeepOpen => tabs.write().keep_open(path),
            TabMenuItem::Pin => tabs.write().pin(path),
            TabMenuItem::Unpin => tabs.write().unpin(path),
            TabMenuItem::MoveLeft => { tabs.write().move_left(path); }
            TabMenuItem::MoveRight => { tabs.write().move_right(path); }
            TabMenuItem::Close | TabMenuItem::CloseOthers | TabMenuItem::CloseRight | TabMenuItem::CloseAll => {
                sync_dirty(s);
                let request = {
                    let t = tabs.read();
                    match item {
                        TabMenuItem::Close => t.close_request(path),
                        TabMenuItem::CloseOthers => t.close_others_request(path),
                        TabMenuItem::CloseRight => t.close_right_request(path),
                        _ => t.close_all_request(),
                    }
                };
                if request.anchor.is_some() {
                    s.scm.close_diff();
                }
                request_close(s, request);
            }
        }
    }

    /// The unsaved-changes question on a close (FR-L31.5), worded and ordered as Code-OSS's dialog:
    /// 저장 (모두 저장 for several files), 저장 안 함, 취소. 취소 is the default and Escape answers it.
    fn close_confirm_sheet(s: &Shared, request: &crate::ui::editor::CloseRequest) -> Element {
        use crate::ui::editor::DirtyChoice;
        let title = request.confirm_title();
        let detail = request.confirm_detail();
        let save_label = request.save_label();
        let names = request.dirty_names();
        let (save_s, dont_s, cancel_s, key_s, scrim_s) = (s.clone(), s.clone(), s.clone(), s.clone(), s.clone());
        rsx! {
            div { class: "modal", role: "presentation",
                onclick: move |_| answer_close(&scrim_s, DirtyChoice::Cancel),
                onkeydown: move |event| {
                    if event.key() == Key::Escape {
                        event.stop_propagation();
                        answer_close(&key_s, DirtyChoice::Cancel);
                    }
                },
                div { class: "card ed-confirm", role: "alertdialog", "aria-modal": "true", "aria-label": "{title}",
                    onclick: move |event| event.stop_propagation(),
                    div { class: "card-b",
                        b { "{title}" }
                        if !names.is_empty() {
                            ul { class: "ed-confirm-files",
                                for name in names.iter() { li { key: "{name}", "{name}" } }
                            }
                        }
                        p { class: "muted", "{detail}" }
                        div { class: "ed-confirm-acts",
                            button { class: "btn primary sm", onclick: move |_| answer_close(&save_s, DirtyChoice::Save), "{save_label}" }
                            button { class: "btn sm", onclick: move |_| answer_close(&dont_s, DirtyChoice::DontSave), "저장 안 함" }
                            button { class: "btn sm", autofocus: true, onclick: move |_| answer_close(&cancel_s, DirtyChoice::Cancel), "취소" }
                        }
                    }
                }
            }
        }
    }

    fn breadcrumbs(s: &Shared, path: &str) -> Element {
        let segments = crate::ui::editor::breadcrumb_segments(path, &s.folder);
        let last = segments.len().saturating_sub(1);
        rsx! { div { class: "ed-crumbs", role: "navigation", "aria-label": "경로",
            for (i, seg) in segments.iter().enumerate() {
                if i > 0 { {icon_px("chev", 11)} }
                span { key: "{i}", class: if i == last { "cur" } else { "" }, "{seg}" }
            }
            {crate::ui::editor_language::view::symbol_crumbs(s, path)}
        } }
    }

    thread_local! {
        /// The terminal panel element, to read its laid-out size (FR-L31.20).
        static TERM_MOUNT: std::cell::RefCell<Option<std::rc::Rc<dioxus_elements::MountedData>>> = const { std::cell::RefCell::new(None) };
        static TERM_FIT: std::cell::RefCell<crate::ui::term_fit::PanelFit> = std::cell::RefCell::new(Default::default());
        /// IME composition state of the terminal panel (FR-L31.21): only committed text is sent.
        static TERMINAL_IME: std::cell::RefCell<crate::terminal_input::ImeInput> = std::cell::RefCell::new(crate::terminal_input::ImeInput::default());
    }

    /// Fits the PTY to the panel on every render (FR-L31.20, #334): the panel's size from the last
    /// layout, debounced, to [`TerminalHandle::resize`]. A held-back size asks for one more render.
    // TODO(dioxus-compose#207): dioxus-compose sends no `onresize` (its `convert_resize_data` is "not sent"), so a size
    // change is seen on the next render (panel drag, output, the wake below), not by an observer.
    fn fit_terminal(s: &Shared, handle: &TerminalHandle) {
        use futures_util::FutureExt;
        let rect = TERM_MOUNT.with(|m| {
            let m = m.borrow();
            let r = m.as_ref()?.get_client_rect().now_or_never()?.ok()?;
            Some((r.size.width as f32, r.size.height as f32))
        });
        let step = TERM_FIT.with(|f| f.borrow_mut().step(rect, std::time::Instant::now()));
        if let Some((cols, rows)) = step.send {
            handle.resize(cols, rows);
        }
        if let Some(after) = step.wake_after {
            let revision = s.revision;
            crate::services::services().rt.spawn(async move {
                tokio::time::sleep(after).await;
                let mut revision = revision;
                *revision.write() += 1;
            });
        }
    }

    /// The active terminal tab's attachment (FR-P6, FR-L31.23). The panel lists the folder's
    /// terminals on first show and reattaches to those the node kept. `None` when the editor was not
    /// given a server URL and a computer, or the list is not there yet.
    fn terminal_handle(s: &Shared) -> Option<TerminalHandle> {
        if !can_attach(&s.server_url, &s.computer) {
            return None;
        }
        let revision = s.revision;
        let on_change: ChangeCallback = Arc::new(move || {
            let mut revision = revision;
            *revision.write() += 1;
        });
        crate::ui::editor_terminals::ensure(&s.server_url, &s.computer, &s.folder, &s.project, on_change);
        crate::ui::editor_terminals::active_handle()
    }

    /// What the terminal panel shows for one state.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) struct PanelView {
        pub banner: Option<String>,
        pub lines: Vec<String>,
        pub notice: Option<String>,
    }

    pub(super) fn panel_view(state: &TerminalState) -> PanelView {
        PanelView { banner: state.banner(), lines: state.lines.clone(), notice: state.notice() }
    }

    /// The editor can attach only with both a server URL and a computer id.
    pub(crate) fn can_attach(server_url: &str, computer: &str) -> bool {
        !server_url.trim().is_empty() && !computer.trim().is_empty()
    }

    /// Bytes for a key press in the terminal. The Meta key is the app's, not the shell's.
    pub(crate) fn terminal_input(name: &str, ctrl: bool, alt: bool, shift: bool, meta: bool) -> Option<Vec<u8>> {
        if meta {
            return None;
        }
        terminal_stream::key_bytes(name, terminal_stream::Modifiers { ctrl, alt, shift })
    }

    /// True when the panel offers the take-control button: another client holds control.
    /// Control changes hands only through that button, never through typing.
    pub(super) fn offers_take_control(state: &TerminalState) -> bool {
        state.controlled_by_other()
    }

    /// True when keystrokes are sent to the node. While another client controls, the node
    /// would refuse them, so they are dropped here.
    pub(super) fn sends_input(state: &TerminalState) -> bool {
        !state.controlled_by_other()
    }

    /// The terminal tab strip (FR-L31.23): one tab per terminal of the folder with switch, rename
    /// (double-click) and kill (trash), and + for a new one, as in Code-OSS's terminal tabs.
    fn terminal_strip(s: &Shared) -> Element {
        let revision = s.revision;
        let bump = move || {
            let mut revision = revision;
            *revision.write() += 1;
        };
        strip_of(terms::view(), bump)
    }

    /// The strip for one [`terms::StripView`]. The + sits outside the scrolling tab list, after it,
    /// so it stays visible in every state as in Code-OSS (#799); inside the scrolling list the
    /// renderer drew it only while a rename field was open.
    /// A terminal tab's width in px from its label (`.ed-tsub>button`: 12 px text, 12 px padding each
    /// side, 1 px border, 14 px icon, 18 px buttons with -6 px end margin, 6 px gaps), at most 240.
    pub(super) fn tab_width(label: &str, on: bool) -> u32 {
        let text: u32 = label.chars().map(|c| if c.is_ascii() { 7 } else { 12 }).sum();
        let buttons = if on { 2 * (6 + 18) } else { 6 + 18 } - 6;
        (2 + 24 + 14 + 6 + text + buttons).min(240)
    }

    pub(super) fn strip_of(view: Option<terms::StripView>, bump: impl Fn() + Copy + 'static) -> Element {
        let (tabs, active, renaming, error) = match view {
            Some(v) => (v.tabs, v.active, v.renaming, v.error),
            None => (Vec::new(), None, None, None),
        };
        rsx! {
            div { class: "ed-tstrip",
            div { class: "ed-tsub", role: "tablist", "aria-label": "터미널",
                for (id, label) in tabs {
                    if renaming.as_ref().is_some_and(|(r, _)| *r == id) {
                        input { key: "{id}", class: "ed-tren", "aria-label": "터미널 이름", value: renaming.as_ref().map(|(_, t)| t.clone()).unwrap_or_default(),
                            oninput: move |event| { terms::rename_text(event.value()); bump(); },
                            onkeydown: move |event| {
                                match event.key() {
                                    Key::Enter => { terms::end_rename(true); bump(); }
                                    Key::Escape => { terms::end_rename(false); bump(); }
                                    _ => {}
                                }
                            },
                        }
                    } else {
                        {
                            let on = active.as_deref() == Some(id.as_str());
                            let (sel, ren, del, f2, pen) = (id.clone(), id.clone(), id.clone(), id.clone(), id.clone());
                            rsx! {
                                button { key: "{id}", class: if on { "on" } else { "" }, role: "tab", "aria-selected": if on { "true" } else { "false" }, title: "{label}",
                                    // TODO(dioxus-compose#227): no max-content width, so an inactive tab
                                    // collapsed to its padding in the scrolling list; its width is set here.
                                    style: "flex:none; width:{tab_width(&label, on)}px;",
                                    onclick: move |_| { terms::select(&sel); bump(); },
                                    // F2 and the pencil button rename until a real double-click arrives.
                                    // v0.0.7: #417 fixed; workaround kept until checked on a display (dioxus-compose has to forward compose-rust's pointer events as oncontextmenu and ondoubleclick; its pin moves separately).
                                    ondoubleclick: move |_| { terms::begin_rename(&ren); bump(); },
                                    onkeydown: move |event| {
                                        let m = event.modifiers();
                                        let name = key_name(&event.key());
                                        if crate::terminal_list::tab_key_action(&name, m.contains(Modifiers::CONTROL), m.contains(Modifiers::ALT), m.contains(Modifiers::SHIFT), m.contains(Modifiers::META)).is_some() {
                                            event.stop_propagation();
                                            event.prevent_default();
                                            terms::begin_rename(&f2);
                                            bump();
                                        }
                                    },
                                    {icon_px("code", 14)} "{label}"
                                    if on {
                                        span { class: "ed-ib", role: "button", title: "이름 바꾸기", "aria-label": "이름 바꾸기: {label}", tabindex: "-1",
                                            onclick: move |event| { event.stop_propagation(); terms::begin_rename(&pen); bump(); },
                                            {icon_px("edit", 12)}
                                        }
                                    }
                                    span { class: "ed-ib", role: "button", title: "터미널 종료", "aria-label": "터미널 종료: {label}", tabindex: "-1",
                                        onclick: move |event| { event.stop_propagation(); terms::kill(&del); bump(); },
                                        {icon_px("trash", 12)}
                                    }
                                }
                            }
                        }
                    }
                }
            }
                button { class: "ed-ib ed-tnew", title: "새 터미널", "aria-label": "새 터미널", onclick: move |_| { terms::new_terminal(); bump(); }, {icon("plus")} }
            }
            if let Some(error) = error {
                div { class: "tl", style: "padding:0 var(--sp3); color:var(--c-red);", "{error}" }
            }
        }
    }

    fn terminal_body(s: &Shared) -> Element {
        if !can_attach(&s.server_url, &s.computer) {
            return rsx! { div { class: "ed-term", role: "log", "aria-label": "터미널 출력", style: "color:var(--muted);", "터미널은 서버에 연결했을 때만 사용할 수 있습니다. 이 창에는 서버 주소나 컴퓨터가 없습니다." } };
        }
        let handle = terminal_handle(s);
        let strip = terminal_strip(s);
        let Some(handle) = handle else {
            let text = match terms::view().map(|v| v.load) {
                Some(terms::Load::Failed(e)) => e,
                Some(terms::Load::Ready) => "열린 터미널이 없습니다. +를 눌러 새 터미널을 엽니다.".to_string(),
                _ => "터미널 목록을 불러오는 중입니다.".to_string(),
            };
            return rsx! { {strip} div { class: "ed-term", role: "log", "aria-label": "터미널 출력", style: "color:var(--muted);", "{text}" } };
        };
        fit_terminal(s, &handle);
        let view = panel_view(&handle.snapshot());
        let key_handle = handle.clone();
        let ime_handle = handle.clone();
        let control_handle = handle.clone();
        let offer_control = offers_take_control(&handle.snapshot());
        let offer_reconnect = handle.snapshot().needs_reconnect();
        let revision = s.revision;
        rsx! {
            {strip}
            div { class: "ed-term", role: "log", "aria-label": "터미널 출력", tabindex: "0", style: "outline:none;",
                onmounted: move |event| TERM_MOUNT.with(|m| *m.borrow_mut() = Some(event.data())),
                // FR-L31.21: paste chord and IME composition (`crate::terminal_input`).
                onkeydown: move |event| {
                    let m = event.modifiers();
                    let name = key_name(&event.key());
                    if TERMINAL_IME.with(|ime| ime.borrow().drops_key(&name, event.is_composing())) {
                        return;
                    }
                    if crate::terminal_input::is_paste_chord(&name, m.contains(Modifiers::CONTROL), m.contains(Modifiers::ALT), m.contains(Modifiers::SHIFT), m.contains(Modifiers::META)) {
                        event.stop_propagation();
                        event.prevent_default();
                        // v0.0.7: #546 fixed (`compose_rust::clipboard::read_text`); workaround kept
                        // until checked on a display (the read has to answer inside the dioxus-compose
                        // window). Then read the clipboard, ask with `needs_multiline_warning` and send
                        // `paste_bytes(text, state.bracketed_paste)`. Until then the chord only says why
                        // nothing is pasted.
                        notice(crate::terminal_input::PASTE_UNAVAILABLE);
                        return;
                    }
                    let bytes = terminal_input(&name, m.contains(Modifiers::CONTROL), m.contains(Modifiers::ALT), m.contains(Modifiers::SHIFT), m.contains(Modifiers::META));
                    let Some(bytes) = bytes else { return };
                    event.stop_propagation();
                    event.prevent_default();
                    if sends_input(&key_handle.snapshot()) {
                        key_handle.send_input(&bytes);
                    }
                },
                // TODO(dioxus-compose#205): the live renderer does not route paste and IME composition
                // events yet; these handlers run in `HtmlDom` tests but not yet on screen.
                oncompositionstart: move |_| TERMINAL_IME.with(|ime| ime.borrow_mut().start()),
                oncompositionupdate: move |event| TERMINAL_IME.with(|ime| ime.borrow_mut().update(&event.data().data())),
                oncompositionend: move |event| {
                    let bytes = TERMINAL_IME.with(|ime| ime.borrow_mut().end(&event.data().data()));
                    if !bytes.is_empty() && sends_input(&ime_handle.snapshot()) {
                        ime_handle.send_input(&bytes);
                    }
                },
                if let Some(banner) = view.banner {
                    div { style: "display:flex; align-items:center; gap:var(--sp2); padding:var(--sp1) var(--sp3); margin-bottom:var(--sp2); background:var(--c-orange-soft); color:var(--c-orange); border-radius:var(--r-sm);",
                        div { style: "flex:1;", "{banner}" }
                        if offer_control {
                            button { class: "ed-pj", style: "background:var(--c-orange); color:var(--ink-text); height:24px;",
                                onclick: move |_| control_handle.take_control(), "제어권 가져오기" }
                        }
                    }
                }
                div { style: "white-space:pre;",
                    for (i, line) in view.lines.iter().enumerate() {
                        div { key: "{i}", class: "tl", "{line}" }
                    }
                }
                if let Some(notice) = view.notice {
                    div { class: "tl", style: "padding-top:var(--sp2); color:var(--muted);", "{notice}" }
                }
                if offer_reconnect {
                    // TODO(dioxus-compose#175): the button sits in a flex row of its own because directly under
                    // the block-level panel the renderer leaves out the second glyph of each word
                    // ("다시 연결" draws as "다 연"). Drop the wrapper once that is fixed.
                    div { style: "display:flex; margin-top:var(--sp2);",
                        button { class: "ed-pj", style: "border:1px solid var(--line);",
                            onclick: move |_| {
                                terms::reattach_active();
                                let mut revision = revision;
                                *revision.write() += 1;
                            }, "다시 연결" }
                    }
                }
            }
        }
    }

    /// The problems list: a row per marker, errors first; a click opens the file and moves the caret.
    fn problems_body(s: &Shared, rows: Vec<(String, ember_editor::widget::Severity, String, String, String, u32, u32)>) -> Element {
        use ember_editor::widget::Severity;
        let mut tabs = s.tabs;
        if rows.is_empty() {
            return rsx! { div { class: "ed-term", style: "color:var(--muted);", "문제가 없습니다." } };
        }
        let limit = *s.problems_limit.read();
        let (visible_rows, overflow) = crate::ui::editor::paginate_problems(&rows, limit);
        let mut problems_limit = s.problems_limit;
        rsx! {
            div { class: "ed-plist", role: "list", "aria-label": "문제 목록",
                for (i, (path, severity, message, location, source, start_line, start_col)) in visible_rows.iter().cloned().enumerate() {
                    {
                        let host = s.host.clone();
                        let key_host = s.host.clone();
                        let mut recent_files = s.recent_files;
                        let mut key_recent_files = s.recent_files;
                        let dot = match severity { Severity::Error => "ed-pdot err", Severity::Warning => "ed-pdot warn", _ => "ed-pdot" };
                        let row_label = crate::ui::editor::problem_aria_label(match severity { Severity::Error => "오류", Severity::Warning => "경고", _ => "정보" }, &message, &location);
                        let mut pending_goto = s.pending_goto;
                        let mut next_goto_id = s.next_goto_id;
                        let key_path = path.clone();
                        rsx! { div { key: "{i}", class: "ed-prow", role: "listitem", "aria-label": "{row_label}", tabindex: "0",
                            onclick: move |_| {
                                tabs.write().open(&path);
                                record_recent_file(&mut recent_files, &path);
                                host.send(HostCommand::Open(path.clone()));
                                host.send(HostCommand::Focus(path.clone()));
                                let gid = *next_goto_id.read();
                                *next_goto_id.write() += 1;
                                pending_goto.set(Some(PendingGoto {
                                    id: gid,
                                    path: path.clone(),
                                    line: start_line,
                                    col: start_col,
                                }));
                            },
                            onkeydown: move |event| {
                                if matches!(event.key(), Key::Enter) || matches!(event.key(), Key::Character(ref c) if c == " ") {
                                    event.stop_propagation();
                                    event.prevent_default();
                                    tabs.write().open(&key_path);
                                    record_recent_file(&mut key_recent_files, &key_path);
                                    key_host.send(HostCommand::Open(key_path.clone()));
                                    key_host.send(HostCommand::Focus(key_path.clone()));
                                    let gid = *next_goto_id.read();
                                    *next_goto_id.write() += 1;
                                    pending_goto.set(Some(PendingGoto {
                                        id: gid,
                                        path: key_path.clone(),
                                        line: start_line,
                                        col: start_col,
                                    }));
                                }
                            },
                            span { class: "{dot}" }
                            span { style: "flex:1; min-width:0; overflow:hidden; white-space:nowrap;", "{message}" }
                            span { style: "color:var(--muted); font-size:var(--fs-xs); white-space:nowrap;", "{source} {location}" }
                        } }
                    }
                }
                if overflow > 0 {
                    button {
                        class: "ed-pj ed-prow-more",
                        style: "width:100%; justify-content:center; padding:var(--sp2); color:var(--c-blue); font-size:var(--fs-xs);",
                        onclick: move |_| {
                            let cur = *problems_limit.read();
                            problems_limit.set(cur.saturating_add(crate::ui::editor::MAX_VISIBLE_PROBLEMS));
                        },
                        "외 {overflow}개 항목 더 보기…"
                    }
                }
            }
        }
    }

    /// The output panel: channel selector, log text, and clear button.
    fn output_body(s: &Shared) -> Element {
        let bridge = s.bridge.borrow();
        let channels = bridge.output_channels().to_vec();
        drop(bridge);

        let selected_sig = s.selected_output_channel;
        let selected_id = selected_sig.read().clone();

        let all_channels: Vec<(String, String)> = std::iter::once(("ember-workbench".into(), "워크벤치".into()))
            .chain(channels.iter().map(|c| (c.id.clone(), c.label.clone())))
            .collect();

        let active_id = if all_channels.iter().any(|(id, _)| id == &selected_id) {
            selected_id
        } else {
            "ember-workbench".into()
        };

        let text = if active_id == "ember-workbench" {
            s.workbench_logs.read().join("\n")
        } else {
            channels.iter().find(|c| c.id == active_id).map(|c| c.text.clone()).unwrap_or_default()
        };

        let clear_id = active_id.clone();
        let mut clear_s = s.clone();

        rsx! {
            div { class: "ed-tsub", style: "display:flex; align-items:center; gap:var(--sp2); padding:var(--sp1) var(--sp2);",
                div { role: "group", "aria-label": "출력 채널", style: "display:flex; gap:var(--sp1); align-items:center; overflow-x:auto;",
                    for (id, label) in all_channels {
                        {
                            let is_on = id == active_id;
                            let cid = id.clone();
                            let mut sel = selected_sig;
                            let bridge = s.bridge.clone();
                            rsx! {
                                button {
                                    key: "{id}",
                                    class: if is_on { "ed-pj on" } else { "ed-pj" },
                                    "aria-pressed": "{is_on}",
                                    onclick: move |_| {
                                        sel.set(cid.clone());
                                        bridge.borrow_mut().set_active_output_channel(cid.clone());
                                    },
                                    "{label}"
                                }
                            }
                        }
                    }
                }
                div { class: "spacer" }
                button {
                    class: "ed-pj",
                    title: "지우기",
                    "aria-label": "지우기",
                    style: "border:1px solid var(--line);",
                    onclick: move |_| {
                        if clear_id == "ember-workbench" {
                            clear_s.workbench_logs.write().clear();
                        } else {
                            clear_s.bridge.borrow_mut().clear_output_channel(&clear_id);
                            let mut rev = clear_s.revision;
                            *rev.write() += 1;
                        }
                    },
                    "지우기"
                }
            }
            div { class: "ed-term", role: "log", "aria-label": "출력 로그", style: "flex:1; overflow-y:auto; padding:var(--sp2); font-family:var(--font-mono); white-space:pre-wrap; outline:none;",
                if text.trim().is_empty() {
                    div { style: "color:var(--muted);", "출력이 없습니다." }
                } else {
                    for (i, line) in text.lines().enumerate() {
                        div { key: "{i}", class: "tl", "{line}" }
                    }
                }
            }
        }
    }

    fn bottom_window(s: &Shared, w: ToolWindow) -> Element {
        let mut stripes = s.stripes;
        let mut bottom_max = s.bottom_max;
        let max = *bottom_max.read();
        let has_left = s.stripes.read().selected(Side::Left).is_some();
        let problem_rows: Vec<(String, ember_editor::widget::Severity, String, String, String, u32, u32)> = s
            .bridge
            .borrow()
            .problems()
            .iter()
            .map(|p| (
                p.uri.path.clone(),
                p.severity,
                p.message.clone(),
                crate::ui::editor::problem_location(&p.uri.path, &s.folder, p.range.start.line),
                p.source.clone().unwrap_or_default(),
                p.range.start.line,
                p.range.start.col,
            ))
            .collect();
        let problem_count = problem_rows.len();
        let body = match w {
            ToolWindow::Run => "실행 구성이 없습니다. 구성이 연결되면 이곳에 출력이 표시됩니다.",
            _ => "진단 정보가 아직 연결되지 않았습니다. 연결되면 이곳에 문제가 표시됩니다.",
        };
        let max_label = if max { "원래 크기" } else { "최대화" };
        rsx! {
            section { class: "ed-tw ed-bottom", style: if max { "grid-row:2 / 4;" } else if has_left { "" } else { "margin-left:calc(var(--gap) * -1);" }, "aria-label": "아래 도구 창",
                div { class: "ed-twh",
                    div { class: "ed-btabs", role: "tablist", "aria-label": "아래 도구 창 탭",
                        for b in ToolWindow::BOTTOM {
                            {
                                let class = if b == w { "on" } else { "" };
                                let label = if b == ToolWindow::Problems { crate::ui::editor::problems_tab_label(problem_count) } else { b.label().to_string() };
                                rsx! { button { key: "{b.label()}", class, role: "tab", "aria-selected": "{b == w}", onclick: move |_| { if !stripes.read().is_selected(b) { stripes.write().toggle(b); } }, "{label}" } }
                            }
                        }
                    }
                    div { class: "spacer" }
                    button { class: "ed-ib", title: "{max_label}", "aria-label": "{max_label}", onclick: move |_| bottom_max.set(!max), {icon(if max { "down" } else { "up" })} }
                    button { class: "ed-ib", title: "숨기기", "aria-label": "숨기기", onclick: move |_| { bottom_max.set(false); stripes.write().hide(Side::Bottom); }, {icon("close")} }
                }
                if w == ToolWindow::Terminal {
                    {terminal_body(s)}
                } else if w == ToolWindow::Problems {
                    {problems_body(s, problem_rows)}
                } else if w == ToolWindow::Output {
                    {output_body(s)}
                } else if w == ToolWindow::Run {
                    {crate::ui::editor_tasks::view::body(s)}
                } else {
                    div { class: "ed-term", style: "color:var(--muted);", "{body}" }
                }
            }
        }
    }

    /// The blame item's text, hover and accessible name for `line` (from 1) of the open file, or
    /// `None` while there is nothing to show (FR-L31.8).
    fn blame_item(s: &Shared, current: Option<&str>, line: usize) -> Option<(String, String, String)> {
        let abs = current?;
        let (dirty, version, text) = {
            let bridge = s.bridge.borrow();
            let doc = bridge.active_document().filter(|d| d.uri.path == abs)?;
            (doc.dirty, doc.version, doc.dirty.then(|| doc.text.clone()))
        };
        let unsaved = if dirty { text.as_deref().map(|t| (version, t)) } else { None };
        let b = s.scm.blame.item(&s.folder, abs, u32::try_from(line).ok()?, unsaved)?;
        let now = crate::ui::editor_blame::now_secs();
        use crate::ui::editor_blame::{aria_text, hover_text, status_text};
        Some((status_text(&b, now), hover_text(&b, now), aria_text(&b, now)))
    }

    fn status_bar(s: &Shared, agent_state: &str, branch: &str, current: Option<&str>, line: usize, col: usize, tab_width: u32) -> Element {
        let crumbs = crate::ui::editor::status_crumbs(&s.folder, current);
        let agent = agent_status_text(agent_state);
        let dot = crate::ui::editor::agent_dot_class(agent_state);
        let position = position_text(line, col);
        let tab_w = crate::ui::editor::tab_width_text(tab_width);
        let branch = branch.to_string();
        let has_file = current.is_some();
        // EOL, encoding and language of the active document (FR-L31.7).
        let format = active_format(s).and_then(|(_, f, _)| f);
        let crate::ui::editor_status::StatusTexts { eol: eol_t, encoding: enc_t, language: lang_t } =
            crate::ui::editor_status::status_texts(format.as_ref());
        let (eol_s, enc_s, lang_s) = (s.clone(), s.clone(), s.clone());
        let goto_s = s.clone();
        let git_s = s.clone();
        // FR-L31.8: the cursor line's blame, as Code-OSS's git.blame.statusBarItem.
        let (blame_text, blame_hover, blame_aria) = blame_item(s, current, line).unwrap_or_default();
        rsx! {
            footer { class: "ed-status", role: "group", "aria-label": "상태 표시줄",
                span { class: "ed-sbi crumbs-s", "{crumbs}" }
                div { class: "spacer" }
                span { class: "ed-sbi", span { class: "{dot}" } "{agent}" }
                if !blame_text.is_empty() {
                    span { class: "ed-sbi hide-md", title: "{blame_hover}", "aria-label": "{blame_aria}", {icon("commit")} "{blame_text}" }
                }
                if has_file {
                    button { class: "ed-sbi", title: "줄/열로 이동", "aria-label": "줄/열로 이동", onclick: move |_| run_action(&goto_s, Action::GotoLine), "{position}" }
                    button { class: "ed-sbi hide-md", title: "줄 끝 시퀀스 선택", "aria-label": "줄 끝 시퀀스 선택: {eol_t}",
                        onclick: move |_| open_picker(&eol_s, crate::ui::palette::Mode::Eol), "{eol_t}" }
                    button { class: "ed-sbi hide-md", title: "인코딩 선택", "aria-label": "인코딩 선택: {enc_t}",
                        onclick: move |_| open_picker(&enc_s, crate::ui::palette::Mode::EncodingAction), "{enc_t}" }
                    span { class: "ed-sbi hide-md", "{tab_w}" }
                    button { class: "ed-sbi hide-md", title: "언어 모드 선택", "aria-label": "언어 모드 선택: {lang_t}",
                        onclick: move |_| open_picker(&lang_s, crate::ui::palette::Mode::Language), "{lang_t}" }
                }
                button { class: "ed-sbi", title: "Git 브랜치", "aria-label": "Git 브랜치", onclick: move |_| crate::ui::editor_scm::view::show_git(&git_s), {icon("git")} "{branch}" }
            }
        }
    }
}

#[cfg(test)]
#[path = "editor_terminal_strip_tests.rs"]
mod terminal_strip_tests;

#[cfg(test)]
#[path = "editor_workbench_glue_tests.rs"]
mod glue_tests;

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
    #[test] fn fr_l31_terminal_panel_attaches_only_with_server_and_computer() {
        assert!(page::can_attach("http://s:1", "node"));
        assert!(!page::can_attach("", "node"));
        assert!(!page::can_attach("http://s:1", "  "));
    }
    #[test] fn fr_l31_terminal_panel_view_shows_banner_lines_and_notice() {
        let mut state = crate::terminal_stream::TerminalState::default();
        let view = page::panel_view(&state);
        assert_eq!(view.notice.as_deref(), Some("터미널이 연결되지 않았습니다."));
        assert!(view.banner.is_none() && view.lines.is_empty());
        state.apply(crate::terminal_stream::TerminalUpdate::Lines(vec!["$ ls".into()]));
        let view = page::panel_view(&state);
        assert_eq!(view.lines, ["$ ls"]);
        assert!(view.notice.is_none());
    }
    #[test] fn fr_l31_terminal_keys_map_to_bytes_and_meta_is_not_sent() {
        assert_eq!(page::terminal_input("Enter", false, false, false, false), Some(b"\r".to_vec()));
        assert_eq!(page::terminal_input("ArrowUp", false, false, false, false), Some(b"\x1b[A".to_vec()));
        assert_eq!(page::terminal_input("c", true, false, false, false), Some(vec![3]));
        assert_eq!(page::terminal_input("c", false, false, false, true), None);
    }
    #[test] fn fr_p4_take_control_is_offered_and_input_held_while_another_client_controls() {
        use crate::terminal_stream::{TermClient, TerminalState};
        let mut state = TerminalState::default();
        assert!(!page::offers_take_control(&state));
        assert!(page::sends_input(&state));
        state.client = Some(1);
        state.controller = Some(TermClient { client: 2, device: "d".into(), kind: None, read_only: false });
        assert!(page::offers_take_control(&state));
        assert!(!page::sends_input(&state));
        state.controller = Some(TermClient { client: 1, device: "me".into(), kind: None, read_only: false });
        assert!(!page::offers_take_control(&state));
        assert!(page::sends_input(&state));
    }
    #[test] fn fr_l31_status_position() { assert_eq!(position_text(2, 3), "2행 3열"); }
}
