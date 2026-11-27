use super::*;
use dioxus_compose::html::HtmlDom;
use dioxus_compose::html::prelude::dioxus_elements::{Code, Modifiers};
use std::cell::RefCell;

thread_local! {
    static ACTIONS: RefCell<Vec<ViewAction>> = const { RefCell::new(Vec::new()) };
}

fn record(action: ViewAction) {
    ACTIONS.with_borrow_mut(|a| a.push(action));
}

fn actions() -> Vec<ViewAction> {
    ACTIONS.with_borrow_mut(std::mem::take)
}

fn requests() -> Vec<DebugRequest> {
    actions()
        .into_iter()
        .filter_map(|a| match a {
            ViewAction::Debug(r) => Some(r),
            _ => None,
        })
        .collect()
}

fn uri(path: &str) -> UriComponents {
    UriComponents::remote("sample", path)
}

fn session(id: &str, state: SessionState, frame: Option<i64>) -> SessionInfo {
    SessionInfo {
        id: id.into(),
        name: "Python: Current File".into(),
        debug_type: "debugpy".into(),
        request: "launch".into(),
        parent: None,
        state,
        thread_id: Some(1),
        frame_id: frame,
        can_set_variables: true,
        no_debug: false,
    }
}

fn paused() -> SessionState {
    SessionState::Stopped {
        reason: "breakpoint".into(),
        thread_id: Some(1),
        description: None,
        text: None,
    }
}

fn sessions(list: Vec<SessionInfo>) -> DebugUpdate {
    let active = list.first().map(|s| s.id.clone());
    DebugUpdate::Sessions {
        sessions: list,
        active,
    }
}

fn var(name: &str, value: &str, reference: i64) -> VariableInfo {
    VariableInfo {
        name: name.into(),
        value: value.into(),
        type_name: Some("int".into()),
        variables_reference: reference,
        evaluate_name: Some(name.into()),
        read_only: false,
    }
}

/// A view stopped at frame 10 of thread 1 of session `s`, with its stack and scopes loaded.
fn stopped_view() -> (DebugView, Effects) {
    let mut v = DebugView::default();
    v.apply(&sessions(vec![session("s", SessionState::Running, None)]));
    v.apply(&sessions(vec![session("s", paused(), Some(10))]));
    v.apply(&DebugUpdate::Threads {
        session: "s".into(),
        threads: vec![ThreadInfo {
            id: 1,
            name: "MainThread".into(),
            stopped: true,
        }],
    });
    v.apply(&DebugUpdate::CallStack {
        session: "s".into(),
        thread_id: 1,
        frames: vec![
            FrameInfo {
                id: 10,
                name: "<module>".into(),
                uri: Some(uri("/w/main.py")),
                source_name: Some("main.py".into()),
                line: 2,
                column: 1,
                hint: None,
            },
            FrameInfo {
                id: 11,
                name: "run".into(),
                uri: Some(uri("/w/lib.py")),
                source_name: Some("lib.py".into()),
                line: 7,
                column: 1,
                hint: None,
            },
        ],
        total_frames: Some(2),
    });
    let fx = v.apply(&DebugUpdate::Scopes {
        session: "s".into(),
        frame_id: 10,
        scopes: vec![
            ScopeInfo {
                name: "Locals".into(),
                variables_reference: 100,
                expensive: false,
            },
            ScopeInfo {
                name: "Globals".into(),
                variables_reference: 200,
                expensive: true,
            },
        ],
    });
    (v, fx)
}

#[test]
fn fr_l31_42_the_first_cheap_scope_opens_and_its_variables_are_asked_for() {
    let (mut v, fx) = stopped_view();
    assert_eq!(
        fx.requests,
        [DebugRequest::Variables {
            session: "s".into(),
            reference: 100
        }]
    );
    let fx = v.apply(&DebugUpdate::Variables {
        session: "s".into(),
        reference: 100,
        variables: vec![var("x", "41", 0), var("items", "[1, 2]", 300)],
        error: None,
    });
    assert!(fx.requests.is_empty(), "a closed child is not loaded");
    let rows = v.variable_rows();
    let names: Vec<(&str, usize)> = rows.iter().map(|r| (r.name.as_str(), r.depth)).collect();
    assert_eq!(
        names,
        [("Locals", 0), ("x", 1), ("items", 1), ("Globals", 0)]
    );
    assert!(rows[0].expanded && !rows[3].expanded);

    // Opening `items` asks for its children; they show below it.
    let items = rows[2].clone();
    assert_eq!(
        v.toggle(&items),
        Some(DebugRequest::Variables {
            session: "s".into(),
            reference: 300
        })
    );
    v.apply(&DebugUpdate::Variables {
        session: "s".into(),
        reference: 300,
        variables: vec![var("0", "1", 0)],
        error: None,
    });
    let rows = v.variable_rows();
    assert_eq!(rows[3].name, "0");
    assert_eq!(rows[3].depth, 2);

    // The next stop keeps what the user opened: Locals and items load again by themselves.
    v.apply(&sessions(vec![session("s", SessionState::Running, None)]));
    assert!(
        v.variable_rows().is_empty(),
        "nothing is shown while the thread runs"
    );
    v.apply(&sessions(vec![session("s", paused(), Some(20))]));
    let fx = v.apply(&DebugUpdate::Scopes {
        session: "s".into(),
        frame_id: 20,
        scopes: vec![ScopeInfo {
            name: "Locals".into(),
            variables_reference: 101,
            expensive: false,
        }],
    });
    assert_eq!(
        fx.requests,
        [DebugRequest::Variables {
            session: "s".into(),
            reference: 101
        }]
    );
    let fx = v.apply(&DebugUpdate::Variables {
        session: "s".into(),
        reference: 101,
        variables: vec![var("items", "[3]", 301)],
        error: None,
    });
    assert_eq!(
        fx.requests,
        [DebugRequest::Variables {
            session: "s".into(),
            reference: 301
        }]
    );
}

#[test]
fn fr_l31_42_watch_expressions_are_evaluated_in_each_stopped_frame() {
    let mut v = DebugView::default();
    assert_eq!(v.add_watch("  "), None);
    assert_eq!(v.add_watch("x + 1"), None, "no frame to evaluate in yet");
    assert_eq!(v.watches()[0].result, None);
    let fx = v.apply(&sessions(vec![session("s", paused(), Some(10))]));
    let [
        DebugRequest::Evaluate {
            id,
            session: sid,
            frame_id,
            expression,
            context,
        },
    ] = fx.requests.as_slice()
    else {
        panic!("{:?}", fx.requests)
    };
    assert_eq!(
        (sid.as_str(), *frame_id, expression.as_str(), *context),
        ("s", Some(10), "x + 1", EvalContext::Watch)
    );
    let result = EvalResult {
        result: "42".into(),
        type_name: Some("int".into()),
        variables_reference: 0,
    };
    v.apply(&DebugUpdate::Evaluated {
        id: *id,
        session: "s".into(),
        result: Ok(result.clone()),
    });
    assert_eq!(v.watches()[0].result, Some(Ok(result)));
    // The same frame again asks nothing; a run clears the value.
    assert!(
        v.apply(&sessions(vec![session("s", paused(), Some(10))]))
            .requests
            .is_empty()
    );
    v.apply(&sessions(vec![session("s", SessionState::Running, None)]));
    assert_eq!(v.watches()[0].result, None);
    // An evaluation this window did not ask for is not taken.
    v.apply(&DebugUpdate::Evaluated {
        id: 7,
        session: "s".into(),
        result: Err("x".into()),
    });
    assert_eq!(v.watches()[0].result, None);
    let wid = v.watches()[0].id;
    v.remove_watch(wid);
    assert!(v.watches().is_empty());
}

#[test]
fn fr_l31_42_console_evaluates_in_the_active_session_and_joins_output() {
    let mut v = DebugView::default();
    assert_eq!(v.submit("x"), None);
    assert!(
        matches!(&v.console()[1].kind, LineKind::Result(Err(_))),
        "without a session the console says so"
    );
    let (mut v, _) = stopped_view();
    let Some(DebugRequest::Evaluate {
        id,
        frame_id,
        context,
        ..
    }) = v.submit("x * 2")
    else {
        panic!()
    };
    assert_eq!((frame_id, context), (Some(10), EvalContext::Repl));
    v.apply(&DebugUpdate::Evaluated {
        id,
        session: "s".into(),
        result: Ok(EvalResult {
            result: "82".into(),
            type_name: None,
            variables_reference: 0,
        }),
    });
    assert_eq!(v.console()[0].kind, LineKind::Input);
    assert!(matches!(&v.console()[1].kind, LineKind::Result(Ok(r)) if r.result == "82"));

    let out = |text: &str, category: &str| DebugUpdate::Console {
        session: Some("s".into()),
        entry: ember_editor::debugger::ConsoleEntry {
            category: category.into(),
            text: text.into(),
            uri: None,
            line: None,
        },
    };
    v.clear_console();
    v.apply(&out("4", "stdout"));
    v.apply(&out("2\n", "stdout"));
    v.apply(&out("next\n", "stdout"));
    v.apply(&out("oops\n", "stderr"));
    v.apply(&out("{}", "telemetry"));
    let texts: Vec<&str> = v.console().iter().map(|l| l.text.as_str()).collect();
    assert_eq!(texts, ["42\n", "next\n", "oops\n"]);
    for i in 0..CONSOLE_LIMIT + 5 {
        v.apply(&out(&format!("{i}\n"), "stdout"));
    }
    assert_eq!(v.console().len(), CONSOLE_LIMIT);
}

#[test]
fn fr_l31_42_console_history_walks_back_and_forward() {
    let mut v = DebugView::default();
    v.submit("a");
    v.submit("b");
    v.submit("b");
    assert_eq!(v.history_back().as_deref(), Some("b"));
    assert_eq!(v.history_back().as_deref(), Some("a"));
    assert_eq!(v.history_back(), None);
    assert_eq!(v.history_forward().as_deref(), Some("b"));
    assert_eq!(v.history_forward().as_deref(), Some(""));
    assert_eq!(v.history_forward(), None);
}

#[test]
fn fr_l31_42_console_opens_on_the_first_session_and_the_view_on_a_stop() {
    let mut v = DebugView::default();
    let fx = v.apply(&sessions(vec![session(
        "s",
        SessionState::Initializing,
        None,
    )]));
    assert!(fx.show_console && !fx.show_view);
    let fx = v.apply(&sessions(vec![session("s", paused(), Some(10))]));
    assert!(fx.show_view && !fx.show_console);
    assert!(
        !v.apply(&sessions(vec![session("s", paused(), Some(11))]))
            .show_view,
        "a frame change is not a new stop"
    );
    v.apply(&sessions(vec![session(
        "s",
        SessionState::Terminated,
        None,
    )]));
    assert!(v.stack_rows().is_empty());
    let fx = v.apply(&sessions(vec![session("t", SessionState::Running, None)]));
    assert!(
        !fx.show_console,
        "only the first session of the window opens the console"
    );
}

#[test]
fn fr_l31_42_creating_launch_json_takes_the_provider_answer_for_its_type() {
    let mut v = DebugView::default();
    v.apply(&DebugUpdate::Configurations {
        configurations: vec![],
        error: None,
    });
    assert!(v.listed() && !v.has_launch_json());
    assert_eq!(
        v.begin_create("debugpy"),
        DebugRequest::ProvideConfigurations {
            debug_type: "debugpy".into()
        }
    );
    let other = v.apply(&DebugUpdate::ProvidedConfigurations {
        debug_type: "node".into(),
        configurations: vec![],
    });
    assert_eq!(other.create_launch, None);
    let fx = v.apply(&DebugUpdate::ProvidedConfigurations {
        debug_type: "debugpy".into(),
        configurations: vec![serde_json::json!({"name": "P"})],
    });
    assert_eq!(
        fx.create_launch,
        Some(("debugpy".into(), vec![serde_json::json!({"name": "P"})]))
    );
    assert_eq!(v.creating(), None);
}

// ---- widget tests ---------------------------------------------------------------------------

fn click(dom: &mut HtmlDom, id: &str) {
    dom.layout(1000.0, 800.0, 1.0);
    let node = dom.element_by_id(id).unwrap_or_else(|| panic!("no #{id}"));
    dom.click_node(node);
    dom.render();
}

fn breakpoint(id: &str, line: u32, enabled: bool) -> SourceBreakpoint {
    SourceBreakpoint {
        id: id.into(),
        uri: uri("/w/src/main.py"),
        line,
        column: None,
        condition: None,
        hit_condition: None,
        log_message: None,
        enabled,
        verified: true,
        adapter_line: None,
        message: None,
        adapter_id: None,
        raw: Default::default(),
    }
}

fn window() -> Element {
    let view = use_hook(|| {
        let (mut v, _) = stopped_view();
        v.apply(&DebugUpdate::Variables {
            session: "s".into(),
            reference: 100,
            variables: vec![var("x", "41", 0)],
            error: None,
        });
        v.apply(&DebugUpdate::Breakpoints {
            source: vec![breakpoint("b1", 2, true), breakpoint("b2", 5, false)],
            functions: vec![],
            exception_filters: vec![ExceptionFilter {
                filter: "raised".into(),
                label: "Raised Exceptions".into(),
                enabled: false,
                supports_condition: false,
                condition: None,
            }],
        });
        v.apply(&DebugUpdate::Configurations {
            configurations: vec![],
            error: None,
        });
        Signal::new(v)
    });
    let debuggers = vec![DebuggerContribution {
        debug_type: "debugpy".into(),
        label: "Python Debugger".into(),
        initial_configurations: vec![],
    }];
    rsx! { RunDebugWindow { view, debuggers, connected: true, folder: "/w", act: record } }
}

#[test]
fn fr_l31_42_run_and_debug_window_selects_frames_and_edits_breakpoints() {
    let mut dom = HtmlDom::new(window);
    dom.layout(1000.0, 800.0, 1.0);
    assert!(
        dom.element_by_id("debug-var-1").is_some(),
        "x of Locals is shown"
    );
    click(&mut dom, "debug-frame-11");
    assert_eq!(
        actions(),
        [
            ViewAction::Debug(DebugRequest::SelectFrame {
                session: "s".into(),
                thread_id: 1,
                frame_id: 11
            }),
            ViewAction::Goto {
                uri: uri("/w/lib.py"),
                line: 7
            },
        ]
    );
    click(&mut dom, "debug-bp-check-b2");
    click(&mut dom, "debug-bp-remove-b1");
    click(&mut dom, "debug-filter-raised");
    assert_eq!(
        requests(),
        [
            DebugRequest::EditBreakpoint {
                id: "b2".into(),
                condition: None,
                hit_condition: None,
                log_message: None,
                enabled: true
            },
            DebugRequest::RemoveBreakpoint { id: "b1".into() },
            DebugRequest::SetExceptionFilter {
                filter: "raised".into(),
                enabled: true
            },
        ]
    );
    click(&mut dom, "debug-bp-b1");
    assert_eq!(
        actions(),
        [ViewAction::Goto {
            uri: uri("/w/src/main.py"),
            line: 2
        }]
    );
    click(&mut dom, "debug-breakpoints-clear");
    assert_eq!(
        requests(),
        [
            DebugRequest::RemoveBreakpoint { id: "b1".into() },
            DebugRequest::RemoveBreakpoint { id: "b2".into() }
        ]
    );
}

#[test]
fn fr_l31_42_run_and_debug_window_adds_watches_and_function_breakpoints() {
    let mut dom = HtmlDom::new(window);
    click(&mut dom, "debug-watch-add");
    dom.layout(1000.0, 800.0, 1.0);
    let field = dom.element_by_id("debug-watch-input").unwrap();
    dom.input(field, "x + 1");
    dom.submit(field);
    let added = actions();
    assert!(
        added.contains(&ViewAction::WatchesChanged),
        "the workspace keeps the new list (FR-L31.42)"
    );
    let added: Vec<DebugRequest> = added
        .into_iter()
        .filter_map(|a| match a {
            ViewAction::Debug(r) => Some(r),
            _ => None,
        })
        .collect();
    let [
        DebugRequest::Evaluate {
            expression,
            context,
            frame_id,
            ..
        },
    ] = <[DebugRequest; 1]>::try_from(added).unwrap()
    else {
        panic!()
    };
    assert_eq!(
        (expression.as_str(), context, frame_id),
        ("x + 1", EvalContext::Watch, Some(10))
    );

    click(&mut dom, "debug-function-add");
    dom.layout(1000.0, 800.0, 1.0);
    let field = dom.element_by_id("debug-function-input").unwrap();
    dom.input(field, " main ");
    dom.submit(field);
    assert_eq!(
        requests(),
        [DebugRequest::AddFunctionBreakpoint {
            name: "main".into()
        }]
    );
}

#[test]
fn fr_l31_42_without_launch_json_the_window_offers_the_debuggers() {
    let mut dom = HtmlDom::new(window);
    click(&mut dom, "debug-create-0");
    assert_eq!(
        requests(),
        [DebugRequest::ProvideConfigurations {
            debug_type: "debugpy".into()
        }]
    );
}

fn console() -> Element {
    let view = use_hook(|| Signal::new(stopped_view().0));
    rsx! { DebugConsole { view, act: record } }
}

#[test]
fn fr_l31_42_debug_console_evaluates_input() {
    let mut dom = HtmlDom::new(console);
    dom.layout(1000.0, 800.0, 1.0);
    let field = dom.element_by_id("debug-console-input").unwrap();
    dom.input(field, "x * 2");
    dom.submit(field);
    let [
        DebugRequest::Evaluate {
            session,
            expression,
            context,
            frame_id,
            ..
        },
    ] = <[DebugRequest; 1]>::try_from(requests()).unwrap()
    else {
        panic!()
    };
    assert_eq!(
        (session.as_str(), expression.as_str(), context, frame_id),
        ("s", "x * 2", EvalContext::Repl, Some(10))
    );
}

/// A stopped view with `x = 41` and a read-only `y` in Locals, the adapter able to set variables
/// when `can_set` is.
fn variables_view(can_set: bool) -> DebugView {
    let (mut v, _) = stopped_view();
    v.apply(&sessions(vec![SessionInfo { can_set_variables: can_set, ..session("s", paused(), Some(10)) }]));
    v.apply(&DebugUpdate::Variables {
        session: "s".into(),
        reference: 100,
        variables: vec![var("x", "41", 0), VariableInfo { read_only: true, ..var("y", "1", 0) }],
        error: None,
    });
    v
}

// Set Value (Code-OSS `variablesView`): a variable the adapter can set and that is not read-only
// is edited in place, and only a changed value is sent, as DAP `setVariable` on its container in
// the focused frame.
#[test]
fn fr_l31_42_set_value_edits_a_variable_and_sends_only_a_change() {
    let mut v = variables_view(true);
    let rows = v.variable_rows();
    let (x, y) = (rows[1].clone(), rows[2].clone());
    assert_eq!((x.name.as_str(), y.name.as_str()), ("x", "y"));
    assert!(x.editable);
    assert!(!y.editable, "a read-only variable is not offered");
    assert!(!rows[0].editable, "a scope is not a variable");
    assert!(!v.begin_edit(&y));
    assert!(v.begin_edit(&x));
    assert_eq!(v.editing(), Some(x.path.as_str()));
    assert_eq!(v.submit_edit(&x, "41"), None, "the same value is not sent");
    assert_eq!(v.editing(), None);
    assert!(v.begin_edit(&x));
    assert_eq!(
        v.submit_edit(&x, "5"),
        Some(DebugRequest::SetVariable {
            session: "s".into(),
            reference: 100,
            name: "x".into(),
            value: "5".into(),
            evaluate_name: Some("x".into()),
            frame_id: Some(10),
        })
    );
    assert!(v.begin_edit(&x));
    v.cancel_edit();
    assert_eq!(v.editing(), None);

    let mut cannot = variables_view(false);
    let x = cannot.variable_rows()[1].clone();
    assert!(!x.editable);
    assert!(!cannot.begin_edit(&x), "the adapter cannot set variables");
}

// After a set, the variables and the watches load again (Code-OSS `updateViews`); a refusal is
// shown in the variable's row.
#[test]
fn fr_l31_42_a_set_variable_loads_the_views_again_or_shows_why_it_failed() {
    let mut v = variables_view(true);
    v.add_watch("x + 1");
    let fx = v.apply(&DebugUpdate::VariableSet {
        session: "s".into(),
        reference: 100,
        name: "x".into(),
        result: Ok("5".into()),
    });
    assert!(fx.requests.contains(&DebugRequest::Variables { session: "s".into(), reference: 100 }), "{:?}", fx.requests);
    assert!(
        fx.requests.iter().any(|r| matches!(r, DebugRequest::Evaluate { expression, context: EvalContext::Watch, .. } if expression == "x + 1")),
        "{:?}",
        fx.requests
    );

    let mut v = variables_view(true);
    let fx = v.apply(&DebugUpdate::VariableSet {
        session: "s".into(),
        reference: 100,
        name: "x".into(),
        result: Err("cannot assign".into()),
    });
    assert!(fx.requests.is_empty());
    let x = v.variable_rows()[1].clone();
    assert_eq!(x.error.as_deref(), Some("cannot assign"));
    assert_eq!(x.name, "x");
}

fn editable_window() -> Element {
    let view = use_hook(|| Signal::new(variables_view(true)));
    rsx! { RunDebugWindow { view, debuggers: vec![], connected: true, folder: "/w", act: record } }
}

// F2 on a variable row opens its value for editing; Enter sends it.
#[test]
fn fr_l31_42_variables_view_f2_sets_a_value() {
    let mut dom = HtmlDom::new(editable_window);
    dom.layout(1000.0, 800.0, 1.0);
    let row = dom.element_by_id("debug-var-1").expect("x is shown");
    dom.key_down(row, Key::F2, Code::F2, Modifiers::empty());
    dom.render();
    dom.layout(1000.0, 800.0, 1.0);
    let field = dom.element_by_id("debug-var-1-edit").expect("the value is edited");
    dom.input(field, "5");
    dom.submit(field);
    assert_eq!(
        requests(),
        [DebugRequest::SetVariable {
            session: "s".into(),
            reference: 100,
            name: "x".into(),
            value: "5".into(),
            evaluate_name: Some("x".into()),
            frame_id: Some(10),
        }]
    );
    // The read-only `y` does not open.
    let row = dom.element_by_id("debug-var-2").unwrap();
    dom.key_down(row, Key::F2, Code::F2, Modifiers::empty());
    dom.render();
    assert!(dom.element_by_id("debug-var-2-edit").is_none());
}

/// SPEC FR-L31.42, Code-OSS `DebugStorage.loadWatchExpressions`: a window opens with the watch
/// expressions its workspace kept, in their order, and evaluates them when a thread stops.
#[test]
fn fr_l31_42_kept_watch_expressions_come_back_and_are_evaluated_at_a_stop() {
    let mut v = DebugView::with_watches(&["x + 1".to_owned(), " ".to_owned(), "order.items".to_owned()]);
    assert_eq!(v.watch_expressions(), ["x + 1", "order.items"], "an empty one is dropped");
    assert!(v.watches().iter().all(|w| w.result.is_none()));
    let fx = v.apply(&sessions(vec![session("s", paused(), Some(10))]));
    let evaluated: Vec<&str> = fx
        .requests
        .iter()
        .filter_map(|r| match r {
            DebugRequest::Evaluate { expression, context: EvalContext::Watch, .. } => Some(expression.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(evaluated, ["x + 1", "order.items"]);
    v.remove_watch(v.watches()[0].id);
    assert_eq!(v.watch_expressions(), ["order.items"]);
}
