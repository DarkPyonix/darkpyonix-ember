//! Data and action logic for 버전 관리 (FR-L19, INTENT D34, `docs/DESIGN.md` section 6.6).
//!
//! The decisions are plain functions and types in the first half of this file, unit-tested
//! without a renderer: section routing, the compact tabs, the remote action and ahead/behind
//! text, the merge gate, unified diff parsing, line, hunk and file staging, and the file tree.
//! The dioxus-compose screen lives in `ui/cr/vcs.rs`.
//!
//! The data comes from the server's `/api/projects/{name}/git/{op}` routes (FR-G4) through
//! `ember_client::Api::git_*`: [`fetch_snapshot`] reads it, [`build_snapshot`] maps status and
//! unified diffs into [`FileDiff`]s, and [`plan_staging`] and [`build_patch`] turn the line
//! staging into server calls. The data views say [`SOON`] when the server is unreachable or a
//! section is not built. A project that is not a repository shows "이 프로젝트의 경로는 git
//! 저장소가 아닙니다". Sections beyond changes and branches still wait for their endpoints (#168).
//!
//! What the pinned toolkit cannot do yet, with the workaround used (`GAPS-WORKLIST.md`):
//! - no clickable Row or Column: a text Button is the hit target of a row or a diff line (TODO(compose-rust#372): drop once Row/Column on_click is confirmed);
//! - no sticky column: the line check sits in the row and scrolls with it (TODO(compose-rust#424));
//! - no SVG graph: the history is a list first (TODO(compose-rust#308): draw the graph on a `Canvas`).

use std::collections::{BTreeMap, BTreeSet};

use dioxus_compose::prelude::WindowSizeClass;

use ember_client::api::{
    ApiError, GitBranchInfo, GitCommitApplied, GitCommitDetail, GitMergeState, GitCommitFileStatus, GitBranches, GitCommitted, GitConflict, GitConflictBlock, GitConflicts, GitDiff, GitLog, GitLogCommit, GitLogRefKind, GitOutput, GitPr, GitPrCheckState, GitPrCreated, GitPrDetail, GitPrMergeMethod, GitPrReview, GitPrReviewAction, GitPrReviewItem, GitPrReviewState, GitPrState, GitPrs, GitStashEntry, GitStatus,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::demo::GitError;
use crate::services::{run, services};
use crate::ui::project::{is_default_project, ProjectBox};

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
    /// A `\ No newline at end of file` marker followed this line.
    pub no_eol: bool,
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
    /// The file mode from `new file mode`, `old mode` or `new mode`; `None` when the diff has none.
    pub mode: Option<String>,
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

/// Read one C-quoted string (git quotes a path with a quote, a backslash, a control character or,
/// unless `core.quotepath=false`, a non-ASCII byte) from the start of `s`. Returns the decoded text
/// and what follows the closing quote. `None` when `s` does not start with a complete quoted string.
fn unquote_c(s: &str) -> Option<(String, &str)> {
    let body = s.strip_prefix('"')?;
    let bytes = body.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Some((String::from_utf8_lossy(&out).into_owned(), &body[i + 1..])),
            b'\\' => {
                i += 1;
                let c = *bytes.get(i)?;
                match c {
                    b'0'..=b'7' => {
                        let mut v = 0u32;
                        let mut n = 0;
                        while n < 3 && i < bytes.len() && (b'0'..=b'7').contains(&bytes[i]) {
                            v = v * 8 + u32::from(bytes[i] - b'0');
                            i += 1;
                            n += 1;
                        }
                        out.push((v & 0xff) as u8);
                        continue;
                    }
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    b'a' => out.push(7),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'v' => out.push(11),
                    other => out.push(other),
                }
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    None
}

/// A path as it follows `--- `, `+++ `, `rename from ` or `rename to `: quoted or plain. A plain
/// path with spaces carries a trailing tab in `---`/`+++` lines, which is not part of the name.
fn path_field(s: &str) -> String {
    match unquote_c(s) {
        Some((p, _)) => p,
        None => s.trim_end_matches('\t').to_string(),
    }
}

/// The new-side path of a `diff --git <a> <b>` header (the text after `diff --git `). Handles
/// quoted names on either side, and unquoted names with spaces.
fn git_header_path(rest: &str) -> String {
    if rest.starts_with('"') {
        if let Some((_, tail)) = unquote_c(rest) {
            let tail = tail.trim_start();
            return strip_side(&path_field(tail)).to_string();
        }
    }
    if let Some(i) = rest.rfind(" \"") {
        if let Some((b, _)) = unquote_c(&rest[i + 1..]) {
            return strip_side(&b).to_string();
        }
    }
    // Same name on both sides: `a/P b/P` is 2L + 5 bytes long.
    if rest.starts_with("a/") && rest.len() >= 5 && (rest.len() - 5) % 2 == 0 {
        let l = (rest.len() - 5) / 2;
        if rest.is_char_boundary(2 + l) && rest[2 + l..].starts_with(" b/") && rest[2..2 + l] == rest[2 + l + 3..] {
            return rest[2 + l + 3..].to_string();
        }
    }
    rest.rsplit_once(" b/")
        .map(|(_, b)| b.to_string())
        .unwrap_or_else(|| strip_side(rest).to_string())
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
            mode: None,
            hunks: Vec::new(),
        }
    }
    fn mark_no_eol(open: &mut Option<Hunk>) {
        if let Some(l) = open.as_mut().and_then(|h| h.lines.last_mut()) {
            l.no_eol = true;
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
                Some('\\') => {
                    mark_no_eol(&mut open);
                    continue;
                }
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
                    no_eol: false,
                });
            }
            continue;
        }
        if line.starts_with('\\') {
            mark_no_eol(&mut open);
            continue;
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            close(&mut cur, &mut open);
            files.extend(cur.take());
            cur = Some(blank(&git_header_path(rest)));
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
                    let old = path_field(p);
                    f.old_path = Some(strip_side(&old).to_string());
                    if f.path.is_empty() {
                        f.path = strip_side(&old).to_string();
                    }
                }
            }
        } else if let Some(p) = line.strip_prefix("+++ ") {
            if let Some(f) = cur.as_mut() {
                if p != "/dev/null" {
                    f.path = strip_side(&path_field(p)).to_string();
                }
            }
        } else if let Some(f) = cur.as_mut() {
            if let Some(m) = line.strip_prefix("new file mode ") {
                f.status = FileStatus::Added;
                f.mode = Some(m.trim().to_string());
            } else if let Some(m) = line.strip_prefix("deleted file mode ") {
                f.status = FileStatus::Deleted;
                f.mode = Some(m.trim().to_string());
            } else if let Some(m) = line.strip_prefix("old mode ") {
                f.mode.get_or_insert_with(|| m.trim().to_string());
            } else if let Some(m) = line.strip_prefix("new mode ") {
                f.mode = Some(m.trim().to_string());
            } else if let Some(from) = line.strip_prefix("rename from ") {
                f.status = FileStatus::Renamed;
                f.old_path = Some(path_field(from));
            } else if let Some(to) = line.strip_prefix("rename to ") {
                f.path = path_field(to);
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
    /// Paths with unresolved merge conflicts (`git/conflicts`).
    pub conflicts: Vec<String>,
    /// Korean lines under the bar about what the snapshot left out (#268).
    pub notes: Vec<String>,
}

/// The state of the repository view of one project.
#[derive(Debug, Clone, PartialEq)]
pub enum Load {
    Loading,
    Ready(RepoSnapshot),
    /// The project's path is not a repository (the server answered 404).
    NotRepo,
    /// The server could not be reached; the text says why.
    Unavailable(String),
    /// The server answered with an error (not 404): the Korean failure line, with the node's
    /// message. The screen shows it with a retry button.
    Failed(String),
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
            Load::Failed(text) => Some(text.clone()),
        }
    }
}

/// A server answer as a view state: 404 means not a repository, an unreachable server means the
/// data is unavailable (준비 중입니다), and any other answer is a failure that shows the node's
/// message. The node's `not_a_repo` code does not reach [`ApiError`] (only its message does), so
/// every 404 of a git route reads as "not a repository".
pub fn load_from(result: Result<RepoSnapshot, ApiError>) -> Load {
    match result {
        Ok(s) => Load::Ready(s),
        Err(e) if e.status() == Some(404) => Load::NotRepo,
        Err(e @ (ApiError::Peer(_) | ApiError::Transport(_) | ApiError::Push(_) | ApiError::BadUrl(_))) => {
            Load::Unavailable(e.to_string())
        }
        Err(e) => Load::Failed(failure_text("저장소 정보를 확인", &e)),
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
                mode: None,
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
        conflicts: Vec::new(),
        notes: Vec::new(),
    }
}

/// A demo source answer as an API result: the status and message of its error become an
/// [`ApiError::Status`], so the demo and the server share the same load states.
pub fn git_result<T: DeserializeOwned>(r: Result<Value, GitError>) -> Result<T, ApiError> {
    let v = r.map_err(|e| ApiError::Status { status: e.status, message: format!("{}: {}", e.code, e.message), code: None })?;
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

/// The `git/diff` body of the whole work tree or the index. `include_untracked` makes the node
/// add the untracked files to the one patch (only meaningful with `staged: false`).
pub fn diff_body(staged: bool, include_untracked: bool) -> Value {
    json!({ "path": null, "staged": staged, "include_untracked": include_untracked })
}

async fn git_diff_of(project: &str, staged: bool) -> Result<String, ApiError> {
    git_diff_with(project, staged, !staged).await
}

async fn git_diff_with(project: &str, staged: bool, include_untracked: bool) -> Result<String, ApiError> {
    let d: GitDiff = git_call(project, "diff", diff_body(staged, include_untracked)).await?;
    Ok(d.patch)
}

/// The line shown when the untracked files' diffs were skipped (#268).
pub const UNTRACKED_SKIPPED_NOTE: &str = "추적하지 않는 파일이 너무 많아 내용을 건너뛰었습니다. 파일 목록만 표시합니다.";

/// The work tree diff from its two reads: `tracked` (tracked files only) and `with_untracked`
/// (the untracked files' diffs added). A 413 (output cap) on the second keeps the first and
/// returns the note; the untracked files stay listed by the status, without hunks. Any other
/// error of either read fails as before.
pub fn merge_untracked_diff(
    tracked: Result<String, ApiError>,
    with_untracked: Result<String, ApiError>,
) -> Result<(String, Option<&'static str>), ApiError> {
    match with_untracked {
        Ok(patch) => Ok((patch, None)),
        Err(e) if e.status() == Some(413) => Ok((tracked?, Some(UNTRACKED_SKIPPED_NOTE))),
        Err(e) => Err(e),
    }
}

/// The warning under the amend checkbox when HEAD is already on the upstream (#269): there is an
/// upstream and the branch is not ahead of it. Limits: a detached HEAD has no upstream, and a
/// branch whose upstream is gone has none either, so neither warns although the commit may be
/// pushed.
pub fn amend_warning(snap: &RepoSnapshot) -> Option<&'static str> {
    if snap.upstream.is_some() && snap.ahead == 0 {
        Some("이 커밋은 이미 원격에 올라가 있습니다. 수정하면 다음 올리기에서 기록을 덮어써야 합니다.")
    } else {
        None
    }
}

/// What a tap on the commit button does while amending a pushed commit (#269): the first tap arms
/// it, and a second tap counts only after the "그래도 수정합니다" label has had time to show, so a
/// fast double click cannot skip the confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmendTap {
    Arm,
    Wait,
    Commit,
}

/// Milliseconds between arming and the second tap that is accepted.
pub const AMEND_CONFIRM_DELAY_MS: i64 = 600;

/// `armed_at` is the time of the first tap (0 when not armed).
pub fn amend_tap(needs_confirm: bool, armed_at: i64, now: i64) -> AmendTap {
    if !needs_confirm {
        AmendTap::Commit
    } else if armed_at == 0 {
        AmendTap::Arm
    } else if now - armed_at < AMEND_CONFIRM_DELAY_MS {
        AmendTap::Wait
    } else {
        AmendTap::Commit
    }
}

/// The commit button's label: the armed second tap of a pushed amend reads "그래도 수정합니다".
pub fn amend_button_label(base: &str, warning: Option<&str>, armed: bool) -> String {
    if warning.is_some() && armed { "그래도 수정합니다".to_string() } else { base.to_string() }
}

/// Read a project's repository: status, the work tree diff (untracked files included, in one
/// call), the staged diff and the conflicts. Runs on the runtime, not the UI thread.
pub async fn fetch_snapshot(project: &str) -> Result<RepoSnapshot, ApiError> {
    let status: GitStatus = git_call(project, "status", json!({})).await?;
    let conflicts = match git_call::<GitConflicts>(project, "conflicts", json!({})).await {
        Ok(c) => c.paths,
        // The conflict list is secondary: a failure here leaves the section empty, not the screen.
        Err(e) => {
            tracing::warn!("reading the conflicts failed: {e}");
            Vec::new()
        }
    };
    let full = git_diff_with(project, false, true).await;
    // The tracked-only read happens only after a 413, so the usual path stays one call.
    let tracked = if matches!(&full, Err(e) if e.status() == Some(413)) {
        git_diff_with(project, false, false).await
    } else {
        Ok(String::new())
    };
    let (unstaged, note) = merge_untracked_diff(tracked, full)?;
    let staged = git_diff_of(project, true).await?;
    let mut snap = build_snapshot(&status, &unstaged, &staged, &BTreeMap::new());
    snap.conflicts = conflicts;
    snap.notes.extend(note.map(str::to_string));
    Ok(snap)
}

// ---------------------------------------------------------------------------------------------
// History (FR-G5)
// ---------------------------------------------------------------------------------------------

/// Which commits the history list displays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HistoryFilter {
    #[default]
    AllBranches,
    LocalOnly,
}

impl HistoryFilter {
    pub const ALL: [HistoryFilter; 2] = [HistoryFilter::AllBranches, HistoryFilter::LocalOnly];

    pub fn label(self) -> &'static str {
        match self {
            HistoryFilter::AllBranches => "모든 브랜치",
            HistoryFilter::LocalOnly => "로컬 전용",
        }
    }
}

/// Commits read per page of the history list.
pub const HISTORY_PAGE: u32 = 30;

/// A ref chip beside a commit: the short name and what it points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRef {
    pub label: String,
    pub kind: GitLogRefKind,
}

/// One row of the history list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub sha: String,
    /// The first 7 characters of the sha.
    pub short: String,
    pub subject: String,
    pub author: String,
    pub time_text: String,
    pub refs: Vec<HistoryRef>,
    /// A commit with two or more parents.
    pub is_merge: bool,
    /// A local commit not yet pushed to the remote.
    pub unpushed: bool,
    /// A commit on a remote ref not present on local branches.
    pub remote_only: bool,
}

/// The chip text of a ref: the node sends full names (`refs/heads/main`), the chip shows `main`.
pub fn ref_label(name: &str) -> String {
    for prefix in ["refs/heads/", "refs/remotes/", "refs/tags/"] {
        if let Some(short) = name.strip_prefix(prefix) {
            return short.to_string();
        }
    }
    name.to_string()
}

/// "방금 전", "N분 전", "N시간 전", "N일 전" for a commit time, relative to `now` (both unix
/// seconds). A time in the future reads as "방금 전".
pub fn history_time_text(now: i64, time: i64) -> String {
    let age = (now - time).max(0);
    match age {
        s if s < 60 => "방금 전".to_string(),
        s if s < 3_600 => format!("{}분 전", s / 60),
        s if s < 86_400 => format!("{}시간 전", s / 3_600),
        s if s < 86_400 * 30 => format!("{}일 전", s / 86_400),
        s if s < 86_400 * 365 => format!("{}개월 전", s / (86_400 * 30)),
        s => format!("{}년 전", s / (86_400 * 365)),
    }
}

/// The list row of one commit. Head and branches come first among the chips, then remotes, then
/// tags, so the chips read in a steady order.
pub fn history_entry(c: &GitLogCommit, now: i64) -> HistoryEntry {
    let rank = |k: &GitLogRefKind| match k {
        GitLogRefKind::Head => 0,
        GitLogRefKind::Branch => 1,
        GitLogRefKind::Remote => 2,
        GitLogRefKind::Tag => 3,
    };
    let mut refs: Vec<HistoryRef> = c.refs.iter().map(|r| HistoryRef { label: ref_label(&r.name), kind: r.kind.clone() }).collect();
    refs.sort_by_key(|r| rank(&r.kind));
    HistoryEntry {
        sha: c.sha.clone(),
        short: c.sha.chars().take(7).collect(),
        subject: c.subject.clone(),
        author: c.author.clone(),
        time_text: history_time_text(now, c.time),
        refs,
        is_merge: c.parents.len() > 1,
        unpushed: false,
        remote_only: false,
    }
}

/// Annotate a sequence of history commits with `unpushed` and `remote_only` flags.
pub fn annotate_history_sync(entries: &mut [HistoryEntry]) {
    let mut past_remote = false;
    for e in entries.iter_mut() {
        let has_remote = e.refs.iter().any(|r| r.kind == GitLogRefKind::Remote);
        let has_local = e.refs.iter().any(|r| r.kind == GitLogRefKind::Branch || r.kind == GitLogRefKind::Head);

        if has_remote {
            past_remote = true;
        }

        if e.unpushed || (!past_remote && !has_remote) {
            e.unpushed = true;
        }
        if e.remote_only || (has_remote && !has_local) {
            e.remote_only = true;
        }
    }
}

/// Whether a commit matches the selected history filter.
pub fn history_matches_filter(entry: &HistoryEntry, filter: HistoryFilter) -> bool {
    match filter {
        HistoryFilter::AllBranches => true,
        HistoryFilter::LocalOnly => !entry.remote_only,
    }
}

/// Filter history entries according to the chosen filter.
pub fn filter_history_entries(entries: &[HistoryEntry], filter: HistoryFilter) -> Vec<HistoryEntry> {
    entries.iter().filter(|e| history_matches_filter(e, filter)).cloned().collect()
}

/// The rows of one page of the log, in the node's order (newest first).
pub fn history_entries(log: &GitLog, now: i64) -> Vec<HistoryEntry> {
    let mut entries: Vec<HistoryEntry> = log.commits.iter().map(|c| history_entry(c, now)).collect();
    annotate_history_sync(&mut entries);
    entries
}

/// Whether another page may exist: the last page came back full. An empty log (a branch with no
/// commit yet) and a short page both mean the end.
pub fn history_may_have_more(page_len: usize) -> bool {
    page_len >= HISTORY_PAGE as usize
}

/// The `git/log` body for the page starting after `skip` commits (the current branch).
pub fn log_body(skip: u64) -> Value {
    json!({ "limit": HISTORY_PAGE, "skip": skip, "branch": null })
}

/// Read one page of a project's history. Runs on the runtime, not the UI thread.
pub async fn fetch_history(project: &str, skip: u64) -> Result<GitLog, ApiError> {
    git_call(project, "log", log_body(skip)).await
}

/// The state of the history list of one project.
#[derive(Debug, Clone, PartialEq)]
pub enum HistoryLoad {
    Loading,
    /// The pages read so far; `more` is whether another page may exist. `loading_more` is set
    /// while the next page is on its way.
    Ready { entries: Vec<HistoryEntry>, more: bool, loading_more: bool },
    /// The server answered with an error: the Korean line with the node's message (shown with a
    /// retry button).
    Failed(String),
}

/// A first page as a view state.
pub fn history_from(result: Result<GitLog, ApiError>, now: i64) -> HistoryLoad {
    match result {
        Ok(log) => {
            let entries = history_entries(&log, now);
            let more = history_may_have_more(entries.len());
            HistoryLoad::Ready { entries, more, loading_more: false }
        }
        Err(e) => HistoryLoad::Failed(failure_text("히스토리를 확인", &e)),
    }
}

/// Add the next page to a ready list. A failure keeps the pages already shown and says why in
/// the returned line; other states are left alone.
pub fn history_append(state: HistoryLoad, result: Result<GitLog, ApiError>, now: i64) -> (HistoryLoad, Option<String>) {
    let HistoryLoad::Ready { mut entries, .. } = state else {
        return (state, None);
    };
    match result {
        Ok(log) => {
            let page = history_entries(&log, now);
            let more = history_may_have_more(page.len());
            entries.extend(page);
            (HistoryLoad::Ready { entries, more, loading_more: false }, None)
        }
        Err(e) => (HistoryLoad::Ready { entries, more: true, loading_more: false }, Some(failure_text("다음 히스토리를 확인", &e))),
    }
}

// ---------------------------------------------------------------------------------------------
// Commit detail, revert, cherry-pick and a branch from a commit (FR-G14, FR-G15, #484)
// ---------------------------------------------------------------------------------------------

/// The most changed files the detail card lists; the rest is a count.
pub const COMMIT_FILES_SHOWN: usize = 100;
/// The most message-body lines the detail card shows.
pub const COMMIT_BODY_LINES: usize = 12;

/// One changed file of a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitFileView {
    pub path: String,
    pub old_path: Option<String>,
    /// "A", "M", "D", "R", "C" or "T".
    pub status_letter: &'static str,
    pub additions: u64,
    pub deletions: u64,
    pub binary: bool,
}

/// One commit in full, as the detail card shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitDetailView {
    pub sha: String,
    pub short: String,
    pub subject: String,
    /// The message after the subject, without blank lines, cut at `COMMIT_BODY_LINES`.
    pub body_lines: Vec<String>,
    pub author: String,
    /// "3시간 전", or the node's own text when it is not a time this code reads.
    pub time_text: String,
    pub parents: Vec<String>,
    pub refs: Vec<HistoryRef>,
    pub is_merge: bool,
    pub files: Vec<CommitFileView>,
    /// The files the commit changed, also those past what the node listed.
    pub files_total: usize,
    pub truncated: bool,
    /// A local commit not yet pushed to the remote.
    pub unpushed: bool,
    /// A commit on a remote ref not present on local branches.
    pub remote_only: bool,
}

/// The state of the detail of the selected commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitDetailLoad {
    Idle,
    Loading,
    Ready(CommitDetailView),
    Failed(String),
}

/// The letter before a file in the list.
pub fn file_status_letter(status: GitCommitFileStatus) -> &'static str {
    match status {
        GitCommitFileStatus::Added => "A",
        GitCommitFileStatus::Modified => "M",
        GitCommitFileStatus::Deleted => "D",
        GitCommitFileStatus::Renamed => "R",
        GitCommitFileStatus::Copied => "C",
        GitCommitFileStatus::TypeChanged => "T",
    }
}

/// The count after a file: "+12 -3", or "바이너리" for a file with no line counts.
pub fn file_stat_text(additions: u64, deletions: u64, binary: bool) -> String {
    if binary { "바이너리".to_string() } else { format!("+{additions} -{deletions}") }
}

/// The detail card's data for a node answer. `now` is unix seconds.
pub fn commit_detail_view(d: &GitCommitDetail, now: i64) -> CommitDetailView {
    let author = clip_line(&d.author, COMMIT_TEXT_MAX);
    let subject = clip_line(&d.subject, COMMIT_TEXT_MAX);
    let files: Vec<CommitFileView> = d
        .files
        .iter()
        .map(|f| CommitFileView {
            path: f.path.clone(),
            old_path: f.old_path.clone().filter(|o| !o.is_empty() && *o != f.path),
            status_letter: file_status_letter(f.status),
            additions: f.additions,
            deletions: f.deletions,
            binary: f.binary,
        })
        .collect();
    let rank = |k: &GitLogRefKind| match k {
        GitLogRefKind::Head => 0,
        GitLogRefKind::Branch => 1,
        GitLogRefKind::Remote => 2,
        GitLogRefKind::Tag => 3,
    };
    let mut refs: Vec<HistoryRef> = d.refs.iter().map(|r| HistoryRef { label: ref_label(&r.name), kind: r.kind.clone() }).collect();
    refs.sort_by_key(|r| rank(&r.kind));
    // The node sends the whole message in `body` (subject first in some answers): drop a leading copy of the subject.
    let mut lines = d.body.lines().map(str::trim_end).skip_while(|l| l.trim().is_empty());
    let first = lines.clone().next();
    if first.is_some_and(|l| l.trim() == d.subject.trim()) {
        lines.next();
    }
    let body_lines: Vec<String> = lines.filter(|l| !l.trim().is_empty()).take(COMMIT_BODY_LINES).map(|l| clip_line(l, COMMIT_TEXT_MAX)).collect();
    let has_remote = refs.iter().any(|r| r.kind == GitLogRefKind::Remote);
    let has_local = refs.iter().any(|r| r.kind == GitLogRefKind::Branch || r.kind == GitLogRefKind::Head);
    let unpushed = has_local && !has_remote;
    let remote_only = has_remote && !has_local;
    CommitDetailView {
        sha: d.sha.clone(),
        short: if d.short.is_empty() { d.sha.chars().take(7).collect() } else { d.short.clone() },
        subject,
        body_lines,
        author,
        time_text: iso_to_unix(&d.time).map(|t| history_time_text(now, t)).unwrap_or_else(|| d.time.clone()),
        parents: d.parents.clone(),
        refs,
        is_merge: d.parents.len() > 1,
        files_total: d.files_total.max(files.len()),
        truncated: d.truncated,
        files,
        unpushed,
        remote_only,
    }
}

/// Attempt to copy `sha` to the system clipboard on macOS, Linux, or Windows.
/// Returns true if an OS clipboard command was launched and succeeded.
/// TODO(compose-rust#471): replace with native toolkit clipboard support once available.
pub fn copy_sha_to_clipboard(sha: &str) -> bool {
    let sha = sha.trim();
    if sha.is_empty() {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(mut child) = std::process::Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                let _ = stdin.write_all(sha.as_bytes());
            }
            return child.wait().map(|s| s.success()).unwrap_or(false);
        }
    }
    #[cfg(target_os = "linux")]
    {
        for cmd in ["wl-copy", "xclip -selection clipboard", "xsel --clipboard --input"] {
            let parts: Vec<&str> = cmd.split_whitespace().collect();
            if let Ok(mut child) = std::process::Command::new(parts[0])
                .args(&parts[1..])
                .stdin(std::process::Stdio::piped())
                .spawn()
            {
                if let Some(mut stdin) = child.stdin.take() {
                    use std::io::Write;
                    let _ = stdin.write_all(sha.as_bytes());
                }
                if child.wait().map(|s| s.success()).unwrap_or(false) {
                    return true;
                }
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(mut child) = std::process::Command::new("clip")
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                let _ = stdin.write_all(sha.as_bytes());
            }
            return child.wait().map(|s| s.success()).unwrap_or(false);
        }
    }
    false
}

/// Read one commit. Runs on the runtime, not the UI thread.
pub async fn fetch_commit_detail(project: &str, sha: &str) -> Result<GitCommitDetail, ApiError> {
    git_call(project, "commit/show", json!({ "sha": sha })).await
}

/// The detail as a view state. A failure keeps the node's Korean line, else a Korean one.
pub fn commit_detail_from(result: Result<GitCommitDetail, ApiError>, now: i64) -> CommitDetailLoad {
    match result {
        Ok(d) => CommitDetailLoad::Ready(commit_detail_view(&d, now)),
        Err(e) => CommitDetailLoad::Failed(commit_failure("커밋 확인", &e)),
    }
}

/// The most characters of one displayed line of commit text (subject, author, one body line).
pub const COMMIT_TEXT_MAX: usize = 200;

/// The node lists at most this many files of one commit.
pub const COMMIT_NODE_FILES_MAX: usize = 500;

/// Commit text is untrusted: control characters are dropped (a tab becomes a space) and a line
/// past `max` characters ends in an ellipsis, so one commit cannot fill the screen.
pub fn clip_line(text: &str, max: usize) -> String {
    let clean: String = text.chars().filter_map(|c| if c == '\t' { Some(' ') } else if c.is_control() { None } else { Some(c) }).collect();
    if clean.chars().count() <= max {
        return clean;
    }
    let mut out: String = clean.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// The line under the file list. `shown` files are on screen of `total`; `truncated` says the
/// node cut its own list. The cut files are part of the "more" count, so they are not counted twice.
pub fn commit_files_notice(shown: usize, total: usize, truncated: bool) -> Option<String> {
    let more = total.saturating_sub(shown);
    let cut = format!("파일 목록이 {COMMIT_NODE_FILES_MAX}개에서 잘렸습니다 (전체 {total}개)");
    match (more > 0, truncated) {
        (false, false) => None,
        (true, false) => Some(format!("{more}개 더 있습니다")),
        (true, true) => Some(format!("{more}개 더 있습니다. {cut}")),
        (false, true) => Some(cut),
    }
}

/// The files shown and the line under them for the rest.
pub fn commit_files_shown(v: &CommitDetailView) -> (&[CommitFileView], Option<String>) {
    let shown = &v.files[..v.files.len().min(COMMIT_FILES_SHOWN)];
    (shown, commit_files_notice(shown.len(), v.files_total, v.truncated))
}

/// Map common git failures before displaying a history operation error.
fn history_error_reason(e: &ApiError) -> Option<&'static str> {
    let ApiError::Status { message, code, .. } = e else { return None };
    let raw = format!("{} {}", code.as_deref().unwrap_or(""), message).to_lowercase();
    if raw.contains("dirty") || raw.contains("local changes") || raw.contains("uncommitted") {
        Some("작업 중인 변경이 있습니다. 먼저 커밋하거나 스태시해야 합니다.")
    } else if raw.contains("conflict") {
        Some("충돌이 발생하여 작업을 완료하지 못했습니다. 저장소 상태를 확인해야 합니다.")
    } else if raw.contains("index.lock") || raw.contains("another git process") || raw.contains("in_progress") {
        Some("진행 중인 git 작업이 있습니다. 작업이 끝난 뒤 다시 시도해야 합니다.")
    } else if raw.contains("unknown revision") || raw.contains("bad revision") || raw.contains("unknown_commit") {
        Some("커밋을 찾을 수 없습니다. 히스토리를 다시 불러와야 합니다.")
    } else if raw.contains("not a git repository") || raw.contains("not_repository") {
        Some("git 저장소를 찾을 수 없습니다. 프로젝트 경로를 확인해야 합니다.")
    } else if raw.contains("identity unknown") || raw.contains("unable to auto-detect email") {
        Some("git 사용자 이름과 이메일이 설정되어 있지 않습니다.")
    } else { None }
}

/// A Korean line for a failed commit call: the node's Korean message as it is; anything else
/// (git's own text, a demo's) becomes "{what}에 실패했습니다" with a Korean reason, never raw English.
pub fn commit_failure(what: &str, e: &ApiError) -> String {
    if let Some(reason) = history_error_reason(e) { return reason.to_string(); }
    match e {
        ApiError::Status { message, .. } if is_korean(message) => message.clone(),
        ApiError::Status { status: 409, .. } => "작업을 하지 못했습니다. 저장소 상태를 확인해 주세요.".to_string(),
        ApiError::Status { .. } => format!("{what}에 실패했습니다. 잠시 후 다시 시도해 주세요"),
        ApiError::Unauthorized(_) => format!("{what}에 실패했습니다. 서버 인증에 실패했습니다. 서버 토큰을 확인해 주세요"),
        _ => format!("{what}에 실패했습니다. 서버에 닿지 않습니다"),
    }
}

/// The two actions that change the current branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitAction {
    Revert,
    CherryPick,
}

impl CommitAction {
    /// The button text.
    pub fn label(self) -> &'static str {
        match self {
            CommitAction::Revert => "되돌리기",
            CommitAction::CherryPick => "Cherry-pick",
        }
    }

    /// The button text of the armed second tap.
    pub fn armed_label(self) -> &'static str {
        match self {
            CommitAction::Revert => "정말 되돌립니다",
            CommitAction::CherryPick => "정말 가져옵니다",
        }
    }

    fn done_text(self) -> &'static str {
        match self {
            CommitAction::Revert => "되돌렸습니다",
            CommitAction::CherryPick => "가져왔습니다",
        }
    }
}

/// Why an action cannot start, before any call. A merge commit cannot be cherry-picked (it has
/// no single parent to diff against); both actions need a clean tracked tree. `dirty` is
/// computed from the screen's snapshot; an unknown state is passed as clean and the node answers.
pub fn commit_action_gate(action: CommitAction, dirty: bool, is_merge: bool) -> Result<(), &'static str> {
    if action == CommitAction::CherryPick && is_merge {
        return Err("병합 커밋은 가져올 수 없습니다");
    }
    if dirty {
        return Err("작업 중인 변경이 있어 먼저 커밋하거나 스태시해야 합니다");
    }
    Ok(())
}

/// Whether the snapshot has changes to tracked files. An untracked file reads as an added file
/// that is not staged (git leaves it alone on a revert), so it does not count.
pub fn snapshot_dirty(snap: &RepoSnapshot) -> bool {
    snap.changes.iter().any(|f| !(f.status == FileStatus::Added && !snap.staged.contains(&f.path)))
}

/// The key of one action on one commit in the armed state (`delete_tap`): two keys per commit,
/// so arming 되돌리기 never confirms Cherry-pick and another commit starts unarmed.
pub fn commit_action_key(sha: &str, action: CommitAction) -> usize {
    let mut h: usize = 0xcbf2_9ce4;
    for b in sha.bytes() {
        h = (h ^ b as usize).wrapping_mul(0x0100_0193);
    }
    (h >> 1).wrapping_mul(2) + usize::from(action == CommitAction::CherryPick)
}

/// The line under the actions while one is armed: which branch gets the new commit.
pub fn commit_action_branch_line(_action: CommitAction, branch: &str) -> String {
    let branch = branch.trim();
    if branch.is_empty() {
        "현재 브랜치에 새 커밋을 만듭니다".to_string()
    } else {
        format!("현재 브랜치 {branch}에 새 커밋을 만듭니다")
    }
}

/// The notice after a revert or cherry-pick: the branch and the new commit's short sha.
pub fn commit_applied_notice(_action: CommitAction, branch: &str, short: &str) -> String {
    let branch = if branch.trim().is_empty() { "현재 브랜치" } else { branch.trim() };
    let short: String = short.chars().take(7).collect();
    if short.is_empty() {
        format!("{branch}에 새 커밋을 만들었습니다")
    } else {
        format!("{branch}에 새 커밋 {short}을 만들었습니다")
    }
}

/// Whether a failure line says the snapshot's dirty flag was wrong (the node found changes the
/// client did not see): the detail then reloads the snapshot so the flag corrects itself.
pub fn commit_failure_reloads_snapshot(line: &str) -> bool {
    line.contains("작업 중인 변경") || line.contains("진행 중인 git 작업")
}

async fn run_commit_apply(project: &str, sha: &str, action: CommitAction, branch: &str) -> Result<String, String> {
    let op = if action == CommitAction::Revert { "revert" } else { "cherry-pick" };
    git_call::<GitCommitApplied>(project, op, json!({ "sha": sha, "confirm": true }))
        .await
        .map(|a| commit_applied_notice(action, branch, &a.new_sha))
        .map_err(|e| commit_failure(action.label(), &e))
}

/// Revert a commit on the current branch. `Ok` is the notice, `Err` a Korean line (the node's
/// conflict answer is Korean and shown as it is).
pub async fn run_commit_revert(project: &str, sha: &str, branch: &str) -> Result<String, String> {
    run_commit_apply(project, sha, CommitAction::Revert, branch).await
}

/// Cherry-pick a commit onto the current branch. Same answers as `run_commit_revert`.
pub async fn run_commit_cherry_pick(project: &str, sha: &str, branch: &str) -> Result<String, String> {
    run_commit_apply(project, sha, CommitAction::CherryPick, branch).await
}

/// Create `name` at `sha` and switch to it (the node's `branch` op with `from`).
pub async fn run_branch_from(project: &str, name: &str, sha: &str) -> Result<String, String> {
    let name = name.trim();
    if !valid_branch_name(name) {
        return Err("브랜치 이름을 확인해 주세요. 공백이나 쓸 수 없는 문자가 있습니다".to_string());
    }
    git_call::<Value>(project, "branch", branch_create_body(name, Some(sha)))
        .await
        .map(|_| format!("브랜치를 만들어 전환했습니다: {name}"))
        .map_err(|e| commit_failure("브랜치 만들기", &e))
}

// ---------------------------------------------------------------------------------------------
// Pull requests (FR-G8)
// ---------------------------------------------------------------------------------------------

/// Which pull requests the list asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PrFilter {
    #[default]
    Open,
    Closed,
    Merged,
    All,
}

impl PrFilter {
    pub const ALL: [PrFilter; 4] = [PrFilter::Open, PrFilter::Closed, PrFilter::Merged, PrFilter::All];

    pub fn label(self) -> &'static str {
        match self {
            PrFilter::Open => "열림",
            PrFilter::Closed => "닫힘",
            PrFilter::Merged => "병합됨",
            PrFilter::All => "전체",
        }
    }

    /// The `state` the node takes.
    pub fn arg(self) -> &'static str {
        match self {
            PrFilter::Open => "open",
            PrFilter::Closed => "closed",
            PrFilter::Merged => "merged",
            PrFilter::All => "all",
        }
    }

    /// The line of an empty list.
    pub fn empty_text(self) -> &'static str {
        match self {
            PrFilter::Open => "열린 풀 리퀘스트가 없습니다",
            PrFilter::Closed => "닫힌 풀 리퀘스트가 없습니다",
            PrFilter::Merged => "병합된 풀 리퀘스트가 없습니다",
            PrFilter::All => "풀 리퀘스트가 없습니다",
        }
    }
}

/// How many pull requests one read asks for.
pub const PR_LIMIT: u32 = 30;

/// The row of one pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrEntry {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub head: String,
    pub base: String,
    pub time_text: String,
    pub state: GitPrState,
    /// "열림", "닫힘" or "병합됨".
    pub state_text: &'static str,
    pub draft: bool,
    pub review: GitPrReview,
    /// "승인됨", "변경 요청", "리뷰 필요" or "리뷰 없음".
    pub review_text: &'static str,
    /// "체크 3/4 통과", or "체크 없음".
    pub checks_text: String,
    pub checks_failed: u32,
    pub checks_pending: u32,
    pub can_merge: bool,
    /// "병합할 수 있습니다", or the node's reason it cannot be merged.
    pub gate_text: String,
    pub url: String,
}

/// The state of the pull request list of one project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrLoad {
    Loading,
    Ready(Vec<PrEntry>),
    /// The Korean line with the node's message (shown with a retry button). `gh` missing, not
    /// logged in and a repository with no GitHub remote arrive here.
    Failed(String),
}

pub fn pr_state_text(state: GitPrState) -> &'static str {
    match state {
        GitPrState::Open => "열림",
        GitPrState::Closed => "닫힘",
        GitPrState::Merged => "병합됨",
    }
}

pub fn pr_review_text(review: GitPrReview) -> &'static str {
    match review {
        GitPrReview::Approved => "승인됨",
        GitPrReview::ChangesRequested => "변경 요청",
        GitPrReview::ReviewRequired => "리뷰 필요",
        GitPrReview::None => "리뷰 없음",
    }
}

/// "체크 3/4 통과": the checks that passed over all of them; "체크 없음" when there are none.
pub fn pr_checks_text(passed: u32, failed: u32, pending: u32) -> String {
    let total = passed + failed + pending;
    if total == 0 {
        "체크 없음".to_string()
    } else {
        format!("체크 {passed}/{total} 통과")
    }
}

/// The row of one pull request. A time that cannot be read shows as the node sent it.
pub fn pr_entry(pr: &GitPr, now: i64) -> PrEntry {
    let c = pr.checks;
    PrEntry {
        number: pr.number,
        title: pr.title.clone(),
        author: pr.author.clone(),
        head: pr.head.clone(),
        base: pr.base.clone(),
        time_text: iso_to_unix(&pr.updated_at).map(|t| history_time_text(now, t)).unwrap_or_else(|| pr.updated_at.clone()),
        state: pr.state,
        state_text: pr_state_text(pr.state),
        draft: pr.draft,
        review: pr.review,
        review_text: pr_review_text(pr.review),
        checks_text: pr_checks_text(c.passed, c.failed, c.pending),
        checks_failed: c.failed,
        checks_pending: c.pending,
        can_merge: pr.merge.can_merge,
        gate_text: if pr.merge.can_merge {
            "병합할 수 있습니다".to_string()
        } else if pr.merge.reason.trim().is_empty() {
            "병합 조건을 채우지 못했습니다".to_string()
        } else {
            pr.merge.reason.clone()
        },
        url: pr.url.clone(),
    }
}

/// The list as a view state. An empty list is a ready, empty state, not an error.
pub fn prs_from(result: Result<GitPrs, ApiError>, now: i64) -> PrLoad {
    match result {
        Ok(list) => PrLoad::Ready(list.prs.iter().map(|p| pr_entry(p, now)).collect()),
        Err(e) => PrLoad::Failed(failure_text("풀 리퀘스트 목록을 확인", &e)),
    }
}

/// The `prs` body for a filter.
pub fn prs_body(filter: PrFilter) -> Value {
    json!({ "state": filter.arg(), "limit": PR_LIMIT })
}

/// Read a project's pull requests. Runs on the runtime, not the UI thread.
pub async fn fetch_prs(project: &str, filter: PrFilter) -> Result<GitPrs, ApiError> {
    git_call(project, "prs", prs_body(filter)).await
}

// ---------------------------------------------------------------------------------------------
// Pull request detail, review, merge and new PR (FR-G10, FR-G11, FR-G12, #483)
// ---------------------------------------------------------------------------------------------

/// The class of one check run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckClass {
    Passed,
    Failed,
    Pending,
}

impl CheckClass {
    /// The word beside the check's dot.
    pub fn label(self) -> &'static str {
        match self {
            CheckClass::Passed => "통과",
            CheckClass::Failed => "실패",
            CheckClass::Pending => "진행 중",
        }
    }
}

/// Where one reviewer stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewerState {
    Approved,
    ChangesRequested,
    /// Asked to review, or a dismissed or pending review.
    Waiting,
    /// Left comments only.
    Commented,
}

impl ReviewerState {
    pub fn label(self) -> &'static str {
        match self {
            ReviewerState::Approved => "승인",
            ReviewerState::ChangesRequested => "변경 요청",
            ReviewerState::Waiting => "대기",
            ReviewerState::Commented => "코멘트",
        }
    }
}

/// The merge hint when the node gave no reason.
pub const MERGE_GATE_DEFAULT: &str = "CI 통과와 승인이 있어야 병합할 수 있습니다";
/// The longest comment shown in the detail, in characters.
pub const PR_COMMENT_CUT: usize = 300;
/// The most description lines shown in the detail.
pub const PR_BODY_LINES: usize = 8;

/// One pull request in full, as the detail card shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrDetailView {
    pub number: u64,
    pub title: String,
    /// "열림", "닫힘" or "병합됨".
    pub state_text: &'static str,
    pub body_lines: Vec<String>,
    pub author: String,
    pub head: String,
    pub base: String,
    /// "파일 3 +40 -2".
    pub files_text: String,
    pub checks: Vec<(String, CheckClass, String)>,
    /// The row's checks summary ("체크 3/4 통과", or "체크 없음").
    pub ci_pill: String,
    pub checks_failed: u32,
    pub checks_pending: u32,
    /// "승인됨", "변경 요청", "리뷰 필요" or "리뷰 없음".
    pub review_pill: &'static str,
    pub review: GitPrReview,
    pub reviewers: Vec<(String, ReviewerState)>,
    pub comments: Vec<(String, String)>,
    pub can_merge: bool,
    /// The node's Korean reason, or `MERGE_GATE_DEFAULT` when it sent none.
    pub merge_reason: String,
    pub open: bool,
    pub draft: bool,
    /// The head commit the user saw (40 hex characters); `pr/merge` carries it. Empty when the node sent none.
    pub head_sha: String,
}

/// The state of the detail of the selected pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrDetailLoad {
    Idle,
    Loading,
    Ready(PrDetailView),
    Failed(String),
}

/// A text cut at `max` characters with an ellipsis.
fn cut_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut s: String = text.chars().take(max).collect();
        s.push('\u{2026}');
        s
    }
}

/// The reviewers: the newest decisive review of each author, then the requested ones that have
/// not reviewed. A comment-only review never replaces an approval or a change request.
pub fn pr_reviewers(reviews: &[GitPrReviewItem], requested: &[String]) -> Vec<(String, ReviewerState)> {
    let mut out: Vec<(String, ReviewerState)> = Vec::new();
    for r in reviews {
        if r.author.is_empty() {
            continue;
        }
        let state = match r.state {
            GitPrReviewState::Approved => ReviewerState::Approved,
            GitPrReviewState::ChangesRequested => ReviewerState::ChangesRequested,
            GitPrReviewState::Commented => ReviewerState::Commented,
            GitPrReviewState::Dismissed | GitPrReviewState::Pending => ReviewerState::Waiting,
        };
        match out.iter_mut().find(|(a, _)| *a == r.author) {
            Some(slot) => {
                if !(state == ReviewerState::Commented && slot.1 != ReviewerState::Waiting) {
                    slot.1 = state;
                }
            }
            None => out.push((r.author.clone(), state)),
        }
    }
    for name in requested {
        if !name.is_empty() && !out.iter().any(|(a, _)| a == name) {
            out.push((name.clone(), ReviewerState::Waiting));
        }
    }
    out
}

/// The detail of one pull request for the card.
pub fn pr_detail_view(d: &GitPrDetail) -> PrDetailView {
    let pr = &d.pr;
    let checks = d
        .check_runs
        .iter()
        .map(|c| match c.state {
            GitPrCheckState::Passed => (c.name.clone(), CheckClass::Passed, CheckClass::Passed.label().to_string()),
            GitPrCheckState::Failed => (c.name.clone(), CheckClass::Failed, CheckClass::Failed.label().to_string()),
            GitPrCheckState::Pending => (c.name.clone(), CheckClass::Pending, CheckClass::Pending.label().to_string()),
        })
        .collect();
    let mut comments: Vec<(String, String)> = d
        .reviews
        .iter()
        .filter(|r| !r.body.trim().is_empty())
        .map(|r| (r.author.clone(), cut_chars(r.body.trim(), PR_COMMENT_CUT)))
        .collect();
    comments.extend(d.comments.iter().filter(|c| !c.body.trim().is_empty()).map(|c| (c.author.clone(), cut_chars(c.body.trim(), PR_COMMENT_CUT))));
    PrDetailView {
        number: pr.number,
        title: pr.title.clone(),
        state_text: pr_state_text(pr.state),
        body_lines: d.body.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).take(PR_BODY_LINES).map(String::from).collect(),
        author: pr.author.clone(),
        head: pr.head.clone(),
        base: pr.base.clone(),
        files_text: format!("파일 {} +{} -{}", d.changed_files, d.additions, d.deletions),
        checks,
        ci_pill: pr_checks_text(pr.checks.passed, pr.checks.failed, pr.checks.pending),
        checks_failed: pr.checks.failed,
        checks_pending: pr.checks.pending,
        review_pill: pr_review_text(pr.review),
        review: pr.review,
        reviewers: pr_reviewers(&d.reviews, &d.requested_reviewers),
        comments,
        can_merge: pr.merge.can_merge,
        merge_reason: if pr.merge.reason.trim().is_empty() { MERGE_GATE_DEFAULT.to_string() } else { pr.merge.reason.clone() },
        open: pr.state == GitPrState::Open,
        draft: pr.draft,
        head_sha: d.head_sha.clone(),
    }
}

/// The detail as a view state. A failure keeps the node's Korean line, else a Korean one.
pub fn pr_detail_from(result: Result<GitPrDetail, ApiError>) -> PrDetailLoad {
    match result {
        Ok(d) => PrDetailLoad::Ready(pr_detail_view(&d)),
        Err(e) => PrDetailLoad::Failed(korean_failure("풀 리퀘스트를 확인", &e)),
    }
}

/// Like `failure_text`, but an English message (git's, gh's or a demo's) never reaches the
/// screen: a Korean message of the node is shown as it is, anything else becomes
/// "{what}하지 못했습니다" with a Korean reason.
pub fn korean_failure(what: &str, e: &ApiError) -> String {
    match e {
        ApiError::Status { message, .. } if is_korean(message) => message.clone(),
        ApiError::Status { .. } => format!("{what}하지 못했습니다. 잠시 후 다시 시도해 주세요"),
        ApiError::Unauthorized(_) => format!("{what}하지 못했습니다. 서버 인증에 실패했습니다. 서버 토큰을 확인해 주세요"),
        _ => format!("{what}하지 못했습니다. 서버에 닿지 않습니다"),
    }
}

/// The merge button: enabled only when the node's gate is open and the pull request is open.
/// The hint is the reason shown under a disabled button.
pub fn merge_button_state(v: &PrDetailView) -> (bool, Option<String>) {
    if !v.open {
        return (false, Some("열려 있는 풀 리퀘스트만 병합할 수 있습니다".to_string()));
    }
    if v.head_sha.trim().is_empty() {
        return (false, Some(MERGE_NO_HEAD.to_string()));
    }
    if v.can_merge {
        (true, None)
    } else {
        (false, Some(v.merge_reason.clone()))
    }
}

/// The merge hint when the detail carries no head SHA to merge against.
pub const MERGE_NO_HEAD: &str = "헤드를 확인할 수 없어 병합할 수 없습니다";

/// The first line of the armed merge: the method is squash.
pub const MERGE_METHOD_TEXT: &str = "스쿼시로 병합합니다";

/// The label of the merge button; the armed second tap reads "정말 병합합니다".
pub fn merge_button_text(armed: bool) -> &'static str {
    if armed { "정말 병합합니다" } else { "병합" }
}

/// Whether a review of this kind can be sent: a comment and a change request need a body.
pub fn review_request_gate(action: GitPrReviewAction, body: &str) -> Result<(), &'static str> {
    match action {
        GitPrReviewAction::Approve => Ok(()),
        GitPrReviewAction::Comment | GitPrReviewAction::RequestChanges => {
            if body.trim().is_empty() { Err("내용을 입력해 주세요") } else { Ok(()) }
        }
    }
}

/// The longest title of a new pull request, in characters (the node's limit).
pub const PR_TITLE_MAX_CHARS: usize = 256;

/// A new pull request needs a title of one line, at most `PR_TITLE_MAX_CHARS` characters.
pub fn pr_create_gate(title: &str) -> Result<(), &'static str> {
    let t = title.trim();
    if t.is_empty() {
        Err("제목을 입력해 주세요")
    } else if t.chars().any(char::is_control) {
        Err("제목은 한 줄로 입력해 주세요")
    } else if t.chars().count() > PR_TITLE_MAX_CHARS {
        Err("제목은 256자까지 입력할 수 있습니다")
    } else {
        Ok(())
    }
}

/// A review or description body without control characters, except newline and tab. Pasted
/// text can carry escapes; the node and the host should never see them.
pub fn clean_body(body: &str) -> String {
    body.chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t').collect()
}

/// The key of the review draft: the project and the selected pull request, so a draft typed
/// for one pull request never shows under another.
pub fn review_draft_key(project: &str, number: Option<u64>) -> String {
    match number {
        Some(n) => format!("{project}#{n}"),
        None => format!("{project}#"),
    }
}

/// The key of one review action of one pull request in the armed state.
pub fn pr_action_key(number: u64, action: GitPrReviewAction) -> usize {
    let slot = match action {
        GitPrReviewAction::Comment => 0,
        GitPrReviewAction::Approve => 1,
        GitPrReviewAction::RequestChanges => 2,
    };
    number as usize * 3 + slot
}

/// What a tap on a review button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewTap {
    /// First tap of 승인 or 변경 요청: the button asks again.
    Arm,
    /// A second tap before 600 ms: ignored.
    Wait,
    /// Send the review.
    Send,
    /// The armed state ran out: arm again.
    Expired,
}

/// 승인 and 변경 요청 are public and cannot be taken back, so they ask twice like 병합.
/// 코멘트 is one tap. `armed` is the key and arm time of the armed review button.
pub fn review_tap(action: GitPrReviewAction, armed: Option<(usize, i64)>, number: u64, now: i64) -> ReviewTap {
    if action == GitPrReviewAction::Comment {
        return ReviewTap::Send;
    }
    match delete_tap(armed, pr_action_key(number, action), now) {
        DeleteTap::Arm => ReviewTap::Arm,
        DeleteTap::Wait => ReviewTap::Wait,
        DeleteTap::Confirm => ReviewTap::Send,
        DeleteTap::Expired => ReviewTap::Expired,
    }
}

/// The label of a review button, with the question while it is armed.
pub fn review_button_label(action: GitPrReviewAction, armed: bool) -> &'static str {
    match (action, armed) {
        (GitPrReviewAction::Approve, true) => "정말 승인합니다",
        (GitPrReviewAction::RequestChanges, true) => "정말 변경을 요청합니다",
        (GitPrReviewAction::Approve, false) => "승인",
        (GitPrReviewAction::RequestChanges, false) => "변경 요청",
        (GitPrReviewAction::Comment, _) => "코멘트",
    }
}

/// The node's Korean line for a merge refused because the head moved (409). Copied from
/// `ember/node/src/git_prs.rs` (the `GitError::Conflict` of the merge); a test pins the stem.
pub const STALE_HEAD_TEXT: &str = "풀 리퀘스트가 바뀌었습니다";

/// Whether an error line says the head moved since the detail was read.
pub fn is_stale_head(error_line: &str) -> bool {
    error_line.contains(STALE_HEAD_TEXT)
}

pub fn pr_review_notice(action: GitPrReviewAction) -> &'static str {
    match action {
        GitPrReviewAction::Approve => "승인했습니다",
        GitPrReviewAction::RequestChanges => "변경을 요청했습니다",
        GitPrReviewAction::Comment => "코멘트를 남겼습니다",
    }
}

fn non_empty(text: &str) -> Option<&str> {
    let t = text.trim();
    if t.is_empty() { None } else { Some(t) }
}

/// The `pr/review` body. The caller confirmed by tapping, so `confirm` is true.
pub fn pr_review_body(number: u64, action: GitPrReviewAction, body: &str) -> Value {
    json!({ "number": number, "action": action, "body": non_empty(&clean_body(body)), "confirm": true })
}

/// The `pr/merge` body (squash by default; the branch stays). `head_sha` is the head the user saw.
pub fn pr_merge_body(number: u64, method: GitPrMergeMethod, head_sha: &str) -> Value {
    json!({ "number": number, "method": method, "delete_branch": false, "head_sha": head_sha, "confirm": true })
}

/// The `pr/create` body; an empty base or head is left to the node.
pub fn pr_create_body(title: &str, body: &str, base: &str, head: &str, draft: bool) -> Value {
    json!({ "title": title.trim(), "body": non_empty(&clean_body(body)), "base": non_empty(base), "head": non_empty(head), "draft": draft, "confirm": true })
}

/// Read one pull request. Runs on the runtime, not the UI thread.
pub async fn fetch_pr_detail(project: &str, number: u64) -> Result<GitPrDetail, ApiError> {
    git_call(project, "pr", json!({ "number": number })).await
}

/// Leave a review. `Ok` is the notice to show, `Err` the Korean line.
pub async fn run_pr_review(project: &str, number: u64, action: GitPrReviewAction, body: &str) -> Result<String, String> {
    review_request_gate(action, body).map_err(String::from)?;
    git_call::<Value>(project, "pr/review", pr_review_body(number, action, body))
        .await
        .map(|_| pr_review_notice(action).to_string())
        .map_err(|e| korean_failure("리뷰 등록", &e))
}

/// Merge. `Ok` is the notice to show, `Err` the Korean line (a closed gate or a moved head answers 409).
pub async fn run_pr_merge(project: &str, number: u64, method: GitPrMergeMethod, head_sha: &str) -> Result<String, String> {
    if head_sha.trim().is_empty() {
        return Err(MERGE_NO_HEAD.to_string());
    }
    git_call::<Value>(project, "pr/merge", pr_merge_body(number, method, head_sha))
        .await
        .map(|_| "병합했습니다".to_string())
        .map_err(|e| korean_failure("병합", &e))
}

/// Create a pull request. `Ok` is the notice with the new number.
pub async fn run_pr_create(project: &str, title: &str, body: &str, base: &str, head: &str, draft: bool) -> Result<String, String> {
    pr_create_gate(title).map_err(String::from)?;
    git_call::<GitPrCreated>(project, "pr/create", pr_create_body(title, body, base, head, draft))
        .await
        .map(|c| pr_created_notice(c.number))
        .map_err(|e| korean_failure("풀 리퀘스트 생성", &e))
}

pub fn pr_created_notice(number: u64) -> String {
    format!("풀 리퀘스트 #{number}을 만들었습니다")
}

// ---------------------------------------------------------------------------------------------
// Branches (FR-G9)
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod fr_l19_branch_followup_tests {
    use super::*;

    #[test]
    fn fr_l19_switch_guard_covers_flight_and_600_ms() {
        assert!(!branch_switch_allowed(true, None, 1000));
        assert!(!branch_switch_allowed(false, Some(1000), 1599));
        assert!(branch_switch_allowed(false, Some(1000), 1600));
        assert!(branch_switch_allowed(false, None, 1000));
    }

    #[test]
    fn fr_l19_branch_search_ignores_case_and_whitespace() {
        assert!(branch_matches("feature/Hello", "  HELLO "));
        assert!(!branch_matches("main", "hello"));
        assert!(branch_matches("main", " "));
    }

    #[test]
    fn fr_l19_branch_and_stash_notes_are_clear() {
        assert!(BRANCH_SWITCH_NOTE.contains("변경"));
        assert!(BRANCH_CREATE_NOTE.contains("변경"));
        assert!(STASH_NOW_NOTE.contains("추적하지 않는 파일"));
        for note in [BRANCH_SWITCH_NOTE, BRANCH_CREATE_NOTE, STASH_NOW_NOTE] {
            assert!(note.ends_with("합니다.") || note.ends_with("입니다.") || note.ends_with("습니다."));
        }
        assert_eq!(branch_bar_text(2, 1), "원격보다 2개 앞서고 1개 뒤처져 있습니다.");
    }
}

/// Guidance shown before branch and stash actions.
pub const BRANCH_SWITCH_NOTE: &str = "전환할 때 커밋하지 않은 변경 사항이 함께 이동할 수 있습니다.";
pub const BRANCH_CREATE_NOTE: &str = "만들고 전환할 때 커밋하지 않은 변경 사항이 함께 이동할 수 있습니다.";
pub const STASH_NOW_NOTE: &str = "스태시는 추적하지 않는 파일을 포함하지 않습니다.";

pub fn branch_switch_allowed(busy: bool, last: Option<i64>, now: i64) -> bool {
    !busy && last.map_or(true, |time| now.saturating_sub(time) >= 600)
}

pub fn branch_matches(name: &str, query: &str) -> bool {
    name.to_lowercase().contains(&query.trim().to_lowercase())
}

pub fn branch_bar_text(ahead: u32, behind: u32) -> String {
    format!("원격보다 {ahead}개 앞서고 {behind}개 뒤처져 있습니다.")
}

/// Commits the ahead/behind bars show at full width; more is drawn as full.
pub const BRANCH_BAR_CAP: u32 = 10;

/// One row of the branch list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchEntry {
    pub name: String,
    pub current: bool,
    pub default: bool,
    /// The upstream's short name, or "업스트림 없음".
    pub upstream_text: String,
    pub has_upstream: bool,
    pub ahead: u32,
    pub behind: u32,
    /// "a1b2c3d \u{b7} subject \u{b7} 12분 전" (the parts that exist).
    pub last_line: String,
    /// The path of the worktree that has the branch checked out.
    pub worktree: Option<String>,
}

impl BranchEntry {
    /// "앞섬 2 · 뒤처짐 1", "원격과 같음" or "업스트림 없음".
    pub fn sync_text(&self) -> String {
        if self.has_upstream {
            ahead_behind_label(self.ahead, self.behind)
        } else {
            "업스트림 없음".to_string()
        }
    }
}

/// The state of the branch list of one project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchLoad {
    Loading,
    Ready(Vec<BranchEntry>),
    /// The Korean line with the node's message (shown with a retry button).
    Failed(String),
}

/// "앞섬 2 · 뒤처짐 1", "앞섬 2", "뒤처짐 1" or "원격과 같음" (for a branch that has an upstream).
pub fn ahead_behind_label(ahead: u32, behind: u32) -> String {
    match (ahead, behind) {
        (0, 0) => "원격과 같음".to_string(),
        (a, 0) => format!("앞섬 {a}"),
        (0, b) => format!("뒤처짐 {b}"),
        (a, b) => format!("앞섬 {a} \u{00b7} 뒤처짐 {b}"),
    }
}

/// The widths of the ahead and behind bars as fractions of the full bar (0.0 to 1.0).
pub fn branch_bar_fractions(ahead: u32, behind: u32) -> (f32, f32) {
    let f = |n: u32| n.min(BRANCH_BAR_CAP) as f32 / BRANCH_BAR_CAP as f32;
    (f(ahead), f(behind))
}

/// The last folder of a worktree path ("/Users/a/worktrees/ui" gives "ui").
pub fn worktree_tail(path: &str) -> String {
    let t = path.trim_end_matches('/');
    t.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(t).to_string()
}

/// The row of one branch. A time that cannot be read shows as the node sent it.
pub fn branch_entry(b: &GitBranchInfo, now: i64) -> BranchEntry {
    let time = b.last_time.as_deref().map(|t| iso_to_unix(t).map(|u| history_time_text(now, u)).unwrap_or_else(|| t.to_string()));
    let parts: Vec<String> = [Some(b.last_sha.clone()), Some(b.last_subject.clone()), time].into_iter().flatten().filter(|s| !s.trim().is_empty()).collect();
    BranchEntry {
        name: b.name.clone(),
        current: b.is_current,
        default: b.is_default,
        upstream_text: b.upstream.clone().filter(|u| !u.is_empty()).unwrap_or_else(|| "업스트림 없음".to_string()),
        has_upstream: b.upstream.as_deref().is_some_and(|u| !u.is_empty()),
        ahead: b.ahead,
        behind: b.behind,
        last_line: parts.join(" \u{00b7} "),
        worktree: b.worktree.clone().filter(|w| !w.is_empty()),
    }
}

/// The list as a view state, in the node's order (current, default, then newest). An empty list
/// is a ready, empty state, not an error.
pub fn branches_from(result: Result<GitBranches, ApiError>, now: i64) -> BranchLoad {
    match result {
        Ok(list) => BranchLoad::Ready(list.branches.iter().map(|b| branch_entry(b, now)).collect()),
        Err(e) => BranchLoad::Failed(failure_text("브랜치 목록을 확인", &e)),
    }
}

/// The 기본 브랜치 row (the first default one) and the 작업 브랜치 rows (all others, in order).
pub fn split_branches(list: &[BranchEntry]) -> (Option<&BranchEntry>, Vec<&BranchEntry>) {
    let default = list.iter().find(|b| b.default);
    let rest = list.iter().filter(|b| default.map_or(true, |d| !std::ptr::eq(*b, d))).collect();
    (default, rest)
}

/// Read a project's branches. Runs on the runtime, not the UI thread.
pub async fn fetch_branches(project: &str) -> Result<GitBranches, ApiError> {
    git_call(project, "branches", json!({})).await
}

/// The `branch` body: create `name` and switch to it, from `base` when one is chosen.
pub fn branch_create_body(name: &str, base: Option<&str>) -> Value {
    match base {
        Some(b) => json!({ "name": name, "from": b }),
        None => json!({ "name": name }),
    }
}

/// The 스태시 button of 변경 사항: a clean tree has nothing to stash, and the API is not called.
/// The Korean line for a failed stash-now. An untracked-only tree passes the gate (the gate counts
/// every change) but git has nothing to stash because untracked files are left out; its English
/// text is replaced by a line that says so.
pub fn stash_now_error(line: &str) -> String {
    if line.contains("No local changes") {
        "스태시할 추적 중인 변경이 없습니다. 추적하지 않는 파일은 포함되지 않습니다".to_string()
    } else {
        line.to_string()
    }
}

pub fn stash_now_gate(changes_len: usize) -> Result<(), &'static str> {
    if changes_len == 0 {
        Err("스태시할 변경이 없습니다")
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Stash (FR-G6)
// ---------------------------------------------------------------------------------------------

/// One row of the stash list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashEntry {
    pub index: u32,
    /// "stash@{N}".
    pub label: String,
    pub message: String,
    pub branch: String,
    pub time_text: String,
}

/// The state of the stash list of one project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StashLoad {
    Loading,
    Ready(Vec<StashEntry>),
    /// The Korean line with the node's message (shown with a retry button).
    Failed(String),
}

/// What a stash button asks the node to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StashAction {
    Apply,
    Pop,
    Drop,
}

impl StashAction {
    fn op(self) -> &'static str {
        match self {
            StashAction::Apply => "stash/apply",
            StashAction::Pop => "stash/pop",
            StashAction::Drop => "stash/drop",
        }
    }

    /// The start of the failure line, to which `failure_text` adds "하지 못했습니다".
    fn what(self) -> &'static str {
        match self {
            StashAction::Apply => "스태시를 적용",
            StashAction::Pop => "스태시 꺼내기를 ",
            StashAction::Drop => "스태시를 삭제",
        }
    }
}

/// Unix seconds of an ISO 8601 time as `git stash list` prints it (`2026-11-06T12:30:00+09:00`,
/// `Z` or an offset). `None` when the text is not that shape.
pub fn iso_to_unix(text: &str) -> Option<i64> {
    let (date, rest) = text.split_once('T')?;
    let mut d = date.split('-');
    let (y, m, day): (i64, i64, i64) = (d.next()?.parse().ok()?, d.next()?.parse().ok()?, d.next()?.parse().ok()?);
    if d.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&day) {
        return None;
    }
    let split_at = rest.find(['+', '-', 'Z']).unwrap_or(rest.len());
    let (clock, zone) = rest.split_at(split_at);
    let mut c = clock.split(':');
    let (hh, mm, ss): (i64, i64, i64) = (c.next()?.parse().ok()?, c.next()?.parse().ok()?, c.next().unwrap_or("0").split('.').next()?.parse().ok()?);
    let offset = match zone {
        "" | "Z" => 0,
        z => {
            let sign = if z.starts_with('-') { -1 } else { 1 };
            let (oh, om) = z[1..].split_once(':')?;
            sign * (oh.parse::<i64>().ok()? * 3_600 + om.parse::<i64>().ok()? * 60)
        }
    };
    // Days since 1970-01-01 from a civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3_600 + mm * 60 + ss - offset)
}

/// The row of one stash. A time that cannot be read shows as the node sent it.
pub fn stash_entry(e: &GitStashEntry, now: i64) -> StashEntry {
    StashEntry {
        index: e.index,
        label: format!("stash@{{{}}}", e.index),
        message: if e.message.trim().is_empty() { "(메시지 없음)".to_string() } else { e.message.clone() },
        branch: e.branch.clone(),
        time_text: iso_to_unix(&e.time).map(|t| history_time_text(now, t)).unwrap_or_else(|| e.time.clone()),
    }
}

/// The list as a view state. An empty list is a ready, empty state, not an error.
pub fn stash_from(result: Result<Vec<GitStashEntry>, ApiError>, now: i64) -> StashLoad {
    match result {
        Ok(list) => StashLoad::Ready(list.iter().map(|e| stash_entry(e, now)).collect()),
        Err(e) => StashLoad::Failed(failure_text("스태시 목록을 확인", &e)),
    }
}

/// The `stash/push` body: a blank message is sent as `null` (git writes its own).
pub fn stash_push_body(message: &str, include_untracked: bool) -> Value {
    let message = message.trim();
    json!({ "message": if message.is_empty() { Value::Null } else { Value::String(message.to_string()) }, "include_untracked": include_untracked })
}

/// Read a project's stashes. Runs on the runtime, not the UI thread.
pub async fn fetch_stashes(project: &str) -> Result<Vec<GitStashEntry>, ApiError> {
    git_call(project, "stash/list", json!({})).await
}

/// Make a stash. `Err` is the Korean line to show (the node's message when it sent one).
pub async fn run_stash_push(project: &str, message: &str, include_untracked: bool) -> Result<(), String> {
    git_call::<Value>(project, "stash/push", stash_push_body(message, include_untracked))
        .await
        .map(|_| ())
        .map_err(|e| failure_text("스태시 만들기를 ", &e))
}

/// Apply, pop or drop one stash. `Err` is the Korean line to show; a conflicting pop answers 409
/// with the node's message and keeps the entry.
pub async fn run_stash_action(project: &str, action: StashAction, index: u32) -> Result<(), String> {
    git_call::<Value>(project, action.op(), json!({ "index": index }))
        .await
        .map(|_| ())
        .map_err(|e| failure_text(action.what(), &e))
}

/// The list after an action came back: a failure keeps the list as it was, a success reads the
/// list again (the caller does that), so this only decides what to say.
pub fn stash_action_notice(action: StashAction, result: &Result<(), String>) -> String {
    match (action, result) {
        (_, Err(line)) => line.clone(),
        (StashAction::Apply, Ok(())) => "스태시를 적용했습니다".to_string(),
        (StashAction::Pop, Ok(())) => "스태시를 꺼냈습니다".to_string(),
        (StashAction::Drop, Ok(())) => "스태시를 삭제했습니다".to_string(),
    }
}

/// A failed action leaves the shown list alone; a success replaces it with the fresh read.
pub fn stash_after_action(shown: StashLoad, result: &Result<(), String>, fresh: StashLoad) -> StashLoad {
    if result.is_ok() { fresh } else { shown }
}

/// Milliseconds an armed stash button waits for its second tap before it disarms (#350).
pub const STASH_ARM_TIMEOUT_MS: i64 = 4000;

/// What a tap on a stash button that asks twice does (#350).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteTap {
    /// Arm this button (a first tap, or a tap on another button than the armed one).
    Arm,
    /// The label has not had time to show yet (under 600 ms): ignore the tap, stay armed.
    Wait,
    /// The second tap, 600 ms to 4000 ms after arming: do it.
    Confirm,
    /// The armed state ran out (4000 ms or more): the caller arms again, as for `Arm`.
    Expired,
}

/// `armed` is the key and arm time (ms) of the armed button. The key is the entry index for 삭제,
/// or `stash_tap_key` for the buttons that act on a dirty tree. Like `amend_tap`, a second tap
/// counts only after `AMEND_CONFIRM_DELAY_MS`, so a fast double click cannot skip the question.
pub fn delete_tap(armed: Option<(usize, i64)>, index: usize, now: i64) -> DeleteTap {
    match armed {
        Some((i, at)) if i == index => {
            let age = now - at;
            if age >= STASH_ARM_TIMEOUT_MS {
                DeleteTap::Expired
            } else if age < AMEND_CONFIRM_DELAY_MS {
                DeleteTap::Wait
            } else {
                DeleteTap::Confirm
            }
        }
        _ => DeleteTap::Arm,
    }
}

/// Whether the armed label ("정말 삭제합니다") shows. Nothing re-renders on a timer, so the label
/// may stay on screen until the next recomposition after the timeout; `delete_tap` is the
/// authority for what a tap does.
pub fn armed_label_visible(armed: Option<(usize, i64)>, index: usize, now: i64) -> bool {
    matches!(armed, Some((i, at)) if i == index && now - at < STASH_ARM_TIMEOUT_MS)
}

/// The key of one button of one entry in the armed state: three buttons per entry.
pub fn stash_tap_key(action: StashAction, index: u32) -> usize {
    let slot = match action {
        StashAction::Apply => 0,
        StashAction::Pop => 1,
        StashAction::Drop => 2,
    };
    index as usize * 3 + slot
}

/// 삭제 always asks; 적용 and 꺼내기 ask once when the work tree has changes.
pub fn stash_needs_confirm(action: StashAction, dirty: bool) -> bool {
    action == StashAction::Drop || dirty
}

/// What a tap on a stash button does. A clean tree acts at once for 적용 and 꺼내기.
pub fn stash_tap(action: StashAction, dirty: bool, armed: Option<(usize, i64)>, index: u32, now: i64) -> DeleteTap {
    if stash_needs_confirm(action, dirty) {
        delete_tap(armed, stash_tap_key(action, index), now)
    } else {
        DeleteTap::Confirm
    }
}

/// The visible text of a stash button; the armed second tap reads differently.
pub fn stash_button_text(action: StashAction, armed: bool) -> &'static str {
    match (action, armed) {
        (StashAction::Apply, false) => "적용",
        (StashAction::Apply, true) => "그래도 적용합니다",
        (StashAction::Pop, false) => "꺼내기",
        (StashAction::Pop, true) => "그래도 꺼냅니다",
        (StashAction::Drop, false) => "삭제",
        (StashAction::Drop, true) => "정말 삭제합니다",
    }
}

/// The line under an entry whose 적용 or 꺼내기 is armed on a dirty tree.
pub const STASH_DIRTY_WARNING: &str = "작업 중인 변경이 있습니다. 충돌할 수 있습니다.";

/// Added under the warning when 꺼내기 is armed: unlike 적용 it removes the stash on success.
pub const STASH_POP_NOTE: &str = "꺼내기는 성공하면 이 스태시를 삭제합니다.";

/// The accessible name of one entry's button: it names the entry, so a screen reader hears more
/// than three identical "적용" buttons (#350).
pub fn stash_button_label(index: u32, message: &str, verb: &str) -> String {
    format!("스태시 {index} {verb}: {message}")
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
        let mode = diff.mode.as_deref().unwrap_or("100644");
        out.push_str(&format!("new file mode {mode}\n--- /dev/null\n"));
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
                (LineKind::Add, false) => continue,
            }
            // The line had no newline in the diff; never invent one.
            if l.no_eol {
                body.push_str("\\ No newline at end of file\n");
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
    /// The commit body; empty means none.
    pub description: String,
    pub amend: bool,
}

/// The `git/commit` body: an empty description is sent as `null`.
pub fn commit_body(req: &CommitRequest) -> Value {
    let d = req.description.trim();
    json!({
        "summary": req.summary,
        "description": if d.is_empty() { Value::Null } else { Value::from(d) },
        "amend": req.amend,
    })
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
    let body = commit_body(&req);
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

/// Whether a text holds Hangul, so it is already a Korean sentence.
fn is_korean(text: &str) -> bool {
    text.chars().any(|c| ('\u{ac00}'..='\u{d7a3}').contains(&c))
}

/// The notice for a failed call. A Korean message from the node is shown as it is (the node
/// writes the whole sentence, detail after it); an English one (git's own, or a demo's) goes
/// after "{what}하지 못했습니다:".
pub fn failure_text(what: &str, e: &ApiError) -> String {
    if let Some(reason) = history_error_reason(e) { return reason.to_string(); }
    match e {
        ApiError::Status { message, .. } if is_korean(message) => message.clone(),
        ApiError::Status { message, .. } => format!("{what}하지 못했습니다: {message}"),
        ApiError::Unauthorized(_) => format!("{what}하지 못했습니다. 서버 인증에 실패했습니다. 서버 토큰을 확인해 주세요"),
        other => format!("{what}하지 못했습니다. 서버에 닿지 않습니다: {other}"),
    }
}

/// The count beside a section's name; `None` while the data is unavailable.
pub fn section_count(section: Section, snap: Option<&RepoSnapshot>) -> Option<usize> {
    let s = snap?;
    match section {
        Section::Changes => Some(s.changes.len()),
        Section::Branches => Some(s.branches.len()),
        Section::Conflicts => Some(s.conflicts.len()),
        _ => None,
    }
}

/// A session of the project with its folder, as the 팀 작업 tab matches it to a worktree.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberCwd {
    pub name: String,
    pub harness: String,
    pub leader: bool,
    pub cwd: String,
    /// "실행 중", "승인 대기" and so on.
    pub state_text: String,
    pub status: ember_client::LauncherStatus,
}

/// The member of a worktree row.
#[derive(Debug, Clone, PartialEq)]
pub struct TeamMember {
    pub name: String,
    pub harness: String,
    pub leader: bool,
    pub state_text: String,
    pub status: ember_client::LauncherStatus,
}

/// One row of 팀 작업: a worktree, its branch, its member and the branch's pull request.
#[derive(Debug, Clone, PartialEq)]
pub struct TeamRow {
    pub branch: String,
    pub worktree: String,
    /// `None` shows "담당 없음".
    pub member: Option<TeamMember>,
    /// "앞섬 2 \u{b7} 뒤처짐 1", "원격과 같음" or "업스트림 없음".
    pub sync_text: String,
    /// "#398 열림" for the branch's pull request (an open one first), `None` when there is none.
    pub pr_text: Option<String>,
    /// The branch the project's main checkout has open.
    pub current: bool,
}

/// `cwd` is `worktree` or inside it (a path boundary, so "/w/ui2" is not inside "/w/ui").
fn cwd_in_worktree(cwd: &str, worktree: &str) -> bool {
    let (c, w) = (cwd.trim_end_matches('/'), worktree.trim_end_matches('/'));
    // A worktree of "/" would hold every folder: it holds none.
    !w.is_empty() && (c == w || (c.starts_with(w) && c[w.len()..].starts_with('/')))
}

/// The rows of 팀 작업, one per branch that has a worktree: the current branch (the main
/// checkout) first, then by branch name. A member belongs to the worktree with the longest path
/// that holds its folder; when several members share one worktree the leader, else the first in
/// `members`, is shown.
pub fn team_rows(branches: &[BranchEntry], members: &[MemberCwd], prs: &[PrEntry]) -> Vec<TeamRow> {
    let trees: Vec<(&BranchEntry, &str)> = branches.iter().filter_map(|b| b.worktree.as_deref().filter(|w| !w.is_empty()).map(|w| (b, w))).collect();
    let owner = |m: &MemberCwd| -> Option<usize> {
        trees.iter().enumerate().filter(|(_, (_, w))| cwd_in_worktree(&m.cwd, w)).max_by_key(|(_, (_, w))| w.trim_end_matches('/').len()).map(|(i, _)| i)
    };
    let mut rows: Vec<TeamRow> = trees
        .iter()
        .enumerate()
        .map(|(i, (b, w))| {
            let mine: Vec<&MemberCwd> = members.iter().filter(|m| owner(m) == Some(i)).collect();
            let chosen = mine.iter().find(|m| m.leader).or_else(|| mine.first());
            let pr = prs.iter().filter(|p| p.head == b.name).max_by_key(|p| (p.state == GitPrState::Open, p.number));
            TeamRow {
                branch: b.name.clone(),
                worktree: w.to_string(),
                member: chosen.map(|m| TeamMember { name: m.name.clone(), harness: m.harness.clone(), leader: m.leader, state_text: m.state_text.clone(), status: m.status }),
                sync_text: b.sync_text(),
                pr_text: pr.map(|p| format!("#{} {}", p.number, p.state_text)),
                current: b.current,
            }
        })
        .collect();
    rows.sort_by(|a, b| b.current.cmp(&a.current).then_with(|| a.branch.cmp(&b.branch)));
    rows
}

/// Like `section_count_with_branches`, plus the count the 팀 작업 section read (`None` until read;
/// the snapshot count stands in meanwhile).
pub fn section_count_with_team(section: Section, snap: Option<&RepoSnapshot>, stash: Option<usize>, prs: Option<usize>, branches: Option<usize>, team: Option<usize>) -> Option<usize> {
    match section {
        Section::Team => team.or_else(|| section_count(section, snap)),
        _ => section_count_with_branches(section, snap, stash, prs, branches),
    }
}

/// The count beside a section's name, with the stash count read by the stash section (`None`
/// until it has been read). The stash count does not need the repository snapshot.
pub fn section_count_with_stash(section: Section, snap: Option<&RepoSnapshot>, stash: Option<usize>) -> Option<usize> {
    section_count_with(section, snap, stash, None)
}

/// The count beside a section's name, with the counts the stash and pull request sections read
/// themselves (`None` until read). Neither needs the repository snapshot.
pub fn section_count_with(section: Section, snap: Option<&RepoSnapshot>, stash: Option<usize>, prs: Option<usize>) -> Option<usize> {
    section_count_with_branches(section, snap, stash, prs, None)
}

/// Like `section_count_with`, plus the count the branch section read (`None` until read; the
/// snapshot's branch count stands in meanwhile).
pub fn section_count_with_branches(section: Section, snap: Option<&RepoSnapshot>, stash: Option<usize>, prs: Option<usize>, branches: Option<usize>) -> Option<usize> {
    match section {
        Section::Stash => stash,
        Section::PullRequests => prs,
        Section::Branches => branches.or_else(|| section_count(section, snap)),
        _ => section_count(section, snap),
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

/// The line under the repository chip. A project without a recorded repository (FR-L26) still
/// has one when the node read it, so the note says "not configured" only while there is no data.
pub fn repo_card_line(project: &ProjectBox, snap: Option<&RepoSnapshot>) -> String {
    match (&project.repo, snap) {
        (None, Some(s)) => format!("git 저장소 \u{00b7} {}", s.branch),
        _ => project.repo_line(),
    }
}

/// How one conflict block is settled (FR-G7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictChoice {
    Unresolved,
    Ours,
    Theirs,
    /// Ours first, then theirs.
    Both,
}

/// One piece of a conflicted file: text both sides share, or a block to settle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictSegment {
    Common(String),
    Block { ours_label: String, theirs_label: String, ours: String, theirs: String, choice: ConflictChoice },
}

/// A conflicted file as the editor shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictFile {
    pub path: String,
    pub segments: Vec<ConflictSegment>,
}

impl ConflictFile {
    /// The node's blocks as segments, every block still unresolved.
    pub fn from_node(path: &str, c: &GitConflict) -> ConflictFile {
        let segments = c
            .blocks
            .iter()
            .map(|b| match b {
                GitConflictBlock::Common { text } => ConflictSegment::Common(text.clone()),
                GitConflictBlock::Conflict { ours, theirs, ours_label, theirs_label } => ConflictSegment::Block {
                    ours_label: ours_label.clone(),
                    theirs_label: theirs_label.clone(),
                    ours: ours.clone(),
                    theirs: theirs.clone(),
                    choice: ConflictChoice::Unresolved,
                },
            })
            .collect();
        ConflictFile { path: path.to_string(), segments }
    }

    /// How many conflict blocks the file has.
    pub fn block_count(&self) -> usize {
        self.segments.iter().filter(|s| matches!(s, ConflictSegment::Block { .. })).count()
    }

    /// How many blocks have a choice.
    pub fn resolved_count(&self) -> usize {
        self.segments.iter().filter(|s| matches!(s, ConflictSegment::Block { choice, .. } if *choice != ConflictChoice::Unresolved)).count()
    }

    /// True when every block has a choice.
    pub fn is_resolved(&self) -> bool {
        self.resolved_count() == self.block_count()
    }

    /// The same file with block number `block` (counting blocks only) set to `choice`. An index
    /// past the last block changes nothing.
    pub fn with_choice(&self, block: usize, choice: ConflictChoice) -> ConflictFile {
        let mut n = 0;
        let segments = self
            .segments
            .iter()
            .map(|seg| match seg {
                ConflictSegment::Block { ours_label, theirs_label, ours, theirs, choice: old } => {
                    let this = n;
                    n += 1;
                    ConflictSegment::Block {
                        ours_label: ours_label.clone(),
                        theirs_label: theirs_label.clone(),
                        ours: ours.clone(),
                        theirs: theirs.clone(),
                        choice: if this == block { choice } else { *old },
                    }
                }
                other => other.clone(),
            })
            .collect();
        ConflictFile { path: self.path.clone(), segments }
    }

    /// The file text with each block replaced by its choice (Both is ours, then theirs). `None`
    /// while a block is unresolved, so a half-settled file is never sent. Text is joined as it
    /// came, so a missing trailing newline stays missing.
    pub fn resolved_content(&self) -> Option<String> {
        if !self.is_resolved() {
            return None;
        }
        let mut out = String::new();
        for seg in &self.segments {
            match seg {
                ConflictSegment::Common(t) => out.push_str(t),
                ConflictSegment::Block { ours, theirs, choice, .. } => match choice {
                    ConflictChoice::Ours => out.push_str(ours),
                    ConflictChoice::Theirs => out.push_str(theirs),
                    ConflictChoice::Both => {
                        out.push_str(ours);
                        out.push_str(theirs);
                    }
                    ConflictChoice::Unresolved => return None,
                },
            }
        }
        Some(out)
    }

    /// "N/M 블록 해결".
    pub fn progress_text(&self) -> String {
        format!("{}/{} 블록 해결", self.resolved_count(), self.block_count())
    }
}

/// How many lines of the merged text the preview shows before "... N줄 더 있습니다".
pub const CONFLICT_PREVIEW_LINES: usize = 40;

/// Which step the conflict editor is on (FR-G7): picking per block, or looking at the merged
/// text before it is written and staged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictStep {
    Editing,
    /// The merged text cut to `CONFLICT_PREVIEW_LINES` lines.
    Preview,
    /// The whole merged text (전체 보기).
    PreviewFull,
}

/// What a tap on 해결하고 스테이징 does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveTap {
    Ignore,
    /// Send this merged text to `git/resolve`.
    Send(String),
}

/// The decision of a 해결하고 스테이징 tap: nothing while a request is in flight (a double tap
/// sends once) or while a block is unchosen, else the merged text to send.
pub fn resolve_tap(busy: bool, file: &ConflictFile) -> ResolveTap {
    if busy {
        return ResolveTap::Ignore;
    }
    match file.resolved_content() {
        Some(text) => ResolveTap::Send(text),
        None => ResolveTap::Ignore,
    }
}

/// 해결하고 스테이징 on `step`: only a preview step sends.
pub fn confirm(step: ConflictStep, busy: bool, file: &ConflictFile) -> ResolveTap {
    if step.is_preview() { resolve_tap(busy, file) } else { ResolveTap::Ignore }
}

/// 취소: back to editing. The choices live in the file, so they are kept.
pub fn cancel_preview(_step: ConflictStep) -> ConflictStep {
    ConflictStep::Editing
}

impl ConflictStep {
    /// The step after tapping 해결 완료: the preview when every block is settled, else unchanged.
    pub fn enter_preview(self, file: &ConflictFile) -> ConflictStep {
        if file.is_resolved() { ConflictStep::Preview } else { self }
    }

    /// Whether the merged text is showing, cut or whole.
    pub fn is_preview(self) -> bool {
        matches!(self, ConflictStep::Preview | ConflictStep::PreviewFull)
    }

    /// How many lines the preview shows on this step.
    pub fn preview_max(self) -> usize {
        if self == ConflictStep::PreviewFull { usize::MAX } else { CONFLICT_PREVIEW_LINES }
    }

    /// 전체 보기 and 접기: flips between the cut and the whole text; editing is unchanged.
    pub fn toggle_full(self) -> ConflictStep {
        match self {
            ConflictStep::Preview => ConflictStep::PreviewFull,
            ConflictStep::PreviewFull => ConflictStep::Preview,
            ConflictStep::Editing => ConflictStep::Editing,
        }
    }
}

/// The first `max` lines of `content` and how many lines were left out.
pub fn preview_lines(content: &str, max: usize) -> (String, usize) {
    let total = content.lines().count();
    if total <= max {
        return (content.to_string(), 0);
    }
    (content.lines().take(max).collect::<Vec<_>>().join("\n"), total - max)
}

/// A block button's text: the chosen one starts with "선택됨 · ", so the choice is not colour-only.
pub fn choice_label(text: &str, selected: bool) -> String {
    if selected { format!("선택됨 · {text}") } else { text.to_string() }
}

impl ConflictFile {
    /// The preview of the merged text, `None` while a block is unresolved.
    pub fn preview(&self, max: usize) -> Option<(String, usize)> {
        self.resolved_content().map(|c| preview_lines(&c, max))
    }
}

/// The state of the conflict editor's file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictLoad {
    Loading,
    Ready(ConflictFile),
    /// The Korean line with the node's message (shown with a retry button).
    Failed(String),
}

/// The editor state from the node's answer.
pub fn conflict_from(path: &str, result: Result<GitConflict, ApiError>) -> ConflictLoad {
    match result {
        Ok(c) => ConflictLoad::Ready(ConflictFile::from_node(path, &c)),
        Err(e) => ConflictLoad::Failed(failure_text("충돌 내용을 확인", &e)),
    }
}

/// Read one unmerged file's blocks. Runs on the runtime, not the UI thread.
pub async fn fetch_conflict(project: &str, path: &str) -> ConflictLoad {
    conflict_from(path, git_call::<GitConflict>(project, "conflict", json!({ "path": path })).await)
}

/// Write the settled content and stage the file. `Err` is the Korean line to show (the node's
/// message when it sent one; a file that is not unmerged answers 409).
pub async fn run_resolve(project: &str, path: &str, content: &str) -> Result<(), String> {
    git_call::<Value>(project, "resolve", json!({ "path": path, "content": content }))
        .await
        .map(|_| ())
        .map_err(|e| failure_text("충돌 해결을 ", &e))
}

/// The merge in progress, as the banner on the conflict screen shows it (FR-G16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeView {
    /// The branch merged in; empty when the node does not know it.
    pub from: String,
    /// The current branch; empty on a detached HEAD.
    pub into: String,
    /// Files that still conflict.
    pub remaining: usize,
    /// True when no unmerged path is left, so 병합 커밋 can run.
    pub can_commit: bool,
}

/// The state of the merge banner's read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeLoad {
    Idle,
    Loading,
    /// `None` when no merge is in progress (a stash pop or a cherry-pick shows no banner).
    Ready(Option<MergeView>),
    Failed(String),
}

/// The banner state from the node's answer.
pub fn merge_view_from(result: Result<GitMergeState, ApiError>) -> MergeLoad {
    match result {
        Ok(m) if m.in_progress => MergeLoad::Ready(Some(MergeView {
            from: m.from.unwrap_or_default(),
            into: m.into.unwrap_or_default(),
            remaining: m.conflict_files as usize,
            can_commit: m.can_commit,
        })),
        Ok(_) => MergeLoad::Ready(None),
        Err(e) => MergeLoad::Failed(failure_text("병합 상태를 확인", &e)),
    }
}

/// Read the merge state. Runs on the runtime, not the UI thread.
pub async fn fetch_merge_state(project: &str) -> MergeLoad {
    merge_view_from(git_call::<GitMergeState>(project, "merge", json!({})).await)
}

/// The banner's title: "feat/x \u{2192} main 병합 중"; "병합 중" when a name is unknown.
pub fn banner_title(v: &MergeView) -> String {
    if v.from.is_empty() || v.into.is_empty() {
        "병합 중".to_string()
    } else {
        format!("{} \u{2192} {} 병합 중", v.from, v.into)
    }
}

/// The banner's second line.
pub fn banner_sub(remaining: usize) -> String {
    if remaining == 0 { "모든 충돌을 해결했습니다".to_string() } else { format!("남은 충돌 파일 {remaining}개") }
}

/// The line shown when a merge call fails with a message that is not Korean.
pub const MERGE_FAILED_LINE: &str = "작업을 하지 못했습니다. 저장소 상태를 확인해 주세요.";
/// Under 병합 취소.
pub const MERGE_ABORT_NOTE: &str = "병합 중이던 변경을 모두 되돌립니다";
/// Under 병합 커밋 once armed (git includes the index).
pub const MERGE_COMMIT_NOTE: &str = "이미 스테이징해 둔 다른 변경도 이 커밋에 들어갑니다";

/// The two buttons of the banner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeAction {
    Abort,
    Commit,
}

/// The key of a banner button in the armed state; one key per action.
pub fn merge_tap_key(action: MergeAction) -> usize {
    match action {
        MergeAction::Abort => 1,
        MergeAction::Commit => 2,
    }
}

/// What a tap on a banner button does (two taps: arm, then confirm after 600 ms, within 4 s).
pub fn merge_tap(action: MergeAction, armed: Option<(usize, i64)>, now: i64) -> DeleteTap {
    delete_tap(armed, merge_tap_key(action), now)
}

/// The visible text of a banner button.
pub fn banner_button_text(action: MergeAction, armed: bool) -> &'static str {
    match (action, armed) {
        (MergeAction::Abort, false) => "병합 취소",
        (MergeAction::Abort, true) => "정말 취소합니다",
        (MergeAction::Commit, false) => "병합 커밋",
        (MergeAction::Commit, true) => "정말 커밋합니다",
    }
}

/// The Korean line for a failed merge call: the node's own fixed Korean line as given, else a
/// fixed Korean line (git's English text never reaches the screen).
pub fn merge_failure(e: &ApiError) -> String {
    match e {
        ApiError::Status { message, .. } if is_korean(message) => message.clone(),
        ApiError::Status { .. } => MERGE_FAILED_LINE.to_string(),
        other => korean_failure("병합 작업을", other),
    }
}

/// The notice after a merge commit.
pub fn merge_commit_notice(sha: &str) -> String {
    let short: String = sha.chars().take(7).collect();
    format!("병합 커밋 {short}을 만들었습니다")
}

/// The result of the abort call: `Ok` is the notice, `Err` the Korean line.
pub fn merge_abort_result(r: Result<ember_client::api::GitMergeAborted, ApiError>) -> Result<String, String> {
    r.map(|_| "병합을 취소했습니다".to_string()).map_err(|e| merge_failure(&e))
}

/// The result of the commit call.
pub fn merge_commit_result(r: Result<GitCommitApplied, ApiError>) -> Result<String, String> {
    r.map(|c| merge_commit_notice(&c.new_sha)).map_err(|e| merge_failure(&e))
}

/// Cancel the merge in progress. Runs on the runtime, not the UI thread.
pub async fn run_merge_abort(project: &str) -> Result<String, String> {
    merge_abort_result(git_call(project, "merge/abort", json!({ "confirm": true })).await)
}

/// Finish the merge with a merge commit. Runs on the runtime, not the UI thread.
pub async fn run_merge_commit(project: &str) -> Result<String, String> {
    merge_commit_result(git_call(project, "merge/commit", json!({ "confirm": true })).await)
}

impl ConflictFile {
    /// The same file with every block set to `choice` (모두 현재 것 / 모두 들어오는 것).
    pub fn pick_all(&self, choice: ConflictChoice) -> ConflictFile {
        let segments = self
            .segments
            .iter()
            .map(|seg| match seg {
                ConflictSegment::Block { ours_label, theirs_label, ours, theirs, .. } => ConflictSegment::Block {
                    ours_label: ours_label.clone(),
                    theirs_label: theirs_label.clone(),
                    ours: ours.clone(),
                    theirs: theirs.clone(),
                    choice,
                },
                other => other.clone(),
            })
            .collect();
        ConflictFile { path: self.path.clone(), segments }
    }
}

/// The line under 모두 현재 것 / 모두 들어오는 것 when a block already has a choice.
pub const PICK_ALL_NOTE: &str = "이미 고른 블록의 선택이 바뀝니다";

/// The note under the pick-all chips: `Some` only when some block is already chosen, because
/// only then does one tap replace a choice the user made.
pub fn pick_all_note(file: &ConflictFile) -> Option<&'static str> {
    if file.resolved_count() > 0 { Some(PICK_ALL_NOTE) } else { None }
}

/// The key of a pick-all button in the armed state (`delete_tap`); one key per choice, so only
/// one of the two can be armed at a time.
pub fn pick_all_key(choice: ConflictChoice) -> usize {
    match choice {
        ConflictChoice::Ours => 1,
        ConflictChoice::Theirs => 2,
        _ => 0,
    }
}

/// The visible text of a pick-all button; the armed second tap asks again.
pub fn pick_all_text(base: &'static str, armed: bool) -> &'static str {
    if armed { "정말 모두 바꿉니다" } else { base }
}

/// Whether the answer of a merge abort or commit makes the screen read the state again. Both
/// answers do: after an error (a 409 means the state on screen was stale) as after success.
pub fn merge_answer_reloads(_result: &Result<String, String>) -> bool {
    true
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
        assert_eq!(section_count(Section::Stash, Some(&snap)), None, "no stash read yet");
        assert_eq!(section_count_with_stash(Section::Stash, Some(&snap), Some(2)), Some(2));
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
        let not_repo = ApiError::Status { status: 404, message: "not_a_repo".into(), code: None };
        assert_eq!(load_from(Err(not_repo)), Load::NotRepo);
        let down = ApiError::Peer("refused".into());
        assert!(matches!(load_from(Err(down)), Load::Unavailable(_)));
        // Any other answer of the node is a failure with its message, not "준비 중입니다".
        let boom = ApiError::Status { status: 500, message: "boom".into(), code: None };
        let failed = load_from(Err(boom));
        assert!(matches!(&failed, Load::Failed(t) if t.contains("boom")), "{failed:?}");
        assert_ne!(failed.note(), Some(soon_line("저장소 정보")));
        let busy = ApiError::Status { status: 409, message: "병합 중이라 차이를 읽지 못했습니다: x".into(), code: None };
        assert_eq!(load_from(Err(busy)), Load::Failed("병합 중이라 차이를 읽지 못했습니다: x".into()));
        assert!(matches!(load_from(Err(ApiError::Decode("bad".into()))), Load::Failed(_)));
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
    fn fr_g3_a_413_on_untracked_keeps_the_tracked_changes_and_notes_it() {
        let too_big = ApiError::Status { status: 413, message: "출력이 너무 큽니다".into(), code: None };
        let (patch, note) = merge_untracked_diff(Ok(SAMPLE.to_string()), Err(too_big)).unwrap();
        assert_eq!(patch, SAMPLE);
        assert_eq!(note, Some("추적하지 않는 파일이 너무 많아 내용을 건너뛰었습니다. 파일 목록만 표시합니다."));
        let (patch, note) = merge_untracked_diff(Ok(String::new()), Ok("full".to_string())).unwrap();
        assert_eq!((patch.as_str(), note), ("full", None));
        // The untracked files stay listed by the status, with no hunks.
        let st = status(vec![gf("src/a.rs", '.', 'M'), gf("notes.txt", '?', '?')]);
        let snap = build_snapshot(&st, SAMPLE, "", &BTreeMap::new());
        assert!(snap.changes.iter().any(|d| d.path == "notes.txt" && d.hunks.is_empty()));
    }

    #[test]
    fn fr_g3_other_untracked_errors_still_fail() {
        let boom = ApiError::Status { status: 500, message: "boom".into(), code: None };
        assert!(merge_untracked_diff(Ok(String::new()), Err(boom)).is_err());
        let tracked_fail = ApiError::Status { status: 413, message: "x".into(), code: None };
        let too_big = ApiError::Status { status: 413, message: "y".into(), code: None };
        assert!(merge_untracked_diff(Err(tracked_fail), Err(too_big)).is_err(), "no tracked diff to keep");
    }

    #[test]
    fn fr_g3_amend_warns_when_head_is_on_the_upstream() {
        let snap = RepoSnapshot { upstream: Some("origin/x".into()), ahead: 0, ..Default::default() };
        let text = amend_warning(&snap).unwrap();
        assert_eq!(text, "이 커밋은 이미 원격에 올라가 있습니다. 수정하면 다음 올리기에서 기록을 덮어써야 합니다.");
        assert_eq!(amend_button_label("main에 커밋", Some(text), false), "main에 커밋");
        assert_eq!(amend_button_label("main에 커밋", Some(text), true), "그래도 수정합니다");
    }

    #[test]
    fn fr_g3_amend_does_not_warn_when_ahead_or_no_upstream() {
        let ahead = RepoSnapshot { upstream: Some("origin/x".into()), ahead: 1, ..Default::default() };
        let none = RepoSnapshot { upstream: None, ahead: 0, ..Default::default() };
        assert_eq!(amend_warning(&ahead), None);
        assert_eq!(amend_warning(&none), None);
        assert_eq!(amend_button_label("main에 커밋", None, true), "main에 커밋");
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
        assert!(patch.ends_with("@@ -20,1 +19,2 @@\n x\n+y\n\\ No newline at end of file\n"), "{patch}");
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
        let e = ApiError::Status { status: 409, message: "not fast-forward".into(), code: None };
        assert_eq!(failure_text("Pull", &e), "Pull하지 못했습니다: not fast-forward");
        let down = ApiError::Peer("refused".into());
        assert!(failure_text("Push", &down).contains("서버에 닿지 않습니다"));
    }

    #[test]
    fn a_korean_message_from_the_node_is_not_wrapped_again() {
        let e = ApiError::Status {
            status: 409,
            message: "분기되어 있어 pull할 수 없습니다: fatal: Not possible to fast-forward".into(),
            code: None,
        };
        assert_eq!(
            failure_text("Pull", &e),
            "분기되어 있어 pull할 수 없습니다: fatal: Not possible to fast-forward"
        );
    }

    #[test]
    fn the_commit_body_sends_the_description_or_null() {
        let mut req = CommitRequest {
            ops: Vec::new(),
            summary: "Fix: x".into(),
            description: "  왜 고쳤는지  ".into(),
            amend: false,
        };
        assert_eq!(
            commit_body(&req),
            json!({ "summary": "Fix: x", "description": "왜 고쳤는지", "amend": false })
        );
        req.description = "  ".into();
        assert_eq!(commit_body(&req)["description"], Value::Null);
    }

    #[test]
    fn the_diff_body_asks_for_untracked_files_in_the_work_tree_patch_only() {
        assert_eq!(
            diff_body(false, true),
            json!({ "path": null, "staged": false, "include_untracked": true })
        );
        assert_eq!(diff_body(true, false)["include_untracked"], false);
    }

    #[test]
    fn the_repo_card_does_not_say_unconfigured_while_data_shows() {
        let p = boxed("demo-project");
        assert_eq!(repo_card_line(&p, None), "저장소가 설정되지 않았습니다");
        let snap = RepoSnapshot { branch: "feat/x".into(), ..Default::default() };
        assert_eq!(repo_card_line(&p, Some(&snap)), "git 저장소 \u{00b7} feat/x");
        let mut q = boxed("core");
        q.repo = Some("github.com/x/y".into());
        assert_eq!(repo_card_line(&q, Some(&snap)), "github.com/x/y");
    }

    #[test]
    fn the_conflicts_section_counts_its_files() {
        let snap = RepoSnapshot { conflicts: s(&["a.rs", "b.rs"]), ..Default::default() };
        assert_eq!(section_count(Section::Conflicts, Some(&snap)), Some(2));
        assert_eq!(section_count(Section::Conflicts, None), None);
    }

    // ---- quoted headers, spaces, modes, no newline ----

    #[test]
    fn quoted_headers_with_octal_escapes_are_decoded() {
        // 한 = ED 95 9C, 글 = EA B8 80.
        let d = "diff --git \"a/\\355\\225\\234\\352\\270\\200.txt\" \"b/\\355\\225\\234\\352\\270\\200.txt\"\n\
                 index 1..2 100644\n--- \"a/\\355\\225\\234\\352\\270\\200.txt\"\n+++ \"b/\\355\\225\\234\\352\\270\\200.txt\"\n\
                 @@ -1 +1 @@\n-a\n+b\n";
        let f = parse_unified(d);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].path, "한글.txt");
        assert_eq!(f[0].old_path.as_deref(), Some("한글.txt"));
        assert_eq!(f[0].hunks.len(), 1);
    }

    #[test]
    fn quoted_names_with_a_quote_a_tab_or_a_space_parse() {
        let d = "diff --git \"a/we\\\"ird\\tname.txt\" \"b/we\\\"ird\\tname.txt\"\nnew file mode 100644\n";
        let f = parse_unified(d);
        assert_eq!(f[0].path, "we\"ird\tname.txt");
        assert_eq!(f[0].status, FileStatus::Added);
        // A rename whose names are quoted.
        let r = parse_unified("diff --git \"a/\\355\\225\\234 x\" \"b/y z\"\nrename from \"\\355\\225\\234 x\"\nrename to \"y z\"\n");
        assert_eq!(r[0].path, "y z");
        assert_eq!(r[0].old_path.as_deref(), Some("한 x"));
        assert_eq!(r[0].status, FileStatus::Renamed);
    }

    #[test]
    fn unquoted_names_with_spaces_parse_with_and_without_hunks() {
        let d = "diff --git a/my docs/read me.md b/my docs/read me.md\nnew file mode 100755\n\
                 --- /dev/null\n+++ b/my docs/read me.md\t\n@@ -0,0 +1 @@\n+hi\n\
                 diff --git a/한 글/파일 b.txt b/한 글/파일 b.txt\nold mode 100644\nnew mode 100755\n";
        let f = parse_unified(d);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, "my docs/read me.md");
        assert_eq!(f[0].mode.as_deref(), Some("100755"));
        // No hunks and no `+++`: the path comes from the header alone.
        assert_eq!(f[1].path, "한 글/파일 b.txt");
        assert_eq!(f[1].mode.as_deref(), Some("100755"));
        assert!(f[1].hunks.is_empty());
    }

    #[test]
    fn the_patch_keeps_the_real_file_mode_of_a_new_file() {
        let d = "diff --git a/run.sh b/run.sh\nnew file mode 100755\n--- /dev/null\n+++ b/run.sh\n@@ -0,0 +1,2 @@\n+#!/bin/sh\n+echo\n";
        let f = parse_unified(d).remove(0);
        let mut st = FileStaging::none();
        st.toggle_line(&f, 0, 0);
        let patch = build_patch(&f, &st).unwrap();
        assert!(patch.starts_with("diff --git a/run.sh b/run.sh\nnew file mode 100755\n--- /dev/null\n"), "{patch}");
        // Without a mode line in the diff the default is a plain file.
        let mut plain = f.clone();
        plain.mode = None;
        assert!(build_patch(&plain, &st).unwrap().contains("new file mode 100644\n"));
    }

    #[test]
    fn a_missing_final_newline_is_kept_in_the_patch_and_never_invented() {
        let d = "diff --git a/n.txt b/n.txt\n--- a/n.txt\n+++ b/n.txt\n@@ -1,2 +1,2 @@\n keep\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n";
        let f = parse_unified(d).remove(0);
        let lines = &f.hunks[0].lines;
        assert_eq!(lines.iter().map(|l| l.no_eol).collect::<Vec<_>>(), [false, true, true]);
        let mut st = FileStaging::none();
        st.toggle_hunk(&f, 0);
        let patch = build_patch(&f, &st).unwrap();
        assert!(
            patch.ends_with("@@ -1,2 +1,2 @@\n keep\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n"),
            "{patch}"
        );
        // A line with a newline gets no marker.
        let with_eol = parse_unified("--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n").remove(0);
        let mut all = FileStaging::none();
        all.toggle_hunk(&with_eol, 0);
        assert!(!build_patch(&with_eol, &all).unwrap().contains("No newline"));
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

    // ---- FR-G5: history ----

    fn commit(sha: &str, parents: &[&str], time: i64, refs: &[(&str, GitLogRefKind)]) -> GitLogCommit {
        GitLogCommit {
            sha: sha.into(),
            parents: s(parents),
            author: "BREW".into(),
            email: "brew@example.com".into(),
            time,
            subject: "Feat: x".into(),
            refs: refs
                .iter()
                .map(|(n, k)| ember_client::api::GitLogRef { name: n.to_string(), kind: k.clone() })
                .collect(),
        }
    }

    #[test]
    fn fr_g5_an_entry_has_a_short_sha_and_the_commit_fields() {
        let e = history_entry(&commit("0123456789abcdef", &["aaa"], 1_000, &[]), 1_000 + 90);
        assert_eq!(e.short, "0123456");
        assert_eq!(e.sha, "0123456789abcdef");
        assert_eq!(e.subject, "Feat: x");
        assert_eq!(e.author, "BREW");
        assert_eq!(e.time_text, "1분 전");
        assert!(!e.is_merge);
        assert!(e.refs.is_empty());
    }

    #[test]
    fn fr_g5_a_commit_with_two_parents_is_a_merge() {
        let merge = history_entry(&commit("m1m1m1m1", &["a", "b"], 0, &[]), 0);
        let root = history_entry(&commit("r1r1r1r1", &[], 0, &[]), 0);
        assert!(merge.is_merge);
        assert!(!root.is_merge);
    }

    #[test]
    fn fr_g5_ref_chips_are_short_and_ordered_head_branch_remote_tag() {
        let c = commit(
            "abcdef012",
            &["p"],
            0,
            &[
                ("refs/tags/v1", GitLogRefKind::Tag),
                ("refs/remotes/origin/main", GitLogRefKind::Remote),
                ("refs/heads/main", GitLogRefKind::Branch),
                ("HEAD", GitLogRefKind::Head),
            ],
        );
        let labels: Vec<String> = history_entry(&c, 0).refs.into_iter().map(|r| r.label).collect();
        assert_eq!(labels, s(&["HEAD", "main", "origin/main", "v1"]));
        assert_eq!(ref_label("refs/heads/feat/a"), "feat/a");
    }

    #[test]
    fn fr_g5_commit_times_are_relative() {
        assert_eq!(history_time_text(100, 100), "방금 전");
        assert_eq!(history_time_text(100, 500), "방금 전");
        assert_eq!(history_time_text(3_600, 0), "1시간 전");
        assert_eq!(history_time_text(86_400 * 3, 0), "3일 전");
        assert_eq!(history_time_text(86_400 * 60, 0), "2개월 전");
        assert_eq!(history_time_text(86_400 * 800, 0), "2년 전");
    }

    #[test]
    fn fr_g5_an_empty_log_is_an_empty_ready_list_without_more() {
        // An unborn branch answers an empty list, which is not an error.
        let load = history_from(Ok(GitLog { commits: Vec::new() }), 0);
        assert_eq!(load, HistoryLoad::Ready { entries: Vec::new(), more: false, loading_more: false });
    }

    #[test]
    fn fr_g5_a_full_page_may_have_more_and_a_short_page_does_not() {
        let page = |n: usize| GitLog { commits: (0..n).map(|i| commit(&format!("{i:040}"), &[], 0, &[])).collect() };
        let HistoryLoad::Ready { more, .. } = history_from(Ok(page(HISTORY_PAGE as usize)), 0) else { panic!() };
        assert!(more);
        let HistoryLoad::Ready { more, .. } = history_from(Ok(page(3)), 0) else { panic!() };
        assert!(!more);
    }

    #[test]
    fn fr_g5_the_next_page_is_appended_and_a_failure_keeps_the_shown_pages() {
        let first = history_from(Ok(GitLog { commits: vec![commit("a0a0a0a0", &[], 0, &[])] }), 0);
        let (next, err) = history_append(first.clone(), Ok(GitLog { commits: vec![commit("b0b0b0b0", &[], 0, &[])] }), 0);
        assert!(err.is_none());
        let HistoryLoad::Ready { entries, more, .. } = &next else { panic!() };
        assert_eq!(entries.iter().map(|e| e.short.as_str()).collect::<Vec<_>>(), ["a0a0a0a", "b0b0b0b"]);
        assert!(!more);

        let down = ApiError::Status { status: 500, message: "boom".into(), code: None };
        let (kept, err) = history_append(first, Err(down), 0);
        assert!(err.unwrap().contains("boom"));
        let HistoryLoad::Ready { entries, more, .. } = kept else { panic!() };
        assert_eq!(entries.len(), 1);
        assert!(more, "the user may retry the page");
    }

    #[test]
    fn fr_g5_a_failed_first_page_shows_the_nodes_message() {
        let e = ApiError::Status { status: 500, message: "로그를 읽지 못했습니다".into(), code: None };
        assert_eq!(history_from(Err(e), 0), HistoryLoad::Failed("로그를 읽지 못했습니다".into()));
    }

    #[test]
    fn fr_g5_the_log_body_pages_with_skip() {
        assert_eq!(log_body(60), json!({ "limit": HISTORY_PAGE, "skip": 60, "branch": null }));
    }

    // ---- FR-G6: stash ----

    fn stash(index: u32, message: &str, time: &str) -> GitStashEntry {
        GitStashEntry { index, message: message.into(), branch: "main".into(), time: time.into() }
    }

    #[test]
    fn fr_g6_iso_times_parse_with_offsets() {
        assert_eq!(iso_to_unix("1970-01-01T00:00:00+00:00"), Some(0));
        assert_eq!(iso_to_unix("1970-01-01T09:00:00+09:00"), Some(0));
        assert_eq!(iso_to_unix("1970-01-02T00:00:00Z"), Some(86_400));
        assert_eq!(iso_to_unix("2026-08-29T10:40:00+00:00"), Some(1_788_000_000));
        assert_eq!(iso_to_unix("2026-08-29T05:40:00-05:00"), Some(1_788_000_000));
        assert_eq!(iso_to_unix("not a time"), None);
        assert_eq!(iso_to_unix("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn fr_g6_an_entry_has_the_stash_label_message_branch_and_relative_time() {
        let e = stash_entry(&stash(2, "작업 중", "2026-08-29T10:40:00+00:00"), 1_788_000_000 + 7_200);
        assert_eq!(e.label, "stash@{2}");
        assert_eq!(e.message, "작업 중");
        assert_eq!(e.branch, "main");
        assert_eq!(e.time_text, "2시간 전");
    }

    #[test]
    fn fr_g6_a_blank_message_and_an_unreadable_time_still_show() {
        let e = stash_entry(&stash(0, "  ", "yesterday"), 0);
        assert_eq!(e.message, "(메시지 없음)");
        assert_eq!(e.time_text, "yesterday");
    }

    #[test]
    fn fr_g6_an_empty_list_is_an_empty_ready_state() {
        assert_eq!(stash_from(Ok(Vec::new()), 0), StashLoad::Ready(Vec::new()));
    }

    #[test]
    fn fr_g6_a_failed_list_shows_the_nodes_message() {
        let e = ApiError::Status { status: 500, message: "스태시를 읽지 못했습니다".into(), code: None };
        assert_eq!(stash_from(Err(e), 0), StashLoad::Failed("스태시를 읽지 못했습니다".into()));
    }

    #[test]
    fn fr_g6_the_push_body_sends_null_for_a_blank_message() {
        assert_eq!(stash_push_body("   ", true), json!({ "message": null, "include_untracked": true }));
        assert_eq!(stash_push_body(" 내 작업 ", false), json!({ "message": "내 작업", "include_untracked": false }));
    }

    #[test]
    fn fr_g6_a_failed_pop_keeps_the_list_and_says_why() {
        let shown = stash_from(Ok(vec![stash(0, "a", "2026-08-29T10:40:00Z")]), 0);
        let down = ApiError::Status { status: 409, message: "충돌 때문에 꺼낼 수 없습니다. 스태시는 그대로 남아 있습니다.".into(), code: None };
        let result: Result<(), String> = Err(failure_text(StashAction::Pop.what(), &down));
        let after = stash_after_action(shown.clone(), &result, StashLoad::Ready(Vec::new()));
        assert_eq!(after, shown);
        assert!(stash_action_notice(StashAction::Pop, &result).contains("스태시는 그대로"));
        let ok: Result<(), String> = Ok(());
        assert_eq!(stash_after_action(shown, &ok, StashLoad::Ready(Vec::new())), StashLoad::Ready(Vec::new()));
        assert_eq!(stash_action_notice(StashAction::Drop, &ok), "스태시를 삭제했습니다");
    }

    #[test]
    fn fr_g6_a_non_korean_failure_gets_a_korean_prefix() {
        let e = ApiError::Status { status: 500, message: "boom".into(), code: None };
        assert_eq!(failure_text(StashAction::Apply.what(), &e), "스태시를 적용하지 못했습니다: boom");
    }

    #[test]
    fn fr_g6_delete_arms_then_confirms() {
        assert_eq!(delete_tap(None, 1, 10_000), DeleteTap::Arm);
        assert_eq!(delete_tap(Some((1, 10_000)), 1, 10_100), DeleteTap::Wait);
        assert_eq!(delete_tap(Some((1, 10_000)), 1, 10_600), DeleteTap::Confirm);
        assert_eq!(delete_tap(Some((1, 10_000)), 1, 13_999), DeleteTap::Confirm);
        assert!(armed_label_visible(Some((1, 10_000)), 1, 10_000));
    }

    #[test]
    fn fr_g6_delete_disarms_after_the_timeout() {
        assert_eq!(delete_tap(Some((1, 10_000)), 1, 14_000), DeleteTap::Expired);
        assert!(!armed_label_visible(Some((1, 10_000)), 1, 14_000));
        assert!(armed_label_visible(Some((1, 10_000)), 1, 13_999));
        assert!(!armed_label_visible(None, 1, 0));
    }

    #[test]
    fn fr_g6_delete_on_another_row_rearms() {
        assert_eq!(delete_tap(Some((1, 10_000)), 2, 10_900), DeleteTap::Arm);
        assert!(!armed_label_visible(Some((1, 10_000)), 2, 10_900));
    }

    #[test]
    fn fr_g6_apply_asks_once_when_the_tree_is_dirty() {
        let key = stash_tap_key(StashAction::Apply, 0);
        assert_eq!(stash_tap(StashAction::Apply, true, None, 0, 1_000), DeleteTap::Arm);
        assert_eq!(stash_tap(StashAction::Apply, true, Some((key, 1_000)), 0, 1_700), DeleteTap::Confirm);
        assert_eq!(stash_tap(StashAction::Pop, true, Some((key, 1_000)), 0, 1_700), DeleteTap::Arm, "pop is its own button");
        assert_eq!(stash_button_text(StashAction::Apply, true), "그래도 적용합니다");
        assert_eq!(stash_button_text(StashAction::Pop, true), "그래도 꺼냅니다");
        assert_eq!(STASH_DIRTY_WARNING, "작업 중인 변경이 있습니다. 충돌할 수 있습니다.");
        assert!(STASH_POP_NOTE.contains("삭제"), "the pop warning says the stash is removed");
    }

    #[test]
    fn fr_g6_apply_is_immediate_when_the_tree_is_clean() {
        assert_eq!(stash_tap(StashAction::Apply, false, None, 0, 1_000), DeleteTap::Confirm);
        assert_eq!(stash_tap(StashAction::Pop, false, None, 3, 1_000), DeleteTap::Confirm);
        assert_eq!(stash_tap(StashAction::Drop, false, None, 3, 1_000), DeleteTap::Arm, "delete always asks");
    }

    #[test]
    fn fr_g6_stash_button_labels_name_the_entry() {
        assert_eq!(stash_button_label(0, "작업 중", "적용"), "스태시 0 적용: 작업 중");
        assert_ne!(stash_button_label(1, "a", "삭제"), stash_button_label(2, "a", "삭제"));
    }

    fn block(ours: &str, theirs: &str) -> GitConflictBlock {
        GitConflictBlock::Conflict { ours: ours.into(), theirs: theirs.into(), ours_label: "HEAD".into(), theirs_label: "origin/main".into() }
    }

    fn two_blocks() -> ConflictFile {
        let c = GitConflict {
            blocks: vec![
                GitConflictBlock::Common { text: "top\n".into() },
                block("o1\n", "t1\n"),
                GitConflictBlock::Common { text: "mid\n".into() },
                block("o2\n", "t2\n"),
                GitConflictBlock::Common { text: "end".into() },
            ],
        };
        ConflictFile::from_node("a.rs", &c)
    }

    #[test]
    fn fr_g7_blocks_parse_into_unresolved_segments_with_labels() {
        let f = two_blocks();
        assert_eq!(f.block_count(), 2);
        assert_eq!(f.resolved_count(), 0);
        assert!(!f.is_resolved());
        assert_eq!(f.progress_text(), "0/2 블록 해결");
        let ConflictSegment::Block { ours_label, theirs_label, choice, .. } = &f.segments[1] else { panic!() };
        assert_eq!((ours_label.as_str(), theirs_label.as_str()), ("HEAD", "origin/main"));
        assert_eq!(*choice, ConflictChoice::Unresolved);
    }

    #[test]
    fn fr_g7_all_ours_all_theirs_and_both_join_the_segments() {
        let f = two_blocks();
        let ours = f.with_choice(0, ConflictChoice::Ours).with_choice(1, ConflictChoice::Ours);
        assert_eq!(ours.resolved_content().unwrap(), "top\no1\nmid\no2\nend");
        let theirs = f.with_choice(0, ConflictChoice::Theirs).with_choice(1, ConflictChoice::Theirs);
        assert_eq!(theirs.resolved_content().unwrap(), "top\nt1\nmid\nt2\nend");
        let both = f.with_choice(0, ConflictChoice::Both).with_choice(1, ConflictChoice::Theirs);
        assert_eq!(both.resolved_content().unwrap(), "top\no1\nt1\nmid\nt2\nend");
        assert_eq!(both.progress_text(), "2/2 블록 해결");
    }

    #[test]
    fn fr_g7_an_unresolved_block_refuses_to_produce_content() {
        let f = two_blocks().with_choice(1, ConflictChoice::Ours);
        assert!(!f.is_resolved());
        assert_eq!(f.resolved_count(), 1);
        assert_eq!(f.resolved_content(), None);
        assert_eq!(two_blocks().resolved_content(), None);
    }

    #[test]
    fn fr_g7_pick_all_asks_twice_when_a_choice_exists() {
        // The two buttons have their own keys, so arming one never confirms the other.
        assert_ne!(pick_all_key(ConflictChoice::Ours), pick_all_key(ConflictChoice::Theirs));
        let (ours, theirs) = (pick_all_key(ConflictChoice::Ours), pick_all_key(ConflictChoice::Theirs));
        assert_eq!(delete_tap(None, ours, 10_000), DeleteTap::Arm);
        assert_eq!(delete_tap(Some((ours, 10_000)), ours, 10_100), DeleteTap::Wait);
        assert_eq!(delete_tap(Some((ours, 10_000)), ours, 10_600), DeleteTap::Confirm);
        assert_eq!(delete_tap(Some((ours, 10_000)), theirs, 10_900), DeleteTap::Arm);
        assert_eq!(delete_tap(Some((ours, 10_000)), ours, 14_000), DeleteTap::Expired);
        assert!(armed_label_visible(Some((ours, 10_000)), ours, 10_000));
        assert!(!armed_label_visible(Some((ours, 10_000)), theirs, 10_000));
        assert_eq!(pick_all_text("모두 현재 것", true), "정말 모두 바꿉니다");
        assert_eq!(pick_all_text("모두 현재 것", false), "모두 현재 것");
    }

    #[test]
    fn fr_g7_pick_all_note_only_when_a_block_is_chosen() {
        let f = two_blocks();
        assert_eq!(pick_all_note(&f), None);
        assert_eq!(pick_all_note(&f.with_choice(1, ConflictChoice::Both)), Some("이미 고른 블록의 선택이 바뀝니다"));
    }

    #[test]
    fn fr_g17_a_409_answer_reloads_the_state() {
        let stale = ApiError::Status { status: 409, message: "병합 중이 아닙니다.".into(), code: None };
        let answer = merge_abort_result(Err(stale));
        assert!(answer.is_err());
        assert!(merge_answer_reloads(&answer));
        assert!(merge_answer_reloads(&Ok("병합을 취소했습니다".to_string())));
    }

    #[test]
    fn fr_g7_a_choice_can_change_and_a_bad_index_changes_nothing() {
        let f = two_blocks().with_choice(0, ConflictChoice::Ours);
        let again = f.with_choice(0, ConflictChoice::Theirs);
        assert_eq!(again.resolved_count(), 1);
        assert_eq!(f.with_choice(9, ConflictChoice::Both), f);
    }

    #[test]
    fn fr_g7_preview_cuts_long_text_and_counts_the_rest() {
        let long: String = (0..45).map(|i| format!("l{i}\n")).collect();
        let (text, more) = preview_lines(&long, 40);
        assert_eq!(text.lines().count(), 40);
        assert_eq!(more, 5);
        let (all, none) = preview_lines("a\nb\n", 40);
        assert_eq!((all.as_str(), none), ("a\nb\n", 0));
    }

    #[test]
    fn fr_g7_preview_is_unavailable_while_a_block_is_unresolved() {
        let f = two_blocks().with_choice(0, ConflictChoice::Ours);
        assert!(f.preview(40).is_none());
        assert_eq!(ConflictStep::Editing.enter_preview(&f), ConflictStep::Editing);
        let done = f.with_choice(1, ConflictChoice::Theirs);
        assert!(done.preview(40).is_some());
        assert_eq!(ConflictStep::Editing.enter_preview(&done), ConflictStep::Preview);
    }

    #[test]
    fn fr_g7_the_chosen_button_is_labelled_not_only_coloured() {
        assert_eq!(choice_label("내 변경", true), "선택됨 · 내 변경");
        assert_eq!(choice_label("내 변경", false), "내 변경");
    }

    #[test]
    fn fr_g7_text_without_a_trailing_newline_is_kept_as_is() {
        let c = GitConflict { blocks: vec![block("a", "b")] };
        let f = ConflictFile::from_node("x", &c).with_choice(0, ConflictChoice::Both);
        assert_eq!(f.resolved_content().unwrap(), "ab");
        let none = ConflictFile::from_node("x", &GitConflict { blocks: vec![GitConflictBlock::Common { text: "plain".into() }] });
        assert!(none.is_resolved());
        assert_eq!(none.resolved_content().unwrap(), "plain");
    }

    #[test]
    fn fr_g7_a_failed_read_shows_the_nodes_korean_message() {
        let e = ApiError::Status { status: 409, message: "병합 충돌 중인 파일이 아닙니다.".into(), code: None };
        assert_eq!(conflict_from("a", Err(e)), ConflictLoad::Failed("병합 충돌 중인 파일이 아닙니다.".into()));
        let e = ApiError::Status { status: 500, message: "boom".into(), code: None };
        assert_eq!(conflict_from("a", Err(e)), ConflictLoad::Failed("충돌 내용을 확인하지 못했습니다: boom".into()));
        assert!(matches!(conflict_from("a", Ok(GitConflict { blocks: vec![] })), ConflictLoad::Ready(_)));
    }

    // ---- FR-G8: pull requests ----

    use ember_client::api::{GitPrChecks, GitPrMerge};

    fn a_pr(number: u64, state: GitPrState, review: GitPrReview, checks: (u32, u32, u32), merge: (bool, &str)) -> GitPr {
        GitPr {
            number,
            title: format!("change {number}"),
            state,
            draft: false,
            author: "octo".into(),
            head: "feat/x".into(),
            base: "develop".into(),
            updated_at: "2026-11-06T12:00:00Z".into(),
            review,
            checks: GitPrChecks { passed: checks.0, failed: checks.1, pending: checks.2 },
            merge: GitPrMerge { can_merge: merge.0, reason: merge.1.into() },
            url: format!("https://github.com/o/r/pull/{number}"),
        }
    }

    #[test]
    fn fr_g8_the_checks_text_counts_passed_over_all() {
        assert_eq!(pr_checks_text(3, 1, 0), "체크 3/4 통과");
        assert_eq!(pr_checks_text(2, 0, 2), "체크 2/4 통과");
        assert_eq!(pr_checks_text(4, 0, 0), "체크 4/4 통과");
        assert_eq!(pr_checks_text(0, 0, 0), "체크 없음");
    }

    #[test]
    fn fr_g8_an_entry_has_badges_the_time_and_the_gate_text() {
        let now = iso_to_unix("2026-11-06T14:00:00Z").unwrap();
        let ok = pr_entry(&a_pr(7, GitPrState::Open, GitPrReview::Approved, (4, 0, 0), (true, "병합할 수 있습니다.")), now);
        assert_eq!((ok.number, ok.state_text, ok.review_text), (7, "열림", "승인됨"));
        assert_eq!(ok.time_text, "2시간 전");
        assert_eq!(ok.checks_text, "체크 4/4 통과");
        assert!(ok.can_merge);
        assert_eq!(ok.gate_text, "병합할 수 있습니다");
        let bad = pr_entry(&a_pr(8, GitPrState::Open, GitPrReview::ChangesRequested, (3, 1, 0), (false, "CI 체크가 실패했습니다 (실패 1개).")), now);
        assert_eq!((bad.review_text, bad.checks_failed, bad.can_merge), ("변경 요청", 1, false));
        assert_eq!(bad.gate_text, "CI 체크가 실패했습니다 (실패 1개).");
        let merged = pr_entry(&a_pr(9, GitPrState::Merged, GitPrReview::None, (0, 0, 0), (false, "")), now);
        assert_eq!((merged.state_text, merged.review_text), ("병합됨", "리뷰 없음"));
        assert!(!merged.gate_text.is_empty(), "an empty reason still says something");
        assert_eq!(pr_state_text(GitPrState::Closed), "닫힘");
        assert_eq!(pr_review_text(GitPrReview::ReviewRequired), "리뷰 필요");
    }

    #[test]
    fn fr_g8_an_unreadable_time_shows_as_sent() {
        let mut pr = a_pr(1, GitPrState::Open, GitPrReview::None, (0, 0, 0), (false, "승인이 필요합니다."));
        pr.updated_at = "yesterday".into();
        assert_eq!(pr_entry(&pr, 0).time_text, "yesterday");
    }

    #[test]
    fn fr_g8_the_list_is_a_view_state_and_an_error_carries_the_nodes_message() {
        assert_eq!(prs_from(Ok(GitPrs::default()), 0), PrLoad::Ready(Vec::new()));
        let one = GitPrs { prs: vec![a_pr(1, GitPrState::Open, GitPrReview::Approved, (1, 0, 0), (true, "x"))] };
        assert!(matches!(prs_from(Ok(one), 0), PrLoad::Ready(v) if v.len() == 1));
        let login = ApiError::Status { status: 409, message: "GitHub에 로그인되어 있지 않습니다.\ngh auth login".into(), code: None };
        assert_eq!(prs_from(Err(login), 0), PrLoad::Failed("GitHub에 로그인되어 있지 않습니다.\ngh auth login".into()));
        let boom = ApiError::Status { status: 500, message: "boom".into(), code: None };
        assert_eq!(prs_from(Err(boom), 0), PrLoad::Failed("풀 리퀘스트 목록을 확인하지 못했습니다: boom".into()));
        let down = ApiError::Peer("refused".into());
        assert!(matches!(prs_from(Err(down), 0), PrLoad::Failed(t) if t.contains("서버에 닿지 않습니다")));
    }

    #[test]
    fn fr_g8_the_filter_sends_its_state_and_has_korean_labels() {
        assert_eq!(prs_body(PrFilter::Open), json!({ "state": "open", "limit": 30 }));
        assert_eq!(prs_body(PrFilter::Merged), json!({ "state": "merged", "limit": 30 }));
        assert_eq!(PrFilter::default(), PrFilter::Open);
        assert_eq!(PrFilter::ALL.map(PrFilter::label), ["열림", "닫힘", "병합됨", "전체"]);
        assert_eq!(PrFilter::Open.empty_text(), "열린 풀 리퀘스트가 없습니다");
        for f in PrFilter::ALL {
            assert!(f.empty_text().ends_with("니다"), "{}", f.empty_text());
        }
    }

    #[test]
    fn fr_g8_the_tab_count_is_read_by_the_pull_request_section() {
        assert_eq!(section_count_with(Section::PullRequests, None, None, None), None, "not read yet");
        assert_eq!(section_count_with(Section::PullRequests, None, Some(9), Some(3)), Some(3));
        assert_eq!(section_count_with(Section::Stash, None, Some(9), Some(3)), Some(9));
        let snap = RepoSnapshot { branches: s(&["main"]), ..Default::default() };
        assert_eq!(section_count_with(Section::Branches, Some(&snap), None, Some(3)), Some(1));
    }

    // ---- FR-G9: branches ----

    fn a_branch(name: &str, current: bool, default: bool, upstream: Option<&str>, ahead: u32, behind: u32, worktree: Option<&str>) -> GitBranchInfo {
        GitBranchInfo {
            name: name.into(),
            is_current: current,
            is_default: default,
            upstream: upstream.map(String::from),
            ahead,
            behind,
            last_sha: "a1b2c3d".into(),
            last_subject: "Fix: x".into(),
            last_time: Some("2026-11-06T12:00:00Z".into()),
            worktree: worktree.map(String::from),
        }
    }

    #[test]
    fn fr_g9_branches_ahead_behind_label_reads_in_korean() {
        assert_eq!(ahead_behind_label(2, 1), "앞섬 2 \u{00b7} 뒤처짐 1");
        assert_eq!(ahead_behind_label(3, 0), "앞섬 3");
        assert_eq!(ahead_behind_label(0, 4), "뒤처짐 4");
        assert_eq!(ahead_behind_label(0, 0), "원격과 같음");
    }

    #[test]
    fn fr_g9_branches_a_branch_without_an_upstream_says_so() {
        let now = iso_to_unix("2026-11-06T14:00:00Z").unwrap();
        let e = branch_entry(&a_branch("feat/x", false, false, None, 0, 0, None), now);
        assert_eq!((e.sync_text().as_str(), e.upstream_text.as_str(), e.has_upstream), ("업스트림 없음", "업스트림 없음", false));
        let t = branch_entry(&a_branch("feat/y", true, false, Some("origin/feat/y"), 2, 1, None), now);
        assert_eq!((t.sync_text().as_str(), t.upstream_text.as_str()), ("앞섬 2 \u{00b7} 뒤처짐 1", "origin/feat/y"));
        assert_eq!(t.last_line, "a1b2c3d \u{00b7} Fix: x \u{00b7} 2시간 전");
    }

    #[test]
    fn fr_g9_branches_an_unreadable_time_shows_as_sent_and_blank_parts_drop() {
        let mut b = a_branch("m", false, true, None, 0, 0, None);
        b.last_time = Some("yesterday".into());
        b.last_sha = String::new();
        assert_eq!(branch_entry(&b, 0).last_line, "Fix: x \u{00b7} yesterday");
        b.last_time = None;
        assert_eq!(branch_entry(&b, 0).last_line, "Fix: x");
    }

    #[test]
    fn fr_g9_branches_worktree_shows_its_last_folder() {
        assert_eq!(worktree_tail("/Users/demo/worktrees/ui-phase-6"), "ui-phase-6");
        assert_eq!(worktree_tail("/Users/demo/darkpyonix-ember/"), "darkpyonix-ember");
        assert_eq!(worktree_tail("ember"), "ember");
        let e = branch_entry(&a_branch("w", false, false, None, 0, 0, Some("/w/ui")), 0);
        assert_eq!(e.worktree.as_deref(), Some("/w/ui"));
        assert_eq!(branch_entry(&a_branch("w", false, false, None, 0, 0, Some("")), 0).worktree, None);
    }

    #[test]
    fn fr_g9_branches_the_bars_are_proportional_and_capped() {
        assert_eq!(branch_bar_fractions(0, 0), (0.0, 0.0));
        assert_eq!(branch_bar_fractions(5, 1), (0.5, 0.1));
        assert_eq!(branch_bar_fractions(50, 10), (1.0, 1.0));
    }

    #[test]
    fn fr_g9_branches_the_default_is_split_from_the_work_branches_in_node_order() {
        let list = GitBranches { branches: vec![
            a_branch("feat/a", true, false, Some("origin/feat/a"), 1, 0, None),
            a_branch("main", false, true, Some("origin/main"), 0, 0, None),
            a_branch("develop", false, false, None, 0, 0, None),
        ] };
        let BranchLoad::Ready(entries) = branches_from(Ok(list), 0) else { panic!() };
        let (default, rest) = split_branches(&entries);
        assert_eq!(default.map(|d| d.name.as_str()), Some("main"));
        assert_eq!(rest.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(), ["feat/a", "develop"]);
        assert!(rest[0].current);
        let (none, all) = split_branches(&entries[..1]);
        assert!(none.is_none() && all.len() == 1);
    }

    #[test]
    fn fr_g9_branches_the_list_is_a_view_state_and_an_error_carries_the_message() {
        assert_eq!(branches_from(Ok(GitBranches::default()), 0), BranchLoad::Ready(Vec::new()));
        let boom = ApiError::Status { status: 500, message: "boom".into(), code: None };
        assert_eq!(branches_from(Err(boom), 0), BranchLoad::Failed("브랜치 목록을 확인하지 못했습니다: boom".into()));
        let down = ApiError::Peer("refused".into());
        assert!(matches!(branches_from(Err(down), 0), BranchLoad::Failed(t) if t.contains("서버에 닿지 않습니다")));
    }

    #[test]
    fn fr_g9_branches_create_sends_the_base_only_when_chosen() {
        assert_eq!(branch_create_body("feat/n", None), json!({ "name": "feat/n" }));
        assert_eq!(branch_create_body("feat/n", Some("main")), json!({ "name": "feat/n", "from": "main" }));
    }

    #[test]
    fn fr_g9_branches_the_tab_count_is_the_loaded_count() {
        assert_eq!(section_count_with_branches(Section::Branches, None, None, None, None), None);
        assert_eq!(section_count_with_branches(Section::Branches, None, None, None, Some(4)), Some(4));
        let snap = RepoSnapshot { branches: s(&["main", "x"]), ..Default::default() };
        assert_eq!(section_count_with_branches(Section::Branches, Some(&snap), None, None, None), Some(2), "snapshot count until read");
        assert_eq!(section_count_with_branches(Section::Branches, Some(&snap), None, None, Some(5)), Some(5));
        assert_eq!(section_count_with_branches(Section::Stash, Some(&snap), Some(1), None, Some(5)), Some(1));
    }

    #[test]
    fn fr_l19_stash_now_an_untracked_only_tree_says_so_in_korean() {
        let raw = "스태시 만들기를 하지 못했습니다: No local changes to save";
        assert!(stash_now_error(raw).contains("추적 중인 변경이 없습니다"));
        assert_eq!(stash_now_error("다른 오류입니다"), "다른 오류입니다");
    }

    #[test]
    fn fr_l19_stash_now_refuses_a_clean_tree_without_calling_the_api() {
        assert_eq!(stash_now_gate(0), Err("스태시할 변경이 없습니다"));
        assert_eq!(stash_now_gate(1), Ok(()));
        assert_eq!(stash_now_gate(40), Ok(()));
    }

    // ---- conflict editor follow-ups (#382) ----

    #[test]
    fn fr_g7_a_second_tap_while_busy_is_ignored() {
        let f = two_blocks().with_choice(0, ConflictChoice::Ours).with_choice(1, ConflictChoice::Ours);
        assert!(matches!(resolve_tap(false, &f), ResolveTap::Send(_)));
        assert_eq!(resolve_tap(true, &f), ResolveTap::Ignore);
    }

    #[test]
    fn fr_g7_resolve_tap_is_ignored_until_every_block_is_chosen() {
        let f = two_blocks().with_choice(0, ConflictChoice::Ours);
        assert_eq!(resolve_tap(false, &f), ResolveTap::Ignore);
        assert_eq!(confirm(ConflictStep::Preview, false, &f), ResolveTap::Ignore);
    }

    #[test]
    fn fr_g7_cancel_returns_to_editing_and_keeps_choices() {
        let f = two_blocks().with_choice(0, ConflictChoice::Ours).with_choice(1, ConflictChoice::Theirs);
        let step = ConflictStep::Editing.enter_preview(&f);
        assert_eq!(step, ConflictStep::Preview);
        assert_eq!(cancel_preview(step), ConflictStep::Editing);
        assert_eq!(cancel_preview(ConflictStep::PreviewFull), ConflictStep::Editing);
        assert_eq!(f.resolved_count(), 2);
    }

    #[test]
    fn fr_g7_confirm_sends_the_merged_text() {
        let f = two_blocks().with_choice(0, ConflictChoice::Ours).with_choice(1, ConflictChoice::Theirs);
        let want = f.resolved_content().unwrap();
        assert_eq!(confirm(ConflictStep::Preview, false, &f), ResolveTap::Send(want.clone()));
        assert_eq!(confirm(ConflictStep::PreviewFull, false, &f), ResolveTap::Send(want));
        assert_eq!(confirm(ConflictStep::Editing, false, &f), ResolveTap::Ignore);
    }

    #[test]
    fn fr_g7_preview_expands_and_collapses() {
        assert_eq!(ConflictStep::Preview.preview_max(), CONFLICT_PREVIEW_LINES);
        let full = ConflictStep::Preview.toggle_full();
        assert_eq!(full, ConflictStep::PreviewFull);
        assert_eq!(full.preview_max(), usize::MAX);
        assert_eq!(full.toggle_full(), ConflictStep::Preview);
        assert_eq!(ConflictStep::Editing.toggle_full(), ConflictStep::Editing);
        let long = "x\n".repeat(CONFLICT_PREVIEW_LINES + 5);
        assert_eq!(preview_lines(&long, ConflictStep::Preview.preview_max()).1, 5);
        assert_eq!(preview_lines(&long, full.preview_max()).1, 0);
    }
    #[test]
    fn fr_g3_a_fast_second_tap_does_not_skip_the_amend_confirmation() {
        assert_eq!(amend_tap(false, 0, 1_000), AmendTap::Commit);
        assert_eq!(amend_tap(true, 0, 1_000), AmendTap::Arm);
        assert_eq!(amend_tap(true, 1_000, 1_000 + AMEND_CONFIRM_DELAY_MS - 1), AmendTap::Wait);
        assert_eq!(amend_tap(true, 1_000, 1_000 + AMEND_CONFIRM_DELAY_MS), AmendTap::Commit);
    }

    #[test]
    fn fr_g3_a_snapshot_keeps_its_note_in_a_rebuilt_status() {
        // Every snapshot comes from `fetch_snapshot`, which sets the note after `build_snapshot`;
        // staging reloads through it, so the note is derived again each time and a bare
        // `build_snapshot` carries none.
        let st = status(vec![gf("src/a.rs", '.', 'M')]);
        assert!(build_snapshot(&st, SAMPLE, "", &BTreeMap::new()).notes.is_empty());
    }


    // ---- 팀 작업 (#482) ----

    fn a_member(name: &str, leader: bool, cwd: &str) -> MemberCwd {
        MemberCwd { name: name.into(), harness: "claude-code".into(), leader, cwd: cwd.into(), state_text: "실행 중".into(), status: ember_client::LauncherStatus::Running }
    }

    fn branches_of(list: &[(&str, bool, Option<&str>)]) -> Vec<BranchEntry> {
        list.iter().map(|(n, cur, wt)| branch_entry(&a_branch(n, *cur, false, Some("origin/x"), 2, 1, *wt), 0)).collect()
    }

    #[test]
    fn fr_g9_team_a_root_worktree_holds_no_member_folder() {
        assert!(!cwd_in_worktree("/Users/x", "/"));
        assert!(!cwd_in_worktree("/", "/"));
        assert!(cwd_in_worktree("/a/b", "/a/"));
    }

    #[test]
    fn fr_g9_team_longest_prefix_wins_for_a_nested_worktree() {
        let b = branches_of(&[("main", true, Some("/r")), ("feat/ui", false, Some("/r/.claude/worktrees/ui"))]);
        let m = [a_member("ui-agent", false, "/r/.claude/worktrees/ui/ember/app"), a_member("lead", true, "/r/docs")];
        let rows = team_rows(&b, &m, &[]);
        assert_eq!(rows[0].branch, "main");
        assert_eq!(rows[0].member.as_ref().unwrap().name, "lead");
        assert_eq!(rows[1].member.as_ref().unwrap().name, "ui-agent");
    }

    #[test]
    fn fr_g9_team_a_sibling_folder_with_the_same_prefix_is_not_inside() {
        let b = branches_of(&[("feat/ui", false, Some("/w/ui"))]);
        let rows = team_rows(&b, &[a_member("x", false, "/w/ui2")], &[]);
        assert_eq!(rows[0].member, None);
        assert_eq!(team_rows(&b, &[a_member("x", false, "/w/ui/")], &[])[0].member.as_ref().unwrap().name, "x");
    }

    #[test]
    fn fr_g9_team_a_worktree_without_a_member_has_none() {
        let b = branches_of(&[("main", true, Some("/r"))]);
        let rows = team_rows(&b, &[a_member("far", false, "/elsewhere")], &[]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].member, None);
        assert_eq!(rows[0].sync_text, "앞섬 2 \u{00b7} 뒤처짐 1");
    }

    #[test]
    fn fr_g9_team_lists_only_branches_with_a_worktree_main_first_then_by_name() {
        let b = branches_of(&[("zeta", false, Some("/w/z")), ("develop", false, None), ("alpha", false, Some("/w/a")), ("main", true, Some("/r"))]);
        let names: Vec<String> = team_rows(&b, &[], &[]).into_iter().map(|r| r.branch).collect();
        assert_eq!(names, ["main", "alpha", "zeta"]);
        assert!(team_rows(&b, &[], &[])[0].current);
    }

    #[test]
    fn fr_g9_team_a_shared_worktree_shows_the_leader() {
        let b = branches_of(&[("main", true, Some("/r"))]);
        let m = [a_member("first", false, "/r"), a_member("lead", true, "/r/x")];
        assert_eq!(team_rows(&b, &m, &[])[0].member.as_ref().unwrap().name, "lead");
    }

    #[test]
    fn fr_g9_team_the_pr_text_is_the_branch_pr_open_first() {
        let b = branches_of(&[("feat/x", false, Some("/w/x")), ("main", true, Some("/r"))]);
        let prs = [
            pr_entry(&a_pr(7, GitPrState::Merged, GitPrReview::None, (0, 0, 0), (false, "")), 0),
            pr_entry(&a_pr(9, GitPrState::Open, GitPrReview::None, (0, 0, 0), (false, "")), 0),
        ];
        let rows = team_rows(&b, &[], &prs);
        let x = rows.iter().find(|r| r.branch == "feat/x").unwrap();
        assert_eq!(x.pr_text.as_deref(), Some("#9 열림"));
        assert_eq!(rows.iter().find(|r| r.branch == "main").unwrap().pr_text, None);
    }

    #[test]
    fn fr_g9_team_empty_input_gives_no_rows_and_the_count_is_the_loaded_count() {
        assert!(team_rows(&[], &[], &[]).is_empty());
        assert_eq!(section_count_with_team(Section::Team, None, None, None, None, None), None);
        assert_eq!(section_count_with_team(Section::Team, None, None, None, None, Some(3)), Some(3));
        assert_eq!(section_count_with_team(Section::Branches, None, None, None, Some(4), Some(3)), Some(4));
    }

    // ---- FR-G10 to FR-G12: pull request detail, review, merge, create (#483) ----

    use ember_client::api::{GitPrCheck, GitPrComment};

    fn a_detail(state: GitPrState, merge: (bool, &str)) -> GitPrDetail {
        GitPrDetail {
            pr: a_pr(7, state, GitPrReview::ReviewRequired, (2, 1, 0), merge),
            body: "첫 줄\n\n  \n둘째 줄".into(),
            check_runs: vec![
                GitPrCheck { name: "build".into(), state: GitPrCheckState::Passed, detail_url: None },
                GitPrCheck { name: "test".into(), state: GitPrCheckState::Failed, detail_url: None },
            ],
            reviews: Vec::new(),
            comments: Vec::new(),
            requested_reviewers: Vec::new(),
            additions: 40,
            deletions: 2,
            changed_files: 3,
            head_sha: A_HEAD.into(),
        }
    }

    const A_HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn fr_g11_merge_is_disabled_without_a_head_sha() {
        let mut v = pr_detail_view(&a_detail(GitPrState::Open, (true, "병합할 수 있습니다")));
        assert_eq!(v.head_sha, A_HEAD);
        v.head_sha = String::new();
        assert_eq!(merge_button_state(&v), (false, Some("헤드를 확인할 수 없어 병합할 수 없습니다".to_string())));
    }

    #[test]
    fn fr_g11_merge_body_carries_the_head_sha() {
        let body = pr_merge_body(7, GitPrMergeMethod::Squash, A_HEAD);
        assert_eq!(body["head_sha"], json!(A_HEAD));
        assert_eq!(body["confirm"], json!(true));
    }

    #[test]
    fn fr_g10_the_detail_maps_a_failed_check_and_the_counts() {
        let v = pr_detail_view(&a_detail(GitPrState::Open, (false, "CI 체크가 실패했습니다.")));
        assert_eq!(v.files_text, "파일 3 +40 -2");
        assert_eq!(v.ci_pill, "체크 2/3 통과");
        assert_eq!(v.review_pill, "리뷰 필요");
        assert_eq!(v.checks[1], ("test".to_string(), CheckClass::Failed, "실패".to_string()));
        assert_eq!(v.checks[0].1, CheckClass::Passed);
        assert_eq!(v.body_lines, vec!["첫 줄".to_string(), "둘째 줄".to_string()]);
        assert!(v.open);
        assert_eq!(v.merge_reason, "CI 체크가 실패했습니다.");
    }

    #[test]
    fn fr_g10_no_reviewers_gives_an_empty_list_and_the_default_reason() {
        let v = pr_detail_view(&a_detail(GitPrState::Open, (false, "  ")));
        assert!(v.reviewers.is_empty());
        assert!(v.comments.is_empty());
        assert_eq!(v.merge_reason, MERGE_GATE_DEFAULT);
    }

    #[test]
    fn fr_g10_reviewers_keep_the_newest_decision_and_requested_ones_wait() {
        let r = |a: &str, st| GitPrReviewItem { author: a.into(), state: st, body: String::new(), submitted_at: String::new() };
        let list = pr_reviewers(
            &[r("a", GitPrReviewState::ChangesRequested), r("a", GitPrReviewState::Approved), r("b", GitPrReviewState::Approved), r("b", GitPrReviewState::Commented), r("c", GitPrReviewState::Commented)],
            &["d".to_string(), "a".to_string()],
        );
        assert_eq!(list, vec![("a".into(), ReviewerState::Approved), ("b".into(), ReviewerState::Approved), ("c".into(), ReviewerState::Commented), ("d".into(), ReviewerState::Waiting)]);
        assert_eq!(ReviewerState::ChangesRequested.label(), "변경 요청");
        assert_eq!(ReviewerState::Waiting.label(), "대기");
    }

    #[test]
    fn fr_g10_a_long_comment_is_cut_and_empty_ones_are_dropped() {
        let mut d = a_detail(GitPrState::Open, (true, ""));
        d.comments = vec![
            GitPrComment { author: "kim".into(), body: "가".repeat(PR_COMMENT_CUT + 50), created_at: String::new() },
            GitPrComment { author: "lee".into(), body: "   ".into(), created_at: String::new() },
        ];
        let v = pr_detail_view(&d);
        assert_eq!(v.comments.len(), 1);
        assert_eq!(v.comments[0].1.chars().count(), PR_COMMENT_CUT + 1);
        assert!(v.comments[0].1.ends_with('\u{2026}'));
    }

    #[test]
    fn fr_g10_the_detail_state_carries_a_korean_failure() {
        assert!(matches!(pr_detail_from(Ok(a_detail(GitPrState::Open, (true, "")))), PrDetailLoad::Ready(v) if v.number == 7));
        let korean = ApiError::Status { status: 409, message: "GitHub에 로그인되어 있지 않습니다.".into(), code: None };
        assert_eq!(pr_detail_from(Err(korean)), PrDetailLoad::Failed("GitHub에 로그인되어 있지 않습니다.".into()));
        let english = ApiError::Status { status: 500, message: "boom".into(), code: None };
        assert_eq!(pr_detail_from(Err(english)), PrDetailLoad::Failed("풀 리퀘스트를 확인하지 못했습니다. 잠시 후 다시 시도해 주세요".into()));
    }

    #[test]
    fn fr_g11_the_merge_button_follows_the_gate_and_the_state() {
        let open_ok = pr_detail_view(&a_detail(GitPrState::Open, (true, "병합할 수 있습니다")));
        assert_eq!(merge_button_state(&open_ok), (true, None));
        let closed_gate = pr_detail_view(&a_detail(GitPrState::Open, (false, "")));
        assert_eq!(merge_button_state(&closed_gate), (false, Some(MERGE_GATE_DEFAULT.to_string())));
        let merged = pr_detail_view(&a_detail(GitPrState::Merged, (true, "")));
        assert!(!merge_button_state(&merged).0, "a merged pull request cannot be merged again");
        assert_eq!(merge_button_text(false), "병합");
        assert_eq!(merge_button_text(true), "정말 병합합니다");
    }

    #[test]
    fn fr_g11_comment_and_request_changes_need_a_body() {
        assert_eq!(review_request_gate(GitPrReviewAction::Comment, "  "), Err("내용을 입력해 주세요"));
        assert_eq!(review_request_gate(GitPrReviewAction::RequestChanges, ""), Err("내용을 입력해 주세요"));
        assert_eq!(review_request_gate(GitPrReviewAction::Comment, " 좋아요 "), Ok(()));
        assert_eq!(review_request_gate(GitPrReviewAction::Approve, ""), Ok(()));
    }

    #[test]
    fn fr_g11_the_request_bodies_confirm_and_trim() {
        assert_eq!(pr_review_body(7, GitPrReviewAction::RequestChanges, " 고쳐 주세요 "), json!({ "number": 7, "action": "request_changes", "body": "고쳐 주세요", "confirm": true }));
        assert_eq!(pr_review_body(7, GitPrReviewAction::Approve, ""), json!({ "number": 7, "action": "approve", "body": null, "confirm": true }));
        assert_eq!(pr_merge_body(7, GitPrMergeMethod::Squash, "abc"), json!({ "number": 7, "method": "squash", "delete_branch": false, "head_sha": "abc", "confirm": true }));
        assert_eq!(pr_review_notice(GitPrReviewAction::Approve), "승인했습니다");
    }

    #[test]
    fn fr_g12_a_new_pull_request_needs_a_title_and_leaves_blanks_to_the_node() {
        assert_eq!(pr_create_gate(" "), Err("제목을 입력해 주세요"));
        assert_eq!(pr_create_gate("Feat: x"), Ok(()));
        assert_eq!(pr_create_body(" Feat: x ", "", "", "feat/x", true), json!({ "title": "Feat: x", "body": null, "base": null, "head": "feat/x", "draft": true, "confirm": true }));
        assert_eq!(pr_created_notice(12), "풀 리퀘스트 #12을 만들었습니다");
    }

    const A: GitPrReviewAction = GitPrReviewAction::Approve;
    const R: GitPrReviewAction = GitPrReviewAction::RequestChanges;
    const C: GitPrReviewAction = GitPrReviewAction::Comment;

    #[test]
    fn fr_g11_approve_asks_twice() {
        let k = pr_action_key(7, A);
        assert_eq!(review_tap(A, None, 7, 10_000), ReviewTap::Arm);
        assert_eq!(review_tap(A, Some((k, 10_000)), 7, 10_100), ReviewTap::Wait);
        assert_eq!(review_tap(A, Some((k, 10_000)), 7, 10_600), ReviewTap::Send);
        assert_eq!(review_tap(A, Some((k, 10_000)), 7, 14_000), ReviewTap::Expired);
        assert_eq!(review_button_label(A, true), "정말 승인합니다");
        assert_eq!(review_button_label(A, false), "승인");
    }

    #[test]
    fn fr_g11_request_changes_asks_twice() {
        let k = pr_action_key(7, R);
        assert_eq!(review_tap(R, None, 7, 10_000), ReviewTap::Arm);
        assert_eq!(review_tap(R, Some((k, 10_000)), 7, 10_100), ReviewTap::Wait);
        assert_eq!(review_tap(R, Some((k, 10_000)), 7, 13_999), ReviewTap::Send);
        assert_eq!(review_tap(R, Some((k, 10_000)), 7, 14_000), ReviewTap::Expired);
        assert_eq!(review_button_label(R, true), "정말 변경을 요청합니다");
    }

    #[test]
    fn fr_g11_comment_is_a_single_tap() {
        assert_eq!(review_tap(C, None, 7, 10_000), ReviewTap::Send);
        assert_eq!(review_tap(C, Some((pr_action_key(7, A), 10_000)), 7, 10_100), ReviewTap::Send);
        assert_eq!(review_button_label(C, true), "코멘트");
    }

    #[test]
    fn fr_g11_arming_one_review_action_disarms_the_other() {
        // 승인 is armed; a tap on 변경 요청 (or on another pull request) arms afresh, never sends.
        let armed = Some((pr_action_key(7, A), 10_000));
        assert_eq!(review_tap(R, armed, 7, 10_900), ReviewTap::Arm);
        assert_eq!(review_tap(A, armed, 8, 10_900), ReviewTap::Arm);
        assert_ne!(pr_action_key(7, A), pr_action_key(7, R));
        assert_ne!(pr_action_key(7, A), pr_action_key(8, A));
        assert!(!armed_label_visible(armed, pr_action_key(7, R), 10_900));
    }

    #[test]
    fn fr_g11_a_review_draft_belongs_to_one_pull_request() {
        assert_ne!(review_draft_key("p", Some(1)), review_draft_key("p", Some(2)));
        assert_ne!(review_draft_key("p", Some(1)), review_draft_key("q", Some(1)));
        assert_eq!(review_draft_key("p", Some(1)), review_draft_key("p", Some(1)));
    }

    #[test]
    fn fr_g12_new_pr_title_limits() {
        assert_eq!(pr_create_gate("a\nb"), Err("제목은 한 줄로 입력해 주세요"));
        assert_eq!(pr_create_gate("a\u{1b}b"), Err("제목은 한 줄로 입력해 주세요"));
        assert_eq!(pr_create_gate(&"가".repeat(256)), Ok(()));
        assert_eq!(pr_create_gate(&"가".repeat(257)), Err("제목은 256자까지 입력할 수 있습니다"));
        assert_eq!(pr_create_gate("  Feat: x \n"), Ok(()));
    }

    #[test]
    fn fr_g11_review_body_is_cleaned() {
        assert_eq!(clean_body("a\u{1b}[31m b\r\n\tc\u{0}"), "a[31m b\n\tc");
        assert_eq!(pr_review_body(7, C, "x\u{7}y")["body"], json!("xy"));
        assert_eq!(pr_create_body("t", "d\u{0}e", "", "", false)["body"], json!("de"));
    }

    #[test]
    fn fr_g11_a_stale_head_error_is_recognised() {
        // The node's line, from ember/node/src/git_prs.rs (the merge's head check).
        let node = "풀 리퀘스트가 바뀌었습니다. 다시 불러온 뒤 병합하세요.";
        assert!(is_stale_head(node));
        assert!(!is_stale_head("CI 체크가 실패했습니다."));
        assert!(include_str!("../../../node/src/git_prs.rs").contains(STALE_HEAD_TEXT));
    }

    #[test]
    fn fr_g11_failures_are_korean_text() {
        let english = ApiError::Status { status: 500, message: "boom".into(), code: None };
        assert_eq!(korean_failure("병합", &english), "병합하지 못했습니다. 잠시 후 다시 시도해 주세요");
        let gate = ApiError::Status { status: 409, message: "CI 체크가 실패했습니다.".into(), code: None };
        assert_eq!(korean_failure("병합", &gate), "CI 체크가 실패했습니다.");
        assert!(!korean_failure("병합", &ApiError::Peer("refused".into())).contains("refused"));
    }

    // ---- merge banner and pick-all (FR-G16, FR-G17, FR-G7, #382) ----

    fn merge_state(in_progress: bool, files: u32, can_commit: bool) -> GitMergeState {
        GitMergeState { in_progress, from: Some("feat/x".into()), into: Some("main".into()), conflict_files: files, can_commit }
    }

    #[test]
    fn fr_g16_a_merge_in_progress_becomes_a_view_and_none_shows_no_banner() {
        let v = MergeView { from: "feat/x".into(), into: "main".into(), remaining: 2, can_commit: false };
        assert_eq!(merge_view_from(Ok(merge_state(true, 2, false))), MergeLoad::Ready(Some(v)));
        assert_eq!(merge_view_from(Ok(merge_state(false, 3, false))), MergeLoad::Ready(None));
        let e = ApiError::Status { status: 500, message: "boom".into(), code: None };
        assert!(matches!(merge_view_from(Err(e)), MergeLoad::Failed(t) if t.contains("하지 못했습니다")));
    }

    #[test]
    fn fr_g16_unknown_names_become_an_empty_string_and_a_short_title() {
        let m = GitMergeState { in_progress: true, from: None, into: None, conflict_files: 0, can_commit: true };
        let MergeLoad::Ready(Some(v)) = merge_view_from(Ok(m)) else { panic!("view") };
        assert_eq!((v.from.as_str(), v.into.as_str(), v.can_commit), ("", "", true));
        assert_eq!(banner_title(&v), "병합 중");
    }

    #[test]
    fn fr_g16_banner_title_and_sub() {
        let v = MergeView { from: "feat/x".into(), into: "main".into(), remaining: 1, can_commit: false };
        assert_eq!(banner_title(&v), "feat/x \u{2192} main 병합 중");
        assert_eq!(banner_sub(2), "남은 충돌 파일 2개");
        assert_eq!(banner_sub(0), "모든 충돌을 해결했습니다");
    }

    #[test]
    fn fr_g17_buttons_ask_twice_with_their_own_key() {
        assert_ne!(merge_tap_key(MergeAction::Abort), merge_tap_key(MergeAction::Commit));
        assert_eq!(merge_tap(MergeAction::Abort, None, 1_000), DeleteTap::Arm);
        let armed = Some((merge_tap_key(MergeAction::Abort), 1_000));
        assert_eq!(merge_tap(MergeAction::Abort, armed, 1_100), DeleteTap::Wait);
        assert_eq!(merge_tap(MergeAction::Abort, armed, 1_600), DeleteTap::Confirm);
        assert_eq!(merge_tap(MergeAction::Abort, armed, 5_000), DeleteTap::Expired);
        assert_eq!(merge_tap(MergeAction::Commit, armed, 1_600), DeleteTap::Arm);
        assert_eq!(banner_button_text(MergeAction::Abort, true), "정말 취소합니다");
        assert_eq!(banner_button_text(MergeAction::Commit, true), "정말 커밋합니다");
        assert_eq!(banner_button_text(MergeAction::Commit, false), "병합 커밋");
    }

    #[test]
    fn fr_g17_results_are_korean_notices_and_lines() {
        assert_eq!(merge_commit_result(Ok(GitCommitApplied { new_sha: "0123456789abcdef".into() })), Ok("병합 커밋 0123456을 만들었습니다".to_string()));
        assert_eq!(merge_abort_result(Ok(ember_client::api::GitMergeAborted { ok: true })), Ok("병합을 취소했습니다".to_string()));
        let ko = ApiError::Status { status: 409, message: "진행 중인 병합이 없습니다.".into(), code: None };
        assert_eq!(merge_commit_result(Err(ko)), Err("진행 중인 병합이 없습니다.".to_string()));
        let en = ApiError::Status { status: 409, message: "fatal: nope".into(), code: None };
        assert_eq!(merge_abort_result(Err(en)), Err(MERGE_FAILED_LINE.to_string()));
        assert!(!merge_failure(&ApiError::Peer("refused".into())).contains("refused"));
    }

    #[test]
    fn fr_g7_pick_all_sets_every_block_and_keeps_the_text() {
        let f = two_blocks().with_choice(0, ConflictChoice::Both);
        let ours = f.pick_all(ConflictChoice::Ours);
        assert!(ours.is_resolved());
        assert_eq!(ours.resolved_count(), 2);
        let theirs = f.pick_all(ConflictChoice::Theirs);
        assert_eq!(theirs.block_count(), 2);
        assert_ne!(ours.resolved_content(), theirs.resolved_content());
    }

    #[test]
    fn fr_g7_pick_all_on_a_file_without_blocks_changes_nothing() {
        let f = ConflictFile { path: "a".into(), segments: vec![ConflictSegment::Common("x".into())] };
        assert_eq!(f.pick_all(ConflictChoice::Ours), f);
    }
}

#[cfg(test)]
mod commit_detail_tests {
    use super::*;
    use ember_client::api::{GitCommitFile, GitLogRef};

    fn file(path: &str, status: GitCommitFileStatus, add: u64, del: u64, binary: bool) -> GitCommitFile {
        GitCommitFile { path: path.to_string(), old_path: None, status, additions: add, deletions: del, binary }
    }

    fn detail(parents: &[&str], files: Vec<GitCommitFile>) -> GitCommitDetail {
        GitCommitDetail {
            sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
            short: "0123456".to_string(),
            subject: "Feat: x".to_string(),
            body: "Feat: x\n\nfirst line\n\nsecond line\n".to_string(),
            author: "BREW".to_string(),
            time: "2026-11-06T12:00:00+09:00".to_string(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            refs: vec![
                GitLogRef { name: "refs/tags/v1".to_string(), kind: GitLogRefKind::Tag },
                GitLogRef { name: "refs/heads/main".to_string(), kind: GitLogRefKind::Branch },
            ],
            files_total: files.len(),
            files,
            truncated: false,
        }
    }

    #[test]
    fn fr_g14_status_letters_cover_every_status() {
        assert_eq!(file_status_letter(GitCommitFileStatus::Added), "A");
        assert_eq!(file_status_letter(GitCommitFileStatus::Modified), "M");
        assert_eq!(file_status_letter(GitCommitFileStatus::Deleted), "D");
        assert_eq!(file_status_letter(GitCommitFileStatus::Renamed), "R");
        assert_eq!(file_status_letter(GitCommitFileStatus::Copied), "C");
        assert_eq!(file_status_letter(GitCommitFileStatus::TypeChanged), "T");
    }

    #[test]
    fn fr_g14_stat_text_is_counts_or_binary() {
        assert_eq!(file_stat_text(12, 3, false), "+12 -3");
        assert_eq!(file_stat_text(0, 0, false), "+0 -0");
        assert_eq!(file_stat_text(0, 0, true), "바이너리");
    }

    #[test]
    fn fr_g14_the_view_drops_the_subject_copy_and_blank_lines_and_reads_the_time() {
        let now = iso_to_unix("2026-11-06T15:00:00+09:00").unwrap();
        let v = commit_detail_view(&detail(&["p1"], vec![file("a.rs", GitCommitFileStatus::Modified, 1, 2, false)]), now);
        assert_eq!(v.body_lines, ["first line", "second line"]);
        assert_eq!(v.time_text, "3시간 전");
        assert_eq!(v.short, "0123456");
        assert!(!v.is_merge);
        assert_eq!(v.refs[0].label, "main");
        assert_eq!(v.refs[1].label, "v1");
        assert_eq!(v.files[0].status_letter, "M");
    }

    #[test]
    fn fr_g14_a_time_that_does_not_parse_is_shown_as_the_node_sent_it() {
        let mut d = detail(&[], vec![]);
        d.time = "yesterday".to_string();
        assert_eq!(commit_detail_view(&d, 0).time_text, "yesterday");
    }

    #[test]
    fn fr_g14_two_parents_are_a_merge_and_a_rename_keeps_its_old_path() {
        let mut f = file("new.rs", GitCommitFileStatus::Renamed, 0, 0, false);
        f.old_path = Some("old.rs".to_string());
        let v = commit_detail_view(&detail(&["a", "b"], vec![f]), 0);
        assert!(v.is_merge);
        assert_eq!(v.files[0].old_path.as_deref(), Some("old.rs"));
    }

    #[test]
    fn fr_g14_the_list_is_cut_at_100_and_says_how_many_more() {
        let files: Vec<_> = (0..130).map(|i| file(&format!("f{i}.rs"), GitCommitFileStatus::Added, 1, 0, false)).collect();
        let v = commit_detail_view(&detail(&["p"], files), 0);
        let (shown, more) = commit_files_shown(&v);
        assert_eq!(shown.len(), 100);
        assert_eq!(more.as_deref(), Some("30개 더 있습니다"));
        // The node cut its list: the total is larger than what came.
        let mut d = detail(&["p"], vec![file("a", GitCommitFileStatus::Added, 1, 0, false)]);
        d.files_total = 500;
        d.truncated = true;
        let v = commit_detail_view(&d, 0);
        assert_eq!(commit_files_shown(&v).1.as_deref(), Some("499개 더 있습니다. 파일 목록이 500개에서 잘렸습니다 (전체 500개)"));
        let small = commit_detail_view(&detail(&["p"], vec![file("a", GitCommitFileStatus::Added, 1, 0, false)]), 0);
        assert_eq!(commit_files_shown(&small).1, None);
    }

    #[test]
    fn fr_g14_a_failed_read_is_a_korean_line() {
        let en = ApiError::Status { status: 404, message: "no such commit".to_string(), code: None };
        let ko = ApiError::Status { status: 404, message: "그런 커밋을 찾을 수 없습니다.".to_string(), code: None };
        let CommitDetailLoad::Failed(a) = commit_detail_from(Err(en), 0) else { panic!() };
        let CommitDetailLoad::Failed(b) = commit_detail_from(Err(ko), 0) else { panic!() };
        assert!(!a.is_ascii() && !a.contains("no such"));
        assert_eq!(b, "그런 커밋을 찾을 수 없습니다.");
        assert!(matches!(commit_detail_from(Ok(detail(&[], vec![])), 0), CommitDetailLoad::Ready(_)));
    }

    #[test]
    fn fr_g15_a_merge_commit_cannot_be_cherry_picked_but_can_be_reverted() {
        assert_eq!(commit_action_gate(CommitAction::CherryPick, false, true), Err("병합 커밋은 가져올 수 없습니다"));
        assert_eq!(commit_action_gate(CommitAction::Revert, false, true), Ok(()));
        assert_eq!(commit_action_gate(CommitAction::CherryPick, false, false), Ok(()));
    }

    #[test]
    fn fr_g15_a_dirty_tree_stops_both_actions() {
        let line = "작업 중인 변경이 있어 먼저 커밋하거나 스태시해야 합니다";
        assert_eq!(commit_action_gate(CommitAction::Revert, true, false), Err(line));
        assert_eq!(commit_action_gate(CommitAction::CherryPick, true, false), Err(line));
        // A merge says why it cannot be picked before it talks about the tree.
        assert_eq!(commit_action_gate(CommitAction::CherryPick, true, true), Err("병합 커밋은 가져올 수 없습니다"));
    }

    #[test]
    fn fr_g15_untracked_files_do_not_make_the_tree_dirty() {
        let f = |path: &str, status| FileDiff { path: path.to_string(), old_path: None, status, binary: false, mode: None, hunks: vec![] };
        let mut snap = RepoSnapshot::default();
        assert!(!snapshot_dirty(&snap));
        snap.changes = vec![f("new.txt", FileStatus::Added)];
        assert!(!snapshot_dirty(&snap));
        snap.staged.insert("new.txt".to_string());
        assert!(snapshot_dirty(&snap));
        snap.staged.clear();
        snap.changes = vec![f("a.rs", FileStatus::Modified)];
        assert!(snapshot_dirty(&snap));
    }

    #[test]
    fn fr_g15_the_armed_keys_differ_by_commit_and_action() {
        let (a, b) = ("0123456789abcdef", "fedcba9876543210");
        assert_ne!(commit_action_key(a, CommitAction::Revert), commit_action_key(a, CommitAction::CherryPick));
        assert_ne!(commit_action_key(a, CommitAction::Revert), commit_action_key(b, CommitAction::Revert));
        assert_eq!(commit_action_key(a, CommitAction::Revert), commit_action_key(a, CommitAction::Revert));
    }

    #[test]
    fn fr_g15_the_branch_form_uses_the_branch_name_rule() {
        assert!(valid_branch_name("fix/from-abc1234"));
        assert!(!valid_branch_name("bad name"));
        assert!(!valid_branch_name("  "));
    }

    #[test]
    fn fr_g15_labels_and_notices_are_korean_without_a_plain_da_ending() {
        assert_eq!(CommitAction::Revert.armed_label(), "정말 되돌립니다");
        assert_eq!(CommitAction::CherryPick.armed_label(), "정말 가져옵니다");
        assert_eq!(commit_applied_notice(CommitAction::Revert, "develop", "abcdef0123"), "develop에 새 커밋 abcdef0을 만들었습니다");
        assert_eq!(commit_applied_notice(CommitAction::CherryPick, "", ""), "현재 브랜치에 새 커밋을 만들었습니다");
    }

    #[test]
    fn fr_g15_failures_keep_the_nodes_korean_and_never_show_english() {
        let conflict = ApiError::Status { status: 409, message: "충돌이 나서 중단했습니다. 되돌리기를 취소했습니다.".to_string(), code: None };
        assert_eq!(commit_failure("되돌리기", &conflict), "충돌이 나서 중단했습니다. 되돌리기를 취소했습니다.");
        let en = ApiError::Status { status: 500, message: "error: could not revert".to_string(), code: None };
        let line = commit_failure("Cherry-pick", &en);
        assert!(!line.contains("could not"));
        assert!(line.ends_with("다시 시도해 주세요"));
    }

    #[test]
    fn fr_g15_the_armed_action_names_the_branch_that_gets_the_commit() {
        assert_eq!(commit_action_branch_line(CommitAction::Revert, "develop"), "현재 브랜치 develop에 새 커밋을 만듭니다");
        assert_eq!(commit_action_branch_line(CommitAction::CherryPick, " "), "현재 브랜치에 새 커밋을 만듭니다");
        assert_eq!(commit_applied_notice(CommitAction::CherryPick, "feat/x", "1234567890"), "feat/x에 새 커밋 1234567을 만들었습니다");
    }

    #[test]
    fn fr_g14_the_files_notice_counts_the_cut_files_once() {
        assert_eq!(commit_files_notice(10, 10, false), None);
        assert_eq!(commit_files_notice(100, 130, false).as_deref(), Some("30개 더 있습니다"));
        assert_eq!(commit_files_notice(500, 500, true).as_deref(), Some("파일 목록이 500개에서 잘렸습니다 (전체 500개)"));
        assert_eq!(commit_files_notice(100, 800, true).as_deref(), Some("700개 더 있습니다. 파일 목록이 500개에서 잘렸습니다 (전체 800개)"));
    }

    #[test]
    fn fr_g15_a_dirty_tree_answer_is_shown_as_is() {
        let text = "작업 중인 변경이 있어 되돌릴 수 없습니다. 먼저 커밋하거나 스태시해 주세요.";
        let e = ApiError::Status { status: 409, message: text.to_string(), code: None };
        let line = commit_failure("되돌리기", &e);
        assert_eq!(line, text);
        assert!(!line.contains("잠시 후"));
        // The client's dirty flag was wrong: the snapshot reloads.
        assert!(commit_failure_reloads_snapshot(&line));
        assert!(!commit_failure_reloads_snapshot("충돌이 발생해 취소했습니다."));
    }

    #[test]
    fn fr_g15_an_in_progress_answer_is_shown_as_is() {
        for text in ["진행 중인 git 작업이 있습니다. 먼저 마무리해 주세요.", "다른 git 작업이 진행 중입니다. 잠시 후 다시 시도해 주세요."] {
            let e = ApiError::Status { status: 409, message: text.to_string(), code: None };
            assert_eq!(commit_failure("Cherry-pick", &e), text);
        }
    }

    #[test]
    fn fr_g15_an_unknown_error_gets_a_korean_line() {
        let e = ApiError::Status { status: 409, message: "error: could not revert".to_string(), code: None };
        assert_eq!(commit_failure("되돌리기", &e), "작업을 하지 못했습니다. 저장소 상태를 확인해 주세요.");
    }

    #[test]
    fn fr_g14_displayed_commit_text_is_clipped_and_cleaned() {
        assert_eq!(clip_line("짧은 줄", 200), "짧은 줄");
        assert_eq!(clip_line("a\u{1b}[31mb\tc", 200), "a[31mb c");
        let long = "x".repeat(300);
        let out = clip_line(&long, COMMIT_TEXT_MAX);
        assert_eq!(out.chars().count(), COMMIT_TEXT_MAX);
        assert!(out.ends_with('\u{2026}'));
        let mut d = detail(&["p"], vec![]);
        d.subject = long.clone();
        d.author = long;
        let v = commit_detail_view(&d, 0);
        assert_eq!(v.subject.chars().count(), COMMIT_TEXT_MAX);
        assert_eq!(v.author.chars().count(), COMMIT_TEXT_MAX);
    }

    #[test]
    fn fr_g15_revert_arms_then_confirms_after_600ms() {
        let k = commit_action_key("abc1234", CommitAction::Revert);
        assert_eq!(delete_tap(None, k, 10_000), DeleteTap::Arm);
        assert_eq!(delete_tap(Some((k, 10_000)), k, 10_599), DeleteTap::Wait);
        assert_eq!(delete_tap(Some((k, 10_000)), k, 10_600), DeleteTap::Confirm);
    }

    #[test]
    fn fr_g15_an_armed_revert_expires_after_4s() {
        let k = commit_action_key("abc1234", CommitAction::Revert);
        assert!(armed_label_visible(Some((k, 10_000)), k, 13_999));
        assert!(!armed_label_visible(Some((k, 10_000)), k, 14_000));
        assert_eq!(delete_tap(Some((k, 10_000)), k, 14_000), DeleteTap::Expired);
    }

    #[test]
    fn fr_g15_arming_cherry_pick_disarms_revert() {
        let (rv, cp) = (commit_action_key("abc1234", CommitAction::Revert), commit_action_key("abc1234", CommitAction::CherryPick));
        // Revert is armed; a tap on Cherry-pick arms Cherry-pick instead of confirming Revert.
        assert_eq!(delete_tap(Some((rv, 10_000)), cp, 11_000), DeleteTap::Arm);
        let armed = Some((cp, 11_000));
        assert!(!armed_label_visible(armed, rv, 11_100));
        assert!(armed_label_visible(armed, cp, 11_100));
    }
    #[test]
    fn fr_g15_history_errors_map_raw_git_output() {
        for (raw, expected) in [
            ("Your local changes would be overwritten", "작업 중인 변경"),
            ("uncommitted changes exist", "작업 중인 변경"),
            ("CONFLICT in file", "충돌이 발생"),
            ("fatal: Unable to create index.lock", "진행 중인 git 작업"),
            ("another git process seems to be running", "진행 중인 git 작업"),
            ("fatal: bad revision abc", "커밋을 찾을 수 없습니다"),
            ("unknown_commit 123", "커밋을 찾을 수 없습니다"),
            ("fatal: not a git repository", "git 저장소를 찾을 수 없습니다"),
            ("unable to auto-detect email address", "git 사용자 이름과 이메일"),
        ] {
            let error = ApiError::Status { status: 409, message: raw.into(), code: None };
            assert!(commit_failure("되돌리기", &error).contains(expected));
            assert!(failure_text("히스토리를 확인", &error).contains(expected));
        }
    }

    #[test]
    fn fr_g5_history_filter_and_sync_derivation() {
        assert_eq!(HistoryFilter::default(), HistoryFilter::AllBranches);
        assert_eq!(HistoryFilter::ALL, [HistoryFilter::AllBranches, HistoryFilter::LocalOnly]);
        assert_eq!(HistoryFilter::AllBranches.label(), "모든 브랜치");
        assert_eq!(HistoryFilter::LocalOnly.label(), "로컬 전용");

        let make_commit = |sha: &str, refs: &[(&str, GitLogRefKind)]| HistoryEntry {
            sha: sha.to_string(),
            short: sha[..7].to_string(),
            subject: "test".to_string(),
            author: "author".to_string(),
            time_text: "1시간 전".to_string(),
            refs: refs.iter().map(|(l, k)| HistoryRef { label: l.to_string(), kind: k.clone() }).collect(),
            is_merge: false,
            unpushed: false,
            remote_only: false,
        };

        let mut entries = vec![
            make_commit("1111111111", &[("HEAD", GitLogRefKind::Head), ("feat/test", GitLogRefKind::Branch)]),
            make_commit("2222222222", &[("develop", GitLogRefKind::Branch), ("origin/develop", GitLogRefKind::Remote)]),
            make_commit("3333333333", &[("origin/other", GitLogRefKind::Remote)]),
        ];

        annotate_history_sync(&mut entries);

        assert!(entries[0].unpushed, "commit 0 is before remote ref and local only");
        assert!(!entries[0].remote_only);

        assert!(!entries[1].unpushed, "commit 1 is synced with origin");
        assert!(!entries[1].remote_only);

        assert!(!entries[2].unpushed);
        assert!(entries[2].remote_only, "commit 2 has only remote ref");

        let all = filter_history_entries(&entries, HistoryFilter::AllBranches);
        assert_eq!(all.len(), 3);

        let local = filter_history_entries(&entries, HistoryFilter::LocalOnly);
        assert_eq!(local.len(), 2);
        assert_eq!(local[0].sha, "1111111111");
        assert_eq!(local[1].sha, "2222222222");

        // detail sync flags
        let mut d = detail(&["p"], vec![]);
        d.refs = vec![GitLogRef { name: "refs/heads/feature".into(), kind: GitLogRefKind::Branch }];
        let v_local = commit_detail_view(&d, 0);
        assert!(v_local.unpushed);
        assert!(!v_local.remote_only);

        d.refs = vec![GitLogRef { name: "refs/remotes/origin/main".into(), kind: GitLogRefKind::Remote }];
        let v_remote = commit_detail_view(&d, 0);
        assert!(!v_remote.unpushed);
        assert!(v_remote.remote_only);

        assert!(!copy_sha_to_clipboard(""));
    }

}
