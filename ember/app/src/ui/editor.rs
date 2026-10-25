//! The Ember 에디터 screen (FR-L31, `docs/DESIGN.md` section 6.3, `docs/INTENT.md` D25, D32).
//!
//! Opened from IDE 열기, Ember 에디터. The layout is the mockup's (`.scratch/design-mockup/
//! editor.html`): rounded island panels, 40 px icon-only tool-window stripes with a faint tint
//! on the selected button, file tabs, breadcrumbs, the code area, a bottom tool window and a
//! status bar. The keymap (FR-L30) lives in [`super::keymap`]; hints are read from its table.
//!
//! What is wired and what is not (#181):
//! - The editor core (`ember/editor`, `ember/editor-conn`, FR-E1 to FR-E5) is not linked into
//!   the app, and the pinned dioxus-compose has no code-editor widget (it exists only on the
//!   compose-rust `integration/ember-m3` branch). The code area says "준비 중입니다".
//! - The server has no file listing or file read endpoint the client wraps, so the project tree
//!   shows the root only and no tab can be opened from it yet. The tab model is complete and tested.
//! - The client has no terminal stream (FR-P), so 터미널, 실행 and 문제 say "준비 중입니다".
//! - The pinned toolkit reports only `Key::Enter` in a key event, so chords are not handled;
//!   [`apply_action`] is the dispatch the key handler will call. See `keymap.rs`.

use dioxus_compose::prelude::*;
use ember_client::LauncherStatus;

use crate::ui::components::Label;
use crate::ui::keymap::{Action, Keymap, Os};
use crate::ui::project::{display_name, project_boxes, status_pill, SOON};
use crate::ui::theme::*;
use crate::ui::use_ui;

/// What the editor was opened for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditorTarget {
    pub project: String,
    /// The project's folder on its computer, when the server said (`IdeLaunch.folder`).
    pub folder: String,
}

// ---------------------------------------------------------------------------------------------
// Demo files
// ---------------------------------------------------------------------------------------------

/// The environment switch the demo launch sets (the server reads the same one for its usage).
pub const DEMO_ENV: &str = "EMBER_DEMO_DATA";

/// Whether the demo switch value turns demo data on. Only `1` does.
pub fn demo_enabled(value: Option<&str>) -> bool {
    value == Some("1")
}

/// A file of the demo project: its path from the project root and a short content.
pub type DemoFile = (&'static str, &'static str);

/// Demo only, replaced when the server serves files (#193): the server has no file listing yet,
/// so the screenshot pass gets a few nested files to open tabs and breadcrumbs on. Empty unless
/// `value` (the `EMBER_DEMO_DATA` switch) is `1`.
pub fn demo_files(value: Option<&str>) -> Vec<DemoFile> {
    if !demo_enabled(value) {
        return Vec::new();
    }
    vec![
        ("README.md", "# demo\n\nA small project for screenshots.\n"),
        ("Cargo.toml", "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n"),
        ("src/main.rs", "mod config;\n\nfn main() {\n    println!(\"hello\");\n}\n"),
        ("src/config.rs", "pub struct Config {\n    pub port: u16,\n}\n"),
        ("src/ui/home.rs", "pub fn home() -> &'static str {\n    \"home\"\n}\n"),
    ]
}

/// The demo files read from the environment; empty in a normal run.
fn demo_files_from_env() -> Vec<DemoFile> {
    demo_files(std::env::var(DEMO_ENV).ok().as_deref())
}

// ---------------------------------------------------------------------------------------------
// Pure state
// ---------------------------------------------------------------------------------------------

/// The status bar's cursor position, "N행 M열".
pub fn position_text(line: usize, column: usize) -> String {
    format!("{line}행 {column}열")
}

/// A tooltip: the name, then the shortcut when the keymap has one.
pub fn tooltip_text(label: &str, hint: &str) -> String {
    if hint.is_empty() {
        label.to_string()
    } else {
        format!("{label}  {hint}")
    }
}

/// The last path segment.
pub fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Where a stripe sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Bottom,
}

/// The tool windows the stripes open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolWindow {
    Project,
    Commit,
    Git,
    Agent,
    Notifications,
    Terminal,
    Run,
    Problems,
}

impl ToolWindow {
    pub const ALL: [ToolWindow; 8] = [
        ToolWindow::Project,
        ToolWindow::Commit,
        ToolWindow::Git,
        ToolWindow::Agent,
        ToolWindow::Notifications,
        ToolWindow::Terminal,
        ToolWindow::Run,
        ToolWindow::Problems,
    ];
    pub const LEFT: [ToolWindow; 3] = [ToolWindow::Project, ToolWindow::Commit, ToolWindow::Git];
    pub const RIGHT: [ToolWindow; 2] = [ToolWindow::Agent, ToolWindow::Notifications];
    pub const BOTTOM: [ToolWindow; 3] =
        [ToolWindow::Terminal, ToolWindow::Run, ToolWindow::Problems];

    pub fn label(self) -> &'static str {
        match self {
            ToolWindow::Project => "프로젝트",
            ToolWindow::Commit => "커밋",
            ToolWindow::Git => "Git",
            ToolWindow::Agent => "에이전트",
            ToolWindow::Notifications => "알림",
            ToolWindow::Terminal => "터미널",
            ToolWindow::Run => "실행",
            ToolWindow::Problems => "문제",
        }
    }

    pub fn side(self) -> Side {
        match self {
            ToolWindow::Project | ToolWindow::Commit | ToolWindow::Git => Side::Left,
            ToolWindow::Agent | ToolWindow::Notifications => Side::Right,
            ToolWindow::Terminal | ToolWindow::Run | ToolWindow::Problems => Side::Bottom,
        }
    }

    /// The keymap action that opens it.
    pub fn action(self) -> Option<Action> {
        match self {
            ToolWindow::Project => Some(Action::Project),
            ToolWindow::Commit => Some(Action::Commit),
            ToolWindow::Git => Some(Action::Git),
            ToolWindow::Agent => Some(Action::Ask),
            ToolWindow::Notifications => None,
            ToolWindow::Terminal => Some(Action::Terminal),
            ToolWindow::Run => Some(Action::Run),
            ToolWindow::Problems => Some(Action::Problems),
        }
    }

    pub fn from_action(action: Action) -> Option<ToolWindow> {
        ToolWindow::ALL
            .into_iter()
            .find(|w| w.action() == Some(action))
    }

    /// The nearest glyph the pinned toolkit has. TODO(#166: 임의 아이콘 세트): terminal, play,
    /// branch and bell glyphs once `Icon` takes an SVG asset.
    pub fn icon(self) -> IconRole {
        match self {
            ToolWindow::Project => IconRole::Library,
            ToolWindow::Commit => IconRole::Check,
            ToolWindow::Git => IconRole::History,
            ToolWindow::Agent => IconRole::Compose,
            ToolWindow::Notifications => IconRole::Inbox,
            ToolWindow::Terminal => IconRole::Menu,
            ToolWindow::Run => IconRole::Forward,
            ToolWindow::Problems => IconRole::Close,
        }
    }
}

/// Which tool window each stripe shows. Selecting the selected one hides it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stripes {
    left: Option<ToolWindow>,
    right: Option<ToolWindow>,
    bottom: Option<ToolWindow>,
}

impl Default for Stripes {
    fn default() -> Self {
        Stripes {
            left: Some(ToolWindow::Project),
            right: None,
            bottom: Some(ToolWindow::Terminal),
        }
    }
}

impl Stripes {
    pub fn selected(&self, side: Side) -> Option<ToolWindow> {
        match side {
            Side::Left => self.left,
            Side::Right => self.right,
            Side::Bottom => self.bottom,
        }
    }

    pub fn is_selected(&self, w: ToolWindow) -> bool {
        self.selected(w.side()) == Some(w)
    }

    pub fn toggle(&mut self, w: ToolWindow) {
        let next = if self.is_selected(w) { None } else { Some(w) };
        match w.side() {
            Side::Left => self.left = next,
            Side::Right => self.right = next,
            Side::Bottom => self.bottom = next,
        }
    }

    pub fn hide(&mut self, side: Side) {
        match side {
            Side::Left => self.left = None,
            Side::Right => self.right = None,
            Side::Bottom => self.bottom = None,
        }
    }
}

/// The open files. A tab holds its unsaved mark, so closing a tab drops it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditorTabs {
    tabs: Vec<(String, bool)>,
    current: Option<usize>,
}

impl EditorTabs {
    fn index(&self, path: &str) -> Option<usize> {
        self.tabs.iter().position(|(p, _)| p == path)
    }

    /// Open a file, or activate it when it is already open.
    pub fn open(&mut self, path: &str) {
        let i = match self.index(path) {
            Some(i) => i,
            None => {
                self.tabs.push((path.to_string(), false));
                self.tabs.len() - 1
            }
        };
        self.current = Some(i);
    }

    pub fn activate(&mut self, path: &str) {
        if let Some(i) = self.index(path) {
            self.current = Some(i);
        }
    }

    /// Close a tab. Closing the current one activates the tab that takes its place, else the
    /// one before it; closing another one keeps the current tab.
    pub fn close(&mut self, path: &str) {
        let Some(i) = self.index(path) else { return };
        self.tabs.remove(i);
        self.current = match self.current {
            _ if self.tabs.is_empty() => None,
            Some(c) if c == i => Some(i.min(self.tabs.len() - 1)),
            Some(c) if c > i => Some(c - 1),
            other => other,
        };
    }

    pub fn set_dirty(&mut self, path: &str, dirty: bool) {
        if let Some(i) = self.index(path) {
            self.tabs[i].1 = dirty;
        }
    }

    pub fn is_dirty(&self, path: &str) -> bool {
        self.index(path).is_some_and(|i| self.tabs[i].1)
    }

    pub fn has_unsaved(&self) -> bool {
        self.tabs.iter().any(|(_, d)| *d)
    }

    pub fn paths(&self) -> Vec<&str> {
        self.tabs.iter().map(|(p, _)| p.as_str()).collect()
    }

    pub fn current_path(&self) -> Option<&str> {
        self.current.map(|i| self.tabs[i].0.as_str())
    }
}

/// Do what a keymap action does. Returns false for an action the editor cannot do yet (the
/// caller says "준비 중입니다"). The key handler calls this with `Keymap::action_for`.
pub fn apply_action(action: Action, tabs: &mut EditorTabs, stripes: &mut Stripes) -> bool {
    if let Some(w) = ToolWindow::from_action(action) {
        stripes.toggle(w);
        return true;
    }
    match action {
        Action::Close => match tabs.current_path().map(str::to_string) {
            Some(p) => {
                tabs.close(&p);
                true
            }
            None => false,
        },
        Action::HideTool => {
            stripes.hide(Side::Bottom);
            true
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------------------------
// Screen
// ---------------------------------------------------------------------------------------------

const STRIPE_W: f32 = 40.0;
/// The button fills the 40 dp stripe (owner decision), so its glyph is about 20 px.
const STRIPE_BTN: f32 = 40.0;
/// A 40 dp button plus 12 dp above and below; a shorter row squeezes the buttons below the
/// 40 dp the toolkit needs for Hangul (#157, dioxus-compose #133).
const TOP_H: f32 = 64.0;
/// The status bar is 40 dp tall for the same reason.
const STATUS_H: f32 = 40.0;
const LEFT_W: f32 = 260.0;
const BOTTOM_H: f32 = 240.0;

fn transparent() -> Paint {
    Paint::Literal(Color::argb(0))
}

fn soon_message() {
    Message::new(SOON)
        .with_duration(MessageDuration::Short)
        .show();
}

/// A rounded panel: `surface` on a 1 px `line`.
#[component]
fn Island(
    #[props(default)] weight: Option<f32>,
    #[props(default)] width: Option<f32>,
    #[props(default)] height: Option<f32>,
    #[props(default)] fill_max_width: bool,
    #[props(default)] fill_max_height: bool,
    children: Element,
) -> Element {
    let p = use_ember_theme().palette;
    rsx! {
        Column {
            weight,
            width,
            height,
            fill_max_width,
            fill_max_height,
            background: paint(p.surface),
            border_width: 1.0,
            border_color: paint(p.line),
            corner_radius: CORNER_LG,
            {children}
        }
    }
}

/// An icon-only stripe button: a faint tint when selected, no fill otherwise (D25).
#[component]
fn StripeButton(
    window: ToolWindow,
    selected: bool,
    tip: String,
    on_click: EventHandler<()>,
) -> Element {
    let p = use_ember_theme().palette;
    rsx! {
        Tooltip { text: tip,
            Button {
                text: "",
                icon: window.icon(),
                width: STRIPE_BTN,
                height: STRIPE_BTN,
                variant: ButtonVariant::Text,
                corner_radius: CORNER_XS,
                background: if selected { paint(p.sel) } else { transparent() },
                color: if selected { paint(p.text) } else { paint(p.text_2) },
                on_click: move |()| on_click.call(()),
            }
        }
    }
}

/// A pill: `ink` when selected.
#[component]
fn PillTab(#[props(into)] text: String, selected: bool, on_click: EventHandler<()>) -> Element {
    let p = use_ember_theme().palette;
    rsx! {
        Button {
            text,
            height: 40.0,
            variant: ButtonVariant::Text,
            shape_role: ShapeRole::Full,
            background: if selected { paint(p.ink) } else { transparent() },
            color: if selected { paint(p.ink_text) } else { paint(p.text_2) },
            on_click: move |()| on_click.call(()),
        }
    }
}

/// A tool window's body that is not built: its title and "준비 중입니다".
#[component]
fn SoonBody(#[props(into)] title: String, #[props(into)] note: String) -> Element {
    let p = use_ember_theme().palette;
    rsx! {
        Column {
            fill_max_width: true,
            weight: 1.0,
            padding: SP[3],
            spacing: SP[0],
            Label { text: title, fs: Fs::Md, weight: Weight::Semibold }
            Label { text: SOON, fs: Fs::Sm, color: paint(p.text_2) }
            Label { text: note, fs: Fs::Xs, color: paint(p.muted) }
        }
    }
}

#[component]
pub fn EditorScreen(target: EditorTarget) -> Element {
    let ui = use_ui();
    let p = use_ember_theme().palette;
    let os = Os::current();
    let keymap = ui.prefs.read().keymap;
    let mut tabs = use_signal(EditorTabs::default);
    let mut stripes = use_signal(Stripes::default);
    let mut menu_open = use_signal(|| false);
    let files = demo_files_from_env();

    let project = target.project.clone();
    let project_name = display_name(&project);
    let (agent_state, branch) = {
        let project = project.clone();
        ui.with_rows_no_previews(|s, _| {
            let boxes = project_boxes(s);
            let b = boxes.into_iter().find(|b| b.id == project);
            let state = b
                .as_ref()
                .and_then(|b| b.top)
                .map(|t: LauncherStatus| status_pill(t).0.to_string())
                .unwrap_or_else(|| "팀원 없음".to_string());
            let branch = b
                .and_then(|b| b.branch)
                .unwrap_or_else(|| "브랜치 미설정".to_string());
            (state, branch)
        })
    };

    let hint = move |a: Action| keymap.hint(a, os);
    let stripe_tip = move |w: ToolWindow| {
        let h = w.action().map(|a| keymap.hint(a, os)).unwrap_or_default();
        tooltip_text(w.label(), &h)
    };
    let mut run_action = move |a: Action| {
        let handled = {
            let mut t = tabs.write();
            let mut s = stripes.write();
            apply_action(a, &mut t, &mut s)
        };
        if !handled {
            soon_message();
        }
    };

    let sel = *stripes.read();
    let open_paths: Vec<(String, bool)> = {
        let t = tabs.read();
        t.paths()
            .into_iter()
            .map(|p| (p.to_string(), t.is_dirty(p)))
            .collect()
    };
    let current = tabs.read().current_path().map(str::to_string);
    let find_line = tooltip_text(keymap.find_label(), &hint(Action::Find));
    let folder_line = if target.folder.is_empty() {
        project_name.clone()
    } else {
        format!("{project_name}  {}", target.folder)
    };

    rsx! {
        Column {
            fill_max_width: true,
            fill_max_height: true,
            background: paint(p.bg),
            // ---- top bar --------------------------------------------------------------------
            Row {
                fill_max_width: true,
                height: TOP_H,
                padding_role: SpaceRole::Sm,
                space_role: SpaceRole::Sm,
                alignment: Alignment::CenterStart,
                Button {
                    text: "닫기",
                    icon: IconRole::Back,
                    height: 40.0,
                    variant: ButtonVariant::Text,
                    shape_role: ShapeRole::Full,
                    on_click: move |()| ui.close_editor(),
                }
                Label { text: project_name.clone(), fs: Fs::Md, weight: Weight::Semibold }
                Label { text: branch.clone(), fs: Fs::Sm, color: paint(p.text_2) }
                Spacer { weight: 1.0 }
                Button {
                    text: find_line,
                    icon: IconRole::Search,
                    height: 40.0,
                    variant: ButtonVariant::Text,
                    shape_role: ShapeRole::Full,
                    background: paint(p.surface),
                    border_width: 1.0,
                    border_color: paint(p.line),
                    color: paint(p.text_2),
                    on_click: move |()| soon_message(),
                }
                Button {
                    text: "실행",
                    icon: IconRole::Forward,
                    height: 40.0,
                    variant: ButtonVariant::Text,
                    shape_role: ShapeRole::Full,
                    on_click: move |()| soon_message(),
                }
            }
            // ---- middle: stripes, tool windows, editor ----------------------------------------
            Row {
                weight: 1.0,
                fill_max_width: true,
                spacing: SP[1],
                // left stripe: tool windows on top, the bottom ones at the foot
                Column {
                    width: STRIPE_W,
                    fill_max_height: true,
                    alignment: Alignment::TopCenter,
                    spacing: 2.0,
                    for w in ToolWindow::LEFT {
                        StripeButton {
                            key: "{w.label()}",
                            window: w,
                            selected: sel.is_selected(w),
                            tip: stripe_tip(w),
                            on_click: move |()| stripes.write().toggle(w),
                        }
                    }
                    Spacer { weight: 1.0 }
                    for w in ToolWindow::BOTTOM {
                        StripeButton {
                            key: "{w.label()}",
                            window: w,
                            selected: sel.is_selected(w),
                            tip: stripe_tip(w),
                            on_click: move |()| stripes.write().toggle(w),
                        }
                    }
                }
                if let Some(w) = sel.selected(Side::Left) {
                    Island { width: LEFT_W, fill_max_height: true,
                        match w {
                            ToolWindow::Project => rsx! {
                                Column { fill_max_width: true, weight: 1.0, padding: SP[3], spacing: SP[0], alignment: Alignment::TopStart,
                                    Label { text: "프로젝트", fs: Fs::Md, weight: Weight::Semibold }
                                    Label { text: folder_line.clone(), fs: Fs::Sm, color: paint(p.text_2), max_lines: 1 }
                                    if files.is_empty() {
                                        Label { text: format!("파일 목록은 {SOON}"), fs: Fs::Xs, color: paint(p.muted) }
                                    }
                                    // demo only, replaced when the server serves files
                                    for (path, _) in files.clone() {
                                        Button {
                                            key: "{path}",
                                            text: path,
                                            variant: ButtonVariant::Text,
                                            color: paint(p.text),
                                            fill_max_width: true,
                                            on_click: move |_| tabs.write().open(path),
                                        }
                                    }
                                }
                            },
                            other => rsx! { SoonBody { title: other.label(), note: "서버가 알려 주면 이곳에 표시됩니다." } },
                        }
                    }
                }
                // editor column: the editor island above, the bottom tool window below
                Column {
                    weight: 1.0,
                    fill_max_height: true,
                    spacing: SP[1],
                    Island { weight: 1.0, fill_max_width: true,
                        // tabs
                        Row {
                            fill_max_width: true,
                            padding_role: SpaceRole::Xs,
                            space_role: SpaceRole::Xs,
                            alignment: Alignment::CenterStart,
                            for (path, dirty) in open_paths.clone() {
                                Row { key: "{path}", alignment: Alignment::CenterStart,
                                    PillTab {
                                        text: format!("{}{}", base_name(&path), if dirty { " \u{25CF}" } else { "" }),
                                        selected: current.as_deref() == Some(path.as_str()),
                                        on_click: {
                                            let path = path.clone();
                                            move |()| tabs.write().activate(&path)
                                        },
                                    }
                                    Button {
                                        text: "",
                                        icon: IconRole::Close,
                                        width: 40.0,
                                        height: 40.0,
                                        variant: ButtonVariant::Text,
                                        on_click: {
                                            let path = path.clone();
                                            move |()| tabs.write().close(&path)
                                        },
                                    }
                                }
                            }
                            Spacer { weight: 1.0 }
                            Menu {
                                expanded: menu_open(),
                                on_dismiss: move |_| menu_open.set(false),
                                anchor: rsx! {
                                    Button { text: "", icon: IconRole::More, width: 40.0, height: 40.0, variant: ButtonVariant::Text, on_click: move |_| menu_open.set(true) }
                                },
                                for a in [Action::Save, Action::Recent, Action::Format, Action::GotoLine, Action::Palette] {
                                    Button {
                                        key: "{a.label()}",
                                        text: tooltip_text(
                                            if a == Action::Palette { keymap.palette_label() } else { a.label() },
                                            &hint(a),
                                        ),
                                        variant: ButtonVariant::Text,
                                        height: 40.0,
                                        fill_max_width: true,
                                        on_click: move |_| {
                                            menu_open.set(false);
                                            run_action(a);
                                        },
                                    }
                                }
                                Row { fill_max_width: true, height: 40.0, padding_role: SpaceRole::Sm, alignment: Alignment::CenterStart,
                                    Label { text: "단축키 체계", fs: Fs::Xs, color: paint(p.muted) }
                                }
                                for k in Keymap::ALL {
                                    Button {
                                        key: "{k.label()}",
                                        text: format!(
                                            "{} {}  {}",
                                            if k == keymap { "\u{2713}" } else { "\u{2003}" },
                                            k.label(),
                                            k.hint(Action::Find, os),
                                        ),
                                        variant: ButtonVariant::Text,
                                        height: 40.0,
                                        fill_max_width: true,
                                        on_click: move |_| {
                                            menu_open.set(false);
                                            ui.update_prefs(|p| p.keymap = k);
                                        },
                                    }
                                }
                            }
                        }
                        // breadcrumbs
                        if let Some(path) = current.clone() {
                            Row { fill_max_width: true, padding_role: SpaceRole::Sm,
                                Label { text: path.replace('/', " \u{203A} "), fs: Fs::Xs, color: paint(p.text_2) }
                            }
                        }
                        // code area
                        Column { weight: 1.0, fill_max_width: true, padding: SP[3], spacing: SP[0],
                            if let Some(path) = current.clone() {
                                Label { text: base_name(&path).to_string(), fs: Fs::Md, weight: Weight::Semibold }
                                // demo only, replaced when the server serves files
                                if let Some((_, body)) = files.iter().find(|(f, _)| *f == path.as_str()) {
                                    for (n, line) in body.lines().enumerate() {
                                        Label { key: "{n}", text: format!("{:>3}  {line}", n + 1), fs: Fs::Sm }
                                    }
                                } else {
                                    Label { text: SOON, fs: Fs::Sm, color: paint(p.text_2) }
                                }
                                // TODO(#166: 코드 편집기 위젯): `ember/editor` widget once the pinned toolkit has CodeEditor.
                                Label { text: "코드 편집기는 편집기 코어(FR-E1)가 연결되면 표시됩니다.", fs: Fs::Xs, color: paint(p.muted) }
                            } else {
                                Label { text: "열린 파일이 없습니다", fs: Fs::Md, weight: Weight::Semibold }
                                Label { text: tooltip_text(keymap.find_label(), &hint(Action::Find)), fs: Fs::Sm, color: paint(p.text_2) }
                                Label { text: tooltip_text("프로젝트 창", &hint(Action::Project)), fs: Fs::Sm, color: paint(p.text_2) }
                            }
                        }
                    }
                    if let Some(w) = sel.selected(Side::Bottom) {
                        Island { height: BOTTOM_H, fill_max_width: true,
                            Row {
                                fill_max_width: true,
                                padding_role: SpaceRole::Xs,
                                space_role: SpaceRole::Xs,
                                alignment: Alignment::CenterStart,
                                for b in ToolWindow::BOTTOM {
                                    PillTab {
                                        key: "{b.label()}",
                                        text: b.label(),
                                        selected: b == w,
                                        on_click: move |()| {
                                            if !stripes.peek().is_selected(b) {
                                                stripes.write().toggle(b);
                                            }
                                        },
                                    }
                                }
                                Spacer { weight: 1.0 }
                                Button {
                                    text: "",
                                    icon: IconRole::Collapse,
                                    width: 40.0,
                                    height: 40.0,
                                    variant: ButtonVariant::Text,
                                    on_click: move |()| stripes.write().hide(Side::Bottom),
                                }
                            }
                            // TODO(#166: 터미널 스트림): the node's terminal stream (FR-P) once the client has one.
                            SoonBody {
                                title: w.label(),
                                note: match w {
                                    ToolWindow::Terminal => "노드의 터미널 스트림이 연결되면 이곳에 표시됩니다.",
                                    ToolWindow::Run => "실행 구성이 연결되면 이곳에 출력이 표시됩니다.",
                                    _ => "편집기 코어가 연결되면 진단이 이곳에 표시됩니다.",
                                },
                            }
                        }
                    }
                }
                if let Some(w) = sel.selected(Side::Right) {
                    Island { width: LEFT_W, fill_max_height: true,
                        SoonBody { title: w.label(), note: "서버가 알려 주면 이곳에 표시됩니다." }
                    }
                }
                // right stripe
                Column {
                    width: STRIPE_W,
                    fill_max_height: true,
                    alignment: Alignment::TopCenter,
                    spacing: 2.0,
                    for w in ToolWindow::RIGHT {
                        StripeButton {
                            key: "{w.label()}",
                            window: w,
                            selected: sel.is_selected(w),
                            tip: stripe_tip(w),
                            on_click: move |()| stripes.write().toggle(w),
                        }
                    }
                }
            }
            // ---- status bar -----------------------------------------------------------------
            Row {
                fill_max_width: true,
                height: STATUS_H,
                padding_role: SpaceRole::Sm,
                space_role: SpaceRole::Md,
                alignment: Alignment::CenterStart,
                Label { text: folder_line.clone(), fs: Fs::Xs, color: paint(p.muted), max_lines: 1 }
                Spacer { weight: 1.0 }
                Label { text: format!("에이전트 {agent_state}"), fs: Fs::Xs, color: paint(p.text_2) }
                if current.is_some() {
                    Label { text: position_text(1, 1), fs: Fs::Xs, color: paint(p.text_2) }
                    Label { text: "LF", fs: Fs::Xs, color: paint(p.text_2) }
                    Label { text: "UTF-8", fs: Fs::Xs, color: paint(p.text_2) }
                }
                Label { text: branch, fs: Fs::Xs, color: paint(p.text_2) }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_toggle_tool_windows_and_close_tabs() {
        let mut tabs = EditorTabs::default();
        let mut stripes = Stripes::default();
        assert!(apply_action(Action::Terminal, &mut tabs, &mut stripes));
        assert_eq!(stripes.selected(Side::Bottom), None);
        assert!(apply_action(Action::Problems, &mut tabs, &mut stripes));
        assert!(stripes.is_selected(ToolWindow::Problems));
        assert!(!apply_action(Action::Close, &mut tabs, &mut stripes));
        tabs.open("a.rs");
        assert!(apply_action(Action::Close, &mut tabs, &mut stripes));
        assert_eq!(tabs.current_path(), None);
        assert!(!apply_action(Action::Format, &mut tabs, &mut stripes));
    }

    #[test]
    fn demo_files_are_empty_unless_the_switch_is_one() {
        assert!(demo_files(None).is_empty());
        assert!(demo_files(Some("0")).is_empty());
        assert!(demo_files(Some("true")).is_empty());
        let f = demo_files(Some("1"));
        assert!(f.len() >= 4);
        assert!(f.iter().any(|(p, _)| p.contains('/')), "nested paths give breadcrumbs");
        assert!(f.iter().all(|(_, c)| !c.is_empty()));
        let mut tabs = EditorTabs::default();
        tabs.open(f[2].0);
        assert_eq!(tabs.current_path(), Some(f[2].0));
    }

    #[test]
    fn tooltip_and_base_name() {
        assert_eq!(tooltip_text("저장", ""), "저장");
        assert_eq!(tooltip_text("저장", "Ctrl+S"), "저장  Ctrl+S");
        assert_eq!(base_name("ember/app/src/lib.rs"), "lib.rs");
        assert_eq!(base_name("lib.rs"), "lib.rs");
    }

    #[test]
    fn every_stripe_window_belongs_to_one_list() {
        for w in ToolWindow::ALL {
            let n = [
                &ToolWindow::LEFT[..],
                &ToolWindow::RIGHT[..],
                &ToolWindow::BOTTOM[..],
            ]
            .iter()
            .filter(|l| l.contains(&w))
            .count();
            assert_eq!(n, 1, "{w:?}");
        }
    }
}
