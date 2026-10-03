//! Document model bridge: native editor change events → `ExtHostDocuments.$acceptModelChanged`.
//!
//! The extension host keeps a mirror of every open document (`MirrorTextModel`,
//! `src/vs/editor/common/model/mirrorTextModel.ts` `onEvents` L91-105) and applies each change
//! as delete-range-then-insert-text, **in the order given**. Monaco sends the changes of one edit
//! operation sorted from the end of the document to the start
//! (`ISerializedModelContentChangedEvent.changes` doc, `textModelEvents.ts` L87-91), so every
//! range and `rangeOffset` is valid in pre-edit coordinates. `rangeOffset`/`rangeLength` surface
//! verbatim in `vscode.TextDocumentContentChangeEvent` (`extHostDocuments.ts` L180-224), so they
//! must be exact.
//!
//! Coordinates: lines 1-based, columns 1-based **UTF-16 code units**, offsets in UTF-16 code
//! units (JS string indices). The editor widget may use byte or char offsets internally;
//! [`DocumentMirror::position_from_byte`] converts.
//!
//! The extension host throws `unknown document` for changes to a document it was not told about
//! (`extHostDocuments.ts` L182-185): always send [`DocumentBridge::open`]'s delta first.

use serde::Serialize;
use serde_json::Value;

use crate::exthost::{self, Call, DocumentsAndEditorsDelta, ModelAddedData, Range};
use crate::uri::UriComponents;
use crate::{Error, Result};

/// One replacement, in pre-edit document coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub range: Range,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditReason {
    #[default]
    Normal,
    Undo,
    Redo,
}

/// What the editor widget reports for one atomic edit operation (one undo stop's worth, or one
/// keystroke across all cursors).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EditorChange {
    /// Non-overlapping edits against the document *before* this operation, any order.
    pub edits: Vec<TextEdit>,
    pub reason: EditReason,
    /// The version the editor assigns after this change. `None` → previous + 1. Must increase.
    pub version: Option<u64>,
}

/// `IModelContentChange` (mirrorTextModel.ts L12-29).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelContentChange {
    pub range: Range,
    pub range_offset: u32,
    pub range_length: u32,
    pub text: String,
}

/// `ISerializedModelContentChangedEvent` (textModelEvents.ts L87-127).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelContentChangedEvent {
    pub changes: Vec<ModelContentChange>,
    pub eol: String,
    pub version_id: u64,
    pub is_undoing: bool,
    pub is_redoing: bool,
    pub is_flush: bool,
    pub is_eol_change: bool,
    /// `@internal` edit-source metadata (`{ source: … }`); omitted = `undefined`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detailed_reason: Option<Value>,
}

fn utf16_len(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

/// Split on `\r\n`, `\r` or `\n` (Monaco's line splitting).
pub fn split_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                lines.push(std::mem::take(&mut cur));
            }
            '\n' => lines.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    lines.push(cur);
    lines
}

/// Detect the document EOL like Monaco: `\r\n` if the first line break is CRLF, else `\n`.
pub fn detect_eol(text: &str) -> &'static str {
    match text.find('\n') {
        Some(i) if i > 0 && text.as_bytes()[i - 1] == b'\r' => "\r\n",
        _ => "\n",
    }
}

/// Ember's copy of one document, kept in lock-step with the extension host's mirror.
#[derive(Debug, Clone)]
pub struct DocumentMirror {
    pub uri: UriComponents,
    pub language_id: String,
    pub encoding: String,
    lines: Vec<String>,
    eol: String,
    version: u64,
    dirty: bool,
}

impl DocumentMirror {
    pub fn new(uri: UriComponents, text: &str, language_id: impl Into<String>) -> Self {
        Self {
            uri,
            language_id: language_id.into(),
            encoding: "utf8".into(),
            eol: detect_eol(text).to_owned(),
            lines: split_lines(text),
            version: 1,
            dirty: false,
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }
    pub fn eol(&self) -> &str {
        &self.eol
    }
    pub fn lines(&self) -> &[String] {
        &self.lines
    }
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }
    pub fn text(&self) -> String {
        self.lines.join(&self.eol)
    }

    pub fn added_data(&self) -> ModelAddedData {
        ModelAddedData {
            uri: self.uri.clone(),
            version_id: self.version,
            lines: self.lines.clone(),
            eol: self.eol.clone(),
            language_id: self.language_id.clone(),
            is_dirty: self.dirty,
            encoding: self.encoding.clone(),
        }
    }

    /// Full range of the document.
    pub fn full_range(&self) -> Range {
        let last = self.lines.len() as u32;
        Range {
            start_line_number: 1,
            start_column: 1,
            end_line_number: last,
            end_column: utf16_len(self.lines.last().map(String::as_str).unwrap_or("")) + 1,
        }
    }

    /// Byte index in `line` (1-based) of UTF-16 `column` (1-based).
    fn byte_index(&self, line: u32, column: u32) -> Result<usize> {
        let l = self
            .lines
            .get(line.checked_sub(1).ok_or_else(|| Error::Malformed("line 0".into()))? as usize)
            .ok_or_else(|| Error::Malformed(format!("line {line} out of range")))?;
        let target = column.checked_sub(1).ok_or_else(|| Error::Malformed("column 0".into()))?;
        let mut units = 0u32;
        for (bi, ch) in l.char_indices() {
            if units == target {
                return Ok(bi);
            }
            units += ch.len_utf16() as u32;
            if units > target {
                return Err(Error::Malformed(format!("column {column} splits a surrogate pair")));
            }
        }
        if units == target {
            Ok(l.len())
        } else {
            Err(Error::Malformed(format!("column {column} out of range on line {line}")))
        }
    }

    /// UTF-16 offset of a position (JS string index into `text()`).
    pub fn offset_at(&self, line: u32, column: u32) -> Result<u32> {
        self.byte_index(line, column)?; // validates
        let eol = utf16_len(&self.eol);
        let before: u32 = self.lines[..(line - 1) as usize].iter().map(|l| utf16_len(l) + eol).sum();
        Ok(before + column - 1)
    }

    /// Convert a byte offset within a line to a 1-based UTF-16 column.
    pub fn position_from_byte(&self, line: u32, byte: usize) -> Result<exthost::Position> {
        let l = self
            .lines
            .get((line as usize).wrapping_sub(1))
            .ok_or_else(|| Error::Malformed(format!("line {line} out of range")))?;
        let prefix = l.get(..byte).ok_or_else(|| Error::Malformed("byte offset not on a char boundary".into()))?;
        Ok(exthost::Position { line_number: line, column: utf16_len(prefix) + 1 })
    }

    fn range_is_valid(&self, r: &Range) -> Result<()> {
        let start = (r.start_line_number, r.start_column);
        let end = (r.end_line_number, r.end_column);
        if start > end {
            return Err(Error::Malformed("range end before start".into()));
        }
        self.byte_index(r.start_line_number, r.start_column)?;
        self.byte_index(r.end_line_number, r.end_column)?;
        Ok(())
    }

    /// Replace `range` with `text` (EOLs normalized to the document EOL). Returns the change with
    /// pre-edit offset/length.
    fn apply_one(&mut self, range: Range, text: &str) -> Result<ModelContentChange> {
        let range_offset = self.offset_at(range.start_line_number, range.start_column)?;
        let end_offset = self.offset_at(range.end_line_number, range.end_column)?;
        let sb = self.byte_index(range.start_line_number, range.start_column)?;
        let eb = self.byte_index(range.end_line_number, range.end_column)?;
        let si = (range.start_line_number - 1) as usize;
        let ei = (range.end_line_number - 1) as usize;

        let prefix = self.lines[si][..sb].to_owned();
        let suffix = self.lines[ei][eb..].to_owned();
        let mut new_lines = split_lines(text);
        let normalized = new_lines.join(&self.eol);
        new_lines[0].insert_str(0, &prefix);
        new_lines.last_mut().unwrap().push_str(&suffix);
        self.lines.splice(si..=ei, new_lines).for_each(drop);

        Ok(ModelContentChange { range, range_offset, range_length: end_offset - range_offset, text: normalized })
    }

    /// Apply one editor operation and build the event for `$acceptModelChanged`.
    pub fn apply(&mut self, change: EditorChange) -> Result<ModelContentChangedEvent> {
        let mut edits = change.edits;
        for e in &edits {
            self.range_is_valid(&e.range)?;
        }
        // End-to-start, like Monaco; then reject overlaps.
        edits.sort_by(|a, b| {
            (b.range.start_line_number, b.range.start_column).cmp(&(a.range.start_line_number, a.range.start_column))
        });
        for w in edits.windows(2) {
            let (later, earlier) = (&w[0].range, &w[1].range);
            if (earlier.end_line_number, earlier.end_column) > (later.start_line_number, later.start_column) {
                return Err(Error::Malformed("overlapping edits".into()));
            }
        }
        let new_version = match change.version {
            Some(v) if v <= self.version => {
                return Err(Error::Malformed(format!("version {v} does not increase past {}", self.version)))
            }
            Some(v) => v,
            None => self.version + 1,
        };

        let mut changes = Vec::with_capacity(edits.len());
        for e in edits {
            changes.push(self.apply_one(e.range, &e.text)?);
        }
        self.version = new_version;
        self.dirty = true;
        Ok(ModelContentChangedEvent {
            changes,
            eol: self.eol.clone(),
            version_id: self.version,
            is_undoing: change.reason == EditReason::Undo,
            is_redoing: change.reason == EditReason::Redo,
            is_flush: false,
            is_eol_change: false,
            detailed_reason: None,
        })
    }

    /// Replace the whole content (`model.setValue`): a single full-range change with `isFlush`.
    pub fn replace_all(&mut self, text: &str) -> Result<ModelContentChangedEvent> {
        let full = self.full_range();
        let change = self.apply_one(full, text)?;
        self.version += 1;
        Ok(ModelContentChangedEvent {
            changes: vec![change],
            eol: self.eol.clone(),
            version_id: self.version,
            is_undoing: false,
            is_redoing: false,
            is_flush: true,
            is_eol_change: false,
            detailed_reason: None,
        })
    }

    /// Change the EOL sequence (`model.setEOL`): no content changes, `isEolChange`.
    pub fn set_eol(&mut self, eol: &str) -> ModelContentChangedEvent {
        self.eol = eol.to_owned();
        self.version += 1;
        ModelContentChangedEvent {
            changes: Vec::new(),
            eol: self.eol.clone(),
            version_id: self.version,
            is_undoing: false,
            is_redoing: false,
            is_flush: false,
            is_eol_change: true,
            detailed_reason: None,
        }
    }

    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }
}

/// All documents open in the editor core, producing the RPC calls that keep the extension host
/// in sync.
#[derive(Debug, Default)]
pub struct DocumentBridge {
    docs: std::collections::HashMap<String, DocumentMirror>,
}

impl DocumentBridge {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, uri: &UriComponents) -> Option<&DocumentMirror> {
        self.docs.get(&uri.key())
    }

    /// Open a document: `onLanguage:<id>` activation + `$acceptDocumentsAndEditorsDelta`.
    pub fn open(&mut self, uri: UriComponents, text: &str, language_id: &str) -> Vec<Call> {
        let doc = DocumentMirror::new(uri.clone(), text, language_id);
        let delta = DocumentsAndEditorsDelta { added_documents: Some(vec![doc.added_data()]), ..Default::default() };
        self.docs.insert(uri.key(), doc);
        vec![exthost::activate_by_event(&format!("onLanguage:{language_id}")), exthost::accept_documents_and_editors_delta(&delta)]
    }

    pub fn close(&mut self, uri: &UriComponents) -> Option<Call> {
        self.docs.remove(&uri.key())?;
        let delta = DocumentsAndEditorsDelta { removed_documents: Some(vec![uri.clone()]), ..Default::default() };
        Some(exthost::accept_documents_and_editors_delta(&delta))
    }

    /// Apply an editor change and return the `$acceptModelChanged` call.
    pub fn change(&mut self, uri: &UriComponents, change: EditorChange) -> Result<Call> {
        let doc = self.docs.get_mut(&uri.key()).ok_or_else(|| Error::Malformed("unknown document".into()))?;
        let ev = doc.apply(change)?;
        Ok(exthost::accept_model_changed(&doc.uri, &ev, doc.is_dirty()))
    }

    pub fn saved(&mut self, uri: &UriComponents) -> Option<Call> {
        let doc = self.docs.get_mut(&uri.key())?;
        doc.mark_saved();
        Some(exthost::accept_model_saved(&doc.uri))
    }

    pub fn set_language(&mut self, uri: &UriComponents, language_id: &str) -> Vec<Call> {
        let Some(doc) = self.docs.get_mut(&uri.key()) else { return Vec::new() };
        doc.language_id = language_id.to_owned();
        vec![
            exthost::activate_by_event(&format!("onLanguage:{language_id}")),
            exthost::accept_model_language_changed(&doc.uri, language_id),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::Arg;

    fn r(sl: u32, sc: u32, el: u32, ec: u32) -> Range {
        Range { start_line_number: sl, start_column: sc, end_line_number: el, end_column: ec }
    }

    fn doc(text: &str) -> DocumentMirror {
        DocumentMirror::new(UriComponents::remote("h", "/w/a.rs"), text, "rust")
    }

    #[test]
    fn single_insert() {
        let mut d = doc("fn main() {\n}\n");
        let ev = d
            .apply(EditorChange { edits: vec![TextEdit { range: r(1, 12, 1, 12), text: "\n    x();".into() }], ..Default::default() })
            .unwrap();
        assert_eq!(d.text(), "fn main() {\n    x();\n}\n");
        assert_eq!(ev.version_id, 2);
        assert_eq!(ev.changes[0].range_offset, 11);
        assert_eq!(ev.changes[0].range_length, 0);
    }

    #[test]
    fn multi_cursor_edits_are_sorted_end_to_start_with_pre_edit_offsets() {
        let mut d = doc("aa\nbb\ncc");
        let ev = d
            .apply(EditorChange {
                edits: vec![
                    TextEdit { range: r(1, 1, 1, 1), text: "X".into() },
                    TextEdit { range: r(3, 1, 3, 1), text: "X".into() },
                    TextEdit { range: r(2, 1, 2, 3), text: "".into() },
                ],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(d.text(), "Xaa\n\nXcc");
        let offs: Vec<(u32, u32)> = ev.changes.iter().map(|c| (c.range_offset, c.range_length)).collect();
        assert_eq!(offs, vec![(6, 0), (3, 2), (0, 0)]);
        assert_eq!(ev.changes[0].range, r(3, 1, 3, 1));
    }

    #[test]
    fn utf16_columns_and_offsets() {
        // '😀' is 2 UTF-16 units, 4 bytes; 'é' is 1 unit, 2 bytes.
        let mut d = doc("é😀z");
        assert_eq!(d.offset_at(1, 4).unwrap(), 3);
        assert!(d.offset_at(1, 3).is_err()); // inside the surrogate pair
        assert_eq!(d.position_from_byte(1, 6).unwrap().column, 4);
        let ev = d
            .apply(EditorChange { edits: vec![TextEdit { range: r(1, 2, 1, 4), text: "!".into() }], ..Default::default() })
            .unwrap();
        assert_eq!(d.text(), "é!z");
        assert_eq!((ev.changes[0].range_offset, ev.changes[0].range_length), (1, 2));
    }

    #[test]
    fn crlf_document_normalizes_inserted_text() {
        let mut d = doc("a\r\nb");
        assert_eq!(d.eol(), "\r\n");
        let ev = d
            .apply(EditorChange { edits: vec![TextEdit { range: r(1, 2, 1, 2), text: "\nx".into() }], ..Default::default() })
            .unwrap();
        assert_eq!(ev.changes[0].text, "\r\nx");
        assert_eq!(d.lines(), &["a".to_string(), "x".into(), "b".into()]);
        assert_eq!(d.offset_at(3, 1).unwrap(), 6);
    }

    #[test]
    fn rejects_overlap_and_stale_version() {
        let mut d = doc("abcdef");
        let overlap = EditorChange {
            edits: vec![
                TextEdit { range: r(1, 1, 1, 4), text: "".into() },
                TextEdit { range: r(1, 3, 1, 5), text: "".into() },
            ],
            ..Default::default()
        };
        assert!(d.apply(overlap).is_err());
        let stale = EditorChange { edits: vec![], version: Some(1), ..Default::default() };
        assert!(d.apply(stale).is_err());
    }

    #[test]
    fn event_json_matches_upstream_shape() {
        let mut d = doc("x");
        let ev = d
            .apply(EditorChange { edits: vec![TextEdit { range: r(1, 2, 1, 2), text: "y".into() }], reason: EditReason::Undo, version: Some(5) })
            .unwrap();
        assert_eq!(
            serde_json::to_value(&ev).unwrap(),
            serde_json::json!({
                "changes": [{"range": {"startLineNumber":1,"startColumn":2,"endLineNumber":1,"endColumn":2}, "rangeOffset":1, "rangeLength":0, "text":"y"}],
                "eol": "\n", "versionId": 5, "isUndoing": true, "isRedoing": false, "isFlush": false, "isEolChange": false
            })
        );
    }

    #[test]
    fn bridge_open_then_change_produces_ordered_calls() {
        let mut b = DocumentBridge::new();
        let uri = UriComponents::remote("h", "/w/a.rs");
        let calls = b.open(uri.clone(), "x\n", "rust");
        assert_eq!(calls[0].method, "$activateByEvent");
        assert_eq!(calls[0].args[0], Arg::Json("onLanguage:rust".into()));
        assert_eq!(calls[1].method, "$acceptDocumentsAndEditorsDelta");
        let added = calls[1].args[0].as_json().unwrap();
        assert_eq!(added["addedDocuments"][0]["lines"], serde_json::json!(["x", ""]));
        assert_eq!(added["addedDocuments"][0]["EOL"], "\n");
        assert_eq!(added["addedDocuments"][0]["versionId"], 1);

        let c = b
            .change(&uri, EditorChange { edits: vec![TextEdit { range: r(2, 1, 2, 1), text: "y".into() }], ..Default::default() })
            .unwrap();
        assert_eq!((c.proxy, c.method), ("ExtHostDocuments", "$acceptModelChanged"));
        assert_eq!(c.args[2], Arg::Json(true.into()));
    }

    #[test]
    fn replace_all_is_flush() {
        let mut d = doc("a\nbc");
        let ev = d.replace_all("z").unwrap();
        assert!(ev.is_flush);
        assert_eq!(ev.changes[0].range, r(1, 1, 2, 3));
        assert_eq!((ev.changes[0].range_offset, ev.changes[0].range_length), (0, 4));
        assert_eq!(d.text(), "z");
    }
}
