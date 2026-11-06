//! Editor target and tab state.

use dioxus_compose::prelude::*;
use crate::ui::keymap::{Action, Chord};


/// What the editor was opened for.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct EditorTarget {
    pub project: String,
    /// The project's folder on its computer, when the server said (`IdeLaunch.folder`).
    pub folder: String,
    pub endpoint: String,
    pub token: String,
    /// The ember server's base URL, for the node terminal relay. Empty when unknown.
    pub server_url: String,
    /// The id of the computer the project lives on. Empty when unknown.
    pub computer: String,
}

impl std::fmt::Debug for EditorTarget {
    // The token authorizes the editor connection, so it never reaches logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditorTarget")
            .field("project", &self.project)
            .field("folder", &self.folder)
            .field("endpoint", &self.endpoint)
            .field("token", &"[REDACTED]")
            .field("server_url", &self.server_url)
            .field("computer", &self.computer)
            .finish()
    }
}

/// The status bar's cursor position, "N행 M열".
pub fn position_text(line: usize, column: usize) -> String {
    format!("{line}행 {column}열")
}

/// The last path segment.
pub fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
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

/// The path segments the breadcrumbs show: the path below the project folder when it lies
/// inside it, split on `/` with empty segments dropped.
pub fn breadcrumb_segments(path: &str, folder: &str) -> Vec<String> {
    let folder = folder.trim_end_matches('/');
    let rest = match path.strip_prefix(folder) {
        Some(r) if !folder.is_empty() && r.starts_with('/') => r,
        _ => path,
    };
    rest.split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// One row of the explorer tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub path: String,
    pub name: String,
    pub depth: u32,
    pub is_dir: bool,
    pub open: bool,
}

/// The rows of the explorer: the folder's entries with directories first, then files by name,
/// and below an expanded directory its own entries once they have been listed.
pub fn tree_rows(
    root: &str,
    listings: &std::collections::HashMap<String, Vec<(String, u32)>>,
    expanded: &std::collections::HashSet<String>,
) -> Vec<TreeRow> {
    fn walk(
        dir: &str,
        depth: u32,
        listings: &std::collections::HashMap<String, Vec<(String, u32)>>,
        expanded: &std::collections::HashSet<String>,
        rows: &mut Vec<TreeRow>,
    ) {
        let Some(entries) = listings.get(dir) else { return };
        let is_dir = |kind: u32| kind & ember_editor_conn::remote_fs::file_type::DIRECTORY != 0;
        let mut sorted: Vec<&(String, u32)> = entries.iter().collect();
        sorted.sort_by_key(|(name, kind)| (!is_dir(*kind), name.to_lowercase()));
        for (name, kind) in sorted {
            let path = super::editor_workbench::join_path(dir, name);
            let dir_row = is_dir(*kind);
            let open = dir_row && expanded.contains(&path);
            rows.push(TreeRow { path: path.clone(), name: name.clone(), depth, is_dir: dir_row, open });
            if open {
                walk(&path, depth + 1, listings, expanded, rows);
            }
        }
    }
    let mut rows = Vec::new();
    walk(root, 0, listings, expanded, &mut rows);
    rows

/// The editor page's grid tracks as inline style: the mockup's `.ed` grid with its `.no-left` and
/// `.no-bottom` cases, a closed left column 0 wide and a closed bottom row 0 high.
pub fn editor_grid_style(has_left: bool, has_bottom: bool) -> String {
    format!(
        "grid-template-columns:{}; grid-template-rows:{};",
        if has_left { "var(--stw) 280px minmax(0,1fr) var(--stw)" } else { "var(--stw) 0 minmax(0,1fr) var(--stw)" },
        if has_bottom { "52px minmax(0,1fr) 260px var(--sbh)" } else { "52px minmax(0,1fr) 0 var(--sbh)" },
    )
}

/// The breadcrumb line: segments joined with a right-angle quote.
pub fn breadcrumb_text(path: &str, folder: &str) -> String {
    breadcrumb_segments(path, folder).join(" \u{203A} ")
}

/// The status bar's agent field.
pub fn agent_status_text(state: &str) -> String {
    format!("에이전트 {state}")
}

/// The chord a pressed key makes. `key` is the key's name as the keymap table writes it
/// (`P`, `F12`, `Esc`); a single letter is upper-cased so Shift does not change the match.
pub fn chord_from_key(key: &str, ctrl: bool, shift: bool, alt: bool, meta: bool) -> Chord {
    let key = if key.chars().count() == 1 { key.to_uppercase() } else { key.to_string() };
    Chord::Keys { ctrl, shift, alt, meta, key }
}

/// The action a pressed key triggers under a keymap, if any. `key` is the key's name as the
/// keymap table writes it.
pub fn action_for_key(
    keymap: crate::ui::keymap::Keymap,
    os: crate::ui::keymap::Os,
    key: &str,
    ctrl: bool,
    shift: bool,
    alt: bool,
    meta: bool,
) -> Option<Action> {
    keymap.action_for(&chord_from_key(key, ctrl, shift, alt, meta), os)
}

/// How long after a Shift press a second one still counts as a double press.
pub const DOUBLE_SHIFT_MS: u64 = 500;

/// Finds Shift pressed twice in quick succession, the JetBrains chord for search everywhere.
/// Feed it every key down event; it answers true on the second press of a pair.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DoubleShift {
    last_press_ms: Option<u64>,
}

impl DoubleShift {
    /// One key down event at `now_ms`. `repeat` is the operating system's auto repeat for a held
    /// key, which is not a new press. Any key but a bare Shift breaks a pair that was started.
    pub fn press(&mut self, key: &str, repeat: bool, ctrl: bool, alt: bool, meta: bool, now_ms: u64) -> bool {
        if key != "Shift" || ctrl || alt || meta {
            self.last_press_ms = None;
            return false;
        }
        if repeat {
            return false;
        }
        match self.last_press_ms.take() {
            Some(first) if now_ms.saturating_sub(first) <= DOUBLE_SHIFT_MS => true,
            _ => {
                self.last_press_ms = Some(now_ms);
                false
            }
        }
    }
}

pub fn tooltip_text(label: &str, hint: &str) -> String {
    if hint.is_empty() {
        label.to_string()
    } else {
        format!("{label}  {hint}")
    }
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

#[component]
pub fn EditorScreen(target: EditorTarget) -> Element {
    // The editor runs as its own HTML-mode window (`editor_workbench::page::app`); this screen
    // is only reached when a Compose root asks for it, and draws nothing.
    let _ = target;
    rsx! {}
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::keymap::{Keymap, Os};

    #[test]
    fn fr_l31_actions_toggle_tool_windows_and_close_tabs() {
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
    fn fr_l31_tooltip_and_base_name() {
        assert_eq!(tooltip_text("저장", ""), "저장");
        assert_eq!(tooltip_text("저장", "Ctrl+S"), "저장  Ctrl+S");
        assert_eq!(base_name("ember/app/src/lib.rs"), "lib.rs");
        assert_eq!(base_name("lib.rs"), "lib.rs");
    }

    #[test]
    fn fr_l31_every_stripe_window_belongs_to_one_list() {
        for w in ToolWindow::ALL {
            let n = [&ToolWindow::LEFT[..], &ToolWindow::RIGHT[..], &ToolWindow::BOTTOM[..]]
                .iter()
                .filter(|l| l.contains(&w))
                .count();
            assert_eq!(n, 1, "{w:?}");
        }
    }

    #[test]
    fn fr_l31_key_chords_drive_tabs_and_stripes() {
        let mut tabs = EditorTabs::default();
        let mut stripes = Stripes::default();
        tabs.open("a.rs");
        tabs.open("b.rs");

        let close = action_for_key(Keymap::VsCode, Os::Other, "w", true, false, false, false);
        assert_eq!(close, Some(Action::Close));
        assert!(apply_action(close.unwrap(), &mut tabs, &mut stripes));
        assert_eq!(tabs.paths(), vec!["a.rs"]);

        assert!(stripes.is_selected(ToolWindow::Terminal));
        let terminal = action_for_key(Keymap::VsCode, Os::Other, "`", true, false, false, false);
        assert_eq!(terminal, Some(Action::Terminal));
        assert!(apply_action(terminal.unwrap(), &mut tabs, &mut stripes));
        assert_eq!(stripes.selected(Side::Bottom), None);

        assert_eq!(action_for_key(Keymap::VsCode, Os::Other, "w", false, false, false, false), None);
    }

    #[test]
    fn fr_l31_editor_target_debug_redacts_the_token() {
        let target = EditorTarget {
            project: "p".into(),
            folder: "/f".into(),
            endpoint: "127.0.0.1:9000".into(),
            token: "s3cret-token".into(),
            server_url: "http://srv:1".into(),
            computer: "node-a".into(),
        };
        let text = format!("{target:?}");
        assert!(!text.contains("s3cret-token"), "{text}");
        assert!(text.contains("[REDACTED]"));
        assert!(text.contains("127.0.0.1:9000"));
        assert!(text.contains("http://srv:1") && text.contains("node-a"));
    }
}
