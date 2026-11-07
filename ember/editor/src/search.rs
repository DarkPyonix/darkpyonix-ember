//! Search across files and replace (SPEC FR-E7): the pure parts the session core and the window share.
//!
//! The search itself runs in the OSE's extension host (ripgrep, see `ember_editor_conn::search`). Ember
//! matches text on its own in two places only, both with [`Matcher`] (Oniguruma, already linked for the
//! TextMate grammars, line by line like ripgrep without multiline):
//!
//! * open documents with unsaved edits are searched in their buffer instead of the disk, as Code-OSS does;
//! * a replace finds the matches again in the file's current text and replaces only those at the position
//!   of a result the user saw ([`replace_in_text`]), so a file that changed since the search is not damaged.
//!
//! Positions are 0-based lines and 0-based UTF-16 columns, the widget's and the search provider's own.

use std::collections::HashSet;

use ember_editor_conn::search::{expand_patterns, FileMatch, TextQuery};
use ember_editor_conn::uri::UriComponents;
use onig::{Captures, Regex, RegexOptions, Syntax};

/// The most matches one search asks the provider for (`maxResults`). Code-OSS asks for 20,000; the panel
/// draws every row (no virtual list), so Ember stops earlier and says so.
pub const MAX_RESULTS: u32 = 2000;

/// How much of a long line a locally found match keeps as its preview: this many characters before the
/// match, and at most [`PREVIEW_MAX_CHARS`] in all (the provider's own previews are 1,000 UTF-16 units).
const PREVIEW_BEFORE_CHARS: usize = 100;
const PREVIEW_MAX_CHARS: usize = 1000;

/// What to look for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchOptions {
    pub pattern: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
}

/// One search as the window asks for it: the options and the include and exclude fields as typed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchRequest {
    pub options: SearchOptions,
    pub include: String,
    pub exclude: String,
    /// "제외 설정 및 무시 파일 사용": `.gitignore` and Code-OSS's default excludes.
    pub use_ignore_files: bool,
}

impl SearchRequest {
    /// The provider query over `folder`.
    pub fn query(&self, folder: UriComponents) -> TextQuery {
        TextQuery {
            pattern: self.options.pattern.clone(),
            is_regexp: self.options.regex,
            is_case_sensitive: self.options.case_sensitive,
            is_word_match: self.options.whole_word,
            folder,
            include: expand_patterns(&self.include),
            exclude: expand_patterns(&self.exclude),
            use_ignore_files: self.use_ignore_files,
            max_results: MAX_RESULTS,
        }
    }
}

/// One match as the panel shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub line: u32,
    pub start: u32,
    pub end_line: u32,
    pub end: u32,
    /// The line, or the part of a long line around the match, without its line break.
    pub preview: String,
    /// Where the match is in `preview` (UTF-16, on its one line).
    pub preview_start: u32,
    pub preview_end: u32,
}

impl Hit {
    /// The match as a replace target. A match over several lines cannot be one.
    pub fn target(&self) -> Option<Target> {
        (self.line == self.end_line).then_some(Target { line: self.line, start: self.start, end: self.end })
    }
}

/// The matches of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHits {
    pub uri: UriComponents,
    pub hits: Vec<Hit>,
}

/// How a search ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchEnd {
    /// The provider finished; `limit_hit` when it stopped at [`MAX_RESULTS`].
    Complete { limit_hit: bool },
    /// Stopped by the user or by a newer search.
    Cancelled,
    /// The provider failed (a bad regular expression, ripgrep missing); its own text.
    Failed(String),
    /// The extension host has not registered its search provider (yet).
    Unavailable,
}

/// A match the user chose to replace, where the search reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Target {
    pub line: u32,
    pub start: u32,
    pub end: u32,
}

/// A replace as the window asks for it: the options of the search, the replacement and the matches per file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplaceRequest {
    pub options: SearchOptions,
    pub replacement: String,
    pub files: Vec<(UriComponents, Vec<Target>)>,
}

/// What a replace did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplaceReport {
    pub replaced: usize,
    /// Matches not replaced: the text at their place no longer matched, or their file failed.
    pub skipped: usize,
    /// Files written.
    pub files: usize,
    /// `(path, error)` for each file that could not be read or written.
    pub failures: Vec<(String, String)>,
}

fn utf16_len(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

/// The UTF-16 column of byte offset `byte` in `line`.
pub fn utf16_col(line: &str, byte: usize) -> u32 {
    utf16_len(&line[..byte.min(line.len())])
}

/// The first line of a preview text, without its line break.
fn first_line(text: &str) -> &str {
    let line = text.split('\n').next().unwrap_or("");
    line.strip_suffix('\r').unwrap_or(line)
}

/// The provider's matches as panel rows.
pub fn hits_from_matches(files: Vec<FileMatch>) -> Vec<FileHits> {
    files
        .into_iter()
        .filter_map(|f| {
            let mut hits = Vec::new();
            for m in f.matches {
                let preview = first_line(&m.preview_text).to_string();
                let len = utf16_len(&preview);
                for r in m.ranges {
                    let preview_start = r.preview.start_column.min(len);
                    let preview_end = if r.preview.end_line_number > r.preview.start_line_number { len } else { r.preview.end_column.min(len) };
                    hits.push(Hit {
                        line: r.source.start_line_number,
                        start: r.source.start_column,
                        end_line: r.source.end_line_number,
                        end: r.source.end_column,
                        preview: preview.clone(),
                        preview_start,
                        preview_end: preview_end.max(preview_start),
                    });
                }
            }
            (!hits.is_empty()).then_some(FileHits { uri: f.resource, hits })
        })
        .collect()
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether the match `start..end` (bytes) of `line` stands on word boundaries, as whole-word search wants.
fn word_bounded(line: &str, start: usize, end: usize) -> bool {
    let first = line[start..].chars().next();
    let last = line[..end].chars().next_back();
    let before = line[..start].chars().next_back();
    let after = line[end..].chars().next();
    let before_ok = match (before, first) {
        (Some(b), Some(f)) => !(is_word_char(b) && is_word_char(f)),
        _ => true,
    };
    let after_ok = match (last, after) {
        (Some(l), Some(a)) => !(is_word_char(l) && is_word_char(a)),
        _ => true,
    };
    before_ok && after_ok
}

/// The characters that have a meaning in an Oniguruma (Ruby syntax) pattern outside a class. Only these are
/// escaped: a backslash before some other punctuation is itself an operator there (`` \` `` and `\'` are
/// buffer anchors).
const REGEX_META: &str = "\\^$.|?*+()[]{}";

fn push_literal(out: &mut String, c: char) {
    match c {
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        c if REGEX_META.contains(c) => {
            out.push('\\');
            out.push(c);
        }
        c => out.push(c),
    }
}

/// `pattern` as a regular expression that matches it literally.
pub fn escape_literal(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() * 2);
    for c in pattern.chars() {
        push_literal(&mut out, c);
    }
    out
}

/// Finds matches of one query in text, line by line.
pub struct Matcher {
    regex: Regex,
    whole_word: bool,
}

impl Matcher {
    /// Fails for an empty pattern or a regular expression Oniguruma cannot compile (its own message).
    pub fn new(options: &SearchOptions) -> Result<Self, String> {
        if options.pattern.is_empty() {
            return Err("empty pattern".to_string());
        }
        let source = if options.regex { options.pattern.clone() } else { escape_literal(&options.pattern) };
        let mut flags = RegexOptions::REGEX_OPTION_CAPTURE_GROUP;
        if !options.case_sensitive {
            flags |= RegexOptions::REGEX_OPTION_IGNORECASE;
        }
        let regex = Regex::with_options(&source, flags, Syntax::default()).map_err(|e| e.description().to_string())?;
        Ok(Self { regex, whole_word: options.whole_word })
    }

    /// The matches on one line (no line break), with their capture groups. Empty matches are left out.
    pub fn captures<'t>(&self, line: &'t str) -> Vec<Captures<'t>> {
        self.regex
            .captures_iter(line)
            .filter(|c| match c.pos(0) {
                Some((s, e)) => s < e && (!self.whole_word || word_bounded(line, s, e)),
                None => false,
            })
            .collect()
    }

    /// Byte ranges of the matches on one line.
    pub fn find(&self, line: &str) -> Vec<(usize, usize)> {
        self.captures(line).iter().filter_map(|c| c.pos(0)).collect()
    }

    /// Every match in `lines` as panel rows.
    pub fn hits_in_lines(&self, lines: &[String]) -> Vec<Hit> {
        let mut out = Vec::new();
        for (i, raw) in lines.iter().enumerate() {
            let line = raw.strip_suffix('\r').unwrap_or(raw.as_str());
            for (s, e) in self.find(line) {
                let (preview, preview_start, preview_end) = preview_window(line, s, e);
                out.push(Hit {
                    line: i as u32,
                    start: utf16_col(line, s),
                    end_line: i as u32,
                    end: utf16_col(line, e),
                    preview,
                    preview_start,
                    preview_end,
                });
            }
        }
        out
    }
}

/// A preview of `line` around the match `start..end` (bytes): the whole line when it is short, else up to
/// [`PREVIEW_BEFORE_CHARS`] characters before the match and [`PREVIEW_MAX_CHARS`] in all.
fn preview_window(line: &str, start: usize, end: usize) -> (String, u32, u32) {
    let chars_before = line[..start].chars().count();
    let total = line.chars().count();
    if total <= PREVIEW_MAX_CHARS {
        return (line.to_string(), utf16_col(line, start), utf16_col(line, end));
    }
    let skip = chars_before.saturating_sub(PREVIEW_BEFORE_CHARS);
    let from = line.char_indices().nth(skip).map(|(b, _)| b).unwrap_or(0);
    let to = line[from..].char_indices().nth(PREVIEW_MAX_CHARS).map(|(b, _)| from + b).unwrap_or(line.len());
    let window = &line[from..to];
    let s = utf16_col(window, start - from);
    let e = utf16_col(window, end.min(to) - from);
    (window.to_string(), s, e.max(s))
}

/// The replacement for one regex match: `$&` and `$0` the match, `$1` to `$99` the groups (an unmatched
/// group is empty), `$$` a dollar, `\n`, `\t` and `\\` a line break, a tab and a backslash. Anything else
/// stays as written.
pub fn expand_replacement(template: &str, caps: &Captures) -> String {
    let chars: Vec<char> = template.chars().collect();
    let groups = caps.len();
    let group = |n: usize| caps.at(n).unwrap_or("").to_string();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match (c, next) {
            ('$', Some('$')) => {
                out.push('$');
                i += 2;
            }
            ('$', Some('&')) => {
                out.push_str(&group(0));
                i += 2;
            }
            ('$', Some(d)) if d.is_ascii_digit() => {
                let d1 = d.to_digit(10).unwrap_or(0) as usize;
                let two = chars.get(i + 2).and_then(|c| c.to_digit(10)).map(|d2| d1 * 10 + d2 as usize);
                match two {
                    Some(n) if n > 0 && n < groups => {
                        out.push_str(&group(n));
                        i += 3;
                    }
                    _ if d1 < groups => {
                        out.push_str(&group(d1));
                        i += 2;
                    }
                    _ => {
                        out.push('$');
                        i += 1;
                    }
                }
            }
            ('\\', Some('n')) => {
                out.push('\n');
                i += 2;
            }
            ('\\', Some('t')) => {
                out.push('\t');
                i += 2;
            }
            ('\\', Some('\\')) => {
                out.push('\\');
                i += 2;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// The replacement text one match gets: expanded for a regex, the literal text otherwise.
pub fn replacement_for(options: &SearchOptions, replacement: &str, caps: &Captures) -> String {
    if options.regex {
        expand_replacement(replacement, caps)
    } else {
        replacement.to_string()
    }
}

/// The text of one line after replacing the matches at `targets` (the ones on this line).
fn replace_line(line: &str, line_no: u32, matcher: &Matcher, options: &SearchOptions, replacement: &str, targets: &HashSet<Target>) -> (String, usize) {
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    for caps in matcher.captures(line) {
        let Some((s, e)) = caps.pos(0) else { continue };
        let t = Target { line: line_no, start: utf16_col(line, s), end: utf16_col(line, e) };
        if targets.contains(&t) {
            edits.push((s, e, replacement_for(options, replacement, &caps)));
        }
    }
    let count = edits.len();
    let mut out = line.to_string();
    for (s, e, text) in edits.into_iter().rev() {
        out.replace_range(s..e, &text);
    }
    (out, count)
}

/// What [`replace_in_text`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextReplace {
    pub text: String,
    pub replaced: usize,
    pub skipped: usize,
}

/// Replace, in `text`, the matches of `options` that sit exactly at `targets`; a target where the text no
/// longer matches is skipped. Line breaks (`\r\n`, `\n`, `\r`) are kept as they are.
pub fn replace_in_text(text: &str, options: &SearchOptions, replacement: &str, targets: &[Target]) -> Result<TextReplace, String> {
    let matcher = Matcher::new(options)?;
    let wanted: HashSet<Target> = targets.iter().copied().collect();
    let lines_with_targets: HashSet<u32> = wanted.iter().map(|t| t.line).collect();
    let mut out = String::with_capacity(text.len());
    let mut replaced = 0;
    let mut rest = text;
    let mut line_no = 0u32;
    loop {
        let (content, brk, tail) = match rest.find(|c: char| c == '\n' || c == '\r') {
            Some(i) => {
                let brk_len = if rest[i..].starts_with("\r\n") { 2 } else { 1 };
                (&rest[..i], &rest[i..i + brk_len], &rest[i + brk_len..])
            }
            None => (rest, "", ""),
        };
        if lines_with_targets.contains(&line_no) {
            let (new_line, n) = replace_line(content, line_no, &matcher, options, replacement, &wanted);
            out.push_str(&new_line);
            replaced += n;
        } else {
            out.push_str(content);
        }
        out.push_str(brk);
        if brk.is_empty() {
            break;
        }
        rest = tail;
        line_no += 1;
    }
    Ok(TextReplace { text: out, replaced, skipped: wanted.len().saturating_sub(replaced) })
}

// ---- globs ----------------------------------------------------------------------------------

/// A glob as an anchored regular expression: `**/` any directories, `**` anything, `*` and `?` within one
/// path segment, `[...]` (`[!...]` negated) and `{a,b}`.
fn glob_regex(glob: &str) -> String {
    let chars: Vec<char> = glob.chars().collect();
    let mut out = String::from("\\A");
    let mut i = 0;
    let mut braces = 0u32;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '*' if chars.get(i + 1) == Some(&'*') => {
                if chars.get(i + 2) == Some(&'/') {
                    out.push_str("(?:.*/)?");
                    i += 3;
                } else {
                    out.push_str(".*");
                    i += 2;
                }
                continue;
            }
            '*' => out.push_str("[^/]*"),
            '?' => out.push_str("[^/]"),
            '[' => match chars[i + 1..].iter().position(|&x| x == ']') {
                Some(len) => {
                    let body: String = chars[i + 1..i + 1 + len].iter().collect();
                    let body = match body.strip_prefix('!') {
                        Some(rest) => format!("^{rest}"),
                        None => body,
                    };
                    out.push('[');
                    out.push_str(&body.replace('\\', "\\\\"));
                    out.push(']');
                    i += len + 2;
                    continue;
                }
                None => out.push_str("\\["),
            },
            '{' => {
                braces += 1;
                out.push_str("(?:");
            }
            '}' if braces > 0 => {
                braces -= 1;
                out.push(')');
            }
            ',' if braces > 0 => out.push('|'),
            c => push_literal(&mut out, c),
        }
        i += 1;
    }
    for _ in 0..braces {
        out.push(')');
    }
    out.push_str("\\z");
    out
}

/// Whether `path` (relative, `/`-separated) matches `glob`. A glob that does not compile matches nothing.
pub fn glob_match(glob: &str, path: &str) -> bool {
    Regex::with_options(&glob_regex(glob), RegexOptions::REGEX_OPTION_NONE, Syntax::default())
        .map(|r| r.find(path).is_some())
        .unwrap_or(false)
}

/// `a/b/c.rs`, `a/b`, `a`: the path and every folder above it, for excludes that name a folder.
fn path_and_parents(relative: &str) -> Vec<&str> {
    let mut out = vec![relative];
    let mut cur = relative;
    while let Some((parent, _)) = cur.rsplit_once('/') {
        out.push(parent);
        cur = parent;
    }
    out
}

/// Whether a file at `relative` is searched: neither it nor a folder above it excluded, and included when
/// there are includes.
pub fn path_included(relative: &str, include: &[String], exclude: &[String]) -> bool {
    let candidates = path_and_parents(relative);
    if exclude.iter().any(|g| candidates.iter().any(|p| glob_match(g, p))) {
        return false;
    }
    include.is_empty() || include.iter().any(|g| glob_match(g, relative))
}

/// `path` relative to `folder`, when it is below it.
pub fn relative_to<'a>(folder: &str, path: &'a str) -> Option<&'a str> {
    let folder = folder.trim_end_matches('/');
    let rest = path.strip_prefix(folder)?;
    let rest = rest.strip_prefix('/')?;
    (!rest.is_empty()).then_some(rest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_editor_conn::search::{RangeLocation, SearchRange, TextMatch};

    fn opts(pattern: &str, regex: bool, case_sensitive: bool, whole_word: bool) -> SearchOptions {
        SearchOptions { pattern: pattern.into(), regex, case_sensitive, whole_word }
    }

    fn range(l: u32, s: u32, e: u32) -> SearchRange {
        SearchRange { start_line_number: l, start_column: s, end_line_number: l, end_column: e }
    }

    #[test]
    fn fr_e7_provider_matches_become_rows_with_the_preview_line() {
        let files = vec![FileMatch {
            resource: UriComponents::remote("h", "/w/a.rs"),
            matches: vec![TextMatch {
                preview_text: "let foo = foo;\r\n".into(),
                ranges: vec![
                    RangeLocation { source: range(9, 4, 7), preview: range(0, 4, 7) },
                    RangeLocation { source: range(9, 10, 13), preview: range(0, 10, 13) },
                ],
            }],
        }];
        let rows = hits_from_matches(files);
        assert_eq!(rows[0].hits.len(), 2);
        assert_eq!(rows[0].hits[0].preview, "let foo = foo;");
        assert_eq!((rows[0].hits[1].line, rows[0].hits[1].start, rows[0].hits[1].end), (9, 10, 13));
        assert_eq!(rows[0].hits[1].target(), Some(Target { line: 9, start: 10, end: 13 }));
    }

    #[test]
    fn fr_e7_literal_search_ignores_case_unless_asked() {
        let m = Matcher::new(&opts("a.b", false, false, false)).unwrap();
        assert_eq!(m.find("A.B axb a.b"), vec![(0, 3), (8, 11)]);
        let m = Matcher::new(&opts("a.b", false, true, false)).unwrap();
        assert_eq!(m.find("A.B axb a.b"), vec![(8, 11)]);
    }

    #[test]
    fn fr_e7_whole_word_needs_word_boundaries() {
        let m = Matcher::new(&opts("foo", false, true, true)).unwrap();
        assert_eq!(m.find("foo foobar _foo foo."), vec![(0, 3), (16, 19)]);
    }

    #[test]
    fn fr_e7_regex_search_and_bad_regex() {
        let m = Matcher::new(&opts(r"fo+\d", true, true, false)).unwrap();
        assert_eq!(m.find("fooo1 fo2 f3"), vec![(0, 5), (6, 9)]);
        assert!(Matcher::new(&opts("(", true, true, false)).is_err());
        assert!(Matcher::new(&opts("", false, true, false)).is_err());
    }

    #[test]
    fn fr_e7_columns_are_utf16() {
        let m = Matcher::new(&opts("x", false, true, false)).unwrap();
        let hits = m.hits_in_lines(&["가😀x".to_string()]);
        // 가 is one UTF-16 unit, 😀 two.
        assert_eq!((hits[0].start, hits[0].end), (3, 4));
        assert_eq!((hits[0].preview_start, hits[0].preview_end), (3, 4));
    }

    #[test]
    fn fr_e7_long_lines_keep_a_window_around_the_match() {
        let line = format!("{}needle{}", "a".repeat(5000), "b".repeat(5000));
        let m = Matcher::new(&opts("needle", false, true, false)).unwrap();
        let hits = m.hits_in_lines(&[line]);
        let h = &hits[0];
        assert_eq!(h.start, 5000);
        assert_eq!(h.preview.chars().count(), 1000);
        assert_eq!(&h.preview[h.preview_start as usize..h.preview_end as usize], "needle");
    }

    #[test]
    fn fr_e7_literal_escape_leaves_ruby_buffer_anchors_alone() {
        let m = Matcher::new(&opts("it's `x` (a+b)*2 \\", false, true, false)).unwrap();
        assert_eq!(m.find("say it's `x` (a+b)*2 \\ ok"), vec![(4, 22)]);
    }

    #[test]
    fn fr_e7_regex_replacement_expands_groups_and_escapes() {
        let m = Matcher::new(&opts(r"(\w+)=(\d+)", true, true, false)).unwrap();
        let caps = m.captures("key=42");
        assert_eq!(expand_replacement("$2:$1 [$&] $$ $9 \\n\\t\\\\", &caps[0]), "42:key [key=42] $ $9 \n\t\\");
        assert_eq!(expand_replacement("$0", &caps[0]), "key=42");
        assert_eq!(expand_replacement("$12", &caps[0]), "key2");
    }

    #[test]
    fn fr_e7_replace_only_the_matches_still_at_the_shown_places() {
        let o = opts("foo", false, true, false);
        let text = "foo foo\r\nbar foo\nfoo";
        let targets = [Target { line: 0, start: 4, end: 7 }, Target { line: 1, start: 4, end: 7 }, Target { line: 2, start: 1, end: 4 }];
        let r = replace_in_text(text, &o, "X", &targets).unwrap();
        assert_eq!(r.text, "foo X\r\nbar X\nfoo");
        assert_eq!((r.replaced, r.skipped), (2, 1));
    }

    #[test]
    fn fr_e7_regex_replace_uses_the_groups_of_each_match() {
        let o = opts(r"v(\d)", true, true, false);
        let r = replace_in_text("v1 v2", &o, "version-$1", &[Target { line: 0, start: 0, end: 2 }, Target { line: 0, start: 3, end: 5 }]).unwrap();
        assert_eq!(r.text, "version-1 version-2");
        assert_eq!(r.replaced, 2);
    }

    #[test]
    fn fr_e7_literal_replacement_is_not_expanded() {
        let o = opts("a", false, true, false);
        let r = replace_in_text("a", &o, "$1\\n", &[Target { line: 0, start: 0, end: 1 }]).unwrap();
        assert_eq!(r.text, "$1\\n");
    }

    #[test]
    fn fr_e7_trailing_line_break_is_kept() {
        let o = opts("a", false, true, false);
        let r = replace_in_text("a\n", &o, "b", &[Target { line: 0, start: 0, end: 1 }]).unwrap();
        assert_eq!(r.text, "b\n");
    }

    #[test]
    fn fr_e7_globs_match_like_code_oss_patterns() {
        assert!(glob_match("**/*.rs", "src/lib.rs"));
        assert!(glob_match("**/*.rs", "lib.rs"));
        assert!(!glob_match("**/*.rs", "src/lib.rs.bak"));
        assert!(glob_match("**/node_modules/**", "web/node_modules/x/y.js"));
        assert!(glob_match("src/**", "src/a/b.rs"));
        assert!(!glob_match("src/*", "src/a/b.rs"));
        assert!(glob_match("**/*.{md,txt}", "docs/a.txt"));
        assert!(glob_match("**/[ab].rs", "x/b.rs"));
        assert!(!glob_match("**/[!ab].rs", "x/b.rs"));
        assert!(glob_match("**/a+b.rs", "a+b.rs"));
    }

    #[test]
    fn fr_e7_include_and_exclude_decide_together() {
        let inc = expand_patterns("*.rs");
        let exc = expand_patterns("target");
        assert!(path_included("src/a.rs", &inc, &exc));
        assert!(!path_included("target/a.rs", &inc, &exc));
        assert!(!path_included("src/a.md", &inc, &exc));
        assert!(path_included("src/a.md", &[], &exc));
        let defaults: Vec<String> = ember_editor_conn::search::DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect();
        assert!(!path_included("web/node_modules/x/y.js", &[], &defaults));
        assert!(path_included("web/src/y.js", &[], &defaults));
        assert_eq!(relative_to("/w/p/", "/w/p/src/a.rs"), Some("src/a.rs"));
        assert_eq!(relative_to("/w/p", "/w/pp/a.rs"), None);
    }
}
