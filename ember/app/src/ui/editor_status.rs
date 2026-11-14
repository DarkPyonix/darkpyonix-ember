//! The status bar's document items and their pickers in the Ember 에디터 window (FR-L31.7, #453):
//! line ending, encoding and language mode, as Code-OSS's `editorStatus.ts` (`ChangeEOLAction`,
//! `ChangeEncodingAction`, `ChangeLanguageAction`). Pure; the window draws the items in the status
//! bar and the pickers in the palette sheet (`editor_workbench`).

use ember_editor::{DocumentFormat, Encoding, EncodingMode, Eol, LanguageInfo};

use super::palette::{Item, Mode, Payload};

/// What the status bar shows for an encoding it does not know.
pub const UNKNOWN: &str = "알 수 없음";

/// The status bar's texts for the active document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusTexts {
    pub eol: String,
    pub encoding: String,
    pub language: String,
}

/// The three texts: "LF" or "CRLF", the encoding's Code-OSS label, and the language's name. Before the
/// session has reported the document's format, the EOL is LF (the default), and the encoding and the
/// language say they are not known.
pub fn status_texts(format: Option<&DocumentFormat>) -> StatusTexts {
    match format {
        Some(f) => StatusTexts {
            eol: f.eol.label().to_string(),
            encoding: f.encoding.label().to_string(),
            language: if f.language_name.is_empty() { f.language_id.clone() } else { f.language_name.clone() },
        },
        None => StatusTexts { eol: Eol::Lf.label().to_string(), encoding: UNKNOWN.to_string(), language: UNKNOWN.to_string() },
    }
}

/// The encoding label for a detection result: `None` is unknown.
pub fn encoding_text(encoding: Option<Encoding>) -> &'static str {
    encoding.map_or(UNKNOWN, Encoding::label)
}

/// Does the palette show one of this module's pickers in `mode`?
pub fn is_picker(mode: Mode) -> bool {
    matches!(mode, Mode::Eol | Mode::EncodingAction | Mode::Encoding(_) | Mode::ConfirmReopen(_) | Mode::Language)
}

fn item(payload: Payload, title: &str, hint: String) -> Item {
    Item { payload, title: title.to_string(), hint, enabled: true }
}

/// The label of 자동 감지 in the language picker (Code-OSS "Auto Detect").
pub const AUTO_DETECT: &str = "자동 감지";
/// The two actions of the encoding picker (Code-OSS "Reopen with Encoding", "Save with Encoding").
pub const REOPEN_WITH_ENCODING: &str = "인코딩으로 다시 열기";
pub const SAVE_WITH_ENCODING: &str = "인코딩으로 저장";
/// The choices when a reopen would discard unsaved edits (Code-OSS asks the same before it reverts).
pub const DISCARD_AND_REOPEN: &str = "변경 사항을 버리고 다시 열기";
pub const CANCEL: &str = "취소";

/// The rows of a picker. `format` is the active document's; `languages` the session's list.
pub fn picker_items(mode: Mode, format: Option<&DocumentFormat>, languages: &[LanguageInfo]) -> Vec<Item> {
    match mode {
        Mode::Eol => Eol::ALL
            .into_iter()
            .map(|e| item(Payload::SetEol(e), e.label(), current_hint(format.is_some_and(|f| f.eol == e), String::new())))
            .collect(),
        Mode::EncodingAction => vec![
            item(Payload::EncodingAction(EncodingMode::Reopen), REOPEN_WITH_ENCODING, String::new()),
            item(Payload::EncodingAction(EncodingMode::Save), SAVE_WITH_ENCODING, String::new()),
        ],
        Mode::Encoding(mode) => Encoding::ALL
            .into_iter()
            .map(|e| {
                let current = format.is_some_and(|f| f.encoding == e);
                item(Payload::SetEncoding { mode, encoding: e }, e.label(), current_hint(current, e.id().to_string()))
            })
            .collect(),
        Mode::ConfirmReopen(encoding) => vec![
            item(Payload::SetEncoding { mode: EncodingMode::Reopen, encoding }, DISCARD_AND_REOPEN, String::new()),
            item(Payload::Cancel, CANCEL, String::new()),
        ],
        Mode::Language => {
            let current = format.map(|f| f.language_id.as_str());
            let mut items = vec![item(Payload::SetLanguage(None), AUTO_DETECT, String::new())];
            items.extend(languages.iter().map(|l| {
                let hint = current_hint(current == Some(l.id.as_str()), format!("({})", l.id));
                item(Payload::SetLanguage(Some(l.id.clone())), &l.name, hint)
            }));
            items
        }
        _ => Vec::new(),
    }
}

fn current_hint(current: bool, rest: String) -> String {
    match (current, rest.is_empty()) {
        (true, true) => "현재".to_string(),
        (true, false) => format!("{rest} - 현재"),
        (false, _) => rest,
    }
}

/// How many picker rows the palette's list shows at once (320 px of 34 px rows).
pub const PICKER_VISIBLE_ROWS: usize = 9;

/// The rows of a picker list of `len` rows drawn in view so the highlighted row `selected` can be
/// read: `(start, end)`, rows `start..end` fit under the sheet's `visible` row budget. When rows above
/// are hidden (`start > 0`) the "위에 N개 더 있습니다" row takes one row of that budget, and the
/// highlighted row keeps one row of context below it unless it is the last one (Code-OSS reveals a
/// quick pick's active item the same way). The rows after `end` are still drawn and scroll into view.
/// The toolkit has no scroll-into-view (TODO(dioxus-compose#200)), so the list is windowed instead.
pub fn list_window(selected: usize, len: usize, visible: usize) -> (usize, usize) {
    let visible = visible.max(3);
    if len <= visible {
        return (0, len);
    }
    let selected = selected.min(len - 1);
    // The first page holds the row and the one below it: no row above is hidden.
    if selected + 2 <= visible {
        return (0, visible);
    }
    // One row goes to "위에 N개"; the selection sits on the second to last of the rest.
    let rows = visible - 1;
    let start = (selected + 2 - rows).min(len - rows);
    (start, start + rows)
}

/// The row count the "위에 N개 더 있습니다" row takes from the budget for a window starting at `start`.
pub fn overflow_rows(start: usize) -> usize {
    usize::from(start > 0)
}

/// The value of the palette's highlight when a picker opens: follow the current entry until the
/// person moves the highlight or types, so a list that grows after the sheet opened (the languages
/// arrive with the session) still opens on the current entry.
pub const FOLLOW_CURRENT: usize = usize::MAX;

/// The highlighted row for the stored highlight `stored`: the current entry of the rows as they are
/// now while it is [`FOLLOW_CURRENT`], else the stored row (clamped to the list).
pub fn resolve_selection(stored: usize, mode: Mode, format: Option<&DocumentFormat>, languages: &[LanguageInfo], len: usize) -> usize {
    let row = if stored == FOLLOW_CURRENT { initial_selection(mode, format, languages) } else { stored };
    row.min(len.saturating_sub(1))
}

/// The row above a windowed picker list that leads back up.
pub fn more_above_text(hidden: usize) -> String {
    format!("위에 {hidden}개 더 있습니다")
}

/// The row highlighted when a picker opens: the current EOL, encoding or language (Code-OSS
/// `activeItem`), and 취소 when a reopen would discard edits.
pub fn initial_selection(mode: Mode, format: Option<&DocumentFormat>, languages: &[LanguageInfo]) -> usize {
    let items = picker_items(mode, format, languages);
    let found = items.iter().position(|i| match (&i.payload, format) {
        (Payload::SetEol(e), Some(f)) => *e == f.eol,
        (Payload::SetEncoding { encoding, .. }, Some(f)) if matches!(mode, Mode::Encoding(_)) => *encoding == f.encoding,
        (Payload::SetLanguage(Some(id)), Some(f)) => *id == f.language_id,
        (Payload::Cancel, _) => true,
        _ => false,
    });
    found.unwrap_or(0)
}

/// After a reopen encoding is chosen: ask first when the document has unsaved edits and the encoding
/// is a new one (a reopen in the current encoding does nothing, so it discards nothing).
pub fn reopen_needs_confirmation(dirty: bool, format: Option<&DocumentFormat>, chosen: Encoding) -> bool {
    dirty && format.is_none_or(|f| f.encoding != chosen)
}

/// The notice when reopening with an encoding failed.
pub fn reopen_failure_text(file: &str, encoding: Option<Encoding>, error: &str) -> String {
    let e = error.to_lowercase();
    let reason = if e.contains("not valid") || e.contains("not utf-8") {
        "파일 내용이 이 인코딩에 맞지 않습니다"
    } else {
        super::editor::file_failure_reason(error)
    };
    match encoding {
        Some(enc) => format!("{file}을(를) {}(으)로 다시 열지 못했습니다: {reason}", enc.label()),
        None => format!("{file}을(를) 다시 열지 못했습니다: {reason}"),
    }
}
