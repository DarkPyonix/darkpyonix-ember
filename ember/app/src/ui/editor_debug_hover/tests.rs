use super::*;
use dioxus_compose::html::HtmlDom;
use ember_editor::StoppedLocation;
use ember_editor::debugger::{SessionInfo, SessionState};
use std::cell::RefCell;

fn uri() -> UriComponents {
    UriComponents::remote("sample", "/work/main.py")
}

fn session(state: SessionState) -> SessionInfo {
    SessionInfo {
        id: "s".into(),
        name: "Python".into(),
        debug_type: "debugpy".into(),
        request: "launch".into(),
        parent: None,
        state,
        thread_id: Some(1),
        frame_id: Some(10),
        can_set_variables: false,
        no_debug: false,
    }
}

fn paused() -> SessionState {
    SessionState::Stopped { reason: "breakpoint".into(), thread_id: Some(1), description: None, text: None }
}

/// A window stopped at line 2 of `main.py`, frame 10.
fn stopped() -> DebugView {
    DebugView {
        sessions: vec![session(paused())],
        active: Some("s".into()),
        stopped_at: Some(StoppedLocation { session: "s".into(), thread_id: 1, frame_id: 10, uri: uri(), line: 2, column: 1 }),
        ..Default::default()
    }
}

const LINE: &str = "total = order.items";

fn value(result: &str, reference: i64) -> Result<EvalResult, String> {
    Ok(EvalResult { result: result.into(), type_name: Some("list".into()), variables_reference: reference })
}

fn var(name: &str, value: &str, reference: i64) -> VariableInfo {
    VariableInfo { name: name.into(), value: value.into(), type_name: None, variables_reference: reference, evaluate_name: None, read_only: false }
}

fn hover_id(step: &RestStep) -> u64 {
    match step {
        RestStep::Debug(DebugRequest::Hover { id, .. }) => *id,
        other => panic!("not a debug hover: {other:?}"),
    }
}

// SPEC FR-L31.42, Code-OSS `showHover`: only while stopped and only in the focused frame's file
// does a rest ask for a value; elsewhere, and where there is no expression, the language hover
// shows.
#[test]
fn fr_l31_42_debug_hover_takes_a_rest_only_while_stopped_in_the_focused_file() {
    let mut h = DebugHover::default();
    let at = WidgetPos::new(1, 16);
    let step = h.rest(&stopped(), &uri(), LINE, at, 7);
    assert_eq!(
        step,
        RestStep::Debug(DebugRequest::Hover {
            id: hover_id(&step),
            session: "s".into(),
            frame_id: 10,
            uri: uri(),
            line: 2,
            column: 9,
            expression: "order.items".into(),
        })
    );
    // The same expression again is not asked again (`NOT_CHANGED`).
    assert_eq!(h.rest(&stopped(), &uri(), LINE, WidgetPos::new(1, 15), 7), RestStep::Keep);

    let running = DebugView { sessions: vec![session(SessionState::Running)], active: Some("s".into()), ..Default::default() };
    assert_eq!(DebugHover::default().rest(&running, &uri(), LINE, at, 0), RestStep::Language);
    let other = UriComponents::remote("sample", "/work/util.py");
    assert_eq!(DebugHover::default().rest(&stopped(), &other, LINE, at, 0), RestStep::Language);
    // Over the `=` there is no expression.
    assert_eq!(DebugHover::default().rest(&stopped(), &uri(), LINE, WidgetPos::new(1, 6), 0), RestStep::Language);
}

// SPEC FR-L31.42, Code-OSS `DebugHoverWidget.doShow`: a value without children is drawn alone; a
// structured one is a title and a tree whose root children are asked for at once and whose rows
// load their own children as they expand.
#[test]
fn fr_l31_42_debug_hover_shows_a_structured_value_as_an_expandable_tree() {
    let mut h = DebugHover::default();
    let now = Instant::now();
    let id = hover_id(&h.rest(&stopped(), &uri(), LINE, WidgetPos::new(1, 16), 0));
    assert_eq!(h.evaluated(id + 1, &value("x", 0)), AnswerStep::Ignore, "another hover's answer");
    assert_eq!(
        h.evaluated(id, &value("[2 items]", 30)),
        AnswerStep::Show(Some(DebugRequest::Variables { session: "s".into(), reference: 30 }))
    );
    let card = h.card(now).unwrap();
    assert!(card.complex && card.rows.is_empty());
    assert_eq!(card.value, "[2 items]");
    assert!(h.variables("s", 30, &[var("0", "Item", 31), var("1", "None", 0)], None));
    assert!(!h.variables("s", 99, &[var("z", "1", 0)], None), "not this tree's");
    let rows = h.card(now).unwrap().rows;
    assert_eq!(rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["0", "1"]);
    assert_eq!(h.toggle(&rows[1]), None, "a leaf does not expand");
    assert_eq!(h.toggle(&rows[0]), Some(DebugRequest::Variables { session: "s".into(), reference: 31 }));
    assert!(h.variables("s", 31, &[var("name", "'pen'", 0)], None));
    let rows = h.card(now).unwrap().rows;
    assert_eq!(rows.iter().map(|r| (r.name.as_str(), r.depth)).collect::<Vec<_>>(), [("0", 0), ("name", 1), ("1", 0)]);
    assert_eq!(h.toggle(&rows[0]), None, "collapsing asks nothing");
    assert_eq!(h.card(now).unwrap().rows.len(), 2);

    // A plain value is drawn alone.
    let mut h = DebugHover::default();
    let id = hover_id(&h.rest(&stopped(), &uri(), LINE, WidgetPos::new(1, 2), 0));
    assert_eq!(h.evaluated(id, &value("12", 0)), AnswerStep::Show(None));
    let card = h.card(now).unwrap();
    assert!(!card.complex);
    assert_eq!((card.expression.as_str(), card.value.as_str()), ("total", "12"));
}

// SPEC FR-L31.42, Code-OSS `showHover`: an expression without a value falls back to the language
// hover at the pointer.
#[test]
fn fr_l31_42_debug_hover_without_a_value_falls_back_to_the_language_hover() {
    let mut h = DebugHover::default();
    let at = WidgetPos::new(1, 16);
    let id = hover_id(&h.rest(&stopped(), &uri(), LINE, at, 7));
    match h.evaluated(id, &Err("not available".into())) {
        AnswerStep::Fallback(target) => assert_eq!((target.pointer, target.decoration), (at, 7)),
        other => panic!("{other:?}"),
    }
    assert!(h.card(Instant::now()).is_none());
}

// SPEC FR-L31.42, Code-OSS `altListener` (#84561): Alt gives a shown debug hover's place to the
// language hover, which keeps it while the pointer stays; resting elsewhere brings the debug
// hover back.
#[test]
fn fr_l31_42_alt_switches_the_debug_hover_to_the_language_hover() {
    let mut h = DebugHover::default();
    let at = WidgetPos::new(1, 16);
    let id = hover_id(&h.rest(&stopped(), &uri(), LINE, at, 0));
    h.evaluated(id, &value("[2 items]", 0));
    let target = h.alt_pressed().expect("the language hover shows where the card was");
    assert_eq!(target.pointer, at);
    assert!(h.card(Instant::now()).is_none());
    assert_eq!(h.rest(&stopped(), &uri(), LINE, at, 0), RestStep::Language);
    assert!(matches!(h.rest(&stopped(), &uri(), LINE, WidgetPos::new(1, 2), 0), RestStep::Debug(_)));
    assert_eq!(h.alt_pressed(), None, "nothing was shown yet, so nothing to switch to");

    assert!(is_alt_alone(compose_rust::Key::Other, "", true));
    assert!(!is_alt_alone(compose_rust::Key::Other, "a", true));
    assert!(keeps_hover(compose_rust::Key::Other, "", true, false, false), "Ctrl alone");
    assert!(keeps_hover(compose_rust::Key::Other, "", false, false, true), "Cmd alone");
    assert!(!keeps_hover(compose_rust::Key::Other, "a", false, false, false));
    assert!(!keeps_hover(compose_rust::Key::Escape, "", false, false, false));
}

// SPEC FR-L31.42, Code-OSS `editor.hover.sticky` and `hidingDelay`: the card stays while the
// pointer is on it and goes 300 ms after the pointer left for somewhere else; a step of the
// session takes it away.
#[test]
fn fr_l31_42_debug_hover_card_is_sticky_and_goes_after_the_hiding_delay() {
    let mut h = DebugHover::default();
    let t0 = Instant::now();
    let id = hover_id(&h.rest(&stopped(), &uri(), LINE, WidgetPos::new(1, 16), 0));
    h.evaluated(id, &value("[2 items]", 0));
    h.leave(t0);
    assert!(h.visible(t0 + Duration::from_millis(100)).is_some(), "time to reach the card");
    h.enter_card();
    assert!(h.visible(t0 + Duration::from_secs(5)).is_some(), "on the card");
    h.leave_card(t0);
    assert!(h.visible(t0 + HIDING_DELAY).is_none());

    let mut h = DebugHover::default();
    let id = hover_id(&h.rest(&stopped(), &uri(), LINE, WidgetPos::new(1, 16), 0));
    h.evaluated(id, &value("[2 items]", 0));
    let running = DebugView { sessions: vec![session(SessionState::Running)], active: Some("s".into()), ..Default::default() };
    h.follow(&stopped());
    assert!(h.visible(t0).is_some(), "still the focused frame");
    h.follow(&running);
    assert!(h.visible(t0).is_none());
}

thread_local! {
    static TOGGLED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn tree_card() -> Element {
    let model = CardModel {
        expression: "order.items".into(),
        value: "[2 items]".into(),
        type_name: Some("list".into()),
        complex: true,
        rows: vec![
            HoverRow { path: "\u{1f}0".into(), depth: 0, name: "0".into(), value: "Item".into(), type_name: None, reference: 31, expanded: false, error: None },
            HoverRow { path: "\u{1f}1".into(), depth: 0, name: "1".into(), value: "None".into(), type_name: None, reference: 0, expanded: false, error: None },
        ],
    };
    rsx! {
        DebugHoverCard { model, style: "position:fixed; left:10px; top:10px;".to_string(), mac: true,
            toggle: move |row: HoverRow| TOGGLED.with_borrow_mut(|t| t.push(row.name)),
            enter: move |_| {}, leave: move |_| {},
        }
    }
}

fn click(dom: &mut HtmlDom, id: &str) {
    dom.layout(1000.0, 600.0, 1.0);
    let node = dom.element_by_id(id).unwrap();
    let rect = dom.display_list().unwrap().get(node).unwrap().rect;
    dom.click(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    dom.render();
}

// SPEC FR-L31.42: the card draws a structured value's title, its rows and the Alt tip, and a
// click on a row with children expands it.
#[test]
fn fr_l31_42_debug_hover_card_expands_a_row_on_click() {
    let mut dom = HtmlDom::new(tree_card);
    dom.layout(1000.0, 600.0, 1.0);
    assert!(dom.element_by_id("debug-hover-title").is_some());
    assert!(dom.element_by_id("debug-hover-tip").is_some());
    assert!(dom.element_by_id("debug-hover-value").is_none());
    click(&mut dom, "debug-hover-row-0");
    click(&mut dom, "debug-hover-row-1");
    assert_eq!(TOGGLED.with_borrow_mut(std::mem::take), ["0"], "only the row with children");
    assert_eq!(tip(true), "Option 키를 누르면 편집기 언어 호버로 바뀝니다");
    assert_eq!(tip(false), "Alt 키를 누르면 편집기 언어 호버로 바뀝니다");
}
