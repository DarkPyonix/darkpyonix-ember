//! Per-document indentation (the status bar's indentation item and its pick): the options of one
//! document, guessing them from its content, and converting its leading whitespace.
//!
//! Code-OSS keeps `tabSize`, `indentSize` and `insertSpaces` on each text model. The pick in
//! `editorStatus.ts` (`showIndentationPicker`) runs the actions of `indentation.ts`
//! (`IndentUsingSpaces`, `IndentUsingTabs`, `ChangeTabDisplaySize`, `DetectIndentation`,
//! `IndentationToSpacesAction`, `IndentationToTabsAction`); the guess is `indentationGuesser.ts`.
//! Pure; the session core applies the result.

use crate::coords::{WidgetPos, WidgetRange};
use crate::language::TextEdit;

/// The indentation options of one document (Code-OSS `TextModelResolvedOptions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Indentation {
    /// How many columns a tab character spans.
    pub tab_size: u32,
    /// How many columns one level of indentation is.
    pub indent_size: u32,
    /// Whether the Tab key inserts spaces (true) or a tab character (false).
    pub insert_spaces: bool,
}

impl Indentation {
    /// Options with one size for both the tab and the indent, as a document opens.
    pub fn new(size: u32, insert_spaces: bool) -> Self {
        Self {
            tab_size: size,
            indent_size: size,
            insert_spaces,
        }
    }
}

/// What a choice in the indentation pick does to a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndentationAction {
    /// "Indent Using Spaces" or "Indent Using Tabs" with the size picked: sets the tab size, the
    /// indent size and `insert_spaces`. The text is not touched.
    IndentUsing { insert_spaces: bool, size: u32 },
    /// "Change Tab Display Size": sets the tab size only.
    DisplaySize(u32),
    /// "Detect Indentation from Content": guesses the options from the text (`model.detectIndentation`).
    Detect,
    /// "Convert Indentation to Spaces" (`true`) or "to Tabs" (`false`): rewrites the leading
    /// whitespace of every line, then sets `insert_spaces` to match.
    Convert { to_spaces: bool },
    /// "Trim Trailing Whitespace", the last row of the same pick: removes the spaces and tabs that end
    /// a line. Options are not touched.
    TrimTrailingWhitespace,
}

const SPACE: u16 = b' ' as u16;
const TAB: u16 = b'\t' as u16;
const COMMA: u16 = b',' as u16;

/// The difference in spaces between the indentation of two lines, and whether it looks like
/// alignment (`spacesDiff` in `indentationGuesser.ts`). `a` and `b` are whole lines, `a_len` and
/// `b_len` the length of their indentation.
fn spaces_diff(a: &[u16], a_len: usize, b: &[u16], b_len: usize) -> (usize, bool) {
    let mut i = 0;
    while i < a_len && i < b_len && a[i] == b[i] {
        i += 1;
    }
    let (mut a_spaces, mut a_tabs) = (0usize, 0usize);
    for &c in &a[i..a_len] {
        if c == SPACE {
            a_spaces += 1;
        } else {
            a_tabs += 1;
        }
    }
    let (mut b_spaces, mut b_tabs) = (0usize, 0usize);
    for &c in &b[i..b_len] {
        if c == SPACE {
            b_spaces += 1;
        } else {
            b_tabs += 1;
        }
    }
    if (a_spaces > 0 && a_tabs > 0) || (b_spaces > 0 && b_tabs > 0) {
        return (0, false);
    }
    let tabs_diff = a_tabs.abs_diff(b_tabs);
    let spaces_diff = a_spaces.abs_diff(b_spaces);
    if tabs_diff == 0 {
        // Folk align code under an earlier line; that is no hint of the indent size.
        let mut alignment = false;
        if spaces_diff > 0
            && b_spaces >= 1
            && b_spaces - 1 < a.len()
            && b_spaces < b.len()
            && b[b_spaces] != SPACE
            && a[b_spaces - 1] == SPACE
            && a[a.len() - 1] == COMMA
        {
            alignment = true;
        }
        return (spaces_diff, alignment);
    }
    if spaces_diff % tabs_diff == 0 {
        return (spaces_diff / tabs_diff, false);
    }
    (0, false)
}

/// Guess the tab size and `insert_spaces` of `lines` (`guessIndentation`): the first 10000 lines
/// decide, tabs win when more lines start with a tab than with two or more spaces, and the tab size
/// is the most common step between the indentation of neighbouring lines. `default_*` stand when
/// the text gives no hint.
pub fn guess_indentation(
    lines: &[String],
    default_tab_size: u32,
    default_insert_spaces: bool,
) -> (u32, bool) {
    const ALLOWED: [usize; 7] = [2, 4, 6, 8, 3, 5, 7];
    const MAX_GUESS: usize = 8;

    let mut tab_lines = 0usize;
    let mut space_lines = 0usize;
    let mut previous: Vec<u16> = Vec::new();
    let mut previous_indent = 0usize;
    let mut counts = [0usize; MAX_GUESS + 1];

    for line in lines.iter().take(10_000) {
        let current: Vec<u16> = line.encode_utf16().collect();
        let (mut spaces, mut tabs) = (0usize, 0usize);
        let mut indent = None;
        for (j, &c) in current.iter().enumerate() {
            match c {
                TAB => tabs += 1,
                SPACE => spaces += 1,
                _ => {
                    indent = Some(j);
                    break;
                }
            }
        }
        // An empty or whitespace-only line says nothing.
        let Some(indent) = indent else { continue };
        if tabs > 0 {
            tab_lines += 1;
        } else if spaces > 1 {
            space_lines += 1;
        }
        let (diff, alignment) = spaces_diff(&previous, previous_indent, &current, indent);
        if alignment && !(default_insert_spaces && default_tab_size as usize == diff) {
            continue;
        }
        if diff <= MAX_GUESS {
            counts[diff] += 1;
        }
        previous = current;
        previous_indent = indent;
    }

    let mut insert_spaces = default_insert_spaces;
    if tab_lines != space_lines {
        insert_spaces = tab_lines < space_lines;
    }
    let mut tab_size = default_tab_size;
    if insert_spaces {
        let mut best = 0;
        for size in ALLOWED {
            if counts[size] > best {
                best = counts[size];
                tab_size = size as u32;
            }
        }
        // 2 beats 4 when it has at least two thirds of the occurrences of 4 (YAML nests by 4 now and then).
        if tab_size == 4
            && counts[4] > 0
            && counts[2] > 0
            && counts[2] as f64 >= counts[4] as f64 * 2.0 / 3.0
        {
            tab_size = 2;
        }
    }
    (tab_size, insert_spaces)
}

/// The edits that convert the leading whitespace of every line (`getIndentationEditOperations`):
/// each tab becomes `tab_size` spaces, or each run of `tab_size` spaces becomes a tab. A line of
/// whitespace only is converted whole. One edit per line that changes, last line first (the order
/// the widget takes them in).
pub fn convert_indentation_edits(
    lines: &[String],
    tab_size: u32,
    to_spaces: bool,
) -> Vec<TextEdit> {
    if tab_size == 0 {
        return Vec::new();
    }
    let spaces = " ".repeat(tab_size as usize);
    let mut edits = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        let end = line.find(|c| c != ' ' && c != '\t').unwrap_or(line.len());
        if end == 0 {
            continue;
        }
        let old = &line[..end];
        let new = if to_spaces {
            old.replace('\t', &spaces)
        } else {
            old.replace(spaces.as_str(), "\t")
        };
        if new != old {
            // The indentation is ASCII, so its byte length is its UTF-16 length.
            let range = WidgetRange::new(
                WidgetPos::new(n as u32, 0),
                WidgetPos::new(n as u32, end as u32),
            );
            edits.push(TextEdit { range, text: new });
        }
    }
    edits.reverse();
    edits
}

/// The edits that remove the spaces and tabs ending each line (`TrimTrailingWhitespaceCommand`),
/// last line first. The caret's own line is trimmed too: the session has no caret position.
pub fn trim_trailing_whitespace_edits(lines: &[String]) -> Vec<TextEdit> {
    let mut edits = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        let kept = line.trim_end_matches([' ', '\t']);
        if kept.len() == line.len() {
            continue;
        }
        let start = crate::coords::utf16_len(kept);
        let end = crate::coords::utf16_len(line);
        edits.push(TextEdit {
            range: WidgetRange::new(
                WidgetPos::new(n as u32, start),
                WidgetPos::new(n as u32, end),
            ),
            text: String::new(),
        });
    }
    edits.reverse();
    edits
}

/// `text` with its indentation converted: [`convert_indentation_edits`] applied.
pub fn convert_indentation(text: &str, tab_size: u32, to_spaces: bool) -> String {
    let lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
    let edits = convert_indentation_edits(&lines, tab_size, to_spaces);
    crate::language::edits::apply_text_edits(text, &edits).unwrap_or_else(|_| text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.split('\n').map(str::to_owned).collect()
    }

    #[test]
    fn fr_l31_indentation_guess_finds_spaces_and_their_size() {
        assert_eq!(
            guess_indentation(&lines("a\n  b\n    c\n  d\ne"), 4, false),
            (2, true)
        );
        assert_eq!(
            guess_indentation(&lines("a\n    b\n        c\n    d"), 2, false),
            (4, true)
        );
    }

    #[test]
    fn fr_l31_indentation_guess_finds_tabs() {
        let (_, insert_spaces) = guess_indentation(&lines("a\n\tb\n\t\tc\n\td"), 4, true);
        assert!(!insert_spaces);
        // With tabs the size stays the default: it is the tab's display width, not a guess.
        assert_eq!(guess_indentation(&lines("a\n\tb\n\t\tc"), 3, true).0, 3);
    }

    #[test]
    fn fr_l31_indentation_guess_keeps_the_defaults_when_the_text_says_nothing() {
        assert_eq!(guess_indentation(&lines("a\nb\nc"), 8, false), (8, false));
        assert_eq!(guess_indentation(&lines(""), 4, true), (4, true));
    }

    #[test]
    fn fr_l31_indentation_guess_prefers_two_over_four_when_two_is_common() {
        // Six steps of 2 and eight of 4: 4 wins by count, but 2 has more than two thirds of it.
        let mut text = String::from("a");
        for _ in 0..3 {
            text.push_str("\n  b\nc");
        }
        for _ in 0..4 {
            text.push_str("\n    d\ne");
        }
        assert_eq!(guess_indentation(&lines(&text), 4, true), (2, true));
    }

    #[test]
    fn fr_l31_convert_indentation_to_spaces_replaces_each_leading_tab() {
        assert_eq!(
            convert_indentation("a\n\tb\n\t\tc\n \td", 4, true),
            "a\n    b\n        c\n     d"
        );
        // Tabs after the first character are text, not indentation.
        assert_eq!(convert_indentation("\ta\tb", 2, true), "  a\tb");
    }

    #[test]
    fn fr_l31_convert_indentation_to_tabs_replaces_each_run_of_tab_size_spaces() {
        assert_eq!(
            convert_indentation("a\n    b\n        c\n      d", 4, false),
            "a\n\tb\n\t\tc\n\t  d"
        );
        assert_eq!(convert_indentation("  a\n   b", 2, false), "\ta\n\t b");
        assert_eq!(convert_indentation("a    b", 4, false), "a    b");
    }

    #[test]
    fn fr_l31_convert_indentation_changes_nothing_in_an_empty_or_converted_text() {
        assert!(convert_indentation_edits(&lines(""), 4, true).is_empty());
        assert!(convert_indentation_edits(&lines("a\n    b"), 4, true).is_empty());
        assert!(convert_indentation_edits(&lines("a\n\tb"), 4, false).is_empty());
    }

    #[test]
    fn fr_l31_trim_trailing_whitespace_removes_spaces_and_tabs_at_line_ends() {
        let edits = trim_trailing_whitespace_edits(&lines("a  \n\tb\t \n   \nc"));
        let text = crate::language::edits::apply_text_edits("a  \n\tb\t \n   \nc", &edits).unwrap();
        assert_eq!(text, "a\n\tb\n\nc");
        assert!(trim_trailing_whitespace_edits(&lines("a\n b")).is_empty());
    }

    #[test]
    fn fr_l31_convert_indentation_edits_come_last_line_first() {
        let edits = convert_indentation_edits(&lines("\ta\n\tb"), 2, true);
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].range.start.line, 1);
        assert_eq!(edits[1].range.start.line, 0);
    }
}
