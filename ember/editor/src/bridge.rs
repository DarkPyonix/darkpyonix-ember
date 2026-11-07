//! FR-38 `CodeEditor` widget bridge: translates [`SessionUpdate`]s from [`crate::EditorSession`]
//! / [`crate::SessionCore`] into per-document widget props (including the 44-byte FR-38
//! `Decorations` wire blob) and translates widget callbacks into `(UriComponents, generation,
//! WidgetEvent)`.

use std::collections::HashMap;

use ember_editor_conn::uri::UriComponents;

use crate::codelens::CommandDto;
use crate::coords::{utf16_to_byte, WidgetPos, WidgetRange};
use crate::engine::{ExternalChange, SessionUpdate};
use crate::hover::HoverPopup;
use crate::widget::{Decoration, HoverPhase, WidgetCommand, WidgetEvent};

/// Per-document state and encoded props for a `CodeEditor` widget (FR-38).
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentWidgetState {
    pub uri: UriComponents,
    /// The node key for `CodeEditor`.
    pub generation: u64,
    /// The document version as last edited or reported.
    pub version: u32,
    /// The current text prop / buffer mirror (`\n`-normalized).
    pub text: String,
    pub language_id: String,
    pub tab_width: u32,
    pub dirty: bool,
    pub decorations: Vec<Decoration>,
    /// The 44-byte FR-38 `Decorations` wire blob, encoded whenever `SessionUpdate::Decorations`
    /// arrives or `Opened` / `SetText` resets it.
    pub decoration_bytes: Vec<u8>,
    pub hover: Option<HoverPopup>,
    /// `EditCode` commands queued for the widget.
    pub pending_edits: Vec<WidgetCommand>,
    pub external_change: Option<ExternalChange>,
    /// The coloured runs of the syntax and the document version they were written against.
    pub syntax: SyntaxRuns,
}

/// The coloured runs the session last reported for a document.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyntaxRuns {
    pub version: u32,
    pub runs: Vec<crate::textmate::DocRun>,
}

/// Action the caller should take after [`WidgetBridge::apply`].
#[derive(Debug, Clone, PartialEq)]
pub enum BridgeAction {
    None,
    /// The widget and session disagreed; call `session.resync(uri)` or `core.resync(now, &uri)`.
    Resync(UriComponents),
    /// A workbench command to execute on the UI side.
    RunCommand(CommandDto),
    /// The connection to the server or extension host was lost.
    ConnectionLost(String),
    /// Opening a document failed.
    OpenFailed { uri: UriComponents, error: String },
    /// Saving a document failed.
    SaveFailed { uri: UriComponents, error: String },
    /// An extension notification surfaced to the workbench.
    ExtensionNotice {
        severity: crate::output::MessageSeverity,
        message: String,
        source: Option<String>,
    },
    /// Running an extension command failed.
    CommandFailed { id: String, error: String },
}

impl BridgeAction {
    /// Alias check for [`BridgeAction::Resync`].
    pub fn resync_uri(&self) -> Option<&UriComponents> {
        match self {
            Self::Resync(u) => Some(u),
            _ => None,
        }
    }
}

pub fn apply_text_change(text: &str, range: WidgetRange, insert: &str) -> Option<String> {
    let mut lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
    let sl = range.start.line as usize;
    let el = range.end.line as usize;
    if sl >= lines.len() || el >= lines.len() || sl > el {
        return None;
    }
    let sb = utf16_to_byte(&lines[sl], range.start.col)?;
    let eb = utf16_to_byte(&lines[el], range.end.col)?;
    if sl == el && sb > eb {
        return None;
    }
    let prefix = lines[sl][..sb].to_owned();
    let suffix = lines[el][eb..].to_owned();
    let normalized = insert.replace("\r\n", "\n").replace('\r', "\n");
    let ins_lines: Vec<&str> = normalized.split('\n').collect();
    let mut replacement: Vec<String> = Vec::with_capacity(ins_lines.len());
    if ins_lines.len() == 1 {
        replacement.push(format!("{prefix}{}{suffix}", ins_lines[0]));
    } else {
        replacement.push(format!("{prefix}{}", ins_lines[0]));
        for mid in &ins_lines[1..ins_lines.len() - 1] {
            replacement.push((*mid).to_owned());
        }
        replacement.push(format!("{}{suffix}", ins_lines[ins_lines.len() - 1]));
    }
    lines.splice(sl..=el, replacement);
    Some(lines.join("\n"))
}

/// Bridges [`SessionUpdate`]s from [`crate::EditorSession`] / [`crate::SessionCore`] to the state
/// and encoded props of `CodeEditor` widgets, and attaches the active `generation` to outgoing
/// [`WidgetEvent`]s.
#[derive(Debug, Default)]
pub struct WidgetBridge {
    docs: HashMap<String, DocumentWidgetState>,
    active: Option<String>,
    problems: Vec<crate::diagnostics::Problem>,
    output_channels: Vec<crate::output::OutputChannel>,
    active_output_channel: Option<String>,
    extension_messages: Vec<crate::output::ExtensionMessage>,
}

impl WidgetBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// The problems list as the session last reported it.
    pub fn problems(&self) -> &[crate::diagnostics::Problem] {
        &self.problems
    }

    /// Every output channel reported by the session.
    pub fn output_channels(&self) -> &[crate::output::OutputChannel] {
        &self.output_channels
    }

    /// The id of the selected output channel.
    pub fn active_output_channel(&self) -> Option<&str> {
        self.active_output_channel.as_deref()
    }

    pub fn set_active_output_channel(&mut self, id: impl Into<String>) {
        self.active_output_channel = Some(id.into());
    }

    pub fn clear_output_channel(&mut self, id: &str) {
        if let Some(ch) = self.output_channels.iter_mut().find(|c| c.id == id) {
            ch.clear();
        }
    }

    /// Recent extension messages reported by the session.
    pub fn extension_messages(&self) -> &[crate::output::ExtensionMessage] {
        &self.extension_messages
    }

    /// Apply one [`SessionUpdate`] and return any [`BridgeAction`] the UI layer must handle.
    pub fn apply(&mut self, update: SessionUpdate) -> BridgeAction {
        match update {
            SessionUpdate::Opened { uri, generation, text, language_id, tab_width } => {
                let key = uri.key();
                self.docs.insert(
                    key.clone(),
                    DocumentWidgetState {
                        uri,
                        generation,
                        version: 0,
                        text,
                        language_id,
                        tab_width,
                        dirty: false,
                        decorations: Vec::new(),
                        decoration_bytes: Vec::new(),
                        hover: None,
                        pending_edits: Vec::new(),
                        external_change: None,
                        syntax: SyntaxRuns::default(),
                    },
                );
                self.active = Some(key);
                BridgeAction::None
            }
            SessionUpdate::OpenFailed { uri, error } => BridgeAction::OpenFailed { uri, error },
            SessionUpdate::Command { uri, command } => {
                if let Some(doc) = self.docs.get_mut(&uri.key()) {
                    match command {
                        WidgetCommand::SetText { generation, text } => {
                            doc.generation = generation;
                            doc.version = 0;
                            doc.text = text;
                            doc.decorations.clear();
                            doc.decoration_bytes.clear();
                            doc.hover = None;
                            doc.pending_edits.clear();
                            doc.external_change = None;
                            doc.syntax = SyntaxRuns::default();
                        }
                         edit @ WidgetCommand::EditCode { .. } => {
                            doc.pending_edits.push(edit);
                        }
                    }
                }
                BridgeAction::None
            }
            SessionUpdate::Decorations { uri, generation, decorations } => {
                if let Some(doc) = self.docs.get_mut(&uri.key()) {
                    if doc.generation == generation {
                        doc.decoration_bytes = Decoration::encode_records(&decorations);
                        doc.decorations = decorations;
                    }
                }
                BridgeAction::None
            }
            SessionUpdate::Syntax { uri, generation, version, runs, .. } => {
                if let Some(doc) = self.docs.get_mut(&uri.key()) {
                    if doc.generation == generation {
                        doc.version = doc.version.max(version);
                        doc.syntax = SyntaxRuns { version, runs };
                    }
                }
                BridgeAction::None
            }
            SessionUpdate::Hover(popup) => {
                if let Some(doc) = self.docs.get_mut(&popup.doc) {
                    if doc.generation == popup.generation {
                        doc.hover = Some(popup);
                    }
                }
                BridgeAction::None
            }
            SessionUpdate::HoverHidden { uri } => {
                if let Some(doc) = self.docs.get_mut(&uri.key()) {
                    doc.hover = None;
                }
                BridgeAction::None
            }
            SessionUpdate::Dirty { uri, dirty } => {
                if let Some(doc) = self.docs.get_mut(&uri.key()) {
                    doc.dirty = dirty;
                }
                BridgeAction::None
            }
            SessionUpdate::Saved { uri } => {
                if let Some(doc) = self.docs.get_mut(&uri.key()) {
                    doc.external_change = None;
                }
                BridgeAction::None
            }
            SessionUpdate::SaveFailed { uri, error } => BridgeAction::SaveFailed { uri, error },
            SessionUpdate::CommandFailed { id, error } => BridgeAction::CommandFailed { id, error },
            SessionUpdate::ExternalChange { uri, change } => {
                if let Some(doc) = self.docs.get_mut(&uri.key()) {
                    doc.external_change = Some(change);
                }
                BridgeAction::None
            }
            SessionUpdate::RunCommand { command } => BridgeAction::RunCommand(command),
            SessionUpdate::Desync { uri, .. } => BridgeAction::Resync(uri),
            SessionUpdate::Closed { uri } => {
                let key = uri.key();
                self.docs.remove(&key);
                self.problems.retain(|p| p.uri.path != uri.path && p.uri.key() != key);
                if self.active.as_deref() == Some(key.as_str()) {
                    self.active = self.docs.keys().next().cloned();
                }
                BridgeAction::None
            }
            SessionUpdate::ConnectionLost { reason } => BridgeAction::ConnectionLost(reason),
            SessionUpdate::Problems { problems } => {
                self.problems = problems;
                BridgeAction::None
            }
            SessionUpdate::OutputChannels { channels, active_id } => {
                self.output_channels = channels;
                if let Some(active) = active_id {
                    self.active_output_channel = Some(active);
                } else if self.active_output_channel.is_none()
                    || !self.output_channels.iter().any(|c| Some(&c.id) == self.active_output_channel.as_ref())
                {
                    self.active_output_channel = self.output_channels.first().map(|c| c.id.clone());
                }
                BridgeAction::None
            }
            SessionUpdate::ExtensionMessage { severity, message, source } => {
                self.extension_messages.push(crate::output::ExtensionMessage {
                    severity,
                    message: message.clone(),
                    source: source.clone(),
                });
                BridgeAction::ExtensionNotice { severity, message, source }
            }
            SessionUpdate::Commands { .. } => BridgeAction::None,
            // Search and replace results are the search panel's, not a document's (FR-E7).
            SessionUpdate::SearchHits { .. } | SessionUpdate::SearchDone { .. } | SessionUpdate::ReplaceDone { .. } => BridgeAction::None,
        }
    }

    pub fn document(&self, uri: &UriComponents) -> Option<&DocumentWidgetState> {
        self.docs.get(&uri.key())
    }

    pub fn active_document(&self) -> Option<&DocumentWidgetState> {
        let key = self.active.as_ref()?;
        self.docs.get(key)
    }

    pub fn set_active(&mut self, uri: &UriComponents) {
        let key = uri.key();
        if self.docs.contains_key(&key) {
            self.active = Some(key);
        }
    }

    pub fn take_pending_edits(&mut self, uri: &UriComponents) -> Vec<WidgetCommand> {
        self.docs
            .get_mut(&uri.key())
            .map(|d| std::mem::take(&mut d.pending_edits))
            .unwrap_or_default()
    }

    pub fn on_code_changed(
        &mut self,
        uri: &UriComponents,
        version: u32,
        range: WidgetRange,
        text: String,
    ) -> Option<(UriComponents, u64, WidgetEvent)> {
        let doc = self.docs.get_mut(&uri.key())?;
        doc.version = version;
        if let Some(updated) = apply_text_change(&doc.text, range, &text) {
            doc.text = updated;
        }
        Some((doc.uri.clone(), doc.generation, WidgetEvent::CodeChanged { version, range, text }))
    }

    pub fn on_edit_rejected(
        &self,
        uri: &UriComponents,
        request_id: u32,
        base_version: u32,
        current_version: u32,
        range: WidgetRange,
    ) -> Option<(UriComponents, u64, WidgetEvent)> {
        let doc = self.docs.get(&uri.key())?;
        Some((
            doc.uri.clone(),
            doc.generation,
            WidgetEvent::CodeEditRejected { request_id, base_version, current_version, range },
        ))
    }

    pub fn on_code_hovered(
        &self,
        uri: &UriComponents,
        decoration: u64,
        pos: WidgetPos,
        phase: HoverPhase,
    ) -> Option<(UriComponents, u64, WidgetEvent)> {
        let doc = self.docs.get(&uri.key())?;
        Some((doc.uri.clone(), doc.generation, WidgetEvent::CodeHovered { decoration, pos, phase }))
    }

    pub fn on_save_requested(&self, uri: &UriComponents, version: u32) -> Option<(UriComponents, u64, WidgetEvent)> {
        let doc = self.docs.get(&uri.key())?;
        Some((doc.uri.clone(), doc.generation, WidgetEvent::CodeSaveRequested { version }))
    }

    pub fn on_decoration_activated(&self, uri: &UriComponents, decoration: u64) -> Option<(UriComponents, u64, WidgetEvent)> {
        let doc = self.docs.get(&uri.key())?;
        Some((doc.uri.clone(), doc.generation, WidgetEvent::DecorationActivated { decoration }))
    }

    pub fn on_selection_changed(
        &self,
        uri: &UriComponents,
        version: u32,
        anchor: WidgetPos,
        active: WidgetPos,
    ) -> Option<(UriComponents, u64, WidgetEvent)> {
        let doc = self.docs.get(&uri.key())?;
        Some((doc.uri.clone(), doc.generation, WidgetEvent::SelectionChanged { version, anchor, active }))
    }

    pub fn on_visible_range_changed(
        &self,
        uri: &UriComponents,
        version: u32,
        first_line: u32,
        last_line: u32,
    ) -> Option<(UriComponents, u64, WidgetEvent)> {
        let doc = self.docs.get(&uri.key())?;
        Some((
            doc.uri.clone(),
            doc.generation,
            WidgetEvent::VisibleRangeChanged { version, first_line, last_line },
        ))
    }
}
