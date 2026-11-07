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

/// A Korean reason for a failed open or save. The server's own text is English, can carry paths and
/// control characters, and is not for the person: the caller logs it and shows this.
pub fn file_failure_reason(error: &str) -> &'static str {
    let e = error.to_lowercase();
    if e.contains("entrynotfound") || e.contains("enoent") || e.contains("not found") || e.contains("no such file") {
        "파일이 없습니다. 지워졌거나 이름이 바뀌었을 수 있습니다"
    } else if e.contains("nopermissions") || e.contains("eacces") || e.contains("eperm") || e.contains("permission") {
        "파일을 읽거나 쓸 권한이 없습니다"
    } else if e.contains("utf-8") || e.contains("utf8") {
        "UTF-8 텍스트 파일이 아니어서 열 수 없습니다"
    } else if e.contains("connection closed") || e.contains("timed out") || e.contains("io:") || e.contains("handshake") || e.contains("refused") {
        "서버와의 연결이 끊어졌습니다. 연결을 확인한 뒤 다시 시도하세요"
    } else if e.contains("filetoolarge") || e.contains("too large") {
        "파일이 너무 커서 열 수 없습니다"
    } else {
        "알 수 없는 오류입니다. 자세한 내용은 로그에 있습니다"
    }
}

/// A file name as shown to the person: control characters (newlines, escapes) are dropped, so a name
/// cannot break the layout or smuggle terminal sequences, and a very long one is shortened.
pub fn display_name(name: &str) -> String {
    let clean: String = name.chars().filter(|c| !c.is_control()).collect();
    if clean.chars().count() > 80 {
        let head: String = clean.chars().take(79).collect();
        format!("{head}…")
    } else {
        clean
    }
}

/// Whether a click on a tree row ends the pending name prompt. An empty field has nothing to lose; a
/// typed name is kept (the person may have clicked by mistake), and a delete question is not a field.
pub fn row_click_closes_prompt(prompt: &TreePrompt, typed: &str) -> bool {
    match prompt {
        TreePrompt::NewFile { .. } | TreePrompt::NewDir { .. } | TreePrompt::Rename { .. } => typed.trim().is_empty(),
        TreePrompt::Delete { .. } => false,
    }
}

/// The widget's colour role for a syntax role: the design system resolves it, light and dark.
pub fn colour_role(role: ember_editor::textmate::SyntaxRole) -> compose_rust::ColorRole {
    use compose_rust::ColorRole as C;
    use ember_editor::textmate::SyntaxRole as R;
    match role {
        R::Keyword => C::SyntaxKeyword,
        R::String => C::SyntaxString,
        R::Comment => C::SyntaxComment,
        R::Number => C::SyntaxNumber,
        R::Constant => C::SyntaxConstant,
        R::Type => C::SyntaxType,
        R::Function => C::SyntaxFunction,
        R::Variable => C::SyntaxVariable,
        R::Property => C::SyntaxProperty,
        R::Operator => C::SyntaxOperator,
        R::Punctuation => C::SyntaxPunctuation,
        R::Tag => C::SyntaxTag,
        R::Attribute => C::SyntaxAttribute,
        R::Escape => C::SyntaxEscape,
        R::Macro => C::SyntaxMacro,
    }
}

/// The `syntax_spans` prop for the coloured runs the session reported.
pub fn syntax_spans(syntax: &ember_editor::SyntaxRuns) -> compose_rust::code::SyntaxSpans {
    use compose_rust::code::{CodeRange, SyntaxSpan, SyntaxSpans};
    SyntaxSpans::new(
        syntax.runs.iter().map(|r| SyntaxSpan::role(syntax.version, CodeRange::of(r.line, r.start, r.line, r.end), colour_role(r.role))),
    )
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

    /// A path was renamed: every tab at or below `old` takes the matching path below `new`, keeping its
    /// place and its dirty mark. Returns the (old, new) pairs of the tabs that moved.
    pub fn rename_prefix(&mut self, old: &str, new: &str) -> Vec<(String, String)> {
        let old = old.trim_end_matches('/');
        let new = new.trim_end_matches('/');
        let mut moved = Vec::new();
        for (path, _) in &mut self.tabs {
            if is_at_or_below(path, old) {
                let renamed = format!("{new}{}", &path[old.len()..]);
                moved.push((std::mem::replace(path, renamed.clone()), renamed));
            }
        }
        moved
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
}

/// The editor page's grid tracks as inline style. An open tool window adds nothing: the
/// stylesheet's tracks apply, including its width breakpoints. A closed left column is 0 wide and
/// a closed bottom row is 0 high (the mockup's `.no-left` and `.no-bottom`).
pub fn editor_grid_style(has_left: bool, has_bottom: bool) -> String {
    let mut style = String::new();
    if !has_left {
        style.push_str("grid-template-columns:var(--stw) 0 minmax(0,1fr) var(--stw); ");
    }
    if !has_bottom {
        style.push_str("grid-template-rows:52px minmax(0,1fr) 0 var(--sbh); ");
    }
    style
}

/// The page's height rule: macOS opens the window FullSize, so the page is the window's height;
/// elsewhere the native chrome stays and the page keeps its 76px allowance.
pub fn editor_page_height(full_size: bool) -> &'static str {
    if full_size { "100vh" } else { "calc(100vh - 76px)" }
}

/// The top bar's left padding past the window controls: under FullSize the traffic lights float
/// over its left end, so it starts after their footprint; otherwise nothing is added.
pub fn editor_strip_padding(full_size: bool, leading: f32) -> String {
    if full_size { format!("padding-left:{leading}px;") } else { String::new() }
}

/// The page grid while the bottom tool window is maximized (the mockup's `.ed.max`): the editor row
/// is 0 high and the bottom window takes its row and the one below.
pub fn editor_grid_style_maximized() -> String {
    "grid-template-rows:52px 0 minmax(0,1fr) var(--sbh);".to_string()
}

/// The breadcrumb line: segments joined with a right-angle quote.
pub fn breadcrumb_text(path: &str, folder: &str) -> String {
    breadcrumb_segments(path, folder).join(" \u{203A} ")
}

/// A pending explorer edit the tree is asking a name or a confirmation for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreePrompt {
    NewFile { parent: String },
    NewDir { parent: String },
    Rename { path: String },
    Delete { path: String, is_dir: bool },
}

/// The directory a new file or folder goes in: the selected directory, the directory of the
/// selected file, or the project folder when nothing is selected.
pub fn new_item_parent(selected: Option<(&str, bool)>, root: &str) -> String {
    match selected {
        Some((path, true)) => path.to_string(),
        Some((path, false)) => match path.trim_end_matches('/').rsplit_once('/') {
            Some(("", _)) | None => "/".to_string(),
            Some((parent, _)) => parent.to_string(),
        },
        None => root.to_string(),
    }
}

/// What the delete confirmation says. There is no trash, so it says the deletion cannot be undone,
/// a directory goes with everything in it, and which open files have unsaved changes that go too.
pub fn delete_confirmation(path: &str, folder: &str, is_dir: bool, unsaved: &[String]) -> String {
    let name = breadcrumb_segments(path, folder).join("/");
    let mut text = if is_dir {
        format!("폴더 {name}와 그 안의 모든 파일을 삭제할까요? 휴지통이 없어 되돌릴 수 없습니다.")
    } else {
        format!("파일 {name}을 삭제할까요? 휴지통이 없어 되돌릴 수 없습니다.")
    };
    if !unsaved.is_empty() {
        let names: Vec<String> = unsaved.iter().map(|p| breadcrumb_segments(p, folder).join("/")).collect();
        text.push_str(&format!(" 저장하지 않은 변경이 있는 파일: {}. 삭제하면 그 편집도 사라집니다.", names.join(", ")));
    }
    text
}

/// Why an explorer edit failed, in Korean: the server's own English text is not shown, only what kind
/// of failure it was.
pub fn edit_failure_text(action: &str, detail: &str) -> String {
    let d = detail.to_lowercase();
    let reason = if d.contains("exists") || d.contains("eexist") {
        "같은 이름이 이미 있습니다"
    } else if d.contains("not found") || d.contains("enoent") || d.contains("does not exist") {
        "대상을 찾을 수 없습니다"
    } else if d.contains("permission") || d.contains("eacces") || d.contains("eperm") {
        "권한이 없습니다"
    } else if d.contains("not empty") || d.contains("enotempty") {
        "폴더가 비어 있지 않습니다"
    } else if d.contains("closed") || d.contains("timed out") || d.contains("connection") {
        "서버와의 연결이 끊겼습니다"
    } else {
        "서버가 요청을 처리하지 못했습니다"
    };
    format!("{action} 실패: {reason}")
}

/// The delete button answers only once the question has been on screen for 600 ms, so a stray double
/// click that opened it cannot also confirm it.
pub const DELETE_GUARD_MS: u64 = 600;

pub fn delete_click_allowed(opened_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(opened_ms) >= DELETE_GUARD_MS
}

/// Whether closing `tab` is needed after `gone` was deleted or renamed: it is that path or below it.
pub fn is_at_or_below(tab: &str, gone: &str) -> bool {
    let gone = gone.trim_end_matches('/');
    tab == gone || tab.strip_prefix(gone).is_some_and(|rest| rest.starts_with('/'))
}

/// The accessible name of a tab: the file name, and that its edits are not saved.
pub fn tab_aria_label(name: &str, dirty: bool) -> String {
    if dirty { format!("{name} (저장 안 됨)") } else { name.to_string() }
}

/// The accessible name of a tree row: the name, and that it is a folder.
pub fn tree_item_aria_label(name: &str, is_dir: bool) -> String {
    if is_dir { format!("{name} 폴더") } else { name.to_string() }
}

/// The accessible name of a problem row: what it is, what it says, and where.
pub fn problem_aria_label(severity: &str, message: &str, location: &str) -> String {
    format!("{severity}: {message} ({location})")
}

/// The problems tab's label: the tool window's name, with the count when there is one.
pub fn problems_tab_label(count: usize) -> String {
    if count == 0 { ToolWindow::Problems.label().to_string() } else { format!("{} {count}", ToolWindow::Problems.label()) }
}

/// Where a problem is, as a row shows it: the file below the project folder and the 1-based line.
pub fn problem_location(path: &str, folder: &str, line: u32) -> String {
    format!("{}:{}", breadcrumb_segments(path, folder).join("/"), line + 1)
}

/// The status bar's agent dot: orange while waiting for approval, blue while running, green otherwise.
pub fn agent_dot_class(state: &str) -> &'static str {
    match state {
        "승인 대기" => "dot wait",
        "실행 중" => "dot run",
        _ => "dot ok",
    }
}

/// The status bar's path: the folder's name, then the directories of the open file below it.
pub fn status_crumbs(folder: &str, current: Option<&str>) -> String {
    let name = folder.trim_end_matches('/').rsplit('/').next().unwrap_or(folder);
    let mut parts = vec![name.to_string()];
    if let Some(path) = current {
        let mut dirs = breadcrumb_segments(path, folder);
        dirs.pop();
        parts.extend(dirs);
    }
    parts.join(" \u{203A} ")
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
    /// Search across files and replace (FR-E7).
    Search,
    Commit,
    Git,
    Agent,
    Notifications,
    Terminal,
    Run,
    Problems,
}

impl ToolWindow {
    pub const ALL: [ToolWindow; 9] = [
        ToolWindow::Project,
        ToolWindow::Search,
        ToolWindow::Commit,
        ToolWindow::Git,
        ToolWindow::Agent,
        ToolWindow::Notifications,
        ToolWindow::Terminal,
        ToolWindow::Run,
        ToolWindow::Problems,
    ];
    pub const LEFT: [ToolWindow; 4] = [ToolWindow::Project, ToolWindow::Search, ToolWindow::Commit, ToolWindow::Git];
    pub const RIGHT: [ToolWindow; 2] = [ToolWindow::Agent, ToolWindow::Notifications];
    pub const BOTTOM: [ToolWindow; 3] =
        [ToolWindow::Terminal, ToolWindow::Run, ToolWindow::Problems];

    pub fn label(self) -> &'static str {
        match self {
            ToolWindow::Project => "프로젝트",
            ToolWindow::Search => "검색",
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
            ToolWindow::Project | ToolWindow::Search | ToolWindow::Commit | ToolWindow::Git => Side::Left,
            ToolWindow::Agent | ToolWindow::Notifications => Side::Right,
            ToolWindow::Terminal | ToolWindow::Run | ToolWindow::Problems => Side::Bottom,
        }
    }

    /// The keymap action that opens it.
    pub fn action(self) -> Option<Action> {
        match self {
            ToolWindow::Project => Some(Action::Project),
            ToolWindow::Search => Some(Action::FindInFiles),
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
            ToolWindow::Search => IconRole::Search,
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


/// Whether the editor does something for `action` today ([`apply_action`] returns true for it
/// when it can act). The palette shows the others as not ready instead of as live commands.
pub fn is_implemented(action: Action) -> bool {
    // The right tool windows (agent, notifications) have no panel in the window yet: toggling one shows nothing.
    ToolWindow::from_action(action).is_some_and(|w| w.side() != Side::Right)
        || matches!(action, Action::Close | Action::HideTool | Action::FindInFile | Action::ReplaceInFiles | Action::Recent | Action::GotoLine)
}

/// Do what a keymap action does. Returns false for an action the editor cannot do yet (the
/// caller says "준비 중입니다"). The key handler calls this with `Keymap::action_for`.
pub fn apply_action(action: Action, tabs: &mut EditorTabs, stripes: &mut Stripes) -> bool {
    if let Some(w) = ToolWindow::from_action(action) {
        if w.side() == Side::Right {
            return false;
        }
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

    #[test]
    fn fr_e6_every_syntax_role_has_its_own_widget_colour_role() {
        use ember_editor::textmate::SyntaxRole as R;
        let roles = [
            R::Keyword, R::String, R::Comment, R::Number, R::Constant, R::Type, R::Function, R::Variable, R::Property, R::Operator,
            R::Punctuation, R::Tag, R::Attribute, R::Escape, R::Macro,
        ];
        let mut seen = std::collections::HashSet::new();
        for r in roles {
            assert!(seen.insert(format!("{:?}", colour_role(r))), "{r:?} shares a colour role");
        }
        assert_eq!(seen.len(), 15);
    }

    #[test]
    fn fr_e6_the_syntax_spans_prop_carries_each_run_with_the_version_it_was_written_against() {
        use ember_editor::textmate::{DocRun, SyntaxRole};
        let syntax = ember_editor::SyntaxRuns {
            version: 7,
            runs: vec![DocRun { line: 2, start: 1, end: 5, role: SyntaxRole::Keyword }, DocRun { line: 3, start: 0, end: 2, role: SyntaxRole::Comment }],
        };
        let spans = syntax_spans(&syntax).spans();
        assert_eq!(spans.len(), 2);
        assert!(spans.iter().all(|s| s.version == 7));
        assert!(syntax_spans(&ember_editor::SyntaxRuns::default()).is_empty());
    }

    #[test]
    fn fr_l31_failure_reasons_are_korean_and_never_the_server_text() {
        assert!(file_failure_reason("remote error: EntryNotFound (FileSystemError)").contains("없습니다"));
        assert!(file_failure_reason("NoPermissions (FileSystemError): /root/x").contains("권한"));
        assert!(file_failure_reason("file is not UTF-8").contains("UTF-8"));
        assert!(file_failure_reason("connection closed").contains("연결"));
        assert!(file_failure_reason("timed out: readFile").contains("연결"));
        let other = file_failure_reason("weird \u{1b}[31m thing at /secret/path");
        assert!(!other.contains("secret") && !other.contains('\u{1b}'));
    }

    #[test]
    fn fr_l31_display_name_drops_control_characters_and_shortens() {
        assert_eq!(display_name("a\nb\u{1b}[2Jc.txt"), "ab[2Jc.txt");
        assert_eq!(display_name(&"x".repeat(200)).chars().count(), 80);
        assert_eq!(display_name("README.md"), "README.md");
    }

    #[test]
    fn fr_l31_row_click_keeps_a_typed_name_and_closes_an_empty_field() {
        let new_file = TreePrompt::NewFile { parent: "/w".into() };
        assert!(row_click_closes_prompt(&new_file, ""));
        assert!(row_click_closes_prompt(&new_file, "  "));
        assert!(!row_click_closes_prompt(&new_file, "notes.md"));
        assert!(!row_click_closes_prompt(&TreePrompt::Rename { path: "/w/a".into() }, "b"));
        assert!(!row_click_closes_prompt(&TreePrompt::Delete { path: "/w/a".into(), is_dir: false }, ""));
    }
    use crate::ui::keymap::{Keymap, Os};

    #[test]
    fn fr_l31_is_implemented_matches_what_apply_action_does() {
        for action in Action::ALL {
            // The palette, find, find-in-file, replace-in-files, recent files and goto-line open sheets and bars in the window:
            // they are not tab or tool window changes, so `apply_action` does not do them.
            if matches!(action, Action::Palette | Action::Find | Action::FindInFile | Action::ReplaceInFiles | Action::Recent | Action::GotoLine) {
                continue;
            }
            let mut tabs = EditorTabs::default();
            tabs.open("/w/p/a.rs");
            let mut stripes = Stripes::default();
            assert_eq!(apply_action(action, &mut tabs, &mut stripes), is_implemented(action), "{action:?}");
        }
    }

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
