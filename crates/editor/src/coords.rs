//! Coordinates.
//!
//! Two systems meet here:
//!
//! * **Widget** (dioxus-compose `CodeEditor`, FR-38 §38.2): lines and columns **0-based**, columns
//!   in **UTF-16 code units**.
//! * **Extension host** (`IRange` / `IPosition`, `ember_editor_conn::exthost`): lines and columns
//!   **1-based**, columns in UTF-16 code units.
//!
//! Both count UTF-16, so converting between them is `±1` on both axes; nothing is re-counted. The
//! helpers for byte / `char` indices exist for the places that touch Rust strings (ghost-text prefix
//! checks, clamping).
//!
//! Line splitting is Monaco's (`\r\n`, `\r`, `\n`), the same as `ember_editor_conn::document::split_lines`.
//! The session hands the widget text joined with `\n` only, so the widget never sees a `\r`.

use ember_editor_conn::exthost::{Position, Range};

/// A widget position: 0-based line, 0-based UTF-16 column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct WidgetPos {
    pub line: u32,
    pub col: u32,
}

impl WidgetPos {
    pub const fn new(line: u32, col: u32) -> Self {
        Self { line, col }
    }

    /// 1-based `IPosition`.
    pub fn to_exthost(self) -> Position {
        Position { line_number: self.line + 1, column: self.col + 1 }
    }

    /// From a 1-based `IPosition`. `0` (invalid upstream) clamps to the first line / column.
    pub fn from_exthost(p: Position) -> Self {
        Self { line: p.line_number.saturating_sub(1), col: p.column.saturating_sub(1) }
    }
}

/// A widget range, `start <= end`, end exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct WidgetRange {
    pub start: WidgetPos,
    pub end: WidgetPos,
}

impl WidgetRange {
    /// Builds a range; swaps the ends if they are reversed.
    pub fn new(start: WidgetPos, end: WidgetPos) -> Self {
        if end < start {
            Self { start: end, end: start }
        } else {
            Self { start, end }
        }
    }

    pub const fn empty(at: WidgetPos) -> Self {
        Self { start: at, end: at }
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Does the range cover `pos`? Inclusive at both ends, like Monaco's `Range.containsPosition`,
    /// so a hover on the last character of a squiggle (or just after it) still finds it.
    pub fn contains(&self, pos: WidgetPos) -> bool {
        self.start <= pos && pos <= self.end
    }

    /// 1-based `IRange`.
    pub fn to_exthost(self) -> Range {
        Range {
            start_line_number: self.start.line + 1,
            start_column: self.start.col + 1,
            end_line_number: self.end.line + 1,
            end_column: self.end.col + 1,
        }
    }

    pub fn from_exthost(r: Range) -> Self {
        Self::new(
            WidgetPos::from_exthost(Position { line_number: r.start_line_number, column: r.start_column }),
            WidgetPos::from_exthost(Position { line_number: r.end_line_number, column: r.end_column }),
        )
    }
}

/// Number of UTF-16 code units in `s`.
pub fn utf16_len(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

/// Byte index of UTF-16 column `col` (0-based) in `line`. `None` if `col` is past the end of the
/// line or falls inside a surrogate pair.
pub fn utf16_to_byte(line: &str, col: u32) -> Option<usize> {
    let mut units = 0u32;
    for (bi, ch) in line.char_indices() {
        if units == col {
            return Some(bi);
        }
        units += ch.len_utf16() as u32;
        if units > col {
            return None;
        }
    }
    (units == col).then_some(line.len())
}

/// UTF-16 column (0-based) of byte index `byte` in `line`. `None` if `byte` is not a char
/// boundary or is past the end.
pub fn byte_to_utf16(line: &str, byte: usize) -> Option<u32> {
    line.get(..byte).map(utf16_len)
}

/// UTF-16 column of the `chars`-th Unicode scalar in `line` (a widget that counts `char`s).
pub fn char_to_utf16(line: &str, chars: usize) -> Option<u32> {
    let mut units = 0u32;
    let mut n = 0usize;
    for ch in line.chars() {
        if n == chars {
            return Some(units);
        }
        units += ch.len_utf16() as u32;
        n += 1;
    }
    (n == chars).then_some(units)
}

/// `char` index of UTF-16 column `col`, `None` inside a surrogate pair or past the end.
pub fn utf16_to_char(line: &str, col: u32) -> Option<usize> {
    let mut units = 0u32;
    for (n, ch) in line.chars().enumerate() {
        if units == col {
            return Some(n);
        }
        units += ch.len_utf16() as u32;
        if units > col {
            return None;
        }
    }
    (units == col).then_some(line.chars().count())
}

/// Clamp a UTF-16 column into `line`, moving off the middle of a surrogate pair to its start.
pub fn clamp_col(line: &str, col: u32) -> u32 {
    let mut units = 0u32;
    for ch in line.chars() {
        let w = ch.len_utf16() as u32;
        if units + w > col {
            return units;
        }
        units += w;
    }
    units
}

/// Clamp `r` into a document given as its lines (`\n`-split, no terminators).
pub fn clamp_range(lines: &[String], r: WidgetRange) -> WidgetRange {
    let clamp = |p: WidgetPos| -> WidgetPos {
        if lines.is_empty() {
            return WidgetPos::default();
        }
        let line = p.line.min(lines.len() as u32 - 1);
        WidgetPos { line, col: clamp_col(&lines[line as usize], p.col) }
    };
    WidgetRange::new(clamp(r.start), clamp(r.end))
}

/// Position just after `text` when it is inserted at `start` (line breaks: `\r\n`, `\r`, `\n`).
pub fn end_of_insert(start: WidgetPos, text: &str) -> WidgetPos {
    let lines = ember_editor_conn::document::split_lines(text);
    let last = lines.last().map(String::as_str).unwrap_or("");
    if lines.len() <= 1 {
        WidgetPos { line: start.line, col: start.col + utf16_len(last) }
    } else {
        WidgetPos { line: start.line + (lines.len() as u32 - 1), col: utf16_len(last) }
    }
}

/// Move a position that is at or after `edit.end` through the edit "replace `edit` with text
/// ending at `ins_end`".
fn shift_after(pos: WidgetPos, edit: WidgetRange, ins_end: WidgetPos) -> WidgetPos {
    if pos.line == edit.end.line {
        WidgetPos { line: ins_end.line, col: ins_end.col + (pos.col - edit.end.col) }
    } else {
        // Line delta may be negative (a multi-line deletion); `pos.line > edit.end.line` here.
        let line = (pos.line as i64 - edit.end.line as i64 + ins_end.line as i64) as u32;
        WidgetPos { line, col: pos.col }
    }
}

/// Carry a decoration range through one edit (`edit` replaced by `text`, `edit` in pre-edit
/// coordinates), the way FR-38 §38.4 says the widget moves decorations: ranges after the edit
/// shift, ranges around it grow or shrink, and a non-empty range the edit wholly removes is gone
/// (`None`). The session mirrors this so its own copy of every decoration matches what the widget
/// shows without a round trip.
pub fn transform_range(r: WidgetRange, edit: WidgetRange, text: &str) -> Option<WidgetRange> {
    let ins_end = end_of_insert(edit.start, text);

    // Entirely before the edit (touching its start counts as before, except for an empty range
    // sitting exactly at an insertion point, which moves with the inserted text).
    if r.end < edit.start || (r.end == edit.start && !(r.is_empty() && edit.is_empty())) {
        return Some(r);
    }
    // Entirely after the edit.
    if r.start >= edit.end {
        return Some(WidgetRange { start: shift_after(r.start, edit, ins_end), end: shift_after(r.end, edit, ins_end) });
    }
    // Overlap.
    if !r.is_empty() && edit.start <= r.start && edit.end >= r.end {
        return None; // wholly replaced
    }
    let start = if r.start < edit.start { r.start } else { ins_end };
    let end = if r.end > edit.end { shift_after(r.end, edit, ins_end) } else { ins_end };
    let out = WidgetRange::new(start, end);
    if out.is_empty() && !r.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(line: u32, col: u32) -> WidgetPos {
        WidgetPos::new(line, col)
    }
    fn r(sl: u32, sc: u32, el: u32, ec: u32) -> WidgetRange {
        WidgetRange::new(p(sl, sc), p(el, ec))
    }

    #[test]
    fn widget_and_exthost_differ_by_one_on_both_axes() {
        let w = r(0, 0, 2, 5);
        let e = w.to_exthost();
        assert_eq!((e.start_line_number, e.start_column, e.end_line_number, e.end_column), (1, 1, 3, 6));
        assert_eq!(WidgetRange::from_exthost(e), w);
        assert_eq!(p(4, 7).to_exthost(), Position { line_number: 5, column: 8 });
        assert_eq!(WidgetPos::from_exthost(Position { line_number: 0, column: 0 }), p(0, 0));
    }

    #[test]
    fn utf16_byte_and_char_conversions_with_surrogate_pairs() {
        // 'é' = 1 unit / 2 bytes, '😀' = 2 units / 4 bytes / 1 char, '한' = 1 unit / 3 bytes.
        let line = "é😀한z";
        assert_eq!(utf16_len(line), 5);
        assert_eq!(utf16_to_byte(line, 0), Some(0));
        assert_eq!(utf16_to_byte(line, 1), Some(2));
        assert_eq!(utf16_to_byte(line, 2), None); // inside the pair
        assert_eq!(utf16_to_byte(line, 3), Some(6));
        assert_eq!(utf16_to_byte(line, 4), Some(9));
        assert_eq!(utf16_to_byte(line, 5), Some(10));
        assert_eq!(utf16_to_byte(line, 6), None);
        assert_eq!(byte_to_utf16(line, 6), Some(3));
        assert_eq!(byte_to_utf16(line, 3), None); // not a char boundary
        assert_eq!(char_to_utf16(line, 2), Some(3));
        assert_eq!(char_to_utf16(line, 4), Some(5));
        assert_eq!(char_to_utf16(line, 5), None);
        assert_eq!(utf16_to_char(line, 3), Some(2));
        assert_eq!(utf16_to_char(line, 2), None);
        assert_eq!(clamp_col(line, 2), 1);
        assert_eq!(clamp_col(line, 99), 5);
    }

    #[test]
    fn end_of_insert_counts_utf16_and_line_breaks() {
        assert_eq!(end_of_insert(p(3, 4), "ab😀"), p(3, 8));
        assert_eq!(end_of_insert(p(3, 4), "x\r\nyz"), p(4, 2));
        assert_eq!(end_of_insert(p(3, 4), "\n"), p(4, 0));
        assert_eq!(end_of_insert(p(3, 4), ""), p(3, 4));
    }

    #[test]
    fn clamp_range_into_document() {
        let lines = vec!["ab".to_string(), "😀".to_string()];
        assert_eq!(clamp_range(&lines, r(0, 1, 9, 9)), r(0, 1, 1, 2));
        assert_eq!(clamp_range(&lines, r(1, 1, 1, 1)), r(1, 0, 1, 0));
    }

    #[test]
    fn ranges_follow_edits_like_the_widget() {
        let squiggle = r(2, 4, 2, 8);
        // Insert a line above: moves down.
        assert_eq!(transform_range(squiggle, r(0, 0, 0, 0), "new\n"), Some(r(3, 4, 3, 8)));
        // Insert on the same line before it: moves right by UTF-16 units.
        assert_eq!(transform_range(squiggle, r(2, 0, 2, 0), "😀"), Some(r(2, 6, 2, 10)));
        // Edit after it: unchanged.
        assert_eq!(transform_range(squiggle, r(2, 9, 2, 9), "x"), Some(squiggle));
        // Typing exactly at its end: unchanged (does not grow).
        assert_eq!(transform_range(squiggle, r(2, 8, 2, 8), "x"), Some(squiggle));
        // Typing inside: grows.
        assert_eq!(transform_range(squiggle, r(2, 5, 2, 5), "xy"), Some(r(2, 4, 2, 10)));
        // Deleting it: gone.
        assert_eq!(transform_range(squiggle, r(2, 3, 2, 9), ""), None);
        assert_eq!(transform_range(squiggle, r(2, 4, 2, 8), ""), None);
        // Deleting the lines above, joining into its line.
        assert_eq!(transform_range(r(3, 2, 3, 4), r(1, 5, 3, 0), ""), Some(r(1, 7, 1, 9)));
        // Partially deleting its head.
        assert_eq!(transform_range(squiggle, r(2, 2, 2, 6), ""), Some(r(2, 2, 2, 4)));
        // An empty range (ghost text anchor) at an insertion point moves with the insertion.
        assert_eq!(transform_range(r(1, 3, 1, 3), r(1, 3, 1, 3), "ab"), Some(r(1, 5, 1, 5)));
    }
}
