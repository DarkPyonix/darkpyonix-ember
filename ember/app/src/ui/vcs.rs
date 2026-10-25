//! The 버전 관리 screen (FR-L19, INTENT D34, `docs/DESIGN.md` section 6.6).
//!
//! The decisions are plain functions and types in the first half of this file, unit-tested
//! without a renderer: section routing, the compact tabs, the remote action and ahead/behind
//! text, the merge gate, unified diff parsing, line, hunk and file staging, and the file tree.
//! The components in the second half only hand them to dioxus-compose.
//!
//! No git data exists in the server, node or client yet. [`snapshot`] returns `None`, every data
//! view says [`SOON`], and nothing is invented. The endpoints the screen needs are listed on
//! issue #168.
//!
//! What the pinned toolkit cannot do yet, with the workaround used (`GAPS-WORKLIST.md`):
//! - no clickable Row or Column: a text Button is the hit target of a row or a diff line;
//! - no sticky column: the line check sits in the row and scrolls with it;
//! - no SVG graph: the history is a list first (TODO: draw the graph on a `Canvas`).

use std::collections::{BTreeMap, BTreeSet};

use dioxus_compose::prelude::*;

use crate::services::services;
use crate::ui::components::{AppButton, ButtonKind, EmberCard, Label, PickChip, Pill, PillTone};
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
    todo!()
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
    todo!()
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
    todo!()
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
    todo!()
}

/// The tab to show after the user picks a file, a commit or a pull request.
pub fn tab_after_select(tab: PaneTab) -> PaneTab {
    todo!()
}

/// The tab to show in a section: the chosen one if the section has it, else its first; `None`
/// for a section with one pane.
pub fn effective_tab(section: Section, chosen: PaneTab) -> Option<PaneTab> {
    todo!()
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
    todo!()
}

impl RemoteAction {
    pub fn label(self) -> String {
        todo!()
    }
}

/// "앞섬 2 · 뒤처짐 1", or a note that the branch equals its remote.
pub fn ahead_behind_text(ahead: u32, behind: u32) -> String {
    todo!()
}

/// The last fetch, from its age in seconds. `None`: never fetched.
pub fn fetched_text(age_secs: Option<u64>) -> String {
    todo!()
}

/// The widths of a branch's behind | ahead bar as fractions of the bar, `(behind, ahead)`.
pub fn bar_fractions(ahead: u32, behind: u32) -> (f32, f32) {
    todo!()
}

/// The line under the branch name.
pub fn upstream_text(upstream: Option<&str>) -> String {
    todo!()
}

/// "<what>은 준비 중입니다", with the topic particle that fits the last syllable of `what`.
pub fn soon_line(what: &str) -> String {
    todo!()
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
    todo!()
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
    todo!()
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
    todo!()
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
    todo!()
}

/// The glyph of a check box.
pub fn check_glyph(check: Check) -> &'static str {
    todo!()
}

/// The check of a folder from its children.
pub fn folder_check(children: &[Check]) -> Check {
    todo!()
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
        todo!()
    }

    /// Toggle one line. A context line or an unknown index changes nothing.
    pub fn toggle_line(&mut self, diff: &FileDiff, hunk: usize, line: usize) {
        todo!()
    }

    /// Toggle a hunk: stage all its changed lines, or unstage them when all are staged.
    pub fn toggle_hunk(&mut self, diff: &FileDiff, hunk: usize) {
        todo!()
    }

    pub fn set_all(&mut self, diff: &FileDiff, on: bool) {
        todo!()
    }

    /// `(staged, total)` changed lines of the file.
    pub fn counts(&self, diff: &FileDiff) -> (usize, usize) {
        todo!()
    }

    pub fn check(&self, diff: &FileDiff) -> Check {
        todo!()
    }

    pub fn hunk_check(&self, diff: &FileDiff, hunk: usize) -> Check {
        todo!()
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
        todo!()
    }

    /// Click the file's check: stage everything unless everything is staged already.
    pub fn toggle_file(&mut self, diff: &FileDiff) {
        todo!()
    }

    /// Click the "all" check.
    pub fn toggle_all(&mut self, diffs: &[FileDiff]) {
        todo!()
    }

    /// Files with at least one staged line.
    pub fn staged_files(&self, diffs: &[FileDiff]) -> usize {
        todo!()
    }
}

/// The commit button is enabled with a summary and at least one staged file. Amending keeps the
/// previous message and may change nothing else, so it is always enabled.
pub fn commit_enabled(summary: &str, staged_files: usize, amend: bool) -> bool {
    todo!()
}

/// "main에 커밋 (2개 파일)".
pub fn commit_label(branch: &str, staged_files: usize) -> String {
    todo!()
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
    todo!()
}

/// The rows of the changed-files tree: folders first, then files in the given order. Folders
/// whose path is in `folded` hide their children.
pub fn build_tree(paths: &[String], folded: &BTreeSet<String>) -> Vec<TreeRow> {
    todo!()
}

// ---------------------------------------------------------------------------------------------
// Repository data
// ---------------------------------------------------------------------------------------------

/// What the screen reads of a project's repository.
/// TODO(server): nothing provides this yet; see [`snapshot`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RepoSnapshot {
    pub branch: String,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub fetched_age_secs: Option<u64>,
    pub branches: Vec<String>,
    pub changes: Vec<FileDiff>,
}

/// The repository of a project, read on the project's computer through ember node.
/// TODO(server, #168): needs git status, diff, stage, commit, log graph, branches, pull requests,
/// stash, and push, pull and fetch. Until then this is `None` and the screen says [`SOON`].
pub fn snapshot(_project: &str) -> Option<RepoSnapshot> {
    None
}

/// The count beside a section's name; `None` while the data is unavailable.
pub fn section_count(section: Section, snap: Option<&RepoSnapshot>) -> Option<usize> {
    todo!()
}

/// Projects that can have a repository: every project except 기본 프로젝트.
pub fn repo_projects(boxes: &[ProjectBox]) -> Vec<&ProjectBox> {
    todo!()
}

/// Which project the screen shows: the selected one if it can have a repository, else the first
/// that can.
pub fn pick_project(selected: Option<&str>, boxes: &[ProjectBox]) -> Option<String> {
    todo!()
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
        assert!(snapshot("ember").is_none());
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
}
