//! The dioxus-compose `CodeEditor` contract (compose-rust `docs/SPEC.md` FR-38, branch
//! `integration/ember-m3` at `7ac0ff4`), as plain Rust data.
//!
//! This crate does not depend on dioxus-compose. The UI layer translates the widget's events into
//! [`WidgetEvent`] and applies [`WidgetCommand`] / [`Decoration`] lists to the widget's props and
//! commands; everything between those two edges is here and testable without a renderer.
//!
//! | FR-38 wire | here |
//! | ---------- | ---- |
//! | event 26 `CodeChanged(version, range, text)` | [`WidgetEvent::CodeChanged`] |
//! | event 27 `CodeEditRejected(request_id, base, current, range)` | [`WidgetEvent::CodeEditRejected`] |
//! | event 28 `CodeHovered(decoration, line, column, phase)` | [`WidgetEvent::CodeHovered`] |
//! | event 29 `CodeSaveRequested(version)` | [`WidgetEvent::CodeSaveRequested`] |
//! | event 30 `DecorationActivated(decoration)` | [`WidgetEvent::DecorationActivated`] |
//! | event `SelectionChange(version, anchor, active)` | [`WidgetEvent::SelectionChanged`] |
//! | event `VisibleRange(version, first_line, last_line)` | [`WidgetEvent::VisibleRangeChanged`] |
//! | command 17 `EditCode(request_id, base_version, range, text)` | [`WidgetCommand::EditCode`] |
//! | prop 1 `Text` + node key | [`WidgetCommand::SetText`] |
//! | prop 100 `Decorations` (44-byte records + text blob) | `Vec<`[`Decoration`]`>` / [`Decoration::encode_records`] |
//! | prop 102 `TabWidth` | `SessionUpdate::Opened::tab_width` |
//!
//! Versions (§38.1): the `Text` the host sets is version 0; every committed change adds 1. A
//! *different* `Text` replaces the buffer and restarts at 0; an *identical* `Text` is ignored, so
//! to restart with the same content the host must change the node key. The session therefore
//! gives every document a `generation` that the UI uses as the node key and passes back with
//! every event; events from an older generation are dropped.

use crate::coords::{WidgetPos, WidgetRange};

/// Bytes of one FR-38 `DecorationRecord` (`DECORATION_LEN` in `compose-rust`).
pub const DECORATION_RECORD_LEN: usize = 44;

/// Underline severity (decoration record `severity: u16`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum Severity {
    Error = 1,
    Warning = 2,
    Information = 3,
    Hint = 4,
}

impl Severity {
    /// From `MarkerSeverity` (1 Hint, 2 Info, 4 Warning, 8 Error). Unknown values are hints.
    pub fn from_marker(sev: u8) -> Self {
        match sev {
            8 => Self::Error,
            4 => Self::Warning,
            2 => Self::Information,
            _ => Self::Hint,
        }
    }

    pub fn from_wire(v: u16) -> Option<Self> {
        Some(match v {
            1 => Self::Error,
            2 => Self::Warning,
            3 => Self::Information,
            4 => Self::Hint,
            _ => return None,
        })
    }

    pub fn wire(self) -> u16 {
        self as u16
    }
}

/// Decoration record `kind: u16`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum DecorationKind {
    Underline = 1,
    CodeLens = 2,
    HoverAnchor = 3,
    GhostText = 4,
}

impl DecorationKind {
    pub fn from_wire(v: u16) -> Option<Self> {
        Some(match v {
            1 => Self::Underline,
            2 => Self::CodeLens,
            3 => Self::HoverAnchor,
            4 => Self::GhostText,
            _ => return None,
        })
    }

    pub fn wire(self) -> u16 {
        self as u16
    }
}

/// A decoration id (record `id: u64`). Never 0 (0 means "not clickable, not reported").
/// See [`crate::ids`] for how ids are made stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DecorationId(pub u64);

impl DecorationId {
    pub fn get(self) -> u64 {
        self.0
    }
}

/// One entry of the `Decorations` prop (44-byte record + text in the blob).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoration {
    pub id: DecorationId,
    /// The widget version the range refers to. The widget moves the range through later edits.
    pub version: u32,
    pub kind: DecorationKind,
    /// Only for [`DecorationKind::Underline`]; `None` is wire `0`.
    pub severity: Option<Severity>,
    /// `ColorRole`; 0 lets the design system choose. The session always sends 0.
    pub color: u16,
    pub range: WidgetRange,
    /// CodeLens title or ghost text; empty otherwise.
    pub text: String,
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_range(out: &mut Vec<u8>, range: WidgetRange) {
    put_u32(out, range.start.line);
    put_u32(out, range.start.col);
    put_u32(out, range.end.line);
    put_u32(out, range.end.col);
}

fn word(record: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([record[at], record[at + 1], record[at + 2], record[at + 3]])
}

fn half(record: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([record[at], record[at + 1]])
}

fn long(record: &[u8], at: usize) -> u64 {
    u64::from(word(record, at)) | (u64::from(word(record, at + 4)) << 32)
}

fn range_at(record: &[u8], at: usize) -> WidgetRange {
    WidgetRange::new(
        WidgetPos::new(word(record, at), word(record, at + 4)),
        WidgetPos::new(word(record, at + 8), word(record, at + 12)),
    )
}

impl Decoration {
    /// Encode decorations into the FR-38 `Decorations` prop binary wire format:
    /// fixed 44-byte little-endian records followed by the concatenated UTF-8 text region.
    /// Matches `compose_rust::code::Decorations::new` byte-for-byte.
    pub fn encode_records(decorations: &[Decoration]) -> Vec<u8> {
        let records_len = decorations.len() * DECORATION_RECORD_LEN;
        let mut bytes = Vec::with_capacity(records_len);
        let mut texts = Vec::new();
        for decoration in decorations {
            put_u32(&mut bytes, decoration.version);
            put_u16(&mut bytes, decoration.kind.wire());
            put_u16(&mut bytes, decoration.severity.map_or(0, Severity::wire));
            put_u16(&mut bytes, decoration.color);
            put_u16(&mut bytes, 0);
            put_range(&mut bytes, decoration.range);
            put_u64(&mut bytes, decoration.id.0);
            // An empty text points at the end of the records, which is where the text
            // region starts, so a reader never sees an offset into the records themselves.
            put_u32(&mut bytes, (records_len + texts.len()) as u32);
            put_u32(&mut bytes, decoration.text.len() as u32);
            texts.extend_from_slice(decoration.text.as_bytes());
        }
        bytes.extend_from_slice(&texts);
        bytes
    }

    /// Decode an FR-38 `Decorations` wire blob back into [`Decoration`] records.
    /// Matches `compose_rust::code::Decorations::decorations()`.
    pub fn decode_records(bytes: &[u8]) -> Vec<Decoration> {
        let mut end = bytes.len();
        let mut at = 0;
        let mut found = Vec::new();
        while at + DECORATION_RECORD_LEN <= end {
            let record = &bytes[at..at + DECORATION_RECORD_LEN];
            at += DECORATION_RECORD_LEN;
            let text_offset = word(record, 36) as usize;
            let text_len = word(record, 40) as usize;
            if text_len > 0 {
                end = end.min(text_offset);
            }
            let Some(kind) = DecorationKind::from_wire(half(record, 4)) else {
                continue;
            };
            let severity = match half(record, 6) {
                0 => None,
                tag => match Severity::from_wire(tag) {
                    Some(sev) => Some(sev),
                    None => continue,
                },
            };
            let color = half(record, 8);
            let text = if text_len == 0 {
                String::new()
            } else {
                match bytes
                    .get(text_offset..text_offset + text_len)
                    .and_then(|run| std::str::from_utf8(run).ok())
                {
                    Some(text) => text.to_owned(),
                    None => continue,
                }
            };
            found.push(Decoration {
                id: DecorationId(long(record, 28)),
                version: word(record, 0),
                kind,
                severity,
                color,
                range: range_at(record, 12),
                text,
            });
        }
        found
    }
}

/// `CodeHovered.phase`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum HoverPhase {
    Rest = 1,
    Leave = 2,
}

/// Events the widget sends, already decoded by the UI layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WidgetEvent {
    /// One committed change. `version` is the version *after* the change; `range` is in the
    /// document before it; `text` replaces it. Several ranges in one user action arrive as several
    /// events, each one version higher.
    CodeChanged { version: u32, range: WidgetRange, text: String },
    /// An [`WidgetCommand::EditCode`] was not applied because the user had edited the same place.
    CodeEditRejected { request_id: u32, base_version: u32, current_version: u32, range: WidgetRange },
    /// The pointer rested at (or left) a position. `decoration` is the id of a hover-anchor
    /// decoration under it, or 0.
    CodeHovered { decoration: u64, pos: WidgetPos, phase: HoverPhase },
    /// Cmd/Ctrl+S.
    CodeSaveRequested { version: u32 },
    /// A CodeLens was clicked, or ghost text was accepted (Tab). For ghost text the widget has
    /// already inserted the text and sent its `CodeChanged` first, in the same frame (§38.4).
    DecorationActivated { decoration: u64 },
    /// The selection or caret moved (`compose_rust::code::SelectionChange`).
    /// `anchor` is where the selection started and `active` is where the caret is.
    SelectionChanged { version: u32, anchor: WidgetPos, active: WidgetPos },
    /// The lines visible on screen changed (`compose_rust::code::VisibleRange`).
    VisibleRangeChanged { version: u32, first_line: u32, last_line: u32 },
}

/// What the session asks the UI to do to the widget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WidgetCommand {
    /// Replace the whole document: render a new `CodeEditor` node keyed by `generation` with this
    /// `Text`. Version restarts at 0.
    SetText { generation: u64, text: String },
    /// FR-38 command 17. `base_version` is the widget version the range refers to; the widget
    /// rebases it over later user edits or answers with `CodeEditRejected`. The edit comes back as
    /// a normal `CodeChanged`.
    EditCode { request_id: u32, base_version: u32, range: WidgetRange, text: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_severity_maps_to_widget_severity() {
        assert_eq!(Severity::from_marker(8), Severity::Error);
        assert_eq!(Severity::from_marker(4), Severity::Warning);
        assert_eq!(Severity::from_marker(2), Severity::Information);
        assert_eq!(Severity::from_marker(1), Severity::Hint);
        assert_eq!(Severity::Error.wire(), 1);
        assert_eq!(Severity::Hint.wire(), 4);
        assert_eq!(DecorationKind::GhostText.wire(), 4);
    }
}
