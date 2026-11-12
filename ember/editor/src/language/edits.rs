//! Text edits and workspace edits the extension host answers with (formatting, rename, code
//! actions, a completion's additional edits), and how they are applied to a text.
//!
//! `TextEdit` is `{ range: IRange, text: string, eol? }` (`languages.ts`); `IWorkspaceEditDto` is
//! `{ edits: [...] }` (extHost.protocol.ts L2830) whose entries are text edits
//! (`{ resource, textEdit, versionId }`), file edits (`{ oldResource?, newResource?, options? }`) or
//! notebook cell edits. Ember applies the text edits. File and cell edits are counted as
//! unsupported so the UI can say they were left out.

use ember_editor_conn::exthost::Range;
use ember_editor_conn::uri::UriComponents;
use serde_json::Value;

use crate::coords::{utf16_to_byte, WidgetPos, WidgetRange};

/// An `IRange` as a widget range, `None` when the value is not one.
pub fn parse_range(v: &Value) -> Option<WidgetRange> {
    serde_json::from_value::<Range>(v.clone()).ok().map(WidgetRange::from_exthost)
}

/// One replacement in a document, in widget coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub range: WidgetRange,
    pub text: String,
}

impl TextEdit {
    pub fn from_json(v: &Value) -> Option<Self> {
        Some(Self { range: parse_range(v.get("range")?)?, text: v.get("text").and_then(Value::as_str).unwrap_or_default().to_owned() })
    }
}

/// A `TextEdit[]` reply. `null`, `undefined` and entries that are not edits give nothing.
pub fn parse_text_edits(v: &Value) -> Vec<TextEdit> {
    v.as_array().map(|a| a.iter().filter_map(TextEdit::from_json).collect()).unwrap_or_default()
}

/// The edits in the order they are sent to the widget: last in the document first, so an earlier
/// edit never moves the place a later one was written for.
pub fn widget_order(edits: &[TextEdit]) -> Vec<TextEdit> {
    let mut v = edits.to_vec();
    v.sort_by(|a, b| b.range.start.cmp(&a.range.start).then(b.range.end.cmp(&a.range.end)));
    v
}

/// Byte offset of `pos` in `text`. A line past the end is the end of the text and a column past the
/// end of its line is the end of that line, as Monaco validates positions.
fn offset_of(text: &str, line_starts: &[usize], pos: WidgetPos) -> usize {
    let Some(&start) = line_starts.get(pos.line as usize) else { return text.len() };
    let end = line_starts.get(pos.line as usize + 1).map_or(text.len(), |next| next - 1);
    // A `\r` before the `\n` belongs to the line break, not to the line.
    let end = if end > start && text.as_bytes()[end - 1] == b'\r' { end - 1 } else { end };
    let line = &text[start..end];
    start + utf16_to_byte(line, crate::coords::clamp_col(line, pos.col)).unwrap_or(line.len())
}

/// Apply `edits`, all written against `text`, and return the new text. Edits that overlap are
/// refused, because the result would depend on their order. Line breaks inside an inserted text
/// follow the text's own (`\r\n` when the text uses it).
pub fn apply_text_edits(text: &str, edits: &[TextEdit]) -> Result<String, String> {
    let mut line_starts = vec![0usize];
    line_starts.extend(text.bytes().enumerate().filter(|(_, b)| *b == b'\n').map(|(i, _)| i + 1));
    let crlf = text.contains("\r\n");
    let mut spans: Vec<(usize, usize, String)> = edits
        .iter()
        .map(|e| {
            let (a, b) = (offset_of(text, &line_starts, e.range.start), offset_of(text, &line_starts, e.range.end));
            let insert = e.text.replace("\r\n", "\n").replace('\r', "\n");
            (a.min(b), a.max(b), if crlf { insert.replace('\n', "\r\n") } else { insert })
        })
        .collect();
    spans.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut out = String::with_capacity(text.len());
    let mut at = 0usize;
    for (start, end, insert) in &spans {
        if *start < at {
            return Err("overlapping edits".to_owned());
        }
        out.push_str(&text[at..*start]);
        out.push_str(insert);
        at = *end;
    }
    out.push_str(&text[at..]);
    Ok(out)
}

/// The text edits of an `IWorkspaceEditDto`, grouped by file in first-seen order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WorkspaceEdit {
    pub files: Vec<(UriComponents, Vec<TextEdit>)>,
    /// File operations (create, rename, delete) and notebook cell edits, which Ember does not apply.
    pub unsupported: usize,
}

impl WorkspaceEdit {
    /// `None` when the value has no `edits` array (a `null` reply, or a rejection).
    pub fn from_json(v: &Value) -> Option<Self> {
        let entries = v.get("edits")?.as_array()?;
        let mut out = Self::default();
        for entry in entries {
            let parsed = entry
                .get("resource")
                .and_then(|r| serde_json::from_value::<UriComponents>(r.clone()).ok())
                .zip(entry.get("textEdit").and_then(TextEdit::from_json));
            let Some((uri, edit)) = parsed else {
                out.unsupported += 1;
                continue;
            };
            let key = uri.key();
            match out.files.iter_mut().find(|(u, _)| u.key() == key) {
                Some((_, edits)) => edits.push(edit),
                None => out.files.push((uri, vec![edit])),
            }
        }
        Some(out)
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.unsupported == 0
    }
}

/// `rejectReason` of a rename reply (`IWorkspaceEditDto & { rejectReason? }`, `RenameLocation & Rejection`).
pub fn reject_reason(v: &Value) -> Option<String> {
    v.get("rejectReason").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned)
}

/// What applying a workspace edit did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EditSummary {
    /// Files changed: open documents edited in their buffer, other files written to disk.
    pub files: usize,
    pub edits: usize,
    /// File operations and cell edits left out.
    pub unsupported: usize,
    /// Files that could not be read, edited or written: (path, reason).
    pub failures: Vec<(String, String)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn edit(sl: u32, sc: u32, el: u32, ec: u32, text: &str) -> TextEdit {
        TextEdit { range: WidgetRange::new(WidgetPos::new(sl, sc), WidgetPos::new(el, ec)), text: text.into() }
    }

    #[test]
    fn fr_l31_13_text_edits_decode_one_based_ranges() {
        let v = json!([
            {"range": {"startLineNumber": 1, "startColumn": 1, "endLineNumber": 1, "endColumn": 4}, "text": "let"},
            {"range": {"startLineNumber": 3, "startColumn": 2, "endLineNumber": 4, "endColumn": 1}, "text": ""},
            {"text": "no range"}
        ]);
        assert_eq!(parse_text_edits(&v), vec![edit(0, 0, 0, 3, "let"), edit(2, 1, 3, 0, "")]);
        assert!(parse_text_edits(&Value::Null).is_empty());
    }

    #[test]
    fn fr_l31_13_edits_apply_against_the_original_text_in_any_order() {
        let text = "fn a() {\n  let x=1;\n}\n";
        let edits = vec![edit(1, 7, 1, 8, " = "), edit(1, 0, 1, 2, "    "), edit(0, 3, 0, 4, "main")];
        assert_eq!(apply_text_edits(text, &edits).unwrap(), "fn main() {\n    let x = 1;\n}\n");
    }

    #[test]
    fn fr_l31_13_edits_count_utf16_columns_and_clamp_past_the_end() {
        // "😀" is two UTF-16 units: column 3 is just after it.
        assert_eq!(apply_text_edits("a😀b", &[edit(0, 3, 0, 4, "c")]).unwrap(), "a😀c");
        assert_eq!(apply_text_edits("ab\ncd", &[edit(0, 99, 0, 99, "!")]).unwrap(), "ab!\ncd");
        assert_eq!(apply_text_edits("ab", &[edit(9, 0, 9, 0, "\nend")]).unwrap(), "ab\nend");
    }

    #[test]
    fn fr_l31_13_edits_keep_crlf_and_refuse_overlap() {
        assert_eq!(apply_text_edits("a\r\nb\r\n", &[edit(0, 1, 0, 1, "\nx")]).unwrap(), "a\r\nx\r\nb\r\n");
        // The column after the last character of a CRLF line is before the `\r`.
        assert_eq!(apply_text_edits("a\r\nb", &[edit(0, 5, 0, 5, ";")]).unwrap(), "a;\r\nb");
        assert!(apply_text_edits("abcdef", &[edit(0, 0, 0, 3, "x"), edit(0, 2, 0, 4, "y")]).is_err());
        // Two inserts at one place do not overlap; they land in the order given.
        assert_eq!(apply_text_edits("ab", &[edit(0, 1, 0, 1, "1"), edit(0, 1, 0, 1, "2")]).unwrap(), "a12b");
    }

    #[test]
    fn fr_l31_13_widget_order_is_last_first() {
        let v = widget_order(&[edit(0, 0, 0, 1, "a"), edit(2, 0, 2, 1, "c"), edit(1, 0, 1, 1, "b")]);
        assert_eq!(v.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(), ["c", "b", "a"]);
    }

    #[test]
    fn fr_l31_13_workspace_edit_groups_by_file_and_counts_what_it_leaves_out() {
        let a = json!({"$mid": 1, "scheme": "vscode-remote", "authority": "h", "path": "/w/a.rs"});
        let b = json!({"$mid": 1, "scheme": "vscode-remote", "authority": "h", "path": "/w/b.rs"});
        let r = json!({"startLineNumber": 1, "startColumn": 1, "endLineNumber": 1, "endColumn": 2});
        let v = json!({"edits": [
            {"resource": a, "textEdit": {"range": r, "text": "x"}, "versionId": 3},
            {"resource": b, "textEdit": {"range": r, "text": "y"}},
            {"resource": a, "textEdit": {"range": r, "text": "z"}},
            {"oldResource": a, "newResource": b, "options": {}}
        ]});
        let ws = WorkspaceEdit::from_json(&v).unwrap();
        assert_eq!(ws.files.len(), 2);
        assert_eq!(ws.files[0].0.path, "/w/a.rs");
        assert_eq!(ws.files[0].1.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(), ["x", "z"]);
        assert_eq!(ws.files[1].1.len(), 1);
        assert_eq!(ws.unsupported, 1);
        assert!(WorkspaceEdit::from_json(&Value::Null).is_none());
        assert_eq!(reject_reason(&json!({"rejectReason": "not a symbol"})).as_deref(), Some("not a symbol"));
        assert_eq!(reject_reason(&v), None);
    }
}
