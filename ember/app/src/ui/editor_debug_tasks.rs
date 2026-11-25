//! The `preLaunchTask` and `postDebugTask` of a debug configuration, through the 실행 task
//! runner (FR-L31.42, #445).
//!
//! The session layer asks for a task with [`DebugUpdate::RunTask`]. The window reads
//! `tasks.json` again, as Code-OSS's task service looks the task up when the start needs it, runs
//! the task in a new terminal like the 실행 window does, and reports how it ended with
//! [`DebugRequest::TaskDone`] (`debugTaskRunner.runTask`). A `postDebugTask` is only run; a
//! failure to run it is a notice, as `onDidEndAdapter` shows it.
//!
//! When the `preLaunchTask` failed the session layer asks [`DebugUpdate::TaskFailed`]; the window
//! shows Code-OSS's dialog with its buttons and answers [`DebugRequest::AnswerTask`]. Show Errors
//! opens the 문제 window and Configure Task opens `tasks.json`, as their Code-OSS actions do.
//!
//! What this runner cannot do: it reads no problem matcher, so every task reports no errors and a
//! background (`isBackground`) task cannot be followed to its ready point. Such a task gets
//! Code-OSS's untracked-task error after Code-OSS's 5 second wait, with Debug Anyway offered.
//! The "Waiting for preLaunchTask" progress, the "Remember my choice" checkbox and the
//! `debug.onTaskErrors` setting are not built.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use dioxus_compose::html::prelude::dioxus_elements::Key;
use dioxus_compose::html::prelude::*;
use dioxus_compose::prelude::{Props, component};
use dioxus_core::EventHandler;
use ember_editor::debugger::{DebugRequest, DebugUpdate, TaskChoice, TaskOutcome};

use super::editor::ToolWindow;
use super::editor_tasks::{TaskDef, TasksLoad};
use super::editor_workbench::Shared;
use crate::editor_host::HostCommand;
use crate::terminal_stream::{TerminalHandle, TerminalState};

/// How long a background task may run before it counts as untracked (`debugTaskRunner`: 5000 ms
/// for a background task without a problem matcher).
pub(crate) const BACKGROUND_WAIT: Duration = Duration::from_secs(5);

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

/// How a task's terminal says it ended, once it has: the process's exit code, a terminal that
/// failed for good, or a background task still running after [`BACKGROUND_WAIT`] (Code-OSS's
/// `taskNotTracked`). `None` while it runs.
pub(crate) fn outcome_of(
    label: &str,
    state: &TerminalState,
    background: bool,
    running_for: Duration,
) -> Option<TaskOutcome> {
    if let Some((code, _signal)) = state.exit {
        // Stopped by a signal there is no exit code: Code-OSS takes that as a cancelled task.
        return Some(TaskOutcome::Exited { code: code.map(i64::from), errors: 0 });
    }
    if state.error.is_some() && state.needs_reconnect() {
        let why = state.error_text().unwrap_or_default();
        return Some(TaskOutcome::Failed { message: format!("The task '{label}' could not run: {why}") });
    }
    if background && running_for >= BACKGROUND_WAIT {
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

thread_local! {
    static WAITING: RefCell<Vec<Waiting>> = const { RefCell::new(Vec::new()) };
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
            let mut debug = s.debug;
            debug.write().task_prompt = Some(TaskPrompt { start: *start, message: message.clone(), choices: choices.clone(), error: *error });
            true
        }
        _ => false,
    }
}

fn report(s: &Shared, start: Option<u64>, label: &str, outcome: TaskOutcome) {
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
    if WAITING.with_borrow(|w| w.is_empty()) {
        return;
    }
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
                        }
                    }
                },
            },
            Waiting::Running { label, start, handle, background, since } => {
                match outcome_of(&label, &handle.snapshot(), background, now.saturating_duration_since(since)) {
                    Some(outcome) => report(s, Some(start), &label, outcome),
                    None => keep.push(Waiting::Running { label, start, handle, background, since }),
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

/// The answer to the open question: the session layer hears it, and Show Errors and Configure
/// Task open what Code-OSS opens.
pub(crate) fn answer(s: &Shared, choice: TaskChoice) {
    let mut debug = s.debug;
    let Some(prompt) = debug.write().task_prompt.take() else { return };
    s.host.send(HostCommand::Debug(DebugRequest::AnswerTask { start: prompt.start, choice }));
    match choice {
        TaskChoice::ShowErrors => {
            let mut stripes = s.stripes;
            if !stripes.read().is_selected(ToolWindow::Problems) {
                stripes.write().show(ToolWindow::Problems);
            }
        }
        TaskChoice::ConfigureTask => {
            let path = super::editor_tasks::tasks_json_path(&s.folder);
            super::editor_workbench::page::open_tab_file(s, &path);
        }
        _ => {}
    }
}

/// Code-OSS's dialog for a failed `preLaunchTask`: the message and its buttons, the last one the
/// cancel button (Escape and a click outside choose it).
#[component]
pub(crate) fn TaskPromptSheet(prompt: TaskPrompt, choose: EventHandler<TaskChoice>) -> Element {
    let cancel = prompt.choices.last().copied().unwrap_or(TaskChoice::Abort);
    let title = if prompt.error { "작업 오류" } else { "작업 경고" };
    let buttons: Vec<(usize, TaskChoice, &'static str)> =
        prompt.choices.iter().copied().enumerate().map(|(i, c)| (i, c, choice_label(c))).collect();
    rsx! {
        div { class: "modal", role: "presentation",
            onclick: move |_| choose.call(cancel),
            onkeydown: move |event| {
                if event.key() == Key::Escape {
                    event.stop_propagation();
                    choose.call(cancel);
                }
            },
            div { id: "debug-task-prompt", class: "card ed-confirm", role: "alertdialog", "aria-modal": "true", "aria-label": "{title}",
                onclick: move |event| event.stop_propagation(),
                div { class: "card-b",
                    b { "{prompt.message}" }
                    div { class: "ed-confirm-acts",
                        for (i, choice, label) in buttons {
                            button { key: "{i}", id: "debug-task-choice-{i}",
                                class: if i == 0 { "btn primary sm" } else { "btn sm" },
                                autofocus: choice == cancel,
                                onclick: move |_| choose.call(choice),
                                "{label}"
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The open question, if there is one, for the window's layers.
pub(crate) fn prompt_sheet(s: &Shared) -> Element {
    let Some(prompt) = s.debug.read().task_prompt.clone() else { return rsx! {} };
    let shared = s.clone();
    rsx! { TaskPromptSheet { prompt, choose: move |choice| answer(&shared, choice) } }
}

#[cfg(test)]
#[path = "editor_debug_tasks/tests.rs"]
mod tests;
