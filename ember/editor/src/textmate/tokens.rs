//! The coloured runs of one open document, kept up to date as it is edited.
//!
//! [`DocumentTokens`] holds the document's lines, the state the tokenizer ended each line in, and the
//! runs (a UTF-16 range and a [`SyntaxRole`]) of each line. An edit re-tokenizes the lines it changed and
//! then goes on only while the state at the end of a line differs from what that line ended in before:
//! the first line that ends in the same state as it used to ends the work, because everything after it
//! tokenizes as it did. Typing inside a line costs one line; opening a comment costs the lines up to
//! where it closes.
//!
//! Memory: lines that end in the same state share one `Arc` of it, so a document costs its text and
//! runs plus one state per distinct state, not one per line. A document over [`MAX_DOCUMENT_BYTES`] or
//! [`MAX_DOCUMENT_LINES`] is not tokenized at all ([`TooLarge`]).

use std::sync::Arc;

use super::interpreter::{Grammar, StateStack};
use super::roles::{role_for_scopes, SyntaxRole};
use crate::coords::{utf16_len, utf16_to_byte, WidgetPos, WidgetRange};

/// The largest document that is tokenized. Larger files are shown without colours, as VS Code does for
/// its "large file" mode.
pub const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_DOCUMENT_LINES: usize = 100_000;

/// The document is over [`MAX_DOCUMENT_BYTES`] or [`MAX_DOCUMENT_LINES`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TooLarge;

/// A coloured run on one line: UTF-16 columns, end exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorRun {
    pub start: u32,
    pub end: u32,
    pub role: SyntaxRole,
}

/// A coloured run in document coordinates (runs never cross a line).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocRun {
    pub line: u32,
    pub start: u32,
    pub end: u32,
    pub role: SyntaxRole,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// The range is outside the document, reversed, or inside a surrogate pair.
    OutOfRange,
    /// The edit would take the document over the size limits (the colours are then stale: the caller drops
    /// the document's tokens).
    TooLarge,
}

/// What an edit touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Changed {
    /// The first line whose runs may differ (in the document after the edit).
    pub first_line: u32,
    /// The last line whose runs may differ (in the document after the edit).
    pub last_line: u32,
    /// How many lines were tokenized again.
    pub tokenized: u32,
}

struct Line {
    text: String,
    runs: Vec<ColorRun>,
    /// The state after this line (shared with the neighbours that end in the same state).
    end: Arc<StateStack>,
    /// Highlighting stopped on this line (too long, or too many rules open).
    skipped: bool,
}

pub struct DocumentTokens {
    grammar: Arc<Grammar>,
    lines: Vec<Line>,
    /// The bytes of every line and its line break, kept up to date by each edit.
    bytes: usize,
}

/// Splits text into lines at `\n` (a `\r` stays at the end of its line).
fn split(text: &str) -> Vec<String> {
    text.split('\n').map(str::to_owned).collect()
}

impl DocumentTokens {
    /// Tokenizes `text` from the start; `Err` when it is too large to colour.
    pub fn new(grammar: Arc<Grammar>, text: &str) -> Result<Self, TooLarge> {
        if text.len() > MAX_DOCUMENT_BYTES || text.bytes().filter(|b| *b == b'\n').count() >= MAX_DOCUMENT_LINES {
            return Err(TooLarge);
        }
        let mut doc = Self { grammar, lines: Vec::new(), bytes: text.len() + 1 };
        let mut prev: Option<Arc<StateStack>> = None;
        for t in split(text) {
            let line = doc.tokenize(t, prev.as_ref());
            prev = Some(line.end.clone());
            doc.lines.push(line);
        }
        Ok(doc)
    }

    fn tokenize(&self, text: String, prev: Option<&Arc<StateStack>>) -> Line {
        let result = self.grammar.tokenize_line(&text, prev.map(|a| &**a));
        let mut runs: Vec<ColorRun> = Vec::new();
        for t in &result.tokens {
            let Some(role) = role_for_scopes(&t.scopes) else { continue };
            let start = utf16_len(&text[..t.start]);
            let end = start + utf16_len(&text[t.start..t.end]);
            match runs.last_mut() {
                Some(last) if last.end == start && last.role == role => last.end = end,
                _ => runs.push(ColorRun { start, end, role }),
            }
        }
        // Most lines end in the state the line before ended in: share it.
        let end = match prev {
            Some(p) if **p == result.end_state => p.clone(),
            _ => Arc::new(result.end_state),
        };
        Line { text, runs, end, skipped: result.skipped }
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// The document text.
    pub fn text(&self) -> String {
        self.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n")
    }

    pub fn line_runs(&self, line: usize) -> &[ColorRun] {
        self.lines.get(line).map(|l| l.runs.as_slice()).unwrap_or(&[])
    }

    /// Whether highlighting stopped on a line (see [`LineTokens::skipped`](super::LineTokens)).
    pub fn line_skipped(&self, line: usize) -> bool {
        self.lines.get(line).is_some_and(|l| l.skipped)
    }

    /// Every run of the document, in order.
    pub fn runs(&self) -> Vec<DocRun> {
        self.runs_between(0, self.lines.len())
    }

    /// The runs of lines `first..last` (end exclusive).
    pub fn runs_between(&self, first: usize, last: usize) -> Vec<DocRun> {
        let mut out = Vec::new();
        for (i, line) in self.lines.iter().enumerate().take(last).skip(first) {
            out.extend(line.runs.iter().map(|r| DocRun { line: i as u32, start: r.start, end: r.end, role: r.role }));
        }
        out
    }

    /// Replaces `range` (UTF-16 line and column, end exclusive) with `text` and brings the runs up to date.
    pub fn apply_edit(&mut self, range: WidgetRange, text: &str) -> Result<Changed, EditError> {
        let (start, end) = (range.start, range.end);
        let (sl, el) = (start.line as usize, end.line as usize);
        if sl >= self.lines.len() || el >= self.lines.len() || sl > el || (sl == el && start.col > end.col) {
            return Err(EditError::OutOfRange);
        }
        let head_byte = utf16_to_byte(&self.lines[sl].text, start.col).ok_or(EditError::OutOfRange)?;
        let tail_byte = utf16_to_byte(&self.lines[el].text, end.col).ok_or(EditError::OutOfRange)?;
        let head = self.lines[sl].text[..head_byte].to_owned();
        let tail = self.lines[el].text[tail_byte..].to_owned();
        let mut new_texts = split(text);
        new_texts.first_mut().expect("split gives at least one line").insert_str(0, &head);
        new_texts.last_mut().expect("a last line").push_str(&tail);
        let removed_bytes: usize = self.lines[sl..=el].iter().map(|l| l.text.len() + 1).sum();
        let added_bytes: usize = new_texts.iter().map(|t| t.len() + 1).sum();
        let new_bytes = self.bytes - removed_bytes + added_bytes;
        if new_bytes > MAX_DOCUMENT_BYTES + 1 || self.lines.len() - (el - sl + 1) + new_texts.len() > MAX_DOCUMENT_LINES {
            return Err(EditError::TooLarge);
        }
        self.bytes = new_bytes;

        let old_last_end = self.lines[el].end.clone();
        let inserted = new_texts.len();
        let mut replacement: Vec<Line> = Vec::with_capacity(inserted);
        let mut prev: Option<Arc<StateStack>> = if sl == 0 { None } else { Some(self.lines[sl - 1].end.clone()) };
        let mut tokenized = 0u32;
        for t in new_texts {
            let line = self.tokenize(t, prev.as_ref());
            prev = Some(line.end.clone());
            replacement.push(line);
            tokenized += 1;
        }
        // The last line of the edit ended in the state the old last line ended in: what follows is as before.
        let settled = replacement.last().is_some_and(|l| *l.end == *old_last_end);
        self.lines.splice(sl..=el, replacement);
        let mut last_changed = sl + inserted - 1;

        // Otherwise go on with the lines after the edit until one ends in the state it ended in before. The old
        // end state is read from the line itself just before it is replaced, so nothing is copied up front.
        let mut i = sl + inserted;
        while !settled && i < self.lines.len() {
            let prev_state = self.lines[i - 1].end.clone();
            let text = std::mem::take(&mut self.lines[i].text);
            let line = self.tokenize(text, Some(&prev_state));
            let old = std::mem::replace(&mut self.lines[i], line);
            tokenized += 1;
            last_changed = i;
            if *old.end == *self.lines[i].end {
                break;
            }
            i += 1;
        }
        Ok(Changed { first_line: sl as u32, last_line: last_changed as u32, tokenized })
    }

    /// The position after `text` inserted at `at`; for callers that track the caret.
    pub fn end_of_insert(at: WidgetPos, text: &str) -> WidgetPos {
        crate::coords::end_of_insert(at, text)
    }
}
