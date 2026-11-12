//! The session core: a synchronous state machine with no I/O.
//!
//! Everything the editor session decides is decided here, from [`Input`]s, as a list of
//! [`Effect`]s: RPC calls to the extension host (with a [`Token`] when the reply matters), RPC
//! cancellations, replies to extension-host requests, remote-filesystem reads / writes / watches,
//! and [`SessionUpdate`]s for the UI. [`crate::session`] owns the sockets and executes the effects;
//! the replies come back as inputs. This split is what lets every bridge be tested without a
//! server, and keeps one ordering for all RPC traffic (effects are executed in order, so
//! `$acceptModelChanged` for version n always precedes a request made at version n).
//!
//! Time is an input too (`now` on every call, plus [`SessionCore::next_wake`]), so debounces are
//! deterministic in tests.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::{Duration, Instant};

use ember_editor_conn::document::DocumentMirror;
use ember_editor_conn::exthost::{self, Call, DocumentsAndEditorsDelta, MainThreadCall};
use ember_editor_conn::remote_fs::{FileChange, FileChangeType};
use ember_editor_conn::rpc::{js_error, Arg, IncomingRequest, Reply};
use ember_editor_conn::uri::UriComponents;
use serde_json::{json, Value};

use crate::codelens::{CodeLenses, CommandDto};
use crate::coords::{WidgetPos, WidgetRange};
use crate::diagnostics::Diagnostics;
use crate::document::{decode, OpenDocument};
use crate::folding::{parse_provider_folds, DocFolds, FoldRegion};
use crate::highlight::{Action as HighlightAction, HighlightJob, HighlightOutput, Highlights, Problem};
use crate::hover::{HoverPopup, HoverState};
use crate::ids::{DecorationIds, IdSpace};
use crate::inline::{self, EndReason, Ghost, InlineDoc, ReplyOutcome, TypeThrough};
use crate::languages::{LanguageInfo, LanguageRegistry};
use crate::text_format::{decode_as, decode_detect, decode_reload, DocumentFormat, Encoding, EncodingMode, Eol};
use crate::registry::{KindTag, ProviderKind, Registry, RegistryChange};
use crate::replies::session_reply;
use crate::search::{FileHits, ReplaceReport, ReplaceRequest, SearchEnd, SearchRequest, Target};
use crate::semantic::{decode_semantic_tokens, overlay_semantic_runs, DocSemanticTokens};
use crate::widget::{Decoration, DecorationKind, WidgetCommand, WidgetEvent};

mod language;

/// How many versions behind the document an answer of the highlighter may be and still be shown.
const MAX_SPAN_VERSION_LAG: u32 = 500;

/// Identifies an outstanding RPC call or file write of the core.
pub type Token = u64;

#[derive(Debug, Clone)]
pub struct CoreOptions {
    /// `TabWidth` for the widget and `tabSize` for the extension host's editor options.
    pub tab_width: u32,
    pub insert_spaces: bool,
    /// Quiet time after the last edit before CodeLenses are re-requested.
    pub codelens_debounce: Duration,
    /// At most this many unresolved lenses are resolved per list (FR-38 has no viewport event).
    pub codelens_resolve_cap: usize,
    /// Ask inline-completion providers while typing.
    pub inline_completions: bool,
}

impl Default for CoreOptions {
    fn default() -> Self {
        Self {
            tab_width: 4,
            insert_spaces: true,
            codelens_debounce: Duration::from_millis(250),
            codelens_resolve_cap: 100,
            inline_completions: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadPurpose {
    /// The user opened the file.
    Open,
    /// The watcher reported a change to an open file.
    Reload,
    /// An explicit reload of an open file after a work-tree discard (replaces the buffer even if dirty).
    ForceReload,
    /// `MainThreadDocuments.$tryOpenDocument` from the extension host (`workspace.openTextDocument`).
    ExtHostOpen { req: u32 },
    /// A grammar file the syntax highlighting needs (`scope` is the grammar's scope name).
    Grammar { scope: String },
    /// A file that is not open, read for replace `id` (FR-E7).
    Replace { id: u64 },
    /// A file that is not open, read for workspace edit `id` (FR-L31.13).
    WorkspaceEdit { id: u64 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Open { uri: UriComponents },
    Close { uri: UriComponents },
    /// Reload an open document from disk even if its buffer has unsaved edits (e.g. after discard).
    Reload { uri: UriComponents },
    /// The editor of `uri` became the active one.
    Focus { uri: UriComponents },
    /// Show `text` (widget form) as the buffer of `uri`, opening the file first if it is not open: the
    /// window's buffer kept across a reconnect (FR-L31.6). The document is dirty unless `text` equals
    /// the file on disk; equal text changes nothing.
    Restore { uri: UriComponents, text: String },
    /// An event from the `CodeEditor` node rendered with key `generation`.
    Widget { uri: UriComponents, generation: u64, event: WidgetEvent },
    /// Explicit inline-completion request at `pos` (a keyboard command).
    TriggerInline { uri: UriComponents, generation: u64, pos: WidgetPos },
    ExtHostRequest(IncomingRequest),
    RpcReply { token: Token, result: Result<Value, String> },
    FileRead { uri: UriComponents, purpose: ReadPurpose, result: Result<Vec<u8>, String> },
    FileWritten { uri: UriComponents, token: Token, result: Result<(), String> },
    FilesChanged(Vec<FileChange>),
    /// The highlighting worker finished a batch of jobs.
    Highlighted(HighlightOutput),
    /// Execute a command (dispatches to extension host or workbench).
    ExecuteCommand { id: String, arguments: Vec<Value> },
    /// Search the workspace folder `folder` (FR-E7). A running search is cancelled first.
    Search { id: u64, request: SearchRequest, folder: UriComponents },
    /// Stop search `id`.
    CancelSearch { id: u64 },
    /// Replace the chosen matches (FR-E7); answered with [`SessionUpdate::ReplaceDone`].
    Replace { id: u64, request: ReplaceRequest },
    /// Change the line ending of an open document (FR-L31.7); it becomes dirty.
    SetEol { uri: UriComponents, eol: Eol },
    /// Reopen an open document in `encoding` (its unsaved edits are discarded) or save it in
    /// `encoding` (FR-L31.7).
    SetEncoding { uri: UriComponents, encoding: Encoding, mode: EncodingMode },
    /// Put an open document under a language id; `None` detects it from the path again (FR-L31.7).
    SetLanguage { uri: UriComponents, language_id: Option<String> },
    /// The user's answer to the workspace trust prompt (FR-L31.18): `true` grants trust.
    DecideWorkspaceTrust { trusted: bool },
    /// The user closed extension message `id`: with the chosen button's handle, or `None` for
    /// `undefined` (FR-L31.16).
    AnswerMessage { id: u64, handle: Option<i64> },
    /// A language feature the window asks for (FR-L31.13); answered with [`SessionUpdate::Language`].
    Language(crate::language::LanguageRequest),
    /// Nothing happened; run due timers.
    Tick,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Send `call`. With a token the reply comes back as [`Input::RpcReply`]; without one it is
    /// only logged on error.
    Rpc { token: Option<Token>, call: Call },
    /// RPC `Cancel` for the call started with this token (its reply is then never delivered).
    Cancel(Token),
    /// Answer extension-host request `req`.
    Respond { req: u32, result: Result<Reply, Option<Value>> },
    ReadFile { uri: UriComponents, purpose: ReadPurpose },
    WriteFile { uri: UriComponents, bytes: Vec<u8>, token: Token },
    Watch { uri: UriComponents },
    Unwatch { uri: UriComponents },
    /// Work for the highlighting thread; its answer comes back as [`Input::Highlighted`].
    Highlight(HighlightJob),
    Ui(SessionUpdate),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalChange {
    /// The file changed on disk while the buffer has unsaved edits: the UI decides (reload or
    /// keep). Clean buffers are reloaded without asking.
    ChangedWhileDirty,
    Deleted,
}

/// What the UI layer applies to the widget and around it.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionUpdate {
    /// The coloured runs of the lines in `window` (first, last inclusive) of the node with key `generation`,
    /// written against document `version`; lines outside the window carry none. Replaces the earlier list.
    Syntax { uri: UriComponents, generation: u64, version: u32, window: (u32, u32), runs: Vec<crate::textmate::DocRun> },
    /// The foldable regions of the node with key `generation`, written against document `version` (FR-E6).
    Folds { uri: UriComponents, generation: u64, version: u32, folds: Vec<FoldRegion> },
    /// Render a `CodeEditor` with key `generation`, `Text = text`, `TabWidth = tab_width`.
    Opened { uri: UriComponents, generation: u64, text: String, language_id: String, tab_width: u32 },
    OpenFailed { uri: UriComponents, error: String },
    /// A widget command (`SetText` = new node with a new key; `EditCode`).
    Command { uri: UriComponents, command: WidgetCommand },
    /// The full `Decorations` prop for the node with key `generation`.
    Decorations { uri: UriComponents, generation: u64, decorations: Vec<Decoration> },
    Hover(HoverPopup),
    HoverHidden { uri: UriComponents },
    Dirty { uri: UriComponents, dirty: bool },
    Saved { uri: UriComponents },
    SaveFailed { uri: UriComponents, error: String },
    ExternalChange { uri: UriComponents, change: ExternalChange },
    /// Acommand neither the extension host nor Ember implements (a workbench command such as
    /// `editor.action.showReferences`); the UI may handle it or ignore it.
    RunCommand { command: CommandDto },
    /// An extension command execution failed (`$executeContributedCommand`).
    CommandFailed { id: String, error: String },
    /// The widget and the session disagree about the document (lost event, bad range). The UI
    /// should call `EditorSession::resync` (re-sends the session's text as a new generation).
    Desync { uri: UriComponents, error: String },
    Closed { uri: UriComponents },
    /// The extension-host or management connection is gone.
    ConnectionLost { reason: String },
    /// An extension asked for workspace trust (`$requestWorkspaceTrust`) while the folder is in
    /// restricted mode: the window shows the trust prompt (FR-L31.18).
    WorkspaceTrustRequested,
    /// The problems list changed: every marker of every file, errors first.
    Problems { problems: Vec<crate::diagnostics::Problem> },
    /// Output channels changed (channels list, or an append/clear).
    OutputChannels { channels: Vec<crate::output::OutputChannel>, active_id: Option<String> },
    /// An extension surfaced a notification message ($showMessage). The call stays open until
    /// [`Input::AnswerMessage`] names its id (FR-L31.16).
    ExtensionMessage(crate::output::ExtensionMessage),
    /// Commands contributed by extensions or registered at runtime.
    Commands { commands: Vec<crate::commands::ContributedCommand> },
    /// More matches of search `id` (FR-E7), in arrival order.
    SearchHits { id: u64, files: Vec<FileHits> },
    /// Search `id` ended; no hits for it follow.
    SearchDone { id: u64, end: SearchEnd },
    /// Replace `id` finished.
    ReplaceDone { id: u64, report: ReplaceReport },
    /// The line ending, encoding and language of an open document (FR-L31.7): after it opens, is
    /// reloaded, or any of the three changes.
    Format { uri: UriComponents, format: DocumentFormat },
    /// Reopening with an encoding failed; the document is unchanged (FR-L31.7).
    FormatFailed { uri: UriComponents, error: String },
    /// Every language the extensions contribute, for the language mode picker (FR-L31.7).
    Languages { languages: Vec<LanguageInfo> },
    /// The answer to a language feature request (FR-L31.13).
    Language(crate::language::LanguageUpdate),
}

#[derive(Debug, Clone)]
enum Pending {
    CodeLens { doc: String, handle: i64, version: u32 },
    CodeLensResolve { doc: String, handle: i64, serial: u64, index: usize },
    Folding { doc: String, handle: i64, version: u32 },
    SemanticTokens { doc: String, handle: i64, version: u32 },
    Hover { doc: String },
    Inline { doc: String },
    /// `$activateByEvent("onCommand:<id>")` before running a command not registered yet.
    ActivateThenRun { command: CommandDto },
    Command { id: String },
    /// `$provideTextSearchResults` of search `id`.
    Search { id: u64 },
    /// A call of the language features (FR-L31.13).
    Language(language::PendingLang),
}

/// The running search: the window's id, the provider session, its call, and the open documents whose
/// unsaved text was searched instead of the disk (their provider matches are dropped).
#[derive(Debug, Clone)]
struct ActiveSearch {
    id: u64,
    session: i64,
    token: Token,
    overlaid: HashSet<String>,
}

/// One replace in progress: what to replace, the matches per file key, files left, and the report so far.
#[derive(Debug, Clone)]
struct ReplaceJob {
    request: ReplaceRequest,
    targets: HashMap<String, Vec<Target>>,
    remaining: usize,
    report: ReplaceReport,
}

/// A file write of a replace: the job, the file, the counts it carries, and for an open document its
/// extension-host version when the new text was made, and that text.
#[derive(Debug, Clone)]
struct ReplaceWrite {
    id: u64,
    key: String,
    replaced: usize,
    skipped: usize,
    open: Option<(u64, String)>,
}

pub struct SessionCore {
    opts: CoreOptions,
    /// The window's settings (FR-L31.17); `None` until the session has read them.
    settings: Option<crate::settings::SettingsScopes>,
    languages: LanguageRegistry,
    language_ids: Vec<String>,
    docs: HashMap<String, OpenDocument>,
    opening: HashSet<String>,
    /// Buffers to restore once their file has been read ([`Input::Restore`] before the open finished).
    restores: HashMap<String, String>,
    /// Documents announced to the extension host only (for `$tryOpenDocument`): key → (uri, version).
    exthost_only: HashMap<String, (UriComponents, u64)>,
    active: Option<String>,
    registry: Registry,
    diagnostics: Diagnostics,
    lenses: CodeLenses,
    highlights: Highlights,
    textmate_runs: HashMap<String, (u32, (u32, u32), Vec<crate::textmate::DocRun>)>,
    folds: HashMap<String, DocFolds>,
    folding_due: HashMap<String, inline::Debouncer>,
    folding_inflight: HashMap<(String, i64), Token>,
    semantic: HashMap<String, DocSemanticTokens>,
    semantic_due: HashMap<String, inline::Debouncer>,
    semantic_inflight: HashMap<(String, i64), Token>,
    codelens_due: HashMap<String, inline::Debouncer>,
    codelens_inflight: HashMap<(String, i64), Token>,
    hover: HoverState,
    inline: HashMap<String, InlineDoc>,
    ids: DecorationIds,
    pending: HashMap<Token, Pending>,
    saves: HashMap<Token, (String, u64, Vec<u8>)>,
    /// Documents being read again in a chosen encoding (FR-L31.7): key → encoding.
    reopen: HashMap<String, Encoding>,
    /// Handle of the extension host's text search provider for the workspace's scheme (FR-E7).
    search_provider: Option<i64>,
    search_session: i64,
    search: Option<ActiveSearch>,
    replaces: HashMap<u64, ReplaceJob>,
    replace_writes: HashMap<Token, ReplaceWrite>,
    lang: language::LangState,
    next_token: Token,
    next_generation: u64,
    next_editor: u64,
    next_edit_request: u32,
    reply_seq: u64,
    /// Workspace trust and the trust requests waiting for the user (FR-L31.18).
    trust: crate::trust::WorkspaceTrust,
    output_seq: u64,
    output_channels: Vec<crate::output::OutputChannel>,
    /// Open `$showMessage` calls by Ember message id (FR-L31.16).
    pending_messages: HashMap<u64, u32>,
    next_message_id: u64,
    touched: BTreeSet<String>,
    out: Vec<Effect>,
    now: Instant,
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

impl SessionCore {
    pub fn new(languages: LanguageRegistry, opts: CoreOptions, now: Instant) -> Self {
        let language_ids = languages.ids();
        Self {
            opts,
            settings: None,
            languages,
            language_ids,
            docs: HashMap::new(),
            opening: HashSet::new(),
            restores: HashMap::new(),
            exthost_only: HashMap::new(),
            active: None,
            registry: Registry::new(),
            diagnostics: Diagnostics::new(),
            lenses: CodeLenses::new(),
            highlights: Highlights::new(),
            textmate_runs: HashMap::new(),
            folds: HashMap::new(),
            folding_due: HashMap::new(),
            folding_inflight: HashMap::new(),
            semantic: HashMap::new(),
            semantic_due: HashMap::new(),
            semantic_inflight: HashMap::new(),
            codelens_due: HashMap::new(),
            codelens_inflight: HashMap::new(),
            hover: HoverState::new(),
            inline: HashMap::new(),
            ids: DecorationIds::new(),
            pending: HashMap::new(),
            saves: HashMap::new(),
            reopen: HashMap::new(),
            search_provider: None,
            search_session: 0,
            search: None,
            replaces: HashMap::new(),
            replace_writes: HashMap::new(),
            lang: language::LangState::default(),
            next_token: 0,
            next_generation: 0,
            next_editor: 0,
            next_edit_request: 0,
            reply_seq: 0,
            trust: crate::trust::WorkspaceTrust::default(),
            output_seq: 0,
            output_channels: Vec::new(),
            pending_messages: HashMap::new(),
            next_message_id: 0,
            touched: BTreeSet::new(),
            out: Vec::new(),
            now,
        }
    }

    /// Take the window's settings (FR-L31.17). `editor.tabSize` and `editor.insertSpaces` apply to
    /// documents opened from now on; an open document keeps what it was opened with.
    pub fn set_settings(&mut self, scopes: crate::settings::SettingsScopes) {
        self.settings = Some(scopes);
    }

    /// Tab size and insert-spaces for a document of `language`: the settings when the session has
    /// them, the start-up options otherwise.
    fn indent_for(&self, language: &str) -> (u32, bool) {
        match &self.settings {
            Some(scopes) => {
                let resolved = crate::settings::EditorSettings::resolve(scopes, Some(language));
                (resolved.tab_size, resolved.insert_spaces)
            }
            None => (self.opts.tab_width, self.opts.insert_spaces),
        }
    }

    /// Process one input at time `now`; returns the effects in the order they must be executed.
    pub fn handle(&mut self, now: Instant, input: Input) -> Vec<Effect> {
        self.now = now;
        match input {
            Input::Open { uri } => self.on_open(uri),
            Input::Close { uri } => self.on_close(&uri),
            Input::Reload { uri } => self.on_reload(uri),
            Input::Focus { uri } => self.on_focus(&uri),
            Input::Restore { uri, text } => self.on_restore(uri, text),
            Input::Widget { uri, generation, event } => self.on_widget(&uri, generation, event),
            Input::TriggerInline { uri, generation, pos } => self.on_trigger_inline(&uri, generation, pos),
            Input::ExtHostRequest(r) => self.on_exthost_request(r),
            Input::RpcReply { token, result } => self.on_rpc_reply(token, result),
            Input::FileRead { uri, purpose, result } => self.on_file_read(uri, purpose, result),
            Input::FileWritten { uri, token, result } => self.on_file_written(&uri, token, result),
            Input::FilesChanged(changes) => self.on_files_changed(changes),
            Input::Highlighted(output) => self.on_highlighted(output),
            Input::ExecuteCommand { id, arguments } => {
                self.run_command(CommandDto { id, title: String::new(), tooltip: None, arguments });
            }
            Input::Search { id, request, folder } => self.on_search(id, request, folder),
            Input::CancelSearch { id } => self.on_cancel_search(id),
            Input::Replace { id, request } => self.on_replace(id, request),
            Input::SetEol { uri, eol } => self.on_set_eol(&uri, eol),
            Input::SetEncoding { uri, encoding, mode } => self.on_set_encoding(&uri, encoding, mode),
            Input::SetLanguage { uri, language_id } => self.on_set_language(&uri, language_id),
            Input::DecideWorkspaceTrust { trusted } => self.on_decide_trust(trusted),
            Input::AnswerMessage { id, handle } => {
                if let Some(req) = self.pending_messages.remove(&id) {
                    let reply = match handle { Some(h) => Reply::Json(Value::from(h)), None => Reply::Empty };
                    self.respond(req, Ok(reply));
                }
            }
            Input::Language(request) => self.on_language(request),
            Input::Tick => {}
        }
        self.run_timers();
        self.flush_decorations();
        std::mem::take(&mut self.out)
    }

    /// The grammars the installed extensions contribute (`contributes.grammars`); documents opened after this
    /// are highlighted when their language has one.
    pub fn set_grammar_catalog(&mut self, catalog: crate::textmate::GrammarCatalog) {
        for why in catalog.skipped() {
            tracing::debug!("ember-editor: grammar skipped: {why}");
        }
        self.highlights.set_catalog(catalog);
    }

    /// When [`Input::Tick`] must be delivered next (earliest debounce deadline).
    pub fn next_wake(&self) -> Option<Instant> {
        let a = self.inline.values().filter_map(|s| s.debounce.deadline());
        let b = self.codelens_due.values().filter_map(|d| d.deadline());
        let c = self.folding_due.values().filter_map(|d| d.deadline());
        let d = self.semantic_due.values().filter_map(|d| d.deadline());
        a.chain(b).chain(c).chain(d).min()
    }

    /// Every known language with its display name (FR-L31.7).
    pub fn language_list(&self) -> Vec<LanguageInfo> {
        self.languages.languages()
    }

    /// The text the session holds for a document, as the widget should have it.
    pub fn widget_text(&self, uri: &UriComponents) -> Option<String> {
        self.docs.get(&uri.key()).map(OpenDocument::widget_text)
    }

    /// Re-send the session's text to the widget as a new generation (after a [`SessionUpdate::Desync`]).
    /// The extension host is re-announced like on a reload, so both sides agree again.
    pub fn resync(&mut self, now: Instant, uri: &UriComponents) -> Vec<Effect> {
        self.now = now;
        let key = uri.key();
        if let Some(text) = self.docs.get(&key).map(|d| d.lines().join("\n")) {
            self.reset_doc(&key, &text, None);
        }
        self.flush_decorations();
        std::mem::take(&mut self.out)
    }

    // ---- small helpers ------------------------------------------------------------------------

    fn token(&mut self) -> Token {
        self.next_token += 1;
        self.next_token
    }

    fn rpc(&mut self, call: Call) {
        self.out.push(Effect::Rpc { token: None, call });
    }

    fn rpc_with(&mut self, call: Call, pending: Pending) -> Token {
        let t = self.token();
        self.pending.insert(t, pending);
        self.out.push(Effect::Rpc { token: Some(t), call });
        t
    }

    fn cancel(&mut self, tokens: Vec<Token>) {
        for t in tokens {
            self.pending.remove(&t);
            self.out.push(Effect::Cancel(t));
        }
    }

    fn ui(&mut self, u: SessionUpdate) {
        self.out.push(Effect::Ui(u));
    }

    /// The trust state the extension host was started with (FR-L31.18); set once, before the
    /// first input.
    pub fn set_workspace_trust(&mut self, trust: crate::trust::WorkspaceTrust) {
        self.trust = trust;
    }

    pub fn workspace_trusted(&self) -> bool {
        self.trust.is_trusted()
    }

    fn on_decide_trust(&mut self, trusted: bool) {
        let (calls, answers) = self.trust.decide(trusted);
        for call in calls {
            self.out.push(Effect::Rpc { token: None, call });
        }
        for (req, reply) in answers {
            self.respond(req, Ok(reply));
        }
    }

    fn respond(&mut self, req: u32, result: Result<Reply, Option<Value>>) {
        self.out.push(Effect::Respond { req, result });
    }

    fn uri_of(&self, key: &str) -> Option<UriComponents> {
        self.docs.get(key).map(|d| d.uri.clone())
    }

    fn edit_request_id(&mut self) -> u32 {
        self.next_edit_request = self.next_edit_request.wrapping_add(1).max(1);
        self.next_edit_request
    }

    fn schedule_codelens(&mut self, key: &str, delay: Duration) {
        let now = self.now;
        self.codelens_due.entry(key.to_owned()).or_default().schedule(now, delay);
    }

    fn schedule_folding(&mut self, key: &str, delay: Duration) {
        let now = self.now;
        self.folding_due.entry(key.to_owned()).or_default().schedule(now, delay);
    }

    fn schedule_semantic(&mut self, key: &str, delay: Duration) {
        let now = self.now;
        self.semantic_due.entry(key.to_owned()).or_default().schedule(now, delay);
    }

    fn emit_folds(&mut self, key: &str) {
        let Some(doc) = self.docs.get(key) else { return };
        let (uri, generation, version) = (doc.uri.clone(), doc.generation(), doc.widget_version());
        let tab_width = self.indent_for(&doc.language_id).0;
        let folds = self
            .folds
            .entry(key.to_owned())
            .or_default()
            .regions(version, doc.lines(), tab_width);
        self.ui(SessionUpdate::Folds { uri, generation, version, folds });
    }

    fn emit_syntax(&mut self, key: &str) {
        let Some(doc) = self.docs.get(key) else { return };
        let (uri, generation, current_version) = (doc.uri.clone(), doc.generation(), doc.widget_version());
        let default_window = self
            .highlights
            .window(key)
            .unwrap_or((0, crate::highlight::WINDOW_MARGIN * 2));
        let (tm_version, window, tm_runs) = self
            .textmate_runs
            .get(key)
            .cloned()
            .unwrap_or((current_version, default_window, Vec::new()));
        let sem_runs = self
            .semantic
            .get(key)
            .map(|s| s.runs.as_slice())
            .unwrap_or(&[]);
        let version = if sem_runs.is_empty() {
            tm_version
        } else {
            tm_version.max(self.semantic.get(key).map_or(0, |s| s.version))
        };
        let runs = overlay_semantic_runs(&tm_runs, sem_runs, window);
        self.ui(SessionUpdate::Syntax { uri, generation, version, window, runs });
    }

    /// `ExtHostEditorTabs.$acceptEditorTabModel`: one group with a tab per open document.
    /// vscode-languageclient uses `window.tabGroups` to decide which documents to pull
    /// diagnostics for (found by the live OSE test).
    fn send_tab_model(&mut self) {
        let mut docs: Vec<&OpenDocument> = self.docs.values().collect();
        docs.sort_by(|a, b| a.editor_id.cmp(&b.editor_id));
        let tabs: Vec<Value> = docs
            .iter()
            .map(|d| {
                json!({
                    "id": format!("ember-tab-{}", d.editor_id),
                    "label": file_name(&d.uri.path),
                    "input": { "kind": 1, "uri": d.uri },
                    "editorId": "default",
                    "isActive": self.active.as_deref() == Some(d.uri.key().as_str()),
                    "isPinned": false,
                    "isPreview": false,
                    "isDirty": d.is_dirty(),
                })
            })
            .collect();
        let model = json!([{ "groupId": 1, "isActive": true, "viewColumn": 0, "tabs": tabs }]);
        self.rpc(Call {
            proxy: "ExtHostEditorTabs",
            method: "$acceptEditorTabModel",
            args: vec![Arg::Json(model)],
            cancellable: false,
        });
    }

    // ---- open / close / focus -----------------------------------------------------------------

    fn on_open(&mut self, uri: UriComponents) {
        let key = uri.key();
        if let Some(d) = self.docs.get(&key) {
            let u = SessionUpdate::Opened {
                uri: d.uri.clone(),
                generation: d.generation(),
                text: d.widget_text(),
                language_id: d.language_id.clone(),
                tab_width: self.indent_for(&d.language_id).0,
            };
            self.ui(u);
            self.emit_format(&key);
            self.emit_folds(&key);
            if self.textmate_runs.contains_key(&key) || self.semantic.contains_key(&key) {
                self.emit_syntax(&key);
            }
            self.touched.insert(key);
            return;
        }
        if self.opening.insert(key) {
            self.out.push(Effect::ReadFile { uri, purpose: ReadPurpose::Open });
        }
    }

    fn finish_open(&mut self, uri: UriComponents, bytes: Vec<u8>) {
        let key = uri.key();
        let (text, encoding) = match decode_detect(&bytes) {
            Ok(t) => t,
            Err(e) => {
                self.ui(SessionUpdate::OpenFailed { uri, error: e.to_string() });
                return;
            }
        };
        let language = self.languages.language_for(&uri.path);
        let mut ext_version = 1;
        if let Some((u, v)) = self.exthost_only.remove(&key) {
            // The extension host already has it (workspace.openTextDocument): remove first, then
            // add with the next version.
            let delta = DocumentsAndEditorsDelta { removed_documents: Some(vec![u]), ..Default::default() };
            self.rpc(exthost::accept_documents_and_editors_delta(&delta));
            ext_version = v + 1;
        }
        self.next_generation += 1;
        self.next_editor += 1;
        let editor_id = format!("ember-editor-{}", self.next_editor);
        let (tab_width, insert_spaces) = self.indent_for(&language);
        let (mut doc, mut calls) = OpenDocument::open(
            uri.clone(),
            &text,
            encoding == Encoding::Utf8Bom,
            &language,
            &editor_id,
            ext_version,
            self.next_generation,
            tab_width,
            insert_spaces,
        );
        calls.extend(doc.set_encoding(encoding));
        for c in calls {
            self.rpc(c);
        }
        self.ui(SessionUpdate::Opened {
            uri: uri.clone(),
            generation: doc.generation(),
            text: doc.widget_text(),
            language_id: language,
            tab_width,
        });
        self.docs.insert(key.clone(), doc);
        self.emit_format(&key);
        self.folds.insert(key.clone(), DocFolds::default());
        self.emit_folds(&key);
        self.highlight_open(&key);
        self.active = Some(key.clone());
        self.out.push(Effect::Watch { uri });
        self.send_tab_model();
        self.schedule_codelens(&key, Duration::ZERO);
        self.schedule_folding(&key, Duration::ZERO);
        self.schedule_semantic(&key, Duration::ZERO);
        self.touched.insert(key.clone());
        if let Some(text) = self.restores.remove(&key) {
            self.restore_doc(&key, &text);
        }
    }

    fn on_restore(&mut self, uri: UriComponents, text: String) {
        let key = uri.key();
        if self.docs.contains_key(&key) {
            self.restore_doc(&key, &text);
            return;
        }
        self.restores.insert(key.clone(), text);
        if self.opening.insert(key) {
            self.out.push(Effect::ReadFile { uri, purpose: ReadPurpose::Open });
        }
    }

    /// Put `text` (widget form) in the buffer of open document `key` as a new generation and tell the
    /// window whether it is dirty. Text equal to the buffer changes nothing.
    fn restore_doc(&mut self, key: &str, text: &str) {
        let normalized = text.replace("\r\n", "\n");
        match self.docs.get(key) {
            Some(doc) if doc.widget_text() != normalized => {}
            _ => return,
        }
        self.replace_doc(key, |doc, generation| doc.restore(&normalized, generation));
        if let Some(doc) = self.docs.get(key) {
            let (uri, dirty) = (doc.uri.clone(), doc.is_dirty());
            self.ui(SessionUpdate::Dirty { uri, dirty });
        }
    }

    fn on_close(&mut self, uri: &UriComponents) {
        let key = uri.key();
        self.restores.remove(&key);
        self.language_close(&key);
        let Some(doc) = self.docs.remove(&key) else { return };
        self.rpc(doc.remove_call());
        if let Some(job) = self.highlights.close(&key) {
            self.out.push(Effect::Highlight(job));
        }
        self.textmate_runs.remove(&key);
        self.folds.remove(&key);
        self.folding_due.remove(&key);
        let fold_inflight: Vec<(String, i64)> = self.folding_inflight.keys().filter(|(d, _)| *d == key).cloned().collect();
        for k in fold_inflight {
            if let Some(t) = self.folding_inflight.remove(&k) {
                self.cancel(vec![t]);
            }
        }
        if let Some(sem) = self.semantic.remove(&key) {
            if let Some((handle, result_id)) = sem.last_result {
                self.rpc(exthost::release_document_semantic_tokens(handle, result_id));
            }
        }
        self.semantic_due.remove(&key);
        let sem_inflight: Vec<(String, i64)> = self.semantic_inflight.keys().filter(|(d, _)| *d == key).cloned().collect();
        for k in sem_inflight {
            if let Some(t) = self.semantic_inflight.remove(&k) {
                self.cancel(vec![t]);
            }
        }
        self.out.push(Effect::Unwatch { uri: doc.uri.clone() });
        for (handle, cache_id) in self.lenses.close(&key) {
            self.rpc(exthost::release_code_lenses(handle, cache_id));
        }
        self.codelens_due.remove(&key);
        let inflight: Vec<(String, i64)> = self.codelens_inflight.keys().filter(|(d, _)| *d == key).cloned().collect();
        for k in inflight {
            if let Some(t) = self.codelens_inflight.remove(&k) {
                self.cancel(vec![t]);
            }
        }
        if let Some(mut st) = self.inline.remove(&key) {
            let tokens = st.cancel();
            self.cancel(tokens);
            if let Some(g) = st.ghost.take() {
                for c in inline::end_calls(&g.item, EndReason::Ignored { user_typing_disagreed: false }) {
                    self.rpc(c);
                }
            }
        }
        let (tokens, _) = self.hover.end_for(&key);
        self.cancel(tokens);
        self.ids.forget_document(&key);
        if self.diagnostics.remove_document(&key) {
            self.ui(SessionUpdate::Problems { problems: self.diagnostics.problems() });
        }
        if self.active.as_deref() == Some(key.as_str()) {
            self.active = self.docs.keys().next().cloned();
        }
        self.send_tab_model();
        self.ui(SessionUpdate::Closed { uri: doc.uri });
    }

    fn on_focus(&mut self, uri: &UriComponents) {
        let key = uri.key();
        let Some(call) = self.docs.get(&key).map(OpenDocument::focus_call) else { return };
        self.rpc(call);
        self.active = Some(key);
        self.send_tab_model();
    }

    fn on_reload(&mut self, uri: UriComponents) {
        if self.docs.contains_key(&uri.key()) {
            self.out.push(Effect::ReadFile { uri, purpose: ReadPurpose::ForceReload });
        }
    }

    // ---- widget events ------------------------------------------------------------------------

    fn on_widget(&mut self, uri: &UriComponents, generation: u64, event: WidgetEvent) {
        let key = uri.key();
        match self.docs.get(&key) {
            Some(d) if d.generation() == generation => {}
            _ => {
                tracing::debug!("ember-editor: dropping event for {key} generation {generation}: {event:?}");
                return;
            }
        }
        match event {
            WidgetEvent::CodeChanged { version, range, text } => self.on_code_changed(&key, version, range, &text),
            WidgetEvent::CodeEditRejected { request_id, base_version, current_version, .. } => {
                tracing::debug!("ember-editor: EditCode {request_id} on {key} rejected (base {base_version}, now {current_version})");
            }
            WidgetEvent::CodeHovered { decoration, pos, phase } => match phase {
                crate::widget::HoverPhase::Rest => self.on_hover_rest(&key, decoration, pos),
                crate::widget::HoverPhase::Leave => self.end_hover(),
            },
            WidgetEvent::CodeSaveRequested { .. } => self.on_save(&key),
            WidgetEvent::DecorationActivated { decoration } => self.on_activated(&key, decoration),
            WidgetEvent::SelectionChanged { active, .. } => self.on_selection_changed(&key, active),
            WidgetEvent::VisibleRangeChanged { first_line, last_line, .. } => self.on_visible_range_changed(&key, first_line, last_line),
            WidgetEvent::FoldChanged { start_line, collapsed } => {
                if self.folds.entry(key.clone()).or_default().set_fold(start_line, collapsed) {
                    self.emit_folds(&key);
                }
            }
        }
    }

    fn on_code_changed(&mut self, key: &str, version: u32, range: WidgetRange, text: &str) {
        let Some(doc) = self.docs.get_mut(key) else { return };
        let was_dirty = doc.is_dirty();
        match doc.apply_widget_change(version, range, text) {
            Ok(call) => {
                let now_dirty = doc.is_dirty();
                let uri = doc.uri.clone();
                self.rpc(call);
                if now_dirty != was_dirty {
                    self.ui(SessionUpdate::Dirty { uri, dirty: now_dirty });
                    self.send_tab_model();
                }
            }
            Err(e) => {
                let uri = doc.uri.clone();
                self.ui(SessionUpdate::Desync { uri, error: e.to_string() });
                return;
            }
        }
        self.diagnostics.on_edit(key, range, text);
        self.lenses.on_edit(key, range, text);
        self.folds.entry(key.to_owned()).or_default().on_edit(range, text);
        self.emit_folds(key);
        self.highlight_edit(key, version, range, text);
        // Monaco hides the hover on typing.
        let (tokens, shown) = self.hover.end_for(key);
        self.cancel(tokens);
        if shown.is_some() {
            if let Some(uri) = self.uri_of(key) {
                self.ui(SessionUpdate::HoverHidden { uri });
            }
        }
        self.inline_after_change(key, range, text);
        let delay = self.opts.codelens_debounce;
        self.schedule_codelens(key, delay);
        self.schedule_folding(key, delay);
        self.schedule_semantic(key, delay);
        self.touched.insert(key.to_owned());
    }

    fn on_selection_changed(&mut self, key: &str, active: WidgetPos) {
        if let Some(doc) = self.docs.get_mut(key) {
            doc.set_cursor(active);
        }
        if self.inline.get(key).and_then(|s| s.ghost.as_ref()).is_some_and(|g| g.pos != active) {
            self.end_ghost(key, EndReason::Ignored { user_typing_disagreed: false });
        }
    }

    fn on_visible_range_changed(&mut self, key: &str, first_line: u32, last_line: u32) {
        if let Some(job) = self.highlights.visible(key, first_line, last_line) {
            self.out.push(Effect::Highlight(job));
        } else if self.semantic.get(key).is_some_and(|s| !s.runs.is_empty()) && !self.textmate_runs.contains_key(key) {
            self.emit_syntax(key);
        }
        let to_resolve = self.lenses.set_visible_range(key, first_line, last_line);
        for (handle, serial, index, dto) in to_resolve {
            self.rpc_with(
                exthost::resolve_code_lens(handle, &dto),
                Pending::CodeLensResolve { doc: key.to_owned(), handle, serial, index },
            );
        }
    }

    fn on_trigger_inline(&mut self, uri: &UriComponents, generation: u64, pos: WidgetPos) {
        let key = uri.key();
        let Some(doc) = self.docs.get_mut(&key) else { return };
        if doc.generation() != generation {
            return;
        }
        doc.set_cursor(pos);
        let now = self.now;
        self.inline.entry(key).or_default().schedule(now, Duration::ZERO, pos, true);
    }

    // ---- hover --------------------------------------------------------------------------------

    fn on_hover_rest(&mut self, key: &str, decoration: u64, pos: WidgetPos) {
        let had_anchor = self.hover.active_range(key).is_some();
        let Some(doc) = self.docs.get(key) else { return };
        let (uri, generation, version, language) = (doc.uri.clone(), doc.generation(), doc.widget_version(), doc.language_id.clone());
        let lookup_pos = if decoration != 0 {
            let dec_range = match DecorationIds::space_of(decoration) {
                Some(IdSpace::Diagnostic) => self.diagnostics.range_of_id(key, decoration, doc.lines(), &mut self.ids),
                Some(IdSpace::HoverAnchor) => self.hover.range_of_id(key, decoration, &mut self.ids),
                _ => None,
            };
            match dec_range {
                Some(r) if !r.contains(pos) => r.start,
                _ => pos,
            }
        } else {
            pos
        };
        let diags = self.diagnostics.at(key, lookup_pos);
        let handles: Vec<i64> = self.registry.matching(KindTag::Hover, &uri, &language).iter().map(|p| p.handle).collect();
        let cancel = self.hover.begin(key, generation, version, lookup_pos, diags);
        self.cancel(cancel);
        if had_anchor {
            self.touched.insert(key.to_owned());
        }
        for (order, handle) in handles.into_iter().enumerate() {
            let t = self.rpc_with(exthost::provide_hover(handle, &uri, lookup_pos.to_exthost()), Pending::Hover { doc: key.to_owned() });
            self.hover.add_pending(t, order);
        }
        if let Some(p) = self.hover.initial() {
            self.ui(SessionUpdate::Hover(p));
        }
    }

    fn end_hover(&mut self) {
        let had_anchor_doc = self.docs.keys().find(|k| self.hover.active_range(k).is_some()).cloned();
        let (tokens, shown) = self.hover.end();
        self.cancel(tokens);
        if let Some(key) = had_anchor_doc {
            self.touched.insert(key);
        }
        if let Some(key) = shown {
            if let Some(uri) = self.uri_of(&key) {
                self.ui(SessionUpdate::HoverHidden { uri });
            }
        }
    }

    // ---- line ending, encoding, language (FR-L31.7) --------------------------------------------

    fn emit_format(&mut self, key: &str) {
        let Some(doc) = self.docs.get(key) else { return };
        let format = DocumentFormat {
            eol: doc.eol(),
            encoding: doc.encoding(),
            language_id: doc.language_id.clone(),
            language_name: self.languages.display_name(&doc.language_id),
        };
        let uri = doc.uri.clone();
        self.ui(SessionUpdate::Format { uri, format });
    }

    /// Code-OSS `ChangeEOLAction`: `pushEOL` on the model, which the extension host gets as a
    /// content change with `isEolChange`; the document is dirty after it.
    fn on_set_eol(&mut self, uri: &UriComponents, eol: Eol) {
        let key = uri.key();
        let Some(doc) = self.docs.get_mut(&key) else { return };
        let was_dirty = doc.is_dirty();
        let Some(call) = doc.set_eol(eol) else { return };
        let uri = doc.uri.clone();
        self.rpc(call);
        if !was_dirty {
            self.ui(SessionUpdate::Dirty { uri, dirty: true });
            self.send_tab_model();
        }
        self.emit_format(&key);
    }

    /// Code-OSS `TextFileEditorModel.setEncoding`: `Save` takes the encoding and saves; `Reopen` reads
    /// the file again decoded in it (a no-op when it is the current one). The window asks before a
    /// reopen discards unsaved edits, as Code-OSS does.
    fn on_set_encoding(&mut self, uri: &UriComponents, encoding: Encoding, mode: EncodingMode) {
        let key = uri.key();
        let Some(doc) = self.docs.get_mut(&key) else { return };
        match mode {
            EncodingMode::Save => {
                if let Some(call) = doc.set_encoding(encoding) {
                    self.rpc(call);
                }
                self.emit_format(&key);
                self.on_save(&key);
            }
            EncodingMode::Reopen => {
                if doc.encoding() == encoding {
                    return;
                }
                let uri = doc.uri.clone();
                self.reopen.insert(key, encoding);
                self.out.push(Effect::ReadFile { uri, purpose: ReadPurpose::ForceReload });
            }
        }
    }

    /// Code-OSS `ChangeLanguageAction` / `model.setLanguage`: the extension host is told with
    /// `$acceptModelLanguageChanged`; colours and lenses follow the new language.
    fn on_set_language(&mut self, uri: &UriComponents, language_id: Option<String>) {
        let key = uri.key();
        let Some(path) = self.docs.get(&key).map(|d| d.uri.path.clone()) else { return };
        let id = language_id.unwrap_or_else(|| self.languages.language_for(&path));
        let Some(doc) = self.docs.get_mut(&key) else { return };
        let calls = doc.set_language(&id);
        if calls.is_empty() {
            return;
        }
        let (uri, generation, version) = (doc.uri.clone(), doc.generation(), doc.widget_version());
        for c in calls {
            self.rpc(c);
        }
        if let Some(job) = self.highlights.close(&key) {
            self.out.push(Effect::Highlight(job));
        }
        self.ui(SessionUpdate::Syntax { uri, generation, version, window: (0, 0), runs: Vec::new() });
        self.highlight_open(&key);
        for (handle, cache_id) in self.lenses.close(&key) {
            self.rpc(exthost::release_code_lenses(handle, cache_id));
        }
        self.schedule_codelens(&key, Duration::ZERO);
        self.send_tab_model();
        self.touched.insert(key.clone());
        self.emit_format(&key);
    }

    /// `MainThreadLanguages.$changeLanguage(resource, languageId)` (`languages.setTextDocumentLanguage`):
    /// refused for an id no extension contributes, as Code-OSS does.
    fn on_change_language_request(&mut self, r: &IncomingRequest) {
        let uri: Option<UriComponents> = r.args.first().and_then(Arg::as_json).and_then(|v| serde_json::from_value(v.clone()).ok());
        let id = r.args.get(1).and_then(Arg::as_json).and_then(Value::as_str).map(str::to_owned);
        let (Some(uri), Some(id)) = (uri, id) else {
            self.respond(r.req, Err(Some(js_error("Error", "bad arguments"))));
            return;
        };
        if !self.language_ids.iter().any(|l| *l == id) {
            self.respond(r.req, Err(Some(js_error("Error", &format!("Unknown language id: {id}")))));
            return;
        }
        let key = uri.key();
        if self.docs.contains_key(&key) {
            self.on_set_language(&uri, Some(id));
        } else if let Some((u, _)) = self.exthost_only.get(&key).cloned() {
            self.rpc(exthost::activate_by_event(&format!("onLanguage:{id}")));
            self.rpc(exthost::accept_model_language_changed(&u, &id));
        }
        self.respond(r.req, Ok(Reply::Empty));
    }

    // ---- save ---------------------------------------------------------------------------------

    fn on_save(&mut self, key: &str) {
        let Some(doc) = self.docs.get(key) else { return };
        let bytes = doc.save_bytes();
        let (uri, ext_version) = (doc.uri.clone(), doc.ext_version());
        let t = self.token();
        self.saves.insert(t, (key.to_owned(), ext_version, bytes.clone()));
        self.out.push(Effect::WriteFile { uri, bytes, token: t });
    }

    fn on_file_written(&mut self, uri: &UriComponents, token: Token, result: Result<(), String>) {
        if let Some(write) = self.replace_writes.remove(&token) {
            self.on_replace_written(uri, write, result);
            return;
        }
        if self.on_workspace_edit_written(uri, token, &result) {
            return;
        }
        let Some((key, ext_version, bytes)) = self.saves.remove(&token) else { return };
        match result {
            Ok(()) => {
                let Some(doc) = self.docs.get_mut(&key) else { return };
                let calls = doc.saved(ext_version, &bytes);
                let dirty = doc.is_dirty();
                for c in calls {
                    self.rpc(c);
                }
                self.ui(SessionUpdate::Saved { uri: uri.clone() });
                self.ui(SessionUpdate::Dirty { uri: uri.clone(), dirty });
                self.send_tab_model();
            }
            Err(error) => self.ui(SessionUpdate::SaveFailed { uri: uri.clone(), error }),
        }
    }

    // ---- decoration activation / commands -----------------------------------------------------

    fn on_activated(&mut self, key: &str, decoration: u64) {
        match DecorationIds::space_of(decoration) {
            Some(IdSpace::CodeLens) => {
                if let Some((_, cmd)) = self.lenses.command_for(decoration) {
                    self.run_command(cmd);
                }
            }
            Some(IdSpace::GhostText) => self.accept_ghost(key, decoration),
            _ => {}
        }
    }

    /// Run a command from a CodeLens or an accepted inline completion. Commands the extension host
    /// registered run there; others are activated with `onCommand:<id>` first (an extension may
    /// register them on activation), and if still unknown handed to the UI.
    fn run_command(&mut self, cmd: CommandDto) {
        if cmd.id.is_empty() {
            return;
        }
        if self.registry.has_command(&cmd.id) {
            let call = exthost::execute_contributed_command(&cmd.id, &cmd.arguments);
            self.rpc_with(call, Pending::Command { id: cmd.id });
        } else {
            let call = exthost::activate_by_event(&format!("onCommand:{}", cmd.id));
            self.rpc_with(call, Pending::ActivateThenRun { command: cmd });
        }
    }

    // ---- inline completions -------------------------------------------------------------------

    fn inline_delay(&self, key: &str) -> Option<Duration> {
        let doc = self.docs.get(key)?;
        let providers = self.registry.matching(KindTag::InlineCompletions, &doc.uri, &doc.language_id);
        if providers.is_empty() {
            return None;
        }
        let ms = providers
            .iter()
            .filter_map(|p| match &p.kind {
                crate::registry::ProviderKind::InlineCompletions(m) => m.debounce_ms,
                _ => None,
            })
            .max();
        Some(ms.map(Duration::from_millis).unwrap_or(inline::DEFAULT_DEBOUNCE))
    }

    fn end_ghost(&mut self, key: &str, reason: EndReason) {
        let Some(g) = self.inline.get_mut(key).and_then(|s| s.ghost.take()) else { return };
        for c in inline::end_calls(&g.item, reason) {
            self.rpc(c);
        }
        self.touched.insert(key.to_owned());
    }

    fn inline_after_change(&mut self, key: &str, range: WidgetRange, text: &str) {
        let st = self.inline.entry(key.to_owned()).or_default();
        // A ghost completed by the previous change and not accepted: it was typed out.
        if st.ghost.as_ref().is_some_and(|g| g.completed) {
            self.end_ghost(key, EndReason::Ignored { user_typing_disagreed: false });
        }
        let st = self.inline.entry(key.to_owned()).or_default();
        let mut keep_ghost = false;
        if let Some(g) = st.ghost.as_mut() {
            match g.type_through(range, text) {
                TypeThrough::Continue => keep_ghost = true,
                TypeThrough::Completed => {}
                TypeThrough::Disagreed => {
                    self.end_ghost(key, EndReason::Ignored { user_typing_disagreed: true });
                }
            }
            self.touched.insert(key.to_owned());
        }
        let st = self.inline.entry(key.to_owned()).or_default();
        let tokens = st.cancel();
        self.cancel(tokens);
        if keep_ghost || !self.opts.inline_completions {
            return;
        }
        let Some(delay) = self.inline_delay(key) else { return };
        let Some(pos) = self.docs.get(key).and_then(OpenDocument::cursor) else { return };
        let now = self.now;
        self.inline.entry(key.to_owned()).or_default().schedule(now, delay, pos, false);
    }

    fn fire_inline(&mut self, key: &str) {
        let Some((pos, explicit)) = self.inline.get_mut(key).and_then(|s| s.trigger.take()) else { return };
        let Some(doc) = self.docs.get(key) else { return };
        let (uri, version, language) = (doc.uri.clone(), doc.widget_version(), doc.language_id.clone());
        let handles: Vec<i64> = self
            .registry
            .matching(KindTag::InlineCompletions, &uri, &language)
            .iter()
            .map(|p| p.handle)
            .collect();
        if handles.is_empty() {
            return;
        }
        let cancel = self.inline.entry(key.to_owned()).or_default().begin(version, pos);
        self.cancel(cancel);
        let ms = now_ms();
        for (order, handle) in handles.into_iter().enumerate() {
            let call = inline::provide_call(handle, &uri, pos, explicit, ms);
            let t = self.rpc_with(call, Pending::Inline { doc: key.to_owned() });
            if let Some(st) = self.inline.get_mut(key) {
                st.add_pending(t, handle, order);
            }
        }
    }

    fn on_inline_reply(&mut self, token: Token, key: &str, result: Result<Value, String>) {
        let Some(version) = self.docs.get(key).map(OpenDocument::widget_version) else { return };
        let Some(st) = self.inline.get_mut(key) else { return };
        let value = result.unwrap_or(Value::Null);
        match st.reply(token, version, value) {
            ReplyOutcome::Ignored | ReplyOutcome::Pending => {}
            ReplyOutcome::Stale { free } => {
                let tokens = st.take_pending();
                self.cancel(tokens);
                for (handle, pid) in free {
                    self.rpc(exthost::free_inline_completions_list(handle, pid, "lostRace"));
                }
            }
            ReplyOutcome::Done { pos, replies } => self.choose_ghost(key, version, pos, replies),
        }
    }

    fn choose_ghost(&mut self, key: &str, version: u32, pos: WidgetPos, replies: Vec<(i64, Value)>) {
        let line = match self.docs.get(key).and_then(|d| d.line(pos.line)) {
            Some(l) => l.to_owned(),
            None => return,
        };
        let mut chosen: Option<(inline::InlineItem, String)> = None;
        let mut frees: Vec<(i64, i64, &'static str)> = Vec::new();
        for (handle, v) in replies {
            let supports = match self.registry.get(handle).map(|p| &p.kind) {
                Some(crate::registry::ProviderKind::InlineCompletions(m)) => m.supports_handle_events,
                _ => false,
            };
            let Some((pid, items)) = inline::parse_list(&v, handle, supports) else { continue };
            if items.is_empty() {
                frees.push((handle, pid, "empty"));
                continue;
            }
            let mut taken = false;
            if chosen.is_none() {
                for item in items {
                    if let Some(ghost) = inline::ghost_for(&item, pos, &line) {
                        chosen = Some((item, ghost));
                        taken = true;
                        break;
                    }
                }
            }
            if !taken {
                frees.push((handle, pid, "notTaken"));
            }
        }
        for (handle, pid, kind) in frees {
            self.rpc(exthost::free_inline_completions_list(handle, pid, kind));
        }
        let Some((item, text)) = chosen else { return };
        // A new suggestion supersedes the one shown.
        self.end_ghost(key, EndReason::Ignored { user_typing_disagreed: false });
        let id = self.ids.id(IdSpace::GhostText, key, &format!("{}|{}|{}", item.handle, item.pid, item.idx));
        if let Some(c) = inline::did_show_call(&item) {
            self.rpc(c);
        }
        let st = self.inline.entry(key.to_owned()).or_default();
        st.ghost = Some(Ghost { item, id, pos, text, base_version: version, completed: false });
        self.touched.insert(key.to_owned());
    }

    fn accept_ghost(&mut self, key: &str, decoration: u64) {
        let Some(g) = self.inline.get(key).and_then(|s| s.ghost.clone()) else { return };
        if g.id.get() != decoration {
            return;
        }
        let Some((uri, version)) = self.docs.get(key).map(|d| (d.uri.clone(), d.widget_version())) else { return };
        if !g.completed && !g.text.is_empty() {
            // The widget did not insert the text itself: do it.
            let request_id = self.edit_request_id();
            self.ui(SessionUpdate::Command {
                uri: uri.clone(),
                command: WidgetCommand::EditCode { request_id, base_version: version, range: WidgetRange::empty(g.pos), text: g.text.clone() },
            });
        }
        for (range, text) in &g.item.additional_edits {
            let request_id = self.edit_request_id();
            self.ui(SessionUpdate::Command {
                uri: uri.clone(),
                command: WidgetCommand::EditCode { request_id, base_version: g.base_version, range: *range, text: text.clone() },
            });
        }
        self.end_ghost(key, EndReason::Accepted);
        if let Some(cmd) = g.item.command.clone() {
            self.run_command(cmd);
        }
    }

    // ---- CodeLens -----------------------------------------------------------------------------

    fn fire_codelens(&mut self, key: &str) {
        let Some(doc) = self.docs.get(key) else { return };
        let (uri, version, language) = (doc.uri.clone(), doc.widget_version(), doc.language_id.clone());
        let handles: Vec<i64> = self.registry.matching(KindTag::CodeLens, &uri, &language).iter().map(|p| p.handle).collect();
        for handle in handles {
            if let Some(old) = self.codelens_inflight.remove(&(key.to_owned(), handle)) {
                self.cancel(vec![old]);
            }
            let t = self.rpc_with(
                exthost::provide_code_lenses(handle, &uri),
                Pending::CodeLens { doc: key.to_owned(), handle, version },
            );
            self.codelens_inflight.insert((key.to_owned(), handle), t);
        }
    }

    fn on_codelens_reply(&mut self, token: Token, key: String, handle: i64, version: u32, result: Result<Value, String>) {
        if self.codelens_inflight.get(&(key.clone(), handle)) == Some(&token) {
            self.codelens_inflight.remove(&(key.clone(), handle));
        }
        let value = match result {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("ember-editor: $provideCodeLenses({handle}) failed: {e}");
                return;
            }
        };
        let current = self.docs.get(&key).map(OpenDocument::widget_version);
        if current != Some(version) {
            // Stale (the document changed or closed): release, a refresh is already scheduled.
            if let Some(c) = value.get("cacheId").and_then(Value::as_i64) {
                self.rpc(exthost::release_code_lenses(handle, c));
            }
            return;
        }
        let cap = self.opts.codelens_resolve_cap;
        let outcome = self.lenses.set_list(&key, handle, &value, cap);
        if let Some(c) = outcome.release {
            self.rpc(exthost::release_code_lenses(handle, c));
        }
        for (index, dto) in outcome.to_resolve {
            self.rpc_with(
                exthost::resolve_code_lens(handle, &dto),
                Pending::CodeLensResolve { doc: key.clone(), handle, serial: outcome.serial, index },
            );
        }
        self.touched.insert(key);
    }

    // ---- Folding & Semantic tokens (FR-E6) ----------------------------------------------------

    fn fire_folding(&mut self, key: &str) {
        let Some(doc) = self.docs.get(key) else { return };
        let (uri, version, language) = (doc.uri.clone(), doc.widget_version(), doc.language_id.clone());
        let handles: Vec<i64> = self
            .registry
            .matching(KindTag::FoldingRange, &uri, &language)
            .iter()
            .map(|p| p.handle)
            .collect();
        self.folds.entry(key.to_owned()).or_default().order = handles.clone();
        for handle in handles {
            if let Some(old) = self.folding_inflight.remove(&(key.to_owned(), handle)) {
                self.cancel(vec![old]);
            }
            let t = self.rpc_with(
                exthost::provide_folding_ranges(handle, &uri),
                Pending::Folding { doc: key.to_owned(), handle, version },
            );
            self.folding_inflight.insert((key.to_owned(), handle), t);
        }
    }

    fn on_folding_reply(&mut self, token: Token, key: String, handle: i64, version: u32, result: Result<Value, String>) {
        if self.folding_inflight.get(&(key.clone(), handle)) == Some(&token) {
            self.folding_inflight.remove(&(key.clone(), handle));
        }
        let value = match result {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("ember-editor: $provideFoldingRanges({handle}) failed: {e}");
                return;
            }
        };
        let Some((current, line_count)) = self.docs.get(&key).map(|d| (d.widget_version(), d.lines().len())) else { return };
        if current != version {
            return;
        }
        let ranges = parse_provider_folds(&value, line_count);
        let state = self.folds.entry(key.clone()).or_default();
        if !state.order.contains(&handle) {
            state.order.push(handle);
        }
        state.providers.insert(handle, ranges);
        self.emit_folds(&key);
    }

    fn fire_semantic(&mut self, key: &str) {
        let Some(doc) = self.docs.get(key) else { return };
        let (uri, version, language) = (doc.uri.clone(), doc.widget_version(), doc.language_id.clone());
        let Some(handle) = self
            .registry
            .matching(KindTag::DocumentSemanticTokens, &uri, &language)
            .first()
            .map(|p| p.handle)
        else {
            return;
        };
        if let Some(old) = self.semantic_inflight.remove(&(key.to_owned(), handle)) {
            self.cancel(vec![old]);
        }
        let t = self.rpc_with(
            exthost::provide_document_semantic_tokens(handle, &uri, 0),
            Pending::SemanticTokens { doc: key.to_owned(), handle, version },
        );
        self.semantic_inflight.insert((key.to_owned(), handle), t);
    }

    fn on_semantic_reply(&mut self, token: Token, key: String, handle: i64, version: u32, result: Result<Value, String>) {
        if self.semantic_inflight.get(&(key.clone(), handle)) == Some(&token) {
            self.semantic_inflight.remove(&(key.clone(), handle));
        }
        let value = match result {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("ember-editor: $provideDocumentSemanticTokens({handle}) failed: {e}");
                return;
            }
        };
        let Some(legend) = self.registry.get(handle).and_then(|p| match &p.kind {
            ProviderKind::DocumentSemanticTokens { legend, .. } => Some(legend.clone()),
            _ => None,
        }) else {
            return;
        };
        let Some((current, lines)) = self.docs.get(&key).map(|d| (d.widget_version(), d.lines().to_vec())) else { return };
        let Some(bytes) = Reply::buffer_from_json(&value) else {
            return;
        };
        let Some((result_id, runs)) = decode_semantic_tokens(&bytes, &legend, &lines) else {
            return;
        };
        if current != version {
            if result_id > 0 {
                self.rpc(exthost::release_document_semantic_tokens(handle, result_id));
            }
            return;
        }
        let prev = self.semantic.get_mut(&key).and_then(|s| s.last_result.take());
        if let Some((prev_handle, prev_id)) = prev {
            if prev_id > 0 {
                self.rpc(exthost::release_document_semantic_tokens(prev_handle, prev_id));
            }
        }
        let entry = self.semantic.entry(key.clone()).or_default();
        entry.version = version;
        entry.runs = runs;
        entry.last_result = (result_id > 0).then_some((handle, result_id));
        self.emit_syntax(&key);
    }

    // ---- extension-host requests --------------------------------------------------------------

    fn on_exthost_request(&mut self, r: IncomingRequest) {
        self.reply_seq += 1;
        let parsed = MainThreadCall::parse(&r);
        match parsed {
            Ok(MainThreadCall::DiagnosticsChangeMany { owner, entries }) => {
                let keys = self.diagnostics.change_many(&owner, entries);
                self.touched.extend(keys);
                self.ui(SessionUpdate::Problems { problems: self.diagnostics.problems() });
                self.respond(r.req, Ok(Reply::Empty));
            }
            Ok(MainThreadCall::DiagnosticsClear { owner }) => {
                let keys = self.diagnostics.clear(&owner);
                self.touched.extend(keys);
                self.ui(SessionUpdate::Problems { problems: self.diagnostics.problems() });
                self.respond(r.req, Ok(Reply::Empty));
            }
            Ok(MainThreadCall::ExecuteCommand { id, args }) => {
                // The extension host runs its own commands locally; what reaches us is a workbench
                // command. `setContext` (context keys) is a no-op for Ember for now.
                if id != "setContext" && !self.registry.has_command(&id) {
                    let arguments = match args {
                        Value::Array(a) => a,
                        Value::Null => Vec::new(),
                        other => vec![other],
                    };
                    self.ui(SessionUpdate::RunCommand { command: CommandDto { id, title: String::new(), tooltip: None, arguments } });
                }
                self.respond(r.req, Ok(Reply::Empty));
            }
            Ok(MainThreadCall::OutputRegister { label, .. }) => {
                self.output_seq += 1;
                let id = format!("ember-output-{}", self.output_seq);
                self.output_channels.push(crate::output::OutputChannel::new(id.clone(), label));
                self.ui(SessionUpdate::OutputChannels {
                    channels: self.output_channels.clone(),
                    active_id: Some(id.clone()),
                });
                self.respond(r.req, Ok(Reply::Json(serde_json::json!(id))));
            }
            Ok(MainThreadCall::OutputAppend { channel_id, value }) => {
                if let Some(ch) = self.output_channels.iter_mut().find(|c| c.id == channel_id) {
                    ch.append(&value);
                    self.ui(SessionUpdate::OutputChannels {
                        channels: self.output_channels.clone(),
                        active_id: None,
                    });
                }
                self.respond(r.req, Ok(Reply::Empty));
            }
            Ok(MainThreadCall::OutputUpdate { .. }) => {
                self.respond(r.req, Ok(Reply::Empty));
            }
            Ok(MainThreadCall::OutputClear { channel_id, .. }) => {
                if let Some(ch) = self.output_channels.iter_mut().find(|c| c.id == channel_id) {
                    ch.clear();
                    self.ui(SessionUpdate::OutputChannels {
                        channels: self.output_channels.clone(),
                        active_id: None,
                    });
                }
                self.respond(r.req, Ok(Reply::Empty));
            }
            Ok(MainThreadCall::OutputReveal { channel_id, .. }) => {
                self.ui(SessionUpdate::OutputChannels {
                    channels: self.output_channels.clone(),
                    active_id: Some(channel_id),
                });
                self.respond(r.req, Ok(Reply::Empty));
            }
            Ok(MainThreadCall::OutputDispose { channel_id }) => {
                self.output_channels.retain(|c| c.id != channel_id);
                self.ui(SessionUpdate::OutputChannels {
                    channels: self.output_channels.clone(),
                    active_id: None,
                });
                self.respond(r.req, Ok(Reply::Empty));
            }
            Ok(MainThreadCall::ShowMessage { severity, message, options, commands }) => {
                // Code-OSS answers when the notification closes: the chosen button's handle, or
                // `undefined`. The call stays open until the UI says (FR-L31.16).
                self.next_message_id += 1;
                let id = self.next_message_id;
                self.pending_messages.insert(id, r.req);
                self.ui(SessionUpdate::ExtensionMessage(crate::output::ExtensionMessage::from_show_message(
                    id, severity, message, &options, &commands,
                )));
            }
            Ok(MainThreadCall::RegisterTextSearchProvider { handle, scheme }) => {
                // The remote scheme wins over `file` should both ever be registered.
                if scheme == ember_editor_conn::uri::SCHEME_REMOTE || (scheme == "file" && self.search_provider.is_none()) {
                    self.search_provider = Some(handle);
                }
                self.respond(r.req, Ok(Reply::Empty));
            }
            Ok(MainThreadCall::UnregisterSearchProvider { handle }) => {
                if self.search_provider == Some(handle) {
                    self.search_provider = None;
                }
                self.respond(r.req, Ok(Reply::Empty));
            }
            Ok(MainThreadCall::HandleTextMatch { session, matches, .. }) => {
                self.respond(r.req, Ok(Reply::Empty));
                self.on_text_matches(session, matches);
            }
            Ok(MainThreadCall::Other { proxy: Some("MainThreadDocuments"), method }) if method == "$tryOpenDocument" => {
                self.on_try_open_document(&r);
            }
            Ok(MainThreadCall::Other { proxy: Some("MainThreadLanguages"), method }) if method == "$changeLanguage" => {
                self.on_change_language_request(&r);
            }
            Ok(MainThreadCall::Other { proxy: Some("MainThreadBulkEdits"), method }) if method == "$tryApplyWorkspaceEdit" => {
                self.on_try_apply_workspace_edit(&r);
            }
            Ok(MainThreadCall::Other { proxy, method }) => match self.trust.on_request(proxy, &method, r.req) {
                // Workspace trust (FR-L31.18).
                Some(crate::trust::TrustAnswer::Now(reply)) => self.respond(r.req, Ok(reply)),
                Some(crate::trust::TrustAnswer::Ask) => self.out.push(Effect::Ui(SessionUpdate::WorkspaceTrustRequested)),
                None => {
                    let reply = session_reply(proxy, &method, self.reply_seq, &self.language_ids);
                    self.respond(r.req, Ok(reply));
                }
            },
            Ok(call) => {
                let change = self.registry.apply(&call);
                self.on_registry_change(change);
                self.respond(r.req, Ok(Reply::Empty));
            }
            Err(e) => {
                tracing::warn!("ember-editor: cannot decode {:?}.{}: {e}", r.proxy, r.method);
                let reply = session_reply(r.proxy, &r.method, self.reply_seq, &self.language_ids);
                self.respond(r.req, Ok(reply));
            }
        }
    }

    fn on_registry_change(&mut self, change: RegistryChange) {
        let keys: Vec<String> = self.docs.keys().cloned().collect();
        match change {
            RegistryChange::Registered { kind: KindTag::CodeLens, .. } | RegistryChange::CodeLensChanged { .. } => {
                let delay = self.opts.codelens_debounce;
                for k in keys {
                    self.schedule_codelens(&k, delay);
                }
            }
            RegistryChange::Unregistered { handle, kind: KindTag::CodeLens } => {
                // The provider is disposed in the extension host together with its cached lists.
                self.lenses.remove_provider(handle);
                self.touched.extend(keys);
            }
            RegistryChange::Registered { kind: KindTag::FoldingRange, .. } | RegistryChange::FoldingRangeChanged { .. } => {
                for k in keys {
                    self.schedule_folding(&k, Duration::ZERO);
                }
            }
            RegistryChange::Unregistered { handle, kind: KindTag::FoldingRange } => {
                for k in keys {
                    if let Some(st) = self.folds.get_mut(&k) {
                        st.providers.remove(&handle);
                        st.order.retain(|h| *h != handle);
                    }
                    self.emit_folds(&k);
                }
            }
            RegistryChange::Registered { kind: KindTag::DocumentSemanticTokens, .. } | RegistryChange::SemanticTokensChanged { .. } => {
                for k in keys {
                    self.schedule_semantic(&k, Duration::ZERO);
                }
            }
            RegistryChange::Unregistered { handle, kind: KindTag::DocumentSemanticTokens } => {
                for k in keys {
                    if self.semantic.get(&k).and_then(|s| s.last_result).is_some_and(|(h, _)| h == handle) {
                        self.semantic.remove(&k);
                        self.emit_syntax(&k);
                    }
                }
            }
            RegistryChange::Unregistered { handle, kind: KindTag::InlineCompletions } => {
                for k in keys {
                    if let Some(st) = self.inline.get_mut(&k) {
                        if st.ghost.as_ref().is_some_and(|g| g.item.handle == handle) {
                            st.ghost = None;
                            self.touched.insert(k);
                        }
                    }
                }
            }
            RegistryChange::InlineCompletionsChanged { .. } => {
                if !self.opts.inline_completions {
                    return;
                }
                if let Some(k) = self.active.clone() {
                    if let (Some(delay), Some(pos)) = (self.inline_delay(&k), self.docs.get(&k).and_then(OpenDocument::cursor)) {
                        let now = self.now;
                        self.inline.entry(k).or_default().schedule(now, delay, pos, false);
                    }
                }
            }
            _ => {}
        }
    }

    /// `$tryOpenDocument(uri, options?)` → the URI once the document is in the extension host.
    fn on_try_open_document(&mut self, r: &IncomingRequest) {
        let uri: Option<UriComponents> = r.args.first().and_then(Arg::as_json).and_then(|v| serde_json::from_value(v.clone()).ok());
        let Some(uri) = uri else {
            self.respond(r.req, Err(Some(js_error("Error", "bad uri"))));
            return;
        };
        let key = uri.key();
        if self.docs.contains_key(&key) || self.exthost_only.contains_key(&key) {
            self.respond(r.req, Ok(Reply::Json(serde_json::to_value(&uri).unwrap_or(Value::Null))));
        } else {
            self.out.push(Effect::ReadFile { uri, purpose: ReadPurpose::ExtHostOpen { req: r.req } });
        }
    }

    fn finish_exthost_open(&mut self, uri: UriComponents, req: u32, result: Result<Vec<u8>, String>) {
        let key = uri.key();
        if self.docs.contains_key(&key) || self.exthost_only.contains_key(&key) {
            self.respond(req, Ok(Reply::Json(serde_json::to_value(&uri).unwrap_or(Value::Null))));
            return;
        }
        let text = match result.and_then(|b| decode_detect(&b).map_err(|e| e.to_string())) {
            Ok((t, _)) => t,
            Err(e) => {
                self.respond(req, Err(Some(js_error("Error", &format!("cannot open {}: {e}", uri.path)))));
                return;
            }
        };
        let language = self.languages.language_for(&uri.path);
        let mirror = DocumentMirror::new(uri.clone(), &text, language.as_str());
        let delta = DocumentsAndEditorsDelta { added_documents: Some(vec![mirror.added_data()]), ..Default::default() };
        self.rpc(exthost::activate_by_event(&format!("onLanguage:{language}")));
        self.rpc(exthost::accept_documents_and_editors_delta(&delta));
        self.exthost_only.insert(key, (uri.clone(), mirror.version()));
        self.respond(req, Ok(Reply::Json(serde_json::to_value(&uri).unwrap_or(Value::Null))));
    }

    // ---- RPC replies --------------------------------------------------------------------------

    fn on_rpc_reply(&mut self, token: Token, result: Result<Value, String>) {
        let Some(p) = self.pending.remove(&token) else { return };
        match p {
            Pending::CodeLens { doc, handle, version } => self.on_codelens_reply(token, doc, handle, version, result),
            Pending::CodeLensResolve { doc, handle, serial, index } => {
                if let Ok(v) = result {
                    if self.lenses.resolved(&doc, handle, serial, index, &v) {
                        self.touched.insert(doc);
                    }
                }
            }
            Pending::Folding { doc, handle, version } => self.on_folding_reply(token, doc, handle, version, result),
            Pending::SemanticTokens { doc, handle, version } => self.on_semantic_reply(token, doc, handle, version, result),
            Pending::Hover { doc } => {
                let Some(version) = self.docs.get(&doc).map(OpenDocument::widget_version) else { return };
                let prev_range = self.hover.active_range(&doc);
                if let Some(p) = self.hover.reply(token, version, result.as_ref().map_err(|_| ())) {
                    self.ui(SessionUpdate::Hover(p));
                }
                if self.hover.active_range(&doc) != prev_range {
                    self.touched.insert(doc);
                }
            }
            Pending::Inline { doc } => self.on_inline_reply(token, &doc, result),
            Pending::ActivateThenRun { command } => {
                if self.registry.has_command(&command.id) {
                    let call = exthost::execute_contributed_command(&command.id, &command.arguments);
                    self.rpc_with(call, Pending::Command { id: command.id });
                } else {
                    self.ui(SessionUpdate::RunCommand { command });
                }
            }
            Pending::Command { id } => {
                if let Err(error) = result {
                    tracing::warn!("ember-editor: command {id} failed: {error}");
                    self.ui(SessionUpdate::CommandFailed { id, error });
                }
            }
            Pending::Search { id } => self.on_search_reply(id, result),
            Pending::Language(pending) => self.on_language_reply(token, pending, result),
        }
    }

    // ---- files --------------------------------------------------------------------------------

    fn on_file_read(&mut self, uri: UriComponents, purpose: ReadPurpose, result: Result<Vec<u8>, String>) {
        match purpose {
            ReadPurpose::Open => {
                self.opening.remove(&uri.key());
                match result {
                    Ok(bytes) => self.finish_open(uri, bytes),
                    Err(error) => {
                        self.restores.remove(&uri.key());
                        self.ui(SessionUpdate::OpenFailed { uri, error })
                    }
                }
            }
            ReadPurpose::Reload => {
                let key = uri.key();
                let Ok(bytes) = result else { return };
                let Some(doc) = self.docs.get(&key) else { return };
                let (text, encoding) = match decode_reload(&bytes, doc.encoding()) {
                    Ok(t) => t,
                    Err(_) => return,
                };
                if doc.matches_disk(&text) || doc.matches_buffer(&text) {
                    return; // our own save, or nothing new
                }
                if doc.is_dirty() {
                    let uri = doc.uri.clone();
                    self.ui(SessionUpdate::ExternalChange { uri, change: ExternalChange::ChangedWhileDirty });
                    return;
                }
                self.reset_doc(&key, &text, Some(encoding));
            }
            ReadPurpose::ForceReload => {
                let key = uri.key();
                // Reopen with an encoding (FR-L31.7) decodes exactly as chosen; a failure leaves the document as it was.
                let reopen = self.reopen.remove(&key);
                let bytes = match result {
                    Ok(b) => b,
                    Err(error) => {
                        if reopen.is_some() {
                            self.ui(SessionUpdate::FormatFailed { uri, error });
                        }
                        return;
                    }
                };
                let Some(current) = self.docs.get(&key).map(OpenDocument::encoding) else { return };
                let decoded = match reopen {
                    Some(enc) => decode_as(&bytes, enc).map(|t| (t, enc)),
                    None => decode_reload(&bytes, current),
                };
                let (text, encoding) = match decoded {
                    Ok(t) => t,
                    Err(e) => {
                        if reopen.is_some() {
                            self.ui(SessionUpdate::FormatFailed { uri, error: e.to_string() });
                        }
                        return;
                    }
                };
                self.reset_doc(&key, &text, Some(encoding));
                self.ui(SessionUpdate::Dirty { uri, dirty: false });
            }
            ReadPurpose::ExtHostOpen { req } => self.finish_exthost_open(uri, req, result),
            ReadPurpose::Grammar { scope } => self.on_grammar_read(&scope, &uri, result),
            ReadPurpose::Replace { id } => self.on_replace_read(id, uri, result),
            ReadPurpose::WorkspaceEdit { id } => self.on_workspace_edit_read(id, uri, result),
        }
    }

    // ---- search and replace (FR-E7) -----------------------------------------------------------

    fn on_search(&mut self, id: u64, request: SearchRequest, folder: UriComponents) {
        if let Some(old) = self.search.take() {
            self.cancel(vec![old.token]);
            self.ui(SessionUpdate::SearchDone { id: old.id, end: SearchEnd::Cancelled });
        }
        let Some(handle) = self.search_provider else {
            self.ui(SessionUpdate::SearchDone { id, end: SearchEnd::Unavailable });
            return;
        };
        let query = request.query(folder.clone());
        // Open documents with unsaved edits are searched in their buffer, as Code-OSS does.
        let mut overlaid = HashSet::new();
        let mut local: Vec<FileHits> = Vec::new();
        if let Ok(matcher) = crate::search::Matcher::new(&request.options) {
            let excludes = query.effective_excludes();
            let mut keys: Vec<&String> = self.docs.keys().collect();
            keys.sort();
            for key in keys {
                let doc = &self.docs[key];
                if !doc.is_dirty() {
                    continue;
                }
                let Some(relative) = crate::search::relative_to(&folder.path, &doc.uri.path) else { continue };
                if !crate::search::path_included(relative, &query.include, &excludes) {
                    continue;
                }
                overlaid.insert(key.clone());
                let hits = matcher.hits_in_lines(doc.lines());
                if !hits.is_empty() {
                    local.push(FileHits { uri: doc.uri.clone(), hits });
                }
            }
        }
        self.search_session += 1;
        let session = self.search_session;
        let token = self.rpc_with(ember_editor_conn::search::provide_text_search_results(handle, session, &query), Pending::Search { id });
        self.search = Some(ActiveSearch { id, session, token, overlaid });
        if !local.is_empty() {
            self.ui(SessionUpdate::SearchHits { id, files: local });
        }
    }

    fn on_cancel_search(&mut self, id: u64) {
        if self.search.as_ref().is_some_and(|s| s.id == id) {
            if let Some(s) = self.search.take() {
                self.cancel(vec![s.token]);
            }
            self.ui(SessionUpdate::SearchDone { id, end: SearchEnd::Cancelled });
        }
    }

    fn on_text_matches(&mut self, session: i64, matches: Vec<ember_editor_conn::search::FileMatch>) {
        let Some(active) = self.search.as_ref().filter(|s| s.session == session) else { return };
        let id = active.id;
        let kept: Vec<_> = matches.into_iter().filter(|m| !active.overlaid.contains(&m.resource.key())).collect();
        let files = crate::search::hits_from_matches(kept);
        if !files.is_empty() {
            self.ui(SessionUpdate::SearchHits { id, files });
        }
    }

    fn on_search_reply(&mut self, id: u64, result: Result<Value, String>) {
        if self.search.as_ref().is_some_and(|s| s.id == id) {
            self.search = None;
        }
        let end = match result {
            Ok(v) => SearchEnd::Complete { limit_hit: ember_editor_conn::search::SearchComplete::from_json(&v).limit_hit },
            Err(e) if e.contains("Canceled") => SearchEnd::Cancelled,
            Err(e) => SearchEnd::Failed(e),
        };
        self.ui(SessionUpdate::SearchDone { id, end });
    }

    fn on_replace(&mut self, id: u64, request: ReplaceRequest) {
        let mut targets: HashMap<String, Vec<Target>> = HashMap::new();
        let mut uris: Vec<UriComponents> = Vec::new();
        for (uri, t) in &request.files {
            if t.is_empty() {
                continue;
            }
            let key = uri.key();
            if !targets.contains_key(&key) {
                uris.push(uri.clone());
            }
            targets.entry(key).or_default().extend(t.iter().copied());
        }
        if uris.is_empty() {
            self.ui(SessionUpdate::ReplaceDone { id, report: ReplaceReport::default() });
            return;
        }
        let job = ReplaceJob { request, targets, remaining: uris.len(), report: ReplaceReport::default() };
        self.replaces.insert(id, job);
        for uri in uris {
            let key = uri.key();
            match self.docs.get(&key) {
                // An open document is replaced in its buffer, unsaved edits included (Code-OSS saves it after).
                Some(doc) => {
                    let (text, bom) = match decode(&doc.save_bytes()) {
                        Ok(t) => t,
                        Err(e) => {
                            self.replace_file_failed(id, &uri, e.to_string());
                            continue;
                        }
                    };
                    let version = doc.ext_version();
                    self.replace_text(id, uri, &text, bom, Some(version));
                }
                None => self.out.push(Effect::ReadFile { uri, purpose: ReadPurpose::Replace { id } }),
            }
        }
    }

    fn on_replace_read(&mut self, id: u64, uri: UriComponents, result: Result<Vec<u8>, String>) {
        if !self.replaces.contains_key(&id) {
            return;
        }
        let decoded = result.and_then(|bytes| decode(&bytes).map_err(|e| e.to_string()));
        match decoded {
            Ok((text, bom)) => self.replace_text(id, uri, &text, bom, None),
            Err(e) => self.replace_file_failed(id, &uri, e),
        }
    }

    /// Replace in one file's text and write it; `open` is the open document's version the text came from.
    fn replace_text(&mut self, id: u64, uri: UriComponents, text: &str, bom: bool, open: Option<u64>) {
        let key = uri.key();
        let Some(job) = self.replaces.get(&id) else { return };
        let targets = job.targets.get(&key).cloned().unwrap_or_default();
        match crate::search::replace_in_text(text, &job.request.options, &job.request.replacement, &targets) {
            Ok(r) if r.replaced == 0 => {
                if let Some(job) = self.replaces.get_mut(&id) {
                    job.report.skipped += r.skipped;
                }
                self.replace_file_done(id);
            }
            Ok(r) => {
                let mut bytes = Vec::with_capacity(r.text.len() + 3);
                if bom {
                    bytes.extend_from_slice("\u{feff}".as_bytes());
                }
                bytes.extend_from_slice(r.text.as_bytes());
                let token = self.token();
                let write = ReplaceWrite { id, key, replaced: r.replaced, skipped: r.skipped, open: open.map(|v| (v, r.text)) };
                self.replace_writes.insert(token, write);
                self.out.push(Effect::WriteFile { uri, bytes, token });
            }
            Err(e) => self.replace_file_failed(id, &uri, e),
        }
    }

    fn on_replace_written(&mut self, uri: &UriComponents, write: ReplaceWrite, result: Result<(), String>) {
        match result {
            Ok(()) => {
                if let Some(job) = self.replaces.get_mut(&write.id) {
                    job.report.replaced += write.replaced;
                    job.report.skipped += write.skipped;
                    job.report.files += 1;
                }
                // The buffer takes the new text only now that it is on disk, and only if nobody typed meanwhile;
                // otherwise the watcher reports the change like any other.
                if let Some((version, text)) = write.open {
                    if self.docs.get(&write.key).is_some_and(|d| d.ext_version() == version) {
                        self.reset_doc(&write.key, &text, None);
                        self.ui(SessionUpdate::Dirty { uri: uri.clone(), dirty: false });
                        self.ui(SessionUpdate::Saved { uri: uri.clone() });
                    }
                }
                self.replace_file_done(write.id);
            }
            Err(e) => {
                if let Some(job) = self.replaces.get_mut(&write.id) {
                    job.report.skipped += write.replaced + write.skipped;
                    job.report.failures.push((uri.path.clone(), e));
                }
                self.replace_file_done(write.id);
            }
        }
    }

    fn replace_file_failed(&mut self, id: u64, uri: &UriComponents, error: String) {
        if let Some(job) = self.replaces.get_mut(&id) {
            job.report.skipped += job.targets.get(&uri.key()).map_or(0, Vec::len);
            job.report.failures.push((uri.path.clone(), error));
        }
        self.replace_file_done(id);
    }

    fn replace_file_done(&mut self, id: u64) {
        let finished = match self.replaces.get_mut(&id) {
            Some(job) => {
                job.remaining = job.remaining.saturating_sub(1);
                job.remaining == 0
            }
            None => false,
        };
        if finished {
            if let Some(job) = self.replaces.remove(&id) {
                self.ui(SessionUpdate::ReplaceDone { id, report: job.report });
            }
        }
    }

    // ---- syntax highlighting ------------------------------------------------------------------

    /// Turns what [`Highlights`] asks for into effects. `like` is any URI of the server: grammar files live on
    /// the same machine as the document.
    fn run_highlight_actions(&mut self, like: &UriComponents, actions: Vec<HighlightAction>) {
        for action in actions {
            match action {
                HighlightAction::Read { scope, path } => {
                    let uri = UriComponents::remote(&like.authority, &path);
                    self.out.push(Effect::ReadFile { uri, purpose: ReadPurpose::Grammar { scope } });
                }
                HighlightAction::Job(job) => self.out.push(Effect::Highlight(job)),
                HighlightAction::Off { key, why } => tracing::debug!("ember-editor: no colours for {key}: {why}"),
            }
        }
    }

    fn highlight_build(&mut self, key: &str) {
        let Some(doc) = self.docs.get(key) else { return };
        let (text, version) = (doc.lines().join("\n"), doc.widget_version());
        if let Some(job) = self.highlights.build_job(key, text, version) {
            self.out.push(Effect::Highlight(job));
        }
    }

    /// A document was opened, or its text replaced (reload, resync): colour it from the start.
    fn highlight_open(&mut self, key: &str) {
        let Some(doc) = self.docs.get(key) else { return };
        let (language, like) = (doc.language_id.clone(), doc.uri.clone());
        let (actions, ready) = self.highlights.open(key, &language);
        self.run_highlight_actions(&like, actions);
        if ready {
            self.highlight_build(key);
        }
    }

    fn highlight_edit(&mut self, key: &str, version: u32, range: WidgetRange, text: &str) {
        if let Some(job) = self.highlights.edit(key, range, text, version) {
            self.out.push(Effect::Highlight(job));
        }
    }

    fn on_grammar_read(&mut self, scope: &str, uri: &UriComponents, result: Result<Vec<u8>, String>) {
        let result = result.and_then(|bytes| {
            if bytes.len() as u64 > crate::textmate::MAX_GRAMMAR_BYTES {
                return Err("the grammar file is too large".to_owned());
            }
            String::from_utf8(bytes).map_err(|_| "not UTF-8".to_owned())
        });
        let (actions, ready) = self.highlights.grammar_read(scope, &uri.path, result);
        self.run_highlight_actions(uri, actions);
        for key in ready {
            self.highlight_build(&key);
        }
    }

    fn on_highlighted(&mut self, output: HighlightOutput) {
        if !self.highlights.accept(&output) {
            return;
        }
        let key = output.key.clone();
        // Runs written against a version the document has not reached cannot be right, and ones far behind it
        // are past what the widget can move them across: neither is shown.
        if let Some(doc) = self.docs.get(&key) {
            let current = doc.widget_version();
            if output.problem.is_none() && (output.version > current || current - output.version > MAX_SPAN_VERSION_LAG) {
                tracing::debug!("ember-editor: dropped runs for version {} of {key} (document at {current})", output.version);
                return;
            }
        }
        match output.problem {
            Some(Problem::Rebuild) => {
                self.highlights.rebuild(&key);
                self.highlight_build(&key);
            }
            Some(Problem::Off(why)) => {
                tracing::debug!("ember-editor: no colours for {key}: {why}");
                self.highlights.turn_off(&key);
                self.textmate_runs.remove(&key);
                self.emit_syntax(&key);
            }
            None => {
                self.textmate_runs.insert(key.clone(), (output.version, output.window, output.runs));
                self.emit_syntax(&key);
            }
        }
    }

    fn on_files_changed(&mut self, changes: Vec<FileChange>) {
        for c in changes {
            // Match by path: the server's URI transformer decides the authority it sends back.
            let Some(uri) = self.docs.values().find(|d| d.uri.path == c.resource.path).map(|d| d.uri.clone()) else { continue };
            match c.kind {
                FileChangeType::Deleted => self.ui(SessionUpdate::ExternalChange { uri, change: ExternalChange::Deleted }),
                FileChangeType::Updated | FileChangeType::Added => {
                    self.out.push(Effect::ReadFile { uri, purpose: ReadPurpose::Reload })
                }
            }
        }
    }

    /// Replace a document's content wholesale (reload from disk, resync). `encoding: None` keeps the
    /// current encoding.
    fn reset_doc(&mut self, key: &str, text: &str, encoding: Option<Encoding>) {
        self.replace_doc(key, |doc, generation| {
            let encoding = encoding.unwrap_or_else(|| doc.encoding());
            doc.reset(text, encoding, generation)
        });
    }

    /// Replace a document's content with what `replace` puts in it, as a new widget generation: the
    /// extension host is re-announced, the overlays of the old text end, and the widget gets `SetText`.
    fn replace_doc(&mut self, key: &str, replace: impl FnOnce(&mut OpenDocument, u64) -> Vec<Call>) {
        self.next_generation += 1;
        let generation = self.next_generation;
        let Some(doc) = self.docs.get_mut(key) else { return };
        let calls = replace(doc, generation);
        let (uri, widget_text) = (doc.uri.clone(), doc.widget_text());
        for c in calls {
            self.rpc(c);
        }
        for (handle, cache_id) in self.lenses.close(key) {
            self.rpc(exthost::release_code_lenses(handle, cache_id));
        }
        self.textmate_runs.remove(key);
        if let Some(sem) = self.semantic.remove(key) {
            if let Some((handle, result_id)) = sem.last_result {
                self.rpc(exthost::release_document_semantic_tokens(handle, result_id));
            }
        }
        let fold_inflight: Vec<(String, i64)> = self.folding_inflight.keys().filter(|(d, _)| d == key).cloned().collect();
        for k in fold_inflight {
            if let Some(t) = self.folding_inflight.remove(&k) {
                self.cancel(vec![t]);
            }
        }
        let sem_inflight: Vec<(String, i64)> = self.semantic_inflight.keys().filter(|(d, _)| d == key).cloned().collect();
        for k in sem_inflight {
            if let Some(t) = self.semantic_inflight.remove(&k) {
                self.cancel(vec![t]);
            }
        }
        if let Some(st) = self.inline.get_mut(key) {
            let tokens = st.cancel();
            self.cancel(tokens);
        }
        self.end_ghost(key, EndReason::Ignored { user_typing_disagreed: false });
        let (tokens, shown) = self.hover.end_for(key);
        self.cancel(tokens);
        if shown.is_some() {
            self.ui(SessionUpdate::HoverHidden { uri: uri.clone() });
        }
        self.ids.forget_document(key);
        self.ui(SessionUpdate::Command { uri, command: WidgetCommand::SetText { generation, text: widget_text } });
        self.emit_format(key);
        self.folds.entry(key.to_owned()).or_default().providers.clear();
        self.emit_folds(key);
        self.highlight_open(key);
        self.send_tab_model();
        self.schedule_codelens(key, Duration::ZERO);
        self.schedule_folding(key, Duration::ZERO);
        self.schedule_semantic(key, Duration::ZERO);
        self.touched.insert(key.to_owned());
    }

    // ---- timers and decoration flush ----------------------------------------------------------

    fn run_timers(&mut self) {
        let now = self.now;
        let due_inline: Vec<String> =
            self.inline.iter_mut().filter_map(|(k, s)| s.debounce.take_due(now).then(|| k.clone())).collect();
        for k in due_inline {
            self.fire_inline(&k);
        }
        let due_lens: Vec<String> =
            self.codelens_due.iter_mut().filter_map(|(k, d)| d.take_due(now).then(|| k.clone())).collect();
        for k in due_lens {
            self.fire_codelens(&k);
        }
        let due_fold: Vec<String> =
            self.folding_due.iter_mut().filter_map(|(k, d)| d.take_due(now).then(|| k.clone())).collect();
        for k in due_fold {
            self.fire_folding(&k);
        }
        let due_sem: Vec<String> =
            self.semantic_due.iter_mut().filter_map(|(k, d)| d.take_due(now).then(|| k.clone())).collect();
        for k in due_sem {
            self.fire_semantic(&k);
        }
    }

    /// The `Decorations` prop of every document whose decorations may have changed.
    fn flush_decorations(&mut self) {
        let touched = std::mem::take(&mut self.touched);
        for key in touched {
            let Some(doc) = self.docs.get(&key) else { continue };
            let version = doc.widget_version();
            let mut decorations = self.diagnostics.decorations(&key, version, doc.lines(), &mut self.ids);
            decorations.extend(self.lenses.decorations(&key, version, doc.lines(), &mut self.ids));
            let mut live_hover = HashSet::new();
            if let Some(range) = self.hover.active_range(&key) {
                let clamped = crate::coords::clamp_range(doc.lines(), range);
                let id = self.ids.id(
                    IdSpace::HoverAnchor,
                    &key,
                    &format!("hover|{}:{}..{}:{}", range.start.line, range.start.col, range.end.line, range.end.col),
                );
                live_hover.insert(id.get());
                decorations.push(Decoration {
                    id,
                    version,
                    kind: DecorationKind::HoverAnchor,
                    severity: None,
                    color: 0,
                    range: clamped,
                    text: String::new(),
                });
            }
            self.ids.retain(IdSpace::HoverAnchor, &key, &live_hover);
            if let Some(g) = self.inline.get(&key).and_then(|s| s.ghost.as_ref()) {
                if !g.text.is_empty() {
                    decorations.push(Decoration {
                        id: g.id,
                        version,
                        kind: DecorationKind::GhostText,
                        severity: None,
                        color: 0,
                        range: WidgetRange::empty(g.pos),
                        text: g.text.clone(),
                    });
                }
            }
            decorations.sort_by(|a, b| a.range.cmp(&b.range).then(a.kind.cmp(&b.kind)).then(a.id.cmp(&b.id)));
            let (uri, generation) = (doc.uri.clone(), doc.generation());
            self.out.push(Effect::Ui(SessionUpdate::Decorations { uri, generation, decorations }));
        }
    }
}
