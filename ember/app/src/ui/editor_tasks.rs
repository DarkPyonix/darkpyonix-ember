//! FR-L31.16, #448: 실행 in the Ember 에디터 window, the `tasks.json` tasks of Code-OSS.
//!
//! The project's `.vscode/tasks.json` (version `2.0.0`, JSONC) is read through the editor
//! session's file channel, so it is the file on the project's computer. A `shell` or `process`
//! task runs in a new terminal on that computer through the node terminal relay (`POST
//! /api/computers/{id}/terms` with a `program`), the same relay as the terminal panel. Code-OSS
//! (`terminalTaskSystem.ts`) decides how a task becomes a command line: a `shell` task with no
//! `args` runs its `command` as written; with `args` each part is quoted when it needs it; a
//! `process` task runs `command` with `args` as an argv, with no shell.
//!
//! Not supported yet (the row says so and does not run): tasks of other types (task providers
//! contributed by extensions, `npm`, `typescript`, ...), `dependsOn`, and variables other than the
//! workspace and file ones. `problemMatcher`, `presentation` and the `windows` / `osx` / `linux`
//! overrides are read past, not applied.

use std::collections::BTreeMap;

use serde_json::Value;

/// Where Code-OSS reads the workspace tasks from, below the project folder.
pub fn tasks_json_path(folder: &str) -> String {
    format!("{}/.vscode/tasks.json", folder.trim_end_matches('/'))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    Shell,
    Process,
}

/// A part of a command line: the text and how Code-OSS quotes it (`quoting` of a `{ value, quoting }` arg).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub value: String,
    pub quoting: Quoting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quoting {
    /// A plain string: quoted with the strong quote when it holds a space or a shell character.
    Auto,
    Escape,
    Strong,
    Weak,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskDef {
    pub label: String,
    pub kind: TaskKind,
    pub command: Part,
    pub args: Vec<Part>,
    pub cwd: Option<String>,
    pub env: BTreeMap<String, String>,
    /// `group` (`build`, `test`, ...).
    pub group: Option<String>,
    pub is_default: bool,
    /// Why this task cannot run here yet, in Korean.
    pub unsupported: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TasksLoad {
    /// Not read yet.
    Loading,
    /// The project has no `.vscode/tasks.json`.
    NoFile,
    /// The file could not be read or parsed (the reason in Korean).
    Failed(String),
    Parsed(Vec<TaskDef>),
}

impl TasksLoad {
    /// From the read of `tasks.json`.
    pub fn from_read(read: Result<String, String>) -> Self {
        match read {
            Ok(text) => match parse_tasks_json(&text) {
                Ok(tasks) => Self::Parsed(tasks),
                Err(e) => Self::Failed(e),
            },
            Err(e) => {
                let l = e.to_lowercase();
                if l.contains("entrynotfound") || l.contains("enoent") || l.contains("not found") || l.contains("no such file") {
                    Self::NoFile
                } else {
                    Self::Failed(format!("tasks.json을 읽지 못했습니다: {}", crate::ui::editor::file_failure_reason(&e)))
                }
            }
        }
    }

    /// What the 실행 window says instead of a list, if anything.
    pub fn message(&self) -> Option<String> {
        match self {
            Self::Loading => Some("작업 구성을 읽는 중입니다.".into()),
            Self::NoFile => Some("실행할 작업이 없습니다. 프로젝트의 .vscode/tasks.json에 작업을 정의하면 이곳에 표시됩니다.".into()),
            Self::Failed(e) => Some(e.clone()),
            Self::Parsed(t) if t.is_empty() => Some("tasks.json에 작업이 없습니다.".into()),
            Self::Parsed(_) => None,
        }
    }
}

/// JSONC to JSON: `//` and `/* */` comments outside strings and trailing commas are removed,
/// as Code-OSS's JSON parser allows them in `tasks.json`.
pub fn strip_jsonc(text: &str) -> String {
    let chars: Vec<char> = text.trim_start_matches('\u{feff}').chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            out.push(c);
            i += 1;
            while i < chars.len() {
                out.push(chars[i]);
                if chars[i] == '\\' && i + 1 < chars.len() {
                    out.push(chars[i + 1]);
                    i += 2;
                    continue;
                }
                i += 1;
                if chars[i - 1] == '"' {
                    break;
                }
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
        } else if c == ',' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            // A comment between the comma and the bracket is removed on a later pass of the loop,
            // so look past it as well.
            let rest: String = chars[j..].iter().collect();
            let next = strip_leading_comments(&rest);
            if !(next.starts_with('}') || next.starts_with(']')) {
                out.push(c);
            }
            i += 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

fn strip_leading_comments(mut s: &str) -> &str {
    loop {
        s = s.trim_start();
        if let Some(r) = s.strip_prefix("//") {
            s = r.find('\n').map_or("", |n| &r[n..]);
        } else if let Some(r) = s.strip_prefix("/*") {
            s = r.find("*/").map_or("", |n| &r[n + 2..]);
        } else {
            return s;
        }
    }
}

fn part_of(v: &Value) -> Option<Part> {
    match v {
        Value::String(s) => Some(Part { value: s.clone(), quoting: Quoting::Auto }),
        Value::Object(o) => {
            let value = match o.get("value")? {
                Value::String(s) => s.clone(),
                // Code-OSS joins an array value with spaces.
                Value::Array(a) => a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" "),
                _ => return None,
            };
            let quoting = match o.get("quoting").and_then(|q| q.as_str()) {
                Some("escape") => Quoting::Escape,
                Some("strong") => Quoting::Strong,
                Some("weak") => Quoting::Weak,
                _ => Quoting::Auto,
            };
            Some(Part { value, quoting })
        }
        _ => None,
    }
}

fn env_of(v: Option<&Value>) -> BTreeMap<String, String> {
    v.and_then(|e| e.as_object())
        .map(|o| o.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned()))).collect())
        .unwrap_or_default()
}

/// The tasks of a `tasks.json` text, in file order.
pub fn parse_tasks_json(text: &str) -> Result<Vec<TaskDef>, String> {
    let root: Value = serde_json::from_str(&strip_jsonc(text)).map_err(|e| format!("tasks.json을 해석하지 못했습니다: {e}"))?;
    let version = root.get("version").and_then(|v| v.as_str()).unwrap_or("2.0.0");
    if !version.starts_with('2') {
        return Err(format!("tasks.json 버전 {version}은 지원하지 않습니다. 버전 2.0.0 형식만 실행할 수 있습니다."));
    }
    let global_options = root.get("options");
    let global_cwd = global_options.and_then(|o| o.get("cwd")).and_then(|c| c.as_str()).map(str::to_owned);
    let global_env = env_of(global_options.and_then(|o| o.get("env")));
    let Some(list) = root.get("tasks").and_then(|t| t.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (index, t) in list.iter().enumerate() {
        let kind_name = t.get("type").and_then(|v| v.as_str()).unwrap_or("process");
        let label = t
            .get("label")
            .or_else(|| t.get("taskName"))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{kind_name}: 작업 {}", index + 1));
        let kind = match kind_name {
            "shell" => Some(TaskKind::Shell),
            "process" => Some(TaskKind::Process),
            _ => None,
        };
        let command = t.get("command").and_then(part_of);
        let args: Vec<Part> = t.get("args").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(part_of).collect()).unwrap_or_default();
        let options = t.get("options");
        let cwd = options.and_then(|o| o.get("cwd")).and_then(|c| c.as_str()).map(str::to_owned).or_else(|| global_cwd.clone());
        let mut env = global_env.clone();
        env.extend(env_of(options.and_then(|o| o.get("env"))));
        let (group, is_default) = match t.get("group") {
            Some(Value::String(g)) => (Some(g.clone()), false),
            Some(Value::Object(o)) => (
                o.get("kind").and_then(|k| k.as_str()).map(str::to_owned),
                o.get("isDefault").is_some_and(|d| d.as_bool() == Some(true) || d.is_string()),
            ),
            _ => (None, false),
        };
        let depends = t.get("dependsOn").is_some_and(|d| !d.is_null() && d.as_array().map_or(true, |a| !a.is_empty()));
        let unsupported = if kind.is_none() {
            Some(format!("{kind_name} 형식의 작업은 확장 작업 제공자가 필요해 아직 실행할 수 없습니다."))
        } else if depends {
            Some("dependsOn이 있는 작업은 아직 실행할 수 없습니다.".to_string())
        } else if command.as_ref().map_or(true, |c| c.value.trim().is_empty()) {
            Some("command가 없는 작업입니다.".to_string())
        } else {
            None
        };
        out.push(TaskDef {
            label,
            kind: kind.unwrap_or(TaskKind::Process),
            command: command.unwrap_or(Part { value: String::new(), quoting: Quoting::Auto }),
            args,
            cwd,
            env,
            group,
            is_default,
            unsupported,
        });
    }
    Ok(out)
}

/// What the variables of a task resolve against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarContext {
    /// The project folder on the project's computer.
    pub folder: String,
    /// The file of the current tab, if any.
    pub file: Option<String>,
}

/// `${...}` of Code-OSS's configuration resolver, for the workspace and file variables. Any other
/// variable is refused with its name, so a task never runs with a variable left in.
pub fn resolve_vars(text: &str, ctx: &VarContext) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return Ok(out);
        };
        let name = &after[..end];
        out.push_str(&resolve_one(name, ctx)?);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn resolve_one(name: &str, ctx: &VarContext) -> Result<String, String> {
    let folder = ctx.folder.trim_end_matches('/');
    let base = |p: &str| p.rsplit('/').next().unwrap_or(p).to_owned();
    let file = || ctx.file.clone().ok_or_else(|| format!("${{{name}}}에 쓸 열린 파일이 없습니다."));
    Ok(match name {
        "workspaceFolder" | "workspaceRoot" | "cwd" | "fileWorkspaceFolder" => folder.to_owned(),
        "workspaceFolderBasename" => base(folder),
        "pathSeparator" | "/" => "/".to_owned(),
        "file" => file()?,
        "relativeFile" => {
            let f = file()?;
            f.strip_prefix(&format!("{folder}/")).map(str::to_owned).unwrap_or(f)
        }
        "fileBasename" => base(&file()?),
        "fileBasenameNoExtension" => {
            let b = base(&file()?);
            match b.rfind('.') { Some(i) if i > 0 => b[..i].to_owned(), _ => b }
        }
        "fileExtname" => {
            let b = base(&file()?);
            match b.rfind('.') { Some(i) if i > 0 => b[i..].to_owned(), _ => String::new() }
        }
        "fileDirname" => {
            let f = file()?;
            f.rfind('/').map(|i| f[..i].to_owned()).unwrap_or_default()
        }
        "relativeFileDirname" => {
            let f = file()?;
            let rel = f.strip_prefix(&format!("{folder}/")).unwrap_or(&f).to_owned();
            rel.rfind('/').map(|i| rel[..i].to_owned()).unwrap_or_default()
        }
        other => return Err(format!("변수 ${{{other}}}는 아직 지원하지 않습니다.")),
    })
}

/// The characters after which Code-OSS quotes a shell argument (POSIX shells).
fn needs_quotes(s: &str) -> bool {
    s.is_empty() || s.chars().any(|c| c.is_whitespace() || "|&;<>()$`\\\"'*?[]#~=%!{}".contains(c))
}

/// One part as a POSIX shell word, the way Code-OSS quotes it for bash and zsh.
pub fn quote_part(part: &Part) -> String {
    let v = &part.value;
    match part.quoting {
        Quoting::Auto if !needs_quotes(v) => v.clone(),
        Quoting::Auto | Quoting::Strong => format!("'{}'", v.replace('\'', "'\\''")),
        Quoting::Weak => format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\"").replace('$', "\\$").replace('`', "\\`")),
        Quoting::Escape => {
            let mut out = String::new();
            for c in v.chars() {
                if c.is_whitespace() || "|&;<>()$`\\\"'*?[]#~=%!{}".contains(c) {
                    out.push('\\');
                }
                out.push(c);
            }
            out
        }
    }
}

/// What the node runs for one task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskProgram {
    /// Through `/bin/sh -c` on the project's computer.
    Shell(String),
    Argv(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskLaunch {
    pub title: String,
    pub program: TaskProgram,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
}

/// The program, folder and environment for running `task`, or why it cannot run.
pub fn launch(task: &TaskDef, ctx: &VarContext) -> Result<TaskLaunch, String> {
    if let Some(why) = &task.unsupported {
        return Err(why.clone());
    }
    let resolve_part = |p: &Part| resolve_vars(&p.value, ctx).map(|value| Part { value, quoting: p.quoting });
    let command = resolve_part(&task.command)?;
    let args = task.args.iter().map(resolve_part).collect::<Result<Vec<_>, _>>()?;
    let program = match task.kind {
        TaskKind::Shell if args.is_empty() => TaskProgram::Shell(command.value),
        TaskKind::Shell => {
            let mut line = quote_part(&command);
            for a in &args {
                line.push(' ');
                line.push_str(&quote_part(a));
            }
            TaskProgram::Shell(line)
        }
        TaskKind::Process => TaskProgram::Argv(std::iter::once(command.value).chain(args.into_iter().map(|a| a.value)).collect()),
    };
    let cwd = match &task.cwd {
        Some(c) => {
            let c = resolve_vars(c, ctx)?;
            if c.starts_with('/') { c } else { format!("{}/{c}", ctx.folder.trim_end_matches('/')) }
        }
        None => ctx.folder.clone(),
    };
    let env = task.env.iter().map(|(k, v)| resolve_vars(v, ctx).map(|v| (k.clone(), v))).collect::<Result<_, _>>()?;
    Ok(TaskLaunch { title: task.label.clone(), program, cwd, env })
}

/// The row's second line: the group and the command as written.
pub fn task_detail(task: &TaskDef) -> String {
    let mut line = task.command.value.clone();
    for a in &task.args {
        line.push(' ');
        line.push_str(&a.value);
    }
    match (&task.group, task.is_default) {
        (Some(g), true) => format!("{g} (기본) · {line}"),
        (Some(g), false) => format!("{g} · {line}"),
        _ => line,
    }
}

/// The task's program for the node's terminal relay.
pub fn term_launch(l: TaskLaunch) -> crate::terminal_stream::TermLaunch {
    let program = match l.program {
        TaskProgram::Shell(line) => crate::terminal_stream::TermProgram::Shell(line),
        TaskProgram::Argv(argv) => crate::terminal_stream::TermProgram::Argv(argv),
    };
    crate::terminal_stream::TermLaunch { program, cwd: l.cwd, env: l.env, title: l.title }
}

/// The task the top bar's 실행 runs: the chosen one, else the default build task, else the only one.
pub fn pick_task<'a>(tasks: &'a [TaskDef], chosen: Option<&str>) -> Option<&'a TaskDef> {
    let runnable = || tasks.iter().filter(|t| t.unsupported.is_none());
    if let Some(label) = chosen {
        if let Some(t) = runnable().find(|t| t.label == label) {
            return Some(t);
        }
    }
    runnable()
        .find(|t| t.is_default && t.group.as_deref() == Some("build"))
        .or_else(|| if runnable().count() == 1 { runnable().next() } else { None })
}

pub mod view {
    use std::cell::RefCell;

    use dioxus_compose::html::prelude::dioxus_elements::point_interaction::ModifiersInteraction;
    use dioxus_compose::html::prelude::dioxus_elements::Modifiers;
    use dioxus_compose::html::prelude::dioxus_signals::{ReadableExt, WritableExt};
    use dioxus_compose::html::prelude::*;

    use super::{launch, pick_task, task_detail, term_launch, TaskDef, TasksLoad, VarContext};
    use crate::editor_host::HostCommand;
    use crate::terminal_stream::{ChangeCallback, TerminalHandle};
    use crate::ui::editor::ToolWindow;
    use crate::ui::editor_workbench::page::{icon, icon_px, key_name, terminal_input};
    use crate::ui::editor_workbench::Shared;

    thread_local! {
        /// The last task run: its label and terminal. A new run starts a new terminal.
        static RUN: RefCell<Option<(String, TerminalHandle)>> = const { RefCell::new(None) };
        /// The task chosen in the 실행 window, for the top bar's 실행.
        static CHOSEN: RefCell<Option<String>> = const { RefCell::new(None) };
    }

    fn bump(s: &Shared) {
        let mut rev = s.revision;
        *rev.write() += 1;
    }

    fn notice(text: impl Into<String>) {
        crate::ui::cr::notice::post(text);
    }

    fn tasks_of(s: &Shared) -> Option<TasksLoad> {
        s.host.state.lock().unwrap().tasks.clone()
    }

    /// Open the 실행 window (and read tasks.json again).
    pub fn open(s: &Shared) {
        let mut stripes = s.stripes;
        if !stripes.read().is_selected(ToolWindow::Run) {
            stripes.write().toggle(ToolWindow::Run);
        }
        s.host.send(HostCommand::LoadTasks);
    }

    /// Run `task` in a new terminal on the project's computer.
    pub fn run(s: &Shared, task: &TaskDef) {
        if !crate::ui::editor_workbench::page::can_attach(&s.server_url, &s.computer) {
            notice("작업은 서버에 연결했을 때만 실행할 수 있습니다. 이 창에는 서버 주소나 컴퓨터가 없습니다.");
            return;
        }
        let ctx = VarContext { folder: s.folder.clone(), file: s.tabs.read().current_path().map(str::to_owned) };
        let l = match launch(task, &ctx) {
            Ok(l) => l,
            Err(e) => {
                notice(format!("{}을(를) 실행할 수 없습니다: {e}", task.label));
                return;
            }
        };
        let revision = s.revision;
        let on_change: ChangeCallback = std::sync::Arc::new(move || {
            let mut revision = revision;
            *revision.write() += 1;
        });
        let handle = crate::terminal_stream::start_task(&s.server_url, &s.computer, &s.project, term_launch(l), Some(on_change));
        // The earlier run's terminal is detached, not killed: it keeps running on the computer,
        // as a Code-OSS task terminal does until it is closed.
        RUN.with(|r| {
            if let Some((_, old)) = r.borrow_mut().replace((task.label.clone(), handle)) {
                old.detach();
            }
        });
        CHOSEN.with(|c| *c.borrow_mut() = Some(task.label.clone()));
        let mut stripes = s.stripes;
        if !stripes.read().is_selected(ToolWindow::Run) {
            stripes.write().toggle(ToolWindow::Run);
        }
        bump(s);
    }

    /// The top bar's 실행: the chosen task, else the default build task; with none, the 실행 window opens.
    pub fn run_default(s: &Shared) {
        let chosen = CHOSEN.with(|c| c.borrow().clone());
        match tasks_of(s) {
            Some(TasksLoad::Parsed(tasks)) => match pick_task(&tasks, chosen.as_deref()) {
                Some(t) => run(s, &t.clone()),
                None => {
                    notice("실행할 작업을 실행 창에서 고르세요.");
                    open(s);
                }
            },
            _ => open(s),
        }
    }

    /// The top bar's 중지: Ctrl+C to the running task's terminal.
    pub fn stop(s: &Shared) {
        let running = RUN.with(|r| r.borrow().as_ref().map(|(_, h)| h.clone()));
        match running {
            Some(h) if h.snapshot().exit_text().is_none() => h.send_input(&[3]),
            _ => notice("실행 중인 작업이 없습니다."),
        }
        bump(s);
    }

    /// The 실행 window: the tasks of tasks.json and the last run's terminal.
    pub fn body(s: &Shared) -> Element {
        let tasks = tasks_of(s);
        if tasks.is_none() {
            s.host.send(HostCommand::LoadTasks);
        }
        let tasks = tasks.unwrap_or(TasksLoad::Loading);
        let message = tasks.message();
        let list: Vec<TaskDef> = if let TasksLoad::Parsed(t) = &tasks { t.clone() } else { Vec::new() };
        let run_state = RUN.with(|r| r.borrow().clone());
        let reload_s = s.clone();
        rsx! {
            div { style: "display:flex; flex:1; min-height:0;",
                div { role: "list", "aria-label": "작업", style: "width:260px; flex:none; overflow-y:auto; border-right:1px solid var(--line); padding:var(--sp1);",
                    div { style: "display:flex; align-items:center; padding:0 var(--sp2); color:var(--muted); font-size:var(--fs-xs);",
                        span { style: "flex:1;", ".vscode/tasks.json" }
                        button { class: "ed-ib", title: "작업 다시 읽기", "aria-label": "작업 다시 읽기", onclick: move |_| { reload_s.host.send(HostCommand::LoadTasks); }, {icon("refresh")} }
                    }
                    if let Some(m) = message {
                        div { style: "padding:var(--sp2); color:var(--muted); white-space:pre-wrap;", "{m}" }
                    }
                    for (i, t) in list.into_iter().enumerate() {
                        {
                            let run_s = s.clone();
                            let detail = task_detail(&t);
                            let why = t.unsupported.clone();
                            let label = t.label.clone();
                            let enabled = why.is_none();
                            let tip = why.clone().unwrap_or_else(|| format!("{label} 실행"));
                            let note = why.clone().unwrap_or(detail);
                            rsx! {
                                div { key: "{i}", role: "listitem", style: "display:flex; align-items:center; gap:var(--sp2); padding:var(--sp1) var(--sp2); border-radius:var(--r-sm);",
                                    div { style: "flex:1; min-width:0;",
                                        div { style: if enabled { "" } else { "color:var(--muted);" }, "{label}" }
                                        div { style: "color:var(--muted); font-size:var(--fs-xs); overflow:hidden; text-overflow:ellipsis; white-space:nowrap;", "{note}" }
                                    }
                                    button { class: "ed-ib", title: "{tip}", "aria-label": "{tip}", disabled: !enabled, onclick: move |_| run(&run_s, &t), {icon("play")} }
                                }
                            }
                        }
                    }
                }
                {run_output(s, run_state)}
            }
        }
    }

    fn run_output(s: &Shared, run_state: Option<(String, TerminalHandle)>) -> Element {
        let Some((label, handle)) = run_state else {
            return rsx! { div { class: "ed-term", style: "flex:1; color:var(--muted);", "작업을 실행하면 프로젝트의 컴퓨터에서 새 터미널로 실행되고 출력이 이곳에 표시됩니다." } };
        };
        let state = handle.snapshot();
        let lines = state.lines.clone();
        let notice_text = state.notice();
        let running = state.exit_text().is_none() && state.error_text().is_none();
        let key_handle = handle.clone();
        let stop_s = s.clone();
        let computer = if s.computer.is_empty() { "로컬".to_string() } else { s.computer.clone() };
        rsx! {
            div { style: "flex:1; min-width:0; display:flex; flex-direction:column;",
                div { class: "ed-tsub",
                    button { class: "on", "aria-pressed": "true", {icon_px("play", 14)} "{label}" }
                    span { style: "color:var(--muted); font-size:var(--fs-xs);", "{computer}" }
                    div { class: "spacer" }
                    if running {
                        button { class: "ed-ib", title: "작업 중지", "aria-label": "작업 중지", onclick: move |_| stop(&stop_s), {icon("pause")} }
                    }
                }
                div { class: "ed-term", role: "log", "aria-label": "작업 출력", tabindex: "0", style: "flex:1; outline:none;",
                    onkeydown: move |event| {
                        let m = event.modifiers();
                        let name = key_name(&event.key());
                        let Some(bytes) = terminal_input(&name, m.contains(Modifiers::CONTROL), m.contains(Modifiers::ALT), m.contains(Modifiers::SHIFT), m.contains(Modifiers::META)) else { return };
                        event.stop_propagation();
                        event.prevent_default();
                        if !key_handle.snapshot().controlled_by_other() { key_handle.send_input(&bytes); }
                    },
                    div { style: "white-space:pre;",
                        for (i, line) in lines.iter().enumerate() {
                            div { key: "{i}", class: "tl", "{line}" }
                        }
                    }
                    if let Some(n) = notice_text {
                        div { class: "tl", style: "padding-top:var(--sp2); color:var(--muted);", "{n}" }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        // See https://go.microsoft.com/fwlink/?LinkId=733558
        "version": "2.0.0",
        "options": { "env": { "RUST_LOG": "info" } },
        "tasks": [
            {
                "label": "cargo build",
                "type": "shell",
                "command": "cargo build --locked",
                "group": { "kind": "build", "isDefault": true },
                "problemMatcher": ["$rustc"], /* read past */
            },
            {
                "label": "echo file",
                "type": "shell",
                "command": "echo",
                "args": ["${relativeFile}", "a b", { "value": "it's", "quoting": "strong" }],
                "options": { "cwd": "${workspaceFolder}/sub", "env": { "X": "${workspaceFolderBasename}" } },
            },
            { "label": "node", "type": "process", "command": "node", "args": ["${file}", "x y"] },
            { "label": "npm test", "type": "npm", "script": "test" },
            { "label": "all", "dependsOn": ["cargo build", "node"] },
        ],
    }"#;

    fn ctx() -> VarContext {
        VarContext { folder: "/home/me/proj".into(), file: Some("/home/me/proj/src/main.rs".into()) }
    }

    #[test]
    fn fr_l31_16_tasks_json_path_is_the_vscode_folder() {
        assert_eq!(tasks_json_path("/home/me/proj/"), "/home/me/proj/.vscode/tasks.json");
    }

    #[test]
    fn fr_l31_16_jsonc_comments_and_trailing_commas_are_removed_outside_strings() {
        let v: Value = serde_json::from_str(&strip_jsonc("{ \"a\": \"// not a comment, \", /* c */ \"b\": [1, 2, // x\n ], }")).unwrap();
        assert_eq!(v["a"], "// not a comment, ");
        assert_eq!(v["b"], serde_json::json!([1, 2]));
        let v: Value = serde_json::from_str(&strip_jsonc("{\"q\": \"a\\\"b\", }")).unwrap();
        assert_eq!(v["q"], "a\"b");
    }

    #[test]
    fn fr_l31_16_parses_shell_process_and_marks_what_cannot_run() {
        let tasks = parse_tasks_json(SAMPLE).unwrap();
        assert_eq!(tasks.iter().map(|t| t.label.as_str()).collect::<Vec<_>>(), vec!["cargo build", "echo file", "node", "npm test", "all"]);
        assert_eq!(tasks[0].kind, TaskKind::Shell);
        assert_eq!(tasks[0].group.as_deref(), Some("build"));
        assert!(tasks[0].is_default);
        assert_eq!(tasks[0].env.get("RUST_LOG").map(String::as_str), Some("info"));
        assert_eq!(tasks[2].kind, TaskKind::Process);
        assert!(tasks[0].unsupported.is_none() && tasks[1].unsupported.is_none() && tasks[2].unsupported.is_none());
        assert!(tasks[3].unsupported.as_deref().unwrap().contains("npm"));
        assert!(tasks[4].unsupported.as_deref().unwrap().contains("dependsOn"));
        assert_eq!(task_detail(&tasks[0]), "build (기본) · cargo build --locked");
    }

    #[test]
    fn fr_l31_16_old_versions_and_bad_json_are_refused_in_korean() {
        assert!(parse_tasks_json("{\"version\": \"0.1.0\"}").unwrap_err().contains("2.0.0"));
        assert!(parse_tasks_json("{").unwrap_err().starts_with("tasks.json을 해석하지 못했습니다"));
        assert_eq!(parse_tasks_json("{\"version\": \"2.0.0\"}").unwrap(), Vec::new());
    }

    #[test]
    fn fr_l31_16_a_shell_task_without_args_runs_its_command_as_written() {
        let tasks = parse_tasks_json(SAMPLE).unwrap();
        let l = launch(&tasks[0], &ctx()).unwrap();
        assert_eq!(l.program, TaskProgram::Shell("cargo build --locked".into()));
        assert_eq!(l.cwd, "/home/me/proj");
        assert_eq!(l.title, "cargo build");
    }

    #[test]
    fn fr_l31_16_a_shell_task_with_args_quotes_each_part_that_needs_it() {
        let tasks = parse_tasks_json(SAMPLE).unwrap();
        let l = launch(&tasks[1], &ctx()).unwrap();
        assert_eq!(l.program, TaskProgram::Shell("echo src/main.rs 'a b' 'it'\\''s'".into()));
        assert_eq!(l.cwd, "/home/me/proj/sub");
        assert_eq!(l.env.get("X").map(String::as_str), Some("proj"));
    }

    #[test]
    fn fr_l31_16_a_process_task_is_an_argv_without_a_shell() {
        let tasks = parse_tasks_json(SAMPLE).unwrap();
        let l = launch(&tasks[2], &ctx()).unwrap();
        assert_eq!(l.program, TaskProgram::Argv(vec!["node".into(), "/home/me/proj/src/main.rs".into(), "x y".into()]));
    }

    #[test]
    fn fr_l31_16_unsupported_tasks_and_variables_do_not_run() {
        let tasks = parse_tasks_json(SAMPLE).unwrap();
        assert!(launch(&tasks[3], &ctx()).is_err());
        assert_eq!(resolve_vars("${input:name}", &ctx()).unwrap_err(), "변수 ${input:name}는 아직 지원하지 않습니다.");
        let no_file = VarContext { folder: "/p".into(), file: None };
        assert!(resolve_vars("${file}", &no_file).unwrap_err().contains("열린 파일이 없습니다"));
    }

    #[test]
    fn fr_l31_16_file_variables_follow_code_oss() {
        let c = ctx();
        assert_eq!(resolve_vars("${fileBasename}|${fileBasenameNoExtension}|${fileExtname}", &c).unwrap(), "main.rs|main|.rs");
        assert_eq!(resolve_vars("${fileDirname}|${relativeFileDirname}|${workspaceFolderBasename}", &c).unwrap(), "/home/me/proj/src|src|proj");
    }

    #[test]
    fn fr_l31_16_quoting_styles() {
        let p = |v: &str, q| Part { value: v.into(), quoting: q };
        assert_eq!(quote_part(&p("plain", Quoting::Auto)), "plain");
        assert_eq!(quote_part(&p("a b", Quoting::Weak)), "\"a b\"");
        assert_eq!(quote_part(&p("a b", Quoting::Escape)), "a\\ b");
        assert_eq!(quote_part(&p("x", Quoting::Strong)), "'x'");
    }

    #[test]
    fn fr_l31_16_the_top_bar_runs_the_chosen_task_else_the_default_build_task() {
        let tasks = parse_tasks_json(SAMPLE).unwrap();
        assert_eq!(pick_task(&tasks, Some("node")).unwrap().label, "node");
        assert_eq!(pick_task(&tasks, Some("npm test")).unwrap().label, "cargo build", "an unsupported choice falls back");
        assert_eq!(pick_task(&tasks, None).unwrap().label, "cargo build");
        let one = parse_tasks_json(r#"{"version":"2.0.0","tasks":[{"label":"a","type":"shell","command":"x"}]}"#).unwrap();
        assert_eq!(pick_task(&one, None).unwrap().label, "a");
        let two = parse_tasks_json(r#"{"version":"2.0.0","tasks":[{"label":"a","type":"shell","command":"x"},{"label":"b","type":"shell","command":"y"}]}"#).unwrap();
        assert!(pick_task(&two, None).is_none());
    }

    #[test]
    fn fr_l31_16_load_states_say_what_is_there() {
        assert_eq!(TasksLoad::from_read(Err("EntryNotFound (FileSystemError)".into())), TasksLoad::NoFile);
        assert!(TasksLoad::NoFile.message().unwrap().contains(".vscode/tasks.json"));
        assert!(matches!(TasksLoad::from_read(Err("permission denied".into())), TasksLoad::Failed(_)));
        assert!(TasksLoad::from_read(Ok(SAMPLE.into())).message().is_none());
    }
}
