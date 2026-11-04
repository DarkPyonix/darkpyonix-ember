//! compose-rust version of the computer monitor sheet (#220 phase 4; FR-L21,
//! `docs/DESIGN.md` section 6.9).
//!
//! The node does not collect metrics yet (FR-X7, Draft), so the sheet draws only what the client
//! knows: name, online state, assigned projects and the member sessions of those projects. The
//! metric cards say "준비 중입니다". The pure functions (rolling series, the process table
//! sort) are `ui::monitor`'s and are used as they are.

#![allow(non_snake_case)]

use compose_rust::runtime::{mutable_state_of, remember, MutableState};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use ember_client::state::Reachability;
use compose_rust::{Alignment, SpaceRole};

pub use crate::ui::monitor::{
    agent_rows, computer_members, cpu_text, fold_samples, mem_text, offline_notice,
    sort_rows, toggle_sort, ComputerMetrics, Metric, NoMetrics, ProcessRow, SortKey, SortState,
};
use super::characters::{Character, CharState};
use super::components::{EmberCard, EmberSheetSized, Label, Pill, PillTone};
use super::theme::*;
use crate::ui::coexist::shared_ui;
use crate::ui::project::{display_name, reach_pill, ComputerChip, ProjectBox, SOON};
use crate::ui::Route;

fn header_label(key: SortKey) -> &'static str {
    match key {
        SortKey::Name => "이름",
        SortKey::Pid => "PID",
        SortKey::Cpu => "CPU",
        SortKey::Memory => "메모리",
    }
}

/// The sort arrow after a header label.
pub fn sort_mark(cur: SortState, key: SortKey) -> &'static str {
    if cur.key != key {
        ""
    } else if cur.descending {
        " \u{25BE}"
    } else {
        " \u{25B4}"
    }
}

/// Cards per row (`.mon-grid`): 4 above 1279 dp, 2 up to 1279, 1 on a phone (up to 560).
pub fn mon_columns(width_dp: f32) -> usize {
    if width_dp <= 560.0 {
        1
    } else if width_dp <= 1279.0 {
        2
    } else {
        4
    }
}

/// The sheet's width: `min(1360px, 100%)` less the dialog's margin.
pub fn sheet_width(window_dp: f32) -> f32 {
    (window_dp - 32.0).clamp(280.0, 1360.0)
}

/// Column widths of the process table (dp): the name takes the rest.
const PID_W: f32 = 72.0;
const PWR_W: f32 = 72.0;
const CPU_W: f32 = 96.0;
const MEM_W: f32 = 96.0;
const ROW_H: f32 = 36.0;

/// The monitor sheet of one computer. `metrics` is [`NoMetrics`] until ember node reports
/// (FR-X7), so the cards say they are not ready and the table lists the members' sessions.
pub fn MonitorSheet(chip: Option<&ComputerChip>, boxes: &[ProjectBox], on_close: impl FnMut() + Clone + 'static) {
    let p = ember_theme().palette;
    let sort = remember(|| mutable_state_of(SortState::default()));
    let metrics = NoMetrics;
    let title = chip.map(|c| c.name.clone()).unwrap_or_else(|| "컴퓨터".to_string());
    let window = compose_rust::runtime::current_window_size();
    let columns = mon_columns(window.width_dp);
    let body_height = (window.height_dp - 32.0 - 60.0).clamp(240.0, 1200.0);
    let state = chip.map(|c| reach_pill(c.reachability));

    // The state pill sits in the header, beside the close button (`.mon-state`).
    let mut actions = || {
        if let Some((text, tone)) = state {
            Pill(text, tone.into(), false);
            cu::Spacer().modifier(Modifier::new().width(SP[2]));
        }
    };
    EmberSheetSized(&title, true, sheet_width(window.width_dp), on_close.clone(), Some(&mut actions), || {
        cu::ScrollColumn()
            .modifier(Modifier::new().fill_max_width().height(body_height).background(paint(p.bg)))
            .content(|| {
                cu::Column().space_role(SpaceRole::Md).modifier(Modifier::new().fill_max_width().padding(SP[4])).content(|| {
                    let Some(c) = chip else {
                        cu::Box().alignment(Alignment::Center).modifier(Modifier::new().fill_max_width().height(240.0)).content(|| {
                            Label("컴퓨터를 찾을 수 없습니다", Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
                        });
                        return;
                    };
                    let online = matches!(c.reachability, Reachability::Online);
                    let members = computer_members(&c.projects, boxes);
                    let mut rows = metrics.processes().unwrap_or_else(|| agent_rows(&members));
                    let cur = sort.get();
                    sort_rows(&mut rows, cur);
                    if let Some(n) = offline_notice(c.reachability) {
                        OfflineBanner(n);
                    }
                    // `.mon-wrap.stale`: a computer that is not online is greyed out.
                    let wrap = if online { Modifier::new() } else { Modifier::new().alpha(0.55) };
                    cu::Column().space_role(SpaceRole::Md).modifier(wrap.fill_max_width()).content(|| {
                        Grid(columns, c);
                        ProcessTable(&rows, cur, &sort, &on_close);
                        Label(
                            &format!("프로세스 목록은 팀원의 세션만 표시합니다. 전체 프로세스는 {SOON}"),
                            Fs::Xs,
                            Weight::Regular,
                            Some(paint(p.text_2)),
                            None,
                        );
                    });
                });
            });
    });
}

/// `.mon-off`: a red soft banner with the notice, for a computer that is not online.
fn OfflineBanner(title: &str) {
    let p = ember_theme().palette;
    cu::Row()
        .alignment(Alignment::TopStart)
        .space_role(SpaceRole::Sm)
        .modifier(
            Modifier::new()
                .fill_max_width()
                .background(paint(p.red.soft))
                .corner_radius(CORNER_MD)
                .padding(SP[2]),
        )
        .content(|| {
            // The glyph's circle fills 18 of 24 units, so 24 px draws the mockup's 20 px mark.
            super::shell::NavIcon(super::shell::ICON_INFO_SVG, p.red.strong.to_argb(), 24.0);
            cu::Column().spacing(2.0).content(|| {
                Label(title, Fs::Md, Weight::Semibold, Some(paint(p.red.strong)), None);
                Label(
                    "마지막 연결 시각은 아직 알 수 없습니다. 아래는 마지막으로 받은 값입니다.",
                    Fs::Sm,
                    Weight::Regular,
                    Some(paint(p.text_2)),
                    None,
                );
            });
        });
}

/// The metric cards: the status card first, then CPU, GPU, memory, temperature, disk, network
/// and fans, `columns` to a row (`.mon-grid`).
fn Grid(columns: usize, c: &ComputerChip) {
    let cards: Vec<Card> = std::iter::once(Card::Status)
        .chain(Metric::ALL.iter().copied().map(Card::Metric))
        .collect();
    for chunk in cards.chunks(columns) {
        cu::Row().space_role(SpaceRole::Md).modifier(Modifier::new().fill_max_width()).content(|| {
            for card in chunk.iter().copied() {
                cu::Column().modifier(Modifier::new().weight(1.0)).content(|| match card {
                    Card::Status => StatusCard(c),
                    Card::Metric(m) => MetricCard(m, NoMetrics.series(m)),
                });
            }
            for _ in chunk.len()..columns {
                cu::Spacer().modifier(Modifier::new().weight(1.0));
            }
        });
    }
}

#[derive(Clone, Copy)]
enum Card {
    Status,
    Metric(Metric),
}

fn metric_icon(m: Metric) -> &'static str {
    match m {
        Metric::Cpu => super::shell::ICON_CPU_SVG,
        Metric::Gpu => super::shell::ICON_GPU_SVG,
        Metric::Memory => super::shell::ICON_MEMORY_SVG,
        Metric::Temperature => super::shell::ICON_THERMO_SVG,
        Metric::Disk => super::shell::ICON_DISK_SVG,
        Metric::Network => super::shell::ICON_NETWORK_SVG,
        Metric::Fans => super::shell::ICON_FAN_SVG,
    }
}

/// `.mon-card`: 200 high on wide windows, a 16 px icon and a 13 px semibold `--text-2` label.
fn MonCard(icon: &'static str, label: &str, body: impl FnOnce()) {
    let p = ember_theme().palette;
    EmberCard(false, true, None, None, None, || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width().height(168.0)).content(|| {
            cu::Row().alignment(Alignment::CenterStart).space_role(SpaceRole::Xs).modifier(Modifier::new().fill_max_width()).content(|| {
                super::shell::NavIcon(icon, p.muted.to_argb(), 16.0);
                Label(label, Fs::Sm, Weight::Semibold, Some(paint(p.text_2)), Some(1));
            });
            body();
        });
    });
}

/// The status card: the score ring (an empty track until the node reports) and the assigned
/// projects as chips (`.mon-score`).
fn StatusCard(c: &ComputerChip) {
    let p = ember_theme().palette;
    let ring = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"72\" height=\"72\" viewBox=\"0 0 72 72\"><circle cx=\"36\" cy=\"36\" r=\"32\" fill=\"none\" stroke=\"#{:06x}\" stroke-width=\"8\"/></svg>",
        p.track.to_argb() & 0x00ff_ffff
    );
    MonCard(super::shell::ICON_GAUGE_SVG, "상태", || {
        cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Column().space_role(SpaceRole::Sm).modifier(Modifier::new().weight(1.0)).content(|| {
                Label(SOON, Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
                if c.projects.is_empty() {
                    Label("할당된 프로젝트가 없습니다", Fs::Xs, Weight::Regular, Some(paint(p.text_2)), None);
                }
                for pr in c.projects.iter() {
                    Pill(&display_name(pr), PillTone::Grey, true);
                }
            });
            cu::Image(compose_rust::asset(compose_rust::AssetKind::Svg, super::shell::leak_doc(ring)))
                .modifier(Modifier::new().size(72.0, 72.0));
        });
    });
}

fn MetricCard(metric: Metric, series: Option<Vec<f32>>) {
    let p = ember_theme().palette;
    MonCard(metric_icon(metric), metric.title(), || match series.as_ref().and_then(|s| s.last()) {
        Some(v) => Label(&format!("{v:.1}"), Fs::Xl4, Weight::Semibold, None, None),
        None => Label(SOON, Fs::Sm, Weight::Regular, Some(paint(p.muted)), None),
    });
}

/// `.mon-tablebox`: a card with a sticky header and 1 px row dividers.
fn ProcessTable(rows: &[ProcessRow], cur: SortState, sort: &MutableState<SortState>, on_close: &(impl FnMut() + Clone + 'static)) {
    let p = ember_theme().palette;
    let ui = shared_ui().clone();
    EmberCard(false, true, None, Some(0.0), None, || {
        cu::Column().modifier(Modifier::new().fill_max_width()).content(|| {
            // Header: 12 px muted, the sorted one in `--text`, numbers right-aligned.
            cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width().height(H_CTL)).content(|| {
                cu::Spacer().modifier(Modifier::new().width(SP[3]));
                for key in [SortKey::Name, SortKey::Pid, SortKey::Cpu, SortKey::Memory] {
                    let sort = sort.clone();
                    let (width, end) = match key {
                        SortKey::Name => (None, false),
                        SortKey::Pid => (Some(PID_W), true),
                        SortKey::Cpu => (Some(CPU_W), true),
                        SortKey::Memory => (Some(MEM_W), true),
                    };
                    let ink = if cur.key == key { p.text } else { p.muted };
                    let text = match key {
                        SortKey::Name => format!("이름 ({}){}", rows.len(), sort_mark(cur, key)),
                        _ => format!("{}{}", header_label(key), sort_mark(cur, key)),
                    };
                    let m = match width {
                        Some(w) => Modifier::new().width(w).height(H_CTL),
                        None => Modifier::new().weight(1.0).height(H_CTL),
                    };
                    super::components::Clickable(
                        m,
                        if end { Alignment::CenterEnd } else { Alignment::CenterStart },
                        move || sort.set(toggle_sort(sort.get_untracked(), key)),
                        || Label(&text, Fs::Xs, Weight::Medium, Some(paint(ink)), Some(1)),
                    );
                    // The power column sits before CPU; the node reports no power yet, so it
                    // is a label only (`th.c-pwr`).
                    if key == SortKey::Pid {
                        cu::Box().alignment(Alignment::CenterEnd).modifier(Modifier::new().width(PWR_W).height(H_CTL)).content(|| {
                            Label("전력", Fs::Xs, Weight::Medium, Some(paint(p.muted)), None);
                        });
                    }
                }
                cu::Spacer().modifier(Modifier::new().width(SP[3]));
            });
            for (i, r) in rows.iter().enumerate() {
                cu::Spacer().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
                let _ = i;
                let (ui, mut close) = (ui.clone(), on_close.clone());
                ProcessRowView(r, move |id: String| {
                    close();
                    ui.go(Route::Conversation(id));
                });
            }
            if rows.is_empty() {
                cu::Spacer().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
                cu::Box().alignment(Alignment::Center).modifier(Modifier::new().fill_max_width().height(64.0)).content(|| {
                    Label("이 컴퓨터에서 실행 중인 팀원이 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
                });
            }
        });
    });
}

/// One row of the process table. A member's agent process (`tr.mine`) has a 3 px blue bar, a
/// 12 % blue wash and the member name in blue, and opens its session (FR-L21).
/// TODO(compose-rust#372): the row is a pointer-up `Clickable`.
fn ProcessRowView(row: &ProcessRow, mut on_open: impl FnMut(String) + 'static) {
    let p = ember_theme().palette;
    let session = row.session.clone();
    let mine = session.is_some();
    let fill = if mine { paint(super::compat::mix(p.surface, p.blue.strong, 0.12)) } else { paint(p.surface) };
    let content = || {
        cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width().height(ROW_H)).content(|| {
            if mine {
                cu::Spacer().modifier(Modifier::new().width(3.0).height(ROW_H).background(paint(p.blue.strong)));
                cu::Spacer().modifier(Modifier::new().width(SP[3] - 3.0));
            } else {
                cu::Spacer().modifier(Modifier::new().width(SP[3]));
            }
            cu::Row().alignment(Alignment::CenterStart).space_role(SpaceRole::Sm).modifier(Modifier::new().weight(1.0)).content(|| {
                if mine {
                    Character(&row.agent, row.leader, 22.0, &row.name, "", CharState::default(), None);
                    Label(&row.agent, Fs::Sm, Weight::Medium, Some(paint(p.text)), Some(1));
                    let who = if row.leader { format!("{} (리더)", row.name) } else { row.name.clone() };
                    Label(&who, Fs::Xs, Weight::Medium, Some(paint(p.blue.strong)), Some(1));
                } else {
                    super::shell::NavIcon(super::shell::ICON_CPU_SVG, p.muted.to_argb(), 14.0);
                    Label(&row.name, Fs::Sm, Weight::Medium, Some(paint(p.text)), Some(1));
                }
            });
            let cell = |w: f32, text: String| {
                cu::Box().alignment(Alignment::CenterEnd).modifier(Modifier::new().width(w)).content(|| {
                    Label(&text, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                });
            };
            cell(PID_W, row.pid.map_or_else(|| "-".to_string(), |v| v.to_string()));
            cell(PWR_W, "-".to_string());
            cell(CPU_W, cpu_text(row.cpu));
            cell(MEM_W, mem_text(row.mem));
            cu::Spacer().modifier(Modifier::new().width(SP[3]));
        });
    };
    let box_modifier = Modifier::new().fill_max_width().background(fill);
    match session {
        Some(id) => super::components::Clickable(box_modifier, Alignment::CenterStart, move || on_open(id.clone()), content),
        None => cu::Box().modifier(box_modifier).content(content),
    }
}

/// A review window (`EMBER_CR_ROUTE=monitor`): the monitor sheet of a demo computer, chosen by
/// `EMBER_CR_COMPUTER` (an index: 0 is the online "이 컴퓨터", 1 the offline "스튜디오 PC").
#[compose_rust::runtime::composable]
pub fn preview() {
    let ui = shared_ui().clone();
    let _ = ui.live.launcher.get();
    let index: usize = std::env::var("EMBER_CR_COMPUTER").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let (boxes, chips) = crate::services::services().client.read(|s| {
        let boxes = crate::ui::project::project_boxes(s);
        let chips = crate::ui::project::computer_chips(s, &boxes);
        (boxes, chips)
    });
    // The sheet opens over the shell, as it does in the app.
    cu::Box().modifier(Modifier::new().fill_max_size()).content(|| {
        super::shell::shell();
        MonitorSheet(chips.get(index), &boxes, || {});
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use compose_rust::{Host, Recomposer};
    use ember_client::state::Reachability;

    fn chip() -> ComputerChip {
        ComputerChip {
            id: "c1".into(),
            name: "맥미니".into(),
            reachability: Reachability::Offline,
            running: 2,
            projects: vec!["ember".into()],
        }
    }

    #[test]
    fn fr_l21_sort_mark_shows_the_direction_on_the_sorted_header_only() {
        let cur = SortState { key: SortKey::Cpu, descending: true };
        assert_eq!(sort_mark(cur, SortKey::Cpu), " \u{25BE}");
        assert_eq!(sort_mark(cur, SortKey::Name), "");
        assert_eq!(sort_mark(SortState { key: SortKey::Cpu, descending: false }, SortKey::Cpu), " \u{25B4}");
    }

    #[test]
    fn fr_l21_the_monitor_sheet_composes_the_computer_and_says_metrics_are_not_ready() {
        let _guard = crate::services::test_guard();
        let c = chip();
        let mut host = Host::new(move || {
            let c = c.clone();
            Box::new(Recomposer::new(move || MonitorSheet(Some(&c), &[], || {})))
        });
        let batch = compose_rust::protocol::decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let text: String = batch.iter().map(|m| format!("{m:?}")).collect();
        for want in ["맥미니", "오프라인", "연결이 끊겼습니다", "이름 (0)", SOON, "상태"] {
            assert!(text.contains(want), "missing {want}");
        }
    }

    #[test]
    fn fr_l21_a_missing_computer_says_so() {
        let _guard = crate::services::test_guard();
        let mut host = Host::new(|| Box::new(Recomposer::new(|| MonitorSheet(None, &[], || {}))));
        let batch = compose_rust::protocol::decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let text: String = batch.iter().map(|m| format!("{m:?}")).collect();
        assert!(text.contains("컴퓨터를 찾을 수 없습니다"));
    }
}
