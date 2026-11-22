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
use std::collections::BTreeMap;
use ember_client::wire::{MemberDefaults, Project, ProjectPatch, TeamView};
use ember_client::{LauncherStatus, State};

use crate::model::status_rank;
use crate::ui::characters::CharStyle;
use crate::ui::components::PillTone;
use crate::ui::routines::{format_next, last_run_text, next_run, schedule_to_entry, Entry, Minutes, RunStatus, Trigger};


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
    id == DEFAULT_PROJECT_ID || id == DEFAULT_PROJECT_NAME
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

/// Names listed on the deck card.
pub const DECK_NAMES: usize = 3;
/// Overlapped avatars on the deck card.
pub const DECK_AVATARS: usize = 4;

/// The first `n` hidden members' names joined by ", " (the deck card's second line).
pub fn deck_names(rest: &[Member], n: usize) -> String {
    rest.iter().take(n).map(|m| m.name.as_str()).collect::<Vec<_>>().join(", ")
}

/// `deck_names` cut to `max_chars` characters (not bytes, so Korean names are never split),
/// ending in an ellipsis when cut. Pure model of the one-line ellipsis of the deck card.
pub fn deck_names_fit(rest: &[Member], n: usize, max_chars: usize) -> String {
    let full = deck_names(rest, n);
    if full.chars().count() <= max_chars {
        return full;
    }
    let keep = max_chars.saturating_sub(1);
    let mut out: String = full.chars().take(keep).collect();
    out.push('…');
    out
}

/// The hidden members that get an avatar on the deck card: at most `n`.
pub fn deck_avatars(rest: &[Member], n: usize) -> &[Member] {
    &rest[..rest.len().min(n)]
}

/// The computer strip shows scroll arrows only when its content overflows.
pub fn strip_arrows_visible(max_offset: f32) -> bool {
    max_offset > 0.0
}

/// Where an arrow scrolls the strip: 80 % of the viewport in `dir` (-1 or 1), clamped to the
/// content.
pub fn strip_scroll_target(offset: f32, viewport: f32, content: f32, dir: f32) -> f32 {
    (offset + dir * 0.8 * viewport).clamp(0.0, (content - viewport).max(0.0))
}

/// Whether the strip arrow pointing in `dir` (-1 or 1) has anywhere to go: the left one is inert
/// at the start, the right one at the end. Half a dp of slack, because the Renderer reports
/// fractional offsets.
pub fn strip_arrow_enabled(offset: f32, max_offset: f32, dir: f32) -> bool {
    if dir < 0.0 {
        offset > 0.5
    } else {
        offset < max_offset - 0.5
    }
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
    /// From the server's project record (FR-L26).
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
    let project_leader = state.project(project).and_then(|p| p.leader_session.as_deref());
    let is_leader = match project_leader {
        Some(lid) => lid == id,
        None => team.is_some_and(|t| t.leader == id),
    };
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

/// Appended to the name of an archived project where archived items are shown (FR-L26, #327).
pub const ARCHIVED_MARK: &str = "(보관됨)";

/// Whether the server holds `name` as archived (FR-L26). An unknown project is not archived.
pub fn project_archived(state: &State, name: &str) -> bool {
    state.project(name).is_some_and(|p| p.archived)
}

/// All projects as boxes, including archived sessions when `show_archived` is true (FR-L9).
/// An archived project (FR-L26, #327) is listed only then, with [`ARCHIVED_MARK`] after its
/// name: the server keeps listing it and the client hides it.
pub fn project_boxes_with_archived(state: &State, show_archived: bool) -> Vec<ProjectBox> {
    let mut out: Vec<(i64, ProjectBox)> = state
        .projects()
        .into_iter()
        .filter(|p| show_archived || !project_archived(state, &p.name))
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
                name: if project_archived(state, &p.name) {
                    format!("{} {ARCHIVED_MARK}", display_name(&p.name))
                } else {
                    display_name(&p.name)
                },
                running: count(LauncherStatus::Running),
                waiting: count(LauncherStatus::WaitingForApproval),
                computers: state.project_computers(&p.name).to_vec(),
                top,
                repo: state.project(&p.name).and_then(|r| r.repository.clone()).filter(|r| !r.is_empty()),
                branch: state.project(&p.name).and_then(|r| r.default_branch.clone()).filter(|r| !r.is_empty()),
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

/// Whether the main server can save a part today: computer assignment (FR-L4) and, through
/// `PATCH /api/projects/{name}`, name, repository and branch, new member defaults, character
/// style, profile override, leader session and the archived flag, and `DELETE /api/projects/{name}`
/// (FR-L26, #327, #384). The rest is shown disabled with an explanation.
pub fn part_supported(part: SettingsPart) -> bool {
    matches!(
        part,
        SettingsPart::Computers
            | SettingsPart::Name
            | SettingsPart::Repository
            | SettingsPart::CharacterStyle
            | SettingsPart::MemberDefaults
            | SettingsPart::Leader
            | SettingsPart::ProfileOverride
            | SettingsPart::Archive
            | SettingsPart::Delete
    )
}

/// The short note under a part the server does not save yet.
pub const NOT_YET: &str = "서버가 아직 지원하지 않습니다";

/// Whether the leader choice is shown at all: never in 기본 프로젝트 (D28).
pub fn leader_choice_visible(project_id: &str) -> bool {
    !is_default_project(project_id)
}

/// Archive and delete are blocked in 기본 프로젝트 whatever the server says.
pub fn danger_allowed(project_id: &str, part: SettingsPart) -> bool {
    part_supported(part) && !is_default_project(project_id)
}

/// The settings sheet's sections in the mockup's order (`project.js` `projSettings`, #508):
/// 기본, 컴퓨터, 새 팀원 기본값, 프로필 덮어쓰기, 팀 캐릭터, 리더, 연결, 위험 영역. `Name` stands
/// for the 기본 section (name, repository, branch) and `Archive` for the danger zone.
pub const SETTINGS_ORDER: [SettingsPart; 8] = [
    SettingsPart::Name,
    SettingsPart::Computers,
    SettingsPart::MemberDefaults,
    SettingsPart::ProfileOverride,
    SettingsPart::CharacterStyle,
    SettingsPart::Leader,
    SettingsPart::Links,
    SettingsPart::Archive,
];

/// One row of the danger zone (`.ps-danger`): title, explanation, button label and enabled state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DangerRow {
    pub part: SettingsPart,
    pub title: &'static str,
    pub note: String,
    pub button: &'static str,
    pub enabled: bool,
}

/// The danger zone's rows as the mockup: 프로젝트 보관 then 프로젝트 삭제, each with what it does,
/// for a project that is not archived and whose delete button is not armed.
pub fn danger_rows(project_id: &str, members: usize) -> [DangerRow; 2] {
    danger_rows_for(project_id, members, false, false)
}

/// The danger zone's rows (FR-L26, #327, #384). An archived project offers 보관 해제. Deleting
/// requires the project to be archived first and cascades to its sessions and history. While the
/// delete button is armed (`delete_armed`) it reads 정말 삭제합니다.
pub fn danger_rows_for(project_id: &str, _members: usize, archived: bool, delete_armed: bool) -> [DangerRow; 2] {
    let default_project = is_default_project(project_id);
    let archive_enabled = danger_allowed(project_id, SettingsPart::Archive);
    let archive = if archived {
        DangerRow {
            part: SettingsPart::Archive,
            title: "프로젝트 보관 해제",
            note: "보관을 해제하면 목록과 검색에 다시 표시됩니다.".to_string(),
            button: "보관 해제",
            enabled: archive_enabled,
        }
    } else {
        DangerRow {
            part: SettingsPart::Archive,
            title: "프로젝트 보관",
            note: "세션과 기록은 남고 목록과 검색에서 숨겨집니다.".to_string(),
            button: "보관",
            enabled: archive_enabled,
        }
    };
    let delete_enabled = danger_allowed(project_id, SettingsPart::Delete) && archived;
    let armed = delete_armed && delete_enabled;
    let delete_note = if default_project {
        "기본 프로젝트는 지울 수 없습니다.".to_string()
    } else if !archived {
        "먼저 보관한 뒤에 삭제할 수 있습니다.".to_string()
    } else {
        "프로젝트와 소속 세션·대화 기록이 모두 함께 삭제됩니다. 되돌릴 수 없습니다.".to_string()
    };
    let delete = DangerRow {
        part: SettingsPart::Delete,
        title: "프로젝트 삭제",
        note: delete_note,
        button: if armed { "정말 삭제합니다" } else { "삭제" },
        enabled: delete_enabled,
    };
    [archive, delete]
}

/// The line under the danger zone's title.
pub fn danger_hint(project_id: &str) -> &'static str {
    if is_default_project(project_id) {
        "되돌리기 어려운 작업입니다. 기본 프로젝트는 보관하거나 지울 수 없습니다"
    } else {
        "되돌리기 어려운 작업입니다. 삭제는 한 번 더 눌러 확인합니다"
    }
}

/// The patch of the 보관 or 보관 해제 button: the flag flipped and nothing else, so unsaved form
/// changes are not sent with it.
pub fn archive_patch(archived_now: bool) -> ProjectPatch {
    ProjectPatch { archived: Some(!archived_now), ..Default::default() }
}

/// What a tap on 프로젝트 삭제 does (FR-L26, #327). Deleting cannot be undone, so it asks twice
/// with the rule of the other destructive buttons (`ui::vcs::delete_tap`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteStep {
    /// The first tap, or a tap after the question ran out: arm and show 정말 삭제합니다.
    Arm,
    /// Too soon after arming for the question to have been read (a double click): nothing.
    Wait,
    /// The second tap: delete.
    Delete,
}

/// `armed_at` is the time (ms) of the tap that armed the button, `None` when it is not armed.
pub fn delete_step(armed_at: Option<i64>, now: i64) -> DeleteStep {
    use crate::ui::vcs::{delete_tap, DeleteTap};
    match delete_tap(armed_at.map(|at| (0, at)), 0, now) {
        DeleteTap::Arm | DeleteTap::Expired => DeleteStep::Arm,
        DeleteTap::Wait => DeleteStep::Wait,
        DeleteTap::Confirm => DeleteStep::Delete,
    }
}

/// Whether the delete button shows its question at `now`.
pub fn delete_armed(armed_at: Option<i64>, now: i64) -> bool {
    crate::ui::vcs::armed_label_visible(armed_at.map(|at| (0, at)), 0, now)
}

/// The sentence a failed delete shows. Never the server's raw text.
pub fn delete_error_message(status: Option<u16>, code: Option<&str>) -> &'static str {
    match (status, code) {
        (Some(409), Some("project_not_archived")) => "먼저 프로젝트를 보관한 뒤에 삭제할 수 있습니다.",
        (Some(409), Some("sessions_running")) => "실행 중인 세션이 있어 삭제할 수 없습니다. 세션이 끝난 뒤에 다시 시도해 주세요.",
        (Some(403), Some("protected_project")) => "기본 프로젝트는 지울 수 없습니다",
        (Some(404), _) => "프로젝트를 찾지 못했습니다",
        (Some(400), Some("not_yet")) | (Some(405), _) => "이 서버는 프로젝트 삭제를 아직 지원하지 않습니다. 서버를 업데이트해 주세요",
        _ => "프로젝트를 지우지 못했습니다. 잠시 뒤에 다시 시도해 주세요",
    }
}

/// True for a 403 `owner_required` (FR-S12): the failure goes to the notice, which opens the
/// owner sheet, instead of a sentence in the settings sheet.
pub fn is_owner_refusal(status: Option<u16>, code: Option<&str>) -> bool {
    status == Some(403) && code == Some("owner_required")
}

/// The open project view after `removed` was deleted: closed when it showed that project.
pub fn full_after_removal(full: Option<&str>, removed: &str) -> Option<String> {
    full.filter(|f| *f != removed).map(str::to_string)
}

/// Columns of the leader choice (`.ps-leads`): 3, 2 at 900 dp and below, 1 at 560 and below.
pub fn leader_cols(width_dp: f32) -> usize {
    if width_dp <= 560.0 {
        1
    } else if width_dp <= 900.0 {
        2
    } else {
        3
    }
}


/// Whether 저장 can do anything: the server saves project metadata (FR-L26).
pub const SETTINGS_SAVE_AVAILABLE: bool = true;

/// The form of the settings sheet. Empty strings mean "not set"; `style` is the server's
/// character style id, empty for none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsDraft {
    pub name: String,
    pub repo: String,
    pub branch: String,
    pub agent: String,
    pub model: String,
    pub mode: String,
    pub style: String,
    /// Chosen leader session id (FR-L26, #384), `None` when unset.
    pub leader_session: Option<String>,
    /// Profile override per harness kind to account id (FR-L26); a harness without an entry is
    /// unset. A deleted account (`null` on the server) counts as unset.
    pub overrides: BTreeMap<String, String>,
    /// Harnesses whose dangling (`null`) server entry the user chose to clear with 기본.
    pub cleared: std::collections::BTreeSet<String>,
}

impl SettingsDraft {
    /// The form as the server holds the project now.
    pub fn from_project(p: &Project) -> SettingsDraft {
        let d = p.member_defaults.clone().unwrap_or_default();
        SettingsDraft {
            name: p.name.clone(),
            repo: p.repository.clone().unwrap_or_default(),
            branch: p.default_branch.clone().unwrap_or_default(),
            agent: d.agent.unwrap_or_default(),
            model: d.model.unwrap_or_default(),
            mode: d.mode.unwrap_or_default(),
            style: p.character_style.clone().unwrap_or_default(),
            leader_session: p.leader_session.clone(),
            overrides: loaded_overrides(p),
            cleared: Default::default(),
        }
    }

    fn defaults(&self) -> MemberDefaults {
        let some = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_string());
        MemberDefaults { agent: some(&self.agent), model: some(&self.model), mode: some(&self.mode) }
    }
}

/// The overrides the server holds, without the entries whose account was deleted (FR-L26).
pub fn loaded_overrides(p: &Project) -> BTreeMap<String, String> {
    p.profile_overrides.iter().filter_map(|(h, a)| a.clone().map(|a| (h.clone(), a))).collect()
}

/// The `profile_overrides` object the patch sends: the whole map, every entry an account id.
pub fn overrides_map(draft: &BTreeMap<String, String>) -> BTreeMap<String, Option<String>> {
    draft.iter().filter(|(_, a)| !a.is_empty()).map(|(h, a)| (h.clone(), Some(a.clone()))).collect()
}

/// The harnesses the override section offers: server kind and label (Claude Code, Codex and
/// Antigravity, the kinds that have accounts).
pub fn override_harnesses() -> [(&'static str, &'static str); 3] {
    [("claude-code", "Claude Code"), ("codex", "Codex"), ("antigravity", "Antigravity")]
}

/// The picker choices of one harness: "" for no fixed account, then that harness's accounts as
/// (id, label). `accounts` are (id, kind, label).
pub fn override_options(harness: &str, accounts: &[(String, String, String)]) -> Vec<(String, String)> {
    let mut v = vec![(String::new(), "기본(고정 없음)".to_string())];
    v.extend(accounts.iter().filter(|(_, k, _)| k == harness).map(|(id, _, l)| (id.clone(), l.clone())));
    v
}

/// Whether the account list the pickers read has arrived (FR-L26).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountsState {
    Loading,
    Ready,
}

impl AccountsState {
    /// An empty list is read as still loading, because the client has no separate flag.
    pub fn of(accounts: &[(String, String, String)]) -> AccountsState {
        if accounts.is_empty() { AccountsState::Loading } else { AccountsState::Ready }
    }
}

/// What the picker of `harness` shows as chosen: the account label, "기본(고정 없음)" when unset,
/// and "삭제된 계정" when the server holds a `null` for it (the account is gone) and the draft has
/// not picked another or cleared it. While the accounts load, a stored id reads "불러오는 중"
/// instead of "삭제된 계정", because the account may simply not have arrived yet.
pub fn override_label(
    server: &Project,
    draft: &SettingsDraft,
    harness: &str,
    accounts: &[(String, String, String)],
    state: AccountsState,
) -> String {
    if let Some(id) = draft.overrides.get(harness) {
        return match accounts.iter().find(|(a, _, _)| a == id) {
            Some((_, _, l)) => l.clone(),
            None if state == AccountsState::Loading => "불러오는 중".to_string(),
            None => "삭제된 계정".to_string(),
        };
    }
    if !draft.cleared.contains(harness) && matches!(server.profile_overrides.get(harness), Some(None)) {
        return "삭제된 계정".to_string();
    }
    "기본(고정 없음)".to_string()
}

/// The user picks `account_id` ("" for 기본) for `harness`. Picking 기본 on a dangling entry marks
/// it cleared, so the draft becomes dirty and 저장 sends the map without that harness.
pub fn pick_override(server: &Project, draft: &mut SettingsDraft, harness: &str, account_id: &str) {
    if account_id.is_empty() {
        draft.overrides.remove(harness);
        if matches!(server.profile_overrides.get(harness), Some(None)) {
            draft.cleared.insert(harness.to_string());
        }
    } else {
        draft.overrides.insert(harness.to_string(), account_id.to_string());
        draft.cleared.remove(harness);
    }
}

/// The patch that turns `server` into `d`: only changed fields are set (FR-L26). A field the
/// user emptied is sent empty (or `null` for the defaults and leader), which clears it. Archive
/// is sent via its own button. The name and leader of 기본 프로젝트 are never sent.
pub fn build_patch(server: &Project, d: &SettingsDraft) -> ProjectPatch {
    let was = SettingsDraft::from_project(server);
    let mut patch = ProjectPatch::default();
    let name = d.name.trim();
    if !is_default_project(&server.name) && name != was.name {
        patch.name = Some(name.to_string());
    }
    if d.repo.trim() != was.repo.trim() {
        patch.repository = Some(d.repo.trim().to_string());
    }
    if d.branch.trim() != was.branch.trim() {
        patch.default_branch = Some(d.branch.trim().to_string());
    }
    let (now, before) = (d.defaults(), was.defaults());
    if now != before {
        patch.member_defaults = Some((!now.is_empty()).then_some(now));
    }
    if d.style != was.style {
        patch.character_style = Some(d.style.clone());
    }
    if !is_default_project(&server.name) && d.leader_session != was.leader_session {
        patch.leader_session = Some(d.leader_session.clone());
    }
    if d.overrides != was.overrides || !d.cleared.is_empty() {
        patch.profile_overrides = Some(overrides_map(&d.overrides));
    }
    patch
}

/// The project the UI shows as selected after `old` was renamed to `new` (FR-L26): the new name
/// when `old` was selected, otherwise what was selected.
pub fn follow_rename(selected: Option<&str>, old: &str, new: &str) -> Option<String> {
    selected.map(|s| if s == old { new.to_string() } else { s.to_string() })
}

/// The character style to show: the server's value when it is a known id, else the one this
/// device remembers, else the default (FR-L26). One source of truth is the server.
pub fn effective_style(server: Option<&str>, device: Option<CharStyle>) -> CharStyle {
    server.and_then(CharStyle::from_id).or(device).unwrap_or_default()
}

/// Why the link to the character screen is disabled: the draft is dirty (FR-L22).
pub fn characters_link_reason(dirty: bool) -> Option<&'static str> {
    dirty.then_some("저장하지 않은 변경 사항이 있어 열 수 없습니다")
}

/// What closing the settings sheet does (FR-L26).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseAction {
    /// Ignored: a save is in flight.
    Ignore,
    /// Ask first: the draft differs from the loaded project.
    Confirm,
    Close,
}

/// The unsaved-changes guard of the settings sheet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SettingsGuard {
    pub saving: bool,
    pub dirty: bool,
}

impl SettingsGuard {
    /// `dirty` when the patch the draft would send is not empty.
    pub fn new(saving: bool, patch_is_empty: bool) -> SettingsGuard {
        SettingsGuard { saving, dirty: !patch_is_empty }
    }

    /// What 취소, X and the backdrop do.
    pub fn on_close(self) -> CloseAction {
        if self.saving {
            CloseAction::Ignore
        } else if self.dirty {
            CloseAction::Confirm
        } else {
            CloseAction::Close
        }
    }
}

/// The question of the unsaved-changes confirm.
pub const DISCARD_QUESTION: &str = "저장하지 않은 변경이 있습니다. 닫을까요?";

/// The harness choices of the new member defaults: server kind and label. The first is "none".
/// Gemini is left out because the server has no Gemini kind (see `Harness::kind`).
pub fn agent_options() -> Vec<(&'static str, &'static str)> {
    vec![("", "기본값 없음"), ("claude-code", "Claude Code"), ("codex", "Codex")]
}

/// The permission mode choices of the new member defaults: server id and label.
pub fn mode_options() -> Vec<(&'static str, &'static str)> {
    use crate::ui::home::PermMode;
    vec![
        ("", "기본값 없음"),
        ("ask", PermMode::Ask.label()),
        ("edits", PermMode::Edits.label()),
        ("plan", PermMode::Plan.label()),
        ("bypass", PermMode::Bypass.label()),
    ]
}

/// The label shown on a `새 팀원 기본값` picker chip when no project default is set.
pub const DEFAULT_UNSET_LABEL: &str = "기본(설정 없음)";

/// One option of the compact model/harness picker chip in `새 팀원 기본값`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberDefaultModelChoice {
    /// Server agent kind (`""` when unset).
    pub agent: &'static str,
    /// Model id (`""` when unset).
    pub model: &'static str,
    /// Display label on the chip and in the menu (e.g. `"Claude Code · Opus 5.5 · 높음"`).
    pub label: String,
    /// Optional description shown under the title in the popover.
    pub description: Option<&'static str>,
}

/// Model/harness choices for the `새 팀원 기본값` picker chip: `"기본(설정 없음)"` followed by
/// one entry per `(harness, model)` from [`crate::ui::home::model_catalog`] for harnesses the
/// server supports (`claude-code`, `codex`).
pub fn member_default_model_choices() -> Vec<MemberDefaultModelChoice> {
    let supported = agent_options();
    let mut out = vec![MemberDefaultModelChoice {
        agent: "",
        model: "",
        label: DEFAULT_UNSET_LABEL.to_string(),
        description: Some("새 팀원이 앱 기본값을 따릅니다"),
    }];
    let mut seen: Vec<(&'static str, &'static str)> = Vec::new();
    for opt in crate::ui::home::model_catalog() {
        let kind = opt.harness.kind();
        if !supported.iter().any(|(k, _)| *k == kind) {
            continue;
        }
        if seen.contains(&(kind, opt.model)) {
            continue;
        }
        seen.push((kind, opt.model));
        out.push(MemberDefaultModelChoice {
            agent: kind,
            model: opt.model,
            label: opt.chip_label(),
            description: None,
        });
    }
    out
}

/// The text shown on the `새 팀원 기본값` model/harness chip for `(agent, model)`.
pub fn member_default_model_label(agent: &str, model: &str) -> String {
    let agent = agent.trim();
    let model = model.trim();
    if agent.is_empty() && model.is_empty() {
        return DEFAULT_UNSET_LABEL.to_string();
    }
    let choices = member_default_model_choices();
    if let Some(found) = choices.iter().find(|c| c.agent == agent && c.model == model) {
        return found.label.clone();
    }
    let harness_label = crate::ui::home::Harness::from_kind(agent)
        .map(|h| h.label())
        .unwrap_or(agent);
    if model.is_empty() {
        harness_label.to_string()
    } else if agent.is_empty() {
        model.to_string()
    } else {
        format!("{harness_label} \u{00b7} {model}")
    }
}

/// Permission mode choices for the `새 팀원 기본값` compact mode picker chip: `"기본(설정 없음)"`
/// followed by the server's four permission modes.
pub fn member_default_mode_choices() -> Vec<(&'static str, &'static str)> {
    use crate::ui::home::PermMode;
    vec![
        ("", DEFAULT_UNSET_LABEL),
        ("ask", PermMode::Ask.label()),
        ("edits", PermMode::Edits.label()),
        ("plan", PermMode::Plan.label()),
        ("bypass", PermMode::Bypass.label()),
    ]
}

/// The text shown on the `새 팀원 기본값` permission mode chip for `mode`.
pub fn member_default_mode_label(mode: &str) -> &'static str {
    let mode = mode.trim();
    member_default_mode_choices()
        .into_iter()
        .find(|(k, _)| *k == mode)
        .map(|(_, l)| l)
        .unwrap_or(DEFAULT_UNSET_LABEL)
}

/// The sentence a failed save shows. Never the server's raw text.
/// The server maps every bad value to `invalid_field`, so the account sentence is used only when
/// the patch carried nothing but `profile_overrides`.
pub fn patch_error_message(status: Option<u16>, code: Option<&str>, patch: &ProjectPatch) -> &'static str {
    let account_only = patch.profile_overrides.is_some()
        && patch.name.is_none()
        && patch.repository.is_none()
        && patch.default_branch.is_none()
        && patch.member_defaults.is_none()
        && patch.character_style.is_none()
        && patch.leader_session.is_none();
    let archive = patch.archived.is_some();
    match (status, code) {
        (Some(409), Some("name_taken")) => "이미 있는 이름입니다",
        (Some(409), Some("sessions_running")) => "실행 중인 세션이 있어 보관할 수 없습니다. 세션이 끝난 뒤에 다시 시도해 주세요.",
        (Some(403), Some("protected_project")) if archive => "기본 프로젝트는 보관할 수 없습니다",
        (Some(400), Some("not_yet")) if archive => "이 서버는 프로젝트 보관을 아직 지원하지 않습니다. 서버를 업데이트해 주세요",
        (Some(403), Some("protected_project")) => "기본 프로젝트의 이름은 바꿀 수 없습니다",
        (Some(400), Some("invalid_field")) if !account_only => "입력한 값을 서버가 받아들이지 않았습니다. 내용을 확인하십시오.",
        (Some(400), Some("invalid_field")) => "선택한 계정을 쓸 수 없습니다. 다시 고르십시오.",
        (Some(400), _) => "입력한 값이 올바르지 않습니다",
        (Some(404), _) => "프로젝트를 찾지 못했습니다",
        _ => "설정을 저장하지 못했습니다. 잠시 뒤에 다시 시도해 주세요",
    }
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

/// The form of the 새 프로젝트 sheet (FR-L26, FR-L1). Empty strings mean "not filled"; `style` is
/// the server's character style id, empty until the user picks one (the picker shows 몽글 then).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewProjectDraft {
    pub name: String,
    pub repo: String,
    pub branch: String,
    pub style: String,
}

/// Why the 새 프로젝트 sheet cannot create yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewProjectError {
    /// A rule the settings sheet has too (name, repository, branch).
    Field(SettingsError),
    NameTaken,
}

impl NewProjectError {
    pub fn message(self) -> &'static str {
        match self {
            NewProjectError::Field(e) => e.message(),
            NewProjectError::NameTaken => "같은 이름의 프로젝트가 이미 있습니다",
        }
    }
}

/// The sentence a failed `POST /api/projects` shows. Never the server's raw text.
pub fn create_error_message(code: Option<&str>) -> &'static str {
    match code {
        Some("name_taken") => "이미 있는 이름입니다",
        Some("protected_project") => "기본 프로젝트와 같은 이름은 쓸 수 없습니다",
        _ => "프로젝트를 만들지 못했습니다. 잠시 뒤에 다시 시도해 주세요",
    }
}

/// What the sheet does once the follow-up `PATCH` has answered. The project exists in both cases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProjectOutcome {
    pub project_kept: bool,
    pub notice: Option<&'static str>,
}

impl NewProjectOutcome {
    pub fn after_followup(failed: bool) -> NewProjectOutcome {
        NewProjectOutcome {
            project_kept: true,
            notice: failed.then_some(
                "프로젝트는 만들었지만 저장소와 캐릭터 설정은 저장하지 못했습니다. 프로젝트 설정에서 다시 저장해 주세요",
            ),
        }
    }
}

impl NewProjectDraft {
    pub fn trimmed_name(&self) -> String {
        self.name.trim().to_string()
    }

    /// Nothing typed or picked: closing needs no question.
    pub fn is_blank(&self) -> bool {
        *self == NewProjectDraft::default()
    }

    /// Name rules as the settings sheet and a name not taken (an empty branch means `develop`). `existing` are the project names that exist now (the server's create is idempotent,
    /// so a taken name must be refused here).
    pub fn validate(&self, existing: &[String]) -> Vec<NewProjectError> {
        let as_settings = SettingsDraft {
            name: self.name.clone(),
            repo: self.repo.clone(),
            branch: self.branch.clone(),
            ..Default::default()
        };
        let mut errors: Vec<NewProjectError> =
            validate_settings(&as_settings, "").into_iter().map(NewProjectError::Field).collect();
        let name = self.name.trim();
        if !name.is_empty() && existing.iter().any(|e| e == name) {
            errors.push(NewProjectError::NameTaken);
        }
        errors
    }

    /// The branch used when the field is empty (the mockup shows `develop`).
    pub const DEFAULT_BRANCH: &'static str = "develop";

    /// The typed branch, or `develop` when the field is empty.
    pub fn effective_branch(&self) -> String {
        let b = self.branch.trim();
        if b.is_empty() { Self::DEFAULT_BRANCH.to_string() } else { b.to_string() }
    }

    /// The second call after `POST /api/projects` (which takes only a name): the filled fields,
    /// empty when only the name was given.
    pub fn followup_patch(&self) -> ProjectPatch {
        let some = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_string());
        ProjectPatch {
            repository: some(&self.repo),
            default_branch: (!self.repo.trim().is_empty() || !self.branch.trim().is_empty())
                .then(|| self.effective_branch()),
            character_style: some(&self.style),
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The project page's 루틴 tile (FR-L26, #327)
// It lives here, not in `routines.rs`, and only reads that module's types.
// ---------------------------------------------------------------------------------------------

/// The tile's state of an event routine: it has no next run, it waits for its event.
pub const TILE_EVENT_WAITING: &str = "이벤트 대기";
/// Rows the project page's 루틴 tile lists; the rest is counted.
pub const TILE_MAX_ROWS: usize = 3;
pub const TILE_LOADING: &str = "루틴을 불러오는 중입니다";
pub const TILE_FAILED: &str = "루틴을 불러오지 못했습니다";
pub const TILE_EMPTY: &str = "이 프로젝트에는 루틴이 없습니다";

/// What the project page knows of its project's routines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TileLoad {
    /// Asked, not answered yet.
    Loading,
    /// The list call failed.
    Failed,
    /// The server's schedules of the project, each with its newest run.
    Loaded(Vec<Entry>),
}

/// One routine of the tile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileRow {
    pub id: String,
    pub name: String,
    /// "다음 실행 오늘 09:00", [`TILE_EVENT_WAITING`], or that it is off.
    pub when: String,
    /// "완료 · 3시간 전" or "실행 기록 없음".
    pub last: String,
}

/// The project page's 루틴 tile as text. Loading, a failed call and an empty project each have a
/// sentence in `summary` and no rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRoutineTile {
    pub summary: String,
    pub rows: Vec<TileRow>,
    /// "외 2개" when more than [`TILE_MAX_ROWS`] routines exist.
    pub more: Option<String>,
}

impl ProjectRoutineTile {
    /// The tile's lines, top to bottom: the summary, one line per row, then the rest count.
    pub fn lines(&self) -> Vec<String> {
        let mut out = vec![self.summary.clone()];
        out.extend(self.rows.iter().map(|r| format!("{} · {} · {}", r.name, r.when, r.last)));
        out.extend(self.more.clone());
        out
    }
}

/// When a routine runs next, as its state: off, waiting for its event, or the next run.
pub fn routine_state_text(e: &Entry, now: Minutes) -> String {
    let r = &e.routine;
    if !r.enabled {
        return "꺼져 있습니다".to_string();
    }
    match r.trigger {
        Trigger::Event => TILE_EVENT_WAITING.to_string(),
        Trigger::Time => match e.next_at.or_else(|| next_run(&r.cron, now)) {
            Some(t) => format!("다음 실행 {}", format_next(t, now)),
            None => "다음 실행 없음".to_string(),
        },
    }
}

fn same_project(a: &str, b: &str) -> bool {
    a == b || (is_default_project(a) && is_default_project(b))
}

/// The tile for `project`: how many routines it has, how many are on and how many last ended
/// in failure, then the first [`TILE_MAX_ROWS`] with next run and last result.
pub fn project_routine_tile(load: &TileLoad, project: &str, now: Minutes) -> ProjectRoutineTile {
    let sentence = |text: &str| ProjectRoutineTile { summary: text.to_string(), rows: Vec::new(), more: None };
    let entries: Vec<&Entry> = match load {
        TileLoad::Loading => return sentence(TILE_LOADING),
        TileLoad::Failed => return sentence(TILE_FAILED),
        TileLoad::Loaded(all) => all.iter().filter(|e| same_project(&e.routine.sel.project, project)).collect(),
    };
    if entries.is_empty() {
        return sentence(TILE_EMPTY);
    }
    let on = entries.iter().filter(|e| e.routine.enabled).count();
    let failed = entries
        .iter()
        .filter(|e| e.runs.first().is_some_and(|r| r.status == RunStatus::Failed))
        .count();
    let mut summary = format!("루틴 {}개 · 켜짐 {on}개", entries.len());
    if failed > 0 {
        summary.push_str(&format!(" · 최근 실패 {failed}개"));
    }
    let rows = entries
        .iter()
        .take(TILE_MAX_ROWS)
        .map(|e| TileRow {
            id: e.routine.id.clone(),
            name: e.routine.name.clone(),
            when: routine_state_text(e, now),
            last: last_run_text(&e.runs, now),
        })
        .collect();
    let rest = entries.len().saturating_sub(TILE_MAX_ROWS);
    ProjectRoutineTile { summary, rows, more: (rest > 0).then(|| format!("외 {rest}개")) }
}

/// One project's schedules, each with its newest run, for the tile. A failed list is
/// [`TileLoad::Failed`]; a failed run list leaves that routine without a last result.
pub async fn load_project_tile(api: &ember_client::api::Api, project: &str) -> TileLoad {
    let list = match api.schedules(Some(project)).await {
        Ok(list) => list,
        Err(e) => {
            tracing::warn!("loading the project's routines failed: {e}");
            return TileLoad::Failed;
        }
    };
    let mut out = Vec::with_capacity(list.len());
    for s in &list {
        let runs = api.schedule_runs(&s.id, Some(1)).await.unwrap_or_default();
        out.push(schedule_to_entry(s, &runs));
    }
    TileLoad::Loaded(out)
}

// ---------------------------------------------------------------------------------------------
// The project page's 채널 tile (FR-L26, #327)
// ---------------------------------------------------------------------------------------------

pub const CHANNEL_TILE_LOADING: &str = "채널을 불러오는 중입니다";
pub const CHANNEL_TILE_FAILED: &str = "채널을 불러오지 못했습니다";
pub const CHANNEL_TILE_EMPTY: &str = "이 프로젝트에 채널이 없습니다";

/// What the project page knows of its project's channels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelTileLoad {
    /// Asked, not answered yet.
    Loading,
    /// The list call failed.
    Failed,
    /// The server's channels of the project.
    Loaded(Vec<crate::ui::channels::Room>),
}

/// One channel row of the project page's 채널 tile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelTileRow {
    pub id: String,
    pub name: String,
    pub members: usize,
    pub unread: u32,
}

/// The project page's 채널 tile as text. Loading, a failed call and an empty project each have a
/// sentence in `summary` and no rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectChannelTile {
    pub summary: String,
    pub rows: Vec<ChannelTileRow>,
    /// "외 2개" when more than [`TILE_MAX_ROWS`] channels exist.
    pub more: Option<String>,
}

impl ProjectChannelTile {
    /// The tile's lines, top to bottom: the summary, one line per row, then the rest count.
    pub fn lines(&self) -> Vec<String> {
        let mut out = vec![self.summary.clone()];
        out.extend(self.rows.iter().map(|r| {
            if r.unread > 0 {
                format!("#{} \u{00b7} 멤버 {}명 \u{00b7} 읽지 않음 {}", r.name, r.members, r.unread)
            } else {
                format!("#{} \u{00b7} 멤버 {}명", r.name, r.members)
            }
        }));
        out.extend(self.more.clone());
        out
    }
}

/// The 채널 tile for `project`: how many channels it has, then the first [`TILE_MAX_ROWS`] with
/// member count and unread count.
pub fn project_channel_tile(load: &ChannelTileLoad, project: &str) -> ProjectChannelTile {
    let sentence = |text: &str| ProjectChannelTile { summary: text.to_string(), rows: Vec::new(), more: None };
    let rooms: Vec<&crate::ui::channels::Room> = match load {
        ChannelTileLoad::Loading => return sentence(CHANNEL_TILE_LOADING),
        ChannelTileLoad::Failed => return sentence(CHANNEL_TILE_FAILED),
        ChannelTileLoad::Loaded(all) => all.iter().filter(|r| same_project(&r.project, project)).collect(),
    };
    if rooms.is_empty() {
        return sentence(CHANNEL_TILE_EMPTY);
    }
    let summary = format!("채널 {}개", rooms.len());
    let rows = rooms
        .iter()
        .take(TILE_MAX_ROWS)
        .map(|r| ChannelTileRow {
            id: r.id.clone(),
            name: r.name.clone(),
            members: r.members.len(),
            unread: r.unread,
        })
        .collect();
    let rest = rooms.len().saturating_sub(TILE_MAX_ROWS);
    ProjectChannelTile { summary, rows, more: (rest > 0).then(|| format!("외 {rest}개")) }
}

/// One project's channels for the project page's 채널 tile (FR-L26, #327).
pub async fn load_project_channels(api: &ember_client::api::Api, project: &str) -> ChannelTileLoad {
    match api.channels(Some(project)).await {
        Ok(list) => ChannelTileLoad::Loaded(crate::ui::channels::rooms_from(&list, &[])),
        Err(e) => {
            tracing::warn!("loading the project's channels failed: {e}");
            ChannelTileLoad::Failed
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The settings sheet's 연결 section (FR-L26, #327)
// ---------------------------------------------------------------------------------------------

/// Load state of one linked resource count in the project settings `연결` section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkCountLoad {
    Loading,
    Failed,
    Loaded(usize),
}

/// Counts of routines and channels linked to a project in the settings sheet (FR-L26, #327).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectLinks {
    pub routines: LinkCountLoad,
    pub channels: LinkCountLoad,
}

impl ProjectLinks {
    pub const fn loading() -> ProjectLinks {
        ProjectLinks { routines: LinkCountLoad::Loading, channels: LinkCountLoad::Loading }
    }

    /// The label of the 루틴 link button.
    pub fn routine_button_label(self) -> String {
        link_button_label("루틴", self.routines)
    }

    /// The label of the 채널 link button.
    pub fn channel_button_label(self) -> String {
        link_button_label("채널", self.channels)
    }

    /// The summary line under the `연결` buttons: loading, failure, empty or live counts.
    pub fn summary_line(self) -> String {
        match (self.routines, self.channels) {
            (LinkCountLoad::Loading, _) | (_, LinkCountLoad::Loading) => {
                "연결된 루틴과 채널을 불러오는 중입니다".to_string()
            }
            (LinkCountLoad::Failed, LinkCountLoad::Failed) => {
                "연결된 루틴과 채널을 불러오지 못했습니다".to_string()
            }
            (LinkCountLoad::Loaded(0), LinkCountLoad::Loaded(0)) => {
                "이 프로젝트에 연결된 루틴과 채널이 없습니다".to_string()
            }
            (r, c) => format!("{} \u{00b7} {}", link_count_text("루틴", r), link_count_text("채널", c)),
        }
    }
}

fn link_button_label(name: &str, load: LinkCountLoad) -> String {
    match load {
        LinkCountLoad::Loaded(n) => format!("{name} ({n})"),
        LinkCountLoad::Loading | LinkCountLoad::Failed => name.to_string(),
    }
}

fn link_count_text(name: &str, load: LinkCountLoad) -> String {
    match load {
        LinkCountLoad::Loaded(0) => format!("{name} 없음"),
        LinkCountLoad::Loaded(n) => format!("{name} {n}개"),
        LinkCountLoad::Loading => format!("{name} 불러오는 중"),
        LinkCountLoad::Failed => format!("{name} 불러오지 못함"),
    }
}

/// Counts for a project from already-loaded sample or cached entries and rooms (demo mode, tests).
pub fn project_links_from(project: &str, routines: &[Entry], rooms: &[crate::ui::channels::Room]) -> ProjectLinks {
    ProjectLinks {
        routines: LinkCountLoad::Loaded(
            routines.iter().filter(|e| same_project(&e.routine.sel.project, project)).count(),
        ),
        channels: LinkCountLoad::Loaded(
            rooms.iter().filter(|r| same_project(&r.project, project)).count(),
        ),
    }
}

/// Loads the project's routine and channel counts for the settings sheet `연결` section (FR-L26, #327).
pub async fn load_project_links(api: &ember_client::api::Api, project: &str) -> ProjectLinks {
    let routines = match api.schedules(Some(project)).await {
        Ok(list) => LinkCountLoad::Loaded(list.len()),
        Err(e) => {
            tracing::warn!("loading the project's routine count failed: {e}");
            LinkCountLoad::Failed
        }
    };
    let channels = match api.channels(Some(project)).await {
        Ok(list) => LinkCountLoad::Loaded(list.len()),
        Err(e) => {
            tracing::warn!("loading the project's channel count failed: {e}");
            LinkCountLoad::Failed
        }
    };
    ProjectLinks { routines, channels }
}

#[cfg(test)]
mod tests {
    #[test]
    fn fr_l26_settings_sections_follow_the_mockup_order() {
        use super::{SettingsPart as P, SETTINGS_ORDER};
        assert_eq!(
            SETTINGS_ORDER,
            [P::Name, P::Computers, P::MemberDefaults, P::ProfileOverride, P::CharacterStyle, P::Leader, P::Links, P::Archive]
        );
    }

    #[test]
    fn fr_l26_danger_rows_say_what_each_does() {
        let [archive, delete] = super::danger_rows("ember", 3);
        assert_eq!((archive.title, archive.button, archive.enabled), ("프로젝트 보관", "보관", true));
        assert_eq!(archive.note, "세션과 기록은 남고 목록과 검색에서 숨겨집니다.");
        assert_eq!((delete.title, delete.button, delete.enabled), ("프로젝트 삭제", "삭제", false));
        assert_eq!(delete.note, "먼저 보관한 뒤에 삭제할 수 있습니다.");
        let [_, d] = super::danger_rows(super::DEFAULT_PROJECT_ID, 3);
        assert_eq!(d.note, "기본 프로젝트는 지울 수 없습니다.");
        assert!(!d.enabled);
    }

    #[test]
    fn fr_l26_leader_choice_columns_follow_the_breakpoints() {
        assert_eq!(super::leader_cols(1440.0), 3);
        assert_eq!(super::leader_cols(900.0), 2);
        assert_eq!(super::leader_cols(768.0), 2);
        assert_eq!(super::leader_cols(390.0), 1);
    }


    #[test]
    fn fr_l22_disabled_characters_link_says_why() {
        assert_eq!(characters_link_reason(true), Some("저장하지 않은 변경 사항이 있어 열 수 없습니다"));
        assert_eq!(characters_link_reason(false), None);
    }

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

    fn mem(n: &str) -> Member {
        Member {
            session_id: n.into(),
            name: n.into(),
            task: String::new(),
            agent: "claude-code".into(),
            status: LauncherStatus::Idle,
            leader: false,
            updated_at: 0,
        }
    }

    #[test]
    fn fr_l1_deck_names_lists_first_three() {
        let rest: Vec<Member> = ["a", "b", "c", "d"].iter().map(|n| mem(n)).collect();
        assert_eq!(deck_names(&rest, DECK_NAMES), "a, b, c");
        assert_eq!(deck_names(&rest[..2], DECK_NAMES), "a, b");
        assert_eq!(deck_names(&[], DECK_NAMES), "");
    }

    #[test]
    fn fr_l1_deck_avatars_caps_at_four() {
        let rest: Vec<Member> = (0..7).map(|i| mem(&format!("m{i}"))).collect();
        assert_eq!(deck_avatars(&rest, DECK_AVATARS).len(), 4);
        assert_eq!(deck_avatars(&rest[..2], DECK_AVATARS).len(), 2);
    }

    #[test]
    fn fr_l1_deck_label_is_count_and_suffix() {
        assert_eq!(deck_label(3), "외 3명");
    }

    #[test]
    fn fr_l1_deck_names_fit_or_ellipsis() {
        let rest: Vec<Member> = ["김하늘", "이서연", "박지후", "최민준"].iter().map(|n| mem(n)).collect();
        // 3 names, "김하늘, 이서연, 박지후" is 13 chars: fits at 13, ellipsis below.
        assert_eq!(deck_names_fit(&rest, DECK_NAMES, 13), "김하늘, 이서연, 박지후");
        assert_eq!(deck_names_fit(&rest, DECK_NAMES, 8), "김하늘, 이서…");
        assert_eq!(deck_names_fit(&rest, DECK_NAMES, 1), "…");
        assert_eq!(deck_names_fit(&[], DECK_NAMES, 8), "");
    }

    #[test]
    fn fr_l26_strip_scroll_step_is_80_percent() {
        assert_eq!(strip_scroll_target(0.0, 500.0, 2000.0, 1.0), 400.0);
        assert_eq!(strip_scroll_target(800.0, 500.0, 2000.0, -1.0), 400.0);
    }

    #[test]
    fn fr_l26_strip_scroll_clamps() {
        assert_eq!(strip_scroll_target(100.0, 500.0, 2000.0, -1.0), 0.0);
        assert_eq!(strip_scroll_target(1400.0, 500.0, 2000.0, 1.0), 1500.0);
    }

    #[test]
    fn fr_l26_strip_arrow_is_inert_at_the_end_it_points_to() {
        // #671: left at the start, right at the end, both live in between.
        assert!(!strip_arrow_enabled(0.0, 300.0, -1.0));
        assert!(strip_arrow_enabled(0.0, 300.0, 1.0));
        assert!(strip_arrow_enabled(120.0, 300.0, -1.0));
        assert!(strip_arrow_enabled(120.0, 300.0, 1.0));
        assert!(strip_arrow_enabled(300.0, 300.0, -1.0));
        assert!(!strip_arrow_enabled(300.0, 300.0, 1.0));
        // A fractional rest position still counts as the end.
        assert!(!strip_arrow_enabled(299.8, 300.0, 1.0));
        assert!(!strip_arrow_enabled(0.2, 300.0, -1.0));
        // Content that fits: neither arrow moves anything.
        assert!(!strip_arrow_enabled(0.0, 0.0, -1.0));
        assert!(!strip_arrow_enabled(0.0, 0.0, 1.0));
    }

    #[test]
    fn fr_l26_strip_arrows_hidden_when_it_fits() {
        assert!(!strip_arrows_visible(0.0));
        assert!(strip_arrows_visible(120.0));
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
            ..Default::default()
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

    // FR-L26: only what the server supports is enabled; the rest says the server cannot yet.
    #[test]
    fn fr_l26_the_server_saves_metadata_leader_profile_override_archive_and_delete() {
        for part in [
            SettingsPart::Computers,
            SettingsPart::Name,
            SettingsPart::Repository,
            SettingsPart::CharacterStyle,
            SettingsPart::MemberDefaults,
            SettingsPart::Leader,
            SettingsPart::ProfileOverride,
            SettingsPart::Archive,
            SettingsPart::Delete,
        ] {
            assert!(part_supported(part), "{part:?}");
        }
        assert!(!part_supported(SettingsPart::Links));
        assert_eq!(SOON, "준비 중입니다");
        assert!(SETTINGS_SAVE_AVAILABLE);
    }

    fn server_project() -> Project {
        Project {
            name: "ember".into(),
            repository: Some("github.com/a/b".into()),
            default_branch: Some("develop".into()),
            member_defaults: Some(MemberDefaults { agent: Some("codex".into()), model: None, mode: Some("plan".into()) }),
            character_style: Some("crew".into()),
            ..Default::default()
        }
    }

    #[test]
    fn fr_l26_an_unchanged_form_builds_an_empty_patch() {
        let p = server_project();
        assert!(build_patch(&p, &SettingsDraft::from_project(&p)).is_empty());
        let mut d = SettingsDraft::from_project(&p);
        d.name = "  ember  ".into();
        assert!(build_patch(&p, &d).is_empty(), "whitespace around the same name is no change");
    }

    #[test]
    fn fr_l26_the_patch_holds_only_the_changed_fields() {
        let p = server_project();
        let mut d = SettingsDraft::from_project(&p);
        d.name = "ember2".into();
        d.branch = "main".into();
        let patch = build_patch(&p, &d);
        assert_eq!(patch.name.as_deref(), Some("ember2"));
        assert_eq!(patch.default_branch.as_deref(), Some("main"));
        assert_eq!((patch.repository, patch.member_defaults, patch.character_style), (None, None, None));
        let mut d = SettingsDraft::from_project(&p);
        d.model = "opus".into();
        d.style = "cream".into();
        let patch = build_patch(&p, &d);
        let md = patch.member_defaults.clone().flatten().expect("defaults sent");
        assert_eq!((md.agent.as_deref(), md.model.as_deref(), md.mode.as_deref()), (Some("codex"), Some("opus"), Some("plan")));
        assert_eq!(patch.character_style.as_deref(), Some("cream"));
        assert!(patch.name.is_none());
    }

    fn acc() -> Vec<(String, String, String)> {
        vec![
            ("a1".into(), "claude-code".into(), "개인".into()),
            ("a2".into(), "claude-code".into(), "회사".into()),
            ("c1".into(), "codex".into(), "코덱스".into()),
        ]
    }

    #[test]
    fn fr_l26_profile_override_unchanged_sends_nothing_and_a_deleted_account_is_unset() {
        let mut p = server_project();
        p.profile_overrides.insert("codex".into(), Some("c1".into()));
        p.profile_overrides.insert("claude-code".into(), None);
        let d = SettingsDraft::from_project(&p);
        assert_eq!(d.overrides.len(), 1, "a null entry counts as unset");
        assert!(build_patch(&p, &d).profile_overrides.is_none());
        assert_eq!(override_label(&p, &d, "claude-code", &acc(), AccountsState::Ready), "삭제된 계정");
        assert_eq!(override_label(&p, &d, "codex", &acc(), AccountsState::Ready), "코덱스");
        assert_eq!(override_label(&p, &d, "antigravity", &acc(), AccountsState::Ready), "기본(고정 없음)");
    }

    #[test]
    fn fr_l26_profile_override_a_change_sends_the_whole_map() {
        let mut p = server_project();
        p.profile_overrides.insert("codex".into(), Some("c1".into()));
        let mut d = SettingsDraft::from_project(&p);
        d.overrides.insert("claude-code".into(), "a2".into());
        let m = build_patch(&p, &d).profile_overrides.expect("sent");
        assert_eq!(m.len(), 2);
        assert_eq!(m.get("codex"), Some(&Some("c1".to_string())));
        assert_eq!(m.get("claude-code"), Some(&Some("a2".to_string())));
        d.overrides.clear();
        assert_eq!(build_patch(&p, &d).profile_overrides, Some(BTreeMap::new()), "clearing all sends {{}}");
    }

    #[test]
    fn fr_l26_profile_override_options_list_that_harness_only() {
        let o = override_options("claude-code", &acc());
        assert_eq!(o.iter().map(|(i, _)| i.as_str()).collect::<Vec<_>>(), ["", "a1", "a2"]);
        assert_eq!(o[0].1, "기본(고정 없음)");
        assert_eq!(override_options("antigravity", &acc()).len(), 1);
        assert_eq!(override_harnesses().len(), 3);
    }

    #[test]
    fn fr_l26_profile_override_invalid_field_reads_a_fixed_sentence() {
        let m = patch_error_message(Some(400), Some("invalid_field"), &only_overrides());
        assert_eq!(m, "선택한 계정을 쓸 수 없습니다. 다시 고르십시오.");
        assert!(!m.contains('\u{2014}'));
    }

    fn only_overrides() -> ProjectPatch {
        ProjectPatch { profile_overrides: Some(BTreeMap::new()), ..Default::default() }
    }

    #[test]
    fn fr_l26_invalid_field_message_depends_on_the_patch() {
        let account = patch_error_message(Some(400), Some("invalid_field"), &only_overrides());
        assert_eq!(account, "선택한 계정을 쓸 수 없습니다. 다시 고르십시오.");
        let mixed = ProjectPatch { repository: Some("x".into()), ..only_overrides() };
        let repo = ProjectPatch { repository: Some("x".into()), ..Default::default() };
        for p in [&mixed, &repo] {
            assert_eq!(patch_error_message(Some(400), Some("invalid_field"), p), "입력한 값을 서버가 받아들이지 않았습니다. 내용을 확인하십시오.");
        }
    }

    #[test]
    fn fr_l26_override_label_while_accounts_load() {
        let mut p = server_project();
        p.profile_overrides.insert("codex".into(), Some("c1".into()));
        let d = SettingsDraft::from_project(&p);
        assert_eq!(AccountsState::of(&[]), AccountsState::Loading);
        assert_eq!(AccountsState::of(&acc()), AccountsState::Ready);
        assert_eq!(override_label(&p, &d, "codex", &[], AccountsState::Loading), "불러오는 중");
        assert_eq!(override_label(&p, &d, "codex", &acc(), AccountsState::Ready), "코덱스");
        let mut gone = d.clone();
        gone.overrides.insert("codex".into(), "zz".into());
        assert_eq!(override_label(&p, &gone, "codex", &acc(), AccountsState::Ready), "삭제된 계정");
    }

    #[test]
    fn fr_l26_a_dangling_override_can_be_cleared() {
        let mut p = server_project();
        p.profile_overrides.insert("codex".into(), Some("c1".into()));
        p.profile_overrides.insert("claude-code".into(), None);
        let mut d = SettingsDraft::from_project(&p);
        assert!(build_patch(&p, &d).is_empty(), "untouched is clean");
        pick_override(&p, &mut d, "claude-code", "");
        assert!(!build_patch(&p, &d).is_empty(), "clearing a dangling entry makes the draft dirty");
        assert_eq!(override_label(&p, &d, "claude-code", &acc(), AccountsState::Ready), "기본(고정 없음)");
        let m = build_patch(&p, &d).profile_overrides.expect("sent");
        assert_eq!(m.len(), 1);
        assert!(m.contains_key("codex") && !m.contains_key("claude-code"));
    }

    #[test]
    fn fr_l26_emptying_a_field_sends_an_empty_value_or_null() {
        let p = server_project();
        let mut d = SettingsDraft::from_project(&p);
        d.repo.clear();
        d.agent.clear();
        d.mode.clear();
        let patch = build_patch(&p, &d);
        assert_eq!(patch.repository.as_deref(), Some(""));
        assert_eq!(patch.member_defaults, Some(None), "all defaults emptied sends null");
        let json = serde_json::to_value(&patch).unwrap();
        assert_eq!(json["repository"], "");
        assert!(json["member_defaults"].is_null() && json.get("member_defaults").is_some());
    }

    #[test]
    fn fr_l26_the_default_project_name_is_never_sent() {
        let p = Project { name: DEFAULT_PROJECT_ID.into(), ..Default::default() };
        let mut d = SettingsDraft::from_project(&p);
        d.name = "other".into();
        assert!(build_patch(&p, &d).name.is_none());
    }

    #[test]
    fn fr_l26_pickers_offer_only_ids_the_server_accepts() {
        assert_eq!(mode_options().iter().map(|m| m.0).collect::<Vec<_>>(), ["", "ask", "edits", "plan", "bypass"]);
        assert_eq!(agent_options().iter().map(|m| m.0).collect::<Vec<_>>(), ["", "claude-code", "codex"]);
        for st in crate::ui::characters::CharStyle::ALL {
            assert_eq!(crate::ui::characters::CharStyle::from_id(st.id()), Some(st));
        }
        assert_eq!(crate::ui::characters::CharStyle::from_id("x"), None);
    }

    #[test]
    fn fr_l26_failures_read_as_fixed_korean_sentences() {
        assert_eq!(patch_error_message(Some(409), Some("name_taken"), &ProjectPatch::default()), "이미 있는 이름입니다");
        assert_eq!(patch_error_message(Some(403), Some("protected_project"), &ProjectPatch::default()), "기본 프로젝트의 이름은 바꿀 수 없습니다");
        assert_eq!(patch_error_message(Some(400), Some("unknown_field"), &ProjectPatch::default()), "입력한 값이 올바르지 않습니다");
        assert_eq!(patch_error_message(Some(404), None, &ProjectPatch::default()), "프로젝트를 찾지 못했습니다");
        for (st, code) in [(None, None), (Some(500), None), (Some(502), Some("x"))] {
            let m = patch_error_message(st, code, &ProjectPatch::default());
            assert!(m.starts_with("설정을 저장하지 못했습니다") && !m.contains("500"), "{m}");
        }
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

    #[test]
    fn fr_l26_rename_updates_selection() {
        assert_eq!(follow_rename(Some("old"), "old", "new").as_deref(), Some("new"));
        assert_eq!(follow_rename(Some("other"), "old", "new").as_deref(), Some("other"));
        assert_eq!(follow_rename(None, "old", "new"), None);
    }

    #[test]
    fn fr_l26_style_prefers_server_value() {
        assert_eq!(effective_style(Some("crew"), Some(CharStyle::Cream)), CharStyle::Crew);
        assert_eq!(effective_style(None, Some(CharStyle::Cream)), CharStyle::Cream);
        assert_eq!(effective_style(Some(""), Some(CharStyle::Cream)), CharStyle::Cream);
        assert_eq!(effective_style(Some("neon"), Some(CharStyle::Cream)), CharStyle::Cream);
        assert_eq!(effective_style(Some("neon"), None), CharStyle::default());
    }

    #[test]
    fn fr_l26_guard_blocks_close_while_saving() {
        assert_eq!(SettingsGuard::new(true, false).on_close(), CloseAction::Ignore);
        assert_eq!(SettingsGuard::new(true, true).on_close(), CloseAction::Ignore);
    }

    #[test]
    fn fr_l26_guard_asks_when_dirty() {
        assert_eq!(SettingsGuard::new(false, false).on_close(), CloseAction::Confirm);
        assert_eq!(SettingsGuard::new(false, true).on_close(), CloseAction::Close);
    }

    // --- the new project sheet (FR-L26, FR-L1) ---

    fn np_draft(name: &str, repo: &str, branch: &str, style: &str) -> NewProjectDraft {
        NewProjectDraft { name: name.into(), repo: repo.into(), branch: branch.into(), style: style.into() }
    }

    #[test]
    fn fr_l26_new_project_name_only_creates_without_a_patch() {
        let d = np_draft("  ember  ", "", "", "");
        assert!(d.validate(&[]).is_empty());
        assert_eq!(d.trimmed_name(), "ember");
        assert!(d.followup_patch().is_empty());
        assert!(!d.is_blank());
    }

    #[test]
    fn fr_l26_new_project_name_rules_as_settings() {
        assert_eq!(np_draft("  ", "", "", "").validate(&[]), vec![NewProjectError::Field(SettingsError::NameEmpty)]);
        assert_eq!(np_draft("a/b", "", "", "").validate(&[]), vec![NewProjectError::Field(SettingsError::NameInvalid)]);
        assert_eq!(np_draft(&"x".repeat(65), "", "", "").validate(&[]), vec![NewProjectError::Field(SettingsError::NameTooLong)]);
        assert_eq!(np_draft("a", "", "", "").validate(&["a".to_string()]), vec![NewProjectError::NameTaken]);
        assert_eq!(NewProjectError::NameTaken.message(), "같은 이름의 프로젝트가 이미 있습니다");
    }

    #[test]
    fn fr_l26_new_project_empty_branch_means_develop() {
        let d = np_draft("ember", "github.com/a/b", "", "");
        assert!(d.validate(&[]).is_empty());
        assert_eq!(d.effective_branch(), "develop");
        let patch = d.followup_patch();
        assert_eq!(patch.repository.as_deref(), Some("github.com/a/b"));
        assert_eq!(patch.default_branch.as_deref(), Some("develop"));
        assert!(patch.name.is_none() && patch.member_defaults.is_none() && patch.character_style.is_none());
        let d = np_draft("ember", "github.com/a/b", " main ", "");
        assert_eq!(d.effective_branch(), "main");
        assert_eq!(d.followup_patch().default_branch.as_deref(), Some("main"));
        // No repository and no branch: no branch is sent.
        assert!(np_draft("ember", "", "", "").followup_patch().default_branch.is_none());
        assert_eq!(np_draft("ember", "", "dev", "").followup_patch().default_branch.as_deref(), Some("dev"));
    }

    #[test]
    fn fr_l26_new_project_create_error_maps_name_taken() {
        assert_eq!(create_error_message(Some("name_taken")), "이미 있는 이름입니다");
        assert_eq!(create_error_message(Some("protected_project")), "기본 프로젝트와 같은 이름은 쓸 수 없습니다");
        assert_eq!(create_error_message(Some("boom")), "프로젝트를 만들지 못했습니다. 잠시 뒤에 다시 시도해 주세요");
        assert_eq!(create_error_message(None), "프로젝트를 만들지 못했습니다. 잠시 뒤에 다시 시도해 주세요");
    }

    #[test]
    fn fr_l26_new_project_invalid_repository_message() {
        let d = np_draft("ember", "github.com/a b", "develop", "");
        assert_eq!(d.validate(&[]), vec![NewProjectError::Field(SettingsError::RepoHasSpace)]);
        assert_eq!(NewProjectError::Field(SettingsError::RepoHasSpace).message(), "저장소 주소에는 공백이 들어갈 수 없습니다");
        let d = np_draft("ember", "github.com/a/b", "-x", "");
        assert_eq!(d.validate(&[]), vec![NewProjectError::Field(SettingsError::BranchInvalid)]);
    }

    #[test]
    fn fr_l26_new_project_character_style_is_in_the_patch() {
        let patch = np_draft("ember", "", "", "crew").followup_patch();
        assert_eq!(patch.character_style.as_deref(), Some("crew"));
        assert!(patch.repository.is_none() && patch.default_branch.is_none());
        assert!(!patch.is_empty());
    }

    #[test]
    fn fr_l26_new_project_a_failed_followup_keeps_the_project_and_says_so() {
        let ok = NewProjectOutcome::after_followup(false);
        assert!(ok.project_kept && ok.notice.is_none());
        let bad = NewProjectOutcome::after_followup(true);
        assert!(bad.project_kept);
        assert_eq!(
            bad.notice,
            Some("프로젝트는 만들었지만 저장소와 캐릭터 설정은 저장하지 못했습니다. 프로젝트 설정에서 다시 저장해 주세요")
        );
    }

    #[test]
    fn fr_l26_new_project_guard_asks_only_after_typing() {
        assert!(NewProjectDraft::default().is_blank());
        assert_eq!(SettingsGuard::new(false, NewProjectDraft::default().is_blank()).on_close(), CloseAction::Close);
        assert_eq!(SettingsGuard::new(false, np_draft("x", "", "", "").is_blank()).on_close(), CloseAction::Confirm);
        assert_eq!(SettingsGuard::new(true, false).on_close(), CloseAction::Ignore);
    }

    // ---- archive and delete (FR-L26, #327) ---------------------------------------------------

    #[test]
    fn fr_l26_archive_and_delete_are_live_but_never_in_the_default_project() {
        assert!(danger_allowed("ember", SettingsPart::Archive) && danger_allowed("ember", SettingsPart::Delete));
        assert!(!danger_allowed(DEFAULT_PROJECT_ID, SettingsPart::Archive));
        assert!(!danger_allowed(DEFAULT_PROJECT_NAME, SettingsPart::Delete));
        assert!(part_supported(SettingsPart::Leader));
        assert!(danger_hint("ember").contains("한 번 더"), "the hint says delete asks twice");
        assert!(danger_hint(DEFAULT_PROJECT_ID).contains("기본 프로젝트"));
    }

    #[test]
    fn fr_l26_archive_patch_flips_the_flag_and_sends_nothing_else() {
        assert_eq!(archive_patch(false), ProjectPatch { archived: Some(true), ..Default::default() });
        assert_eq!(archive_patch(true), ProjectPatch { archived: Some(false), ..Default::default() });
        assert_eq!(serde_json::to_value(archive_patch(false)).unwrap(), serde_json::json!({ "archived": true }));
    }

    #[test]
    fn fr_l26_an_archived_project_offers_restore() {
        let [archive, _] = danger_rows_for("ember", 0, true, false);
        assert_eq!((archive.title, archive.button, archive.enabled), ("프로젝트 보관 해제", "보관 해제", true));
        assert_eq!(archive.note, "보관을 해제하면 목록과 검색에 다시 표시됩니다.");
        let [archive, _] = danger_rows_for("ember", 0, false, false);
        assert_eq!((archive.title, archive.button, archive.enabled), ("프로젝트 보관", "보관", true));
        assert_eq!(archive.note, "세션과 기록은 남고 목록과 검색에서 숨겨집니다.");
    }

    #[test]
    fn fr_l26_delete_row_follows_the_server_rule_and_the_armed_state() {
        // Unarchived: disabled, cannot be armed.
        let [_, d] = danger_rows_for("ember", 0, false, false);
        assert_eq!((d.button, d.enabled), ("삭제", false));
        assert_eq!(d.note, "먼저 보관한 뒤에 삭제할 수 있습니다.");
        let [_, d] = danger_rows_for("ember", 2, false, true);
        assert_eq!((d.button, d.enabled), ("삭제", false));
        assert_eq!(d.note, "먼저 보관한 뒤에 삭제할 수 있습니다.");
        // Archived: enabled, cascade warning, armed label on second tap.
        let [_, d] = danger_rows_for("ember", 2, true, false);
        assert_eq!((d.button, d.enabled), ("삭제", true));
        assert_eq!(d.note, "프로젝트와 소속 세션·대화 기록이 모두 함께 삭제됩니다. 되돌릴 수 없습니다.");
        let [_, armed] = danger_rows_for("ember", 0, true, true);
        assert_eq!((armed.button, armed.enabled), ("정말 삭제합니다", true));
        assert_eq!(armed.note, "프로젝트와 소속 세션·대화 기록이 모두 함께 삭제됩니다. 되돌릴 수 없습니다.");
        // 기본 프로젝트 never enables or shows the armed question.
        let [_, d] = danger_rows_for(DEFAULT_PROJECT_ID, 0, true, true);
        assert_eq!((d.button, d.enabled, d.note.as_str()), ("삭제", false, "기본 프로젝트는 지울 수 없습니다."));
    }

    #[test]
    fn fr_l26_a_single_tap_never_deletes() {
        const T0: i64 = 1_000_000;
        // The first tap only arms.
        assert_eq!(delete_step(None, T0), DeleteStep::Arm);
        assert!(!delete_armed(None, T0));
        // A double click: the second tap comes before the question could be read.
        assert_eq!(delete_step(Some(T0), T0), DeleteStep::Wait);
        assert_eq!(delete_step(Some(T0), T0 + 599), DeleteStep::Wait);
        // The second tap counts from 600 ms to 4 s after the first.
        assert_eq!(delete_step(Some(T0), T0 + 600), DeleteStep::Delete);
        assert_eq!(delete_step(Some(T0), T0 + 3999), DeleteStep::Delete);
        // Later the question is gone, so the tap arms again.
        assert_eq!(delete_step(Some(T0), T0 + 4000), DeleteStep::Arm);
        assert!(delete_armed(Some(T0), T0 + 100));
        assert!(!delete_armed(Some(T0), T0 + 4000));
        // Whatever the time of one tap from the unarmed state, nothing is deleted.
        for now in [0, 1, T0, i64::MAX / 2] {
            assert_ne!(delete_step(None, now), DeleteStep::Delete);
        }
    }

    #[test]
    fn fr_l26_delete_and_archive_failures_read_as_fixed_sentences() {
        assert_eq!(
            delete_error_message(Some(409), Some("project_not_archived")),
            "먼저 프로젝트를 보관한 뒤에 삭제할 수 있습니다."
        );
        assert_eq!(
            delete_error_message(Some(409), Some("sessions_running")),
            "실행 중인 세션이 있어 삭제할 수 없습니다. 세션이 끝난 뒤에 다시 시도해 주세요."
        );
        assert_eq!(delete_error_message(Some(403), Some("protected_project")), "기본 프로젝트는 지울 수 없습니다");
        assert_eq!(delete_error_message(Some(404), Some("not_found")), "프로젝트를 찾지 못했습니다");
        for (st, code) in [(Some(400), Some("not_yet")), (Some(405), None)] {
            assert!(delete_error_message(st, code).contains("아직 지원하지 않습니다"), "{st:?} {code:?}");
        }
        for (st, code) in [(None, None), (Some(500), Some("internal"))] {
            let m = delete_error_message(st, code);
            assert!(m.starts_with("프로젝트를 지우지 못했습니다") && !m.contains("500"), "{m}");
        }
        let archive = archive_patch(false);
        assert_eq!(
            patch_error_message(Some(409), Some("sessions_running"), &archive),
            "실행 중인 세션이 있어 보관할 수 없습니다. 세션이 끝난 뒤에 다시 시도해 주세요."
        );
        assert_eq!(patch_error_message(Some(403), Some("protected_project"), &archive), "기본 프로젝트는 보관할 수 없습니다");
        assert!(patch_error_message(Some(400), Some("not_yet"), &archive).contains("아직 지원하지 않습니다"));
        // A rename refusal keeps its own sentence.
        assert_eq!(
            patch_error_message(Some(403), Some("protected_project"), &ProjectPatch::default()),
            "기본 프로젝트의 이름은 바꿀 수 없습니다"
        );
        // A missing owner key goes to the notice, which opens the owner sheet (FR-S12).
        assert!(is_owner_refusal(Some(403), Some("owner_required")));
        assert!(!is_owner_refusal(Some(403), Some("protected_project")));
        assert!(!is_owner_refusal(Some(409), Some("owner_required")));
    }

    #[test]
    fn fr_l26_leader_session_draft_patch_and_badge() {
        use ember_client::wire::{Push, SessionRecord, SessionStatus, TeamMember, TeamRole};
        use ember_client::Input;

        let mut p = server_project();
        let mut d = SettingsDraft::from_project(&p);
        assert_eq!(d.leader_session, None);
        assert!(build_patch(&p, &d).leader_session.is_none());

        // Selecting a leader sends Some(Some(id)).
        d.leader_session = Some("s1".into());
        assert_eq!(build_patch(&p, &d).leader_session, Some(Some("s1".into())));

        // Clearing an existing leader sends Some(None).
        p.leader_session = Some("s1".into());
        let mut d = SettingsDraft::from_project(&p);
        assert_eq!(d.leader_session.as_deref(), Some("s1"));
        assert!(build_patch(&p, &d).leader_session.is_none());
        d.leader_session = None;
        assert_eq!(build_patch(&p, &d).leader_session, Some(None));

        // Default project ignores leader_session in build_patch.
        for def_name in [DEFAULT_PROJECT_ID, DEFAULT_PROJECT_NAME] {
            let dp = Project { name: def_name.into(), ..Default::default() };
            let mut dd = SettingsDraft::from_project(&dp);
            dd.leader_session = Some("s1".into());
            assert!(build_patch(&dp, &dd).leader_session.is_none(), "default project {def_name} must not send leader_session");
        }

        // member_of prefers project.leader_session over team leader, falls back when None,
        // and never badges a leader in the default project.
        let make_rec = |id: &str, project: &str| SessionRecord {
            id: id.into(),
            project: project.into(),
            title: format!("task {id}"),
            agent: "claude-code".into(),
            cwd: "/work".into(),
            model: None,
            native_id: None,
            status: SessionStatus::Idle,
            created_at: 10,
            updated_at: 20,
            last_seq: 1,
            account_id: None,
            account_reason: None,
            pinned: false,
            archived: false,
        };
        let mut state = State::default();
        state.apply(Input::SessionsLoaded(vec![
            make_rec("s1", "ember"),
            make_rec("s2", "ember"),
            make_rec("s3", DEFAULT_PROJECT_ID),
        ]));
        state.apply(Input::Push(Push::TeamUpdated {
            team: TeamView {
                id: "t1".into(),
                project: "ember".into(),
                leader: "s1".into(),
                created_at: 1,
                members: vec![
                    TeamMember {
                        session_id: "s1".into(),
                        name: "lead".into(),
                        role: TeamRole::Leader,
                        joined_at: 1,
                        ended_at: None,
                        title: "task s1".into(),
                        agent: "claude-code".into(),
                        status: Some(SessionStatus::Idle),
                    },
                    TeamMember {
                        session_id: "s2".into(),
                        name: "worker".into(),
                        role: TeamRole::Teammate,
                        joined_at: 1,
                        ended_at: None,
                        title: "task s2".into(),
                        agent: "claude-code".into(),
                        status: Some(SessionStatus::Idle),
                    },
                ],
                tasks: Vec::new(),
            },
        }));

        // When project.leader_session is None, falls back to team leader (s1).
        state.apply(Input::ProjectsLoaded(vec![
            Project { name: "ember".into(), leader_session: None, ..Default::default() },
            Project { name: DEFAULT_PROJECT_ID.into(), leader_session: Some("s3".into()), ..Default::default() },
        ]));
        let boxes = project_boxes(&state);
        let ember = boxes.iter().find(|b| b.id == "ember").expect("ember box");
        assert_eq!(ember.leader().map(|m| m.session_id.as_str()), Some("s1"));
        let def = boxes.iter().find(|b| b.id == DEFAULT_PROJECT_ID).expect("default box");
        assert!(def.leader().is_none(), "default project never has a leader badge");

        // When project.leader_session is Some("s2"), overrides team leader.
        state.apply(Input::ProjectsLoaded(vec![
            Project { name: "ember".into(), leader_session: Some("s2".into()), ..Default::default() },
        ]));
        let boxes = project_boxes(&state);
        let ember = boxes.iter().find(|b| b.id == "ember").expect("ember box");
        assert_eq!(ember.leader().map(|m| m.session_id.as_str()), Some("s2"));
    }

    #[test]
    fn fr_l26_delete_requires_archived_and_warns_cascade() {
        let [_, unarchived] = danger_rows_for("ember", 5, false, false);
        assert!(!unarchived.enabled);
        assert_eq!(unarchived.note, "먼저 보관한 뒤에 삭제할 수 있습니다.");
        assert!(!unarchived.note.contains("비어 있는 프로젝트만"));

        let [_, archived] = danger_rows_for("ember", 5, true, false);
        assert!(archived.enabled);
        assert_eq!(archived.note, "프로젝트와 소속 세션·대화 기록이 모두 함께 삭제됩니다. 되돌릴 수 없습니다.");
        assert!(!archived.note.contains("비어 있는 프로젝트만"));
    }

    #[test]
    fn fr_l26_archive_and_delete_error_messages_cover_running_and_not_archived() {
        let archive = archive_patch(false);
        assert_eq!(
            patch_error_message(Some(409), Some("sessions_running"), &archive),
            "실행 중인 세션이 있어 보관할 수 없습니다. 세션이 끝난 뒤에 다시 시도해 주세요."
        );
        // Mixing profile_overrides with leader_session is not account_only.
        let mixed = ProjectPatch { leader_session: Some(Some("s1".into())), ..only_overrides() };
        assert_eq!(
            patch_error_message(Some(400), Some("invalid_field"), &mixed),
            "입력한 값을 서버가 받아들이지 않았습니다. 내용을 확인하십시오."
        );
        assert_eq!(
            delete_error_message(Some(409), Some("project_not_archived")),
            "먼저 프로젝트를 보관한 뒤에 삭제할 수 있습니다."
        );
        assert_eq!(
            delete_error_message(Some(409), Some("sessions_running")),
            "실행 중인 세션이 있어 삭제할 수 없습니다. 세션이 끝난 뒤에 다시 시도해 주세요."
        );
    }

    #[test]
    fn fr_l26_an_archived_project_is_hidden_until_archived_items_are_shown() {
        use ember_client::Input;
        let mut state = State::default();
        state.apply(Input::ProjectsLoaded(vec![
            Project { name: "live".into(), ..Default::default() },
            Project { name: "old".into(), archived: true, ..Default::default() },
        ]));
        let names = |show: bool| -> Vec<(String, String)> {
            project_boxes_with_archived(&state, show).into_iter().map(|b| (b.id, b.name)).collect()
        };
        assert_eq!(names(false), [("live".to_string(), "live".to_string())]);
        let shown = names(true);
        assert_eq!(shown.len(), 2);
        assert!(shown.contains(&("old".to_string(), "old (보관됨)".to_string())), "{shown:?}");
        assert!(project_archived(&state, "old"));
        assert!(!project_archived(&state, "live"));
        assert!(!project_archived(&state, "missing"));
    }

    #[test]
    fn fr_l26_the_open_view_closes_when_its_project_goes_away() {
        assert_eq!(full_after_removal(Some("old"), "old"), None);
        assert_eq!(full_after_removal(Some("live"), "old"), Some("live".to_string()));
        assert_eq!(full_after_removal(None, "old"), None);
    }

    // ---- the project page's 루틴 tile (FR-L26, #327) ------------------------------------------

    use crate::ui::routines::{Routine, Run};

    fn at(y: i64, mo: u32, d: u32, h: u32, mi: u32) -> Minutes {
        crate::ui::routines::minutes_at(y, mo, d, h, mi)
    }

    fn routine(id: &str, name: &str, project: &str, cron: &str) -> Routine {
        Routine {
            id: id.into(),
            name: name.into(),
            trigger: Trigger::Time,
            cron: cron.into(),
            prompt: "CI 실패를 요약합니다".into(),
            enabled: true,
            sel: crate::ui::home::Selection { project: project.into(), ..crate::ui::home::Selection::initial() },
            continue_session: None,
        }
    }

    fn run(at: Minutes, status: RunStatus) -> Run {
        Run { at, status, session: None }
    }

    fn entry(r: Routine, runs: Vec<Run>) -> Entry {
        Entry { routine: r, runs, next_at: None }
    }

    #[test]
    fn fr_l26_routine_tile_says_loading_failed_and_empty() {
        let now = at(2026, 10, 25, 8, 0);
        assert_eq!(project_routine_tile(&TileLoad::Loading, "ember", now).lines(), [TILE_LOADING]);
        assert_eq!(project_routine_tile(&TileLoad::Failed, "ember", now).lines(), [TILE_FAILED]);
        assert_eq!(project_routine_tile(&TileLoad::Loaded(Vec::new()), "ember", now).lines(), [TILE_EMPTY]);
        // Another project's routine is not this project's.
        let other = entry(routine("1", "요약", "other", "0 9 * * *"), Vec::new());
        let tile = project_routine_tile(&TileLoad::Loaded(vec![other]), "ember", now);
        assert!(tile.rows.is_empty());
        assert_eq!(tile.lines(), [TILE_EMPTY]);
    }

    #[test]
    fn fr_l26_routine_tile_counts_and_lists_next_run_and_last_result() {
        let now = at(2026, 10, 25, 8, 0);
        let next = at(2026, 10, 25, 9, 0);
        let done = vec![run(now - 180, RunStatus::Done)];
        let mut daily = entry(routine("1", "아침 요약", "ember", "0 9 * * *"), done.clone());
        daily.next_at = Some(next);
        let mut off = entry(routine("2", "주간 정리", "ember", "0 9 * * 1"), Vec::new());
        off.routine.enabled = false;
        let mut waiting = routine("3", "CI 고치기", "ember", "o/r@main");
        waiting.trigger = Trigger::Event;
        let waiting = entry(waiting, vec![run(now - 5, RunStatus::Failed)]);

        let tile = project_routine_tile(&TileLoad::Loaded(vec![daily, off, waiting]), "ember", now);
        assert_eq!(tile.summary, "루틴 3개 · 켜짐 2개 · 최근 실패 1개");
        assert_eq!(tile.rows.len(), 3);
        assert_eq!(tile.rows[0].name, "아침 요약");
        assert_eq!(tile.rows[0].when, format!("다음 실행 {}", format_next(next, now)));
        assert_eq!(tile.rows[0].last, last_run_text(&done, now));
        assert_eq!(tile.rows[1].when, "꺼져 있습니다");
        assert_eq!(tile.rows[1].last, "실행 기록 없음");
        assert_eq!(tile.rows[2].when, TILE_EVENT_WAITING, "an event routine has no next run, it waits");
        assert!(tile.rows[2].last.starts_with("실패"), "{}", tile.rows[2].last);
        assert_eq!(tile.more, None);
        let lines = tile.lines();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], tile.summary);
        assert_eq!(lines[1], format!("아침 요약 · {} · {}", tile.rows[0].when, tile.rows[0].last));
    }

    #[test]
    fn fr_l26_routine_tile_without_a_failure_does_not_mention_one() {
        let now = at(2026, 10, 25, 8, 0);
        let one = entry(routine("1", "요약", "ember", "0 9 * * *"), vec![run(now - 60, RunStatus::Done)]);
        let tile = project_routine_tile(&TileLoad::Loaded(vec![one]), "ember", now);
        assert_eq!(tile.summary, "루틴 1개 · 켜짐 1개");
    }

    #[test]
    fn fr_l26_routine_tile_shows_three_rows_and_counts_the_rest() {
        let now = at(2026, 10, 25, 8, 0);
        let all: Vec<Entry> = (1..=5)
            .map(|n| entry(routine(&n.to_string(), &format!("루틴 {n}"), "ember", "0 9 * * *"), Vec::new()))
            .collect();
        let tile = project_routine_tile(&TileLoad::Loaded(all), "ember", now);
        assert_eq!(tile.summary, "루틴 5개 · 켜짐 5개");
        assert_eq!(tile.rows.len(), TILE_MAX_ROWS);
        assert_eq!(tile.more.as_deref(), Some("외 2개"));
        assert_eq!(tile.lines().len(), 1 + TILE_MAX_ROWS + 1);
    }

    #[test]
    fn fr_l26_routine_tile_matches_the_default_project_by_id_or_name() {
        let now = at(2026, 10, 25, 8, 0);
        let one = entry(routine("1", "요약", crate::ui::home::DEFAULT_PROJECT, "0 9 * * *"), Vec::new());
        let tile = project_routine_tile(&TileLoad::Loaded(vec![one]), DEFAULT_PROJECT_ID, now);
        assert_eq!(tile.rows.len(), 1);
    }

    // ---- the project page's 채널 tile and settings 연결 section (FR-L26, #327) ----------------

    fn room(id: &str, name: &str, project: &str, members: usize, unread: u32) -> crate::ui::channels::Room {
        crate::ui::channels::Room {
            id: id.into(),
            name: name.into(),
            project: project.into(),
            members: (0..members)
                .map(|i| crate::ui::channels::Member {
                    id: format!("m{i}"),
                    name: format!("멤버{i}"),
                    is_agent: false,
                    harness: String::new(),
                })
                .collect(),
            messages: Vec::new(),
            unread,
        }
    }

    #[test]
    fn fr_l26_channel_tile_says_loading_failed_and_empty() {
        assert_eq!(project_channel_tile(&ChannelTileLoad::Loading, "ember").lines(), [CHANNEL_TILE_LOADING]);
        assert_eq!(project_channel_tile(&ChannelTileLoad::Failed, "ember").lines(), [CHANNEL_TILE_FAILED]);
        assert_eq!(project_channel_tile(&ChannelTileLoad::Loaded(Vec::new()), "ember").lines(), [CHANNEL_TILE_EMPTY]);
        let other = room("r1", "잡담", "other", 2, 0);
        let tile = project_channel_tile(&ChannelTileLoad::Loaded(vec![other]), "ember");
        assert!(tile.rows.is_empty());
        assert_eq!(tile.lines(), [CHANNEL_TILE_EMPTY]);
    }

    #[test]
    fn fr_l26_channel_tile_lists_rooms_members_unread_and_rest() {
        let rooms = vec![
            room("r1", "배포 논의", "ember", 3, 2),
            room("r2", "디자인 리뷰", "ember", 2, 0),
            room("r3", "질문", "ember", 1, 0),
            room("r4", "릴리즈", "ember", 4, 1),
            room("r5", "다른 프로젝트", "other", 2, 5),
        ];
        let tile = project_channel_tile(&ChannelTileLoad::Loaded(rooms), "ember");
        assert_eq!(tile.summary, "채널 4개");
        assert_eq!(tile.rows.len(), TILE_MAX_ROWS);
        assert_eq!(tile.more.as_deref(), Some("외 1개"));
        let lines = tile.lines();
        assert_eq!(lines[0], "채널 4개");
        assert_eq!(lines[1], "#배포 논의 \u{00b7} 멤버 3명 \u{00b7} 읽지 않음 2");
        assert_eq!(lines[2], "#디자인 리뷰 \u{00b7} 멤버 2명");
        assert_eq!(lines[3], "#질문 \u{00b7} 멤버 1명");
        assert_eq!(lines[4], "외 1개");
        // Matches default project by id or display name.
        let default_room = room("r0", "기본", crate::ui::home::DEFAULT_PROJECT, 1, 0);
        let tile = project_channel_tile(&ChannelTileLoad::Loaded(vec![default_room]), DEFAULT_PROJECT_ID);
        assert_eq!(tile.rows.len(), 1);
    }

    #[test]
    fn fr_l26_project_links_labels_and_summary_cover_loading_failed_empty_and_live() {
        let loading = ProjectLinks::loading();
        assert_eq!(loading.routine_button_label(), "루틴");
        assert_eq!(loading.channel_button_label(), "채널");
        assert_eq!(loading.summary_line(), "연결된 루틴과 채널을 불러오는 중입니다");

        let failed = ProjectLinks { routines: LinkCountLoad::Failed, channels: LinkCountLoad::Failed };
        assert_eq!(failed.routine_button_label(), "루틴");
        assert_eq!(failed.channel_button_label(), "채널");
        assert_eq!(failed.summary_line(), "연결된 루틴과 채널을 불러오지 못했습니다");

        let empty = ProjectLinks { routines: LinkCountLoad::Loaded(0), channels: LinkCountLoad::Loaded(0) };
        assert_eq!(empty.routine_button_label(), "루틴 (0)");
        assert_eq!(empty.channel_button_label(), "채널 (0)");
        assert_eq!(empty.summary_line(), "이 프로젝트에 연결된 루틴과 채널이 없습니다");

        let live = ProjectLinks { routines: LinkCountLoad::Loaded(2), channels: LinkCountLoad::Loaded(3) };
        assert_eq!(live.routine_button_label(), "루틴 (2)");
        assert_eq!(live.channel_button_label(), "채널 (3)");
        assert_eq!(live.summary_line(), "루틴 2개 \u{00b7} 채널 3개");

        let partial = ProjectLinks { routines: LinkCountLoad::Loaded(1), channels: LinkCountLoad::Failed };
        assert_eq!(partial.routine_button_label(), "루틴 (1)");
        assert_eq!(partial.channel_button_label(), "채널");
        assert_eq!(partial.summary_line(), "루틴 1개 \u{00b7} 채널 불러오지 못함");

        let routines = vec![
            entry(routine("1", "아침 요약", "ember", "0 9 * * *"), Vec::new()),
            entry(routine("2", "다른 프로젝트", "other", "0 9 * * *"), Vec::new()),
        ];
        let rooms = vec![
            room("r1", "배포 논의", "ember", 3, 0),
            room("r2", "디자인 리뷰", "ember", 2, 0),
            room("r3", "자유", "other", 1, 0),
        ];
        let counted = project_links_from("ember", &routines, &rooms);
        assert_eq!(counted, ProjectLinks { routines: LinkCountLoad::Loaded(1), channels: LinkCountLoad::Loaded(2) });
        assert_eq!(counted.summary_line(), "루틴 1개 \u{00b7} 채널 2개");
    }

    #[test]
    fn fr_l26_member_default_chip_choices_and_labels_match_catalog() {
        let models = super::member_default_model_choices();
        assert_eq!(models[0].agent, "");
        assert_eq!(models[0].model, "");
        assert_eq!(models[0].label, super::DEFAULT_UNSET_LABEL);
        assert!(models.iter().any(|c| c.agent == "claude-code" && c.model == "opus" && c.label.contains("Opus 5.5")));
        assert!(models.iter().any(|c| c.agent == "codex" && c.model == "gpt-5"));
        assert!(!models.iter().any(|c| c.agent == "gemini"), "unsupported harness is omitted");

        assert_eq!(super::member_default_model_label("", ""), super::DEFAULT_UNSET_LABEL);
        assert!(super::member_default_model_label("claude-code", "opus").contains("Claude Code"));
        assert_eq!(super::member_default_model_label("claude-code", ""), "Claude Code");
        assert_eq!(super::member_default_model_label("claude-code", "custom"), "Claude Code \u{00b7} custom");

        let modes = super::member_default_mode_choices();
        assert_eq!(modes[0], ("", super::DEFAULT_UNSET_LABEL));
        assert_eq!(super::member_default_mode_label(""), super::DEFAULT_UNSET_LABEL);
        assert_eq!(super::member_default_mode_label("ask"), crate::ui::home::PermMode::Ask.label());
        assert_eq!(super::member_default_mode_label("edits"), crate::ui::home::PermMode::Edits.label());
        assert_eq!(super::member_default_mode_label("plan"), crate::ui::home::PermMode::Plan.label());
        assert_eq!(super::member_default_mode_label("bypass"), crate::ui::home::PermMode::Bypass.label());
    }
}
