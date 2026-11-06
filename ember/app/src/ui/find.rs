//! Find and replace in the open file (D38, #440): the pure model. The matching is a literal search
//! with case and whole-word options over the lines of the document; columns are UTF-16 code units,
//! the widget's own. The find bar in `editor_workbench` shows the result and moves the reader with
//! the editor handle.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FindOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
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
    if case_sensitive { c } else { c.to_lowercase().next().unwrap_or(c) }
}

/// The most matches kept. The find bar searches on every render, so a query that matches a huge file
/// everywhere stops here instead of building an unbounded list; replace all refuses past it.
pub const MAX_MATCHES: usize = 10_000;

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
    // One set of buffers for every line: a line is scanned without allocating.
    let mut chars: Vec<char> = Vec::new();
    let mut cols: Vec<u32> = Vec::new();
    let mut folded: Vec<char> = Vec::new();
    for (line_no, line) in text.split('\n').enumerate() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        chars.clear();
        chars.extend(line.chars());
        if chars.len() < needle.len() {
            continue;
        }
        // UTF-16 column of each char, and one past the end.
        cols.clear();
        let mut col = 0u32;
        for c in &chars {
            cols.push(col);
            col += c.len_utf16() as u32;
        }
        cols.push(col);
        folded.clear();
        folded.extend(chars.iter().map(|c| fold(*c, options.case_sensitive)));
        let mut i = 0;
        while i + needle.len() <= folded.len() {
            if folded[i..i + needle.len()] == needle[..] {
                let end = i + needle.len();
                let before_ok = i == 0 || !is_word(chars[i - 1]) || !is_word(chars[i]);
                let after_ok = end == chars.len() || !is_word(chars[end]) || !is_word(chars[end - 1]);
                if !options.whole_word || (before_ok && after_ok) {
                    if out.len() == MAX_MATCHES {
                        return (out, true);
                    }
                    out.push(Match { line: line_no as u32, start: cols[i], end: cols[end] });
                    i = end;
                    continue;
                }
            }
            i += 1;
        }
    }
    (out, false)
}

/// [`find_matches_capped`] computed once per text, query and options: the find bar asks on every render
/// and on every button, and the answer only changes when one of the three does. The text is compared by
/// its hash and length: one pass over it per call. The caller's own copy of the text (the find bar clones it on every render) is a separate cost this does not remove.
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

/// The edits for a replacement: the one match at `only`, or every match. They are last first, so each
/// edit is written against the same document version and none moves another.
pub fn replace_edits(matches: &[Match], only: Option<usize>, replacement: &str) -> Vec<(Match, String)> {
    match only {
        Some(i) => matches.get(i).map(|m| vec![(*m, replacement.to_string())]).unwrap_or_default(),
        None => matches.iter().rev().map(|m| (*m, replacement.to_string())).collect(),
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
