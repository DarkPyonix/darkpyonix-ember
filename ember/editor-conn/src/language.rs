//! Language features beyond diagnostics, hover, CodeLens and inline completions (SPEC FR-L31.13):
//! the provider registrations the extension host makes and the `ExtHostLanguageFeatures` calls
//! that ask those providers.
//!
//! Signatures are `extHost.protocol.ts` at [`crate::PINNED_COMMIT`]: the registrations are in
//! `MainThreadLanguageFeaturesShape` (L528-594) and the calls in `ExtHostLanguageFeaturesShape`
//! (L2954-3029). Every `$provide…` and `$resolve…` call takes a cancellation token as its last
//! argument, which the RPC layer adds for a cancellable [`Call`]. The `$release…` calls free a
//! cached result in the extension host and take none.

use serde_json::{json, Value};

use crate::exthost::{arg_i64, arg_json, j, proxy, Call, DocumentFilter, Position, Range};
use crate::rpc::Arg;
use crate::uri::UriComponents;
use crate::Result;

/// What a `$register…` call of this module registered, with the arguments Ember acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LanguageFeature {
    /// `$registerDocumentSymbolProvider(handle, selector, label)` (L530).
    DocumentSymbol { label: String },
    /// `$registerDefinitionSupport(handle, selector)` (L533).
    Definition,
    /// `$registerReferenceSupport(handle, selector)` (L544).
    Reference,
    /// `$registerCodeActionSupport(handle, selector, metadata, displayName, extensionID, supportsResolve)` (L545).
    CodeAction { provided_kinds: Vec<String>, supports_resolve: bool },
    /// `$registerDocumentFormattingSupport(handle, selector, extensionId, displayName)` (L547).
    DocumentFormatting { display_name: String },
    /// `$registerRangeFormattingSupport(handle, selector, extensionId, displayName, supportRanges)` (L548).
    RangeFormatting { display_name: String },
    /// `$registerRenameSupport(handle, selector, supportsResolveInitialValues)` (L551).
    Rename { supports_resolve: bool },
    /// `$registerCompletionsProvider(handle, selector, triggerCharacters, supportsResolveDetails, extensionId)` (L557).
    Completion { trigger_characters: Vec<String>, supports_resolve: bool },
    /// `$registerSignatureHelpProvider(handle, selector, { triggerCharacters, retriggerCharacters })` (L580).
    SignatureHelp { trigger_characters: Vec<String>, retrigger_characters: Vec<String> },
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect()).unwrap_or_default()
}

/// Is `method` one of the registrations [`parse_registration`] decodes?
pub fn is_registration(method: &str) -> bool {
    matches!(
        method,
        "$registerDocumentSymbolProvider"
            | "$registerDefinitionSupport"
            | "$registerReferenceSupport"
            | "$registerCodeActionSupport"
            | "$registerDocumentFormattingSupport"
            | "$registerRangeFormattingSupport"
            | "$registerRenameSupport"
            | "$registerCompletionsProvider"
            | "$registerSignatureHelpProvider"
    )
}

/// Decode one registration: handle, selector and the feature. `None` for any other method.
pub fn parse_registration(method: &str, a: &[Arg]) -> Result<Option<(i64, Vec<DocumentFilter>, LanguageFeature)>> {
    let feature = match method {
        "$registerDocumentSymbolProvider" => {
            LanguageFeature::DocumentSymbol { label: arg_json(a, 2).as_str().unwrap_or_default().to_owned() }
        }
        "$registerDefinitionSupport" => LanguageFeature::Definition,
        "$registerReferenceSupport" => LanguageFeature::Reference,
        "$registerCodeActionSupport" => LanguageFeature::CodeAction {
            provided_kinds: strings(&arg_json(a, 2)["providedKinds"]),
            supports_resolve: arg_json(a, 5).as_bool().unwrap_or(false),
        },
        "$registerDocumentFormattingSupport" => {
            LanguageFeature::DocumentFormatting { display_name: arg_json(a, 3).as_str().unwrap_or_default().to_owned() }
        }
        "$registerRangeFormattingSupport" => {
            LanguageFeature::RangeFormatting { display_name: arg_json(a, 3).as_str().unwrap_or_default().to_owned() }
        }
        "$registerRenameSupport" => LanguageFeature::Rename { supports_resolve: arg_json(a, 2).as_bool().unwrap_or(false) },
        "$registerCompletionsProvider" => LanguageFeature::Completion {
            trigger_characters: strings(&arg_json(a, 2)),
            supports_resolve: arg_json(a, 3).as_bool().unwrap_or(false),
        },
        "$registerSignatureHelpProvider" => {
            let meta = arg_json(a, 2);
            LanguageFeature::SignatureHelp {
                trigger_characters: strings(&meta["triggerCharacters"]),
                retrigger_characters: strings(&meta["retriggerCharacters"]),
            }
        }
        _ => return Ok(None),
    };
    let handle = arg_i64(a, 0)?;
    let selector: Vec<DocumentFilter> = serde_json::from_value(arg_json(a, 1))?;
    Ok(Some((handle, selector, feature)))
}

fn lang(method: &'static str, args: Vec<Arg>) -> Call {
    Call::new(proxy::EXT_HOST_LANGUAGE_FEATURES, method, args)
}

fn h(handle: i64) -> Arg {
    Arg::Json(handle.into())
}

/// `ChainedCacheId` (`[CacheId, CacheId]`, L2869): the list's id and the item's index in it.
pub type ChainedCacheId = (i64, i64);

fn chained(id: ChainedCacheId) -> Arg {
    Arg::Json(json!([id.0, id.1]))
}

// ---- completion ---------------------------------------------------------------------------------

/// `CompletionTriggerKind` (`languages.ts`): how a completion request started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionTrigger {
    Invoke = 0,
    TriggerCharacter = 1,
    TriggerForIncompleteCompletions = 2,
}

/// `$provideCompletionItems(handle, resource, position, context, token)` → `ISuggestResultDto | undefined`
/// (L2992). `context` is `{ triggerKind, triggerCharacter? }`.
pub fn provide_completion_items(handle: i64, uri: &UriComponents, pos: Position, kind: CompletionTrigger, ch: Option<&str>) -> Call {
    let mut ctx = json!({ "triggerKind": kind as u8 });
    if let Some(ch) = ch {
        ctx["triggerCharacter"] = Value::from(ch);
    }
    lang("$provideCompletionItems", vec![h(handle), j(uri), j(&pos), Arg::Json(ctx)]).cancellable()
}

/// `$resolveCompletionItem(handle, id, token)` → `ISuggestDataDto | undefined` (L2993). Only for a
/// provider registered with `supportsResolveDetails`.
pub fn resolve_completion_item(handle: i64, id: ChainedCacheId) -> Call {
    lang("$resolveCompletionItem", vec![h(handle), chained(id)]).cancellable()
}

/// `$releaseCompletionItems(handle, id)` (L2994): required once a list is no longer shown.
pub fn release_completion_items(handle: i64, id: i64) -> Call {
    lang("$releaseCompletionItems", vec![h(handle), Arg::Json(id.into())])
}

// ---- navigation ---------------------------------------------------------------------------------

/// `$provideDefinition(handle, resource, position, token)` → `ILocationLinkDto[]` (L2959).
pub fn provide_definition(handle: i64, uri: &UriComponents, pos: Position) -> Call {
    lang("$provideDefinition", vec![h(handle), j(uri), j(&pos)]).cancellable()
}

/// `$provideReferences(handle, resource, position, { includeDeclaration }, token)` →
/// `ILocationDto[] | undefined` (L2970).
pub fn provide_references(handle: i64, uri: &UriComponents, pos: Position, include_declaration: bool) -> Call {
    lang(
        "$provideReferences",
        vec![h(handle), j(uri), j(&pos), Arg::Json(json!({ "includeDeclaration": include_declaration }))],
    )
    .cancellable()
}

/// `$provideDocumentSymbols(handle, resource, token)` → `DocumentSymbol[] | undefined` (L2955).
pub fn provide_document_symbols(handle: i64, uri: &UriComponents) -> Call {
    lang("$provideDocumentSymbols", vec![h(handle), j(uri)]).cancellable()
}

// ---- formatting ---------------------------------------------------------------------------------

fn formatting_options(tab_size: u32, insert_spaces: bool) -> Arg {
    Arg::Json(json!({ "tabSize": tab_size, "insertSpaces": insert_spaces }))
}

/// `$provideDocumentFormattingEdits(handle, resource, { tabSize, insertSpaces }, token)` →
/// `TextEdit[] | undefined` (L2978).
pub fn provide_document_formatting_edits(handle: i64, uri: &UriComponents, tab_size: u32, insert_spaces: bool) -> Call {
    lang("$provideDocumentFormattingEdits", vec![h(handle), j(uri), formatting_options(tab_size, insert_spaces)]).cancellable()
}

/// `$provideDocumentRangeFormattingEdits(handle, resource, range, options, token)` →
/// `TextEdit[] | undefined` (L2979).
pub fn provide_document_range_formatting_edits(handle: i64, uri: &UriComponents, range: Range, tab_size: u32, insert_spaces: bool) -> Call {
    lang(
        "$provideDocumentRangeFormattingEdits",
        vec![h(handle), j(uri), j(&range), formatting_options(tab_size, insert_spaces)],
    )
    .cancellable()
}

// ---- rename -------------------------------------------------------------------------------------

/// `$resolveRenameLocation(handle, resource, position, token)` → `RenameLocation | undefined`
/// (`{ range, text }`, or with `rejectReason`; L2986). Only for a provider registered with
/// `supportsResolveInitialValues`.
pub fn resolve_rename_location(handle: i64, uri: &UriComponents, pos: Position) -> Call {
    lang("$resolveRenameLocation", vec![h(handle), j(uri), j(&pos)]).cancellable()
}

/// `$provideRenameEdits(handle, resource, position, newName, token)` →
/// `IWorkspaceEditDto & { rejectReason? } | undefined` (L2985).
pub fn provide_rename_edits(handle: i64, uri: &UriComponents, pos: Position, new_name: &str) -> Call {
    lang("$provideRenameEdits", vec![h(handle), j(uri), j(&pos), Arg::Json(new_name.into())]).cancellable()
}

// ---- code actions -------------------------------------------------------------------------------

/// `CodeActionTriggerType` (`languages.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeActionTrigger {
    Invoke = 1,
    Auto = 2,
}

/// `$provideCodeActions(handle, resource, rangeOrSelection, { only?, trigger }, token)` →
/// `ICodeActionListDto | undefined` (L2971). The extension host adds the diagnostics of the range
/// itself.
pub fn provide_code_actions(handle: i64, uri: &UriComponents, range: Range, only: Option<&str>, trigger: CodeActionTrigger) -> Call {
    let mut ctx = json!({ "trigger": trigger as u8 });
    if let Some(only) = only {
        ctx["only"] = Value::from(only);
    }
    lang("$provideCodeActions", vec![h(handle), j(uri), j(&range), Arg::Json(ctx)]).cancellable()
}

/// `$resolveCodeAction(handle, id, token)` → `{ edit?, command? }` (L2972). Only for a provider
/// registered with `supportsResolve`.
pub fn resolve_code_action(handle: i64, id: ChainedCacheId) -> Call {
    lang("$resolveCodeAction", vec![h(handle), chained(id)]).cancellable()
}

/// `$releaseCodeActions(handle, cacheId)` (L2973).
pub fn release_code_actions(handle: i64, cache_id: i64) -> Call {
    lang("$releaseCodeActions", vec![h(handle), Arg::Json(cache_id.into())])
}

// ---- signature help -----------------------------------------------------------------------------

/// `SignatureHelpTriggerKind` (`languages.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureTrigger {
    Invoke = 1,
    TriggerCharacter = 2,
    ContentChange = 3,
}

/// `$provideSignatureHelp(handle, resource, position, context, token)` → `ISignatureHelpDto | undefined`
/// (L3004). `context` is `{ triggerKind, triggerCharacter?, isRetrigger, activeSignatureHelp? }`;
/// `active` is the help now shown, exactly as it was received.
pub fn provide_signature_help(
    handle: i64,
    uri: &UriComponents,
    pos: Position,
    kind: SignatureTrigger,
    ch: Option<&str>,
    active: Option<&Value>,
) -> Call {
    let mut ctx = json!({ "triggerKind": kind as u8, "isRetrigger": active.is_some() });
    if let Some(ch) = ch {
        ctx["triggerCharacter"] = Value::from(ch);
    }
    if let Some(active) = active {
        ctx["activeSignatureHelp"] = active.clone();
    }
    lang("$provideSignatureHelp", vec![h(handle), j(uri), j(&pos), Arg::Json(ctx)]).cancellable()
}

/// `$releaseSignatureHelp(handle, id)` (L3005).
pub fn release_signature_help(handle: i64, id: i64) -> Call {
    lang("$releaseSignatureHelp", vec![h(handle), Arg::Json(id.into())])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: Value) -> Vec<Arg> {
        v.as_array().unwrap().iter().cloned().map(Arg::Json).collect()
    }

    fn json_arg(c: &Call, i: usize) -> Value {
        c.args[i].as_json().cloned().unwrap()
    }

    fn u() -> UriComponents {
        UriComponents::remote("h", "/w/a.rs")
    }

    const POS: Position = Position { line_number: 3, column: 5 };

    #[test]
    fn fr_l31_13_decodes_every_registration() {
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
            assert!(is_registration(method), "{method}");
            let (handle, selector, feature) = parse_registration(method, &args(a)).unwrap().unwrap();
            assert_eq!(handle, 1, "{method}");
            assert_eq!(selector[0].language.as_deref(), Some("rust"), "{method}");
            assert_eq!(feature, want, "{method}");
        }
        assert!(!is_registration("$registerHoverProvider"));
        assert_eq!(parse_registration("$registerHoverProvider", &args(json!([1, sel]))).unwrap(), None);
    }

    #[test]
    fn fr_l31_13_registration_without_a_handle_is_malformed() {
        assert!(parse_registration("$registerDefinitionSupport", &args(json!(["x", []]))).is_err());
    }

    #[test]
    fn fr_l31_13_completion_calls() {
        let c = provide_completion_items(7, &u(), POS, CompletionTrigger::TriggerCharacter, Some("."));
        assert_eq!((c.proxy, c.method, c.cancellable), ("ExtHostLanguageFeatures", "$provideCompletionItems", true));
        assert_eq!(c.args.len(), 4);
        assert_eq!(json_arg(&c, 0), json!(7));
        assert_eq!(json_arg(&c, 2), json!({"lineNumber": 3, "column": 5}));
        assert_eq!(json_arg(&c, 3), json!({"triggerKind": 1, "triggerCharacter": "."}));
        let c = provide_completion_items(7, &u(), POS, CompletionTrigger::Invoke, None);
        assert_eq!(json_arg(&c, 3), json!({"triggerKind": 0}));
        let c = resolve_completion_item(7, (4, 2));
        assert_eq!((c.method, c.cancellable), ("$resolveCompletionItem", true));
        assert_eq!(json_arg(&c, 1), json!([4, 2]));
        let c = release_completion_items(7, 4);
        assert_eq!((c.method, c.cancellable), ("$releaseCompletionItems", false));
        assert_eq!(json_arg(&c, 1), json!(4));
    }

    #[test]
    fn fr_l31_13_navigation_calls() {
        let c = provide_definition(2, &u(), POS);
        assert_eq!((c.method, c.cancellable, c.args.len()), ("$provideDefinition", true, 3));
        let c = provide_references(2, &u(), POS, true);
        assert_eq!((c.method, c.cancellable), ("$provideReferences", true));
        assert_eq!(json_arg(&c, 3), json!({"includeDeclaration": true}));
        let c = provide_document_symbols(2, &u());
        assert_eq!((c.method, c.cancellable, c.args.len()), ("$provideDocumentSymbols", true, 2));
    }

    #[test]
    fn fr_l31_13_formatting_and_rename_calls() {
        let c = provide_document_formatting_edits(3, &u(), 4, true);
        assert_eq!((c.method, c.cancellable), ("$provideDocumentFormattingEdits", true));
        assert_eq!(json_arg(&c, 2), json!({"tabSize": 4, "insertSpaces": true}));
        let r = Range { start_line_number: 1, start_column: 1, end_line_number: 2, end_column: 4 };
        let c = provide_document_range_formatting_edits(3, &u(), r, 2, false);
        assert_eq!(c.method, "$provideDocumentRangeFormattingEdits");
        assert_eq!(json_arg(&c, 2), json!({"startLineNumber": 1, "startColumn": 1, "endLineNumber": 2, "endColumn": 4}));
        assert_eq!(json_arg(&c, 3), json!({"tabSize": 2, "insertSpaces": false}));
        let c = resolve_rename_location(5, &u(), POS);
        assert_eq!((c.method, c.cancellable, c.args.len()), ("$resolveRenameLocation", true, 3));
        let c = provide_rename_edits(5, &u(), POS, "next");
        assert_eq!((c.method, c.cancellable), ("$provideRenameEdits", true));
        assert_eq!(json_arg(&c, 3), json!("next"));
    }

    #[test]
    fn fr_l31_13_code_action_and_signature_calls() {
        let r = Range { start_line_number: 3, start_column: 5, end_line_number: 3, end_column: 5 };
        let c = provide_code_actions(6, &u(), r, None, CodeActionTrigger::Invoke);
        assert_eq!((c.method, c.cancellable), ("$provideCodeActions", true));
        assert_eq!(json_arg(&c, 3), json!({"trigger": 1}));
        let c = provide_code_actions(6, &u(), r, Some("quickfix"), CodeActionTrigger::Auto);
        assert_eq!(json_arg(&c, 3), json!({"trigger": 2, "only": "quickfix"}));
        let c = resolve_code_action(6, (9, 0));
        assert_eq!((c.method, c.cancellable), ("$resolveCodeAction", true));
        assert_eq!(json_arg(&c, 1), json!([9, 0]));
        assert!(!release_code_actions(6, 9).cancellable);

        let c = provide_signature_help(8, &u(), POS, SignatureTrigger::TriggerCharacter, Some("("), None);
        assert_eq!((c.method, c.cancellable), ("$provideSignatureHelp", true));
        assert_eq!(json_arg(&c, 3), json!({"triggerKind": 2, "triggerCharacter": "(", "isRetrigger": false}));
        let active = json!({"id": 1, "signatures": [], "activeSignature": 0, "activeParameter": 0});
        let c = provide_signature_help(8, &u(), POS, SignatureTrigger::ContentChange, None, Some(&active));
        assert_eq!(json_arg(&c, 3), json!({"triggerKind": 3, "isRetrigger": true, "activeSignatureHelp": active}));
        let c = release_signature_help(8, 1);
        assert_eq!((c.method, c.cancellable), ("$releaseSignatureHelp", false));
    }
}
