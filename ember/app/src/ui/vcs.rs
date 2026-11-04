//! The 버전 관리 screen (FR-L19, INTENT D34, `docs/DESIGN.md` section 6.6).
//!
//! The decisions are plain functions and types in the first half of this file, unit-tested
//! without a renderer: section routing, the compact tabs, the remote action and ahead/behind
//! text, the merge gate, unified diff parsing, line, hunk and file staging, and the file tree.
//! The components in the second half only hand them to dioxus-compose.
//!
//! The data comes from the server's `/projects/{id}/git/...` routes (FR-G4) through
//! `ember_client::Api::git_*`: [`fetch_snapshot`] reads it, [`build_snapshot`] maps status and
//! unified diffs into [`FileDiff`]s, and [`plan_staging`] and [`build_patch`] turn the line
//! staging into server calls. When the server is unreachable or the project is not a
//! repository, the data views say [`SOON`] and nothing is invented. Sections beyond changes and
//! branches still wait for their endpoints (#168).
//!
//! What the pinned toolkit cannot do yet, with the workaround used (`GAPS-WORKLIST.md`):
//! - no clickable Row or Column: a text Button is the hit target of a row or a diff line (TODO(compose-rust#372): drop once Row/Column on_click is confirmed);
//! - no sticky column: the line check sits in the row and scrolls with it (TODO(compose-rust#424));
//! - no SVG graph: the history is a list first (TODO(compose-rust#308): draw the graph on a `Canvas`).

use std::collections::{BTreeMap, BTreeSet};

use dioxus_compose::prelude::*;

use ember_client::api::{ApiError, GitCommitted, GitDiff, GitOutput, GitStatus};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::demo::GitError;
use crate::services::{run, services};
use crate::ui::components::{AppButton, ButtonKind, EmberCard, EmberField, Label, PickChip, Pill, PillTone};
use crate::ui::home::MenuItem;
use crate::ui::project::{display_name, is_default_project, project_boxes, ProjectBox};
use crate::ui::shell::NavMenu;
use crate::ui::theme::*;
use crate::ui::{use_ui, Route, Ui};

/// What a view shows while the server does not provide its data.
pub const SOON: &str = "준비 중입니다";

// ---------------------------------------------------------------------------------------------
// Sections and layout
// ---------------------------------------------------------------------------------------------

/// The seven sections, in the order of `docs/DESIGN.md` section 6.6.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Changes,
    History,
    Branches,
    PullRequests,
    Stash,
    Conflicts,
    Team,
}

impl Section {
    pub const ALL: [Section; 7] = [
        Section::Changes,
        Section::History,
        Section::Branches,
        Section::PullRequests,
        Section::Stash,
        Section::Conflicts,
        Section::Team,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Section::Changes => "변경 사항",
            Section::History => "히스토리",
            Section::Branches => "브랜치",
            Section::PullRequests => "풀 리퀘스트",
            Section::Stash => "스태시",
            Section::Conflicts => "충돌",
            Section::Team => "팀 작업",
        }
    }

    /// The stable key (the mockup's URL fragment).
    pub fn key(self) -> &'static str {
        match self {
            Section::Changes => "changes",
            Section::History => "history",
            Section::Branches => "branches",
            Section::PullRequests => "prs",
            Section::Stash => "stash",
            Section::Conflicts => "conflict",
            Section::Team => "team",
        }
    }

    pub fn from_key(key: &str) -> Option<Section> {
        Section::ALL.into_iter().find(|s| s.key() == key)
    }
}

/// The section a key opens; an unknown key opens 변경 사항.
pub fn route_section(key: &str) -> Section {
    Section::from_key(key).unwrap_or(Section::Changes)
}

/// How the section list is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionNav {
    /// A column on the left with counts (wide windows).
    List,
    /// A row of pills above the panes (medium and compact windows).
    Pills,
}

pub fn section_nav(class: WindowSizeClass) -> SectionNav {
    match class {
        WindowSizeClass::Expanded => SectionNav::List,
        _ => SectionNav::Pills,
    }
}

/// How a section's panes are arranged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneLayout {
    /// Side by side.
    Two,
    /// One above the other (768).
    Stacked,
    /// One pane at a time behind in-screen tabs (390).
    Tabs,
}

pub fn pane_layout(class: WindowSizeClass) -> PaneLayout {
    match class {
        WindowSizeClass::Expanded => PaneLayout::Two,
        WindowSizeClass::Medium => PaneLayout::Stacked,
        WindowSizeClass::Compact => PaneLayout::Tabs,
    }
}

/// An in-screen tab of the compact layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaneTab {
    Files,
    Diff,
    Commit,
    Graph,
    Detail,
    List,
}

impl PaneTab {
    pub fn label(self) -> &'static str {
        match self {
            PaneTab::Files => "파일",
            PaneTab::Diff => "차이",
            PaneTab::Commit => "커밋",
            PaneTab::Graph => "그래프",
            PaneTab::Detail => "상세",
            PaneTab::List => "목록",
        }
    }
}

/// The tabs a section has at compact width; empty when it has one pane.
pub fn pane_tabs(section: Section) -> &'static [PaneTab] {
    match section {
        Section::Changes => &[PaneTab::Files, PaneTab::Diff, PaneTab::Commit],
        Section::History => &[PaneTab::Graph, PaneTab::Detail],
        Section::PullRequests => &[PaneTab::List, PaneTab::Detail],
        _ => &[],
    }
}

/// The tab to show after the user picks a file, a commit or a pull request.
pub fn tab_after_select(tab: PaneTab) -> PaneTab {
    match tab {
        PaneTab::Files => PaneTab::Diff,
        PaneTab::Graph | PaneTab::List => PaneTab::Detail,
        other => other,
    }
}

/// The tab to show in a section: the chosen one if the section has it, else its first; `None`
/// for a section with one pane.
pub fn effective_tab(section: Section, chosen: PaneTab) -> Option<PaneTab> {
    let tabs = pane_tabs(section);
    if tabs.contains(&chosen) {
        Some(chosen)
    } else {
        tabs.first().copied()
    }
}

// ---------------------------------------------------------------------------------------------
// Remote: pull, push, fetch
// ---------------------------------------------------------------------------------------------

/// The remote bar's main button. Behind wins over ahead, because a push is rejected until the
/// branch has pulled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteAction {
    Pull(u32),
    Push(u32),
    Fetch,
}

pub fn remote_action(ahead: u32, behind: u32) -> RemoteAction {
    if behind > 0 {
        RemoteAction::Pull(behind)
    } else if ahead > 0 {
        RemoteAction::Push(ahead)
    } else {
        RemoteAction::Fetch
    }
}

impl RemoteAction {
    pub fn label(self) -> String {
        match self {
            RemoteAction::Pull(n) => format!("Pull {n}"),
            RemoteAction::Push(n) => format!("Push {n}"),
            RemoteAction::Fetch => "Fetch".to_string(),
        }
    }
}

/// "앞섬 2 · 뒤처짐 1", or a note that the branch equals its remote.
pub fn ahead_behind_text(ahead: u32, behind: u32) -> String {
    match (ahead, behind) {
        (0, 0) => "원격과 같습니다".to_string(),
        (a, 0) => format!("앞섬 {a}"),
        (0, b) => format!("뒤처짐 {b}"),
        (a, b) => format!("앞섬 {a} \u{00b7} 뒤처짐 {b}"),
    }
}

/// The last fetch, from its age in seconds. `None`: never fetched.
pub fn fetched_text(age_secs: Option<u64>) -> String {
    match age_secs {
        None => "아직 fetch하지 않았습니다".to_string(),
        Some(s) if s < 60 => "방금 fetch했습니다".to_string(),
        Some(s) if s < 3_600 => format!("{}분 전에 fetch했습니다", s / 60),
        Some(s) if s < 86_400 => format!("{}시간 전에 fetch했습니다", s / 3_600),
        Some(s) => format!("{}일 전에 fetch했습니다", s / 86_400),
    }
}

/// The widths of a branch's behind | ahead bar as fractions of the bar, `(behind, ahead)`.
pub fn bar_fractions(ahead: u32, behind: u32) -> (f32, f32) {
    let total = ahead + behind;
    if total == 0 {
        (0.0, 0.0)
    } else {
        (behind as f32 / total as f32, ahead as f32 / total as f32)
    }
}

/// The line under the branch name.
pub fn upstream_text(upstream: Option<&str>) -> String {
    match upstream {
        Some(u) if !u.is_empty() => format!("{u} 추적"),
        _ => "추적하는 원격 브랜치가 없습니다".to_string(),
    }
}

/// "<what>은 준비 중입니다", with the topic particle that fits the last syllable of `what`.
pub fn soon_line(what: &str) -> String {
    let has_final = what
        .chars()
        .last()
        .map(|c| ('\u{ac00}'..='\u{d7a3}').contains(&c) && (c as u32 - 0xac00) % 28 != 0)
        .unwrap_or(false);
    format!("{what}{} {SOON}", if has_final { "은" } else { "는" })
}

// ---------------------------------------------------------------------------------------------
// Pull requests: the merge gate
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiState {
    Passed,
    Running,
    Waiting,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    Pending,
    NoReviewer,
}

/// Merge is enabled only with CI passing and an approval; otherwise the reason is shown under
/// the button (FR-L19).
pub fn merge_gate(ci: CiState, review: ReviewState) -> Result<(), &'static str> {
    match ci {
        CiState::Failed => return Err("CI가 실패했습니다"),
        CiState::Running => return Err("CI가 실행 중입니다"),
        CiState::Waiting => return Err("CI를 기다리는 중입니다"),
        CiState::Passed => {}
    }
    match review {
        ReviewState::Approved => Ok(()),
        ReviewState::ChangesRequested => Err("변경 요청이 있습니다"),
        ReviewState::Pending | ReviewState::NoReviewer => Err("승인이 필요합니다"),
    }
}

// ---------------------------------------------------------------------------------------------
// Unified diff
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Add,
    Del,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub text: String,
    /// The line number in the old file; `None` for an added line.
    pub old_no: Option<u32>,
    /// The line number in the new file; `None` for a deleted line.
    pub new_no: Option<u32>,
}

impl DiffLine {
    /// An added or deleted line can be staged; a context line cannot.
    pub fn is_change(&self) -> bool {
        self.kind != LineKind::Context
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// The `@@ -a,b +c,d @@ section` line.
    pub header: String,
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
}

impl FileStatus {
    /// The letter in the file list.
    pub fn letter(self) -> &'static str {
        match self {
            FileStatus::Modified => "M",
            FileStatus::Added => "A",
            FileStatus::Deleted => "D",
            FileStatus::Renamed => "R",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FileStatus::Modified => "수정",
            FileStatus::Added => "추가",
            FileStatus::Deleted => "삭제",
            FileStatus::Renamed => "이름 변경",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    /// The old path of a rename.
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    pub fn added(&self) -> usize {
        self.count(LineKind::Add)
    }

    pub fn removed(&self) -> usize {
        self.count(LineKind::Del)
    }

    fn count(&self, kind: LineKind) -> usize {
        self.hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == kind)
            .count()
    }
}

/// Parse `@@ -a[,b] +c[,d] @@ ...` into `(old_start, old_len, new_start, new_len)`. A missing
/// length is 1.
pub fn parse_hunk_header(line: &str) -> Option<(u32, u32, u32, u32)> {
    let rest = line.strip_prefix("@@ -")?;
    let (ranges, _) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let range = |s: &str| -> Option<(u32, u32)> {
        match s.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let (os, ol) = range(old)?;
    let (ns, nl) = range(new)?;
    Some((os, ol, ns, nl))
}

fn strip_side(path: &str) -> &str {
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
}

/// Parse the output of `git diff` (or any unified diff) into files, hunks and lines.
///
/// A hunk's lengths decide where its content ends, so a deleted line that reads `-- x` (shown
/// as `--- x`) is not mistaken for a file header.
pub fn parse_unified(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut cur: Option<FileDiff> = None;
    let mut open: Option<Hunk> = None;
    let (mut left_old, mut left_new) = (0u32, 0u32);
    let (mut no_old, mut no_new) = (0u32, 0u32);

    fn blank(path: &str) -> FileDiff {
        FileDiff {
            path: path.to_string(),
            old_path: None,
            status: FileStatus::Modified,
            binary: false,
            hunks: Vec::new(),
        }
    }
    fn close(cur: &mut Option<FileDiff>, open: &mut Option<Hunk>) {
        if let (Some(f), Some(h)) = (cur.as_mut(), open.take()) {
            f.hunks.push(h);
        }
    }

    for line in text.lines() {
        if left_old > 0 || left_new > 0 {
            let (kind, body) = match line.chars().next() {
                Some('+') => (LineKind::Add, &line[1..]),
                Some('-') => (LineKind::Del, &line[1..]),
                Some(' ') => (LineKind::Context, &line[1..]),
                None => (LineKind::Context, ""),
                Some('\\') => continue,
                Some(_) => (LineKind::Context, line),
            };
            let (old_no, new_no) = match kind {
                LineKind::Add => {
                    left_new = left_new.saturating_sub(1);
                    no_new += 1;
                    (None, Some(no_new - 1))
                }
                LineKind::Del => {
                    left_old = left_old.saturating_sub(1);
                    no_old += 1;
                    (Some(no_old - 1), None)
                }
                LineKind::Context => {
                    left_old = left_old.saturating_sub(1);
                    left_new = left_new.saturating_sub(1);
                    no_old += 1;
                    no_new += 1;
                    (Some(no_old - 1), Some(no_new - 1))
                }
            };
            if let Some(h) = open.as_mut() {
                h.lines.push(DiffLine {
                    kind,
                    text: body.to_string(),
                    old_no,
                    new_no,
                });
            }
            continue;
        }
        if line.starts_with('\\') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            close(&mut cur, &mut open);
            files.extend(cur.take());
            let path = rest
                .rsplit_once(" b/")
                .map(|(_, b)| b)
                .unwrap_or_else(|| strip_side(rest));
            cur = Some(blank(path));
        } else if let Some((os, ol, ns, nl)) = parse_hunk_header(line) {
            close(&mut cur, &mut open);
            if cur.is_none() {
                cur = Some(blank(""));
            }
            left_old = ol;
            left_new = nl;
            no_old = os;
            no_new = ns;
            open = Some(Hunk {
                header: line.to_string(),
                old_start: os,
                old_len: ol,
                new_start: ns,
                new_len: nl,
                lines: Vec::new(),
            });
        } else if let Some(p) = line.strip_prefix("--- ") {
            // A header only when no hunk is open (checked above), so a new file starts when the
            // current one already has hunks.
            if open.is_some() || cur.as_ref().map_or(true, |f| !f.hunks.is_empty()) {
                close(&mut cur, &mut open);
                files.extend(cur.take());
                cur = Some(blank(""));
            }
            if let Some(f) = cur.as_mut() {
                if p != "/dev/null" {
                    f.old_path = Some(strip_side(p).to_string());
                    if f.path.is_empty() {
                        f.path = strip_side(p).to_string();
                    }
                }
            }
        } else if let Some(p) = line.strip_prefix("+++ ") {
            if let Some(f) = cur.as_mut() {
                if p != "/dev/null" {
                    f.path = strip_side(p).to_string();
                }
            }
        } else if let Some(f) = cur.as_mut() {
            if line.starts_with("new file mode") {
                f.status = FileStatus::Added;
            } else if line.starts_with("deleted file mode") {
                f.status = FileStatus::Deleted;
            } else if let Some(from) = line.strip_prefix("rename from ") {
                f.status = FileStatus::Renamed;
                f.old_path = Some(from.to_string());
            } else if let Some(to) = line.strip_prefix("rename to ") {
                f.path = to.to_string();
            } else if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
                f.binary = true;
            }
        }
    }
    close(&mut cur, &mut open);
    files.extend(cur);
    // A deleted file's `+++ /dev/null` leaves the path from the `---` side; a rename keeps `old_path`.
    for f in &mut files {
        if f.status == FileStatus::Modified
            && f.old_path.is_none()
            && f.hunks.first().is_some_and(|h| h.old_len == 0)
        {
            f.status = FileStatus::Added;
        }
    }
    files
}

// ---------------------------------------------------------------------------------------------
// Staging
// ---------------------------------------------------------------------------------------------

/// A check box state: none, some or all of the lines below it are staged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    Off,
    Partial,
    On,
}

pub fn check_of(staged: usize, total: usize) -> Check {
    if total == 0 || staged == 0 {
        Check::Off
    } else if staged >= total {
        Check::On
    } else {
        Check::Partial
    }
}

/// The glyph of a check box.
pub fn check_glyph(check: Check) -> &'static str {
    match check {
        Check::On => "\u{25cf}",
        Check::Partial => "\u{25d0}",
        Check::Off => "\u{25cb}",
    }
}

/// The check of a folder from its children.
pub fn folder_check(children: &[Check]) -> Check {
    if children.is_empty() || children.iter().all(|c| *c == Check::Off) {
        Check::Off
    } else if children.iter().all(|c| *c == Check::On) {
        Check::On
    } else {
        Check::Partial
    }
}

/// The staged lines of one file, as `(hunk, line)` indexes into its [`FileDiff`]. Only added and
/// deleted lines can be staged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileStaging {
    staged: BTreeSet<(usize, usize)>,
}

impl FileStaging {
    pub fn none() -> FileStaging {
        FileStaging::default()
    }

    pub fn all(diff: &FileDiff) -> FileStaging {
        let mut s = FileStaging::none();
        s.set_all(diff, true);
        s
    }

    pub fn is_staged(&self, hunk: usize, line: usize) -> bool {
        self.staged.contains(&(hunk, line))
    }

    fn change_lines(diff: &FileDiff, hunk: usize) -> Vec<(usize, usize)> {
        diff.hunks
            .get(hunk)
            .map(|h| {
                h.lines
                    .iter()
                    .enumerate()
                    .filter(|(_, l)| l.is_change())
                    .map(|(i, _)| (hunk, i))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Toggle one line. A context line or an unknown index changes nothing.
    pub fn toggle_line(&mut self, diff: &FileDiff, hunk: usize, line: usize) {
        let is_change = diff
            .hunks
            .get(hunk)
            .and_then(|h| h.lines.get(line))
            .is_some_and(DiffLine::is_change);
        if !is_change {
            return;
        }
        if !self.staged.remove(&(hunk, line)) {
            self.staged.insert((hunk, line));
        }
    }

    /// Toggle a hunk: stage all its changed lines, or unstage them when all are staged.
    pub fn toggle_hunk(&mut self, diff: &FileDiff, hunk: usize) {
        let lines = Self::change_lines(diff, hunk);
        let all = !lines.is_empty() && lines.iter().all(|k| self.staged.contains(k));
        for k in lines {
            if all {
                self.staged.remove(&k);
            } else {
                self.staged.insert(k);
            }
        }
    }

    pub fn set_all(&mut self, diff: &FileDiff, on: bool) {
        self.staged.clear();
        if on {
            for h in 0..diff.hunks.len() {
                self.staged.extend(Self::change_lines(diff, h));
            }
        }
    }

    /// `(staged, total)` changed lines of the file.
    pub fn counts(&self, diff: &FileDiff) -> (usize, usize) {
        let total = diff.added() + diff.removed();
        let staged = self
            .staged
            .iter()
            .filter(|(h, l)| {
                diff.hunks
                    .get(*h)
                    .and_then(|h| h.lines.get(*l))
                    .is_some_and(DiffLine::is_change)
            })
            .count();
        (staged, total)
    }

    pub fn check(&self, diff: &FileDiff) -> Check {
        let (s, t) = self.counts(diff);
        check_of(s, t)
    }

    pub fn hunk_check(&self, diff: &FileDiff, hunk: usize) -> Check {
        let lines = Self::change_lines(diff, hunk);
        let staged = lines.iter().filter(|k| self.staged.contains(k)).count();
        check_of(staged, lines.len())
    }
}

/// The staging of every changed file, by path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Staging {
    files: BTreeMap<String, FileStaging>,
}

impl Staging {
    pub fn new(diffs: &[FileDiff], all_staged: bool) -> Staging {
        let files = diffs
            .iter()
            .map(|d| {
                let s = if all_staged {
                    FileStaging::all(d)
                } else {
                    FileStaging::none()
                };
                (d.path.clone(), s)
            })
            .collect();
        Staging { files }
    }

    pub fn file(&self, path: &str) -> Option<&FileStaging> {
        self.files.get(path)
    }

    pub fn file_mut(&mut self, path: &str) -> &mut FileStaging {
        self.files.entry(path.to_string()).or_default()
    }

    pub fn check(&self, diff: &FileDiff) -> Check {
        self.file(&diff.path).map_or(Check::Off, |s| s.check(diff))
    }

    /// Click the file's check: stage everything unless everything is staged already.
    pub fn toggle_file(&mut self, diff: &FileDiff) {
        let on = self.check(diff) != Check::On;
        self.file_mut(&diff.path).set_all(diff, on);
    }

    /// Click the "all" check.
    pub fn toggle_all(&mut self, diffs: &[FileDiff]) {
        let has_lines = |d: &FileDiff| d.added() + d.removed() > 0;
        let on = diffs
            .iter()
            .any(|d| has_lines(d) && self.check(d) != Check::On);
        for d in diffs {
            self.file_mut(&d.path).set_all(d, on);
        }
    }

    /// Files with at least one staged line.
    pub fn staged_files(&self, diffs: &[FileDiff]) -> usize {
        diffs.iter().filter(|d| self.check(d) != Check::Off).count()
    }
}

/// The commit button is enabled with a summary and at least one staged file. Amending keeps the
/// previous message and may change nothing else, so it is always enabled.
pub fn commit_enabled(summary: &str, staged_files: usize, amend: bool) -> bool {
    if amend {
        return true;
    }
    !summary.trim().is_empty() && staged_files > 0
}

/// "main에 커밋 (2개 파일)".
pub fn commit_label(branch: &str, staged_files: usize) -> String {
    if staged_files == 0 {
        format!("{branch}에 커밋")
    } else {
        format!("{branch}에 커밋 ({staged_files}개 파일)")
    }
}

// ---------------------------------------------------------------------------------------------
// File tree
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeRow {
    /// A folder; a chain of single-child folders is merged into one label.
    Dir {
        label: String,
        /// With a trailing `/`; the key of the fold set.
        path: String,
        depth: usize,
        open: bool,
    },
    File {
        /// The index into the list the tree was built from.
        index: usize,
        name: String,
        depth: usize,
    },
}

#[derive(Default)]
struct Node {
    dirs: BTreeMap<String, Node>,
    files: Vec<usize>,
}

fn walk(
    node: &Node,
    paths: &[String],
    folded: &BTreeSet<String>,
    depth: usize,
    prefix: &str,
    out: &mut Vec<TreeRow>,
) {
    for (name, child) in &node.dirs {
        let mut label = name.clone();
        let mut path = format!("{prefix}{name}/");
        let mut ch = child;
        while ch.files.is_empty() && ch.dirs.len() == 1 {
            let (n2, c2) = ch.dirs.iter().next().expect("one dir");
            label.push('/');
            label.push_str(n2);
            path.push_str(n2);
            path.push('/');
            ch = c2;
        }
        let open = !folded.contains(&path);
        out.push(TreeRow::Dir {
            label,
            path: path.clone(),
            depth,
            open,
        });
        if open {
            walk(ch, paths, folded, depth + 1, &path, out);
        }
    }
    for &i in &node.files {
        let name = paths[i].rsplit('/').next().unwrap_or(&paths[i]).to_string();
        out.push(TreeRow::File {
            index: i,
            name,
            depth,
        });
    }
}

/// The rows of the changed-files tree: folders first, then files in the given order. Folders
/// whose path is in `folded` hide their children.
pub fn build_tree(paths: &[String], folded: &BTreeSet<String>) -> Vec<TreeRow> {
    let mut root = Node::default();
    for (i, p) in paths.iter().enumerate() {
        let mut parts: Vec<&str> = p.split('/').collect();
        let _ = parts.pop();
        let mut n = &mut root;
        for part in parts {
            n = n.dirs.entry(part.to_string()).or_default();
        }
        n.files.push(i);
    }
    let mut out = Vec::new();
    walk(&root, paths, folded, 0, "", &mut out);
    out
}

// ---------------------------------------------------------------------------------------------
// Repository data
// ---------------------------------------------------------------------------------------------

/// What the screen reads of a project's repository.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RepoSnapshot {
    pub branch: String,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub fetched_age_secs: Option<u64>,
    pub branches: Vec<String>,
    pub changes: Vec<FileDiff>,
    /// Paths already fully staged in the index when the snapshot was read.
    pub staged: BTreeSet<String>,
}

/// The state of the repository view of one project.
#[derive(Debug, Clone, PartialEq)]
pub enum Load {
    Loading,
    Ready(RepoSnapshot),
    /// The project's path is not a repository (the server answered 404).
    NotRepo,
    /// The server could not be reached, or it failed; the text says why.
    Unavailable(String),
}

impl Load {
    pub fn snapshot(&self) -> Option<&RepoSnapshot> {
        match self {
            Load::Ready(s) => Some(s),
            _ => None,
        }
    }

    /// The line under the bar while there is no snapshot.
    pub fn note(&self) -> Option<String> {
        match self {
            Load::Ready(_) => None,
            Load::Loading => Some("저장소 정보를 불러오는 중입니다".to_string()),
            Load::NotRepo => Some("이 프로젝트의 경로는 git 저장소가 아닙니다".to_string()),
            Load::Unavailable(_) => Some(soon_line("저장소 정보")),
        }
    }
}

/// A server answer as a view state: 404 means not a repository, any other failure means the data
/// is unavailable.
pub fn load_from(result: Result<RepoSnapshot, ApiError>) -> Load {
    match result {
        Ok(s) => Load::Ready(s),
        Err(e) if e.status() == Some(404) => Load::NotRepo,
        Err(e) => Load::Unavailable(e.to_string()),
    }
}

/// The file status shown for porcelain v2 characters (index first, then the work tree).
pub fn status_of(index: char, worktree: char) -> FileStatus {
    match (index, worktree) {
        ('R', _) | (_, 'R') | ('C', _) | (_, 'C') => FileStatus::Renamed,
        ('D', _) | (_, 'D') => FileStatus::Deleted,
        ('A', _) | ('?', _) => FileStatus::Added,
        _ => FileStatus::Modified,
    }
}

/// Files by path in a unified diff.
fn diffs_by_path(patch: &str) -> BTreeMap<String, FileDiff> {
    parse_unified(patch)
        .into_iter()
        .map(|d| (d.path.clone(), d))
        .collect()
}

/// Paths of untracked files in a status, which have no entry in `git diff`.
pub fn untracked_paths(status: &GitStatus) -> Vec<String> {
    status
        .files
        .iter()
        .filter(|f| f.index == '?' && f.worktree == '?')
        .map(|f| f.path.clone())
        .collect()
}

/// Map a status and its diffs into a snapshot. A file takes its work tree diff, else its staged
/// diff, else the diff read for an untracked file; one with none shows no hunks. The status
/// decides the file's status letter and whether it starts staged (index changed, work tree
/// clean).
pub fn build_snapshot(
    status: &GitStatus,
    unstaged: &str,
    staged: &str,
    untracked: &BTreeMap<String, String>,
) -> RepoSnapshot {
    let work = diffs_by_path(unstaged);
    let index = diffs_by_path(staged);
    let mut changes = Vec::new();
    let mut already = BTreeSet::new();
    for f in &status.files {
        let mut d = work
            .get(&f.path)
            .or_else(|| index.get(&f.path))
            .cloned()
            .or_else(|| {
                untracked
                    .get(&f.path)
                    .and_then(|p| parse_unified(p).into_iter().next())
            })
            .unwrap_or_else(|| FileDiff {
                path: f.path.clone(),
                old_path: None,
                status: FileStatus::Modified,
                binary: false,
                hunks: Vec::new(),
            });
        d.path = f.path.clone();
        d.status = status_of(f.index, f.worktree);
        if f.old_path.is_some() {
            d.old_path = f.old_path.clone();
        }
        if !matches!(f.index, '.' | '?' | ' ') && f.worktree == '.' {
            already.insert(f.path.clone());
        }
        changes.push(d);
    }
    RepoSnapshot {
        branch: status.branch.clone(),
        upstream: status.upstream.clone(),
        ahead: status.ahead,
        behind: status.behind,
        fetched_age_secs: status.fetched_age_secs,
        branches: status.branches.clone(),
        changes,
        staged: already,
    }
}

/// A demo source answer as an API result: the status and message of its error become an
/// [`ApiError::Status`], so the demo and the server share the same load states.
pub fn git_result<T: DeserializeOwned>(r: Result<Value, GitError>) -> Result<T, ApiError> {
    let v = r.map_err(|e| ApiError::Status { status: e.status, message: format!("{}: {}", e.code, e.message) })?;
    serde_json::from_value(v).map_err(|e| ApiError::Decode(e.to_string()))
}

/// One git operation of a project: the demo source when the app serves itself, else the main
/// server. `body` is the contract's body without `repo`; the answer is decoded into `T` (use
/// `Value` for the operations that answer 204).
pub async fn git_call<T: DeserializeOwned>(project: &str, op: &str, body: Value) -> Result<T, ApiError> {
    let svc = services();
    let v = match &svc.demo {
        Some(demo) => return git_result(demo.git(project, op, &body)),
        None => svc.client.api().git_raw(project, op, body).await?,
    };
    serde_json::from_value(v).map_err(|e| ApiError::Decode(e.to_string()))
}

async fn git_diff_of(project: &str, path: Option<&str>, staged: bool) -> Result<String, ApiError> {
    let d: GitDiff = git_call(project, "diff", json!({ "path": path, "staged": staged })).await?;
    Ok(d.patch)
}

/// Read a project's repository: status, both diffs, and the diff of each untracked file. Runs
/// on the runtime, not the UI thread.
pub async fn fetch_snapshot(project: &str) -> Result<RepoSnapshot, ApiError> {
    let status: GitStatus = git_call(project, "status", json!({})).await?;
    let unstaged = git_diff_of(project, None, false).await?;
    let staged = git_diff_of(project, None, true).await?;
    let mut untracked = BTreeMap::new();
    for path in untracked_paths(&status) {
        // A file that cannot be read stays listed without hunks.
        if let Ok(patch) = git_diff_of(project, Some(&path), false).await {
            untracked.insert(path, patch);
        }
    }
    Ok(build_snapshot(&status, &unstaged, &staged, &untracked))
}

/// One call that brings the index to the staging the user chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageOp {
    Stage(Vec<String>),
    Unstage(Vec<String>),
    /// A patch of only the staged lines of one file.
    Apply(String),
}

/// A unified patch holding only the staged lines of `diff`: an unstaged addition is left out and
/// an unstaged deletion stays as context. `None` when no line is staged.
pub fn build_patch(diff: &FileDiff, staging: &FileStaging) -> Option<String> {
    let old = diff.old_path.as_deref().unwrap_or(&diff.path);
    let mut out = format!("diff --git a/{old} b/{}\n", diff.path);
    if diff.status == FileStatus::Added {
        out.push_str("new file mode 100644\n--- /dev/null\n");
    } else {
        out.push_str(&format!("--- a/{old}\n"));
    }
    out.push_str(&format!("+++ b/{}\n", diff.path));
    let mut any = false;
    // The lines already added or removed by earlier hunks shift where a later hunk lands.
    let mut shift: i64 = 0;
    for (hi, h) in diff.hunks.iter().enumerate() {
        let mut body = String::new();
        let (mut old_len, mut new_len) = (0u32, 0u32);
        let mut picked = false;
        for (li, l) in h.lines.iter().enumerate() {
            let on = staging.is_staged(hi, li);
            match (l.kind, on) {
                (LineKind::Context, _) | (LineKind::Del, false) => {
                    body.push_str(&format!(" {}\n", l.text));
                    old_len += 1;
                    new_len += 1;
                }
                (LineKind::Add, true) => {
                    body.push_str(&format!("+{}\n", l.text));
                    new_len += 1;
                    picked = true;
                }
                (LineKind::Del, true) => {
                    body.push_str(&format!("-{}\n", l.text));
                    old_len += 1;
                    picked = true;
                }
                (LineKind::Add, false) => {}
            }
        }
        if !picked {
            continue;
        }
        any = true;
        let new_start = (h.old_start as i64 + shift).max(0);
        out.push_str(&format!("@@ -{},{} +{},{} @@\n", h.old_start, old_len, new_start, new_len));
        out.push_str(&body);
        shift += new_len as i64 - old_len as i64;
    }
    any.then_some(out)
}

/// The calls that make the index match `staging`, for the files that changed. A file fully
/// checked is staged whole (unless the index has it already); a partly checked one goes as a
/// patch; an unchecked one is unstaged when the index had it.
pub fn plan_staging(snap: &RepoSnapshot, staging: &Staging) -> Vec<StageOp> {
    let (mut stage, mut unstage, mut patches) = (Vec::new(), Vec::new(), Vec::new());
    for d in &snap.changes {
        let in_index = snap.staged.contains(&d.path);
        let no_lines = d.added() + d.removed() == 0;
        match staging.check(d) {
            Check::On => {
                if !in_index {
                    stage.push(d.path.clone());
                }
            }
            Check::Partial => {
                if let Some(p) = staging.file(&d.path).and_then(|s| build_patch(d, s)) {
                    patches.push(StageOp::Apply(p));
                }
            }
            Check::Off => {
                // A file without lines (binary, empty) has no check to uncheck: keep the index.
                if in_index && !no_lines {
                    unstage.push(d.path.clone());
                }
            }
        }
    }
    let mut ops = Vec::new();
    if !unstage.is_empty() {
        ops.push(StageOp::Unstage(unstage));
    }
    if !stage.is_empty() {
        ops.push(StageOp::Stage(stage));
    }
    ops.extend(patches);
    ops
}

/// What the commit button hands over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRequest {
    pub ops: Vec<StageOp>,
    pub summary: String,
    pub amend: bool,
}

/// Staging calls in order, then the commit. Stops at the first failure.
pub async fn run_commit(project: &str, req: CommitRequest) -> Result<String, ApiError> {
    for op in &req.ops {
        let (name, body) = match op {
            StageOp::Stage(p) => ("stage", json!({ "paths": p })),
            StageOp::Unstage(p) => ("unstage", json!({ "paths": p })),
            StageOp::Apply(patch) => ("apply", json!({ "patch": patch, "reverse": false })),
        };
        git_call::<Value>(project, name, body).await?;
    }
    let body = json!({ "summary": req.summary, "description": null, "amend": req.amend });
    let done: GitCommitted = git_call(project, "commit", body).await?;
    Ok(commit_done_text(&done.sha))
}

/// "커밋했습니다 (abc1234)".
pub fn commit_done_text(sha: &str) -> String {
    let short: String = sha.chars().take(7).collect();
    format!("커밋했습니다 ({short})")
}

/// Fetch, pull or push, as the remote button's action says.
pub async fn run_remote(project: &str, action: RemoteAction) -> Result<String, ApiError> {
    let op = match action {
        RemoteAction::Pull(_) => "pull",
        RemoteAction::Push(_) => "push",
        RemoteAction::Fetch => "fetch",
    };
    let out: GitOutput = git_call(project, op, json!({ "remote": null })).await?;
    Ok(remote_done_text(action, &out.output))
}

/// The notice after a remote action: the first line git printed, else a plain "done".
pub fn remote_done_text(action: RemoteAction, output: &str) -> String {
    let name = match action {
        RemoteAction::Pull(_) => "Pull",
        RemoteAction::Push(_) => "Push",
        RemoteAction::Fetch => "Fetch",
    };
    match output.lines().map(str::trim).find(|l| !l.is_empty()) {
        Some(line) => format!("{name} 완료: {line}"),
        None => format!("{name}을 마쳤습니다"),
    }
}

/// A name the new-branch button accepts: not empty, no spaces, and none of the characters git
/// refuses in a ref name. The server answers 400 for anything else git rejects.
pub fn valid_branch_name(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty()
        && !n.starts_with('-')
        && !n.starts_with('/')
        && !n.ends_with('/')
        && !n.ends_with('.')
        && !n.contains("..")
        && !n.chars().any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c))
}

/// The notice for a failed call; git's own message is kept.
pub fn failure_text(what: &str, e: &ApiError) -> String {
    match e {
        ApiError::Status { message, .. } => format!("{what}하지 못했습니다: {message}"),
        other => format!("{what}하지 못했습니다. 서버에 닿지 않습니다: {other}"),
    }
}

/// The count beside a section's name; `None` while the data is unavailable.
pub fn section_count(section: Section, snap: Option<&RepoSnapshot>) -> Option<usize> {
    let s = snap?;
    match section {
        Section::Changes => Some(s.changes.len()),
        Section::Branches => Some(s.branches.len()),
        _ => None,
    }
}

/// Projects that can have a repository: every project except 기본 프로젝트.
pub fn repo_projects(boxes: &[ProjectBox]) -> Vec<&ProjectBox> {
    boxes
        .iter()
        .filter(|b| !is_default_project(&b.id))
        .collect()
}

/// Which project the screen shows: the selected one if it can have a repository, else the first
/// that can.
pub fn pick_project(selected: Option<&str>, boxes: &[ProjectBox]) -> Option<String> {
    let list = repo_projects(boxes);
    selected
        .and_then(|s| list.iter().find(|b| b.id == s))
        .or_else(|| list.first())
        .map(|b| b.id.clone())
}

/// Open 버전 관리 on a project (from the project screen and the session top bar).
pub fn open_for_project(ui: Ui, project: &str) {
    let mut sel = ui.selected_project;
    sel.set(Some(project.to_string()));
    // Leaves an open conversation; the menu is set after, because `go` marks 프로젝트.
    ui.go(Route::Projects);
    let mut menu = ui.menu;
    menu.set(NavMenu::VersionControl);
}

// ---------------------------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------------------------

fn notice(text: &str) {
    Message::new(text.to_string())
        .with_duration(MessageDuration::Short)
        .show();
}

#[component]
fn SmallNote(#[props(into)] text: String) -> Element {
    let p = use_ember_theme().palette;
    rsx! { Label { text, fs: Fs::Xs, color: paint(p.text_2) } }
}

#[component]
fn Unavailable(section: Section) -> Element {
    let p = use_ember_theme().palette;
    rsx! {
        EmberCard {
            fill_max_width: true,
            Column {
                fill_max_width: true,
                padding: SP[4],
                space_role: SpaceRole::Sm,
                Label { text: section.label(), fs: Fs::Lg, weight: Weight::Semibold }
                // TODO(server, #168): this section needs the repository endpoints listed there.
                Label { text: SOON, color: paint(p.text_2) }
            }
        }
    }
}

/// The bar of three: repository, current branch, remote origin.
#[component]
fn RepoBar(
    projects: Vec<(String, String)>,
    project: ProjectBox,
    snap: Option<RepoSnapshot>,
    compact: bool,
    on_pick: EventHandler<String>,
    on_remote: EventHandler<RemoteAction>,
    on_checkout: EventHandler<String>,
    on_new_branch: EventHandler<String>,
) -> Element {
    let mut project_menu = use_signal(|| false);
    let mut branch_menu = use_signal(|| false);
    let mut new_branch = use_signal(String::new);
    let has = snap.is_some();
    let (ahead, behind) = snap.as_ref().map_or((0, 0), |s| (s.ahead, s.behind));
    let action = remote_action(ahead, behind);
    let branch_text = match (&snap, &project.branch) {
        (Some(s), _) => s.branch.clone(),
        (None, Some(b)) => b.clone(),
        _ => "브랜치".to_string(),
    };
    let upstream = snap
        .as_ref()
        .map(|s| upstream_text(s.upstream.as_deref()))
        .unwrap_or_else(|| SOON.to_string());
    let remote_line = match &snap {
        Some(s) => format!(
            "{} \u{00b7} {}",
            ahead_behind_text(s.ahead, s.behind),
            fetched_text(s.fetched_age_secs)
        ),
        None => SOON.to_string(),
    };
    let branches = snap
        .as_ref()
        .map(|s| s.branches.clone())
        .unwrap_or_default();
    let current_branch = snap.as_ref().map(|s| s.branch.clone());

    let repo_cell = rsx! {
        EmberCard {
            fill_max_width: true,
            Column {
                fill_max_width: true,
                padding: SP[2],
                space_role: SpaceRole::Xs,
                SmallNote { text: "저장소" }
                Menu {
                    expanded: project_menu(),
                    on_dismiss: move |_| project_menu.set(false),
                    anchor: rsx! {
                        PickChip {
                            text: display_name(&project.id),
                            icon: IconRole::Library,
                            on_click: move |()| project_menu.set(true),
                        }
                    },
                    for (id, name) in projects.iter().cloned() {
                        MenuItem {
                            key: "{id}",
                            text: name.clone(),
                            selected: id == project.id,
                            on_click: move |()| {
                                on_pick.call(id.clone());
                                project_menu.set(false);
                            },
                        }
                    }
                }
                SmallNote { text: project.repo_line() }
            }
        }
    };
    let branch_cell = rsx! {
        EmberCard {
            fill_max_width: true,
            Column {
                fill_max_width: true,
                padding: SP[2],
                space_role: SpaceRole::Xs,
                SmallNote { text: "현재 브랜치" }
                Menu {
                    expanded: branch_menu(),
                    on_dismiss: move |_| branch_menu.set(false),
                    anchor: rsx! {
                        PickChip {
                            text: branch_text.clone(),
                            icon: IconRole::List,
                            on_click: move |()| branch_menu.set(true),
                        }
                    },
                    if branches.is_empty() {
                        Label { text: soon_line("브랜치 목록"), fs: Fs::Sm }
                    }
                    for b in branches.iter().cloned() {
                        MenuItem {
                            key: "{b}",
                            text: b.clone(),
                            selected: Some(&b) == current_branch.as_ref(),
                            on_click: {
                                let name = b.clone();
                                let current = current_branch.clone();
                                move |()| {
                                    if Some(&name) != current.as_ref() {
                                        on_checkout.call(name.clone());
                                    }
                                    branch_menu.set(false);
                                }
                            },
                        }
                    }
                    EmberField {
                        fill_max_width: true,
                        placeholder: "새 브랜치 이름",
                        on_value_change: move |v: String| new_branch.set(v),
                    }
                    AppButton {
                        text: "새 브랜치",
                        icon: IconRole::Add,
                        small: true,
                        enabled: has && valid_branch_name(&new_branch.read()),
                        on_click: move |()| {
                            on_new_branch.call(new_branch.read().trim().to_string());
                            branch_menu.set(false);
                        },
                    }
                }
                SmallNote { text: upstream.clone() }
            }
        }
    };
    let remote_cell = rsx! {
        EmberCard {
            fill_max_width: true,
            Column {
                fill_max_width: true,
                padding: SP[2],
                space_role: SpaceRole::Xs,
                SmallNote { text: "원격 origin" }
                Row {
                    fill_max_width: true,
                    space_role: SpaceRole::Sm,
                    alignment: Alignment::CenterStart,
                    AppButton {
                        text: action.label(),
                        kind: ButtonKind::Primary,
                        small: true,
                        enabled: has,
                        on_click: move |()| on_remote.call(action),
                    }
                    if has && action != RemoteAction::Fetch {
                        AppButton {
                            text: "Fetch",
                            small: true,
                            on_click: move |()| on_remote.call(RemoteAction::Fetch),
                        }
                    }
                }
                SmallNote { text: remote_line.clone() }
            }
        }
    };

    rsx! {
        if compact {
            Column { fill_max_width: true, space_role: SpaceRole::Sm, {repo_cell} {branch_cell} {remote_cell} }
        } else {
            Row {
                fill_max_width: true,
                space_role: SpaceRole::Sm,
                Column { weight: 1.0, {repo_cell} }
                Column { weight: 1.0, {branch_cell} }
                Column { weight: 1.5, {remote_cell} }
            }
        }
    }
}

/// The section list on the left (wide windows) with counts.
#[component]
fn SectionList(
    active: Section,
    snap: Option<RepoSnapshot>,
    on_pick: EventHandler<Section>,
) -> Element {
    let p = use_ember_theme().palette;
    rsx! {
        Column {
            width: 200.0,
            space_role: SpaceRole::Xs,
            for sec in Section::ALL {
                Button {
                    key: "{sec.key()}",
                    text: match section_count(sec, snap.as_ref()) {
                        Some(n) => format!("{}  {n}", sec.label()),
                        None => sec.label().to_string(),
                    },
                    variant: ButtonVariant::Text,
                    fill_max_width: true,
                    height: H_NAV_ITEM,
                    shape_role: ShapeRole::Full,
                    background: if sec == active { paint(p.sel) } else { paint(p.bg) },
                    color: paint(if sec == active { p.text } else { p.muted }),
                    on_click: move |()| on_pick.call(sec),
                }
            }
        }
    }
}

/// The section pills above the panes (medium and compact windows).
#[component]
fn SectionPills(active: Section, on_pick: EventHandler<Section>) -> Element {
    rsx! {
        ScrollRow {
            fill_max_width: true,
            Row {
                spacing: SP[1],
                for sec in Section::ALL {
                    PickChip {
                        key: "{sec.key()}",
                        text: sec.label(),
                        selected: sec == active,
                        on_click: move |()| on_pick.call(sec),
                    }
                }
            }
        }
    }
}

#[component]
fn PaneTabs(section: Section, active: Option<PaneTab>, on_pick: EventHandler<PaneTab>) -> Element {
    rsx! {
        Row {
            fill_max_width: true,
            spacing: SP[1],
            for t in pane_tabs(section).iter().copied() {
                PickChip {
                    key: "{t.label()}",
                    text: t.label(),
                    selected: Some(t) == active,
                    on_click: move |()| on_pick.call(t),
                }
            }
        }
    }
}

/// The changes section: tree, diff and commit box.
#[component]
fn ChangesPane(
    snap: RepoSnapshot,
    layout: PaneLayout,
    tab: Option<PaneTab>,
    on_tab: EventHandler<PaneTab>,
    on_commit: EventHandler<CommitRequest>,
) -> Element {
    let p = use_ember_theme().palette;
    let diffs = snap.changes.clone();
    let mut staging = use_signal(|| {
        // Files the index holds already start checked.
        let mut st = Staging::new(&diffs, false);
        for d in diffs.iter().filter(|d| snap.staged.contains(&d.path)) {
            st.file_mut(&d.path).set_all(d, true);
        }
        st
    });
    let mut folded = use_signal(BTreeSet::<String>::new);
    let mut selected = use_signal(|| 0usize);
    let mut summary = use_signal(String::new);
    let mut amend = use_signal(|| false);

    let paths: Vec<String> = diffs.iter().map(|d| d.path.clone()).collect();
    let rows = build_tree(&paths, &folded.read());
    let sel = (*selected.read()).min(diffs.len().saturating_sub(1));
    let staged_files = staging.read().staged_files(&diffs);
    let can_commit = commit_enabled(&summary.read(), staged_files, *amend.read());

    let files_pane = rsx! {
        EmberCard {
            fill_max_width: true,
            Column {
                fill_max_width: true,
                padding: SP[2],
                space_role: SpaceRole::Xs,
                Row {
                    fill_max_width: true,
                    alignment: Alignment::CenterStart,
                    Button {
                        text: format!("{}  변경 파일 {}", check_glyph(folder_check(&diffs.iter().map(|d| staging.read().check(d)).collect::<Vec<_>>())), diffs.len()),
                        variant: ButtonVariant::Text,
                        on_click: { let d = diffs.clone(); move |()| staging.write().toggle_all(&d) },
                    }
                }
                for row in rows.iter().cloned() {
                    match row {
                        TreeRow::Dir { label, path, depth, open } => rsx! {
                            Row {
                                key: "{path}",
                                fill_max_width: true,
                                Spacer { width: depth as f32 * SP[3] },
                                Button {
                                    text: format!("{} {label}", if open { "\u{25be}" } else { "\u{25b8}" }),
                                    variant: ButtonVariant::Text,
                                    on_click: move |()| {
                                        let mut f = folded.write();
                                        if !f.remove(&path) { f.insert(path.clone()); }
                                    },
                                }
                            }
                        },
                        TreeRow::File { index, name, depth } => rsx! {
                            Row {
                                key: "{index}",
                                fill_max_width: true,
                                Spacer { width: depth as f32 * SP[3] },
                                Button {
                                    text: check_glyph(staging.read().check(&diffs[index])),
                                    variant: ButtonVariant::Text,
                                    on_click: { let d = diffs[index].clone(); move |()| staging.write().toggle_file(&d) },
                                }
                                Button {
                                    text: format!("{name}  {}", diffs[index].status.letter()),
                                    variant: ButtonVariant::Text,
                                    background: if index == sel { paint(p.sel) } else { paint(p.surface) },
                                    on_click: move |()| {
                                        selected.set(index);
                                        on_tab.call(tab_after_select(PaneTab::Files));
                                    },
                                }
                            }
                        },
                    }
                }
            }
        }
    };

    let diff_pane = rsx! {
        EmberCard {
            fill_max_width: true,
            Column {
                fill_max_width: true,
                padding: SP[2],
                space_role: SpaceRole::Xs,
                if let Some(file) = diffs.get(sel).cloned() {
                    DiffView { file: file.clone(), staging }
                } else {
                    Label { text: "변경 사항이 없습니다. 작업 트리가 깨끗합니다", color: paint(p.text_2) }
                }
            }
        }
    };

    let commit_pane = rsx! {
        EmberCard {
            fill_max_width: true,
            Column {
                fill_max_width: true,
                padding: SP[2],
                space_role: SpaceRole::Sm,
                EmberField {
                    fill_max_width: true,
                    placeholder: "요약 (필수)",
                    on_value_change: move |v: String| summary.set(v),
                }
                Row {
                    fill_max_width: true,
                    alignment: Alignment::CenterStart,
                    space_role: SpaceRole::Sm,
                    Switch { checked: amend(), on_change: move |v: bool| amend.set(v) }
                    Label { text: "마지막 커밋 수정 (amend)", fs: Fs::Sm }
                }
                AppButton {
                    text: commit_label(&snap.branch, staged_files),
                    kind: ButtonKind::Primary,
                    enabled: can_commit,
                    on_click: {
                        let snap = snap.clone();
                        move |()| {
                            on_commit.call(CommitRequest {
                                ops: plan_staging(&snap, &staging.read()),
                                summary: summary.read().trim().to_string(),
                                amend: *amend.read(),
                            });
                            summary.set(String::new());
                        }
                    },
                }
                if !can_commit {
                    SmallNote { text: "요약과 스테이징한 파일이 필요합니다" }
                }
            }
        }
    };

    rsx! {
        match layout {
            PaneLayout::Two => rsx! {
                Row { fill_max_width: true, space_role: SpaceRole::Sm,
                    Column { width: 320.0, {files_pane} }
                    Column { weight: 1.0, space_role: SpaceRole::Sm, {diff_pane} {commit_pane} }
                }
            },
            PaneLayout::Stacked => rsx! {
                Column { fill_max_width: true, space_role: SpaceRole::Sm, {files_pane} {diff_pane} {commit_pane} }
            },
            PaneLayout::Tabs => rsx! {
                Column { fill_max_width: true, space_role: SpaceRole::Sm,
                    PaneTabs { section: Section::Changes, active: tab, on_pick: move |t| on_tab.call(t) }
                    match tab.unwrap_or(PaneTab::Files) {
                        PaneTab::Diff => diff_pane,
                        PaneTab::Commit => commit_pane,
                        _ => files_pane,
                    }
                }
            },
        }
    }
}

/// One file's diff with a check per changed line and per hunk.
#[component]
fn DiffView(file: FileDiff, staging: Signal<Staging>) -> Element {
    let p = use_ember_theme().palette;
    let (on, total) = staging
        .read()
        .file(&file.path)
        .map_or((0, file.added() + file.removed()), |s| s.counts(&file));
    rsx! {
        Row {
            fill_max_width: true,
            space_role: SpaceRole::Sm,
            alignment: Alignment::CenterStart,
            Label { text: file.path.clone(), weight: Weight::Medium, max_lines: 1 }
            Spacer { weight: 1.0 }
            Pill { text: format!("+{}", file.added()), tone: PillTone::Green, small: true }
            Pill { text: format!("-{}", file.removed()), tone: PillTone::Red, small: true }
            Pill { text: format!("{on}/{total}줄 스테이징"), tone: PillTone::Grey, small: true }
        }
        if file.binary {
            Label { text: "이진 파일은 차이를 보여 주지 않습니다", color: paint(p.text_2) }
        }
        for (hi, h) in file.hunks.iter().enumerate() {
            Row {
                key: "h{hi}",
                fill_max_width: true,
                alignment: Alignment::CenterStart,
                Label { text: h.header.clone(), fs: Fs::Xs, color: paint(p.text_2), max_lines: 1 }
                Spacer { weight: 1.0 }
                AppButton {
                    text: "이 덩어리 전체",
                    small: true,
                    on_click: { let f = file.clone(); move |()| staging.write().file_mut(&f.path).toggle_hunk(&f, hi) },
                }
            }
            for (li, l) in h.lines.iter().enumerate() {
                Surface {
                    key: "{hi}:{li}",
                    fill_max_width: true,
                    background: match l.kind {
                        LineKind::Add => paint(p.green.soft),
                        LineKind::Del => paint(p.red.soft),
                        LineKind::Context => paint(p.surface),
                    },
                    Row {
                        fill_max_width: true,
                        alignment: Alignment::CenterStart,
                        if l.is_change() {
                            Button {
                                text: check_glyph(if staging.read().file(&file.path).is_some_and(|s| s.is_staged(hi, li)) { Check::On } else { Check::Off }),
                                variant: ButtonVariant::Text,
                                on_click: { let f = file.clone(); move |()| staging.write().file_mut(&f.path).toggle_line(&f, hi, li) },
                            }
                        } else {
                            Spacer { width: H_BTN }
                        }
                        Text {
                            text: format!(
                                "{:>4} {:>4} {}{}",
                                l.old_no.map(|n| n.to_string()).unwrap_or_default(),
                                l.new_no.map(|n| n.to_string()).unwrap_or_default(),
                                match l.kind { LineKind::Add => "+", LineKind::Del => "-", LineKind::Context => " " },
                                l.text
                            ),
                            type_role: TypeRole::Mono,
                            max_lines: 1,
                        }
                    }
                }
            }
        }
    }
}

/// Runs a server call, shows its result as a notice, then reloads the snapshot.
fn act<F>(what: &'static str, work: F, reload: impl FnOnce() + 'static)
where
    F: std::future::Future<Output = Result<String, ApiError>> + Send + 'static,
{
    run(work, move |r| {
        match r {
            Ok(text) => notice(&text),
            Err(e) => notice(&failure_text(what, &e)),
        }
        reload();
    });
}

/// One project's repository: reads the snapshot from the server, shows it, and runs the
/// actions. Keyed by project, so a project switch starts a fresh load.
#[component]
fn Repo(
    project: ProjectBox,
    projects: Vec<(String, String)>,
    on_pick: EventHandler<String>,
) -> Element {
    let window = use_window_size();
    let p = use_ember_theme().palette;
    let class = window.class;
    let mut section = use_signal(|| Section::Changes);
    let mut tab = use_signal(|| PaneTab::Files);
    let mut load = use_signal(|| Load::Loading);
    // Bumped after each reload, so the changes pane starts with fresh staging.
    let mut rev = use_signal(|| 0u32);
    let id = project.id.clone();

    let reload = use_callback({
        let id = id.clone();
        move |()| {
            let id = id.clone();
            run(
                async move { fetch_snapshot(&id).await },
                move |r| {
                    if let Err(e) = &r {
                        tracing::warn!("reading the repository failed: {e}");
                    }
                    load.set(load_from(r));
                    rev += 1;
                },
            );
        }
    });
    use_hook(|| reload.call(()));

    let on_remote = {
        let id = id.clone();
        move |a: RemoteAction| {
            let id = id.clone();
            let what = match a {
                RemoteAction::Pull(_) => "Pull",
                RemoteAction::Push(_) => "Push",
                RemoteAction::Fetch => "Fetch",
            };
            act(what, async move { run_remote(&id, a).await }, move || reload.call(()));
        }
    };
    let on_checkout = {
        let id = id.clone();
        move |branch: String| {
            let id = id.clone();
            act(
                "브랜치를 전환",
                async move {
                    git_call::<Value>(&id, "checkout", json!({ "branch": branch })).await?;
                    Ok(format!("{branch} 브랜치로 전환했습니다"))
                },
                move || reload.call(()),
            );
        }
    };
    let on_new_branch = {
        let id = id.clone();
        move |name: String| {
            let id = id.clone();
            act(
                "브랜치를 만들",
                async move {
                    git_call::<Value>(&id, "branch", json!({ "name": name, "from": null })).await?;
                    Ok(format!("{name} 브랜치를 만들었습니다"))
                },
                move || reload.call(()),
            );
        }
    };
    let on_commit = {
        let id = id.clone();
        move |req: CommitRequest| {
            let id = id.clone();
            act("커밋", async move { run_commit(&id, req).await }, move || reload.call(()));
        }
    };

    let state = load.read().clone();
    let snap = state.snapshot().cloned();
    let sec = *section.read();
    let active_tab = effective_tab(sec, *tab.read());
    let layout = pane_layout(class);
    let wide = section_nav(class) == SectionNav::List;

    let panes = match (&snap, sec) {
        (Some(s), Section::Changes) => rsx! {
            ChangesPane {
                key: "{s.branch}:{rev}",
                snap: s.clone(),
                layout,
                tab: active_tab,
                on_tab: move |t| tab.set(t),
                on_commit,
            }
        },
        _ => rsx! { Unavailable { section: sec } },
    };

    rsx! {
        RepoBar {
            projects,
            project: project.clone(),
            snap: snap.clone(),
            compact: class == WindowSizeClass::Compact,
            on_pick: move |id: String| on_pick.call(id),
            on_remote,
            on_checkout,
            on_new_branch,
        }
        if let Some(text) = state.note() {
            Label { text, fs: Fs::Sm, color: paint(p.text_2) }
        }
        if wide {
            Row {
                fill_max_width: true,
                space_role: SpaceRole::Md,
                SectionList { active: sec, snap: snap.clone(), on_pick: move |s| section.set(s) }
                Column { weight: 1.0, space_role: SpaceRole::Sm, {panes} }
            }
        } else {
            Column {
                fill_max_width: true,
                space_role: SpaceRole::Sm,
                SectionPills { active: sec, on_pick: move |s| section.set(s) }
                if layout == PaneLayout::Tabs && snap.is_none() && !pane_tabs(sec).is_empty() {
                    PaneTabs { section: sec, active: active_tab, on_pick: move |t| tab.set(t) }
                }
                {panes}
            }
        }
    }
}

/// The screen wired as 버전 관리.
#[component]
pub fn VcsScreen() -> Element {
    let ui = use_ui();

    let boxes = {
        let _ = *ui.live.launcher.read();
        services().client.read(project_boxes)
    };
    let selected = ui.selected_project.read().clone();
    let current = pick_project(selected.as_deref(), &boxes);
    let project = current
        .as_ref()
        .and_then(|id| boxes.iter().find(|b| &b.id == id).cloned());
    let projects: Vec<(String, String)> = repo_projects(&boxes)
        .into_iter()
        .map(|b| (b.id.clone(), b.name.clone()))
        .collect();

    rsx! {
        ScrollColumn {
            fill_max_width: true,
            weight: 1.0,
            Column {
                fill_max_width: true,
                padding: SP[3],
                space_role: SpaceRole::Md,
                Label { text: "버전 관리", fs: Fs::Xl2, weight: Weight::Semibold }
                if let Some(project) = project {
                    Repo {
                        key: "{project.id}",
                        project: project.clone(),
                        projects,
                        on_pick: move |id: String| {
                            let mut s = ui.selected_project;
                            s.set(Some(id));
                        },
                    }
                } else {
                    Label { text: "저장소를 둘 수 있는 프로젝트가 없습니다" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    // ---- section routing ----

    #[test]
    fn sections_are_in_the_design_order_with_stable_keys() {
        let labels: Vec<_> = Section::ALL.iter().map(|x| x.label()).collect();
        assert_eq!(
            labels,
            [
                "변경 사항",
                "히스토리",
                "브랜치",
                "풀 리퀘스트",
                "스태시",
                "충돌",
                "팀 작업"
            ]
        );
        for sec in Section::ALL {
            assert_eq!(Section::from_key(sec.key()), Some(sec));
        }
    }

    #[test]
    fn routing_an_unknown_key_opens_changes() {
        assert_eq!(route_section("prs"), Section::PullRequests);
        assert_eq!(route_section("conflict"), Section::Conflicts);
        assert_eq!(route_section("nope"), Section::Changes);
        assert_eq!(route_section(""), Section::Changes);
    }

    #[test]
    fn layout_follows_the_size_class() {
        assert_eq!(section_nav(WindowSizeClass::Expanded), SectionNav::List);
        assert_eq!(section_nav(WindowSizeClass::Medium), SectionNav::Pills);
        assert_eq!(section_nav(WindowSizeClass::Compact), SectionNav::Pills);
        assert_eq!(pane_layout(WindowSizeClass::Expanded), PaneLayout::Two);
        assert_eq!(pane_layout(WindowSizeClass::Medium), PaneLayout::Stacked);
        assert_eq!(pane_layout(WindowSizeClass::Compact), PaneLayout::Tabs);
    }

    #[test]
    fn compact_tabs_per_section() {
        assert_eq!(
            pane_tabs(Section::Changes),
            [PaneTab::Files, PaneTab::Diff, PaneTab::Commit]
        );
        assert_eq!(
            pane_tabs(Section::History),
            [PaneTab::Graph, PaneTab::Detail]
        );
        assert_eq!(
            pane_tabs(Section::PullRequests),
            [PaneTab::List, PaneTab::Detail]
        );
        assert!(pane_tabs(Section::Stash).is_empty());
        assert!(pane_tabs(Section::Team).is_empty());
    }

    #[test]
    fn picking_an_item_moves_to_the_next_tab() {
        assert_eq!(tab_after_select(PaneTab::Files), PaneTab::Diff);
        assert_eq!(tab_after_select(PaneTab::Graph), PaneTab::Detail);
        assert_eq!(tab_after_select(PaneTab::List), PaneTab::Detail);
        assert_eq!(tab_after_select(PaneTab::Commit), PaneTab::Commit);
    }

    #[test]
    fn a_stale_tab_falls_back_to_the_first() {
        assert_eq!(
            effective_tab(Section::Changes, PaneTab::Diff),
            Some(PaneTab::Diff)
        );
        assert_eq!(
            effective_tab(Section::History, PaneTab::Diff),
            Some(PaneTab::Graph)
        );
        assert_eq!(effective_tab(Section::Stash, PaneTab::Diff), None);
    }

    // ---- branch ahead/behind ----

    #[test]
    fn the_remote_button_follows_the_state() {
        assert_eq!(remote_action(0, 0), RemoteAction::Fetch);
        assert_eq!(remote_action(2, 0), RemoteAction::Push(2));
        assert_eq!(remote_action(0, 3), RemoteAction::Pull(3));
        // Behind wins: a push is rejected until the branch has pulled.
        assert_eq!(remote_action(2, 3), RemoteAction::Pull(3));
        assert_eq!(RemoteAction::Pull(3).label(), "Pull 3");
        assert_eq!(RemoteAction::Push(2).label(), "Push 2");
        assert_eq!(RemoteAction::Fetch.label(), "Fetch");
    }

    #[test]
    fn ahead_behind_is_formatted_in_korean() {
        assert_eq!(ahead_behind_text(0, 0), "원격과 같습니다");
        assert_eq!(ahead_behind_text(2, 0), "앞섬 2");
        assert_eq!(ahead_behind_text(0, 1), "뒤처짐 1");
        assert_eq!(ahead_behind_text(2, 1), "앞섬 2 \u{00b7} 뒤처짐 1");
    }

    #[test]
    fn the_last_fetch_is_relative() {
        assert_eq!(fetched_text(None), "아직 fetch하지 않았습니다");
        assert_eq!(fetched_text(Some(5)), "방금 fetch했습니다");
        assert_eq!(fetched_text(Some(60)), "1분 전에 fetch했습니다");
        assert_eq!(fetched_text(Some(7_300)), "2시간 전에 fetch했습니다");
        assert_eq!(fetched_text(Some(200_000)), "2일 전에 fetch했습니다");
    }

    #[test]
    fn the_branch_bar_splits_behind_and_ahead() {
        assert_eq!(bar_fractions(0, 0), (0.0, 0.0));
        assert_eq!(bar_fractions(1, 3), (0.75, 0.25));
        assert_eq!(bar_fractions(4, 0), (0.0, 1.0));
        assert_eq!(upstream_text(Some("origin/develop")), "origin/develop 추적");
        assert_eq!(upstream_text(None), "추적하는 원격 브랜치가 없습니다");
    }

    // ---- merge gate ----

    #[test]
    fn merge_needs_passing_ci_and_an_approval() {
        assert_eq!(merge_gate(CiState::Passed, ReviewState::Approved), Ok(()));
        assert_eq!(
            merge_gate(CiState::Failed, ReviewState::Approved),
            Err("CI가 실패했습니다")
        );
        assert_eq!(
            merge_gate(CiState::Running, ReviewState::Approved),
            Err("CI가 실행 중입니다")
        );
        assert_eq!(
            merge_gate(CiState::Waiting, ReviewState::Approved),
            Err("CI를 기다리는 중입니다")
        );
        assert_eq!(
            merge_gate(CiState::Passed, ReviewState::ChangesRequested),
            Err("변경 요청이 있습니다")
        );
        assert_eq!(
            merge_gate(CiState::Passed, ReviewState::Pending),
            Err("승인이 필요합니다")
        );
        assert_eq!(
            merge_gate(CiState::Passed, ReviewState::NoReviewer),
            Err("승인이 필요합니다")
        );
    }

    // ---- diff parsing ----

    const SAMPLE: &str = "\
diff --git a/src/a.rs b/src/a.rs
index 111..222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,4 +1,4 @@ fn main
 keep
-old one
-- dashes
+new one
+new two
 tail
@@ -20 +21,2 @@
 x
+y
\\ No newline at end of file
diff --git a/docs/new.md b/docs/new.md
new file mode 100644
--- /dev/null
+++ b/docs/new.md
@@ -0,0 +1,2 @@
+hello
+world
diff --git a/old.txt b/new.txt
similarity index 90%
rename from old.txt
rename to new.txt
";

    #[test]
    fn hunk_headers_parse_with_and_without_lengths() {
        assert_eq!(
            parse_hunk_header("@@ -1,4 +1,5 @@ fn main"),
            Some((1, 4, 1, 5))
        );
        assert_eq!(parse_hunk_header("@@ -20 +21,2 @@"), Some((20, 1, 21, 2)));
        assert_eq!(parse_hunk_header("@@ -0,0 +1,2 @@"), Some((0, 0, 1, 2)));
        assert_eq!(parse_hunk_header("not a header"), None);
        assert_eq!(parse_hunk_header("@@ -x +1 @@"), None);
    }

    #[test]
    fn a_unified_diff_becomes_files_hunks_and_lines() {
        let files = parse_unified(SAMPLE);
        assert_eq!(files.len(), 3);

        let a = &files[0];
        assert_eq!(a.path, "src/a.rs");
        assert_eq!(a.status, FileStatus::Modified);
        assert_eq!(a.hunks.len(), 2);
        assert_eq!((a.added(), a.removed()), (3, 2));
        let h = &a.hunks[0];
        assert_eq!(h.header, "@@ -1,4 +1,4 @@ fn main");
        assert_eq!(h.lines.len(), 6);
        assert_eq!(h.lines[0].kind, LineKind::Context);
        assert_eq!((h.lines[0].old_no, h.lines[0].new_no), (Some(1), Some(1)));
        assert_eq!(h.lines[1].kind, LineKind::Del);
        assert_eq!(h.lines[1].text, "old one");
        assert_eq!((h.lines[1].old_no, h.lines[1].new_no), (Some(2), None));
        // A deleted line that reads "-- dashes" is a line, not a file header.
        assert_eq!(h.lines[2].kind, LineKind::Del);
        assert_eq!(h.lines[2].text, "- dashes");
        assert_eq!((h.lines[3].old_no, h.lines[3].new_no), (None, Some(2)));
        assert_eq!((h.lines[5].old_no, h.lines[5].new_no), (Some(4), Some(4)));
        // The second hunk starts at its own numbers and skips the "No newline" marker.
        let h2 = &a.hunks[1];
        assert_eq!(h2.lines.len(), 2);
        assert_eq!(h2.lines[1].new_no, Some(22));
    }

    #[test]
    fn new_and_renamed_files_are_told_apart() {
        let files = parse_unified(SAMPLE);
        assert_eq!(files[1].path, "docs/new.md");
        assert_eq!(files[1].status, FileStatus::Added);
        assert_eq!((files[1].added(), files[1].removed()), (2, 0));
        assert_eq!(files[2].path, "new.txt");
        assert_eq!(files[2].old_path.as_deref(), Some("old.txt"));
        assert_eq!(files[2].status, FileStatus::Renamed);
        assert!(files[2].hunks.is_empty());
    }

    #[test]
    fn a_plain_diff_without_git_headers_parses_too() {
        let d = "--- a/x.txt\n+++ b/x.txt\n@@ -1 +1 @@\n-a\n+b\n--- a/y.txt\n+++ b/y.txt\n@@ -1 +1 @@\n-c\n+d\n";
        let files = parse_unified(d);
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["x.txt", "y.txt"]);
        assert_eq!(files[1].hunks[0].lines[1].text, "d");
    }

    #[test]
    fn binary_files_and_empty_input() {
        assert!(parse_unified("").is_empty());
        let files =
            parse_unified("diff --git a/i.png b/i.png\nBinary files a/i.png and b/i.png differ\n");
        assert_eq!(files.len(), 1);
        assert!(files[0].binary);
        assert!(files[0].hunks.is_empty());
    }

    #[test]
    fn status_letters() {
        assert_eq!(FileStatus::Modified.letter(), "M");
        assert_eq!(FileStatus::Added.letter(), "A");
        assert_eq!(FileStatus::Deleted.letter(), "D");
        assert_eq!(FileStatus::Renamed.letter(), "R");
    }

    // ---- staging ----

    fn a_diff() -> FileDiff {
        parse_unified(SAMPLE).remove(0)
    }

    #[test]
    fn a_line_toggles_and_a_context_line_does_not() {
        let d = a_diff();
        let mut st = FileStaging::none();
        assert_eq!(st.counts(&d), (0, 5));
        st.toggle_line(&d, 0, 1);
        assert!(st.is_staged(0, 1));
        assert_eq!(st.counts(&d), (1, 5));
        assert_eq!(st.check(&d), Check::Partial);
        st.toggle_line(&d, 0, 0);
        assert!(!st.is_staged(0, 0));
        st.toggle_line(&d, 9, 9);
        assert_eq!(st.counts(&d), (1, 5));
        st.toggle_line(&d, 0, 1);
        assert_eq!(st.check(&d), Check::Off);
    }

    #[test]
    fn a_hunk_stages_all_its_changes_then_unstages_them() {
        let d = a_diff();
        let mut st = FileStaging::none();
        st.toggle_line(&d, 0, 1);
        st.toggle_hunk(&d, 0);
        assert_eq!(st.hunk_check(&d, 0), Check::On);
        assert_eq!(st.hunk_check(&d, 1), Check::Off);
        assert_eq!(st.check(&d), Check::Partial);
        st.toggle_hunk(&d, 0);
        assert_eq!(st.hunk_check(&d, 0), Check::Off);
    }

    #[test]
    fn a_file_stages_all_lines_or_none() {
        let d = a_diff();
        let mut all = Staging::new(std::slice::from_ref(&d), false);
        assert_eq!(all.check(&d), Check::Off);
        all.toggle_file(&d);
        assert_eq!(all.check(&d), Check::On);
        assert_eq!(all.file(&d.path).unwrap().counts(&d), (5, 5));
        // A partly staged file stages everything on the next click.
        all.file_mut(&d.path).toggle_line(&d, 0, 1);
        assert_eq!(all.check(&d), Check::Partial);
        all.toggle_file(&d);
        assert_eq!(all.check(&d), Check::On);
        all.toggle_file(&d);
        assert_eq!(all.check(&d), Check::Off);
    }

    #[test]
    fn the_all_check_and_the_staged_file_count() {
        let diffs = parse_unified(SAMPLE);
        let mut st = Staging::new(&diffs, false);
        assert_eq!(st.staged_files(&diffs), 0);
        st.toggle_all(&diffs);
        assert_eq!(st.staged_files(&diffs), 3 - 1); // the rename has no lines to stage
        assert!(diffs.iter().take(2).all(|d| st.check(d) == Check::On));
        st.toggle_all(&diffs);
        assert_eq!(st.staged_files(&diffs), 0);
        assert_eq!(Staging::new(&diffs, true).staged_files(&diffs), 2);
        assert_eq!(Staging::default().check(&diffs[0]), Check::Off);
    }

    #[test]
    fn tri_state_checks() {
        assert_eq!(check_of(0, 0), Check::Off);
        assert_eq!(check_of(0, 3), Check::Off);
        assert_eq!(check_of(2, 3), Check::Partial);
        assert_eq!(check_of(3, 3), Check::On);
        assert_eq!(folder_check(&[]), Check::Off);
        assert_eq!(folder_check(&[Check::On, Check::On]), Check::On);
        assert_eq!(folder_check(&[Check::Off, Check::Off]), Check::Off);
        assert_eq!(folder_check(&[Check::On, Check::Off]), Check::Partial);
        assert_eq!(folder_check(&[Check::Partial]), Check::Partial);
    }

    #[test]
    fn the_soon_line_picks_the_topic_particle() {
        assert_eq!(soon_line("브랜치"), "브랜치는 준비 중입니다");
        assert_eq!(soon_line("브랜치 목록"), "브랜치 목록은 준비 중입니다");
        assert_eq!(soon_line("커밋"), "커밋은 준비 중입니다");
        assert_eq!(soon_line("저장소 정보"), "저장소 정보는 준비 중입니다");
    }

    #[test]
    fn check_glyphs_differ() {
        let g = [Check::Off, Check::Partial, Check::On].map(check_glyph);
        assert!(g[0] != g[1] && g[1] != g[2] && g[0] != g[2]);
    }

    #[test]
    fn the_commit_button_needs_a_summary_and_a_staged_file() {
        assert!(!commit_enabled("", 2, false));
        assert!(!commit_enabled("  ", 2, false));
        assert!(!commit_enabled("Feat: x", 0, false));
        assert!(commit_enabled("Feat: x", 1, false));
        assert!(commit_enabled("", 0, true));
        assert_eq!(commit_label("main", 0), "main에 커밋");
        assert_eq!(commit_label("main", 2), "main에 커밋 (2개 파일)");
    }

    // ---- file tree ----

    #[test]
    fn single_child_folders_merge_and_folders_fold() {
        let paths = s(&[
            "ember/app/src/a.rs",
            "ember/app/src/b.rs",
            "README.md",
            "docs/x.md",
        ]);
        let rows = build_tree(&paths, &BTreeSet::new());
        assert_eq!(
            rows,
            vec![
                TreeRow::Dir {
                    label: "docs".into(),
                    path: "docs/".into(),
                    depth: 0,
                    open: true
                },
                TreeRow::File {
                    index: 3,
                    name: "x.md".into(),
                    depth: 1
                },
                TreeRow::Dir {
                    label: "ember/app/src".into(),
                    path: "ember/app/src/".into(),
                    depth: 0,
                    open: true
                },
                TreeRow::File {
                    index: 0,
                    name: "a.rs".into(),
                    depth: 1
                },
                TreeRow::File {
                    index: 1,
                    name: "b.rs".into(),
                    depth: 1
                },
                TreeRow::File {
                    index: 2,
                    name: "README.md".into(),
                    depth: 0
                },
            ]
        );
        let folded: BTreeSet<String> = ["ember/app/src/".to_string()].into();
        let rows = build_tree(&paths, &folded);
        assert_eq!(rows.len(), 4);
        assert!(matches!(&rows[2], TreeRow::Dir { open: false, .. }));
    }

    #[test]
    fn a_folder_with_files_and_folders_is_not_merged() {
        let paths = s(&["a/x.rs", "a/b/y.rs"]);
        let rows = build_tree(&paths, &BTreeSet::new());
        assert!(matches!(&rows[0], TreeRow::Dir { label, depth: 0, .. } if label == "a"));
        assert!(matches!(&rows[1], TreeRow::Dir { label, depth: 1, .. } if label == "b"));
        assert!(build_tree(&[], &BTreeSet::new()).is_empty());
    }

    // ---- data availability ----

    fn boxed(id: &str) -> ProjectBox {
        ProjectBox {
            id: id.into(),
            name: id.into(),
            members: Vec::new(),
            running: 0,
            waiting: 0,
            computers: Vec::new(),
            top: None,
            repo: None,
            branch: None,
        }
    }

    #[test]
    fn the_default_project_has_no_repository() {
        let boxes = vec![boxed("unsorted"), boxed("ember"), boxed("core")];
        assert_eq!(repo_projects(&boxes).len(), 2);
        assert_eq!(pick_project(Some("core"), &boxes).as_deref(), Some("core"));
        assert_eq!(
            pick_project(Some("unsorted"), &boxes).as_deref(),
            Some("ember")
        );
        assert_eq!(pick_project(None, &boxes).as_deref(), Some("ember"));
        assert_eq!(pick_project(None, &[boxed("unsorted")]), None);
    }

    #[test]
    fn no_repository_data_is_invented() {
        for sec in Section::ALL {
            assert_eq!(section_count(sec, None), None);
        }
        let snap = RepoSnapshot {
            changes: vec![a_diff()],
            branches: s(&["main", "develop"]),
            ..Default::default()
        };
        assert_eq!(section_count(Section::Changes, Some(&snap)), Some(1));
        assert_eq!(section_count(Section::Branches, Some(&snap)), Some(2));
        assert_eq!(section_count(Section::Stash, Some(&snap)), None);
        assert_eq!(SOON, "준비 중입니다");
    }

    // ---- NFR-L4 ----

    #[test]
    fn nfr_l4_texts_end_in_hamnida_form() {
        let mut texts = vec![
            ahead_behind_text(0, 0),
            fetched_text(None),
            fetched_text(Some(1)),
            fetched_text(Some(90)),
            upstream_text(None),
            commit_label("main", 1),
        ];
        for (ci, rv) in [
            (CiState::Failed, ReviewState::Approved),
            (CiState::Running, ReviewState::Approved),
            (CiState::Waiting, ReviewState::Approved),
            (CiState::Passed, ReviewState::ChangesRequested),
            (CiState::Passed, ReviewState::Pending),
        ] {
            texts.push(merge_gate(ci, rv).unwrap_err().to_string());
        }
        for t in &texts {
            assert!(
                !t.ends_with('다') || t.ends_with("니다"),
                "plain ending: {t}"
            );
        }
    }

    // ---- git data (FR-G4) ----

    use ember_client::api::GitFile;

    fn gf(path: &str, index: char, worktree: char) -> GitFile {
        GitFile { path: path.into(), old_path: None, index, worktree }
    }

    fn status(files: Vec<GitFile>) -> GitStatus {
        GitStatus {
            branch: "feat/x".into(),
            upstream: Some("origin/feat/x".into()),
            ahead: 2,
            behind: 1,
            fetched_age_secs: Some(60),
            branches: s(&["main", "feat/x"]),
            files,
        }
    }

    #[test]
    fn a_missing_repository_or_server_is_not_data() {
        let not_repo = ApiError::Status { status: 404, message: "not_a_repo".into() };
        assert_eq!(load_from(Err(not_repo)), Load::NotRepo);
        let down = ApiError::Peer("refused".into());
        assert!(matches!(load_from(Err(down)), Load::Unavailable(_)));
        let conflict = ApiError::Status { status: 500, message: "boom".into() };
        assert!(matches!(load_from(Err(conflict)), Load::Unavailable(_)));
        assert!(Load::Loading.snapshot().is_none());
        assert_eq!(Load::Unavailable("x".into()).note(), Some(soon_line("저장소 정보")));
        assert!(Load::NotRepo.note().unwrap().contains("저장소가 아닙니다"));
        assert_eq!(Load::Ready(RepoSnapshot::default()).note(), None);
        assert_eq!(SOON, "준비 중입니다");
    }

    #[test]
    fn porcelain_characters_map_to_file_statuses() {
        assert_eq!(status_of('M', '.'), FileStatus::Modified);
        assert_eq!(status_of('.', 'M'), FileStatus::Modified);
        assert_eq!(status_of('A', '.'), FileStatus::Added);
        assert_eq!(status_of('?', '?'), FileStatus::Added);
        assert_eq!(status_of('D', '.'), FileStatus::Deleted);
        assert_eq!(status_of('.', 'D'), FileStatus::Deleted);
        assert_eq!(status_of('R', '.'), FileStatus::Renamed);
    }

    #[test]
    fn the_snapshot_joins_status_with_its_diffs() {
        let st = status(vec![
            gf("src/a.rs", '.', 'M'),
            gf("docs/new.md", 'A', '.'),
            gf("notes.txt", '?', '?'),
        ]);
        let untracked_patch = "diff --git a/notes.txt b/notes.txt\nnew file mode 100644\n--- /dev/null\n+++ b/notes.txt\n@@ -0,0 +1 @@\n+hi\n";
        let mut untracked = BTreeMap::new();
        untracked.insert("notes.txt".to_string(), untracked_patch.to_string());
        assert_eq!(untracked_paths(&st), s(&["notes.txt"]));

        // SAMPLE holds src/a.rs (work tree) and docs/new.md (also used as the staged diff).
        let snap = build_snapshot(&st, SAMPLE, SAMPLE, &untracked);
        assert_eq!(snap.branch, "feat/x");
        assert_eq!((snap.ahead, snap.behind), (2, 1));
        assert_eq!(snap.branches, s(&["main", "feat/x"]));
        let paths: Vec<_> = snap.changes.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, ["src/a.rs", "docs/new.md", "notes.txt"]);
        assert_eq!(snap.changes[0].hunks.len(), 2);
        assert_eq!(snap.changes[1].status, FileStatus::Added);
        assert_eq!(snap.changes[2].added(), 1);
        assert_eq!(snap.changes[2].status, FileStatus::Added);
        // Only the file whose index changed and work tree is clean starts staged.
        assert_eq!(snap.staged, ["docs/new.md".to_string()].into_iter().collect());
    }

    #[test]
    fn a_file_without_a_diff_stays_listed_and_a_rename_keeps_its_old_path() {
        let mut renamed = gf("new.txt", 'R', '.');
        renamed.old_path = Some("old.txt".into());
        let st = status(vec![gf("gone.bin", '.', 'M'), renamed]);
        let snap = build_snapshot(&st, "", "", &BTreeMap::new());
        assert_eq!(snap.changes.len(), 2);
        assert!(snap.changes[0].hunks.is_empty());
        assert_eq!(snap.changes[1].status, FileStatus::Renamed);
        assert_eq!(snap.changes[1].old_path.as_deref(), Some("old.txt"));
    }

    #[test]
    fn the_patch_holds_only_the_staged_lines() {
        let d = a_diff();
        let mut st = FileStaging::none();
        assert_eq!(build_patch(&d, &st), None);
        // Only "old one" (hunk 0, line 1): the other lines turn into context or vanish.
        st.toggle_line(&d, 0, 1);
        assert_eq!(
            build_patch(&d, &st).unwrap(),
            "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n\
             @@ -1,4 +1,3 @@\n keep\n-old one\n - dashes\n tail\n"
        );
        // A second hunk lands one line earlier after the first removed a line.
        st.toggle_line(&d, 1, 1);
        let patch = build_patch(&d, &st).unwrap();
        assert!(patch.ends_with("@@ -20,1 +19,2 @@\n x\n+y\n"), "{patch}");
    }

    #[test]
    fn staging_only_the_second_hunk_skips_the_first() {
        let d = a_diff();
        let mut st = FileStaging::none();
        st.toggle_hunk(&d, 1);
        let patch = build_patch(&d, &st).unwrap();
        assert!(!patch.contains("keep"));
        assert!(patch.contains("@@ -20,1 +20,2 @@\n x\n+y\n"), "{patch}");
    }

    fn snap_of(files: Vec<GitFile>) -> RepoSnapshot {
        build_snapshot(&status(files), SAMPLE, "", &BTreeMap::new())
    }

    #[test]
    fn a_fully_checked_file_is_staged_whole() {
        let snap = snap_of(vec![gf("src/a.rs", '.', 'M')]);
        let mut st = Staging::new(&snap.changes, false);
        assert_eq!(plan_staging(&snap, &st), vec![]);
        st.toggle_file(&snap.changes[0]);
        assert_eq!(plan_staging(&snap, &st), vec![StageOp::Stage(s(&["src/a.rs"]))]);
    }

    #[test]
    fn a_partly_checked_file_goes_as_a_patch() {
        let snap = snap_of(vec![gf("src/a.rs", '.', 'M')]);
        let mut st = Staging::new(&snap.changes, false);
        st.file_mut("src/a.rs").toggle_line(&snap.changes[0], 0, 3);
        let ops = plan_staging(&snap, &st);
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            StageOp::Apply(p) => assert!(p.contains("+new one\n") && !p.contains("+new two")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_file_the_index_holds_is_not_staged_again_and_is_unstaged_when_unchecked() {
        let mut snap = snap_of(vec![gf("src/a.rs", 'M', '.')]);
        snap.staged.insert("src/a.rs".to_string());
        let mut st = Staging::new(&snap.changes, false);
        st.file_mut("src/a.rs").set_all(&snap.changes[0], true);
        assert_eq!(plan_staging(&snap, &st), vec![]);
        st.file_mut("src/a.rs").set_all(&snap.changes[0], false);
        assert_eq!(plan_staging(&snap, &st), vec![StageOp::Unstage(s(&["src/a.rs"]))]);
    }

    #[test]
    fn unstaging_comes_before_staging() {
        let mut snap = snap_of(vec![gf("src/a.rs", '.', 'M'), gf("docs/new.md", 'A', '.')]);
        snap.staged.insert("docs/new.md".to_string());
        let mut st = Staging::new(&snap.changes, false);
        st.toggle_file(&snap.changes[0]);
        let ops = plan_staging(&snap, &st);
        assert_eq!(
            ops,
            vec![
                StageOp::Unstage(s(&["docs/new.md"])),
                StageOp::Stage(s(&["src/a.rs"])),
            ]
        );
    }

    #[test]
    fn notice_texts_are_korean_and_carry_git_s_message() {
        assert_eq!(commit_done_text("abcdef0123"), "커밋했습니다 (abcdef0)");
        assert_eq!(
            remote_done_text(RemoteAction::Pull(1), "\n Already up to date.\n"),
            "Pull 완료: Already up to date."
        );
        assert_eq!(remote_done_text(RemoteAction::Fetch, ""), "Fetch을 마쳤습니다");
        let e = ApiError::Status { status: 409, message: "not fast-forward".into() };
        assert_eq!(failure_text("Pull", &e), "Pull하지 못했습니다: not fast-forward");
        let down = ApiError::Peer("refused".into());
        assert!(failure_text("Push", &down).contains("서버에 닿지 않습니다"));
    }

    #[test]
    fn a_new_branch_name_must_be_one_git_accepts() {
        assert!(valid_branch_name("feat/git-api"));
        assert!(valid_branch_name("  fix_1 "));
        for bad in ["", "  ", "a b", "-x", "a..b", "a~1", "a:b", "x/", "/x", "x.", "a?b", "a[b"] {
            assert!(!valid_branch_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_demo_answer_maps_to_the_same_load_states() {
        let not_repo = GitError { status: 404, code: "not_a_repo".into(), message: "no".into() };
        let r: Result<GitStatus, ApiError> = git_result(Err(not_repo));
        let snap = r.map(|_| RepoSnapshot::default());
        assert_eq!(load_from(snap), Load::NotRepo);

        let st: GitStatus = git_result(Ok(json!({ "branch": "main", "files": [] }))).unwrap();
        assert_eq!(st.branch, "main");
        // A 204 operation answers null and is read as a plain value.
        assert_eq!(git_result::<Value>(Ok(Value::Null)).unwrap(), Value::Null);
        let c: GitCommitted = git_result(Ok(json!({ "sha": "abc" }))).unwrap();
        assert_eq!(c.sha, "abc");
        let bad: Result<GitStatus, _> = git_result(Ok(json!({ "nope": 1 })));
        assert!(matches!(bad, Err(ApiError::Decode(_))));
        let conflict = GitError { status: 409, code: "conflict".into(), message: "diverged".into() };
        let e = git_result::<Value>(Err(conflict)).unwrap_err();
        assert_eq!(e.status(), Some(409));
        assert!(e.to_string().contains("diverged"));
    }
}
