//! Extension-host requests, routed through the generated `MainThread*` actors.
//!
//! [`SessionCore`] implements the trait of every actor whose requests the editor acts on. A
//! request is decoded and dispatched by the actor's own `dispatch`; whatever no trait method
//! covers (an actor without an implementation, a method left at its default, or a method of the
//! pinned protocol the extension host never sends) gets the fallback reply and is counted in
//! [`SessionCore::unimplemented_calls`], which says where behaviour is needed next.

use ember_editor_conn::actors::generated::{
    main_thread_bulk_edits, main_thread_commands, main_thread_diagnostics, main_thread_documents, main_thread_language_features,
    main_thread_languages, main_thread_message_service, main_thread_output_service, main_thread_search, main_thread_secret_state,
    main_thread_window, main_thread_workspace,
};
use ember_editor_conn::actors::{Ctx, Outcome};
use ember_editor_conn::exthost::{DocumentFilter, MarkerData, SemanticTokensLegend};
use ember_editor_conn::language::LanguageFeature;
use ember_editor_conn::rpc::{IncomingRequest, Reply};
use ember_editor_conn::uri::UriComponents;
use serde_json::{json, Value};

use super::{Effect, SessionCore, SessionUpdate};
use crate::codelens::CommandDto;
use crate::registry::{InlineMeta, RegistryCall};
use crate::replies::session_reply;
use crate::trust::TrustAnswer;

fn empty() -> Outcome {
    Outcome::Reply(Reply::Empty)
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect()).unwrap_or_default()
}

fn selector(v: Vec<Value>) -> Result<Vec<DocumentFilter>, serde_json::Error> {
    serde_json::from_value(Value::Array(v))
}

impl SessionCore {
    /// Route one request that is not a webview call: through the actor of its proxy, or to the
    /// fallback reply.
    pub(super) fn route_exthost_request(&mut self, r: IncomingRequest) {
        // Not part of the pinned protocol, but the extension host asks for it
        // (`languages.getLanguages()`), and the session owns the answer.
        if r.proxy == Some(main_thread_languages::PROXY) && r.method == "$getLanguages" {
            self.fallback_reply(&r);
            return;
        }
        if r.proxy == Some(main_thread_output_service::PROXY) && self.on_output_text(&r) {
            return;
        }
        let cx = Ctx { req: r.req, cancellable: r.cancellable };
        let (method, args) = (r.method.as_str(), r.args.as_slice());
        let routed = match r.proxy {
            Some(main_thread_diagnostics::PROXY) => Some(main_thread_diagnostics::dispatch(self, &cx, method, args)),
            Some(main_thread_commands::PROXY) => Some(main_thread_commands::dispatch(self, &cx, method, args)),
            Some(main_thread_output_service::PROXY) => Some(main_thread_output_service::dispatch(self, &cx, method, args)),
            Some(main_thread_message_service::PROXY) => Some(main_thread_message_service::dispatch(self, &cx, method, args)),
            Some(main_thread_search::PROXY) => Some(main_thread_search::dispatch(self, &cx, method, args)),
            Some(main_thread_secret_state::PROXY) => Some(main_thread_secret_state::dispatch(self, &cx, method, args)),
            Some(main_thread_language_features::PROXY) => Some(main_thread_language_features::dispatch(self, &cx, method, args)),
            Some(main_thread_documents::PROXY) => Some(main_thread_documents::dispatch(self, &cx, method, args)),
            Some(main_thread_languages::PROXY) => Some(main_thread_languages::dispatch(self, &cx, method, args)),
            Some(main_thread_bulk_edits::PROXY) => Some(main_thread_bulk_edits::dispatch(self, &cx, method, args)),
            Some(main_thread_workspace::PROXY) => Some(main_thread_workspace::dispatch(self, &cx, method, args)),
            Some(main_thread_window::PROXY) => Some(main_thread_window::dispatch(self, &cx, method, args)),
            _ => None,
        };
        match routed {
            Some(Ok(Some(Outcome::Reply(reply)))) => self.respond(r.req, Ok(reply)),
            Some(Ok(Some(Outcome::Fail(error)))) => self.respond(r.req, Err(error)),
            Some(Ok(Some(Outcome::Deferred))) => {}
            Some(Err(e)) => {
                tracing::warn!("ember-editor: cannot decode {:?}.{}: {e}", r.proxy, r.method);
                // `workspace.openTextDocument` and `languages.setTextDocumentLanguage` reject on a
                // bad uri; resolving them with `undefined` hands the extension a document that is
                // not there.
                let rejected = match (r.proxy, method) {
                    (Some(main_thread_documents::PROXY), "$tryOpenDocument") => Some("bad uri"),
                    (Some(main_thread_languages::PROXY), "$changeLanguage") => Some("bad arguments"),
                    _ => None,
                };
                match rejected {
                    Some(why) => self.respond(r.req, Err(Some(ember_editor_conn::rpc::js_error("Error", why)))),
                    None => self.fallback_reply(&r),
                }
            }
            Some(Ok(Some(Outcome::Unimplemented))) | Some(Ok(None)) | None => {
                self.count_unimplemented(&r);
                self.fallback_reply(&r);
            }
        }
    }

    /// `$append` and `$clear` are not in the generated output service, and the output panel
    /// needs them, so the session answers them here. So is `$register`: only its label is
    /// used, and the generated decoder would reject a missing or odd `file`, which the session
    /// has always tolerated. Returns false when the call is not one of these or its arguments
    /// are malformed, so the caller falls through to the generated dispatch.
    fn on_output_text(&mut self, r: &IncomingRequest) -> bool {
        let text = |i: usize| r.args.get(i).and_then(|a| a.as_json()).and_then(Value::as_str);
        let Some(channel_id) = text(0) else { return false };
        if r.method == "$register" {
            let reply = self.register_output_channel(channel_id.to_owned());
            self.respond(r.req, Ok(reply));
            return true;
        }
        let value = match r.method.as_str() {
            "$append" => match text(1) {
                Some(v) => Some(v),
                None => return false,
            },
            "$clear" => None,
            _ => return false,
        };
        if let Some(ch) = self.output_channels.iter_mut().find(|c| c.id == channel_id) {
            match value {
                Some(v) => ch.append(v),
                None => ch.clear(),
            }
            self.output_changed(None);
        }
        self.respond(r.req, Ok(Reply::Empty));
        true
    }

    fn count_unimplemented(&mut self, r: &IncomingRequest) {
        let proxy = r.proxy.map_or_else(|| format!("rpc{}", r.rpc_id), str::to_owned);
        let count = self.unimplemented.entry((proxy.clone(), r.method.clone())).or_insert(0);
        *count += 1;
        if *count == 1 {
            tracing::info!("ember-editor: unimplemented {proxy}.{}", r.method);
        }
    }

    /// The reply for a request no trait method answered: workspace trust first (it needs no
    /// decoded arguments), then [`session_reply`].
    fn fallback_reply(&mut self, r: &IncomingRequest) {
        match self.trust.on_request(r.proxy, &r.method, r.req) {
            Some(TrustAnswer::Now(reply)) => self.respond(r.req, Ok(reply)),
            Some(TrustAnswer::Ask) => self.out.push(Effect::Ui(SessionUpdate::WorkspaceTrustRequested)),
            None => {
                let reply = session_reply(r.proxy, &r.method, self.reply_seq, &self.language_ids);
                self.respond(r.req, Ok(reply));
            }
        }
    }

    /// Apply a registry call, react to what it changed and answer `undefined`. A call that could
    /// not be built from its arguments is logged and answered the same way.
    fn registry_call(&mut self, what: &str, call: Result<RegistryCall, serde_json::Error>) -> Outcome {
        match call {
            Ok(call) => {
                let change = self.registry.apply(&call);
                self.on_registry_change(change);
            }
            Err(e) => tracing::warn!("ember-editor: cannot decode {what}: {e}"),
        }
        empty()
    }

    fn trust_request(&mut self, method: &str, cx: &Ctx) -> Outcome {
        match self.trust.on_request(Some(main_thread_workspace::PROXY), method, cx.req) {
            Some(TrustAnswer::Now(reply)) => Outcome::Reply(reply),
            Some(TrustAnswer::Ask) => {
                self.out.push(Effect::Ui(SessionUpdate::WorkspaceTrustRequested));
                Outcome::Deferred
            }
            None => Outcome::Unimplemented,
        }
    }

    fn output_changed(&mut self, active_id: Option<String>) {
        let channels = self.output_channels.clone();
        self.ui(SessionUpdate::OutputChannels { channels, active_id });
    }

    fn register_output_channel(&mut self, label: String) -> Reply {
        self.output_seq += 1;
        let id = format!("ember-output-{}", self.output_seq);
        self.output_channels.push(crate::output::OutputChannel::new(id.clone(), label));
        self.output_changed(Some(id.clone()));
        Reply::Json(json!(id))
    }

    fn dispose_output_channel(&mut self, channel_id: &str) -> Outcome {
        self.output_channels.retain(|c| c.id != channel_id);
        self.output_changed(None);
        empty()
    }
}

impl main_thread_diagnostics::Diagnostics for SessionCore {
    fn change_many(&mut self, _cx: &Ctx, args: main_thread_diagnostics::ChangeManyArgs) -> Outcome {
        let entries: Vec<(UriComponents, Option<Vec<MarkerData>>)> = match serde_json::from_value(Value::Array(args.entries)) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("ember-editor: cannot decode MainThreadDiagnostics.$changeMany: {e}");
                return empty();
            }
        };
        let keys = self.diagnostics.change_many(&args.owner, entries);
        self.touched.extend(keys);
        self.ui(SessionUpdate::Problems { problems: self.diagnostics.problems() });
        empty()
    }

    fn clear(&mut self, _cx: &Ctx, args: main_thread_diagnostics::ClearArgs) -> Outcome {
        let keys = self.diagnostics.clear(&args.owner);
        self.touched.extend(keys);
        self.ui(SessionUpdate::Problems { problems: self.diagnostics.problems() });
        empty()
    }
}

impl main_thread_commands::Commands for SessionCore {
    fn register_command(&mut self, _cx: &Ctx, args: main_thread_commands::RegisterCommandArgs) -> Outcome {
        self.registry_call("MainThreadCommands.$registerCommand", Ok(RegistryCall::RegisterCommand { id: args.id }))
    }

    fn unregister_command(&mut self, _cx: &Ctx, args: main_thread_commands::UnregisterCommandArgs) -> Outcome {
        self.registry_call("MainThreadCommands.$unregisterCommand", Ok(RegistryCall::UnregisterCommand { id: args.id }))
    }

    fn execute_command(&mut self, _cx: &Ctx, args: main_thread_commands::ExecuteCommandArgs) -> Outcome {
        // The extension host runs its own commands locally; what reaches us is a workbench
        // command. Setting a context key is a no-op for Ember: the API name is `setContext`, and
        // the extension host at 1.139.1 sends it as `_setContext`.
        let id = args.id;
        if !matches!(id.as_str(), "setContext" | "_setContext") && !self.registry.has_command(&id) {
            let arguments = match args.args {
                Value::Array(a) => a,
                Value::Null => Vec::new(),
                other => vec![other],
            };
            self.ui(SessionUpdate::RunCommand { command: CommandDto { id, title: String::new(), tooltip: None, arguments } });
        }
        empty()
    }

    fn get_commands(&mut self, _cx: &Ctx, _args: main_thread_commands::GetCommandsArgs) -> Outcome {
        Outcome::Reply(Reply::Json(json!([])))
    }
}

impl main_thread_output_service::OutputService for SessionCore {
    fn register(&mut self, _cx: &Ctx, args: main_thread_output_service::RegisterArgs) -> Outcome {
        Outcome::Reply(self.register_output_channel(args.label))
    }

    fn update(&mut self, _cx: &Ctx, _args: main_thread_output_service::UpdateArgs) -> Outcome {
        empty()
    }

    fn reveal(&mut self, _cx: &Ctx, args: main_thread_output_service::RevealArgs) -> Outcome {
        self.output_changed(Some(args.channel_id));
        empty()
    }

    fn close(&mut self, _cx: &Ctx, args: main_thread_output_service::CloseArgs) -> Outcome {
        self.dispose_output_channel(&args.channel_id)
    }

    fn dispose(&mut self, _cx: &Ctx, args: main_thread_output_service::DisposeArgs) -> Outcome {
        self.dispose_output_channel(&args.channel_id)
    }
}

impl main_thread_message_service::MessageService for SessionCore {
    fn show_message(&mut self, cx: &Ctx, args: main_thread_message_service::ShowMessageArgs) -> Outcome {
        // Code-OSS answers when the notification closes: the chosen button's handle, or
        // `undefined`. The call stays open until the UI says.
        let severity = args.severity.as_i64().map(|s| s as u8).unwrap_or(1);
        self.next_message_id += 1;
        let id = self.next_message_id;
        self.pending_messages.insert(id, cx.req);
        self.ui(SessionUpdate::ExtensionMessage(crate::output::ExtensionMessage::from_show_message(
            id,
            severity,
            args.message,
            &args.options,
            &args.commands,
        )));
        Outcome::Deferred
    }
}

impl main_thread_search::Search for SessionCore {
    fn register_text_search_provider(&mut self, _cx: &Ctx, args: main_thread_search::RegisterTextSearchProviderArgs) -> Outcome {
        // The remote scheme wins over `file` should both ever be registered.
        if args.scheme == ember_editor_conn::uri::SCHEME_REMOTE || (args.scheme == "file" && self.search_provider.is_none()) {
            self.search_provider = Some(args.handle);
        }
        empty()
    }

    fn unregister_provider(&mut self, _cx: &Ctx, args: main_thread_search::UnregisterProviderArgs) -> Outcome {
        if self.search_provider == Some(args.handle) {
            self.search_provider = None;
        }
        empty()
    }

    fn handle_text_match(&mut self, cx: &Ctx, args: main_thread_search::HandleTextMatchArgs) -> Outcome {
        let matches = match ember_editor_conn::search::parse_file_matches(&Value::Array(args.data)) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("ember-editor: cannot decode MainThreadSearch.$handleTextMatch: {e}");
                return empty();
            }
        };
        self.respond(cx.req, Ok(Reply::Empty));
        self.on_text_matches(args.session, matches);
        Outcome::Deferred
    }
}

impl main_thread_language_features::LanguageFeatures for SessionCore {
    fn register_hover_provider(&mut self, _cx: &Ctx, args: main_thread_language_features::RegisterHoverProviderArgs) -> Outcome {
        let call = selector(args.selector).map(|selector| RegistryCall::RegisterHoverProvider { handle: args.handle, selector });
        self.registry_call("MainThreadLanguageFeatures.$registerHoverProvider", call)
    }

    fn register_code_lens_support(&mut self, _cx: &Ctx, args: main_thread_language_features::RegisterCodeLensSupportArgs) -> Outcome {
        let call = selector(args.selector)
            .map(|selector| RegistryCall::RegisterCodeLensSupport { handle: args.handle, selector, event_handle: args.event_handle });
        self.registry_call("MainThreadLanguageFeatures.$registerCodeLensSupport", call)
    }

    fn emit_code_lens_event(&mut self, _cx: &Ctx, args: main_thread_language_features::EmitCodeLensEventArgs) -> Outcome {
        self.registry_call(
            "MainThreadLanguageFeatures.$emitCodeLensEvent",
            Ok(RegistryCall::EmitCodeLensEvent { event_handle: args.event_handle }),
        )
    }

    fn register_folding_range_provider(
        &mut self,
        _cx: &Ctx,
        args: main_thread_language_features::RegisterFoldingRangeProviderArgs,
    ) -> Outcome {
        let call = selector(args.selector)
            .map(|selector| RegistryCall::RegisterFoldingRangeProvider { handle: args.handle, selector, event_handle: args.event_handle });
        self.registry_call("MainThreadLanguageFeatures.$registerFoldingRangeProvider", call)
    }

    fn emit_folding_range_event(&mut self, _cx: &Ctx, args: main_thread_language_features::EmitFoldingRangeEventArgs) -> Outcome {
        self.registry_call(
            "MainThreadLanguageFeatures.$emitFoldingRangeEvent",
            Ok(RegistryCall::EmitFoldingRangeEvent { event_handle: args.event_handle }),
        )
    }

    fn register_document_semantic_tokens_provider(
        &mut self,
        _cx: &Ctx,
        args: main_thread_language_features::RegisterDocumentSemanticTokensProviderArgs,
    ) -> Outcome {
        let call = selector(args.selector).and_then(|selector| {
            let legend: SemanticTokensLegend = serde_json::from_value(args.legend)?;
            Ok(RegistryCall::RegisterDocumentSemanticTokensProvider { handle: args.handle, selector, legend, event_handle: args.event_handle })
        });
        self.registry_call("MainThreadLanguageFeatures.$registerDocumentSemanticTokensProvider", call)
    }

    fn emit_document_semantic_tokens_event(
        &mut self,
        _cx: &Ctx,
        args: main_thread_language_features::EmitDocumentSemanticTokensEventArgs,
    ) -> Outcome {
        self.registry_call(
            "MainThreadLanguageFeatures.$emitDocumentSemanticTokensEvent",
            Ok(RegistryCall::EmitDocumentSemanticTokensEvent { event_handle: args.event_handle }),
        )
    }

    fn register_inline_completions_support(
        &mut self,
        _cx: &Ctx,
        args: main_thread_language_features::RegisterInlineCompletionsSupportArgs,
    ) -> Outcome {
        let meta = InlineMeta {
            supports_handle_events: args.supports_handle_events,
            extension_id: args.extension_id,
            yields_to: args.yields_to_extension_ids,
            debounce_ms: args.debounce_delay_ms.and_then(|d| u64::try_from(d).ok()),
            excludes: args.excludes_extension_ids,
        };
        let call = selector(args.selector).map(|selector| RegistryCall::RegisterInlineCompletionsSupport { handle: args.handle, selector, meta });
        self.registry_call("MainThreadLanguageFeatures.$registerInlineCompletionsSupport", call)
    }

    fn emit_inline_completions_change(
        &mut self,
        _cx: &Ctx,
        args: main_thread_language_features::EmitInlineCompletionsChangeArgs,
    ) -> Outcome {
        self.registry_call(
            "MainThreadLanguageFeatures.$emitInlineCompletionsChange",
            Ok(RegistryCall::EmitInlineCompletionsChange { handle: args.handle }),
        )
    }

    fn register_completions_provider(
        &mut self,
        _cx: &Ctx,
        args: main_thread_language_features::RegisterCompletionsProviderArgs,
    ) -> Outcome {
        let feature =
            LanguageFeature::Completion { trigger_characters: args.trigger_characters, supports_resolve: args.supports_resolve_details };
        let call = selector(args.selector).map(|selector| RegistryCall::RegisterLanguageFeature { handle: args.handle, selector, feature });
        self.registry_call("MainThreadLanguageFeatures.$registerCompletionsProvider", call)
    }

    fn register_definition_support(
        &mut self,
        _cx: &Ctx,
        args: main_thread_language_features::RegisterDefinitionSupportArgs,
    ) -> Outcome {
        let call = selector(args.selector)
            .map(|selector| RegistryCall::RegisterLanguageFeature { handle: args.handle, selector, feature: LanguageFeature::Definition });
        self.registry_call("MainThreadLanguageFeatures.$registerDefinitionSupport", call)
    }

    fn register_reference_support(&mut self, _cx: &Ctx, args: main_thread_language_features::RegisterReferenceSupportArgs) -> Outcome {
        let call = selector(args.selector)
            .map(|selector| RegistryCall::RegisterLanguageFeature { handle: args.handle, selector, feature: LanguageFeature::Reference });
        self.registry_call("MainThreadLanguageFeatures.$registerReferenceSupport", call)
    }

    fn register_rename_support(&mut self, _cx: &Ctx, args: main_thread_language_features::RegisterRenameSupportArgs) -> Outcome {
        let feature = LanguageFeature::Rename { supports_resolve: args.supports_resolve_initial_values };
        let call = selector(args.selector).map(|selector| RegistryCall::RegisterLanguageFeature { handle: args.handle, selector, feature });
        self.registry_call("MainThreadLanguageFeatures.$registerRenameSupport", call)
    }

    fn register_code_action_support(
        &mut self,
        _cx: &Ctx,
        args: main_thread_language_features::RegisterCodeActionSupportArgs,
    ) -> Outcome {
        let feature = LanguageFeature::CodeAction {
            provided_kinds: strings(&args.metadata["providedKinds"]),
            supports_resolve: args.supports_resolve,
        };
        let call = selector(args.selector).map(|selector| RegistryCall::RegisterLanguageFeature { handle: args.handle, selector, feature });
        self.registry_call("MainThreadLanguageFeatures.$registerCodeActionSupport", call)
    }

    fn register_document_formatting_support(
        &mut self,
        _cx: &Ctx,
        args: main_thread_language_features::RegisterDocumentFormattingSupportArgs,
    ) -> Outcome {
        let feature = LanguageFeature::DocumentFormatting { display_name: args.display_name };
        let call = selector(args.selector).map(|selector| RegistryCall::RegisterLanguageFeature { handle: args.handle, selector, feature });
        self.registry_call("MainThreadLanguageFeatures.$registerDocumentFormattingSupport", call)
    }

    fn register_range_formatting_support(
        &mut self,
        _cx: &Ctx,
        args: main_thread_language_features::RegisterRangeFormattingSupportArgs,
    ) -> Outcome {
        let feature = LanguageFeature::RangeFormatting { display_name: args.display_name };
        let call = selector(args.selector).map(|selector| RegistryCall::RegisterLanguageFeature { handle: args.handle, selector, feature });
        self.registry_call("MainThreadLanguageFeatures.$registerRangeFormattingSupport", call)
    }

    fn register_signature_help_provider(
        &mut self,
        _cx: &Ctx,
        args: main_thread_language_features::RegisterSignatureHelpProviderArgs,
    ) -> Outcome {
        let feature = LanguageFeature::SignatureHelp {
            trigger_characters: strings(&args.metadata["triggerCharacters"]),
            retrigger_characters: strings(&args.metadata["retriggerCharacters"]),
        };
        let call = selector(args.selector).map(|selector| RegistryCall::RegisterLanguageFeature { handle: args.handle, selector, feature });
        self.registry_call("MainThreadLanguageFeatures.$registerSignatureHelpProvider", call)
    }

    fn register_document_symbol_provider(
        &mut self,
        _cx: &Ctx,
        args: main_thread_language_features::RegisterDocumentSymbolProviderArgs,
    ) -> Outcome {
        let feature = LanguageFeature::DocumentSymbol { label: args.label };
        let call = selector(args.selector).map(|selector| RegistryCall::RegisterLanguageFeature { handle: args.handle, selector, feature });
        self.registry_call("MainThreadLanguageFeatures.$registerDocumentSymbolProvider", call)
    }

    fn unregister(&mut self, _cx: &Ctx, args: main_thread_language_features::UnregisterArgs) -> Outcome {
        self.registry_call("MainThreadLanguageFeatures.$unregister", Ok(RegistryCall::Unregister { handle: args.handle }))
    }
}

impl main_thread_documents::Documents for SessionCore {
    fn try_open_document(&mut self, cx: &Ctx, args: main_thread_documents::TryOpenDocumentArgs) -> Outcome {
        self.on_try_open_document(cx.req, args.uri);
        Outcome::Deferred
    }
}

impl main_thread_languages::Languages for SessionCore {
    fn change_language(&mut self, cx: &Ctx, args: main_thread_languages::ChangeLanguageArgs) -> Outcome {
        self.on_change_language_request(cx.req, args.resource, args.language_id);
        Outcome::Deferred
    }
}

impl main_thread_bulk_edits::BulkEdits for SessionCore {
    fn try_apply_workspace_edit(&mut self, cx: &Ctx, args: main_thread_bulk_edits::TryApplyWorkspaceEditArgs) -> Outcome {
        self.on_try_apply_workspace_edit(cx.req, &args.workspace_edit_dto.value);
        Outcome::Deferred
    }
}

impl main_thread_workspace::Workspace for SessionCore {
    fn is_resource_trusted(&mut self, cx: &Ctx, _args: main_thread_workspace::IsResourceTrustedArgs) -> Outcome {
        self.trust_request("$isResourceTrusted", cx)
    }

    fn request_workspace_trust(&mut self, cx: &Ctx, _args: main_thread_workspace::RequestWorkspaceTrustArgs) -> Outcome {
        self.trust_request("$requestWorkspaceTrust", cx)
    }

    fn request_resource_trust(&mut self, cx: &Ctx, _args: main_thread_workspace::RequestResourceTrustArgs) -> Outcome {
        self.trust_request("$requestResourceTrust", cx)
    }

    /// `workspaceContains:` activation; `false` until Ember searches the remote filesystem by glob.
    fn check_exists(&mut self, _cx: &Ctx, _args: main_thread_workspace::CheckExistsArgs) -> Outcome {
        Outcome::Reply(Reply::Json(Value::Bool(false)))
    }
}

impl main_thread_window::Window for SessionCore {
    fn get_initial_state(&mut self, _cx: &Ctx, _args: main_thread_window::GetInitialStateArgs) -> Outcome {
        Outcome::Reply(Reply::Json(json!({ "isFocused": true, "isActive": true })))
    }
}

impl main_thread_secret_state::SecretState for SessionCore {
    fn get_password(&mut self, cx: &Ctx, args: main_thread_secret_state::GetPasswordArgs) -> Outcome {
        self.out.push(Effect::SecretState {
            req: cx.req,
            op: crate::secret_state::SecretOp::Get {
                extension_id: args.extension_id,
                key: args.key,
            },
        });
        Outcome::Deferred
    }

    fn set_password(&mut self, cx: &Ctx, args: main_thread_secret_state::SetPasswordArgs) -> Outcome {
        self.out.push(Effect::SecretState {
            req: cx.req,
            op: crate::secret_state::SecretOp::Set {
                extension_id: args.extension_id,
                key: args.key,
                value: args.value,
            },
        });
        Outcome::Deferred
    }

    fn delete_password(&mut self, cx: &Ctx, args: main_thread_secret_state::DeletePasswordArgs) -> Outcome {
        self.out.push(Effect::SecretState {
            req: cx.req,
            op: crate::secret_state::SecretOp::Delete {
                extension_id: args.extension_id,
                key: args.key,
            },
        });
        Outcome::Deferred
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use ember_editor_conn::actors::generated::main_thread_language_features::RegisterDefinitionSupportArgs;
    use ember_editor_conn::rpc::Arg;

    use super::super::{CoreOptions, Input};
    use super::*;
    use crate::languages::LanguageRegistry;
    use crate::registry::ProviderKind;

    fn args(v: Value) -> Vec<Arg> {
        v.as_array().unwrap().iter().cloned().map(Arg::Json).collect()
    }

    #[test]
    fn fr_l31_13_decodes_every_registration() {
        let t0 = Instant::now();
        let mut core = SessionCore::new(LanguageRegistry::from_extensions(&[]), CoreOptions::default(), t0);
        let sel = json!([{"language": "rust"}]);
        let ext = json!({"value": "rust-lang.rust-analyzer", "_lower": "rust-lang.rust-analyzer"});
        let cases: Vec<(&str, Value, LanguageFeature)> = vec![
            ("$registerDocumentSymbolProvider", json!([1, sel, "Rust"]), LanguageFeature::DocumentSymbol { label: "Rust".into() }),
            ("$registerDefinitionSupport", json!([1, sel]), LanguageFeature::Definition),
            ("$registerReferenceSupport", json!([1, sel]), LanguageFeature::Reference),
            (
                "$registerCodeActionSupport",
                json!([1, sel, {"providedKinds": ["quickfix", "refactor"]}, "rust-analyzer", "rust-lang.rust-analyzer", true]),
                LanguageFeature::CodeAction { provided_kinds: vec!["quickfix".into(), "refactor".into()], supports_resolve: true },
            ),
            (
                "$registerDocumentFormattingSupport",
                json!([1, sel, ext, "rust-analyzer"]),
                LanguageFeature::DocumentFormatting { display_name: "rust-analyzer".into() },
            ),
            (
                "$registerRangeFormattingSupport",
                json!([1, sel, ext, "rust-analyzer", false]),
                LanguageFeature::RangeFormatting { display_name: "rust-analyzer".into() },
            ),
            ("$registerRenameSupport", json!([1, sel, true]), LanguageFeature::Rename { supports_resolve: true }),
            (
                "$registerCompletionsProvider",
                json!([1, sel, [".", ":"], true, ext]),
                LanguageFeature::Completion { trigger_characters: vec![".".into(), ":".into()], supports_resolve: true },
            ),
            (
                "$registerSignatureHelpProvider",
                json!([1, sel, {"triggerCharacters": ["(", ","], "retriggerCharacters": [")"]}]),
                LanguageFeature::SignatureHelp { trigger_characters: vec!["(".into(), ",".into()], retrigger_characters: vec![")".into()] },
            ),
        ];
        for (method, a, want) in cases {
            let proxy = main_thread_language_features::PROXY;
            let request = IncomingRequest {
                req: 5,
                rpc_id: ember_editor_conn::rpc_ids::id_of(proxy).unwrap(),
                proxy: Some(proxy),
                method: method.into(),
                args: args(a),
                cancellable: false,
            };
            let fx = core.handle(t0, Input::ExtHostRequest(request));
            assert!(fx.contains(&Effect::Respond { req: 5, result: Ok(Reply::Empty) }), "{method}: {fx:?}");
            let provider = core.registry.get(1).unwrap_or_else(|| panic!("{method} registered nothing"));
            assert_eq!(provider.handle, 1, "{method}");
            assert_eq!(provider.selector[0].language.as_deref(), Some("rust"), "{method}");
            assert_eq!(provider.kind, ProviderKind::Language(want), "{method}");
        }
        assert!(core.unimplemented_calls().is_empty());
    }

    #[test]
    fn fr_l31_13_registration_without_a_handle_is_malformed() {
        assert!(RegisterDefinitionSupportArgs::decode(&args(json!(["x", []]))).is_err());
    }

    #[test]
    fn secret_state_routes_get_set_delete_and_replies() {
        use crate::secret_state::{SecretOp, SecretReply};

        let t0 = Instant::now();
        let mut core = SessionCore::new(LanguageRegistry::from_extensions(&[]), CoreOptions::default(), t0);
        let proxy = main_thread_secret_state::PROXY;
        let rpc_id = ember_editor_conn::rpc_ids::id_of(proxy).unwrap();

        let mk = |req: u32, method: &str, a: Value| IncomingRequest {
            req,
            rpc_id,
            proxy: Some(proxy),
            method: method.into(),
            args: args(a),
            cancellable: false,
        };

        let fx = core.handle(t0, Input::ExtHostRequest(mk(1, "$setPassword", json!(["ext.a", "k", "v"]))));
        assert_eq!(
            fx,
            vec![Effect::SecretState {
                req: 1,
                op: SecretOp::Set { extension_id: "ext.a".into(), key: "k".into(), value: "v".into() },
            }]
        );
        let fx = core.handle(t0, Input::SecretState { req: 1, result: Ok(SecretReply::Unit) });
        assert_eq!(fx, vec![Effect::Respond { req: 1, result: Ok(Reply::Empty) }]);

        let fx = core.handle(t0, Input::ExtHostRequest(mk(2, "$getPassword", json!(["ext.a", "k"]))));
        assert_eq!(
            fx,
            vec![Effect::SecretState {
                req: 2,
                op: SecretOp::Get { extension_id: "ext.a".into(), key: "k".into() },
            }]
        );
        let fx = core.handle(t0, Input::SecretState { req: 2, result: Ok(SecretReply::Value(Some("v".into()))) });
        assert_eq!(fx, vec![Effect::Respond { req: 2, result: Ok(Reply::Json(json!("v"))) }]);

        let fx = core.handle(t0, Input::ExtHostRequest(mk(3, "$getPassword", json!(["ext.a", "missing"]))));
        assert_eq!(
            fx,
            vec![Effect::SecretState {
                req: 3,
                op: SecretOp::Get { extension_id: "ext.a".into(), key: "missing".into() },
            }]
        );
        let fx = core.handle(t0, Input::SecretState { req: 3, result: Ok(SecretReply::Value(None)) });
        assert_eq!(fx, vec![Effect::Respond { req: 3, result: Ok(Reply::Empty) }]);

        let fx = core.handle(t0, Input::ExtHostRequest(mk(4, "$deletePassword", json!(["ext.a", "k"]))));
        assert_eq!(
            fx,
            vec![Effect::SecretState {
                req: 4,
                op: SecretOp::Delete { extension_id: "ext.a".into(), key: "k".into() },
            }]
        );
        let fx = core.handle(t0, Input::SecretState { req: 4, result: Ok(SecretReply::Unit) });
        assert_eq!(fx, vec![Effect::Respond { req: 4, result: Ok(Reply::Empty) }]);

        assert!(core.unimplemented_calls().is_empty());
    }
}
