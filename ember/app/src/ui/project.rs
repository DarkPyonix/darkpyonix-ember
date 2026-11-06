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
/// `PATCH /api/projects/{name}`, name, repository and branch, new member defaults and character
/// style (FR-L26). The rest is shown disabled with an explanation.
/// TODO(#384): leader, archive and delete (server answers 400 `not_yet`).
pub fn part_supported(part: SettingsPart) -> bool {
    matches!(
        part,
        SettingsPart::Computers
            | SettingsPart::Name
            | SettingsPart::Repository
            | SettingsPart::CharacterStyle
            | SettingsPart::MemberDefaults
            | SettingsPart::ProfileOverride
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
/// user emptied is sent empty (or `null` for the defaults), which clears it. Leader, profile
/// override and archive are never sent. The name of 기본 프로젝트 is never sent.
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

/// The sentence a failed save shows. Never the server's raw text.
/// The server maps every bad value to `invalid_field`, so the account sentence is used only when
/// the patch carried nothing but `profile_overrides`.
pub fn patch_error_message(status: Option<u16>, code: Option<&str>, patch: &ProjectPatch) -> &'static str {
    let account_only = patch.profile_overrides.is_some()
        && patch.name.is_none()
        && patch.repository.is_none()
        && patch.default_branch.is_none()
        && patch.member_defaults.is_none()
        && patch.character_style.is_none();
    match (status, code) {
        (Some(409), Some("name_taken")) => "이미 있는 이름입니다",
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
    fn fr_l26_the_server_saves_metadata_and_profile_override_but_not_leader_archive_or_delete() {
        for part in [
            SettingsPart::Computers,
            SettingsPart::Name,
            SettingsPart::Repository,
            SettingsPart::CharacterStyle,
            SettingsPart::MemberDefaults,
            SettingsPart::ProfileOverride,
        ] {
            assert!(part_supported(part), "{part:?}");
        }
        for part in [
            SettingsPart::Leader,
            SettingsPart::Links,
            SettingsPart::Archive,
            SettingsPart::Delete,
        ] {
            assert!(!part_supported(part), "{part:?}");
        }
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
}
