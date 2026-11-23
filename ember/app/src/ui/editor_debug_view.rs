//! The Run and Debug tool window and the debug console (FR-L31.42).
//!
//! Code-OSS draws these in the debug viewlet (`variablesView.ts`, `watchExpressionsView.ts`,
//! `callStackView.ts`, `breakpointsView.ts`) and the panel's `repl.ts`. The session layer
//! (`ember_editor::debugger`) loads the threads, the stack and the scopes of the top frame when a
//! thread stops; this window asks for the variables of the scopes it shows expanded, evaluates
//! the watch expressions and the console input, selects frames and threads, and edits the
//! breakpoint registry. When the folder has no `launch.json` it offers to create one from the
//! debuggers the running extensions contribute, as the viewlet's welcome view does.
//!
//! The window opens the way Code-OSS's defaults open it: the debug console on the first session
//! start (`debug.internalConsoleOptions: openOnFirstSessionStart`) and Run and Debug when a
//! thread stops (`debug.openDebug: openOnDebugBreak`).

use std::collections::{HashMap, HashSet};

use dioxus_compose::html::prelude::dioxus_elements::Key;
use dioxus_compose::html::prelude::*;
use dioxus_compose::prelude::{Props, component};
use dioxus_core::{EventHandler, use_hook};
use dioxus_signals::{ReadableExt, Signal, WritableExt};
use ember_editor::debugger::launch::{DebuggerContribution, initial_launch_json};
use ember_editor::debugger::{
    DebugRequest, DebugUpdate, EvalContext, EvalResult, ExceptionFilter, FrameInfo,
    FunctionBreakpoint, ScopeInfo, SessionInfo, SessionState, SourceBreakpoint, ThreadInfo,
    VariableInfo,
};
use ember_editor_conn::uri::UriComponents;
use serde_json::Value;

use super::editor::ToolWindow;
use super::editor_workbench::{
    PendingGoto, Shared,
    page::{icon, icon_tone},
};
use crate::editor_host::HostCommand;

/// How many lines the console keeps; older ones are dropped first.
pub(crate) const CONSOLE_LIMIT: usize = 1000;
/// Evaluation ids of this window start here, so they never meet ids another part of the window
/// gives [`DebugRequest::Evaluate`].
const EVAL_BASE: u64 = 1 << 40;
/// Separates the parts of a tree path (scope, then variable names).
const SEP: char = '\u{1f}';

/// What an evaluation answers.
#[derive(Debug, Clone, PartialEq)]
enum EvalFor {
    Watch(u64),
    Console,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Watch {
    pub id: u64,
    pub expression: String,
    /// `None` while there is no stopped frame to evaluate it in, or the answer has not come.
    pub result: Option<Result<EvalResult, String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LineKind {
    /// Adapter output, by DAP category (`console`, `stdout`, `stderr`, `important`).
    Output(String),
    /// What the user typed.
    Input,
    /// The answer to an input.
    Result(Result<EvalResult, String>),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ConsoleLine {
    pub id: u64,
    pub kind: LineKind,
    pub text: String,
    pub uri: Option<UriComponents>,
    pub line: Option<u32>,
}

/// What an update asks the window to do besides drawing.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Effects {
    pub requests: Vec<DebugRequest>,
    pub show_console: bool,
    pub show_view: bool,
    /// A provider answered the `launch.json` being created: (type, what it gave).
    pub create_launch: Option<(String, Vec<Value>)>,
}

/// One row of the variables, watch or console trees.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VarRow {
    pub path: String,
    pub depth: usize,
    pub name: String,
    pub value: String,
    pub type_name: Option<String>,
    pub session: String,
    pub reference: i64,
    pub expanded: bool,
    pub error: Option<String>,
}

impl VarRow {
    pub fn expandable(&self) -> bool {
        self.reference > 0
    }
}

/// One row of the call stack.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum StackRow {
    Session {
        name: String,
        state: String,
    },
    Thread {
        session: String,
        thread: ThreadInfo,
        selected: bool,
    },
    Frame {
        session: String,
        thread_id: i64,
        frame: FrameInfo,
        selected: bool,
    },
    More {
        total: i64,
        shown: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct DebugView {
    configurations: usize,
    configuration_error: Option<String>,
    listed: bool,
    sessions: Vec<SessionInfo>,
    active: Option<String>,
    started_once: bool,
    threads: HashMap<String, Vec<ThreadInfo>>,
    stacks: HashMap<(String, i64), (Vec<FrameInfo>, Option<i64>)>,
    scopes: HashMap<(String, i64), Vec<ScopeInfo>>,
    variables: HashMap<(String, i64), Result<Vec<VariableInfo>, String>>,
    ref_path: HashMap<(String, i64), String>,
    expanded: HashSet<String>,
    seen_scopes: HashSet<String>,
    watches: Vec<Watch>,
    next_watch: u64,
    watched_frame: Option<(String, i64)>,
    console: Vec<ConsoleLine>,
    next_line: u64,
    history: Vec<String>,
    history_at: Option<usize>,
    pending: HashMap<u64, EvalFor>,
    next_eval: u64,
    source: Vec<SourceBreakpoint>,
    functions: Vec<FunctionBreakpoint>,
    filters: Vec<ExceptionFilter>,
    creating: Option<String>,
}

fn stopped(s: &SessionInfo) -> bool {
    matches!(s.state, SessionState::Stopped { .. })
}

fn state_label(state: &SessionState) -> String {
    match state {
        SessionState::Initializing => "시작하는 중".into(),
        SessionState::Running => "실행 중".into(),
        SessionState::Stopped { reason, .. } => format!("일시 중지됨: {}", stop_reason(reason)),
        SessionState::Terminated => "종료됨".into(),
    }
}

/// The reason of a `stopped` event as Code-OSS words it (`debugSession.ts`, `callStackView.ts`).
fn stop_reason(reason: &str) -> String {
    match reason {
        "step" => "단계".into(),
        "breakpoint" => "중단점".into(),
        "exception" => "예외".into(),
        "pause" => "일시 중지".into(),
        "entry" => "진입".into(),
        "goto" => "이동".into(),
        "function breakpoint" => "함수 중단점".into(),
        "data breakpoint" => "데이터 중단점".into(),
        "instruction breakpoint" => "명령 중단점".into(),
        other => other.to_owned(),
    }
}

impl DebugView {
    fn eval_id(&mut self) -> u64 {
        self.next_eval += 1;
        EVAL_BASE + self.next_eval
    }

    fn session(&self, id: &str) -> Option<&SessionInfo> {
        self.sessions.iter().find(|s| s.id == id)
    }

    /// The active session when one of its threads is stopped: (session, thread, frame).
    pub fn focus(&self) -> Option<(String, Option<i64>, Option<i64>)> {
        let s = self.session(self.active.as_deref()?)?;
        stopped(s).then(|| (s.id.clone(), s.thread_id, s.frame_id))
    }

    /// The session the console evaluates in: the active one while it runs.
    fn console_session(&self) -> Option<&SessionInfo> {
        self.session(self.active.as_deref()?)
            .filter(|s| s.state != SessionState::Terminated)
    }

    pub fn has_launch_json(&self) -> bool {
        self.configurations > 0 || self.configuration_error.is_some()
    }

    pub fn listed(&self) -> bool {
        self.listed
    }

    pub fn configuration_error(&self) -> Option<&str> {
        self.configuration_error.as_deref()
    }

    pub fn creating(&self) -> Option<&str> {
        self.creating.as_deref()
    }

    pub fn watches(&self) -> &[Watch] {
        &self.watches
    }

    pub fn console(&self) -> &[ConsoleLine] {
        &self.console
    }

    pub fn source_breakpoints(&self) -> &[SourceBreakpoint] {
        &self.source
    }

    pub fn function_breakpoints(&self) -> &[FunctionBreakpoint] {
        &self.functions
    }

    pub fn exception_filters(&self) -> &[ExceptionFilter] {
        &self.filters
    }

    fn drop_session_data(&mut self, id: &str) {
        self.threads.remove(id);
        self.stacks.retain(|(s, _), _| s != id);
        self.scopes.retain(|(s, _), _| s != id);
        self.variables.retain(|(s, _), _| s != id);
        self.ref_path.retain(|(s, _), _| s != id);
    }

    /// Follow the session layer. Returns the requests the window should send and the windows it
    /// should show.
    pub fn apply(&mut self, update: &DebugUpdate) -> Effects {
        let mut fx = Effects::default();
        match update {
            DebugUpdate::Configurations {
                configurations,
                error,
            } => {
                self.configurations = configurations.len();
                self.configuration_error.clone_from(error);
                self.listed = true;
            }
            DebugUpdate::ProvidedConfigurations {
                debug_type,
                configurations,
            } => {
                if self.creating.as_deref() == Some(debug_type.as_str()) {
                    self.creating = None;
                    fx.create_launch = Some((debug_type.clone(), configurations.clone()));
                }
            }
            DebugUpdate::Sessions { sessions, active } => {
                self.sessions_changed(sessions, active, &mut fx)
            }
            DebugUpdate::Threads { session, threads } => {
                self.threads.insert(session.clone(), threads.clone());
            }
            DebugUpdate::CallStack {
                session,
                thread_id,
                frames,
                total_frames,
            } => {
                self.stacks.insert(
                    (session.clone(), *thread_id),
                    (frames.clone(), *total_frames),
                );
            }
            DebugUpdate::Scopes {
                session,
                frame_id,
                scopes,
            } => {
                self.scopes
                    .insert((session.clone(), *frame_id), scopes.clone());
                // Code-OSS expands the first scope that is not expensive the first time it is
                // shown; after that the user's choice is kept.
                let any_known = scopes.iter().any(|sc| self.seen_scopes.contains(&sc.name));
                let first_cheap = scopes
                    .iter()
                    .find(|sc| !sc.expensive && sc.variables_reference > 0)
                    .filter(|_| !any_known);
                if let Some(first) = first_cheap {
                    self.expanded.insert(first.name.clone());
                }
                for sc in scopes {
                    self.seen_scopes.insert(sc.name.clone());
                    if sc.variables_reference > 0 {
                        let key = (session.clone(), sc.variables_reference);
                        self.ref_path.insert(key.clone(), sc.name.clone());
                        if self.expanded.contains(&sc.name) && !self.variables.contains_key(&key) {
                            fx.requests.push(DebugRequest::Variables {
                                session: session.clone(),
                                reference: sc.variables_reference,
                            });
                        }
                    }
                }
            }
            DebugUpdate::Variables {
                session,
                reference,
                variables,
                error,
            } => {
                let key = (session.clone(), *reference);
                let value = match error {
                    Some(e) => Err(e.clone()),
                    None => Ok(variables.clone()),
                };
                self.variables.insert(key.clone(), value);
                if let Some(parent) = self.ref_path.get(&key).cloned() {
                    for v in variables.iter().filter(|v| v.variables_reference > 0) {
                        let path = format!("{parent}{SEP}{}", v.name);
                        let child = (session.clone(), v.variables_reference);
                        self.ref_path.insert(child.clone(), path.clone());
                        if self.expanded.contains(&path) && !self.variables.contains_key(&child) {
                            fx.requests.push(DebugRequest::Variables {
                                session: session.clone(),
                                reference: v.variables_reference,
                            });
                        }
                    }
                }
            }
            DebugUpdate::Evaluated {
                id,
                session,
                result,
            } => {
                let Some(target) = self.pending.remove(id) else {
                    return fx;
                };
                let path = match target {
                    EvalFor::Watch(wid) => {
                        let Some(w) = self.watches.iter_mut().find(|w| w.id == wid) else {
                            return fx;
                        };
                        w.result = Some(result.clone());
                        format!("watch:{wid}")
                    }
                    EvalFor::Console => {
                        let lid = self.push_line(
                            LineKind::Result(result.clone()),
                            String::new(),
                            None,
                            None,
                        );
                        format!("console:{lid}")
                    }
                };
                if let Some(r) = result.as_ref().ok().filter(|r| r.variables_reference > 0) {
                    let key = (session.clone(), r.variables_reference);
                    self.ref_path.insert(key.clone(), path.clone());
                    if self.expanded.contains(&path) && !self.variables.contains_key(&key) {
                        fx.requests.push(DebugRequest::Variables {
                            session: session.clone(),
                            reference: r.variables_reference,
                        });
                    }
                }
            }
            DebugUpdate::Console { entry, .. } => {
                // Code-OSS sends telemetry output to its telemetry service, not to the console.
                if entry.category != "telemetry" {
                    self.push_output(&entry.category, &entry.text, entry.uri.clone(), entry.line);
                }
            }
            DebugUpdate::Breakpoints {
                source,
                functions,
                exception_filters,
            } => {
                self.source.clone_from(source);
                self.functions.clone_from(functions);
                self.filters.clone_from(exception_filters);
            }
            DebugUpdate::StartFailed { message } => {
                self.push_output("important", message, None, None);
            }
        }
        fx
    }

    fn sessions_changed(
        &mut self,
        sessions: &[SessionInfo],
        active: &Option<String>,
        fx: &mut Effects,
    ) {
        let was_stopped = self
            .active
            .as_deref()
            .and_then(|id| self.session(id))
            .is_some_and(stopped);
        let previous_active = self.active.clone();
        let started = sessions
            .iter()
            .any(|s| s.state != SessionState::Terminated && self.session(&s.id).is_none());
        if started && !self.started_once {
            self.started_once = true;
            fx.show_console = true;
        }
        for s in sessions {
            if !stopped(s) {
                self.drop_session_data(&s.id);
            }
        }
        let gone: Vec<String> = self
            .sessions
            .iter()
            .filter(|old| !sessions.iter().any(|s| s.id == old.id))
            .map(|s| s.id.clone())
            .collect();
        for id in gone {
            self.drop_session_data(&id);
        }
        self.sessions = sessions
            .iter()
            .filter(|s| s.state != SessionState::Terminated)
            .cloned()
            .collect();
        self.active = active
            .clone()
            .filter(|id| self.sessions.iter().any(|s| &s.id == id));
        let now_stopped = self.focus().is_some();
        if now_stopped && (!was_stopped || previous_active != self.active) {
            fx.show_view = true;
        }
        // Watch expressions are evaluated again in each frame the window focuses.
        match self.focus() {
            Some((session, _, Some(frame))) => {
                let key = (session.clone(), frame);
                if self.watched_frame.as_ref() != Some(&key) {
                    self.watched_frame = Some(key);
                    let ids: Vec<u64> = self.watches.iter().map(|w| w.id).collect();
                    for wid in ids {
                        if let Some(request) = self.evaluate_watch(wid) {
                            fx.requests.push(request);
                        }
                    }
                }
            }
            _ => {
                self.watched_frame = None;
                for w in &mut self.watches {
                    w.result = None;
                }
            }
        }
    }

    fn evaluate_watch(&mut self, wid: u64) -> Option<DebugRequest> {
        let (session, _, frame) = self.focus()?;
        let expression = self
            .watches
            .iter()
            .find(|w| w.id == wid)?
            .expression
            .clone();
        let id = self.eval_id();
        self.pending.insert(id, EvalFor::Watch(wid));
        Some(DebugRequest::Evaluate {
            id,
            session,
            frame_id: frame,
            expression,
            context: EvalContext::Watch,
        })
    }

    fn push_line(
        &mut self,
        kind: LineKind,
        text: String,
        uri: Option<UriComponents>,
        line: Option<u32>,
    ) -> u64 {
        self.next_line += 1;
        let id = self.next_line;
        self.console.push(ConsoleLine {
            id,
            kind,
            text,
            uri,
            line,
        });
        if self.console.len() > CONSOLE_LIMIT {
            let extra = self.console.len() - CONSOLE_LIMIT;
            self.console.drain(..extra);
        }
        id
    }

    /// Output joins the line before it while that line has no line break yet, as the REPL's
    /// `appendToRepl` does with simple output of one category.
    fn push_output(
        &mut self,
        category: &str,
        text: &str,
        uri: Option<UriComponents>,
        line: Option<u32>,
    ) {
        let joins = |last: &&mut ConsoleLine| {
            matches!(&last.kind, LineKind::Output(c) if c == category)
                && !last.text.ends_with('\n')
                && last.uri.is_none()
        };
        if let Some(last) = self.console.last_mut().filter(joins) {
            last.text.push_str(text);
            return;
        }
        self.push_line(
            LineKind::Output(category.to_owned()),
            text.to_owned(),
            uri,
            line,
        );
    }

    // ---- what the user does -----------------------------------------------------------------

    /// Expand or collapse a tree row; the variables of a row opened for the first time are asked for.
    pub fn toggle(&mut self, row: &VarRow) -> Option<DebugRequest> {
        if !self.expanded.remove(&row.path) {
            self.expanded.insert(row.path.clone());
            let key = (row.session.clone(), row.reference);
            if row.reference > 0 && !self.variables.contains_key(&key) {
                self.ref_path.insert(key, row.path.clone());
                return Some(DebugRequest::Variables {
                    session: row.session.clone(),
                    reference: row.reference,
                });
            }
        }
        None
    }

    /// Add a watch expression; it is evaluated at once in the stopped frame, if there is one.
    pub fn add_watch(&mut self, expression: &str) -> Option<DebugRequest> {
        let expression = expression.trim();
        if expression.is_empty() {
            return None;
        }
        self.next_watch += 1;
        let id = self.next_watch;
        self.watches.push(Watch {
            id,
            expression: expression.to_owned(),
            result: None,
        });
        self.evaluate_watch(id)
    }

    pub fn remove_watch(&mut self, id: u64) {
        self.watches.retain(|w| w.id != id);
        self.expanded.remove(&format!("watch:{id}"));
    }

    pub fn remove_all_watches(&mut self) {
        self.watches.clear();
    }

    /// Evaluate console input in the active session, at its focused frame (`repl.ts`
    /// `acceptReplInput`). Without a session the console says so.
    pub fn submit(&mut self, input: &str) -> Option<DebugRequest> {
        if input.trim().is_empty() {
            return None;
        }
        if self.history.last().map(String::as_str) != Some(input) {
            self.history.push(input.to_owned());
        }
        self.history_at = None;
        self.push_line(LineKind::Input, input.to_owned(), None, None);
        let Some(session) = self.console_session() else {
            self.push_line(
                LineKind::Result(Err(
                    "실행 중인 디버그 세션이 없어 식을 계산할 수 없습니다.".to_owned()
                )),
                String::new(),
                None,
                None,
            );
            return None;
        };
        let (session, frame_id) = (session.id.clone(), session.frame_id);
        let id = self.eval_id();
        self.pending.insert(id, EvalFor::Console);
        Some(DebugRequest::Evaluate {
            id,
            session,
            frame_id,
            expression: input.to_owned(),
            context: EvalContext::Repl,
        })
    }

    /// The input before the one shown (↑ in the console), or `None` at the oldest.
    pub fn history_back(&mut self) -> Option<String> {
        let at = match self.history_at {
            None => self.history.len().checked_sub(1)?,
            Some(0) => return None,
            Some(i) => i - 1,
        };
        self.history_at = Some(at);
        self.history.get(at).cloned()
    }

    /// The input after the one shown (↓), or an empty field past the newest.
    pub fn history_forward(&mut self) -> Option<String> {
        let at = self.history_at? + 1;
        if at >= self.history.len() {
            self.history_at = None;
            return Some(String::new());
        }
        self.history_at = Some(at);
        self.history.get(at).cloned()
    }

    pub fn clear_console(&mut self) {
        self.console.clear();
    }

    /// Start creating `launch.json` for a debugger: its providers are asked first
    /// (`ConfigurationManager.guessDebugger` then `Launch.openConfigFile`).
    pub fn begin_create(&mut self, debug_type: &str) -> DebugRequest {
        self.creating = Some(debug_type.to_owned());
        DebugRequest::ProvideConfigurations {
            debug_type: debug_type.to_owned(),
        }
    }

    // ---- what the window draws --------------------------------------------------------------

    fn children(
        &self,
        session: &str,
        reference: i64,
        path: &str,
        depth: usize,
        out: &mut Vec<VarRow>,
    ) {
        match self.variables.get(&(session.to_owned(), reference)) {
            None => {}
            Some(Err(error)) => out.push(VarRow {
                path: format!("{path}{SEP}!"),
                depth,
                name: String::new(),
                value: String::new(),
                type_name: None,
                session: session.to_owned(),
                reference: 0,
                expanded: false,
                error: Some(error.clone()),
            }),
            Some(Ok(vars)) => {
                for v in vars {
                    let child = format!("{path}{SEP}{}", v.name);
                    let expanded = v.variables_reference > 0 && self.expanded.contains(&child);
                    out.push(VarRow {
                        path: child.clone(),
                        depth,
                        name: v.name.clone(),
                        value: v.value.clone(),
                        type_name: v.type_name.clone(),
                        session: session.to_owned(),
                        reference: v.variables_reference,
                        expanded,
                        error: None,
                    });
                    if expanded {
                        self.children(session, v.variables_reference, &child, depth + 1, out);
                    }
                }
            }
        }
    }

    /// The scopes of the focused frame and the variables of the expanded ones.
    pub fn variable_rows(&self) -> Vec<VarRow> {
        let mut out = Vec::new();
        let Some((session, _, Some(frame))) = self.focus() else {
            return out;
        };
        let Some(scopes) = self.scopes.get(&(session.clone(), frame)) else {
            return out;
        };
        for sc in scopes {
            let expanded = sc.variables_reference > 0 && self.expanded.contains(&sc.name);
            out.push(VarRow {
                path: sc.name.clone(),
                depth: 0,
                name: sc.name.clone(),
                value: String::new(),
                type_name: None,
                session: session.clone(),
                reference: sc.variables_reference,
                expanded,
                error: None,
            });
            if expanded {
                self.children(&session, sc.variables_reference, &sc.name, 1, &mut out);
            }
        }
        out
    }

    /// The children of a watch or a console result that is expanded.
    pub fn result_children(&self, path: &str, result: &EvalResult) -> Vec<VarRow> {
        let mut out = Vec::new();
        if result.variables_reference > 0 && self.expanded.contains(path) {
            let session = self
                .ref_path
                .iter()
                .find(|((_, r), p)| *r == result.variables_reference && p.as_str() == path)
                .map(|((s, _), _)| s.clone());
            if let Some(session) = session {
                self.children(&session, result.variables_reference, path, 1, &mut out);
            }
        }
        out
    }

    pub fn is_expanded(&self, path: &str) -> bool {
        self.expanded.contains(path)
    }

    /// The session the watch and console results were evaluated in (for their expansion).
    pub fn result_session(&self, path: &str) -> Option<String> {
        self.ref_path
            .iter()
            .find(|(_, p)| p.as_str() == path)
            .map(|((s, _), _)| s.clone())
    }

    pub fn stack_rows(&self) -> Vec<StackRow> {
        let mut out = Vec::new();
        let many = self.sessions.len() > 1;
        for s in &self.sessions {
            if many {
                out.push(StackRow::Session {
                    name: s.name.clone(),
                    state: state_label(&s.state),
                });
            }
            if self.active.as_deref() != Some(s.id.as_str()) && many {
                continue;
            }
            let threads = self.threads.get(&s.id).cloned().unwrap_or_default();
            let show_threads = threads.len() > 1;
            for t in &threads {
                let selected = s.thread_id == Some(t.id);
                if show_threads {
                    out.push(StackRow::Thread {
                        session: s.id.clone(),
                        thread: t.clone(),
                        selected,
                    });
                }
                if let Some((frames, total)) = self.stacks.get(&(s.id.clone(), t.id)) {
                    for f in frames {
                        out.push(StackRow::Frame {
                            session: s.id.clone(),
                            thread_id: t.id,
                            frame: f.clone(),
                            selected: s.frame_id == Some(f.id),
                        });
                    }
                    if let Some(total) = total.filter(|t| *t > frames.len() as i64) {
                        out.push(StackRow::More {
                            total,
                            shown: frames.len(),
                        });
                    }
                }
            }
            if threads.is_empty() && !many {
                out.push(StackRow::Session {
                    name: s.name.clone(),
                    state: state_label(&s.state),
                });
            }
        }
        out
    }
}

/// The disclosure arrow of a tree row, as the explorer tree draws it.
fn chevron(open: bool) -> Element {
    if open {
        icon_tone("chev-dn", 12, "muted")
    } else {
        icon_tone("chev", 12, "muted")
    }
}

/// The base name of a path.
fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The folder of a breakpoint's file, relative to the project folder, for its row.
fn relative_dir(path: &str, folder: &str) -> String {
    let folder = folder.trim_end_matches('/');
    let dir = path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    dir.strip_prefix(folder)
        .map(|rest| rest.trim_start_matches('/').to_owned())
        .unwrap_or_else(|| dir.to_owned())
}

/// The text of a breakpoint row's hover: its condition, hit count and log message.
fn breakpoint_detail(
    condition: Option<&str>,
    hit: Option<&str>,
    log: Option<&str>,
    message: Option<&str>,
) -> String {
    let mut parts = Vec::new();
    if let Some(c) = condition {
        parts.push(format!("조건: {c}"));
    }
    if let Some(h) = hit {
        parts.push(format!("적중 횟수: {h}"));
    }
    if let Some(l) = log {
        parts.push(format!("로그 메시지: {l}"));
    }
    if let Some(m) = message {
        parts.push(m.to_owned());
    }
    parts.join("\n")
}

// ---- glue -----------------------------------------------------------------------------------------

/// Show a tool window without hiding it when it is already shown.
fn show(s: &Shared, w: ToolWindow) {
    let mut stripes = s.stripes;
    if !stripes.read().is_selected(w) {
        stripes.write().show(w);
    }
}

pub(crate) fn on_update(s: &Shared, update: &ember_editor::SessionUpdate) {
    let fx = match update {
        ember_editor::SessionUpdate::Debug(update) => {
            let mut view = s.debug_view;
            view.write().apply(update)
        }
        ember_editor::SessionUpdate::ConnectionLost { .. } => {
            let mut view = s.debug_view;
            let keep = {
                let v = view.read();
                (v.watches.clone(), v.history.clone(), v.console.clone())
            };
            let mut fresh = DebugView::default();
            (fresh.watches, fresh.history, fresh.console) = keep;
            view.set(fresh);
            return;
        }
        _ => return,
    };
    for request in fx.requests {
        s.host.send(HostCommand::Debug(request));
    }
    if fx.show_console {
        show(s, ToolWindow::DebugConsole);
    }
    if fx.show_view {
        show(s, ToolWindow::Debug);
    }
    if let Some((debug_type, provided)) = fx.create_launch {
        let initial = s
            .host
            .state
            .lock()
            .unwrap()
            .debuggers
            .iter()
            .find(|d| d.debug_type == debug_type)
            .map(|d| d.initial_configurations.clone())
            .unwrap_or_default();
        open_launch_json(s, initial_launch_json(&initial, &provided));
    }
}

/// Open `launch.json`, writing `text` first when the folder has none.
fn open_launch_json(s: &Shared, text: String) {
    let path = crate::editor_host::launch_json_path(&s.folder);
    let mut tabs = s.tabs;
    tabs.write().open(&path);
    s.host.send(HostCommand::OpenLaunchJson { text });
}

fn goto(s: &Shared, uri: &UriComponents, line: u32) {
    let path = uri.path.clone();
    super::editor_workbench::page::open_tab_file(s, &path);
    let mut next_goto_id = s.next_goto_id;
    let id = *next_goto_id.read();
    *next_goto_id.write() += 1;
    let mut pending_goto = s.pending_goto;
    pending_goto.set(Some(PendingGoto {
        id,
        path,
        line: line.saturating_sub(1),
        col: 0,
    }));
}

/// What a click in the window asks for.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ViewAction {
    Debug(DebugRequest),
    Goto {
        uri: UriComponents,
        line: u32,
    },
    /// Open `launch.json` (it exists) from the header's gear.
    OpenLaunch,
    /// Show the 확장 window to find a debugger.
    FindDebuggers,
    Hide,
}

fn act(s: &Shared, action: ViewAction) {
    match action {
        ViewAction::Debug(request) => {
            s.host.send(HostCommand::Debug(request));
        }
        ViewAction::Goto { uri, line } => goto(s, &uri, line),
        ViewAction::OpenLaunch => open_launch_json(s, initial_launch_json(&[], &[])),
        ViewAction::FindDebuggers => show(s, ToolWindow::Extensions),
        ViewAction::Hide => {
            let mut stripes = s.stripes;
            stripes.write().hide(super::editor::Side::Left);
        }
    }
}

/// The Run and Debug tool window.
pub(crate) fn panel(s: &Shared) -> Element {
    let connected = s.host.state.lock().unwrap().connected;
    let debuggers = s.host.state.lock().unwrap().debuggers.clone();
    let folder = s.folder.clone();
    let shared = s.clone();
    rsx! {
        RunDebugWindow {
            view: s.debug_view,
            debuggers,
            connected,
            folder,
            act: move |action| act(&shared, action),
        }
    }
}

/// The debug console tab of the bottom tool window.
pub(crate) fn console_body(s: &Shared) -> Element {
    let shared = s.clone();
    rsx! {
        DebugConsole { view: s.debug_view, act: move |action| act(&shared, action) }
    }
}

// ---- components -----------------------------------------------------------------------------

#[component]
fn Section(
    id: String,
    title: String,
    open: bool,
    toggle: EventHandler<()>,
    actions: Element,
    children: Element,
) -> Element {
    rsx! {
        div { class: "ed-dbg-sec",
            div { class: "ed-dbg-sech",
                button { id: "{id}", class: "ed-dbg-sect", "aria-expanded": open,
                    onclick: move |_| toggle.call(()),
                    {chevron(open)}
                    b { "{title}" }
                }
                div { class: "spacer" }
                {actions}
            }
            if open { div { class: "ed-dbg-secb", {children} } }
        }
    }
}

#[component]
fn VarRows(
    view: Signal<DebugView>,
    rows: Vec<VarRow>,
    act: EventHandler<ViewAction>,
    prefix: String,
) -> Element {
    let mut view = view;
    rsx! {
        for (i, row) in rows.into_iter().enumerate() {
            {
                let depth = row.depth;
                let label = match (&row.error, row.value.is_empty()) {
                    (Some(e), _) => e.clone(),
                    (None, true) => row.name.clone(),
                    (None, false) => format!("{} = {}", row.name, row.value),
                };
                let title = row.type_name.clone().unwrap_or_default();
                let toggle_row = row.clone();
                rsx! {
                    div { key: "{row.path}", id: "{prefix}-{i}", class: "ed-tn ed-dbg-var", role: "treeitem",
                        "aria-expanded": if row.expandable() { "{row.expanded}" } else { "" },
                        "aria-label": "{label}", title: "{title}", style: "--d:{depth}",
                        onclick: move |_| {
                            if toggle_row.expandable() {
                                let request = view.write().toggle(&toggle_row);
                                if let Some(request) = request { act.call(ViewAction::Debug(request)); }
                            }
                        },
                        if row.expandable() { {chevron(row.expanded)} } else { span { class: "chev-sp" } }
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
}

#[component]
fn RunDebugWindow(
    view: Signal<DebugView>,
    debuggers: Vec<DebuggerContribution>,
    connected: bool,
    folder: String,
    act: EventHandler<ViewAction>,
) -> Element {
    let mut view = view;
    let open = use_hook(|| Signal::new([true, true, true, true]));
    let mut watch_input = use_hook(|| Signal::new(None::<String>));
    let mut function_input = use_hook(|| Signal::new(None::<String>));
    let sections = *open.read();
    let v = view.read();
    let has_launch = v.has_launch_json();
    let listed = v.listed();
    let creating = v.creating().map(str::to_owned);
    let configuration_error = v.configuration_error().map(str::to_owned);
    let variable_rows = v.variable_rows();
    let focus = v.focus();
    let watches: Vec<(Watch, Vec<VarRow>, bool)> = v
        .watches()
        .iter()
        .map(|w| {
            let path = format!("watch:{}", w.id);
            let children = match &w.result {
                Some(Ok(r)) => v.result_children(&path, r),
                _ => Vec::new(),
            };
            (w.clone(), children, v.is_expanded(&path))
        })
        .collect();
    let no_watches = watches.is_empty();
    let stack = v.stack_rows();
    let filters = v.exception_filters().to_vec();
    let functions = v.function_breakpoints().to_vec();
    let mut source = v.source_breakpoints().to_vec();
    source.sort_by(|a, b| a.uri.path.cmp(&b.uri.path).then(a.line.cmp(&b.line)));
    let breakpoint_ids: Vec<String> = source
        .iter()
        .map(|b| b.id.clone())
        .chain(functions.iter().map(|b| b.id.clone()))
        .collect();
    let no_breakpoints = source.is_empty() && functions.is_empty() && filters.is_empty();
    drop(v);
    let toggle_section = move |i: usize| {
        let mut open = open;
        let mut s = *open.read();
        s[i] = !s[i];
        open.set(s);
    };
    rsx! {
        section { class: "ed-tw ed-left ed-dbg", "aria-label": "실행 및 디버그",
            div { class: "ed-twh",
                b { class: "ed-twt", "실행 및 디버그" }
                div { class: "spacer" }
                if has_launch {
                    button { id: "debug-open-launch", class: "ed-ib", title: "launch.json 열기", "aria-label": "launch.json 열기",
                        disabled: !connected, onclick: move |_| act.call(ViewAction::OpenLaunch), {icon("edit")} }
                }
                button { class: "ed-ib", title: "숨기기", "aria-label": "실행 및 디버그 창 숨기기",
                    onclick: move |_| act.call(ViewAction::Hide), {icon("close")} }
            }
            div { class: "ed-dbg-body",
                if let Some(error) = configuration_error {
                    div { class: "ed-note ed-dbg-error", role: "status", "{error}" }
                }
                if listed && !has_launch {
                    div { class: "ed-dbg-welcome", id: "debug-welcome",
                        p { "실행 구성을 사용자 지정하려면 launch.json 파일을 만드세요." }
                        if let Some(t) = &creating {
                            p { class: "ed-dbg-muted", role: "status", "{t} 구성을 만드는 중입니다." }
                        } else if debuggers.is_empty() {
                            p { class: "ed-dbg-muted", "디버거 확장이 설치되어 있지 않습니다." }
                            button { id: "debug-find-debuggers", class: "ed-pj", onclick: move |_| act.call(ViewAction::FindDebuggers), "디버거 확장 찾기" }
                        } else {
                            p { class: "ed-dbg-muted", "디버거를 선택하세요." }
                            for (i, d) in debuggers.iter().cloned().enumerate() {
                                {
                                    let debug_type = d.debug_type.clone();
                                    rsx! {
                                        button { key: "{d.debug_type}", id: "debug-create-{i}", class: "ed-pj", disabled: !connected,
                                            onclick: move |_| {
                                                let request = view.write().begin_create(&debug_type);
                                                act.call(ViewAction::Debug(request));
                                            },
                                            "{d.label}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Section { id: "debug-sec-variables", title: "변수", open: sections[0], toggle: move |_| toggle_section(0), actions: rsx! {},
                    if variable_rows.is_empty() {
                        div { class: "ed-dbg-muted ed-dbg-pad", if focus.is_some() { "변수를 불러오는 중입니다." } else { "일시 중지된 스레드가 없습니다." } }
                    } else {
                        div { role: "tree", "aria-label": "변수",
                            VarRows { view, rows: variable_rows, act, prefix: "debug-var" }
                        }
                    }
                }
                Section { id: "debug-sec-watch", title: "조사식", open: sections[1], toggle: move |_| toggle_section(1),
                    actions: rsx! {
                        button { id: "debug-watch-add", class: "ed-ib", title: "식 추가", "aria-label": "식 추가",
                            onclick: move |_| { watch_input.set(Some(String::new())); if !sections[1] { toggle_section(1); } }, {icon("plus")} }
                        button { class: "ed-ib", title: "모든 식 제거", "aria-label": "모든 식 제거", disabled: no_watches,
                            onclick: move |_| view.write().remove_all_watches(), {icon("trash")} }
                    },
                    div { role: "tree", "aria-label": "조사식",
                        for (w, children, expanded) in watches.into_iter() {
                            {
                                let path = format!("watch:{}", w.id);
                                let (value, reference, error) = match &w.result {
                                    None => ("사용할 수 없음".to_owned(), 0, false),
                                    Some(Ok(r)) => (r.result.clone(), r.variables_reference, false),
                                    Some(Err(e)) => (e.clone(), 0, true),
                                };
                                let session = view.read().result_session(&path).or_else(|| focus.as_ref().map(|f| f.0.clone())).unwrap_or_default();
                                let row = VarRow { path: path.clone(), depth: 0, name: w.expression.clone(), value: value.clone(), type_name: None, session, reference, expanded, error: None };
                                let wid = w.id;
                                let value_class = if error { "ed-dbg-err ellipsis" } else { "ed-dbg-val ellipsis" };
                                rsx! {
                                    div { key: "{path}", id: "debug-watch-{wid}", class: "ed-tn ed-dbg-var", role: "treeitem", style: "--d:0",
                                        "aria-label": "{w.expression} = {value}",
                                        onclick: move |_| {
                                            if row.expandable() {
                                                let request = view.write().toggle(&row);
                                                if let Some(request) = request { act.call(ViewAction::Debug(request)); }
                                            }
                                        },
                                        if reference > 0 { {chevron(expanded)} } else { span { class: "chev-sp" } }
                                        span { class: "ed-dbg-name", "{w.expression}" }
                                        span { class: "{value_class}", "{value}" }
                                        div { class: "spacer" }
                                        button { id: "debug-watch-remove-{wid}", class: "ed-ib ed-dbg-rowbtn", title: "식 제거", "aria-label": "식 제거",
                                            onclick: move |e| { e.stop_propagation(); view.write().remove_watch(wid); }, {icon("close")} }
                                    }
                                    VarRows { view, rows: children, act, prefix: "debug-watch-{wid}-var" }
                                }
                            }
                        }
                    }
                    if let Some(text) = watch_input.read().clone() {
                        form { class: "ed-dbg-form",
                            onsubmit: move |event| {
                                event.prevent_default();
                                let text = watch_input.read().clone().unwrap_or_default();
                                let request = view.write().add_watch(&text);
                                if let Some(request) = request { act.call(ViewAction::Debug(request)); }
                                watch_input.set(None);
                            },
                            input { id: "debug-watch-input", class: "ed-dbg-input", placeholder: "조사할 식", value: "{text}", autofocus: true,
                                oninput: move |e| watch_input.set(Some(e.value())),
                                onkeydown: move |e| { if matches!(e.key(), Key::Escape) { watch_input.set(None); } } }
                            button { id: "debug-watch-save", r#type: "submit", class: "ed-dbg-hidden", "추가" }
                        }
                    }
                }
                Section { id: "debug-sec-stack", title: "호출 스택", open: sections[2], toggle: move |_| toggle_section(2), actions: rsx! {},
                    if stack.is_empty() {
                        div { class: "ed-dbg-muted ed-dbg-pad", "실행 중인 디버그 세션이 없습니다." }
                    }
                    div { role: "tree", "aria-label": "호출 스택",
                        for (i, row) in stack.into_iter().enumerate() {
                            {
                                match row {
                                    StackRow::Session { name, state } => rsx! {
                                        div { key: "s{i}", class: "ed-tn ed-dbg-stack", style: "--d:0",
                                            span { class: "ed-dbg-name", "{name}" }
                                            span { class: "ed-dbg-muted ellipsis", "{state}" }
                                        }
                                    },
                                    StackRow::Thread { session, thread, selected } => {
                                        let class = if selected { "ed-tn ed-dbg-stack sel" } else { "ed-tn ed-dbg-stack" };
                                        let state = if thread.stopped { "일시 중지됨" } else { "실행 중" };
                                        let thread_id = thread.id;
                                        rsx! {
                                            div { key: "t{i}", id: "debug-thread-{thread_id}", class, role: "treeitem", style: "--d:0",
                                                onclick: move |_| act.call(ViewAction::Debug(DebugRequest::SelectThread { session: session.clone(), thread_id })),
                                                span { class: "ed-dbg-name", "{thread.name}" }
                                                span { class: "ed-dbg-muted ellipsis", "{state}" }
                                            }
                                        }
                                    }
                                    StackRow::Frame { session, thread_id, frame, selected } => {
                                        let class = match (selected, frame.hint.as_deref()) {
                                            (true, _) => "ed-tn ed-dbg-stack sel",
                                            (false, Some("deemphasize" | "subtle")) => "ed-tn ed-dbg-stack muted",
                                            _ => "ed-tn ed-dbg-stack",
                                        };
                                        let place = match (&frame.source_name, &frame.uri) {
                                            (Some(name), _) => format!("{name}:{}", frame.line),
                                            (None, Some(uri)) => format!("{}:{}", base_name(&uri.path), frame.line),
                                            (None, None) => String::new(),
                                        };
                                        let frame_id = frame.id;
                                        let line = frame.line;
                                        let uri = frame.uri.clone();
                                        rsx! {
                                            div { key: "f{i}", id: "debug-frame-{frame_id}", class, role: "treeitem", style: "--d:1",
                                                "aria-selected": selected, "aria-label": "{frame.name} {place}",
                                                onclick: move |_| {
                                                    act.call(ViewAction::Debug(DebugRequest::SelectFrame { session: session.clone(), thread_id, frame_id }));
                                                    if let Some(uri) = &uri { act.call(ViewAction::Goto { uri: uri.clone(), line }); }
                                                },
                                                span { class: "ed-dbg-name ellipsis", "{frame.name}" }
                                                div { class: "spacer" }
                                                span { class: "ed-dbg-place", "{place}" }
                                            }
                                        }
                                    }
                                    StackRow::More { total, shown } => rsx! {
                                        div { key: "m{i}", class: "ed-tn ed-dbg-stack muted", style: "--d:1", "프레임 {shown}/{total}개 표시" }
                                    },
                                }
                            }
                        }
                    }
                }
                Section { id: "debug-sec-breakpoints", title: "중단점", open: sections[3], toggle: move |_| toggle_section(3),
                    actions: rsx! {
                        button { id: "debug-function-add", class: "ed-ib", title: "함수 중단점 추가", "aria-label": "함수 중단점 추가", disabled: !connected,
                            onclick: move |_| { function_input.set(Some(String::new())); if !sections[3] { toggle_section(3); } }, {icon("plus")} }
                        button { id: "debug-breakpoints-clear", class: "ed-ib", title: "모든 중단점 제거", "aria-label": "모든 중단점 제거", disabled: breakpoint_ids.is_empty() || !connected,
                            onclick: move |_| { for id in &breakpoint_ids { act.call(ViewAction::Debug(DebugRequest::RemoveBreakpoint { id: id.clone() })); } }, {icon("trash")} }
                    },
                    if no_breakpoints && function_input.read().is_none() {
                        div { class: "ed-dbg-muted ed-dbg-pad", "중단점이 없습니다. 편집기 여백을 눌러 추가하세요." }
                    }
                    div { role: "list", "aria-label": "중단점",
                        for f in filters.into_iter() {
                            {
                                let enabled = f.enabled;
                                let filter = f.filter.clone();
                                rsx! {
                                    div { key: "x{f.filter}", class: "ed-tn ed-dbg-bp", role: "listitem", style: "--d:0",
                                        button { id: "debug-filter-{f.filter}", class: "ed-dbg-check", role: "checkbox", "aria-checked": enabled, "aria-label": "{f.label}", disabled: !connected,
                                            onclick: move |_| act.call(ViewAction::Debug(DebugRequest::SetExceptionFilter { filter: filter.clone(), enabled: !enabled })),
                                            if enabled { {icon("check")} }
                                        }
                                        span { class: "ellipsis", "{f.label}" }
                                    }
                                }
                            }
                        }
                        for b in functions.into_iter() {
                            {
                                let detail = breakpoint_detail(b.condition.as_deref(), b.hit_condition.as_deref(), b.log_message.as_deref(), b.message.as_deref());
                                let edit = DebugRequest::EditBreakpoint { id: b.id.clone(), condition: b.condition.clone(), hit_condition: b.hit_condition.clone(), log_message: b.log_message.clone(), enabled: !b.enabled };
                                let remove = DebugRequest::RemoveBreakpoint { id: b.id.clone() };
                                let enabled = b.enabled;
                                rsx! {
                                    div { key: "f{b.id}", class: "ed-tn ed-dbg-bp", role: "listitem", style: "--d:0", title: "{detail}",
                                        button { id: "debug-bp-check-{b.id}", class: "ed-dbg-check", role: "checkbox", "aria-checked": enabled, "aria-label": "{b.name}", disabled: !connected,
                                            onclick: move |_| act.call(ViewAction::Debug(edit.clone())),
                                            if enabled { {icon("check")} }
                                        }
                                        span { class: "ed-dbg-name ellipsis", "{b.name}" }
                                        div { class: "spacer" }
                                        button { id: "debug-bp-remove-{b.id}", class: "ed-ib ed-dbg-rowbtn", title: "중단점 제거", "aria-label": "중단점 제거", disabled: !connected,
                                            onclick: move |_| act.call(ViewAction::Debug(remove.clone())), {icon("close")} }
                                    }
                                }
                            }
                        }
                        for b in source.into_iter() {
                            {
                                let detail = breakpoint_detail(b.condition.as_deref(), b.hit_condition.as_deref(), b.log_message.as_deref(), b.message.as_deref());
                                let edit = DebugRequest::EditBreakpoint { id: b.id.clone(), condition: b.condition.clone(), hit_condition: b.hit_condition.clone(), log_message: b.log_message.clone(), enabled: !b.enabled };
                                let remove = DebugRequest::RemoveBreakpoint { id: b.id.clone() };
                                let enabled = b.enabled;
                                let name = base_name(&b.uri.path).to_owned();
                                let dir = relative_dir(&b.uri.path, &folder);
                                let line = b.adapter_line.unwrap_or(b.line);
                                let unverified = !b.verified;
                                let uri = b.uri.clone();
                                let class = if unverified { "ed-tn ed-dbg-bp unverified" } else { "ed-tn ed-dbg-bp" };
                                rsx! {
                                    div { key: "b{b.id}", id: "debug-bp-{b.id}", class, role: "listitem", style: "--d:0", title: "{detail}",
                                        onclick: move |_| act.call(ViewAction::Goto { uri: uri.clone(), line }),
                                        button { id: "debug-bp-check-{b.id}", class: "ed-dbg-check", role: "checkbox", "aria-checked": enabled, "aria-label": "{name}:{line}", disabled: !connected,
                                            onclick: move |e| { e.stop_propagation(); act.call(ViewAction::Debug(edit.clone())); },
                                            if enabled { {icon("check")} }
                                        }
                                        span { class: "ed-dbg-name", "{name}" }
                                        span { class: "ed-dbg-muted ellipsis", "{dir}" }
                                        div { class: "spacer" }
                                        span { class: "ed-dbg-place", "{line}" }
                                        button { id: "debug-bp-remove-{b.id}", class: "ed-ib ed-dbg-rowbtn", title: "중단점 제거", "aria-label": "중단점 제거", disabled: !connected,
                                            onclick: move |e| { e.stop_propagation(); act.call(ViewAction::Debug(remove.clone())); }, {icon("close")} }
                                    }
                                }
                            }
                        }
                    }
                    if let Some(text) = function_input.read().clone() {
                        form { class: "ed-dbg-form",
                            onsubmit: move |event| {
                                event.prevent_default();
                                let name = function_input.read().clone().unwrap_or_default();
                                if !name.trim().is_empty() {
                                    act.call(ViewAction::Debug(DebugRequest::AddFunctionBreakpoint { name: name.trim().to_owned() }));
                                }
                                function_input.set(None);
                            },
                            input { id: "debug-function-input", class: "ed-dbg-input", placeholder: "함수 이름", value: "{text}", autofocus: true,
                                oninput: move |e| function_input.set(Some(e.value())),
                                onkeydown: move |e| { if matches!(e.key(), Key::Escape) { function_input.set(None); } } }
                            button { id: "debug-function-save", r#type: "submit", class: "ed-dbg-hidden", "추가" }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn DebugConsole(view: Signal<DebugView>, act: EventHandler<ViewAction>) -> Element {
    let mut view = view;
    let mut input = use_hook(|| Signal::new(String::new()));
    let v = view.read();
    let lines: Vec<(ConsoleLine, Vec<VarRow>, bool)> = v
        .console()
        .iter()
        .map(|l| {
            let path = format!("console:{}", l.id);
            let children = match &l.kind {
                LineKind::Result(Ok(r)) => v.result_children(&path, r),
                _ => Vec::new(),
            };
            (l.clone(), children, v.is_expanded(&path))
        })
        .collect();
    drop(v);
    let text = input.read().clone();
    rsx! {
        div { class: "ed-dbg-console",
            div { class: "ed-dbg-ctools",
                div { class: "spacer" }
                button { id: "debug-console-clear", class: "ed-ib", title: "콘솔 지우기", "aria-label": "콘솔 지우기",
                    onclick: move |_| view.write().clear_console(), {icon("trash")} }
            }
            div { class: "ed-term ed-dbg-lines", role: "log", "aria-label": "디버그 콘솔 출력",
                for (line, children, expanded) in lines.into_iter() {
                    {
                        let id = line.id;
                        match &line.kind {
                            LineKind::Output(category) => {
                                let class = match category.as_str() {
                                    "stderr" => "tl err",
                                    "important" => "tl warn",
                                    "console" => "tl ed-dbg-muted",
                                    _ => "tl",
                                };
                                let text = line.text.trim_end_matches('\n').to_owned();
                                let place = match (&line.uri, line.line) {
                                    (Some(uri), Some(n)) => Some((uri.clone(), n)),
                                    _ => None,
                                };
                                rsx! {
                                    div { key: "{id}", class,
                                        "{text}"
                                        if let Some((uri, n)) = place {
                                            {
                                                let label = format!("{}:{n}", base_name(&uri.path));
                                                rsx! { button { class: "ed-dbg-link", onclick: move |_| act.call(ViewAction::Goto { uri: uri.clone(), line: n }), "{label}" } }
                                            }
                                        }
                                    }
                                }
                            }
                            LineKind::Input => rsx! {
                                div { key: "{id}", class: "tl ed-dbg-in", span { class: "pr", "› " } "{line.text}" }
                            },
                            LineKind::Result(Err(e)) => rsx! {
                                div { key: "{id}", class: "tl err", "{e}" }
                            },
                            LineKind::Result(Ok(r)) => {
                                let path = format!("console:{id}");
                                let session = view.read().result_session(&path).unwrap_or_default();
                                let row = VarRow { path: path.clone(), depth: 0, name: String::new(), value: r.result.clone(), type_name: r.type_name.clone(), session, reference: r.variables_reference, expanded, error: None };
                                let reference = r.variables_reference;
                                let value = r.result.clone();
                                rsx! {
                                    div { key: "{id}", id: "debug-console-result-{id}", class: "tl ed-dbg-res",
                                        onclick: move |_| {
                                            if row.expandable() {
                                                let request = view.write().toggle(&row);
                                                if let Some(request) = request { act.call(ViewAction::Debug(request)); }
                                            }
                                        },
                                        if reference > 0 { {chevron(expanded)} }
                                        "{value}"
                                    }
                                    div { class: "ed-tree ed-dbg-restree",
                                        VarRows { view, rows: children, act, prefix: "debug-console-{id}-var" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            form { class: "ed-tin ed-dbg-cin",
                onsubmit: move |event| {
                    event.prevent_default();
                    let text = input.read().clone();
                    let request = view.write().submit(&text);
                    if let Some(request) = request { act.call(ViewAction::Debug(request)); }
                    input.set(String::new());
                },
                span { class: "pr", "› " }
                input { id: "debug-console-input", "aria-label": "디버그 콘솔에서 계산할 식", value: "{text}",
                    oninput: move |e| input.set(e.value()),
                    onkeydown: move |e| {
                        let next = match e.key() {
                            Key::ArrowUp => view.write().history_back(),
                            Key::ArrowDown => view.write().history_forward(),
                            _ => None,
                        };
                        if let Some(next) = next { e.prevent_default(); input.set(next); }
                    }
                }
                button { id: "debug-console-submit", r#type: "submit", class: "ed-dbg-hidden", "계산" }
            }
        }
    }
}

#[cfg(test)]
#[path = "editor_debug_view/tests.rs"]
mod tests;
