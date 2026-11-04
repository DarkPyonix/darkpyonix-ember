//! The Project screen (FR-L1 to FR-L4, FR-L24, FR-L26; `docs/DESIGN.md` section 6.2).
//!
//! Top to bottom: the computer strip, then the project boxes in 3, 2 or 1 columns. A box is two
//! levels: a header (status disc, name, repository, counts, buttons) over an inset of up to five
//! member tiles and a "+N" deck. Tapping the header opens the full project view; the gear opens
//! the settings sheet. A computer chip opens the monitor sheet (`monitor.rs`).
//!
//! The decisions (columns per size class, member order, the deck count, who wears the leader
//! badge, settings validation, which settings the server can save) are plain functions at the top
//! of this file, tested without a renderer. The components below draw them.
//!
//! What the pinned toolkit cannot do yet, with the workaround used (`GAPS-WORKLIST.md`):
//! - no clickable Row/Column/Card: a text Button is the hit target of a header or a tile;
//! - no Heroicons set: the nearest `IconRole` glyph;
//! - no continuous-curve corners, one shadow: from `components.rs`;
//! - no animated characters: static poses per state (`ui/characters.rs`, `docs/DESIGN.md` section 7);
//! - the status donut would need `Canvas` arcs, which are unverified here: a disc with the member
//!   count, tinted by the most urgent status.

use dioxus_compose::prelude::WindowSizeClass;

use ember_client::state::Reachability;
use ember_client::wire::TeamView;
use ember_client::{LauncherStatus, State};

use crate::model::status_rank;
use crate::ui::components::PillTone;


// ---------------------------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------------------------

/// The built-in project's id. The id stays `unsorted` (D28); only its name changed.
pub const DEFAULT_PROJECT_ID: &str = "unsorted";
/// What the built-in project is called on screen (D28).
pub const DEFAULT_PROJECT_NAME: &str = "기본 프로젝트";
/// Shown wherever something is not built yet.
pub const SOON: &str = "준비 중입니다";

pub fn is_default_project(id: &str) -> bool {
    id == DEFAULT_PROJECT_ID
}

/// The name to draw for a project id.
pub fn display_name(id: &str) -> String {
    if is_default_project(id) {
        DEFAULT_PROJECT_NAME.to_string()
    } else {
        id.to_string()
    }
}

// ---------------------------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------------------------

/// Project box columns per size class: 3 wide, 2 medium, 1 on a phone.
/// TODO(GAPS: 창 폭 레이아웃): the design breaks at 1279 and 900; the toolkit reports its three
/// classes only.
pub fn grid_columns(class: WindowSizeClass) -> usize {
    match class {
        WindowSizeClass::Compact => 1,
        WindowSizeClass::Medium => 2,
        WindowSizeClass::Expanded => 3,
    }
}

/// Members shown in a box before the rest fold into the deck.
pub const MAX_TILES: usize = 5;

/// How many members the "+N" deck holds: none when the box is expanded.
pub fn deck_count(total: usize, expanded: bool) -> usize {
    if expanded {
        0
    } else {
        total.saturating_sub(MAX_TILES)
    }
}

/// How many member tiles a box draws.
pub fn visible_tiles(total: usize, expanded: bool) -> usize {
    total - deck_count(total, expanded)
}

/// The deck's label, "외 3명".
pub fn deck_label(n: usize) -> String {
    format!("외 {n}명")
}

/// Cells that fill the last row of a grid so every card keeps the same width.
pub fn row_padding(items: usize, columns: usize) -> usize {
    if columns == 0 || items % columns == 0 {
        0
    } else {
        columns - items % columns
    }
}

// ---------------------------------------------------------------------------------------------
// Members
// ---------------------------------------------------------------------------------------------

/// One member tile: a session seen as a team member (D27).
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub session_id: String,
    /// "ember-리더" style name; the team's own name when known, else `<project>-<harness>`.
    pub name: String,
    /// The session title: the task, shown as the subtitle.
    pub task: String,
    pub agent: String,
    pub status: LauncherStatus,
    pub leader: bool,
    pub updated_at: i64,
}

impl Member {
    /// The accessible name; the leader's carries "(리더)" (FR-L24).
    pub fn accessible_name(&self) -> String {
        if self.leader {
            format!("{} (리더)", self.name)
        } else {
            self.name.clone()
        }
    }
}

/// Waiting for approval first, then by urgency (`status_rank`), and most recent first inside a
/// status (FR-L2: "waiting" outranks "running").
pub fn order_members(members: &mut [Member]) {
    members.sort_by(|a, b| {
        status_rank(b.status)
            .cmp(&status_rank(a.status))
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
}

/// Whether a member wears the leader badge: only in a project that has a leader (never in
/// 기본 프로젝트, D28), and on at most one member (`already_has` is true once one was marked).
pub fn leader_badge(project_id: &str, is_team_leader: bool, already_has: bool) -> bool {
    !is_default_project(project_id) && is_team_leader && !already_has
}

/// The pill for a status: label and colour family.
pub fn status_pill(s: LauncherStatus) -> (&'static str, PillTone) {
    match s {
        LauncherStatus::WaitingForApproval => ("승인 대기", PillTone::Orange),
        LauncherStatus::Running => ("실행 중", PillTone::Blue),
        LauncherStatus::FinishedUnread => ("새 결과", PillTone::Green),
        LauncherStatus::Finished => ("완료", PillTone::Grey),
        LauncherStatus::Failed => ("실패", PillTone::Red),
        LauncherStatus::Idle => ("대기", PillTone::Grey),
        LauncherStatus::Unknown => ("알 수 없음", PillTone::Grey),
    }
}

/// The pill colour family by harness.
pub fn harness_tone(agent: &str) -> PillTone {
    let a = agent.to_lowercase();
    if a.contains("claude") {
        PillTone::Orange
    } else if a.contains("codex") || a.contains("gpt") || a.contains("openai") {
        PillTone::Green
    } else if a.contains("gemini") || a.contains("agy") || a.contains("antigravity") {
        PillTone::Blue
    } else {
        PillTone::Grey
    }
}

/// The status filter of the full project view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberFilter {
    All,
    Waiting,
    Running,
    Unread,
    Done,
    Failed,
}

impl MemberFilter {
    pub const ALL: [MemberFilter; 6] = [
        MemberFilter::All,
        MemberFilter::Waiting,
        MemberFilter::Running,
        MemberFilter::Unread,
        MemberFilter::Done,
        MemberFilter::Failed,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MemberFilter::All => "전체",
            MemberFilter::Waiting => "승인 대기",
            MemberFilter::Running => "실행 중",
            MemberFilter::Unread => "새 결과",
            MemberFilter::Done => "완료",
            MemberFilter::Failed => "실패",
        }
    }

    pub fn matches(self, s: LauncherStatus) -> bool {
        match self {
            MemberFilter::All => true,
            MemberFilter::Waiting => s == LauncherStatus::WaitingForApproval,
            MemberFilter::Running => s == LauncherStatus::Running,
            MemberFilter::Unread => s == LauncherStatus::FinishedUnread,
            MemberFilter::Done => s == LauncherStatus::Finished,
            MemberFilter::Failed => s == LauncherStatus::Failed,
        }
    }
}

/// Members matching the filter and the search text (name, task or harness, case-insensitive).
pub fn filter_members(members: &[Member], filter: MemberFilter, query: &str) -> Vec<Member> {
    let q = query.trim().to_lowercase();
    members
        .iter()
        .filter(|m| filter.matches(m.status))
        .filter(|m| {
            q.is_empty()
                || m.name.to_lowercase().contains(&q)
                || m.task.to_lowercase().contains(&q)
                || m.agent.to_lowercase().contains(&q)
        })
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Project boxes
// ---------------------------------------------------------------------------------------------

/// Everything one project box draws.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectBox {
    pub id: String,
    pub name: String,
    /// Ordered, waiting first.
    pub members: Vec<Member>,
    pub running: usize,
    pub waiting: usize,
    /// Assigned computer ids (FR-L4).
    pub computers: Vec<String>,
    /// The most urgent status, for the status disc.
    pub top: Option<LauncherStatus>,
    /// TODO(server): the server has no project repository or branch yet (FR-L26).
    pub repo: Option<String>,
    pub branch: Option<String>,
}

impl ProjectBox {
    /// The line under the name: repository and branch, or a note that none is set.
    pub fn repo_line(&self) -> String {
        match (&self.repo, &self.branch) {
            (Some(r), Some(b)) => format!("{r} \u{00b7} {b}"),
            (Some(r), None) => r.clone(),
            _ => "저장소가 설정되지 않았습니다".to_string(),
        }
    }

    pub fn leader(&self) -> Option<&Member> {
        self.members.iter().find(|m| m.leader)
    }
}

/// The tile's name: the team's own name when it has one, else `<project>-<harness>`, else the
/// session title when the harness is empty. It never ends with "-".
pub fn member_name(team_name: Option<&str>, project: &str, agent: &str, title: &str) -> String {
    if let Some(n) = team_name.map(str::trim).filter(|n| !n.is_empty()) {
        return n.to_string();
    }
    let agent = agent.trim();
    if agent.is_empty() {
        let title = title.trim();
        if title.is_empty() {
            return display_name(project);
        }
        return title.to_string();
    }
    format!("{}-{}", display_name(project), agent)
}

fn member_of(
    state: &State,
    project: &str,
    id: &str,
    leader_seen: &mut bool,
    show_archived: bool,
) -> Option<Member> {
    let v = state
        .session(id)
        .filter(|v| show_archived || !v.record.archived)?;
    let team: Option<&TeamView> = state.team_for_session(id);
    let is_leader = team.is_some_and(|t| t.leader == id);
    let leader = leader_badge(project, is_leader, *leader_seen);
    if leader {
        *leader_seen = true;
    }
    let name = member_name(
        team.and_then(|t| t.member(id)).map(|m| m.name.as_str()),
        project,
        &v.record.agent,
        &v.record.title,
    );
    Some(Member {
        session_id: id.to_string(),
        name,
        task: v.record.title.clone(),
        agent: v.record.agent.clone(),
        status: v.status,
        leader,
        updated_at: v.record.updated_at,
    })
}

/// All projects as boxes (excluding archived sessions), most recently active first (like `model::project_cards`).
pub fn project_boxes(state: &State) -> Vec<ProjectBox> {
    project_boxes_with_archived(state, false)
}

/// All projects as boxes, including archived sessions when `show_archived` is true (FR-L9).
pub fn project_boxes_with_archived(state: &State, show_archived: bool) -> Vec<ProjectBox> {
    let mut out: Vec<(i64, ProjectBox)> = state
        .projects()
        .into_iter()
        .map(|p| {
            let mut seen = false;
            let mut members: Vec<Member> = p
                .sessions
                .iter()
                .filter_map(|id| member_of(state, &p.name, id, &mut seen, show_archived))
                .collect();
            order_members(&mut members);
            let latest = members.iter().map(|m| m.updated_at).max().unwrap_or(0);
            let count = |s| members.iter().filter(|m| m.status == s).count();
            let top = members
                .iter()
                .map(|m| m.status)
                .max_by_key(|s| status_rank(*s));
            let b = ProjectBox {
                id: p.name.clone(),
                name: display_name(&p.name),
                running: count(LauncherStatus::Running),
                waiting: count(LauncherStatus::WaitingForApproval),
                computers: state.project_computers(&p.name).to_vec(),
                top,
                repo: None,
                branch: None,
                members,
            };
            (latest, b)
        })
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
    out.into_iter().map(|(_, b)| b).collect()
}

// ---------------------------------------------------------------------------------------------
// Computers
// ---------------------------------------------------------------------------------------------

/// One chip of the computer strip.
#[derive(Debug, Clone, PartialEq)]
pub struct ComputerChip {
    pub id: String,
    pub name: String,
    pub reachability: Reachability,
    /// Running members in the projects assigned to this computer. TODO(FR-X7): the client does
    /// not know which computer a session runs on; this counts the assigned projects' running
    /// sessions, an upper bound.
    pub running: usize,
    pub projects: Vec<String>,
}

pub fn reach_pill(r: Reachability) -> (&'static str, PillTone) {
    match r {
        Reachability::Online => ("온라인", PillTone::Green),
        Reachability::Offline => ("오프라인", PillTone::Grey),
        Reachability::Unknown => ("알 수 없음", PillTone::Grey),
    }
}

/// The chip's second line: status, and the running count when there is one. TODO(FR-X7): the OS
/// name joins it once ember node reports it.
pub fn chip_line(c: &ComputerChip) -> String {
    let (status, _) = reach_pill(c.reachability);
    if c.running > 0 {
        format!("{status} \u{00b7} 실행 {}", c.running)
    } else {
        status.to_string()
    }
}

pub fn computer_chips(state: &State, boxes: &[ProjectBox]) -> Vec<ComputerChip> {
    state
        .computers()
        .iter()
        .map(|c| ComputerChip {
            id: c.id.clone(),
            name: c.name.clone(),
            reachability: c.reachability,
            running: boxes
                .iter()
                .filter(|b| c.projects.contains(&b.id))
                .map(|b| b.running)
                .sum(),
            projects: c.projects.clone(),
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Settings (FR-L26)
// ---------------------------------------------------------------------------------------------

/// The sections of the settings sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsPart {
    Name,
    Repository,
    Computers,
    CharacterStyle,
    MemberDefaults,
    Leader,
    ProfileOverride,
    Links,
    Archive,
    Delete,
}

/// Whether the main server can save a part today. Only computer assignment has an API
/// (`assign_computer`, `unassign_computer`, FR-L4). The rest is shown disabled with "준비 중입니다".
/// TODO(server): project metadata (name, repository, branch, defaults, leader, profile override,
/// character style) and archive and delete need a project settings API (FR-L26).
pub fn part_supported(part: SettingsPart) -> bool {
    matches!(part, SettingsPart::Computers)
}

/// Whether the leader choice is shown at all: never in 기본 프로젝트 (D28).
pub fn leader_choice_visible(project_id: &str) -> bool {
    !is_default_project(project_id)
}

/// Archive and delete are blocked in 기본 프로젝트 whatever the server says.
pub fn danger_allowed(project_id: &str, part: SettingsPart) -> bool {
    part_supported(part) && !is_default_project(project_id)
}

/// Whether 저장 can do anything. False until the server saves project metadata.
pub const SETTINGS_SAVE_AVAILABLE: bool = false;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsDraft {
    pub name: String,
    pub repo: String,
    pub branch: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsError {
    NameEmpty,
    NameTooLong,
    NameInvalid,
    RepoHasSpace,
    BranchHasSpace,
    BranchInvalid,
}

impl SettingsError {
    pub fn message(self) -> &'static str {
        match self {
            SettingsError::NameEmpty => "이름을 입력해 주세요",
            SettingsError::NameTooLong => "이름은 64자 이하여야 합니다",
            SettingsError::NameInvalid => "이름에는 슬래시와 제어 문자를 쓸 수 없습니다",
            SettingsError::RepoHasSpace => "저장소 주소에는 공백이 들어갈 수 없습니다",
            SettingsError::BranchHasSpace => "브랜치 이름에는 공백이 들어갈 수 없습니다",
            SettingsError::BranchInvalid => "브랜치 이름이 올바르지 않습니다",
        }
    }
}

pub const NAME_MAX_CHARS: usize = 64;

/// Check the settings form. 기본 프로젝트's name is fixed, so it is not checked. Repository and
/// branch may be empty (a project without a repository).
pub fn validate_settings(d: &SettingsDraft, project_id: &str) -> Vec<SettingsError> {
    let mut errors = Vec::new();
    if !is_default_project(project_id) {
        let name = d.name.trim();
        if name.is_empty() {
            errors.push(SettingsError::NameEmpty);
        } else if name.chars().count() > NAME_MAX_CHARS {
            errors.push(SettingsError::NameTooLong);
        } else if name.contains('/') || name.chars().any(char::is_control) {
            errors.push(SettingsError::NameInvalid);
        }
    }
    if d.repo.trim().chars().any(char::is_whitespace) {
        errors.push(SettingsError::RepoHasSpace);
    }
    let branch = d.branch.trim();
    if branch.chars().any(char::is_whitespace) {
        errors.push(SettingsError::BranchHasSpace);
    } else if !branch.is_empty()
        && (branch.starts_with('-')
            || branch.starts_with('/')
            || branch.ends_with('/')
            || branch.ends_with(".lock")
            || branch.contains(".."))
    {
        errors.push(SettingsError::BranchInvalid);
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: &str, status: LauncherStatus, at: i64) -> Member {
        Member {
            session_id: id.into(),
            name: format!("ember-{id}"),
            task: format!("task {id}"),
            agent: "claude-code".into(),
            status,
            leader: false,
            updated_at: at,
        }
    }

    #[test]
    fn member_name_never_ends_with_a_dash() {
        assert_eq!(
            member_name(Some("ember-리더"), "ember", "codex", "t"),
            "ember-리더"
        );
        assert_eq!(
            member_name(Some(""), "demo-project", "codex", "t"),
            "demo-project-codex"
        );
        assert_eq!(
            member_name(None, "demo-project", "", "빌드 고치기"),
            "빌드 고치기"
        );
        assert_eq!(member_name(None, "demo-project", " ", ""), "demo-project");
        assert!(!member_name(None, "demo-project", "", "").ends_with('-'));
    }

    // FR-L1: 3, 2 or 1 columns by size class.
    #[test]
    fn grid_columns_per_size_class() {
        assert_eq!(grid_columns(WindowSizeClass::Expanded), 3);
        assert_eq!(grid_columns(WindowSizeClass::Medium), 2);
        assert_eq!(grid_columns(WindowSizeClass::Compact), 1);
    }

    #[test]
    fn last_grid_row_is_padded_to_the_column_count() {
        assert_eq!(row_padding(4, 3), 2);
        assert_eq!(row_padding(6, 3), 0);
        assert_eq!(row_padding(1, 1), 0);
        assert_eq!(row_padding(5, 2), 1);
    }

    // FR-L2: waiting for approval outranks running; most recent first inside a status.
    #[test]
    fn members_are_ordered_with_waiting_first() {
        let mut ms = vec![
            member("done", LauncherStatus::Finished, 9),
            member("run-old", LauncherStatus::Running, 1),
            member("fail", LauncherStatus::Failed, 5),
            member("wait", LauncherStatus::WaitingForApproval, 0),
            member("run-new", LauncherStatus::Running, 7),
            member("unread", LauncherStatus::FinishedUnread, 3),
        ];
        order_members(&mut ms);
        let ids: Vec<&str> = ms.iter().map(|m| m.session_id.as_str()).collect();
        assert_eq!(
            ids,
            ["wait", "run-new", "run-old", "fail", "unread", "done"]
        );
    }

    // FR-L1: up to five tiles, the rest in the "+N" deck.
    #[test]
    fn deck_holds_everything_past_five() {
        assert_eq!(MAX_TILES, 5);
        assert_eq!((visible_tiles(3, false), deck_count(3, false)), (3, 0));
        assert_eq!((visible_tiles(5, false), deck_count(5, false)), (5, 0));
        assert_eq!((visible_tiles(8, false), deck_count(8, false)), (5, 3));
        assert_eq!(deck_label(3), "외 3명");
    }

    #[test]
    fn expanding_the_box_empties_the_deck() {
        assert_eq!((visible_tiles(8, true), deck_count(8, true)), (8, 0));
    }

    // FR-L24: exactly one leader in a project with a leader, none in 기본 프로젝트.
    #[test]
    fn leader_badge_rules() {
        assert!(leader_badge("ember", true, false));
        assert!(!leader_badge("ember", false, false));
        assert!(
            !leader_badge("ember", true, true),
            "at most one badge per project"
        );
        assert!(
            !leader_badge(DEFAULT_PROJECT_ID, true, false),
            "no leader in 기본 프로젝트"
        );
    }

    #[test]
    fn leader_accessible_name_says_leader() {
        let mut m = member("lead", LauncherStatus::Running, 1);
        assert_eq!(m.accessible_name(), "ember-lead");
        m.leader = true;
        assert!(m.accessible_name().contains("(리더)"));
    }

    // D28: the id stays `unsorted`, the name is 기본 프로젝트.
    #[test]
    fn default_project_is_named_but_keeps_its_id() {
        assert_eq!(DEFAULT_PROJECT_ID, "unsorted");
        assert_eq!(display_name("unsorted"), "기본 프로젝트");
        assert_eq!(display_name("ember"), "ember");
        assert!(!leader_choice_visible("unsorted"));
        assert!(leader_choice_visible("ember"));
    }

    #[test]
    fn status_pills_cover_every_status() {
        assert_eq!(
            status_pill(LauncherStatus::WaitingForApproval),
            ("승인 대기", PillTone::Orange)
        );
        assert_eq!(status_pill(LauncherStatus::Running).0, "실행 중");
        assert_eq!(status_pill(LauncherStatus::Failed).1, PillTone::Red);
        assert_eq!(
            status_pill(LauncherStatus::FinishedUnread).1,
            PillTone::Green
        );
    }

    #[test]
    fn character_placeholder_colour_follows_the_harness() {
        assert_eq!(harness_tone("claude-code"), PillTone::Orange);
        assert_eq!(harness_tone("codex"), PillTone::Green);
        assert_eq!(harness_tone("gemini"), PillTone::Blue);
        assert_eq!(harness_tone("antigravity"), PillTone::Blue);
        assert_eq!(harness_tone("other"), PillTone::Grey);
    }

    // FR-L1: the full view filters by status and searches name, task and harness.
    #[test]
    fn members_filter_by_status_and_search() {
        let ms = vec![
            member("a", LauncherStatus::Running, 1),
            member("b", LauncherStatus::WaitingForApproval, 2),
            member("c", LauncherStatus::Failed, 3),
        ];
        assert_eq!(filter_members(&ms, MemberFilter::All, "").len(), 3);
        assert_eq!(
            filter_members(&ms, MemberFilter::Waiting, "")[0].session_id,
            "b"
        );
        assert_eq!(
            filter_members(&ms, MemberFilter::All, "TASK C")[0].session_id,
            "c"
        );
        assert!(filter_members(&ms, MemberFilter::Running, "task c").is_empty());
        assert_eq!(filter_members(&ms, MemberFilter::All, "claude").len(), 3);
        assert_eq!(MemberFilter::ALL.len(), 6);
    }

    // FR-L3: the chip line shows status and the running count.
    #[test]
    fn computer_chip_line() {
        let mut c = ComputerChip {
            id: "c1".into(),
            name: "mac-mini".into(),
            reachability: Reachability::Online,
            running: 0,
            projects: vec![],
        };
        assert_eq!(chip_line(&c), "온라인");
        c.running = 2;
        assert_eq!(chip_line(&c), "온라인 \u{00b7} 실행 2");
        c.reachability = Reachability::Offline;
        assert_eq!(reach_pill(c.reachability).0, "오프라인");
    }

    // FR-L26: validation.
    fn draft(name: &str, repo: &str, branch: &str) -> SettingsDraft {
        SettingsDraft {
            name: name.into(),
            repo: repo.into(),
            branch: branch.into(),
        }
    }

    #[test]
    fn settings_accept_a_normal_form_and_empty_repository() {
        assert!(
            validate_settings(
                &draft("ember", "github.com/DarkPyonix/darkpyonix-ember", "develop"),
                "ember"
            )
            .is_empty()
        );
        assert!(validate_settings(&draft("ember", "", ""), "ember").is_empty());
    }

    #[test]
    fn settings_reject_a_bad_name() {
        assert_eq!(
            validate_settings(&draft("  ", "", ""), "ember"),
            [SettingsError::NameEmpty]
        );
        assert_eq!(
            validate_settings(&draft(&"a".repeat(65), "", ""), "ember"),
            [SettingsError::NameTooLong]
        );
        assert_eq!(
            validate_settings(&draft("a/b", "", ""), "ember"),
            [SettingsError::NameInvalid]
        );
        assert!(validate_settings(&draft(&"a".repeat(64), "", ""), "ember").is_empty());
    }

    #[test]
    fn settings_reject_a_bad_repository_or_branch() {
        assert_eq!(
            validate_settings(&draft("p", "a b", ""), "p"),
            [SettingsError::RepoHasSpace]
        );
        assert_eq!(
            validate_settings(&draft("p", "", "a b"), "p"),
            [SettingsError::BranchHasSpace]
        );
        for bad in ["-x", "/x", "x/", "a..b", "x.lock"] {
            assert_eq!(
                validate_settings(&draft("p", "", bad), "p"),
                [SettingsError::BranchInvalid],
                "{bad}"
            );
        }
        assert!(validate_settings(&draft("p", "", "feat/client-project"), "p").is_empty());
    }

    #[test]
    fn default_project_name_is_not_checked() {
        assert!(validate_settings(&draft("", "", ""), DEFAULT_PROJECT_ID).is_empty());
    }

    #[test]
    fn error_messages_are_in_the_explaining_style() {
        for e in [
            SettingsError::NameEmpty,
            SettingsError::NameTooLong,
            SettingsError::NameInvalid,
            SettingsError::RepoHasSpace,
            SettingsError::BranchHasSpace,
            SettingsError::BranchInvalid,
        ] {
            let m = e.message();
            assert!(m.ends_with("니다") || m.ends_with("주세요"), "{m}");
            assert!(!m.contains('\u{2014}'));
        }
    }

    // FR-L26: only what the server supports is enabled; the rest is "준비 중입니다".
    #[test]
    fn only_computer_assignment_is_saved_by_the_server() {
        assert!(part_supported(SettingsPart::Computers));
        for part in [
            SettingsPart::Name,
            SettingsPart::Repository,
            SettingsPart::CharacterStyle,
            SettingsPart::MemberDefaults,
            SettingsPart::Leader,
            SettingsPart::ProfileOverride,
            SettingsPart::Links,
            SettingsPart::Archive,
            SettingsPart::Delete,
        ] {
            assert!(!part_supported(part), "{part:?}");
        }
        assert_eq!(SOON, "준비 중입니다");
        assert!(!SETTINGS_SAVE_AVAILABLE);
    }

    #[test]
    fn default_project_cannot_be_archived_or_deleted() {
        assert!(!danger_allowed(DEFAULT_PROJECT_ID, SettingsPart::Delete));
        assert!(!danger_allowed(DEFAULT_PROJECT_ID, SettingsPart::Archive));
    }

    #[test]
    fn repo_line_falls_back_to_a_note() {
        let mut b = ProjectBox {
            id: "p".into(),
            name: "p".into(),
            members: vec![],
            running: 0,
            waiting: 0,
            computers: vec![],
            top: None,
            repo: None,
            branch: None,
        };
        assert_eq!(b.repo_line(), "저장소가 설정되지 않았습니다");
        b.repo = Some("github.com/x/y".into());
        b.branch = Some("develop".into());
        assert_eq!(b.repo_line(), "github.com/x/y \u{00b7} develop");
    }
}
