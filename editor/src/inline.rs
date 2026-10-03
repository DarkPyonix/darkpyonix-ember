//! FR-E4: inline completions → ghost text.
//!
//! Protocol (extHost.protocol.ts L2995-3000 at the pinned commit):
//!
//! * `$provideInlineCompletions(handle, uri, IPosition, InlineCompletionContext, token)` →
//!   `IdentifiableInlineCompletions | undefined` = `{ pid, languageId, items: [{ insertText:
//!   string | { snippet }, range?: IRange, command?: ICommandDto, additionalTextEdits?, idx, … }] }`.
//! * `$handleInlineCompletionDidShow(handle, pid, idx, updatedInsertText)` when an item is shown,
//!   `$handleInlineCompletionEndOfLifetime(handle, pid, idx, reason)` when it goes away — only if
//!   the provider registered with `supportsHandleEvents` (`mainThreadLanguageFeatures.ts`
//!   L1383-1420).
//! * `$freeInlineCompletionsList(handle, pid, { kind })` for every list once it is no longer used
//!   (`lostRace | tokenCancellation | other | empty | notTaken`), or the extension host keeps it.
//!
//! Flow: every `CodeChanged` (Automatic) or an explicit trigger restarts a debounce (the largest
//! `debounceDelayMs` among matching providers, else [`DEFAULT_DEBOUNCE`], Monaco's 50 ms).
//! When it fires, any outstanding request is cancelled (RPC `Cancel`) and every matching provider
//! is asked at the cursor. When all have answered, the first item (provider order, then item order)
//! that can be shown as ghost text wins; the other lists are freed. The ghost is one
//! [`crate::widget::DecorationKind::GhostText`] decoration: an empty range at the cursor carrying
//! the text still to insert.
//!
//! Which items can be ghost text (Monaco's rule for the simple case): the item's range lies on the
//! cursor line, ends at the cursor, and the document text from the range start to the cursor is a
//! prefix of `insertText`. The ghost is the rest. Items whose range extends past the cursor (e.g.
//! over an auto-closed bracket) are skipped: FR-38 ghost text can only insert at the cursor.
//!
//! Typing through: if the user types exactly the next characters of the ghost, the ghost shrinks
//! and stays. Typing anything else ends it (`Ignored`, `userTypingDisagreed: true`).
//!
//! Acceptance (FR-38 §38.4): on Tab the widget itself inserts the ghost text — a `CodeChanged`
//! that looks like typing the whole ghost — and then sends `DecorationActivated(ghost id)` in the
//! same frame. So a fully typed-through ghost is parked as "just completed" until the next event:
//! an activation for its id makes it `Accepted`; anything else makes it `Ignored`. If an activation
//! arrives while the ghost text is still pending (a widget that does not insert), the session
//! inserts it with `EditCode`.

use std::time::{Duration, Instant};

use ember_editor_conn::exthost::{self, Call};
use ember_editor_conn::uri::UriComponents;
use serde_json::{json, Value};

use crate::codelens::CommandDto;
use crate::coords::{utf16_to_byte, WidgetPos, WidgetRange};
use crate::widget::DecorationId;

/// Monaco's inline-completions debounce (`InlineCompletionsDebounce`, min = max = 50 ms).
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(50);

type Token = u64;

// ---- debounce ---------------------------------------------------------------------------------

/// A restartable one-shot timer driven by explicit `now` values (no clock inside, so it is
/// testable and the session's event loop owns time).
#[derive(Debug, Default, Clone)]
pub struct Debouncer {
    deadline: Option<Instant>,
}

impl Debouncer {
    /// (Re)start: fire `delay` after `now`, replacing any earlier deadline.
    pub fn schedule(&mut self, now: Instant, delay: Duration) {
        self.deadline = Some(now + delay);
    }
    pub fn cancel(&mut self) {
        self.deadline = None;
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }
    /// `true` once, when `now` has reached the deadline.
    pub fn take_due(&mut self, now: Instant) -> bool {
        match self.deadline {
            Some(d) if d <= now => {
                self.deadline = None;
                true
            }
            _ => false,
        }
    }
}

// ---- items ------------------------------------------------------------------------------------

/// One inline completion item, decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct InlineItem {
    pub handle: i64,
    pub pid: i64,
    pub idx: i64,
    pub insert_text: String,
    /// Widget coordinates; `None` = at the cursor.
    pub range: Option<WidgetRange>,
    pub command: Option<CommandDto>,
    pub additional_edits: Vec<(WidgetRange, String)>,
    pub supports_events: bool,
}

/// Plain text of a snippet string: `$1`, `${1}`, `$TM_x` → "", `${1:default}` → "default",
/// `${1|a,b|}` → "a", `\$` → "$". Good enough for a ghost preview and for insertion when the
/// widget has no snippet mode.
pub fn snippet_to_text(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    fn parse_braced(chars: &[char], mut i: usize, out: &mut String) -> usize {
        // at chars[i] == '{'
        i += 1;
        // name or number
        while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
            i += 1;
        }
        if i < chars.len() && chars[i] == ':' {
            i += 1;
            while i < chars.len() && chars[i] != '}' {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    out.push(chars[i + 1]);
                    i += 2;
                } else if chars[i] == '$' && i + 1 < chars.len() && chars[i + 1] == '{' {
                    i = parse_braced(chars, i + 1, out);
                } else {
                    out.push(chars[i]);
                    i += 1;
                }
            }
        } else if i < chars.len() && chars[i] == '|' {
            i += 1;
            let mut first = true;
            while i < chars.len() && chars[i] != '|' {
                if chars[i] == ',' {
                    first = false;
                } else if first {
                    out.push(chars[i]);
                }
                i += 1;
            }
            i += 1; // closing '|'
        } else {
            while i < chars.len() && chars[i] != '}' {
                i += 1;
            }
        }
        i + 1 // past '}'
    }
    while i < chars.len() {
        match chars[i] {
            '\\' if i + 1 < chars.len() && matches!(chars[i + 1], '$' | '}' | '\\') => {
                out.push(chars[i + 1]);
                i += 2;
            }
            '$' if i + 1 < chars.len() && chars[i + 1] == '{' => {
                i = parse_braced(&chars, i + 1, &mut out);
            }
            '$' if i + 1 < chars.len() && (chars[i + 1].is_ascii_alphanumeric() || chars[i + 1] == '_') => {
                i += 1;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Decode an `IdentifiableInlineCompletions` reply. Returns `(pid, items)`; `None` for
/// `undefined`.
pub fn parse_list(reply: &Value, handle: i64, supports_events: bool) -> Option<(i64, Vec<InlineItem>)> {
    let pid = reply.get("pid")?.as_i64()?;
    let mut items = Vec::new();
    for it in reply.get("items").and_then(Value::as_array).into_iter().flatten() {
        let insert_text = match it.get("insertText") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Object(o)) => match o.get("snippet").and_then(Value::as_str) {
                Some(s) => snippet_to_text(s),
                None => continue,
            },
            _ => continue,
        };
        if it.get("isInlineEdit").and_then(Value::as_bool) == Some(true) {
            continue; // next-edit suggestions are not ghost text
        }
        let range = it.get("range").and_then(|r| serde_json::from_value(r.clone()).ok()).map(WidgetRange::from_exthost);
        let additional_edits = it
            .get("additionalTextEdits")
            .and_then(Value::as_array)
            .map(|edits| {
                edits
                    .iter()
                    .filter_map(|e| {
                        let r = serde_json::from_value(e.get("range")?.clone()).ok()?;
                        Some((WidgetRange::from_exthost(r), e.get("text").and_then(Value::as_str).unwrap_or("").to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        items.push(InlineItem {
            handle,
            pid,
            idx: it.get("idx").and_then(Value::as_i64).unwrap_or(items.len() as i64),
            insert_text,
            range,
            command: it.get("command").and_then(CommandDto::from_json),
            additional_edits,
            supports_events,
        });
    }
    Some((pid, items))
}

/// The ghost text for `item` at `cursor`, given the cursor line's text; `None` if the item cannot
/// be shown as ghost text there.
pub fn ghost_for(item: &InlineItem, cursor: WidgetPos, line: &str) -> Option<String> {
    let range = item.range.unwrap_or(WidgetRange::empty(cursor));
    if range.start.line != cursor.line || range.end != cursor || range.start.col > cursor.col {
        return None;
    }
    let a = utf16_to_byte(line, range.start.col)?;
    let b = utf16_to_byte(line, cursor.col)?;
    let prefix = &line[a..b];
    let rest = item.insert_text.strip_prefix(prefix)?;
    if rest.is_empty() {
        None
    } else {
        Some(rest.to_owned())
    }
}

// ---- ghost lifecycle --------------------------------------------------------------------------

/// The ghost text currently shown in one document.
#[derive(Debug, Clone, PartialEq)]
pub struct Ghost {
    pub item: InlineItem,
    pub id: DecorationId,
    /// Where the remaining ghost text is anchored (the cursor).
    pub pos: WidgetPos,
    /// What is left to insert.
    pub text: String,
    /// Widget version the item was computed for (base of its `additionalTextEdits`).
    pub base_version: u32,
    /// Typed through completely (or inserted by the widget on Tab); waiting one event to learn
    /// whether it was an acceptance.
    pub completed: bool,
}

/// How a typed change relates to the ghost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeThrough {
    /// The change typed the next part of the ghost; it shrank.
    Continue,
    /// The change typed (or the widget inserted) all of it.
    Completed,
    /// Anything else.
    Disagreed,
}

impl Ghost {
    /// Apply one widget change to the ghost.
    pub fn type_through(&mut self, range: WidgetRange, text: &str) -> TypeThrough {
        if self.completed || !range.is_empty() || range.start != self.pos || text.is_empty() {
            return TypeThrough::Disagreed;
        }
        match self.text.strip_prefix(text) {
            Some("") => {
                self.pos = crate::coords::end_of_insert(self.pos, text);
                self.text.clear();
                self.completed = true;
                TypeThrough::Completed
            }
            Some(rest) => {
                self.pos = crate::coords::end_of_insert(self.pos, text);
                self.text = rest.to_owned();
                TypeThrough::Continue
            }
            None => TypeThrough::Disagreed,
        }
    }
}

/// `InlineCompletionEndOfLifeReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    Accepted,
    Rejected,
    Ignored { user_typing_disagreed: bool },
}

impl EndReason {
    pub fn to_json(self) -> Value {
        match self {
            Self::Accepted => json!({ "kind": 0, "alternativeAction": false }),
            Self::Rejected => json!({ "kind": 1 }),
            Self::Ignored { user_typing_disagreed } => json!({ "kind": 2, "userTypingDisagreed": user_typing_disagreed }),
        }
    }
}

/// `$handleInlineCompletionDidShow` for a newly shown item (only if the provider wants events).
pub fn did_show_call(item: &InlineItem) -> Option<Call> {
    item.supports_events
        .then(|| exthost::handle_inline_completion_did_show(item.handle, item.pid, item.idx, &item.insert_text))
}

/// The calls when a shown item goes away: end-of-lifetime (if wanted), then free its list.
pub fn end_calls(item: &InlineItem, reason: EndReason) -> Vec<Call> {
    let mut calls = Vec::new();
    if item.supports_events {
        calls.push(exthost::handle_inline_completion_end_of_lifetime(item.handle, item.pid, item.idx, reason.to_json()));
    }
    calls.push(exthost::free_inline_completions_list(item.handle, item.pid, "other"));
    calls
}

// ---- request bookkeeping ----------------------------------------------------------------------

/// What a reply did to the outstanding request.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplyOutcome {
    /// Not for the current request (cancelled or superseded): ignore.
    Ignored,
    /// For the current request, others still outstanding.
    Pending,
    /// The document changed since the request was sent: free these lists (`lostRace`) and drop.
    Stale { free: Vec<(i64, i64)> },
    /// All providers answered: replies in provider order.
    Done { pos: WidgetPos, replies: Vec<(i64, Value)> },
}

#[derive(Debug, Clone)]
struct Request {
    version: u32,
    pos: WidgetPos,
    pending: Vec<(Token, i64, usize)>,
    replies: Vec<(usize, i64, Value)>,
}

/// Inline-completion state of one document.
#[derive(Debug, Default)]
pub struct InlineDoc {
    pub debounce: Debouncer,
    /// (cursor, explicit) of the scheduled trigger.
    pub trigger: Option<(WidgetPos, bool)>,
    request: Option<Request>,
    pub ghost: Option<Ghost>,
}

impl InlineDoc {
    pub fn new() -> Self {
        Self::default()
    }

    /// Schedule a request at `pos` after `delay` (restarting any earlier schedule).
    pub fn schedule(&mut self, now: Instant, delay: Duration, pos: WidgetPos, explicit: bool) {
        self.trigger = Some((pos, explicit));
        self.debounce.schedule(now, delay);
    }

    /// Start a request (the debounce fired). Returns the tokens of the previous request to
    /// cancel.
    pub fn begin(&mut self, version: u32, pos: WidgetPos) -> Vec<Token> {
        let cancel = self.cancel();
        self.request = Some(Request { version, pos, pending: Vec::new(), replies: Vec::new() });
        cancel
    }

    pub fn add_pending(&mut self, token: Token, handle: i64, order: usize) {
        if let Some(r) = self.request.as_mut() {
            r.pending.push((token, handle, order));
        }
    }

    /// Cancel the outstanding request and the scheduled trigger. Returns tokens to cancel.
    pub fn cancel(&mut self) -> Vec<Token> {
        self.debounce.cancel();
        self.trigger = None;
        self.request.take().map(|r| r.pending.into_iter().map(|(t, _, _)| t).collect()).unwrap_or_default()
    }

    pub fn has_request(&self) -> bool {
        self.request.is_some()
    }

    /// A reply for `token`, with the document's widget version now.
    pub fn reply(&mut self, token: Token, current_version: u32, value: Value) -> ReplyOutcome {
        let Some(req) = self.request.as_mut() else { return ReplyOutcome::Ignored };
        let Some(i) = req.pending.iter().position(|(t, _, _)| *t == token) else { return ReplyOutcome::Ignored };
        let (_, handle, order) = req.pending.remove(i);
        if req.version != current_version {
            let mut free: Vec<(i64, i64)> = req
                .replies
                .drain(..)
                .filter_map(|(_, h, v)| v.get("pid").and_then(Value::as_i64).map(|p| (h, p)))
                .collect();
            if let Some(p) = value.get("pid").and_then(Value::as_i64) {
                free.push((handle, p));
            }
            // Outstanding ones of this request are dropped too (the caller cancels them).
            return ReplyOutcome::Stale { free };
        }
        req.replies.push((order, handle, value));
        if !req.pending.is_empty() {
            return ReplyOutcome::Pending;
        }
        let mut req = self.request.take().unwrap();
        req.replies.sort_by_key(|(o, _, _)| *o);
        ReplyOutcome::Done { pos: req.pos, replies: req.replies.into_iter().map(|(_, h, v)| (h, v)).collect() }
    }

    /// Tokens still outstanding (to cancel after a [`ReplyOutcome::Stale`]).
    pub fn take_pending(&mut self) -> Vec<Token> {
        self.request.take().map(|r| r.pending.into_iter().map(|(t, _, _)| t).collect()).unwrap_or_default()
    }
}

/// `InlineCompletionContext` for a request.
pub fn context(explicit: bool, now_ms: f64) -> exthost::InlineCompletionContext {
    exthost::InlineCompletionContext {
        trigger_kind: if explicit { 1 } else { 0 },
        selected_suggestion_info: None,
        request_uuid: uuid::Uuid::new_v4().to_string(),
        include_inline_edits: false,
        include_inline_completions: true,
        request_issued_date_time: now_ms,
        earliest_shown_date_time: now_ms,
    }
}

/// `$provideInlineCompletions` for one provider.
pub fn provide_call(handle: i64, uri: &UriComponents, pos: WidgetPos, explicit: bool, now_ms: f64) -> Call {
    exthost::provide_inline_completions(handle, uri, pos.to_exthost(), &context(explicit, now_ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(text: &str, range: Option<WidgetRange>) -> InlineItem {
        InlineItem {
            handle: 3,
            pid: 9,
            idx: 0,
            insert_text: text.into(),
            range,
            command: None,
            additional_edits: vec![],
            supports_events: true,
        }
    }

    fn p(line: u32, col: u32) -> WidgetPos {
        WidgetPos::new(line, col)
    }

    #[test]
    fn debounce_restarts_and_fires_once() {
        let t0 = Instant::now();
        let mut d = Debouncer::default();
        d.schedule(t0, Duration::from_millis(50));
        assert!(!d.take_due(t0 + Duration::from_millis(30)));
        // A keystroke at 30 ms restarts it.
        d.schedule(t0 + Duration::from_millis(30), Duration::from_millis(50));
        assert!(!d.take_due(t0 + Duration::from_millis(60)));
        assert_eq!(d.deadline(), Some(t0 + Duration::from_millis(80)));
        assert!(d.take_due(t0 + Duration::from_millis(80)));
        assert!(!d.take_due(t0 + Duration::from_millis(200)));
        d.schedule(t0, Duration::from_millis(1));
        d.cancel();
        assert!(!d.take_due(t0 + Duration::from_secs(1)));
    }

    #[test]
    fn new_request_cancels_the_outstanding_one_and_late_replies_are_ignored() {
        let mut s = InlineDoc::new();
        assert!(s.begin(1, p(0, 3)).is_empty());
        s.add_pending(100, 3, 0);
        s.add_pending(101, 4, 1);
        let cancel = s.begin(2, p(0, 4));
        assert_eq!(cancel, vec![100, 101]);
        assert_eq!(s.reply(100, 2, json!({"pid": 1, "items": []})), ReplyOutcome::Ignored);
        s.add_pending(102, 3, 0);
        assert!(matches!(s.reply(102, 2, json!({"pid": 2, "items": []})), ReplyOutcome::Done { .. }));
        assert!(!s.has_request());
    }

    #[test]
    fn replies_wait_for_all_providers_and_sort_by_order() {
        let mut s = InlineDoc::new();
        s.begin(5, p(1, 2));
        s.add_pending(1, 30, 0);
        s.add_pending(2, 40, 1);
        assert_eq!(s.reply(2, 5, json!({"pid": 20})), ReplyOutcome::Pending);
        match s.reply(1, 5, json!({"pid": 10})) {
            ReplyOutcome::Done { pos, replies } => {
                assert_eq!(pos, p(1, 2));
                assert_eq!(replies.iter().map(|(h, _)| *h).collect::<Vec<_>>(), vec![30, 40]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn stale_replies_free_their_lists() {
        let mut s = InlineDoc::new();
        s.begin(5, p(0, 0));
        s.add_pending(1, 30, 0);
        s.add_pending(2, 40, 1);
        assert_eq!(s.reply(1, 5, json!({"pid": 10})), ReplyOutcome::Pending);
        assert_eq!(s.reply(2, 6, json!({"pid": 11})), ReplyOutcome::Stale { free: vec![(30, 10), (40, 11)] });
    }

    #[test]
    fn explicit_cancel_clears_trigger_and_request() {
        let t0 = Instant::now();
        let mut s = InlineDoc::new();
        s.schedule(t0, DEFAULT_DEBOUNCE, p(0, 1), false);
        s.begin(1, p(0, 1));
        s.add_pending(7, 1, 0);
        assert_eq!(s.cancel(), vec![7]);
        assert!(s.trigger.is_none());
        assert!(!s.debounce.take_due(t0 + Duration::from_secs(1)));
    }

    #[test]
    fn ghost_text_requires_a_prefix_match_ending_at_the_cursor() {
        let line = "let x = fo";
        let cursor = p(0, 10);
        // No range: inserted at the cursor.
        assert_eq!(ghost_for(&item("o()", None), cursor, line).as_deref(), Some("o()"));
        // Range over the typed prefix "fo".
        let r = WidgetRange::new(p(0, 8), cursor);
        assert_eq!(ghost_for(&item("foo()", Some(r)), cursor, line).as_deref(), Some("o()"));
        // Prefix mismatch.
        assert_eq!(ghost_for(&item("bar()", Some(r)), cursor, line), None);
        // Range past the cursor.
        assert_eq!(ghost_for(&item("foo()", Some(WidgetRange::new(p(0, 8), p(0, 11)))), cursor, line), None);
        // Nothing left to show.
        assert_eq!(ghost_for(&item("fo", Some(r)), cursor, line), None);
        // UTF-16 columns: '😀' is two units.
        let line = "😀a";
        assert_eq!(ghost_for(&item("ab", Some(WidgetRange::new(p(0, 2), p(0, 3)))), p(0, 3), line).as_deref(), Some("b"));
        // Multi-line ghost text is allowed.
        assert_eq!(ghost_for(&item("{\n}", None), p(0, 3), "fn ").as_deref(), Some("{\n}"));
    }

    #[test]
    fn typing_through_shrinks_then_completes_or_disagrees() {
        let mk = || Ghost { item: item("hello", None), id: DecorationId(1), pos: p(0, 0), text: "hello".into(), base_version: 0, completed: false };
        let mut g = mk();
        assert_eq!(g.type_through(WidgetRange::empty(p(0, 0)), "he"), TypeThrough::Continue);
        assert_eq!((g.pos, g.text.as_str()), (p(0, 2), "llo"));
        assert_eq!(g.type_through(WidgetRange::empty(p(0, 2)), "llo"), TypeThrough::Completed);
        assert!(g.completed);
        // The widget inserting the whole ghost on Tab looks the same.
        let mut g = mk();
        assert_eq!(g.type_through(WidgetRange::empty(p(0, 0)), "hello"), TypeThrough::Completed);
        let mut g = mk();
        assert_eq!(g.type_through(WidgetRange::empty(p(0, 0)), "x"), TypeThrough::Disagreed);
        let mut g = mk();
        assert_eq!(g.type_through(WidgetRange::empty(p(0, 1)), "e"), TypeThrough::Disagreed);
        let mut g = mk();
        assert_eq!(g.type_through(WidgetRange::new(p(0, 0), p(0, 1)), "h"), TypeThrough::Disagreed);
    }

    #[test]
    fn list_parsing_and_snippets() {
        let reply = json!({"pid": 4, "languageId": "rust", "items": [
            {"insertText": "foo()", "idx": 0, "command": {"id": "__vsc", "title": "", "arguments": ["c /1"]}},
            {"insertText": {"snippet": "bar(${1:x}, $2)$0"}, "idx": 1,
             "range": {"startLineNumber": 1, "startColumn": 1, "endLineNumber": 1, "endColumn": 3},
             "additionalTextEdits": [{"range": {"startLineNumber": 1, "startColumn": 1, "endLineNumber": 1, "endColumn": 1}, "text": "use x;\n"}]},
            {"insertText": "edit", "idx": 2, "isInlineEdit": true}
        ]});
        let (pid, items) = parse_list(&reply, 3, false).unwrap();
        assert_eq!(pid, 4);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].command.as_ref().unwrap().id, "__vsc");
        assert_eq!(items[1].insert_text, "bar(x, )");
        assert_eq!(items[1].range, Some(WidgetRange::new(p(0, 0), p(0, 2))));
        assert_eq!(items[1].additional_edits[0].1, "use x;\n");
        assert!(parse_list(&Value::Null, 3, false).is_none());
        assert_eq!(snippet_to_text(r"a\$b ${1|one,two|} $TM_FILENAME ${2:${3:deep}}"), "a$b one  deep");
    }

    #[test]
    fn lifetime_calls_respect_supports_events() {
        let mut it = item("x", None);
        let calls = end_calls(&it, EndReason::Accepted);
        assert_eq!(calls[0].method, "$handleInlineCompletionEndOfLifetime");
        assert_eq!(calls[0].args[3].as_json().unwrap()["kind"], 0);
        assert_eq!(calls[1].method, "$freeInlineCompletionsList");
        assert!(did_show_call(&it).is_some());
        it.supports_events = false;
        assert_eq!(end_calls(&it, EndReason::Ignored { user_typing_disagreed: true }).len(), 1);
        assert!(did_show_call(&it).is_none());
        assert_eq!(EndReason::Ignored { user_typing_disagreed: true }.to_json(), json!({"kind": 2, "userTypingDisagreed": true}));
    }
}
