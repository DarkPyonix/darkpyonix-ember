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
    expanded: Signal<std::collections::HashSet<String>>,
    bottom_max: Signal<bool>,
    tree_sel: Signal<Option<(String, bool)>>,
    prompt: Signal<Option<crate::ui::editor::TreePrompt>>,
    prompt_text: Signal<String>,
    prompt_at: Signal<u64>,
    theme: Signal<crate::ui::theme::ThemePref>,
    position: Signal<(usize, usize)>,
    stripes: Signal<Stripes>,
    menu_open: Signal<bool>,
    keymap: Signal<Keymap>,
    folder: String,
    server_url: String,
    computer: String,
    project: String,
    project_name: String,
    uris: Rc<RefCell<HashMap<String, ember_editor_conn::uri::UriComponents>>>,
}

// WidgetView accepts a function without props and owns a separate tree. Share signals between
// the trees until WidgetView supports props and flex stretch sizing (dioxus-compose #160).
thread_local! { static DOUBLE_SHIFT: RefCell<super::editor::DoubleShift> = RefCell::new(super::editor::DoubleShift::default()); }

/// Milliseconds on a clock that only moves forward, for telling two Shift presses apart.
fn now_ms() -> u64 {
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
}

thread_local! { static ICON_DARK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
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
    let expanded = use_hook(|| Signal::new_in_scope(std::collections::HashSet::<String>::new(), root));
    let theme = use_hook(|| Signal::new_in_scope(crate::services::services().ui.prefs.get_untracked().theme, root));
    let tree_sel = use_hook(|| Signal::new_in_scope(None::<(String, bool)>, root));
    let prompt = use_hook(|| Signal::new_in_scope(None::<crate::ui::editor::TreePrompt>, root));
    let prompt_at = use_hook(|| Signal::new_in_scope(0u64, root));
    let prompt_text = use_hook(|| Signal::new_in_scope(String::new(), root));
    let bottom_max = use_hook(|| Signal::new_in_scope(false, root));
    let position = use_hook(|| Signal::new_in_scope((1, 1), root));
    let stripes = use_hook(|| Signal::new_in_scope(Stripes::default(), root));
    let menu_open = use_hook(|| Signal::new_in_scope(false, root));
    let keymap = use_hook(|| Signal::new_in_scope(crate::services::services().ui.prefs.get_untracked().keymap, root));
    let bridge = use_hook(|| Rc::new(RefCell::new(WidgetBridge::new())));
    let uris = use_hook(|| Rc::new(RefCell::new(HashMap::new())));
    SHARED.with(|s| *s.borrow_mut() = Some(Shared { host, bridge, revision, tabs, directory, expanded, bottom_max, tree_sel, prompt, prompt_text, prompt_at, theme, position, stripes, menu_open, keymap, project_name: crate::ui::project::display_name(&target.project), project: target.project, folder: target.folder, server_url: target.server_url, computer: target.computer, uris }));
}

pub(crate) fn join_path(dir: &str, name: &str) -> String {
    if dir == "/" { format!("/{name}") } else if dir.is_empty() { name.to_string() } else { format!("{}/{name}", dir.trim_end_matches('/')) }
}

/// The open files whose edits are not saved.
fn unsaved_open_paths(s: &Shared) -> Vec<String> {
    let bridge = s.bridge.borrow();
    let uris = s.uris.borrow();
    s.tabs
        .read()
        .paths()
        .into_iter()
        .filter(|p| uris.get(*p).and_then(|u| bridge.document(u)).is_some_and(|d| d.dirty))
        .map(str::to_owned)
        .collect()
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
    let change_s = s.clone(); let change_uri = uri.clone();
    let save_s = s.clone(); let save_uri = uri.clone();
    let hover_s = s.clone(); let hover_uri = uri.clone();
    let click_s = s.clone(); let click_uri = uri.clone();
    let selection_s = s.clone(); let selection_uri = uri;
    rsx! { CodeEditor {
        key: "{generation}", width: Some(box_w), height: Some(box_h),
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
    use std::sync::Arc;
    use crate::terminal_stream::{self, ChangeCallback, TerminalHandle, TerminalState};
    use super::{DOUBLE_SHIFT, now_ms, editor_pane, join_path, shared, workbench, base_name, position_text, BridgeAction, HostCommand, Shared};
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

    fn workbench_page() -> Element {
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
            let mut expanded = s.expanded;
            expanded.write().retain(|p| !crate::ui::editor::is_at_or_below(p, &path));
            let mut tree_sel = s.tree_sel;
            tree_sel.set(None);
        }
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
        let sel = *s.stripes.read();
        let keymap = *s.keymap.read();
        let (agent_state, branch) = project_status(&s.project);
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
        // page keeps its 76px allowance. TODO(#320): the height is fixed at launch (compose-rust#467).
        let height = crate::ui::editor::editor_page_height(cfg!(target_os = "macos"));
        let ed_style = format!("width:100vw; height:{height}; {grid}");
        rsx! {
            // TODO(#320): shrinking the window does not reflow this page; growing does.
            div { class: "{ed_class}", "data-theme": if dark { "dark" } else { "light" }, tabindex: "0", style: "{ed_style}",
                onkeydown: move |event| {
                    let m = event.modifiers();
                    let keymap = *key_s.keymap.read();
                    let name = key_name(&event.key());
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
                    if let Some(action) = action_for_key(
                        keymap,
                        Os::current(),
                        &key_name(&event.key()),
                        m.contains(Modifiers::CONTROL),
                        m.contains(Modifiers::SHIFT),
                        m.contains(Modifiers::ALT),
                        m.contains(Modifiers::META),
                    ) {
                        run_action(&key_s, action);
                    }
                },
                if let Some(error) = error {
                    div { style: "padding:20px; grid-column:1 / -1; padding-top:76px;", "{error}" button { onclick: move |_| crate::services::services().ui.close_editor(), "닫기" } }
                } else {
                    {top_bar(&s, &branch, keymap)}
                    div { class: "ed-rail",
                        {stripe_column(&s, sel, keymap, Strip::Left)}
                        {stripe_column(&s, sel, keymap, Strip::Bottom)}
                    }
                    if let (Some(w), false) = (sel.selected(Side::Left), maximized) {
                        if w == ToolWindow::Project {
                            {project_tree(&s, current.as_deref())}
                        } else {
                            {side_note(w, "ed-left")}
                        }
                    }
                    section { class: "ed-main", style: if maximized { "display:none;" } else if has_left { "" } else { "margin-left:calc(var(--gap) * -1);" }, "aria-label": "편집기",
                        {tab_strip(&s, &paths, &dirty, current.as_deref())}
                        if let Some(path) = current.as_deref() {
                            {breadcrumbs(path, &s.folder)}
                        }
                        div { style: "flex:1; min-height:0; min-width:0; display:flex; overflow:hidden;",
                            if active.is_some() { WidgetView { app: editor_pane, style: "flex:1; min-width:0; height:100%;" } }
                            else { div { class: "ed-empty", b { "열린 파일이 없습니다" } } }
                        }
                    }
                    {stripe_column(&s, sel, keymap, Strip::Right)}
                    if let Some(w) = sel.selected(Side::Bottom) {
                        {bottom_window(&s, w)}
                    }
                    {status_bar(&s, &agent_state, &branch, current.as_deref().filter(|_| active.is_some()), line, col)}
                }
            }
        }
    }

    /// A mockup icon as an image: the HTML path draws SVG through `img`, which cannot take
    /// `currentColor`, so the stroke is the text colour of the light palette.
    // TODO(editor-redesign step 4): tint per theme once the dark palette is switched on.
    pub(super) fn icon(name: &str) -> Element {
        icon_px(name, 16)
    }

    /// A mockup icon at `px` dp (the mockup sizes tree, tab and breadcrumb icons by context).
    pub(super) fn icon_px(name: &str, px: u32) -> Element {
        let dark = if super::ICON_DARK.with(|d| d.get()) { "@dark" } else { "" };
        rsx! { img { class: "i", src: "icon:{name}{dark}", width: "{px}", height: "{px}" } }
    }

    fn top_bar(s: &Shared, branch: &str, keymap: Keymap) -> Element {
        let mut menu_open = s.menu_open;
        let mut theme = s.theme;
        let theme_pref = *s.theme.read();
        let theme_title = format!("테마: {}", theme_pref.label());
        let name = s.project_name.clone();
        let branch = branch.to_string();
        let find = tooltip_text(keymap.find_label(), &keymap.hint(Action::Find, Os::current()));
        let find_hint = keymap.hint(Action::Find, Os::current());
        let find_note = format!("{}: {SOON}", keymap.find_label());
        let open = *menu_open.read();
        // The traffic lights float over the top bar's left end under FullSize.
        let strip_pad = crate::ui::editor::editor_strip_padding(cfg!(target_os = "macos"), crate::ui::safe_area::safe_area().leading);
        rsx! {
            header { class: "ed-top", style: "{strip_pad}",
                img { class: "logo logo-img", src: "icon:mark", width: "28", height: "28", alt: "Ember" }
                button { class: "ed-pj", title: "닫기", onclick: move |_| crate::services::services().ui.close_editor(), "{name}" {icon_px("chev-dn", 12)} }
                button { class: "ed-pj", title: "브랜치", onclick: move |_| notice(format!("브랜치: {SOON}")), {icon("git")} "{branch}" {icon_px("chev-dn", 12)} }
                div { class: "spacer" }
                button { class: "ed-find", title: "{find}", onclick: move |_| notice(find_note.clone()),
                    {icon("search")}
                    span { class: "hide-md", "{keymap.find_label()}" }
                    kbd { class: "ed-sc", "{find_hint}" }
                }
                div { class: "ed-runcfg",
                    button { class: "ed-pj", title: "실행 구성", onclick: move |_| notice(format!("실행 구성: {SOON}")), {icon("play")} "실행 구성" {icon_px("chev-dn", 12)} }
                    button { class: "ed-ib run", title: "실행", onclick: move |_| notice(format!("실행: {SOON}")), {icon("play")} }
                    button { class: "ed-ib", title: "중지", onclick: move |_| notice(format!("중지: {SOON}")), {icon("pause")} }
                    button { class: "ed-ib", title: "{theme_title}", onclick: move |_| {
                        let next = theme_pref.next();
                        theme.set(next);
                        crate::services::services().ui.update_prefs(|p| p.theme = next);
                    }, {icon(if super::ICON_DARK.with(|d| d.get()) { "moon" } else { "sun" })} }
                    button { class: "ed-ib", title: "에이전트에게 묻기", onclick: move |_| notice(format!("에이전트에게 묻기: {SOON}")), {icon("spark")} }
                    button { class: "ed-ib", title: "메뉴", onclick: move |_| menu_open.set(!open), {icon("more")} }
                }
                if open { {menu(s, keymap)} }
            }
        }
    }

    fn menu(s: &Shared, keymap: Keymap) -> Element {
        let os = Os::current();
        rsx! {
            div { class: "ed-menu", role: "menu",
                for a in [Action::Save, Action::Recent, Action::Format, Action::GotoLine, Action::Palette] {
                    {
                        let s = s.clone();
                        let label = if a == Action::Palette { keymap.palette_label() } else { a.label() };
                        let text = tooltip_text(label, &keymap.hint(a, os));
                        let mut menu_open = s.menu_open;
                        rsx! { button { key: "{a.label()}", role: "menuitem", onclick: move |_| { menu_open.set(false); run_action(&s, a); }, "{text}" } }
                    }
                }
                div { class: "ed-msep" }
                div { class: "ed-mlab", "단축키 체계" }
                for k in Keymap::ALL {
                    {
                        let mut menu_open = s.menu_open;
                        let mut keymap_sig = s.keymap;
                        let mark = if k == keymap { "\u{2713}" } else { "\u{2003}" };
                        let text = format!("{mark} {}  {}", k.label(), k.hint(Action::Find, os));
                        rsx! { button { key: "{k.label()}", role: "menuitemradio", "aria-checked": "{k == keymap}", onclick: move |_| {
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
        let class = if selected { "ed-sb on" } else { "ed-sb" };
        rsx! {
            button { key: "{w.label()}", class, title: "{tip}", "aria-label": "{tip}", onclick: move |_| {
                if w.side() == Side::Right { notice(format!("{}: {SOON}", w.label())); } else { stripes.write().toggle(w); }
            }, {icon(icon_name(w))} }
        }
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Strip { Left, Bottom, Right }

    fn icon_name(w: ToolWindow) -> &'static str {
        match w {
            ToolWindow::Project => "folder",
            ToolWindow::Commit => "commit",
            ToolWindow::Git => "git",
            ToolWindow::Agent => "spark",
            ToolWindow::Notifications => "bell",
            ToolWindow::Terminal => "code",
            ToolWindow::Run => "play",
            ToolWindow::Problems => "warn",
        }
    }

    fn stripe_column(s: &Shared, sel: Stripes, keymap: Keymap, strip: Strip) -> Element {
        let (class, windows): (&str, &[ToolWindow]) = match strip {
            Strip::Left => ("ed-stripe ed-stripe-l", &ToolWindow::LEFT),
            Strip::Bottom => ("ed-stripe ed-stripe-b", &ToolWindow::BOTTOM),
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
        let mut prompt = s.prompt;
        let mut prompt_text = s.prompt_text;
        let mut prompt_at = s.prompt_at;
        let unsaved = super::unsaved_open_paths(s);
        let root = s.folder.clone();
        let rows = {
            let state = s.host.state.lock().unwrap();
            crate::ui::editor::tree_rows(&root, &state.listings, &expanded.read())
        };
        let root_name = base_name(&root).to_string();
        let selected: Option<(String, bool)> = tree_sel.read().clone();
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
            }
            let _ = &submit_root;
            prompt.set(None);
            prompt_text.set(String::new());
        };
        let mut submit_enter = submit.clone();
        let mut submit_ok = submit.clone();
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
                    div { class: "ed-tn", role: "alertdialog", tabindex: "0", style: "--d:0; height:auto; flex-wrap:wrap; padding:var(--sp2); white-space:normal; background:var(--c-red-soft); color:var(--c-red);",
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
                rsx! {
                    div { class: "ed-tn", style: "--d:0;",
                        {icon_px("file", 15)}
                        input { style: "flex:1; min-width:0; height:22px; border:1px solid var(--line); border-radius:var(--r-xs); background:var(--surface); color:var(--text); padding:0 6px;",
                            "aria-label": "{label}", placeholder: "{label}", value: "{prompt_text}",
                            oninput: move |event| prompt_text.set(event.value()),
                            onkeydown: move |event| {
                                match event.key() {
                                    Key::Enter => { event.stop_propagation(); submit_enter(enter.clone()); }
                                    Key::Escape => { event.stop_propagation(); prompt.set(None); }
                                    _ => {}
                                }
                            },
                        }
                    }
                }
            }
        };
        rsx! {
            section { class: "ed-tw ed-left", "aria-label": "프로젝트",
                div { class: "ed-twh",
                    b { class: "ed-twt", "프로젝트" }
                    div { class: "spacer" }
                    button { class: "ed-ib", title: "새 파일", onclick: move |_| { prompt_text.set(String::new()); prompt.set(Some(TreePrompt::NewFile { parent: new_file_parent.clone() })); }, {icon("plus")} }
                    button { class: "ed-ib", title: "새 폴더", onclick: move |_| { prompt_text.set(String::new()); prompt.set(Some(TreePrompt::NewDir { parent: new_dir_parent.clone() })); }, {icon("folder")} }
                    button { class: "ed-ib", title: "모두 접기", onclick: move |_| expanded.write().clear(), {icon_px("chev", 16)} }
                    button { class: "ed-ib", title: "파일 검색", onclick: move |_| notice(format!("파일 검색: {SOON}")), {icon("search")} }
                    button { class: "ed-ib", title: "숨기기", onclick: move |_| stripes.write().hide(Side::Left), {icon("close")} }
                }
                div { class: "ed-tree", role: "tree",
                    div { class: "ed-tn", style: "--d:0;", {icon_px("chev-dn", 12)} {icon_px("folder", 15)} "{root_name}" }
                    {prompt_row}
                    for row in rows {
                        {
                            let path = row.path.clone();
                            let host = s.host.clone();
                            let is_dir = row.is_dir;
                            let chosen = selected.as_ref().map(|(p, _)| p.as_str()) == Some(row.path.as_str());
                            let class = if chosen || (selected.is_none() && current == Some(row.path.as_str())) { "ed-tn sel" } else { "ed-tn" };
                            let depth = row.depth + 1;
                            let rename_path = row.path.clone();
                            let delete_path = row.path.clone();
                            let rename_name = row.name.clone();
                            let unsaved_for_rename = unsaved.clone();
                            rsx! { div { key: "{row.path}", class, role: "treeitem", style: "--d:{depth};",
                                "aria-expanded": if is_dir { "{row.open}" } else { "" },
                                onclick: move |_| {
                                    tree_sel.set(Some((path.clone(), is_dir)));
                                    if is_dir {
                                        let was_open = expanded.read().contains(&path);
                                        if was_open { expanded.write().remove(&path); }
                                        else { expanded.write().insert(path.clone()); host.send(HostCommand::List(path.clone())); }
                                    } else { tabs.write().open(&path); host.send(HostCommand::Open(path.clone())); }
                                },
                                if is_dir { if row.open { {icon_px("chev-dn", 12)} } else { {icon_px("chev", 12)} } } else { span { class: "chev-sp" } }
                                if is_dir { {icon_px("folder", 15)} } else { {icon_px("file", 15)} }
                                "{row.name}"
                                if chosen {
                                    div { class: "spacer" }
                                    button { class: "ed-ib", title: "이름 변경", style: "width:22px; height:22px;",
                                        onclick: move |event| {
                                            event.stop_propagation();
                                            let blocked: Vec<String> = unsaved_for_rename.iter().filter(|p| crate::ui::editor::is_at_or_below(p, &rename_path)).cloned().collect();
                                            if !blocked.is_empty() {
                                                notice(format!("저장하지 않은 변경이 있어 이름을 바꿀 수 없습니다. 먼저 저장하세요: {}", blocked.iter().map(|p| base_name(p).to_string()).collect::<Vec<_>>().join(", ")));
                                                return;
                                            }
                                            prompt_text.set(rename_name.clone());
                                            prompt.set(Some(TreePrompt::Rename { path: rename_path.clone() }));
                                        }, {icon_px("edit", 13)} }
                                    button { class: "ed-ib", title: "삭제", style: "width:22px; height:22px; color:var(--c-red);",
                                        onclick: move |event| { event.stop_propagation(); prompt_at.set(now_ms()); prompt.set(Some(TreePrompt::Delete { path: delete_path.clone(), is_dir })); }, {icon_px("close", 13)} }
                                }
                            } }
                        }
                    }
                }
            }
        }
    }

    fn tab_strip(s: &Shared, paths: &[String], dirty: &[bool], current: Option<&str>) -> Element {
        let mut tabs = s.tabs;
        let mut menu_open = s.menu_open;
        rsx! {
            div { class: "ed-tabs", role: "tablist",
                for (i, path) in paths.iter().enumerate() {
                    {
                        let path = path.clone(); let close_path = path.clone();
                        let host = s.host.clone(); let close_host = s.host.clone();
                        let selected = current == Some(path.as_str());
                        let label = base_name(&path).to_string();
                                                let class = if selected { "ed-tab on" } else { "ed-tab" };
                        rsx! { div { class, role: "tab", "aria-selected": "{selected}", onclick: move |_| {
                            tabs.write().activate(&path);
                            host.send(HostCommand::Focus(path.clone()));
                        }, {icon_px(if selected { "file-w" } else { "file" }, 14)} "{label}" if dirty[i] { span { class: "ed-dirty" } } button { class: "ed-tx", "aria-label": "닫기", onclick: move |event| { event.stop_propagation(); tabs.write().close(&close_path); close_host.send(HostCommand::Close(close_path.clone())); if let Some(next) = tabs.read().current_path() { close_host.send(HostCommand::Focus(next.to_string())); } }, {icon_px(if selected { "close-w" } else { "close" }, 12)} } } }
                    }
                }
                div { class: "spacer" }
                button { class: "ed-ib", title: "탭 메뉴", onclick: move |_| { let open = *menu_open.read(); menu_open.set(!open); }, {icon("more")} }
            }
        }
    }

    fn breadcrumbs(path: &str, folder: &str) -> Element {
        let segments = crate::ui::editor::breadcrumb_segments(path, folder);
        let last = segments.len().saturating_sub(1);
        rsx! { div { class: "ed-crumbs",
            for (i, seg) in segments.iter().enumerate() {
                if i > 0 { {icon_px("chev", 11)} }
                span { key: "{i}", class: if i == last { "cur" } else { "" }, "{seg}" }
            }
        } }
    }

    thread_local! {
        /// The editor's terminal session. Started on first show and kept while the panel is hidden.
        static TERMINAL: std::cell::RefCell<Option<TerminalHandle>> = const { std::cell::RefCell::new(None) };
    }

    /// The terminal handle, started on first use. `None` when the editor was not given a server
    /// URL and a computer to attach to.
    fn terminal_handle(s: &Shared) -> Option<TerminalHandle> {
        if !can_attach(&s.server_url, &s.computer) {
            return None;
        }
        TERMINAL.with(|t| {
            let mut slot = t.borrow_mut();
            if slot.is_none() {
                let revision = s.revision;
                let on_change: ChangeCallback = Arc::new(move || {
                    let mut revision = revision;
                    *revision.write() += 1;
                });
                *slot = Some(terminal_stream::start(&s.server_url, &s.computer, &s.folder, &s.project, Some(on_change)));
            }
            slot.clone()
        })
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
    pub(super) fn can_attach(server_url: &str, computer: &str) -> bool {
        !server_url.trim().is_empty() && !computer.trim().is_empty()
    }

    /// Bytes for a key press in the terminal. The Meta key is the app's, not the shell's.
    pub(super) fn terminal_input(name: &str, ctrl: bool, alt: bool, shift: bool, meta: bool) -> Option<Vec<u8>> {
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

    fn terminal_body(s: &Shared) -> Element {
        let Some(handle) = terminal_handle(s) else {
            return rsx! { div { class: "ed-term", style: "color:var(--muted);", "노드의 터미널에 연결되어 있지 않습니다. 서버 주소와 컴퓨터 정보가 없어 터미널을 열 수 없습니다." } };
        };
        let view = panel_view(&handle.snapshot());
        let key_handle = handle.clone();
        let control_handle = handle.clone();
        let offer_control = offers_take_control(&handle.snapshot());
        let offer_reconnect = handle.snapshot().needs_reconnect();
        let revision = s.revision;
        let computer = if s.computer.is_empty() { "로컬".to_string() } else { s.computer.clone() };
        rsx! {
            div { class: "ed-tsub",
                button { class: "on", {icon_px("code", 14)} "{computer}" }
                button { class: "ed-ib", title: "새 터미널", onclick: move |_| notice(format!("새 터미널: {SOON}")), {icon("plus")} }
            }
            div { class: "ed-term", tabindex: "0", style: "outline:none;",
                // TODO(#335): paste and IME. Only key presses are forwarded; pasted and composed text is not.
                onkeydown: move |event| {
                    let m = event.modifiers();
                    let name = key_name(&event.key());
                    let bytes = terminal_input(&name, m.contains(Modifiers::CONTROL), m.contains(Modifiers::ALT), m.contains(Modifiers::SHIFT), m.contains(Modifiers::META));
                    let Some(bytes) = bytes else { return };
                    event.stop_propagation();
                    event.prevent_default();
                    if sends_input(&key_handle.snapshot()) {
                        key_handle.send_input(&bytes);
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
                    button { class: "ed-pj", style: "margin-top:var(--sp2); border:1px solid var(--line);",
                        onclick: move |_| {
                            TERMINAL.with(|t| *t.borrow_mut() = None);
                            let mut revision = revision;
                            *revision.write() += 1;
                        }, "다시 연결" }
                }
            }
        }
    }

    /// The problems list: a row per marker, errors first; a click opens the file.
    fn problems_body(s: &Shared, rows: Vec<(String, ember_editor::widget::Severity, String, String, String)>) -> Element {
        use ember_editor::widget::Severity;
        let mut tabs = s.tabs;
        if rows.is_empty() {
            return rsx! { div { class: "ed-term", style: "color:var(--muted);", "문제가 없습니다." } };
        }
        rsx! {
            div { class: "ed-plist", role: "list",
                for (i, (path, severity, message, location, source)) in rows.into_iter().enumerate() {
                    {
                        let host = s.host.clone();
                        let dot = match severity { Severity::Error => "ed-pdot err", Severity::Warning => "ed-pdot warn", _ => "ed-pdot" };
                        rsx! { div { key: "{i}", class: "ed-prow", role: "listitem", onclick: move |_| {
                            tabs.write().open(&path);
                            host.send(HostCommand::Open(path.clone()));
                        },
                            span { class: "{dot}" }
                            span { style: "flex:1; min-width:0; overflow:hidden; white-space:nowrap;", "{message}" }
                            span { style: "color:var(--muted); font-size:var(--fs-xs); white-space:nowrap;", "{source} {location}" }
                        } }
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
        let problem_rows: Vec<(String, ember_editor::widget::Severity, String, String, String)> = s
            .bridge
            .borrow()
            .problems()
            .iter()
            .map(|p| (p.uri.path.clone(), p.severity, p.message.clone(), crate::ui::editor::problem_location(&p.uri.path, &s.folder, p.range.start.line), p.source.clone().unwrap_or_default()))
            .collect();
        let problem_count = problem_rows.len();
        let body = match w {
            ToolWindow::Run => "실행 구성이 없습니다. 구성이 연결되면 이곳에 출력이 표시됩니다.",
            _ => "진단 정보가 아직 연결되지 않았습니다. 연결되면 이곳에 문제가 표시됩니다.",
        };
        rsx! {
            section { class: "ed-tw ed-bottom", style: if max { "grid-row:2 / 4;" } else if has_left { "" } else { "margin-left:calc(var(--gap) * -1);" }, "aria-label": "아래 도구 창",
                div { class: "ed-twh",
                    div { class: "ed-btabs", role: "tablist",
                        for b in ToolWindow::BOTTOM {
                            {
                                let class = if b == w { "on" } else { "" };
                                let label = if b == ToolWindow::Problems { crate::ui::editor::problems_tab_label(problem_count) } else { b.label().to_string() };
                                rsx! { button { key: "{b.label()}", class, role: "tab", onclick: move |_| { if !stripes.read().is_selected(b) { stripes.write().toggle(b); } }, "{label}" } }
                            }
                        }
                    }
                    div { class: "spacer" }
                    button { class: "ed-ib", title: if max { "원래 크기" } else { "최대화" }, onclick: move |_| bottom_max.set(!max), {icon(if max { "down" } else { "up" })} }
                    button { class: "ed-ib", title: "숨기기", onclick: move |_| { bottom_max.set(false); stripes.write().hide(Side::Bottom); }, {icon("close")} }
                }
                if w == ToolWindow::Terminal {
                    {terminal_body(s)}
                } else if w == ToolWindow::Problems {
                    {problems_body(s, problem_rows)}
                } else {
                    div { class: "ed-term", style: "color:var(--muted);", "{body}" }
                }
            }
        }
    }

    fn status_bar(s: &Shared, agent_state: &str, branch: &str, current: Option<&str>, line: usize, col: usize) -> Element {
        let crumbs = crate::ui::editor::status_crumbs(&s.folder, current);
        let agent = agent_status_text(agent_state);
        let dot = crate::ui::editor::agent_dot_class(agent_state);
        let position = position_text(line, col);
        let branch = branch.to_string();
        let has_file = current.is_some();
        rsx! {
            footer { class: "ed-status",
                span { class: "ed-sbi crumbs-s", "{crumbs}" }
                div { class: "spacer" }
                span { class: "ed-sbi", span { class: "{dot}" } "{agent}" }
                if has_file {
                    span { class: "ed-sbi", "{position}" }
                    span { class: "ed-sbi hide-md", "LF" }
                    span { class: "ed-sbi hide-md", "UTF-8" }
                    span { class: "ed-sbi hide-md", "4칸" }
                }
                span { class: "ed-sbi", {icon("git")} "{branch}" }
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
