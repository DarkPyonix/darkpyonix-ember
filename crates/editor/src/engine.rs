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
use crate::hover::{HoverPopup, HoverState};
use crate::ids::{DecorationIds, IdSpace};
use crate::inline::{self, EndReason, Ghost, InlineDoc, ReplyOutcome, TypeThrough};
use crate::languages::LanguageRegistry;
use crate::registry::{KindTag, Registry, RegistryChange};
use crate::replies::session_reply;
use crate::widget::{Decoration, DecorationKind, WidgetCommand, WidgetEvent};

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
    /// `MainThreadDocuments.$tryOpenDocument` from the extension host (`workspace.openTextDocument`).
    ExtHostOpen { req: u32 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Open { uri: UriComponents },
    Close { uri: UriComponents },
    /// The editor of `uri` became the active one.
    Focus { uri: UriComponents },
    /// An event from the `CodeEditor` node rendered with key `generation`.
    Widget { uri: UriComponents, generation: u64, event: WidgetEvent },
    /// Explicit inline-completion request at `pos` (a keyboard command).
    TriggerInline { uri: UriComponents, generation: u64, pos: WidgetPos },
    ExtHostRequest(IncomingRequest),
    RpcReply { token: Token, result: Result<Value, String> },
    FileRead { uri: UriComponents, purpose: ReadPurpose, result: Result<Vec<u8>, String> },
    FileWritten { uri: UriComponents, token: Token, result: Result<(), String> },
    FilesChanged(Vec<FileChange>),
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
    /// A command neither the extension host nor Ember implements (a workbench command such as
    /// `editor.action.showReferences`); the UI may handle it or ignore it.
    RunCommand { command: CommandDto },
    /// The widget and the session disagree about the document (lost event, bad range). The UI
    /// should call `EditorSession::resync` (re-sends the session's text as a new generation).
    Desync { uri: UriComponents, error: String },
    Closed { uri: UriComponents },
    /// The extension-host or management connection is gone.
    ConnectionLost { reason: String },
}

#[derive(Debug, Clone)]
enum Pending {
    CodeLens { doc: String, handle: i64, version: u32 },
    CodeLensResolve { doc: String, handle: i64, serial: u64, index: usize },
    Hover { doc: String },
    Inline { doc: String },
    /// `$activateByEvent("onCommand:<id>")` before running a command not registered yet.
    ActivateThenRun { command: CommandDto },
    Command { id: String },
}

pub struct SessionCore {
    opts: CoreOptions,
    languages: LanguageRegistry,
    language_ids: Vec<String>,
    docs: HashMap<String, OpenDocument>,
    opening: HashSet<String>,
    /// Documents announced to the extension host only (for `$tryOpenDocument`): key → (uri, version).
    exthost_only: HashMap<String, (UriComponents, u64)>,
    active: Option<String>,
    registry: Registry,
    diagnostics: Diagnostics,
    lenses: CodeLenses,
    codelens_due: HashMap<String, inline::Debouncer>,
    codelens_inflight: HashMap<(String, i64), Token>,
    hover: HoverState,
    inline: HashMap<String, InlineDoc>,
    ids: DecorationIds,
    pending: HashMap<Token, Pending>,
    saves: HashMap<Token, (String, u64, Vec<u8>)>,
    next_token: Token,
    next_generation: u64,
    next_editor: u64,
    next_edit_request: u32,
    reply_seq: u64,
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
            languages,
            language_ids,
            docs: HashMap::new(),
            opening: HashSet::new(),
            exthost_only: HashMap::new(),
            active: None,
            registry: Registry::new(),
            diagnostics: Diagnostics::new(),
            lenses: CodeLenses::new(),
            codelens_due: HashMap::new(),
            codelens_inflight: HashMap::new(),
            hover: HoverState::new(),
            inline: HashMap::new(),
            ids: DecorationIds::new(),
            pending: HashMap::new(),
            saves: HashMap::new(),
            next_token: 0,
            next_generation: 0,
            next_editor: 0,
            next_edit_request: 0,
            reply_seq: 0,
            touched: BTreeSet::new(),
            out: Vec::new(),
            now,
        }
    }

    /// Process one input at time `now`; returns the effects in the order they must be executed.
    pub fn handle(&mut self, now: Instant, input: Input) -> Vec<Effect> {
        self.now = now;
        match input {
            Input::Open { uri } => self.on_open(uri),
            Input::Close { uri } => self.on_close(&uri),
            Input::Focus { uri } => self.on_focus(&uri),
            Input::Widget { uri, generation, event } => self.on_widget(&uri, generation, event),
            Input::TriggerInline { uri, generation, pos } => self.on_trigger_inline(&uri, generation, pos),
            Input::ExtHostRequest(r) => self.on_exthost_request(r),
            Input::RpcReply { token, result } => self.on_rpc_reply(token, result),
            Input::FileRead { uri, purpose, result } => self.on_file_read(uri, purpose, result),
            Input::FileWritten { uri, token, result } => self.on_file_written(&uri, token, result),
            Input::FilesChanged(changes) => self.on_files_changed(changes),
            Input::Tick => {}
        }
        self.run_timers();
        self.flush_decorations();
        std::mem::take(&mut self.out)
    }

    /// When [`Input::Tick`] must be delivered next (earliest debounce deadline).
    pub fn next_wake(&self) -> Option<Instant> {
        let a = self.inline.values().filter_map(|s| s.debounce.deadline());
        let b = self.codelens_due.values().filter_map(|d| d.deadline());
        a.chain(b).min()
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
                tab_width: self.opts.tab_width,
            };
            self.ui(u);
            self.touched.insert(key);
            return;
        }
        if self.opening.insert(key) {
            self.out.push(Effect::ReadFile { uri, purpose: ReadPurpose::Open });
        }
    }

    fn finish_open(&mut self, uri: UriComponents, bytes: Vec<u8>) {
        let key = uri.key();
        let (text, bom) = match decode(&bytes) {
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
        let (doc, calls) = OpenDocument::open(
            uri.clone(),
            &text,
            bom,
            &language,
            &editor_id,
            ext_version,
            self.next_generation,
            self.opts.tab_width,
            self.opts.insert_spaces,
        );
        for c in calls {
            self.rpc(c);
        }
        self.ui(SessionUpdate::Opened {
            uri: uri.clone(),
            generation: doc.generation(),
            text: doc.widget_text(),
            language_id: language,
            tab_width: self.opts.tab_width,
        });
        self.docs.insert(key.clone(), doc);
        self.active = Some(key.clone());
        self.out.push(Effect::Watch { uri });
        self.send_tab_model();
        self.schedule_codelens(&key, Duration::ZERO);
        self.touched.insert(key);
    }

    fn on_close(&mut self, uri: &UriComponents) {
        let key = uri.key();
        let Some(doc) = self.docs.remove(&key) else { return };
        self.rpc(doc.remove_call());
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
            WidgetEvent::CodeHovered { pos, phase, .. } => match phase {
                crate::widget::HoverPhase::Rest => self.on_hover_rest(&key, pos),
                crate::widget::HoverPhase::Leave => self.end_hover(),
            },
            WidgetEvent::CodeSaveRequested { .. } => self.on_save(&key),
            WidgetEvent::DecorationActivated { decoration } => self.on_activated(&key, decoration),
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
        self.touched.insert(key.to_owned());
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

    fn on_hover_rest(&mut self, key: &str, pos: WidgetPos) {
        let Some(doc) = self.docs.get(key) else { return };
        let (uri, generation, version, language) = (doc.uri.clone(), doc.generation(), doc.widget_version(), doc.language_id.clone());
        let diags = self.diagnostics.at(key, pos);
        let handles: Vec<i64> = self.registry.matching(KindTag::Hover, &uri, &language).iter().map(|p| p.handle).collect();
        let cancel = self.hover.begin(key, generation, version, pos, diags);
        self.cancel(cancel);
        for (order, handle) in handles.into_iter().enumerate() {
            let t = self.rpc_with(exthost::provide_hover(handle, &uri, pos.to_exthost()), Pending::Hover { doc: key.to_owned() });
            self.hover.add_pending(t, order);
        }
        if let Some(p) = self.hover.initial() {
            self.ui(SessionUpdate::Hover(p));
        }
    }

    fn end_hover(&mut self) {
        let (tokens, shown) = self.hover.end();
        self.cancel(tokens);
        if let Some(key) = shown {
            if let Some(uri) = self.uri_of(&key) {
                self.ui(SessionUpdate::HoverHidden { uri });
            }
        }
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

    // ---- extension-host requests --------------------------------------------------------------

    fn on_exthost_request(&mut self, r: IncomingRequest) {
        self.reply_seq += 1;
        let parsed = MainThreadCall::parse(&r);
        match parsed {
            Ok(MainThreadCall::DiagnosticsChangeMany { owner, entries }) => {
                let keys = self.diagnostics.change_many(&owner, entries);
                self.touched.extend(keys);
                self.respond(r.req, Ok(Reply::Empty));
            }
            Ok(MainThreadCall::DiagnosticsClear { owner }) => {
                let keys = self.diagnostics.clear(&owner);
                self.touched.extend(keys);
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
            Ok(MainThreadCall::Other { proxy: Some("MainThreadDocuments"), method }) if method == "$tryOpenDocument" => {
                self.on_try_open_document(&r);
            }
            Ok(MainThreadCall::Other { proxy, method }) => {
                let reply = session_reply(proxy, &method, self.reply_seq, &self.language_ids);
                self.respond(r.req, Ok(reply));
            }
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
        let text = match result.and_then(|b| decode(&b).map_err(|e| e.to_string())) {
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
            Pending::Hover { doc } => {
                let Some(version) = self.docs.get(&doc).map(OpenDocument::widget_version) else { return };
                if let Some(p) = self.hover.reply(token, version, result.as_ref().map_err(|_| ())) {
                    self.ui(SessionUpdate::Hover(p));
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
                if let Err(e) = result {
                    tracing::warn!("ember-editor: command {id} failed: {e}");
                }
            }
        }
    }

    // ---- files --------------------------------------------------------------------------------

    fn on_file_read(&mut self, uri: UriComponents, purpose: ReadPurpose, result: Result<Vec<u8>, String>) {
        match purpose {
            ReadPurpose::Open => {
                self.opening.remove(&uri.key());
                match result {
                    Ok(bytes) => self.finish_open(uri, bytes),
                    Err(error) => self.ui(SessionUpdate::OpenFailed { uri, error }),
                }
            }
            ReadPurpose::Reload => {
                let key = uri.key();
                let Ok(bytes) = result else { return };
                let Some(doc) = self.docs.get(&key) else { return };
                let (text, bom) = match decode(&bytes) {
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
                self.reset_doc(&key, &text, Some(bom));
            }
            ReadPurpose::ExtHostOpen { req } => self.finish_exthost_open(uri, req, result),
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

    /// Replace a document's content wholesale (reload from disk, resync). `bom: None` keeps the
    /// current BOM setting.
    fn reset_doc(&mut self, key: &str, text: &str, bom: Option<bool>) {
        self.next_generation += 1;
        let generation = self.next_generation;
        let Some(doc) = self.docs.get_mut(key) else { return };
        let bom = bom.unwrap_or_else(|| doc.save_bytes().starts_with("\u{feff}".as_bytes()));
        let calls = doc.reset(text, bom, generation);
        let (uri, widget_text) = (doc.uri.clone(), doc.widget_text());
        for c in calls {
            self.rpc(c);
        }
        for (handle, cache_id) in self.lenses.close(key) {
            self.rpc(exthost::release_code_lenses(handle, cache_id));
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
        self.send_tab_model();
        self.schedule_codelens(key, Duration::ZERO);
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
    }

    /// The `Decorations` prop of every document whose decorations may have changed.
    fn flush_decorations(&mut self) {
        let touched = std::mem::take(&mut self.touched);
        for key in touched {
            let Some(doc) = self.docs.get(&key) else { continue };
            let version = doc.widget_version();
            let mut decorations = self.diagnostics.decorations(&key, version, doc.lines(), &mut self.ids);
            decorations.extend(self.lenses.decorations(&key, version, doc.lines(), &mut self.ids));
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
