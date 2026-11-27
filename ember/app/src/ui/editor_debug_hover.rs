//! The debug hover of the code editor (FR-L31.42), Code-OSS's `DebugHoverWidget`
//! (`debugHover.ts`) and the hover part of `debugEditorContribution.ts`.
//!
//! While the shown session is stopped, a pointer that rests on an expression in the file of the
//! focused frame shows the expression's value instead of the language hover: the expression is
//! the one `getExactExpressionStartAndEnd` finds ([`ember_editor::debugger::hover`]), the
//! session layer evaluates it in the `hover` context or looks it up in the frame's scopes, and a
//! structured value opens as a tree whose children load as it expands (`variablesReference`).
//! Where there is no expression, or it has no value there, the language hover shows, as
//! `showHover` falls back to `showEditorHover`. Alt (Option on macOS) switches a shown debug
//! hover to the language hover (#84561), and the card says so. A key other than Ctrl, Cmd and
//! Alt, or a press in the editor, hides it (`onKeyDown`, `onEditorMouseDown`). The card stays
//! while the pointer moves onto it (`editor.hover.sticky`), and goes `editor.hover.hidingDelay`
//! (300 ms) after the pointer left the expression for somewhere else.
//!
//! The pure part is [`DebugHover`]; [`view`] holds one per window and draws its card.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use dioxus_compose::html::prelude::*;
use dioxus_compose::prelude::{Props, component};
use dioxus_core::EventHandler;
use ember_editor::DebugView;
use ember_editor::coords::WidgetPos;
use ember_editor::debugger::hover::expression_at;
use ember_editor::debugger::source::same_resource;
use ember_editor::debugger::{DebugRequest, EvalResult, VariableInfo};
use ember_editor_conn::uri::UriComponents;

/// Hover ids start here, apart from those of the Run and Debug window (`1 << 40`).
const HOVER_EVAL_BASE: u64 = 1 << 41;
/// Code-OSS's `editor.hover.hidingDelay` default: how long a card waits after the pointer left
/// its expression before it goes, so the pointer can reach it.
// TODO(compose-rust#1091): keep Code-OSS's safe triangle once the hover reports the pointer's
// window position; this fixed grace stands in for it.
pub(crate) const HIDING_DELAY: Duration = Duration::from_millis(300);
/// Separates the parts of a tree row's path.
const SEP: char = '\u{1f}';

/// What a hover is over: the expression on one line of a document, in widget coordinates
/// (0-based line, 0-based UTF-16 columns, `end` exclusive), and where the pointer rested.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HoverTarget {
    pub uri: UriComponents,
    pub line: u32,
    pub start: u32,
    pub end: u32,
    pub expression: String,
    pub session: String,
    pub frame_id: i64,
    pub pointer: WidgetPos,
    /// The hover anchor under the pointer, passed on when the language hover shows instead.
    pub decoration: u64,
}

impl HoverTarget {
    fn same_place(&self, other: &HoverTarget) -> bool {
        self.uri == other.uri
            && (self.line, self.start, self.end) == (other.line, other.start, other.end)
            && self.session == other.session
            && self.frame_id == other.frame_id
    }
}

/// What a resting pointer does.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RestStep {
    /// The language hover shows, as without a debugger.
    Language,
    /// The debug hover asks for the value; the language hover is hidden meanwhile
    /// (`preventDefaultEditorHover`).
    Debug(DebugRequest),
    /// The same expression is shown or asked for already (`ShowDebugHoverResult.NOT_CHANGED`).
    Keep,
}

/// What the answer to a hover does.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AnswerStep {
    /// Not this hover's answer.
    Ignore,
    /// Draw the card; for a structured value, the children of its root are asked for.
    Show(Option<DebugRequest>),
    /// No value (`NOT_AVAILABLE`): the language hover shows at the target's pointer.
    Fallback(HoverTarget),
}

/// One row of a structured value's tree.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HoverRow {
    pub path: String,
    pub depth: usize,
    pub name: String,
    pub value: String,
    pub type_name: Option<String>,
    pub reference: i64,
    pub expanded: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct Shown {
    target: HoverTarget,
    result: EvalResult,
    children: HashMap<i64, Result<Vec<VariableInfo>, String>>,
    expanded: HashSet<String>,
}

/// What the card draws.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CardModel {
    pub expression: String,
    pub value: String,
    pub type_name: Option<String>,
    /// A structured value: a title and a tree of `rows`, else the value alone.
    pub complex: bool,
    pub rows: Vec<HoverRow>,
}

/// The debug hover of one window.
#[derive(Debug, Clone, Default)]
pub(crate) struct DebugHover {
    next: u64,
    pending: Option<(u64, HoverTarget)>,
    shown: Option<Shown>,
    /// Alt was pressed over this pointer: the language hover shows there until the pointer rests
    /// somewhere else. Code-OSS waits for the key's release instead, which the code editor does
    /// not report (a toolkit gap, see the module's notes in SPEC FR-L31.42).
    alt_at: Option<(UriComponents, WidgetPos)>,
    on_card: bool,
    left_at: Option<Instant>,
}

impl DebugHover {
    /// The pointer rests at `pos` of `uri`, whose line is `line_text`.
    pub fn rest(
        &mut self,
        view: &DebugView,
        uri: &UriComponents,
        line_text: &str,
        pos: WidgetPos,
        decoration: u64,
    ) -> RestStep {
        self.left_at = None;
        if let Some((at_uri, at)) = &self.alt_at {
            if at_uri == uri && *at == pos {
                return RestStep::Language;
            }
            self.alt_at = None;
        }
        // Only while a thread is stopped, and only in the file of the focused frame
        // (`showHover`: `sf.source.uri` is the model's).
        let Some(focus) = view.focused_location().filter(|f| same_resource(&f.uri, uri)) else {
            self.hide();
            return RestStep::Language;
        };
        let Some(found) = expression_at(line_text, pos.col + 1) else {
            self.hide();
            return RestStep::Language;
        };
        let target = HoverTarget {
            uri: uri.clone(),
            line: pos.line,
            start: found.start_column - 1,
            end: found.end_column - 1,
            expression: found.expression,
            session: focus.session.clone(),
            frame_id: focus.frame_id,
            pointer: pos,
            decoration,
        };
        let same_shown = self.shown.as_ref().is_some_and(|s| s.target.same_place(&target));
        let same_pending = self.pending.as_ref().is_some_and(|(_, t)| t.same_place(&target));
        if same_shown || same_pending {
            return RestStep::Keep;
        }
        self.next += 1;
        let id = HOVER_EVAL_BASE + self.next;
        let request = DebugRequest::Hover {
            id,
            session: target.session.clone(),
            frame_id: target.frame_id,
            uri: uri.clone(),
            line: target.line + 1,
            column: target.start + 1,
            expression: target.expression.clone(),
        };
        self.pending = Some((id, target));
        RestStep::Debug(request)
    }

    /// The answer to a hover request: the value, or why there is none.
    pub fn evaluated(&mut self, id: u64, result: &Result<EvalResult, String>) -> AnswerStep {
        let Some((pending, target)) = self.pending.take() else {
            return AnswerStep::Ignore;
        };
        if pending != id {
            self.pending = Some((pending, target));
            return AnswerStep::Ignore;
        }
        match result {
            Ok(value) => {
                let children = (value.variables_reference > 0).then(|| DebugRequest::Variables {
                    session: target.session.clone(),
                    reference: value.variables_reference,
                });
                self.shown = Some(Shown {
                    target,
                    result: value.clone(),
                    children: HashMap::new(),
                    expanded: HashSet::new(),
                });
                self.on_card = false;
                AnswerStep::Show(children)
            }
            Err(_) => {
                self.shown = None;
                AnswerStep::Fallback(target)
            }
        }
    }

    /// Variables of `reference` came; returns whether they belong to the shown tree.
    pub fn variables(
        &mut self,
        session: &str,
        reference: i64,
        variables: &[VariableInfo],
        error: Option<&String>,
    ) -> bool {
        let Some(shown) = self.shown.as_mut().filter(|s| s.target.session == session) else {
            return false;
        };
        let wanted = reference == shown.result.variables_reference
            || shown.children.values().any(|c| {
                c.as_ref()
                    .is_ok_and(|vars| vars.iter().any(|v| v.variables_reference == reference))
            });
        if !wanted {
            return false;
        }
        let value = match error {
            Some(e) => Err(e.clone()),
            None => Ok(variables.to_vec()),
        };
        shown.children.insert(reference, value);
        true
    }

    /// Expand or collapse a row; a row opened for the first time asks for its children.
    pub fn toggle(&mut self, row: &HoverRow) -> Option<DebugRequest> {
        let shown = self.shown.as_mut()?;
        if row.reference <= 0 {
            return None;
        }
        if shown.expanded.remove(&row.path) {
            return None;
        }
        shown.expanded.insert(row.path.clone());
        (!shown.children.contains_key(&row.reference)).then(|| DebugRequest::Variables {
            session: shown.target.session.clone(),
            reference: row.reference,
        })
    }

    /// Alt went down: a shown debug hover gives way to the language hover at its pointer, which
    /// is returned (`altListener`). A hover still being asked for is dropped.
    pub fn alt_pressed(&mut self) -> Option<HoverTarget> {
        let target = self
            .shown
            .as_ref()
            .map(|s| s.target.clone())
            .or_else(|| self.pending.as_ref().map(|(_, t)| t.clone()))?;
        let was_shown = self.shown.is_some();
        self.alt_at = Some((target.uri.clone(), target.pointer));
        self.hide();
        was_shown.then_some(target)
    }

    /// The pointer left the expression (`CodeHover` `Leave`): the card goes after
    /// [`HIDING_DELAY`] unless the pointer is on it.
    pub fn leave(&mut self, now: Instant) {
        if self.shown.is_some() || self.pending.is_some() {
            self.left_at = Some(now);
        }
    }

    pub fn enter_card(&mut self) {
        self.on_card = true;
        self.left_at = None;
    }

    pub fn leave_card(&mut self, now: Instant) {
        self.on_card = false;
        self.left_at = Some(now);
    }

    pub fn hide(&mut self) {
        self.pending = None;
        self.shown = None;
        self.on_card = false;
        self.left_at = None;
    }

    /// The session moved on: a hover of a frame that is no longer focused goes
    /// (`onFocusStackFrame`, and its references are gone once the thread runs).
    pub fn follow(&mut self, view: &DebugView) {
        let focus = view.focused_location();
        let live = |t: &HoverTarget| {
            focus
                .as_ref()
                .is_some_and(|f| f.session == t.session && f.frame_id == t.frame_id && same_resource(&f.uri, &t.uri))
        };
        if self.shown.as_ref().is_some_and(|s| !live(&s.target))
            || self.pending.as_ref().is_some_and(|(_, t)| !live(t))
        {
            self.hide();
        }
    }

    /// The shown hover's target while the card is up at `now`.
    pub fn visible(&self, now: Instant) -> Option<&HoverTarget> {
        let shown = self.shown.as_ref()?;
        let gone = !self.on_card && self.left_at.is_some_and(|t| now.duration_since(t) >= HIDING_DELAY);
        (!gone).then_some(&shown.target)
    }

    /// What the card draws at `now`.
    pub fn card(&self, now: Instant) -> Option<CardModel> {
        self.visible(now)?;
        let shown = self.shown.as_ref()?;
        let complex = shown.result.variables_reference > 0;
        let mut rows = Vec::new();
        if complex {
            self.rows(shown, shown.result.variables_reference, "", 0, &mut rows);
        }
        Some(CardModel {
            expression: shown.target.expression.clone(),
            value: shown.result.result.clone(),
            type_name: shown.result.type_name.clone(),
            complex,
            rows,
        })
    }

    fn rows(&self, shown: &Shown, reference: i64, path: &str, depth: usize, out: &mut Vec<HoverRow>) {
        match shown.children.get(&reference) {
            None => {}
            Some(Err(error)) => out.push(HoverRow {
                path: format!("{path}{SEP}!"),
                depth,
                name: String::new(),
                value: String::new(),
                type_name: None,
                reference: 0,
                expanded: false,
                error: Some(error.clone()),
            }),
            Some(Ok(vars)) => {
                for v in vars {
                    let child = format!("{path}{SEP}{}", v.name);
                    let expanded = v.variables_reference > 0 && shown.expanded.contains(&child);
                    out.push(HoverRow {
                        path: child.clone(),
                        depth,
                        name: v.name.clone(),
                        value: v.value.clone(),
                        type_name: v.type_name.clone(),
                        reference: v.variables_reference,
                        expanded,
                        error: None,
                    });
                    if expanded {
                        self.rows(shown, v.variables_reference, &child, depth + 1, out);
                    }
                }
            }
        }
    }
}

// TODO(compose-rust#1089): hold the language hover until Alt is released once the hover event
// carries modifier keys (or the editor reports key release).
/// Whether a key is Alt alone, as the focused code editor offers it (`on_key_preview`).
pub(crate) fn is_alt_alone(key: compose_rust::Key, text: &str, alt: bool) -> bool {
    alt && key == compose_rust::Key::Other && text.is_empty()
}

/// Whether a key leaves the hover up: Ctrl or Cmd alone (the stop key of `onKeyDown`), or Alt,
/// which switches it instead.
pub(crate) fn keeps_hover(key: compose_rust::Key, text: &str, ctrl: bool, alt: bool, meta: bool) -> bool {
    key == compose_rust::Key::Other && text.is_empty() && (ctrl || alt || meta)
}

/// The card's tip (Code-OSS `quickTip`).
pub(crate) fn tip(mac: bool) -> String {
    let key = if mac { "Option" } else { "Alt" };
    format!("{key} 키를 누르면 편집기 언어 호버로 바뀝니다")
}

#[component]
pub(crate) fn DebugHoverCard(
    model: CardModel,
    style: String,
    mac: bool,
    toggle: EventHandler<HoverRow>,
    enter: EventHandler<()>,
    leave: EventHandler<()>,
) -> Element {
    let title = match &model.type_name {
        Some(t) if !t.is_empty() => format!("{}: {t}", model.expression),
        _ => model.expression.clone(),
    };
    let tip = tip(mac);
    rsx! {
        div { id: "debug-hover", class: "ed-menu ed-dbg-hover", role: "tooltip", "aria-label": "디버그 호버", "data-ed": "debug-hover", style: "{style}",
            onmouseenter: move |_| enter.call(()),
            onmouseleave: move |_| leave.call(()),
            if model.complex {
                div { id: "debug-hover-title", class: "ed-dbg-htitle", title: "{title}",
                    span { class: "ed-dbg-val", "{model.value}" }
                }
                div { class: "ed-dbg-htree", role: "tree", "aria-label": "{model.expression}",
                    for (i, row) in model.rows.into_iter().enumerate() {
                        {
                            let label = match (&row.error, row.value.is_empty()) {
                                (Some(e), _) => e.clone(),
                                (None, true) => row.name.clone(),
                                (None, false) => format!("{} = {}", row.name, row.value),
                            };
                            let row_title = row.type_name.clone().unwrap_or_default();
                            let depth = row.depth;
                            let expandable = row.reference > 0;
                            let expanded = row.expanded;
                            let clicked = row.clone();
                            rsx! {
                                div { key: "{row.path}", id: "debug-hover-row-{i}", class: "ed-tn ed-dbg-var", role: "treeitem",
                                    "aria-expanded": if expandable { "{expanded}" } else { "" },
                                    "aria-label": "{label}", title: "{row_title}", style: "--d:{depth}",
                                    onclick: move |_| { if expandable { toggle.call(clicked.clone()); } },
                                    if expandable { {super::editor_debug_view::chevron(expanded)} } else { span { class: "chev-sp" } }
                                    if let Some(error) = &row.error {
                                        span { class: "ed-dbg-err ellipsis", "{error}" }
                                    } else {
                                        span { class: "ed-dbg-name", "{row.name}" }
                                        if !row.value.is_empty() {
                                            span { class: "ed-dbg-val ellipsis", "{row.value}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                div { id: "debug-hover-tip", class: "ed-dbg-tip ed-dbg-muted", "{tip}" }
            } else {
                div { id: "debug-hover-value", class: "ed-dbg-hval ed-dbg-val", title: "{title}", "{model.value}" }
            }
        }
    }
}

/// The window's debug hover: one [`DebugHover`] per window thread, its card and its highlight.
pub(crate) mod view {
    use std::cell::{Cell, RefCell};
    use std::time::Instant;

    use compose_rust::code::{CodeRange, Decoration};
    use compose_rust::schema::ColorRole;
    use dioxus_compose::html::prelude::*;
    use dioxus_compose::html::prelude::dioxus_signals::WritableExt;
    use ember_editor::coords::WidgetPos;
    use ember_editor::debugger::DebugUpdate;
    use ember_editor::widget::HoverPhase;
    use ember_editor_conn::uri::UriComponents;

    use super::{AnswerStep, DebugHover, DebugHoverCard, HIDING_DELAY, HoverTarget, RestStep};
    use crate::editor_host::HostCommand;
    use crate::ui::editor_workbench::Shared;

    /// Code-OSS's `editor.hoverHighlightBackground` has no colour role of its own in compose-rust
    /// yet, so the expression is marked with the quiet fill of the primary accent.
    // TODO(compose-rust#1090): Code-OSS's translucent `editor.hoverHighlightBackground`.
    const HIGHLIGHT: ColorRole = ColorRole::PrimaryContainer;

    thread_local! {
        static HOVER: RefCell<DebugHover> = RefCell::new(DebugHover::default());
        /// The card's anchor (document key, line, start) the editor was asked to place.
        static ASKED: RefCell<Option<(String, u32, u32)>> = const { RefCell::new(None) };
        static RECT: Cell<Option<compose_rust::code::CodeRect>> = const { Cell::new(None) };
    }

    fn redraw(s: &Shared) {
        let mut revision = s.revision;
        *revision.write() += 1;
    }

    fn send(s: &Shared, request: ember_editor::debugger::DebugRequest) {
        s.host.send(HostCommand::Debug(request));
    }

    /// The language hover's own event at `pos`, as the code editor would have sent it.
    fn language(s: &Shared, uri: &UriComponents, decoration: u64, pos: WidgetPos, phase: HoverPhase) {
        let event = s.bridge.borrow().on_code_hovered(uri, decoration, pos, phase);
        if let Some((uri, generation, event)) = event {
            s.host.send(HostCommand::Widget(uri, generation, event));
        }
    }

    fn show_language(s: &Shared, target: &HoverTarget) {
        language(s, &target.uri, target.decoration, target.pointer, HoverPhase::Rest);
    }

    /// The code editor's hover event. Returns `true` when the debug hover took it, so the
    /// language hover does not see it.
    pub fn code_hover(s: &Shared, uri: &UriComponents, decoration: u64, pos: WidgetPos, phase: HoverPhase) -> bool {
        if phase == HoverPhase::Leave {
            HOVER.with_borrow_mut(|h| h.leave(Instant::now()));
            // Wake the window when the card's time is up.
            let revision = s.revision;
            crate::services::services().rt.spawn(async move {
                tokio::time::sleep(HIDING_DELAY + std::time::Duration::from_millis(20)).await;
                let mut revision = revision;
                *revision.write() += 1;
            });
            return false;
        }
        let line_text = {
            let bridge = s.bridge.borrow();
            let Some(doc) = bridge.document(uri) else { return false };
            doc.text.split('\n').nth(pos.line as usize).unwrap_or("").to_owned()
        };
        let view = s.bridge.borrow().debug().clone();
        let step = HOVER.with_borrow_mut(|h| h.rest(&view, uri, &line_text, pos, decoration));
        match step {
            RestStep::Language => {
                redraw(s);
                false
            }
            RestStep::Keep => true,
            RestStep::Debug(request) => {
                // `preventDefaultEditorHover`: the language hover goes while the value is asked for.
                language(s, uri, decoration, pos, HoverPhase::Leave);
                send(s, request);
                true
            }
        }
    }

    /// A debug update, after the bridge took it.
    pub fn on_debug(s: &Shared, update: &DebugUpdate) {
        let changed = match update {
            DebugUpdate::Evaluated { id, result, .. } => {
                match HOVER.with_borrow_mut(|h| h.evaluated(*id, result)) {
                    AnswerStep::Ignore => false,
                    AnswerStep::Show(children) => {
                        if let Some(request) = children {
                            send(s, request);
                        }
                        true
                    }
                    AnswerStep::Fallback(target) => {
                        show_language(s, &target);
                        true
                    }
                }
            }
            DebugUpdate::Variables { session, reference, variables, error } => {
                HOVER.with_borrow_mut(|h| h.variables(session, *reference, variables, error.as_ref()))
            }
            DebugUpdate::Sessions { .. } | DebugUpdate::CallStack { .. } => {
                let view = s.bridge.borrow().debug().clone();
                HOVER.with_borrow_mut(|h| {
                    let before = h.visible(Instant::now()).is_some();
                    h.follow(&view);
                    before != h.visible(Instant::now()).is_some()
                })
            }
            _ => false,
        };
        if changed {
            redraw(s);
        }
    }

    pub fn hide(s: &Shared) {
        let was = HOVER.with_borrow(|h| h.visible(Instant::now()).is_some());
        HOVER.with_borrow_mut(|h| h.hide());
        if was {
            redraw(s);
        }
    }

    /// A key the focused code editor offers: Alt switches to the language hover, Ctrl and Cmd
    /// keep the hover, any other key hides it. The key goes on either way.
    pub fn code_key(s: &Shared, event: &compose_rust::KeyEvent) {
        if super::is_alt_alone(event.key(), event.text(), event.alt_key()) {
            if let Some(target) = HOVER.with_borrow_mut(|h| h.alt_pressed()) {
                show_language(s, &target);
            }
            redraw(s);
            return;
        }
        if !super::keeps_hover(event.key(), event.text(), event.ctrl_key(), event.alt_key(), event.meta_key()) {
            hide(s);
        }
    }

    /// The expression of the card, tinted as Code-OSS's `hoverHighlight` decoration does.
    pub fn highlight(uri: &UriComponents, version: u32) -> Option<Decoration> {
        HOVER.with_borrow(|h| {
            let t = h.visible(Instant::now()).filter(|t| t.uri == *uri)?;
            Some(Decoration::range_background(version, CodeRange::of(t.line, t.start, t.line, t.end), HIGHLIGHT))
        })
    }

    /// Ask the editor where the card's expression is drawn, once per card.
    pub fn place(s: &Shared, uri: &UriComponents, handle: &dioxus_compose::CodeEditorHandle) {
        let at = HOVER.with_borrow(|h| {
            h.visible(Instant::now()).filter(|t| t.uri == *uri).map(|t| (uri.key(), t.line, t.start))
        });
        let Some(at) = at else {
            ASKED.with_borrow_mut(|a| *a = None);
            return;
        };
        if ASKED.with_borrow(|a| a.as_ref() == Some(&at)) {
            return;
        }
        ASKED.with_borrow_mut(|a| *a = Some(at.clone()));
        RECT.with(|r| r.set(None));
        let revision = s.revision;
        handle.query_position_rect(compose_rust::code::Position::new(at.1, at.2), move |rect| {
            RECT.with(|r| r.set(rect));
            let mut revision = revision;
            *revision.write() += 1;
        });
    }

    /// The card over the editor, while a value is shown and its place is known.
    pub fn card(s: &Shared) -> Element {
        let Some(model) = HOVER.with_borrow(|h| h.card(Instant::now())) else { return rsx! {} };
        let Some(rect) = RECT.with(|r| r.get()) else { return rsx! {} };
        let height = crate::ui::window_size::current_window_size().height_dp;
        let style = crate::ui::editor::hover_card_style(rect, height);
        let mac = crate::ui::keymap::Os::current() == crate::ui::keymap::Os::Mac;
        let (toggle_s, leave_s) = (s.clone(), s.clone());
        rsx! {
            DebugHoverCard { model, style, mac,
                toggle: move |row| {
                    let request = HOVER.with_borrow_mut(|h| h.toggle(&row));
                    if let Some(request) = request { send(&toggle_s, request); }
                    redraw(&toggle_s);
                },
                enter: move |_| HOVER.with_borrow_mut(|h| h.enter_card()),
                leave: move |_| {
                    HOVER.with_borrow_mut(|h| h.leave_card(Instant::now()));
                    let revision = leave_s.revision;
                    crate::services::services().rt.spawn(async move {
                        tokio::time::sleep(HIDING_DELAY + std::time::Duration::from_millis(20)).await;
                        let mut revision = revision;
                        *revision.write() += 1;
                    });
                },
            }
        }
    }
}

#[cfg(test)]
#[path = "editor_debug_hover/tests.rs"]
mod tests;
