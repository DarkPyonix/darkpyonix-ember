//! The editor's launch picker, debug controls and source breakpoint gutter (FR-L31.42).
//!
//! The top bar keeps the task runner's 실행 and puts 디버깅 시작 beside it, as Code-OSS keeps
//! Run Task and Start Debugging apart: 디버깅 시작 (F5) starts the configuration chosen in the
//! 실행 구성 picker, and F5 on a stopped session continues it. The sessions, the stopped line
//! and the breakpoints come from the widget bridge ([`ember_editor::DebugView`]), which keeps
//! them from the session's debug updates.

use compose_rust::code::{GutterDecoration, GutterDecorations};
use compose_rust::schema::ColorRole;
use dioxus_compose::html::prelude::*;
use dioxus_compose::prelude::{Props, component};
use dioxus_core::{EventHandler, use_hook};
use dioxus_signals::{ReadableExt, Signal, WritableExt};
use ember_editor::DebugView;
use ember_editor::debugger::{
    ConfigInfo, ConfigSelector, DebugRequest, DebugUpdate, SessionInfo, SessionState,
    SourceBreakpoint,
};
use ember_editor_conn::uri::UriComponents;

use super::editor_workbench::{Shared, page::icon_tone};
use super::workspace_trust::{Restricted, view::blocked};
use crate::editor_host::HostCommand;

#[derive(Default)]
pub(crate) struct DebugControls {
    configurations: Vec<ConfigInfo>,
    selected: Option<String>,
    configuration_error: Option<String>,
    /// The bridge's view as of the last debug update, here so the page renders when it changes.
    view: DebugView,
    last_gutter_click: Option<(UriComponents, u32)>,
}

impl DebugControls {
    /// Take one debug update: the launch picker's configurations from the update itself, the rest
    /// from the bridge's `view`, which has already taken it.
    fn apply(&mut self, update: &DebugUpdate, view: &DebugView) {
        if let DebugUpdate::Configurations {
            configurations,
            error,
        } = update
        {
            self.configuration_error.clone_from(error);
            self.configurations.clone_from(configurations);
            if !self
                .configurations
                .iter()
                .any(|c| Some(&c.name) == self.selected.as_ref())
            {
                self.selected = self.configurations.first().map(|c| c.name.clone());
            }
        }
        self.view.clone_from(view);
    }

    fn active(&self) -> Option<&SessionInfo> {
        self.view.active_session()
    }

    /// F5 (`workbench.action.debug.start`, or `.continue` while stopped): continue the shown
    /// session when it is stopped, start the chosen configuration when none runs, and nothing while
    /// one runs.
    fn f5_command(&self) -> Option<DebugRequest> {
        match self.active() {
            Some(session) => match session.state {
                SessionState::Stopped { thread_id, .. } => {
                    Some(DebugRequest::Continue {
                        session: session.id.clone(),
                        thread_id: session.thread_id.or(thread_id)?,
                    })
                }
                _ => None,
            },
            None => self.run_command(),
        }
    }

    /// Shift+F5 (`workbench.action.debug.stop`): the shown session; `None` when there is none, and
    /// the key then stops the running task instead.
    fn stop_command(&self) -> Option<DebugRequest> {
        self.active().map(|session| DebugRequest::Stop {
            session: Some(session.id.clone()),
        })
    }

    fn configuration_names(&self) -> Vec<String> {
        self.configurations.iter().map(|c| c.name.clone()).collect()
    }

    fn run_command(&self) -> Option<DebugRequest> {
        Some(DebugRequest::Start {
            selector: ConfigSelector::Name(self.selected.clone()?),
        })
    }

    fn editing_target<'a>(&'a self, uri: &'a UriComponents, caret_line: u32) -> Option<&'a SourceBreakpoint> {
        if let Some(bp) = self.view.breakpoints_in(uri).find(|bp| bp.line == caret_line) {
            return Some(bp);
        }
        let (clicked_uri, line) = self.last_gutter_click.as_ref()?;
        if clicked_uri != uri {
            return None;
        }
        self.view.breakpoints_in(uri).find(|bp| bp.line == *line)
    }
}

/// A debug update the widget bridge passed on (`BridgeAction::Debug`), after it took it.
pub(crate) fn on_debug(s: &Shared, update: &DebugUpdate) {
    if let DebugUpdate::StartFailed { message } = update {
        crate::ui::cr::notice::post(format!("디버깅을 시작하지 못했습니다: {message}"));
        return;
    }
    let view = s.bridge.borrow().debug().clone();
    let mut debug = s.debug;
    debug.write().apply(update, &view);
}

/// The session updates other than debugging ones that the controls follow.
pub(crate) fn on_update(s: &Shared, update: &ember_editor::SessionUpdate) {
    let mut debug = s.debug;
    match update {
        ember_editor::SessionUpdate::ConnectionLost { .. } => debug.set(DebugControls::default()),
        ember_editor::SessionUpdate::Saved { uri }
            if uri.path == format!("{}/.vscode/launch.json", s.folder.trim_end_matches('/')) =>
        {
            s.host
                .send(HostCommand::Debug(DebugRequest::ListConfigurations));
        }
        _ => {}
    }
}

// The renderer reports a gutter mark's ID and uses its own row map to do the hit testing.
// These action IDs use a separate prefix from the session's decoration IDs.
const BREAKPOINT_ACTION: u64 = 0x4250_0000_0000_0000;

fn toggle_from_gutter(uri: &UriComponents, id: u64) -> Option<DebugRequest> {
    let line = id as u32;
    (id >> 32 == BREAKPOINT_ACTION >> 32 && line != 0).then(|| DebugRequest::ToggleBreakpoint {
        uri: uri.clone(),
        line,
    })
}

fn breakpoint_gutter(
    uri: &UriComponents,
    text: &str,
    version: u32,
    view: &DebugView,
    existing: GutterDecorations,
) -> GutterDecorations {
    let by_line: std::collections::HashMap<_, _> =
        view.breakpoints_in(uri).map(|bp| (bp.line, bp)).collect();
    let stopped = view.stopped_line(uri);
    // While a session runs, a breakpoint its adapter did not verify is hollow, as in Code-OSS.
    let in_session = view.active_session().is_some();
    // A blank glyph supplies a hit target on lines without a breakpoint. The other marks
    // share this gutter, including source-control bars and the renderer's fold column.
    let glyphs = (1_u32..).zip(text.split('\n')).map(|(line, _)| {
        let (glyph, color) = match by_line.get(&line) {
            // The stopped line's arrow covers its breakpoint; a click still toggles it.
            _ if stopped == Some(line) => ('▶', ColorRole::Tertiary),
            None => (' ', ColorRole::Background),
            Some(bp) if !bp.enabled => ('●', ColorRole::Outline),
            Some(bp) if in_session && !bp.verified => ('○', ColorRole::Outline),
            Some(bp) if bp.log_message.is_some() => ('▰', ColorRole::Error),
            Some(bp) if bp.condition.is_some() => ('◆', ColorRole::Error),
            Some(_) => ('●', ColorRole::Error),
        };
        GutterDecoration::glyph(
            version,
            line - 1,
            glyph,
            BREAKPOINT_ACTION | u64::from(line),
        )
        .with_color(color)
    });
    GutterDecorations::new(existing.decorations().into_iter().chain(glyphs))
}

pub(crate) fn gutter(
    s: &Shared,
    uri: &UriComponents,
    text: &str,
    version: u32,
    existing: GutterDecorations,
) -> GutterDecorations {
    breakpoint_gutter(uri, text, version, &s.debug.read().view, existing)
}

pub(crate) fn activate_gutter(s: &Shared, uri: &UriComponents, id: u64) -> bool {
    let Some(request @ DebugRequest::ToggleBreakpoint { line, .. }) = toggle_from_gutter(uri, id)
    else {
        return false;
    };
    if s.host.state.lock().unwrap().connected {
        let mut debug = s.debug;
        debug.write().last_gutter_click = Some((uri.clone(), line));
        s.host.send(HostCommand::Debug(request));
    }
    true
}

/// The debug toolbar of a live session (Code-OSS `debugToolBar`): 계속 while stopped and 일시 중지
/// while running in the first place, then the three steps, 다시 시작 and 디버깅 중지.
/// `pause_thread` is the thread a pause goes to ([`DebugView::pause_thread`]).
#[component]
fn DebugToolbar(session: SessionInfo, pause_thread: Option<i64>, send: EventHandler<DebugRequest>) -> Element {
    let id = &session.id;
    let stopped_thread = match session.state {
        SessionState::Stopped { thread_id, .. } => session.thread_id.or(thread_id),
        _ => None,
    };
    let stopped = matches!(session.state, SessionState::Stopped { .. });
    let thread_command = |request: fn(String, i64) -> DebugRequest| {
        stopped_thread.map(|thread| request(id.clone(), thread))
    };
    let first = if stopped {
        (
            "debug-continue",
            "계속",
            "▶",
            thread_command(|session, thread_id| DebugRequest::Continue { session, thread_id }),
        )
    } else {
        (
            "debug-pause",
            "일시 중지",
            "‖",
            pause_thread
                .filter(|_| session.state == SessionState::Running)
                .map(|thread_id| DebugRequest::Pause { session: id.clone(), thread_id }),
        )
    };
    let commands = [
        first,
        (
            "debug-over",
            "프로시저 단위 실행",
            "↷",
            thread_command(|session, thread_id| DebugRequest::StepOver { session, thread_id }),
        ),
        (
            "debug-into",
            "한 단계씩 실행",
            "↓",
            thread_command(|session, thread_id| DebugRequest::StepInto { session, thread_id }),
        ),
        (
            "debug-out",
            "프로시저 나가기",
            "↑",
            thread_command(|session, thread_id| DebugRequest::StepOut { session, thread_id }),
        ),
        (
            "debug-restart",
            "다시 시작",
            "↻",
            Some(DebugRequest::Restart {
                session: id.clone(),
            }),
        ),
        (
            "debug-stop",
            "디버깅 중지",
            "■",
            Some(DebugRequest::Stop {
                session: Some(id.clone()),
            }),
        ),
    ];
    rsx! {
        div { class: "ed-debug-toolbar", role: "toolbar", "aria-label": "디버깅",
            for (id, label, glyph, request) in commands {
                button { key: "{id}", id, class: "ed-ib", title: label, "aria-label": label,
                    disabled: request.is_none(),
                    onclick: move |_| {
                        if let Some(request) = &request { send.call(request.clone()); }
                    },
                    "{glyph}"
                }
            }
        }
    }
}

/// The 실행 구성 pill and the two start buttons: 실행 runs the default task (the task runner),
/// 디버깅 시작 starts the chosen launch configuration (`enabled` says whether there is one to start).
#[component]
fn RunPicker(
    configurations: Vec<String>,
    selected: Option<String>,
    error: Option<String>,
    enabled: bool,
    select: EventHandler<String>,
    run: EventHandler<()>,
    refresh: EventHandler<()>,
    tasks: EventHandler<()>,
    run_task: EventHandler<()>,
) -> Element {
    let mut open = use_hook(|| Signal::new(false));
    let expanded = *open.read();
    let label = selected.as_deref().unwrap_or("실행 구성");
    rsx! {
        div { class: "ed-debug-picker",
            button { id: "debug-configurations", class: "ed-pj", title: "실행 구성", "aria-label": "실행 구성",
                "aria-expanded": expanded,
                onclick: move |_| {
                    open.set(!expanded);
                    if !expanded { refresh.call(()); }
                },
                {icon_tone("play", 16, "text")} span { class: "ellipsis", "{label}" } {icon_tone("chev-dn", 12, "muted")}
            }
            button { id: "task-run", class: "ed-ib run", title: "실행", "aria-label": "실행", onclick: move |_| run_task.call(()), {icon_tone("play", 16, "green")} }
            button { id: "debug-run", class: "ed-ib run", title: "디버깅 시작", "aria-label": "디버깅 시작",
                disabled: !enabled, onclick: move |_| { if enabled { run.call(()); } },
                {icon_tone("bug", 16, "green")}
            }
            if expanded {
                div { class: "ed-debug-popup",
                    if let Some(error) = error { div { class: "ed-debug-error", "{error}" } }
                    for (index, name) in configurations.into_iter().enumerate() {
                        button { key: "{index}", id: "debug-config-{index}", class: "ed-pj",
                            "aria-pressed": selected.as_ref() == Some(&name),
                            onclick: {
                                let name = name.clone();
                                move |_| { select.call(name.clone()); open.set(false); }
                            },
                            "{name}"
                        }
                    }
                    button { class: "ed-pj", onclick: move |_| { tasks.call(()); open.set(false); }, "작업 보기" }
                    button { class: "ed-pj", onclick: move |_| { run_task.call(()); open.set(false); }, "기본 작업 실행" }
                }
            }
        }
    }
}

#[component]
fn BreakpointEditor(breakpoint: SourceBreakpoint, send: EventHandler<DebugRequest>) -> Element {
    let mut open = use_hook(|| Signal::new(false));
    let mut condition = use_hook(|| Signal::new(String::new()));
    let mut log = use_hook(|| Signal::new(String::new()));
    let expanded = *open.read();
    let line = breakpoint.line;
    rsx! {
        div { class: "ed-debug-breakpoint",
            button { id: "debug-edit-breakpoint", class: "ed-pj",
                "aria-expanded": expanded,
                onclick: {
                    let initial_condition = breakpoint.condition.clone();
                    let initial_log = breakpoint.log_message.clone();
                    move |_| {
                        if !expanded {
                            condition.set(initial_condition.clone().unwrap_or_default());
                            log.set(initial_log.clone().unwrap_or_default());
                        }
                        open.set(!expanded);
                    }
                },
                "{line}행 중단점 편집"
            }
            if expanded {
                form { class: "ed-debug-popup",
                    onsubmit: move |event| {
                        event.prevent_default();
                        let condition = condition.read().clone();
                        let log = log.read().clone();
                        send.call(DebugRequest::EditBreakpoint {
                            id: breakpoint.id.clone(),
                            condition: (!condition.trim().is_empty()).then_some(condition),
                            hit_condition: breakpoint.hit_condition.clone(),
                            log_message: (!log.trim().is_empty()).then_some(log),
                            enabled: breakpoint.enabled,
                        });
                        open.set(false);
                    },
                    label { "for": "debug-breakpoint-condition", "조건" }
                    input { id: "debug-breakpoint-condition", value: "{condition}", oninput: move |e| condition.set(e.value()) }
                    label { "for": "debug-breakpoint-log", "로그 메시지" }
                    input { id: "debug-breakpoint-log", value: "{log}", oninput: move |e| log.set(e.value()) }
                    button { id: "debug-save-breakpoint", r#type: "submit", class: "ed-pj", "저장" }
                    button { r#type: "button", class: "ed-pj", onclick: move |_| open.set(false), "취소" }
                }
            }
        }
    }
}

pub(crate) fn run_picker(s: &Shared) -> Element {
    let mut debug = s.debug;
    let state = debug.read();
    let connected = s.host.state.lock().unwrap().connected;
    let run = s.clone();
    let refresh = s.host.clone();
    let tasks = s.clone();
    let run_task = s.clone();
    let stop = s.clone();
    rsx! {
        div { class: "ed-runcfg",
            RunPicker {
                configurations: state.configuration_names(),
                selected: state.selected.clone(),
                error: state.configuration_error.clone(),
                enabled: connected && state.run_command().is_some(),
                select: move |name| debug.write().selected = Some(name),
                run: move |_| {
                    if let Some(reason) = blocked(&run, Restricted::Debug) {
                        crate::ui::cr::notice::post(reason);
                    } else if let Some(request) = debug.peek().run_command() {
                        run.host.send(HostCommand::Debug(request));
                    }
                },
                refresh: move |_| { if connected { refresh.send(HostCommand::Debug(DebugRequest::ListConfigurations)); } },
                tasks: move |_| super::editor_tasks::view::open(&tasks),
                // The task runner's 실행 (and the picker's 기본 작업 실행): a task, so restricted
                // mode names it as one.
                run_task: move |_| {
                    if let Some(reason) = blocked(&run_task, Restricted::Task) {
                        crate::ui::cr::notice::post(reason);
                    } else {
                        super::editor_tasks::view::run_default(&run_task);
                    }
                },
            }
            if let Some(session) = state.active().filter(|_| connected) {
                DebugToolbar { session: session.clone(), pause_thread: state.view.pause_thread(), send: move |request| { stop.host.send(HostCommand::Debug(request)); } }
            } else {
                button { class: "ed-ib", title: "작업 중지", "aria-label": "작업 중지",
                    onclick: move |_| super::editor_tasks::view::stop(&stop),
                    {super::editor_workbench::page::icon("pause")}
                }
            }
        }
    }
}

/// F5 from the keymap (`Action::Debug`): continue the stopped session, or start the chosen
/// configuration, as 디버깅 시작 does.
pub(crate) fn start_or_continue(s: &Shared) {
    let Some(request) = s.debug.peek().f5_command() else {
        if s.debug.peek().active().is_none() {
            crate::ui::cr::notice::post("시작할 실행 구성이 없습니다. .vscode/launch.json에 구성을 추가하세요.");
        }
        return;
    };
    if !s.host.state.lock().unwrap().connected {
        crate::ui::cr::notice::post("서버에 연결되어 있지 않아 디버깅을 시작할 수 없습니다.");
        return;
    }
    if matches!(request, DebugRequest::Start { .. }) {
        if let Some(reason) = blocked(s, Restricted::Debug) {
            crate::ui::cr::notice::post(reason);
            return;
        }
    }
    s.host.send(HostCommand::Debug(request));
}

/// Shift+F5 from the keymap (`Action::Stop`): the debug session, else the running task.
pub(crate) fn stop(s: &Shared) {
    match s.debug.peek().stop_command() {
        Some(request) => {
            s.host.send(HostCommand::Debug(request));
        }
        None => super::editor_tasks::view::stop(s),
    }
}

pub(crate) fn breakpoint_editor(s: &Shared, path: &str) -> Element {
    // Nothing to edit (no file, no caret line or no breakpoint there): an empty element.
    let target = (|| {
        let uri = s.uris.borrow().get(path).cloned()?;
        let line = u32::try_from(s.position.read().0).ok()?;
        s.debug.read().editing_target(&uri, line).cloned()
    })();
    let Some(breakpoint) = target else { return rsx! {} };
    let id = breakpoint.id.clone();
    let host = s.host.clone();
    rsx! {
        BreakpointEditor { key: "{id}", breakpoint, send: move |request| { host.send(HostCommand::Debug(request)); } }
    }
}

#[cfg(test)]
#[path = "editor_debug/tests.rs"]
mod tests;
