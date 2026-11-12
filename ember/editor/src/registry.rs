//! Provider and command registrations made by the extension host.
//!
//! Upstream `MainThreadLanguageFeatures` keeps `handle → registration` and registers each provider
//! in the matching `LanguageFeatureRegistry` with its selector (`mainThreadLanguageFeatures.ts`).
//! When the editor needs hovers for a model it asks the registry for the providers whose selectors
//! score above 0, ordered by score and then most-recently-registered first
//! (`languageFeatureRegistry.ts` `_orderedForEach`). This module does the same for the three
//! provider kinds Ember's bridges use, plus the extension host's own commands
//! (`MainThreadCommands.$registerCommand`), which decide whether a command can be executed with
//! `ExtHostCommands.$executeContributedCommand`.

use std::collections::{BTreeMap, HashMap, HashSet};

use ember_editor_conn::exthost::{DocumentFilter, MainThreadCall, SemanticTokensLegend};
use ember_editor_conn::language::LanguageFeature;
use ember_editor_conn::uri::UriComponents;
use serde_json::Value;

use crate::selector;

/// `$registerInlineCompletionsSupport` arguments beyond handle and selector
/// (extHost.protocol.ts L558-576 at the pinned commit, 17 positional args).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InlineMeta {
    /// arg 2 `supportsHandleEvents`: send `$handleInlineCompletionDidShow` / `…EndOfLifetime`.
    pub supports_handle_events: bool,
    /// arg 3
    pub extension_id: String,
    /// arg 6
    pub yields_to: Vec<String>,
    /// arg 8 `debounceDelayMs`
    pub debounce_ms: Option<u64>,
    /// arg 9
    pub excludes: Vec<String>,
}

impl InlineMeta {
    pub fn from_raw(raw: &[Value]) -> Self {
        let strs = |v: Option<&Value>| -> Vec<String> {
            v.and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default()
        };
        Self {
            supports_handle_events: raw.get(2).and_then(Value::as_bool).unwrap_or(false),
            extension_id: raw.get(3).and_then(Value::as_str).unwrap_or_default().to_owned(),
            yields_to: strs(raw.get(6)),
            debounce_ms: raw.get(8).and_then(Value::as_u64),
            excludes: strs(raw.get(9)),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProviderKind {
    Hover,
    CodeLens { event_handle: Option<i64> },
    InlineCompletions(InlineMeta),
    FoldingRange { event_handle: Option<i64> },
    DocumentSemanticTokens { legend: SemanticTokensLegend, event_handle: Option<i64> },
    /// Completion, definition, references, rename, code actions, formatting, signature help or
    /// document symbols (FR-L31.13), with what its registration said.
    Language(LanguageFeature),
}

/// Which kind of provider a query is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KindTag {
    Hover,
    CodeLens,
    InlineCompletions,
    FoldingRange,
    DocumentSemanticTokens,
    Completion,
    Definition,
    Reference,
    CodeAction,
    DocumentFormatting,
    RangeFormatting,
    Rename,
    SignatureHelp,
    DocumentSymbol,
}

impl ProviderKind {
    pub fn tag(&self) -> KindTag {
        match self {
            Self::Hover => KindTag::Hover,
            Self::CodeLens { .. } => KindTag::CodeLens,
            Self::InlineCompletions(_) => KindTag::InlineCompletions,
            Self::FoldingRange { .. } => KindTag::FoldingRange,
            Self::DocumentSemanticTokens { .. } => KindTag::DocumentSemanticTokens,
            Self::Language(feature) => match feature {
                LanguageFeature::Completion { .. } => KindTag::Completion,
                LanguageFeature::Definition => KindTag::Definition,
                LanguageFeature::Reference => KindTag::Reference,
                LanguageFeature::CodeAction { .. } => KindTag::CodeAction,
                LanguageFeature::DocumentFormatting { .. } => KindTag::DocumentFormatting,
                LanguageFeature::RangeFormatting { .. } => KindTag::RangeFormatting,
                LanguageFeature::Rename { .. } => KindTag::Rename,
                LanguageFeature::SignatureHelp { .. } => KindTag::SignatureHelp,
                LanguageFeature::DocumentSymbol { .. } => KindTag::DocumentSymbol,
            },
        }
    }

    pub fn event_handle(&self) -> Option<i64> {
        match self {
            Self::CodeLens { event_handle }
            | Self::FoldingRange { event_handle }
            | Self::DocumentSemanticTokens { event_handle, .. } => *event_handle,
            Self::Hover | Self::InlineCompletions(_) | Self::Language(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Provider {
    pub handle: i64,
    pub selector: Vec<DocumentFilter>,
    pub kind: ProviderKind,
    /// Registration order (later = preferred among equal scores).
    pub seq: u64,
}

/// What a `MainThread*` call changed, for the session to react to.
#[derive(Debug, Clone, PartialEq)]
pub enum RegistryChange {
    Registered { handle: i64, kind: KindTag },
    Unregistered { handle: i64, kind: KindTag },
    /// `$emitCodeLensEvent(eventHandle)` resolved to the provider handle.
    CodeLensChanged { handle: i64 },
    InlineCompletionsChanged { handle: i64 },
    FoldingRangeChanged { handle: i64 },
    SemanticTokensChanged { handle: i64 },
    CommandsChanged,
    None,
}

#[derive(Debug, Default)]
pub struct Registry {
    providers: BTreeMap<i64, Provider>,
    event_handles: HashMap<i64, i64>,
    commands: HashSet<String>,
    seq: u64,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    fn insert(&mut self, handle: i64, selector: Vec<DocumentFilter>, kind: ProviderKind) -> RegistryChange {
        self.seq += 1;
        let tag = kind.tag();
        if let Some(e) = kind.event_handle() {
            self.event_handles.insert(e, handle);
        }
        self.providers.insert(handle, Provider { handle, selector, kind, seq: self.seq });
        RegistryChange::Registered { handle, kind: tag }
    }

    /// Apply a decoded extension-host call. Calls that are not about registrations return
    /// [`RegistryChange::None`].
    pub fn apply(&mut self, call: &MainThreadCall) -> RegistryChange {
        match call {
            MainThreadCall::RegisterHoverProvider { handle, selector } => {
                self.insert(*handle, selector.clone(), ProviderKind::Hover)
            }
            MainThreadCall::RegisterCodeLensSupport { handle, selector, event_handle } => {
                self.insert(*handle, selector.clone(), ProviderKind::CodeLens { event_handle: *event_handle })
            }
            MainThreadCall::RegisterFoldingRangeProvider { handle, selector, event_handle } => {
                self.insert(*handle, selector.clone(), ProviderKind::FoldingRange { event_handle: *event_handle })
            }
            MainThreadCall::RegisterDocumentSemanticTokensProvider { handle, selector, legend, event_handle } => {
                self.insert(
                    *handle,
                    selector.clone(),
                    ProviderKind::DocumentSemanticTokens { legend: legend.clone(), event_handle: *event_handle },
                )
            }
            MainThreadCall::RegisterInlineCompletionsSupport { handle, selector, raw, .. } => {
                self.insert(*handle, selector.clone(), ProviderKind::InlineCompletions(InlineMeta::from_raw(raw)))
            }
            MainThreadCall::RegisterLanguageFeature { handle, selector, feature } => {
                self.insert(*handle, selector.clone(), ProviderKind::Language(feature.clone()))
            }
            MainThreadCall::EmitCodeLensEvent { event_handle } => match self.event_handles.get(event_handle) {
                Some(&handle) => RegistryChange::CodeLensChanged { handle },
                None => RegistryChange::None,
            },
            MainThreadCall::EmitFoldingRangeEvent { event_handle } => match self.event_handles.get(event_handle) {
                Some(&handle) => RegistryChange::FoldingRangeChanged { handle },
                None => RegistryChange::None,
            },
            MainThreadCall::EmitDocumentSemanticTokensEvent { event_handle } => match self.event_handles.get(event_handle) {
                Some(&handle) => RegistryChange::SemanticTokensChanged { handle },
                None => RegistryChange::None,
            },
            MainThreadCall::EmitInlineCompletionsChange { handle } => {
                if self.providers.contains_key(handle) {
                    RegistryChange::InlineCompletionsChanged { handle: *handle }
                } else {
                    RegistryChange::None
                }
            }
            MainThreadCall::Unregister { handle } => {
                // Upstream `$unregister` is also called for event handles (they live in the same
                // `_registrations` map); those just drop the mapping.
                if self.event_handles.remove(handle).is_some() {
                    return RegistryChange::None;
                }
                match self.providers.remove(handle) {
                    Some(p) => {
                        if let Some(e) = p.kind.event_handle() {
                            self.event_handles.remove(&e);
                        }
                        RegistryChange::Unregistered { handle: *handle, kind: p.kind.tag() }
                    }
                    None => RegistryChange::None,
                }
            }
            MainThreadCall::RegisterCommand { id } => {
                self.commands.insert(id.clone());
                RegistryChange::CommandsChanged
            }
            MainThreadCall::UnregisterCommand { id } => {
                self.commands.remove(id);
                RegistryChange::CommandsChanged
            }
            _ => RegistryChange::None,
        }
    }

    pub fn get(&self, handle: i64) -> Option<&Provider> {
        self.providers.get(&handle)
    }

    /// Providers of `kind` for a document, best first: score descending, then newest first.
    pub fn matching(&self, kind: KindTag, uri: &UriComponents, language: &str) -> Vec<&Provider> {
        let mut v: Vec<(u32, &Provider)> = self
            .providers
            .values()
            .filter(|p| p.kind.tag() == kind)
            .map(|p| (selector::score(&p.selector, uri, language), p))
            .filter(|(s, _)| *s > 0)
            .collect();
        v.sort_by(|(sa, pa), (sb, pb)| sb.cmp(sa).then(pb.seq.cmp(&pa.seq)));
        v.into_iter().map(|(_, p)| p).collect()
    }

    /// Is `id` a command the extension host registered (so `$executeContributedCommand` runs it)?
    pub fn has_command(&self, id: &str) -> bool {
        self.commands.contains(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sel(v: Value) -> Vec<DocumentFilter> {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn matching_orders_by_score_then_newest() {
        let mut r = Registry::new();
        let u = UriComponents::remote("h", "/w/a.json");
        r.apply(&MainThreadCall::RegisterHoverProvider { handle: 1, selector: sel(json!([{"language": "*"}])) });
        r.apply(&MainThreadCall::RegisterHoverProvider { handle: 2, selector: sel(json!([{"language": "json"}])) });
        r.apply(&MainThreadCall::RegisterHoverProvider { handle: 3, selector: sel(json!([{"language": "json"}])) });
        r.apply(&MainThreadCall::RegisterHoverProvider { handle: 4, selector: sel(json!([{"language": "rust"}])) });
        r.apply(&MainThreadCall::RegisterCodeLensSupport { handle: 5, selector: sel(json!([{"language": "json"}])), event_handle: None });
        let order: Vec<i64> = r.matching(KindTag::Hover, &u, "json").iter().map(|p| p.handle).collect();
        assert_eq!(order, vec![3, 2, 1]);
        assert_eq!(r.matching(KindTag::CodeLens, &u, "json").len(), 1);
        assert!(r.matching(KindTag::InlineCompletions, &u, "json").is_empty());
    }

    #[test]
    fn fr_l31_13_language_features_register_match_and_unregister() {
        let mut r = Registry::new();
        let u = UriComponents::remote("h", "/w/a.rs");
        let feature = LanguageFeature::Completion { trigger_characters: vec![".".into()], supports_resolve: true };
        assert_eq!(
            r.apply(&MainThreadCall::RegisterLanguageFeature { handle: 9, selector: sel(json!([{"language": "rust"}])), feature: feature.clone() }),
            RegistryChange::Registered { handle: 9, kind: KindTag::Completion }
        );
        r.apply(&MainThreadCall::RegisterLanguageFeature { handle: 10, selector: sel(json!([{"language": "rust"}])), feature: LanguageFeature::Definition });
        let found = r.matching(KindTag::Completion, &u, "rust");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, ProviderKind::Language(feature));
        assert_eq!(r.matching(KindTag::Definition, &u, "rust").len(), 1);
        assert!(r.matching(KindTag::Completion, &u, "json").is_empty());
        assert!(r.matching(KindTag::Rename, &u, "rust").is_empty());
        assert_eq!(r.apply(&MainThreadCall::Unregister { handle: 9 }), RegistryChange::Unregistered { handle: 9, kind: KindTag::Completion });
        assert!(r.matching(KindTag::Completion, &u, "rust").is_empty());
    }

    #[test]
    fn code_lens_event_handles_and_unregister() {
        let mut r = Registry::new();
        r.apply(&MainThreadCall::RegisterCodeLensSupport { handle: 7, selector: vec![], event_handle: Some(8) });
        assert_eq!(r.apply(&MainThreadCall::EmitCodeLensEvent { event_handle: 8 }), RegistryChange::CodeLensChanged { handle: 7 });
        assert_eq!(r.apply(&MainThreadCall::EmitCodeLensEvent { event_handle: 99 }), RegistryChange::None);
        // Unregistering the event handle only drops the mapping.
        assert_eq!(r.apply(&MainThreadCall::Unregister { handle: 8 }), RegistryChange::None);
        assert!(r.get(7).is_some());
        assert_eq!(
            r.apply(&MainThreadCall::Unregister { handle: 7 }),
            RegistryChange::Unregistered { handle: 7, kind: KindTag::CodeLens }
        );
        assert!(r.get(7).is_none());
    }

    #[test]
    fn inline_meta_from_the_17_positional_args() {
        let raw = vec![
            json!(3),
            json!([{"language": "*"}]),
            json!(true),
            json!("GitHub.copilot"),
            json!("1.2.3"),
            Value::Null,
            json!(["other.ext"]),
            json!("Copilot"),
            json!(75),
            json!([]),
            json!(false),
            json!(true),
            Value::Null,
            json!(false),
            json!(false),
            Value::Null,
            json!(false),
        ];
        let m = InlineMeta::from_raw(&raw);
        assert!(m.supports_handle_events);
        assert_eq!(m.extension_id, "GitHub.copilot");
        assert_eq!(m.yields_to, vec!["other.ext".to_string()]);
        assert_eq!(m.debounce_ms, Some(75));
    }

    #[test]
    fn commands_are_tracked() {
        let mut r = Registry::new();
        r.apply(&MainThreadCall::RegisterCommand { id: "__vsc123".into() });
        assert!(r.has_command("__vsc123"));
        r.apply(&MainThreadCall::UnregisterCommand { id: "__vsc123".into() });
        assert!(!r.has_command("__vsc123"));
    }

    #[test]
    fn fr_e6_folding_and_semantic_tokens_providers_and_events() {
        let mut r = Registry::new();
        let u = UriComponents::remote("h", "/w/src/lib.rs");
        assert_eq!(
            r.apply(&MainThreadCall::RegisterFoldingRangeProvider {
                handle: 10,
                selector: sel(json!([{"language": "rust"}])),
                event_handle: Some(11),
            }),
            RegistryChange::Registered { handle: 10, kind: KindTag::FoldingRange }
        );
        assert_eq!(r.matching(KindTag::FoldingRange, &u, "rust").len(), 1);
        assert_eq!(
            r.apply(&MainThreadCall::EmitFoldingRangeEvent { event_handle: 11 }),
            RegistryChange::FoldingRangeChanged { handle: 10 }
        );

        let legend = SemanticTokensLegend {
            token_types: vec!["namespace".into(), "type".into(), "function".into()],
            token_modifiers: vec!["declaration".into()],
        };
        assert_eq!(
            r.apply(&MainThreadCall::RegisterDocumentSemanticTokensProvider {
                handle: 20,
                selector: sel(json!([{"language": "rust"}])),
                legend: legend.clone(),
                event_handle: Some(21),
            }),
            RegistryChange::Registered { handle: 20, kind: KindTag::DocumentSemanticTokens }
        );
        let matched = r.matching(KindTag::DocumentSemanticTokens, &u, "rust");
        assert_eq!(matched.len(), 1);
        assert_eq!(
            matched[0].kind,
            ProviderKind::DocumentSemanticTokens { legend, event_handle: Some(21) }
        );
        assert_eq!(
            r.apply(&MainThreadCall::EmitDocumentSemanticTokensEvent { event_handle: 21 }),
            RegistryChange::SemanticTokensChanged { handle: 20 }
        );
        assert_eq!(
            r.apply(&MainThreadCall::Unregister { handle: 20 }),
            RegistryChange::Unregistered { handle: 20, kind: KindTag::DocumentSemanticTokens }
        );
        assert_eq!(
            r.apply(&MainThreadCall::EmitDocumentSemanticTokensEvent { event_handle: 21 }),
            RegistryChange::None
        );
    }
}
