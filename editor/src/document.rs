//! One open document: widget versions ↔ extension-host versions, the extension host's mirror,
//! and the save / reload state.
//!
//! **Versions.** The widget numbers versions from 0 at every `Text` it is given and adds 1 per
//! `CodeChanged` (FR-38 §38.1). The extension host needs one strictly increasing `versionId` per
//! URI for as long as the document is open (`ExtHostDocumentData`, and every language server's
//! `textDocument/didChange`). So each document keeps `ext_base`, the extension-host version at
//! widget version 0, and sends widget version `v` as `ext_base + v`.
//!
//! **Reset** (the host replaces the whole text, e.g. the file changed on disk): the widget restarts
//! at 0 under a new node key (`generation`), and the extension host is told *remove the document,
//! then add it again* with `versionId = last + 1`, so versions stay monotonic and extensions see
//! `onDidCloseTextDocument` + `onDidOpenTextDocument` for content they never saw being typed.
//!
//! **Text form.** The widget gets the text joined with `\n`; the mirror keeps the file's own EOL
//! (`\n` or `\r\n`, normalized like Monaco) for the extension host and for saving. A UTF-8 BOM is
//! stripped on open and written back on save. Files that are not UTF-8 are refused (no encoding
//! guessing yet).

use ember_editor_conn::document::{DocumentMirror, EditReason, EditorChange, TextEdit};
use ember_editor_conn::exthost::{
    self, Call, DocumentsAndEditorsDelta, Selection, TextEditorAddData,
};
use ember_editor_conn::uri::UriComponents;

use crate::coords::{end_of_insert, WidgetPos, WidgetRange};

const BOM: &str = "\u{feff}";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DocError {
    #[error("file is not UTF-8")]
    NotUtf8,
    /// A `CodeChanged` whose version is not the next one: an event was lost or duplicated.
    #[error("widget version {got}, expected {expected}")]
    VersionGap { expected: u32, got: u32 },
    /// The change did not apply to Ember's copy of the text (range outside the document, …): the
    /// widget and the session disagree about the content.
    #[error("change does not fit the document: {0}")]
    BadChange(String),
}

/// Decode file bytes: strip a UTF-8 BOM, refuse anything that is not UTF-8.
pub fn decode(bytes: &[u8]) -> Result<(String, bool), DocError> {
    let s = std::str::from_utf8(bytes).map_err(|_| DocError::NotUtf8)?;
    match s.strip_prefix(BOM) {
        Some(rest) => Ok((rest.to_owned(), true)),
        None => Ok((s.to_owned(), false)),
    }
}

#[derive(Debug)]
pub struct OpenDocument {
    pub uri: UriComponents,
    pub language_id: String,
    /// `ITextEditorAddData.id` of the one editor showing this document.
    pub editor_id: String,
    mirror: DocumentMirror,
    generation: u64,
    widget_version: u32,
    ext_base: u64,
    bom: bool,
    /// The file content as last read or written, in mirror form (`mirror.text()`), to tell our own
    /// save echoing back through the watcher from a real external change.
    disk_text: String,
    /// Where the user's cursor probably is: the end of the last change. FR-38 has no cursor event,
    /// so inline completions use this.
    cursor: Option<WidgetPos>,
    tab_size: u32,
    insert_spaces: bool,
}

impl OpenDocument {
    /// Open `text` (already decoded) as extension-host version `ext_version`, widget generation
    /// `generation`. Returns the document and the calls that announce it to the extension host:
    /// `$activateByEvent("onLanguage:<id>")` and one `$acceptDocumentsAndEditorsDelta` adding the
    /// document, a visible editor for it, and making that editor active.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        uri: UriComponents,
        text: &str,
        bom: bool,
        language_id: &str,
        editor_id: &str,
        ext_version: u64,
        generation: u64,
        tab_size: u32,
        insert_spaces: bool,
    ) -> (Self, Vec<Call>) {
        let mirror = DocumentMirror::new(uri.clone(), text, language_id).with_version(ext_version);
        let doc = Self {
            uri,
            language_id: language_id.to_owned(),
            editor_id: editor_id.to_owned(),
            disk_text: mirror.text(),
            mirror,
            generation,
            widget_version: 0,
            ext_base: ext_version,
            bom,
            cursor: None,
            tab_size,
            insert_spaces,
        };
        let calls = doc.add_calls();
        (doc, calls)
    }

    fn add_calls(&self) -> Vec<Call> {
        let editor = TextEditorAddData {
            id: self.editor_id.clone(),
            document_uri: self.uri.clone(),
            options: exthost::default_editor_options(self.tab_size, self.insert_spaces),
            selections: vec![Selection {
                selection_start_line_number: 1,
                selection_start_column: 1,
                position_line_number: 1,
                position_column: 1,
            }],
            visible_ranges: vec![self.mirror.full_range()],
            editor_position: Some(0),
        };
        let delta = DocumentsAndEditorsDelta {
            added_documents: Some(vec![self.mirror.added_data()]),
            added_editors: Some(vec![editor]),
            new_active_editor: Some(Some(self.editor_id.clone())),
            ..Default::default()
        };
        vec![
            exthost::activate_by_event(&format!("onLanguage:{}", self.language_id)),
            exthost::accept_documents_and_editors_delta(&delta),
        ]
    }

    /// The delta that removes this document and its editor.
    pub fn remove_call(&self) -> Call {
        let delta = DocumentsAndEditorsDelta {
            removed_documents: Some(vec![self.uri.clone()]),
            removed_editors: Some(vec![self.editor_id.clone()]),
            ..Default::default()
        };
        exthost::accept_documents_and_editors_delta(&delta)
    }

    /// Make this document's editor the active one.
    pub fn focus_call(&self) -> Call {
        let delta = DocumentsAndEditorsDelta { new_active_editor: Some(Some(self.editor_id.clone())), ..Default::default() };
        exthost::accept_documents_and_editors_delta(&delta)
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn widget_version(&self) -> u32 {
        self.widget_version
    }
    /// The extension host's current `versionId` for this document.
    pub fn ext_version(&self) -> u64 {
        self.ext_base + self.widget_version as u64
    }
    pub fn is_dirty(&self) -> bool {
        self.mirror.is_dirty()
    }
    pub fn cursor(&self) -> Option<WidgetPos> {
        self.cursor
    }
    pub fn set_cursor(&mut self, pos: WidgetPos) {
        self.cursor = Some(pos);
    }
    /// Lines (no terminators), shared by the widget and the extension host.
    pub fn lines(&self) -> &[String] {
        self.mirror.lines()
    }
    pub fn line(&self, line: u32) -> Option<&str> {
        self.mirror.lines().get(line as usize).map(String::as_str)
    }
    /// The text as the widget holds it (`\n` line breaks).
    pub fn widget_text(&self) -> String {
        self.mirror.lines().join("\n")
    }
    /// The bytes to write on save: file EOL, BOM if the file had one.
    pub fn save_bytes(&self) -> Vec<u8> {
        let mut s = String::new();
        if self.bom {
            s.push_str(BOM);
        }
        s.push_str(&self.mirror.text());
        s.into_bytes()
    }

    /// Apply one `CodeChanged` and return the `$acceptModelChanged` call. Updates the cursor
    /// estimate to the end of the inserted text.
    pub fn apply_widget_change(&mut self, version: u32, range: WidgetRange, text: &str) -> Result<Call, DocError> {
        let expected = self.widget_version + 1;
        if version != expected {
            return Err(DocError::VersionGap { expected, got: version });
        }
        let change = EditorChange {
            edits: vec![TextEdit { range: range.to_exthost(), text: text.to_owned() }],
            reason: EditReason::Normal,
            version: Some(self.ext_base + version as u64),
        };
        let ev = self.mirror.apply(change).map_err(|e| DocError::BadChange(e.to_string()))?;
        self.widget_version = version;
        self.cursor = Some(end_of_insert(range.start, text));
        Ok(exthost::accept_model_changed(&self.uri, &ev, self.mirror.is_dirty()))
    }

    /// Replace the whole content (file changed on disk): bump the generation, restart the widget
    /// at version 0, and re-announce the document to the extension host one version later.
    /// Returns the calls: remove (document + editor), then add (document + editor + active).
    pub fn reset(&mut self, text: &str, bom: bool, generation: u64) -> Vec<Call> {
        let next = self.ext_version() + 1;
        let mut calls = vec![self.remove_call()];
        self.mirror = DocumentMirror::new(self.uri.clone(), text, &self.language_id).with_version(next);
        self.ext_base = next;
        self.widget_version = 0;
        self.generation = generation;
        self.bom = bom;
        self.disk_text = self.mirror.text();
        self.cursor = None;
        calls.extend(self.add_calls());
        calls
    }

    /// Does `text` (decoded file content) equal what Ember last read or wrote? Used to ignore
    /// watcher events caused by our own save.
    pub fn matches_disk(&self, text: &str) -> bool {
        DocumentMirror::new(self.uri.clone(), text, &self.language_id).text() == self.disk_text
    }

    /// Does `text` equal the current buffer (so a reload would change nothing)?
    pub fn matches_buffer(&self, text: &str) -> bool {
        DocumentMirror::new(self.uri.clone(), text, &self.language_id).text() == self.mirror.text()
    }

    /// A save of the content at extension-host version `saved_ext_version` (with `bytes`)
    /// completed. Returns the calls: always `$acceptModelSaved` (fires `onDidSaveTextDocument`),
    /// and `$acceptDirtyStateChanged(true)` when the user typed while the write was in flight.
    pub fn saved(&mut self, saved_ext_version: u64, bytes: &[u8]) -> Vec<Call> {
        if let Ok((text, _)) = decode(bytes) {
            self.disk_text = DocumentMirror::new(self.uri.clone(), &text, &self.language_id).text();
        }
        let mut calls = vec![exthost::accept_model_saved(&self.uri)];
        if saved_ext_version == self.ext_version() {
            self.mirror.mark_saved();
        } else {
            calls.push(exthost::accept_dirty_state_changed(&self.uri, true));
        }
        calls
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_editor_conn::rpc::Arg;
    use serde_json::json;

    fn p(line: u32, col: u32) -> WidgetPos {
        WidgetPos::new(line, col)
    }

    fn open(text: &str) -> (OpenDocument, Vec<Call>) {
        OpenDocument::open(UriComponents::remote("h", "/w/a.json"), text, false, "json", "e1", 1, 1, 4, true)
    }

    fn json_arg(c: &Call, i: usize) -> serde_json::Value {
        c.args[i].as_json().cloned().unwrap()
    }

    #[test]
    fn open_announces_document_editor_and_activation() {
        let (d, calls) = open("{}\n");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].args[0], Arg::Json("onLanguage:json".into()));
        let delta = json_arg(&calls[1], 0);
        assert_eq!(delta["addedDocuments"][0]["versionId"], 1);
        assert_eq!(delta["addedEditors"][0]["id"], "e1");
        assert_eq!(delta["newActiveEditor"], "e1");
        assert_eq!(d.widget_version(), 0);
        assert_eq!(d.ext_version(), 1);
    }

    #[test]
    fn widget_zero_based_change_becomes_one_based_model_change() {
        let (mut d, _) = open("{\n  \"ok\": true\n}\n");
        // Insert ",," after `true` (line 1, UTF-16 col 12 in the widget).
        let c = d.apply_widget_change(1, WidgetRange::empty(p(1, 12)), ",,").unwrap();
        assert_eq!((c.proxy, c.method), ("ExtHostDocuments", "$acceptModelChanged"));
        let ev = json_arg(&c, 1);
        assert_eq!(ev["versionId"], 2);
        assert_eq!(ev["changes"][0]["range"], json!({"startLineNumber": 2, "startColumn": 13, "endLineNumber": 2, "endColumn": 13}));
        assert_eq!(ev["changes"][0]["rangeOffset"], 14);
        assert_eq!(c.args[2], Arg::Json(true.into()));
        assert_eq!(d.widget_text(), "{\n  \"ok\": true,,\n}\n");
        assert_eq!(d.cursor(), Some(p(1, 14)));
    }

    #[test]
    fn utf16_columns_pass_through_unchanged() {
        let (mut d, _) = open("😀x");
        let c = d.apply_widget_change(1, WidgetRange::new(p(0, 2), p(0, 3)), "y").unwrap();
        let ev = json_arg(&c, 1);
        assert_eq!(ev["changes"][0]["range"]["startColumn"], 3);
        assert_eq!(ev["changes"][0]["rangeOffset"], 2);
        assert_eq!(d.widget_text(), "😀y");
        // Splitting the surrogate pair is refused, not silently mangled.
        assert!(matches!(d.apply_widget_change(2, WidgetRange::empty(p(0, 1)), "z"), Err(DocError::BadChange(_))));
    }

    #[test]
    fn version_gaps_are_detected() {
        let (mut d, _) = open("a");
        assert_eq!(
            d.apply_widget_change(2, WidgetRange::empty(p(0, 0)), "x").unwrap_err(),
            DocError::VersionGap { expected: 1, got: 2 }
        );
        d.apply_widget_change(1, WidgetRange::empty(p(0, 0)), "x").unwrap();
        assert!(d.apply_widget_change(1, WidgetRange::empty(p(0, 0)), "x").is_err());
    }

    #[test]
    fn reset_restarts_widget_versions_and_keeps_exthost_versions_monotonic() {
        let (mut d, _) = open("a");
        d.apply_widget_change(1, WidgetRange::empty(p(0, 1)), "b").unwrap();
        d.apply_widget_change(2, WidgetRange::empty(p(0, 2)), "c").unwrap();
        assert_eq!(d.ext_version(), 3);

        let calls = d.reset("fresh\r\ncontent", false, 2);
        assert_eq!(d.generation(), 2);
        assert_eq!(d.widget_version(), 0);
        assert_eq!(d.ext_version(), 4);
        // remove, then activation + add
        let removed = json_arg(&calls[0], 0);
        assert_eq!(removed["removedDocuments"][0]["path"], "/w/a.json");
        assert_eq!(removed["removedEditors"][0], "e1");
        let added = json_arg(&calls[2], 0);
        assert_eq!(added["addedDocuments"][0]["versionId"], 4);
        assert_eq!(added["addedDocuments"][0]["EOL"], "\r\n");
        assert_eq!(d.widget_text(), "fresh\ncontent");

        // The widget's next change is version 1 again → extension-host version 5.
        let c = d.apply_widget_change(1, WidgetRange::empty(p(0, 0)), "!").unwrap();
        assert_eq!(json_arg(&c, 1)["versionId"], 5);
        assert_eq!(d.save_bytes(), b"!fresh\r\ncontent".to_vec());
    }

    #[test]
    fn bom_and_encoding() {
        assert_eq!(decode(b"\xef\xbb\xbfhi").unwrap(), ("hi".to_string(), true));
        assert_eq!(decode(b"hi").unwrap(), ("hi".to_string(), false));
        assert_eq!(decode(b"\xff\xfe").unwrap_err(), DocError::NotUtf8);
        let (d, _) = OpenDocument::open(UriComponents::remote("h", "/w/a"), "hi", true, "plaintext", "e", 1, 1, 4, true);
        assert_eq!(d.save_bytes(), b"\xef\xbb\xbfhi".to_vec());
    }

    #[test]
    fn save_marks_clean_only_if_nothing_changed_meanwhile() {
        let (mut d, _) = open("a");
        d.apply_widget_change(1, WidgetRange::empty(p(0, 1)), "b").unwrap();
        assert!(d.is_dirty());
        let v = d.ext_version();
        let bytes = d.save_bytes();
        let calls = d.saved(v, &bytes);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, "$acceptModelSaved");
        assert!(!d.is_dirty());
        assert!(d.matches_disk("ab"));

        // Typing while the write is in flight: still dirty afterwards.
        let v = d.ext_version();
        let bytes = d.save_bytes();
        d.apply_widget_change(2, WidgetRange::empty(p(0, 2)), "c").unwrap();
        let calls = d.saved(v, &bytes);
        assert_eq!(calls[1].method, "$acceptDirtyStateChanged");
        assert!(d.is_dirty());
        assert!(d.matches_disk("ab"));
        assert!(!d.matches_disk("abc"));
        assert!(d.matches_buffer("abc"));
    }
}
