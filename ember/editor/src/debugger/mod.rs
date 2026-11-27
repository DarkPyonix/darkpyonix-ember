//! Debugging, the session side.
//!
//! Debug adapters come from extensions, through the extension host, as in Code-OSS: the
//! extension host owns the adapter process and relays every Debug Adapter Protocol (DAP) message
//! as an RPC call. [`DebugState`] is the renderer side of that (`mainThreadDebugService.ts` and
//! the parts of `debugService.ts`, `debugSession.ts` and `rawDebugSession.ts` that drive a
//! session). It does no I/O: [`crate::SessionCore`] executes what it returns ([`Out`]), like
//! every other `MainThread*` feature, and it draws nothing: the window reads [`DebugUpdate`]s.
//!
//! **Start**, in the order `debugService.createSession` runs it:
//!
//! 1. the configuration, from `.vscode/launch.json` (by name or index) or given whole;
//! 2. `$activateByEvent` for `onDebugResolve`, `onDebug` and `onDebugResolve:<type>`
//!    (`debugAdapterManager.activateDebuggers`), so the extension registers its providers;
//! 3. `$resolveDebugConfiguration` through the providers of the type, then the `*` providers,
//!    again for a type the result changed to (`debugConfigurationManager.resolveConfigurationByProviders`);
//! 4. the variables Ember can answer are substituted ([`launch::VarContext`]);
//! 5. `preLaunchTask`, through the window's task runner ([`DebugUpdate::RunTask`]); a task that
//!    fails, or leaves error markers in the problems, asks whether to debug anyway
//!    ([`DebugUpdate::TaskFailed`]) unless `debug.onTaskErrors` answers first ([`OnTaskErrors`]),
//!    as `debugTaskRunner.runTaskAndCheckErrors` does;
//! 6. `$resolveDebugConfigurationWithSubstitutedVariables` through the same chain;
//! 7. `$startDASession`: the extension host creates and starts the adapter.
//!
//! `postDebugTask` runs when the session ends, and nothing waits for it. A start with `noDebug`
//! (Run Without Debugging) sends no breakpoints, as `debugService.sendBreakpoints` skips such
//! sessions.
//!
//! **Compounds** (`debugService.startDebugging`): the compound's own `preLaunchTask` runs once,
//! through `runTaskAndCheckErrors`, before any of its configurations starts; a failure ends the
//! start. Each configuration then starts as above, with its own `preLaunchTask`. With `stopAll`
//! the sessions share a compound root: stopping one stops the others (`DebugCompoundRoot`).
//!
//! **Restart** (`debugService.restartSession`): a restart the user asks for runs the
//! configuration's `preRestartTask` and `postDebugTask` (`runTask`), then its `preLaunchTask` and
//! `postRestartTask` (`runTaskAndCheckErrors`), and a failure stops the restart. An adapter with
//! `supportsRestartRequest` runs them before it is sent `restart`; any other session stops first
//! and starts again after them. A restart the adapter asks for (`terminated` with `restart`) runs
//! no tasks and passes the value on as `__restart`.
//!
//! **DAP**, in the order `debugSession.ts` drives it: `initialize`; `launch` or `attach`; on the
//! `initialized` event `setBreakpoints` per file, `setFunctionBreakpoints`,
//! `setExceptionBreakpoints`; once they are answered `configurationDone`, then `threads`. A
//! `stopped` event loads `threads` and the `stackTrace` of its thread, then the `scopes` of the
//! top frame. Variables, evaluation and stepping are requested by the window
//! ([`DebugRequest`]).
//!
//! **What is not done.** The visualizer methods of the actor stay unimplemented (the actor
//! router counts them). There is no terminal in this window, so `runInTerminal` is refused and
//! the adapter has to use its internal console. A restart starts from the configuration as it was
//! written and resolves it again, where Code-OSS reuses the resolved one unless `launch.json`
//! changed. The platform-specific blocks of a configuration (`windows`, `osx`, `linux`) are not
//! merged.

pub mod breakpoints;
pub mod hover;
pub mod launch;
pub mod source;
pub mod storage;

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use ember_editor_conn::debug::{
    self as wire, BreakpointsDelta, DapMessage, SessionDto, SessionRef,
};
use ember_editor_conn::exthost::{self, Call};
use ember_editor_conn::rpc::{js_error, Reply};
use ember_editor_conn::uri::UriComponents;
use serde_json::{json, Map, Value};

use breakpoints::{set_breakpoints_args, set_function_breakpoints_args, Breakpoints, Touched};
pub use breakpoints::{ExceptionFilter, FunctionBreakpoint, SourceBreakpoint};
use launch::{config_name, parse_launch_json, LaunchFile, VarContext, LAUNCH_JSON};

/// How long a `disconnect` may stay unanswered before the session is ended anyway
/// (`rawDebugSession.shutdown` waits 500 ms).
pub const DISCONNECT_TIMEOUT: Duration = Duration::from_millis(500);

// ---- what the window reads --------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum SessionState {
    /// The adapter is starting, or has not answered `launch` yet.
    Initializing,
    Running,
    Stopped {
        reason: String,
        thread_id: Option<i64>,
        description: Option<String>,
        text: Option<String>,
    },
    Terminated,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionInfo {
    pub id: String,
    pub name: String,
    pub debug_type: String,
    pub request: String,
    pub parent: Option<String>,
    pub state: SessionState,
    /// The thread and frame the window shows.
    pub thread_id: Option<i64>,
    pub frame_id: Option<i64>,
    /// The adapter can change a variable's value (`supportsSetVariable`, or `supportsSetExpression`
    /// for a variable with an `evaluateName`), so the Variables view offers to edit one.
    pub can_set_variables: bool,
    /// Run Without Debugging: the configuration has `noDebug`.
    pub no_debug: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadInfo {
    pub id: i64,
    pub name: String,
    pub stopped: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrameInfo {
    pub id: i64,
    pub name: String,
    /// The file of the frame, on the workspace's server ([`source::source_uri`]); `None` for a
    /// source without a path or one the adapter serves itself (`sourceReference`).
    pub uri: Option<UriComponents>,
    pub source_name: Option<String>,
    /// 1-based.
    pub line: u32,
    /// 1-based.
    pub column: u32,
    pub hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScopeInfo {
    pub name: String,
    pub variables_reference: i64,
    pub expensive: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VariableInfo {
    pub name: String,
    pub value: String,
    pub type_name: Option<String>,
    /// Non-zero when the variable has children to request.
    pub variables_reference: i64,
    pub evaluate_name: Option<String>,
    /// `presentationHint.attributes` has `readOnly`: Code-OSS does not offer to edit it.
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EvalResult {
    pub result: String,
    pub type_name: Option<String>,
    pub variables_reference: i64,
}

/// One line of the debug console. `category` is the DAP output category (`console`, `stdout`,
/// `stderr`, `important`); text an extension appends and Ember's own notices are `important`.
#[derive(Debug, Clone, PartialEq)]
pub struct ConsoleEntry {
    pub category: String,
    pub text: String,
    pub uri: Option<UriComponents>,
    pub line: Option<u32>,
}

/// A configuration the window can start.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigInfo {
    pub name: String,
    pub debug_type: String,
    pub request: String,
    /// A compound starts several configurations together.
    pub compound: bool,
}

/// How a task the session layer asked for ended ([`DebugUpdate::RunTask`]): what
/// `debugTaskRunner.runTask` resolves with, and the errors the problems view holds after it.
/// The session counts the error markers it holds itself ([`Env::error_markers`]) and takes the
/// larger of that and `errors`, so the window may send 0.
#[derive(Debug, Clone, PartialEq)]
pub enum TaskOutcome {
    /// The task's process ended. `code` is `None` when it ended without one (stopped by the user
    /// or a signal), which Code-OSS takes as a cancelled task.
    Exited { code: Option<i64>, errors: usize },
    /// A background task went inactive: its problem matcher saw the end of its first pass.
    Ready { errors: usize },
    /// The task could not be found, run or tracked: the message, Code-OSS's error dialog.
    Failed { message: String },
}

/// The setting `debug.onTaskErrors`: what a `preLaunchTask` that ended badly does before any
/// question (`debugTaskRunner.runTaskAndCheckErrors`). A task that could not run is not covered:
/// it always gets the error dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnTaskErrors {
    /// Start the session anyway.
    DebugAnyway,
    /// Open the problems view; the start is abandoned.
    ShowErrors,
    /// Ask (the default).
    #[default]
    Prompt,
    /// Abandon the start.
    Abort,
}

impl OnTaskErrors {
    /// The setting's value; anything else is the default, `prompt`.
    pub fn from_setting(value: Option<&Value>) -> Self {
        match value.and_then(Value::as_str) {
            Some("debugAnyway") => Self::DebugAnyway,
            Some("showErrors") => Self::ShowErrors,
            Some("abort") => Self::Abort,
            _ => Self::Prompt,
        }
    }

    /// The value written to settings.json.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DebugAnyway => "debugAnyway",
            Self::ShowErrors => "showErrors",
            Self::Prompt => "prompt",
            Self::Abort => "abort",
        }
    }

    /// What "Remember my choice in user settings" writes for a button of the warning: Debug
    /// Anyway, Abort, else Show Errors, as Code-OSS maps them.
    pub fn remembered(choice: TaskChoice) -> Self {
        match choice {
            TaskChoice::DebugAnyway => Self::DebugAnyway,
            TaskChoice::Abort | TaskChoice::Cancel => Self::Abort,
            TaskChoice::ShowErrors | TaskChoice::ConfigureTask => Self::ShowErrors,
        }
    }
}

/// The buttons of the question a failed `preLaunchTask` asks ([`DebugUpdate::TaskFailed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskChoice {
    DebugAnyway,
    /// Open the problems view; the start is abandoned.
    ShowErrors,
    /// Open `tasks.json`; the start is abandoned.
    ConfigureTask,
    /// The cancel button of the warning (Code-OSS labels it Abort).
    Abort,
    /// The cancel button of the error dialog (Code-OSS labels it Cancel).
    Cancel,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DebugUpdate {
    /// The configurations of `launch.json`, answering [`DebugRequest::ListConfigurations`].
    Configurations {
        configurations: Vec<ConfigInfo>,
        error: Option<String>,
    },
    /// What the providers of a type offer, all of them together, answering
    /// [`DebugRequest::ProvideConfigurations`]; empty when the type has no provider.
    ProvidedConfigurations {
        debug_type: String,
        configurations: Vec<Value>,
    },
    /// Every session with its state, and the one the window shows. Sent when a session starts,
    /// changes state, changes thread or frame, or ends (the ended one is in the list once).
    Sessions {
        sessions: Vec<SessionInfo>,
        active: Option<String>,
    },
    Threads {
        session: String,
        threads: Vec<ThreadInfo>,
    },
    CallStack {
        session: String,
        thread_id: i64,
        frames: Vec<FrameInfo>,
        total_frames: Option<i64>,
    },
    Scopes {
        session: String,
        frame_id: i64,
        scopes: Vec<ScopeInfo>,
    },
    Variables {
        session: String,
        reference: i64,
        variables: Vec<VariableInfo>,
        error: Option<String>,
    },
    /// The answer to [`DebugRequest::Evaluate`] with the same `id`.
    Evaluated {
        id: u64,
        session: String,
        result: Result<EvalResult, String>,
    },
    Console {
        session: Option<String>,
        entry: ConsoleEntry,
    },
    /// The whole breakpoint registry, with what the adapters said about it.
    Breakpoints {
        source: Vec<SourceBreakpoint>,
        functions: Vec<FunctionBreakpoint>,
        exception_filters: Vec<ExceptionFilter>,
    },
    /// A session could not start: the message to show.
    StartFailed { message: String },
    /// Run the task `label` of `tasks.json` through the window's task runner: a `preLaunchTask`
    /// when `start` is set (answer with [`DebugRequest::TaskDone`]), else a `postDebugTask`,
    /// which nothing waits for (a failure is only shown).
    RunTask { label: String, start: Option<u64> },
    /// The `preLaunchTask` of start `start` failed: ask the question of `debugTaskRunner` with
    /// these buttons, the last one the cancel button. `error` is the error dialog (a task that
    /// could not run), else the warning (a task that ended badly). Answer with
    /// [`DebugRequest::AnswerTask`].
    TaskFailed {
        start: u64,
        message: String,
        choices: Vec<TaskChoice>,
        error: bool,
    },
    /// `debug.onTaskErrors` is `showErrors` and the `preLaunchTask` of a start ended badly: open
    /// the problems view (`viewsService.openView(Markers.MARKERS_VIEW_ID)`); the start is
    /// abandoned.
    ShowProblems,
    /// The answer to [`DebugRequest::SetVariable`]: the new value, or why it was refused. The
    /// window loads the variables again either way, as Code-OSS's `updateViews` does.
    VariableSet {
        session: String,
        reference: i64,
        name: String,
        result: Result<String, String>,
    },
}

// ---- what the window asks ---------------------------------------------------------------------

/// Which configuration to start.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigSelector {
    /// A configuration or compound of `launch.json` by name.
    Name(String),
    /// A configuration of `launch.json` by position.
    Index(usize),
    /// A whole configuration, such as one an extension provided.
    Config(Value),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalContext {
    /// The debug console.
    Repl,
    Watch,
    Hover,
    Clipboard,
}

impl EvalContext {
    fn as_str(self) -> &'static str {
        match self {
            Self::Repl => "repl",
            Self::Watch => "watch",
            Self::Hover => "hover",
            Self::Clipboard => "clipboard",
        }
    }
}

/// A request of the window. Lines are 1-based, as in DAP and in the extension host.
#[derive(Debug, Clone, PartialEq)]
pub enum DebugRequest {
    ListConfigurations,
    /// Configurations the providers of `debug_type` offer.
    ProvideConfigurations {
        debug_type: String,
    },
    /// `no_debug`: Run Without Debugging (Ctrl+F5, `workbench.action.debug.run`), the
    /// configuration starts with `noDebug: true`.
    Start {
        selector: ConfigSelector,
        no_debug: bool,
    },
    /// How the `preLaunchTask` of start `start` ended ([`DebugUpdate::RunTask`]).
    TaskDone {
        start: u64,
        outcome: TaskOutcome,
    },
    /// The button chosen in the question of [`DebugUpdate::TaskFailed`].
    AnswerTask {
        start: u64,
        choice: TaskChoice,
    },
    /// Set the variable `name` of container `reference` to `value` (DAP `setVariable`, or
    /// `setExpression` on `evaluate_name` in frame `frame_id` for an adapter with only that).
    SetVariable {
        session: String,
        reference: i64,
        name: String,
        value: String,
        evaluate_name: Option<String>,
        frame_id: Option<i64>,
    },
    /// Stop a session, or every session with `None`.
    Stop {
        session: Option<String>,
    },
    Restart {
        session: String,
    },
    Continue {
        session: String,
        thread_id: i64,
    },
    Pause {
        session: String,
        thread_id: i64,
    },
    StepOver {
        session: String,
        thread_id: i64,
    },
    StepInto {
        session: String,
        thread_id: i64,
    },
    StepOut {
        session: String,
        thread_id: i64,
    },
    /// Show another thread: its stack is loaded.
    SelectThread {
        session: String,
        thread_id: i64,
    },
    /// Show another frame: its scopes are loaded.
    SelectFrame {
        session: String,
        thread_id: i64,
        frame_id: i64,
    },
    Scopes {
        session: String,
        frame_id: i64,
    },
    Variables {
        session: String,
        reference: i64,
    },
    Evaluate {
        id: u64,
        session: String,
        frame_id: Option<i64>,
        expression: String,
        context: EvalContext,
    },
    ToggleBreakpoint {
        uri: UriComponents,
        line: u32,
    },
    AddBreakpoint {
        uri: UriComponents,
        line: u32,
        column: Option<u32>,
        condition: Option<String>,
        hit_condition: Option<String>,
        log_message: Option<String>,
    },
    /// Replace the condition, hit condition, log message and enabled flag of a breakpoint.
    EditBreakpoint {
        id: String,
        condition: Option<String>,
        hit_condition: Option<String>,
        log_message: Option<String>,
        enabled: bool,
    },
    RemoveBreakpoint {
        id: String,
    },
    AddFunctionBreakpoint {
        name: String,
    },
    SetExceptionFilter {
        filter: String,
        enabled: bool,
    },
    /// The debug hover (`DebugHoverWidget.showAt`): the value of `expression` in frame `frame_id`,
    /// hovered at the 1-based `line` and `column` of `uri` ([`hover::expression_at`]). An adapter
    /// with `supportsEvaluateForHovers` is sent `evaluate` with the context `hover`; for any
    /// other the expression is looked up in the frame's scopes ([`hover::HoverSearch`]). Answered
    /// with [`DebugUpdate::Evaluated`] of the same `id`; an error there means there is nothing to
    /// show, and the window shows the language hover instead.
    Hover {
        id: u64,
        session: String,
        frame_id: i64,
        uri: UriComponents,
        line: u32,
        column: u32,
        expression: String,
    },
    /// Put back the breakpoints a window kept when it last closed ([`storage`], Code-OSS's
    /// `DebugStorage`), in files of the workspace.
    RestoreBreakpoints(storage::StoredBreakpoints),
}

/// What the core knows that a request may need.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Env {
    /// Path of the file in the active editor, for `${file}`.
    pub active_file: Option<String>,
    /// The setting `debug.onTaskErrors`.
    pub on_task_errors: OnTaskErrors,
    /// The error markers the problems hold, counted up to 2, as `runTaskAndCheckErrors` reads
    /// `markerService.read({ severities: Error, take: 2 })`.
    pub error_markers: usize,
}

// ---- what the core executes -------------------------------------------------------------------

/// An extension-host call whose reply [`DebugState::on_reply`] reads.
#[derive(Debug, Clone, PartialEq)]
pub enum Pending {
    Activation {
        start: u64,
    },
    Resolve {
        start: u64,
    },
    StartDa {
        session: String,
        handle: i64,
    },
    /// An `onDebugInitialConfigurations` activation before the providers of `id` are asked.
    ProvideActivation {
        id: u64,
    },
    Provide {
        id: u64,
    },
}

#[derive(Debug, Clone)]
pub enum Out {
    Rpc(Call),
    RpcWith(Call, Pending),
    /// Answer extension-host request `req`.
    Respond {
        req: u32,
        result: Result<Reply, Option<Value>>,
    },
    Ui(DebugUpdate),
    /// Read `.vscode/launch.json`; the answer goes to [`DebugState::on_launch_json`].
    ReadLaunchJson {
        uri: UriComponents,
        start: u64,
    },
}

// ---- state ------------------------------------------------------------------------------------

/// A debug configuration provider an extension registered.
#[derive(Debug, Clone, PartialEq)]
pub struct Provider {
    pub handle: i64,
    pub debug_type: String,
    pub trigger_kind: i64,
    pub has_provide: bool,
    pub has_resolve: bool,
    pub has_resolve2: bool,
}

/// Who is owed the answer to a start.
#[derive(Debug, Clone, Default, PartialEq)]
enum Notify {
    #[default]
    Nobody,
    /// `$startDebugging` of the extension host.
    Ext { req: u32 },
    /// The adapter's `startDebugging` reverse request.
    Dap { handle: i64, request_seq: i64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Reading,
    Activate,
    Activating,
    Resolve,
    /// The `preLaunchTask` runs in the window's task runner.
    Task,
    /// The `preLaunchTask` failed and the window asks whether to debug anyway.
    TaskAsk,
    Adapter,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Purpose {
    List,
    Launch,
}

/// What every start of a session carries.
#[derive(Debug, Clone, Default)]
struct Origin {
    parent: Option<String>,
    folder: Option<UriComponents>,
    notify: Notify,
    no_debug: bool,
    vars: VarContext,
    /// The compound whose `stopAll` ties this session to the others it started
    /// (`DebugCompoundRoot`).
    compound: Option<u64>,
    /// The `preLaunchTask` already ran in a restart's tasks (`restartSession`), so the start does
    /// not run it again.
    skip_pre_launch: bool,
}

#[derive(Debug)]
struct Start {
    id: u64,
    purpose: Purpose,
    selector: Option<ConfigSelector>,
    origin: Origin,
    config: Value,
    unresolved: Value,
    phase: Phase,
    current_type: String,
    seen: Vec<String>,
    waiting: usize,
    todo: VecDeque<i64>,
    /// 0: the providers of the type are next; 1: the `*` providers; 2: both are done.
    group: u8,
    substituted: bool,
}

/// What an outstanding DAP request was for.
#[derive(Debug, Clone)]
enum Dap {
    Initialize,
    Launch,
    SetBreakpoints {
        ids: Vec<String>,
        initial: bool,
    },
    SetFunctionBreakpoints {
        ids: Vec<String>,
        initial: bool,
    },
    SetExceptionBreakpoints {
        initial: bool,
    },
    ConfigurationDone,
    Threads,
    StackTrace {
        thread_id: i64,
        epoch: u64,
    },
    Scopes {
        frame_id: i64,
    },
    Variables {
        reference: i64,
    },
    Evaluate {
        id: u64,
    },
    /// The scopes of a hover's frame, for a [`hover::HoverSearch`].
    HoverScopes {
        id: u64,
    },
    /// The variables a [`hover::HoverSearch`] asked at `depth`.
    HoverVariables {
        id: u64,
        depth: usize,
    },
    /// `setVariable` or `setExpression` of variable `name` in container `reference`.
    SetVariable {
        reference: i64,
        name: String,
    },
    /// `continue`, `next`, `stepIn` or `stepOut` sent while the session was stopped at `epoch`.
    Step {
        epoch: u64,
    },
    Disconnect,
    Custom {
        req: u32,
    },
    Other,
}

struct Ending {
    deadline: Instant,
    /// What starts once the session is gone.
    restart: Option<Relaunch>,
    /// `$stopDebugging` requests waiting for the end.
    waiters: Vec<u32>,
}

/// A session that stopped to start again (`restartSession` without `supportsRestartRequest`):
/// the configuration, and the restart's tasks to run before it starts (none for a restart the
/// adapter asked for with `terminated`).
#[derive(Debug, Clone)]
struct Relaunch {
    config: Value,
    tasks: Vec<ChainTask>,
}

/// A task of a compound or a restart, outside a configuration's own start: the label (`Err` for
/// a task identifier, which the task runner cannot look up), and whether it goes through
/// `runTaskAndCheckErrors` (`checked`) or only `runTask`.
#[derive(Debug, Clone, PartialEq)]
struct ChainTask {
    label: Result<String, ()>,
    checked: bool,
}

/// What follows a [`Chain`]'s tasks when every one went well.
#[derive(Debug)]
enum ChainThen {
    /// Start the configurations of a compound; the first answers whoever waits.
    Compound { configs: Vec<Value>, origin: Origin },
    /// Send the adapter `restart` (`supportsRestartRequest`).
    RestartRequest { session: String },
    /// Start a stopped session's configuration again.
    Relaunch { config: Value, origin: Origin },
}

/// Tasks run one after another before something starts: a compound's `preLaunchTask`
/// (`startDebugging`), or a restart's `preRestartTask`, `postDebugTask`, `preLaunchTask` and
/// `postRestartTask` (`restartSession`'s `runTasks`).
#[derive(Debug)]
struct Chain {
    tasks: VecDeque<ChainTask>,
    /// The task running, or the one whose question is open.
    current: Option<ChainTask>,
    asking: bool,
    then: ChainThen,
}

/// The task `key` of a configuration: `None` when it has none, `Err` for a task identifier.
fn task_ref(config: &Value, key: &str) -> Option<Result<String, ()>> {
    task_label(config.get(key))
}

/// A task reference as written: a label, `Err` for a task identifier, `None` for none.
fn task_label(value: Option<&Value>) -> Option<Result<String, ()>> {
    match value {
        None | Some(Value::Null) => None,
        Some(Value::String(label)) if label.is_empty() => None,
        Some(Value::String(label)) => Some(Ok(label.clone())),
        Some(_) => Some(Err(())),
    }
}

/// The tasks `restartSession` runs for a restart the user asked for, in its order:
/// `preRestartTask` and `postDebugTask` through `runTask`, then `preLaunchTask` and
/// `postRestartTask` through `runTaskAndCheckErrors`.
fn restart_tasks(config: &Value) -> Vec<ChainTask> {
    [
        ("preRestartTask", false),
        ("postDebugTask", false),
        ("preLaunchTask", true),
        ("postRestartTask", true),
    ]
    .into_iter()
    .filter_map(|(key, checked)| task_ref(config, key).map(|label| ChainTask { label, checked }))
    .collect()
}

/// The message of a task identifier the task runner cannot look up.
const TASK_NOT_FOUND: &str = "Could not find the specified task.";

struct Session {
    id: String,
    handle: i64,
    name: String,
    debug_type: String,
    request: String,
    parent: Option<String>,
    folder: Option<UriComponents>,
    configuration: Value,
    unresolved: Value,
    vars: VarContext,
    state: SessionState,
    caps: Map<String, Value>,
    seq: i64,
    pending: HashMap<i64, Dap>,
    /// The adapter sent `initialized`.
    ready: bool,
    /// Initial breakpoint requests still unanswered.
    bp_wait: usize,
    config_done: bool,
    announced: bool,
    da_started: bool,
    threads: Vec<ThreadInfo>,
    thread_id: Option<i64>,
    frame_id: Option<i64>,
    stop_epoch: u64,
    all_stopped: bool,
    /// A `stopped` event without a thread: the stack is requested for the first thread listed.
    stack_after_threads: bool,
    notify: Notify,
    ending: Option<Ending>,
    /// The compound root of a compound with `stopAll`.
    compound: Option<u64>,
}

impl Session {
    fn cap(&self, key: &str) -> bool {
        self.caps.get(key).and_then(Value::as_bool).unwrap_or(false)
    }

    /// Run Without Debugging: no breakpoints go to the adapter.
    fn no_debug(&self) -> bool {
        self.configuration
            .get("noDebug")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
}

pub struct DebugState {
    now: Instant,
    workspace: Option<UriComponents>,
    debug_types: Vec<String>,
    providers: Vec<Provider>,
    factories: Vec<(i64, String)>,
    breakpoints: Breakpoints,
    sessions: Vec<Session>,
    starts: HashMap<u64, Start>,
    next_start: u64,
    next_handle: i64,
    active: Option<String>,
    /// Sessions the extension host said `$sessionCached` for: it needs only their id.
    cached: HashSet<String>,
    /// [`DebugRequest::ProvideConfigurations`] in progress, by id.
    provides: HashMap<u64, Provide>,
    /// Debug hovers looked up in a frame's scopes, by the id of their [`DebugRequest::Hover`].
    hovers: HashMap<u64, hover::HoverSearch>,
    /// The tasks of compounds and restarts on their way, by the id their tasks carry
    /// ([`DebugUpdate::RunTask`]'s `start`).
    chains: HashMap<u64, Chain>,
    /// Compound roots whose sessions were stopped together already (`DebugCompoundRoot.stopped`).
    stopped_compounds: HashSet<u64>,
}

/// One [`DebugRequest::ProvideConfigurations`]: the activations, then every provider of the type,
/// as `debugConfigurationManager.provideDebugConfigurations` runs it after
/// `activateDebuggers('onDebugInitialConfigurations')`.
#[derive(Debug)]
struct Provide {
    debug_type: String,
    activating: bool,
    waiting: usize,
    configurations: Vec<Value>,
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// A DAP `Variable` or `EvaluateResponse` body whose `presentationHint` says `readOnly`.
fn is_read_only(v: &Value) -> bool {
    v.get("presentationHint")
        .and_then(|h| h.get("attributes"))
        .and_then(Value::as_array)
        .is_some_and(|a| a.iter().any(|x| x == "readOnly"))
}

/// What a `preLaunchTask` outcome means for the start (`runTaskAndCheckErrors`).
#[derive(Debug, PartialEq)]
enum TaskCheck {
    Go,
    /// Cancelled, or `debug.onTaskErrors` is `abort`: the start ends without a question.
    Abandon,
    /// `debug.onTaskErrors` is `showErrors`: the problems view opens and the start ends.
    ShowErrors,
    Ask {
        message: String,
        choices: Vec<TaskChoice>,
        error: bool,
    },
}

fn check_task(label: &str, outcome: &TaskOutcome, policy: OnTaskErrors, error_markers: usize) -> TaskCheck {
    let (errors, code) = match outcome {
        TaskOutcome::Failed { message } => {
            return TaskCheck::Ask {
                message: message.clone(),
                choices: vec![
                    TaskChoice::DebugAnyway,
                    TaskChoice::ConfigureTask,
                    TaskChoice::Cancel,
                ],
                error: true,
            }
        }
        // `exitCode === undefined`: the user cancelled the task.
        TaskOutcome::Exited { code: None, .. } => return TaskCheck::Abandon,
        TaskOutcome::Exited {
            code: Some(code),
            errors,
        } => ((*errors).max(error_markers), Some(*code)),
        TaskOutcome::Ready { errors } => ((*errors).max(error_markers), None),
    };
    // `successExitCode || onTaskErrors === 'debugAnyway' || (errorCount === 0 &&
    // !failureExitCode)`; a background task has no summary, so only its errors count.
    if code == Some(0) || policy == OnTaskErrors::DebugAnyway || (errors == 0 && code.is_none()) {
        return TaskCheck::Go;
    }
    match policy {
        OnTaskErrors::ShowErrors => return TaskCheck::ShowErrors,
        OnTaskErrors::Abort => return TaskCheck::Abandon,
        _ => {}
    }
    let message = if errors > 1 {
        format!("Errors exist after running preLaunchTask '{label}'.")
    } else if errors == 1 {
        format!("Error exists after running preLaunchTask '{label}'.")
    } else if let Some(code) = code {
        format!("The preLaunchTask '{label}' terminated with exit code {code}.")
    } else {
        format!("The preLaunchTask '{label}' terminated.")
    };
    TaskCheck::Ask {
        message,
        choices: vec![
            TaskChoice::DebugAnyway,
            TaskChoice::ShowErrors,
            TaskChoice::Abort,
        ],
        error: false,
    }
}

/// The scopes of a `scopes` response body.
fn parse_scopes(body: &Value) -> Vec<ScopeInfo> {
    body.get("scopes")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|s| ScopeInfo {
                    name: str_of(s, "name").unwrap_or_default(),
                    variables_reference: s
                        .get("variablesReference")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                    expensive: s
                        .get("expensive")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The variables of a `variables` response body.
fn parse_variables(body: &Value) -> Vec<VariableInfo> {
    body.get("variables")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|v| VariableInfo {
                    name: str_of(v, "name").unwrap_or_default(),
                    value: str_of(v, "value").unwrap_or_default(),
                    type_name: str_of(v, "type"),
                    variables_reference: v
                        .get("variablesReference")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                    evaluate_name: str_of(v, "evaluateName"),
                    read_only: is_read_only(v),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn u32_of(v: &Value, key: &str) -> Option<u32> {
    v.get(key)
        .and_then(Value::as_i64)
        .and_then(|n| u32::try_from(n).ok())
}

fn merge_caps(caps: &mut Map<String, Value>, body: &Value) {
    if let Some(object) = body.as_object() {
        for (k, v) in object {
            caps.insert(k.clone(), v.clone());
        }
    }
}

fn config_type(config: &Value) -> String {
    str_of(config, "type").unwrap_or_default()
}

fn send_dap(s: &mut Session, command: &str, arguments: Value, tag: Dap, out: &mut Vec<Out>) {
    s.seq += 1;
    let seq = s.seq;
    s.pending.insert(seq, tag);
    out.push(Out::Rpc(wire::send_da_message(
        s.handle,
        &DapMessage::request(seq, command, arguments),
    )));
}

fn send_file(
    s: &mut Session,
    bps: &Breakpoints,
    uri: &UriComponents,
    initial: bool,
    out: &mut Vec<Out>,
) {
    let enabled = bps.enabled_in(uri);
    let args = set_breakpoints_args(uri, &enabled);
    let ids: Vec<String> = enabled.iter().map(|b| b.id.clone()).collect();
    send_dap(
        s,
        "setBreakpoints",
        args,
        Dap::SetBreakpoints { ids, initial },
        out,
    );
}

/// Whether a request was sent: an adapter without function breakpoints gets none.
fn send_functions(s: &mut Session, bps: &Breakpoints, initial: bool, out: &mut Vec<Out>) -> bool {
    if !s.cap("supportsFunctionBreakpoints") {
        return false;
    }
    let enabled = bps.enabled_functions();
    let args = set_function_breakpoints_args(&enabled);
    let ids: Vec<String> = enabled.iter().map(|b| b.id.clone()).collect();
    send_dap(
        s,
        "setFunctionBreakpoints",
        args,
        Dap::SetFunctionBreakpoints { ids, initial },
        out,
    );
    true
}

/// Whether a request was sent: an adapter that has `configurationDone` and offers no filters
/// gets none (`debugService.sendExceptionBreakpoints`).
fn send_exceptions(s: &mut Session, bps: &Breakpoints, initial: bool, out: &mut Vec<Out>) -> bool {
    if s.cap("supportsConfigurationDoneRequest") && bps.exception_filters.is_empty() {
        return false;
    }
    let args = bps.exception_args(s.cap("supportsExceptionFilterOptions"));
    send_dap(
        s,
        "setExceptionBreakpoints",
        args,
        Dap::SetExceptionBreakpoints { initial },
        out,
    );
    true
}

fn request_stack(s: &mut Session, thread_id: i64, out: &mut Vec<Out>) {
    let epoch = s.stop_epoch;
    send_dap(
        s,
        "stackTrace",
        json!({ "threadId": thread_id, "startFrame": 0, "levels": 20 }),
        Dap::StackTrace { thread_id, epoch },
        out,
    );
}

fn initialize_args(debug_type: &str) -> Value {
    // `clientID` stays "vscode": adapters key behaviour on it. There is no terminal, so
    // `runInTerminal` is not offered.
    json!({
        "clientID": "vscode",
        "clientName": "Ember",
        "adapterID": debug_type,
        "pathFormat": "path",
        "linesStartAt1": true,
        "columnsStartAt1": true,
        "supportsVariableType": true,
        "supportsVariablePaging": true,
        "supportsRunInTerminalRequest": false,
        "supportsStartDebuggingRequest": true,
        "locale": "en-US",
    })
}

impl DebugState {
    pub fn new(now: Instant) -> Self {
        Self {
            now,
            workspace: None,
            debug_types: Vec::new(),
            providers: Vec::new(),
            factories: Vec::new(),
            breakpoints: Breakpoints::default(),
            sessions: Vec::new(),
            starts: HashMap::new(),
            next_start: 0,
            next_handle: 1,
            active: None,
            cached: HashSet::new(),
            provides: HashMap::new(),
            hovers: HashMap::new(),
            chains: HashMap::new(),
            stopped_compounds: HashSet::new(),
        }
    }

    pub fn set_now(&mut self, now: Instant) {
        self.now = now;
    }

    pub fn set_workspace(&mut self, folder: UriComponents) {
        self.workspace = Some(folder);
    }

    // ---- registrations (`MainThreadDebugService`) ---------------------------------------------

    pub fn register_types(&mut self, types: Vec<String>) {
        for t in types {
            if !self.debug_types.contains(&t) {
                self.debug_types.push(t);
            }
        }
    }

    /// The debug types the extensions declare (`contributes.debuggers`).
    pub fn debug_types(&self) -> &[String] {
        &self.debug_types
    }

    pub fn register_provider(&mut self, provider: Provider) {
        self.providers.retain(|p| p.handle != provider.handle);
        self.providers.push(provider);
    }

    pub fn unregister_provider(&mut self, handle: i64) {
        self.providers.retain(|p| p.handle != handle);
    }

    pub fn providers(&self) -> &[Provider] {
        &self.providers
    }

    pub fn register_factory(&mut self, debug_type: String, handle: i64) {
        self.factories.retain(|(h, _)| *h != handle);
        self.factories.push((handle, debug_type));
    }

    pub fn unregister_factory(&mut self, handle: i64) {
        self.factories.retain(|(h, _)| *h != handle);
    }

    /// The types with an adapter descriptor factory registered.
    pub fn factory_types(&self) -> Vec<String> {
        self.factories.iter().map(|(_, t)| t.clone()).collect()
    }

    pub fn session_cached(&mut self, id: String) {
        self.cached.insert(id);
    }

    pub fn breakpoints(&self) -> &Breakpoints {
        &self.breakpoints
    }

    /// The threads the adapter of a session last listed.
    pub fn threads(&self, session: &str) -> Vec<ThreadInfo> {
        self.idx(session)
            .map(|i| self.sessions[i].threads.clone())
            .unwrap_or_default()
    }

    pub fn sessions(&self) -> Vec<SessionInfo> {
        self.sessions
            .iter()
            .map(|s| SessionInfo {
                id: s.id.clone(),
                name: s.name.clone(),
                debug_type: s.debug_type.clone(),
                request: s.request.clone(),
                parent: s.parent.clone(),
                state: s.state.clone(),
                thread_id: s.thread_id,
                frame_id: s.frame_id,
                can_set_variables: s.cap("supportsSetVariable")
                    || s.cap("supportsSetExpression"),
                no_debug: s.no_debug(),
            })
            .collect()
    }

    // ---- small helpers ------------------------------------------------------------------------

    fn idx(&self, id: &str) -> Option<usize> {
        self.sessions.iter().position(|s| s.id == id)
    }

    /// The workspace file a DAP `source` names (#1151, Code-OSS's `Source.uri`).
    fn uri_of(&self, source: Option<&Value>) -> Option<UriComponents> {
        source::source_uri(source?, self.workspace.as_ref())
    }

    fn session_ref(&self, i: usize) -> SessionRef {
        let s = &self.sessions[i];
        if self.cached.contains(&s.id) {
            SessionRef::Id(s.id.clone())
        } else {
            SessionRef::Full(SessionDto {
                id: s.id.clone(),
                debug_type: s.debug_type.clone(),
                name: s.name.clone(),
                parent: s.parent.clone(),
                folder_uri: s.folder.clone(),
                configuration: s.configuration.clone(),
            })
        }
    }

    fn emit_sessions(&self, out: &mut Vec<Out>) {
        out.push(Out::Ui(DebugUpdate::Sessions {
            sessions: self.sessions(),
            active: self.active.clone(),
        }));
    }

    fn emit_breakpoints(&self, out: &mut Vec<Out>) {
        out.push(Out::Ui(DebugUpdate::Breakpoints {
            source: self.breakpoints.source.clone(),
            functions: self.breakpoints.functions.clone(),
            exception_filters: self.breakpoints.exception_filters.clone(),
        }));
    }

    /// Answer a [`DebugRequest::ProvideConfigurations`] once every provider has, even when the
    /// type has none (the window then writes only the contribution's `initialConfigurations`).
    fn finish_provide(&mut self, id: u64, p: Provide, out: &mut Vec<Out>) {
        if p.activating || p.waiting > 0 {
            self.provides.insert(id, p);
            return;
        }
        out.push(Out::Ui(DebugUpdate::ProvidedConfigurations {
            debug_type: p.debug_type,
            configurations: p.configurations,
        }));
    }

    fn console(&self, session: Option<&str>, category: &str, text: String, out: &mut Vec<Out>) {
        let entry = ConsoleEntry {
            category: category.to_owned(),
            text,
            uri: None,
            line: None,
        };
        out.push(Out::Ui(DebugUpdate::Console {
            session: session.map(str::to_owned),
            entry,
        }));
    }

    fn resolve_notify(&self, notify: Notify, ok: bool, out: &mut Vec<Out>) {
        match notify {
            Notify::Nobody => {}
            Notify::Ext { req } => out.push(Out::Respond {
                req,
                result: Ok(Reply::Json(Value::Bool(ok))),
            }),
            Notify::Dap {
                handle,
                request_seq,
            } => {
                let error = (!ok).then_some("Failed to start debugging");
                out.push(Out::Rpc(wire::send_da_message(
                    handle,
                    &DapMessage::response(request_seq, "startDebugging", error, Value::Null),
                )));
            }
        }
    }

    fn start_failed(&self, notify: Notify, message: String, out: &mut Vec<Out>) {
        out.push(Out::Ui(DebugUpdate::StartFailed { message }));
        self.resolve_notify(notify, false, out);
    }

    // ---- requests of the window ---------------------------------------------------------------

    pub fn request(&mut self, request: DebugRequest, env: &Env) -> Vec<Out> {
        let mut out = Vec::new();
        match request {
            DebugRequest::ListConfigurations => self.begin_list(env, &mut out),
            DebugRequest::ProvideConfigurations { debug_type } => {
                let id = self.next_start_id();
                let events = [
                    "onDebugInitialConfigurations".to_owned(),
                    "onDebug".to_owned(),
                ];
                for event in &events {
                    out.push(Out::RpcWith(
                        exthost::activate_by_event(event),
                        Pending::ProvideActivation { id },
                    ));
                }
                self.provides.insert(
                    id,
                    Provide {
                        debug_type,
                        activating: true,
                        waiting: events.len(),
                        configurations: Vec::new(),
                    },
                );
            }
            DebugRequest::Start { selector, no_debug } => {
                let origin = self.origin(None, Notify::Nobody, None, no_debug, env);
                self.begin_selected(selector, origin, &mut out);
            }
            DebugRequest::TaskDone { start, outcome } => {
                self.task_done(start, outcome, env, &mut out)
            }
            DebugRequest::AnswerTask { start, choice } => {
                self.answer_task(start, choice, &mut out)
            }
            DebugRequest::SetVariable {
                session,
                reference,
                name,
                value,
                evaluate_name,
                frame_id,
            } => self.set_variable(
                &session,
                reference,
                name,
                value,
                evaluate_name,
                frame_id,
                &mut out,
            ),
            DebugRequest::Stop { session: Some(id) } => self.stop_session(&id, None, &mut out),
            DebugRequest::Stop { session: None } => self.stop_all(&mut out),
            DebugRequest::Restart { session } => self.restart(&session, &mut out),
            DebugRequest::Continue { session, thread_id } => {
                self.thread_command(&session, "continue", thread_id, true, &mut out)
            }
            DebugRequest::Pause { session, thread_id } => {
                self.thread_command(&session, "pause", thread_id, false, &mut out)
            }
            DebugRequest::StepOver { session, thread_id } => {
                self.thread_command(&session, "next", thread_id, true, &mut out)
            }
            DebugRequest::StepInto { session, thread_id } => {
                self.thread_command(&session, "stepIn", thread_id, true, &mut out)
            }
            DebugRequest::StepOut { session, thread_id } => {
                self.thread_command(&session, "stepOut", thread_id, true, &mut out)
            }
            DebugRequest::SelectThread { session, thread_id } => {
                if let Some(i) = self.idx(&session) {
                    let s = &mut self.sessions[i];
                    s.thread_id = Some(thread_id);
                    s.frame_id = None;
                    request_stack(s, thread_id, &mut out);
                    self.emit_sessions(&mut out);
                }
            }
            DebugRequest::SelectFrame {
                session,
                thread_id,
                frame_id,
            } => {
                if let Some(i) = self.idx(&session) {
                    let s = &mut self.sessions[i];
                    s.thread_id = Some(thread_id);
                    s.frame_id = Some(frame_id);
                    send_dap(
                        s,
                        "scopes",
                        json!({ "frameId": frame_id }),
                        Dap::Scopes { frame_id },
                        &mut out,
                    );
                    out.push(Out::Rpc(wire::accept_stack_frame_focus(
                        &session,
                        thread_id,
                        Some(frame_id),
                    )));
                    self.emit_sessions(&mut out);
                }
            }
            DebugRequest::Scopes { session, frame_id } => {
                if let Some(i) = self.idx(&session) {
                    send_dap(
                        &mut self.sessions[i],
                        "scopes",
                        json!({ "frameId": frame_id }),
                        Dap::Scopes { frame_id },
                        &mut out,
                    );
                }
            }
            DebugRequest::Variables { session, reference } => {
                if let Some(i) = self.idx(&session) {
                    let args = json!({ "variablesReference": reference });
                    send_dap(
                        &mut self.sessions[i],
                        "variables",
                        args,
                        Dap::Variables { reference },
                        &mut out,
                    );
                }
            }
            DebugRequest::Evaluate {
                id,
                session,
                frame_id,
                expression,
                context,
            } => match self.idx(&session) {
                Some(i) => {
                    let mut args = json!({ "expression": expression, "context": context.as_str() });
                    if let Some(frame) = frame_id {
                        args["frameId"] = json!(frame);
                    }
                    send_dap(
                        &mut self.sessions[i],
                        "evaluate",
                        args,
                        Dap::Evaluate { id },
                        &mut out,
                    );
                }
                None => out.push(Out::Ui(DebugUpdate::Evaluated {
                    id,
                    session,
                    result: Err("debug session not found".to_owned()),
                })),
            },
            DebugRequest::ToggleBreakpoint { uri, line } => {
                let (id, added, touched) = self.breakpoints.toggle(&uri, line);
                let mut delta = BreakpointsDelta::default();
                if added {
                    delta.added.extend(self.breakpoints.dto(&id));
                } else {
                    delta.removed.push(id);
                }
                self.breakpoints_changed(delta, touched, &mut out);
            }
            DebugRequest::AddBreakpoint {
                uri,
                line,
                column,
                condition,
                hit_condition,
                log_message,
            } => {
                let id = self.breakpoints.add_source(
                    None,
                    uri.clone(),
                    line,
                    column,
                    condition,
                    hit_condition,
                    log_message,
                    true,
                );
                let mut delta = BreakpointsDelta::default();
                delta.added.extend(self.breakpoints.dto(&id));
                self.breakpoints_changed(
                    delta,
                    Touched {
                        files: vec![uri],
                        ..Default::default()
                    },
                    &mut out,
                );
            }
            DebugRequest::EditBreakpoint {
                id,
                condition,
                hit_condition,
                log_message,
                enabled,
            } => {
                if let Some(touched) =
                    self.breakpoints
                        .edit(&id, condition, hit_condition, log_message, enabled)
                {
                    let mut delta = BreakpointsDelta::default();
                    delta.changed.extend(self.breakpoints.dto(&id));
                    self.breakpoints_changed(delta, touched, &mut out);
                }
            }
            DebugRequest::RemoveBreakpoint { id } => {
                if let Some(touched) = self.breakpoints.remove(&id) {
                    let delta = BreakpointsDelta {
                        removed: vec![id],
                        ..Default::default()
                    };
                    self.breakpoints_changed(delta, touched, &mut out);
                }
            }
            DebugRequest::AddFunctionBreakpoint { name } => {
                let id = self
                    .breakpoints
                    .add_function(None, name, None, None, None, true);
                let mut delta = BreakpointsDelta::default();
                delta.added.extend(self.breakpoints.dto(&id));
                self.breakpoints_changed(
                    delta,
                    Touched {
                        functions: true,
                        ..Default::default()
                    },
                    &mut out,
                );
            }
            DebugRequest::Hover {
                id,
                session,
                frame_id,
                uri,
                line,
                column,
                expression,
            } => self.hover(id, &session, frame_id, &uri, line, column, &expression, &mut out),
            DebugRequest::RestoreBreakpoints(stored) => {
                if let Some(base) = self.workspace.clone() {
                    let (ids, touched) = self.breakpoints.restore(&stored, &base);
                    if !touched.is_empty() {
                        let mut delta = BreakpointsDelta::default();
                        for id in &ids {
                            delta.added.extend(self.breakpoints.dto(id));
                        }
                        self.breakpoints_changed(delta, touched, &mut out);
                    }
                }
            }
            DebugRequest::SetExceptionFilter { filter, enabled } => {
                if self.breakpoints.set_filter_enabled(&filter, enabled) {
                    self.breakpoints_changed(
                        BreakpointsDelta::default(),
                        Touched {
                            exceptions: true,
                            ..Default::default()
                        },
                        &mut out,
                    );
                }
            }
        }
        out
    }

    fn origin(
        &self,
        parent: Option<String>,
        notify: Notify,
        folder: Option<UriComponents>,
        no_debug: bool,
        env: &Env,
    ) -> Origin {
        let folder = folder.or_else(|| self.workspace.clone());
        let vars = VarContext {
            workspace_folder: folder.as_ref().map(|f| f.path.clone()),
            file: env.active_file.clone(),
        };
        Origin {
            parent,
            folder,
            notify,
            no_debug,
            vars,
            compound: None,
            skip_pre_launch: false,
        }
    }

    fn thread_command(
        &mut self,
        session: &str,
        command: &str,
        thread_id: i64,
        step: bool,
        out: &mut Vec<Out>,
    ) {
        let Some(i) = self.idx(session) else { return };
        let s = &mut self.sessions[i];
        let tag = if step {
            Dap::Step {
                epoch: s.stop_epoch,
            }
        } else {
            Dap::Other
        };
        send_dap(s, command, json!({ "threadId": thread_id }), tag, out);
    }

    // ---- the debug hover ------------------------------------------------------------------------

    /// [`DebugRequest::Hover`]: `evaluate` in the `hover` context, or the lookup in the frame's
    /// scopes for an adapter without `supportsEvaluateForHovers` (`DebugHoverComputer.evaluate`).
    #[allow(clippy::too_many_arguments)]
    fn hover(
        &mut self,
        id: u64,
        session: &str,
        frame_id: i64,
        uri: &UriComponents,
        line: u32,
        column: u32,
        expression: &str,
        out: &mut Vec<Out>,
    ) {
        let Some(i) = self.idx(session) else {
            out.push(Out::Ui(DebugUpdate::Evaluated {
                id,
                session: session.to_owned(),
                result: Err("debug session not found".to_owned()),
            }));
            return;
        };
        let s = &mut self.sessions[i];
        if s.cap("supportsEvaluateForHovers") {
            let name = uri.path.rsplit('/').next().unwrap_or(&uri.path);
            let args = json!({
                "expression": expression,
                "frameId": frame_id,
                "context": EvalContext::Hover.as_str(),
                "line": line,
                "column": column,
                "source": { "name": name, "path": uri.path },
            });
            send_dap(s, "evaluate", args, Dap::Evaluate { id }, out);
            return;
        }
        self.hovers
            .insert(id, hover::HoverSearch::new(session.to_owned(), expression));
        send_dap(
            s,
            "scopes",
            json!({ "frameId": frame_id }),
            Dap::HoverScopes { id },
            out,
        );
    }

    /// Ask what a hover search wants next, or answer the hover when it has ended.
    fn hover_step(
        &mut self,
        sid: &str,
        id: u64,
        steps: Vec<hover::SearchStep>,
        done: Option<hover::SearchResult>,
        out: &mut Vec<Out>,
    ) {
        if let Some(i) = self.idx(sid) {
            for hover::SearchStep::Variables { reference, depth } in steps {
                send_dap(
                    &mut self.sessions[i],
                    "variables",
                    json!({ "variablesReference": reference }),
                    Dap::HoverVariables { id, depth },
                    out,
                );
            }
        }
        if let Some(result) = done {
            self.hovers.remove(&id);
            out.push(Out::Ui(DebugUpdate::Evaluated {
                id,
                session: sid.to_owned(),
                result: result.map(|v| EvalResult {
                    result: v.value,
                    type_name: v.type_name,
                    variables_reference: v.variables_reference,
                }),
            }));
        }
    }

    // ---- breakpoint changes -------------------------------------------------------------------

    /// A change of the registry: tell the window, the extension host and the running sessions.
    fn breakpoints_changed(
        &mut self,
        delta: BreakpointsDelta,
        touched: Touched,
        out: &mut Vec<Out>,
    ) {
        self.emit_breakpoints(out);
        if !delta.is_empty() {
            out.push(Out::Rpc(wire::accept_breakpoints_delta(&delta)));
        }
        for s in &mut self.sessions {
            if !s.ready
                || s.state == SessionState::Terminated
                || s.ending.is_some()
                || s.no_debug()
            {
                continue;
            }
            for file in &touched.files {
                send_file(s, &self.breakpoints, file, false, out);
            }
            if touched.functions {
                send_functions(s, &self.breakpoints, false, out);
            }
            if touched.exceptions {
                send_exceptions(s, &self.breakpoints, false, out);
            }
        }
    }

    /// `$registerBreakpoints`: breakpoints from `debug.addBreakpoints`.
    pub fn register_breakpoints(&mut self, dtos: &[Value]) -> Vec<Out> {
        let mut out = Vec::new();
        let (ids, touched) = self
            .breakpoints
            .add_registered(wire::parse_registered_breakpoints(dtos));
        let mut delta = BreakpointsDelta::default();
        for id in &ids {
            delta.added.extend(self.breakpoints.dto(id));
        }
        self.breakpoints_changed(delta, touched, &mut out);
        out
    }

    /// `$unregisterBreakpoints`.
    pub fn unregister_breakpoints(&mut self, ids: &[String]) -> Vec<Out> {
        let mut out = Vec::new();
        let mut touched = Touched::default();
        let mut delta = BreakpointsDelta::default();
        for id in ids {
            if let Some(t) = self.breakpoints.remove(id) {
                touched.absorb(t);
                delta.removed.push(id.clone());
            }
        }
        self.breakpoints_changed(delta, touched, &mut out);
        out
    }

    // ---- starting a session -------------------------------------------------------------------

    fn next_start_id(&mut self) -> u64 {
        self.next_start += 1;
        self.next_start
    }

    fn read_launch_json(
        &mut self,
        purpose: Purpose,
        selector: Option<ConfigSelector>,
        origin: Origin,
        out: &mut Vec<Out>,
    ) {
        let Some(folder) = origin.folder.clone() else {
            if purpose == Purpose::List {
                out.push(Out::Ui(DebugUpdate::Configurations {
                    configurations: Vec::new(),
                    error: Some("no folder is open".to_owned()),
                }));
            } else {
                self.start_failed(
                    origin.notify,
                    "There is no open folder, so there is no launch.json to read.".to_owned(),
                    out,
                );
            }
            return;
        };
        let uri = UriComponents::new(
            folder.scheme.clone(),
            folder.authority.clone(),
            format!("{}/{}", folder.path.trim_end_matches('/'), LAUNCH_JSON),
        );
        let id = self.next_start_id();
        self.starts.insert(
            id,
            Start {
                id,
                purpose,
                selector,
                origin,
                config: Value::Null,
                unresolved: Value::Null,
                phase: Phase::Reading,
                current_type: String::new(),
                seen: Vec::new(),
                waiting: 0,
                todo: VecDeque::new(),
                group: 0,
                substituted: false,
            },
        );
        out.push(Out::ReadLaunchJson { uri, start: id });
    }

    fn begin_list(&mut self, env: &Env, out: &mut Vec<Out>) {
        let origin = self.origin(None, Notify::Nobody, None, false, env);
        self.read_launch_json(Purpose::List, None, origin, out);
    }

    fn begin_selected(&mut self, selector: ConfigSelector, origin: Origin, out: &mut Vec<Out>) {
        match selector {
            ConfigSelector::Config(config) => self.begin_config(config, origin, out),
            other => self.read_launch_json(Purpose::Launch, Some(other), origin, out),
        }
    }

    /// The answer to a [`Out::ReadLaunchJson`]: the file's bytes, `None` for a missing file.
    pub fn on_launch_json(
        &mut self,
        start: u64,
        result: Result<Option<Vec<u8>>, String>,
    ) -> Vec<Out> {
        let mut out = Vec::new();
        let Some(st) = self.starts.remove(&start) else {
            return out;
        };
        let parsed: Result<LaunchFile, String> = match result {
            Ok(Some(bytes)) => match String::from_utf8(bytes) {
                Ok(text) => parse_launch_json(&text),
                Err(_) => Err("cannot read launch.json: it is not UTF-8 text".to_owned()),
            },
            Ok(None) => Ok(LaunchFile::default()),
            Err(e) => Err(format!("cannot read launch.json: {e}")),
        };
        if st.purpose == Purpose::List {
            let (configurations, error) = match parsed {
                Ok(file) => {
                    let mut list: Vec<ConfigInfo> = file
                        .configurations
                        .iter()
                        .map(|c| ConfigInfo {
                            name: config_name(c).to_owned(),
                            debug_type: config_type(c),
                            request: str_of(c, "request").unwrap_or_default(),
                            compound: false,
                        })
                        .collect();
                    list.extend(file.compounds.iter().map(|c| ConfigInfo {
                        name: c.name.clone(),
                        debug_type: String::new(),
                        request: String::new(),
                        compound: true,
                    }));
                    (list, None)
                }
                Err(e) => (Vec::new(), Some(e)),
            };
            out.push(Out::Ui(DebugUpdate::Configurations {
                configurations,
                error,
            }));
            return out;
        }
        let file = match parsed {
            Ok(f) => f,
            Err(e) => {
                self.start_failed(st.origin.notify, e, &mut out);
                return out;
            }
        };
        let wanted = match &st.selector {
            Some(ConfigSelector::Name(n)) => n.clone(),
            Some(ConfigSelector::Index(i)) => format!("#{i}"),
            _ => String::new(),
        };
        let origin = st.origin;
        if let Some(ConfigSelector::Name(n)) = &st.selector {
            if file.find(n).is_none() {
                if let Some(compound) = file.compound(n) {
                    self.begin_compound(&file, compound, origin, &mut out);
                    return out;
                }
            }
        }
        let config = match &st.selector {
            Some(ConfigSelector::Name(n)) => file.find(n).cloned(),
            Some(ConfigSelector::Index(i)) => file.configurations.get(*i).cloned(),
            _ => None,
        };
        let Some(config) = config else {
            let message = format!("There is no configuration {wanted} in launch.json.");
            self.start_failed(origin.notify, message, &mut out);
            return out;
        };
        self.begin_config(config, origin, &mut out);
        out
    }

    /// A compound (`startDebugging`): its configurations are looked up, its own `preLaunchTask`
    /// runs once through `runTaskAndCheckErrors` and a failure ends the start, then every
    /// configuration starts with its own `preLaunchTask`. With `stopAll` they share a compound
    /// root, so stopping one stops the others. A configuration with the compound's own name is
    /// skipped, as Code-OSS skips it.
    fn begin_compound(
        &mut self,
        file: &LaunchFile,
        compound: &launch::Compound,
        mut origin: Origin,
        out: &mut Vec<Out>,
    ) {
        let mut configs = Vec::new();
        for name in compound.configurations.iter().filter(|n| **n != compound.name) {
            match file.find(name) {
                Some(c) => configs.push(c.clone()),
                None => {
                    let message =
                        format!("Could not find launch configuration '{name}' in the workspace.");
                    self.start_failed(origin.notify, message, out);
                    return;
                }
            }
        }
        if compound.stop_all {
            origin.compound = Some(self.next_start_id());
        }
        let tasks: Vec<ChainTask> = task_label(compound.pre_launch_task.as_ref())
            .map(|label| ChainTask {
                label,
                checked: true,
            })
            .into_iter()
            .collect();
        self.run_chain(tasks, ChainThen::Compound { configs, origin }, out);
    }

    fn begin_config(&mut self, mut config: Value, origin: Origin, out: &mut Vec<Out>) {
        if origin.no_debug {
            if let Some(object) = config.as_object_mut() {
                object.insert("noDebug".to_owned(), Value::Bool(true));
            }
        }
        let id = self.next_start_id();
        let mut st = Start {
            id,
            purpose: Purpose::Launch,
            selector: None,
            origin,
            unresolved: config.clone(),
            config,
            phase: Phase::Activate,
            current_type: String::new(),
            seen: Vec::new(),
            waiting: 0,
            todo: VecDeque::new(),
            group: 0,
            substituted: false,
        };
        self.advance(&mut st, out);
        self.keep(st);
    }

    fn keep(&mut self, st: Start) {
        if st.phase != Phase::Done {
            self.starts.insert(st.id, st);
        }
    }

    fn provider_handles(&self, debug_type: &str, substituted: bool) -> VecDeque<i64> {
        self.providers
            .iter()
            .filter(|p| {
                p.debug_type == debug_type
                    && if substituted {
                        p.has_resolve2
                    } else {
                        p.has_resolve
                    }
            })
            .map(|p| p.handle)
            .collect()
    }

    /// Run the start as far as it can go without an answer from the extension host.
    fn advance(&mut self, st: &mut Start, out: &mut Vec<Out>) {
        loop {
            match st.phase {
                Phase::Reading
                | Phase::Activating
                | Phase::Task
                | Phase::TaskAsk
                | Phase::Done => return,
                Phase::Activate => {
                    let ty = config_type(&st.config);
                    if ty.is_empty() {
                        let notify = std::mem::take(&mut st.origin.notify);
                        self.start_failed(
                            notify,
                            "Missing property 'type' in the chosen debug configuration.".to_owned(),
                            out,
                        );
                        st.phase = Phase::Done;
                        return;
                    }
                    st.current_type = ty.clone();
                    st.seen.push(ty.clone());
                    st.group = 0;
                    let events = [
                        "onDebugResolve".to_owned(),
                        "onDebug".to_owned(),
                        format!("onDebugResolve:{ty}"),
                    ];
                    st.waiting = events.len();
                    for event in &events {
                        out.push(Out::RpcWith(
                            exthost::activate_by_event(event),
                            Pending::Activation { start: st.id },
                        ));
                    }
                    st.phase = Phase::Activating;
                    return;
                }
                Phase::Resolve => {
                    if let Some(handle) = st.todo.pop_front() {
                        let folder = st.origin.folder.as_ref();
                        let call = if st.substituted {
                            wire::resolve_debug_configuration_with_substituted_variables(
                                handle, folder, &st.config,
                            )
                        } else {
                            wire::resolve_debug_configuration(handle, folder, &st.config)
                        };
                        out.push(Out::RpcWith(call, Pending::Resolve { start: st.id }));
                        return;
                    }
                    if st.group == 0 {
                        st.todo = self.provider_handles(&st.current_type, st.substituted);
                        st.group = 1;
                        continue;
                    }
                    if st.group == 1 {
                        st.todo = self.provider_handles("*", st.substituted);
                        st.group = 2;
                        continue;
                    }
                    let ty = config_type(&st.config);
                    if !st.substituted {
                        if !ty.is_empty() && !st.seen.contains(&ty) {
                            // A provider changed the type: its providers get their turn.
                            st.phase = Phase::Activate;
                            continue;
                        }
                        st.config = st.origin.vars.substitute(&st.config);
                        st.substituted = true;
                        if !ty.is_empty() {
                            st.current_type = ty;
                        }
                        st.group = 0;
                        // `debugService.createSession` runs the task after the substitution and
                        // before the second chain of providers.
                        // A label, or `Err` for a task identifier (`{ "type": ..., ... }`): the
                        // task runner finds tasks by label only. A restart ran it already.
                        let task = if st.origin.skip_pre_launch {
                            None
                        } else {
                            task_ref(&st.config, "preLaunchTask")
                        };
                        match task {
                            None => {}
                            Some(Ok(label)) => {
                                out.push(Out::Ui(DebugUpdate::RunTask {
                                    label,
                                    start: Some(st.id),
                                }));
                                st.phase = Phase::Task;
                                return;
                            }
                            Some(Err(())) => {
                                st.phase = Phase::Task;
                                let outcome = TaskOutcome::Failed {
                                    message: TASK_NOT_FOUND.to_owned(),
                                };
                                self.task_checked(st, outcome, &Env::default(), out);
                                return;
                            }
                        }
                        continue;
                    }
                    st.phase = Phase::Adapter;
                }
                Phase::Adapter => {
                    self.create_session(st, out);
                    st.phase = Phase::Done;
                    return;
                }
            }
        }
    }

    fn create_session(&mut self, st: &mut Start, out: &mut Vec<Out>) {
        let notify = std::mem::take(&mut st.origin.notify);
        let mut configuration = st.config.clone();
        let debug_type = config_type(&configuration);
        let request = str_of(&configuration, "request").unwrap_or_default();
        if request != "launch" && request != "attach" {
            let message = if request.is_empty() {
                "Attribute 'request' is missing from the chosen debug configuration.".to_owned()
            } else {
                format!("Attribute 'request' has an unsupported value '{request}' in the chosen debug configuration.")
            };
            self.start_failed(notify, message, out);
            return;
        }
        if !self.debug_types.contains(&debug_type) {
            let message = format!("Configured debug type '{debug_type}' is not supported. Install an extension that provides a debugger for it.");
            self.start_failed(notify, message, out);
            return;
        }
        let id = uuid::Uuid::new_v4().to_string();
        let handle = self.next_handle;
        self.next_handle += 1;
        let name = str_of(&configuration, "name").unwrap_or_else(|| debug_type.clone());
        if let Some(object) = configuration.as_object_mut() {
            object.insert("__sessionId".to_owned(), Value::String(id.clone()));
        }
        let dto = SessionRef::Full(SessionDto {
            id: id.clone(),
            debug_type: debug_type.clone(),
            name: name.clone(),
            parent: st.origin.parent.clone(),
            folder_uri: st.origin.folder.clone(),
            configuration: configuration.clone(),
        });
        self.sessions.push(Session {
            id: id.clone(),
            handle,
            name,
            debug_type,
            request,
            parent: st.origin.parent.clone(),
            folder: st.origin.folder.clone(),
            configuration,
            unresolved: st.unresolved.clone(),
            vars: st.origin.vars.clone(),
            state: SessionState::Initializing,
            caps: Map::new(),
            seq: 0,
            pending: HashMap::new(),
            ready: false,
            bp_wait: 0,
            config_done: false,
            announced: false,
            da_started: false,
            threads: Vec::new(),
            thread_id: None,
            frame_id: None,
            stop_epoch: 0,
            all_stopped: false,
            stack_after_threads: false,
            notify,
            ending: None,
            compound: st.origin.compound,
        });
        if self.active.is_none() || st.origin.parent.is_none() {
            self.active = Some(id.clone());
        }
        out.push(Out::RpcWith(
            wire::start_da_session(handle, &dto),
            Pending::StartDa {
                session: id,
                handle,
            },
        ));
        self.emit_sessions(out);
    }

    // ---- the preLaunchTask ---------------------------------------------------------------------

    /// What the `preLaunchTask` of `st` ended with: the start goes on, asks, or is abandoned.
    fn task_checked(&mut self, st: &mut Start, outcome: TaskOutcome, env: &Env, out: &mut Vec<Out>) {
        let label = str_of(&st.config, "preLaunchTask").unwrap_or_default();
        match check_task(&label, &outcome, env.on_task_errors, env.error_markers) {
            TaskCheck::Go => {
                st.phase = Phase::Resolve;
                self.advance(st, out);
            }
            TaskCheck::Abandon => self.abandon(st, out),
            TaskCheck::ShowErrors => {
                out.push(Out::Ui(DebugUpdate::ShowProblems));
                self.abandon(st, out);
            }
            TaskCheck::Ask {
                message,
                choices,
                error,
            } => {
                st.phase = Phase::TaskAsk;
                out.push(Out::Ui(DebugUpdate::TaskFailed {
                    start: st.id,
                    message,
                    choices,
                    error,
                }));
            }
        }
    }

    /// The start ends without a session: whoever waits for it hears `false`, as
    /// `createSession` returns false on `TaskRunResult.Failure`.
    fn abandon(&self, st: &mut Start, out: &mut Vec<Out>) {
        let notify = std::mem::take(&mut st.origin.notify);
        self.resolve_notify(notify, false, out);
        st.phase = Phase::Done;
    }

    fn task_done(&mut self, start: u64, outcome: TaskOutcome, env: &Env, out: &mut Vec<Out>) {
        if let Some(chain) = self.chains.remove(&start) {
            self.chain_task_done(start, chain, outcome, env, out);
            return;
        }
        let Some(mut st) = self.starts.remove(&start) else {
            return;
        };
        if st.phase == Phase::Task {
            self.task_checked(&mut st, outcome, env, out);
        }
        self.keep(st);
    }

    fn answer_task(&mut self, start: u64, choice: TaskChoice, out: &mut Vec<Out>) {
        if let Some(mut chain) = self.chains.remove(&start) {
            if !chain.asking {
                self.chains.insert(start, chain);
            } else if choice == TaskChoice::DebugAnyway {
                chain.asking = false;
                chain.current = None;
                self.chain_next(start, chain, &Env::default(), out);
            } else {
                self.chain_abandon(chain, None, out);
            }
            return;
        }
        let Some(mut st) = self.starts.remove(&start) else {
            return;
        };
        if st.phase == Phase::TaskAsk {
            if choice == TaskChoice::DebugAnyway {
                st.phase = Phase::Resolve;
                self.advance(&mut st, out);
            } else {
                // Show Errors and Configure Task are the window's to open; the start ends.
                self.abandon(&mut st, out);
            }
        }
        self.keep(st);
    }

    // ---- the tasks of a compound or a restart --------------------------------------------------

    /// Run `tasks` one after another, then `then`.
    fn run_chain(&mut self, tasks: Vec<ChainTask>, then: ChainThen, out: &mut Vec<Out>) {
        let id = self.next_start_id();
        let chain = Chain {
            tasks: tasks.into(),
            current: None,
            asking: false,
            then,
        };
        self.chain_next(id, chain, &Env::default(), out);
    }

    /// The next task of `chain`, or what follows them all.
    fn chain_next(&mut self, id: u64, mut chain: Chain, env: &Env, out: &mut Vec<Out>) {
        let Some(task) = chain.tasks.pop_front() else {
            self.chain_then(chain.then, out);
            return;
        };
        match task.label.clone() {
            Ok(label) => {
                out.push(Out::Ui(DebugUpdate::RunTask {
                    label,
                    start: Some(id),
                }));
                chain.current = Some(task);
                self.chains.insert(id, chain);
            }
            Err(()) => {
                chain.current = Some(task);
                let outcome = TaskOutcome::Failed {
                    message: TASK_NOT_FOUND.to_owned(),
                };
                self.chain_task_done(id, chain, outcome, env, out);
            }
        }
    }

    /// How the running task of `chain` ended. A task of `runTask` stops the chain only when it
    /// could not run (Code-OSS's rejected promise, shown as an error); one of
    /// `runTaskAndCheckErrors` is checked as a configuration's `preLaunchTask` is.
    fn chain_task_done(
        &mut self,
        id: u64,
        mut chain: Chain,
        outcome: TaskOutcome,
        env: &Env,
        out: &mut Vec<Out>,
    ) {
        let Some(task) = chain.current.clone().filter(|_| !chain.asking) else {
            self.chains.insert(id, chain);
            return;
        };
        if !task.checked {
            if let TaskOutcome::Failed { message } = outcome {
                self.chain_abandon(chain, Some(message), out);
            } else {
                chain.current = None;
                self.chain_next(id, chain, env, out);
            }
            return;
        }
        let label = task.label.clone().unwrap_or_default();
        match check_task(&label, &outcome, env.on_task_errors, env.error_markers) {
            TaskCheck::Go => {
                chain.current = None;
                self.chain_next(id, chain, env, out);
            }
            TaskCheck::Abandon => self.chain_abandon(chain, None, out),
            TaskCheck::ShowErrors => {
                out.push(Out::Ui(DebugUpdate::ShowProblems));
                self.chain_abandon(chain, None, out);
            }
            TaskCheck::Ask {
                message,
                choices,
                error,
            } => {
                chain.asking = true;
                out.push(Out::Ui(DebugUpdate::TaskFailed {
                    start: id,
                    message,
                    choices,
                    error,
                }));
                self.chains.insert(id, chain);
            }
        }
    }

    /// The chain ends without starting anything: a compound's caller hears `false`
    /// (`TaskRunResult.Failure`); a restart does not happen, and a session that stopped for it
    /// stays stopped. `message` is a task that could not run.
    fn chain_abandon(&self, chain: Chain, message: Option<String>, out: &mut Vec<Out>) {
        if let Some(message) = message {
            out.push(Out::Ui(DebugUpdate::StartFailed { message }));
        }
        if let ChainThen::Compound { origin, .. } = chain.then {
            self.resolve_notify(origin.notify, false, out);
        }
    }

    fn chain_then(&mut self, then: ChainThen, out: &mut Vec<Out>) {
        match then {
            ChainThen::Compound { configs, mut origin } => {
                // Only the first configuration of a compound answers the caller; a compound with
                // none left succeeds, as `[].every(...)` is true.
                let none = configs.is_empty();
                for config in configs {
                    let notify = std::mem::take(&mut origin.notify);
                    let each = Origin {
                        notify,
                        ..origin.clone()
                    };
                    self.begin_config(config, each, out);
                }
                self.resolve_notify(std::mem::take(&mut origin.notify), none, out);
            }
            ChainThen::RestartRequest { session } => {
                let Some(i) = self.idx(&session) else { return };
                let s = &mut self.sessions[i];
                if s.state == SessionState::Terminated || s.ending.is_some() {
                    return;
                }
                let args = json!({ "arguments": s.configuration.clone() });
                send_dap(s, "restart", args, Dap::Other, out);
            }
            ChainThen::Relaunch { config, origin } => self.begin_config(config, origin, out),
        }
    }

    // ---- setting a variable -------------------------------------------------------------------

    /// `Variable.setVariable` of Code-OSS: `setVariable` when the adapter supports it, else
    /// `setExpression` on the variable's `evaluateName` when the adapter supports that.
    #[allow(clippy::too_many_arguments)]
    fn set_variable(
        &mut self,
        sid: &str,
        reference: i64,
        name: String,
        value: String,
        evaluate_name: Option<String>,
        frame_id: Option<i64>,
        out: &mut Vec<Out>,
    ) {
        let refuse = |why: &str, out: &mut Vec<Out>| {
            out.push(Out::Ui(DebugUpdate::VariableSet {
                session: sid.to_owned(),
                reference,
                name: name.clone(),
                result: Err(why.to_owned()),
            }))
        };
        let Some(i) = self.idx(sid) else {
            refuse("debug session not found", out);
            return;
        };
        let s = &mut self.sessions[i];
        let tag = Dap::SetVariable {
            reference,
            name: name.clone(),
        };
        let by_expression = s.cap("supportsSetExpression");
        if s.cap("supportsSetVariable") {
            let args = json!({ "variablesReference": reference, "name": name, "value": value });
            send_dap(s, "setVariable", args, tag, out);
        } else if let Some(expression) = evaluate_name.filter(|_| by_expression) {
            let mut args = json!({ "expression": expression, "value": value });
            if let Some(frame) = frame_id {
                args["frameId"] = json!(frame);
            }
            send_dap(s, "setExpression", args, tag, out);
        } else {
            refuse("The debug adapter cannot change the value of a variable.", out);
        }
    }

    // ---- replies of the extension host --------------------------------------------------------

    pub fn on_reply(&mut self, pending: Pending, result: Result<Value, String>) -> Vec<Out> {
        let mut out = Vec::new();
        match pending {
            Pending::Activation { start } => {
                let Some(mut st) = self.starts.remove(&start) else {
                    return out;
                };
                // A failed activation is not fatal: the provider it would have registered may not exist.
                st.waiting = st.waiting.saturating_sub(1);
                if st.waiting == 0 && st.phase == Phase::Activating {
                    st.phase = Phase::Resolve;
                }
                self.advance(&mut st, &mut out);
                self.keep(st);
            }
            Pending::Resolve { start } => {
                let Some(mut st) = self.starts.remove(&start) else {
                    return out;
                };
                match result {
                    Ok(config) if config.is_object() => {
                        st.config = config;
                        self.advance(&mut st, &mut out);
                    }
                    Ok(_) => {
                        // `null` or `undefined`: a provider cancelled the launch.
                        let notify = std::mem::take(&mut st.origin.notify);
                        self.resolve_notify(notify, false, &mut out);
                        st.phase = Phase::Done;
                    }
                    Err(e) => {
                        let notify = std::mem::take(&mut st.origin.notify);
                        self.start_failed(notify, e, &mut out);
                        st.phase = Phase::Done;
                    }
                }
                self.keep(st);
            }
            Pending::StartDa { session, handle } => match (self.idx(&session), result) {
                (Some(i), Ok(_)) => {
                    let s = &mut self.sessions[i];
                    s.da_started = true;
                    let args = initialize_args(&s.debug_type);
                    send_dap(s, "initialize", args, Dap::Initialize, &mut out);
                }
                (Some(_), Err(e)) => self.fail_session(
                    &session,
                    format!("The debug adapter could not start: {e}"),
                    &mut out,
                ),
                // The session ended while its adapter was starting: do not leave the adapter running.
                (None, Ok(_)) => out.push(Out::Rpc(wire::stop_da_session(handle))),
                (None, Err(_)) => {}
            },
            Pending::ProvideActivation { id } => {
                let Some(mut p) = self.provides.remove(&id) else {
                    return out;
                };
                // A failed activation is not fatal, as for a start.
                p.waiting = p.waiting.saturating_sub(1);
                if p.waiting == 0 && p.activating {
                    p.activating = false;
                    let folder = self.workspace.clone();
                    let handles: Vec<i64> = self
                        .providers
                        .iter()
                        .filter(|q| q.has_provide && q.debug_type == p.debug_type)
                        .map(|q| q.handle)
                        .collect();
                    p.waiting = handles.len();
                    for handle in handles {
                        out.push(Out::RpcWith(
                            wire::provide_debug_configurations(handle, folder.as_ref()),
                            Pending::Provide { id },
                        ));
                    }
                }
                self.finish_provide(id, p, &mut out);
            }
            Pending::Provide { id } => {
                let Some(mut p) = self.provides.remove(&id) else {
                    return out;
                };
                if let Ok(Value::Array(configurations)) = result {
                    p.configurations.extend(configurations);
                }
                p.waiting = p.waiting.saturating_sub(1);
                self.finish_provide(id, p, &mut out);
            }
        }
        out
    }

    // ---- DAP from the adapter -----------------------------------------------------------------

    /// `$acceptDAMessage`.
    pub fn on_dap(&mut self, handle: i64, message: DapMessage) -> Vec<Out> {
        let mut out = Vec::new();
        let Some(i) = self.sessions.iter().position(|s| s.handle == handle) else {
            return out;
        };
        let sid = self.sessions[i].id.clone();
        match message {
            DapMessage::Response {
                request_seq,
                success,
                body,
                message,
                ..
            } => {
                if let Some(tag) = self.sessions[i].pending.remove(&request_seq) {
                    self.on_response(&sid, tag, success, body, message, &mut out);
                }
            }
            DapMessage::Event { event, body, .. } => self.on_event(&sid, &event, body, &mut out),
            DapMessage::Request {
                seq,
                command,
                arguments,
            } => self.on_reverse(&sid, seq, &command, arguments, &mut out),
        }
        out
    }

    fn parse_frames(&self, body: &Value) -> Vec<FrameInfo> {
        let Some(frames) = body.get("stackFrames").and_then(Value::as_array) else {
            return Vec::new();
        };
        frames
            .iter()
            .filter_map(|f| {
                let id = f.get("id")?.as_i64()?;
                let source = f.get("source");
                let uri = self.uri_of(source);
                Some(FrameInfo {
                    id,
                    name: str_of(f, "name").unwrap_or_default(),
                    uri,
                    source_name: source.and_then(|s| str_of(s, "name")),
                    line: u32_of(f, "line").unwrap_or(0),
                    column: u32_of(f, "column").unwrap_or(0),
                    hint: str_of(f, "presentationHint"),
                })
            })
            .collect()
    }

    fn on_response(
        &mut self,
        sid: &str,
        tag: Dap,
        success: bool,
        body: Value,
        message: Option<String>,
        out: &mut Vec<Out>,
    ) {
        let error = message.unwrap_or_else(|| "the debug adapter reported an error".to_owned());
        match tag {
            Dap::Initialize => {
                if !success {
                    self.fail_session(
                        sid,
                        format!("The debug adapter did not initialize: {error}"),
                        out,
                    );
                    return;
                }
                let Some(i) = self.idx(sid) else { return };
                let s = &mut self.sessions[i];
                merge_caps(&mut s.caps, &body);
                let filters = s.caps.get("exceptionBreakpointFilters").cloned();
                let (request, config) = (s.request.clone(), s.configuration.clone());
                send_dap(s, &request, config, Dap::Launch, out);
                if let Some(filters) = filters {
                    self.breakpoints.set_filters(&filters);
                    self.emit_breakpoints(out);
                }
            }
            Dap::Launch => {
                if !success {
                    self.fail_session(sid, error, out);
                    return;
                }
                let Some(i) = self.idx(sid) else { return };
                let s = &mut self.sessions[i];
                merge_caps(&mut s.caps, &body);
                if s.state == SessionState::Initializing {
                    s.state = SessionState::Running;
                }
                let announce = !s.announced;
                s.announced = true;
                let notify = std::mem::take(&mut s.notify);
                if announce {
                    out.push(Out::Rpc(wire::accept_debug_session_started(
                        &self.session_ref(i),
                    )));
                    if self.active.as_deref() == Some(sid) {
                        out.push(Out::Rpc(wire::accept_debug_session_active_changed(Some(
                            &self.session_ref(i),
                        ))));
                    }
                }
                self.resolve_notify(notify, true, out);
                self.emit_sessions(out);
            }
            Dap::SetBreakpoints { ids, initial } => {
                if success {
                    let answers = body.get("breakpoints").cloned().unwrap_or(Value::Null);
                    self.breakpoints.apply_source_response(&ids, &answers);
                    self.emit_breakpoints(out);
                }
                if initial {
                    self.initial_request_answered(sid, out);
                }
            }
            Dap::SetFunctionBreakpoints { ids, initial } => {
                if success {
                    let answers = body.get("breakpoints").cloned().unwrap_or(Value::Null);
                    self.breakpoints.apply_function_response(&ids, &answers);
                    self.emit_breakpoints(out);
                }
                if initial {
                    self.initial_request_answered(sid, out);
                }
            }
            Dap::SetExceptionBreakpoints { initial } => {
                if initial {
                    self.initial_request_answered(sid, out);
                }
            }
            Dap::ConfigurationDone => {
                if let Some(i) = self.idx(sid) {
                    send_dap(
                        &mut self.sessions[i],
                        "threads",
                        Value::Null,
                        Dap::Threads,
                        out,
                    );
                }
            }
            Dap::Threads => {
                if !success {
                    return;
                }
                let Some(i) = self.idx(sid) else { return };
                let s = &mut self.sessions[i];
                let stopped_id = match &s.state {
                    SessionState::Stopped { thread_id, .. } => Some(*thread_id),
                    _ => None,
                };
                let threads: Vec<ThreadInfo> = body
                    .get("threads")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|t| {
                                let id = t.get("id")?.as_i64()?;
                                let stopped = match stopped_id {
                                    Some(stopped) => s.all_stopped || stopped == Some(id),
                                    None => false,
                                };
                                Some(ThreadInfo {
                                    id,
                                    name: str_of(t, "name").unwrap_or_default(),
                                    stopped,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                s.threads = threads.clone();
                if s.stack_after_threads {
                    s.stack_after_threads = false;
                    if let Some(first) = threads.first() {
                        s.thread_id = Some(first.id);
                        request_stack(s, first.id, out);
                    }
                }
                out.push(Out::Ui(DebugUpdate::Threads {
                    session: sid.to_owned(),
                    threads,
                }));
            }
            Dap::StackTrace { thread_id, epoch } => {
                if !success {
                    return;
                }
                let frames = self.parse_frames(&body);
                let Some(i) = self.idx(sid) else { return };
                let s = &mut self.sessions[i];
                if s.stop_epoch != epoch {
                    return; // the thread ran on since this was asked
                }
                s.thread_id = Some(thread_id);
                s.frame_id = frames.first().map(|f| f.id);
                if let Some(top) = frames.first() {
                    send_dap(
                        s,
                        "scopes",
                        json!({ "frameId": top.id }),
                        Dap::Scopes { frame_id: top.id },
                        out,
                    );
                    out.push(Out::Rpc(wire::accept_stack_frame_focus(
                        sid,
                        thread_id,
                        Some(top.id),
                    )));
                }
                let total_frames = body.get("totalFrames").and_then(Value::as_i64);
                out.push(Out::Ui(DebugUpdate::CallStack {
                    session: sid.to_owned(),
                    thread_id,
                    frames,
                    total_frames,
                }));
                self.emit_sessions(out);
            }
            Dap::Scopes { frame_id } => {
                if !success {
                    return;
                }
                let scopes = parse_scopes(&body);
                out.push(Out::Ui(DebugUpdate::Scopes {
                    session: sid.to_owned(),
                    frame_id,
                    scopes,
                }));
            }
            Dap::Variables { reference } => {
                let variables = if success { parse_variables(&body) } else { Vec::new() };
                let error = (!success).then_some(error);
                out.push(Out::Ui(DebugUpdate::Variables {
                    session: sid.to_owned(),
                    reference,
                    variables,
                    error,
                }));
            }
            Dap::Evaluate { id } => {
                let result = if success {
                    Ok(EvalResult {
                        result: str_of(&body, "result").unwrap_or_default(),
                        type_name: str_of(&body, "type"),
                        variables_reference: body
                            .get("variablesReference")
                            .and_then(Value::as_i64)
                            .unwrap_or(0),
                    })
                } else {
                    Err(error)
                };
                out.push(Out::Ui(DebugUpdate::Evaluated {
                    id,
                    session: sid.to_owned(),
                    result,
                }));
            }
            Dap::HoverScopes { id } => {
                let scopes: Vec<(i64, bool)> = if success {
                    parse_scopes(&body)
                        .iter()
                        .map(|s| (s.variables_reference, s.expensive))
                        .collect()
                } else {
                    Vec::new()
                };
                let Some(search) = self.hovers.get_mut(&id) else { return };
                let (steps, done) = search.scopes(&scopes);
                self.hover_step(sid, id, steps, done, out);
            }
            Dap::HoverVariables { id, depth } => {
                let variables = if success { parse_variables(&body) } else { Vec::new() };
                let Some(search) = self.hovers.get_mut(&id) else { return };
                let (steps, done) = search.variables(depth, &variables);
                self.hover_step(sid, id, steps, done, out);
            }
            Dap::SetVariable { reference, name } => {
                let result = if success {
                    Ok(str_of(&body, "value").unwrap_or_default())
                } else {
                    Err(error)
                };
                out.push(Out::Ui(DebugUpdate::VariableSet {
                    session: sid.to_owned(),
                    reference,
                    name,
                    result,
                }));
            }
            Dap::Step { epoch } => {
                // An adapter that does not send `continued` still resumed: the window shows it running.
                if let Some(i) = self.idx(sid) {
                    let s = &mut self.sessions[i];
                    if success
                        && s.stop_epoch == epoch
                        && matches!(s.state, SessionState::Stopped { .. })
                    {
                        s.state = SessionState::Running;
                        self.emit_sessions(out);
                    }
                }
            }
            Dap::Disconnect => self.finish_session(sid, out),
            Dap::Custom { req } => {
                let result = if success {
                    Ok(Reply::Json(body))
                } else {
                    Err(Some(js_error("Error", &error)))
                };
                out.push(Out::Respond { req, result });
            }
            Dap::Other => {}
        }
    }

    /// One of the breakpoint requests sent at `initialized` was answered; the last one releases
    /// `configurationDone`.
    fn initial_request_answered(&mut self, sid: &str, out: &mut Vec<Out>) {
        let Some(i) = self.idx(sid) else { return };
        let s = &mut self.sessions[i];
        s.bp_wait = s.bp_wait.saturating_sub(1);
        if s.bp_wait == 0 {
            Self::configuration_done(s, out);
        }
    }

    fn configuration_done(s: &mut Session, out: &mut Vec<Out>) {
        if s.config_done {
            return;
        }
        s.config_done = true;
        if s.cap("supportsConfigurationDoneRequest") {
            send_dap(
                s,
                "configurationDone",
                json!({}),
                Dap::ConfigurationDone,
                out,
            );
        } else {
            send_dap(s, "threads", Value::Null, Dap::Threads, out);
        }
    }

    fn on_event(&mut self, sid: &str, event: &str, body: Value, out: &mut Vec<Out>) {
        let Some(i) = self.idx(sid) else { return };
        match event {
            "initialized" => {
                let s = &mut self.sessions[i];
                s.ready = true;
                if s.no_debug() {
                    // Run Without Debugging: `sendAllBreakpoints` sends none.
                    Self::configuration_done(s, out);
                    return;
                }
                let mut wait = 0;
                for file in self.breakpoints.files() {
                    send_file(s, &self.breakpoints, &file, true, out);
                    wait += 1;
                }
                if send_functions(s, &self.breakpoints, true, out) {
                    wait += 1;
                }
                if send_exceptions(s, &self.breakpoints, true, out) {
                    wait += 1;
                }
                s.bp_wait = wait;
                if wait == 0 {
                    Self::configuration_done(s, out);
                }
            }
            "stopped" => {
                let s = &mut self.sessions[i];
                let thread_id = body.get("threadId").and_then(Value::as_i64);
                s.stop_epoch += 1;
                s.all_stopped = body
                    .get("allThreadsStopped")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                s.state = SessionState::Stopped {
                    reason: str_of(&body, "reason").unwrap_or_else(|| "unknown".to_owned()),
                    thread_id,
                    description: str_of(&body, "description"),
                    text: str_of(&body, "text"),
                };
                s.thread_id = thread_id;
                s.frame_id = None;
                send_dap(s, "threads", Value::Null, Dap::Threads, out);
                match thread_id {
                    Some(t) => request_stack(s, t, out),
                    None => s.stack_after_threads = true,
                }
                self.emit_sessions(out);
            }
            "continued" => {
                let s = &mut self.sessions[i];
                if matches!(s.state, SessionState::Stopped { .. }) {
                    s.state = SessionState::Running;
                    self.emit_sessions(out);
                }
            }
            "thread" => send_dap(
                &mut self.sessions[i],
                "threads",
                Value::Null,
                Dap::Threads,
                out,
            ),
            "output" => {
                let category = str_of(&body, "category").unwrap_or_else(|| "console".to_owned());
                if category == "telemetry" {
                    return;
                }
                let source = body.get("source");
                let uri = self.uri_of(source);
                let entry = ConsoleEntry {
                    category,
                    text: str_of(&body, "output").unwrap_or_default(),
                    uri,
                    line: u32_of(&body, "line"),
                };
                out.push(Out::Ui(DebugUpdate::Console {
                    session: Some(sid.to_owned()),
                    entry,
                }));
            }
            "breakpoint" => {
                let reason = str_of(&body, "reason").unwrap_or_default();
                if let Some(raw) = body.get("breakpoint") {
                    if self.breakpoints.apply_event(&reason, raw) {
                        self.emit_breakpoints(out);
                    }
                }
            }
            "capabilities" => {
                if let Some(caps) = body.get("capabilities") {
                    let s = &mut self.sessions[i];
                    merge_caps(&mut s.caps, caps);
                    if let Some(filters) = caps.get("exceptionBreakpointFilters") {
                        self.breakpoints.set_filters(filters);
                        self.emit_breakpoints(out);
                    }
                }
            }
            "terminated" => {
                // `restart` asks for an automatic restart (`restartSession(session, restartData)`):
                // no tasks run, and the value reaches the next `launch` or `attach` as
                // `__restart`, uninterpreted, as the protocol says.
                let restart = body.get("restart").filter(|r| !r.is_null() && **r != Value::Bool(false)).map(|r| {
                    let mut config = self.sessions[i].unresolved.clone();
                    if let Some(target) = config.as_object_mut() {
                        target.insert("__restart".to_owned(), r.clone());
                    }
                    Relaunch {
                        config,
                        tasks: Vec::new(),
                    }
                });
                self.disconnect(sid, Some(false), restart, None, out);
            }
            "exited" | "loadedSource" | "module" | "process" | "progressStart"
            | "progressUpdate" | "progressEnd" | "invalidated" | "memory" => {}
            _ => {
                let event = json!({ "event": event, "body": body });
                out.push(Out::Rpc(wire::accept_debug_session_custom_event(
                    &self.session_ref(i),
                    &event,
                )));
            }
        }
    }

    fn on_reverse(
        &mut self,
        sid: &str,
        seq: i64,
        command: &str,
        arguments: Value,
        out: &mut Vec<Out>,
    ) {
        let Some(i) = self.idx(sid) else { return };
        let handle = self.sessions[i].handle;
        let refuse = |why: &str| {
            Out::Rpc(wire::send_da_message(
                handle,
                &DapMessage::response(seq, command, Some(why), Value::Null),
            ))
        };
        match command {
            "runInTerminal" => out.push(refuse(
                "This window has no terminal to run the program in. Set \"console\": \"internalConsole\" in the launch configuration.",
            )),
            "startDebugging" => {
                let s = &self.sessions[i];
                let mut config = arguments.get("configuration").cloned().unwrap_or_else(|| json!({}));
                let name = str_of(&config, "name").unwrap_or_else(|| s.name.clone());
                let request = str_of(&arguments, "request").unwrap_or_else(|| "launch".to_owned());
                if let Some(object) = config.as_object_mut() {
                    object.insert("request".to_owned(), Value::String(request));
                    object.insert("type".to_owned(), Value::String(s.debug_type.clone()));
                    object.insert("name".to_owned(), Value::String(name));
                }
                let no_debug = s.configuration.get("noDebug").and_then(Value::as_bool).unwrap_or(false);
                let origin = Origin {
                    parent: Some(s.id.clone()),
                    folder: s.folder.clone(),
                    notify: Notify::Dap { handle, request_seq: seq },
                    no_debug,
                    vars: s.vars.clone(),
                    compound: s.compound,
                    skip_pre_launch: false,
                };
                self.begin_config(config, origin, out);
            }
            _ => out.push(refuse(&format!("unknown request '{command}'"))),
        }
    }

    /// `$acceptDAError`: the adapter failed; the session ends.
    pub fn on_da_error(&mut self, handle: i64, name: &str, message: &str) -> Vec<Out> {
        let mut out = Vec::new();
        if let Some(sid) = self
            .sessions
            .iter()
            .find(|s| s.handle == handle)
            .map(|s| s.id.clone())
        {
            self.console(
                Some(sid.as_str()),
                "stderr",
                format!("{name}: {message}"),
                &mut out,
            );
            self.finish_session(&sid, &mut out);
        }
        out
    }

    /// `$acceptDAExit`: the adapter process ended.
    pub fn on_da_exit(&mut self, handle: i64, code: Option<i64>) -> Vec<Out> {
        let mut out = Vec::new();
        if let Some(sid) = self
            .sessions
            .iter()
            .find(|s| s.handle == handle)
            .map(|s| s.id.clone())
        {
            if let Some(code) = code.filter(|c| *c != 0) {
                self.console(
                    Some(sid.as_str()),
                    "stderr",
                    format!("The debug adapter exited with code {code}."),
                    &mut out,
                );
            }
            self.finish_session(&sid, &mut out);
        }
        out
    }

    // ---- ending a session ---------------------------------------------------------------------

    fn fail_session(&mut self, sid: &str, message: String, out: &mut Vec<Out>) {
        self.console(Some(sid), "stderr", message.clone(), out);
        out.push(Out::Ui(DebugUpdate::StartFailed { message }));
        self.finish_session(sid, out);
    }

    /// Stop one session as the user or an extension asks (`session.terminate`): with a compound
    /// root of `stopAll`, the compound's other sessions stop too, once
    /// (`DebugCompoundRoot.sessionStopped`). A session that ends by itself does not stop them.
    fn stop_session(&mut self, sid: &str, waiter: Option<u32>, out: &mut Vec<Out>) {
        let root = self.idx(sid).and_then(|i| self.sessions[i].compound);
        self.disconnect(sid, None, None, waiter, out);
        let Some(root) = root else { return };
        if !self.stopped_compounds.insert(root) {
            return;
        }
        let others: Vec<String> = self
            .sessions
            .iter()
            .filter(|s| s.compound == Some(root) && s.id != sid && s.state != SessionState::Terminated)
            .map(|s| s.id.clone())
            .collect();
        for other in others {
            self.disconnect(&other, None, None, None, out);
        }
    }

    fn stop_all(&mut self, out: &mut Vec<Out>) {
        let ids: Vec<String> = self
            .sessions
            .iter()
            .filter(|s| s.state != SessionState::Terminated)
            .map(|s| s.id.clone())
            .collect();
        for id in ids {
            self.disconnect(&id, None, None, None, out);
        }
    }

    /// Ask the adapter to disconnect. `terminate` says whether the debuggee goes with it: by
    /// default a launched program is terminated and an attached one is left running. The session
    /// ends when the adapter answers, exits, or [`DISCONNECT_TIMEOUT`] passes.
    fn disconnect(
        &mut self,
        sid: &str,
        terminate: Option<bool>,
        restart: Option<Relaunch>,
        waiter: Option<u32>,
        out: &mut Vec<Out>,
    ) {
        let now = self.now;
        let Some(i) = self.idx(sid) else {
            if let Some(req) = waiter {
                out.push(Out::Respond {
                    req,
                    result: Err(Some(js_error("Error", "debug session not found"))),
                });
            }
            return;
        };
        let s = &mut self.sessions[i];
        if s.state == SessionState::Terminated {
            if let Some(req) = waiter {
                out.push(Out::Respond {
                    req,
                    result: Ok(Reply::Empty),
                });
            }
            return;
        }
        if let Some(ending) = s.ending.as_mut() {
            ending.waiters.extend(waiter);
            if restart.is_some() {
                ending.restart = restart;
            }
            return;
        }
        let waiters: Vec<u32> = waiter.into_iter().collect();
        if !s.da_started {
            // No adapter yet: nothing to disconnect.
            s.ending = Some(Ending {
                deadline: now,
                restart,
                waiters,
            });
            self.finish_session(sid, out);
            return;
        }
        let terminate =
            terminate.unwrap_or(s.request == "launch") && s.cap("supportTerminateDebuggee");
        // `terminate(true)` and `disconnect(true)` of a restart tell the adapter so.
        let restarting = restart.is_some();
        let args = if terminate {
            json!({ "restart": restarting, "terminateDebuggee": true })
        } else {
            json!({ "restart": restarting })
        };
        send_dap(s, "disconnect", args, Dap::Disconnect, out);
        s.ending = Some(Ending {
            deadline: now + DISCONNECT_TIMEOUT,
            restart,
            waiters,
        });
    }

    /// A restart the user asked for (`restartSession` without restart data). The restart's tasks
    /// come from the session's configuration: `preRestartTask` and `postDebugTask` through
    /// `runTask`, then `preLaunchTask` and `postRestartTask` through `runTaskAndCheckErrors`. An
    /// adapter with `supportsRestartRequest` runs them first and is then sent `restart`; any
    /// other session stops first, runs them, and starts its configuration again without running
    /// its `preLaunchTask` a second time. A task that fails stops the restart.
    fn restart(&mut self, sid: &str, out: &mut Vec<Out>) {
        let Some(i) = self.idx(sid) else { return };
        let s = &self.sessions[i];
        if s.state == SessionState::Terminated || s.ending.is_some() {
            return;
        }
        let tasks = restart_tasks(&s.configuration);
        let config = s.unresolved.clone();
        if s.da_started && s.cap("supportsRestartRequest") {
            let then = ChainThen::RestartRequest {
                session: sid.to_owned(),
            };
            self.run_chain(tasks, then, out);
            return;
        }
        self.disconnect(sid, None, Some(Relaunch { config, tasks }), None, out);
    }

    /// The end of a session: the extension host stops the adapter and hears of the end, waiting
    /// stops are answered, children end with their parent, and a restart starts again.
    fn finish_session(&mut self, sid: &str, out: &mut Vec<Out>) {
        let Some(i) = self.idx(sid) else { return };
        if self.sessions[i].state == SessionState::Terminated {
            return;
        }
        let announced = self.sessions[i].announced;
        let terminated_ref = announced.then(|| self.session_ref(i));
        let s = &mut self.sessions[i];
        s.state = SessionState::Terminated;
        s.pending.clear();
        self.hovers.retain(|_, h| h.session != sid);
        let notify = std::mem::take(&mut s.notify);
        let ending = s.ending.take();
        let (handle, da_started, parent, folder, vars, compound) = (
            s.handle,
            s.da_started,
            s.parent.clone(),
            s.folder.clone(),
            s.vars.clone(),
            s.compound,
        );
        let post_task = str_of(&s.configuration, "postDebugTask").filter(|t| !t.is_empty());
        if da_started {
            out.push(Out::Rpc(wire::stop_da_session(handle)));
        }
        // `onDidEndAdapter` runs the `postDebugTask` and waits for nothing but its failure; a
        // restart the user asked for runs it again among its tasks, as `restartSession` does.
        if let Some(label) = post_task.filter(|_| da_started) {
            out.push(Out::Ui(DebugUpdate::RunTask { label, start: None }));
        }
        if let Some(r) = terminated_ref {
            out.push(Out::Rpc(wire::accept_debug_session_terminated(&r)));
        }
        self.resolve_notify(notify, false, out);
        let children: Vec<String> = self
            .sessions
            .iter()
            .filter(|c| c.parent.as_deref() == Some(sid) && c.state != SessionState::Terminated)
            .map(|c| c.id.clone())
            .collect();
        let was_active = self.active.as_deref() == Some(sid);
        if was_active {
            self.active = self
                .sessions
                .iter()
                .find(|c| c.id != sid && c.state != SessionState::Terminated)
                .map(|c| c.id.clone());
        }
        // The ended session is in the list once, so the window can show how it ended.
        self.emit_sessions(out);
        self.sessions.retain(|c| c.id != sid);
        if was_active {
            let next = self
                .active
                .as_deref()
                .and_then(|a| self.idx(a))
                .map(|j| self.session_ref(j));
            out.push(Out::Rpc(wire::accept_debug_session_active_changed(
                next.as_ref(),
            )));
        }
        if self.sessions.is_empty() {
            self.breakpoints.clear_adapter_data();
            self.emit_breakpoints(out);
        }
        if let Some(ending) = ending {
            for req in ending.waiters {
                out.push(Out::Respond {
                    req,
                    result: Ok(Reply::Empty),
                });
            }
            if let Some(Relaunch { config, tasks }) = ending.restart {
                // `launchOrAttachToSession` after the tasks: the `preLaunchTask` ran among them,
                // or not at all for a restart the adapter asked for.
                let origin = Origin {
                    parent,
                    folder,
                    notify: Notify::Nobody,
                    no_debug: false,
                    vars,
                    compound,
                    skip_pre_launch: true,
                };
                self.run_chain(tasks, ChainThen::Relaunch { config, origin }, out);
            }
        }
        for child in children {
            self.disconnect(&child, None, None, None, out);
        }
    }

    /// Sessions whose disconnect went unanswered for [`DISCONNECT_TIMEOUT`] end now.
    pub fn tick(&mut self, now: Instant) -> Vec<Out> {
        let mut out = Vec::new();
        self.now = now;
        let due: Vec<String> = self
            .sessions
            .iter()
            .filter(|s| s.ending.as_ref().is_some_and(|e| e.deadline <= now))
            .map(|s| s.id.clone())
            .collect();
        for id in due {
            self.finish_session(&id, &mut out);
        }
        out
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.sessions
            .iter()
            .filter_map(|s| s.ending.as_ref().map(|e| e.deadline))
            .min()
    }

    // ---- requests of the extension host (`MainThreadDebugService`) ----------------------------

    /// `$startDebugging(folder, nameOrConfig, options)`: answered with `true` once the adapter
    /// answered `launch`, `false` when the start was cancelled or failed.
    pub fn ext_start_debugging(
        &mut self,
        req: u32,
        folder: Option<UriComponents>,
        name_or_config: Value,
        options: &Value,
        env: &Env,
    ) -> Vec<Out> {
        let mut out = Vec::new();
        let selector = match name_or_config {
            Value::String(name) => ConfigSelector::Name(name),
            config @ Value::Object(_) => ConfigSelector::Config(config),
            _ => {
                out.push(Out::Respond {
                    req,
                    result: Err(Some(js_error(
                        "Error",
                        "cannot start debugging: no configuration",
                    ))),
                });
                return out;
            }
        };
        let parent = str_of(options, "parentSessionID");
        // `createSession`: without `noDebug` in the options, a child runs as its parent does.
        let no_debug = match options.get("noDebug").and_then(Value::as_bool) {
            Some(no_debug) => no_debug,
            None => parent
                .as_deref()
                .and_then(|p| self.idx(p))
                .is_some_and(|i| self.sessions[i].no_debug()),
        };
        // A child shares its parent's compound root (`compoundRoot: parentSession?.compoundRoot`).
        let compound = parent
            .as_deref()
            .and_then(|p| self.idx(p))
            .and_then(|i| self.sessions[i].compound);
        let mut origin = self.origin(parent, Notify::Ext { req }, folder, no_debug, env);
        origin.compound = compound;
        self.begin_selected(selector, origin, &mut out);
        out
    }

    /// `$stopDebugging(sessionId | undefined)`.
    pub fn ext_stop_debugging(&mut self, req: u32, session: Option<String>) -> Vec<Out> {
        let mut out = Vec::new();
        match session {
            Some(id) => self.stop_session(&id, Some(req), &mut out),
            None => {
                self.stop_all(&mut out);
                out.push(Out::Respond {
                    req,
                    result: Ok(Reply::Empty),
                });
            }
        }
        out
    }

    /// `$customDebugAdapterRequest(id, command, args)`: the adapter's body, or an error.
    pub fn ext_custom_request(
        &mut self,
        req: u32,
        sid: &str,
        command: &str,
        args: Value,
    ) -> Vec<Out> {
        let mut out = Vec::new();
        match self.idx(sid) {
            Some(i) => send_dap(
                &mut self.sessions[i],
                command,
                args,
                Dap::Custom { req },
                &mut out,
            ),
            None => out.push(Out::Respond {
                req,
                result: Err(Some(js_error("Error", "debug session not found"))),
            }),
        }
        out
    }

    /// `$getDebugProtocolBreakpoint(id, breakpointId)`: the adapter's own description of one.
    pub fn ext_protocol_breakpoint(
        &mut self,
        req: u32,
        sid: &str,
        breakpoint_id: &str,
    ) -> Vec<Out> {
        if self.idx(sid).is_none() {
            return vec![Out::Respond {
                req,
                result: Err(Some(js_error("Error", "debug session not found"))),
            }];
        }
        let raw = self
            .breakpoints
            .find_source(breakpoint_id)
            .map(|b| b.raw.clone())
            .or_else(|| {
                self.breakpoints
                    .functions
                    .iter()
                    .find(|b| b.id == breakpoint_id)
                    .map(|b| b.raw.clone())
            })
            .filter(|raw| !raw.is_null());
        let reply = raw.map_or(Reply::Empty, Reply::Json);
        vec![Out::Respond {
            req,
            result: Ok(reply),
        }]
    }

    /// `$setDebugSessionName`.
    pub fn ext_set_session_name(&mut self, sid: &str, name: String) -> Vec<Out> {
        let mut out = Vec::new();
        if let Some(i) = self.idx(sid) {
            self.sessions[i].name = name.clone();
            out.push(Out::Rpc(wire::accept_debug_session_name_changed(
                &self.session_ref(i),
                &name,
            )));
            self.emit_sessions(&mut out);
        }
        out
    }

    /// `$appendDebugConsole`: text an extension prints to the console of the session in focus.
    pub fn ext_append_console(&mut self, text: String) -> Vec<Out> {
        let mut out = Vec::new();
        let session = self.active.clone();
        self.console(session.as_deref(), "important", text, &mut out);
        out
    }
}
