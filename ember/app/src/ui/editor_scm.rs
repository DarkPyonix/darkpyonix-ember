//! Source control in the Ember editor window (FR-L31.2, #444).
//!
//! The 커밋 and Git tool windows read and change the project's repository through the git relay
//! the 버전 관리 screen already uses (`/api/projects/{name}/git/{op}`, FR-G4, via
//! [`crate::ui::vcs::git_call`]). This module holds the state and the rules as plain functions,
//! and the island views in [`view`].
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use dioxus_compose::html::prelude::dioxus_signals::WritableExt;
use ember_client::api::{ApiError, GitCommitted, GitDiff, GitFile, GitStatus};
use serde_json::{json, Value};

use crate::ui::editor_scm_repo::{self as repo, Toplevel};
use crate::ui::vcs::{
    commit_body, commit_done_text, failure_text, git_call, parse_unified, CommitRequest, ConflictChoice,
    ConflictFile, ConflictLoad, ConflictSegment, ConflictStep, DeleteTap, FileDiff, FileStaging, Hunk,
    LineKind, RemoteAction,
};

/// Given the current file text and a `FileDiff` (from `git/diff`), reconstruct the base text prior to `diff.hunks`.
pub fn reconstruct_base_text(current_text: &str, diff: &FileDiff) -> String {
    let norm = current_text.replace("\r\n", "\n").replace('\r', "\n");
    if diff.hunks.is_empty() {
        return norm;
    }
    let cur_lines: Vec<&str> = if norm.is_empty() {
        Vec::new()
    } else {
        norm.split('\n').collect()
    };
    let mut base_lines: Vec<&str> = Vec::new();
    let mut cur_idx = 1usize;

    for hunk in &diff.hunks {
        while (if hunk.new_len > 0 { cur_idx < hunk.new_start as usize } else { cur_idx <= hunk.new_start as usize }) && cur_idx <= cur_lines.len() {
            base_lines.push(cur_lines[cur_idx - 1]);
            cur_idx += 1;
        }
        for line in &hunk.lines {
            match line.kind {
                LineKind::Context => {
                    base_lines.push(line.text.as_str());
                    cur_idx += 1;
                }
                LineKind::Del => {
                    base_lines.push(line.text.as_str());
                }
                LineKind::Add => {
                    cur_idx += 1;
                }
            }
        }
    }
    while cur_idx <= cur_lines.len() {
        base_lines.push(cur_lines[cur_idx - 1]);
        cur_idx += 1;
    }
    base_lines.join("\n")
}

/// Reconstruct the base text of `path` by reading the current file with `read_file` and applying
/// [`reconstruct_base_text`]. Returns `Err` without producing a partial or empty base if reading fails
/// or if `diff.hunks` is empty, so a failed read never overwrites the file with wrong content.
pub fn reconstruct_base_from_read<E, F>(read_file: F, diff: &FileDiff) -> Result<String, E>
where
    F: FnOnce() -> Result<String, E>,
{
    let cur_text = read_file()?;
    Ok(reconstruct_base_text(&cur_text, diff))
}

/// Compute line markers directly from a FileDiff.
pub fn markers_from_file_diff(diff: &FileDiff, new_line_count: usize) -> Vec<crate::ui::editor::DiffLineMarker> {
    use crate::ui::editor::{DiffLineKind, DiffLineMarker};
    let mut markers = Vec::new();
    for hunk in &diff.hunks {
        let mut i = 0;
        while i < hunk.lines.len() {
            match hunk.lines[i].kind {
                LineKind::Context => {
                    i += 1;
                }
                LineKind::Del | LineKind::Add => {
                    let mut del_count = 0;
                    let mut add_lines = Vec::new();
                    while i < hunk.lines.len() {
                        match hunk.lines[i].kind {
                            LineKind::Del => {
                                del_count += 1;
                                i += 1;
                            }
                            LineKind::Add => {
                                if let Some(new_no) = hunk.lines[i].new_no {
                                    add_lines.push(new_no.saturating_sub(1));
                                }
                                i += 1;
                            }
                            LineKind::Context => break,
                        }
                    }
                    if del_count > 0 && add_lines.is_empty() {
                        let del_line = if hunk.new_start == 0 {
                            0
                        } else {
                            (hunk.new_start.saturating_sub(1)) as u32
                        };
                        let del_line = del_line.min(new_line_count.saturating_sub(1) as u32);
                        markers.push(DiffLineMarker { line: del_line, kind: DiffLineKind::Deleted });
                    } else {
                        let mod_count = del_count.min(add_lines.len());
                        for (idx, &line) in add_lines.iter().enumerate() {
                            let kind = if idx < mod_count {
                                DiffLineKind::Modified
                            } else {
                                DiffLineKind::Added
                            };
                            markers.push(DiffLineMarker { line, kind });
                        }
                    }
                }
            }
        }
    }
    markers
}

/// Pairs consecutive Del and Add lines side-by-side, placing Context lines on both sides.
pub fn side_by_side_rows(hunk: &Hunk) -> Vec<(Option<(u32, LineKind, String)>, Option<(u32, LineKind, String)>)> {
    let mut rows = Vec::new();
    let mut i = 0;
    while i < hunk.lines.len() {
        if hunk.lines[i].kind == LineKind::Context {
            let l = &hunk.lines[i];
            let left = l.old_no.map(|n| (n, LineKind::Context, l.text.clone()));
            let right = l.new_no.map(|n| (n, LineKind::Context, l.text.clone()));
            rows.push((left, right));
            i += 1;
        } else {
            let mut dels = Vec::new();
            let mut adds = Vec::new();
            while i < hunk.lines.len() && hunk.lines[i].kind != LineKind::Context {
                let l = &hunk.lines[i];
                match l.kind {
                    LineKind::Del => {
                        let no = l.old_no.unwrap_or(0);
                        dels.push((no, LineKind::Del, l.text.clone()));
                    }
                    LineKind::Add => {
                        let no = l.new_no.unwrap_or(0);
                        adds.push((no, LineKind::Add, l.text.clone()));
                    }
                    LineKind::Context => unreachable!(),
                }
                i += 1;
            }
            let max_len = dels.len().max(adds.len());
            for idx in 0..max_len {
                rows.push((dels.get(idx).cloned(), adds.get(idx).cloned()));
            }
        }
    }
    rows
}

/// How one working-tree line of the editable diff stands against the base (FR-L31.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// The same text as the base line beside it.
    Same,
    /// Changed: the base line beside it is the text it replaced.
    Changed,
    /// Added: no base line beside it.
    Added,
}

/// One row of the editable diff: one working-tree line, with the base line shown beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingRow {
    pub kind: RowKind,
    /// The base line beside it (1-based number, text); `None` for an added line.
    pub base: Option<(u32, String)>,
    /// Base lines removed just above this line (1-based number, text).
    pub deleted_above: Vec<(u32, String)>,
}

/// The base laid out against the working tree, one row per working-tree line, so the read-only base
/// side lines up with the editor beside it line by line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkingAlignment {
    pub rows: Vec<WorkingRow>,
    /// Base lines removed after the last working-tree line.
    pub deleted_at_end: Vec<(u32, String)>,
}

impl WorkingAlignment {
    /// Working-tree lines that are new or changed, as Code-OSS counts "+".
    pub fn added(&self) -> usize {
        self.rows.iter().filter(|r| r.kind != RowKind::Same).count()
    }

    /// Base lines that are gone or changed, as Code-OSS counts "-".
    pub fn removed(&self) -> usize {
        self.rows.iter().map(|r| r.deleted_above.len() + usize::from(r.kind == RowKind::Changed)).sum::<usize>()
            + self.deleted_at_end.len()
    }
}

fn text_lines(text: &str) -> Vec<String> {
    let norm = text.replace("\r\n", "\n").replace('\r', "\n");
    if norm.is_empty() {
        Vec::new()
    } else {
        norm.split('\n').map(str::to_owned).collect()
    }
}

/// Lays the base out against the working text (FR-L31.10). The edit script is the one
/// [`crate::ui::editor::compute_line_diffs`] walks for the gutter (common prefix and suffix, then the
/// longest common subsequence of the middle with the same tie-break, paired by position above
/// [`crate::ui::editor::MAX_DIFF_MATRIX_CELLS`]), so the rows and the gutter marks agree. In a block of
/// changes the first removed lines pair with the first added ones as changed rows; the rest of the
/// added lines are added rows, and the rest of the removed lines sit above the next row.
pub fn align_working_rows(base: &str, working: &str) -> WorkingAlignment {
    #[derive(Clone, Copy)]
    enum Edit {
        Equal(usize, usize),
        Delete(usize),
        Insert(usize),
    }
    let old = text_lines(base);
    let new = text_lines(working);
    let mut prefix = 0;
    while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old.len() - prefix && suffix < new.len() - prefix && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix] {
        suffix += 1;
    }
    let (m, n) = (old.len() - prefix - suffix, new.len() - prefix - suffix);
    let mut edits: Vec<Edit> = (0..prefix).map(|i| Edit::Equal(i, i)).collect();
    if m.saturating_mul(n) > crate::ui::editor::MAX_DIFF_MATRIX_CELLS || m == 0 || n == 0 {
        edits.extend((0..m).map(|i| Edit::Delete(prefix + i)));
        edits.extend((0..n).map(|j| Edit::Insert(prefix + j)));
    } else {
        let (om, nm) = (&old[prefix..prefix + m], &new[prefix..prefix + n]);
        let mut dp = vec![vec![0u32; n + 1]; m + 1];
        for i in 0..m {
            for j in 0..n {
                dp[i + 1][j + 1] = if om[i] == nm[j] { dp[i][j] + 1 } else { dp[i + 1][j].max(dp[i][j + 1]) };
            }
        }
        let mut mid = Vec::new();
        let (mut i, mut j) = (m, n);
        while i > 0 || j > 0 {
            if i > 0 && j > 0 && om[i - 1] == nm[j - 1] {
                mid.push(Edit::Equal(prefix + i - 1, prefix + j - 1));
                i -= 1;
                j -= 1;
            } else if j > 0 && (i == 0 || dp[i][j - 1] >= dp[i - 1][j]) {
                mid.push(Edit::Insert(prefix + j - 1));
                j -= 1;
            } else {
                mid.push(Edit::Delete(prefix + i - 1));
                i -= 1;
            }
        }
        mid.reverse();
        edits.extend(mid);
    }
    edits.extend((0..suffix).map(|k| Edit::Equal(old.len() - suffix + k, new.len() - suffix + k)));

    let numbered = |i: usize| (i as u32 + 1, old[i].clone());
    let mut out = WorkingAlignment::default();
    let mut pending: Vec<(u32, String)> = Vec::new();
    let mut k = 0;
    while k < edits.len() {
        if let Edit::Equal(i, _) = edits[k] {
            out.rows.push(WorkingRow { kind: RowKind::Same, base: Some(numbered(i)), deleted_above: std::mem::take(&mut pending) });
            k += 1;
            continue;
        }
        let (mut dels, mut ins) = (Vec::new(), 0usize);
        while k < edits.len() {
            match edits[k] {
                Edit::Delete(i) => dels.push(i),
                Edit::Insert(_) => ins += 1,
                Edit::Equal(..) => break,
            }
            k += 1;
        }
        for idx in 0..ins {
            let (kind, base) = match dels.get(idx) {
                Some(&i) => (RowKind::Changed, Some(numbered(i))),
                None => (RowKind::Added, None),
            };
            out.rows.push(WorkingRow { kind, base, deleted_above: std::mem::take(&mut pending) });
        }
        pending.extend(dels.iter().skip(ins).map(|&i| numbered(i)));
    }
    out.deleted_at_end = pending;
    out
}

/// What the editable side of a diff shows for the text it holds now: the rows of the base side and
/// the gutter marks, both recomputed from the reconstructed base on every change (FR-L31.10).
#[derive(Debug, Clone, PartialEq)]
pub struct DiffEditorModel {
    pub alignment: WorkingAlignment,
    pub markers: Vec<crate::ui::editor::DiffLineMarker>,
}

pub fn diff_editor_model(base: &str, working: &str) -> DiffEditorModel {
    DiffEditorModel { alignment: align_working_rows(base, working), markers: crate::ui::editor::compute_line_diffs(base, working) }
}

/// What becomes of a document the diff opened, once the diff no longer shows it (FR-L31.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffDocRelease {
    /// A tab holds it: it stays as it is.
    Keep,
    /// Nothing holds it and it has no unsaved edits: the session closes it.
    Close,
    /// It has unsaved edits: a tab opens for it (the current tab stays), so they are not lost.
    OpenTab,
}

pub fn release_diff_document(has_tab: bool, dirty: bool) -> DiffDocRelease {
    match (has_tab, dirty) {
        (true, _) => DiffDocRelease::Keep,
        (false, true) => DiffDocRelease::OpenTab,
        (false, false) => DiffDocRelease::Close,
    }
}

/// Whether the diff must ask the session to open `wanted`: not when a tab or the diff already has
/// it open, nor when the diff asked once already (a failed open is not retried every frame).
pub fn diff_document_needs_open(wanted: &str, requested: Option<&str>, already_open: bool) -> bool {
    !already_open && requested != Some(wanted)
}

/// The file Cmd/Ctrl+S saves: the working tree of a diff being edited while it covers the editor area,
/// else the current tab's.
pub fn save_target(current_tab: Option<&str>, editable_diff: Option<String>) -> Option<String> {
    editable_diff.or_else(|| current_tab.map(str::to_owned))
}

/// Build a full patch representing all changes in diff to discard.
pub fn discard_patch(diff: &FileDiff) -> Option<String> {
    crate::ui::vcs::build_patch(diff, &crate::ui::vcs::FileStaging::all(diff))
}

/// Confirm discard with a 600ms to 4000ms confirmation window.
pub fn discard_tap(armed: Option<(&str, u64)>, path: &str, now_ms: u64) -> DeleteTap {
    match armed {
        Some((p, at)) if p == path => {
            let age = now_ms.saturating_sub(at);
            if age >= 4000 {
                DeleteTap::Expired
            } else if age < 600 {
                DeleteTap::Wait
            } else {
                DeleteTap::Confirm
            }
        }
        _ => DeleteTap::Arm,
    }
}

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

/// The set of absolute paths below `root` that `load` reports as modified, added, renamed, copied,
/// untracked or conflicted in the index or working tree. Returns an empty set unless `load` is
/// [`ScmLoad::Ready`]: without a real `git/status` answer no file is highlighted.Deleted-only files
/// (`D` with no surviving path) are excluded because they do not appear in the explorer or tabs.
pub fn modified_paths_set(root: &str, load: &ScmLoad) -> std::collections::HashSet<String> {
    modified_paths_in(root, "", load)
}

/// [`modified_paths_set`] for a folder `prefix` below the repository root (FR-L31.9): each
/// repository path becomes the path the editor uses for it ([`repo::editor_path`]).
pub fn modified_paths_in(root: &str, prefix: &str, load: &ScmLoad) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let ScmLoad::Ready(status) = load else { return out };
    for f in &status.files {
        let deleted_only = (f.index == 'D' && matches!(f.worktree, '.' | ' '))
            || (f.worktree == 'D' && matches!(f.index, '.' | ' '));
        if deleted_only {
            continue;
        }
        let changed = is_conflict(f)
            || !matches!(f.index, '.' | '!' | ' ')
            || !matches!(f.worktree, '.' | '!' | ' ');
        if changed {
            out.insert(repo::editor_path(root, prefix, &f.path));
        }
    }
    out
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

/// Whether the commit button may call the server, with optional amend flag.
pub fn commit_gate_with_amend(load: &ScmLoad, message: &str, amend: bool, busy: bool) -> Result<CommitRequest, &'static str> {
    if busy {
        return Err(BUSY);
    }
    let ScmLoad::Ready(status) = load else { return Err(NOT_READ_YET) };
    let g = groups(status);
    if !g.conflicts.is_empty() {
        return Err(CONFLICTS_LEFT);
    }
    let Some((summary, description)) = split_message(message) else { return Err(NEED_MESSAGE) };
    if !amend && g.staged.is_empty() {
        return Err(NOTHING_STAGED);
    }
    Ok(CommitRequest { ops: Vec::new(), summary, description, amend })
}

/// Whether the commit button may call the server, and with what; else the line it shows instead.
pub fn commit_gate(load: &ScmLoad, message: &str, busy: bool) -> Result<CommitRequest, &'static str> {
    commit_gate_with_amend(load, message, false, busy)
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
    pub staging: FileStaging,
    pub side_by_side: bool,
    /// The base text rebuilt from the diff and the working text it was read against, for the
    /// editable side (FR-L31.10); `None` until the document is open.
    pub base: Option<String>,
    /// Whether `base` was rebuilt from the latest diff answer.
    base_fresh: bool,
    seq: u64,
}

impl DiffState {
    pub fn visible(&self, current_tab: Option<&str>) -> bool {
        self.over.as_deref() == current_tab
    }

    /// Whether this diff is of the working tree, whose side is the file's own document (FR-L31.10).
    /// The staged side is the index and stays read-only, as in Code-OSS.
    pub fn of_working_tree(&self) -> bool {
        !self.staged
    }

    /// Whether the working-tree side shows as an editor: side by side, once a diff has been read.
    pub fn editable(&self) -> bool {
        self.of_working_tree() && self.side_by_side && matches!(self.view, DiffView::Ready(_))
    }

    /// The base for the editable side, given the document's text now and whether it has unsaved
    /// edits. The base is rebuilt from a new diff answer only while the document is clean, because
    /// the answer describes the file on disk; until then the last base stays.
    pub fn base_for(&mut self, doc_text: &str, dirty: bool) -> Option<String> {
        let DiffView::Ready(fd) = &self.view else { return self.base.clone() };
        if self.base.is_none() || (!self.base_fresh && !dirty) {
            self.base = Some(reconstruct_base_text(doc_text, fd));
            self.base_fresh = true;
        }
        self.base.clone()
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
    /// Diffs for the gutter, by repository path.
    pub gutter_diffs: BTreeMap<String, FileDiff>,
    pub gutter_seq: u64,
    /// The `git/toplevel` answer once one came (FR-L31.9); until then the folder is the root.
    pub toplevel: Option<Toplevel>,
    pub amend: bool,
    pub new_branch: String,
    pub armed_discard: Option<(String, u64)>,
    pub conflict: Option<(String, ConflictLoad, ConflictStep)>,
    pub side_by_side: bool,
    /// The first and last line the editable diff's editor shows, which the base side follows.
    pub diff_lines: (u32, u32),
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
            gutter_diffs: BTreeMap::new(),
            gutter_seq: 0,
            toplevel: None,
            amend: false,
            new_branch: String::new(),
            armed_discard: None,
            conflict: None,
            side_by_side: false,
            diff_lines: (0, 0),
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
    /// Reading the same diff again (after a save) keeps what it shows until the answer, so the
    /// editable side stays open meanwhile.
    pub fn begin_diff(&mut self, path: &str, staged: bool, over: Option<String>) -> u64 {
        self.diff_seq += 1;
        let again = self.diff.take().filter(|d| d.path == path && d.staged == staged);
        if again.is_none() {
            self.diff_lines = (0, 0);
        }
        let (view, staging, base) = match again {
            Some(d) => (d.view, d.staging, d.base),
            None => (DiffView::Loading, FileStaging::none(), None),
        };
        self.diff = Some(DiffState {
            path: path.to_string(),
            staged,
            over,
            view,
            staging,
            side_by_side: self.side_by_side,
            base,
            base_fresh: false,
            seq: self.diff_seq,
        });
        self.diff_seq
    }

    /// Takes a diff answer if that diff is still the one open.
    pub fn finish_diff(&mut self, seq: u64, view: DiffView) -> bool {
        match self.diff.as_mut() {
            Some(d) if d.seq == seq => {
                if let DiffView::Ready(_) = view {
                    d.staging = FileStaging::none();
                }
                d.view = view;
                d.base_fresh = false;
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
    /// The cursor line's blame in the status bar (FR-L31.8); dropped on every status read.
    pub blame: crate::ui::editor_blame::Blame,
}

impl Scm {
    pub fn new(project: &str, revision: dioxus_compose::prelude::SyncSignal<u64>) -> Scm {
        let project = project.trim().to_string();
        let blame = crate::ui::editor_blame::Blame::new(&project, revision);
        Scm { state: Arc::new(Mutex::new(ScmState::new(!project.is_empty()))), revision, project, blame }
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

    /// The absolute paths below `root` that `git/status` reported as modified, added or conflicted.
    /// Empty when no status has been read yet.
    pub fn modified_paths(&self, root: &str) -> std::collections::HashSet<String> {
        self.with(|s| {
            let prefix = s.toplevel.as_ref().map(|t| repo::folder_prefix(root, t)).unwrap_or_default();
            modified_paths_in(root, &prefix, &s.load)
        })
    }

    /// The folder's path below the repository root (FR-L31.9); empty until `git/toplevel`
    /// answered, and when the folder is the root.
    pub fn prefix(&self, folder: &str) -> String {
        self.with(|s| s.toplevel.as_ref().map(|t| repo::folder_prefix(folder, t)).unwrap_or_default())
    }

    /// The path the editor uses for a repository path of `git/status` (FR-L31.9).
    pub fn editor_path(&self, folder: &str, repo_path: &str) -> String {
        repo::editor_path(folder, &self.prefix(folder), repo_path)
    }

    /// Whether the diff covers the editor area for this current tab.
    pub fn diff_visible(&self, current_tab: Option<&str>) -> bool {
        self.with(|s| s.diff.as_ref().is_some_and(|d| d.visible(current_tab)))
    }

    /// The repository path of the diff whose working-tree side is being edited, while it covers the
    /// editor area for this current tab (FR-L31.10).
    pub fn editable_diff_path(&self, current_tab: Option<&str>) -> Option<String> {
        self.with(|s| s.diff.as_ref().filter(|d| d.visible(current_tab) && d.editable()).map(|d| d.path.clone()))
    }

    /// The repository path of an open working-tree diff, whose document the diff keeps open in the
    /// session in either view, so switching views keeps unsaved edits (FR-L31.10).
    pub fn working_diff_path(&self) -> Option<String> {
        self.with(|s| s.diff.as_ref().filter(|d| d.of_working_tree()).map(|d| d.path.clone()))
    }

    /// The base of the editable diff for the document's text now (see [`DiffState::base_for`]).
    pub fn diff_base(&self, doc_text: &str, dirty: bool) -> Option<String> {
        self.with(|s| s.diff.as_mut().filter(|d| d.editable()).and_then(|d| d.base_for(doc_text, dirty)))
    }

    /// The editable diff's editor shows lines `first` to `last`: the base side follows.
    pub fn set_diff_lines(&self, first: u32, last: u32) {
        let changed = self.with(|s| std::mem::replace(&mut s.diff_lines, (first, last)) != (first, last));
        if changed {
            Self::redraw(self.revision);
        }
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
        self.with(|s| s.gutter_diffs.clear());
        // A save, a commit or a stage can change what any line's blame is (FR-L31.8).
        self.blame.invalidate();
        let seq = self.with(ScmState::begin_load);
        // The repository root is asked once (FR-L31.9); a failed answer is asked again next time.
        let need_top = self.with(|s| s.toplevel.is_none());
        let (state, revision, project) = (self.state.clone(), self.revision, self.project.clone());
        crate::services::run_shared(
            async move {
                let top = if need_top { git_call::<Toplevel>(&project, "toplevel", json!({})).await.ok() } else { None };
                (top, git_call::<GitStatus>(&project, "status", json!({})).await)
            },
            move |(top, result)| {
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(t) = top {
                    s.toplevel = Some(t);
                }
                let taken = s.finish_load(seq, load_from(result));
                drop(s);
                if taken {
                    Self::redraw(revision);
                }
            },
        );
        let open = self.with(|s| s.diff.as_ref().map(|d| (d.path.clone(), d.staged, d.over.clone())));
        if let Some((path, staged, over)) = open {
            self.load_diff(&path, staged, over);
        }
    }

    /// Reads the diff of `rel_path` (a repository path) for the gutter, once per status.
    pub fn ensure_gutter_diff(&self, rel_path: &str) {
        let (needed, staged) = self.with(|s| {
            if s.gutter_diffs.contains_key(rel_path) {
                return (false, false);
            }
            if let ScmLoad::Ready(status) = &s.load {
                if let Some(f) = status.files.iter().find(|f| f.path == rel_path) {
                    if f.index == '?' && f.worktree == '?' {
                        return (false, false);
                    }
                    let staged = f.worktree == '.' || f.worktree == ' ';
                    return (true, staged);
                }
            }
            (false, false)
        });
        if !needed || self.project.is_empty() {
            return;
        }
        let (state, revision, project, path) = (self.state.clone(), self.revision, self.project.clone(), rel_path.to_string());
        let path_for_call = path.clone();
        crate::services::run_shared(
            async move {
                let d: Result<GitDiff, ApiError> = git_call(&project, "diff", json!({ "path": path_for_call, "staged": staged, "include_untracked": false })).await;
                d.map(|d| d.patch)
            },
            move |result| {
                if let Ok(patch) = result {
                    if let Some(fd) = parse_unified(&patch).into_iter().next() {
                        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                        s.gutter_diffs.insert(path, fd);
                        Self::redraw(revision);
                    }
                }
            },
        );
    }

    pub fn gutter_decorations(&self, folder: &str, abs_path: &str, current_text: &str, version: u32) -> compose_rust::code::GutterDecorations {
        // The status names files by repository path, wherever the folder is in it (FR-L31.9).
        let Some(rel_path) = repo::gutter_key(folder, &self.prefix(folder), abs_path) else {
            return compose_rust::code::GutterDecorations::default();
        };
        let rel_path = rel_path.as_str();
        let snapshot = self.snapshot();
        let ScmLoad::Ready(status) = &snapshot.load else {
            return compose_rust::code::GutterDecorations::default();
        };
        let Some(f) = status.files.iter().find(|f| f.path == rel_path) else {
            return compose_rust::code::GutterDecorations::default();
        };
        if f.index == '?' && f.worktree == '?' {
            let line_count = if current_text.is_empty() { 0 } else { current_text.lines().count() };
            let markers: Vec<crate::ui::editor::DiffLineMarker> = (0..line_count as u32)
                .map(|line| crate::ui::editor::DiffLineMarker { line, kind: crate::ui::editor::DiffLineKind::Added })
                .collect();
            return crate::ui::editor::diff_gutter_decorations(version, &markers);
        }
        if let Some(d) = snapshot.gutter_diffs.get(rel_path) {
            let base = reconstruct_base_text(current_text, d);
            let markers = crate::ui::editor::compute_line_diffs(&base, current_text);
            crate::ui::editor::diff_gutter_decorations(version, &markers)
        } else {
            self.ensure_gutter_diff(rel_path);
            compose_rust::code::GutterDecorations::default()
        }
    }

    pub fn toggle_side_by_side(&self) {
        self.with(|s| {
            s.side_by_side = !s.side_by_side;
            if let Some(d) = &mut s.diff {
                d.side_by_side = s.side_by_side;
            }
        });
        Self::redraw(self.revision);
    }

    pub fn toggle_diff_line(&self, hunk: usize, line: usize) {
        self.with(|s| {
            if let Some(d) = &mut s.diff {
                if let DiffView::Ready(fd) = &d.view {
                    d.staging.toggle_line(fd, hunk, line);
                }
            }
        });
        Self::redraw(self.revision);
    }

    pub fn toggle_diff_hunk(&self, hunk: usize) {
        self.with(|s| {
            if let Some(d) = &mut s.diff {
                if let DiffView::Ready(fd) = &d.view {
                    d.staging.toggle_hunk(fd, hunk);
                }
            }
        });
        Self::redraw(self.revision);
    }

    pub fn stage_diff_selection(&self) {
        let (patch, staged) = self.with(|s| {
            if let Some(d) = &s.diff {
                if let DiffView::Ready(fd) = &d.view {
                    (crate::ui::vcs::build_patch(fd, &d.staging), d.staged)
                } else {
                    (None, false)
                }
            } else {
                (None, false)
            }
        });
        if let Some(patch) = patch {
            self.apply_patch(patch, staged);
        }
    }

    pub fn apply_patch(&self, patch: String, reverse: bool) {
        let what = if reverse { "스테이지 해제" } else { "스테이지" };
        self.change(what, "apply", json!({ "patch": patch, "reverse": reverse }), None);
    }

    pub fn toggle_amend(&self) {
        self.with(|s| s.amend = !s.amend);
        Self::redraw(self.revision);
    }

    pub fn set_new_branch(&self, text: String) {
        self.with(|s| s.new_branch = text);
        Self::redraw(self.revision);
    }

    pub fn checkout(&self, branch: &str) {
        let branch = branch.trim().to_string();
        if branch.is_empty() {
            return;
        }
        self.change("브랜치 전환", "checkout", json!({ "branch": branch }), Some(format!("{branch}(으)로 전환했습니다.")));
    }

    pub fn create_branch(&self, name: &str) {
        let name = name.trim().to_string();
        if !crate::ui::vcs::valid_branch_name(&name) {
            crate::ui::cr::notice::post("올바른 브랜치 이름을 입력하세요.");
            return;
        }
        self.change("새 브랜치", "branch", json!({ "name": name }), Some(format!("브랜치 {name}을 만들었습니다.")));
    }

    pub fn remote(&self, action: RemoteAction) {
        if self.project.is_empty() {
            return;
        }
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
        let (this, project, state, revision) = (self.clone(), self.project.clone(), self.state.clone(), self.revision);
        crate::services::run_shared(
            async move { crate::ui::vcs::run_remote(&project, action).await },
            move |result| {
                state.lock().unwrap_or_else(|e| e.into_inner()).busy = false;
                match result {
                    Ok(output) => {
                        crate::ui::cr::notice::post(crate::ui::vcs::remote_done_text(action, &output));
                    }
                    Err(e) => crate::ui::cr::notice::post(failure_text(&action.label(), &e)),
                }
                Self::redraw(revision);
                this.refresh();
            },
        );
    }

    pub fn conflict_visible(&self) -> bool {
        self.with(|s| s.conflict.is_some())
    }

    pub fn open_conflict(&self, path: &str) {
        if self.project.is_empty() {
            return;
        }
        let path_str = path.to_string();
        self.with(|s| {
            s.diff = None;
            s.conflict = Some((path_str.clone(), ConflictLoad::Loading, ConflictStep::Editing));
        });
        Self::redraw(self.revision);
        let (state, revision, project, path_owned) = (self.state.clone(), self.revision, self.project.clone(), path_str);
        let path_for_call = path_owned.clone();
        crate::services::run_shared(
            async move { crate::ui::vcs::fetch_conflict(&project, &path_for_call).await },
            move |load| {
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some((p, _, step)) = &mut s.conflict {
                    if *p == path_owned {
                        *step = ConflictStep::Editing;
                        s.conflict = Some((path_owned, load, ConflictStep::Editing));
                    }
                }
                Self::redraw(revision);
            },
        );
    }

    pub fn set_conflict_choice(&self, block: usize, choice: ConflictChoice) {
        self.with(|s| {
            if let Some((path, ConflictLoad::Ready(file), step)) = &s.conflict {
                let updated = file.with_choice(block, choice);
                s.conflict = Some((path.clone(), ConflictLoad::Ready(updated), *step));
            }
        });
        Self::redraw(self.revision);
    }

    pub fn pick_all_conflicts(&self, choice: ConflictChoice) {
        self.with(|s| {
            if let Some((path, ConflictLoad::Ready(file), step)) = &s.conflict {
                let updated = file.pick_all(choice);
                s.conflict = Some((path.clone(), ConflictLoad::Ready(updated), *step));
            }
        });
        Self::redraw(self.revision);
    }

    pub fn set_conflict_step(&self, new_step: ConflictStep) {
        self.with(|s| {
            if let Some((_, _, step)) = &mut s.conflict {
                *step = new_step;
            }
        });
        Self::redraw(self.revision);
    }

    pub fn close_conflict(&self) {
        let had = self.with(|s| s.conflict.take().is_some());
        if had {
            Self::redraw(self.revision);
        }
    }

    pub fn resolve_conflict(&self, path: &str, content: &str) {
        if self.project.is_empty() {
            return;
        }
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
        let (this, project, state, revision, content_owned, path_owned) =
            (self.clone(), self.project.clone(), self.state.clone(), self.revision, content.to_string(), path.to_string());
        crate::services::run_shared(
            async move { crate::ui::vcs::run_resolve(&project, &path_owned, &content_owned).await },
            move |result| {
                state.lock().unwrap_or_else(|e| e.into_inner()).busy = false;
                match result {
                    Ok(_) => {
                        crate::ui::cr::notice::post("충돌을 해결하고 스테이지했습니다.");
                        state.lock().unwrap_or_else(|e| e.into_inner()).conflict = None;
                    }
                    Err(e) => crate::ui::cr::notice::post(e),
                }
                Self::redraw(revision);
                this.refresh();
            },
        );
    }

    pub fn discard_file(&self, path: &str, abs_path: &str, host: &crate::editor_host::EditorHost) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let tap = self.with(|s| {
            let armed = s.armed_discard.as_ref().map(|(p, t)| (p.as_str(), *t));
            let tap = discard_tap(armed, path, now_ms);
            match tap {
                DeleteTap::Arm => {
                    s.armed_discard = Some((path.to_string(), now_ms));
                }
                DeleteTap::Wait => {}
                DeleteTap::Confirm => {
                    s.armed_discard = None;
                }
                DeleteTap::Expired => {
                    s.armed_discard = Some((path.to_string(), now_ms));
                }
            }
            tap
        });
        Self::redraw(self.revision);
        if tap != DeleteTap::Confirm {
            return;
        }

        let (this, project, path_str, abs_str, host_clone) =
            (self.clone(), self.project.clone(), path.to_string(), abs_path.to_string(), host.clone());
        crate::services::run_shared(
            async move {
                let d: Result<GitDiff, ApiError> = git_call(&project, "diff", json!({ "path": path_str, "staged": false, "include_untracked": false })).await;
                d.map(|d| d.patch)
            },
            move |result| {
                match result {
                    Ok(patch) => match parse_unified(&patch).into_iter().next() {
                        Some(fd) if !fd.hunks.is_empty() => {
                            host_clone.send(crate::editor_host::HostCommand::DiscardFile { path: abs_str, diff: fd });
                            this.refresh();
                        }
                        _ => {
                            crate::ui::cr::notice::post("되돌릴 변경 사항을 찾지 못했습니다.");
                        }
                    },
                    Err(e) => {
                        crate::ui::cr::notice::post(failure_text("변경 사항 취소", &e));
                    }
                }
            },
        );
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
        let gate = self.with(|s| commit_gate_with_amend(&s.load, &s.message, s.amend, s.busy).map(|req| {
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
                    if result.is_ok() {
                        s.amend = false;
                        if split_message(&s.message).map(|(a, b)| format!("{a}\n{b}")) == Some(submitted) {
                            // Only the text that was committed is cleared; a box edited meanwhile keeps the edit.
                            s.message.clear();
                        }
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

/// The notice when 파일 열기 could not open a change (FR-L31.9, #781): the file and the reason.
pub fn open_failed_text(name: &str, reason: &str) -> String {
    format!("{name}을(를) 열지 못했습니다: {reason}")
}

/// The notice when 파일 열기 is clicked while the connection is lost: the host drops the open then,
/// so the window says so instead of nothing happening (FR-L31.9, #781).
pub fn open_offline_text(name: &str) -> String {
    format!("서버와의 연결이 끊어져 {name}을(를) 열 수 없습니다. 다시 연결한 뒤 여세요")
}

/// The island tool windows and the diff view.
pub mod view {
    use dioxus_compose::html::prelude::dioxus_elements::point_interaction::ModifiersInteraction;
    use dioxus_compose::html::prelude::dioxus_elements::{Key, Modifiers};
    use dioxus_compose::html::prelude::dioxus_signals::{ReadableExt, WritableExt};
    use dioxus_compose::html::prelude::*;

    use super::{
        branch_name, groups, letter_color, letter_label, side_by_side_rows, upstream_text, Entry, Group,
        DIFF_LINE_CAP, DiffView,
    };
    use crate::editor_host::HostCommand;
    use crate::ui::editor::{base_name, Side, ToolWindow};
    use crate::ui::editor_workbench::page::{icon, icon_px};
    use crate::ui::editor_workbench::Shared;
    use crate::ui::vcs::{ConflictChoice, ConflictLoad, ConflictStep, LineKind, RemoteAction};

    /// The folder part of a repository path, for the muted text after the name.
    fn dir_of(path: &str) -> &str {
        path.rfind('/').map(|i| &path[..i]).unwrap_or("")
    }

    thread_local! {
        /// The paths 파일 열기 asked the session to open and that have not opened or failed yet, so
        /// a failure among them is said in a notice (FR-L31.9, #781).
        static OPEN_ASKED: std::cell::RefCell<std::collections::HashSet<String>> = std::cell::RefCell::new(std::collections::HashSet::new());
    }

    /// Opens the file of a repository path in a tab: below the folder as the explorer opens it,
    /// outside it by its absolute path (FR-L31.9). While the connection is lost the host would drop
    /// the open, so a notice says so instead (#781).
    fn open_file(s: &Shared, rel: &str) {
        let path = s.scm.editor_path(&s.folder, rel);
        if s.host.state.lock().unwrap_or_else(|e| e.into_inner()).lost.is_some() {
            crate::ui::cr::notice::post(super::open_offline_text(&crate::ui::editor::display_name(base_name(&path))));
            return;
        }
        s.scm.close_diff();
        s.open_errors.borrow_mut().remove(&path);
        let mut tabs = s.tabs;
        tabs.write().open(&path);
        // A document the diff already opened is not opened again: a second open would clear its
        // unsaved mark (FR-L31.10).
        if !s.uris.borrow().contains_key(&path) {
            OPEN_ASKED.with(|asked| asked.borrow_mut().insert(path.clone()));
            s.host.send(HostCommand::Open(path.clone()));
        }
        s.host.send(HostCommand::Focus(path));
    }

    /// The session opened `path`: a later failure of another open of it is not 파일 열기's.
    pub fn open_settled(path: &str) {
        OPEN_ASKED.with(|asked| asked.borrow_mut().remove(path));
    }

    /// The session could not open `path`: when 파일 열기 asked for it, a notice names the file and
    /// the reason (FR-L31.9, #781); the editor area shows the reason as for any other open.
    pub fn open_failed(path: &str, reason: &str) {
        if OPEN_ASKED.with(|asked| asked.borrow_mut().remove(path)) {
            crate::ui::cr::notice::post(super::open_failed_text(&crate::ui::editor::display_name(base_name(path)), reason));
        }
    }

    thread_local! {
        /// The working-tree document this window's diff asked the session to open (FR-L31.10).
        static DIFF_DOC: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    }

    /// Keeps the document of an open working-tree diff open in the session, and lets it go when the
    /// diff closes or moves to another file: closed when nothing else holds it, kept when a tab does,
    /// and given a tab (the current tab stays) when it has unsaved edits (FR-L31.10). Called once a
    /// frame by the page, after the session's updates are applied.
    pub fn sync_diff_document(s: &Shared) {
        let wanted = s.scm.working_diff_path().map(|rel| s.scm.editor_path(&s.folder, &rel));
        let requested = DIFF_DOC.with(|d| d.borrow().clone());
        if let Some(owned) = requested.as_deref().filter(|p| wanted.as_deref() != Some(*p)) {
            let has_tab = s.tabs.read().paths().contains(&owned);
            let uri = s.uris.borrow().get(owned).cloned();
            let dirty = uri.as_ref().and_then(|u| s.bridge.borrow().document(u).map(|d| d.dirty)).unwrap_or(false);
            match super::release_diff_document(has_tab, dirty) {
                super::DiffDocRelease::Keep => {}
                super::DiffDocRelease::Close => {
                    if uri.is_some() {
                        s.host.send(HostCommand::Close(owned.to_string()));
                    }
                }
                super::DiffDocRelease::OpenTab => {
                    let mut tabs = s.tabs;
                    let current = tabs.read().current_path().map(str::to_owned);
                    tabs.write().open(owned);
                    if let Some(current) = current {
                        tabs.write().activate(&current);
                    }
                }
            }
            DIFF_DOC.with(|d| *d.borrow_mut() = None);
        }
        let requested = DIFF_DOC.with(|d| d.borrow().clone());
        if let Some(wanted) = wanted {
            let already_open = s.uris.borrow().contains_key(&wanted) || s.tabs.read().paths().contains(&wanted.as_str());
            if super::diff_document_needs_open(&wanted, requested.as_deref(), already_open) {
                s.open_errors.borrow_mut().remove(&wanted);
                s.host.send(HostCommand::Open(wanted.clone()));
                DIFF_DOC.with(|d| *d.borrow_mut() = Some(wanted));
            }
        }
    }

    /// The hover text of a base row with lines removed above it: how many, then each line.
    pub fn deleted_lines_title(lines: &[(u32, String)]) -> String {
        if lines.is_empty() {
            return String::new();
        }
        let mut out = format!("삭제된 줄 {}개", lines.len());
        for (no, text) in lines.iter().take(20) {
            out.push_str(&format!("\n{no}: {text}"));
        }
        if lines.len() > 20 {
            out.push_str(&format!("\n외 {}줄", lines.len() - 20));
        }
        out
    }

    /// Rows of the base side drawn from the editor's first visible line, enough to fill the pane.
    // v0.0.7: #544 fixed (`CodeMetrics`, `CodeScrolled` and `Batch::set_scroll_offset`);
    // workaround kept until checked on a display (dioxus-compose's `CodeEditor` has to expose the
    // `on_code_metrics` and `on_code_scrolled` handlers first). Until then the base side follows the
    // editor's first visible line and draws 20 px rows on the assumption that this is the line height.
    const BASE_ROWS_SHOWN: usize = 400;

    /// The editable diff (FR-L31.10): the base read-only on the left, one row per working-tree line
    /// so it lines up with the editor, and the file's document in an editor on the right.
    fn editable_body(s: &Shared, abs: &str, model: Option<super::DiffEditorModel>, first_line: u32) -> Element {
        let Some(model) = model else {
            let text = match s.open_errors.borrow().get(abs) {
                Some(error) => format!("파일을 열 수 없습니다: {error}"),
                None => "파일을 여는 중입니다.".to_string(),
            };
            return rsx! { div { class: "ed-empty", role: "status", p { "{text}" } } };
        };
        let rows = model.alignment.rows;
        let start = (first_line as usize).min(rows.len());
        let end = (start + BASE_ROWS_SHOWN).min(rows.len());
        let at_end = end == rows.len();
        let tail_title = deleted_lines_title(&model.alignment.deleted_at_end);
        let shown: Vec<(usize, super::WorkingRow)> = rows.into_iter().enumerate().skip(start).take(end - start).collect();
        rsx! {
            div { style: "flex:1; min-height:0; min-width:0; display:flex;",
                div { style: "flex:1; min-width:0; overflow:hidden; font-family:var(--mono); font-size:var(--fs-sm); border-right:1px solid var(--line);", role: "region", "aria-label": "기준 (읽기 전용)",
                    for (ri, row) in shown {
                        {
                            let (bg, sign) = match row.kind {
                                super::RowKind::Same => ("", " "),
                                super::RowKind::Changed => ("background:var(--c-red-soft);", "-"),
                                super::RowKind::Added => ("background:var(--surface-2);", " "),
                            };
                            let (no, text) = row.base.map(|(n, t)| (n.to_string(), t)).unwrap_or_default();
                            // TODO(compose-rust#543): removed lines are a red rule and a hover list here,
                            // because the editor cannot hold blank rows opposite them; give them rows
                            // of their own once the toolkit has view zones.
                            let mark = if row.deleted_above.is_empty() { "" } else { "box-shadow:inset 0 2px 0 var(--c-red);" };
                            let title = deleted_lines_title(&row.deleted_above);
                            rsx! {
                                div { key: "base-{ri}", title: "{title}", style: "display:flex; white-space:pre; height:20px; line-height:20px; {bg} {mark}",
                                    span { style: "width:40px; flex:none; text-align:right; padding-right:6px; color:var(--muted);", "{no}" }
                                    span { style: "width:14px; flex:none; text-align:center;", "{sign}" }
                                    span { style: "flex:1; overflow:hidden; text-overflow:ellipsis;", "{text}" }
                                }
                            }
                        }
                    }
                    // TODO(compose-rust#543): the lines removed at the end, as a rule until view zones.
                    if at_end && !tail_title.is_empty() {
                        div { title: "{tail_title}", style: "height:4px; box-shadow:inset 0 2px 0 var(--c-red);" }
                    }
                }
                WidgetView { app: super::diff_editor_view::diff_editor, style: "flex:1; min-width:0; height:100%;" }
            }
        }
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
        let refresh_label = if refreshing { "읽는 중" } else { "새로 고침" };
        rsx! {
            div { class: "ed-twh",
                b { class: "ed-twt", "{title}" }
                div { class: "spacer" }
                button { class: "ed-ib", title: "{refresh_label}", "aria-label": "{refresh_label}", disabled: refreshing, onclick: move |_| refresh_s.scm.refresh(), {icon("refresh")} }
                button { class: "ed-ib", title: "숨기기", "aria-label": "숨기기", onclick: move |_| stripes.write().hide(Side::Left), {icon("close")} }
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
        let (input_s, key_s, click_s, amend_s) = (s.clone(), s.clone(), s.clone(), s.clone());
        let commit_btn_text = if busy {
            "작업하는 중…"
        } else if st.amend {
            "이전 커밋 수정"
        } else {
            "커밋"
        };
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
                    div { style: "display:flex; align-items:center; gap:6px; font-size:var(--fs-xs); color:var(--text-2); cursor:pointer;",
                        onclick: move |_| amend_s.scm.toggle_amend(),
                        input {
                            r#type: "checkbox",
                            checked: st.amend,
                            "aria-label": "이전 커밋 수정",
                        }
                        span { "이전 커밋 수정 (amend)" }
                    }
                    button { class: "ed-pj", title: "{commit_title}", "aria-label": "{commit_title}", disabled: busy,
                        style: "justify-content:center; background:var(--ink); color:var(--ink-text); height:30px;",
                        onclick: move |_| click_s.scm.commit(),
                        {icon_px("check", 14)} "{commit_btn_text}"
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
                        button { class: "ed-ib", style: "width:22px; height:22px;", title: "{all_title}", "aria-label": "{all_title}", disabled: busy,
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
        let (diff_s, key_diff_s, open_s, op_s, discard_s) = (s.clone(), s.clone(), s.clone(), s.clone(), s.clone());
        let (diff_path, key_diff_path, open_path, discard_path) = (e.path.clone(), e.path.clone(), e.path.clone(), e.path.clone());
        let op_entry = vec![e.clone()];
        let (op_title, op_icon) = match group {
            Group::Staged => ("스테이지 해제", "minus"),
            _ => ("스테이지", "plus"),
        };
        let tip = match &e.old_path {
            Some(old) => format!("{old} \u{2192} {}, {label}", e.path),
            None => format!("{}, {label}", e.path),
        };
        let st = s.scm.snapshot();
        let armed = st.armed_discard.as_ref().map(|(p, t)| (p.as_str(), *t));
        let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        let is_armed = armed.is_some_and(|(p, at)| p == e.path && (now_ms.saturating_sub(at)) >= 600 && (now_ms.saturating_sub(at)) < 4000);
        let discard_title = if is_armed { "정말 변경 사항을 취소합니다" } else { "변경 사항 취소" };

        // The diff opens from the row's file icon, name and folder, and from its status letter: two
        // click targets beside the action buttons, never around them. When the row itself took the
        // click, the 파일 열기 icon was a click target inside another one, and in the window a click
        // on it opened nothing (#781). Side by side, each point of the row has one target under it,
        // whatever the renderer does with nested ones; Enter and Space on the row still open the diff.
        let open_row_diff = move |s: &Shared, path: &str| {
            if group == Group::Conflicts {
                s.scm.open_conflict(path);
            } else {
                let over = s.tabs.read().current_path().map(str::to_owned);
                s.scm.open_diff(path, group.staged(), over);
            }
        };
        let (letter_s, letter_path) = (s.clone(), e.path.clone());

        rsx! {
            div { key: "{group.title()}:{e.path}", class, role: "listitem", "aria-label": "{row_label}", title: "{tip}", tabindex: "0", style: "--d:1; gap:3px; padding-right:var(--sp2);",
                onkeydown: move |event| {
                    if matches!(event.key(), Key::Enter) || matches!(event.key(), Key::Character(ref c) if c == " ") {
                        event.stop_propagation();
                        event.prevent_default();
                        open_row_diff(&key_diff_s, &key_diff_path);
                    }
                },
                div { style: "display:flex; align-items:center; gap:3px; flex:1; min-width:0; align-self:stretch;",
                    onclick: move |_| open_row_diff(&diff_s, &diff_path),
                    {icon_px("file", 15)}
                    span { style: "overflow:hidden; text-overflow:ellipsis; flex:0 1 auto; min-width:20px;", "{name}" }
                    if !dir.is_empty() { span { style: "color:var(--muted); font-size:var(--fs-2xs); overflow:hidden; text-overflow:ellipsis; flex:1 2 0px; min-width:0;", "{dir}" } }
                }
                if !deleted {
                    button { class: "ed-ib", style: "width:20px; height:20px; flex:none;", title: "파일 열기: {name}", "aria-label": "파일 열기: {name}",
                        onclick: move |event| { event.stop_propagation(); open_file(&open_s, &open_path); }, {icon_px("file", 13)} }
                }
                if group == Group::Changes && e.letter != 'U' {
                    button { class: "ed-ib", style: if is_armed { "width:20px; height:20px; flex:none; color:var(--c-red);" } else { "width:20px; height:20px; flex:none;" },
                        title: "{discard_title}: {name}", "aria-label": "{discard_title}: {name}", disabled: busy,
                        onclick: move |event| {
                            event.stop_propagation();
                            let abs = discard_s.scm.editor_path(&discard_s.folder, &discard_path);
                            discard_s.scm.discard_file(&discard_path, &abs, &discard_s.host);
                        }, {icon_px("close", 13)} }
                }
                if group != Group::Conflicts {
                    button { class: "ed-ib", style: "width:20px; height:20px; flex:none;", title: "{op_title}: {name}", "aria-label": "{op_title}: {name}", disabled: busy,
                        onclick: move |event| {
                            event.stop_propagation();
                            if group == Group::Staged { op_s.scm.unstage(&op_entry) } else { op_s.scm.stage(&op_entry) }
                        }, {icon_px(op_icon, 13)} }
                }
                span { style: "width:14px; flex:none; text-align:center; font-family:var(--mono); font-size:var(--fs-xs); font-weight:var(--fw-semibold); color:{color}; align-self:stretch; display:flex; align-items:center; justify-content:center;", title: "{label}",
                    onclick: move |_| open_row_diff(&letter_s, &letter_path), "{e.letter}" }
            }
        }
    }

    /// The Git window: the branch, its upstream and the local branches from the same status.
    pub fn git_window(s: &Shared) -> Element {
        let st = s.scm.snapshot();
        let status = st.load.status().cloned();
        let busy = st.busy;
        let new_branch = st.new_branch.clone();
        let new_branch_for_create = new_branch.clone();
        let (fetch_s, pull_s, push_s, new_branch_input_s, s_create) = (s.clone(), s.clone(), s.clone(), s.clone(), s.clone());

        rsx! {
            section { class: "ed-tw ed-left", "aria-label": "Git",
                {header(s, "Git")}
                div { class: "ed-tree", role: "list", "aria-label": "브랜치",
                    if let Some(text) = st.load.note() { {note(&text)} }
                    if let Some(status) = status {
                        {
                            let pull_label = if status.behind > 0 { format!("Pull {}", status.behind) } else { "Pull".to_string() };
                            let push_label = if status.ahead > 0 { format!("Push {}", status.ahead) } else { "Push".to_string() };
                            rsx! {
                                div { class: "ed-tn", style: "--d:0; cursor:default; font-weight:var(--fw-semibold);", {icon_px("git", 15)} "{branch_name(&status)}" }
                                {note(&upstream_text(&status))}
                                div { style: "display:flex; gap:var(--sp1); padding:var(--sp1) var(--sp3);",
                                    button { class: "ed-pj", style: "flex:1; justify-content:center; border:1px solid var(--line); height:26px; font-size:var(--fs-xs);",
                                        disabled: busy, onclick: move |_| fetch_s.scm.remote(crate::ui::vcs::RemoteAction::Fetch), "Fetch" }
                                    button { class: "ed-pj", style: "flex:1; justify-content:center; border:1px solid var(--line); height:26px; font-size:var(--fs-xs);",
                                        disabled: busy, onclick: move |_| pull_s.scm.remote(crate::ui::vcs::RemoteAction::Pull(status.behind)),
                                        "{pull_label}" }
                                    button { class: "ed-pj", style: "flex:1; justify-content:center; border:1px solid var(--line); height:26px; font-size:var(--fs-xs);",
                                        disabled: busy, onclick: move |_| push_s.scm.remote(crate::ui::vcs::RemoteAction::Push(status.ahead)),
                                        "{push_label}" }
                                }
                                div { style: "display:flex; gap:var(--sp1); padding:var(--sp1) var(--sp3);",
                                    input {
                                        style: "flex:1 1 0px; min-width:0; border:1px solid var(--line); border-radius:var(--r-sm); background:var(--surface); color:var(--text); padding:2px 6px; font:inherit; font-size:var(--fs-xs);",
                                        "aria-label": "새 브랜치 이름", placeholder: "새 브랜치 이름", value: "{new_branch}",
                                        oninput: move |e| new_branch_input_s.scm.set_new_branch(e.value()),
                                    }
                                    button { class: "ed-pj", style: "flex:none; border:1px solid var(--line); height:26px; font-size:var(--fs-xs);", "aria-label": "새 브랜치", disabled: busy,
                                        onclick: move |_| {
                                            if !new_branch_for_create.trim().is_empty() {
                                                s_create.scm.create_branch(&new_branch_for_create);
                                                s_create.scm.set_new_branch(String::new());
                                            }
                                        }, "새 브랜치" }
                                }
                            }
                        }
                        div { class: "ed-tn", style: "--d:0; cursor:default; color:var(--text-2); font-size:var(--fs-xs); font-weight:var(--fw-semibold);", "로컬 브랜치"
                            span { style: "margin-left:var(--sp1); padding:0 6px; border-radius:var(--pill); background:var(--surface-2); color:var(--text-2);", "{status.branches.len()}" } }
                        if status.branches.is_empty() { {note("로컬 브랜치가 없습니다.")} }
                        for b in status.branches.iter() {
                            {
                                let current = *b == status.branch;
                                let label = if current { format!("{b}, 현재 브랜치") } else { b.clone() };
                                let b_str = b.clone();
                                let b_key_str = b.clone();
                                let branch_click_s = s.clone();
                                let branch_key_s = s.clone();
                                rsx! { div { key: "{b}", class: if current { "ed-tn sel" } else { "ed-tn" }, role: "listitem",
                                    "aria-label": "{label}",
                                    tabindex: "0",
                                    title: if current { format!("{b}") } else { format!("{b} (클릭하여 전환)") },
                                    style: if current { "--d:1; cursor:default;" } else { "--d:1; cursor:pointer;" },
                                    onclick: move |_| { if !current { branch_click_s.scm.checkout(&b_str); } },
                                    onkeydown: move |event| {
                                        if !current && (matches!(event.key(), Key::Enter) || matches!(event.key(), Key::Character(ref c) if c == " ")) {
                                            event.stop_propagation();
                                            event.prevent_default();
                                            branch_key_s.scm.checkout(&b_key_str);
                                        }
                                    },
                                    if current { {icon_px("check", 13)} } else { span { class: "chev-sp" } }
                                    "{b}"
                                } }
                            }
                        }
                        div { style: "padding:var(--sp2) var(--sp3); color:var(--muted); font-size:var(--fs-2xs); white-space:normal; line-height:1.6;", "브랜치 전환과 원격 작업은 위 버튼으로 실행할 수 있습니다." }
                    }
                }
            }
        }
    }

    /// The diff of one file in the editor area: a header with 파일 열기 and 닫기, then the hunks
    /// with old and new line numbers.
    pub fn diff_pane(s: &Shared) -> Element {
        let snapshot = s.scm.snapshot();
        let first_line = snapshot.diff_lines.0;
        let Some(d) = snapshot.diff else { return rsx! {} };
        let title = d.title();
        let (open_s, close_s, side_s, stage_sel_s) = (s.clone(), s.clone(), s.clone(), s.clone());
        let open_path = d.path.clone();
        // The editable side: the file's document, as the session holds it now (FR-L31.10).
        let abs = s.scm.editor_path(&s.folder, &d.path);
        let doc = if d.editable() {
            let uri = s.uris.borrow().get(&abs).cloned();
            uri.and_then(|u| s.bridge.borrow().document(&u).cloned())
        } else {
            None
        };
        let dirty = doc.as_ref().is_some_and(|doc| doc.dirty);
        let model = doc.as_ref().and_then(|doc| s.scm.diff_base(&doc.text, doc.dirty).map(|base| super::diff_editor_model(&base, &doc.text)));
        let summary = match (&model, &d.view) {
            (Some(m), _) => format!("+{} \u{2212}{}", m.alignment.added(), m.alignment.removed()),
            (None, DiffView::Ready(fd)) => format!("+{} \u{2212}{}", fd.added(), fd.removed()),
            _ => String::new(),
        };
        let (staged_cnt, _) = match &d.view {
            DiffView::Ready(fd) => d.staging.counts(fd),
            _ => (0, 0),
        };
        let stage_lines_label = if d.staged { "선택 줄 스테이지 해제" } else { "선택 줄 스테이지" };

        rsx! {
            div { style: "flex:1; min-width:0; min-height:0; display:flex; flex-direction:column;", role: "region", "aria-label": "변경 내용: {title}",
                div { class: "ed-tsub",
                    {icon_px("file", 14)}
                    b { "{title}" }
                    if dirty { span { class: "ed-dirty", title: "저장하지 않은 변경 사항이 있습니다", "aria-label": "저장 안 됨" } }
                    span { class: "ed-sbi", "{summary}" }
                    div { class: "spacer" }
                    button { class: "ed-pj", style: "border:1px solid var(--line);", onclick: move |_| side_s.scm.toggle_side_by_side(),
                        if d.side_by_side { "통합 보기" } else { "나란히 보기" } }
                    if staged_cnt > 0 {
                        button { class: "ed-pj", style: "border:1px solid var(--line); background:var(--surface-2);",
                            onclick: move |_| stage_sel_s.scm.stage_diff_selection(),
                            "{stage_lines_label} ({staged_cnt})" }
                    }
                    button { class: "ed-pj", style: "border:1px solid var(--line);", onclick: move |_| open_file(&open_s, &open_path), "파일 열기" }
                    button { class: "ed-ib", title: "차이 닫기", "aria-label": "차이 닫기", onclick: move |_| close_s.scm.close_diff(), {icon("close")} }
                }
                if d.editable() {
                    {editable_body(s, &abs, model, first_line)}
                } else if let Some(text) = d.view.note() {
                    div { class: "ed-empty", role: "status", p { "{text}" } }
                } else if let DiffView::Ready(fd) = &d.view {
                    div { style: "flex:1; min-height:0; overflow:auto; font-family:var(--mono); font-size:var(--fs-sm); padding:var(--sp2) 0;",
                        for (hi, h) in fd.hunks.iter().enumerate() {
                            {
                                let hunk_action_title = if d.staged { "이 덩어리 스테이지 해제" } else { "이 덩어리 스테이지" };
                                let hunk_s = s.clone();
                                rsx! {
                                    div { key: "hunk-{hi}", style: "display:flex; align-items:center; background:var(--surface-2); color:var(--muted); font-size:var(--fs-xs); padding:2px 8px; margin:4px 0;",
                                        span { style: "flex:1;", "{h.header}" }
                                        button { class: "ed-pj", style: "border:1px solid var(--line); height:20px; font-size:var(--fs-2xs); padding:0 6px;",
                                            onclick: move |_| hunk_s.scm.toggle_diff_hunk(hi),
                                            "{hunk_action_title}" }
                                    }
                                    if d.side_by_side {
                                        for (ri, (left, right)) in side_by_side_rows(h).into_iter().enumerate() {
                                            {
                                                let (l_bg, l_sign, l_no, l_text) = match left {
                                                    None => ("", " ", String::new(), String::new()),
                                                    Some((no, LineKind::Del, text)) => ("background:var(--c-red-soft);", "-", no.to_string(), text),
                                                    Some((no, _, text)) => ("", " ", no.to_string(), text),
                                                };
                                                let (r_bg, r_sign, r_no, r_text) = match right {
                                                    None => ("", " ", String::new(), String::new()),
                                                    Some((no, LineKind::Add, text)) => ("background:var(--c-green-soft);", "+", no.to_string(), text),
                                                    Some((no, _, text)) => ("", " ", no.to_string(), text),
                                                };
                                                rsx! {
                                                    div { key: "sbs-{hi}-{ri}", style: "display:flex; white-space:pre; min-height:20px; line-height:20px; border-bottom:1px solid var(--surface-2);",
                                                        div { style: "flex:1; display:flex; min-width:0; overflow:hidden; border-right:1px solid var(--line); {l_bg}",
                                                            span { style: "width:40px; flex:none; text-align:right; padding-right:6px; color:var(--muted);", "{l_no}" }
                                                            span { style: "width:14px; flex:none; text-align:center;", "{l_sign}" }
                                                            span { style: "flex:1; overflow:hidden; text-overflow:ellipsis;", "{l_text}" }
                                                        }
                                                        div { style: "flex:1; display:flex; min-width:0; overflow:hidden; {r_bg}",
                                                            span { style: "width:40px; flex:none; text-align:right; padding-right:6px; color:var(--muted);", "{r_no}" }
                                                            span { style: "width:14px; flex:none; text-align:center;", "{r_sign}" }
                                                            span { style: "flex:1; overflow:hidden; text-overflow:ellipsis;", "{r_text}" }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    } else {
                                        for (li, l) in h.lines.iter().enumerate() {
                                            {
                                                let is_staged = d.staging.is_staged(hi, li);
                                                let (bg, sign, fg) = match l.kind {
                                                    LineKind::Add => ("background:var(--c-green-soft);", "+", ""),
                                                    LineKind::Del => ("background:var(--c-red-soft);", "-", ""),
                                                    LineKind::Context => ("", " ", ""),
                                                };
                                                let old = l.old_no.map(|n| n.to_string()).unwrap_or_default();
                                                let new = l.new_no.map(|n| n.to_string()).unwrap_or_default();
                                                let is_change = l.is_change();
                                                let cursor = if is_change { "cursor:pointer;" } else { "" };
                                                let border = if is_staged { "outline:1px solid var(--ink); z-index:1;" } else { "" };
                                                let line_click_s = s.clone();
                                                rsx! {
                                                    div { key: "line-{hi}-{li}", style: "display:flex; white-space:pre; min-height:20px; line-height:20px; {bg} {fg} {cursor} {border}",
                                                        onclick: move |_| {
                                                            if is_change {
                                                                line_click_s.scm.toggle_diff_line(hi, li);
                                                            }
                                                        },
                                                        span { style: "width:48px; flex:none; text-align:right; padding-right:8px; color:var(--muted);", "{old}" }
                                                        span { style: "width:48px; flex:none; text-align:right; padding-right:8px; color:var(--muted);", "{new}" }
                                                        span { style: "width:16px; flex:none; text-align:center;", "{sign}" }
                                                        span { style: "flex:1;", "{l.text}" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if fd.hunks.iter().map(|h| h.lines.len()).sum::<usize>() >= DIFF_LINE_CAP {
                            div { style: "padding:var(--sp2) var(--sp3); color:var(--muted); font-family:var(--font);", "차이가 길어 앞의 {DIFF_LINE_CAP}줄만 표시합니다. 전체는 파일을 열어 확인하세요." }
                        }
                    }
                }
            }
        }
    }

    /// The conflict resolution pane in the editor area (FR-G7).
    pub fn conflict_pane(s: &Shared) -> Element {
        let st = s.scm.snapshot();
        let Some((path, load, step)) = st.conflict else { return rsx! {} };
        let name = base_name(&path).to_string();
        let close_s = s.clone();
        let resolve_s = s.clone();
        let preview_s = s.clone();
        let pick_ours_s = s.clone();
        let pick_theirs_s = s.clone();
        let ready_meta = match &load {
            ConflictLoad::Ready(file) => Some((file.progress_text(), file.is_resolved(), file.resolved_content())),
            _ => None,
        };

        rsx! {
            div { style: "flex:1; min-width:0; min-height:0; display:flex; flex-direction:column;", role: "region", "aria-label": "충돌 해결: {name}",
                div { class: "ed-tsub",
                    {icon_px("git", 14)}
                    b { "충돌 해결: {name}" }
                    if let Some((progress, resolved, resolved_content)) = ready_meta {
                        span { class: "ed-sbi", "{progress}" }
                        div { class: "spacer" }
                        button { class: "ed-pj", style: "border:1px solid var(--line);", onclick: move |_| pick_ours_s.scm.pick_all_conflicts(ConflictChoice::Ours), "모두 현재 것" }
                        button { class: "ed-pj", style: "border:1px solid var(--line);", onclick: move |_| pick_theirs_s.scm.pick_all_conflicts(ConflictChoice::Theirs), "모두 들어오는 것" }
                        button { class: "ed-pj", style: "border:1px solid var(--line);",
                            onclick: move |_| preview_s.scm.set_conflict_step(if step.is_preview() { ConflictStep::Editing } else { ConflictStep::Preview }),
                            if step.is_preview() { "편집" } else { "미리보기" }
                        }
                        button { class: "ed-pj", style: "background:var(--ink); color:var(--ink-text);",
                            disabled: !resolved || st.busy,
                            onclick: move |_| {
                                if let Some(content) = resolved_content.as_deref() {
                                    resolve_s.scm.resolve_conflict(&path, content);
                                }
                            },
                            "해결하고 스테이징"
                        }
                    } else {
                        div { class: "spacer" }
                    }
                    button { class: "ed-ib", title: "닫기", "aria-label": "닫기", onclick: move |_| close_s.scm.close_conflict(), {icon("close")} }
                }
                match load {
                    ConflictLoad::Loading => rsx! { div { class: "ed-empty", role: "status", p { "충돌 내용을 확인하는 중입니다." } } },
                    ConflictLoad::Failed(text) => rsx! { div { class: "ed-empty", role: "alert", p { "{text}" } } },
                    ConflictLoad::Ready(file) => {
                        if step.is_preview() {
                            let preview_text = file.resolved_content().unwrap_or_default();
                            rsx! {
                                div { style: "flex:1; min-height:0; overflow:auto; font-family:var(--mono); font-size:var(--fs-sm); padding:var(--sp3); white-space:pre;",
                                    "{preview_text}"
                                }
                            }
                        } else {
                            let mut block_idx = 0usize;
                            rsx! {
                                div { style: "flex:1; min-height:0; overflow:auto; font-family:var(--mono); font-size:var(--fs-sm); padding:var(--sp2) 0;",
                                    for (si, seg) in file.segments.iter().enumerate() {
                                        match seg {
                                            crate::ui::vcs::ConflictSegment::Common(text) => rsx! {
                                                div { key: "c-{si}", style: "white-space:pre; padding:0 var(--sp3); min-height:20px; line-height:20px;", "{text}" }
                                            },
                                            crate::ui::vcs::ConflictSegment::Block { ours_label, theirs_label, ours, theirs, choice } => {
                                                let b_i = block_idx;
                                                block_idx += 1;
                                                let ours_choice_s = s.clone();
                                                let theirs_choice_s = s.clone();
                                                let both_choice_s = s.clone();
                                                rsx! {
                                                    div { key: "b-{si}", style: "margin:var(--sp2) 0; border:1px solid var(--line); border-radius:var(--r-sm); overflow:hidden;",
                                                        div { style: "display:flex; gap:var(--sp2); align-items:center; padding:4px 8px; background:var(--surface-2); font-family:var(--font); font-size:var(--fs-xs);",
                                                            span { style: "font-weight:var(--fw-semibold);", "블록 #{b_i + 1}" }
                                                            button { class: "ed-pj", "aria-pressed": "{*choice == ConflictChoice::Ours}", style: if *choice == ConflictChoice::Ours { "background:var(--ink); color:var(--ink-text); height:22px; padding:0 6px;" } else { "border:1px solid var(--line); height:22px; padding:0 6px;" },
                                                                onclick: move |_| ours_choice_s.scm.set_conflict_choice(b_i, ConflictChoice::Ours),
                                                                "현재 변경 사항 ({ours_label})" }
                                                            button { class: "ed-pj", "aria-pressed": "{*choice == ConflictChoice::Theirs}", style: if *choice == ConflictChoice::Theirs { "background:var(--ink); color:var(--ink-text); height:22px; padding:0 6px;" } else { "border:1px solid var(--line); height:22px; padding:0 6px;" },
                                                                onclick: move |_| theirs_choice_s.scm.set_conflict_choice(b_i, ConflictChoice::Theirs),
                                                                "들어오는 변경 사항 ({theirs_label})" }
                                                            button { class: "ed-pj", "aria-pressed": "{*choice == ConflictChoice::Both}", style: if *choice == ConflictChoice::Both { "background:var(--ink); color:var(--ink-text); height:22px; padding:0 6px;" } else { "border:1px solid var(--line); height:22px; padding:0 6px;" },
                                                                onclick: move |_| both_choice_s.scm.set_conflict_choice(b_i, ConflictChoice::Both),
                                                                "모두 수용 (Both)" }
                                                        }
                                                        div { style: "background:var(--c-blue-soft); padding:4px 8px; border-left:3px solid var(--c-blue); white-space:pre;", "{ours}" }
                                                        div { style: "background:var(--c-orange-soft); padding:4px 8px; border-left:3px solid var(--c-orange); white-space:pre;", "{theirs}" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The working-tree side of an editable diff (FR-L31.10): the file's own document in a `CodeEditor`,
/// bound through the same widget bridge as its tab, so an edit shows in both, the tab's unsaved mark
/// follows it and Cmd/Ctrl+S saves it. The gutter marks are recomputed against the diff's base on
/// every change. It is its own tree (a `WidgetView`), like the tab's editor pane.
pub mod diff_editor_view {
    use dioxus_compose::prelude::*;
    use ember_editor::widget::HoverPhase as EditorHoverPhase;
    use ember_editor::{WidgetPos, WidgetRange};

    use crate::editor_host::HostCommand;
    use crate::ui::editor_workbench::{shared, Shared};

    fn pos(p: compose_rust::code::Position) -> WidgetPos {
        WidgetPos::new(p.line, p.column)
    }

    fn send(s: &Shared, event: Option<(ember_editor_conn::uri::UriComponents, u64, ember_editor::WidgetEvent)>) {
        if let Some((uri, generation, event)) = event {
            s.host.send(HostCommand::Widget(uri, generation, event));
        }
    }

    pub fn diff_editor() -> Element {
        let s = shared();
        let (box_w, box_h) = dioxus_compose::use_widget_box();
        let handle = dioxus_compose::use_code_editor();
        if box_w <= 0.0 || box_h <= 0.0 {
            return rsx! {};
        }
        let _ = *s.revision.read();
        let Some(rel) = s.scm.working_diff_path() else { return rsx! {} };
        let abs = s.scm.editor_path(&s.folder, &rel);
        let uri = s.uris.borrow().get(&abs).cloned();
        let Some(uri) = uri else { return rsx! {} };
        let doc = s.bridge.borrow().document(&uri).cloned();
        let Some(doc) = doc else { return rsx! {} };
        let generation = doc.generation;
        // The tab and the diff share the document, so they share its widget version too.
        let version = {
            let mut map = s.editor_versions.borrow_mut();
            let current = map.entry(generation).or_insert(doc.version);
            *current = (*current).max(doc.version);
            *current
        };
        // TODO(compose-rust#542): the change shows as gutter bars only; tint the added and changed
        // lines once the toolkit has whole-line background decorations.
        let gutter_decorations = match s.scm.diff_base(&doc.text, doc.dirty) {
            Some(base) => crate::ui::editor::diff_gutter_decorations(version, &super::diff_editor_model(&base, &doc.text).markers),
            None => compose_rust::code::GutterDecorations::default(),
        };
        let change_s = s.clone();
        let change_uri = uri.clone();
        let save_s = s.clone();
        let save_uri = uri.clone();
        let hover_s = s.clone();
        let hover_uri = uri.clone();
        let click_s = s.clone();
        let click_uri = uri.clone();
        let selection_s = s.clone();
        let selection_uri = uri;
        let range_s = s.clone();
        rsx! { CodeEditor {
            key: "diff-{generation}", width: Some(box_w), height: Some(box_h), handle,
            text: doc.text, decorations: compose_rust::code::Decorations::from_bytes(doc.decoration_bytes), syntax_spans: crate::ui::editor::syntax_spans(&doc.syntax), tab_width: Some(doc.tab_width),
            gutter_decorations,
            on_change: move |e: compose_rust::code::CodeChange| {
                change_s.editor_versions.borrow_mut().insert(generation, e.version);
                let event = change_s.bridge.borrow_mut().on_code_changed(&change_uri, e.version, WidgetRange::new(pos(e.range.start), pos(e.range.end)), e.text);
                send(&change_s, event);
            },
            on_save: move |e: compose_rust::code::SaveRequest| {
                let event = save_s.bridge.borrow().on_save_requested(&save_uri, e.version);
                send(&save_s, event);
                save_s.scm.refresh();
            },
            on_hover: move |e: compose_rust::code::CodeHover| {
                let phase = match e.phase { compose_rust::schema::HoverPhase::Rest => EditorHoverPhase::Rest, _ => EditorHoverPhase::Leave };
                let event = hover_s.bridge.borrow().on_code_hovered(&hover_uri, e.decoration.unwrap_or(0), pos(e.position), phase);
                send(&hover_s, event);
            },
            on_decoration_click: move |id| {
                let event = click_s.bridge.borrow().on_decoration_activated(&click_uri, id);
                send(&click_s, event);
            },
            on_selection_change: move |e: compose_rust::code::SelectionChange| {
                let mut position = selection_s.position;
                position.set((e.active.line as usize + 1, e.active.column as usize + 1));
                let event = selection_s.bridge.borrow().on_selection_changed(&selection_uri, e.version, pos(e.anchor), pos(e.active));
                send(&selection_s, event);
            },
            // Lines only: the base side cannot follow a partial-line scroll. v0.0.7: #544 fixed;
            // workaround kept until checked on a display (dioxus-compose has to forward `CodeScrolled`).
            on_visible_range_change: move |e: compose_rust::code::VisibleRange| range_s.scm.set_diff_lines(e.first_line, e.last_line),
        } }
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
    fn fr_l31_9_the_open_notices_name_the_file_and_say_why() {
        assert_eq!(
            open_failed_text("GUIDE.md", "파일이 없습니다. 지워졌거나 이름이 바뀌었을 수 있습니다"),
            "GUIDE.md을(를) 열지 못했습니다: 파일이 없습니다. 지워졌거나 이름이 바뀌었을 수 있습니다"
        );
        assert_eq!(open_offline_text("main.rs"), "서버와의 연결이 끊어져 main.rs을(를) 열 수 없습니다. 다시 연결한 뒤 여세요");
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

    #[test]
    fn fr_l31_2_reconstruct_base_text_handles_add_mod_del_and_empty() {
        let diff_patch = "diff --git a/file.rs b/file.rs\n--- a/file.rs\n+++ b/file.rs\n@@ -1,4 +1,5 @@\n line1\n-line2\n+line2_mod\n line3\n+line4_add\n line5\n";
        let diff = parse_unified(diff_patch).into_iter().next().unwrap();
        let cur_text = "line1\nline2_mod\nline3\nline4_add\nline5";
        let base = reconstruct_base_text(cur_text, &diff);
        assert_eq!(base, "line1\nline2\nline3\nline5");

        // When diff hunks are empty, returns current text normalized
        let empty_diff = FileDiff {
            path: String::new(),
            old_path: None,
            status: crate::ui::vcs::FileStatus::Modified,
            mode: None,
            binary: false,
            hunks: Vec::new(),
        };
        assert_eq!(reconstruct_base_text("hello\r\nworld", &empty_diff), "hello\nworld");

        // Deletion-only hunk
        let del_patch = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,3 +1,2 @@\n a\n-b\n c\n";
        let del_diff = parse_unified(del_patch).into_iter().next().unwrap();
        let cur_del = "a\nc";
        assert_eq!(reconstruct_base_text(cur_del, &del_diff), "a\nb\nc");

        // Addition-only hunk
        let add_patch = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,3 @@\n a\n+new_b\n c\n";
        let add_diff = parse_unified(add_patch).into_iter().next().unwrap();
        let cur_add = "a\nnew_b\nc";
        assert_eq!(reconstruct_base_text(cur_add, &add_diff), "a\nc");
    }

    #[test]
    fn fr_l31_2_markers_from_file_diff_identifies_added_modified_deleted() {
        use crate::ui::editor::{DiffLineKind, DiffLineMarker};

        let patch = "diff --git a/f.rs b/f.rs\n--- a/f.rs\n+++ b/f.rs\n@@ -1,4 +1,5 @@\n c1\n-d2\n+m2\n c3\n+a4\n";
        let diff = parse_unified(patch).into_iter().next().unwrap();
        let markers = markers_from_file_diff(&diff, 4);
        assert_eq!(markers, vec![
            DiffLineMarker { line: 1, kind: DiffLineKind::Modified },
            DiffLineMarker { line: 3, kind: DiffLineKind::Added },
        ]);

        let del_patch = "diff --git a/f.rs b/f.rs\n--- a/f.rs\n+++ b/f.rs\n@@ -1,3 +1,2 @@\n c1\n-d2\n c3\n";
        let del_diff = parse_unified(del_patch).into_iter().next().unwrap();
        let del_markers = markers_from_file_diff(&del_diff, 2);
        assert_eq!(del_markers, vec![
            DiffLineMarker { line: 0, kind: DiffLineKind::Deleted },
        ]);
    }

    #[test]
    fn fr_l31_2_side_by_side_rows_pairs_dels_and_adds() {
        let patch = "diff --git a/f.rs b/f.rs\n--- a/f.rs\n+++ b/f.rs\n@@ -1,3 +1,3 @@\n c1\n-old2\n+new2\n c3\n";
        let diff = parse_unified(patch).into_iter().next().unwrap();
        let hunk = &diff.hunks[0];
        let rows = side_by_side_rows(hunk);
        assert_eq!(rows.len(), 3);

        // Row 0: Context c1 on both sides
        assert_eq!(rows[0].0.as_ref().unwrap().1, LineKind::Context);
        assert_eq!(rows[0].1.as_ref().unwrap().1, LineKind::Context);

        // Row 1: Del on left, Add on right
        assert_eq!(rows[1].0.as_ref().unwrap().1, LineKind::Del);
        assert_eq!(rows[1].0.as_ref().unwrap().2, "old2");
        assert_eq!(rows[1].1.as_ref().unwrap().1, LineKind::Add);
        assert_eq!(rows[1].1.as_ref().unwrap().2, "new2");

        // Row 2: Context c3 on both sides
        assert_eq!(rows[2].0.as_ref().unwrap().1, LineKind::Context);
        assert_eq!(rows[2].1.as_ref().unwrap().1, LineKind::Context);
    }

    #[test]
    fn fr_l31_2_commit_gate_with_amend_allows_empty_staged() {
        let unstaged = ScmLoad::Ready(status(vec![file("a.rs", '.', 'M')]));
        // Normal commit without amend fails when nothing is staged
        assert_eq!(commit_gate_with_amend(&unstaged, "msg", false, false).unwrap_err(), NOTHING_STAGED);
        // Amend commit succeeds even when nothing is staged
        let req = commit_gate_with_amend(&unstaged, "Fix: fix previous\n\nexplanation", true, false).unwrap();
        assert!(req.amend);
        assert_eq!(req.summary, "Fix: fix previous");
        assert_eq!(req.description, "explanation");

        // Still checks message, busy, conflicts
        assert_eq!(commit_gate_with_amend(&unstaged, "", true, false).unwrap_err(), NEED_MESSAGE);
        assert_eq!(commit_gate_with_amend(&unstaged, "msg", true, true).unwrap_err(), BUSY);
        let conflicted = ScmLoad::Ready(status(vec![file("a.rs", 'U', 'U')]));
        assert_eq!(commit_gate_with_amend(&conflicted, "msg", true, false).unwrap_err(), CONFLICTS_LEFT);
    }

    #[test]
    fn fr_l31_2_discard_patch_and_tap_confirmation() {
        let patch = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n-one\n+two\n";
        let diff = parse_unified(patch).into_iter().next().unwrap();
        let p = discard_patch(&diff);
        assert!(p.is_some());
        assert!(p.unwrap().contains("+two"));

        // 600ms confirmation window for discard
        let path = "src/lib.rs";
        assert_eq!(discard_tap(None, path, 1000), DeleteTap::Arm);
        assert_eq!(discard_tap(Some((path, 1000)), path, 1200), DeleteTap::Wait);
        assert_eq!(discard_tap(Some((path, 1000)), path, 1600), DeleteTap::Confirm);
        assert_eq!(discard_tap(Some((path, 1000)), path, 4999), DeleteTap::Confirm);
        assert_eq!(discard_tap(Some((path, 1000)), path, 5000), DeleteTap::Expired);
        assert_eq!(discard_tap(Some(("other.rs", 1000)), path, 1600), DeleteTap::Arm);

        // If reading the file fails, reconstruct_base_from_read aborts with Err and never rebuilds from empty text.
        let read_err: Result<String, &str> = reconstruct_base_from_read(|| Err("permission denied"), &diff);
        assert_eq!(read_err, Err("permission denied"));
        let read_ok: Result<String, &str> = reconstruct_base_from_read(|| Ok("two".into()), &diff);
        assert_eq!(read_ok, Ok("one".into()));
    }

    #[test]
    fn fr_g7_conflict_resolution_choices_work() {
        let f = ConflictFile {
            path: "test.txt".into(),
            segments: vec![
                ConflictSegment::Common("header\n".into()),
                ConflictSegment::Block {
                    ours_label: "main".into(),
                    theirs_label: "feat".into(),
                    ours: "ours line\n".into(),
                    theirs: "theirs line\n".into(),
                    choice: ConflictChoice::Unresolved,
                },
                ConflictSegment::Common("footer\n".into()),
            ],
        };
        assert!(!f.is_resolved());
        assert_eq!(f.resolved_content(), None);

        let resolved_ours = f.with_choice(0, ConflictChoice::Ours);
        assert!(resolved_ours.is_resolved());
        assert_eq!(resolved_ours.resolved_content(), Some("header\nours line\nfooter\n".into()));

        let resolved_theirs = f.with_choice(0, ConflictChoice::Theirs);
        assert_eq!(resolved_theirs.resolved_content(), Some("header\ntheirs line\nfooter\n".into()));

        let resolved_both = f.with_choice(0, ConflictChoice::Both);
        assert_eq!(resolved_both.resolved_content(), Some("header\nours line\ntheirs line\nfooter\n".into()));

        let pick_all = f.pick_all(ConflictChoice::Theirs);
        assert!(pick_all.is_resolved());
        assert_eq!(pick_all.resolved_content(), Some("header\ntheirs line\nfooter\n".into()));
    }

    fn base_row(kind: RowKind, base: Option<(u32, &str)>, deleted_above: &[(u32, &str)]) -> WorkingRow {
        WorkingRow {
            kind,
            base: base.map(|(n, t)| (n, t.to_string())),
            deleted_above: deleted_above.iter().map(|(n, t)| (*n, t.to_string())).collect(),
        }
    }

    #[test]
    fn fr_l31_10_the_base_side_lines_up_with_the_working_tree_line_by_line() {
        // A changed line sits beside the line it replaced.
        let a = align_working_rows("a\nb\nc", "a\nB\nc");
        assert_eq!(
            a.rows,
            vec![
                base_row(RowKind::Same, Some((1, "a")), &[]),
                base_row(RowKind::Changed, Some((2, "b")), &[]),
                base_row(RowKind::Same, Some((3, "c")), &[]),
            ]
        );
        assert_eq!((a.added(), a.removed()), (1, 1));
        // An added line has an empty base row beside it.
        let a = align_working_rows("a\nc", "a\nb\nc");
        assert_eq!(a.rows[1], base_row(RowKind::Added, None, &[]));
        assert_eq!(a.rows[2], base_row(RowKind::Same, Some((2, "c")), &[]));
        assert_eq!((a.added(), a.removed()), (1, 0));
        // Removed lines sit above the next line, or at the end.
        let a = align_working_rows("a\nb\nc", "a\nc");
        assert_eq!(a.rows, vec![base_row(RowKind::Same, Some((1, "a")), &[]), base_row(RowKind::Same, Some((3, "c")), &[(2, "b")])]);
        let a = align_working_rows("a\nb", "a");
        assert_eq!(a.rows.len(), 1);
        assert_eq!(a.deleted_at_end, vec![(2, "b".to_string())]);
        // Two lines replaced by one: the first pairs with it, the second sits above the next line.
        let a = align_working_rows("a\nb\nc\nd", "a\nX\nd");
        assert_eq!(
            a.rows,
            vec![
                base_row(RowKind::Same, Some((1, "a")), &[]),
                base_row(RowKind::Changed, Some((2, "b")), &[]),
                base_row(RowKind::Same, Some((4, "d")), &[(3, "c")]),
            ]
        );
        assert_eq!((a.added(), a.removed()), (1, 2));
        // One row per working-tree line, always, so the rows follow the editor's lines.
        for (base, working) in [("", "x\ny"), ("x\ny", ""), ("p\nq\nr", "r\nq\np"), ("a\r\nb", "a\nb")] {
            assert_eq!(align_working_rows(base, working).rows.len(), text_lines(working).len(), "{base:?} {working:?}");
        }
        // The rows and the gutter marks come from the same edit script.
        let m = diff_editor_model("a\nb\nc\nd", "a\nX\nd");
        use crate::ui::editor::{DiffLineKind, DiffLineMarker};
        assert_eq!(m.markers, vec![DiffLineMarker { line: 1, kind: DiffLineKind::Modified }]);
    }

    #[test]
    fn fr_l31_10_deleted_lines_are_named_on_the_row_below() {
        assert_eq!(view::deleted_lines_title(&[]), "");
        assert_eq!(view::deleted_lines_title(&[(3, "c".into()), (4, "d".into())]), "삭제된 줄 2개\n3: c\n4: d");
    }

    const PATCH_ONE_TWO: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,3 +1,3 @@\n one\n-two\n+2\n three\n";

    fn open_working_diff(st: &mut ScmState) {
        st.side_by_side = true;
        let seq = st.begin_diff("a.rs", false, Some("/w/a.rs".into()));
        assert!(st.finish_diff(seq, diff_from(Ok(PATCH_ONE_TWO.into()))));
    }

    #[test]
    fn fr_l31_10_only_the_working_tree_side_by_side_is_editable() {
        let mut st = ScmState::new(true);
        st.side_by_side = true;
        let seq = st.begin_diff("a.rs", false, None);
        assert!(!st.diff.as_ref().unwrap().editable(), "not before the diff is read");
        st.finish_diff(seq, diff_from(Ok(PATCH_ONE_TWO.into())));
        assert!(st.diff.as_ref().unwrap().editable());
        st.diff.as_mut().unwrap().side_by_side = false;
        assert!(!st.diff.as_ref().unwrap().editable(), "the unified view stays read-only");
        let seq = st.begin_diff("a.rs", true, None);
        st.finish_diff(seq, diff_from(Ok(PATCH_ONE_TWO.into())));
        assert!(!st.diff.as_ref().unwrap().editable(), "the staged side is the index and stays read-only");
        assert!(!st.diff.as_ref().unwrap().of_working_tree());
    }

    #[test]
    fn fr_l31_10_reading_the_same_diff_again_keeps_it_editable() {
        let mut st = ScmState::new(true);
        open_working_diff(&mut st);
        st.diff_lines = (12, 40);
        st.begin_diff("a.rs", false, Some("/w/a.rs".into()));
        assert!(st.diff.as_ref().unwrap().editable());
        assert_eq!(st.diff_lines, (12, 40));
        st.begin_diff("b.rs", false, Some("/w/a.rs".into()));
        assert_eq!(st.diff.as_ref().unwrap().view, DiffView::Loading);
        assert_eq!(st.diff_lines, (0, 0));
    }

    #[test]
    fn fr_l31_10_an_edit_recomputes_the_diff_against_the_same_base() {
        use ember_editor::{SessionUpdate, WidgetBridge, WidgetPos, WidgetRange};
        use ember_editor_conn::uri::UriComponents;
        let uri = UriComponents::file("/w/a.rs");
        let mut bridge = WidgetBridge::new();
        bridge.apply(SessionUpdate::Opened { uri: uri.clone(), generation: 7, text: "one\n2\nthree".into(), language_id: "rust".into(), tab_width: 4 });
        let mut st = ScmState::new(true);
        open_working_diff(&mut st);
        let d = st.diff.as_mut().unwrap();
        let doc = bridge.document(&uri).unwrap().clone();
        let base = d.base_for(&doc.text, doc.dirty).unwrap();
        assert_eq!(base, "one\ntwo\nthree");
        let before = diff_editor_model(&base, &doc.text);
        assert_eq!(before.alignment.rows[1].kind, RowKind::Changed);
        assert_eq!((before.alignment.added(), before.alignment.removed()), (1, 1));

        // Typing a new line at the end of the file.
        bridge.on_code_changed(&uri, 1, WidgetRange::new(WidgetPos::new(2, 5), WidgetPos::new(2, 5)), "\nfour".into());
        bridge.apply(SessionUpdate::Dirty { uri: uri.clone(), dirty: true });
        let doc = bridge.document(&uri).unwrap().clone();
        assert_eq!(doc.text, "one\n2\nthree\nfour");
        let base = d.base_for(&doc.text, doc.dirty).unwrap();
        assert_eq!(base, "one\ntwo\nthree", "the base is not rebuilt from the edited text");
        let after = diff_editor_model(&base, &doc.text);
        assert_eq!(after.alignment.rows.len(), 4);
        assert_eq!(after.alignment.rows[3], base_row(RowKind::Added, None, &[]));
        assert_eq!((after.alignment.added(), after.alignment.removed()), (2, 1));
        use crate::ui::editor::{DiffLineKind, DiffLineMarker};
        assert_eq!(
            after.markers,
            vec![DiffLineMarker { line: 1, kind: DiffLineKind::Modified }, DiffLineMarker { line: 3, kind: DiffLineKind::Added }]
        );

        // A new diff answer while the edits are unsaved keeps the base; once saved it is rebuilt.
        let seq = st.begin_diff("a.rs", false, Some("/w/a.rs".into()));
        let saved_patch = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,3 +1,4 @@\n one\n-two\n+2\n three\n+four\n";
        st.finish_diff(seq, diff_from(Ok(saved_patch.into())));
        let d = st.diff.as_mut().unwrap();
        assert_eq!(d.base_for("one\n2\nthree\nfour\nfive", true).unwrap(), "one\ntwo\nthree");
        assert_eq!(d.base_for("one\n2\nthree\nfour", false).unwrap(), "one\ntwo\nthree");
    }

    #[test]
    fn fr_l31_10_the_tab_and_the_diff_edit_one_document() {
        use ember_editor::{SessionUpdate, WidgetBridge, WidgetPos, WidgetRange};
        use ember_editor_conn::uri::UriComponents;
        let uri = UriComponents::file("/w/a.rs");
        let mut bridge = WidgetBridge::new();
        bridge.apply(SessionUpdate::Opened { uri: uri.clone(), generation: 3, text: "one\n2\nthree".into(), language_id: "rust".into(), tab_width: 4 });
        // The diff's editor writes through the bridge entry the tab's editor reads.
        let event = bridge.on_code_changed(&uri, 1, WidgetRange::new(WidgetPos::new(0, 0), WidgetPos::new(0, 0)), "// ".into());
        assert!(matches!(event, Some((ref u, 3, _)) if *u == uri));
        bridge.apply(SessionUpdate::Dirty { uri: uri.clone(), dirty: true });
        let tab = bridge.active_document().unwrap();
        assert_eq!(tab.text, "// one\n2\nthree");
        assert!(tab.dirty, "the tab shows the unsaved mark of an edit made in the diff");
        assert_eq!(bridge.document(&uri).unwrap(), tab);
    }

    #[test]
    fn fr_l31_10_the_diff_opens_its_document_once_and_lets_it_go() {
        assert!(diff_document_needs_open("/w/a.rs", None, false));
        assert!(!diff_document_needs_open("/w/a.rs", None, true), "a tab already has it open");
        assert!(!diff_document_needs_open("/w/a.rs", Some("/w/a.rs"), false), "asked once already");
        assert!(diff_document_needs_open("/w/b.rs", Some("/w/a.rs"), false));
        assert_eq!(release_diff_document(true, true), DiffDocRelease::Keep);
        assert_eq!(release_diff_document(true, false), DiffDocRelease::Keep);
        assert_eq!(release_diff_document(false, true), DiffDocRelease::OpenTab);
        assert_eq!(release_diff_document(false, false), DiffDocRelease::Close);
    }

    #[test]
    fn fr_l31_10_save_from_the_diff_saves_the_working_tree_document() {
        use ember_editor::{SessionUpdate, WidgetBridge, WidgetEvent};
        use ember_editor_conn::uri::UriComponents;
        assert_eq!(save_target(Some("/w/b.rs"), Some("/w/a.rs".into())), Some("/w/a.rs".into()));
        assert_eq!(save_target(Some("/w/b.rs"), None), Some("/w/b.rs".into()));
        assert_eq!(save_target(None, None), None);
        let uri = UriComponents::file("/w/a.rs");
        let mut bridge = WidgetBridge::new();
        bridge.apply(SessionUpdate::Opened { uri: uri.clone(), generation: 5, text: "x".into(), language_id: "rust".into(), tab_width: 4 });
        let (u, generation, event) = bridge.on_save_requested(&uri, 4).unwrap();
        assert_eq!((u, generation, event), (uri, 5, WidgetEvent::CodeSaveRequested { version: 4 }));
    }
}
