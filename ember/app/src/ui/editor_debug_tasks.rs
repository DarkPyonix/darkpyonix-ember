//! The `preLaunchTask` and `postDebugTask` of a debug configuration, through the 실행 task
//! runner (FR-L31.42, #445).
//!
//! The session layer asks for a task with [`DebugUpdate::RunTask`]. The window reads
//! `tasks.json` again, as Code-OSS's task service looks the task up when the start needs it, runs
//! the task in a new terminal like the 실행 window does, and reports how it ended with
//! [`DebugRequest::TaskDone`] (`debugTaskRunner.runTask`). A `postDebugTask` is only run; a
//! failure to run it is a notice, as `onDidEndAdapter` shows it.
//!
//! The task's problem matchers run in [`super::editor_task_problems`]. Their problems reach the
//! session before the outcome does, and the session counts the error markers it holds, as
//! `runTaskAndCheckErrors` reads the marker service. A background (`isBackground`) task is ready
//! when a pass of its matcher ends (`endsPattern`); one whose matcher never began a pass (no
//! matcher, or one without `background`) gets Code-OSS's untracked-task error after Code-OSS's
//! 5 second wait, with Debug Anyway offered.
//!
//! When the `preLaunchTask` failed the session layer asks [`DebugUpdate::TaskFailed`]; the window
//! shows Code-OSS's dialog with its buttons and answers [`DebugRequest::AnswerTask`]. Show Errors
//! opens the 문제 window and Configure Task opens `tasks.json`, as their Code-OSS actions do. The
//! warning's "Remember my choice in user settings" writes `debug.onTaskErrors` to the user's
//! settings.json with the choice, which the session then reads before it asks again; when the
//! setting is `showErrors` the session asks for the 문제 window itself ([`DebugUpdate::ShowProblems`]).
//!
//! A `preLaunchTask` still running after 10 s shows the "Waiting for preLaunchTask" card, unless
//! `debug.hideSlowPreLaunchWarning` is on: Debug Anyway starts the session as if the task had
//! exited with 0, and Abort and Configure Task cancel the start and stop the task with Ctrl+C, as
//! the 실행 window's 작업 중지 does. Code-OSS draws it as a progress notification; the toolkit has no
//! animation yet (dioxus-compose#46), so its bar does not move.
//!
//! The error dialog (a task that could not run) has "Remember my choice for this task" instead:
//! the answer is kept per workspace by the dialog's message, as `debugTaskRunner` keeps it in the
//! workspace's storage, and the next time the same message comes the answer is given without the
//! dialog (Configure Task still opens `tasks.json`).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use dioxus_compose::html::prelude::dioxus_elements::Key;
use dioxus_compose::html::prelude::*;
use dioxus_compose::prelude::{Props, component};
use dioxus_signals::{ReadableExt, WritableExt};
use dioxus_core::EventHandler;
use ember_editor::debugger::{DebugRequest, DebugUpdate, TaskChoice, TaskOutcome};

use super::editor::ToolWindow;
use super::editor_task_problems::TrackStatus;
use super::editor_tasks::{TaskDef, TasksLoad};
use super::editor_workbench::Shared;
use crate::editor_host::HostCommand;
use crate::terminal_stream::{TerminalHandle, TerminalState};

/// How long a background task may run before it counts as untracked (`debugTaskRunner`: 5000 ms
/// for a background task without a problem matcher).
pub(crate) const BACKGROUND_WAIT: Duration = Duration::from_secs(5);

/// How long a `preLaunchTask` runs before the "Waiting for preLaunchTask" card shows
/// (`debugTaskRunner.runTask`: 10 000 ms).
pub(crate) const SLOW_WAIT: Duration = Duration::from_secs(10);

/// Whether the waiting card is due for a task running for `running_for`, with the setting
/// `debug.hideSlowPreLaunchWarning` at `hide`.
pub(crate) fn slow_warning_due(running_for: Duration, hide: bool) -> bool {
    !hide && running_for >= SLOW_WAIT
}

/// The waiting card's message (`runningTask`).
pub(crate) fn waiting_message(label: &str) -> String {
    format!("Waiting for preLaunchTask '{label}'...")
}

/// The buttons of the waiting card, in Code-OSS's order: Debug Anyway, Configure Task (a task of
/// `tasks.json` can always be configured), Abort.
pub(crate) const WAITING_CHOICES: [TaskChoice; 3] = [TaskChoice::DebugAnyway, TaskChoice::ConfigureTask, TaskChoice::Abort];

/// What a button of the waiting card reports for the task: Debug Anyway is a success
/// (`{ exitCode: 0 }`), the others a cancelled task (`{ exitCode: undefined, cancelled: true }`).
pub(crate) fn waiting_outcome(choice: TaskChoice) -> TaskOutcome {
    match choice {
        TaskChoice::DebugAnyway => TaskOutcome::Exited { code: Some(0), errors: 0 },
        _ => TaskOutcome::Exited { code: None, errors: 0 },
    }
}

/// The setting "Remember my choice in user settings" writes for the warning's answer `choice`:
/// the key and its value.
pub(crate) fn remembered_setting(choice: TaskChoice) -> (&'static str, &'static str) {
    ("debug.onTaskErrors", ember_editor::debugger::OnTaskErrors::remembered(choice).as_str())
}

/// The number `debugTaskRunner` stores for an answer of the error dialog (`DebugChoice`: 0 Debug
/// Anyway, 1 Configure Task, 2 Cancel); the warning's answers are not stored this way.
pub(crate) fn task_choice_code(choice: TaskChoice) -> Option<u8> {
    match choice {
        TaskChoice::DebugAnyway => Some(0),
        TaskChoice::ConfigureTask => Some(1),
        TaskChoice::Cancel => Some(2),
        TaskChoice::ShowErrors | TaskChoice::Abort => None,
    }
}

/// The answer this workspace remembered for the error dialog's `message`, if it did.
pub(crate) fn remembered_task_choice(choices: &BTreeMap<String, u8>, message: &str) -> Option<TaskChoice> {
    match choices.get(message)? {
        0 => Some(TaskChoice::DebugAnyway),
        1 => Some(TaskChoice::ConfigureTask),
        2 => Some(TaskChoice::Cancel),
        _ => None,
    }
}

/// The question of a failed `preLaunchTask` while it is open.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TaskPrompt {
    pub start: u64,
    pub message: String,
    pub choices: Vec<TaskChoice>,
    pub error: bool,
}

/// What finding a task in `tasks.json` gave.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Lookup {
    /// `tasks.json` is still being read.
    Wait,
    Found(TaskDef),
    /// The task cannot run: Code-OSS's message, or the reason this runner cannot run it.
    Missing(String),
}

/// The task `label` of the `tasks.json` read (`taskService.getTask`).
pub(crate) fn find_task(tasks: &TasksLoad, label: &str) -> Lookup {
    let missing = || Lookup::Missing(format!("Could not find the task '{label}'."));
    match tasks {
        TasksLoad::Loading => Lookup::Wait,
        TasksLoad::NoFile => missing(),
        TasksLoad::Failed(e) => Lookup::Missing(e.clone()),
        TasksLoad::Parsed(list) => match list.iter().find(|t| t.label == label) {
            None => missing(),
            Some(t) => match &t.unsupported {
                Some(why) => Lookup::Missing(why.clone()),
                None => Lookup::Found(t.clone()),
            },
        },
    }
}

/// How a task's terminal and its problem matchers say it ended, once it has: the process's exit
/// code, a terminal that failed for good, a background task whose matcher ended a pass (ready),
/// or a background task still running after [`BACKGROUND_WAIT`] without its matcher having begun
/// a pass (Code-OSS's `taskNotTracked`). `None` while it runs.
pub(crate) fn outcome_of(
    label: &str,
    state: &TerminalState,
    background: bool,
    running_for: Duration,
    track: TrackStatus,
) -> Option<TaskOutcome> {
    if let Some((code, _signal)) = state.exit {
        // Stopped by a signal there is no exit code: Code-OSS takes that as a cancelled task.
        return Some(TaskOutcome::Exited { code: code.map(i64::from), errors: 0 });
    }
    if state.error.is_some() && state.needs_reconnect() {
        let why = state.error_text().unwrap_or_default();
        return Some(TaskOutcome::Failed { message: format!("The task '{label}' could not run: {why}") });
    }
    if background && track.ready {
        return Some(TaskOutcome::Ready { errors: 0 });
    }
    if background && !track.began && running_for >= BACKGROUND_WAIT {
        return Some(TaskOutcome::Failed {
            message: format!(
                "The task '{label}' has not exited and doesn't have a 'problemMatcher' defined. Make sure to define a problem matcher for watch tasks."
            ),
        });
    }
    None
}

/// A button's label: Code-OSS's words in Korean.
pub(crate) fn choice_label(choice: TaskChoice) -> &'static str {
    match choice {
        TaskChoice::DebugAnyway => "그래도 디버그",
        TaskChoice::ShowErrors => "오류 표시",
        TaskChoice::ConfigureTask => "작업 구성",
        TaskChoice::Abort => "중단",
        TaskChoice::Cancel => "취소",
    }
}

/// A task the session layer asked for, on its way.
#[derive(Clone)]
enum Waiting {
    /// `tasks.json` is being read again.
    Tasks { label: String, start: Option<u64> },
    /// The task runs in `handle`; only a `preLaunchTask` (`start`) is followed to its end.
    Running { label: String, start: u64, handle: TerminalHandle, background: bool, since: Instant },
}

/// The waiting card while it is shown.
#[derive(Clone)]
struct Slow {
    start: u64,
    label: String,
}

thread_local! {
    static WAITING: RefCell<Vec<Waiting>> = const { RefCell::new(Vec::new()) };
    static SLOW: RefCell<Option<Slow>> = const { RefCell::new(None) };
}

fn wake_after(s: &Shared, after: Duration) {
    let revision = s.revision;
    crate::services::services().rt.spawn(async move {
        tokio::time::sleep(after).await;
        let mut revision = revision;
        *revision.write() += 1;
    });
}

fn hide_slow_warning(s: &Shared) -> bool {
    s.host.state.lock().unwrap().settings.scopes.value("debug.hideSlowPreLaunchWarning", None).and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Take a debug update the session layer sent for a task, if it is one. Returns whether it was.
pub(crate) fn on_debug(s: &Shared, update: &DebugUpdate) -> bool {
    match update {
        DebugUpdate::RunTask { label, start } => {
            // A fresh read: the task is looked up as `tasks.json` is now.
            s.host.state.lock().unwrap().tasks = Some(TasksLoad::Loading);
            s.host.send(HostCommand::LoadTasks);
            WAITING.with_borrow_mut(|w| w.push(Waiting::Tasks { label: label.clone(), start: *start }));
            true
        }
        DebugUpdate::TaskFailed { start, message, choices, error } => {
            // The error dialog is not asked again for a message whose answer was remembered.
            let remembered = if *error {
                let store = crate::services::services().ui.prefs.get_untracked().debug_store(&s.computer, &s.folder);
                remembered_task_choice(&store.task_error_choices, message).filter(|c| choices.contains(c))
            } else {
                None
            };
            match remembered {
                Some(choice) => give_answer(s, *start, choice),
                None => {
                    let mut debug = s.debug;
                    debug.write().task_prompt = Some(TaskPrompt { start: *start, message: message.clone(), choices: choices.clone(), error: *error });
                }
            }
            true
        }
        DebugUpdate::ShowProblems => {
            show_problems(s);
            true
        }
        _ => false,
    }
}

fn show_problems(s: &Shared) {
    let mut stripes = s.stripes;
    if !stripes.read().is_selected(ToolWindow::Problems) {
        stripes.write().show(ToolWindow::Problems);
    }
}

fn report(s: &Shared, start: Option<u64>, label: &str, outcome: TaskOutcome) {
    if start.is_some() && SLOW.with_borrow(|w| w.as_ref().map(|w| w.start)) == start {
        SLOW.with_borrow_mut(|w| *w = None);
    }
    match start {
        Some(start) => {
            s.host.send(HostCommand::Debug(DebugRequest::TaskDone { start, outcome }));
        }
        // `postDebugTask`: only a failure to run it is shown.
        None => {
            if let TaskOutcome::Failed { message } = outcome {
                crate::ui::cr::notice::post(format!("postDebugTask '{label}': {message}"));
            }
        }
    }
}

/// Move the waiting tasks on: run the ones whose `tasks.json` has been read, report the ones that
/// ended. Called on every render of the workbench page, after its session updates; every task
/// terminal change and every read of `tasks.json` renders the page again.
pub(crate) fn poll(s: &Shared) {
    // Every tracked task's output goes through its problem matchers first.
    super::editor_task_problems::poll(s);
    if WAITING.with_borrow(|w| w.is_empty()) {
        return;
    }
    let hide_slow = hide_slow_warning(s);
    let tasks = s.host.state.lock().unwrap().tasks.clone();
    let waiting = WAITING.with_borrow_mut(std::mem::take);
    let mut keep = Vec::new();
    let now = Instant::now();
    for w in waiting {
        match w {
            Waiting::Tasks { label, start } => match tasks.as_ref().map_or(Lookup::Wait, |t| find_task(t, &label)) {
                Lookup::Wait => keep.push(Waiting::Tasks { label, start }),
                Lookup::Missing(message) => report(s, start, &label, TaskOutcome::Failed { message }),
                Lookup::Found(task) => match super::editor_tasks::view::run(s, &task) {
                    None => report(s, start, &label, TaskOutcome::Failed { message: format!("The task '{label}' could not run.") }),
                    Some(handle) => {
                        if let Some(start) = start {
                            keep.push(Waiting::Running { label, start, handle, background: task.is_background, since: now });
                            // The page renders again when the waiting card is due.
                            wake_after(s, SLOW_WAIT + Duration::from_millis(50));
                        }
                    }
                },
            },
            Waiting::Running { label, start, handle, background, since } => {
                let state = handle.snapshot();
                // The problems of a task that ended reach the session before its outcome does.
                let track = super::editor_task_problems::step(s, &handle, &state);
                let running_for = now.saturating_duration_since(since);
                match outcome_of(&label, &state, background, running_for, track) {
                    Some(outcome) => report(s, Some(start), &label, outcome),
                    None => {
                        if slow_warning_due(running_for, hide_slow) && SLOW.with_borrow(Option::is_none) {
                            SLOW.with_borrow_mut(|w| *w = Some(Slow { start, label: label.clone() }));
                        }
                        keep.push(Waiting::Running { label, start, handle, background, since });
                    }
                }
            }
        }
    }
    // A background task is checked again after the wait even if its terminal says nothing.
    if keep.iter().any(|w| matches!(w, Waiting::Running { background: true, .. })) {
        let revision = s.revision;
        crate::services::services().rt.spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let mut revision = revision;
            *revision.write() += 1;
        });
    }
    WAITING.with_borrow_mut(|w| {
        keep.append(w);
        *w = keep;
    });
}

/// The answer to the open question: the session layer hears it, Show Errors and Configure Task
/// open what Code-OSS opens, and a checked "Remember my choice" of the warning writes
/// `debug.onTaskErrors` to the user's settings (`configurationService.updateValue`); that of the
/// error dialog keeps the answer for the message in this workspace (`debug.taskerrorchoice`).
pub(crate) fn answer(s: &Shared, choice: TaskChoice, remember: bool) {
    let mut debug = s.debug;
    let Some(prompt) = debug.write().task_prompt.take() else { return };
    if remember && !prompt.error {
        let (key, value) = remembered_setting(choice);
        s.host.send(HostCommand::WriteSetting {
            scope: ember_editor::SettingsScope::User,
            key: key.to_owned(),
            value: Some(serde_json::Value::String(value.to_owned())),
        });
    }
    if remember && prompt.error {
        if let Some(code) = task_choice_code(choice) {
            let message = prompt.message.clone();
            super::editor_debug::store(s, move |d| {
                d.task_error_choices.insert(message, code);
            });
        }
    }
    give_answer(s, prompt.start, choice);
}

/// Tell the session layer the answer for start `start`, and open what the answer opens.
fn give_answer(s: &Shared, start: u64, choice: TaskChoice) {
    s.host.send(HostCommand::Debug(DebugRequest::AnswerTask { start, choice }));
    match choice {
        TaskChoice::ShowErrors => show_problems(s),
        TaskChoice::ConfigureTask => open_tasks_json(s),
        _ => {}
    }
}

fn open_tasks_json(s: &Shared) {
    let path = super::editor_tasks::tasks_json_path(&s.folder);
    super::editor_workbench::page::open_tab_file(s, &path);
}

/// A button of the waiting card: the start goes on or is cancelled, and a cancelled task is
/// stopped (`taskService.terminate`, here Ctrl+C); Configure Task also opens `tasks.json`.
pub(crate) fn answer_waiting(s: &Shared, choice: TaskChoice) {
    let Some(slow) = SLOW.with_borrow_mut(Option::take) else { return };
    let running = WAITING.with_borrow_mut(|w| {
        let i = w.iter().position(|x| matches!(x, Waiting::Running { start, .. } if *start == slow.start))?;
        match w.remove(i) {
            Waiting::Running { handle, .. } => Some(handle),
            Waiting::Tasks { .. } => None,
        }
    });
    let Some(handle) = running else { return };
    if choice != TaskChoice::DebugAnyway {
        handle.send_input(&[3]);
    }
    if choice == TaskChoice::ConfigureTask {
        open_tasks_json(s);
    }
    report(s, Some(slow.start), &slow.label, waiting_outcome(choice));
}

/// The checkbox of the warning (`remember`), in Korean.
pub(crate) const REMEMBER_LABEL: &str = "사용자 설정에 내 선택 기억";

/// The checkbox of the error dialog (`rememberTask`, "Remember my choice for this task"), in Korean.
pub(crate) const REMEMBER_TASK_LABEL: &str = "이 작업에 대한 내 선택 기억";

/// The checkbox's label: the warning's, or the error dialog's.
pub(crate) fn remember_label(error: bool) -> &'static str {
    if error { REMEMBER_TASK_LABEL } else { REMEMBER_LABEL }
}

/// Code-OSS's dialog for a failed `preLaunchTask`: the message and its buttons, the last one the
/// cancel button (Escape and a click outside choose it), and the checkbox: "Remember my choice in
/// user settings" for the warning, "Remember my choice for this task" for the error dialog.
/// `choose` hears the button and whether the box is checked.
#[component]
pub(crate) fn TaskPromptSheet(prompt: TaskPrompt, choose: EventHandler<(TaskChoice, bool)>) -> Element {
    let mut remember = dioxus_compose::prelude::use_signal(|| false);
    let cancel = prompt.choices.last().copied().unwrap_or(TaskChoice::Abort);
    let title = if prompt.error { "작업 오류" } else { "작업 경고" };
    let buttons: Vec<(usize, TaskChoice, &'static str)> =
        prompt.choices.iter().copied().enumerate().map(|(i, c)| (i, c, choice_label(c))).collect();
    let checked = *remember.read();
    let remember_text = remember_label(prompt.error);
    rsx! {
        div { class: "modal", role: "presentation",
            onclick: move |_| choose.call((cancel, *remember.peek())),
            onkeydown: move |event| {
                if event.key() == Key::Escape {
                    event.stop_propagation();
                    choose.call((cancel, *remember.peek()));
                }
            },
            div { id: "debug-task-prompt", class: "card ed-confirm", role: "alertdialog", "aria-modal": "true", "aria-label": "{title}",
                onclick: move |event| event.stop_propagation(),
                div { class: "card-b",
                    b { "{prompt.message}" }
                    // The mockup's square check (`ed-scm-cb`), as the commit box's amend uses it;
                    // the row takes the click.
                    div { id: "debug-task-remember", style: "display:flex; align-items:center; gap:6px; font-size:var(--fs-xs); color:var(--text-2); cursor:pointer;",
                        onclick: move |_| {
                            let now = *remember.peek();
                            remember.set(!now);
                        },
                        button { class: if checked { "ed-scm-cb on" } else { "ed-scm-cb" }, role: "checkbox", "aria-checked": "{checked}", "aria-label": "{remember_text}",
                            if checked { {super::editor_workbench::page::icon_px("check-w", 12)} }
                        }
                        span { "{remember_text}" }
                    }
                    div { class: "ed-confirm-acts",
                        for (i, choice, label) in buttons {
                            button { key: "{i}", id: "debug-task-choice-{i}",
                                class: if i == 0 { "btn primary sm" } else { "btn sm" },
                                autofocus: choice == cancel,
                                onclick: move |_| choose.call((choice, *remember.peek())),
                                "{label}"
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The "Waiting for preLaunchTask" card: Code-OSS's progress notification, drawn where the
/// window's notifications are, with the notification card's tokens (the mockup has no
/// notification screen). The bar is still: the toolkit has no animation yet (dioxus-compose#46).
// TODO(dioxus-compose#46): slide the bar (Code-OSS's indeterminate progress) once `@keyframes` run.
#[component]
pub(crate) fn WaitingCard(label: String, choose: EventHandler<TaskChoice>) -> Element {
    let message = waiting_message(&label);
    let bottom = super::editor_notifications::overlay_bottom();
    let buttons: Vec<(usize, TaskChoice, &'static str)> = WAITING_CHOICES.iter().copied().enumerate().map(|(i, c)| (i, c, choice_label(c))).collect();
    rsx! {
        div { id: "debug-task-waiting", role: "status", "aria-label": "{message}",
            style: "position:fixed; right:12px; bottom:{bottom}; width:min(360px, calc(100vw - 24px)); background:var(--surface); border:1px solid var(--line); border-radius:var(--r); box-shadow:var(--shadow-lg); padding:var(--sp3); display:flex; flex-direction:column; gap:var(--sp2);",
            div { style: "white-space:pre-wrap; font-size:var(--fs-sm);", "{message}" }
            div { style: "height:2px; background:var(--line); border-radius:1px; overflow:hidden;",
                div { style: "width:30%; height:100%; background:var(--c-blue);" }
            }
            div { style: "display:flex; justify-content:flex-end; gap:var(--sp2);",
                for (i, choice, text) in buttons {
                    button { key: "{i}", id: "debug-task-waiting-{i}", class: "ed-pj",
                        style: if i == 0 { "background:var(--ink); color:var(--ink-text); height:26px;" } else { "border:1px solid var(--line); height:26px;" },
                        onclick: move |_| choose.call(choice),
                        "{text}"
                    }
                }
            }
        }
    }
}

/// The open question and the waiting card, if there are any, for the window's layers.
pub(crate) fn prompt_sheet(s: &Shared) -> Element {
    let prompt = s.debug.read().task_prompt.clone();
    let slow = SLOW.with_borrow(Clone::clone);
    let shared = s.clone();
    let waiting_s = s.clone();
    rsx! {
        if let Some(slow) = slow {
            WaitingCard { key: "{slow.start}", label: slow.label.clone(), choose: move |choice| answer_waiting(&waiting_s, choice) }
        }
        if let Some(prompt) = prompt {
            TaskPromptSheet { key: "{prompt.start}", prompt, choose: move |(choice, remember): (TaskChoice, bool)| answer(&shared, choice, remember) }
        }
    }
}

#[cfg(test)]
#[path = "editor_debug_tasks/tests.rs"]
mod tests;
