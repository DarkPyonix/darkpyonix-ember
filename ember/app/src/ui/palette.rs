//! The command palette's model (D38, #447): fuzzy ranking of commands and files. Pure; the sheet
//! that shows it is in `editor_workbench`.
//!
//! A title matches when it holds the query's letters in order (case ignored, blanks in the query
//! ignored). The best alignment is chosen by score: a letter that starts a word or the title, a run of
//! letters in a row, and a late start all count. Equal scores: the shorter title, then the list's order.

use super::keymap::{Action, Keymap, Os};
pub use ember_editor::{parse_contributed_commands, ContributedCommand};

/// What choosing an item does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    /// One of the editor's own actions.
    Action(Action),
    /// A file, by its path.
    File(String),
    /// A file, by its path, with line and optional column.
    FileAt { path: String, line: usize, column: Option<usize> },
    /// Jump caret to a line and optional column in the current file.
    GotoLine { line: usize, column: Option<usize> },
    /// An extension command, by its id.
    ExtensionCommand(String),
    /// Change the active document's line ending (FR-L31.7).
    SetEol(ember_editor::Eol),
    /// Go on to the encoding list for reopening or saving (FR-L31.7).
    EncodingAction(ember_editor::EncodingMode),
    /// Reopen or save the active document in an encoding (FR-L31.7).
    SetEncoding { mode: ember_editor::EncodingMode, encoding: ember_editor::Encoding },
    /// Put the active document under a language id; `None` is 자동 감지 (FR-L31.7).
    SetLanguage(Option<String>),
    /// Close the picker and do nothing.
    Cancel,
}

/// One line of the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub payload: Payload,
    pub title: String,
    /// The shortcut, or empty.
    pub hint: String,
    /// False for a command the editor cannot do yet: it is listed after the working ones, shown as
    /// not ready, and choosing it does nothing.
    pub enabled: bool,
}

/// An item that matched: its index in the list, its score, and the character positions of the title
/// the query's letters matched (empty for an empty query), for highlighting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranked {
    pub index: usize,
    pub score: i32,
    pub matched: Vec<usize>,
}

const WORD_START: i32 = 8;
const TITLE_START: i32 = 4;
const RUN: i32 = 5;
const GAP_PENALTY_CAP: i32 = 3;
const LEAD_PENALTY_CAP: i32 = 5;

fn starts_word(chars: &[char], at: usize) -> bool {
    at == 0 || {
        let prev = chars[at - 1];
        let here = chars[at];
        !prev.is_alphanumeric() || (prev.is_lowercase() && here.is_uppercase())
    }
}

/// The best score and matched positions of `query` (lowercase, no blanks) in `title`, or `None`.
fn best_match(query: &[char], title: &[char]) -> Option<(i32, Vec<usize>)> {
    let (m, n) = (query.len(), title.len());
    if m == 0 || m > n {
        return None;
    }
    let lower: Vec<char> = title.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
    // dp[i][j]: best score with query[i] matched at title[j]; from[i][j]: the position of query[i - 1].
    let mut dp = vec![vec![i32::MIN; n]; m];
    let mut from = vec![vec![usize::MAX; n]; m];
    for j in 0..n {
        if lower[j] == query[0] {
            let mut s = 1;
            if starts_word(title, j) {
                s += WORD_START;
            }
            if j == 0 {
                s += TITLE_START;
            }
            dp[0][j] = s - (j as i32).min(LEAD_PENALTY_CAP);
        }
    }
    for i in 1..m {
        for j in i..n {
            if lower[j] != query[i] {
                continue;
            }
            let mut best = i32::MIN;
            let mut best_k = usize::MAX;
            for k in (i - 1)..j {
                if dp[i - 1][k] == i32::MIN {
                    continue;
                }
                let gap = (j - k - 1) as i32;
                let link = if gap == 0 { RUN } else { -gap.min(GAP_PENALTY_CAP) };
                let s = dp[i - 1][k] + link;
                if s > best {
                    best = s;
                    best_k = k;
                }
            }
            if best != i32::MIN {
                let mut s = best + 1;
                if starts_word(title, j) {
                    s += WORD_START;
                }
                dp[i][j] = s;
                from[i][j] = best_k;
            }
        }
    }
    let (mut j, mut score) = (usize::MAX, i32::MIN);
    for (jj, &s) in dp[m - 1].iter().enumerate() {
        if s > score {
            score = s;
            j = jj;
        }
    }
    if score == i32::MIN {
        return None;
    }
    let mut matched = vec![0; m];
    for i in (0..m).rev() {
        matched[i] = j;
        j = from[i][j];
    }
    Some((score, matched))
}

/// The items whose title holds the query, best first. An empty or blank query lists every item in order.
pub fn rank(query: &str, items: &[Item]) -> Vec<Ranked> {
    let query: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).flat_map(|c| c.to_lowercase()).collect();
    if query.is_empty() {
        let mut all: Vec<Ranked> = (0..items.len()).map(|index| Ranked { index, score: 0, matched: Vec::new() }).collect();
        all.sort_by_key(|r| !items[r.index].enabled);
        return all;
    }
    let mut ranked: Vec<Ranked> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let chars: Vec<char> = item.title.chars().collect();
            best_match(&query, &chars).map(|(score, matched)| Ranked { index, score, matched })
        })
        .collect();
    // Equal scores: the shorter title first (it is the closer match), then the list's order.
    ranked.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(items[b.index].enabled.cmp(&items[a.index].enabled))
            .then(items[a.index].title.chars().count().cmp(&items[b.index].title.chars().count()))
            .then(a.index.cmp(&b.index))
    });
    ranked
}

/// The editor's own actions, with the shortcut of `keymap` on `os`. Neither the palette nor find is a
/// row (find is the quick open the file modes do); the actions the editor cannot do yet are marked.
pub fn action_items(keymap: Keymap, os: Os) -> Vec<Item> {
    // Find is the quick open the palette's own file and everything modes do, so it is not a row.
    Action::ALL
        .into_iter()
        .filter(|a| !matches!(a, Action::Palette | Action::Find))
        .map(|a| Item {
            payload: Payload::Action(a),
            title: a.label().to_string(),
            hint: keymap.hint(a, os),
            enabled: super::editor::is_implemented(a),
        })
        .collect()
}

/// Palette items for extension commands.
pub fn extension_command_items(commands: &[ContributedCommand]) -> Vec<Item> {
    commands
        .iter()
        .map(|cmd| Item {
            payload: Payload::ExtensionCommand(cmd.id.clone()),
            title: cmd.display_title(),
            hint: String::new(),
            enabled: true,
        })
        .collect()
}

/// A title cut into runs for drawing: `(text, matched)`, where a matched run is bold. `matched` holds
/// character positions, as [`Ranked`] reports them.
pub fn highlight_segments(title: &str, matched: &[usize]) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    for (i, c) in title.chars().enumerate() {
        let hit = matched.contains(&i);
        match out.last_mut() {
            Some((text, last)) if *last == hit => text.push(c),
            _ => out.push((c.to_string(), hit)),
        }
    }
    out
}

/// Where the highlighted row goes after an arrow key: it stops at both ends. `len` is the row count.
pub fn move_selection(selected: usize, len: usize, down: bool) -> usize {
    if len == 0 {
        0
    } else if down {
        (selected + 1).min(len - 1)
    } else {
        selected.saturating_sub(1)
    }
}

/// What the palette lists: commands, files, both, go-to-line, or recent files; or one of the status
/// bar's pickers (FR-L31.7, rows from `editor_status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Commands,
    Files,
    Everything,
    GotoLine,
    Recent,
    /// LF or CRLF.
    Eol,
    /// 인코딩으로 다시 열기 or 인코딩으로 저장.
    EncodingAction,
    /// The encodings, for reopening or saving.
    Encoding(ember_editor::EncodingMode),
    /// Discard unsaved edits to reopen in this encoding?
    ConfirmReopen(ember_editor::Encoding),
    /// The language modes.
    Language,
}

/// Does the mode list the file index (so the index notes apply)?
pub fn lists_files(mode: Mode) -> bool {
    matches!(mode, Mode::Files | Mode::Everything | Mode::Recent)
}

/// The mode an action opens the palette in. `Find` follows the keymap's own meaning of it.
pub fn mode_for(action: Action, keymap: Keymap) -> Option<Mode> {
    match (action, keymap) {
        (Action::Palette, _) => Some(Mode::Commands),
        (Action::Find, Keymap::VsCode) => Some(Mode::Files),
        (Action::Find, Keymap::JetBrains) => Some(Mode::Everything),
        (Action::Recent, _) => Some(Mode::Recent),
        (Action::GotoLine, _) => Some(Mode::GotoLine),
        _ => None,
    }
}

/// The hint the empty field shows: the keymap's own name for what opened it.
pub fn placeholder(mode: Mode, keymap: Keymap) -> &'static str {
    match mode {
        Mode::Commands => keymap.palette_label(),
        Mode::Files | Mode::Everything => keymap.find_label(),
        Mode::GotoLine => "이동할 줄 번호 입력",
        Mode::Recent => "최근 파일",
        Mode::Eol => "줄 끝 시퀀스 선택",
        Mode::EncodingAction => "작업 선택",
        Mode::Encoding(ember_editor::EncodingMode::Reopen) => "다시 열 때 사용할 파일 인코딩 선택",
        Mode::Encoding(ember_editor::EncodingMode::Save) => "저장할 때 사용할 파일 인코딩 선택",
        Mode::ConfirmReopen(_) => "저장하지 않은 변경 사항을 버리고 다른 인코딩으로 다시 여시겠습니까?",
        Mode::Language => "언어 모드 선택",
    }
}

/// A directory the file index does not enter: version control internals, dependencies and build
/// output. `target` is build output only beside a `Cargo.toml` or a `pom.xml`; in any other project it
/// is an ordinary directory. `siblings` are the entries of the directory that holds `name`.
pub fn skipped_directory(name: &str, siblings: &[(String, u32)]) -> bool {
    match name {
        ".git" | ".hg" | ".svn" | "node_modules" | "__pycache__" => true,
        "target" => siblings.iter().any(|(n, _)| n == "Cargo.toml" || n == "pom.xml"),
        _ => false,
    }
}

/// The files of the index as palette items: the path below the project folder, sorted, the file's
/// full path as the payload. A path outside the folder keeps its full path.
pub fn file_items(folder: &str, paths: &[String]) -> Vec<Item> {
    let root = folder.trim_end_matches('/');
    let mut items: Vec<Item> = paths
        .iter()
        .map(|path| {
            let title = path.strip_prefix(root).and_then(|rest| rest.strip_prefix('/')).unwrap_or(path).to_string();
            Item { payload: Payload::File(path.clone()), title, hint: String::new(), enabled: true }
        })
        .collect();
    items.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    items
}

/// Record a path in a recent files list in MRU order (most recent first, no duplicates, capped at 50).
pub fn record_recent_path(recent: &mut Vec<String>, path: &str) {
    let path = path.trim();
    if path.is_empty() {
        return;
    }
    recent.retain(|p| p != path);
    recent.insert(0, path.to_string());
    if recent.len() > 50 {
        recent.truncate(50);
    }
}

/// Order `files` so that recently accessed files come first in MRU order.
pub fn prioritize_recent_files(folder: &str, files: &[Item], recent_paths: &[String]) -> Vec<Item> {
    if recent_paths.is_empty() {
        return files.to_vec();
    }
    let mut recent_items = Vec::new();
    let mut matched_recent = std::collections::HashSet::new();
    for r_path in recent_paths {
        if let Some(item) = files.iter().find(|i| match &i.payload {
            Payload::File(p) => p == r_path,
            _ => false,
        }) {
            recent_items.push(item.clone());
            matched_recent.insert(r_path.clone());
        } else {
            let root = folder.trim_end_matches('/');
            let title = r_path.strip_prefix(root).and_then(|rest| rest.strip_prefix('/')).unwrap_or(r_path).to_string();
            recent_items.push(Item {
                payload: Payload::File(r_path.clone()),
                title,
                hint: String::new(),
                enabled: true,
            });
            matched_recent.insert(r_path.clone());
        }
    }
    let mut result = recent_items;
    for item in files {
        if let Payload::File(path) = &item.payload {
            if matched_recent.contains(path) {
                continue;
            }
        }
        result.push(item.clone());
    }
    result
}

/// The items for a mode, including extension commands and prioritized recent files.
pub fn items_for_workbench(
    mode: Mode,
    keymap: Keymap,
    os: Os,
    folder: &str,
    files: &[Item],
    extension_commands: &[ContributedCommand],
    recent_paths: &[String],
) -> Vec<Item> {
    match mode {
        Mode::Commands => {
            let mut items = action_items(keymap, os);
            items.extend(extension_command_items(extension_commands));
            items
        }
        Mode::Files | Mode::Recent => prioritize_recent_files(folder, files, recent_paths),
        Mode::Everything => {
            let mut items = action_items(keymap, os);
            items.extend(extension_command_items(extension_commands));
            items.extend(prioritize_recent_files(folder, files, recent_paths));
            items
        }
        // The status bar's pickers get their rows from `editor_status::picker_items`.
        Mode::GotoLine | Mode::Eol | Mode::EncodingAction | Mode::Encoding(_) | Mode::ConfirmReopen(_) | Mode::Language => Vec::new(),
    }
}

/// The items for a mode: commands, files, or commands then files. `files` are the indexed files as
/// [`file_items`] makes them.
pub fn items_for(mode: Mode, keymap: Keymap, os: Os, files: &[Item]) -> Vec<Item> {
    items_for_workbench(mode, keymap, os, "", files, &[], &[])
}

/// Resolve the effective palette mode and the query string after stripping prefix markers.
/// `>` in file/everything/recent modes switches to `Mode::Commands`.
/// `:` in file/everything/commands/recent modes switches to `Mode::GotoLine`.
pub fn effective_mode(mode: Mode, query: &str) -> (Mode, String) {
    let trimmed = query.trim_start();
    if mode == Mode::GotoLine {
        let q = if let Some(rest) = trimmed.strip_prefix(':') { rest.trim() } else { trimmed };
        (Mode::GotoLine, q.to_string())
    } else if matches!(mode, Mode::Files | Mode::Recent | Mode::Everything) && trimmed.starts_with('>') {
        (Mode::Commands, trimmed[1..].trim_start().to_string())
    } else if matches!(mode, Mode::Files | Mode::Commands | Mode::Recent | Mode::Everything) && trimmed.starts_with(':') {
        (Mode::GotoLine, trimmed[1..].trim().to_string())
    } else {
        (mode, query.to_string())
    }
}

/// Parse line and optional column from go-to-line query text (`42` or `42:10`).
pub fn parse_goto_line(text: &str) -> Option<(usize, Option<usize>)> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Some((l_str, c_str)) = text.split_once(':') {
        let line = l_str.trim().parse::<usize>().ok()?;
        if line == 0 {
            return None;
        }
        let c_str = c_str.trim();
        let col = if c_str.is_empty() {
            None
        } else {
            let c = c_str.parse::<usize>().ok()?;
            if c == 0 {
                return None;
            }
            Some(c)
        };
        Some((line, col))
    } else {
        let line = text.parse::<usize>().ok()?;
        if line == 0 {
            return None;
        }
        Some((line, None))
    }
}

/// Create a row item for GotoLine mode.
pub fn goto_line_item(query: &str) -> Item {
    match parse_goto_line(query) {
        Some((line, None)) => Item {
            payload: Payload::GotoLine { line, column: None },
            title: format!("{line}번 줄로 이동"),
            hint: String::new(),
            enabled: true,
        },
        Some((line, Some(col))) => Item {
            payload: Payload::GotoLine { line, column: Some(col) },
            title: format!("{line}번 줄, {col}번째 칸으로 이동"),
            hint: String::new(),
            enabled: true,
        },
        None => {
            let title = if query.trim().is_empty() {
                "이동할 줄 번호를 입력하세요".to_string()
            } else {
                "올바른 줄 번호를 입력하세요".to_string()
            };
            Item {
                payload: Payload::GotoLine { line: 1, column: None },
                title,
                hint: String::new(),
                enabled: false,
            }
        }
    }
}

/// Parse a `:line` or `:line:col` suffix from the end of a file search query.
/// Returns `(path_query, Option<(line, Option<col>)>)`.
pub fn parse_path_line_suffix(query: &str) -> (&str, Option<(usize, Option<usize>)>) {
    let query_trimmed = query.trim();
    if query_trimmed.starts_with(':') {
        return (query, None);
    }
    if let Some((path_part, suffix)) = query_trimmed.rsplit_once(':') {
        if path_part.is_empty() {
            return (query, None);
        }
        if let Some((path_inner, line_part)) = path_part.rsplit_once(':') {
            if let (Ok(line), Ok(col)) = (line_part.trim().parse::<usize>(), suffix.trim().parse::<usize>()) {
                if line > 0 && col > 0 && !path_inner.trim().is_empty() {
                    return (path_inner.trim(), Some((line, Some(col))));
                }
            }
        }
        if let Ok(line) = suffix.trim().parse::<usize>() {
            if line > 0 {
                return (path_part.trim(), Some((line, None)));
            }
        }
    }
    (query, None)
}

/// Attach line and column information to a `Payload::File` when chosen.
pub fn apply_line_suffix(payload: Payload, suffix: Option<(usize, Option<usize>)>) -> Payload {
    match (payload, suffix) {
        (Payload::File(path), Some((line, col))) => Payload::FileAt { path, line, column: col },
        (p, _) => p,
    }
}

/// The accessible name of the search field.
pub fn search_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Commands => "명령 검색",
        Mode::Files => "파일 검색",
        Mode::Everything => "파일과 명령 검색",
        Mode::GotoLine => "줄 번호 검색",
        Mode::Recent => "최근 파일 검색",
        Mode::Eol => "줄 끝 시퀀스 검색",
        Mode::EncodingAction => "인코딩 작업 검색",
        Mode::Encoding(_) | Mode::ConfirmReopen(_) => "인코딩 검색",
        Mode::Language => "언어 모드 검색",
    }
}

/// What the list says when nothing is listed: the index is still being built, or nothing matches.
pub fn empty_text(mode: Mode, index_running: bool) -> &'static str {
    if index_running && matches!(mode, Mode::Files | Mode::Everything | Mode::Recent) {
        "파일을 찾는 중입니다…"
    } else {
        "일치하는 항목이 없습니다."
    }
}
