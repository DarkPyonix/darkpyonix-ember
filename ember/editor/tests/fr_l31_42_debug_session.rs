//! The debug session layer against a fake extension host: the host side of the flow, no server. The rig plays the extension host and a debug adapter. It answers every call
//! the core makes (activation, configuration resolution, `$startDASession`, `$sendDAMessage`)
//! and delivers the adapter's messages through `$acceptDAMessage`, so each test asserts the
//! calls and the DAP requests in the order the core sent them.
//!
//! The fake adapter behaves as debugpy does: `initialized` follows the `launch` request, the
//! `launch` response waits for `configurationDone`, and a source breakpoint stops the program.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use ember_editor::debugger::{
    ConfigSelector, DebugRequest, DebugUpdate, EvalContext, EvalResult, SessionInfo, SessionState,
};
use ember_editor::languages::LanguageRegistry;
use ember_editor::{BridgeAction, CoreOptions, Effect, Input, SessionCore, SessionUpdate, WidgetBridge};
use ember_editor_conn::rpc::{Arg, IncomingRequest, Reply};
use ember_editor_conn::uri::UriComponents;
use serde_json::{json, Value};

const AUTH: &str = "127.0.0.1:9";
const PROXY: &str = "MainThreadDebugService";
const LAUNCH_JSON: &str = r#"{
    // the usual file
    "version": "0.2.0",
    "configurations": [
        {
            "name": "Python: Current File",
            "type": "debugpy",
            "request": "launch",
            "program": "${file}",
            "console": "internalConsole", // no terminal in this window
        },
    ],
}"#;
const MAIN_PY: &str = "x = 41\ny = x + 1\nprint(y)\n";

fn uri(path: &str) -> UriComponents {
    UriComponents::remote(AUTH, path)
}

fn resp(req: &Value, body: Value) -> Value {
    json!({"type": "response", "seq": 0, "request_seq": req["seq"], "command": req["command"], "success": true, "body": body})
}

fn event(name: &str, body: Value) -> Value {
    json!({"type": "event", "seq": 0, "event": name, "body": body})
}

/// A debug adapter that behaves as debugpy does.
#[derive(Default)]
struct Adapter {
    held_launch: Option<Value>,
    armed: bool,
    swallow_disconnect: bool,
}

impl Adapter {
    fn answer(&mut self, req: &Value) -> Vec<Value> {
        let args = &req["arguments"];
        match req["command"].as_str().unwrap_or("") {
            "initialize" => vec![resp(
                req,
                json!({
                    "supportsConfigurationDoneRequest": true,
                    "supportsFunctionBreakpoints": true,
                    "supportTerminateDebuggee": true,
                    "exceptionBreakpointFilters": [{"filter": "raised", "label": "Raised Exceptions", "default": false}],
                }),
            )],
            "launch" | "attach" => {
                self.held_launch = Some(req.clone());
                vec![event("initialized", json!({}))]
            }
            "setBreakpoints" => {
                let lines: Vec<Value> = args["lines"].as_array().cloned().unwrap_or_default();
                self.armed |= !lines.is_empty();
                let bps: Vec<Value> = lines.iter().map(|l| json!({"verified": true, "line": l, "id": 1000 + l.as_i64().unwrap_or(0)})).collect();
                vec![resp(req, json!({ "breakpoints": bps }))]
            }
            "setFunctionBreakpoints" => vec![resp(req, json!({"breakpoints": []}))],
            "configurationDone" => {
                let mut out = vec![resp(req, json!({}))];
                if let Some(launch) = self.held_launch.take() {
                    out.push(resp(&launch, Value::Null));
                }
                if self.armed {
                    out.push(event(
                        "stopped",
                        json!({"reason": "breakpoint", "threadId": 1, "allThreadsStopped": true}),
                    ));
                }
                out
            }
            "threads" => vec![resp(
                req,
                json!({"threads": [{"id": 1, "name": "MainThread"}]}),
            )],
            "stackTrace" => vec![resp(
                req,
                json!({"stackFrames": [{"id": 10, "name": "<module>", "source": {"name": "main.py", "path": "/w/main.py"}, "line": 2, "column": 1}], "totalFrames": 1}),
            )],
            "scopes" => vec![resp(
                req,
                json!({"scopes": [{"name": "Locals", "variablesReference": 100, "expensive": false}]}),
            )],
            "variables" => vec![resp(
                req,
                json!({"variables": [{"name": "x", "value": "41", "type": "int", "variablesReference": 0}]}),
            )],
            "evaluate" => vec![resp(
                req,
                json!({"result": "42", "type": "int", "variablesReference": 0}),
            )],
            "continue" => vec![
                resp(req, json!({"allThreadsContinued": true})),
                event("output", json!({"category": "stdout", "output": "42\n"})),
                event("terminated", json!({})),
            ],
            "disconnect" if self.swallow_disconnect => vec![],
            _ => vec![resp(req, Value::Null)],
        }
    }
}

/// The extension host and the adapter, around one core.
struct Rig {
    core: SessionCore,
    now: Instant,
    next_req: u32,
    files: HashMap<String, String>,
    /// What the configuration providers answer; `None` hands the configuration back unchanged.
    resolve: Option<Value>,
    adapter: Adapter,
    /// Every call the core made, in order.
    calls: Vec<(String, Vec<Value>)>,
    /// Every DAP request the core sent to the adapter, in order.
    dap: Vec<Value>,
    /// Every DAP message the core sent, responses to reverse requests included.
    dap_all: Vec<Value>,
    ui: Vec<DebugUpdate>,
    responses: Vec<(u32, Result<Reply, Option<Value>>)>,
}

impl Rig {
    fn new() -> Self {
        let t0 = Instant::now();
        let mut core = SessionCore::new(
            LanguageRegistry::from_extensions(&[]),
            CoreOptions::default(),
            t0,
        );
        core.set_workspace_folder(uri("/w"));
        Rig {
            core,
            now: t0,
            next_req: 0,
            files: HashMap::new(),
            resolve: None,
            adapter: Adapter::default(),
            calls: Vec::new(),
            dap: Vec::new(),
            dap_all: Vec::new(),
            ui: Vec::new(),
            responses: Vec::new(),
        }
    }

    /// A window with `main.py` open and `launch.json` on the server, and the debugpy extension
    /// registered as it does at activation.
    fn fixture() -> Self {
        let mut rig = Rig::new();
        rig.files
            .insert("/w/.vscode/launch.json".into(), LAUNCH_JSON.into());
        rig.open("/w/main.py", MAIN_PY);
        rig.ext("$registerDebugTypes", vec![json!(["debugpy"])]);
        rig.ext(
            "$registerDebugConfigurationProvider",
            vec![
                json!("debugpy"),
                json!(1),
                json!(false),
                json!(true),
                json!(true),
                json!(7),
            ],
        );
        rig.calls.clear();
        rig.ui.clear();
        rig
    }

    fn open(&mut self, path: &str, text: &str) {
        self.files.insert(path.into(), text.into());
        self.feed(Input::Open { uri: uri(path) });
        self.feed(Input::Focus { uri: uri(path) });
    }

    fn request(&mut self, method: &str, args: Vec<Value>) -> Input {
        self.next_req += 1;
        Input::ExtHostRequest(IncomingRequest {
            req: self.next_req,
            rpc_id: ember_editor_conn::rpc_ids::id_of(PROXY).unwrap(),
            proxy: Some(PROXY),
            method: method.into(),
            args: args.into_iter().map(Arg::Json).collect(),
            cancellable: false,
        })
    }

    /// A request of the extension host; returns its request id.
    fn ext(&mut self, method: &str, args: Vec<Value>) -> u32 {
        let input = self.request(method, args);
        let req = self.next_req;
        self.feed(input);
        req
    }

    /// The adapter of `handle` sends `message`.
    fn adapter_says(&mut self, handle: i64, message: Value) {
        self.ext("$acceptDAMessage", vec![json!(handle), message]);
    }

    fn debug(&mut self, request: DebugRequest) {
        self.feed(Input::Debug(request));
    }

    /// Run `input` and everything it sets going, in the order a server would.
    fn feed(&mut self, input: Input) {
        let mut queue = VecDeque::from([input]);
        while let Some(input) = queue.pop_front() {
            let fx = self.core.handle(self.now, input);
            for e in fx {
                match e {
                    Effect::Rpc { token, call } => {
                        let args: Vec<Value> = call
                            .args
                            .iter()
                            .map(|a| a.as_json().cloned().unwrap_or(Value::Null))
                            .collect();
                        self.calls.push((call.method.to_owned(), args.clone()));
                        match call.method {
                            "$sendDAMessage" => {
                                let handle = args[0].as_i64().unwrap();
                                let message = args[1].clone();
                                self.dap_all.push(message.clone());
                                if message["type"] == "request" {
                                    self.dap.push(message.clone());
                                    for answer in self.adapter.answer(&message) {
                                        let input = self.request(
                                            "$acceptDAMessage",
                                            vec![json!(handle), answer],
                                        );
                                        queue.push_back(input);
                                    }
                                }
                            }
                            "$startDASession" => {
                                if let Some(t) = token {
                                    queue.push_back(Input::RpcReply {
                                        token: t,
                                        result: Ok(Value::Null),
                                    });
                                }
                                let id = args[1]["id"].clone();
                                let cached = self.request("$sessionCached", vec![id]);
                                queue.push_back(cached);
                            }
                            "$resolveDebugConfiguration"
                            | "$resolveDebugConfigurationWithSubstitutedVariables" => {
                                if let Some(t) = token {
                                    let answer =
                                        self.resolve.clone().unwrap_or_else(|| args[2].clone());
                                    queue.push_back(Input::RpcReply {
                                        token: t,
                                        result: Ok(answer),
                                    });
                                }
                            }
                            _ => {
                                if let Some(t) = token {
                                    queue.push_back(Input::RpcReply {
                                        token: t,
                                        result: Ok(Value::Null),
                                    });
                                }
                            }
                        }
                    }
                    Effect::ReadFile { uri, purpose } => {
                        let result = self
                            .files
                            .get(&uri.path)
                            .map(|t| t.as_bytes().to_vec())
                            .ok_or_else(|| "EntryNotFound (FileSystemError)".to_owned());
                        queue.push_back(Input::FileRead {
                            uri,
                            purpose,
                            result,
                        });
                    }
                    Effect::Respond { req, result } => self.responses.push((req, result)),
                    Effect::Ui(SessionUpdate::Debug(update)) => self.ui.push(update),
                    _ => {}
                }
            }
        }
    }

    fn methods(&self) -> Vec<&str> {
        self.calls.iter().map(|(m, _)| m.as_str()).collect()
    }

    fn calls_of(&self, method: &str) -> Vec<&Vec<Value>> {
        self.calls
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, a)| a)
            .collect()
    }

    fn dap_commands(&self) -> Vec<String> {
        self.dap
            .iter()
            .map(|m| m["command"].as_str().unwrap_or("").to_owned())
            .collect()
    }

    fn dap_request(&self, command: &str) -> Value {
        self.dap
            .iter()
            .rev()
            .find(|m| m["command"] == command)
            .cloned()
            .unwrap_or_else(|| panic!("no {command} request in {:#?}", self.dap))
    }

    fn sessions(&self) -> Vec<SessionInfo> {
        self.ui
            .iter()
            .rev()
            .find_map(|u| match u {
                DebugUpdate::Sessions { sessions, .. } => Some(sessions.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn session_id(&self) -> String {
        self.sessions().first().expect("a session").id.clone()
    }

    fn response(&self, req: u32) -> &Result<Reply, Option<Value>> {
        &self
            .responses
            .iter()
            .rev()
            .find(|(r, _)| *r == req)
            .unwrap_or_else(|| panic!("no response to {req}: {:?}", self.responses))
            .1
    }

    /// Start the configuration of `launch.json` and let the adapter run to its first stop.
    fn start_python(&mut self) {
        self.debug(DebugRequest::Start {
            selector: ConfigSelector::Name("Python: Current File".into()),
        });
    }

    fn toggle(&mut self, line: u32) {
        self.debug(DebugRequest::ToggleBreakpoint {
            uri: uri("/w/main.py"),
            line,
        });
    }
}

#[test]
fn fr_l31_42_start_from_launch_json_resolves_and_starts_the_adapter() {
    let mut rig = Rig::fixture();
    rig.start_python();
    let names = rig.methods();
    assert_eq!(
        names[..6].to_vec(),
        vec![
            "$activateByEvent",
            "$activateByEvent",
            "$activateByEvent",
            "$resolveDebugConfiguration",
            "$resolveDebugConfigurationWithSubstitutedVariables",
            "$startDASession",
        ],
        "{names:?}"
    );
    let events: Vec<Value> = rig
        .calls_of("$activateByEvent")
        .iter()
        .take(3)
        .map(|a| a[0].clone())
        .collect();
    assert_eq!(
        events,
        vec![
            json!("onDebugResolve"),
            json!("onDebug"),
            json!("onDebugResolve:debugpy")
        ]
    );

    let resolve = rig.calls_of("$resolveDebugConfiguration")[0].clone();
    assert_eq!(resolve[0], json!(7));
    assert_eq!(resolve[1]["path"], "/w");
    assert_eq!(
        resolve[2]["program"], "${file}",
        "providers see the configuration before substitution"
    );
    let resolved = rig.calls_of("$resolveDebugConfigurationWithSubstitutedVariables")[0].clone();
    assert_eq!(
        resolved[2]["program"], "/w/main.py",
        "the second chain sees the substituted values"
    );

    let started = rig.calls_of("$startDASession")[0].clone();
    assert_eq!(started[0], json!(1));
    let dto = &started[1];
    assert_eq!(dto["type"], "debugpy");
    assert_eq!(dto["name"], "Python: Current File");
    assert_eq!(dto["folderUri"]["path"], "/w");
    assert_eq!(dto["configuration"]["program"], "/w/main.py");
    assert_eq!(dto["configuration"]["__sessionId"], dto["id"]);
    assert_eq!(rig.dap_commands()[0], "initialize");
    assert_eq!(rig.sessions().len(), 1);
}

#[test]
fn fr_l31_42_a_missing_launch_json_an_unknown_name_and_an_unsupported_type_fail_with_a_message() {
    let failure = |rig: &Rig| {
        rig.ui.iter().rev().find_map(|u| match u {
            DebugUpdate::StartFailed { message } => Some(message.clone()),
            _ => None,
        })
    };
    let mut rig = Rig::fixture();
    rig.start_python();
    rig.debug(DebugRequest::Stop { session: None });
    rig.ui.clear();
    rig.debug(DebugRequest::Start {
        selector: ConfigSelector::Name("Nope".into()),
    });
    assert!(
        failure(&rig).unwrap().contains("no configuration"),
        "{:?}",
        failure(&rig)
    );

    let mut rig = Rig::fixture();
    rig.files.remove("/w/.vscode/launch.json");
    rig.start_python();
    assert!(
        failure(&rig).is_some(),
        "a missing file is no configuration"
    );

    let mut rig = Rig::fixture();
    rig.debug(DebugRequest::Start {
        selector: ConfigSelector::Config(json!({"type": "nope", "request": "launch", "name": "x"})),
    });
    assert!(
        failure(&rig).unwrap().contains("'nope' is not supported"),
        "{:?}",
        failure(&rig)
    );
    assert!(!rig.methods().contains(&"$startDASession"));
}

#[test]
fn fr_l31_42_a_provider_that_answers_null_cancels_the_start() {
    let mut rig = Rig::fixture();
    rig.resolve = Some(Value::Null);
    let config =
        json!({"type": "debugpy", "request": "launch", "name": "ext", "program": "/w/a.py"});
    let req = rig.ext("$startDebugging", vec![Value::Null, config, json!({})]);
    assert!(!rig.methods().contains(&"$startDASession"));
    assert!(
        matches!(rig.response(req), Ok(Reply::Json(Value::Bool(false)))),
        "{:?}",
        rig.response(req)
    );
}

#[test]
fn fr_l31_42_dap_initialize_launch_breakpoints_configuration_done_order() {
    let mut rig = Rig::fixture();
    rig.toggle(2);
    rig.start_python();
    assert_eq!(
        rig.dap_commands()[..6].to_vec(),
        vec![
            "initialize",
            "launch",
            "setBreakpoints",
            "setFunctionBreakpoints",
            "setExceptionBreakpoints",
            "configurationDone"
        ],
        "{:?}",
        rig.dap_commands()
    );
    let initialize = rig.dap_request("initialize");
    assert_eq!(initialize["arguments"]["adapterID"], "debugpy");
    assert_eq!(initialize["arguments"]["linesStartAt1"], true);
    assert_eq!(initialize["arguments"]["pathFormat"], "path");
    assert_eq!(
        initialize["arguments"]["supportsRunInTerminalRequest"],
        false
    );
    let launch = rig.dap_request("launch");
    assert_eq!(launch["arguments"]["program"], "/w/main.py");
    assert_eq!(launch["arguments"]["console"], "internalConsole");
    let set = rig.dap_request("setBreakpoints");
    assert_eq!(
        set["arguments"]["source"],
        json!({"name": "main.py", "path": "/w/main.py"})
    );
    assert_eq!(set["arguments"]["lines"], json!([2]));
    assert_eq!(set["arguments"]["breakpoints"], json!([{"line": 2}]));
    // The sequence numbers count up from 1 and never repeat.
    let seqs: Vec<i64> = rig.dap.iter().map(|m| m["seq"].as_i64().unwrap()).collect();
    assert_eq!(seqs, (1..=seqs.len() as i64).collect::<Vec<_>>());

    // The extension host saw the breakpoint (0-based) and the session start.
    let delta = rig.calls_of("$acceptBreakpointsDelta")[0][0].clone();
    assert_eq!(delta["added"][0]["type"], "source");
    assert_eq!(delta["added"][0]["line"], 1);
    assert_eq!(delta["added"][0]["uri"]["path"], "/w/main.py");
    assert!(rig.methods().contains(&"$acceptDebugSessionStarted"));
    // What the adapter said about the breakpoint is kept.
    let verified = rig.ui.iter().rev().find_map(|u| match u {
        DebugUpdate::Breakpoints { source, .. } => Some(source.clone()),
        _ => None,
    });
    let source = verified.expect("a breakpoints update");
    assert!(
        source[0].verified && source[0].adapter_line == Some(2),
        "{source:?}"
    );
}

#[test]
fn fr_l31_42_a_session_without_breakpoints_still_sends_configuration_done_and_runs() {
    let mut rig = Rig::fixture();
    rig.start_python();
    assert_eq!(
        rig.dap_commands()[..5].to_vec(),
        vec![
            "initialize",
            "launch",
            "setFunctionBreakpoints",
            "setExceptionBreakpoints",
            "configurationDone"
        ]
    );
    assert_eq!(rig.sessions()[0].state, SessionState::Running);
}

#[test]
fn fr_l31_42_stopped_event_loads_threads_and_stack_and_updates_the_session() {
    let mut rig = Rig::fixture();
    rig.toggle(2);
    rig.start_python();
    let id = rig.session_id();
    let sessions = rig.sessions();
    let session = &sessions[0];
    assert!(
        matches!(&session.state, SessionState::Stopped { reason, thread_id: Some(1), .. } if reason == "breakpoint"),
        "{:?}",
        session.state
    );
    assert_eq!((session.thread_id, session.frame_id), (Some(1), Some(10)));
    let commands = rig.dap_commands();
    let after = &commands[commands
        .iter()
        .position(|c| c == "configurationDone")
        .unwrap()..];
    assert!(
        after.contains(&"threads".to_owned())
            && after.contains(&"stackTrace".to_owned())
            && after.contains(&"scopes".to_owned()),
        "{commands:?}"
    );
    assert_eq!(rig.dap_request("stackTrace")["arguments"]["threadId"], 1);
    assert_eq!(rig.dap_request("scopes")["arguments"]["frameId"], 10);

    let threads = rig.ui.iter().find_map(|u| match u {
        DebugUpdate::Threads { threads, .. } => Some(threads.clone()),
        _ => None,
    });
    let threads = threads.expect("threads");
    assert_eq!((threads[0].id, threads[0].name.as_str()), (1, "MainThread"));
    assert!(threads[0].stopped);
    let stack = rig.ui.iter().find_map(|u| match u {
        DebugUpdate::CallStack {
            session,
            thread_id,
            frames,
            ..
        } => Some((session.clone(), *thread_id, frames.clone())),
        _ => None,
    });
    let (stack_session, thread_id, frames) = stack.expect("a call stack");
    assert_eq!((stack_session, thread_id), (id.clone(), 1));
    assert_eq!((frames[0].id, frames[0].line, frames[0].column), (10, 2, 1));
    assert_eq!(frames[0].uri, Some(uri("/w/main.py")));
    let scopes = rig.ui.iter().find_map(|u| match u {
        DebugUpdate::Scopes {
            frame_id: 10,
            scopes,
            ..
        } => Some(scopes.clone()),
        _ => None,
    });
    assert_eq!(scopes.expect("scopes")[0].variables_reference, 100);
    // The extension host is told which frame has the focus.
    let focus = rig.calls_of("$acceptStackFrameFocus")[0][0].clone();
    assert_eq!(
        (focus["kind"].as_str(), focus["frameId"].as_i64()),
        (Some("stackFrame"), Some(10))
    );
}

/// Hand the debug updates the rig collected to a widget bridge, as the window's update loop does,
/// and return the actions it gave back.
fn bridge_takes(bridge: &mut WidgetBridge, rig: &mut Rig) -> Vec<BridgeAction> {
    rig.ui
        .drain(..)
        .map(|u| bridge.apply(SessionUpdate::Debug(u)))
        .collect()
}

// FR-L31.42: the debug updates reach the window through the widget bridge, which keeps what the
// editor's controls show: the session for the toolbar, the stopped line for the gutter and the
// breakpoints as the adapter verified them.
#[test]
fn fr_l31_42_the_bridge_passes_debug_updates_and_keeps_the_session_the_stop_and_the_breakpoints() {
    let mut rig = Rig::fixture();
    let mut bridge = WidgetBridge::new();
    let main = uri("/w/main.py");
    rig.toggle(2);
    let actions = bridge_takes(&mut bridge, &mut rig);
    assert!(!actions.is_empty());
    assert!(actions.iter().all(|a| matches!(a, BridgeAction::Debug(_))), "{actions:?}");
    let marked: Vec<_> = bridge.debug().breakpoints_in(&main).collect();
    assert_eq!((marked.len(), marked[0].line, marked[0].verified), (1, 2, false));
    assert!(bridge.debug().active_session().is_none());

    rig.start_python();
    let id = rig.session_id();
    let actions = bridge_takes(&mut bridge, &mut rig);
    assert!(actions.iter().all(|a| matches!(a, BridgeAction::Debug(_))), "{actions:?}");
    let view = bridge.debug();
    let session = view.active_session().expect("the shown session");
    assert_eq!(session.id, id);
    assert!(matches!(session.state, SessionState::Stopped { .. }), "{:?}", session.state);
    assert_eq!(view.stopped_line(&main), Some(2));
    assert_eq!(view.stopped_line(&uri("/w/other.py")), None);
    let at = view.stopped_at.as_ref().unwrap();
    assert_eq!((at.thread_id, at.frame_id, at.column), (1, 10, 1));
    assert_eq!(view.pause_thread(), Some(1));
    assert!(view.breakpoints_in(&main).all(|bp| bp.verified), "{:?}", view.breakpoints);

    // Continuing runs the thread on: the stop mark goes, and the session ends with the program.
    rig.debug(DebugRequest::Continue {
        session: id.clone(),
        thread_id: 1,
    });
    bridge_takes(&mut bridge, &mut rig);
    assert_eq!(bridge.debug().stopped_at, None);
    assert!(bridge.debug().active_session().is_none(), "{:?}", bridge.debug().sessions);
    assert!(bridge.debug().threads.is_empty());
    // The breakpoints outlive the session, as in Code-OSS.
    assert_eq!(bridge.debug().breakpoints_in(&main).count(), 1);
}

#[test]
fn fr_l31_42_stepping_runs_the_session_and_a_stop_without_a_thread_uses_the_first_one() {
    let mut rig = Rig::fixture();
    rig.toggle(2);
    rig.start_python();
    let id = rig.session_id();
    rig.debug(DebugRequest::StepOver {
        session: id.clone(),
        thread_id: 1,
    });
    assert_eq!(rig.dap_request("next")["arguments"], json!({"threadId": 1}));
    assert_eq!(
        rig.sessions()[0].state,
        SessionState::Running,
        "a step the adapter answered without `stopped` leaves the thread running"
    );
    rig.adapter_says(
        1,
        event(
            "stopped",
            json!({"reason": "pause", "allThreadsStopped": true}),
        ),
    );
    assert!(
        matches!(&rig.sessions()[0].state, SessionState::Stopped { reason, thread_id: None, .. } if reason == "pause")
    );
    assert_eq!(
        rig.dap_request("stackTrace")["arguments"]["threadId"],
        1,
        "the first listed thread"
    );
}

#[test]
fn fr_l31_42_breakpoint_change_during_a_session_sends_set_breakpoints_and_a_delta() {
    let mut rig = Rig::fixture();
    rig.toggle(2);
    rig.start_python();
    let before = rig.dap.len();
    rig.calls.clear();

    rig.toggle(3);
    assert_eq!(rig.dap.len(), before + 1);
    let set = rig.dap_request("setBreakpoints");
    assert_eq!(set["arguments"]["lines"], json!([2, 3]));
    let delta = rig.calls_of("$acceptBreakpointsDelta")[0][0].clone();
    assert_eq!(delta["added"][0]["line"], 2);
    assert!(delta.get("removed").is_none());

    // A condition and a log message ride on the same request.
    let id = rig
        .ui
        .iter()
        .rev()
        .find_map(|u| match u {
            DebugUpdate::Breakpoints { source, .. } => {
                source.iter().find(|b| b.line == 3).map(|b| b.id.clone())
            }
            _ => None,
        })
        .unwrap();
    rig.debug(DebugRequest::EditBreakpoint {
        id: id.clone(),
        condition: Some("x > 40".into()),
        hit_condition: Some("2".into()),
        log_message: Some("x is {x}".into()),
        enabled: true,
    });
    let set = rig.dap_request("setBreakpoints");
    assert_eq!(
        set["arguments"]["breakpoints"][1],
        json!({"line": 3, "condition": "x > 40", "hitCondition": "2", "logMessage": "x is {x}"})
    );
    let changed = rig.calls_of("$acceptBreakpointsDelta")[1][0].clone();
    assert_eq!(changed["changed"][0]["condition"], "x > 40");
    assert_eq!(changed["changed"][0]["logMessage"], "x is {x}");

    // Disabling leaves the line out; removing the last ones sends the file empty.
    rig.debug(DebugRequest::EditBreakpoint {
        id: id.clone(),
        condition: None,
        hit_condition: None,
        log_message: None,
        enabled: false,
    });
    assert_eq!(
        rig.dap_request("setBreakpoints")["arguments"]["lines"],
        json!([2])
    );
    rig.debug(DebugRequest::RemoveBreakpoint { id });
    rig.toggle(2);
    assert_eq!(
        rig.dap_request("setBreakpoints")["arguments"]["lines"],
        json!([])
    );
    let last = rig.calls_of("$acceptBreakpointsDelta").last().unwrap()[0].clone();
    assert_eq!(last["removed"].as_array().map(Vec::len), Some(1));
}

#[test]
fn fr_l31_42_breakpoints_from_the_extension_api_join_the_registry_and_function_breakpoints_are_sent(
) {
    let mut rig = Rig::fixture();
    rig.start_python();
    let dto = json!({"type": "sourceMulti", "uri": uri("/w/main.py"), "lines": [{"id": "ext-1", "enabled": true, "line": 2, "character": 0, "condition": "y"}]});
    rig.ext("$registerBreakpoints", vec![json!([dto])]);
    let set = rig.dap_request("setBreakpoints");
    assert_eq!(
        set["arguments"]["breakpoints"],
        json!([{"line": 3, "condition": "y"}])
    );
    rig.debug(DebugRequest::AddFunctionBreakpoint {
        name: "main".into(),
    });
    assert_eq!(
        rig.dap_request("setFunctionBreakpoints")["arguments"]["breakpoints"],
        json!([{"name": "main"}])
    );
    rig.debug(DebugRequest::SetExceptionFilter {
        filter: "raised".into(),
        enabled: true,
    });
    assert_eq!(
        rig.dap_request("setExceptionBreakpoints")["arguments"]["filters"],
        json!(["raised"])
    );
    rig.ext(
        "$unregisterBreakpoints",
        vec![json!(["ext-1"]), json!([]), json!([])],
    );
    assert_eq!(
        rig.dap_request("setBreakpoints")["arguments"]["lines"],
        json!([])
    );
    let removed = rig.calls_of("$acceptBreakpointsDelta").last().unwrap()[0]["removed"].clone();
    assert_eq!(removed, json!(["ext-1"]));
}

#[test]
fn fr_l31_42_output_and_append_debug_console_reach_the_console() {
    let mut rig = Rig::fixture();
    rig.toggle(2);
    rig.start_python();
    let id = rig.session_id();
    rig.debug(DebugRequest::Continue {
        session: id.clone(),
        thread_id: 1,
    });
    assert_eq!(
        rig.dap_request("continue")["arguments"],
        json!({"threadId": 1})
    );
    let lines: Vec<(Option<String>, String, String)> = rig
        .ui
        .iter()
        .filter_map(|u| match u {
            DebugUpdate::Console { session, entry } => {
                Some((session.clone(), entry.category.clone(), entry.text.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        lines,
        vec![(Some(id.clone()), "stdout".to_owned(), "42\n".to_owned())]
    );

    // The adapter ended the program: the session is gone from the list after one `Terminated`.
    assert!(
        rig.sessions()
            .iter()
            .all(|s| s.state == SessionState::Terminated)
            || rig.sessions().is_empty()
    );
    assert!(rig.dap_commands().contains(&"disconnect".to_owned()));

    // Text an extension appends goes to the console too, marked as a notice.
    let mut rig = Rig::fixture();
    rig.start_python();
    let id = rig.session_id();
    let req = rig.ext("$appendDebugConsole", vec![json!("from the extension\n")]);
    assert!(matches!(rig.response(req), Ok(Reply::Empty)));
    let entry = rig.ui.iter().rev().find_map(|u| match u {
        DebugUpdate::Console { session, entry } => Some((session.clone(), entry.clone())),
        _ => None,
    });
    let (session, entry) = entry.expect("a console line");
    assert_eq!(
        (session, entry.category.as_str(), entry.text.as_str()),
        (Some(id), "important", "from the extension\n")
    );
}

#[test]
fn fr_l31_42_evaluate_and_variables_round_trip() {
    let mut rig = Rig::fixture();
    rig.toggle(2);
    rig.start_python();
    let id = rig.session_id();
    rig.debug(DebugRequest::Variables {
        session: id.clone(),
        reference: 100,
    });
    assert_eq!(
        rig.dap_request("variables")["arguments"],
        json!({"variablesReference": 100})
    );
    let vars = rig.ui.iter().rev().find_map(|u| match u {
        DebugUpdate::Variables {
            reference: 100,
            variables,
            error,
            ..
        } => Some((variables.clone(), error.clone())),
        _ => None,
    });
    let (variables, error) = vars.expect("variables");
    assert_eq!(error, None);
    assert_eq!(
        (
            variables[0].name.as_str(),
            variables[0].value.as_str(),
            variables[0].type_name.as_deref()
        ),
        ("x", "41", Some("int"))
    );

    rig.debug(DebugRequest::Evaluate {
        id: 5,
        session: id.clone(),
        frame_id: Some(10),
        expression: "x + 1".into(),
        context: EvalContext::Repl,
    });
    assert_eq!(
        rig.dap_request("evaluate")["arguments"],
        json!({"expression": "x + 1", "context": "repl", "frameId": 10})
    );
    let evaluated = rig.ui.iter().rev().find_map(|u| match u {
        DebugUpdate::Evaluated { id: 5, result, .. } => Some(result.clone()),
        _ => None,
    });
    assert_eq!(
        evaluated,
        Some(Ok(EvalResult {
            result: "42".into(),
            type_name: Some("int".into()),
            variables_reference: 0
        }))
    );

    // A watch asks in its own context, and a session that is not there answers with an error.
    rig.debug(DebugRequest::Evaluate {
        id: 6,
        session: id,
        frame_id: None,
        expression: "y".into(),
        context: EvalContext::Watch,
    });
    assert_eq!(
        rig.dap_request("evaluate")["arguments"],
        json!({"expression": "y", "context": "watch"})
    );
    rig.debug(DebugRequest::Evaluate {
        id: 7,
        session: "nope".into(),
        frame_id: None,
        expression: "y".into(),
        context: EvalContext::Repl,
    });
    assert!(rig.ui.iter().any(|u| matches!(
        u,
        DebugUpdate::Evaluated {
            id: 7,
            result: Err(_),
            ..
        }
    )));
}

#[test]
fn fr_l31_42_extension_start_debugging_and_stop_debugging() {
    let mut rig = Rig::fixture();
    let config = json!({"type": "debugpy", "request": "launch", "name": "ext", "program": "${workspaceFolder}/a.py"});
    let started = rig.ext(
        "$startDebugging",
        vec![json!(uri("/w")), config, json!({"noDebug": true})],
    );
    assert!(
        matches!(rig.response(started), Ok(Reply::Json(Value::Bool(true)))),
        "{:?}",
        rig.response(started)
    );
    let launch = rig.dap_request("launch");
    assert_eq!(launch["arguments"]["program"], "/w/a.py");
    assert_eq!(launch["arguments"]["noDebug"], true);
    let id = rig.session_id();
    assert_eq!(rig.sessions()[0].name, "ext");

    let stopped = rig.ext("$stopDebugging", vec![json!(id.clone())]);
    assert_eq!(
        rig.dap_request("disconnect")["arguments"],
        json!({"restart": false, "terminateDebuggee": true})
    );
    assert!(
        matches!(rig.response(stopped), Ok(Reply::Empty)),
        "{:?}",
        rig.response(stopped)
    );
    let methods = rig.methods();
    assert!(
        methods.contains(&"$stopDASession") && methods.contains(&"$acceptDebugSessionTerminated"),
        "{methods:?}"
    );
    let ended = rig.ui.iter().rev().find_map(|u| match u {
        DebugUpdate::Sessions { sessions, .. }
            if sessions.iter().any(|s| s.state == SessionState::Terminated) =>
        {
            Some(sessions.clone())
        }
        _ => None,
    });
    assert_eq!(ended.expect("one update with the ended session")[0].id, id);
    assert!(
        rig.core.unimplemented_calls().is_empty(),
        "{:?}",
        rig.core.unimplemented_calls()
    );

    let unknown = rig.ext("$stopDebugging", vec![json!("not-a-session")]);
    assert!(matches!(rig.response(unknown), Err(Some(_))));
    let all = rig.ext("$stopDebugging", vec![Value::Null]);
    assert!(matches!(rig.response(all), Ok(Reply::Empty)));
}

#[test]
fn fr_l31_42_a_disconnect_the_adapter_never_answers_ends_after_the_timeout() {
    let mut rig = Rig::fixture();
    rig.adapter.swallow_disconnect = true;
    rig.start_python();
    let id = rig.session_id();
    rig.debug(DebugRequest::Stop { session: Some(id) });
    assert_eq!(
        rig.sessions()[0].state,
        SessionState::Running,
        "waiting for the adapter"
    );
    let wake = rig.core.next_wake().expect("a deadline");
    assert!(wake > rig.now, "the disconnect timeout lies ahead");
    rig.now += Duration::from_millis(600);
    rig.feed(Input::Tick);
    assert_eq!(rig.sessions()[0].state, SessionState::Terminated);
    assert!(rig.methods().contains(&"$stopDASession"));
}

#[test]
fn fr_l31_42_adapter_requests_run_in_terminal_is_refused_and_start_debugging_starts_a_child() {
    let mut rig = Rig::fixture();
    rig.start_python();
    let parent = rig.session_id();
    rig.adapter_says(1, json!({"type": "request", "seq": 50, "command": "runInTerminal", "arguments": {"args": ["python"], "cwd": "/w"}}));
    let refused = rig
        .dap_all
        .iter()
        .rev()
        .find(|m| m["request_seq"] == 50)
        .cloned()
        .expect("an answer");
    assert_eq!(
        (
            refused["type"].as_str(),
            refused["success"].as_bool(),
            refused["command"].as_str()
        ),
        (Some("response"), Some(false), Some("runInTerminal"))
    );
    assert!(refused["message"]
        .as_str()
        .unwrap()
        .contains("internalConsole"));

    rig.adapter_says(
        1,
        json!({"type": "request", "seq": 51, "command": "startDebugging", "arguments": {"request": "launch", "configuration": {"name": "child", "program": "/w/child.py"}}}),
    );
    let child = rig.calls_of("$startDASession").last().unwrap()[1].clone();
    assert_eq!(child["parent"], json!(parent));
    assert_eq!(
        child["type"], "debugpy",
        "the child is of the parent's type"
    );
    assert_eq!(child["configuration"]["program"], "/w/child.py");
    let answer = rig
        .dap_all
        .iter()
        .rev()
        .find(|m| m["request_seq"] == 51)
        .cloned()
        .expect("an answer to startDebugging");
    assert_eq!(answer["success"], true);
    assert_eq!(rig.sessions().len(), 2);

    // The child ends with its parent.
    rig.debug(DebugRequest::Stop {
        session: Some(parent),
    });
    assert!(
        rig.sessions()
            .iter()
            .all(|s| s.state == SessionState::Terminated)
            || rig.sessions().is_empty()
    );
    assert_eq!(rig.calls_of("$stopDASession").len(), 2);
}

#[test]
fn fr_l31_42_restart_without_the_request_stops_and_starts_the_same_configuration_again() {
    let mut rig = Rig::fixture();
    rig.start_python();
    let first = rig.session_id();
    rig.debug(DebugRequest::Restart {
        session: first.clone(),
    });
    let started = rig.calls_of("$startDASession");
    assert_eq!(started.len(), 2, "a second session started");
    assert_ne!(started[1][1]["id"], json!(first));
    assert_eq!(started[1][1]["configuration"]["program"], "/w/main.py");
    assert!(rig.methods().contains(&"$acceptDebugSessionTerminated"));
}

#[test]
fn fr_l31_42_list_configurations_reads_launch_json_and_provided_configurations_come_from_the_provider(
) {
    let mut rig = Rig::fixture();
    rig.debug(DebugRequest::ListConfigurations);
    let list = rig.ui.iter().find_map(|u| match u {
        DebugUpdate::Configurations {
            configurations,
            error,
        } => Some((configurations.clone(), error.clone())),
        _ => None,
    });
    let (configs, error) = list.expect("a list");
    assert_eq!(error, None);
    assert_eq!(
        (
            configs[0].name.as_str(),
            configs[0].debug_type.as_str(),
            configs[0].request.as_str()
        ),
        ("Python: Current File", "debugpy", "launch")
    );

    rig.ext(
        "$registerDebugConfigurationProvider",
        vec![
            json!("debugpy"),
            json!(1),
            json!(true),
            json!(false),
            json!(false),
            json!(8),
        ],
    );
    rig.debug(DebugRequest::ProvideConfigurations {
        debug_type: "debugpy".into(),
    });
    assert_eq!(rig.calls_of("$provideDebugConfigurations")[0][0], json!(8));
}

#[test]
fn fr_l31_42_extension_host_calls_without_behaviour_are_counted_not_answered_wrong() {
    let mut rig = Rig::fixture();
    let req = rig.ext(
        "$registerDebugVisualizer",
        vec![json!("ext.id"), json!("viz")],
    );
    assert!(matches!(rig.response(req), Ok(Reply::Empty)));
    assert_eq!(
        rig.core
            .unimplemented_calls()
            .get(&(PROXY.to_owned(), "$registerDebugVisualizer".to_owned())),
        Some(&1)
    );
}
