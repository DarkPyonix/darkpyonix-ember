use super::*;
use compose_rust::protocol::{HostEvent, Mutation, PropertyValue, decode_batch, encode_event};
use compose_rust::schema::{EventPayload, PropertyKind, WidgetKind};
use dioxus_compose::html::HtmlDom;
use std::cell::RefCell;

thread_local! {
    static REQUESTS: RefCell<Vec<DebugRequest>> = const { RefCell::new(Vec::new()) };
}

fn record(request: DebugRequest) {
    REQUESTS.with_borrow_mut(|requests| requests.push(request));
}

fn requests() -> Vec<DebugRequest> {
    REQUESTS.with_borrow_mut(std::mem::take)
}

fn uri() -> UriComponents {
    UriComponents::remote("sample", "/work/main.py")
}

fn breakpoint() -> SourceBreakpoint {
    SourceBreakpoint {
        id: "bp".into(),
        uri: uri(),
        line: 2,
        column: None,
        condition: Some("count > 0".into()),
        hit_condition: Some("3".into()),
        log_message: None,
        enabled: true,
        verified: false,
        adapter_line: None,
        message: None,
        adapter_id: None,
        raw: Default::default(),
    }
}

fn gutter_editor() -> Element {
    use dioxus_compose::prelude::*;
    let uri = uri();
    let text = "first\nsecond\n";
    let gutter = breakpoint_gutter(
        &uri,
        text,
        0,
        &[breakpoint()],
        GutterDecorations::new([GutterDecoration::bar(0, 1, 0)]),
    );
    rsx! {
        CodeEditor {
            text,
            gutter_decorations: gutter,
            on_decoration_click: move |id| {
                if let Some(request) = toggle_from_gutter(&uri, id) {
                    record(request);
                }
            }
        }
    }
}

#[test]
fn fr_l31_42_code_editor_glyph_margin_toggles_empty_and_marked_lines() {
    let mut host = dioxus_compose::Host::new(gutter_editor);
    let batch = decode_batch(host.rebuild().unwrap()).unwrap();
    let editor = batch
        .iter()
        .find_map(|mutation| match mutation {
            Mutation::Create {
                node_id,
                widget: WidgetKind::CodeEditor,
            } => Some(*node_id),
            _ => None,
        })
        .unwrap();
    let marks = batch
        .iter()
        .find_map(|mutation| match mutation {
            Mutation::SetProp {
                node_id,
                property: PropertyKind::GutterDecorations,
                value: PropertyValue::Bytes(bytes),
            } if *node_id == editor => {
                Some(GutterDecorations::from_bytes(bytes.to_vec()).decorations())
            }
            _ => None,
        })
        .unwrap();
    let click_handler = batch
        .iter()
        .find_map(|mutation| match mutation {
            Mutation::SetProp {
                node_id,
                property: PropertyKind::OnDecorationClick,
                value: PropertyValue::Integer(id),
            } if *node_id == editor => Some(*id as u64),
            _ => None,
        })
        .unwrap();
    assert_eq!(marks.len(), 4);
    assert_eq!(marks[0], GutterDecoration::bar(0, 1, 0));
    assert_eq!(marks[1].glyph, ' ' as u32);
    assert_eq!(marks[2].glyph, '◆' as u32);
    assert_eq!(marks[3].line, 2);

    for id in [marks[1].id, marks[2].id, 41] {
        let mut bytes = Vec::new();
        encode_event(
            &HostEvent::new(
                editor,
                click_handler,
                EventPayload::DecorationActivated { decoration: id },
            ),
            &mut bytes,
        )
        .unwrap();
        host.dispatch_event(&bytes).unwrap();
    }
    assert_eq!(
        requests(),
        [
            DebugRequest::ToggleBreakpoint {
                uri: uri(),
                line: 1
            },
            DebugRequest::ToggleBreakpoint {
                uri: uri(),
                line: 2
            },
        ]
    );
}

fn click(dom: &mut HtmlDom, id: &str) {
    dom.layout(1000.0, 600.0, 1.0);
    let node = dom.element_by_id(id).unwrap();
    let rect = dom.display_list().unwrap().get(node).unwrap().rect;
    dom.click(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    dom.render();
}

fn toolbar() -> Element {
    let session = SessionInfo {
        id: "session".into(),
        name: "Python".into(),
        debug_type: "debugpy".into(),
        request: "launch".into(),
        parent: None,
        state: SessionState::Stopped {
            reason: "breakpoint".into(),
            thread_id: Some(7),
            description: None,
            text: None,
        },
        thread_id: Some(7),
        frame_id: None,
    };
    rsx! { DebugToolbar { session, send: record } }
}

#[test]
fn fr_l31_42_debug_toolbar_dispatches_all_six_actions() {
    let mut dom = HtmlDom::new(toolbar);
    for id in [
        "debug-continue",
        "debug-over",
        "debug-into",
        "debug-out",
        "debug-restart",
        "debug-stop",
    ] {
        click(&mut dom, id);
    }
    let session = "session".to_string();
    assert_eq!(
        requests(),
        [
            DebugRequest::Continue {
                session: session.clone(),
                thread_id: 7
            },
            DebugRequest::StepOver {
                session: session.clone(),
                thread_id: 7
            },
            DebugRequest::StepInto {
                session: session.clone(),
                thread_id: 7
            },
            DebugRequest::StepOut {
                session: session.clone(),
                thread_id: 7
            },
            DebugRequest::Restart {
                session: session.clone()
            },
            DebugRequest::Stop {
                session: Some(session)
            },
        ]
    );
}

fn run_picker() -> Element {
    let mut state = use_hook(|| {
        let mut state = DebugControls::default();
        state.apply(&DebugUpdate::Configurations {
            configurations: ["Python", "Tests"]
                .into_iter()
                .map(|name| ConfigInfo {
                    name: name.to_string(),
                    debug_type: "debugpy".into(),
                    request: "launch".into(),
                    compound: false,
                })
                .collect(),
            error: None,
        });
        Signal::new(state)
    });
    let configuration = state.read().run_command();
    let names = state.read().configuration_names();
    rsx! {
        RunPicker {
            configurations: names,
            selected: state.read().selected.clone(),
            error: None,
            enabled: configuration.is_some(),
            select: move |name| state.write().selected = Some(name),
            run: move |_| { if let Some(request) = &configuration { record(request.clone()); } },
            refresh: move |_| {},
            tasks: move |_| {},
            run_task: move |_| {},
        }
    }
}

#[test]
fn fr_l31_42_run_button_starts_the_selected_configuration() {
    let mut dom = HtmlDom::new(run_picker);
    click(&mut dom, "debug-configurations");
    click(&mut dom, "debug-config-1");
    click(&mut dom, "debug-run");
    assert_eq!(
        requests(),
        [DebugRequest::Start {
            selector: ConfigSelector::Name("Tests".into())
        }]
    );
}

fn breakpoint_editor() -> Element {
    rsx! { BreakpointEditor { breakpoint: breakpoint(), send: record } }
}

#[test]
fn fr_l31_42_breakpoint_editor_saves_conditions_and_log_messages() {
    let mut dom = HtmlDom::new(breakpoint_editor);
    click(&mut dom, "debug-edit-breakpoint");
    dom.layout(1000.0, 600.0, 1.0);
    let condition = dom.element_by_id("debug-breakpoint-condition").unwrap();
    let log = dom.element_by_id("debug-breakpoint-log").unwrap();
    dom.input(condition, "count == 2");
    dom.input(log, "count = {count}");
    click(&mut dom, "debug-save-breakpoint");
    assert_eq!(
        requests(),
        [DebugRequest::EditBreakpoint {
            id: "bp".into(),
            condition: Some("count == 2".into()),
            hit_condition: Some("3".into()),
            log_message: Some("count = {count}".into()),
            enabled: true,
        }]
    );
}
