//! FR-L31.13 in the session core: [`LanguageRequest`]s in, provider calls out, replies back as
//! [`LanguageUpdate`]s and widget edits. See [`crate::language`] for the data and what each
//! feature does.
//!
//! One request per feature is alive at a time: a new one cancels the calls of the one before it
//! (RPC `Cancel`), and a list the extension host cached (`x` / `cacheId` / `id`) is released when
//! it is no longer shown.

use std::collections::HashMap;

use ember_editor_conn::exthost;
use ember_editor_conn::language::{self as calls, CodeActionTrigger, CompletionTrigger, LanguageFeature, SignatureTrigger};
use ember_editor_conn::rpc::{Arg, IncomingRequest, Reply};
use ember_editor_conn::uri::UriComponents;
use serde_json::Value;

use super::{Effect, Pending, ReadPurpose, SessionCore, SessionUpdate, Token};
use crate::coords::{WidgetPos, WidgetRange};
use crate::document::decode;
use crate::language::assist::{parse_code_actions, parse_rename_location, parse_signature_help, word_at};
use crate::language::completion::{expand_snippet, inflate_item, parse_suggest_result};
use crate::language::edits::{apply_text_edits, parse_text_edits, reject_reason, widget_order};
use crate::language::navigation::{merge_locations, parse_document_symbols, parse_locations};
use crate::language::{CodeAction, CompletionItem, EditSummary, Feature, LanguageRequest, LanguageUpdate, Location, TextEdit, WorkspaceEdit};
use crate::registry::{KindTag, ProviderKind};
use crate::widget::WidgetCommand;

/// What an outstanding call of this module is for.
#[derive(Debug, Clone)]
pub(super) enum PendingLang {
    Completion { session: u64, handle: i64 },
    CompletionResolve { session: u64, index: usize },
    Locations { request: u64, order: usize },
    Format { doc: String, version: u32 },
    PrepareRename { doc: String, pos: WidgetPos },
    Rename { doc: String },
    CodeActions { session: u64, handle: i64 },
    CodeActionResolve { session: u64, index: usize },
    /// `rest` are the providers to ask if this one has nothing.
    Signature { doc: String, handle: i64, rest: Vec<i64>, pos: WidgetPos, trigger: Option<String> },
    Symbols { doc: String, version: u32 },
}

#[derive(Debug)]
struct CompletionSession {
    id: u64,
    doc: String,
    uri: UriComponents,
    version: u32,
    pos: WidgetPos,
    pending: Vec<Token>,
    /// (provider, cache id) of every list received, to release.
    lists: Vec<(i64, i64)>,
    items: Vec<CompletionItem>,
    incomplete: bool,
}

#[derive(Debug)]
struct LocationRequest {
    id: u64,
    doc: String,
    uri: UriComponents,
    feature: Feature,
    origin: WidgetPos,
    pending: Vec<Token>,
    /// (provider order, its locations)
    results: Vec<(usize, Vec<Location>)>,
}

#[derive(Debug)]
struct ActionSession {
    id: u64,
    doc: String,
    uri: UriComponents,
    version: u32,
    pending: Vec<Token>,
    lists: Vec<(i64, i64)>,
    actions: Vec<CodeAction>,
}

/// The parameter hint now shown: its document, provider, cache id and DTO.
#[derive(Debug)]
struct ShownSignature {
    doc: String,
    handle: i64,
    id: Option<i64>,
    raw: Value,
}

/// A workspace edit whose files that are not open are still being read and written.
#[derive(Debug)]
struct WorkspaceEditJob {
    feature: Feature,
    /// file key → its edits
    files: HashMap<String, Vec<TextEdit>>,
    remaining: usize,
    summary: EditSummary,
}

#[derive(Debug, Default)]
pub(super) struct LangState {
    seq: u64,
    completion: Option<CompletionSession>,
    locations: Option<LocationRequest>,
    actions: Option<ActionSession>,
    signature: Option<ShownSignature>,
    /// The one outstanding call per document of the features that have no session.
    single: HashMap<(String, Feature), Token>,
    edit_jobs: HashMap<u64, WorkspaceEditJob>,
    /// write token → (job, file path, edits in the file)
    edit_writes: HashMap<Token, (u64, usize)>,
}

/// An open document as a request sees it.
struct Ctx {
    key: String,
    uri: UriComponents,
    version: u32,
    language: String,
    pos: WidgetPos,
}

impl SessionCore {
    fn lang_ui(&mut self, update: LanguageUpdate) {
        self.ui(SessionUpdate::Language(update));
    }

    fn lang_call(&mut self, call: exthost::Call, pending: PendingLang) -> Token {
        self.rpc_with(call, Pending::Language(pending))
    }

    fn lang_ctx(&self, uri: &UriComponents, pos: Option<WidgetPos>) -> Option<Ctx> {
        let key = uri.key();
        let doc = self.docs.get(&key)?;
        Some(Ctx {
            key,
            uri: doc.uri.clone(),
            version: doc.widget_version(),
            language: doc.language_id.clone(),
            pos: pos.or(doc.cursor()).unwrap_or_default(),
        })
    }

    /// The providers of `kind` for a document, best first, with what their registration said.
    fn lang_providers(&self, kind: KindTag, ctx: &Ctx) -> Vec<(i64, LanguageFeature)> {
        self.registry
            .matching(kind, &ctx.uri, &ctx.language)
            .into_iter()
            .filter_map(|p| match &p.kind {
                ProviderKind::Language(feature) => Some((p.handle, feature.clone())),
                _ => None,
            })
            .collect()
    }

    fn lang_feature_of(&self, handle: i64) -> Option<LanguageFeature> {
        match self.registry.get(handle).map(|p| &p.kind) {
            Some(ProviderKind::Language(feature)) => Some(feature.clone()),
            _ => None,
        }
    }

    /// Replace the outstanding call of `feature` on a document: the old one is cancelled.
    fn lang_single(&mut self, key: &str, feature: Feature, token: Token) {
        if let Some(old) = self.lang.single.insert((key.to_owned(), feature), token) {
            self.cancel(vec![old]);
        }
    }

    fn lang_single_done(&mut self, key: &str, feature: Feature, token: Token) {
        if self.lang.single.get(&(key.to_owned(), feature)) == Some(&token) {
            self.lang.single.remove(&(key.to_owned(), feature));
        }
    }

    /// Send `edits`, written against widget version `version`, to the document's widget.
    fn lang_edit_widget(&mut self, uri: &UriComponents, version: u32, edits: &[TextEdit]) {
        for edit in widget_order(edits) {
            let request_id = self.edit_request_id();
            self.ui(SessionUpdate::Command {
                uri: uri.clone(),
                command: WidgetCommand::EditCode { request_id, base_version: version, range: edit.range, text: edit.text },
            });
        }
    }

    pub(super) fn on_language(&mut self, request: LanguageRequest) {
        let uri = request.uri().clone();
        let pos = match &request {
            LanguageRequest::Complete { pos, .. }
            | LanguageRequest::Definition { pos, .. }
            | LanguageRequest::References { pos, .. }
            | LanguageRequest::PrepareRename { pos, .. }
            | LanguageRequest::SignatureHelp { pos, .. } => *pos,
            LanguageRequest::Rename { pos, .. } => Some(*pos),
            _ => None,
        };
        let Some(ctx) = self.lang_ctx(&uri, pos) else {
            tracing::debug!("ember-editor: language request for a document that is not open: {request:?}");
            return;
        };
        match request {
            LanguageRequest::Complete { trigger, .. } => self.lang_complete(ctx, trigger),
            LanguageRequest::AcceptCompletion { index, .. } => self.lang_accept_completion(&ctx.key, index),
            LanguageRequest::CancelCompletion { .. } => self.lang_end_completion(Some(&ctx.key)),
            LanguageRequest::Definition { .. } => self.lang_locations(ctx, Feature::Definition),
            LanguageRequest::References { .. } => self.lang_locations(ctx, Feature::References),
            LanguageRequest::Format { range, .. } => self.lang_format(ctx, range),
            LanguageRequest::PrepareRename { .. } => self.lang_prepare_rename(ctx),
            LanguageRequest::Rename { new_name, .. } => self.lang_rename(ctx, &new_name),
            LanguageRequest::CodeActions { range, .. } => self.lang_code_actions(ctx, range),
            LanguageRequest::ApplyCodeAction { index, .. } => self.lang_apply_code_action(&ctx.key, index),
            LanguageRequest::DismissCodeActions { .. } => self.lang_end_actions(Some(&ctx.key)),
            LanguageRequest::SignatureHelp { trigger, .. } => self.lang_signature(ctx, trigger),
            LanguageRequest::DismissSignatureHelp { .. } => self.lang_end_signature(&ctx.key, false),
            LanguageRequest::DocumentSymbols { .. } => self.lang_symbols(ctx),
        }
    }

    pub(super) fn on_language_reply(&mut self, token: Token, pending: PendingLang, result: Result<Value, String>) {
        match pending {
            PendingLang::Completion { session, handle } => self.lang_completion_reply(token, session, handle, result),
            PendingLang::CompletionResolve { session, index } => {
                if self.lang.completion.as_ref().is_some_and(|s| s.id == session) {
                    self.lang_apply_completion(index, result.ok());
                }
            }
            PendingLang::Locations { request, order } => self.lang_locations_reply(token, request, order, result),
            PendingLang::Format { doc, version } => self.lang_format_reply(token, &doc, version, result),
            PendingLang::PrepareRename { doc, pos } => self.lang_prepare_rename_reply(token, &doc, pos, result),
            PendingLang::Rename { doc } => self.lang_rename_reply(token, &doc, result),
            PendingLang::CodeActions { session, handle } => self.lang_code_actions_reply(token, session, handle, result),
            PendingLang::CodeActionResolve { session, index } => {
                if self.lang.actions.as_ref().is_some_and(|s| s.id == session) {
                    self.lang_run_code_action(index, result.ok());
                }
            }
            PendingLang::Signature { doc, handle, rest, pos, trigger } => self.lang_signature_reply(token, doc, handle, rest, pos, trigger, result),
            PendingLang::Symbols { doc, version } => {
                self.lang_single_done(&doc, Feature::DocumentSymbols, token);
                let Some(uri) = self.uri_of(&doc) else { return };
                match result {
                    Ok(v) => self.lang_ui(LanguageUpdate::DocumentSymbols { uri, version, symbols: parse_document_symbols(&v) }),
                    Err(error) => self.lang_ui(LanguageUpdate::Failed { uri, feature: Feature::DocumentSymbols, error }),
                }
            }
        }
    }

    /// A document closed: drop what was shown or asked for it.
    pub(super) fn language_close(&mut self, key: &str) {
        self.lang_end_completion(Some(key));
        self.lang_end_actions(Some(key));
        self.lang_end_signature(key, false);
        if self.lang.locations.as_ref().is_some_and(|r| r.doc == key) {
            let tokens = self.lang.locations.take().map(|r| r.pending).unwrap_or_default();
            self.cancel(tokens);
        }
        let singles: Vec<(String, Feature)> = self.lang.single.keys().filter(|(doc, _)| doc == key).cloned().collect();
        for k in singles {
            if let Some(token) = self.lang.single.remove(&k) {
                self.cancel(vec![token]);
            }
        }
    }

    // ---- completion -----------------------------------------------------------------------------

    fn lang_complete(&mut self, ctx: Ctx, trigger: Option<String>) {
        self.lang_end_completion(None);
        let mut providers = self.lang_providers(KindTag::Completion, &ctx);
        if let Some(ch) = &trigger {
            // A typed trigger character asks only the providers that named it (suggestModel.ts).
            providers.retain(|(_, f)| matches!(f, LanguageFeature::Completion { trigger_characters, .. } if trigger_characters.contains(ch)));
        }
        if providers.is_empty() {
            self.lang_ui(LanguageUpdate::Unavailable { uri: ctx.uri, feature: Feature::Completion });
            return;
        }
        self.lang.seq += 1;
        let id = self.lang.seq;
        let kind = if trigger.is_some() { CompletionTrigger::TriggerCharacter } else { CompletionTrigger::Invoke };
        let mut pending = Vec::new();
        for (handle, _) in providers {
            let call = calls::provide_completion_items(handle, &ctx.uri, ctx.pos.to_exthost(), kind, trigger.as_deref());
            pending.push(self.lang_call(call, PendingLang::Completion { session: id, handle }));
        }
        self.lang.completion = Some(CompletionSession {
            id,
            doc: ctx.key,
            uri: ctx.uri,
            version: ctx.version,
            pos: ctx.pos,
            pending,
            lists: Vec::new(),
            items: Vec::new(),
            incomplete: false,
        });
    }

    fn lang_completion_reply(&mut self, token: Token, session: u64, handle: i64, result: Result<Value, String>) {
        let Some(s) = self.lang.completion.as_mut().filter(|s| s.id == session) else {
            // The list is for a session that ended: nothing shows it, so it is released at once.
            if let Some(cache) = result.ok().and_then(|v| v.get("x").and_then(Value::as_i64)) {
                self.rpc(calls::release_completion_items(handle, cache));
            }
            return;
        };
        s.pending.retain(|t| *t != token);
        match result {
            Ok(v) => {
                // The word before the caret, for a provider that sent no default range.
                let word = self.docs.get(&s.doc).and_then(|d| d.line(s.pos.line)).and_then(|line| word_at(line, s.pos));
                let start = word.as_ref().map_or(s.pos, |w| w.range.start);
                let fallback = (WidgetRange::new(start, s.pos), word.map_or(WidgetRange::empty(s.pos), |w| w.range));
                if let Some(list) = parse_suggest_result(handle, &v, fallback) {
                    if let Some(cache) = list.cache_id {
                        s.lists.push((handle, cache));
                    }
                    s.incomplete |= list.incomplete;
                    s.items.extend(list.items);
                }
            }
            Err(error) => tracing::debug!("ember-editor: $provideCompletionItems({handle}) failed: {error}"),
        }
        let update = LanguageUpdate::Completions {
            uri: s.uri.clone(),
            version: s.version,
            pos: s.pos,
            items: s.items.clone(),
            incomplete: s.incomplete,
            complete: s.pending.is_empty(),
        };
        self.lang_ui(update);
    }

    fn lang_accept_completion(&mut self, key: &str, index: usize) {
        let Some(s) = self.lang.completion.as_ref().filter(|s| s.doc == key) else { return };
        let Some(item) = s.items.get(index) else { return };
        let (session, handle, cache_id) = (s.id, item.handle, item.cache_id);
        let resolves = matches!(self.lang_feature_of(handle), Some(LanguageFeature::Completion { supports_resolve: true, .. }));
        match cache_id {
            // Code-OSS resolves an item before inserting it: the additional edits (an import) and
            // the command may only exist after that.
            Some(id) if resolves => {
                self.lang_call(calls::resolve_completion_item(handle, id), PendingLang::CompletionResolve { session, index });
            }
            _ => self.lang_apply_completion(index, None),
        }
    }

    /// Insert item `index`. `resolved` is the `$resolveCompletionItem` reply: its additional edits
    /// and command replace the item's; the insert text and range stay as first given, which is
    /// what the extension host enforces too.
    fn lang_apply_completion(&mut self, index: usize, resolved: Option<Value>) {
        let Some(s) = self.lang.completion.as_ref() else { return };
        let Some(mut item) = s.items.get(index).cloned() else { return };
        let (uri, version) = (s.uri.clone(), s.version);
        if let Some(full) = resolved.as_ref().and_then(|v| inflate_item(item.handle, v, (item.insert_range, item.replace_range))) {
            item.additional_edits = full.additional_edits;
            item.command = full.command.or(item.command);
        }
        // TODO(compose-rust#556): a snippet goes in as plain text and the caret stays after it.
        let text = if item.is_snippet { expand_snippet(&item.insert_text).0 } else { item.insert_text.clone() };
        let mut edits = vec![TextEdit { range: item.insert_range, text }];
        edits.extend(item.additional_edits.iter().cloned());
        self.lang_edit_widget(&uri, version, &edits);
        self.lang_end_completion(None);
        self.lang_ui(LanguageUpdate::CompletionApplied { uri });
        if let Some(command) = item.command {
            self.run_command(command);
        }
    }

    /// End the completion session (of `key` only, when given): cancel what is outstanding and
    /// release the lists.
    fn lang_end_completion(&mut self, key: Option<&str>) {
        if key.is_some_and(|k| self.lang.completion.as_ref().is_some_and(|s| s.doc != k)) {
            return;
        }
        let Some(s) = self.lang.completion.take() else { return };
        self.cancel(s.pending);
        let resolving: Vec<Token> = self
            .pending
            .iter()
            .filter(|(_, p)| matches!(p, Pending::Language(PendingLang::CompletionResolve { session, .. }) if *session == s.id))
            .map(|(t, _)| *t)
            .collect();
        self.cancel(resolving);
        for (handle, cache) in s.lists {
            self.rpc(calls::release_completion_items(handle, cache));
        }
    }

    // ---- definition and references --------------------------------------------------------------

    fn lang_locations(&mut self, ctx: Ctx, feature: Feature) {
        if let Some(old) = self.lang.locations.take() {
            self.cancel(old.pending);
        }
        let kind = if feature == Feature::Definition { KindTag::Definition } else { KindTag::Reference };
        let providers = self.lang_providers(kind, &ctx);
        if providers.is_empty() {
            self.lang_ui(LanguageUpdate::Unavailable { uri: ctx.uri, feature });
            return;
        }
        self.lang.seq += 1;
        let id = self.lang.seq;
        let mut pending = Vec::new();
        for (order, (handle, _)) in providers.into_iter().enumerate() {
            let call = if feature == Feature::Definition {
                calls::provide_definition(handle, &ctx.uri, ctx.pos.to_exthost())
            } else {
                // Code-OSS's "Go to References" lists the declaration too.
                calls::provide_references(handle, &ctx.uri, ctx.pos.to_exthost(), true)
            };
            pending.push(self.lang_call(call, PendingLang::Locations { request: id, order }));
        }
        self.lang.locations = Some(LocationRequest { id, doc: ctx.key, uri: ctx.uri, feature, origin: ctx.pos, pending, results: Vec::new() });
    }

    fn lang_locations_reply(&mut self, token: Token, request: u64, order: usize, result: Result<Value, String>) {
        let Some(r) = self.lang.locations.as_mut().filter(|r| r.id == request) else { return };
        r.pending.retain(|t| *t != token);
        match result {
            Ok(v) => r.results.push((order, parse_locations(&v))),
            Err(error) => tracing::debug!("ember-editor: location provider {order} failed: {error}"),
        }
        if !r.pending.is_empty() {
            return;
        }
        let Some(mut r) = self.lang.locations.take() else { return };
        r.results.sort_by_key(|(order, _)| *order);
        let mut locations = merge_locations(r.results.into_iter().map(|(_, l)| l).collect());
        for location in &mut locations {
            let line = location.target().start.line;
            location.preview = self.docs.get(&location.uri.key()).and_then(|d| d.line(line)).map(|l| l.trim().to_owned());
        }
        self.lang_ui(LanguageUpdate::Locations { uri: r.uri, feature: r.feature, origin: r.origin, locations });
    }

    // ---- formatting -----------------------------------------------------------------------------

    fn lang_format(&mut self, ctx: Ctx, range: Option<WidgetRange>) {
        let (tab, spaces) = (self.opts.tab_width, self.opts.insert_spaces);
        let whole = self.lang_providers(KindTag::DocumentFormatting, &ctx).first().map(|(h, _)| *h);
        let ranged = self.lang_providers(KindTag::RangeFormatting, &ctx).first().map(|(h, _)| *h);
        let call = match (range, whole, ranged) {
            (None, Some(handle), _) => calls::provide_document_formatting_edits(handle, &ctx.uri, tab, spaces),
            // A language with only a range formatter formats the document as one range (format.ts).
            (None, None, Some(handle)) => {
                let lines = self.docs.get(&ctx.key).map(|d| d.lines()).unwrap_or_default();
                let last = lines.len().saturating_sub(1);
                let end = WidgetPos::new(last as u32, lines.get(last).map_or(0, |l| crate::coords::utf16_len(l)));
                let full = WidgetRange::new(WidgetPos::default(), end);
                calls::provide_document_range_formatting_edits(handle, &ctx.uri, full.to_exthost(), tab, spaces)
            }
            (Some(range), _, Some(handle)) => calls::provide_document_range_formatting_edits(handle, &ctx.uri, range.to_exthost(), tab, spaces),
            _ => {
                self.lang_ui(LanguageUpdate::Unavailable { uri: ctx.uri, feature: Feature::Format });
                return;
            }
        };
        let token = self.lang_call(call, PendingLang::Format { doc: ctx.key.clone(), version: ctx.version });
        self.lang_single(&ctx.key, Feature::Format, token);
    }

    fn lang_format_reply(&mut self, token: Token, doc: &str, version: u32, result: Result<Value, String>) {
        self.lang_single_done(doc, Feature::Format, token);
        let Some(uri) = self.uri_of(doc) else { return };
        match result {
            Ok(v) => {
                let edits = parse_text_edits(&v);
                self.lang_edit_widget(&uri, version, &edits);
                self.lang_ui(LanguageUpdate::Formatted { uri, edits: edits.len() });
            }
            Err(error) => self.lang_ui(LanguageUpdate::Failed { uri, feature: Feature::Format, error }),
        }
    }

    // ---- rename ---------------------------------------------------------------------------------

    fn lang_word(&self, key: &str, pos: WidgetPos) -> Option<crate::language::RenameLocation> {
        self.docs.get(key).and_then(|d| d.line(pos.line)).and_then(|line| word_at(line, pos))
    }

    fn lang_rename_ready(&mut self, uri: UriComponents, key: &str, pos: WidgetPos, location: Option<crate::language::RenameLocation>) {
        match location.or_else(|| self.lang_word(key, pos)) {
            Some(l) => self.lang_ui(LanguageUpdate::RenameReady { uri, pos, range: l.range, placeholder: l.text }),
            None => self.lang_ui(LanguageUpdate::RenameRejected { uri, reason: None }),
        }
    }

    fn lang_prepare_rename(&mut self, ctx: Ctx) {
        let Some((handle, feature)) = self.lang_providers(KindTag::Rename, &ctx).into_iter().next() else {
            self.lang_ui(LanguageUpdate::Unavailable { uri: ctx.uri, feature: Feature::Rename });
            return;
        };
        if matches!(feature, LanguageFeature::Rename { supports_resolve: true }) {
            let call = calls::resolve_rename_location(handle, &ctx.uri, ctx.pos.to_exthost());
            let token = self.lang_call(call, PendingLang::PrepareRename { doc: ctx.key.clone(), pos: ctx.pos });
            self.lang_single(&ctx.key, Feature::Rename, token);
        } else {
            self.lang_rename_ready(ctx.uri, &ctx.key, ctx.pos, None);
        }
    }

    fn lang_prepare_rename_reply(&mut self, token: Token, doc: &str, pos: WidgetPos, result: Result<Value, String>) {
        self.lang_single_done(doc, Feature::Rename, token);
        let Some(uri) = self.uri_of(doc) else { return };
        match result.map(|v| parse_rename_location(&v)) {
            Ok(Ok(location)) => self.lang_rename_ready(uri, doc, pos, location),
            Ok(Err(reason)) => self.lang_ui(LanguageUpdate::RenameRejected { uri, reason: Some(reason) }),
            Err(error) => self.lang_ui(LanguageUpdate::Failed { uri, feature: Feature::Rename, error }),
        }
    }

    fn lang_rename(&mut self, ctx: Ctx, new_name: &str) {
        let Some((handle, _)) = self.lang_providers(KindTag::Rename, &ctx).into_iter().next() else {
            self.lang_ui(LanguageUpdate::Unavailable { uri: ctx.uri, feature: Feature::Rename });
            return;
        };
        let call = calls::provide_rename_edits(handle, &ctx.uri, ctx.pos.to_exthost(), new_name);
        let token = self.lang_call(call, PendingLang::Rename { doc: ctx.key.clone() });
        self.lang_single(&ctx.key, Feature::Rename, token);
    }

    fn lang_rename_reply(&mut self, token: Token, doc: &str, result: Result<Value, String>) {
        self.lang_single_done(doc, Feature::Rename, token);
        let Some(uri) = self.uri_of(doc) else { return };
        match result {
            Ok(v) => match (reject_reason(&v), WorkspaceEdit::from_json(&v)) {
                (Some(reason), _) => self.lang_ui(LanguageUpdate::RenameRejected { uri, reason: Some(reason) }),
                (None, Some(edit)) => self.lang_apply_workspace_edit(edit, Feature::Rename),
                (None, None) => self.lang_ui(LanguageUpdate::RenameRejected { uri, reason: None }),
            },
            Err(error) => self.lang_ui(LanguageUpdate::Failed { uri, feature: Feature::Rename, error }),
        }
    }

    // ---- workspace edits ------------------------------------------------------------------------

    /// Apply the text edits of a workspace edit: open documents through their widget, other files
    /// by reading, editing and writing them. Answered with `WorkspaceEditApplied` when every file
    /// is done.
    pub(super) fn lang_apply_workspace_edit(&mut self, edit: WorkspaceEdit, feature: Feature) {
        self.lang.seq += 1;
        let id = self.lang.seq;
        let mut job = WorkspaceEditJob { feature, files: HashMap::new(), remaining: 0, summary: EditSummary { unsupported: edit.unsupported, ..Default::default() } };
        for (uri, edits) in edit.files {
            let key = uri.key();
            match self.docs.get(&key).map(|d| (d.uri.clone(), d.widget_version())) {
                Some((doc_uri, version)) => {
                    self.lang_edit_widget(&doc_uri, version, &edits);
                    job.summary.files += 1;
                    job.summary.edits += edits.len();
                }
                None => {
                    job.files.insert(key, edits);
                    job.remaining += 1;
                    self.out.push(Effect::ReadFile { uri, purpose: ReadPurpose::WorkspaceEdit { id } });
                }
            }
        }
        if job.remaining == 0 {
            self.lang_ui(LanguageUpdate::WorkspaceEditApplied { feature, summary: job.summary });
        } else {
            self.lang.edit_jobs.insert(id, job);
        }
    }

    pub(super) fn on_workspace_edit_read(&mut self, id: u64, uri: UriComponents, result: Result<Vec<u8>, String>) {
        let Some(job) = self.lang.edit_jobs.get(&id) else { return };
        let edits = job.files.get(&uri.key()).cloned().unwrap_or_default();
        let written = result
            .and_then(|bytes| decode(&bytes).map_err(|e| e.to_string()))
            .and_then(|(text, bom)| apply_text_edits(&text, &edits).map(|text| (text, bom)));
        match written {
            Ok((text, bom)) => {
                let mut bytes = Vec::with_capacity(text.len() + 3);
                if bom {
                    bytes.extend_from_slice("\u{feff}".as_bytes());
                }
                bytes.extend_from_slice(text.as_bytes());
                let token = self.token();
                self.lang.edit_writes.insert(token, (id, edits.len()));
                self.out.push(Effect::WriteFile { uri, bytes, token });
            }
            Err(error) => self.lang_edit_file_done(id, Err((uri.path.clone(), error))),
        }
    }

    /// A file write finished. `false` when the token is not a workspace edit's.
    pub(super) fn on_workspace_edit_written(&mut self, uri: &UriComponents, token: Token, result: &Result<(), String>) -> bool {
        let Some((id, edits)) = self.lang.edit_writes.remove(&token) else { return false };
        match result {
            Ok(()) => self.lang_edit_file_done(id, Ok(edits)),
            Err(error) => self.lang_edit_file_done(id, Err((uri.path.clone(), error.clone()))),
        }
        true
    }

    fn lang_edit_file_done(&mut self, id: u64, outcome: Result<usize, (String, String)>) {
        let Some(job) = self.lang.edit_jobs.get_mut(&id) else { return };
        match outcome {
            Ok(edits) => {
                job.summary.files += 1;
                job.summary.edits += edits;
            }
            Err(failure) => job.summary.failures.push(failure),
        }
        job.remaining = job.remaining.saturating_sub(1);
        if job.remaining == 0 {
            if let Some(job) = self.lang.edit_jobs.remove(&id) {
                self.lang_ui(LanguageUpdate::WorkspaceEditApplied { feature: job.feature, summary: job.summary });
            }
        }
    }

    /// `MainThreadBulkEdits.$tryApplyWorkspaceEdit(dto, undoRedoGroupId?, isRefactoring?)` →
    /// `boolean` (extHost.protocol.ts L344): an extension's `workspace.applyEdit`, which is how
    /// the command of many code actions makes its change. Answered `true` once the edits are on
    /// their way; a file that then fails is reported in the summary.
    pub(super) fn on_try_apply_workspace_edit(&mut self, r: &IncomingRequest) {
        let edit = r.args.first().and_then(Arg::as_json).and_then(WorkspaceEdit::from_json);
        let applied = edit.is_some();
        if let Some(edit) = edit {
            self.lang_apply_workspace_edit(edit, Feature::CodeActions);
        }
        self.respond(r.req, Ok(Reply::Json(Value::Bool(applied))));
    }

    // ---- code actions ---------------------------------------------------------------------------

    fn lang_code_actions(&mut self, ctx: Ctx, range: Option<WidgetRange>) {
        self.lang_end_actions(None);
        let providers = self.lang_providers(KindTag::CodeAction, &ctx);
        if providers.is_empty() {
            self.lang_ui(LanguageUpdate::Unavailable { uri: ctx.uri, feature: Feature::CodeActions });
            return;
        }
        self.lang.seq += 1;
        let id = self.lang.seq;
        let range = range.unwrap_or(WidgetRange::empty(ctx.pos)).to_exthost();
        let mut pending = Vec::new();
        for (handle, _) in providers {
            let call = calls::provide_code_actions(handle, &ctx.uri, range, None, CodeActionTrigger::Invoke);
            pending.push(self.lang_call(call, PendingLang::CodeActions { session: id, handle }));
        }
        self.lang.actions = Some(ActionSession { id, doc: ctx.key, uri: ctx.uri, version: ctx.version, pending, lists: Vec::new(), actions: Vec::new() });
    }

    fn lang_code_actions_reply(&mut self, token: Token, session: u64, handle: i64, result: Result<Value, String>) {
        let Some(s) = self.lang.actions.as_mut().filter(|s| s.id == session) else {
            if let Some(cache) = result.ok().and_then(|v| v.get("cacheId").and_then(Value::as_i64)) {
                self.rpc(calls::release_code_actions(handle, cache));
            }
            return;
        };
        s.pending.retain(|t| *t != token);
        match result {
            Ok(v) => {
                let (cache, actions) = parse_code_actions(handle, &v);
                if let Some(cache) = cache {
                    s.lists.push((handle, cache));
                }
                s.actions.extend(actions);
            }
            Err(error) => tracing::debug!("ember-editor: $provideCodeActions({handle}) failed: {error}"),
        }
        let update = LanguageUpdate::CodeActions { uri: s.uri.clone(), version: s.version, actions: s.actions.clone(), complete: s.pending.is_empty() };
        self.lang_ui(update);
    }

    fn lang_apply_code_action(&mut self, key: &str, index: usize) {
        let Some(s) = self.lang.actions.as_ref().filter(|s| s.doc == key) else { return };
        let Some(action) = s.actions.get(index) else { return };
        if action.disabled.is_some() {
            return;
        }
        let (session, handle, cache_id, has_edit) = (s.id, action.handle, action.cache_id, action.edit.is_some());
        let resolves = matches!(self.lang_feature_of(handle), Some(LanguageFeature::CodeAction { supports_resolve: true, .. }));
        match cache_id {
            // An action without its edit is resolved first (codeAction.ts `resolve`).
            Some(id) if resolves && !has_edit => {
                self.lang_call(calls::resolve_code_action(handle, id), PendingLang::CodeActionResolve { session, index });
            }
            _ => self.lang_run_code_action(index, None),
        }
    }

    /// Apply action `index`: its edit, then its command. `resolved` is the `$resolveCodeAction`
    /// reply (`{ edit?, command? }`).
    fn lang_run_code_action(&mut self, index: usize, resolved: Option<Value>) {
        let Some(mut action) = self.lang.actions.as_ref().and_then(|s| s.actions.get(index).cloned()) else { return };
        if let Some(v) = &resolved {
            action.edit = v.get("edit").and_then(WorkspaceEdit::from_json).or(action.edit);
            action.command = v.get("command").and_then(crate::codelens::CommandDto::from_json).or(action.command);
        }
        self.lang_end_actions(None);
        if let Some(edit) = action.edit.filter(|e| !e.is_empty()) {
            self.lang_apply_workspace_edit(edit, Feature::CodeActions);
        }
        if let Some(command) = action.command {
            self.run_command(command);
        }
    }

    fn lang_end_actions(&mut self, key: Option<&str>) {
        if key.is_some_and(|k| self.lang.actions.as_ref().is_some_and(|s| s.doc != k)) {
            return;
        }
        let Some(s) = self.lang.actions.take() else { return };
        self.cancel(s.pending);
        let resolving: Vec<Token> = self
            .pending
            .iter()
            .filter(|(_, p)| matches!(p, Pending::Language(PendingLang::CodeActionResolve { session, .. }) if *session == s.id))
            .map(|(t, _)| *t)
            .collect();
        self.cancel(resolving);
        for (handle, cache) in s.lists {
            self.rpc(calls::release_code_actions(handle, cache));
        }
    }

    // ---- signature help -------------------------------------------------------------------------

    fn lang_signature(&mut self, ctx: Ctx, trigger: Option<String>) {
        let handles: Vec<i64> = self.lang_providers(KindTag::SignatureHelp, &ctx).into_iter().map(|(h, _)| h).collect();
        if handles.is_empty() {
            self.lang_ui(LanguageUpdate::Unavailable { uri: ctx.uri, feature: Feature::SignatureHelp });
            return;
        }
        self.lang_signature_ask(ctx.key, handles, ctx.pos, trigger);
    }

    /// Ask the first of `handles`; the others follow if it has nothing (provideSignatureHelp.ts
    /// takes the first provider that answers).
    fn lang_signature_ask(&mut self, doc: String, mut handles: Vec<i64>, pos: WidgetPos, trigger: Option<String>) {
        let Some(uri) = self.uri_of(&doc) else { return };
        if handles.is_empty() {
            self.lang_end_signature(&doc, true);
            return;
        }
        let handle = handles.remove(0);
        let active = self.lang.signature.as_ref().filter(|s| s.doc == doc).map(|s| s.raw.clone());
        let kind = if trigger.is_some() { SignatureTrigger::TriggerCharacter } else { SignatureTrigger::Invoke };
        let call = calls::provide_signature_help(handle, &uri, pos.to_exthost(), kind, trigger.as_deref(), active.as_ref());
        let token = self.lang_call(call, PendingLang::Signature { doc: doc.clone(), handle, rest: handles, pos, trigger });
        self.lang_single(&doc, Feature::SignatureHelp, token);
    }

    #[allow(clippy::too_many_arguments)]
    fn lang_signature_reply(&mut self, token: Token, doc: String, handle: i64, rest: Vec<i64>, pos: WidgetPos, trigger: Option<String>, result: Result<Value, String>) {
        self.lang_single_done(&doc, Feature::SignatureHelp, token);
        let Some(uri) = self.uri_of(&doc) else { return };
        match result.ok().as_ref().and_then(parse_signature_help) {
            Some(help) => {
                // The hint this one replaces is released; the new one is kept until it is.
                self.lang_release_signature();
                self.lang.signature = Some(ShownSignature { doc, handle, id: help.id, raw: help.raw.clone() });
                self.lang_ui(LanguageUpdate::SignatureHelp { uri, help: Some(help) });
            }
            None => self.lang_signature_ask(doc, rest, pos, trigger),
        }
    }

    fn lang_release_signature(&mut self) {
        if let Some(ShownSignature { handle, id: Some(id), .. }) = self.lang.signature.take() {
            self.rpc(calls::release_signature_help(handle, id));
        }
    }

    /// Hide the hint of `key`. `tell` says so to the window (not needed when the window asked).
    fn lang_end_signature(&mut self, key: &str, tell: bool) {
        if let Some(token) = self.lang.single.remove(&(key.to_owned(), Feature::SignatureHelp)) {
            self.cancel(vec![token]);
        }
        if self.lang.signature.as_ref().is_some_and(|s| s.doc == key) {
            self.lang_release_signature();
        }
        if tell {
            if let Some(uri) = self.uri_of(key) {
                self.lang_ui(LanguageUpdate::SignatureHelp { uri, help: None });
            }
        }
    }

    // ---- document symbols -----------------------------------------------------------------------

    fn lang_symbols(&mut self, ctx: Ctx) {
        let Some((handle, _)) = self.lang_providers(KindTag::DocumentSymbol, &ctx).into_iter().next() else {
            self.lang_ui(LanguageUpdate::Unavailable { uri: ctx.uri, feature: Feature::DocumentSymbols });
            return;
        };
        let call = calls::provide_document_symbols(handle, &ctx.uri);
        let token = self.lang_call(call, PendingLang::Symbols { doc: ctx.key.clone(), version: ctx.version });
        self.lang_single(&ctx.key, Feature::DocumentSymbols, token);
    }
}
