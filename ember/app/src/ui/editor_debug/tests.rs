use super::*;
use compose_rust::protocol::{HostEvent, Mutation, PropertyValue, decode_batch, encode_event};
use compose_rust::schema::{EventPayload, PropertyKind, WidgetKind};
use dioxus_compose::html::HtmlDom;
use std::cell::RefCell;

thread_local! {
    static REQUESTS: RefCell<Vec<DebugRequest>> = const { RefCell::new(Vec::new()) };
    static TASK_RUNS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
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
    let view = DebugView {
        breakpoints: vec![breakpoint()],
        ..Default::default()
    };
    let gutter = breakpoint_gutter(
        &uri,
        text,
        0,
        &view,
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

fn stopped_session() -> SessionInfo {
    SessionInfo {
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
    }
}

fn running_session() -> SessionInfo {
    SessionInfo {
        state: SessionState::Running,
        thread_id: None,
        ..stopped_session()
    }
}

fn toolbar() -> Element {
    rsx! { DebugToolbar { session: stopped_session(), pause_thread: Some(7), send: record } }
}

fn running_toolbar() -> Element {
    rsx! { DebugToolbar { session: running_session(), pause_thread: Some(5), send: record } }
}

// Code-OSS's toolbar puts 일시 중지 where 계속 is while the session runs; the steps wait for a stop.
#[test]
fn fr_l31_42_debug_toolbar_pauses_a_running_session() {
    let mut dom = HtmlDom::new(running_toolbar);
    dom.layout(1000.0, 600.0, 1.0);
    assert!(dom.element_by_id("debug-continue").is_none());
    for id in ["debug-pause", "debug-over", "debug-into", "debug-out", "debug-stop"] {
        click(&mut dom, id);
    }
    assert_eq!(
        requests(),
        [
            DebugRequest::Pause {
                session: "session".into(),
                thread_id: 5
            },
            DebugRequest::Stop {
                session: Some("session".into())
            },
        ]
    );
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
        }, &DebugView::default());
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
            run_task: move |_| TASK_RUNS.set(TASK_RUNS.get() + 1),
        }
    }
}

// The task runner's 실행 stays beside 디버깅 시작: one runs the default task, the other starts the
// chosen launch configuration, and neither does the other's job.
#[test]
fn fr_l31_42_the_task_run_and_the_debug_start_sit_side_by_side() {
    let mut dom = HtmlDom::new(run_picker);
    TASK_RUNS.set(0);
    click(&mut dom, "task-run");
    assert_eq!(TASK_RUNS.get(), 1);
    assert_eq!(requests(), []);
    click(&mut dom, "debug-run");
    assert_eq!(TASK_RUNS.get(), 1);
    assert_eq!(
        requests(),
        [DebugRequest::Start {
            selector: ConfigSelector::Name("Python".into())
        }]
    );
    // In the top bar's order: the picker, 실행, then 디버깅 시작.
    let order: Vec<_> = ["debug-configurations", "task-run", "debug-run"]
        .into_iter()
        .map(|id| {
            let node = dom.element_by_id(id).unwrap();
            dom.display_list().unwrap().get(node).unwrap().rect.x
        })
        .collect();
    assert!(order[0] < order[1] && order[1] < order[2], "{order:?}");
}

fn configured(view: DebugView) -> DebugControls {
    let mut controls = DebugControls::default();
    controls.apply(
        &DebugUpdate::Configurations {
            configurations: vec![ConfigInfo {
                name: "Python".into(),
                debug_type: "debugpy".into(),
                request: "launch".into(),
                compound: false,
            }],
            error: None,
        },
        &view,
    );
    controls
}

fn showing(session: SessionInfo) -> DebugView {
    DebugView {
        active: Some(session.id.clone()),
        sessions: vec![session],
        ..Default::default()
    }
}

// F5 and Shift+F5 as Code-OSS binds them: start when nothing runs, continue when stopped, nothing
// while running; stop the shown session, else leave the key to the task runner.
#[test]
fn fr_l31_42_f5_starts_or_continues_and_shift_f5_stops_the_session() {
    let idle = configured(DebugView::default());
    assert_eq!(
        idle.f5_command(),
        Some(DebugRequest::Start {
            selector: ConfigSelector::Name("Python".into())
        })
    );
    assert_eq!(idle.stop_command(), None);
    assert_eq!(DebugControls::default().f5_command(), None, "no configuration to start");

    let stopped = configured(showing(stopped_session()));
    assert_eq!(
        stopped.f5_command(),
        Some(DebugRequest::Continue {
            session: "session".into(),
            thread_id: 7
        })
    );
    assert_eq!(
        stopped.stop_command(),
        Some(DebugRequest::Stop {
            session: Some("session".into())
        })
    );

    let running = configured(showing(running_session()));
    assert_eq!(running.f5_command(), None);
    assert!(running.stop_command().is_some());

    let ended = configured(showing(SessionInfo {
        state: SessionState::Terminated,
        ..stopped_session()
    }));
    assert!(matches!(ended.f5_command(), Some(DebugRequest::Start { .. })));
    assert_eq!(ended.stop_command(), None);
}

// The bridge's stopped location is the gutter's arrow, and a breakpoint the adapter has not
// verified is hollow while a session runs.
#[test]
fn fr_l31_42_the_gutter_marks_the_stopped_line_and_unverified_breakpoints() {
    let text = "a\nb\nc\n";
    let glyphs = |view: &DebugView| -> Vec<char> {
        breakpoint_gutter(&uri(), text, 0, view, GutterDecorations::default())
            .decorations()
            .into_iter()
            .map(|d| char::from_u32(d.glyph).unwrap())
            .collect()
    };
    let plain = SourceBreakpoint {
        condition: None,
        hit_condition: None,
        ..breakpoint()
    };
    let mut view = DebugView {
        breakpoints: vec![plain.clone()],
        ..Default::default()
    };
    assert_eq!(glyphs(&view)[1], '●', "no session: the user's mark");
    view = DebugView {
        breakpoints: vec![plain.clone()],
        ..showing(running_session())
    };
    assert_eq!(glyphs(&view)[1], '○', "a session the adapter did not verify it for");
    view.breakpoints[0].verified = true;
    assert_eq!(glyphs(&view)[1], '●');
    view.stopped_at = Some(ember_editor::StoppedLocation {
        session: "session".into(),
        thread_id: 7,
        frame_id: 1,
        uri: uri(),
        line: 3,
        column: 1,
    });
    let marks = glyphs(&view);
    assert_eq!((marks[1], marks[2]), ('●', '▶'));
    // The arrow line is still a toggle target.
    let decorations = breakpoint_gutter(&uri(), text, 0, &view, GutterDecorations::default()).decorations();
    assert_eq!(
        toggle_from_gutter(&uri(), decorations[2].id),
        Some(DebugRequest::ToggleBreakpoint { uri: uri(), line: 3 })
    );
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
