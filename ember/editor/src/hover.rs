//! FR-E3: hover. `CodeHovered(Rest)` → `$provideHover` on every matching provider → one popup
//! model (Markdown) for the UI to draw with dioxus-compose's overlay widgets.
//!
//! Protocol: `$provideHover(handle, uri, IPosition, HoverContext | undefined, token)` →
//! `HoverWithId | undefined` = `{ id, contents: IMarkdownString[], range?: IRange,
//! canIncreaseVerbosity?, canDecreaseVerbosity? }` (extHost.protocol.ts L2338, L2963). Upstream
//! (`mainThreadLanguageFeatures.ts` L272-290) never calls `$releaseHover` (its finalization registry
//! is commented out), so neither does Ember.
//!
//! The popup lists diagnostics under the position first (what Monaco's marker hover part shows),
//! then provider contents in provider order. It is re-emitted as replies arrive (`complete` tells
//! the UI whether more may come). Any edit, a `Leave`, or a new `Rest` cancels outstanding requests
//! (RPC `Cancel`) and drops late replies.

use serde_json::Value;

use crate::coords::{WidgetPos, WidgetRange};
use crate::diagnostics::HoverDiagnostic;

/// `IMarkdownString`.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkdownString {
    pub value: String,
    /// `isTrusted` (may be `true` or `{ enabledCommands }` upstream; any truthy value → `true`).
    /// Only trusted Markdown may run `command:` links.
    pub is_trusted: bool,
    /// `$(icon)` syntax allowed.
    pub support_theme_icons: bool,
    pub support_html: bool,
    pub raw: Value,
}

impl MarkdownString {
    pub fn from_json(v: &Value) -> Option<Self> {
        // A plain string is accepted too (older DTOs / MarkedString).
        if let Some(s) = v.as_str() {
            return Some(Self { value: s.to_owned(), is_trusted: false, support_theme_icons: false, support_html: false, raw: v.clone() });
        }
        let value = v.get("value")?.as_str()?.to_owned();
        let truthy = |k: &str| match v.get(k) {
            Some(Value::Bool(b)) => *b,
            Some(Value::Object(_)) => true,
            _ => false,
        };
        Some(Self {
            value,
            is_trusted: truthy("isTrusted"),
            support_theme_icons: truthy("supportThemeIcons"),
            support_html: truthy("supportHtml"),
            raw: v.clone(),
        })
    }
}

/// What the UI draws.
#[derive(Debug, Clone, PartialEq)]
pub struct HoverPopup {
    /// Document key (`UriComponents::key`).
    pub doc: String,
    pub generation: u64,
    /// Where the pointer rests.
    pub pos: WidgetPos,
    /// The range the hover applies to (first provider that gave one), for highlighting / anchoring.
    pub range: Option<WidgetRange>,
    pub diagnostics: Vec<HoverDiagnostic>,
    pub contents: Vec<MarkdownString>,
    /// No provider request is outstanding.
    pub complete: bool,
}

type Token = u64;

#[derive(Debug)]
struct Active {
    doc: String,
    generation: u64,
    version: u32,
    pos: WidgetPos,
    diagnostics: Vec<HoverDiagnostic>,
    /// (token, provider order) still outstanding
    pending: Vec<(Token, usize)>,
    /// provider order → reply
    results: Vec<(usize, Option<WidgetRange>, Vec<MarkdownString>)>,
    shown: bool,
}

#[derive(Debug, Default)]
pub struct HoverState {
    active: Option<Active>,
}

impl HoverState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a hover. Returns the tokens of the previous hover to cancel.
    pub fn begin(&mut self, doc: &str, generation: u64, version: u32, pos: WidgetPos, diagnostics: Vec<HoverDiagnostic>) -> Vec<Token> {
        let cancel = self.cancel_tokens();
        self.active = Some(Active {
            doc: doc.to_owned(),
            generation,
            version,
            pos,
            diagnostics,
            pending: Vec::new(),
            results: Vec::new(),
            shown: false,
        });
        cancel
    }

    /// Record an outstanding `$provideHover` for provider number `order` (0 = best).
    pub fn add_pending(&mut self, token: Token, order: usize) {
        if let Some(a) = self.active.as_mut() {
            a.pending.push((token, order));
        }
    }

    /// The popup as it stands, if there is anything to show.
    pub fn popup(&self) -> Option<HoverPopup> {
        let a = self.active.as_ref()?;
        let mut results: Vec<&(usize, Option<WidgetRange>, Vec<MarkdownString>)> = a.results.iter().collect();
        results.sort_by_key(|r| r.0);
        let contents: Vec<MarkdownString> = results.iter().flat_map(|r| r.2.iter().cloned()).collect();
        if contents.is_empty() && a.diagnostics.is_empty() {
            return None;
        }
        Some(HoverPopup {
            doc: a.doc.clone(),
            generation: a.generation,
            pos: a.pos,
            range: results.iter().find_map(|r| r.1),
            diagnostics: a.diagnostics.clone(),
            contents,
            complete: a.pending.is_empty(),
        })
    }

    /// A `$provideHover` reply. `current_version` is the document's widget version now; a reply
    /// for an older version is dropped. Returns the popup to (re)draw, if any.
    pub fn reply(&mut self, token: Token, current_version: u32, reply: Result<&Value, ()>) -> Option<HoverPopup> {
        let a = self.active.as_mut()?;
        let idx = a.pending.iter().position(|(t, _)| *t == token)?;
        let (_, order) = a.pending.remove(idx);
        if a.version != current_version {
            return None;
        }
        if let Ok(v) = reply {
            let contents: Vec<MarkdownString> = v
                .get("contents")
                .and_then(Value::as_array)
                .map(|c| c.iter().filter_map(MarkdownString::from_json).filter(|m| !m.value.trim().is_empty()).collect())
                .unwrap_or_default();
            let range = v
                .get("range")
                .and_then(|r| serde_json::from_value(r.clone()).ok())
                .map(WidgetRange::from_exthost);
            if !contents.is_empty() {
                a.results.push((order, range, contents));
            }
        }
        let popup = self.popup();
        let a = self.active.as_mut()?;
        if popup.is_some() {
            a.shown = true;
        }
        popup
    }

    /// Initial popup (diagnostics only) right after [`Self::begin`] when that is all there is
    /// to show, or `None`.
    pub fn initial(&mut self) -> Option<HoverPopup> {
        let p = self.popup()?;
        if let Some(a) = self.active.as_mut() {
            a.shown = true;
        }
        Some(p)
    }

    /// Pointer left / user typed / document closed. Returns `(tokens to cancel, the document of a
    /// popup that was visible)`.
    pub fn end(&mut self) -> (Vec<Token>, Option<String>) {
        let cancel = self.cancel_tokens();
        let shown = self.active.take().filter(|a| a.shown).map(|a| a.doc);
        (cancel, shown)
    }

    /// End the hover only if it is on `doc`.
    pub fn end_for(&mut self, doc: &str) -> (Vec<Token>, Option<String>) {
        if self.active.as_ref().is_some_and(|a| a.doc == doc) {
            self.end()
        } else {
            (Vec::new(), None)
        }
    }

    fn cancel_tokens(&self) -> Vec<Token> {
        self.active.as_ref().map(|a| a.pending.iter().map(|(t, _)| *t).collect()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::Severity;
    use serde_json::json;

    fn diag() -> HoverDiagnostic {
        HoverDiagnostic { owner: "json".into(), severity: Severity::Error, message: "Expected comma".into(), source: None, code: None }
    }

    #[test]
    fn replies_merge_in_provider_order_and_complete() {
        let mut h = HoverState::new();
        assert!(h.begin("d", 1, 3, WidgetPos::new(2, 4), vec![]).is_empty());
        h.add_pending(10, 0);
        h.add_pending(11, 1);
        assert!(h.initial().is_none());
        let p = h.reply(11, 3, Ok(&json!({"id": 1, "contents": [{"value": "second", "isTrusted": {"enabledCommands": ["x"]}}]}))).unwrap();
        assert!(!p.complete);
        assert!(p.contents[0].is_trusted);
        let p = h
            .reply(10, 3, Ok(&json!({"id": 2, "contents": [{"value": "first"}, {"value": "  "}], "range": {"startLineNumber": 3, "startColumn": 3, "endLineNumber": 3, "endColumn": 8}})))
            .unwrap();
        assert!(p.complete);
        assert_eq!(p.contents.iter().map(|m| m.value.as_str()).collect::<Vec<_>>(), vec!["first", "second"]);
        assert_eq!(p.range, Some(WidgetRange::new(WidgetPos::new(2, 2), WidgetPos::new(2, 7))));
        let (cancel, shown) = h.end();
        assert!(cancel.is_empty());
        assert_eq!(shown.as_deref(), Some("d"));
    }

    #[test]
    fn diagnostics_show_immediately_and_stale_or_cancelled_replies_are_dropped() {
        let mut h = HoverState::new();
        h.begin("d", 1, 3, WidgetPos::new(0, 0), vec![diag()]);
        h.add_pending(20, 0);
        let p = h.initial().unwrap();
        assert_eq!(p.diagnostics[0].message, "Expected comma");
        assert!(!p.complete);
        // New hover cancels the old request.
        let cancel = h.begin("d", 1, 3, WidgetPos::new(5, 0), vec![]);
        assert_eq!(cancel, vec![20]);
        assert!(h.reply(20, 3, Ok(&json!({"contents": [{"value": "late"}]}))).is_none());
        // A reply computed for an older version is dropped.
        h.add_pending(21, 0);
        assert!(h.reply(21, 4, Ok(&json!({"contents": [{"value": "stale"}]}))).is_none());
        // Undefined reply → nothing to show.
        h.add_pending(22, 0);
        assert!(h.reply(22, 3, Ok(&Value::Null)).is_none());
        let (_, shown) = h.end();
        assert_eq!(shown, None);
    }
}
