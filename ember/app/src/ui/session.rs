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

use std::collections::HashSet;

use dioxus_compose::prelude::WindowSizeClass;
use serde::{Deserialize, Serialize};

use ember_client::LauncherStatus;
use ember_client::wire::SessionRecord;

use crate::ide::IdeKind;
use crate::ui::home::{Harness, PermMode, default_option, model_catalog, modes_for};
use crate::ui::project::{
    Member, MemberFilter, ProjectBox, display_name, filter_members,
};
use crate::ui::theme::{RAIL_W, SIDE_W};

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
const WEB_LOADING: &str = "불러오는 중입니다";
const WEB_EMPTY: &str = "열려 있는 웹 포트가 없습니다";
const WEB_UNSUPPORTED: &str = "이 컴퓨터는 포트 목록을 지원하지 않습니다";
const WEB_LOOPBACK_ONLY: &str = "이 컴퓨터 안에서만 열 수 있습니다";
const WEB_NO_HOST: &str = "주소를 알 수 없어 열 수 없습니다";
const COMPUTER_LOADING: &str = "확인하는 중입니다";
const NO_SESSION: &str = "세션이 없습니다";
const PR_TITLE: &str = "PR과 CI";
const SUBAGENT_TITLE: &str = "서브 에이전트 스레드";
const LOAD_TITLE: &str = "부하";
const NONE_YET: &str = "아직 없습니다";
const WAIT_SERVER: &str = "서버 데이터를 기다리고 있습니다";
const TIMELINE_TITLE: &str = "에이전트 소통 타임라인";

/// Every sentence this screen shows, checked for plain "~다" endings (NFR-L4).
/// The summary line of the agent-to-agent timeline card. The figures are demo data; a real
/// session has no duration comparison and no cost (FR-U7: cost is unknown, never invented).
pub fn timeline_summary(demo: bool) -> &'static str {
    if demo {
        "5명 · 16.4s · 순차 실행보다 2.4배 빠름 · $0.24"
    } else {
        "비용은 알 수 없습니다"
    }
}

pub const UI_STRINGS: &[&str] = &[
    crate::ui::home::INTEGRATIONS_TITLE,
    crate::ui::enrol_flow::NAME_HELP,
    crate::ui::enrol_flow::STDIN_NOTE,
    crate::ui::enrol_flow::WAITING_LINE,
    crate::ui::enrol_flow::CODE_ONCE,
    crate::ui::enrol_flow::DOOR_OFF,
    crate::ui::enrol_flow::DOOR_URL_UNKNOWN,
    crate::ui::enrol_flow::NO_COMMAND,
    crate::ui::enrol_flow::HTTP_WARNING,
    crate::ui::enrol_flow::CONFIRM_QUESTION,
    crate::ui::enrol_flow::CONFIRM_CHECK,
    crate::ui::enrol_flow::CONFIRMED_WAIT,
    crate::ui::enrol_flow::JOINED_LINE,
    crate::ui::enrol_flow::JOINED_NEXT,
    crate::ui::enrol_flow::NODE_CMD_NOTE,
    crate::ui::enrol_flow::ZSH_QUOTE_NOTE,
    crate::ui::enrol_flow::DENIED_LINE,
    crate::ui::enrol_flow::EXPIRED_LINE,
    crate::ui::enrol_flow::GONE_LINE,
    crate::ui::enrol_flow::DEMO_LINE,
    crate::ui::enrol_flow::WARN_ADDR_DIFFERS,
    crate::ui::enrol_flow::WARN_NAME_DIFFERS,
    crate::ui::enrol_flow::WARN_PUBLIC_ADDR,
    crate::ui::enrol_flow::NOTE_PROPOSED,
    crate::ui::enrol_flow::WARN_INFORMS,
    crate::ui::enrol_flow::PEER_ID_UNUSED,
    crate::ui::enrol_flow::PEER_ONLY_NOTE,
    crate::ui::enrol_flow::TYPED_FINGERPRINT_HELP,
    crate::ui::enrol_flow::FINGERPRINT_MISMATCH,
    crate::ui::enrol_flow::UNKNOWN_STATE,
    crate::ui::add_computer::ENROL_NOTE,
    crate::ui::home::GITHUB_NO_INFO,
    crate::ui::home::GITHUB_UNKNOWN,
    crate::ui::home::GITHUB_SAMPLE_MARK,
    crate::ui::home::GITHUB_NO_ACTIVITY,
    crate::ui::home::GITHUB_LOGIN_HINT,
    crate::ui::home::GITHUB_UNAVAILABLE_HINT,
    crate::ui::home::GITHUB_INSTALL_HINT,
    crate::ui::home::GITHUB_HOW,
    crate::ui::home::LATER_PILL,
    crate::ui::home::LATER_LINE,
    crate::ui::home::ATTACH_UNSUPPORTED,
    SOON,
    NO_PROJECT,
    NO_MEMBERS,
    PICK_FROM_LIST,
    NO_CONVERSATION,
    TERMINAL_SOON,
    MODEL_LOCKED,
    WEB_LOADING,
    WEB_EMPTY,
    WEB_UNSUPPORTED,
    WEB_LOOPBACK_ONLY,
    WEB_NO_HOST,
    COMPUTER_LOADING,
    NO_SESSION,
    NONE_YET,
    WAIT_SERVER,
];

// ---------------------------------------------------------------------------------------------
// Web pages (FR-L18, FR-P7, #326)
// ---------------------------------------------------------------------------------------------

/// Where a session's computer can be reached from this app: `local` when its loopback is this
/// machine's loopback, otherwise the `host` its ports are served on (None when unknown).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ComputerHost {
    pub local: bool,
    pub host: Option<String>,
}

/// One row of the 웹 페이지 list. `url` is None when the page cannot be opened from here, and
/// `note` then says why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebPage {
    pub label: String,
    pub url: Option<String>,
    pub note: Option<&'static str>,
}

/// The state of the 웹 페이지 list.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum WebPagesLoad {
    #[default]
    Loading,
    Ready(Vec<WebPage>),
    Empty,
    Unsupported,
    Failed(String),
}

/// The host part of an `http(s)://host[:port][/path]` address; None for any other form
/// (a peer address, an empty string).
fn url_host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://"))?;
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    let auth = auth.rsplit('@').next().unwrap_or("");
    let host = if let Some(v6) = auth.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        auth.split(':').next().unwrap_or("")
    };
    (!host.is_empty()).then(|| host.to_string())
}

fn is_loopback_host(host: &str) -> bool {
    host == "localhost" || host == "::1" || host.starts_with("127.")
}

/// How to reach the session's computer. A local computer of a server on another machine is
/// not this machine: its loopback ports stay closed and its LAN ports use the server's host.
/// A remote computer uses the host of its own `url` (empty or a peer address: unknown).
pub fn computer_host(local: bool, computer_url: &str, server_url: &str) -> ComputerHost {
    if local {
        return match url_host(server_url) {
            Some(h) if !is_loopback_host(&h) => ComputerHost { local: false, host: Some(h) },
            _ => ComputerHost { local: true, host: None },
        };
    }
    ComputerHost { local: false, host: url_host(computer_url) }
}

fn web_host_text(host: &str) -> String {
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

/// The 웹 페이지 list from the node's listening ports: sorted by port, one row per
/// (port, address). A loopback port opens only on the local computer.
pub fn web_pages_from(
    result: Result<ember_client::api::Ports, String>,
    computer: &ComputerHost,
) -> WebPagesLoad {
    use ember_client::api::PortScope;
    let ports = match result {
        Ok(p) => p,
        Err(e) => return WebPagesLoad::Failed(format!("포트 목록을 불러오지 못했습니다: {e}")),
    };
    if !ports.supported {
        return WebPagesLoad::Unsupported;
    }
    let mut rows: Vec<(u16, WebPage)> = Vec::new();
    for lp in &ports.ports {
        let port = lp.port;
        let (url, note) = match lp.scope {
            PortScope::Loopback if computer.local => (Some(format!("http://127.0.0.1:{port}")), None),
            PortScope::Loopback => (None, Some(WEB_LOOPBACK_ONLY)),
            _ => match (&computer.host, computer.local) {
                (Some(h), _) => (Some(format!("http://{}:{port}", web_host_text(h))), None),
                (None, true) => (Some(format!("http://127.0.0.1:{port}")), None),
                (None, false) => (None, Some(WEB_NO_HOST)),
            },
        };
        let shown = url.clone().unwrap_or_else(|| format!(":{port}"));
        let label = if lp.process.is_empty() { shown } else { format!("{shown} ({})", lp.process) };
        rows.push((port, WebPage { label, url, note }));
    }
    rows.sort_by(|a, b| (a.0, &a.1.url, &a.1.label).cmp(&(b.0, &b.1.url, &b.1.label)));
    rows.dedup_by(|b, a| a.0 == b.0 && a.1.url == b.1.url);
    if rows.is_empty() {
        return WebPagesLoad::Empty;
    }
    WebPagesLoad::Ready(rows.into_iter().map(|(_, w)| w).collect())
}

/// The OptionSheet lines of a load state: one per page, or the single state line.
pub fn web_menu_lines(load: &WebPagesLoad) -> Vec<String> {
    match load {
        WebPagesLoad::Loading => vec![WEB_LOADING.to_string()],
        WebPagesLoad::Ready(pages) => pages.iter().map(|w| w.label.clone()).collect(),
        WebPagesLoad::Empty => vec![WEB_EMPTY.to_string()],
        WebPagesLoad::Unsupported => vec![WEB_UNSUPPORTED.to_string()],
        WebPagesLoad::Failed(e) => vec![e.clone()],
    }
}

/// What a tap on line `idx` does: open a url, or post a text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebTap {
    Open(String),
    Say(String),
    Nothing,
}

pub fn web_tap(load: &WebPagesLoad, idx: usize) -> WebTap {
    match load {
        WebPagesLoad::Ready(pages) => match pages.get(idx) {
            Some(WebPage { url: Some(u), .. }) => WebTap::Open(u.clone()),
            Some(WebPage { note: Some(n), .. }) => WebTap::Say((*n).to_string()),
            _ => WebTap::Nothing,
        },
        WebPagesLoad::Loading => WebTap::Nothing,
        other => web_menu_lines(other).into_iter().next().map(WebTap::Say).unwrap_or(WebTap::Nothing),
    }
}

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

/// The width the session toolbar really has: the window minus the navigation column (the full
/// sidebar from 1280 dp, the rail below), minus the side list when it is a column, and minus
/// the 목록 button when the list is a drawer. Folding on the window width instead made IDE 열기
/// run off the right edge at 1200 dp (#195).
pub fn toolbar_width(window_dp: f32, list_is_column: bool) -> f32 {
    let nav = if window_dp >= 1280.0 { SIDE_W } else { RAIL_W };
    let list = if list_is_column { SIDE_LIST_W } else { 90.0 };
    window_dp - nav - list
}

/// How many of 웹 페이지, 버전 관리 and 새 팀원 (in that order) fold into ⋯ at a toolbar width:
/// 0 shows all three, 1 folds 웹 페이지, 2 also 버전 관리, 3 also 새 팀원. The widths are the
/// space the project, the view chips, the buttons and IDE 열기 need (about 945 dp for all).
pub fn toolbar_fold_level(toolbar_dp: f32) -> usize {
    if toolbar_dp >= 980.0 {
        0
    } else if toolbar_dp >= 900.0 {
        1
    } else if toolbar_dp >= 790.0 {
        2
    } else {
        3
    }
}

/// Width of the left session drawer (mockup `.sside`, #582): `min(300, 86% of the window)` at
/// phone width, 290 between 561 and 900 where the 64 px rail stays and the drawer opens beside it (the list column's place).
pub fn drawer_width(window_dp: f32) -> f32 {
    // The window size reads 0 until the renderer reports it: the widest phone drawer then.
    if window_dp <= 0.0 {
        300.0
    } else if window_dp > 560.0 {
        290.0
    } else {
        (window_dp * 0.86).min(300.0)
    }
}

/// Whether the session list opens as an overlay drawer rather than sitting as a column (the
/// column shows only above 900 dp, FR-L18).
pub fn drawer_is_overlay(window_dp: f32) -> bool {
    window_dp <= 900.0
}

/// Whether the drawer is open for the overlay decision: it counts only while the list is not a
/// column, so widening past 900 dp and shrinking again does not reopen it (FR-L18).
pub fn drawer_open_effective(show_column: bool, drawer_open: bool) -> bool {
    !show_column && drawer_open
}

/// Whether the 멀티/싱글/상태 chips move to their own row: at the bottom-bar width (mockup
/// `max-width:560px`) the toolbar has no room for them beside the project and IDE 열기 (#218).
pub fn view_chips_below(window_dp: f32) -> bool {
    window_dp <= crate::ui::shell::BAR_MAX_DP
}

/// Whether the 멀티/싱글/상태 chips show only their icons (mockup `.viewseg`: icons below the
/// desktop width, including the full-width row at phone width, FR-L18, #512).
pub fn view_seg_icon_only(window_dp: f32) -> bool {
    window_dp < 1280.0
}

/// 264 dp at 1440 in the design.
pub const SIDE_LIST_W: f32 = 264.0;
/// A pane's height in the grid (a scroll column cannot give a list a bounded height).
pub const PANE_H: f32 = 560.0;
/// The right panel of the single view.
pub const INFO_PANEL_W: f32 = 312.0;

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

    /// The view the pane header's single icon button switches to (`.ph-mode`, FR-L18).
    pub fn other(self) -> PaneToggle {
        match self {
            PaneToggle::Conversation => PaneToggle::Terminal,
            PaneToggle::Terminal => PaneToggle::Conversation,
        }
    }

    /// Label of that icon button: it names the view it opens, not the current one.
    pub fn switch_label(self) -> &'static str {
        match self {
            PaneToggle::Conversation => "터미널로 보기",
            PaneToggle::Terminal => "대화로 보기",
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
// Status view cards (FR-L18, #324, #349)
// ---------------------------------------------------------------------------------------------

/// The 오늘 토큰 tile: (value, sub line), today's tokens of the project on the UTC day (FR-U7,
/// #332). Loading, failed and nothing-reported each say so; the cost is unknown, so no dollar
/// figure is shown. In demo mode the load holds sample numbers from `demo.rs`.
pub fn token_tile(load: &crate::ui::usage::Load<ember_client::api::UsageSummary>, today: &str) -> (String, String) {
    let t = crate::ui::usage::today_tile(load, today);
    (t.value, t.sub)
}

/// Card titles and empty states, in grid order.
pub const PR_CARD_TITLE: &str = PR_TITLE;
pub const SUBAGENT_CARD_TITLE: &str = SUBAGENT_TITLE;
pub const LOAD_CARD_TITLE: &str = LOAD_TITLE;
pub const PR_EMPTY: &str = WAIT_SERVER;
pub const SUBAGENT_EMPTY: &str = "서브 에이전트가 없습니다";
pub const TOKENS_CAPTION: &str = "팀원별 오늘 토큰";
pub const COMPUTERS_CAPTION: &str = "컴퓨터";
pub const TOKENS_EMPTY: &str = NONE_YET;
pub const COMPUTERS_EMPTY: &str = "컴퓨터가 없습니다";

/// The CI state of a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiState {
    Pass,
    Fail,
    Run,
}

/// The colour family of a pill, without the toolkit type so the model stays pure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusTone {
    Green,
    Red,
    Blue,
    Orange,
    Sky,
}

impl CiState {
    pub fn label(self) -> &'static str {
        match self {
            CiState::Pass => "CI 통과",
            CiState::Fail => "CI 실패",
            CiState::Run => "CI 실행 중",
        }
    }

    pub fn tone(self) -> StatusTone {
        match self {
            CiState::Pass => StatusTone::Green,
            CiState::Fail => StatusTone::Red,
            CiState::Run => StatusTone::Blue,
        }
    }
}

/// One pull request row of the PR과 CI card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrRow {
    pub num: u32,
    pub title: String,
    pub member: Option<String>,
    pub branch: String,
    pub ci: CiState,
    pub review: String,
}

impl PrRow {
    /// `#num title`.
    pub fn headline(&self) -> String {
        format!("#{} {}", self.num, self.title)
    }

    /// `<member> · <branch>`, or the branch alone.
    pub fn sub_line(&self) -> String {
        match &self.member {
            Some(m) => format!("{m} \u{00b7} {}", self.branch),
            None => self.branch.clone(),
        }
    }
}

/// PR rows. No PR API exists yet (TODO(server): PR list with CI state, #324), so only the demo
/// shows sample rows.
pub fn pr_rows(demo: bool) -> Vec<PrRow> {
    if !demo {
        return Vec::new();
    }
    vec![
        PrRow {
            num: 212,
            title: "센터 시트와 스크림".into(),
            member: Some("구현".into()),
            branch: "feat/sheets".into(),
            ci: CiState::Pass,
            review: "승인".into(),
        },
        PrRow {
            num: 273,
            title: "모니터 폭 1360 dp".into(),
            member: Some("리뷰".into()),
            branch: "feat/monitor-width".into(),
            ci: CiState::Run,
            review: "리뷰 대기".into(),
        },
    ]
}

/// The count pill text of the PR card header.
pub fn pr_count_text(rows: &[PrRow]) -> String {
    rows.len().to_string()
}

/// The state of a sub-agent thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeStatus {
    Run,
    Wait,
    Done,
    Fail,
    Unknown,
}

impl NodeStatus {
    /// The colour of the status marker.
    pub fn dot_tone(self) -> Option<StatusTone> {
        match self {
            NodeStatus::Run => Some(StatusTone::Blue),
            NodeStatus::Wait => Some(StatusTone::Orange),
            NodeStatus::Done => Some(StatusTone::Green),
            NodeStatus::Fail => Some(StatusTone::Red),
            NodeStatus::Unknown => None,
        }
    }

    /// The status as text, so the marker's colour is not the only carrier (FR-L18).
    pub fn label(self) -> &'static str {
        match self {
            NodeStatus::Run => "실행 중",
            NodeStatus::Wait => "대기",
            NodeStatus::Done => "완료",
            NodeStatus::Fail => "실패",
            NodeStatus::Unknown => "",
        }
    }
}

/// Side padding of the status page: 12 dp up to 560 dp, 20 dp above.
pub fn status_page_pad(width_dp: f32) -> f32 {
    if width_dp <= 560.0 { 12.0 } else { 20.0 }
}

/// Columns of the status cards grid: three from 1280 dp, two from 900 dp, one below.
pub fn status_grid_columns(width_dp: f32) -> usize {
    if width_dp >= 1280.0 {
        3
    } else if width_dp >= 900.0 {
        2
    } else {
        1
    }
}

/// Width of one status card when the grid has `columns` columns and `gap` between them: the
/// whole page row (window minus both paddings) shared equally, as the stat tiles do.
pub fn status_card_width(window_dp: f32, columns: usize, gap: f32) -> f32 {
    let row = window_dp - 2.0 * status_page_pad(window_dp);
    let n = columns.max(1) as f32;
    (row - gap * (n - 1.0)) / n
}

/// One node of the sub-agent tree: a team member that has threads (`member` set), or a thread.
#[derive(Debug, Clone, PartialEq)]
pub struct SubNode {
    pub name: String,
    pub task: String,
    pub status: NodeStatus,
    pub member: Option<Member>,
    pub children: Vec<SubNode>,
}

impl SubNode {
    pub fn depth(&self) -> usize {
        1 + self.children.iter().map(SubNode::depth).max().unwrap_or(0)
    }
}

fn thread(name: &str, task: &str, status: NodeStatus, children: Vec<SubNode>) -> SubNode {
    SubNode { name: name.into(), task: task.into(), status, member: None, children }
}

/// The sub-agent tree. The server reports no sub-agent threads yet (TODO(server): FR-U7, #324);
/// members are not threads, so outside the demo the tree is empty and nothing is derived.
pub fn subagent_tree(demo: bool, members: &[Member]) -> Vec<SubNode> {
    if !demo || members.is_empty() {
        return Vec::new();
    }
    let root = |m: &Member, children: Vec<SubNode>| SubNode {
        name: m.name.clone(),
        task: m.task.clone(),
        status: NodeStatus::Run,
        member: Some(m.clone()),
        children,
    };
    let lead = members.iter().find(|m| m.leader).unwrap_or(&members[0]);
    let mut out = vec![root(
        lead,
        vec![
            thread("Explore", "proxy router", NodeStatus::Done, Vec::new()),
            thread("Test runner", "단위 테스트", NodeStatus::Wait, Vec::new()),
        ],
    )];
    if let Some(m) = members.iter().find(|m| m.session_id != lead.session_id) {
        out.push(root(
            m,
            vec![thread(
                "CSS scanner",
                "스타일 검색",
                NodeStatus::Run,
                vec![
                    thread(":has() rewrite", "셀렉터 변환", NodeStatus::Run, Vec::new()),
                    thread("Snapshot diff", "캡처 비교", NodeStatus::Done, Vec::new()),
                ],
            )],
        ));
    }
    out
}

/// The vendor of a harness, as the mockup's `vendorOf`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    Claude,
    Codex,
    Gemini,
}

pub fn vendor_of(agent: &str) -> Vendor {
    let a = agent.to_lowercase();
    if a.contains("codex") {
        Vendor::Codex
    } else if a.contains("gemini") {
        Vendor::Gemini
    } else {
        Vendor::Claude
    }
}

/// The bar colour of a vendor: claude orange, codex blue, gemini sky (`--v-*`).
pub fn vendor_accent(agent: &str) -> StatusTone {
    match vendor_of(agent) {
        Vendor::Claude => StatusTone::Orange,
        Vendor::Codex => StatusTone::Blue,
        Vendor::Gemini => StatusTone::Sky,
    }
}

/// One member row of the token block.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenRow {
    pub member: Member,
    /// Thousands of tokens today.
    pub k: u32,
}

/// Token rows, biggest first. The server sends no per-member usage yet (TODO(server): FR-U7), so
/// outside the demo there are none.
pub fn token_rows(demo: bool, members: &[Member]) -> Vec<TokenRow> {
    if !demo {
        return Vec::new();
    }
    const SAMPLE: [u32; 4] = [360, 210, 90, 40];
    let mut rows: Vec<TokenRow> = members
        .iter()
        .enumerate()
        .map(|(i, m)| TokenRow { member: m.clone(), k: SAMPLE[i % SAMPLE.len()] })
        .collect();
    rows.sort_by(|a, b| b.k.cmp(&a.k));
    rows
}

/// The bar fill width in percent, relative to the biggest value; 0 when there is none.
pub fn token_width_pct(k: u32, max: u32) -> f32 {
    if max == 0 { 0.0 } else { (k.min(max) as f32 / max as f32) * 100.0 }
}

/// The text at the right of a token row.
pub fn token_value_text(k: u32) -> String {
    format!("{k}k")
}

/// One computer row of the 컴퓨터 block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputerRow {
    pub name: String,
    pub ip: Option<String>,
    pub online: bool,
    /// CPU load in percent; `None` without a metric (TODO(server): FR-X7 monitor metrics).
    pub cpu: Option<u8>,
    /// Running sessions on it; `None` when the session record does not say.
    pub running: Option<u32>,
}

/// The computers assigned to `project`. CPU and run counts exist only in the demo.
pub fn computer_rows(
    computers: &[ember_client::state::Computer],
    project: &str,
    demo: bool,
) -> Vec<ComputerRow> {
    use ember_client::state::Reachability;
    computers
        .iter()
        .filter(|c| c.projects.iter().any(|p| p == project))
        .map(|c| {
            let online = c.reachability == Reachability::Online;
            ComputerRow {
                name: c.name.clone(),
                ip: None,
                online,
                cpu: if demo && online { Some(62) } else { None },
                running: if demo && online { Some(2) } else { None },
            }
        })
        .collect()
}

/// The fill colour of a computer's bar: orange above 75 percent, else green.
pub fn cpu_fill_tone(cpu: u8) -> StatusTone {
    if cpu > 75 { StatusTone::Orange } else { StatusTone::Green }
}

/// The bar fill in percent: 0 when offline or without a metric.
pub fn computer_fill_pct(c: &ComputerRow) -> f32 {
    match (c.online, c.cpu) {
        (true, Some(p)) => p.min(100) as f32,
        _ => 0.0,
    }
}

/// The text at the right of a computer row. Never a number without a metric.
pub fn computer_value_text(c: &ComputerRow) -> String {
    if !c.online {
        return "오프라인".to_string();
    }
    match (c.cpu, c.running) {
        (Some(p), Some(n)) => format!("{p}% \u{00b7} {n}실행"),
        (Some(p), None) => format!("{p}%"),
        _ => "온라인".to_string(),
    }
}

/// The real session counts of the 부하 card (not in the mockup, kept because they are real).
pub fn session_count_rows(members: &[Member]) -> Vec<String> {
    if members.is_empty() {
        return Vec::new();
    }
    let n = |s: LauncherStatus| members.iter().filter(|m| m.status == s).count();
    vec![
        format!("실행 중 세션 {}개", n(LauncherStatus::Running)),
        format!("승인 대기 세션 {}개", n(LauncherStatus::WaitingForApproval)),
    ]
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
    fn fr_l_toolbar_folds_in_stages_so_ide_open_stays_visible() {
        let level = |w: f32| toolbar_fold_level(toolbar_width(w, w >= 840.0));
        assert_eq!(level(800.0), 3);
        assert_eq!(level(1000.0), 3);
        assert_eq!(level(1200.0), 2);
        assert_eq!(level(1500.0), 0);
        assert_eq!(toolbar_width(1200.0, true), 864.0);
    }

    #[test]
    fn view_chips_get_their_own_row_at_phone_width() {
        assert!(view_chips_below(390.0));
        assert!(view_chips_below(560.0));
        assert!(!view_chips_below(768.0));
    }

    #[test]
    fn fr_l18_drawer_width_is_86_percent_capped_at_300_then_290() {
        assert_eq!(drawer_width(0.0), 300.0);
        assert_eq!(drawer_width(300.0), 300.0 * 0.86);
        assert_eq!(drawer_width(360.0), 300.0);
        assert_eq!(drawer_width(390.0), 300.0);
        assert_eq!(drawer_width(560.0), 300.0);
        assert_eq!(drawer_width(561.0), 290.0);
        assert_eq!(drawer_width(900.0), 290.0);
    }

    #[test]
    fn fr_l18_drawer_is_an_overlay_unless_the_list_is_a_column() {
        assert!(drawer_is_overlay(390.0));
        assert!(drawer_is_overlay(561.0));
        assert!(drawer_is_overlay(900.0));
        assert!(!drawer_is_overlay(901.0));
    }

    #[test]
    fn fr_l18_drawer_open_effective_resets_when_the_list_is_a_column() {
        assert!(drawer_open_effective(false, true));
        assert!(!drawer_open_effective(false, false));
        assert!(!drawer_open_effective(true, true));
        assert!(!drawer_open_effective(true, false));
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
    fn fr_l18_pane_header_has_one_icon_toggle_that_names_the_other_view() {
        assert_eq!(PaneToggle::Conversation.other(), PaneToggle::Terminal);
        assert_eq!(PaneToggle::Terminal.other(), PaneToggle::Conversation);
        assert_eq!(PaneToggle::Conversation.switch_label(), "터미널로 보기");
        assert_eq!(PaneToggle::Terminal.switch_label(), "대화로 보기");
    }

    #[test]
    fn fr_l18_view_segment_is_icon_only_below_desktop_width() {
        assert!(view_seg_icon_only(390.0));
        assert!(view_seg_icon_only(1024.0));
        assert!(!view_seg_icon_only(1280.0));
        assert!(!view_seg_icon_only(1440.0));
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

    // ---- FR-L18 web pages (#326) -------------------------------------------------------------

    fn lp(port: u16, scope: ember_client::api::PortScope, process: &str) -> ember_client::api::ListeningPort {
        ember_client::api::ListeningPort { port, scope, process: process.into(), ..Default::default() }
    }

    fn ports_of(v: Vec<ember_client::api::ListeningPort>) -> ember_client::api::Ports {
        ember_client::api::Ports { ports: v, supported: true }
    }

    fn remote(host: &str) -> ComputerHost {
        ComputerHost { local: false, host: Some(host.into()) }
    }

    fn local_host() -> ComputerHost {
        ComputerHost { local: true, host: None }
    }

    fn ready(l: WebPagesLoad) -> Vec<WebPage> {
        match l {
            WebPagesLoad::Ready(v) => v,
            other => panic!("not ready: {other:?}"),
        }
    }

    #[test]
    fn fr_l18_web_lan_port_opens_on_the_computer_host() {
        use ember_client::api::PortScope::*;
        let pages = ready(web_pages_from(Ok(ports_of(vec![lp(3000, Lan, "node"), lp(80, Any, "nginx")])), &remote("192.168.0.10")));
        assert_eq!(pages[0].url.as_deref(), Some("http://192.168.0.10:80"));
        assert_eq!(pages[0].label, "http://192.168.0.10:80 (nginx)");
        assert_eq!(pages[1].url.as_deref(), Some("http://192.168.0.10:3000"));
        assert_eq!(pages[1].note, None);
    }

    #[test]
    fn fr_l18_web_loopback_opens_only_on_the_local_computer() {
        use ember_client::api::PortScope::*;
        let l = ready(web_pages_from(Ok(ports_of(vec![lp(5173, Loopback, "vite")])), &local_host()));
        assert_eq!(l[0].url.as_deref(), Some("http://127.0.0.1:5173"));
        assert_eq!(l[0].label, "http://127.0.0.1:5173 (vite)");
        let r = ready(web_pages_from(Ok(ports_of(vec![lp(5173, Loopback, "vite")])), &remote("192.168.0.10")));
        assert_eq!(r[0].url, None);
        assert_eq!(r[0].note, Some("이 컴퓨터 안에서만 열 수 있습니다"));
        assert_eq!(r[0].label, ":5173 (vite)");
    }

    #[test]
    fn fr_l18_web_lan_port_on_the_local_computer_uses_loopback() {
        use ember_client::api::PortScope::*;
        let l = ready(web_pages_from(Ok(ports_of(vec![lp(3000, Lan, "node")])), &local_host()));
        assert_eq!(l[0].url.as_deref(), Some("http://127.0.0.1:3000"));
    }

    #[test]
    fn fr_l18_web_peer_addressed_computer_has_no_url() {
        use ember_client::api::PortScope::*;
        let none = ComputerHost { local: false, host: None };
        let l = ready(web_pages_from(Ok(ports_of(vec![lp(3000, Any, ""), lp(4000, Lan, "bun")])), &none));
        assert_eq!(l[0].url, None);
        assert_eq!(l[0].note, Some("주소를 알 수 없어 열 수 없습니다"));
        assert_eq!(l[0].label, ":3000");
        assert_eq!(l[1].label, ":4000 (bun)");
    }

    #[test]
    fn fr_l18_web_ipv6_host_is_bracketed() {
        use ember_client::api::PortScope::*;
        let l = ready(web_pages_from(Ok(ports_of(vec![lp(3000, Lan, "node")])), &remote("fd00::1")));
        assert_eq!(l[0].url.as_deref(), Some("http://[fd00::1]:3000"));
    }

    #[test]
    fn fr_l18_web_sorts_by_port_and_drops_duplicates() {
        use ember_client::api::PortScope::*;
        let v = vec![lp(6006, Lan, "a"), lp(3000, Lan, "node"), lp(3000, Any, "node"), lp(80, Lan, "n")];
        let l = ready(web_pages_from(Ok(ports_of(v)), &remote("h")));
        let ports: Vec<_> = l.iter().map(|w| w.url.clone().unwrap()).collect();
        assert_eq!(ports, ["http://h:80", "http://h:3000", "http://h:6006"]);
    }

    #[test]
    fn fr_l18_web_unsupported_empty_and_failed() {
        let h = remote("h");
        let unsupported = ember_client::api::Ports { ports: Vec::new(), supported: false };
        assert_eq!(web_pages_from(Ok(unsupported), &h), WebPagesLoad::Unsupported);
        assert_eq!(web_pages_from(Ok(ports_of(Vec::new())), &h), WebPagesLoad::Empty);
        assert_eq!(
            web_pages_from(Err("timeout".into()), &h),
            WebPagesLoad::Failed("포트 목록을 불러오지 못했습니다: timeout".into())
        );
    }

    #[test]
    fn fr_l18_web_menu_lines_and_taps() {
        use ember_client::api::PortScope::*;
        let load = web_pages_from(Ok(ports_of(vec![lp(3000, Lan, "node"), lp(5173, Loopback, "vite")])), &remote("h"));
        assert_eq!(web_menu_lines(&load), ["http://h:3000 (node)", ":5173 (vite)"]);
        assert_eq!(web_tap(&load, 0), WebTap::Open("http://h:3000".into()));
        assert_eq!(web_tap(&load, 1), WebTap::Say("이 컴퓨터 안에서만 열 수 있습니다".into()));
        assert_eq!(web_tap(&load, 9), WebTap::Nothing);
        assert_eq!(web_menu_lines(&WebPagesLoad::Loading), ["불러오는 중입니다"]);
        assert_eq!(web_tap(&WebPagesLoad::Loading, 0), WebTap::Nothing);
        assert_eq!(web_menu_lines(&WebPagesLoad::Empty), ["열려 있는 웹 포트가 없습니다"]);
        assert_eq!(web_tap(&WebPagesLoad::Empty, 0), WebTap::Say("열려 있는 웹 포트가 없습니다".into()));
        assert_eq!(
            web_menu_lines(&WebPagesLoad::Unsupported),
            ["이 컴퓨터는 포트 목록을 지원하지 않습니다"]
        );
    }

    #[test]
    fn fr_l18_web_computer_host_from_the_address() {
        assert_eq!(computer_host(true, "", "http://127.0.0.1:8740"), local_host());
        assert_eq!(computer_host(true, "", "http://mini.local:8740"), remote("mini.local"));
        assert_eq!(computer_host(false, "http://192.168.0.20:8740/x", ""), remote("192.168.0.20"));
        assert_eq!(computer_host(false, "http://[fd00::2]:8740", ""), remote("fd00::2"));
        assert_eq!(computer_host(false, "", "http://127.0.0.1:8740"), ComputerHost { local: false, host: None });
        assert_eq!(computer_host(false, "peer:abc", ""), ComputerHost { local: false, host: None });
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

    use ember_client::state::{Computer, Reachability};

    fn comp(name: &str, projects: &[&str], r: Reachability) -> Computer {
        Computer {
            id: name.into(),
            name: name.into(),
            reachability: r,
            projects: projects.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn two_members() -> Vec<Member> {
        vec![
            member("a", LauncherStatus::Running, true, 1),
            member("b", LauncherStatus::WaitingForApproval, false, 2),
        ]
    }

    #[test]
    fn fr_u7_timeline_cost_is_demo_only() {
        assert!(timeline_summary(true).contains("$0.24"));
        assert!(!timeline_summary(false).contains('$'));
        assert_eq!(timeline_summary(false), "비용은 알 수 없습니다");
    }

    #[test]
    fn fr_l18_status_data_present() {
        let ms = vec![
            member("a", LauncherStatus::Running, true, 1),
            member("b", LauncherStatus::WaitingForApproval, false, 2),
            member("c", LauncherStatus::Running, false, 3),
        ];
        assert_eq!(session_count_rows(&ms), vec!["실행 중 세션 2개", "승인 대기 세션 1개"]);
        assert!(session_count_rows(&[]).is_empty());
    }

    #[test]
    fn fr_l18_status_no_data_is_honest() {
        let ms = two_members();
        assert!(pr_rows(false).is_empty());
        assert!(subagent_tree(false, &ms).is_empty());
        assert!(token_rows(false, &ms).is_empty());
        assert_eq!(PR_EMPTY, "서버 데이터를 기다리고 있습니다");
        assert_eq!(TOKENS_EMPTY, "아직 없습니다");
        use crate::ui::usage::Load;
        assert_eq!(token_tile(&Load::Loading, "2026-11-06"), ("-".to_string(), "불러오는 중입니다".to_string()));
        assert_eq!(token_tile(&Load::Failed("x".into()), "2026-11-06").1, "불러오지 못했습니다");
        let empty = Load::Ready(ember_client::api::UsageSummary::default());
        assert_eq!(token_tile(&empty, "2026-11-06"), ("-".to_string(), "보고된 사용량이 없습니다".to_string()));
    }

    #[test]
    fn fr_l18_status_demo_mode_shows_samples() {
        let ms = two_members();
        assert!(pr_rows(true).len() >= 2);
        assert!(!subagent_tree(true, &ms).is_empty());
        assert!(!token_rows(true, &ms).is_empty());
        let now = crate::demo::now_ms();
        let mut sample = crate::demo::usage_summary(now);
        let today = crate::demo::day_string(now);
        sample.days.retain(|d| d.day == today);
        let (value, sub) = token_tile(&crate::ui::usage::Load::Ready(sample), &today);
        assert_ne!(value, "-");
        assert!(sub.contains("UTC 기준") && !sub.contains('$'));
    }

    #[test]
    fn fr_l18_status_ci_pill_label_and_tone() {
        assert_eq!(CiState::Pass.label(), "CI 통과");
        assert_eq!(CiState::Fail.label(), "CI 실패");
        assert_eq!(CiState::Run.label(), "CI 실행 중");
        assert_eq!(CiState::Pass.tone(), StatusTone::Green);
        assert_eq!(CiState::Fail.tone(), StatusTone::Red);
        assert_eq!(CiState::Run.tone(), StatusTone::Blue);
    }

    #[test]
    fn fr_l18_status_pr_rows_empty_without_demo() {
        assert!(pr_rows(false).is_empty());
        let rows = pr_rows(true);
        assert_eq!(pr_count_text(&rows), "2");
        assert_eq!(rows[0].headline(), "#212 센터 시트와 스크림");
        assert!(rows[0].sub_line().contains("feat/sheets"));
    }

    #[test]
    fn fr_l18_status_subagent_tree_empty_without_demo() {
        assert!(subagent_tree(false, &two_members()).is_empty());
        assert!(subagent_tree(true, &[]).is_empty());
    }

    #[test]
    fn fr_l18_status_subagent_tree_nests_children() {
        let tree = subagent_tree(true, &two_members());
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].children.len(), 2);
        assert_eq!(tree[1].depth(), 3);
        assert_eq!(NodeStatus::Run.dot_tone(), Some(StatusTone::Blue));
        assert_eq!(NodeStatus::Wait.dot_tone(), Some(StatusTone::Orange));
        assert_eq!(NodeStatus::Done.dot_tone(), Some(StatusTone::Green));
        assert_eq!(NodeStatus::Fail.dot_tone(), Some(StatusTone::Red));
        assert_eq!(NodeStatus::Unknown.dot_tone(), None);
    }

    #[test]
    fn fr_l18_status_never_lists_members_as_threads() {
        let ms = two_members();
        let tree = subagent_tree(true, &ms);
        fn walk(n: &SubNode, out: &mut Vec<(String, bool)>) {
            out.push((n.name.clone(), n.member.is_some()));
            for c in &n.children {
                walk(c, out);
            }
        }
        let mut all = Vec::new();
        for n in &tree {
            walk(n, &mut all);
            assert!(n.member.is_some(), "roots are members that have threads");
            assert!(!n.children.is_empty(), "a member without threads is not listed");
            for c in &n.children {
                assert!(c.member.is_none(), "threads are not members");
            }
        }
    }

    #[test]
    fn fr_l18_status_vendor_of_harness_name() {
        assert_eq!(vendor_of("claude-code"), Vendor::Claude);
        assert_eq!(vendor_of("Codex"), Vendor::Codex);
        assert_eq!(vendor_of("gemini-cli"), Vendor::Gemini);
        assert_eq!(vendor_of("other"), Vendor::Claude);
    }

    #[test]
    fn fr_l18_status_vendor_colours_claude_orange_codex_blue_gemini_sky() {
        assert_eq!(vendor_accent("claude-code"), StatusTone::Orange);
        assert_eq!(vendor_accent("codex"), StatusTone::Blue);
        assert_eq!(vendor_accent("gemini"), StatusTone::Sky);
    }

    #[test]
    fn fr_l18_status_token_rows_sorted_descending() {
        let mut ms = two_members();
        ms.push(member("c", LauncherStatus::Idle, false, 3));
        let rows = token_rows(true, &ms);
        assert_eq!(rows.len(), 3);
        assert!(rows.windows(2).all(|w| w[0].k >= w[1].k));
    }

    #[test]
    fn fr_l18_status_token_bar_width_is_relative_to_max() {
        assert_eq!(token_width_pct(50, 100), 50.0);
        assert_eq!(token_width_pct(100, 100), 100.0);
        assert_eq!(token_width_pct(5, 0), 0.0);
        assert_eq!(token_value_text(360), "360k");
    }

    #[test]
    fn fr_l18_status_token_rows_empty_without_demo() {
        assert!(token_rows(false, &two_members()).is_empty());
    }

    #[test]
    fn fr_l18_status_computer_rows_only_project_computers() {
        let cs = vec![
            comp("a", &["p1"], Reachability::Online),
            comp("b", &["p2"], Reachability::Online),
        ];
        let rows = computer_rows(&cs, "p1", false);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "a");
    }

    #[test]
    fn fr_l18_status_cpu_over_75_is_orange() {
        assert_eq!(cpu_fill_tone(75), StatusTone::Green);
        assert_eq!(cpu_fill_tone(76), StatusTone::Orange);
    }

    #[test]
    fn fr_l18_status_computer_without_metrics_shows_no_percent() {
        let cs = vec![comp("a", &["p1"], Reachability::Online)];
        let r = &computer_rows(&cs, "p1", false)[0];
        assert_eq!(r.cpu, None);
        assert_eq!(computer_value_text(r), "온라인");
        assert_eq!(computer_fill_pct(r), 0.0);
        let d = &computer_rows(&cs, "p1", true)[0];
        assert_eq!(computer_value_text(d), "62% \u{00b7} 2실행");
    }

    #[test]
    fn fr_l18_status_offline_computer_has_empty_bar() {
        let cs = vec![comp("a", &["p1"], Reachability::Offline)];
        let r = &computer_rows(&cs, "p1", true)[0];
        assert_eq!(computer_value_text(r), "오프라인");
        assert_eq!(computer_fill_pct(r), 0.0);
    }

    #[test]
    fn fr_l18_status_texts_have_no_plain_da_endings() {
        for t in [PR_EMPTY, SUBAGENT_EMPTY, TOKENS_EMPTY, COMPUTERS_EMPTY, TOKENS_CAPTION] {
            assert!(
                !t.ends_with('다')
                    || t.ends_with("입니다")
                    || t.ends_with("합니다")
                    || t.ends_with("습니다")
                    || t.ends_with("됩니다")
                    || t.ends_with("없다"),
                "{t}"
            );
            assert!(!t.contains('\u{2014}'), "no em-dash: {t}");
        }
    }

    #[test]
    fn fr_l18_status_cards_fill_the_page_width_per_column_count() {
        assert_eq!(status_grid_columns(390.0), 1);
        assert_eq!(status_grid_columns(900.0), 2);
        assert_eq!(status_grid_columns(1440.0), 3);
        // 390 dp: one card as wide as the stat tiles row, window minus both paddings.
        assert_eq!(status_card_width(390.0, 1, 20.0), 390.0 - 2.0 * status_page_pad(390.0));
        // 1440 dp: three cards and two gaps fill the row exactly.
        let w = status_card_width(1440.0, 3, 20.0);
        assert!((3.0 * w + 40.0 - (1440.0 - 40.0)).abs() < 0.01);
    }

    #[test]
    fn fr_l18_tree_status_has_a_text_label_besides_the_colour() {
        assert_eq!(NodeStatus::Run.label(), "실행 중");
        assert_eq!(NodeStatus::Wait.label(), "대기");
        assert_eq!(NodeStatus::Done.label(), "완료");
        assert_eq!(NodeStatus::Fail.label(), "실패");
        assert_eq!(NodeStatus::Unknown.label(), "");
    }
}
