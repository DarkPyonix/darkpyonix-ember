//! Find and replace in the open file (D38, #440): the pure model. The matching is a literal search
//! with case and whole-word options over the lines of the document; columns are UTF-16 code units,
//! the widget's own. The find bar in `editor_workbench` shows the result and moves the reader with
//! the editor handle.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Every non-overlapping occurrence of `query` in `text`, in document order. An empty query, or one
/// with a line break, matches nothing.
pub fn find_matches(text: &str, query: &str, options: FindOptions) -> Vec<Match> {
    if query.is_empty() || query.contains('\n') || query.contains('\r') {
        return Vec::new();
    }
    let needle: Vec<char> = query.chars().map(|c| fold(c, options.case_sensitive)).collect();
    let mut out = Vec::new();
    for (line_no, line) in text.split('\n').enumerate() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let chars: Vec<char> = line.chars().collect();
        // UTF-16 column of each char, and one past the end.
        let mut cols: Vec<u32> = Vec::with_capacity(chars.len() + 1);
        let mut col = 0u32;
        for c in &chars {
            cols.push(col);
            col += c.len_utf16() as u32;
        }
        cols.push(col);
        let folded: Vec<char> = chars.iter().map(|c| fold(*c, options.case_sensitive)).collect();
        let mut i = 0;
        while i + needle.len() <= folded.len() {
            if folded[i..i + needle.len()] == needle[..] {
                let end = i + needle.len();
                let before_ok = i == 0 || !is_word(chars[i - 1]) || !is_word(chars[i]);
                let after_ok = end == chars.len() || !is_word(chars[end]) || !is_word(chars[end - 1]);
                if !options.whole_word || (before_ok && after_ok) {
                    out.push(Match { line: line_no as u32, start: cols[i], end: cols[end] });
                    i = end;
                    continue;
                }
            }
            i += 1;
        }
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
