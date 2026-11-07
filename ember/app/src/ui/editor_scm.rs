//! Source control in the Ember editor window (FR-L31.2, #444).
//!
//! The 커밋 and Git tool windows read and change the project's repository through the git relay
//! the 버전 관리 screen already uses (`/api/projects/{name}/git/{op}`, FR-G4, via
//! [`crate::ui::vcs::git_call`]). This module holds the state and the rules as plain functions,
//! and the island views in [`view`].
use std::sync::{Arc, Mutex};

use dioxus_compose::html::prelude::dioxus_signals::WritableExt;
use ember_client::api::{ApiError, GitCommitted, GitDiff, GitFile, GitStatus};
use serde_json::{json, Value};

use crate::ui::vcs::{commit_body, commit_done_text, failure_text, git_call, parse_unified, CommitRequest, FileDiff};

/// The longest diff the editor area draws, in lines; a longer one says it was cut.
pub const DIFF_LINE_CAP: usize = 4000;

/// The three groups of the changes list, in the order Code-OSS shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// Unmerged paths.
    Conflicts,
    /// Changes in the index.
    Staged,
    /// Changes in the work tree, untracked files included.
    Changes,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Group::Conflicts => "병합 충돌",
            Group::Staged => "스테이지된 변경 사항",
            Group::Changes => "변경 사항",
        }
    }

    /// Whether a diff of a row in this group is read from the index.
    pub fn staged(self) -> bool {
        self == Group::Staged
    }
}

/// One row of the changes list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The path relative to the repository root, as `git/status` gave it.
    pub path: String,
    /// The old path of a rename or copy.
    pub old_path: Option<String>,
    /// The status letter shown at the row's end.
    pub letter: char,
    pub group: Group,
}

/// The changes of a status, grouped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Groups {
    pub conflicts: Vec<Entry>,
    pub staged: Vec<Entry>,
    pub changes: Vec<Entry>,
}

impl Groups {
    pub fn is_empty(&self) -> bool {
        self.conflicts.is_empty() && self.staged.is_empty() && self.changes.is_empty()
    }

    pub fn get(&self, group: Group) -> &[Entry] {
        match group {
            Group::Conflicts => &self.conflicts,
            Group::Staged => &self.staged,
            Group::Changes => &self.changes,
        }
    }
}

/// A porcelain v2 pair is unmerged when either side is `U`, or both sides added or both deleted.
pub fn is_conflict(f: &GitFile) -> bool {
    f.index == 'U' || f.worktree == 'U' || (f.index == 'A' && f.worktree == 'A') || (f.index == 'D' && f.worktree == 'D')
}

/// The letter Code-OSS shows for one side of a pair: `?` (untracked) reads `U`.
pub fn letter_of(c: char) -> char {
    match c {
        '?' => 'U',
        'M' | 'A' | 'D' | 'R' | 'C' | 'T' => c,
        _ => 'M',
    }
}

/// The Korean word for a status letter, for the row's tooltip and accessible name.
pub fn letter_label(letter: char) -> &'static str {
    match letter {
        'M' => "수정됨",
        'A' => "추가됨",
        'D' => "삭제됨",
        'R' => "이름 바뀜",
        'C' => "복사됨",
        'T' => "형식 바뀜",
        'U' => "추적하지 않음",
        '!' => "충돌",
        _ => "변경됨",
    }
}

/// The colour token of a status letter.
pub fn letter_color(letter: char) -> &'static str {
    match letter {
        'A' | 'U' => "var(--c-green)",
        'D' => "var(--c-red)",
        'R' | 'C' => "var(--c-sky)",
        '!' => "var(--c-orange)",
        'T' => "var(--muted)",
        _ => "var(--c-blue)",
    }
}

/// Group a status as Code-OSS does: a conflict is listed once under 병합 충돌; otherwise a file
/// changed in the index is under 스테이지된 변경 사항 and a file changed in the work tree under
/// 변경 사항, so a file with both is in both lists. Each list is sorted by path.
pub fn groups(status: &GitStatus) -> Groups {
    let mut g = Groups::default();
    for f in &status.files {
        if is_conflict(f) {
            g.conflicts.push(Entry { path: f.path.clone(), old_path: None, letter: '!', group: Group::Conflicts });
            continue;
        }
        if !matches!(f.index, '.' | '?' | '!' | ' ') {
            g.staged.push(Entry { path: f.path.clone(), old_path: f.old_path.clone(), letter: letter_of(f.index), group: Group::Staged });
        }
        if !matches!(f.worktree, '.' | '!' | ' ') {
            g.changes.push(Entry { path: f.path.clone(), old_path: f.old_path.clone(), letter: letter_of(f.worktree), group: Group::Changes });
        }
    }
    for list in [&mut g.conflicts, &mut g.staged, &mut g.changes] {
        list.sort_by(|a, b| a.path.cmp(&b.path));
    }
    g
}

/// The paths a stage or unstage of `entries` sends. A staged rename carries its old path too, so
/// unstaging it also unstages the deletion of the old name (`git reset -- new old`), as Code-OSS does.
pub fn op_paths(entries: &[Entry]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in entries {
        let old = if e.group == Group::Staged { e.old_path.as_ref() } else { None };
        for p in std::iter::once(&e.path).chain(old) {
            if !out.contains(p) {
                out.push(p.clone());
            }
        }
    }
    out
}

/// The branch name as the bars show it: a detached HEAD has no name.
pub fn branch_name(status: &GitStatus) -> String {
    match status.branch.trim() {
        "(detached)" => "분리된 HEAD".to_string(),
        "" => "브랜치 알 수 없음".to_string(),
        b => b.to_string(),
    }
}

/// The branch with the commits ahead of and behind the upstream, for the status bar.
pub fn branch_line(status: &GitStatus) -> String {
    let mut line = branch_name(status);
    if status.ahead > 0 {
        line.push_str(&format!(" \u{2191}{}", status.ahead));
    }
    if status.behind > 0 {
        line.push_str(&format!(" \u{2193}{}", status.behind));
    }
    line
}

/// The upstream sentence of the Git window.
pub fn upstream_text(status: &GitStatus) -> String {
    match &status.upstream {
        None => "추적하는 원격 브랜치가 없습니다.".to_string(),
        Some(u) => match (status.ahead, status.behind) {
            (0, 0) => format!("{u} 기준으로 앞서거나 뒤처진 커밋이 없습니다."),
            (a, 0) => format!("{u}보다 커밋 {a}개 앞서 있습니다."),
            (0, b) => format!("{u}보다 커밋 {b}개 뒤처져 있습니다."),
            (a, b) => format!("{u}보다 커밋 {a}개 앞서 있고 {b}개 뒤처져 있습니다."),
        },
    }
}

/// The commit message box as the `git/commit` body wants it: the first line is the summary and the
/// rest, trimmed, the description. `None` when the first line is empty.
pub fn split_message(text: &str) -> Option<(String, String)> {
    let text = text.trim_start_matches(['\n', '\r']);
    let mut lines = text.lines();
    let summary = lines.next()?.trim().to_string();
    if summary.is_empty() {
        return None;
    }
    let description = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    Some((summary, description))
}

pub const NEED_MESSAGE: &str = "커밋 메시지를 입력하세요.";
pub const NOTHING_STAGED: &str = "스테이지된 변경이 없습니다. 커밋할 파일을 먼저 스테이지하세요.";
pub const CONFLICTS_LEFT: &str = "병합 충돌이 남아 있어 커밋할 수 없습니다. 충돌을 해결한 뒤 스테이지하세요.";
pub const NOT_READ_YET: &str = "저장소 상태를 아직 읽지 못했습니다.";
pub const BUSY: &str = "앞의 git 작업이 끝나기를 기다리는 중입니다.";

/// Whether the commit button may call the server, and with what; else the line it shows instead.
pub fn commit_gate(load: &ScmLoad, message: &str, busy: bool) -> Result<CommitRequest, &'static str> {
    if busy {
        return Err(BUSY);
    }
    let ScmLoad::Ready(status) = load else { return Err(NOT_READ_YET) };
    let g = groups(status);
    if !g.conflicts.is_empty() {
        return Err(CONFLICTS_LEFT);
    }
    let Some((summary, description)) = split_message(message) else { return Err(NEED_MESSAGE) };
    if g.staged.is_empty() {
        return Err(NOTHING_STAGED);
    }
    Ok(CommitRequest { ops: Vec::new(), summary, description, amend: false })
}

/// The state of the repository as the window last read it.
#[derive(Debug, Clone, PartialEq)]
pub enum ScmLoad {
    /// The window has no project, so there is no repository to ask about.
    NoProject,
    Loading,
    Ready(GitStatus),
    /// The project's folder is not a repository (404).
    NotRepo,
    /// The server could not be reached; the text says why.
    Unavailable(String),
    /// The server answered with an error; the Korean line with the node's message.
    Failed(String),
}

impl ScmLoad {
    pub fn status(&self) -> Option<&GitStatus> {
        match self {
            ScmLoad::Ready(s) => Some(s),
            _ => None,
        }
    }

    /// The line the window shows instead of the list.
    pub fn note(&self) -> Option<String> {
        match self {
            ScmLoad::Ready(_) => None,
            ScmLoad::NoProject => Some("이 편집기 창에는 프로젝트가 없어 저장소를 읽을 수 없습니다.".to_string()),
            ScmLoad::Loading => Some("저장소 상태를 읽는 중입니다.".to_string()),
            ScmLoad::NotRepo => Some("이 프로젝트의 폴더는 git 저장소가 아닙니다.".to_string()),
            ScmLoad::Unavailable(why) => Some(format!("서버에 닿지 않아 저장소 상태를 읽을 수 없습니다. ({why})")),
            ScmLoad::Failed(text) => Some(text.clone()),
        }
    }
}

/// A `git/status` answer as a load state, by the rules of the 버전 관리 screen.
pub fn load_from(result: Result<GitStatus, ApiError>) -> ScmLoad {
    match result {
        Ok(s) => ScmLoad::Ready(s),
        Err(e) if e.status() == Some(404) => ScmLoad::NotRepo,
        Err(e @ (ApiError::Peer(_) | ApiError::Transport(_) | ApiError::Push(_) | ApiError::BadUrl(_))) => ScmLoad::Unavailable(e.to_string()),
        Err(e) => ScmLoad::Failed(failure_text("저장소 상태를 확인", &e)),
    }
}

/// What the diff view holds.
#[derive(Debug, Clone, PartialEq)]
pub enum DiffView {
    Loading,
    Ready(FileDiff),
    Binary,
    Empty,
    Failed(String),
}

impl DiffView {
    /// The line shown instead of the lines.
    pub fn note(&self) -> Option<String> {
        match self {
            DiffView::Ready(_) => None,
            DiffView::Loading => Some("차이를 읽는 중입니다.".to_string()),
            DiffView::Binary => Some("바이너리 파일이라 차이를 표시할 수 없습니다.".to_string()),
            DiffView::Empty => Some("표시할 차이가 없습니다.".to_string()),
            DiffView::Failed(text) => Some(text.clone()),
        }
    }
}

/// A `git/diff` answer for one path as a diff view.
pub fn diff_from(result: Result<String, ApiError>) -> DiffView {
    match result {
        Err(e) => DiffView::Failed(failure_text("차이를 확인", &e)),
        Ok(patch) => match parse_unified(&patch).into_iter().next() {
            None => DiffView::Empty,
            Some(d) if d.binary => DiffView::Binary,
            Some(d) if d.hunks.is_empty() => DiffView::Empty,
            Some(d) => DiffView::Ready(d),
        },
    }
}

/// The diff open in the editor area.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffState {
    pub path: String,
    pub staged: bool,
    /// The tab that was current when the diff opened: the diff covers the editor area only while
    /// that tab stays current, so opening or choosing another file shows the file.
    pub over: Option<String>,
    pub view: DiffView,
    seq: u64,
}

impl DiffState {
    pub fn visible(&self, current_tab: Option<&str>) -> bool {
        self.over.as_deref() == current_tab
    }

    /// "이름 (스테이지됨)" or "이름 (작업 트리)", as Code-OSS titles a diff "(Index)" or "(Working Tree)".
    pub fn title(&self) -> String {
        let side = if self.staged { "스테이지됨" } else { "작업 트리" };
        format!("{} ({side})", crate::ui::editor::base_name(&self.path))
    }
}

/// Everything the source control windows keep between frames.
#[derive(Debug, Clone, PartialEq)]
pub struct ScmState {
    pub load: ScmLoad,
    /// A status read is on the way (the list keeps the last answer meanwhile).
    pub refreshing: bool,
    /// A stage, unstage or commit is on the way; the buttons wait.
    pub busy: bool,
    pub message: String,
    pub diff: Option<DiffState>,
    seq: u64,
    diff_seq: u64,
}

impl ScmState {
    pub fn new(has_project: bool) -> ScmState {
        ScmState {
            load: if has_project { ScmLoad::Loading } else { ScmLoad::NoProject },
            refreshing: false,
            busy: false,
            message: String::new(),
            diff: None,
            seq: 0,
            diff_seq: 0,
        }
    }

    /// Starts a status read; the answer carries the returned number.
    pub fn begin_load(&mut self) -> u64 {
        self.seq += 1;
        self.refreshing = true;
        if !matches!(self.load, ScmLoad::Ready(_)) {
            self.load = ScmLoad::Loading;
        }
        self.seq
    }

    /// Takes a status answer unless a newer read started after it. Returns whether it was taken.
    pub fn finish_load(&mut self, seq: u64, load: ScmLoad) -> bool {
        if seq != self.seq {
            return false;
        }
        self.refreshing = false;
        self.load = load;
        true
    }

    /// Opens the diff of `path` over the current tab; the answer carries the returned number.
    pub fn begin_diff(&mut self, path: &str, staged: bool, over: Option<String>) -> u64 {
        self.diff_seq += 1;
        self.diff = Some(DiffState { path: path.to_string(), staged, over, view: DiffView::Loading, seq: self.diff_seq });
        self.diff_seq
    }

    /// Takes a diff answer if that diff is still the one open.
    pub fn finish_diff(&mut self, seq: u64, view: DiffView) -> bool {
        match self.diff.as_mut() {
            Some(d) if d.seq == seq => {
                d.view = view;
                true
            }
            _ => false,
        }
    }

    pub fn close_diff(&mut self) {
        self.diff = None;
    }
}

/// The source control handle the editor window shares: the state, the window's redraw counter
/// and the project the git calls name.
#[derive(Clone)]
pub struct Scm {
    pub state: Arc<Mutex<ScmState>>,
    revision: dioxus_compose::prelude::SyncSignal<u64>,
    project: String,
}

impl Scm {
    pub fn new(project: &str, revision: dioxus_compose::prelude::SyncSignal<u64>) -> Scm {
        let project = project.trim().to_string();
        Scm { state: Arc::new(Mutex::new(ScmState::new(!project.is_empty()))), revision, project }
    }

    pub fn snapshot(&self) -> ScmState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn with<R>(&self, f: impl FnOnce(&mut ScmState) -> R) -> R {
        f(&mut self.state.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn redraw(revision: dioxus_compose::prelude::SyncSignal<u64>) {
        let mut revision = revision;
        *revision.write() += 1;
    }

    /// The branch the bars show, once a status has been read.
    pub fn branch_line(&self) -> Option<String> {
        self.with(|s| s.load.status().map(branch_line))
    }

    /// Whether the diff covers the editor area for this current tab.
    pub fn diff_visible(&self, current_tab: Option<&str>) -> bool {
        self.with(|s| s.diff.as_ref().is_some_and(|d| d.visible(current_tab)))
    }

    pub fn close_diff(&self) {
        let had = self.with(|s| s.diff.take().is_some());
        if had {
            Self::redraw(self.revision);
        }
    }

    pub fn set_message(&self, text: String) {
        self.with(|s| s.message = text);
        Self::redraw(self.revision);
    }

    /// Reads the status again, and the open diff with it. It does not redraw before the answer, so
    /// a render may call it (after a save) without writing the signal it reads.
    pub fn refresh(&self) {
        if self.project.is_empty() {
            return;
        }
        let seq = self.with(ScmState::begin_load);
        let (state, revision, project) = (self.state.clone(), self.revision, self.project.clone());
        crate::services::run_shared(async move { git_call::<GitStatus>(&project, "status", json!({})).await }, move |result| {
            let taken = state.lock().unwrap_or_else(|e| e.into_inner()).finish_load(seq, load_from(result));
            if taken {
                Self::redraw(revision);
            }
        });
        let open = self.with(|s| s.diff.as_ref().map(|d| (d.path.clone(), d.staged, d.over.clone())));
        if let Some((path, staged, over)) = open {
            self.load_diff(&path, staged, over);
        }
    }

    /// Opens the diff of `path` (relative to the repository) in the editor area over `over`.
    pub fn open_diff(&self, path: &str, staged: bool, over: Option<String>) {
        if self.project.is_empty() {
            return;
        }
        self.load_diff(path, staged, over);
        Self::redraw(self.revision);
    }

    fn load_diff(&self, path: &str, staged: bool, over: Option<String>) {
        let seq = self.with(|s| s.begin_diff(path, staged, over));
        let (state, revision, project, path) = (self.state.clone(), self.revision, self.project.clone(), path.to_string());
        crate::services::run_shared(
            async move {
                let d: Result<GitDiff, ApiError> = git_call(&project, "diff", json!({ "path": path, "staged": staged, "include_untracked": false })).await;
                d.map(|d| d.patch)
            },
            move |result| {
                let taken = state.lock().unwrap_or_else(|e| e.into_inner()).finish_diff(seq, diff_from(result));
                if taken {
                    Self::redraw(revision);
                }
            },
        );
    }

    /// Runs one git call that changes the repository, then reads the status again. `what` names it
    /// in the failure line; `done` is the notice on success, if any.
    fn change(&self, what: &'static str, op: &'static str, body: Value, done: Option<String>) {
        let started = self.with(|s| {
            if s.busy {
                false
            } else {
                s.busy = true;
                true
            }
        });
        if !started {
            crate::ui::cr::notice::post(BUSY);
            return;
        }
        Self::redraw(self.revision);
        let (this, project) = (self.clone(), self.project.clone());
        let state = self.state.clone();
        let revision = self.revision;
        crate::services::run_shared(async move { git_call::<Value>(&project, op, body).await }, move |result| {
            state.lock().unwrap_or_else(|e| e.into_inner()).busy = false;
            match result {
                Ok(_) => {
                    if let Some(text) = done {
                        crate::ui::cr::notice::post(text);
                    }
                }
                Err(e) => crate::ui::cr::notice::post(failure_text(what, &e)),
            }
            Self::redraw(revision);
            this.refresh();
        });
    }

    pub fn stage(&self, entries: &[Entry]) {
        let paths = op_paths(entries);
        if paths.is_empty() {
            return;
        }
        self.change("스테이지", "stage", json!({ "paths": paths }), None);
    }

    pub fn unstage(&self, entries: &[Entry]) {
        let paths = op_paths(entries);
        if paths.is_empty() {
            return;
        }
        self.change("스테이지 해제", "unstage", json!({ "paths": paths }), None);
    }

    /// Commits the index with the message box, after the gate's checks.
    pub fn commit(&self) {
        let gate = self.with(|s| commit_gate(&s.load, &s.message, s.busy).map(|req| {
            s.busy = true;
            req
        }));
        let req = match gate {
            Ok(req) => req,
            Err(line) => {
                crate::ui::cr::notice::post(line);
                return;
            }
        };
        Self::redraw(self.revision);
        let (this, project, state, revision) = (self.clone(), self.project.clone(), self.state.clone(), self.revision);
        let submitted = format!("{}\n{}", req.summary, req.description);
        crate::services::run_shared(
            async move { git_call::<GitCommitted>(&project, "commit", commit_body(&req)).await },
            move |result| {
                {
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.busy = false;
                    if result.is_ok() && split_message(&s.message).map(|(a, b)| format!("{a}\n{b}")) == Some(submitted) {
                        // Only the text that was committed is cleared; a box edited meanwhile keeps the edit.
                        s.message.clear();
                    }
                }
                crate::ui::cr::notice::post(match result {
                    Ok(done) => commit_done_text(&done.sha),
                    Err(e) => failure_text("커밋", &e),
                });
                Self::redraw(revision);
                this.refresh();
            },
        );
    }
}

/// The island tool windows and the diff view.
pub mod view {
    use dioxus_compose::html::prelude::dioxus_elements::point_interaction::ModifiersInteraction;
    use dioxus_compose::html::prelude::dioxus_elements::{Key, Modifiers};
    use dioxus_compose::html::prelude::dioxus_signals::{ReadableExt, WritableExt};
    use dioxus_compose::html::prelude::*;

    use super::{groups, letter_color, letter_label, upstream_text, branch_name, Entry, Group, DIFF_LINE_CAP, DiffView};
    use crate::editor_host::HostCommand;
    use crate::ui::editor::{base_name, Side, ToolWindow};
    use crate::ui::editor_workbench::page::{icon, icon_px};
    use crate::ui::editor_workbench::{join_path, Shared};
    use crate::ui::vcs::LineKind;

    /// The folder part of a repository path, for the muted text after the name.
    fn dir_of(path: &str) -> &str {
        path.rfind('/').map(|i| &path[..i]).unwrap_or("")
    }

    /// Opens the file of a repository path in a tab (the project folder is taken as the repository root).
    fn open_file(s: &Shared, rel: &str) {
        let path = join_path(&s.folder, rel);
        s.scm.close_diff();
        s.open_errors.borrow_mut().remove(&path);
        let mut tabs = s.tabs;
        tabs.write().open(&path);
        s.host.send(HostCommand::Open(path.clone()));
        s.host.send(HostCommand::Focus(path));
    }

    /// Shows the Git window (from the branch in the top bar or the status bar).
    pub fn show_git(s: &Shared) {
        let mut stripes = s.stripes;
        if !stripes.read().is_selected(ToolWindow::Git) {
            stripes.write().toggle(ToolWindow::Git);
        }
        s.scm.refresh();
    }

    fn header(s: &Shared, title: &str) -> Element {
        let mut stripes = s.stripes;
        let refresh_s = s.clone();
        let refreshing = s.scm.snapshot().refreshing;
        rsx! {
            div { class: "ed-twh",
                b { class: "ed-twt", "{title}" }
                div { class: "spacer" }
                button { class: "ed-ib", title: if refreshing { "읽는 중" } else { "새로 고침" }, disabled: refreshing, onclick: move |_| refresh_s.scm.refresh(), {icon("refresh")} }
                button { class: "ed-ib", title: "숨기기", onclick: move |_| stripes.write().hide(Side::Left), {icon("close")} }
            }
        }
    }

    fn note(text: &str) -> Element {
        rsx! { div { style: "padding:var(--sp2) var(--sp3); color:var(--muted); font-size:var(--fs-xs); white-space:normal; line-height:1.6;", role: "status", "{text}" } }
    }

    /// The 커밋 window: the branch, the message box and commit button, and the changes in groups.
    pub fn commit_window(s: &Shared) -> Element {
        let st = s.scm.snapshot();
        let busy = st.busy;
        let message = st.message.clone();
        let g = st.load.status().map(groups);
        let branch = st.load.status().map(super::branch_line);
        let open_diff = st.diff.as_ref().map(|d| (d.path.clone(), d.staged));
        let commit_hint = if cfg!(target_os = "macos") { "\u{2318}Enter" } else { "Ctrl+Enter" };
        let commit_title = format!("커밋 ({commit_hint})");
        let (input_s, key_s, click_s) = (s.clone(), s.clone(), s.clone());
        rsx! {
            section { class: "ed-tw ed-left", "aria-label": "커밋",
                {header(s, "커밋")}
                div { style: "display:flex; flex-direction:column; gap:var(--sp2); padding:var(--sp2) var(--sp3); border-bottom:1px solid var(--line); flex:none;",
                    if let Some(branch) = branch {
                        div { style: "display:flex; align-items:center; gap:6px; color:var(--text-2); font-size:var(--fs-xs); min-width:0; white-space:nowrap; overflow:hidden;", {icon_px("git", 14)} "{branch}" }
                    }
                    textarea {
                        style: "min-height:64px; resize:vertical; border:1px solid var(--line); border-radius:var(--r-sm); background:var(--surface); color:var(--text); padding:6px 8px; font:inherit; font-size:var(--fs-sm);",
                        "aria-label": "커밋 메시지", placeholder: "커밋 메시지 (첫 줄은 요약)", value: "{message}",
                        oninput: move |event| input_s.scm.set_message(event.value()),
                        onkeydown: move |event| {
                            let m = event.modifiers();
                            if event.key() == Key::Enter && (m.contains(Modifiers::META) || m.contains(Modifiers::CONTROL)) {
                                event.stop_propagation();
                                event.prevent_default();
                                key_s.scm.commit();
                            }
                        },
                    }
                    button { class: "ed-pj", title: "{commit_title}", disabled: busy,
                        style: "justify-content:center; background:var(--ink); color:var(--ink-text); height:30px;",
                        onclick: move |_| click_s.scm.commit(),
                        {icon_px("check", 14)} if busy { "작업하는 중…" } else { "커밋" }
                    }
                }
                div { class: "ed-tree", role: "list", "aria-label": "변경된 파일",
                    if let Some(text) = st.load.note() { {note(&text)} }
                    if let Some(g) = g {
                        if g.is_empty() {
                            {note("변경 사항이 없습니다.")}
                        }
                        for group in [Group::Conflicts, Group::Staged, Group::Changes] {
                            if !g.get(group).is_empty() || (group == Group::Staged && !g.changes.is_empty()) {
                                {group_block(s, group, g.get(group).to_vec(), busy, open_diff.clone())}
                            }
                        }
                    }
                }
            }
        }
    }

    fn group_block(s: &Shared, group: Group, entries: Vec<Entry>, busy: bool, open_diff: Option<(String, bool)>) -> Element {
        let count = entries.len();
        let all_s = s.clone();
        let all = entries.clone();
        let (all_title, all_icon) = match group {
            Group::Staged => ("모두 스테이지 해제", "minus"),
            _ => ("모두 스테이지", "plus"),
        };
        rsx! {
            div { key: "{group.title()}", role: "group", "aria-label": "{group.title()} {count}개",
                div { class: "ed-tn", style: "--d:0; cursor:default; color:var(--text-2); font-size:var(--fs-xs); font-weight:var(--fw-semibold);",
                    "{group.title()}"
                    span { style: "margin-left:var(--sp1); padding:0 6px; border-radius:var(--pill); background:var(--surface-2); color:var(--text-2);", "{count}" }
                    div { class: "spacer" }
                    if count > 0 && group != Group::Conflicts {
                        button { class: "ed-ib", style: "width:22px; height:22px;", title: "{all_title}", disabled: busy,
                            onclick: move |_| { if group == Group::Staged { all_s.scm.unstage(&all) } else { all_s.scm.stage(&all) } },
                            {icon_px(all_icon, 13)} }
                    }
                }
                if count == 0 && group == Group::Staged {
                    div { style: "padding:2px var(--sp3) var(--sp2) 22px; color:var(--muted); font-size:var(--fs-2xs);", "스테이지된 파일이 없습니다. 아래 변경 사항에서 +를 누르세요." }
                }
                for e in entries {
                    {entry_row(s, e.clone(), busy, open_diff.as_ref() == Some(&(e.path.clone(), group.staged())))}
                }
            }
        }
    }

    fn entry_row(s: &Shared, e: Entry, busy: bool, selected: bool) -> Element {
        let name = base_name(&e.path).to_string();
        let dir = dir_of(&e.path).to_string();
        let label = letter_label(e.letter);
        let color = letter_color(e.letter);
        let row_label = if dir.is_empty() { format!("{name}, {label}") } else { format!("{name}, {label}, {dir}") };
        let class = if selected { "ed-tn sel" } else { "ed-tn" };
        let group = e.group;
        let deleted = e.letter == 'D';
        let (diff_s, open_s, op_s) = (s.clone(), s.clone(), s.clone());
        let (diff_path, open_path) = (e.path.clone(), e.path.clone());
        let op_entry = vec![e.clone()];
        let (op_title, op_icon) = match group {
            Group::Staged => ("스테이지 해제", "minus"),
            _ => ("스테이지", "plus"),
        };
        let tip = match &e.old_path {
            Some(old) => format!("{old} \u{2192} {}, {label}", e.path),
            None => format!("{}, {label}", e.path),
        };
        rsx! {
            div { key: "{group.title()}:{e.path}", class, role: "listitem", "aria-label": "{row_label}", title: "{tip}", style: "--d:1;",
                onclick: move |_| {
                    let over = diff_s.tabs.read().current_path().map(str::to_owned);
                    diff_s.scm.open_diff(&diff_path, group.staged(), over);
                },
                {icon_px("file", 15)}
                span { style: "overflow:hidden; text-overflow:ellipsis; flex:0 1 auto; min-width:0;", "{name}" }
                if !dir.is_empty() { span { style: "color:var(--muted); font-size:var(--fs-2xs); overflow:hidden; text-overflow:ellipsis; flex:1 1 auto; min-width:0;", "{dir}" } }
                div { class: "spacer" }
                if !deleted {
                    button { class: "ed-ib", style: "width:22px; height:22px;", title: "파일 열기: {name}",
                        onclick: move |event| { event.stop_propagation(); open_file(&open_s, &open_path); }, {icon_px("file", 13)} }
                }
                if group != Group::Conflicts {
                    button { class: "ed-ib", style: "width:22px; height:22px;", title: "{op_title}: {name}", disabled: busy,
                        onclick: move |event| {
                            event.stop_propagation();
                            if group == Group::Staged { op_s.scm.unstage(&op_entry) } else { op_s.scm.stage(&op_entry) }
                        }, {icon_px(op_icon, 13)} }
                }
                span { style: "width:14px; flex:none; text-align:center; font-family:var(--mono); font-size:var(--fs-xs); font-weight:var(--fw-semibold); color:{color};", title: "{label}", "{e.letter}" }
            }
        }
    }

    /// The Git window: the branch, its upstream and the local branches from the same status.
    pub fn git_window(s: &Shared) -> Element {
        let st = s.scm.snapshot();
        let status = st.load.status().cloned();
        rsx! {
            section { class: "ed-tw ed-left", "aria-label": "Git",
                {header(s, "Git")}
                div { class: "ed-tree", role: "list", "aria-label": "브랜치",
                    if let Some(text) = st.load.note() { {note(&text)} }
                    if let Some(status) = status {
                        div { class: "ed-tn", style: "--d:0; cursor:default; font-weight:var(--fw-semibold);", {icon_px("git", 15)} "{branch_name(&status)}" }
                        {note(&upstream_text(&status))}
                        div { class: "ed-tn", style: "--d:0; cursor:default; color:var(--text-2); font-size:var(--fs-xs); font-weight:var(--fw-semibold);", "로컬 브랜치"
                            span { style: "margin-left:var(--sp1); padding:0 6px; border-radius:var(--pill); background:var(--surface-2); color:var(--text-2);", "{status.branches.len()}" } }
                        if status.branches.is_empty() { {note("로컬 브랜치가 없습니다.")} }
                        for b in status.branches.iter() {
                            {
                                let current = *b == status.branch;
                                let label = if current { format!("{b}, 현재 브랜치") } else { b.clone() };
                                rsx! { div { key: "{b}", class: if current { "ed-tn sel" } else { "ed-tn" }, role: "listitem", "aria-label": "{label}", style: "--d:1; cursor:default;",
                                    if current { {icon_px("check", 13)} } else { span { class: "chev-sp" } }
                                    "{b}"
                                } }
                            }
                        }
                        div { style: "padding:var(--sp2) var(--sp3); color:var(--muted); font-size:var(--fs-2xs); white-space:normal; line-height:1.6;", "브랜치 전환과 원격 작업은 앱의 버전 관리 화면에서 할 수 있습니다." }
                    }
                }
            }
        }
    }

    /// The diff of one file in the editor area: a header with 파일 열기 and 닫기, then the hunks
    /// with old and new line numbers.
    pub fn diff_pane(s: &Shared) -> Element {
        let Some(d) = s.scm.snapshot().diff else { return rsx! {} };
        let title = d.title();
        let (open_s, close_s) = (s.clone(), s.clone());
        let open_path = d.path.clone();
        let mut lines: Vec<(Option<u32>, Option<u32>, Option<LineKind>, String)> = Vec::new();
        let mut cut = false;
        if let DiffView::Ready(fd) = &d.view {
            'outer: for h in &fd.hunks {
                lines.push((None, None, None, h.header.clone()));
                for l in &h.lines {
                    if lines.len() >= DIFF_LINE_CAP {
                        cut = true;
                        break 'outer;
                    }
                    lines.push((l.old_no, l.new_no, Some(l.kind), l.text.clone()));
                }
            }
        }
        let summary = match &d.view {
            DiffView::Ready(fd) => format!("+{} \u{2212}{}", fd.added(), fd.removed()),
            _ => String::new(),
        };
        rsx! {
            div { style: "flex:1; min-width:0; min-height:0; display:flex; flex-direction:column;", role: "region", "aria-label": "변경 내용: {title}",
                div { class: "ed-tsub",
                    {icon_px("file", 14)}
                    b { "{title}" }
                    span { class: "ed-sbi", "{summary}" }
                    div { class: "spacer" }
                    button { class: "ed-pj", style: "border:1px solid var(--line);", onclick: move |_| open_file(&open_s, &open_path), "파일 열기" }
                    button { class: "ed-ib", title: "차이 닫기", onclick: move |_| close_s.scm.close_diff(), {icon("close")} }
                }
                if let Some(text) = d.view.note() {
                    div { class: "ed-empty", role: "status", p { "{text}" } }
                } else {
                    div { style: "flex:1; min-height:0; overflow:auto; font-family:var(--mono); font-size:var(--fs-sm); padding:var(--sp2) 0;",
                        for (i, (old_no, new_no, kind, text)) in lines.into_iter().enumerate() {
                            {
                                let (bg, sign, fg) = match kind {
                                    None => ("background:var(--surface-2);", " ", "color:var(--muted);"),
                                    Some(LineKind::Add) => ("background:var(--c-green-soft);", "+", ""),
                                    Some(LineKind::Del) => ("background:var(--c-red-soft);", "-", ""),
                                    Some(LineKind::Context) => ("", " ", ""),
                                };
                                let old = old_no.map(|n| n.to_string()).unwrap_or_default();
                                let new = new_no.map(|n| n.to_string()).unwrap_or_default();
                                rsx! { div { key: "{i}", style: "display:flex; white-space:pre; min-height:20px; line-height:20px; {bg} {fg}",
                                    span { style: "width:48px; flex:none; text-align:right; padding-right:8px; color:var(--muted);", "{old}" }
                                    span { style: "width:48px; flex:none; text-align:right; padding-right:8px; color:var(--muted);", "{new}" }
                                    span { style: "width:16px; flex:none; text-align:center;", "{sign}" }
                                    span { style: "flex:1;", "{text}" }
                                } }
                            }
                        }
                        if cut {
                            div { style: "padding:var(--sp2) var(--sp3); color:var(--muted); font-family:var(--font);", "차이가 길어 앞의 {DIFF_LINE_CAP}줄만 표시합니다. 전체는 파일을 열어 확인하세요." }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, index: char, worktree: char) -> GitFile {
        GitFile { path: path.into(), old_path: None, index, worktree }
    }

    fn status(files: Vec<GitFile>) -> GitStatus {
        GitStatus { branch: "main".into(), files, ..GitStatus::default() }
    }

    #[test]
    fn fr_l31_2_a_file_changed_in_index_and_work_tree_is_in_both_groups() {
        let g = groups(&status(vec![file("src/b.rs", 'M', 'M'), file("a.rs", 'A', '.'), file("new.txt", '?', '?'), file("gone.rs", '.', 'D')]));
        assert_eq!(g.staged.iter().map(|e| (e.path.as_str(), e.letter)).collect::<Vec<_>>(), vec![("a.rs", 'A'), ("src/b.rs", 'M')]);
        assert_eq!(g.changes.iter().map(|e| (e.path.as_str(), e.letter)).collect::<Vec<_>>(), vec![("gone.rs", 'D'), ("new.txt", 'U'), ("src/b.rs", 'M')]);
        assert!(g.conflicts.is_empty());
    }

    #[test]
    fn fr_l31_2_unmerged_pairs_are_conflicts_listed_once() {
        let g = groups(&status(vec![file("x", 'U', 'U'), file("y", 'A', 'A'), file("z", 'D', 'D'), file("w", 'D', 'U')]));
        assert_eq!(g.conflicts.len(), 4);
        assert!(g.conflicts.iter().all(|e| e.letter == '!'));
        assert!(g.staged.is_empty() && g.changes.is_empty());
    }

    #[test]
    fn fr_l31_2_status_letters_and_words() {
        assert_eq!(letter_of('?'), 'U');
        assert_eq!(letter_of('R'), 'R');
        assert_eq!(letter_of('T'), 'T');
        assert_eq!(letter_label('U'), "추적하지 않음");
        assert_eq!(letter_label('!'), "충돌");
    }

    #[test]
    fn fr_l31_2_a_staged_rename_unstages_with_its_old_path() {
        let mut f = file("new.rs", 'R', '.');
        f.old_path = Some("old.rs".into());
        let mut w = file("moved.rs", '.', 'R');
        w.old_path = Some("was.rs".into());
        let g = groups(&status(vec![f, file("a.rs", 'M', '.'), w]));
        assert_eq!(op_paths(&g.staged), vec!["a.rs", "new.rs", "old.rs"]);
        assert_eq!(op_paths(&g.changes), vec!["moved.rs"]);
    }

    #[test]
    fn fr_l31_2_commit_message_first_line_is_the_summary() {
        assert_eq!(split_message("Fix: thing\n\nWhy it was wrong.\n"), Some(("Fix: thing".into(), "Why it was wrong.".into())));
        assert_eq!(split_message("\n\nOnly summary  "), Some(("Only summary".into(), String::new())));
        assert_eq!(split_message("   \nbody"), None);
        assert_eq!(split_message(""), None);
    }

    #[test]
    fn fr_l31_2_commit_is_refused_with_a_reason_before_any_call() {
        let ready = ScmLoad::Ready(status(vec![file("a.rs", 'M', '.')]));
        assert_eq!(commit_gate(&ready, "", false).unwrap_err(), NEED_MESSAGE);
        assert_eq!(commit_gate(&ready, "msg", true).unwrap_err(), BUSY);
        assert_eq!(commit_gate(&ScmLoad::Loading, "msg", false).unwrap_err(), NOT_READ_YET);
        let unstaged = ScmLoad::Ready(status(vec![file("a.rs", '.', 'M')]));
        assert_eq!(commit_gate(&unstaged, "msg", false).unwrap_err(), NOTHING_STAGED);
        let conflicted = ScmLoad::Ready(status(vec![file("a.rs", 'U', 'U')]));
        assert_eq!(commit_gate(&conflicted, "msg", false).unwrap_err(), CONFLICTS_LEFT);
        let req = commit_gate(&ready, "Feat: x\n\nbody", false).unwrap();
        assert_eq!((req.summary.as_str(), req.description.as_str(), req.amend), ("Feat: x", "body", false));
        assert!(req.ops.is_empty());
    }

    #[test]
    fn fr_l31_2_branch_and_upstream_text() {
        let mut s = status(vec![]);
        assert_eq!(branch_line(&s), "main");
        s.ahead = 2;
        s.behind = 1;
        assert_eq!(branch_line(&s), "main \u{2191}2 \u{2193}1");
        s.branch = "(detached)".into();
        assert_eq!(branch_name(&s), "분리된 HEAD");
        assert_eq!(upstream_text(&s), "추적하는 원격 브랜치가 없습니다.");
        s.upstream = Some("origin/main".into());
        assert_eq!(upstream_text(&s), "origin/main보다 커밋 2개 앞서 있고 1개 뒤처져 있습니다.");
        s.ahead = 0;
        s.behind = 0;
        assert_eq!(upstream_text(&s), "origin/main 기준으로 앞서거나 뒤처진 커밋이 없습니다.");
    }

    #[test]
    fn fr_l31_2_each_server_answer_has_a_load_state() {
        assert_eq!(load_from(Ok(GitStatus::default())), ScmLoad::Ready(GitStatus::default()));
        let not_repo = ApiError::Status { status: 404, message: "not_a_repo".into(), code: None };
        assert_eq!(load_from(Err(not_repo)), ScmLoad::NotRepo);
        let failed = ApiError::Status { status: 500, message: "git exploded".into(), code: None };
        assert_eq!(load_from(Err(failed)), ScmLoad::Failed("저장소 상태를 확인하지 못했습니다: git exploded".into()));
        assert!(matches!(load_from(Err(ApiError::Peer("down".into()))), ScmLoad::Unavailable(_)));
        assert_eq!(ScmState::new(false).load, ScmLoad::NoProject);
        assert_eq!(ScmState::new(true).load, ScmLoad::Loading);
    }

    #[test]
    fn fr_l31_2_a_stale_status_answer_is_dropped() {
        let mut st = ScmState::new(true);
        let first = st.begin_load();
        let second = st.begin_load();
        assert!(!st.finish_load(first, ScmLoad::NotRepo));
        assert_eq!(st.load, ScmLoad::Loading);
        assert!(st.refreshing);
        assert!(st.finish_load(second, ScmLoad::Ready(GitStatus::default())));
        assert!(!st.refreshing);
        // A refresh keeps the last list on screen until the answer comes.
        st.begin_load();
        assert!(matches!(st.load, ScmLoad::Ready(_)));
    }

    #[test]
    fn fr_l31_2_the_diff_covers_only_the_tab_it_opened_over() {
        let mut st = ScmState::new(true);
        let first = st.begin_diff("a.rs", false, Some("/w/a.rs".into()));
        let second = st.begin_diff("b.rs", true, Some("/w/a.rs".into()));
        assert!(!st.finish_diff(first, DiffView::Empty));
        assert!(st.finish_diff(second, DiffView::Binary));
        let d = st.diff.clone().unwrap();
        assert_eq!(d.title(), "b.rs (스테이지됨)");
        assert!(d.visible(Some("/w/a.rs")));
        assert!(!d.visible(Some("/w/other.rs")));
        assert!(!d.visible(None));
        st.close_diff();
        assert!(st.diff.is_none());
    }

    #[test]
    fn fr_l31_2_diff_answers_become_views() {
        let patch = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n one\n-two\n+three\n";
        match diff_from(Ok(patch.into())) {
            DiffView::Ready(d) => assert_eq!((d.added(), d.removed()), (1, 1)),
            other => panic!("expected a diff, got {other:?}"),
        }
        assert_eq!(diff_from(Ok(String::new())), DiffView::Empty);
        let failed = ApiError::Status { status: 500, message: "boom".into(), code: None };
        assert_eq!(diff_from(Err(failed)), DiffView::Failed("차이를 확인하지 못했습니다: boom".into()));
    }

    /// NFR-L4: a sentence ends in 합니다체 ("~니다."), a call to action may end in "~하세요.".
    fn polite(text: &str) -> bool {
        text.split(['.', '?'])
            .map(str::trim)
            .filter(|s| s.ends_with('다'))
            .all(|s| s.ends_with("니다"))
    }

    #[test]
    fn nfr_l4_source_control_strings_are_polite() {
        let mut s = status(vec![]);
        s.upstream = Some("origin/main".into());
        let mut lines: Vec<String> = vec![NEED_MESSAGE, NOTHING_STAGED, CONFLICTS_LEFT, NOT_READ_YET, BUSY].into_iter().map(String::from).collect();
        for load in [ScmLoad::NoProject, ScmLoad::Loading, ScmLoad::NotRepo, ScmLoad::Unavailable("x".into())] {
            lines.push(load.note().unwrap());
        }
        for view in [DiffView::Loading, DiffView::Binary, DiffView::Empty] {
            lines.push(view.note().unwrap());
        }
        for (a, b) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            s.ahead = a;
            s.behind = b;
            lines.push(upstream_text(&s));
        }
        s.upstream = None;
        lines.push(upstream_text(&s));
        for line in &lines {
            assert!(polite(line), "not 합니다체: {line}");
            assert!(!line.contains('\u{2014}'), "em-dash in: {line}");
        }
    }
}
