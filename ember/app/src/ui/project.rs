//! The Project screen (FR-L1 to FR-L4, FR-L24, FR-L26; `docs/DESIGN.md` section 6.2).
//!
//! Top to bottom: the computer strip, then the project boxes in 3, 2 or 1 columns. A box is two
//! levels: a header (status disc, name, repository, counts, buttons) over an inset of up to five
//! member tiles and a "+N" deck. Tapping the header opens the full project view; the gear opens
//! the settings sheet. A computer chip opens a detail sheet.
//!
//! The decisions (columns per size class, member order, the deck count, who wears the leader
//! badge, settings validation, which settings the server can save) are plain functions at the top
//! of this file, tested without a renderer. The components below draw them.
//!
//! What the pinned toolkit cannot do yet, with the workaround used (`GAPS-WORKLIST.md`):
//! - no clickable Row/Column/Card: a text Button is the hit target of a header or a tile;
//! - no Heroicons set: the nearest `IconRole` glyph;
//! - no continuous-curve corners, one shadow: from `components.rs`;
//! - no animated characters: a harness-coloured circle (TODO FR-L22, `docs/DESIGN.md` section 7);
//! - the status donut would need `Canvas` arcs, which are unverified here: a disc with the member
//!   count, tinted by the most urgent status.

use dioxus_compose::prelude::*;

use ember_client::state::Reachability;
use ember_client::wire::TeamView;
use ember_client::{LauncherStatus, State};

use crate::model::status_rank;
use crate::services::{run, services};
use crate::ui::components::{
    AppButton, ButtonKind, EmberCard, EmberSheet, InsetPanel, Label, PickChip, Pill, PillTone,
};
use crate::ui::launcher::ConnectionBanner;
use crate::ui::theme::*;
use crate::ui::use_ui;

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

/// The placeholder character's colour family by harness, until FR-L22 draws real characters.
/// TODO(FR-L22): replace with the team character renderer (shape and shade seeded from the name).
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

fn member_of(state: &State, project: &str, id: &str, leader_seen: &mut bool) -> Option<Member> {
    let v = state.session(id).filter(|v| !v.record.archived)?;
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

/// All projects as boxes, most recently active first (like `model::project_cards`).
pub fn project_boxes(state: &State) -> Vec<ProjectBox> {
    let mut out: Vec<(i64, ProjectBox)> = state
        .projects()
        .into_iter()
        .map(|p| {
            let mut seen = false;
            let mut members: Vec<Member> = p
                .sessions
                .iter()
                .filter_map(|id| member_of(state, &p.name, id, &mut seen))
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

// ---------------------------------------------------------------------------------------------
// Server calls
// ---------------------------------------------------------------------------------------------

/// Assign or unassign a computer on the main server (FR-L4); say so when it fails.
fn set_assignment(project: String, computer_id: String, assign: bool) {
    run(
        async move {
            let c = &services().client;
            if assign {
                c.assign_computer(&project, &computer_id).await
            } else {
                c.unassign_computer(&project, &computer_id).await
            }
        },
        |r| {
            if let Err(e) = r {
                Message::new(format!("컴퓨터 할당을 바꾸지 못했습니다: {e}"))
                    .with_duration(MessageDuration::Long)
                    .show();
            }
        },
    );
}

fn tone_fill(tone: PillTone, p: &Palette) -> (Color, Color) {
    crate::ui::components::pill_colors(tone, p)
}

// ---------------------------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------------------------

/// What the screen has open on top of the grid.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Overlay {
    /// The full project view of this project id.
    full: Option<String>,
    /// The settings sheet of this project id.
    settings: Option<String>,
    /// The detail sheet of this computer id.
    computer: Option<String>,
}

#[component]
pub fn ProjectScreen() -> Element {
    let ui = use_ui();
    let window = use_window_size();
    let mut overlay = use_signal(Overlay::default);

    let (boxes, chips) = {
        let _ = *ui.live.launcher.read();
        services().client.read(|s| {
            let boxes = project_boxes(s);
            let chips = computer_chips(s, &boxes);
            (boxes, chips)
        })
    };
    let columns = grid_columns(window.class);
    let cur = overlay.read().clone();

    rsx! {
        ScrollColumn {
            fill_max_width: true,
            weight: 1.0,
            Column {
                fill_max_width: true,
                padding: SP[3],
                space_role: SpaceRole::Md,
                ConnectionBanner {}
                if let Some(id) = cur.full.clone() {
                    if let Some(b) = boxes.iter().find(|b| b.id == id).cloned() {
                        FullProject {
                            key: "{id}",
                            project: b,
                            all_computers: chips.clone(),
                            on_back: move |()| overlay.write().full = None,
                            on_settings: move |p: String| overlay.write().settings = Some(p),
                            on_computer: move |c: String| overlay.write().computer = Some(c),
                        }
                    } else {
                        Label { text: "프로젝트를 찾을 수 없습니다" }
                    }
                } else {
                    Row {
                        fill_max_width: true,
                        alignment: Alignment::CenterStart,
                        Label { text: "프로젝트", fs: Fs::Xl2, weight: Weight::Semibold }
                        Spacer { weight: 1.0 }
                        AppButton {
                            text: "새 프로젝트",
                            icon: IconRole::Add,
                            enabled: false,
                        }
                    }
                    ComputerStrip {
                        chips: chips.clone(),
                        on_open: move |c: String| overlay.write().computer = Some(c),
                    }
                    if boxes.is_empty() {
                        Label { text: "아직 프로젝트가 없습니다" }
                    }
                    ProjectGrid {
                        boxes: boxes.clone(),
                        columns,
                        on_open: move |p: String| overlay.write().full = Some(p),
                        on_settings: move |p: String| overlay.write().settings = Some(p),
                    }
                }
            }
        }
        if let Some(c) = cur.computer.clone() {
            ComputerSheet {
                chip: chips.iter().find(|x| x.id == c).cloned(),
                on_close: move |()| overlay.write().computer = None,
            }
        }
        if let Some(p) = cur.settings.clone() {
            if let Some(b) = boxes.iter().find(|b| b.id == p).cloned() {
                SettingsSheet {
                    project: b,
                    computers: chips.clone(),
                    on_close: move |()| overlay.write().settings = None,
                }
            }
        }
    }
}

// --- computers -------------------------------------------------------------------------------

#[component]
fn ComputerStrip(chips: Vec<ComputerChip>, on_open: EventHandler<String>) -> Element {
    let theme = use_ember_theme();
    let p = theme.palette;
    rsx! {
        if chips.is_empty() {
            Label { text: "등록된 컴퓨터가 없습니다", color: paint(p.text_2) }
        } else {
            ScrollRow {
                fill_max_width: true,
                height: 64.0,
                for c in chips {
                    ComputerChipView { key: "{c.id}", chip: c.clone(), on_open: move |id| on_open.call(id) }
                }
            }
        }
    }
}

/// A left-aligned label that is also a hit target. The toolkit's `Button` centres its label and has
/// no content-alignment prop, so a `Label` sits at the start of a `Box` and a transparent,
/// text-less `Button` lies over it.
/// TODO(GAPS: 클릭 가능한 컨테이너): replace with a clickable Row once the toolkit has one.
#[component]
fn HitLabel(#[props(into)] text: String, on_click: EventHandler<()>) -> Element {
    rsx! {
        dioxus_compose::Box {
            fill_max_width: true,
            height: 40.0,
            alignment: Alignment::CenterStart,
            Label { text: text.clone(), weight: Weight::Medium, max_lines: 1 }
            Button {
                text: "",
                fill_max_width: true,
                fill_max_height: true,
                variant: ButtonVariant::Text,
                on_click: move |()| on_click.call(()),
            }
        }
    }
}

#[component]
fn ComputerChipView(chip: ComputerChip, on_open: EventHandler<String>) -> Element {
    let theme = use_ember_theme();
    let p = theme.palette;
    let (_, tone) = reach_pill(chip.reachability);
    let (dot, _) = tone_fill(tone, p);
    let id = chip.id.clone();
    rsx! {
        Row {
            width: 200.0,
            height: 56.0,
            background: paint(p.surface_2),
            corner_radius: CORNER_MD,
            alignment: Alignment::CenterStart,
            space_role: SpaceRole::Sm,
            Spacer { width: SP[2] }
            dioxus_compose::Box { width: 10.0, height: 10.0, shape_role: ShapeRole::Full, background: paint(dot) }
            Column {
                weight: 1.0,
                // TODO(GAPS: 클릭 가능한 컨테이너): the name button is the hit target of the chip.
                HitLabel { text: chip.name.clone(), on_click: move |()| on_open.call(id.clone()) }
                Label { text: chip_line(&chip), fs: Fs::Xs, color: paint(p.text_2), max_lines: 1 }
            }
        }
        Spacer { width: SP[1] }
    }
}

/// The computer detail sheet. TODO(FR-L21, FR-X7): the full monitor (CPU per core, GPU, memory,
/// disk, network, process table) replaces this once ember node reports it. Until then the sheet
/// shows what the client has: reachability, assigned projects and running count.
#[component]
fn ComputerSheet(chip: Option<ComputerChip>, on_close: EventHandler<()>) -> Element {
    let theme = use_ember_theme();
    let p = theme.palette;
    let title = chip
        .as_ref()
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "컴퓨터".to_string());
    rsx! {
        EmberSheet {
            title,
            open: true,
            on_dismiss: move |()| on_close.call(()),
            Column {
                fill_max_width: true,
                padding: SP[4],
                space_role: SpaceRole::Md,
                if let Some(c) = chip {
                    Row {
                        fill_max_width: true,
                        alignment: Alignment::CenterStart,
                        space_role: SpaceRole::Sm,
                        Pill { text: reach_pill(c.reachability).0, tone: reach_pill(c.reachability).1 }
                        Label { text: format!("실행 중 {}", c.running), fs: Fs::Sm, color: paint(p.text_2) }
                    }
                    Label { text: "할당된 프로젝트", fs: Fs::Sm, weight: Weight::Medium, color: paint(p.text_2) }
                    if c.projects.is_empty() {
                        Label { text: "할당된 프로젝트가 없습니다", color: paint(p.text_2) }
                    }
                    for pr in c.projects.iter() {
                        Label { key: "{pr}", text: display_name(pr) }
                    }
                    InsetPanel {
                        fill_max_width: true,
                        Label { text: format!("CPU, GPU, 메모리, 디스크 사용량 모니터는 {SOON}"), fs: Fs::Sm, color: paint(p.text_2) }
                    }
                    // TODO(FR-L21): monitor sheet fed by FR-X7 (about 1 s while visible).
                } else {
                    Label { text: "컴퓨터를 찾을 수 없습니다" }
                }
                Row {
                    fill_max_width: true,
                    arrangement: Arrangement::End,
                    AppButton { text: "닫기", on_click: move |()| on_close.call(()) }
                }
            }
        }
    }
}

// --- project grid ----------------------------------------------------------------------------

#[component]
fn ProjectGrid(
    boxes: Vec<ProjectBox>,
    columns: usize,
    on_open: EventHandler<String>,
    on_settings: EventHandler<String>,
) -> Element {
    let columns = columns.max(1);
    let pad = row_padding(boxes.len(), columns);
    let rows: Vec<Vec<ProjectBox>> = boxes.chunks(columns).map(|c| c.to_vec()).collect();
    let last = rows.len().saturating_sub(1);
    rsx! {
        Column {
            fill_max_width: true,
            space_role: SpaceRole::Md,
            for (i, row) in rows.into_iter().enumerate() {
                Row {
                    key: "{i}",
                    fill_max_width: true,
                    space_role: SpaceRole::Md,
                    alignment: Alignment::TopCenter,
                    for b in row {
                        Column {
                            key: "{b.id}",
                            weight: 1.0,
                            ProjectBoxView {
                                project: b.clone(),
                                on_open: move |id| on_open.call(id),
                                on_settings: move |id| on_settings.call(id),
                            }
                        }
                    }
                    if i == last {
                        for k in 0..pad {
                            Spacer { key: "pad{k}", weight: 1.0 }
                        }
                    }
                }
            }
        }
    }
}

/// The status disc: the member count on the colour of the most urgent status.
/// TODO(GAPS: 차트): a segmented donut with `Canvas` arcs once verified on the pinned toolkit.
#[component]
fn StatusDisc(total: usize, top: Option<LauncherStatus>, size: f32) -> Element {
    let theme = use_ember_theme();
    let p = theme.palette;
    let tone = top.map(|s| status_pill(s).1).unwrap_or(PillTone::Grey);
    let (fg, bg) = tone_fill(tone, p);
    rsx! {
        dioxus_compose::Box {
            width: size,
            height: size,
            shape_role: ShapeRole::Full,
            background: paint(bg),
            alignment: Alignment::Center,
            Label { text: format!("{total}"), fs: Fs::Md, weight: Weight::Semibold, color: paint(fg) }
        }
    }
}

/// The placeholder character: a circle in the harness colour, with the crown for a leader.
/// TODO(FR-L22): the six animated team character styles replace this.
#[component]
pub fn Character(agent: String, leader: bool, size: f32, name: String) -> Element {
    let theme = use_ember_theme();
    let p = theme.palette;
    let (fg, bg) = tone_fill(harness_tone(&agent), p);
    let initial: String = agent
        .chars()
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    rsx! {
        Row {
            alignment: Alignment::TopCenter,
            if leader {
                // The leader badge (FR-L24): a small ink circle with a gold crown. The crown
                // is drawn as a glyph until the shared character renderer owns it.
                dioxus_compose::Box {
                    width: 18.0,
                    height: 18.0,
                    shape_role: ShapeRole::Full,
                    background: paint(p.ink),
                    alignment: Alignment::Center,
                    Label { text: "\u{265b}", fs: Fs::Xs2, color: Paint::Literal(Color::rgb(0xE8B931)) }
                }
            }
            dioxus_compose::Box {
                width: size,
                height: size,
                shape_role: ShapeRole::Full,
                background: paint(bg),
                alignment: Alignment::Center,
                Label { text: initial, fs: Fs::Sm, weight: Weight::Semibold, color: paint(fg) }
            }
        }
    }
}

#[component]
fn MemberTile(member: Member, on_open: EventHandler<String>) -> Element {
    let theme = use_ember_theme();
    let p = theme.palette;
    let (label, tone) = status_pill(member.status);
    let id = member.session_id.clone();
    rsx! {
        Row {
            fill_max_width: true,
            alignment: Alignment::CenterStart,
            space_role: SpaceRole::Sm,
            Character { agent: member.agent.clone(), leader: member.leader, size: 32.0, name: member.name.clone() }
            Column {
                weight: 1.0,
                // TODO(GAPS: 클릭 가능한 컨테이너): the name button opens the member's session.
                HitLabel { text: member.accessible_name(), on_click: move |()| on_open.call(id.clone()) }
                Label { text: member.task.clone(), fs: Fs::Xs, color: paint(p.text_2), max_lines: 1 }
            }
            Pill { text: label, tone, small: true }
        }
    }
}

#[component]
fn ProjectBoxView(
    project: ProjectBox,
    on_open: EventHandler<String>,
    on_settings: EventHandler<String>,
) -> Element {
    let ui = use_ui();
    let theme = use_ember_theme();
    let p = theme.palette;
    let mut expanded = use_signal(|| false);
    let total = project.members.len();
    let shown = visible_tiles(total, expanded());
    let deck = deck_count(total, expanded());
    let (id_open, id_set, id_sessions, id_vcs) = (
        project.id.clone(),
        project.id.clone(),
        project.id.clone(),
        project.id.clone(),
    );
    let first = project.members.first().map(|m| m.session_id.clone());
    rsx! {
        EmberCard {
            fill_max_width: true,
            Column {
                fill_max_width: true,
                padding: SP[3],
                space_role: SpaceRole::Sm,
                // Header (level 1).
                Row {
                    fill_max_width: true,
                    alignment: Alignment::CenterStart,
                    space_role: SpaceRole::Sm,
                    StatusDisc { total, top: project.top, size: 52.0 }
                    Column {
                        weight: 1.0,
                        // TODO(GAPS: 클릭 가능한 컨테이너): the name button opens the full view.
                        HitLabel { text: project.name.clone(), on_click: move |()| on_open.call(id_open.clone()) }
                        Label { text: project.repo_line(), fs: Fs::Xs, color: paint(p.text_2), max_lines: 1 }
                    }
                }
                Row {
                    fill_max_width: true,
                    space_role: SpaceRole::Sm,
                    alignment: Alignment::CenterStart,
                    if project.waiting > 0 {
                        Pill { text: format!("승인 대기 {}", project.waiting), tone: PillTone::Orange }
                    }
                    Pill { text: format!("실행 중 {}", project.running), tone: PillTone::Blue }
                    Pill { text: format!("컴퓨터 {}", project.computers.len()), tone: PillTone::Grey }
                }
                Row {
                    fill_max_width: true,
                    space_role: SpaceRole::Sm,
                    alignment: Alignment::CenterStart,
                    AppButton {
                        text: "세션 열기",
                        icon: IconRole::Forward,
                        kind: ButtonKind::Primary,
                        on_click: move |()| match &first {
                            Some(s) => ui.open_conversation(s),
                            None => ui.open_new_session(Some(id_sessions.clone())),
                        },
                    }
                    AppButton {
                        text: "버전 관리",
                        icon: IconRole::List,
                        on_click: move |()| crate::ui::vcs::open_for_project(ui, &id_vcs),
                    }
                    AppButton {
                        text: "설정",
                        icon: IconRole::Settings,
                        on_click: move |()| on_settings.call(id_set.clone()),
                    }
                }
                // Members (level 2).
                InsetPanel {
                    fill_max_width: true,
                    Column {
                        fill_max_width: true,
                        space_role: SpaceRole::Xs,
                        if total == 0 {
                            Label { text: "아직 팀원이 없습니다", fs: Fs::Sm, color: paint(p.text_2) }
                        }
                        for m in project.members.iter().take(shown).cloned() {
                            MemberTile { key: "{m.session_id}", member: m, on_open: move |s: String| ui.open_conversation(&s) }
                        }
                        if deck > 0 {
                            AppButton {
                                text: deck_label(deck),
                                small: true,
                                fill_max_width: true,
                                on_click: move |()| expanded.set(true),
                            }
                        } else if total > MAX_TILES {
                            AppButton {
                                text: "접기",
                                small: true,
                                fill_max_width: true,
                                on_click: move |()| expanded.set(false),
                            }
                        }
                    }
                }
            }
        }
    }
}

// --- full project view -----------------------------------------------------------------------

#[component]
fn FullProject(
    project: ProjectBox,
    all_computers: Vec<ComputerChip>,
    on_back: EventHandler<()>,
    on_settings: EventHandler<String>,
    on_computer: EventHandler<String>,
) -> Element {
    let ui = use_ui();
    let theme = use_ember_theme();
    let p = theme.palette;
    let mut filter = use_signal(|| MemberFilter::All);
    let mut query = use_signal(String::new);
    let list = filter_members(&project.members, filter(), &query());
    let assigned: Vec<ComputerChip> = all_computers
        .iter()
        .filter(|c| project.computers.contains(&c.id))
        .cloned()
        .collect();
    let id_set = project.id.clone();
    let id_new = project.id.clone();
    rsx! {
        Row {
            fill_max_width: true,
            alignment: Alignment::CenterStart,
            space_role: SpaceRole::Sm,
            AppButton { text: "프로젝트", icon: IconRole::Back, small: true, on_click: move |()| on_back.call(()) }
            Spacer { weight: 1.0 }
            AppButton {
                text: "새 세션",
                icon: IconRole::Compose,
                small: true,
                on_click: move |()| ui.open_new_session(Some(id_new.clone())),
            }
            AppButton {
                text: "설정",
                icon: IconRole::Settings,
                small: true,
                on_click: move |()| on_settings.call(id_set.clone()),
            }
        }
        Label { text: project.name.clone(), fs: Fs::Xl2, weight: Weight::Semibold }
        Label { text: project.repo_line(), fs: Fs::Sm, color: paint(p.text_2) }
        Row {
            fill_max_width: true,
            space_role: SpaceRole::Sm,
            Pill { text: format!("멤버 {}", project.members.len()), tone: PillTone::Grey }
            Pill { text: format!("승인 대기 {}", project.waiting), tone: PillTone::Orange }
            Pill { text: format!("실행 중 {}", project.running), tone: PillTone::Blue }
        }
        Label { text: "이 프로젝트의 컴퓨터", fs: Fs::Sm, weight: Weight::Medium, color: paint(p.text_2) }
        if assigned.is_empty() {
            Label { text: "할당된 컴퓨터가 없습니다", color: paint(p.text_2) }
        } else {
            ComputerStrip { chips: assigned, on_open: move |c| on_computer.call(c) }
        }
        EmberCard {
            fill_max_width: true,
            Column {
                fill_max_width: true,
                padding: SP[3],
                space_role: SpaceRole::Sm,
                Label { text: "멤버", fs: Fs::Lg, weight: Weight::Semibold }
                // TODO(dioxus-compose): TextField is uncontrolled and has no initial value.
                TextField {
                    fill_max_width: true,
                    placeholder: "멤버 검색",
                    on_value_change: move |v: String| query.set(v),
                }
                ScrollRow {
                    fill_max_width: true,
                    height: 48.0,
                    for f in MemberFilter::ALL {
                        PickChip { key: "{f.label()}", text: f.label(), selected: f == filter(), on_click: move |()| filter.set(f) }
                        Spacer { width: SP[0] }
                    }
                }
                if list.is_empty() {
                    Label { text: "일치하는 멤버가 없습니다", color: paint(p.text_2) }
                }
                for m in list {
                    MemberTile { key: "{m.session_id}", member: m, on_open: move |s: String| ui.open_conversation(&s) }
                }
            }
        }
        // Links. TODO(channels, routines, version control screens): they link here when built.
        EmberCard {
            fill_max_width: true,
            Column {
                fill_max_width: true,
                padding: SP[3],
                space_role: SpaceRole::Sm,
                Label { text: "연결", fs: Fs::Lg, weight: Weight::Semibold }
                Row {
                    fill_max_width: true,
                    space_role: SpaceRole::Sm,
                    AppButton { text: "채널", small: true, enabled: false }
                    AppButton { text: "루틴", small: true, enabled: false }
                    AppButton { text: "버전 관리", small: true, enabled: false }
                }
                Label { text: SOON, fs: Fs::Xs, color: paint(p.text_2) }
            }
        }
    }
}

// --- settings sheet --------------------------------------------------------------------------

#[component]
fn SettingsSection(
    #[props(into)] title: String,
    #[props(into)] hint: String,
    children: Element,
) -> Element {
    let theme = use_ember_theme();
    let p = theme.palette;
    rsx! {
        Column {
            fill_max_width: true,
            space_role: SpaceRole::Xs,
            Label { text: title, fs: Fs::Md, weight: Weight::Semibold }
            if !hint.is_empty() {
                Label { text: hint, fs: Fs::Xs, color: paint(p.text_2) }
            }
            {children}
        }
    }
}

#[component]
fn SettingsSheet(
    project: ProjectBox,
    computers: Vec<ComputerChip>,
    on_close: EventHandler<()>,
) -> Element {
    let theme = use_ember_theme();
    let p = theme.palette;
    let default_project = is_default_project(&project.id);
    let mut draft = use_signal(|| SettingsDraft {
        name: project.name.clone(),
        repo: project.repo.clone().unwrap_or_default(),
        branch: project.branch.clone().unwrap_or_default(),
    });
    let errors = validate_settings(&draft.read(), &project.id);
    let can_save = SETTINGS_SAVE_AVAILABLE && errors.is_empty();
    let pid = project.id.clone();
    rsx! {
        EmberSheet {
            title: format!("{} 설정", project.name),
            open: true,
            on_dismiss: move |()| on_close.call(()),
            ScrollColumn {
                fill_max_width: true,
                height: 560.0,
                Column {
                    fill_max_width: true,
                    padding: SP[4],
                    space_role: SpaceRole::Md,
                    SettingsSection { title: "기본", hint: if part_supported(SettingsPart::Name) { "" } else { SOON },
                        // TODO(server): rename and repository need a project settings API.
                        TextField {
                            fill_max_width: true,
                            placeholder: project.name.clone(),
                            enabled: part_supported(SettingsPart::Name) && !default_project,
                            on_value_change: move |v: String| draft.write().name = v,
                        }
                        TextField {
                            fill_max_width: true,
                            placeholder: "저장소",
                            enabled: part_supported(SettingsPart::Repository),
                            on_value_change: move |v: String| draft.write().repo = v,
                        }
                        TextField {
                            fill_max_width: true,
                            placeholder: "기본 브랜치",
                            enabled: part_supported(SettingsPart::Repository),
                            on_value_change: move |v: String| draft.write().branch = v,
                        }
                        for e in errors.iter() {
                            Label { key: "{e.message()}", text: e.message(), fs: Fs::Xs, color: paint(p.red.strong) }
                        }
                    }
                    SettingsSection { title: "컴퓨터", hint: "이 프로젝트의 팀원이 일할 수 있는 컴퓨터입니다",
                        if computers.is_empty() {
                            Label { text: "등록된 컴퓨터가 없습니다", color: paint(p.text_2) }
                        }
                        for c in computers.iter().cloned() {
                            Row {
                                key: "{c.id}",
                                fill_max_width: true,
                                alignment: Alignment::CenterStart,
                                space_role: SpaceRole::Sm,
                                Label { text: c.name.clone() }
                                Label { text: reach_pill(c.reachability).0, fs: Fs::Xs, color: paint(p.text_2) }
                                Spacer { weight: 1.0 }
                                Switch {
                                    checked: project.computers.contains(&c.id),
                                    on_change: {
                                        let (pr, id) = (pid.clone(), c.id.clone());
                                        move |v: bool| set_assignment(pr.clone(), id.clone(), v)
                                    },
                                }
                            }
                        }
                    }
                    SettingsSection { title: "새 팀원 기본값", hint: "하네스, 모델, 권한 모드는 새 팀원에게만 적용됩니다. 준비 중입니다",
                        AppButton { text: "기본값 고르기", enabled: part_supported(SettingsPart::MemberDefaults) }
                    }
                    SettingsSection { title: "팀 캐릭터", hint: "팀 캐릭터 고르기는 준비 중입니다",
                        // TODO(FR-L22): the six styles (몽글, 말랑, 크루, 포켓, 크림, 키링).
                        AppButton { text: "몽글", enabled: part_supported(SettingsPart::CharacterStyle) }
                    }
                    if leader_choice_visible(&project.id) {
                        SettingsSection { title: "리더", hint: "리더 지정은 준비 중입니다",
                            // TODO(server): leader change; the badge follows the team's leader today.
                            if let Some(l) = project.leader() {
                                Label { text: format!("현재 리더: {}", l.name) }
                            } else {
                                Label { text: "리더가 없습니다", color: paint(p.text_2) }
                            }
                        }
                    } else {
                        SettingsSection { title: "리더", hint: "",
                            Label { text: "기본 프로젝트에는 리더가 없습니다", color: paint(p.text_2) }
                        }
                    }
                    SettingsSection { title: "프로필 덮어쓰기", hint: "이 프로젝트에서만 하네스에 다른 프로필을 쓰는 기능은 준비 중입니다",
                        AppButton { text: "프로필 고르기", enabled: part_supported(SettingsPart::ProfileOverride) }
                    }
                    SettingsSection { title: "연결", hint: "채널, 루틴, 버전 관리는 준비 중입니다",
                        Label { text: "연결된 항목이 없습니다", color: paint(p.text_2) }
                    }
                    SettingsSection { title: "위험 영역", hint: if default_project { "기본 프로젝트는 보관하거나 지울 수 없습니다" } else { SOON },
                        AppButton {
                            text: "프로젝트 보관",
                            kind: ButtonKind::Destructive,
                            enabled: danger_allowed(&project.id, SettingsPart::Archive),
                        }
                        AppButton {
                            text: "프로젝트 삭제",
                            kind: ButtonKind::Destructive,
                            enabled: danger_allowed(&project.id, SettingsPart::Delete),
                        }
                    }
                    Row {
                        fill_max_width: true,
                        arrangement: Arrangement::End,
                        space_role: SpaceRole::Sm,
                        AppButton { text: "취소", on_click: move |()| on_close.call(()) }
                        AppButton { text: "저장", kind: ButtonKind::Primary, enabled: can_save }
                    }
                }
            }
        }
    }
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
