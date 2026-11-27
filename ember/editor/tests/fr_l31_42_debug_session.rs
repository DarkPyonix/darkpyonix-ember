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
    OnTaskErrors, TaskChoice, TaskOutcome,
};
use ember_editor::{SettingsLayer, SettingsScopes};
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
    /// The `launch` or `attach` request of each adapter handle, answered at `configurationDone`.
    held_launch: HashMap<i64, Value>,
    armed: bool,
    swallow_disconnect: bool,
    /// Extra capabilities the `initialize` response offers.
    caps: Value,
    /// The `source` of the top frame, as the extension host hands it on; `main.py` by its path
    /// when `None`.
    frame_source: Option<Value>,
}

impl Adapter {
    fn answer(&mut self, handle: i64, req: &Value) -> Vec<Value> {
        let args = &req["arguments"];
        match req["command"].as_str().unwrap_or("") {
            "initialize" => {
                let mut caps = json!({
                    "supportsConfigurationDoneRequest": true,
                    "supportsFunctionBreakpoints": true,
                    "supportTerminateDebuggee": true,
                    "exceptionBreakpointFilters": [{"filter": "raised", "label": "Raised Exceptions", "default": false}],
                });
                if let Some(extra) = self.caps.as_object() {
                    for (k, v) in extra {
                        caps[k] = v.clone();
                    }
                }
                vec![resp(req, caps)]
            }
            "setVariable" | "setExpression" => vec![resp(
                req,
                json!({"value": args["value"], "type": "int", "variablesReference": 0}),
            )],
            "launch" | "attach" => {
                self.held_launch.insert(handle, req.clone());
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
                if let Some(launch) = self.held_launch.remove(&handle) {
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
            "stackTrace" => {
                let source = self
                    .frame_source
                    .clone()
                    .unwrap_or_else(|| json!({"name": "main.py", "path": "/w/main.py"}));
                vec![resp(
                    req,
                    json!({"stackFrames": [{"id": 10, "name": "<module>", "source": source, "line": 2, "column": 1}], "totalFrames": 1}),
                )]
            }
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
    /// What `$provideDebugConfigurations` answers.
    provided: Value,
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
            provided: Value::Null,
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
                                    for answer in self.adapter.answer(handle, &message) {
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
                            "$provideDebugConfigurations" => {
                                if let Some(t) = token {
                                    queue.push_back(Input::RpcReply {
                                        token: t,
                                        result: Ok(self.provided.clone()),
                                    });
                                }
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
            no_debug: false,
        });
    }

    /// The task runs the session layer asked for: (label, the start waiting for it).
    fn task_runs(&self) -> Vec<(String, Option<u64>)> {
        self.ui
            .iter()
            .filter_map(|u| match u {
                DebugUpdate::RunTask { label, start } => Some((label.clone(), *start)),
                _ => None,
            })
            .collect()
    }

    /// The question a failed `preLaunchTask` asks, if one was asked.
    fn task_question(&self) -> Option<(u64, String, Vec<TaskChoice>, bool)> {
        self.ui.iter().rev().find_map(|u| match u {
            DebugUpdate::TaskFailed {
                start,
                message,
                choices,
                error,
            } => Some((*start, message.clone(), choices.clone(), *error)),
            _ => None,
        })
    }

    /// Start a configuration whose `preLaunchTask` is `build` and whose `postDebugTask` is
    /// `cleanup`; returns the start id the session layer gave the task run.
    fn start_with_tasks(&mut self) -> u64 {
        self.debug(DebugRequest::Start {
            selector: ConfigSelector::Config(json!({
                "name": "Build and run",
                "type": "debugpy",
                "request": "launch",
                "program": "${file}",
                "preLaunchTask": "build",
                "postDebugTask": "cleanup",
            })),
            no_debug: false,
        });
        let runs = self.task_runs();
        assert_eq!(runs.len(), 1, "{runs:?}");
        assert_eq!(runs[0].0, "build");
        runs[0].1.expect("the start waits for its preLaunchTask")
    }

    fn task_done(&mut self, start: u64, outcome: TaskOutcome) {
        self.debug(DebugRequest::TaskDone { start, outcome });
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
        no_debug: false,
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
        no_debug: false,
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

// #1151: debugpy reports `source.path` as a plain absolute path. The OSE's extension host turns
// it into a URI before it reaches the window (`convertToVSCPaths(message, true)`), and its URI
// transformer gives that the editor's `vscode-remote` scheme and authority, so the frame's file
// arrives as a marshalled URI object. The open document is the workspace's remote URI. The stop
// marks that document, and opening the stopped file is opening the same document again.
#[test]
fn fr_l31_42_a_frame_given_by_path_marks_and_opens_the_remote_document() {
    let main = uri("/w/main.py");
    let sources = [
        // What the OSE hands on for debugpy's `"path": "/w/main.py"`.
        json!({"name": "main.py", "path": {"$mid": 1, "scheme": "vscode-remote", "authority": AUTH, "path": "/w/main.py", "external": format!("vscode-remote://{AUTH}/w/main.py")}}),
        // A host without the URI transformer: the server's own `file` URI.
        json!({"name": "main.py", "path": {"$mid": 1, "scheme": "file", "authority": "", "path": "/w/main.py"}}),
        // The plain path, spelled the way an adapter may spell it.
        json!({"name": "main.py", "path": "/w/./main.py"}),
        json!({"name": "main.py", "path": "/w//main.py"}),
        json!({"name": "main.py", "path": format!("vscode-remote://{AUTH}/w/main.py")}),
    ];
    for source in sources {
        let mut rig = Rig::fixture();
        rig.adapter.frame_source = Some(source.clone());
        let mut bridge = WidgetBridge::new();
        rig.toggle(2);
        rig.start_python();
        let id = rig.session_id();
        bridge_takes(&mut bridge, &mut rig);
        let view = bridge.debug();
        assert_eq!(view.stopped_line(&main), Some(2), "{source}");
        let at = view.stopped_at.clone().unwrap_or_else(|| panic!("no stop for {source}"));
        assert_eq!(at.uri, main, "the frame's file is the open document's URI: {source}");
        assert_eq!(view.focused_location().map(|f| f.uri), Some(main.clone()), "{source}");

        // Opening the stopped file finds the open document: nothing is read from the server again.
        let fx = rig.core.handle(rig.now, Input::Open { uri: at.uri.clone() });
        assert!(
            !fx.iter().any(|e| matches!(e, Effect::ReadFile { .. })),
            "the stopped file opened a second document for {source}: {fx:?}"
        );

        // A lower frame given the same way is the focused frame, marked in the same document.
        let frames = vec![
            ember_editor::debugger::FrameInfo {
                id: 10,
                name: "greet".into(),
                uri: Some(at.uri.clone()),
                source_name: Some("main.py".into()),
                line: 2,
                column: 1,
                hint: None,
            },
            ember_editor::debugger::FrameInfo {
                id: 11,
                name: "<module>".into(),
                uri: ember_editor::debugger::source::source_uri(&source, Some(&uri("/w"))),
                source_name: Some("main.py".into()),
                line: 3,
                column: 1,
                hint: None,
            },
        ];
        let mut sessions = bridge.debug().sessions.clone();
        sessions[0].frame_id = Some(11);
        bridge.apply(SessionUpdate::Debug(DebugUpdate::Sessions { sessions, active: Some(id.clone()) }));
        bridge.apply(SessionUpdate::Debug(DebugUpdate::CallStack {
            session: id.clone(),
            thread_id: 1,
            frames,
            total_frames: Some(2),
        }));
        let view = bridge.debug();
        assert_eq!(view.focused_frame().map(|f| (f.frame_id, f.line)), Some((11, 3)), "{source}");
        assert_eq!(view.focused_line(&main), Some(3), "{source}");
        assert_eq!(view.stopped_line(&main), Some(2), "{source}");
    }
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
    rig.provided = json!([{"name": "Provided", "type": "debugpy", "request": "launch"}]);
    rig.calls.clear();
    rig.debug(DebugRequest::ProvideConfigurations {
        debug_type: "debugpy".into(),
    });
    assert_eq!(rig.calls_of("$provideDebugConfigurations")[0][0], json!(8));
    // `activateDebuggers('onDebugInitialConfigurations')` runs before the providers are asked.
    let first_provide = rig
        .methods()
        .iter()
        .position(|m| *m == "$provideDebugConfigurations")
        .unwrap();
    let activations: Vec<Value> = rig.calls[..first_provide]
        .iter()
        .filter(|(m, _)| m == "$activateByEvent")
        .map(|(_, a)| a[0].clone())
        .collect();
    assert_eq!(
        activations,
        [json!("onDebugInitialConfigurations"), json!("onDebug")]
    );
    let provided: Vec<Vec<Value>> = rig
        .ui
        .iter()
        .filter_map(|u| match u {
            DebugUpdate::ProvidedConfigurations {
                debug_type,
                configurations,
            } if debug_type == "debugpy" => Some(configurations.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(provided.len(), 1, "one answer for all the providers");
    assert_eq!(provided[0][0]["name"], json!("Provided"));
}

#[test]
fn fr_l31_42_a_type_without_providers_is_answered_with_no_configurations() {
    let mut rig = Rig::fixture();
    rig.debug(DebugRequest::ProvideConfigurations {
        debug_type: "node".into(),
    });
    assert!(rig.calls_of("$provideDebugConfigurations").is_empty());
    assert!(rig.ui.iter().any(|u| matches!(
        u,
        DebugUpdate::ProvidedConfigurations { debug_type, configurations }
            if debug_type == "node" && configurations.is_empty()
    )));
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

// The preLaunchTask runs where `debugService.createSession` runs it: after the providers and the
// substitution, before the second chain of providers and the adapter. The session starts only
// once the task has ended well.
#[test]
fn fr_l31_42_pre_launch_task_runs_before_the_adapter_and_a_clean_exit_starts_it() {
    let mut rig = Rig::fixture();
    let start = rig.start_with_tasks();
    let names = rig.methods();
    assert!(names.contains(&"$resolveDebugConfiguration"), "{names:?}");
    assert!(
        !names.contains(&"$resolveDebugConfigurationWithSubstitutedVariables"),
        "the second chain waits for the task: {names:?}"
    );
    assert!(!names.contains(&"$startDASession"));
    assert!(rig.sessions().is_empty());

    rig.task_done(start, TaskOutcome::Exited { code: Some(0), errors: 3 });
    let names = rig.methods();
    let second = names
        .iter()
        .position(|m| *m == "$resolveDebugConfigurationWithSubstitutedVariables")
        .expect("the second chain ran");
    let adapter = names.iter().position(|m| *m == "$startDASession").expect("the adapter started");
    assert!(second < adapter, "{names:?}");
    assert_eq!(rig.task_question(), None, "exit code 0 is a success whatever the problems say");
    assert_eq!(rig.sessions().len(), 1);
    assert_eq!(rig.dap_request("launch")["arguments"]["preLaunchTask"], "build");

    // A second answer for the same start does nothing.
    rig.calls.clear();
    rig.task_done(start, TaskOutcome::Exited { code: Some(0), errors: 0 });
    assert!(!rig.methods().contains(&"$startDASession"));
}

// A task that ends badly asks Code-OSS's question (Debug Anyway, Show Errors, Abort) with
// Code-OSS's message; Debug Anyway starts the session and the other answers do not.
#[test]
fn fr_l31_42_a_failed_pre_launch_task_asks_whether_to_debug_anyway() {
    let mut rig = Rig::fixture();
    let start = rig.start_with_tasks();
    rig.task_done(start, TaskOutcome::Exited { code: Some(2), errors: 0 });
    let (asked, message, choices, error) = rig.task_question().expect("a question");
    assert_eq!(asked, start);
    assert_eq!(message, "The preLaunchTask 'build' terminated with exit code 2.");
    assert_eq!(
        choices,
        vec![TaskChoice::DebugAnyway, TaskChoice::ShowErrors, TaskChoice::Abort]
    );
    assert!(!error, "a warning, not the error dialog");
    assert!(!rig.methods().contains(&"$startDASession"));
    rig.debug(DebugRequest::AnswerTask { start, choice: TaskChoice::DebugAnyway });
    assert!(rig.methods().contains(&"$startDASession"));
    assert_eq!(rig.sessions().len(), 1);

    for choice in [TaskChoice::ShowErrors, TaskChoice::Abort] {
        let mut rig = Rig::fixture();
        let start = rig.start_with_tasks();
        rig.task_done(start, TaskOutcome::Exited { code: Some(1), errors: 0 });
        rig.debug(DebugRequest::AnswerTask { start, choice });
        assert!(!rig.methods().contains(&"$startDASession"), "{choice:?}");
        // The start is gone: a late Debug Anyway starts nothing.
        rig.debug(DebugRequest::AnswerTask { start, choice: TaskChoice::DebugAnyway });
        assert!(!rig.methods().contains(&"$startDASession"), "{choice:?}");
        assert!(rig.sessions().is_empty());
    }
}

// The messages and buttons of `runTaskAndCheckErrors` for each way a task can end.
#[test]
fn fr_l31_42_pre_launch_task_outcomes_follow_code_oss() {
    let question = |outcome: TaskOutcome| {
        let mut rig = Rig::fixture();
        let start = rig.start_with_tasks();
        rig.task_done(start, outcome);
        let started = rig.methods().contains(&"$startDASession");
        let failed = rig.ui.iter().any(|u| matches!(u, DebugUpdate::StartFailed { .. }));
        (rig.task_question().map(|(_, m, c, e)| (m, c, e)), started, failed)
    };
    // Problems the matcher reported.
    let (asked, started, _) = question(TaskOutcome::Exited { code: Some(1), errors: 2 });
    assert_eq!(asked.unwrap().0, "Errors exist after running preLaunchTask 'build'.");
    assert!(!started);
    let (asked, _, _) = question(TaskOutcome::Exited { code: Some(1), errors: 1 });
    assert_eq!(asked.unwrap().0, "Error exists after running preLaunchTask 'build'.");
    // A background task that went inactive: only its problems count.
    let (asked, started, _) = question(TaskOutcome::Ready { errors: 0 });
    assert_eq!(asked, None);
    assert!(started);
    let (asked, started, _) = question(TaskOutcome::Ready { errors: 2 });
    assert_eq!(asked.unwrap().0, "Errors exist after running preLaunchTask 'build'.");
    assert!(!started);
    // A task that could not run: the error dialog, with Configure Task and Cancel.
    let (asked, started, _) = question(TaskOutcome::Failed {
        message: "Could not find the task 'build'.".into(),
    });
    assert_eq!(
        asked,
        Some((
            "Could not find the task 'build'.".to_owned(),
            vec![TaskChoice::DebugAnyway, TaskChoice::ConfigureTask, TaskChoice::Cancel],
            true
        ))
    );
    assert!(!started);
    // A task stopped without an exit code was cancelled: no question, no session, no error.
    let (asked, started, failed) = question(TaskOutcome::Exited { code: None, errors: 0 });
    assert_eq!(asked, None);
    assert!(!started);
    assert!(!failed);
}

// `$startDebugging` of an extension waits for the task and hears `false` when the start is
// abandoned, as `createSession` returns false on a failed task.
#[test]
fn fr_l31_42_an_abandoned_pre_launch_task_answers_the_extension_false() {
    let mut rig = Rig::fixture();
    let config = json!({"type": "debugpy", "request": "launch", "name": "ext", "program": "/w/a.py", "preLaunchTask": "build"});
    let req = rig.ext("$startDebugging", vec![Value::Null, config, json!({})]);
    let start = rig.task_runs()[0].1.unwrap();
    assert!(!rig.responses.iter().any(|(r, _)| *r == req), "it waits for the task");
    rig.task_done(start, TaskOutcome::Exited { code: Some(1), errors: 0 });
    rig.debug(DebugRequest::AnswerTask { start, choice: TaskChoice::Abort });
    assert!(
        matches!(rig.response(req), Ok(Reply::Json(Value::Bool(false)))),
        "{:?}",
        rig.response(req)
    );
}

// `postDebugTask` runs when the session ends, and nothing waits for it.
#[test]
fn fr_l31_42_post_debug_task_runs_when_the_session_ends() {
    let mut rig = Rig::fixture();
    let start = rig.start_with_tasks();
    rig.task_done(start, TaskOutcome::Exited { code: Some(0), errors: 0 });
    assert_eq!(rig.task_runs().len(), 1, "only the preLaunchTask so far");
    let id = rig.session_id();
    rig.debug(DebugRequest::Stop { session: Some(id) });
    let runs = rig.task_runs();
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[1], ("cleanup".to_owned(), None));
    assert!(rig.sessions().iter().all(|s| s.state == SessionState::Terminated));
}

// Run Without Debugging (Ctrl+F5) starts the configuration with `noDebug: true`, and the session
// gets no breakpoints, at the start or later, as `debugService.sendBreakpoints` skips it.
#[test]
fn fr_l31_42_run_without_debugging_sends_no_debug_and_no_breakpoints() {
    let mut rig = Rig::fixture();
    rig.toggle(2);
    rig.debug(DebugRequest::Start {
        selector: ConfigSelector::Name("Python: Current File".into()),
        no_debug: true,
    });
    let resolved = rig.calls_of("$resolveDebugConfiguration")[0].clone();
    assert_eq!(resolved[2]["noDebug"], true, "the providers see it");
    assert_eq!(rig.dap_request("launch")["arguments"]["noDebug"], true);
    assert_eq!(
        rig.dap_commands()[..3].to_vec(),
        vec!["initialize", "launch", "configurationDone"],
        "{:?}",
        rig.dap_commands()
    );
    assert!(!rig.dap_commands().iter().any(|c| c.starts_with("set")));
    let session = rig.sessions()[0].clone();
    assert!(session.no_debug);
    assert!(
        !matches!(session.state, SessionState::Stopped { .. }),
        "the breakpoint on line 2 was never sent"
    );
    rig.dap.clear();
    rig.toggle(3);
    assert!(rig.dap.is_empty(), "{:?}", rig.dap);

    // A child the adapter starts without saying runs as its parent does.
    let handle = rig.calls_of("$startDASession")[0][0].as_i64().unwrap();
    rig.adapter_says(
        handle,
        json!({"type": "request", "seq": 50, "command": "startDebugging", "arguments": {"request": "launch", "configuration": {"name": "child"}}}),
    );
    let child = rig.calls_of("$startDASession").last().unwrap()[1].clone();
    assert_eq!(child["configuration"]["noDebug"], true);
}

// setVariable from the Variables view: DAP `setVariable` when the adapter offers it, else
// `setExpression` on the variable's `evaluateName`, else a refusal; the answer reaches the window.
#[test]
fn fr_l31_42_set_variable_follows_the_adapter_capabilities() {
    let stopped_rig = |caps: Value| {
        let mut rig = Rig::fixture();
        rig.adapter.caps = caps;
        rig.toggle(2);
        rig.start_python();
        assert!(matches!(rig.sessions()[0].state, SessionState::Stopped { .. }));
        rig
    };
    let set = |rig: &mut Rig| {
        let session = rig.session_id();
        rig.debug(DebugRequest::SetVariable {
            session,
            reference: 100,
            name: "x".into(),
            value: "5".into(),
            evaluate_name: Some("x".into()),
            frame_id: Some(10),
        });
        rig.ui.iter().rev().find_map(|u| match u {
            DebugUpdate::VariableSet { reference, name, result, .. } => {
                Some((*reference, name.clone(), result.clone()))
            }
            _ => None,
        })
    };

    let mut rig = stopped_rig(json!({"supportsSetVariable": true}));
    assert!(rig.sessions()[0].can_set_variables);
    assert_eq!(set(&mut rig), Some((100, "x".to_owned(), Ok("5".to_owned()))));
    let request = rig.dap_request("setVariable");
    assert_eq!(
        request["arguments"],
        json!({"variablesReference": 100, "name": "x", "value": "5"})
    );

    let mut rig = stopped_rig(json!({"supportsSetExpression": true}));
    assert!(rig.sessions()[0].can_set_variables);
    assert_eq!(set(&mut rig), Some((100, "x".to_owned(), Ok("5".to_owned()))));
    assert_eq!(
        rig.dap_request("setExpression")["arguments"],
        json!({"expression": "x", "value": "5", "frameId": 10})
    );

    let mut rig = stopped_rig(json!({}));
    assert!(!rig.sessions()[0].can_set_variables);
    let (_, _, result) = set(&mut rig).unwrap();
    assert!(result.is_err());
    let commands = rig.dap_commands();
    assert!(
        !commands.iter().any(|c| c == "setVariable" || c == "setExpression"),
        "{commands:?}"
    );
}

/// A rig whose user settings say `"debug.onTaskErrors": <value>`.
fn rig_with_on_task_errors(value: &str) -> Rig {
    let mut rig = Rig::fixture();
    rig.core.set_settings(SettingsScopes {
        user: SettingsLayer::from_values(vec![("debug.onTaskErrors".to_owned(), json!(value))]),
        ..Default::default()
    });
    rig
}

// `debug.onTaskErrors` decides before the question, as `runTaskAndCheckErrors` reads it:
// `debugAnyway` starts the session, `showErrors` opens the problems view and abandons the start,
// `abort` abandons it, and `prompt` (the default) asks.
#[test]
fn fr_l31_42_on_task_errors_decides_before_the_question() {
    let outcome = |value: &str| {
        let mut rig = rig_with_on_task_errors(value);
        let start = rig.start_with_tasks();
        rig.task_done(start, TaskOutcome::Exited { code: Some(1), errors: 0 });
        let started = rig.methods().contains(&"$startDASession");
        let shown = rig.ui.contains(&DebugUpdate::ShowProblems);
        (rig.task_question().is_some(), started, shown)
    };
    assert_eq!(outcome("debugAnyway"), (false, true, false));
    assert_eq!(outcome("showErrors"), (false, false, true));
    assert_eq!(outcome("abort"), (false, false, false));
    assert_eq!(outcome("prompt"), (true, false, false));
    assert_eq!(outcome("anything else"), (true, false, false), "an unknown value is the default");

    // A clean exit starts whatever the setting says; a task that could not run always gets the
    // error dialog, which the setting does not cover.
    let mut rig = rig_with_on_task_errors("abort");
    let start = rig.start_with_tasks();
    rig.task_done(start, TaskOutcome::Exited { code: Some(0), errors: 0 });
    assert!(rig.methods().contains(&"$startDASession"));
    let mut rig = rig_with_on_task_errors("debugAnyway");
    let start = rig.start_with_tasks();
    rig.task_done(start, TaskOutcome::Failed { message: "The task 'build' could not run.".into() });
    assert!(rig.task_question().is_some_and(|(_, _, _, error)| error));

    assert_eq!(OnTaskErrors::from_setting(None), OnTaskErrors::Prompt);
    for v in [OnTaskErrors::DebugAnyway, OnTaskErrors::ShowErrors, OnTaskErrors::Prompt, OnTaskErrors::Abort] {
        assert_eq!(OnTaskErrors::from_setting(Some(&json!(v.as_str()))), v);
    }
}

// "Remember my choice in user settings" writes what Code-OSS writes for each button.
#[test]
fn fr_l31_42_remember_my_choice_maps_the_buttons_to_the_setting() {
    assert_eq!(OnTaskErrors::remembered(TaskChoice::DebugAnyway).as_str(), "debugAnyway");
    assert_eq!(OnTaskErrors::remembered(TaskChoice::Abort).as_str(), "abort");
    assert_eq!(OnTaskErrors::remembered(TaskChoice::ShowErrors).as_str(), "showErrors");
}

fn error_marker(line: u32) -> Value {
    json!({ "severity": 8, "message": "broken", "startLineNumber": line, "startColumn": 1, "endLineNumber": line, "endColumn": 2 })
}

// The errors of a `preLaunchTask` are the error markers the problems hold (up to two), what its
// problem matchers reported among them, as `markerService.read({ severities: Error, take: 2 })`.
#[test]
fn fr_l31_42_the_error_markers_of_the_problems_count_for_the_pre_launch_task() {
    let mut rig = Rig::fixture();
    let start = rig.start_with_tasks();
    let markers = serde_json::from_value(json!([error_marker(1)])).unwrap();
    rig.feed(Input::TaskMarkers { owner: "go".into(), entries: vec![(uri("/w/main.go"), Some(markers))] });
    rig.task_done(start, TaskOutcome::Exited { code: Some(1), errors: 0 });
    let (_, message, _, _) = rig.task_question().expect("a question");
    assert_eq!(message, "Error exists after running preLaunchTask 'build'.");

    // A background task that reached its ready point with errors in the problems.
    let mut rig = Rig::fixture();
    let start = rig.start_with_tasks();
    let markers = serde_json::from_value(json!([error_marker(1), error_marker(2), error_marker(3)])).unwrap();
    rig.feed(Input::TaskMarkers { owner: "watch".into(), entries: vec![(uri("/w/a.ts"), Some(markers))] });
    rig.task_done(start, TaskOutcome::Ready { errors: 0 });
    let (_, message, choices, error) = rig.task_question().expect("a question");
    assert_eq!(message, "Errors exist after running preLaunchTask 'build'.");
    assert_eq!(choices, vec![TaskChoice::DebugAnyway, TaskChoice::ShowErrors, TaskChoice::Abort]);
    assert!(!error);

    // Removed again (`None`), they no longer count.
    let mut rig = Rig::fixture();
    let start = rig.start_with_tasks();
    let markers = serde_json::from_value(json!([error_marker(1)])).unwrap();
    rig.feed(Input::TaskMarkers { owner: "go".into(), entries: vec![(uri("/w/main.go"), Some(markers))] });
    rig.feed(Input::TaskMarkers { owner: "go".into(), entries: vec![(uri("/w/main.go"), None)] });
    rig.task_done(start, TaskOutcome::Ready { errors: 0 });
    assert_eq!(rig.task_question(), None);
    assert!(rig.methods().contains(&"$startDASession"));
}

/// The answer to hover `id`, once it came.
fn hover_answer(rig: &Rig, id: u64) -> Option<Result<EvalResult, String>> {
    rig.ui.iter().rev().find_map(|u| match u {
        DebugUpdate::Evaluated { id: got, result, .. } if *got == id => Some(result.clone()),
        _ => None,
    })
}

fn hover(rig: &mut Rig, id: u64, expression: &str) {
    let session = rig.session_id();
    rig.debug(DebugRequest::Hover {
        id,
        session,
        frame_id: 10,
        uri: uri("/w/main.py"),
        line: 2,
        column: 5,
        expression: expression.into(),
    });
}

/// SPEC FR-L31.42, Code-OSS `DebugHoverComputer.evaluate`: an adapter with
/// `supportsEvaluateForHovers` evaluates the hovered expression in the `hover` context, with the
/// hovered place.
#[test]
fn fr_l31_42_debug_hover_evaluates_in_the_hover_context_when_the_adapter_can() {
    let mut rig = Rig::fixture();
    rig.adapter.caps = json!({"supportsEvaluateForHovers": true});
    rig.toggle(2);
    rig.start_python();
    hover(&mut rig, 41, "x");
    assert_eq!(
        rig.dap_request("evaluate")["arguments"],
        json!({
            "expression": "x",
            "frameId": 10,
            "context": "hover",
            "line": 2,
            "column": 5,
            "source": {"name": "main.py", "path": "/w/main.py"},
        })
    );
    assert_eq!(
        hover_answer(&rig, 41),
        Some(Ok(EvalResult {
            result: "42".into(),
            type_name: Some("int".into()),
            variables_reference: 0
        }))
    );
}

/// SPEC FR-L31.42, Code-OSS `findExpressionInStackFrame`: without `supportsEvaluateForHovers` the
/// expression is looked up by name in the frame's scopes, and a name that is not there shows
/// nothing (an error, so the window shows the language hover).
#[test]
fn fr_l31_42_debug_hover_without_evaluate_for_hovers_looks_the_name_up_in_the_scopes() {
    let mut rig = Rig::fixture();
    rig.toggle(2);
    rig.start_python();
    let before = rig.dap.len();
    hover(&mut rig, 42, "x");
    let asked: Vec<String> = rig.dap[before..]
        .iter()
        .map(|m| m["command"].as_str().unwrap_or("").to_owned())
        .collect();
    assert_eq!(asked, ["scopes", "variables"], "no evaluate request");
    assert_eq!(rig.dap_request("scopes")["arguments"], json!({"frameId": 10}));
    assert_eq!(
        hover_answer(&rig, 42),
        Some(Ok(EvalResult {
            result: "41".into(),
            type_name: Some("int".into()),
            variables_reference: 0
        }))
    );
    // The lookup's variables are not the Variables view's.
    assert!(!rig.ui.iter().any(|u| matches!(u, DebugUpdate::Variables { .. })));

    hover(&mut rig, 43, "nothere");
    assert!(matches!(hover_answer(&rig, 43), Some(Err(_))));
    hover(&mut rig, 44, "x.y");
    assert!(
        matches!(hover_answer(&rig, 44), Some(Err(_))),
        "x has no children to look y up in"
    );
}

/// SPEC FR-L31.42, Code-OSS `DebugStorage`: the breakpoints a window kept come back into the
/// registry in the workspace's files, the extension host hears of them, and a session gets them.
#[test]
fn fr_l31_42_restored_breakpoints_join_the_registry_and_reach_the_adapter() {
    use ember_editor::debugger::storage::{StoredBreakpoint, StoredBreakpoints, StoredFunctionBreakpoint};
    let mut rig = Rig::fixture();
    let stored = StoredBreakpoints {
        breakpoints: vec![StoredBreakpoint {
            path: "/w/main.py".into(),
            line_number: 2,
            column: None,
            enabled: true,
            condition: Some("x > 1".into()),
            hit_condition: None,
            log_message: None,
        }],
        function_breakpoints: vec![StoredFunctionBreakpoint {
            name: "main".into(),
            enabled: true,
            condition: None,
            hit_condition: None,
            log_message: None,
        }],
        exception_breakpoints: Vec::new(),
    };
    rig.debug(DebugRequest::RestoreBreakpoints(stored.clone()));
    let source = rig
        .ui
        .iter()
        .rev()
        .find_map(|u| match u {
            DebugUpdate::Breakpoints { source, .. } => Some(source.clone()),
            _ => None,
        })
        .expect("the registry is reported");
    assert_eq!(source.len(), 1);
    assert_eq!(source[0].uri, uri("/w/main.py"), "the workspace's authority");
    assert_eq!(source[0].condition.as_deref(), Some("x > 1"));
    assert_eq!(rig.calls_of("$acceptBreakpointsDelta").len(), 1);

    // Restoring the same again changes nothing.
    rig.calls.clear();
    rig.debug(DebugRequest::RestoreBreakpoints(stored));
    assert!(rig.calls_of("$acceptBreakpointsDelta").is_empty());

    rig.start_python();
    let set = rig.dap_request("setBreakpoints");
    assert_eq!(set["arguments"]["lines"], json!([2]));
    assert_eq!(set["arguments"]["breakpoints"][0]["condition"], "x > 1");
    assert_eq!(
        rig.dap_request("setFunctionBreakpoints")["arguments"]["breakpoints"][0]["name"],
        "main"
    );
    assert!(matches!(rig.sessions()[0].state, SessionState::Stopped { .. }));
}

// ---- compounds and restarts (`debugService.startDebugging`, `restartSession`) -------------------

const COMPOUND_LAUNCH_JSON: &str = r#"{
    "configurations": [
        { "name": "A", "type": "debugpy", "request": "launch", "program": "/w/a.py", "preLaunchTask": "buildA" },
        { "name": "B", "type": "debugpy", "request": "launch", "program": "/w/b.py" },
        { "name": "C", "type": "debugpy", "request": "launch", "program": "/w/c.py" },
    ],
    "compounds": [
        { "name": "Both", "configurations": ["A", "B"], "preLaunchTask": "prep" },
        { "name": "Tied", "configurations": ["B", "C"], "stopAll": true },
        { "name": "Loose", "configurations": ["B", "C"] },
        { "name": "Broken", "configurations": ["B", "Nope"] },
    ],
}"#;

fn compound_rig() -> Rig {
    let mut rig = Rig::fixture();
    rig.files
        .insert("/w/.vscode/launch.json".into(), COMPOUND_LAUNCH_JSON.into());
    rig
}

fn start_named(rig: &mut Rig, name: &str) {
    rig.debug(DebugRequest::Start {
        selector: ConfigSelector::Name(name.into()),
        no_debug: false,
    });
}

fn live_sessions(rig: &Rig) -> Vec<SessionInfo> {
    rig.sessions()
        .into_iter()
        .filter(|s| s.state != SessionState::Terminated)
        .collect()
}

fn disconnects(rig: &Rig) -> usize {
    rig.dap.iter().filter(|m| m["command"] == "disconnect").count()
}

// The compound's own `preLaunchTask` runs once, before any configuration is resolved; then each
// configuration starts with its own `preLaunchTask`.
#[test]
fn fr_l31_42_a_compound_runs_its_pre_launch_task_once_before_its_configurations() {
    let mut rig = compound_rig();
    start_named(&mut rig, "Both");
    let runs = rig.task_runs();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].0, "prep");
    let prep = runs[0].1.expect("the compound waits for its task");
    assert!(
        !rig.methods().contains(&"$resolveDebugConfiguration"),
        "no configuration starts before the compound's task: {:?}",
        rig.methods()
    );

    rig.task_done(prep, TaskOutcome::Exited { code: Some(0), errors: 0 });
    let runs = rig.task_runs();
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[1].0, "buildA", "A runs its own preLaunchTask");
    assert_eq!(rig.calls_of("$startDASession").len(), 1, "B, which has no task, starts");
    let build_a = runs[1].1.unwrap();
    rig.task_done(build_a, TaskOutcome::Exited { code: Some(0), errors: 0 });
    assert_eq!(rig.calls_of("$startDASession").len(), 2);
    assert_eq!(live_sessions(&rig).len(), 2);
    assert_eq!(
        rig.task_runs().iter().filter(|(l, _)| l == "prep").count(),
        1,
        "the compound's task ran once"
    );
}

// A compound's task that ends badly asks `runTaskAndCheckErrors`'s question; an abort ends the
// whole start, and an extension's `$startDebugging` hears `false`.
#[test]
fn fr_l31_42_a_failed_compound_pre_launch_task_starts_none_of_its_configurations() {
    let mut rig = compound_rig();
    let req = rig.ext("$startDebugging", vec![Value::Null, json!("Both"), json!({})]);
    let prep = rig.task_runs()[0].1.unwrap();
    rig.task_done(prep, TaskOutcome::Exited { code: Some(1), errors: 0 });
    let (asked, message, choices, error) = rig.task_question().expect("a question");
    assert_eq!(asked, prep);
    assert_eq!(message, "The preLaunchTask 'prep' terminated with exit code 1.");
    assert_eq!(choices, vec![TaskChoice::DebugAnyway, TaskChoice::ShowErrors, TaskChoice::Abort]);
    assert!(!error);
    rig.debug(DebugRequest::AnswerTask { start: prep, choice: TaskChoice::Abort });
    assert!(!rig.methods().contains(&"$resolveDebugConfiguration"));
    assert!(!rig.methods().contains(&"$startDASession"));
    assert!(matches!(rig.response(req), Ok(Reply::Json(Value::Bool(false)))), "{:?}", rig.response(req));

    // Debug Anyway goes on with the configurations.
    let mut rig = compound_rig();
    start_named(&mut rig, "Both");
    let prep = rig.task_runs()[0].1.unwrap();
    rig.task_done(prep, TaskOutcome::Failed { message: "Could not find the task 'prep'.".into() });
    let (_, _, choices, error) = rig.task_question().expect("the error dialog");
    assert!(error);
    assert_eq!(choices, vec![TaskChoice::DebugAnyway, TaskChoice::ConfigureTask, TaskChoice::Cancel]);
    rig.debug(DebugRequest::AnswerTask { start: prep, choice: TaskChoice::DebugAnyway });
    assert_eq!(rig.calls_of("$startDASession").len(), 1, "B started");
    assert_eq!(rig.task_runs().last().unwrap().0, "buildA");

    // A compound naming a configuration that does not exist starts nothing.
    let mut rig = compound_rig();
    start_named(&mut rig, "Broken");
    assert!(rig.ui.iter().any(|u| matches!(u, DebugUpdate::StartFailed { message } if message == "Could not find launch configuration 'Nope' in the workspace.")), "{:?}", rig.ui);
    assert!(!rig.methods().contains(&"$startDASession"));
}

// `stopAll`: stopping one session of the compound stops the others; without it, only that one.
#[test]
fn fr_l31_42_stop_all_stops_the_whole_compound() {
    let mut rig = compound_rig();
    start_named(&mut rig, "Tied");
    let live = live_sessions(&rig);
    assert_eq!(live.len(), 2, "{live:?}");
    rig.debug(DebugRequest::Stop { session: Some(live[0].id.clone()) });
    assert_eq!(disconnects(&rig), 2, "both sessions were asked to disconnect");
    assert!(live_sessions(&rig).is_empty(), "{:?}", rig.sessions());

    let mut rig = compound_rig();
    start_named(&mut rig, "Loose");
    let live = live_sessions(&rig);
    assert_eq!(live.len(), 2);
    rig.debug(DebugRequest::Stop { session: Some(live[0].id.clone()) });
    assert_eq!(disconnects(&rig), 1);
    let left = live_sessions(&rig);
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, live[1].id);

    // An extension's `$stopDebugging` of one session stops the compound too.
    let mut rig = compound_rig();
    start_named(&mut rig, "Tied");
    let live = live_sessions(&rig);
    rig.ext("$stopDebugging", vec![json!(live[1].id)]);
    assert_eq!(disconnects(&rig), 2);
}

/// Start a configuration with every task a restart reads, and let its `preLaunchTask` succeed.
fn start_with_restart_tasks(rig: &mut Rig) {
    rig.debug(DebugRequest::Start {
        selector: ConfigSelector::Config(json!({
            "name": "Restartable",
            "type": "debugpy",
            "request": "launch",
            "program": "/w/main.py",
            "preLaunchTask": "build",
            "postDebugTask": "cleanup",
            "preRestartTask": "prerestart",
            "postRestartTask": "postrestart",
        })),
        no_debug: false,
    });
    let start = rig.task_runs()[0].1.unwrap();
    rig.task_done(start, TaskOutcome::Exited { code: Some(0), errors: 0 });
    assert_eq!(live_sessions(rig).len(), 1);
}

/// Finish every task the session layer is waiting for, one at a time, each with exit code 0.
fn finish_waiting_tasks(rig: &mut Rig, from: usize) {
    let mut done = from;
    loop {
        let runs = rig.task_runs();
        let Some((_, Some(start))) = runs.get(done).cloned() else {
            if done < runs.len() {
                done += 1;
                continue;
            }
            return;
        };
        done += 1;
        rig.task_done(start, TaskOutcome::Exited { code: Some(0), errors: 0 });
    }
}

// Without `supportsRestartRequest` the session stops (`restart: true`), then the restart's tasks
// run in `restartSession`'s order, then the configuration starts again without its
// `preLaunchTask` a second time.
#[test]
fn fr_l31_42_restart_runs_the_restart_tasks_in_code_oss_order_then_starts_again() {
    let mut rig = Rig::fixture();
    start_with_restart_tasks(&mut rig);
    let first = rig.session_id();
    rig.debug(DebugRequest::Restart { session: first });
    assert_eq!(rig.dap_request("disconnect")["arguments"]["restart"], true);
    finish_waiting_tasks(&mut rig, 1);
    let labels: Vec<(String, bool)> = rig
        .task_runs()
        .into_iter()
        .map(|(l, s)| (l, s.is_some()))
        .collect();
    assert_eq!(
        labels,
        vec![
            ("build".to_owned(), true),
            // `onDidEndAdapter`: nothing waits for it.
            ("cleanup".to_owned(), false),
            ("prerestart".to_owned(), true),
            ("cleanup".to_owned(), true),
            ("build".to_owned(), true),
            ("postrestart".to_owned(), true),
        ]
    );
    assert_eq!(rig.calls_of("$startDASession").len(), 2, "started again after the tasks");
    assert_eq!(live_sessions(&rig).len(), 1);
}

// With `supportsRestartRequest` the tasks run first and the adapter is then sent `restart` with
// the configuration; a `postRestartTask` that fails and is aborted keeps the restart from going.
#[test]
fn fr_l31_42_restart_request_waits_for_the_restart_tasks() {
    let mut rig = Rig::fixture();
    rig.adapter.caps = json!({"supportsRestartRequest": true});
    start_with_restart_tasks(&mut rig);
    let first = rig.session_id();
    rig.debug(DebugRequest::Restart { session: first.clone() });
    assert!(!rig.dap_commands().contains(&"restart".to_owned()), "the tasks come first");
    assert!(!rig.dap_commands().contains(&"disconnect".to_owned()));
    assert_eq!(rig.task_runs()[1].0, "prerestart");
    finish_waiting_tasks(&mut rig, 1);
    let restart = rig.dap_request("restart");
    assert_eq!(restart["arguments"]["arguments"]["program"], "/w/main.py");
    let labels: Vec<String> = rig.task_runs().into_iter().map(|(l, _)| l).collect();
    assert_eq!(labels, ["build", "prerestart", "cleanup", "build", "postrestart"]);
    assert_eq!(rig.session_id(), first, "the same session goes on");

    let mut rig = Rig::fixture();
    rig.adapter.caps = json!({"supportsRestartRequest": true});
    start_with_restart_tasks(&mut rig);
    let first = rig.session_id();
    rig.debug(DebugRequest::Restart { session: first });
    for _ in 0..3 {
        let start = rig.task_runs().last().unwrap().1.unwrap();
        rig.task_done(start, TaskOutcome::Exited { code: Some(0), errors: 0 });
    }
    let (start, label) = {
        let runs = rig.task_runs();
        let last = runs.last().unwrap().clone();
        (last.1.unwrap(), last.0)
    };
    assert_eq!(label, "postrestart");
    rig.task_done(start, TaskOutcome::Exited { code: Some(1), errors: 0 });
    let (asked, message, _, _) = rig.task_question().expect("runTaskAndCheckErrors asks");
    assert_eq!(asked, start);
    assert_eq!(message, "The preLaunchTask 'postrestart' terminated with exit code 1.");
    rig.debug(DebugRequest::AnswerTask { start, choice: TaskChoice::Abort });
    assert!(!rig.dap_commands().contains(&"restart".to_owned()));
}

// A `preRestartTask` that cannot run stops the restart with its message; the session goes on.
#[test]
fn fr_l31_42_a_restart_task_that_cannot_run_stops_the_restart() {
    let mut rig = Rig::fixture();
    rig.adapter.caps = json!({"supportsRestartRequest": true});
    start_with_restart_tasks(&mut rig);
    let first = rig.session_id();
    rig.debug(DebugRequest::Restart { session: first });
    let start = rig.task_runs()[1].1.unwrap();
    rig.task_done(start, TaskOutcome::Failed { message: "Could not find the task 'prerestart'.".into() });
    assert!(rig.ui.iter().any(|u| matches!(u, DebugUpdate::StartFailed { message } if message == "Could not find the task 'prerestart'.")));
    assert_eq!(rig.task_runs().len(), 2, "no further task runs");
    assert!(!rig.dap_commands().contains(&"restart".to_owned()));
    assert_eq!(live_sessions(&rig).len(), 1);
}

// A restart the adapter asks for (`terminated` with `restart`) runs no tasks and hands the value
// to the next `launch` as `__restart`.
#[test]
fn fr_l31_42_an_automatic_restart_runs_no_tasks_and_passes_restart_on() {
    let mut rig = Rig::fixture();
    start_with_restart_tasks(&mut rig);
    rig.adapter_says(1, event("terminated", json!({"restart": {"port": 5678}})));
    assert_eq!(rig.calls_of("$startDASession").len(), 2);
    assert_eq!(rig.dap_request("launch")["arguments"]["__restart"], json!({"port": 5678}));
    let labels: Vec<String> = rig.task_runs().into_iter().map(|(l, _)| l).collect();
    assert_eq!(labels, ["build", "cleanup"], "only the ended adapter's postDebugTask");
}
