//! The computer monitor sheet (FR-L21; `docs/DESIGN.md` section 6.9).
//!
//! The node does not collect metrics yet (FR-X7, Draft), so the sheet draws only what the client
//! knows: name, online state, assigned projects and the member sessions of those projects. The
//! metric cards say "준비 중입니다". The pure functions below (rolling series, the process table
//! sort) are what the live update will use once a [`ComputerMetrics`] source exists.

use dioxus_compose::prelude::*;
use ember_client::state::Reachability;

use crate::ui::components::{EmberCard, EmberSheet, InsetPanel, Label, Pill};
use crate::ui::project::{
    display_name, reach_pill, Character, ComputerChip, Member, ProjectBox, SOON,
};
use crate::ui::theme::*;
use crate::ui::{use_ui, Route};

/// The metric cards of the monitor, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Cpu,
    Gpu,
    Memory,
    Temperature,
    Disk,
    Network,
    Fans,
}

impl Metric {
    pub const ALL: [Metric; 7] = [
        Metric::Cpu,
        Metric::Gpu,
        Metric::Memory,
        Metric::Temperature,
        Metric::Disk,
        Metric::Network,
        Metric::Fans,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Metric::Cpu => "CPU",
            Metric::Gpu => "GPU",
            Metric::Memory => "메모리",
            Metric::Temperature => "온도",
            Metric::Disk => "디스크",
            Metric::Network => "네트워크",
            Metric::Fans => "팬",
        }
    }
}

/// Where the live values come from. Nothing implements it with real data until ember node
/// reports them (FR-X7). `None` means "not reported", never an estimate.
pub trait ComputerMetrics {
    /// The latest samples of one metric, oldest first.
    fn series(&self, metric: Metric) -> Option<Vec<f32>>;
    /// The computer's process list.
    fn processes(&self) -> Option<Vec<ProcessRow>>;
}

/// The source used today: reports nothing.
pub struct NoMetrics;

impl ComputerMetrics for NoMetrics {
    fn series(&self, _metric: Metric) -> Option<Vec<f32>> {
        None
    }
    fn processes(&self) -> Option<Vec<ProcessRow>> {
        None
    }
}

/// Samples kept per graph: one minute at one sample a second.
pub const SERIES_CAP: usize = 60;

/// Fold new samples into a rolling series: append, drop non-finite values, keep the newest `cap`.
pub fn fold_samples(prev: &[f32], incoming: &[f32], cap: usize) -> Vec<f32> {
    let mut out: Vec<f32> = prev
        .iter()
        .chain(incoming.iter())
        .copied()
        .filter(|v| v.is_finite())
        .collect();
    if out.len() > cap {
        out.drain(..out.len() - cap);
    }
    out
}

/// One row of the process table.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessRow {
    pub pid: Option<u32>,
    pub name: String,
    /// CPU percent, when reported.
    pub cpu: Option<f32>,
    /// Resident memory in bytes, when reported.
    pub mem: Option<u64>,
    /// The session of a member's agent process (FR-L21: the row opens it).
    pub session: Option<String>,
    pub agent: String,
    pub leader: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Pid,
    Cpu,
    Memory,
}

/// The table's sort, kept by the sheet across updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortState {
    pub key: SortKey,
    pub descending: bool,
}

impl Default for SortState {
    fn default() -> Self {
        SortState {
            key: SortKey::Cpu,
            descending: true,
        }
    }
}

/// A click on a header: the same key flips the direction; a new key starts descending for CPU
/// and memory and ascending for name and PID.
pub fn toggle_sort(state: SortState, key: SortKey) -> SortState {
    if state.key == key {
        SortState {
            key,
            descending: !state.descending,
        }
    } else {
        SortState {
            key,
            descending: matches!(key, SortKey::Cpu | SortKey::Memory),
        }
    }
}

/// Sort rows by the state. Rows without the value go last in either direction; ties break by
/// name, then PID, so rows do not jump between updates.
pub fn sort_rows(rows: &mut [ProcessRow], state: SortState) {
    use std::cmp::Ordering;
    // `None` goes last in both directions, so the direction applies to known values only.
    fn known<T: PartialOrd>(a: &Option<T>, b: &Option<T>, desc: bool) -> Ordering {
        match (a, b) {
            (Some(x), Some(y)) => {
                let o = x.partial_cmp(y).unwrap_or(Ordering::Equal);
                if desc {
                    o.reverse()
                } else {
                    o
                }
            }
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
    }
    let desc = state.descending;
    rows.sort_by(|a, b| {
        let primary = match state.key {
            SortKey::Name => {
                let o = a.name.cmp(&b.name);
                if desc {
                    o.reverse()
                } else {
                    o
                }
            }
            SortKey::Pid => known(&a.pid, &b.pid, desc),
            SortKey::Cpu => known(&a.cpu, &b.cpu, desc),
            SortKey::Memory => known(&a.mem, &b.mem, desc),
        };
        primary
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.pid.cmp(&b.pid))
    });
}

/// Members of the projects assigned to a computer, once each. TODO(FR-X7): the client does not
/// know which computer a session runs on, so this is every member of the assigned projects.
pub fn computer_members(projects: &[String], boxes: &[ProjectBox]) -> Vec<Member> {
    let mut out: Vec<Member> = Vec::new();
    for b in boxes.iter().filter(|b| projects.contains(&b.id)) {
        for m in &b.members {
            if !out.iter().any(|o| o.session_id == m.session_id) {
                out.push(m.clone());
            }
        }
    }
    out
}

/// Agent process rows for members; CPU, memory and PID are unknown until FR-X7.
pub fn agent_rows(members: &[Member]) -> Vec<ProcessRow> {
    members
        .iter()
        .map(|m| ProcessRow {
            pid: None,
            name: m.name.clone(),
            cpu: None,
            mem: None,
            session: Some(m.session_id.clone()),
            agent: m.agent.clone(),
            leader: m.leader,
        })
        .collect()
}

/// The line under the title: the online state, and for a computer that is not online the notice
/// and what is not known (the client keeps no last connection time yet).
pub fn offline_notice(r: Reachability) -> Option<&'static str> {
    match r {
        Reachability::Online => None,
        Reachability::Offline => Some("연결이 끊겼습니다"),
        Reachability::Unknown => Some("연결 상태를 알 수 없습니다"),
    }
}

/// Cards per row: 2 wide or medium, 1 on a phone. TODO(GAPS: 창 폭 레이아웃): 4 at 1280 and up.
pub fn card_columns(compact: bool) -> usize {
    if compact {
        1
    } else {
        2
    }
}

/// CPU percent text for a reported value; a missing one is a dash.
pub fn cpu_text(v: Option<f32>) -> String {
    v.map_or_else(|| "-".to_string(), |v| format!("{v:.1}%"))
}

/// Memory text in MB or GB for a reported value; a missing one is a dash.
pub fn mem_text(v: Option<u64>) -> String {
    match v {
        None => "-".to_string(),
        Some(b) if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        Some(b) => format!("{} MB", b >> 20),
    }
}

fn header_label(key: SortKey) -> &'static str {
    match key {
        SortKey::Name => "이름",
        SortKey::Pid => "PID",
        SortKey::Cpu => "CPU",
        SortKey::Memory => "메모리",
    }
}

/// The monitor sheet of one computer. `metrics` is [`NoMetrics`] until ember node reports
/// (FR-X7), so the cards say they are not ready and the table lists the members' sessions.
#[component]
pub fn MonitorSheet(
    chip: Option<ComputerChip>,
    boxes: Vec<ProjectBox>,
    on_close: EventHandler<()>,
) -> Element {
    let theme = use_ember_theme();
    let p = theme.palette;
    let ui = use_ui();
    let window = use_window_size();
    let mut sort = use_signal(SortState::default);
    let metrics = NoMetrics;
    let title = chip
        .as_ref()
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "컴퓨터".to_string());
    let columns = card_columns(window.class == WindowSizeClass::Compact);

    rsx! {
        EmberSheet {
            title,
            open: true,
            on_dismiss: move |()| on_close.call(()),
            ScrollColumn {
                fill_max_width: true,
                Column {
                    fill_max_width: true,
                    padding: SP[4],
                    space_role: SpaceRole::Md,
                    if let Some(c) = chip {
                        {
                            let (state_text, tone) = reach_pill(c.reachability);
                            let members = computer_members(&c.projects, &boxes);
                            let mut rows = metrics.processes().unwrap_or_else(|| agent_rows(&members));
                            sort_rows(&mut rows, *sort.read());
                            let cur = *sort.read();
                            rsx! {
                                Row {
                                    fill_max_width: true,
                                    alignment: Alignment::CenterStart,
                                    space_role: SpaceRole::Sm,
                                    Pill { text: state_text, tone }
                                    Label { text: format!("실행 중 {}", c.running), fs: Fs::Sm, color: paint(p.text_2) }
                                }
                                if let Some(n) = offline_notice(c.reachability) {
                                    InsetPanel { fill_max_width: true,
                                        Label { text: n, fs: Fs::Sm, weight: Weight::Medium }
                                        Label { text: "마지막 연결 시각은 아직 알 수 없습니다", fs: Fs::Xs, color: paint(p.text_2) }
                                    }
                                }
                                Label { text: "할당된 프로젝트", fs: Fs::Sm, weight: Weight::Medium, color: paint(p.text_2) }
                                if c.projects.is_empty() {
                                    Label { text: "할당된 프로젝트가 없습니다", color: paint(p.text_2) }
                                }
                                for pr in c.projects.iter() {
                                    Label { key: "{pr}", text: display_name(pr) }
                                }
                                for chunk in Metric::ALL.chunks(columns) {
                                    Row {
                                        key: "{chunk[0].title()}",
                                        fill_max_width: true,
                                        space_role: SpaceRole::Sm,
                                        for m in chunk.iter().copied() {
                                            Column { key: "{m.title()}", weight: 1.0,
                                                MetricCard { metric: m, series: metrics.series(m) }
                                            }
                                        }
                                        if chunk.len() < columns {
                                            Spacer { weight: 1.0 }
                                        }
                                    }
                                }
                                Label { text: "프로세스", fs: Fs::Lg, weight: Weight::Semibold }
                                Row {
                                    fill_max_width: true,
                                    alignment: Alignment::CenterStart,
                                    for key in [SortKey::Name, SortKey::Pid, SortKey::Cpu, SortKey::Memory] {
                                        {
                                            let mark = if cur.key == key { if cur.descending { " \u{25BE}" } else { " \u{25B4}" } } else { "" };
                                            rsx! {
                                                Column { key: "{header_label(key)}", weight: 1.0,
                                                    Button {
                                                        text: format!("{}{mark}", header_label(key)),
                                                        height: 40.0,
                                                        variant: ButtonVariant::Text,
                                                        color: paint(p.text_2),
                                                        fill_max_width: true,
                                                        on_click: move |()| {
                                                            let next = toggle_sort(*sort.peek(), key);
                                                            sort.set(next);
                                                        },
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                if rows.is_empty() {
                                    Label { text: "이 컴퓨터에서 실행 중인 팀원이 없습니다", color: paint(p.text_2) }
                                }
                                for (i, r) in rows.into_iter().enumerate() {
                                    ProcessRowView {
                                        key: "{i}-{r.name}",
                                        row: r,
                                        on_open: move |id: String| {
                                            on_close.call(());
                                            ui.go(Route::Conversation(id));
                                        },
                                    }
                                }
                                Label { text: format!("프로세스 목록은 팀원의 세션만 표시합니다. 전체 프로세스는 {SOON}"), fs: Fs::Xs, color: paint(p.text_2) }
                            }
                        }
                    } else {
                        Label { text: "컴퓨터를 찾을 수 없습니다" }
                    }
                }
            }
        }
    }
}

/// One metric card: the title and, with no data, "준비 중입니다".
#[component]
fn MetricCard(metric: Metric, series: Option<Vec<f32>>) -> Element {
    let theme = use_ember_theme();
    let p = theme.palette;
    rsx! {
        EmberCard { fill_max_width: true,
            Column {
                fill_max_width: true,
                padding: SP[3],
                spacing: SP[1],
                Label { text: metric.title(), fs: Fs::Md, weight: Weight::Semibold }
                match series.as_ref().and_then(|s| s.last()) {
                    // TODO(FR-X7): the graph from `fold_samples` series.
                    Some(v) => rsx! { Label { text: format!("{v:.1}"), fs: Fs::Xl, weight: Weight::Semibold } },
                    None => rsx! { Label { text: SOON, fs: Fs::Sm, color: paint(p.muted) } },
                }
            }
        }
    }
}

/// A process row; a member's agent row has the character, the leader badge and opens its session.
#[component]
fn ProcessRowView(row: ProcessRow, on_open: EventHandler<String>) -> Element {
    let theme = use_ember_theme();
    let p = theme.palette;
    let session = row.session.clone();
    let agent_row = session.is_some();
    rsx! {
        Row {
            fill_max_width: true,
            alignment: Alignment::CenterStart,
            space_role: SpaceRole::Sm,
            background: paint(if agent_row { p.blue.soft } else { p.surface }),
            corner_radius: CORNER_MD,
            if agent_row {
                Character { agent: row.agent.clone(), leader: row.leader, size: 24.0, name: row.name.clone() }
            }
            // TODO(GAPS: 클릭 가능한 컨테이너): the name button opens the member's session.
            Column { weight: 1.0,
                Button {
                    text: if row.leader { format!("{} (리더)", row.name) } else { row.name.clone() },
                    height: 40.0,
                    variant: ButtonVariant::Text,
                    color: paint(p.text),
                    fill_max_width: true,
                    enabled: agent_row,
                    on_click: move |()| {
                        if let Some(id) = session.clone() {
                            on_open.call(id);
                        }
                    },
                }
            }
            Label { text: row.pid.map_or_else(|| "-".to_string(), |v| v.to_string()), fs: Fs::Xs, color: paint(p.text_2) }
            Label { text: cpu_text(row.cpu), fs: Fs::Xs, color: paint(p.text_2) }
            Label { text: mem_text(row.mem), fs: Fs::Xs, color: paint(p.text_2) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_client::LauncherStatus;

    fn row(name: &str, pid: Option<u32>, cpu: Option<f32>, mem: Option<u64>) -> ProcessRow {
        ProcessRow {
            pid,
            name: name.into(),
            cpu,
            mem,
            session: None,
            agent: "claude-code".into(),
            leader: false,
        }
    }

    fn member(id: &str, leader: bool) -> Member {
        Member {
            session_id: id.into(),
            name: format!("m-{id}"),
            task: String::new(),
            agent: "claude-code".into(),
            status: LauncherStatus::Running,
            leader,
            updated_at: 0,
        }
    }

    fn names(rows: &[ProcessRow]) -> Vec<&str> {
        rows.iter().map(|r| r.name.as_str()).collect()
    }

    #[test]
    fn fold_appends_and_keeps_the_newest() {
        assert_eq!(fold_samples(&[1.0, 2.0], &[3.0], 5), vec![1.0, 2.0, 3.0]);
        assert_eq!(
            fold_samples(&[1.0, 2.0, 3.0], &[4.0, 5.0], 3),
            vec![3.0, 4.0, 5.0]
        );
        assert_eq!(fold_samples(&[], &[1.0, 2.0, 3.0], 2), vec![2.0, 3.0]);
    }

    #[test]
    fn fold_drops_non_finite_and_handles_zero_cap() {
        assert_eq!(
            fold_samples(&[1.0], &[f32::NAN, f32::INFINITY, 2.0], 5),
            vec![1.0, 2.0]
        );
        assert!(fold_samples(&[1.0], &[2.0], 0).is_empty());
    }

    #[test]
    fn no_metrics_reports_nothing() {
        for m in Metric::ALL {
            assert!(NoMetrics.series(m).is_none());
        }
        assert!(NoMetrics.processes().is_none());
    }

    #[test]
    fn toggle_flips_the_same_key_and_resets_a_new_one() {
        let s = SortState::default();
        assert_eq!((s.key, s.descending), (SortKey::Cpu, true));
        let s = toggle_sort(s, SortKey::Cpu);
        assert!(!s.descending);
        let s = toggle_sort(s, SortKey::Name);
        assert_eq!((s.key, s.descending), (SortKey::Name, false));
        let s = toggle_sort(s, SortKey::Memory);
        assert_eq!((s.key, s.descending), (SortKey::Memory, true));
    }

    #[test]
    fn sort_by_cpu_descending_puts_unknown_last() {
        let mut rows = vec![
            row("a", Some(1), None, None),
            row("b", Some(2), Some(5.0), None),
            row("c", Some(3), Some(50.0), None),
        ];
        sort_rows(
            &mut rows,
            SortState {
                key: SortKey::Cpu,
                descending: true,
            },
        );
        assert_eq!(names(&rows), ["c", "b", "a"]);
        sort_rows(
            &mut rows,
            SortState {
                key: SortKey::Cpu,
                descending: false,
            },
        );
        assert_eq!(names(&rows), ["b", "c", "a"]);
    }

    #[test]
    fn sort_keeps_its_state_across_updates() {
        let state = toggle_sort(SortState::default(), SortKey::Name);
        let mut first = vec![row("b", None, None, None), row("a", None, None, None)];
        sort_rows(&mut first, state);
        let mut second = vec![
            row("c", None, None, None),
            row("a", None, None, None),
            row("b", None, None, None),
        ];
        sort_rows(&mut second, state);
        assert_eq!(names(&first), ["a", "b"]);
        assert_eq!(names(&second), ["a", "b", "c"]);
    }

    #[test]
    fn equal_values_break_ties_by_name_then_pid() {
        let mut rows = vec![
            row("x", Some(9), Some(1.0), Some(5)),
            row("x", Some(3), Some(1.0), Some(5)),
            row("a", Some(7), Some(1.0), Some(5)),
        ];
        sort_rows(
            &mut rows,
            SortState {
                key: SortKey::Memory,
                descending: true,
            },
        );
        assert_eq!(
            rows.iter().map(|r| r.pid).collect::<Vec<_>>(),
            [Some(7), Some(3), Some(9)]
        );
    }

    #[test]
    fn members_of_assigned_projects_appear_once() {
        let mk = |id: &str, ms: Vec<Member>| ProjectBox {
            id: id.into(),
            name: id.into(),
            members: ms,
            running: 0,
            waiting: 0,
            computers: vec![],
            top: None,
            repo: None,
            branch: None,
        };
        let boxes = vec![
            mk("p1", vec![member("s1", true), member("s2", false)]),
            mk("p2", vec![member("s2", false), member("s3", false)]),
            mk("p3", vec![member("s4", false)]),
        ];
        let got = computer_members(&["p1".into(), "p2".into()], &boxes);
        let ids: Vec<&str> = got.iter().map(|m| m.session_id.as_str()).collect();
        assert_eq!(ids, ["s1", "s2", "s3"]);
        assert!(computer_members(&[], &boxes).is_empty());
    }

    #[test]
    fn agent_rows_carry_session_agent_and_leader() {
        let rows = agent_rows(&[member("s1", true), member("s2", false)]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].session.as_deref(), Some("s1"));
        assert!(rows[0].leader && !rows[1].leader);
        assert_eq!(rows[0].name, "m-s1");
        assert_eq!(rows[0].pid, None);
        assert_eq!(rows[0].cpu, None);
    }

    // NFR-L4: notices in 합니다체.
    #[test]
    fn offline_notice_only_when_not_online() {
        assert_eq!(offline_notice(Reachability::Online), None);
        assert_eq!(
            offline_notice(Reachability::Offline),
            Some("연결이 끊겼습니다")
        );
        assert_eq!(
            offline_notice(Reachability::Unknown),
            Some("연결 상태를 알 수 없습니다")
        );
    }

    #[test]
    fn value_text_uses_a_dash_for_unknown() {
        assert_eq!(cpu_text(None), "-");
        assert_eq!(cpu_text(Some(12.34)), "12.3%");
        assert_eq!(mem_text(None), "-");
        assert_eq!(mem_text(Some(512 << 20)), "512 MB");
        assert_eq!(mem_text(Some(3 << 30)), "3.0 GB");
    }

    #[test]
    fn card_columns_by_width() {
        assert_eq!(card_columns(false), 2);
        assert_eq!(card_columns(true), 1);
    }
}
