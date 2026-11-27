//! FR-L31.42, #445: the problem matchers of a running task, as Code-OSS's terminal task system
//! runs them (`terminalTaskSystem._executeInTerminal`).
//!
//! Every task the 실행 runner starts, from the 실행 window or as a `preLaunchTask`, is tracked
//! here when its `problemMatcher` gives matchers ([`ember_editor::problem_matcher`]). The lines of
//! its output ([`crate::terminal_stream::OutputLines`]) go through a start-stop collector, or the
//! watching one for an `isBackground` task, and what the collectors report goes to the session as
//! the problems of the matcher's owner ([`HostCommand::TaskMarkers`]): the 문제 window and the
//! squiggles show them like an extension's.
//!
//! A background task's passes are what a debug start waits for ([`TrackStatus`]): a pass that
//! began makes the task active (`TaskEventKind.Active`) and one that ended makes it ready
//! (`TaskEventKind.Inactive`).
//!
//! What differs: the problems a run reports are delivered on the window's next render instead of
//! after Code-OSS's 3 s quiet delay, and the "background task but uses a problem matcher without a
//! background pattern" line Code-OSS writes to its task output channel is a notice, because Ember
//! has no such channel.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use ember_editor::problem_matcher::{CollectorEvent, ProblemCollector, ProblemMatcher, Registry};
use dioxus_compose::html::prelude::dioxus_signals::ReadableExt;
use ember_editor_conn::exthost::MarkerData;

use super::editor_tasks::{resolve_vars, TaskDef, VarContext};
use super::editor_workbench::Shared;
use crate::editor_host::HostCommand;
use crate::terminal_stream::{TerminalHandle, TerminalState};

/// What a run's background passes say about it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TrackStatus {
    /// A pass began (`BackgroundProcessingBegins`): the task is tracked, so no untracked-task
    /// error follows.
    pub began: bool,
    /// A pass ended (`BackgroundProcessingEnds`): the task is ready to debug against.
    pub ready: bool,
}

/// One marker change for the session: the owner and, per path, its markers (`None` removes them).
pub(crate) type MarkerChange = (String, Vec<(String, Option<Vec<MarkerData>>)>);

/// The matchers of `task`, with their prefixes resolved against the task's folder `cwd` and the
/// project (`${cwd}`, `${workspaceFolder}`, the file variables), and the problems the definition
/// has in Code-OSS's words.
pub(crate) fn matchers_for(registry: &Registry, task: &TaskDef, ctx: &VarContext, cwd: &str) -> (Vec<ProblemMatcher>, Vec<String>) {
    let mut errors = Vec::new();
    let mut matchers = registry.resolve(task.problem_matcher.as_ref(), &mut errors);
    for m in &mut matchers {
        m.resolve_prefix(|p| {
            let p = p.replace("${cwd}", cwd);
            resolve_vars(&p, ctx).unwrap_or(p)
        });
    }
    (matchers, errors)
}

/// The collector's events as marker changes for the session, with `known` (where each owner has
/// task problems) and the run's `status` kept up to date.
pub(crate) fn apply_events(events: Vec<CollectorEvent>, known: &mut BTreeMap<String, BTreeSet<String>>, status: &mut TrackStatus) -> Vec<MarkerChange> {
    let mut out: Vec<MarkerChange> = Vec::new();
    for e in events {
        match e {
            CollectorEvent::Markers { owner, path, markers } => {
                known.entry(owner.clone()).or_default().insert(path.clone());
                push_change(&mut out, owner, (path, Some(markers)));
            }
            CollectorEvent::Remove { owner, paths } => {
                if let Some(set) = known.get_mut(&owner) {
                    for p in &paths {
                        set.remove(p);
                    }
                }
                for p in paths {
                    push_change(&mut out, owner.clone(), (p, None));
                }
            }
            CollectorEvent::Begins => status.began = true,
            CollectorEvent::Ends => status.ready = true,
        }
    }
    out
}

/// Changes of one owner in a row go out together.
fn push_change(out: &mut Vec<MarkerChange>, owner: String, entry: (String, Option<Vec<MarkerData>>)) {
    match out.last_mut() {
        Some(last) if last.0 == owner => last.1.push(entry),
        _ => out.push((owner, vec![entry])),
    }
}

/// Feed one run: the new `lines`, then `done` when its process has `exited`. Returns the marker
/// changes, and updates `status`.
pub(crate) fn feed(
    collector: &mut ProblemCollector,
    lines: Vec<String>,
    exited: bool,
    open: Vec<String>,
    known: &mut BTreeMap<String, BTreeSet<String>>,
    status: &mut TrackStatus,
) -> Vec<MarkerChange> {
    collector.set_open_documents(open);
    let any = !lines.is_empty();
    for line in &lines {
        collector.process_line(line);
    }
    if any {
        collector.force_delivery();
    }
    if exited {
        collector.done();
    }
    apply_events(collector.take_events(), known, status)
}

struct Tracked {
    handle: TerminalHandle,
    collector: ProblemCollector,
    status: TrackStatus,
}

thread_local! {
    static TRACKED: RefCell<Vec<Tracked>> = const { RefCell::new(Vec::new()) };
    /// Where each owner has problems from tasks, across runs: what Code-OSS's collectors read
    /// back from the marker service to clean.
    static KNOWN: RefCell<BTreeMap<String, BTreeSet<String>>> = const { RefCell::new(BTreeMap::new()) };
}

fn send(s: &Shared, changes: Vec<MarkerChange>) {
    for (owner, entries) in changes {
        s.host.send(HostCommand::TaskMarkers { owner, entries });
    }
}

fn open_documents(s: &Shared) -> Vec<String> {
    s.tabs.read().paths().into_iter().map(str::to_owned).collect()
}

/// Track the run of `task` in `handle`, started in the folder `cwd`, when its `problemMatcher`
/// gives matchers.
pub(crate) fn track(s: &Shared, task: &TaskDef, handle: &TerminalHandle, cwd: &str) {
    let registry = s.host.state.lock().unwrap().problem_matchers.clone().unwrap_or_else(|| std::sync::Arc::new(Registry::builtin()));
    let ctx = VarContext { folder: s.folder.clone(), file: s.tabs.read().current_path().map(str::to_owned) };
    let (matchers, errors) = matchers_for(&registry, task, &ctx, cwd);
    for e in errors {
        crate::ui::cr::notice::post(format!("{}: {}", task.label, e.trim_end()));
    }
    if matchers.is_empty() {
        return;
    }
    let known = KNOWN.with_borrow(Clone::clone);
    let mut collector = ProblemCollector::new(matchers, task.is_background, known);
    if task.is_background && !collector.is_watching() {
        crate::ui::cr::notice::post(format!(
            "Task {} is a background task but uses a problem matcher without a background pattern",
            task.label
        ));
    }
    collector.about_to_start();
    let mut status = TrackStatus::default();
    let changes = KNOWN.with_borrow_mut(|known| apply_events(collector.take_events(), known, &mut status));
    send(s, changes);
    TRACKED.with_borrow_mut(|t| t.push(Tracked { handle: handle.clone(), collector, status }));
}

/// Move the run in `handle` on with the terminal state `state`, and say where its passes are.
/// The debug start calls this with the state it decides on, so the problems of a task that ended
/// reach the session before its outcome does.
pub(crate) fn step(s: &Shared, handle: &TerminalHandle, state: &TerminalState) -> TrackStatus {
    let open = open_documents(s);
    let (changes, status) = TRACKED.with_borrow_mut(|tracked| {
        let Some(i) = tracked.iter().position(|t| t.handle.same(handle)) else {
            return (Vec::new(), TrackStatus::default());
        };
        let exited = state.exit.is_some() || (state.error.is_some() && state.needs_reconnect());
        let t = &mut tracked[i];
        let lines = t.handle.take_output_lines();
        let changes = KNOWN.with_borrow_mut(|known| feed(&mut t.collector, lines, exited, open, known, &mut t.status));
        let status = t.status;
        if exited {
            tracked.remove(i);
        }
        (changes, status)
    });
    send(s, changes);
    status
}

/// Move every tracked run on. Called on every render of the workbench page, before the debug
/// start looks at its task; every terminal change renders the page again.
pub(crate) fn poll(s: &Shared) {
    let handles: Vec<TerminalHandle> = TRACKED.with_borrow(|t| t.iter().map(|t| t.handle.clone()).collect());
    for h in handles {
        let state = h.snapshot();
        step(s, &h, &state);
    }
}

#[cfg(test)]
#[path = "editor_task_problems/tests.rs"]
mod tests;
