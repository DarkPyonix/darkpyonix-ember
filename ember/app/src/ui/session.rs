//! The Session screen (FR-L5 to FR-L7, FR-L15, FR-L16, FR-L18, FR-L24;
//! `docs/DESIGN.md` section 6.3). The shell's 세션 menu: the inside of one project.
//!
//! Left, the side list (search, one section per project, the leader first); on the right a top
//! bar (project, view switcher, new member, version control, web page, IDE) over one of three
//! views: **멀티** (panes side by side, pane tabs per project), **싱글** (one member's
//! conversation plus a right panel) and **상태** (kanban).
//!
//! The decisions (view persistence, panes per row per size class, section order, kanban
//! bucketing, pane order and tabs, the IDE list, which leases a view opens) are plain
//! functions and types at the top of this file, tested without a renderer. The components
//! below draw them.
//!
//! What the pinned toolkit or the server cannot do yet, with the workaround used
//! (`GAPS-WORKLIST.md`):
//! - no drag and drop between panes (GAPS: 앱 내부 드래그 앤 드롭): a pane's menu moves it left
//!   or right, and a member picked in the side list is added as a pane. TODO: drag to swap and
//!   split on an edge when the toolkit has drag sources and targets;
//! - no back stack (GAPS: 뒤로 가기 스택): the side list sheet and the right panel sheet close
//!   with their own buttons;
//! - only three size classes (GAPS: 창 폭 레이아웃): expanded shows the side list as a column
//!   and two panes per row; medium and compact use a sheet and one pane at a time;
//! - no terminal data in the client: the 터미널 toggle says "준비 중입니다";
//! - the server has no context, outputs, changes, PR, timeline or port APIs for a session.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use dioxus_compose::prelude::*;
use serde::{Deserialize, Serialize};

use ember_client::LauncherStatus;
use ember_client::wire::SessionRecord;

use crate::ide::{IdeAction, IdeKind, open_external, plan};
use crate::server::{CurrentComputer, IdeLaunch};
use crate::services::{run, services};
use crate::ui::components::{
    AppButton, ButtonKind, EmberCard, EmberSheet, IconButton, Label, PickChip, Pill, PillTone,
};
use crate::ui::conversation::{ConversationView, TranscriptRow, item_key};
use crate::ui::home::{Harness, PermMode, default_option, model_catalog, modes_for};
use crate::ui::project::{
    Character, Member, MemberFilter, ProjectBox, display_name, filter_members, project_boxes,
    status_pill,
};
use crate::ui::theme::*;
use crate::ui::use_ui;

/// Shown wherever something is not built yet.
pub use crate::ui::project::SOON;

// ---------------------------------------------------------------------------------------------
// Texts (합니다체, NFR-L4)
// ---------------------------------------------------------------------------------------------

const NO_PROJECT: &str = "프로젝트가 없습니다";
const NO_MEMBERS: &str = "표시할 팀원이 없습니다";
const PICK_FROM_LIST: &str = "목록에서 팀원을 고르면 이 탭에 추가됩니다";
const NO_CONVERSATION: &str = "아직 대화가 없습니다";
const TERMINAL_SOON: &str = "터미널은 준비 중입니다";
const MODEL_LOCKED: &str = "실행 중인 세션의 모델은 바꿀 수 없습니다";
const VCS_SOON: &str = "버전 관리는 준비 중입니다";
const WEB_SOON: &str = "웹 페이지 목록은 준비 중입니다";
const COMPUTER_LOADING: &str = "확인하는 중입니다";
const NO_SESSION: &str = "세션이 없습니다";
const PR_TITLE: &str = "PR과 CI";
const TIMELINE_TITLE: &str = "에이전트 소통 타임라인";

/// Every sentence this screen shows, checked for plain "~다" endings (NFR-L4).
pub const UI_STRINGS: &[&str] = &[
    SOON,
    NO_PROJECT,
    NO_MEMBERS,
    PICK_FROM_LIST,
    NO_CONVERSATION,
    TERMINAL_SOON,
    MODEL_LOCKED,
    VCS_SOON,
    WEB_SOON,
    COMPUTER_LOADING,
    NO_SESSION,
];

// ---------------------------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------------------------

/// The three views of the header switcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionView {
    Multi,
    Single,
    Status,
}

impl SessionView {
    pub const ALL: [SessionView; 3] =
        [SessionView::Multi, SessionView::Single, SessionView::Status];

    pub fn label(self) -> &'static str {
        match self {
            SessionView::Multi => "멀티",
            SessionView::Single => "싱글",
            SessionView::Status => "상태",
        }
    }

    /// The key saved in the prefs.
    pub fn key(self) -> &'static str {
        match self {
            SessionView::Multi => "multi",
            SessionView::Single => "single",
            SessionView::Status => "status",
        }
    }

    pub fn from_key(k: &str) -> Option<SessionView> {
        SessionView::ALL.into_iter().find(|v| v.key() == k)
    }
}

/// The view to start in: the saved one, else multi.
pub fn view_from_saved(saved: Option<&str>) -> SessionView {
    saved
        .and_then(SessionView::from_key)
        .unwrap_or(SessionView::Multi)
}

// ---------------------------------------------------------------------------------------------
// Layout per size class
// ---------------------------------------------------------------------------------------------

/// How panes are placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneLayout {
    pub per_row: usize,
    /// One pane at a time with previous and next buttons (the swipe of the design).
    pub paged: bool,
}

/// Two panes per row on expanded, one at a time otherwise.
/// TODO(GAPS: 창 폭 레이아웃): the design allows up to four per row at wide widths and breaks at
/// 1279 and 900; the toolkit reports only its three classes.
pub fn pane_layout(class: WindowSizeClass) -> PaneLayout {
    match class {
        WindowSizeClass::Expanded => PaneLayout {
            per_row: 2,
            paged: false,
        },
        WindowSizeClass::Medium | WindowSizeClass::Compact => PaneLayout {
            per_row: 1,
            paged: true,
        },
    }
}

/// Pane indexes grouped into rows of `per_row`.
pub fn pane_rows(n: usize, per_row: usize) -> Vec<Vec<usize>> {
    let per_row = per_row.max(1);
    (0..n)
        .collect::<Vec<_>>()
        .chunks(per_row)
        .map(<[usize]>::to_vec)
        .collect()
}

/// The page after a previous or next press, never past either end.
pub fn page_step(current: usize, count: usize, forward: bool) -> usize {
    if count == 0 {
        return 0;
    }
    let cur = current.min(count - 1);
    if forward {
        (cur + 1).min(count - 1)
    } else {
        cur.saturating_sub(1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideListMode {
    /// A column beside the content.
    Column,
    /// A sheet opened from the top bar.
    Drawer,
}

pub fn side_list_mode(class: WindowSizeClass) -> SideListMode {
    match class {
        WindowSizeClass::Expanded => SideListMode::Column,
        WindowSizeClass::Medium | WindowSizeClass::Compact => SideListMode::Drawer,
    }
}

/// 264 dp at 1440 in the design.
pub const SIDE_LIST_W: f32 = 264.0;
/// A pane's height in the grid (a scroll column cannot give a list a bounded height).
pub const PANE_H: f32 = 560.0;
/// The right panel of the single view.
pub const INFO_PANEL_W: f32 = 320.0;

// ---------------------------------------------------------------------------------------------
// Side list
// ---------------------------------------------------------------------------------------------

/// One section of the side list.
#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub project: String,
    pub name: String,
    pub members: Vec<Member>,
}

/// Sections for the side list: the current project first, the rest in the given order; inside a
/// section the leader first, then the order the project gives (waiting first). A search
/// matches a member's name, task or harness, or a project's name (which keeps the whole
/// section); sections with no match drop out.
pub fn side_sections(boxes: &[ProjectBox], current: &str, query: &str) -> Vec<Section> {
    let q = query.trim().to_lowercase();
    let mut idx: Vec<usize> = (0..boxes.len()).collect();
    idx.sort_by_key(|&i| boxes[i].id != current);
    idx.into_iter()
        .filter_map(|i| {
            let b = &boxes[i];
            let name = display_name(&b.id);
            let project_match = !q.is_empty()
                && (name.to_lowercase().contains(&q) || b.id.to_lowercase().contains(&q));
            let mut members = if q.is_empty() || project_match {
                b.members.clone()
            } else {
                filter_members(&b.members, MemberFilter::All, &q)
            };
            if members.is_empty() && !q.is_empty() {
                return None;
            }
            members.sort_by_key(|m| !m.leader);
            Some(Section {
                project: b.id.clone(),
                name,
                members,
            })
        })
        .collect()
}

/// The project the screen shows: the focused session's, else the chosen one, else the first.
pub fn resolve_project(
    focus: Option<&str>,
    choice: Option<&str>,
    boxes: &[ProjectBox],
) -> Option<String> {
    [focus, choice]
        .into_iter()
        .flatten()
        .find(|p| boxes.iter().any(|b| b.id == *p))
        .map(String::from)
        .or_else(|| boxes.first().map(|b| b.id.clone()))
}

/// The member the single view shows: the focused one, else the leader, else the first.
pub fn single_target(members: &[Member], focus: Option<&str>) -> Option<String> {
    focus
        .and_then(|f| members.iter().find(|m| m.session_id == f))
        .or_else(|| members.iter().find(|m| m.leader))
        .or_else(|| members.first())
        .map(|m| m.session_id.clone())
}

// ---------------------------------------------------------------------------------------------
// Kanban
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KanbanColumn {
    Waiting,
    Running,
    Approval,
    Review,
    Done,
}

impl KanbanColumn {
    pub const ALL: [KanbanColumn; 5] = [
        KanbanColumn::Waiting,
        KanbanColumn::Running,
        KanbanColumn::Approval,
        KanbanColumn::Review,
        KanbanColumn::Done,
    ];

    pub fn label(self) -> &'static str {
        match self {
            KanbanColumn::Waiting => "대기",
            KanbanColumn::Running => "실행",
            KanbanColumn::Approval => "승인 대기",
            KanbanColumn::Review => "검토",
            KanbanColumn::Done => "완료",
        }
    }
}

/// The column a status belongs to. A new result and a failure both wait for a person to look at
/// them, so both are in 검토.
pub fn kanban_column(s: LauncherStatus) -> KanbanColumn {
    match s {
        LauncherStatus::Idle | LauncherStatus::Unknown => KanbanColumn::Waiting,
        LauncherStatus::Running => KanbanColumn::Running,
        LauncherStatus::WaitingForApproval => KanbanColumn::Approval,
        LauncherStatus::FinishedUnread | LauncherStatus::Failed => KanbanColumn::Review,
        LauncherStatus::Finished => KanbanColumn::Done,
    }
}

/// All five columns, each with its members in the given order.
pub fn kanban(members: &[Member]) -> Vec<(KanbanColumn, Vec<Member>)> {
    KanbanColumn::ALL
        .into_iter()
        .map(|c| {
            (
                c,
                members
                    .iter()
                    .filter(|m| kanban_column(m.status) == c)
                    .cloned()
                    .collect(),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// IDE chooser (FR-L7, D32)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdeChoice {
    pub kind: IdeKind,
    pub label: &'static str,
}

fn ide_label(kind: IdeKind) -> &'static str {
    match kind {
        IdeKind::Ember => "Ember 에디터",
        IdeKind::VsCode => "VS Code",
        IdeKind::Gateway => "JetBrains Gateway",
    }
}

/// Exactly the three IDEs of D32, the remembered one first.
pub fn ide_choices(remembered: Option<IdeKind>) -> Vec<IdeChoice> {
    let mut kinds: Vec<IdeKind> = IdeKind::ALL.to_vec();
    if let Some(k) = remembered {
        kinds.retain(|x| *x != k);
        kinds.insert(0, k);
    }
    kinds
        .into_iter()
        .map(|kind| IdeChoice {
            kind,
            label: ide_label(kind),
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Panes and tabs (FR-L18)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneMove {
    Left,
    Right,
}

/// Swap `id` with its neighbour. False when it is at that end or not in the list.
pub fn move_pane(order: &mut Vec<String>, id: &str, dir: PaneMove) -> bool {
    let Some(i) = order.iter().position(|x| x == id) else {
        return false;
    };
    let j = match dir {
        PaneMove::Left if i > 0 => i - 1,
        PaneMove::Right if i + 1 < order.len() => i + 1,
        _ => return false,
    };
    order.swap(i, j);
    true
}

pub fn can_move(order: &[String], id: &str, dir: PaneMove) -> bool {
    match (order.iter().position(|x| x == id), dir) {
        (Some(i), PaneMove::Left) => i > 0,
        (Some(i), PaneMove::Right) => i + 1 < order.len(),
        _ => false,
    }
}

/// One tab of panes. `members: None` shows every member of the project; the first edit pins the
/// list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneTab {
    pub name: String,
    #[serde(default)]
    pub members: Option<Vec<String>>,
}

impl PaneTab {
    pub fn all(name: impl Into<String>) -> PaneTab {
        PaneTab {
            name: name.into(),
            members: None,
        }
    }
}

/// The panes a tab shows now: members that still exist, in the tab's order.
pub fn visible_panes(tab: &PaneTab, all: &[String]) -> Vec<String> {
    match &tab.members {
        None => all.to_vec(),
        Some(v) => v.iter().filter(|id| all.contains(id)).cloned().collect(),
    }
}

pub fn close_pane(tab: &mut PaneTab, all: &[String], id: &str) {
    let mut v = visible_panes(tab, all);
    v.retain(|x| x != id);
    tab.members = Some(v);
}

pub fn add_pane(tab: &mut PaneTab, all: &[String], id: &str) {
    if !all.iter().any(|x| x == id) {
        return;
    }
    let mut v = visible_panes(tab, all);
    if !v.iter().any(|x| x == id) {
        v.push(id.to_string());
    }
    tab.members = Some(v);
}

pub fn reorder_pane(tab: &mut PaneTab, all: &[String], id: &str, dir: PaneMove) -> bool {
    let mut v = visible_panes(tab, all);
    if !move_pane(&mut v, id, dir) {
        return false;
    }
    tab.members = Some(v);
    true
}

/// The tabs of one project and the selected one. Saved per project in the prefs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectPanes {
    pub tabs: Vec<PaneTab>,
    #[serde(default)]
    pub active: usize,
}

impl Default for ProjectPanes {
    fn default() -> Self {
        ProjectPanes {
            tabs: vec![PaneTab::all("탭 1")],
            active: 0,
        }
    }
}

impl ProjectPanes {
    /// Repair a value read from disk: at least one tab, a valid selection.
    pub fn normalized(mut self) -> ProjectPanes {
        if self.tabs.is_empty() {
            return ProjectPanes::default();
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self
    }

    pub fn active_tab(&self) -> &PaneTab {
        &self.tabs[self.active.min(self.tabs.len() - 1)]
    }

    pub fn active_tab_mut(&mut self) -> &mut PaneTab {
        let i = self.active.min(self.tabs.len() - 1);
        &mut self.tabs[i]
    }

    /// A new tab holding the panes shown now, selected.
    pub fn add_tab(&mut self, current: &[String]) {
        let mut n = self.tabs.len() + 1;
        let name = loop {
            let candidate = format!("탭 {n}");
            if !self.tabs.iter().any(|t| t.name == candidate) {
                break candidate;
            }
            n += 1;
        };
        self.tabs.push(PaneTab {
            name,
            members: Some(current.to_vec()),
        });
        self.active = self.tabs.len() - 1;
    }

    pub fn select(&mut self, i: usize) {
        if i < self.tabs.len() {
            self.active = i;
        }
    }

    /// Remove a tab; the last one stays.
    pub fn remove_tab(&mut self, i: usize) {
        if self.tabs.len() <= 1 || i >= self.tabs.len() {
            return;
        }
        self.tabs.remove(i);
        if self.active > i {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len() - 1);
    }
}

// ---------------------------------------------------------------------------------------------
// Leases
// ---------------------------------------------------------------------------------------------

/// Which sessions a view opens and closes. The client keeps one lease per open session, so a
/// view opens what it shows and is not open yet, and closes only what it opened itself and
/// no longer shows; never the session the route has open (the shell closes that one).
pub fn lease_plan(
    wanted: &[String],
    is_open: impl Fn(&str) -> bool,
    mine: &HashSet<String>,
    focus: Option<&str>,
) -> (Vec<String>, Vec<String>) {
    let mut open: Vec<String> = Vec::new();
    for id in wanted {
        if !is_open(id) && !open.contains(id) {
            open.push(id.clone());
        }
    }
    let mut close: Vec<String> = mine
        .iter()
        .filter(|id| !wanted.contains(id) && Some(id.as_str()) != focus)
        .cloned()
        .collect();
    close.sort();
    (open, close)
}

/// Keep `wanted` open for as long as the calling component is mounted.
fn use_leases(wanted: &[String], focus: Option<&str>) {
    let mine = use_hook(|| Rc::new(RefCell::new(HashSet::<String>::new())));
    let focus_now = use_hook(|| Rc::new(RefCell::new(None::<String>)));
    *focus_now.borrow_mut() = focus.map(String::from);
    let (open, close) = {
        let m = mine.borrow();
        lease_plan(
            wanted,
            |id| services().client.read(|s| s.is_open(id)),
            &m,
            focus,
        )
    };
    for id in open {
        services().open_session(&id);
        mine.borrow_mut().insert(id);
    }
    for id in close {
        services().close_session(&id);
        mine.borrow_mut().remove(&id);
    }
    dioxus_core::use_drop(move || {
        let keep = focus_now.borrow().clone();
        for id in mine.borrow().iter() {
            if Some(id) != keep.as_ref() {
                services().close_session(id);
            }
        }
    });
}

// ---------------------------------------------------------------------------------------------
// Composer chips, pane header, right panel
// ---------------------------------------------------------------------------------------------

/// The chip text of a running session, with the labels of Home's picker (FR-L15): the harness
/// label and the model's display name from the shared catalog. An existing session's model
/// cannot be changed, so the chip is read-only.
pub fn model_chip_label(agent: &str, model: Option<&str>) -> String {
    let harness = Harness::from_kind(agent);
    let name = model.filter(|m| !m.is_empty()).map(|m| {
        model_catalog()
            .into_iter()
            .find(|o| o.model == m)
            .map(|o| o.name.to_string())
            .unwrap_or_else(|| m.to_string())
    });
    match (harness, name) {
        (Some(h), Some(n)) => format!("{} \u{00b7} {}", h.label(), n),
        (Some(h), None) => format!("{} \u{00b7} {}", h.label(), default_option(h).name),
        (None, Some(n)) => format!("{agent} \u{00b7} {n}"),
        (None, None) => format!("{agent} \u{00b7} 기본 모델"),
    }
}

/// The mode a running session is in. The client only knows approve-everything (FR-A5), which
/// is bypass; every other session asks for each command.
pub fn current_mode(approve_all: bool) -> PermMode {
    if approve_all {
        PermMode::Bypass
    } else {
        PermMode::Ask
    }
}

pub fn mode_chip_label(approve_all: bool) -> &'static str {
    current_mode(approve_all).label()
}

/// Bypass is drawn selected, as Home does (FR-L16).
pub fn mode_chip_warns(approve_all: bool) -> bool {
    current_mode(approve_all).is_bypass()
}

/// The modes the harness supports (Home's list), each with whether a running session can be
/// switched to it now. Only ask and bypass can: edits-only and plan need CLI flags that are
/// set when a session starts. An unknown harness offers ask and bypass.
pub fn mode_rows(agent: &str) -> Vec<(PermMode, bool)> {
    let modes: &[PermMode] = match Harness::from_kind(agent) {
        Some(h) => modes_for(h),
        None => &[PermMode::Ask, PermMode::Bypass],
    };
    modes
        .iter()
        .map(|m| (*m, matches!(m, PermMode::Ask | PermMode::Bypass)))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneToggle {
    Conversation,
    Terminal,
}

impl PaneToggle {
    pub const ALL: [PaneToggle; 2] = [PaneToggle::Conversation, PaneToggle::Terminal];

    pub fn label(self) -> &'static str {
        match self {
            PaneToggle::Conversation => "대화",
            PaneToggle::Terminal => "터미널",
        }
    }
}

/// The four cards of the single view's right panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfoCard {
    Environment,
    Context,
    Outputs,
    Changes,
}

impl InfoCard {
    pub const ALL: [InfoCard; 4] = [
        InfoCard::Environment,
        InfoCard::Context,
        InfoCard::Outputs,
        InfoCard::Changes,
    ];

    pub fn label(self) -> &'static str {
        match self {
            InfoCard::Environment => "환경",
            InfoCard::Context => "컨텍스트",
            InfoCard::Outputs => "결과물",
            InfoCard::Changes => "변경 사항",
        }
    }

    /// Whether the client has data for the card. Context (tokens per item), outputs (PRs,
    /// artifacts, web pages) and changes (files, +/-) have no server API yet.
    pub fn ready(self) -> bool {
        matches!(self, InfoCard::Environment)
    }
}

/// Label and value rows of the environment card, from what the client knows.
pub fn environment_rows(
    rec: &SessionRecord,
    computer: Option<&str>,
    approve_all: bool,
) -> Vec<(&'static str, String)> {
    vec![
        (
            "컴퓨터",
            computer
                .map(String::from)
                .unwrap_or_else(|| COMPUTER_LOADING.to_string()),
        ),
        ("하네스", rec.agent.clone()),
        (
            "모델",
            rec.model
                .clone()
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| "기본 모델".to_string()),
        ),
        ("권한 모드", mode_chip_label(approve_all).to_string()),
        ("프로젝트", display_name(&rec.project)),
        ("작업 폴더", rec.cwd.clone()),
        // TODO(server): the repository and branch of a session's project (FR-L26).
        ("저장소", SOON.to_string()),
    ]
}

// ---------------------------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------------------------

/// Change the saved panes of a project.
fn edit_panes(ui: crate::ui::Ui, project: &str, f: impl FnOnce(&mut ProjectPanes)) {
    let project = project.to_string();
    ui.update_prefs(move |p| {
        let mut cur = p
            .panes
            .get(&project)
            .cloned()
            .unwrap_or_default()
            .normalized();
        f(&mut cur);
        p.panes.insert(project, cur);
    });
}

fn panes_of(ui: crate::ui::Ui, project: &str) -> ProjectPanes {
    ui.prefs
        .read()
        .panes
        .get(project)
        .cloned()
        .unwrap_or_default()
        .normalized()
}

/// The model and mode chips of a composer (FR-L15, FR-L16).
#[component]
pub fn ComposerChips(session: String) -> Element {
    let ui = use_ui();
    let p = use_ember_theme().palette;
    let mut model_menu = use_signal(|| false);
    let mut mode_menu = use_signal(|| false);
    let (info, approve_all) = {
        let _ = *ui.live.launcher.read();
        services().client.read(|s| {
            (
                s.session(&session)
                    .map(|v| (v.record.agent.clone(), v.record.model.clone())),
                s.approve_all(&session),
            )
        })
    };
    let set_mode = use_callback({
        let session = session.clone();
        move |enable: bool| {
            mode_menu.set(false);
            let sid = session.clone();
            run(
                async move {
                    services()
                        .client
                        .set_approve_all(&sid, enable)
                        .await
                        .map_err(|e| e.to_string())
                },
                |r| {
                    if let Err(e) = r {
                        show_message(format!("모드를 바꾸지 못했습니다: {e}"));
                    }
                },
            );
        }
    });
    let Some((agent, model)) = info else {
        return rsx! {};
    };
    rsx! {
        Row {
            fill_max_width: true,
            space_role: SpaceRole::Xs,
            alignment: Alignment::CenterStart,
            Menu {
                expanded: model_menu(),
                on_dismiss: move |_| model_menu.set(false),
                anchor: rsx! {
                    PickChip { text: model_chip_label(&agent, model.as_deref()), icon: IconRole::Settings, on_click: move |()| model_menu.set(true) }
                },
                Column {
                    padding_role: SpaceRole::Sm,
                    space_role: SpaceRole::Xs,
                    Label { text: MODEL_LOCKED }
                    Label { text: SOON, color: paint(p.muted) }
                }
            }
            Menu {
                expanded: mode_menu(),
                on_dismiss: move |_| mode_menu.set(false),
                anchor: rsx! {
                    PickChip {
                        text: mode_chip_label(approve_all),
                        selected: mode_chip_warns(approve_all),
                        on_click: move |()| mode_menu.set(true),
                    }
                },
                for (m, settable) in mode_rows(&agent) {
                    Button {
                        key: "{m.label()}",
                        text: if settable { m.label().to_string() } else { format!("{} ({})", m.label(), SOON) },
                        enabled: settable,
                        variant: ButtonVariant::Text,
                        height: H_BTN,
                        fill_max_width: true,
                        color: if m.is_bypass() { paint(p.orange.strong) } else { paint(p.text) },
                        on_click: move |_: ()| set_mode.call(m.is_bypass()),
                    }
                }
            }
        }
    }
}

/// Hand an IDE choice to the OS, as the conversation view does.
/// TODO(conversation.rs): the conversation view keeps its own copy of this; unify.
fn launch_ide(kind: IdeKind, launch: Option<Result<IdeLaunch, String>>) {
    let target = match &launch {
        None => {
            show_message("IDE 목록을 받아오는 중입니다");
            return;
        }
        Some(Err(e)) => {
            show_message(format!("IDE 목록을 받지 못했습니다: {e}"));
            return;
        }
        Some(Ok(l)) => l.target(kind.key()).cloned(),
    };
    let label = ide_label(kind);
    match plan(kind, target.as_ref()) {
        IdeAction::OpenUrl(url) => run(
            async move {
                tokio::task::spawn_blocking(move || open_external(&url))
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r.map_err(|e| e.to_string()))
            },
            move |r| match r {
                Ok(()) => show_message(format!("{label}: 여는 중입니다")),
                Err(e) => show_message(format!("{label}: 열지 못했습니다: {e}")),
            },
        ),
        IdeAction::EmberPlaceholder => {
            // TODO(M8, editor core): open the Ember IDE window here.
            show_message("Ember 에디터는 준비 중입니다. VS Code나 JetBrains Gateway를 사용하세요");
        }
        IdeAction::Unavailable(why) => show_message(format!("{label}: 사용할 수 없습니다: {why}")),
    }
}

/// "IDE 열기" with the three-entry chooser (FR-L7). Keyed by session: it asks the server for
/// that session's targets when it appears.
#[component]
fn IdeMenu(session: String) -> Element {
    let ui = use_ui();
    let ide = use_signal(|| None::<Result<IdeLaunch, String>>);
    let mut open = use_signal(|| false);
    use_hook({
        let sid = session.clone();
        move || {
            run(
                async move {
                    services()
                        .server
                        .ide_targets(&sid, None)
                        .await
                        .map_err(|e| e.to_string())
                },
                move |r| {
                    let mut ide = ide;
                    ide.set(Some(r));
                },
            );
        }
    });
    let remembered = ui
        .prefs
        .read()
        .ide_target
        .as_deref()
        .and_then(IdeKind::from_key);
    let choices = ide_choices(remembered);
    rsx! {
        Menu {
            expanded: open(),
            on_dismiss: move |_| open.set(false),
            anchor: rsx! {
                AppButton { text: "IDE 열기", icon: IconRole::Forward, kind: ButtonKind::Primary, on_click: move |()| open.set(true) }
            },
            for c in choices {
                Button {
                    key: "{c.kind.key()}",
                    text: c.label,
                    variant: ButtonVariant::Text,
                    height: H_BTN,
                    fill_max_width: true,
                    on_click: move |_: ()| {
                        open.set(false);
                        ui.update_prefs(|p| p.ide_target = Some(c.kind.key().to_string()));
                        launch_ide(c.kind, ide.read().clone());
                    },
                }
            }
        }
    }
}

/// The side list: search, then a section per project (FR-L18).
#[component]
fn SideList(
    sections: Vec<Section>,
    current: String,
    active: Option<String>,
    on_query: EventHandler<String>,
    on_open: EventHandler<(String, String)>,
) -> Element {
    let p = use_ember_theme().palette;
    rsx! {
        Column {
            fill_max_width: true,
            fill_max_height: true,
            padding_role: SpaceRole::Sm,
            space_role: SpaceRole::Sm,
            TextField {
                fill_max_width: true,
                placeholder: "세션 검색",
                on_value_change: move |v: String| on_query.call(v),
            }
            ScrollColumn {
                fill_max_width: true,
                weight: 1.0,
                for s in sections {
                    Column {
                        key: "{s.project}",
                        fill_max_width: true,
                        space_role: SpaceRole::Xs,
                        Row {
                            fill_max_width: true,
                            alignment: Alignment::CenterStart,
                            space_role: SpaceRole::Sm,
                            Label {
                                text: s.name.clone(),
                                fs: Fs::Sm,
                                weight: Weight::Semibold,
                                color: if s.project == current { paint(p.text) } else { paint(p.text_2) },
                                max_lines: 1,
                            }
                            Pill { text: s.members.len().to_string(), tone: PillTone::Grey, small: true }
                        }
                        for m in s.members.clone() {
                            Row {
                                key: "{m.session_id}",
                                fill_max_width: true,
                                alignment: Alignment::CenterStart,
                                space_role: SpaceRole::Xs,
                                background: if active.as_deref() == Some(m.session_id.as_str()) { paint(p.sel) } else { paint(Color::argb(0)) },
                                shape_role: ShapeRole::Full,
                                Character { agent: m.agent.clone(), leader: m.leader, size: 24.0, name: m.name.clone() }
                                Button {
                                    weight: 1.0,
                                    text: m.accessible_name(),
                                    height: H_BTN,
                                    variant: ButtonVariant::Text,
                                    color: paint(p.text),
                                    on_click: {
                                        let (proj, id) = (s.project.clone(), m.session_id.clone());
                                        move |()| on_open.call((proj.clone(), id.clone()))
                                    },
                                }
                                Pill { text: status_pill(m.status).0, tone: status_pill(m.status).1, small: true }
                            }
                        }
                        Spacer { height: SP[1] }
                    }
                }
            }
        }
    }
}

/// The transcript of one pane, with approvals (reuses the conversation view's rows).
#[component]
fn PaneTranscript(session: String, agent: String) -> Element {
    let ui = use_ui();
    let p = use_ember_theme().palette;
    let expanded = use_signal(HashSet::<String>::new);
    let answering = use_signal(HashSet::<String>::new);
    let live = ui.live;
    let (count, keys) = {
        let _ = *live.transcripts.read();
        services().client.read(|st| match st.transcript(&session) {
            Some(t) => (
                t.items.len(),
                t.items.iter().map(item_key).collect::<Vec<_>>(),
            ),
            None => (0, Vec::new()),
        })
    };
    let id_items = session.clone();
    rsx! {
        dioxus_compose::Box {
            fill_max_width: true,
            weight: 1.0,
            LazyColumn {
                fill_max_width: true,
                fill_max_height: true,
                item_count: count,
                key_of: move |i: usize| keys[i].clone(),
                item: move |i: usize| {
                    let _ = *live.transcripts.read();
                    let item = services().client.read(|st| st.transcript(&id_items).and_then(|t| t.items.get(i).cloned()));
                    match item {
                        Some(item) => rsx! {
                            TranscriptRow { session: id_items.clone(), agent: agent.clone(), item, expanded, answering }
                        },
                        None => rsx! {},
                    }
                },
            }
            if count == 0 {
                Column {
                    fill_max_width: true,
                    fill_max_height: true,
                    alignment: Alignment::Center,
                    arrangement: Arrangement::Center,
                    Label { text: NO_CONVERSATION, color: paint(p.muted) }
                }
            }
        }
    }
}

/// A pane's own composer: Enter sends, a message while the agent is busy is queued (FR-L6).
#[component]
fn PaneComposer(session: String, busy: bool) -> Element {
    let ui = use_ui();
    let p = use_ember_theme().palette;
    let mut draft = use_signal(String::new);
    // Changing this rebuilds the (uncontrolled) text field, which is how it is cleared.
    let mut generation = use_signal(|| 0_u64);
    let send = use_callback({
        let id = session.clone();
        move |text: String| {
            let text = text.trim().to_string();
            if text.is_empty() {
                return;
            }
            let s = services();
            s.outbox.lock().unwrap().push(&id, text);
            s.bump_outbox();
            s.drive_outbox(&id);
            draft.set(String::new());
            generation += 1;
        }
    });
    let interrupt = {
        let id = session.clone();
        move |()| {
            let sid = id.clone();
            run(
                async move {
                    services()
                        .client
                        .interrupt(&sid)
                        .await
                        .map_err(|e| e.to_string())
                },
                |r| {
                    if let Err(e) = r {
                        show_message(format!("중단하지 못했습니다: {e}"));
                    }
                },
            );
        }
    };
    let queued = {
        let _ = *ui.live.outbox.read();
        services().outbox.lock().unwrap().queued(&session)
    };
    rsx! {
        Column {
            fill_max_width: true,
            padding_role: SpaceRole::Xs,
            space_role: SpaceRole::Xs,
            if queued > 0 {
                Pill { text: format!("{queued}개 대기 중입니다"), tone: PillTone::Blue, small: true }
            }
            ComposerChips { session: session.clone() }
            Row {
                fill_max_width: true,
                background: paint(p.surface_2),
                corner_radius: CORNER_MD,
                padding_role: SpaceRole::Xs,
                space_role: SpaceRole::Xs,
                alignment: Alignment::CenterStart,
                for g in [generation()] {
                    TextField {
                        key: "pane-composer-{g}",
                        weight: 1.0,
                        multiline: true,
                        placeholder: if busy { "다음 턴에 보낼 메시지를 입력하세요" } else { "메시지를 입력하세요" },
                        on_value_change: move |v: String| draft.set(v),
                        on_submit: move |v: String| send.call(v),
                    }
                }
                if busy {
                    AppButton { text: "중단", icon: IconRole::Close, on_click: interrupt }
                }
                IconButton { icon: IconRole::Send, on_click: move |()| send.call(draft()) }
            }
        }
    }
}

/// One pane of the multi view: header, transcript with approvals, composer.
#[component]
fn Pane(
    project: String,
    member: Member,
    all: Vec<String>,
    order: Vec<String>,
    on_enlarge: EventHandler<String>,
) -> Element {
    let ui = use_ui();
    let p = use_ember_theme().palette;
    let mut toggle = use_signal(|| PaneToggle::Conversation);
    let mut menu = use_signal(|| false);
    use_hook({
        let id = member.session_id.clone();
        move || {
            run(
                async move {
                    services()
                        .client
                        .load_approve_all(&id)
                        .await
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                },
                |r| {
                    if let Err(e) = r {
                        tracing::debug!("approve-all: {e}");
                    }
                },
            );
        }
    });
    let id = member.session_id.clone();
    let (label, tone) = status_pill(member.status);
    let busy = crate::model::is_busy(member.status);
    let can_left = can_move(&order, &id, PaneMove::Left);
    let can_right = can_move(&order, &id, PaneMove::Right);
    let move_to = use_callback({
        let (project, all, id) = (project.clone(), all.clone(), id.clone());
        move |dir: PaneMove| {
            menu.set(false);
            let (all, id) = (all.clone(), id.clone());
            edit_panes(ui, &project, move |pp| {
                reorder_pane(pp.active_tab_mut(), &all, &id, dir);
            });
        }
    });
    let close = {
        let (project, all, id) = (project.clone(), all.clone(), id.clone());
        move |()| {
            let (all, id) = (all.clone(), id.clone());
            edit_panes(ui, &project, move |pp| {
                close_pane(pp.active_tab_mut(), &all, &id)
            });
        }
    };
    rsx! {
        Column {
            fill_max_width: true,
            fill_max_height: true,
            background: paint(p.surface),
            border_width: 1.0,
            border_color: paint(p.line),
            corner_radius: CORNER_LG,
            padding_role: SpaceRole::Sm,
            space_role: SpaceRole::Xs,
            Row {
                fill_max_width: true,
                alignment: Alignment::CenterStart,
                space_role: SpaceRole::Xs,
                Character { agent: member.agent.clone(), leader: member.leader, size: 28.0, name: member.name.clone() }
                Column {
                    weight: 1.0,
                    Label { text: member.accessible_name(), weight: Weight::Semibold, max_lines: 1 }
                    Label { text: member.task.clone(), fs: Fs::Xs, color: paint(p.text_2), max_lines: 1 }
                }
                Pill { text: label, tone, small: true }
                IconButton { icon: IconRole::Forward, on_click: { let id = id.clone(); move |()| on_enlarge.call(id.clone()) } }
                Menu {
                    expanded: menu(),
                    on_dismiss: move |_| menu.set(false),
                    anchor: rsx! { IconButton { icon: IconRole::More, on_click: move |()| menu.set(true) } },
                    // TODO(GAPS: 앱 내부 드래그 앤 드롭): drag the header to swap or split instead.
                    Button {
                        text: "왼쪽으로 이동",
                        icon: IconRole::Back,
                        enabled: can_left,
                        variant: ButtonVariant::Text,
                        height: H_BTN,
                        fill_max_width: true,
                        on_click: move |_: ()| move_to.call(PaneMove::Left),
                    }
                    Button {
                        text: "오른쪽으로 이동",
                        icon: IconRole::Forward,
                        enabled: can_right,
                        variant: ButtonVariant::Text,
                        height: H_BTN,
                        fill_max_width: true,
                        on_click: move |_: ()| move_to.call(PaneMove::Right),
                    }
                }
                IconButton { icon: IconRole::Close, on_click: close }
            }
            Row {
                fill_max_width: true,
                space_role: SpaceRole::Xs,
                for t in PaneToggle::ALL {
                    PickChip { key: "{t.label()}", text: t.label(), selected: toggle() == t, on_click: move |()| toggle.set(t) }
                }
            }
            if toggle() == PaneToggle::Conversation {
                PaneTranscript { session: id.clone(), agent: member.agent.clone() }
                PaneComposer { session: id.clone(), busy }
            } else {
                Column {
                    fill_max_width: true,
                    weight: 1.0,
                    alignment: Alignment::Center,
                    arrangement: Arrangement::Center,
                    // TODO(server): the client has no terminal stream to draw (FR-T9).
                    Label { text: TERMINAL_SOON, color: paint(p.muted) }
                }
            }
        }
    }
}

/// Panes of the project's active tab, with the tab strip (FR-L18).
#[component]
fn MultiView(
    project: String,
    members: Vec<Member>,
    focus: Option<String>,
    on_enlarge: EventHandler<String>,
) -> Element {
    let ui = use_ui();
    let window = use_window_size();
    let p = use_ember_theme().palette;
    let layout = pane_layout(window.class);
    let mut page = use_signal(|| 0_usize);
    let mut add_menu = use_signal(|| false);
    let all: Vec<String> = members.iter().map(|m| m.session_id.clone()).collect();
    let panes = panes_of(ui, &project);
    let vis = visible_panes(panes.active_tab(), &all);
    use_leases(&vis, focus.as_deref());
    let hidden: Vec<Member> = members
        .iter()
        .filter(|m| !vis.contains(&m.session_id))
        .cloned()
        .collect();
    let n = vis.len();
    let cur = page().min(n.saturating_sub(1));
    let pane_of = |idx: usize| -> Element {
        match members.iter().find(|m| m.session_id == vis[idx]) {
            Some(m) => rsx! {
                Pane {
                    key: "{m.session_id}",
                    project: project.clone(),
                    member: m.clone(),
                    all: all.clone(),
                    order: vis.clone(),
                    on_enlarge: move |id: String| on_enlarge.call(id),
                }
            },
            None => rsx! {},
        }
    };
    rsx! {
        Column {
            fill_max_width: true,
            fill_max_height: true,
            padding_role: SpaceRole::Sm,
            space_role: SpaceRole::Sm,
            ScrollRow {
                fill_max_width: true,
                for (i, t) in panes.tabs.iter().enumerate() {
                    PickChip {
                        key: "{t.name}",
                        text: t.name.clone(),
                        selected: i == panes.active,
                        on_click: {
                            let project = project.clone();
                            move |()| {
                                page.set(0);
                                edit_panes(ui, &project, move |pp| pp.select(i));
                            }
                        },
                    }
                }
                PickChip {
                    text: "새 탭",
                    icon: IconRole::Add,
                    on_click: {
                        let (project, vis) = (project.clone(), vis.clone());
                        move |()| {
                            page.set(0);
                            let vis = vis.clone();
                            edit_panes(ui, &project, move |pp| pp.add_tab(&vis));
                        }
                    },
                }
                if panes.tabs.len() > 1 {
                    PickChip {
                        text: "탭 닫기",
                        icon: IconRole::Close,
                        on_click: {
                            let project = project.clone();
                            let active = panes.active;
                            move |()| {
                                page.set(0);
                                edit_panes(ui, &project, move |pp| pp.remove_tab(active));
                            }
                        },
                    }
                }
                if !hidden.is_empty() {
                    Menu {
                        expanded: add_menu(),
                        on_dismiss: move |_| add_menu.set(false),
                        anchor: rsx! { PickChip { text: "팀원 추가", icon: IconRole::Add, on_click: move |()| add_menu.set(true) } },
                        for m in hidden.clone() {
                            Button {
                                key: "{m.session_id}",
                                text: m.accessible_name(),
                                variant: ButtonVariant::Text,
                                height: H_BTN,
                                fill_max_width: true,
                                on_click: {
                                    let (project, all, id) = (project.clone(), all.clone(), m.session_id.clone());
                                    move |_: ()| {
                                        add_menu.set(false);
                                        let (all, id) = (all.clone(), id.clone());
                                        edit_panes(ui, &project, move |pp| add_pane(pp.active_tab_mut(), &all, &id));
                                    }
                                },
                            }
                        }
                    }
                }
            }
            if n == 0 {
                Column {
                    fill_max_width: true,
                    weight: 1.0,
                    alignment: Alignment::Center,
                    arrangement: Arrangement::Center,
                    space_role: SpaceRole::Sm,
                    Label { text: NO_MEMBERS, fs: Fs::Lg, weight: Weight::Medium }
                    Label { text: PICK_FROM_LIST, color: paint(p.muted) }
                }
            } else if layout.paged {
                Row {
                    fill_max_width: true,
                    alignment: Alignment::CenterStart,
                    space_role: SpaceRole::Sm,
                    AppButton { text: "이전", icon: IconRole::Back, enabled: cur > 0, on_click: move |()| page.set(page_step(cur, n, false)) }
                    Label { text: format!("{} / {}", cur + 1, n), color: paint(p.text_2) }
                    AppButton { text: "다음", icon: IconRole::Forward, enabled: cur + 1 < n, on_click: move |()| page.set(page_step(cur, n, true)) }
                }
                Column { fill_max_width: true, weight: 1.0, {pane_of(cur)} }
            } else {
                ScrollColumn {
                    fill_max_width: true,
                    weight: 1.0,
                    for (r, row) in pane_rows(n, layout.per_row).into_iter().enumerate() {
                        Row {
                            key: "row-{r}",
                            fill_max_width: true,
                            space_role: SpaceRole::Sm,
                            for idx in row.clone() {
                                Column { key: "{idx}", weight: 1.0, height: PANE_H, {pane_of(idx)} }
                            }
                            for pad in 0..(layout.per_row - row.len()) {
                                Spacer { key: "pad-{pad}", weight: 1.0 }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The right panel of the single view (environment, context, outputs, changes).
#[component]
fn InfoPanel(session: String) -> Element {
    let ui = use_ui();
    let p = use_ember_theme().palette;
    let computer = use_signal(|| None::<String>);
    use_hook({
        let id = session.clone();
        move || {
            run(
                async move {
                    services()
                        .server
                        .session_computer(&id)
                        .await
                        .map_err(|e| e.to_string())
                },
                move |r: Result<CurrentComputer, String>| {
                    let mut computer = computer;
                    match r {
                        Ok(c) => computer.set(Some(c.computer.name.clone())),
                        Err(e) => tracing::debug!("current computer: {e}"),
                    }
                },
            );
        }
    });
    let (rec, approve_all) = {
        let _ = *ui.live.launcher.read();
        services().client.read(|s| {
            (
                s.session(&session).map(|v| v.record.clone()),
                s.approve_all(&session),
            )
        })
    };
    let rows = rec
        .as_ref()
        .map(|r| environment_rows(r, computer.read().as_deref(), approve_all))
        .unwrap_or_default();
    rsx! {
        ScrollColumn {
            fill_max_width: true,
            fill_max_height: true,
            padding_role: SpaceRole::Sm,
            Column {
                fill_max_width: true,
                space_role: SpaceRole::Sm,
                for card in InfoCard::ALL {
                    EmberCard { key: "{card.label()}", fill_max_width: true,
                        Column {
                            fill_max_width: true,
                            padding_role: SpaceRole::Sm,
                            space_role: SpaceRole::Xs,
                            Label { text: card.label(), weight: Weight::Semibold }
                            if card.ready() {
                                for (k, v) in rows.clone() {
                                    Row {
                                        key: "{k}",
                                        fill_max_width: true,
                                        space_role: SpaceRole::Sm,
                                        Label { text: k, fs: Fs::Sm, color: paint(p.muted) }
                                        Column { weight: 1.0, alignment: Alignment::CenterEnd, Label { text: v, fs: Fs::Sm, max_lines: 2 } }
                                    }
                                }
                            } else {
                                // TODO(server): no per-session context, outputs or changes API.
                                Label { text: SOON, fs: Fs::Sm, color: paint(p.muted) }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One member's conversation with the right panel (inline on expanded, a sheet elsewhere).
#[component]
fn SingleView(session: String, focus: Option<String>) -> Element {
    let window = use_window_size();
    let mut info_open = use_signal(|| false);
    use_leases(std::slice::from_ref(&session), focus.as_deref());
    let wide = window.class == WindowSizeClass::Expanded;
    rsx! {
        Row {
            fill_max_width: true,
            fill_max_height: true,
            Column {
                weight: 1.0,
                fill_max_height: true,
                if !wide {
                    Row {
                        fill_max_width: true,
                        padding_role: SpaceRole::Sm,
                        alignment: Alignment::CenterEnd,
                        AppButton { text: "정보", icon: IconRole::Sidebar, on_click: move |()| info_open.set(true) }
                    }
                }
                ConversationView { key: "{session}", id: session.clone(), embedded: true }
            }
            if wide {
                Column { width: INFO_PANEL_W, fill_max_height: true, InfoPanel { session: session.clone() } }
            }
        }
        if !wide {
            EmberSheet {
                title: "정보",
                open: info_open(),
                on_dismiss: move |()| info_open.set(false),
                Column { fill_max_width: true, height: PANE_H, InfoPanel { session: session.clone() } }
            }
        }
    }
}

/// The kanban of member status, with the PR list and timeline not built yet.
#[component]
fn StatusView(members: Vec<Member>, on_open: EventHandler<String>) -> Element {
    let p = use_ember_theme().palette;
    let columns = kanban(&members);
    let count = |s: LauncherStatus| members.iter().filter(|m| m.status == s).count();
    rsx! {
        ScrollColumn {
            fill_max_width: true,
            fill_max_height: true,
            Column {
                fill_max_width: true,
                padding_role: SpaceRole::Sm,
                space_role: SpaceRole::Md,
                ScrollRow {
                    fill_max_width: true,
                    Pill { text: format!("팀원 {}", members.len()), tone: PillTone::Grey }
                    Spacer { width: SP[0] }
                    Pill { text: format!("실행 중 {}", count(LauncherStatus::Running)), tone: PillTone::Blue }
                    Spacer { width: SP[0] }
                    Pill { text: format!("승인 대기 {}", count(LauncherStatus::WaitingForApproval)), tone: PillTone::Orange }
                    Spacer { width: SP[0] }
                    Pill { text: format!("실패 {}", count(LauncherStatus::Failed)), tone: PillTone::Red }
                }
                ScrollRow {
                    fill_max_width: true,
                    for (col, ms) in columns {
                        Column {
                            key: "{col.label()}",
                            width: 240.0,
                            space_role: SpaceRole::Xs,
                            padding_role: SpaceRole::Xs,
                            Row {
                                fill_max_width: true,
                                alignment: Alignment::CenterStart,
                                space_role: SpaceRole::Sm,
                                Label { text: col.label(), weight: Weight::Semibold }
                                Pill { text: ms.len().to_string(), tone: PillTone::Grey, small: true }
                            }
                            for m in ms {
                                EmberCard { key: "{m.session_id}", fill_max_width: true,
                                    Row {
                                        fill_max_width: true,
                                        padding_role: SpaceRole::Xs,
                                        alignment: Alignment::CenterStart,
                                        space_role: SpaceRole::Xs,
                                        Character { agent: m.agent.clone(), leader: m.leader, size: 28.0, name: m.name.clone() }
                                        Column {
                                            weight: 1.0,
                                            Button {
                                                text: m.accessible_name(),
                                                height: H_BTN,
                                                variant: ButtonVariant::Text,
                                                color: paint(p.text),
                                                fill_max_width: true,
                                                on_click: { let id = m.session_id.clone(); move |()| on_open.call(id.clone()) },
                                            }
                                            Label { text: m.task.clone(), fs: Fs::Xs, color: paint(p.text_2), max_lines: 2 }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                // TODO(server): no PR/CI or recorded-event APIs yet (FR-L18 timeline).
                EmberCard { fill_max_width: true,
                    Column {
                        fill_max_width: true,
                        padding_role: SpaceRole::Sm,
                        space_role: SpaceRole::Xs,
                        Label { text: PR_TITLE, weight: Weight::Semibold }
                        Label { text: SOON, color: paint(p.muted) }
                    }
                }
                EmberCard { fill_max_width: true,
                    Column {
                        fill_max_width: true,
                        padding_role: SpaceRole::Sm,
                        space_role: SpaceRole::Xs,
                        Label { text: TIMELINE_TITLE, weight: Weight::Semibold }
                        Label { text: SOON, color: paint(p.muted) }
                    }
                }
            }
        }
    }
}

/// The 세션 screen. `focus` is the session the route has open, if any.
#[component]
pub fn SessionScreen(#[props(default)] focus: Option<String>) -> Element {
    let ui = use_ui();
    let window = use_window_size();
    let p = use_ember_theme().palette;
    let focus_project = focus.as_ref().and_then(|id| {
        let _ = *ui.live.launcher.read();
        services()
            .client
            .read(|s| s.session(id).map(|v| v.record.project.clone()))
    });
    let mut view = use_signal(|| view_from_saved(ui.prefs.peek().session_view.as_deref()));
    let mut project = use_signal(|| {
        focus_project
            .clone()
            .or_else(|| ui.selected_project.peek().clone())
    });
    let mut query = use_signal(String::new);
    let mut drawer = use_signal(|| false);
    let mut project_menu = use_signal(|| false);
    let mut more_menu = use_signal(|| false);

    let boxes = {
        let _ = *ui.live.launcher.read();
        services().client.read(|s| project_boxes(s))
    };
    let Some(current) = resolve_project(None, project.read().as_deref(), &boxes) else {
        return rsx! {
            Column {
                fill_max_width: true,
                fill_max_height: true,
                alignment: Alignment::Center,
                arrangement: Arrangement::Center,
                space_role: SpaceRole::Sm,
                Label { text: NO_PROJECT, fs: Fs::Xl, weight: Weight::Medium }
                AppButton { text: "새 세션", icon: IconRole::Compose, kind: ButtonKind::Primary, on_click: move |()| ui.open_new_session(None) }
            }
        };
    };
    let members = boxes
        .iter()
        .find(|b| b.id == current)
        .map(|b| b.members.clone())
        .unwrap_or_default();
    let target = single_target(&members, focus.as_deref());
    let sections = side_sections(&boxes, &current, &query.read());
    let mode = side_list_mode(window.class);

    let set_view = use_callback(move |v: SessionView| {
        view.set(v);
        ui.update_prefs(|p| p.session_view = Some(v.key().to_string()));
    });
    // A member picked in the side list or on a card.
    let open_member = use_callback({
        let current = current.clone();
        let members = members.clone();
        move |(proj, id): (String, String)| {
            drawer.set(false);
            let same = proj == current;
            project.set(Some(proj.clone()));
            ui.update_prefs(|p| p.last_project = Some(proj.clone()));
            let now = *view.peek();
            match now {
                // No drag and drop yet: picking from the list adds the pane.
                SessionView::Multi if same => {
                    let all: Vec<String> = members.iter().map(|m| m.session_id.clone()).collect();
                    edit_panes(ui, &proj, move |pp| {
                        add_pane(pp.active_tab_mut(), &all, &id)
                    });
                }
                SessionView::Status => {
                    set_view.call(SessionView::Single);
                    ui.open_conversation(&id);
                }
                _ => ui.open_conversation(&id),
            }
        }
    });
    // Enlarge a pane or open a card: the member's single view.
    let to_single = use_callback({
        let current = current.clone();
        move |id: String| {
            set_view.call(SessionView::Single);
            ui.open_conversation(&id);
            project.set(Some(current.clone()));
        }
    });
    let side = {
        rsx! {
            SideList {
                sections: sections.clone(),
                current: current.clone(),
                active: target.clone(),
                on_query: move |q: String| query.set(q),
                on_open: move |a: (String, String)| open_member.call(a),
            }
        }
    };
    let project_name = display_name(&current);
    let view_now = *view.read();

    rsx! {
        Row {
            fill_max_width: true,
            fill_max_height: true,
            if mode == SideListMode::Column {
                Column {
                    width: SIDE_LIST_W,
                    fill_max_height: true,
                    background: paint(p.surface),
                    {side.clone()}
                }
            }
            Column {
                weight: 1.0,
                fill_max_height: true,
                ScrollRow {
                    fill_max_width: true,
                    padding_role: SpaceRole::Sm,
                    if mode == SideListMode::Drawer {
                        AppButton { text: "목록", icon: IconRole::List, on_click: move |()| drawer.set(true) }
                        Spacer { width: SP[1] }
                    }
                    Menu {
                        expanded: project_menu(),
                        on_dismiss: move |_| project_menu.set(false),
                        anchor: rsx! {
                            AppButton { text: project_name.clone(), icon: IconRole::Library, on_click: move |()| project_menu.set(true) }
                        },
                        for b in boxes.clone() {
                            Button {
                                key: "{b.id}",
                                text: display_name(&b.id),
                                variant: ButtonVariant::Text,
                                height: H_BTN,
                                fill_max_width: true,
                                on_click: {
                                    let id = b.id.clone();
                                    move |_: ()| {
                                        project_menu.set(false);
                                        project.set(Some(id.clone()));
                                        ui.update_prefs(|p| p.last_project = Some(id.clone()));
                                    }
                                },
                            }
                        }
                    }
                    Spacer { width: SP[1] }
                    for v in SessionView::ALL {
                        PickChip { key: "{v.key()}", text: v.label(), selected: view_now == v, on_click: move |()| set_view.call(v) }
                    }
                    Spacer { width: SP[2] }
                    if mode == SideListMode::Column {
                        AppButton { text: "새 팀원", icon: IconRole::Add, on_click: { let c = current.clone(); move |()| ui.open_new_session(Some(c.clone())) } }
                        Spacer { width: SP[1] }
                        // TODO(version control screen): a large sheet for this project (FR-L18).
                        AppButton { text: "버전 관리", on_click: move |()| show_message(VCS_SOON) }
                        Spacer { width: SP[1] }
                        // TODO(server): the ports detected on the member's computer (FR-L18).
                        AppButton { text: "웹 페이지", on_click: move |()| show_message(WEB_SOON) }
                        Spacer { width: SP[1] }
                    } else {
                        // Medium and compact: the less used actions fold into a ⋯ menu so
                        // IDE 열기 stays on screen at 800 px.
                        Menu {
                            expanded: more_menu(),
                            on_dismiss: move |_| more_menu.set(false),
                            anchor: rsx! {
                                AppButton { text: "⋯", on_click: move |()| more_menu.set(true) }
                            },
                            Button {
                                text: "새 팀원",
                                variant: ButtonVariant::Text,
                                height: H_BTN,
                                fill_max_width: true,
                                on_click: { let c = current.clone(); move |_: ()| { more_menu.set(false); ui.open_new_session(Some(c.clone())) } },
                            }
                            Button {
                                text: "버전 관리",
                                variant: ButtonVariant::Text,
                                height: H_BTN,
                                fill_max_width: true,
                                on_click: move |_: ()| { more_menu.set(false); show_message(VCS_SOON) },
                            }
                            Button {
                                text: "웹 페이지",
                                variant: ButtonVariant::Text,
                                height: H_BTN,
                                fill_max_width: true,
                                on_click: move |_: ()| { more_menu.set(false); show_message(WEB_SOON) },
                            }
                        }
                        Spacer { width: SP[1] }
                    }
                    if let Some(t) = target.clone() {
                        IdeMenu { key: "{t}", session: t }
                    } else {
                        AppButton { text: "IDE 열기", icon: IconRole::Forward, enabled: false }
                    }
                }
                Separator { color: paint(p.line) }
                Column {
                    weight: 1.0,
                    fill_max_width: true,
                    match view_now {
                        SessionView::Multi => rsx! {
                            MultiView { key: "{current}", project: current.clone(), members: members.clone(), focus: focus.clone(), on_enlarge: move |id: String| to_single.call(id) }
                        },
                        SessionView::Single => match target.clone() {
                            Some(t) => rsx! { SingleView { session: t, focus: focus.clone() } },
                            None => rsx! {
                                Column {
                                    fill_max_width: true,
                                    fill_max_height: true,
                                    alignment: Alignment::Center,
                                    arrangement: Arrangement::Center,
                                    Label { text: NO_SESSION, color: paint(p.muted) }
                                }
                            },
                        },
                        SessionView::Status => rsx! {
                            StatusView { members: members.clone(), on_open: move |id: String| to_single.call(id) }
                        },
                    }
                }
            }
        }
        if mode == SideListMode::Drawer {
            EmberSheet {
                title: "세션 목록",
                open: drawer(),
                on_dismiss: move |()| drawer.set(false),
                Column { fill_max_width: true, height: PANE_H, {side} }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ide::IdeKind;
    use crate::ui::project::{Member, ProjectBox};
    use dioxus_compose::prelude::WindowSizeClass;
    use ember_client::LauncherStatus;

    fn member(id: &str, status: LauncherStatus, leader: bool, at: i64) -> Member {
        Member {
            session_id: id.into(),
            name: format!("m-{id}"),
            task: format!("task {id}"),
            agent: "claude-code".into(),
            status,
            leader,
            updated_at: at,
        }
    }

    fn project(id: &str, members: Vec<Member>) -> ProjectBox {
        ProjectBox {
            id: id.into(),
            name: id.into(),
            members,
            running: 0,
            waiting: 0,
            computers: vec![],
            top: None,
            repo: None,
            branch: None,
        }
    }

    fn ids(v: &[Member]) -> Vec<&str> {
        v.iter().map(|m| m.session_id.as_str()).collect()
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // ---- FR-L18: views -----------------------------------------------------------------------

    #[test]
    fn fr_l18_view_switcher_has_multi_single_status() {
        let labels: Vec<_> = SessionView::ALL.iter().map(|v| v.label()).collect();
        assert_eq!(labels, ["멀티", "싱글", "상태"]);
        for v in SessionView::ALL {
            assert_eq!(SessionView::from_key(v.key()), Some(v));
        }
        assert_eq!(SessionView::from_key("nope"), None);
    }

    #[test]
    fn fr_l18_unknown_or_missing_saved_view_falls_back_to_multi() {
        assert_eq!(view_from_saved(None), SessionView::Multi);
        assert_eq!(view_from_saved(Some("bogus")), SessionView::Multi);
        assert_eq!(view_from_saved(Some("status")), SessionView::Status);
    }

    #[test]
    fn fr_l18_view_and_pane_tabs_survive_a_restart() {
        use crate::prefs::{Prefs, load, save};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        let mut p = Prefs::default();
        p.session_view = Some(SessionView::Single.key().to_string());
        let mut panes = ProjectPanes::default();
        panes.add_tab(&strings(&["a", "b"]));
        panes.select(1);
        p.panes.insert("ember".into(), panes.clone());
        save(&path, &p).unwrap();
        let back = load(&path);
        assert_eq!(
            view_from_saved(back.session_view.as_deref()),
            SessionView::Single
        );
        assert_eq!(back.panes.get("ember"), Some(&panes));
        // An older prefs file has neither.
        std::fs::write(&path, br#"{"format":1}"#).unwrap();
        let old = load(&path);
        assert!(old.session_view.is_none() && old.panes.is_empty());
    }

    // ---- FR-L18: layout per size class -------------------------------------------------------

    #[test]
    fn fr_l18_panes_two_per_row_on_expanded_one_at_a_time_otherwise() {
        assert_eq!(
            pane_layout(WindowSizeClass::Expanded),
            PaneLayout {
                per_row: 2,
                paged: false
            }
        );
        assert_eq!(
            pane_layout(WindowSizeClass::Medium),
            PaneLayout {
                per_row: 1,
                paged: true
            }
        );
        assert_eq!(
            pane_layout(WindowSizeClass::Compact),
            PaneLayout {
                per_row: 1,
                paged: true
            }
        );
    }

    #[test]
    fn fr_l18_pane_rows_chunk_by_columns() {
        assert_eq!(pane_rows(5, 2), vec![vec![0, 1], vec![2, 3], vec![4]]);
        assert_eq!(pane_rows(0, 2), Vec::<Vec<usize>>::new());
        assert_eq!(pane_rows(3, 0), vec![vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn fr_l18_paging_stops_at_both_ends() {
        assert_eq!(page_step(0, 3, false), 0);
        assert_eq!(page_step(0, 3, true), 1);
        assert_eq!(page_step(2, 3, true), 2);
        assert_eq!(
            page_step(5, 3, false),
            1,
            "an out-of-range page is clamped first"
        );
        assert_eq!(page_step(0, 0, true), 0);
    }

    #[test]
    fn fr_l18_side_list_is_a_column_on_expanded_and_a_drawer_elsewhere() {
        assert_eq!(
            side_list_mode(WindowSizeClass::Expanded),
            SideListMode::Column
        );
        assert_eq!(
            side_list_mode(WindowSizeClass::Medium),
            SideListMode::Drawer
        );
        assert_eq!(
            side_list_mode(WindowSizeClass::Compact),
            SideListMode::Drawer
        );
    }

    // ---- FR-L18, FR-L24: member grouping and order -------------------------------------------

    #[test]
    fn fr_l18_side_list_groups_by_project_current_first_leader_first() {
        let boxes = vec![
            project("a", vec![member("a1", LauncherStatus::Running, false, 5)]),
            project(
                "b",
                vec![
                    member("b1", LauncherStatus::WaitingForApproval, false, 9),
                    member("b2", LauncherStatus::Idle, true, 1),
                ],
            ),
        ];
        let s = side_sections(&boxes, "b", "");
        assert_eq!(
            s.iter().map(|x| x.project.as_str()).collect::<Vec<_>>(),
            ["b", "a"]
        );
        // The leader comes first even though another member is waiting for approval.
        assert_eq!(ids(&s[0].members), ["b2", "b1"]);
        assert!(s[0].members[0].leader);
    }

    #[test]
    fn fr_l24_default_project_has_no_leader_first_rule_to_apply() {
        let boxes = vec![project(
            "unsorted",
            vec![
                member("u1", LauncherStatus::Idle, false, 1),
                member("u2", LauncherStatus::Running, false, 2),
            ],
        )];
        let s = side_sections(&boxes, "unsorted", "");
        assert_eq!(s[0].name, "기본 프로젝트");
        assert_eq!(ids(&s[0].members), ["u1", "u2"], "the given order is kept");
    }

    #[test]
    fn fr_l18_session_search_matches_member_task_harness_or_project() {
        let boxes = vec![
            project("alpha", vec![member("a1", LauncherStatus::Idle, false, 1)]),
            project(
                "beta",
                vec![
                    member("b1", LauncherStatus::Idle, false, 1),
                    member("b2", LauncherStatus::Idle, false, 2),
                ],
            ),
        ];
        let by_task = side_sections(&boxes, "alpha", "task b2");
        assert_eq!(by_task.len(), 1);
        assert_eq!(ids(&by_task[0].members), ["b2"]);
        // A project name match keeps the whole section.
        let by_project = side_sections(&boxes, "alpha", "BETA");
        assert_eq!(by_project.len(), 1);
        assert_eq!(by_project[0].members.len(), 2);
        assert!(side_sections(&boxes, "alpha", "zzz").is_empty());
    }

    #[test]
    fn fr_l18_project_resolves_from_focus_then_choice_then_first() {
        let boxes = vec![project("a", vec![]), project("b", vec![])];
        assert_eq!(
            resolve_project(Some("b"), Some("a"), &boxes).as_deref(),
            Some("b")
        );
        assert_eq!(
            resolve_project(None, Some("b"), &boxes).as_deref(),
            Some("b")
        );
        assert_eq!(
            resolve_project(None, Some("gone"), &boxes).as_deref(),
            Some("a")
        );
        assert_eq!(resolve_project(None, None, &boxes).as_deref(), Some("a"));
        assert_eq!(resolve_project(None, None, &[]), None);
    }

    #[test]
    fn fr_l18_single_view_target_is_focus_then_leader_then_first() {
        let ms = vec![
            member("x", LauncherStatus::Idle, false, 1),
            member("l", LauncherStatus::Idle, true, 1),
        ];
        assert_eq!(single_target(&ms, Some("x")).as_deref(), Some("x"));
        assert_eq!(single_target(&ms, Some("elsewhere")).as_deref(), Some("l"));
        assert_eq!(single_target(&ms, None).as_deref(), Some("l"));
        assert_eq!(single_target(&ms[..1], None).as_deref(), Some("x"));
        assert_eq!(single_target(&[], None), None);
    }

    // ---- FR-L18: kanban ----------------------------------------------------------------------

    #[test]
    fn fr_l18_kanban_columns_in_design_order() {
        let labels: Vec<_> = KanbanColumn::ALL.iter().map(|c| c.label()).collect();
        assert_eq!(labels, ["대기", "실행", "승인 대기", "검토", "완료"]);
    }

    #[test]
    fn fr_l18_kanban_buckets_every_status() {
        use KanbanColumn::*;
        use LauncherStatus as S;
        assert_eq!(kanban_column(S::Idle), Waiting);
        assert_eq!(kanban_column(S::Unknown), Waiting);
        assert_eq!(kanban_column(S::Running), Running);
        assert_eq!(kanban_column(S::WaitingForApproval), Approval);
        assert_eq!(kanban_column(S::FinishedUnread), Review);
        assert_eq!(kanban_column(S::Failed), Review);
        assert_eq!(kanban_column(S::Finished), Done);
    }

    #[test]
    fn fr_l18_kanban_always_has_five_columns_and_keeps_member_order() {
        let ms = vec![
            member("1", LauncherStatus::Running, false, 3),
            member("2", LauncherStatus::Running, false, 2),
            member("3", LauncherStatus::Finished, false, 1),
        ];
        let k = kanban(&ms);
        assert_eq!(k.len(), 5);
        assert_eq!(k[1].0, KanbanColumn::Running);
        assert_eq!(ids(&k[1].1), ["1", "2"]);
        assert_eq!(ids(&k[4].1), ["3"]);
        assert!(k[0].1.is_empty() && k[2].1.is_empty() && k[3].1.is_empty());
    }

    // ---- FR-L7: IDE chooser ------------------------------------------------------------------

    #[test]
    fn fr_l7_ide_chooser_lists_exactly_three() {
        let c = ide_choices(None);
        let labels: Vec<_> = c.iter().map(|x| x.label).collect();
        assert_eq!(labels, ["Ember 에디터", "VS Code", "JetBrains Gateway"]);
        for l in labels {
            for banned in ["Cursor", "Zed", "SSH"] {
                assert!(!l.contains(banned));
            }
        }
    }

    #[test]
    fn fr_l7_remembered_ide_is_listed_first_and_nothing_is_added() {
        let c = ide_choices(Some(IdeKind::Gateway));
        assert_eq!(c.len(), 3);
        assert_eq!(c[0].kind, IdeKind::Gateway);
        let kinds: Vec<_> = c.iter().map(|x| x.kind).collect();
        assert!(kinds.contains(&IdeKind::Ember) && kinds.contains(&IdeKind::VsCode));
    }

    // ---- FR-L18: panes, tabs and the reorder menu --------------------------------------------

    #[test]
    fn fr_l18_move_pane_swaps_with_its_neighbour_and_stops_at_the_ends() {
        let mut o = strings(&["a", "b", "c"]);
        assert!(move_pane(&mut o, "b", PaneMove::Left));
        assert_eq!(o, ["b", "a", "c"]);
        assert!(!move_pane(&mut o, "b", PaneMove::Left), "already first");
        assert!(move_pane(&mut o, "a", PaneMove::Right));
        assert_eq!(o, ["b", "c", "a"]);
        assert!(!move_pane(&mut o, "a", PaneMove::Right), "already last");
        assert!(!move_pane(&mut o, "zz", PaneMove::Left), "unknown pane");
    }

    #[test]
    fn fr_l18_move_availability_matches_position() {
        let o = strings(&["a", "b", "c"]);
        assert_eq!(
            (
                can_move(&o, "a", PaneMove::Left),
                can_move(&o, "a", PaneMove::Right)
            ),
            (false, true)
        );
        assert_eq!(
            (
                can_move(&o, "c", PaneMove::Left),
                can_move(&o, "c", PaneMove::Right)
            ),
            (true, false)
        );
    }

    #[test]
    fn fr_l18_new_tab_shows_all_members_until_edited() {
        let t = PaneTab::all("탭");
        let all = strings(&["a", "b"]);
        assert_eq!(visible_panes(&t, &all), all);
    }

    #[test]
    fn fr_l18_closing_a_pane_in_an_automatic_tab_pins_the_rest() {
        let all = strings(&["a", "b", "c"]);
        let mut t = PaneTab::all("탭");
        close_pane(&mut t, &all, "b");
        assert_eq!(visible_panes(&t, &all), ["a", "c"]);
        // A member created later is not forced into a tab that was edited.
        let later = strings(&["a", "b", "c", "d"]);
        assert_eq!(visible_panes(&t, &later), ["a", "c"]);
    }

    #[test]
    fn fr_l18_adding_from_the_side_list_appends_once() {
        let all = strings(&["a", "b", "c"]);
        let mut t = PaneTab {
            name: "탭".into(),
            members: Some(strings(&["a"])),
        };
        add_pane(&mut t, &all, "c");
        add_pane(&mut t, &all, "c");
        add_pane(&mut t, &all, "gone");
        assert_eq!(visible_panes(&t, &all), ["a", "c"]);
    }

    #[test]
    fn fr_l18_removed_members_drop_out_of_a_saved_tab() {
        let t = PaneTab {
            name: "탭".into(),
            members: Some(strings(&["a", "x", "b"])),
        };
        assert_eq!(visible_panes(&t, &strings(&["a", "b"])), ["a", "b"]);
    }

    #[test]
    fn fr_l18_reordering_an_automatic_tab_saves_the_order() {
        let all = strings(&["a", "b", "c"]);
        let mut t = PaneTab::all("탭");
        assert!(reorder_pane(&mut t, &all, "c", PaneMove::Left));
        assert_eq!(visible_panes(&t, &all), ["a", "c", "b"]);
    }

    #[test]
    fn fr_l18_tabs_add_select_remove_and_always_keep_one() {
        let mut p = ProjectPanes::default();
        assert_eq!(p.tabs.len(), 1);
        assert_eq!(p.active, 0);
        p.add_tab(&strings(&["a"]));
        assert_eq!((p.tabs.len(), p.active), (2, 1), "a new tab is selected");
        assert_eq!(
            p.tabs[1].members,
            Some(strings(&["a"])),
            "it saves the panes shown now"
        );
        p.select(9);
        assert_eq!(p.active, 1, "out of range is ignored");
        p.select(0);
        p.remove_tab(0);
        assert_eq!(p.tabs.len(), 1);
        p.remove_tab(0);
        assert_eq!(p.tabs.len(), 1, "the last tab stays");
        assert!(p.active < p.tabs.len());
    }

    #[test]
    fn fr_l18_removing_the_active_tab_selects_a_valid_one() {
        let mut p = ProjectPanes::default();
        p.add_tab(&[]);
        p.add_tab(&[]);
        p.select(2);
        p.remove_tab(2);
        assert_eq!(p.active, 1);
        assert_eq!(p.active_tab().name, p.tabs[1].name);
    }

    #[test]
    fn fr_l18_tab_names_are_numbered_and_unique() {
        let mut p = ProjectPanes::default();
        p.add_tab(&[]);
        p.add_tab(&[]);
        let mut names: Vec<_> = p.tabs.iter().map(|t| t.name.clone()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 3);
    }

    // ---- FR-L5: leases of the panes ----------------------------------------------------------

    #[test]
    fn fr_l5_panes_open_what_is_missing_and_close_only_their_own() {
        let mine: std::collections::HashSet<String> =
            strings(&["old", "kept"]).into_iter().collect();
        let wanted = strings(&["kept", "new", "focus"]);
        let (open, close) = lease_plan(
            &wanted,
            |id| id == "kept" || id == "focus",
            &mine,
            Some("focus"),
        );
        assert_eq!(open, ["new"]);
        assert_eq!(close, ["old"]);
        // The route's own session is never closed by a pane.
        let mine2: std::collections::HashSet<String> = strings(&["focus"]).into_iter().collect();
        let (_, close2) = lease_plan(&[], |_| true, &mine2, Some("focus"));
        assert!(close2.is_empty());
    }

    // ---- FR-L5, FR-L6, FR-L15, FR-L16: composer chips and the pane header -------------------

    #[test]
    fn fr_l15_model_chip_reads_harness_and_model() {
        assert_eq!(
            model_chip_label("claude-code", Some("opus")),
            "Claude Code · Opus 5.5"
        );
        assert_eq!(model_chip_label("codex", None), "Codex · GPT-5");
        assert_eq!(model_chip_label("codex", Some("")), "Codex · GPT-5");
        assert_eq!(model_chip_label("codex", Some("custom")), "Codex · custom");
        assert_eq!(model_chip_label("omp", None), "omp · 기본 모델");
    }

    #[test]
    fn fr_l16_mode_chip_follows_approve_everything() {
        assert_eq!(mode_chip_label(false), PermMode::Ask.label());
        assert_eq!(mode_chip_label(true), PermMode::Bypass.label());
        assert!(
            mode_chip_warns(true) && !mode_chip_warns(false),
            "bypass is drawn in the warning colour"
        );
    }

    #[test]
    fn fr_l16_mode_rows_follow_the_harness_and_only_ask_and_bypass_can_be_set() {
        let claude = mode_rows("claude-code");
        assert_eq!(claude.len(), 4);
        let codex = mode_rows("codex");
        assert_eq!(codex.len(), 3);
        assert!(!codex.iter().any(|(m, _)| *m == PermMode::Plan));
        for (m, settable) in claude {
            assert_eq!(settable, matches!(m, PermMode::Ask | PermMode::Bypass));
        }
        assert_eq!(mode_rows("omp").len(), 2);
    }

    #[test]
    fn fr_l18_pane_toggle_has_conversation_and_terminal() {
        let l: Vec<_> = PaneToggle::ALL.iter().map(|t| t.label()).collect();
        assert_eq!(l, ["대화", "터미널"]);
    }

    #[test]
    fn fr_l5_right_panel_has_four_cards_and_only_environment_has_data() {
        let l: Vec<_> = InfoCard::ALL.iter().map(|c| c.label()).collect();
        assert_eq!(l, ["환경", "컨텍스트", "결과물", "변경 사항"]);
        assert!(InfoCard::Environment.ready());
        assert!(
            !InfoCard::Context.ready() && !InfoCard::Outputs.ready() && !InfoCard::Changes.ready()
        );
    }

    #[test]
    fn fr_l5_environment_rows_come_from_the_record() {
        let rec: ember_client::wire::SessionRecord = serde_json::from_value(serde_json::json!({
            "id": "s1", "project": "ember", "agent": "codex", "cwd": "/w/ember",
            "model": "o3", "status": "idle", "title": "t",
            "created_at": 1, "updated_at": 2, "last_seq": 3
        }))
        .unwrap();
        let rows = environment_rows(&rec, Some("mac-mini"), true);
        let get = |k: &str| rows.iter().find(|(a, _)| *a == k).map(|(_, v)| v.clone());
        assert_eq!(get("컴퓨터").as_deref(), Some("mac-mini"));
        assert_eq!(get("하네스").as_deref(), Some("codex"));
        assert_eq!(get("모델").as_deref(), Some("o3"));
        assert_eq!(get("권한 모드").as_deref(), Some(PermMode::Bypass.label()));
        assert_eq!(get("작업 폴더").as_deref(), Some("/w/ember"));
        assert_eq!(get("저장소").as_deref(), Some("준비 중입니다"));
        let none = environment_rows(&rec, None, false);
        assert_eq!(
            none.iter().find(|(a, _)| *a == "컴퓨터").unwrap().1,
            "확인하는 중입니다"
        );
    }

    // ---- NFR-L4 ------------------------------------------------------------------------------

    #[test]
    fn nfr_l4_session_text_has_no_plain_da_endings() {
        for s in UI_STRINGS {
            let t = s.trim();
            assert!(
                !t.ends_with('다')
                    || t.ends_with("입니다")
                    || t.ends_with("합니다")
                    || t.ends_with("습니다")
                    || t.ends_with("됩니다"),
                "{t}"
            );
            assert!(!t.contains('\u{2014}'), "no em-dash: {t}");
        }
        assert!(UI_STRINGS.contains(&"준비 중입니다"));
    }
}
