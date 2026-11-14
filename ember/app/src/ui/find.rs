//! Find and replace in the open file (D38, #440, #628, FR-L31.12): the pure model. The matching is a
//! literal or regular-expression search with case and whole-word options over the lines of the document,
//! optionally limited to a scope (찾기 범위: 선택 영역); columns are UTF-16 code units, the widget's own.
//! The regex engine is the one search across files uses (`ember_editor::search::Matcher`, Oniguruma), so
//! a pattern means the same in both. A match never spans lines (Code-OSS can match `\n` in a regex; the
//! bar here cannot yet). The find bar in `editor_workbench` shows the result and moves the reader with
//! the editor handle.

use ember_editor::search::{expand_replacement, utf16_col, Matcher, SearchOptions};

/// What 선택 영역에서 찾기 limits the search to: from `start` to `end`, each (line, UTF-16 column).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Scope {
    pub start: (u32, u32),
    pub end: (u32, u32),
}

impl Scope {
    /// The scope for a selection, as Code-OSS takes it when 선택 영역에서 찾기 is turned on: the two ends in
    /// order, and a selection over several lines that ends at the start of a line ends at the end of the
    /// line before. An empty selection gives no scope, so the whole document is searched.
    pub fn from_selection(anchor: (u32, u32), active: (u32, u32), text: &str) -> Option<Scope> {
        let (start, mut end) = if anchor <= active { (anchor, active) } else { (active, anchor) };
        if start == end {
            return None;
        }
        if end.1 == 0 && end.0 > start.0 {
            let prev = end.0 - 1;
            let line = text.split('\n').nth(prev as usize).unwrap_or("");
            let line = line.strip_suffix('\r').unwrap_or(line);
            end = (prev, line.encode_utf16().count() as u32);
        }
        Some(Scope { start, end })
    }

    /// Whether the match lies wholly inside the scope.
    pub fn contains(&self, m: &Match) -> bool {
        (m.line, m.start) >= self.start && (m.line, m.end) <= self.end
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FindOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    /// 대소문자 보존: a replacement takes the case of the text it replaces (Code-OSS `preserveCase`).
    pub preserve_case: bool,
    /// 선택 영역에서 찾기: only matches inside this scope count.
    pub scope: Option<Scope>,
}

impl FindOptions {
    pub const fn new(case_sensitive: bool, whole_word: bool) -> Self {
        Self {
            case_sensitive,
            whole_word,
            regex: false,
            preserve_case: false,
            scope: None,
        }
    }
}

/// How one query finds its matches on a line: a literal needle (folded for case) or a compiled regex.
enum LineFinder {
    Literal(Vec<char>),
    Regex(Matcher),
}

impl LineFinder {
    /// `None` for an empty query, a literal query with a line break, or a regex that does not compile.
    fn new(query: &str, options: FindOptions) -> Option<Self> {
        if query.is_empty() {
            return None;
        }
        if options.regex {
            return Matcher::new(&search_options(query, options)).ok().map(LineFinder::Regex);
        }
        if query.contains('\n') || query.contains('\r') {
            return None;
        }
        Some(LineFinder::Literal(query.chars().map(|c| fold(c, options.case_sensitive)).collect()))
    }

    fn line(&self, line: &str, line_no: u32, options: FindOptions) -> Vec<Match> {
        let mut found = match self {
            LineFinder::Literal(needle) => matches_in_line(line, line_no, needle, options),
            LineFinder::Regex(matcher) => {
                let line = line.strip_suffix('\r').unwrap_or(line);
                matcher
                    .find(line)
                    .into_iter()
                    .map(|(s, e)| Match { line: line_no, start: utf16_col(line, s), end: utf16_col(line, e) })
                    .collect()
            }
        };
        if let Some(scope) = options.scope {
            found.retain(|m| scope.contains(m));
        }
        found
    }
}

fn search_options(query: &str, options: FindOptions) -> SearchOptions {
    SearchOptions {
        pattern: query.to_string(),
        regex: options.regex,
        case_sensitive: options.case_sensitive,
        whole_word: options.whole_word,
    }
}

/// The engine's message when 정규식 is on and `query` does not compile, else `None`.
pub fn regex_error(query: &str, options: FindOptions) -> Option<String> {
    if !options.regex || query.is_empty() {
        return None;
    }
    Matcher::new(&search_options(query, options)).err()
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
    let Some(finder) = LineFinder::new(query, options) else {
        return (Vec::new(), false);
    };
    let mut out = Vec::new();
    for (line_no, line) in text.split('\n').enumerate() {
        let line_matches = finder.line(line, line_no as u32, options);
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
    let finder = LineFinder::new(query, options)?;
    let lines: Vec<&str> = text.split('\n').collect();
    if lines.is_empty() {
        return None;
    }
    let total_lines = lines.len() as u32;
    let (from_line, from_col) = from;

    if forward {
        if from_line < total_lines {
            let m_line = finder.line(lines[from_line as usize], from_line, options);
            if let Some(m) = m_line.into_iter().find(|m| m.start >= from_col) {
                return Some(m);
            }
            for l in (from_line + 1)..total_lines {
                let m_line = finder.line(lines[l as usize], l, options);
                if let Some(m) = m_line.into_iter().next() {
                    return Some(m);
                }
            }
        }
        let wrap_limit = from_line.min(total_lines.saturating_sub(1));
        for l in 0..wrap_limit {
            let m_line = finder.line(lines[l as usize], l, options);
            if let Some(m) = m_line.into_iter().next() {
                return Some(m);
            }
        }
        if from_line < total_lines {
            let m_line = finder.line(lines[from_line as usize], from_line, options);
            if let Some(m) = m_line.into_iter().find(|m| m.start < from_col) {
                return Some(m);
            }
        }
    } else {
        if from_line < total_lines {
            let m_line = finder.line(lines[from_line as usize], from_line, options);
            if let Some(m) = m_line.into_iter().rfind(|m| m.end <= from_col) {
                return Some(m);
            }
            for l in (0..from_line).rev() {
                let m_line = finder.line(lines[l as usize], l, options);
                if let Some(m) = m_line.into_iter().next_back() {
                    return Some(m);
                }
            }
        }
        let start_line = total_lines.saturating_sub(1);
        for l in (from_line + 1..=start_line).rev() {
            let m_line = finder.line(lines[l as usize], l, options);
            if let Some(m) = m_line.into_iter().next_back() {
                return Some(m);
            }
        }
        if from_line < total_lines {
            let m_line = finder.line(lines[from_line as usize], from_line, options);
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

/// The case of `matched` carried over to `replacement`, as Code-OSS's `buildReplaceStringWithCasePreserved`
/// does: segments split by `-` or `_` (when both have the same number of segments) each keep their case;
/// otherwise all upper, all lower, a capital first letter or a small first letter.
pub fn preserve_case(matched: &str, replacement: &str) -> String {
    if matched.is_empty() {
        return replacement.to_string();
    }
    let same_split = |ch: char| {
        matched.contains(ch) && replacement.contains(ch) && matched.split(ch).count() == replacement.split(ch).count()
    };
    let (hyphens, underscores) = (same_split('-'), same_split('_'));
    if hyphens != underscores {
        let ch = if hyphens { '-' } else { '_' };
        let parts: Vec<String> = matched
            .split(ch)
            .zip(replacement.split(ch))
            .map(|(m, r)| preserve_case(m, r))
            .collect();
        return parts.join(&ch.to_string());
    }
    let first_as = |upper: bool| -> String {
        let mut rest = replacement.chars();
        match rest.next() {
            Some(c) if upper => c.to_uppercase().collect::<String>() + rest.as_str(),
            Some(c) => c.to_lowercase().collect::<String>() + rest.as_str(),
            None => String::new(),
        }
    };
    let m0 = matched.chars().next().unwrap_or_default().to_string();
    if matched.to_uppercase() == matched {
        replacement.to_uppercase()
    } else if matched.to_lowercase() == matched {
        replacement.to_lowercase()
    } else if m0.to_lowercase() != m0 {
        first_as(true)
    } else if m0.to_uppercase() != m0 {
        first_as(false)
    } else {
        replacement.to_string()
    }
}

/// The text between UTF-16 columns `start` and `end` of `line`.
fn utf16_slice(line: &str, start: u32, end: u32) -> &str {
    let byte = |col: u32| {
        let mut units = 0u32;
        for (b, c) in line.char_indices() {
            if units >= col {
                return b;
            }
            units += c.len_utf16() as u32;
        }
        line.len()
    };
    &line[byte(start)..byte(end)]
}

/// The edits for a replacement as Code-OSS makes them: the one match at `only`, or every match, last
/// first. With 정규식 each replacement expands `$&`, `$0`, `$1` to `$99`, `$$`, `\n`, `\t` and `\\`
/// against that match's groups (the rules of search across files); with 대소문자 보존 it then takes the
/// case of the text it replaces. A match the text no longer has is left out.
pub fn replacement_edits(
    text: &str,
    query: &str,
    options: FindOptions,
    matches: &[Match],
    only: Option<usize>,
    replacement: &str,
) -> Vec<(Match, String)> {
    let chosen: Vec<Match> = match only {
        Some(i) => matches.get(i).copied().into_iter().collect(),
        None => matches.iter().rev().copied().collect(),
    };
    if !options.regex && !options.preserve_case {
        return chosen.into_iter().map(|m| (m, replacement.to_string())).collect();
    }
    let matcher = if options.regex { Matcher::new(&search_options(query, options)).ok() } else { None };
    if options.regex && matcher.is_none() {
        return Vec::new();
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out = Vec::with_capacity(chosen.len());
    for m in chosen {
        let Some(raw) = lines.get(m.line as usize) else { continue };
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let base = match &matcher {
            Some(matcher) => {
                let caps = matcher.captures(line).into_iter().find(|c| {
                    c.pos(0).map(|(s, e)| (utf16_col(line, s), utf16_col(line, e))) == Some((m.start, m.end))
                });
                match caps {
                    Some(caps) => expand_replacement(replacement, &caps),
                    None => continue,
                }
            }
            None => replacement.to_string(),
        };
        let text = if options.preserve_case { preserve_case(utf16_slice(line, m.start, m.end), &base) } else { base };
        out.push((m, text));
    }
    out
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
