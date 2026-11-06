//! The command palette's model (D38, #447): fuzzy ranking of commands and files. Pure; the sheet
//! that shows it is in `editor_workbench`.
//!
//! A title matches when it holds the query's letters in order (case ignored, blanks in the query
//! ignored). The best alignment is chosen by score: a letter that starts a word or the title, a run of
//! letters in a row, and a late start all count. Equal scores: the shorter title, then the list's order.

use super::keymap::{Action, Keymap, Os};

/// What choosing an item does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    /// One of the editor's own actions.
    Action(Action),
    /// A file, by its path.
    File(String),
}

/// One line of the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub payload: Payload,
    pub title: String,
    /// The shortcut, or empty.
    pub hint: String,
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
        return (0..items.len()).map(|index| Ranked { index, score: 0, matched: Vec::new() }).collect();
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
            .then(items[a.index].title.chars().count().cmp(&items[b.index].title.chars().count()))
            .then(a.index.cmp(&b.index))
    });
    ranked
}

/// The editor's own actions, with the shortcut of `keymap` on `os`. The palette does not list itself.
pub fn action_items(keymap: Keymap, os: Os) -> Vec<Item> {
    Action::ALL
        .into_iter()
        .filter(|a| *a != Action::Palette)
        .map(|a| Item {
            payload: Payload::Action(a),
            title: if a == Action::Find { keymap.find_label().to_string() } else { a.label().to_string() },
            hint: keymap.hint(a, os),
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

/// What the palette lists: commands (the palette action), files (find in a keymap that means "go to
/// file"), or both (find in a keymap that means "search everywhere").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Commands,
    Files,
    Everything,
}

/// The mode an action opens the palette in. `Find` follows the keymap's own meaning of it.
pub fn mode_for(action: Action, keymap: Keymap) -> Option<Mode> {
    match (action, keymap) {
        (Action::Palette, _) => Some(Mode::Commands),
        (Action::Find, Keymap::VsCode) => Some(Mode::Files),
        (Action::Find, Keymap::JetBrains) => Some(Mode::Everything),
        _ => None,
    }
}

/// The hint the empty field shows.
pub fn placeholder(mode: Mode) -> &'static str {
    match mode {
        Mode::Commands => "명령 검색",
        Mode::Files => "파일 이름으로 열기",
        Mode::Everything => "파일, 명령 검색",
    }
}

/// A directory the file index does not enter: version control internals, dependencies and build output.
pub fn skipped_directory(name: &str) -> bool {
    matches!(name, ".git" | ".hg" | ".svn" | "node_modules" | "target" | "__pycache__" | ".DS_Store")
}

/// The files of the index as palette items: the path below the project folder, sorted, the file's
/// full path as the payload. A path outside the folder keeps its full path.
pub fn file_items(folder: &str, paths: &[String]) -> Vec<Item> {
    let root = folder.trim_end_matches('/');
    let mut items: Vec<Item> = paths
        .iter()
        .map(|path| {
            let title = path.strip_prefix(root).and_then(|rest| rest.strip_prefix('/')).unwrap_or(path).to_string();
            Item { payload: Payload::File(path.clone()), title, hint: String::new() }
        })
        .collect();
    items.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    items
}

/// The items for a mode: commands, files, or commands then files.
pub fn items_for(mode: Mode, keymap: Keymap, os: Os, folder: &str, files: &[String]) -> Vec<Item> {
    match mode {
        Mode::Commands => action_items(keymap, os),
        Mode::Files => file_items(folder, files),
        Mode::Everything => {
            let mut items = action_items(keymap, os);
            items.extend(file_items(folder, files));
            items
        }
    }
}
