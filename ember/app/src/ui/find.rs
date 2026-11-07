//! Find and replace in the open file (D38, #440, #628): the pure model. The matching is a literal
//! search with case, whole-word and regex options over the lines of the document; columns are UTF-16
//! code units, the widget's own. The find bar in `editor_workbench` shows the result and moves the
//! reader with the editor handle.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FindOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
}

impl FindOptions {
    pub const fn new(case_sensitive: bool, whole_word: bool) -> Self {
        Self {
            case_sensitive,
            whole_word,
            regex: false,
        }
    }
}

/// One match: a line and the UTF-16 columns it covers on it (a match never spans lines).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub line: u32,
    pub start: u32,
    pub end: u32,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn fold(c: char, case_sensitive: bool) -> char {
    if case_sensitive {
        c
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

/// The most matches kept in the decoration list. The find bar searches on render, so a query that
/// matches a huge file everywhere stops here instead of building an unbounded list; replace all
/// refuses past it.
pub const MAX_MATCHES: usize = 10_000;
pub const MAX_FIND_MATCHES: usize = MAX_MATCHES;

/// Matches of `needle` on a single line with line number `line_no`.
pub fn matches_in_line(line: &str, line_no: u32, needle: &[char], options: FindOptions) -> Vec<Match> {
    if needle.is_empty() {
        return Vec::new();
    }
    let line = line.strip_suffix('\r').unwrap_or(line);
    let chars: Vec<char> = line.chars().collect();
    if chars.len() < needle.len() {
        return Vec::new();
    }
    let mut cols: Vec<u32> = Vec::with_capacity(chars.len() + 1);
    let mut col = 0u32;
    for c in &chars {
        cols.push(col);
        col += c.len_utf16() as u32;
    }
    cols.push(col);
    let folded: Vec<char> = chars.iter().map(|c| fold(*c, options.case_sensitive)).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + needle.len() <= folded.len() {
        if folded[i..i + needle.len()] == needle[..] {
            let end = i + needle.len();
            let before_ok = i == 0 || !is_word(chars[i - 1]) || !is_word(chars[i]);
            let after_ok = end == chars.len() || !is_word(chars[end]) || !is_word(chars[end - 1]);
            if !options.whole_word || (before_ok && after_ok) {
                out.push(Match {
                    line: line_no,
                    start: cols[i],
                    end: cols[end],
                });
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Every non-overlapping occurrence of `query` in `text`, in document order, at most [`MAX_MATCHES`].
/// An empty query, or one with a line break, matches nothing.
pub fn find_matches(text: &str, query: &str, options: FindOptions) -> Vec<Match> {
    find_matches_capped(text, query, options).0
}

/// [`find_matches`] and whether more matches exist than were kept.
pub fn find_matches_capped(text: &str, query: &str, options: FindOptions) -> (Vec<Match>, bool) {
    if query.is_empty() || query.contains('\n') || query.contains('\r') {
        return (Vec::new(), false);
    }
    let needle: Vec<char> = query.chars().map(|c| fold(c, options.case_sensitive)).collect();
    let mut out = Vec::new();
    for (line_no, line) in text.split('\n').enumerate() {
        let line_matches = matches_in_line(line, line_no as u32, &needle, options);
        for m in line_matches {
            if out.len() == MAX_MATCHES {
                return (out, true);
            }
            out.push(m);
        }
    }
    (out, false)
}

/// Key for caching find matches by document ID, generation, version, query, and options.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DocCacheKey {
    pub doc_id: String,
    pub generation: u64,
    pub version: u32,
    pub query: String,
    pub options: FindOptions,
}

/// [`find_matches_capped`] computed once per document version, query, and options.
/// When the cache hits, `get_text` is not evaluated, avoiding any cloning or hashing of the document text.
pub fn cached_doc_matches<'a>(
    doc_id: &str,
    generation: u64,
    version: u32,
    query: &str,
    options: FindOptions,
    get_text: impl FnOnce() -> &'a str,
) -> (Vec<Match>, bool) {
    use std::cell::RefCell;
    type Entry = (DocCacheKey, (Vec<Match>, bool));
    thread_local! { static DOC_LAST: RefCell<Option<Entry>> = const { RefCell::new(None) }; }
    let key = DocCacheKey {
        doc_id: doc_id.to_string(),
        generation,
        version,
        query: query.to_string(),
        options,
    };
    DOC_LAST.with(|last| {
        if let Some((k, v)) = last.borrow().as_ref() {
            if *k == key {
                return v.clone();
            }
        }
        let text = get_text();
        let found = find_matches_capped(text, query, options);
        *last.borrow_mut() = Some((key, found.clone()));
        found
    })
}

/// [`find_matches_capped`] computed once per text, query and options.
pub fn cached_matches(text: &str, query: &str, options: FindOptions) -> (Vec<Match>, bool) {
    use std::cell::RefCell;
    use std::hash::{Hash, Hasher};
    type Key = (u64, usize, String, FindOptions);
    type Entry = (Key, (Vec<Match>, bool));
    thread_local! { static LAST: RefCell<Option<Entry>> = const { RefCell::new(None) }; }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    let key: Key = (hasher.finish(), text.len(), query.to_string(), options);
    LAST.with(|last| {
        if let Some((k, v)) = last.borrow().as_ref() {
            if *k == key {
                return v.clone();
            }
        }
        let found = find_matches_capped(text, query, options);
        *last.borrow_mut() = Some((key, found.clone()));
        found
    })
}

/// Finds the next occurrence of `query` in `text` starting from `from` (line, column).
/// If `forward` is true, searches forward (at or after `from`), wrapping to line 0 if needed.
/// If `forward` is false, searches backward (at or before `from`), wrapping to the end of text if needed.
pub fn scan_match(
    text: &str,
    query: &str,
    options: FindOptions,
    from: (u32, u32),
    forward: bool,
) -> Option<Match> {
    if query.is_empty() || query.contains('\n') || query.contains('\r') {
        return None;
    }
    let needle: Vec<char> = query.chars().map(|c| fold(c, options.case_sensitive)).collect();
    let lines: Vec<&str> = text.split('\n').collect();
    if lines.is_empty() {
        return None;
    }
    let total_lines = lines.len() as u32;
    let (from_line, from_col) = from;

    if forward {
        if from_line < total_lines {
            let m_line = matches_in_line(lines[from_line as usize], from_line, &needle, options);
            if let Some(m) = m_line.into_iter().find(|m| m.start >= from_col) {
                return Some(m);
            }
            for l in (from_line + 1)..total_lines {
                let m_line = matches_in_line(lines[l as usize], l, &needle, options);
                if let Some(m) = m_line.into_iter().next() {
                    return Some(m);
                }
            }
        }
        let wrap_limit = from_line.min(total_lines.saturating_sub(1));
        for l in 0..wrap_limit {
            let m_line = matches_in_line(lines[l as usize], l, &needle, options);
            if let Some(m) = m_line.into_iter().next() {
                return Some(m);
            }
        }
        if from_line < total_lines {
            let m_line = matches_in_line(lines[from_line as usize], from_line, &needle, options);
            if let Some(m) = m_line.into_iter().find(|m| m.start < from_col) {
                return Some(m);
            }
        }
    } else {
        if from_line < total_lines {
            let m_line = matches_in_line(lines[from_line as usize], from_line, &needle, options);
            if let Some(m) = m_line.into_iter().rfind(|m| m.end <= from_col) {
                return Some(m);
            }
            for l in (0..from_line).rev() {
                let m_line = matches_in_line(lines[l as usize], l, &needle, options);
                if let Some(m) = m_line.into_iter().next_back() {
                    return Some(m);
                }
            }
        }
        let start_line = total_lines.saturating_sub(1);
        for l in (from_line + 1..=start_line).rev() {
            let m_line = matches_in_line(lines[l as usize], l, &needle, options);
            if let Some(m) = m_line.into_iter().next_back() {
                return Some(m);
            }
        }
        if from_line < total_lines {
            let m_line = matches_in_line(lines[from_line as usize], from_line, &needle, options);
            if let Some(m) = m_line.into_iter().rfind(|m| m.end > from_col) {
                return Some(m);
            }
        }
    }
    None
}

/// The match to go to from `from` (line, column).
/// If `truncated` is false, navigation uses `matches` directly.
/// If `truncated` is true (more than [`MAX_MATCHES`] exist), navigation uses `matches`
/// where available and scans `text` when moving past or across the capped matches.
pub fn find_nav_match(
    text: &str,
    query: &str,
    options: FindOptions,
    matches: &[Match],
    truncated: bool,
    from: (u32, u32),
    forward: bool,
) -> Option<Match> {
    if !truncated {
        next_match(matches, from, forward).and_then(|i| matches.get(i).copied())
    } else {
        if forward {
            if let Some(last) = matches.last() {
                if from <= (last.line, last.start) {
                    if let Some(i) = matches.iter().position(|m| (m.line, m.start) >= from) {
                        return Some(matches[i]);
                    }
                }
            }
        } else if let (Some(first), Some(last)) = (matches.first(), matches.last()) {
            if from >= (first.line, first.end) && from <= (last.line, last.end) {
                if let Some(i) = matches.iter().rposition(|m| (m.line, m.end) <= from) {
                    return Some(matches[i]);
                }
            }
        }
        scan_match(text, query, options, from, forward)
    }
}

/// The edits for a replacement: the one match at `only`, or every match. They are last first, so each
/// edit is written against the same document version and none moves another.
pub fn replace_edits(
    matches: &[Match],
    only: Option<usize>,
    replacement: &str,
) -> Vec<(Match, String)> {
    match only {
        Some(i) => matches
            .get(i)
            .map(|m| vec![(*m, replacement.to_string())])
            .unwrap_or_default(),
        None => matches
            .iter()
            .rev()
            .map(|m| (*m, replacement.to_string()))
            .collect(),
    }
}

/// The match to go to from the caret at `from` (line, column): the first one that starts at or after
/// it going forward, the last one that ends at or before it going backward; past the end it wraps.
pub fn next_match(matches: &[Match], from: (u32, u32), forward: bool) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    if forward {
        matches.iter().position(|m| (m.line, m.start) >= from).or(Some(0))
    } else {
        matches.iter().rposition(|m| (m.line, m.end) <= from).or(Some(matches.len() - 1))
    }
}

/// The counter beside the field: `3/12` with a current match, `12개` without, `결과 없음` when a query
/// found nothing, and nothing before a query is typed.
pub fn count_text(current: Option<usize>, total: usize, query: &str) -> String {
    if query.is_empty() {
        return String::new();
    }
    match (current, total) {
        (_, 0) => "결과 없음".to_string(),
        (Some(i), n) => format!("{}/{n}", i + 1),
        (None, n) => format!("{n}개"),
    }
}

/// Pure model state for the find bar, tracking visibility, query, options, current match, and focus requests.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FindBarState {
    pub open: bool,
    pub query: String,
    pub replacement: String,
    pub options: FindOptions,
    pub current: Option<usize>,
    pub focus_seq: u64,
}

impl FindBarState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open the find bar, or if already open, request refocusing on the query input by incrementing `focus_seq`.
    pub fn open_find_bar(&mut self) {
        self.open = true;
        self.focus_seq = self.focus_seq.wrapping_add(1);
    }

    pub fn close(&mut self) {
        self.open = false;
    }
}
