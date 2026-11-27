//! FR-38 `CodeEditor` widget bridge: translates [`SessionUpdate`]s from [`crate::EditorSession`]
//! / [`crate::SessionCore`] into per-document widget props (including the 44-byte FR-38
//! `Decorations` wire blob) and translates widget callbacks into `(UriComponents, generation,
//! WidgetEvent)`.

use std::collections::HashMap;

use ember_editor_conn::uri::UriComponents;

use crate::codelens::CommandDto;
use crate::coords::{utf16_to_byte, WidgetPos, WidgetRange};
use crate::engine::{ExternalChange, SessionUpdate};
use crate::debugger::{DebugUpdate, FrameInfo, SessionInfo, SessionState, SourceBreakpoint, ThreadInfo};
use crate::folding::{compute_indent_folds, merge_folds, FoldRegion};
use crate::hover::HoverPopup;
use crate::widget::{Decoration, HoverPhase, WidgetCommand, WidgetEvent};

/// Per-document state and encoded props for a `CodeEditor` widget (FR-38).
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentWidgetState {
    pub uri: UriComponents,
    /// The node key for `CodeEditor`.
    pub generation: u64,
    /// The document version in the session's numbering: the widget's own version plus
    /// [`DocumentWidgetState::widget_base`], raised by the versions the session reports.
    pub version: u32,
    /// The session's document version at the moment the current widget started counting from 0.
    /// A widget that is mounted again (a tab switch, a pane that was out of the tree) restarts at
    /// version 0 while the session keeps counting, so every version the widget sends is moved up
    /// by this much and every version sent to it is moved down.
    pub widget_base: u32,
    /// The version of the widget's last change, in the widget's own numbering.
    pub widget_last: u32,
    /// The file text the buffer is based on, as far as the window has seen it: the text the file
    /// had when the document was opened or last saved. A restore after a reconnect passes it as the
    /// base, so a file that changed meanwhile is noticed.
    pub disk_base: Option<String>,
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
    /// Why the last save of this document failed, until a save succeeds (FR-L31.6).
    pub save_error: Option<String>,
    /// The coloured runs of the syntax and the document version they were written against.
    pub syntax: SyntaxRuns,
    /// Line ending, encoding and language for the status bar (FR-L31.7); `None` until the session
    /// reports it.
    pub format: Option<crate::text_format::DocumentFormat>,
    /// The document's own indentation options; `None` until the session reports them, when the
    /// document is indented with spaces `tab_width` wide.
    pub indentation: Option<crate::indentation::Indentation>,
    /// The foldable regions of the document (FR-E6).
    pub folds: Vec<FoldRegion>,
}

impl DocumentWidgetState {
    /// The document's indentation options: the session's report, or before it a document indented
    /// with spaces `tab_width` wide.
    pub fn effective_indentation(&self) -> crate::indentation::Indentation {
        self.indentation.unwrap_or_else(|| crate::indentation::Indentation::new(self.tab_width.max(1), true))
    }

    /// The widget counts from 0 again: the session's current version becomes the base.
    fn restart_widget_count(&mut self) {
        self.widget_base += self.widget_last;
        self.widget_last = 0;
        self.version = self.version.max(self.widget_base);
        self.decoration_bytes = encode_for_widget(&self.decorations, self.widget_base);
    }

    /// A version of the session's numbering as the widget numbers it.
    pub fn widget_version_of(&self, session_version: u32) -> u32 {
        session_version.saturating_sub(self.widget_base)
    }
}

/// The decorations' wire blob with their versions in the widget's numbering.
fn encode_for_widget(decorations: &[Decoration], base: u32) -> Vec<u8> {
    if base == 0 {
        return Decoration::encode_records(decorations);
    }
    let moved: Vec<Decoration> =
        decorations.iter().map(|d| Decoration { version: d.version.saturating_sub(base), ..d.clone() }).collect();
    Decoration::encode_records(&moved)
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
    ExtensionNotice(crate::output::ExtensionMessage),
    /// Running an extension command failed.
    CommandFailed { id: String, error: String },
    /// Reopening a document with an encoding failed (FR-L31.7).
    FormatFailed { uri: UriComponents, error: String },
    /// Something about a file went wrong without a user action to blame: a reload that failed, a
    /// watch the server refused, an edit the widget did not apply. Show it.
    FileNotice { uri: Option<UriComponents>, kind: crate::engine::FileNoticeKind, detail: String },
    /// An extension webview changed (FR-E8): pass it to the window's `WebviewDriver`. The bridge has
    /// already updated its placeholder panes ([`WidgetBridge::webview_panes`]).
    Webview(crate::webview::WebviewUpdate),
    /// A debugging update (FR-L31.42). The bridge has already folded what the editor controls show
    /// into [`WidgetBridge::debug`]; the update itself is for what the bridge does not keep, such as
    /// the configurations of the launch picker and a start that failed.
    Debug(DebugUpdate),
}

/// Where the shown session is stopped: the top frame of the stopped thread's call stack, which
/// Code-OSS marks in the glyph margin and reveals (`debugEditorDecorations`, `focusStackFrame`).
#[derive(Debug, Clone, PartialEq)]
pub struct StoppedLocation {
    pub session: String,
    pub thread_id: i64,
    pub frame_id: i64,
    pub uri: UriComponents,
    /// 1-based, as the debugger reports it.
    pub line: u32,
    /// 1-based.
    pub column: u32,
}

/// What the editor's debug controls show (FR-L31.42), kept from the session's [`DebugUpdate`]s:
/// the sessions for the debug toolbar, the stopped location for the gutter, and the source
/// breakpoints with what the adapters said about them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DebugView {
    pub sessions: Vec<SessionInfo>,
    /// The session the window shows.
    pub active: Option<String>,
    /// The threads of each live session, as last listed.
    pub threads: HashMap<String, Vec<ThreadInfo>>,
    pub stopped_at: Option<StoppedLocation>,
    /// The call stack of the stopped thread, as last loaded, to find a frame the window focuses.
    pub stack: Vec<FrameInfo>,
    /// The whole source breakpoint registry; `verified` and `adapter_line` are the adapters' answer.
    pub breakpoints: Vec<SourceBreakpoint>,
}

impl DebugView {
    /// The session the window shows, while it has not ended.
    pub fn active_session(&self) -> Option<&SessionInfo> {
        let id = self.active.as_ref()?;
        self.sessions.iter().find(|s| &s.id == id && s.state != SessionState::Terminated)
    }

    /// The thread a pause of the shown session goes to: the one it shows, else the first one listed
    /// (Code-OSS pauses the focused thread).
    pub fn pause_thread(&self) -> Option<i64> {
        let session = self.active_session()?;
        session.thread_id.or_else(|| self.threads.get(&session.id)?.first().map(|t| t.id))
    }

    /// The source breakpoints in the document `uri`.
    pub fn breakpoints_in<'a>(&'a self, uri: &'a UriComponents) -> impl Iterator<Item = &'a SourceBreakpoint> + 'a {
        self.breakpoints.iter().filter(move |bp| bp.uri == *uri)
    }

    /// The 1-based line the shown session is stopped at in `uri`.
    pub fn stopped_line(&self, uri: &UriComponents) -> Option<u32> {
        self.stopped_at.as_ref().filter(|at| at.uri == *uri).map(|at| at.line)
    }

    /// The frame the window focuses when it is not the top one: Code-OSS marks it apart from the
    /// top frame (`debug-focused-stack-frame-line`, `editor.focusedStackFrameHighlightBackground`).
    pub fn focused_frame(&self) -> Option<StoppedLocation> {
        let top = self.stopped_at.as_ref()?;
        let session = self.active_session()?;
        let frame_id = session.frame_id.filter(|f| *f != top.frame_id)?;
        let frame = self.stack.iter().find(|f| f.id == frame_id)?;
        Some(StoppedLocation {
            session: top.session.clone(),
            thread_id: top.thread_id,
            frame_id,
            uri: frame.uri.clone()?,
            line: frame.line,
            column: frame.column,
        })
    }

    /// The frame the window focuses while the shown session is stopped, Code-OSS's
    /// `focusedStackFrame`: a lower frame the window selected, else the top frame. `None` while
    /// nothing is stopped or the focused frame has no file. The debug hover evaluates here.
    pub fn focused_location(&self) -> Option<StoppedLocation> {
        let top = self.stopped_at.as_ref()?;
        let session = self.active_session()?;
        match session.frame_id {
            Some(frame) if frame != top.frame_id => self.focused_frame(),
            _ => Some(top.clone()),
        }
    }

    /// The 1-based line of the focused lower frame ([`Self::focused_frame`]) in `uri`.
    pub fn focused_line(&self, uri: &UriComponents) -> Option<u32> {
        self.focused_frame().filter(|at| at.uri == *uri).map(|at| at.line)
    }

    fn apply(&mut self, update: &DebugUpdate) {
        match update {
            DebugUpdate::Sessions { sessions, active } => {
                self.sessions.clone_from(sessions);
                self.active.clone_from(active);
                let live = |id: &str| sessions.iter().any(|s| s.id == id && s.state != SessionState::Terminated);
                self.threads.retain(|id, _| live(id.as_str()));
                // The mark goes when its thread runs on, its session ends or another one is shown.
                let still_stopped = |at: &StoppedLocation| {
                    active.as_deref() == Some(at.session.as_str())
                        && sessions.iter().any(|s| {
                            s.id == at.session
                                && matches!(s.state, SessionState::Stopped { .. })
                                && s.thread_id.is_none_or(|t| t == at.thread_id)
                        })
                };
                if self.stopped_at.as_ref().is_some_and(|at| !still_stopped(at)) {
                    self.stopped_at = None;
                    self.stack.clear();
                }
            }
            DebugUpdate::Threads { session, threads } => {
                self.threads.insert(session.clone(), threads.clone());
            }
            DebugUpdate::CallStack { session, thread_id, frames, .. } => {
                let shown = self.active.as_deref() == Some(session.as_str())
                    && self.sessions.iter().any(|s| &s.id == session && matches!(s.state, SessionState::Stopped { .. }));
                if !shown {
                    return;
                }
                self.stack.clone_from(frames);
                // A frame without a file (a native or generated one) leaves nothing to mark.
                self.stopped_at = frames.first().and_then(|top| {
                    Some(StoppedLocation {
                        session: session.clone(),
                        thread_id: *thread_id,
                        frame_id: top.id,
                        uri: top.uri.clone()?,
                        line: top.line,
                        column: top.column,
                    })
                });
            }
            DebugUpdate::Breakpoints { source, .. } => self.breakpoints.clone_from(source),
            _ => {}
        }
    }
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
    webview_panes: crate::webview::host::WebviewPanes,
    debug: DebugView,
}

impl WidgetBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// The extension webviews the window has a place for (FR-E8); each shows a placeholder until
    /// the webview host node exists.
    pub fn webview_panes(&self) -> &crate::webview::host::WebviewPanes {
        &self.webview_panes
    }

    /// What the debug controls show, as the session last reported it (FR-L31.42).
    pub fn debug(&self) -> &DebugView {
        &self.debug
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

    /// Apply one [`SessionUpdate`] and return any [`BridgeAction`] the UI layer must handle.
    pub fn apply(&mut self, update: SessionUpdate) -> BridgeAction {
        match update {
            SessionUpdate::Opened { uri, generation, text, language_id, tab_width } => {
                let key = uri.key();
                // Opening a document that is already open (a click on its explorer row or its tab)
                // brings it forward. Its unsaved state, pending edits, conflict bar, save error and
                // decorations stay: rebuilding them would show an unsaved buffer as saved, and
                // closing that tab would then skip the save prompt.
                if let Some(doc) = self.docs.get_mut(&key).filter(|d| d.generation == generation) {
                    if doc.text != text || doc.tab_width != tab_width {
                        let lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
                        let indent = compute_indent_folds(&lines, tab_width);
                        doc.folds = merge_folds(0, lines.len(), &[], &indent, &std::collections::HashSet::new());
                    }
                    doc.text = text;
                    doc.language_id = language_id;
                    doc.tab_width = tab_width;
                    self.active = Some(key);
                    return BridgeAction::None;
                }
                let lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
                let indent = compute_indent_folds(&lines, tab_width);
                let folds = merge_folds(0, lines.len(), &[], &indent, &std::collections::HashSet::new());
                self.docs.insert(
                    key.clone(),
                    DocumentWidgetState {
                        uri,
                        generation,
                        version: 0,
                        widget_base: 0,
                        widget_last: 0,
                        disk_base: Some(text.clone()),
                        text,
                        language_id,
                        tab_width,
                        dirty: false,
                        decorations: Vec::new(),
                        decoration_bytes: Vec::new(),
                        hover: None,
                        pending_edits: Vec::new(),
                        external_change: None,
                        save_error: None,
                        syntax: SyntaxRuns::default(),
                        format: None,
                        indentation: None,
                        folds,
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
                            let lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
                            let indent = compute_indent_folds(&lines, doc.tab_width);
                            let folds = merge_folds(0, lines.len(), &[], &indent, &std::collections::HashSet::new());
                            doc.generation = generation;
                            doc.version = 0;
                            doc.widget_base = 0;
                            doc.widget_last = 0;
                            doc.text = text;
                            doc.decorations.clear();
                            doc.decoration_bytes.clear();
                            doc.hover = None;
                            doc.pending_edits.clear();
                            doc.external_change = None;
                            doc.syntax = SyntaxRuns::default();
                            doc.folds = folds;
                        }
                        edit @ (WidgetCommand::EditCode { .. } | WidgetCommand::InsertSnippet { .. }) => {
                            doc.pending_edits.push(edit);
                        }
                    }
                }
                BridgeAction::None
            }
            SessionUpdate::Decorations { uri, generation, decorations } => {
                if let Some(doc) = self.docs.get_mut(&uri.key()) {
                    if doc.generation == generation {
                        doc.decoration_bytes = encode_for_widget(&decorations, doc.widget_base);
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
            SessionUpdate::Folds { uri, generation, version, folds } => {
                if let Some(doc) = self.docs.get_mut(&uri.key()) {
                    if doc.generation == generation {
                        doc.version = doc.version.max(version);
                        doc.folds = folds;
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
                    if !dirty {
                        // Nothing is left to save (saved, or reverted to the disk version).
                        doc.save_error = None;
                        doc.disk_base = Some(doc.text.clone());
                    }
                }
                BridgeAction::None
            }
            SessionUpdate::Saved { uri } => {
                if let Some(doc) = self.docs.get_mut(&uri.key()) {
                    doc.external_change = None;
                    doc.save_error = None;
                }
                BridgeAction::None
            }
            SessionUpdate::SaveFailed { uri, error } => {
                if let Some(doc) = self.docs.get_mut(&uri.key()) {
                    doc.save_error = Some(error.clone());
                }
                BridgeAction::SaveFailed { uri, error }
            }
            SessionUpdate::CommandFailed { id, error } => BridgeAction::CommandFailed { id, error },
            SessionUpdate::ExternalChange { uri, change } => {
                if let Some(doc) = self.docs.get_mut(&uri.key()) {
                    doc.external_change = Some(change);
                }
                BridgeAction::None
            }
            SessionUpdate::RunCommand { command } => BridgeAction::RunCommand(command),
            SessionUpdate::FileNotice { uri, kind, detail } => BridgeAction::FileNotice { uri, kind, detail },
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
            SessionUpdate::ConnectionLost { reason } => {
                // The sessions lived in the extension host that is gone; the next one lists them again.
                self.debug = DebugView::default();
                BridgeAction::ConnectionLost(reason)
            }
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
            // The workbench keeps the toasts and the center (`notifications::Notifications`).
            SessionUpdate::ExtensionMessage(message) => BridgeAction::ExtensionNotice(message),
            SessionUpdate::Commands { .. } | SessionUpdate::Languages { .. } => BridgeAction::None,
            // The window's trust prompt handles it (FR-L31.18).
            SessionUpdate::WorkspaceTrustRequested => BridgeAction::None,
            SessionUpdate::Format { uri, format } => {
                if let Some(doc) = self.docs.get_mut(&uri.key()) {
                    doc.format = Some(format);
                }
                BridgeAction::None
            }
            SessionUpdate::Indentation { uri, indentation } => {
                if let Some(doc) = self.docs.get_mut(&uri.key()) {
                    doc.tab_width = indentation.tab_size;
                    doc.indentation = Some(indentation);
                }
                BridgeAction::None
            }
            SessionUpdate::FormatFailed { uri, error } => BridgeAction::FormatFailed { uri, error },
            // Search and replace results are the search panel's, not a document's (FR-E7).
            SessionUpdate::SearchHits { .. } | SessionUpdate::SearchDone { .. } | SessionUpdate::ReplaceDone { .. } => BridgeAction::None,
            // Language feature answers are the window's lists and boxes, not a document's props (FR-L31.13).
            SessionUpdate::Language(_) => BridgeAction::None,
            // The editor's controls read the sessions, the stopped frame and the breakpoints from
            // here; the window takes the rest (configurations, variables, console) from the action.
            SessionUpdate::Debug(update) => {
                self.debug.apply(&update);
                BridgeAction::Debug(update)
            }
            SessionUpdate::Webview(update) => {
                self.webview_panes.apply(&update);
                BridgeAction::Webview(update)
            }
            // Protocol coverage records are for the regression run and the logs (FR-E9).
            SessionUpdate::ProtocolGap { .. } => BridgeAction::None,
        }
    }

    pub fn document(&self, uri: &UriComponents) -> Option<&DocumentWidgetState> {
        self.docs.get(&uri.key())
    }

    pub fn active_document(&self) -> Option<&DocumentWidgetState> {
        let key = self.active.as_ref()?;
        self.docs.get(key)
    }

    /// Hide the failed save of a document (the bar's 닫기); the next failure shows again (FR-L31.6).
    pub fn dismiss_save_error(&mut self, uri: &UriComponents) {
        if let Some(doc) = self.docs.get_mut(&uri.key()) {
            doc.save_error = None;
        }
    }

    pub fn set_active(&mut self, uri: &UriComponents) {
        let key = uri.key();
        if self.docs.contains_key(&key) {
            self.active = Some(key);
        }
    }

    /// The edits queued for the widget of `uri`, with their base versions in the widget's own
    /// numbering (they were written against the session's).
    pub fn take_pending_edits(&mut self, uri: &UriComponents) -> Vec<WidgetCommand> {
        let Some(doc) = self.docs.get_mut(&uri.key()) else { return Vec::new() };
        let base = doc.widget_base;
        std::mem::take(&mut doc.pending_edits)
            .into_iter()
            .map(|edit| match edit {
                WidgetCommand::EditCode { request_id, base_version, range, text } => {
                    WidgetCommand::EditCode { request_id, base_version: base_version.saturating_sub(base), range, text }
                }
                WidgetCommand::InsertSnippet { base_version, range, snippet } => {
                    WidgetCommand::InsertSnippet { base_version: base_version.saturating_sub(base), range, snippet }
                }
                other => other,
            })
            .collect()
    }

    /// The `CodeEditor` of `uri` with node key `generation` was mounted: it counts versions from 0
    /// again, while the session keeps its own count. Call it once per mount, from the mount of the
    /// widget. A mount of a node whose key is not the document's current generation is ignored.
    ///
    /// Without this call a restart is still noticed when the widget sends a version that is not
    /// above its last one, but a command queued for the widget before its first change would be
    /// written against the wrong version.
    pub fn on_widget_mounted(&mut self, uri: &UriComponents, generation: u64) {
        let Some(doc) = self.docs.get_mut(&uri.key()).filter(|d| d.generation == generation) else { return };
        doc.restart_widget_count();
    }

    pub fn on_code_changed(
        &mut self,
        uri: &UriComponents,
        version: u32,
        range: WidgetRange,
        text: String,
    ) -> Option<(UriComponents, u64, WidgetEvent)> {
        let doc = self.docs.get_mut(&uri.key())?;
        if version <= doc.widget_last {
            // The widget went back to a lower number: it was mounted again and counts from 0.
            doc.restart_widget_count();
        }
        doc.widget_last = version;
        let session_version = doc.widget_base + version;
        doc.version = session_version;
        // The edit is unsaved from this moment. The session says so too, but not before a round
        // trip, and not at all while no session answers: closing the tab in that time must still ask.
        doc.dirty = true;
        if let Some(updated) = apply_text_change(&doc.text, range, &text) {
            doc.text = updated;
        }
        Some((doc.uri.clone(), doc.generation, WidgetEvent::CodeChanged { version: session_version, range, text }))
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
            WidgetEvent::CodeEditRejected {
                request_id,
                base_version: doc.widget_base + base_version,
                current_version: doc.widget_base + current_version,
                range,
            },
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
        Some((doc.uri.clone(), doc.generation, WidgetEvent::CodeSaveRequested { version: doc.widget_base + version }))
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
        Some((doc.uri.clone(), doc.generation, WidgetEvent::SelectionChanged { version: doc.widget_base + version, anchor, active }))
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
            WidgetEvent::VisibleRangeChanged { version: doc.widget_base + version, first_line, last_line },
        ))
    }

    pub fn on_fold_changed(
        &mut self,
        uri: &UriComponents,
        start_line: u32,
        collapsed: bool,
    ) -> Option<(UriComponents, u64, WidgetEvent)> {
        let doc = self.docs.get_mut(&uri.key())?;
        for f in &mut doc.folds {
            if f.start_line == start_line {
                f.collapsed = collapsed;
            }
        }
        Some((
            doc.uri.clone(),
            doc.generation,
            WidgetEvent::FoldChanged { start_line, collapsed },
        ))
    }
}
