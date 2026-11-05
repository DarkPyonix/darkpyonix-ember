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
    ApiError, GitCommitted, GitConflicts, GitDiff, GitLog, GitLogCommit, GitLogRefKind, GitOutput, GitStashEntry, GitStatus,
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

/// The `git/diff` body of the whole work tree or the index. `include_untracked` makes the node
/// add the untracked files to the one patch (only meaningful with `staged: false`).
pub fn diff_body(staged: bool, include_untracked: bool) -> Value {
    json!({ "path": null, "staged": staged, "include_untracked": include_untracked })
}

async fn git_diff_of(project: &str, staged: bool) -> Result<String, ApiError> {
    let d: GitDiff = git_call(project, "diff", diff_body(staged, !staged)).await?;
    Ok(d.patch)
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
    let unstaged = git_diff_of(project, false).await?;
    let staged = git_diff_of(project, true).await?;
    let mut snap = build_snapshot(&status, &unstaged, &staged, &BTreeMap::new());
    snap.conflicts = conflicts;
    Ok(snap)
}

// ---------------------------------------------------------------------------------------------
// History (FR-G5)
// ---------------------------------------------------------------------------------------------

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
    }
}

/// The rows of one page of the log, in the node's order (newest first).
pub fn history_entries(log: &GitLog, now: i64) -> Vec<HistoryEntry> {
    log.commits.iter().map(|c| history_entry(c, now)).collect()
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

/// The count beside a section's name, with the stash count read by the stash section (`None`
/// until it has been read). The stash count does not need the repository snapshot.
pub fn section_count_with_stash(section: Section, snap: Option<&RepoSnapshot>, stash: Option<usize>) -> Option<usize> {
    if section == Section::Stash {
        return stash;
    }
    section_count(section, snap)
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
        let not_repo = ApiError::Status { status: 404, message: "not_a_repo".into() };
        assert_eq!(load_from(Err(not_repo)), Load::NotRepo);
        let down = ApiError::Peer("refused".into());
        assert!(matches!(load_from(Err(down)), Load::Unavailable(_)));
        // Any other answer of the node is a failure with its message, not "준비 중입니다".
        let boom = ApiError::Status { status: 500, message: "boom".into() };
        let failed = load_from(Err(boom));
        assert!(matches!(&failed, Load::Failed(t) if t.contains("boom")), "{failed:?}");
        assert_ne!(failed.note(), Some(soon_line("저장소 정보")));
        let busy = ApiError::Status { status: 409, message: "병합 중이라 차이를 읽지 못했습니다: x".into() };
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
        let e = ApiError::Status { status: 409, message: "not fast-forward".into() };
        assert_eq!(failure_text("Pull", &e), "Pull하지 못했습니다: not fast-forward");
        let down = ApiError::Peer("refused".into());
        assert!(failure_text("Push", &down).contains("서버에 닿지 않습니다"));
    }

    #[test]
    fn a_korean_message_from_the_node_is_not_wrapped_again() {
        let e = ApiError::Status {
            status: 409,
            message: "분기되어 있어 pull할 수 없습니다: fatal: Not possible to fast-forward".into(),
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

        let down = ApiError::Status { status: 500, message: "boom".into() };
        let (kept, err) = history_append(first, Err(down), 0);
        assert!(err.unwrap().contains("boom"));
        let HistoryLoad::Ready { entries, more, .. } = kept else { panic!() };
        assert_eq!(entries.len(), 1);
        assert!(more, "the user may retry the page");
    }

    #[test]
    fn fr_g5_a_failed_first_page_shows_the_nodes_message() {
        let e = ApiError::Status { status: 500, message: "로그를 읽지 못했습니다".into() };
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
        let e = ApiError::Status { status: 500, message: "스태시를 읽지 못했습니다".into() };
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
        let down = ApiError::Status { status: 409, message: "충돌 때문에 꺼낼 수 없습니다. 스태시는 그대로 남아 있습니다.".into() };
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
        let e = ApiError::Status { status: 500, message: "boom".into() };
        assert_eq!(failure_text(StashAction::Apply.what(), &e), "스태시를 적용하지 못했습니다: boom");
    }
}
