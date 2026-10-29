//! compose-rust version of the computer monitor sheet (#220 phase 4; FR-L21,
//! `docs/DESIGN.md` section 6.9).
//!
//! The node does not collect metrics yet (FR-X7, Draft), so the sheet draws only what the client
//! knows: name, online state, assigned projects and the member sessions of those projects. The
//! metric cards say "준비 중입니다". The pure functions (rolling series, the process table
//! sort) are `ui::monitor`'s and are used as they are.

#![allow(non_snake_case)]

use compose_rust::runtime::{mutable_state_of, remember};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{Alignment, ButtonVariant, SpaceRole, WindowSizeClass};

pub use crate::ui::monitor::{
    agent_rows, card_columns, computer_members, cpu_text, fold_samples, mem_text, offline_notice,
    sort_rows, toggle_sort, ComputerMetrics, Metric, NoMetrics, ProcessRow, SortKey, SortState,
};
use super::characters::{Character, CharState};
use super::components::{EmberCard, EmberSheet, InsetPanel, Label, Pill};
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

/// The monitor sheet of one computer. `metrics` is [`NoMetrics`] until ember node reports
/// (FR-X7), so the cards say they are not ready and the table lists the members' sessions.
pub fn MonitorSheet(chip: Option<&ComputerChip>, boxes: &[ProjectBox], on_close: impl FnMut() + Clone + 'static) {
    let p = ember_theme().palette;
    let ui = shared_ui().clone();
    let sort = remember(|| mutable_state_of(SortState::default()));
    let metrics = NoMetrics;
    let title = chip.map(|c| c.name.clone()).unwrap_or_else(|| "컴퓨터".to_string());
    let window = compose_rust::runtime::current_window_size();
    let columns = card_columns(window.class == WindowSizeClass::Compact);
    let body_height = (window.height_dp - 32.0 - 60.0).clamp(240.0, 900.0);

    EmberSheet(&title, true, on_close.clone(), None, || {
        cu::ScrollColumn().modifier(Modifier::new().fill_max_width().height(body_height)).content(|| {
            cu::Column().space_role(SpaceRole::Md).modifier(Modifier::new().fill_max_width().padding(SP[4])).content(|| {
                let Some(c) = chip else {
                    Label("컴퓨터를 찾을 수 없습니다", Fs::Md, Weight::Regular, None, None);
                    return;
                };
                let (state_text, state_tone) = reach_pill(c.reachability);
                let members = computer_members(&c.projects, boxes);
                let mut rows = metrics.processes().unwrap_or_else(|| agent_rows(&members));
                let cur = sort.get();
                sort_rows(&mut rows, cur);
                cu::Row().alignment(Alignment::CenterStart).space_role(SpaceRole::Sm).modifier(Modifier::new().fill_max_width()).content(|| {
                    Pill(state_text, state_tone.into(), false);
                    Label(&format!("실행 중 {}", c.running), Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                });
                if let Some(n) = offline_notice(c.reachability) {
                    InsetPanel(true, || {
                        Label(n, Fs::Sm, Weight::Medium, None, None);
                        Label("마지막 연결 시각은 아직 알 수 없습니다", Fs::Xs, Weight::Regular, Some(paint(p.text_2)), None);
                    });
                }
                Label("할당된 프로젝트", Fs::Sm, Weight::Medium, Some(paint(p.text_2)), None);
                if c.projects.is_empty() {
                    Label("할당된 프로젝트가 없습니다", Fs::Md, Weight::Regular, Some(paint(p.text_2)), None);
                }
                for pr in c.projects.iter() {
                    Label(&display_name(pr), Fs::Md, Weight::Regular, None, None);
                }
                for chunk in Metric::ALL.chunks(columns) {
                    cu::Row().space_role(SpaceRole::Sm).modifier(Modifier::new().fill_max_width()).content(|| {
                        for m in chunk.iter().copied() {
                            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                                MetricCard(m, metrics.series(m));
                            });
                        }
                        if chunk.len() < columns {
                            cu::Spacer().modifier(Modifier::new().weight(1.0));
                        }
                    });
                }
                Label("프로세스", Fs::Lg, Weight::Semibold, None, None);
                cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                    for key in [SortKey::Name, SortKey::Pid, SortKey::Cpu, SortKey::Memory] {
                        cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                            let sort = sort.clone();
                            cu::Button(&format!("{}{}", header_label(key), sort_mark(cur, key)))
                                .variant(ButtonVariant::Text)
                                .color(paint(p.text_2))
                                .modifier(Modifier::new().height(H_BTN).fill_max_width())
                                .on_click(move || sort.set(toggle_sort(sort.get_untracked(), key)));
                        });
                    }
                });
                if rows.is_empty() {
                    Label("이 컴퓨터에서 실행 중인 팀원이 없습니다", Fs::Md, Weight::Regular, Some(paint(p.text_2)), None);
                }
                for r in rows {
                    let (ui, mut close) = (ui.clone(), on_close.clone());
                    ProcessRowView(&r, move |id: String| {
                        close();
                        ui.go(Route::Conversation(id));
                    });
                }
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
}

fn MetricCard(metric: Metric, series: Option<Vec<f32>>) {
    let p = ember_theme().palette;
    EmberCard(false, true, None, Some(SP[3]), None, || {
        cu::Column().spacing(SP[0]).modifier(Modifier::new().fill_max_width()).content(|| {
            Label(metric.title(), Fs::Md, Weight::Semibold, None, None);
            match series.as_ref().and_then(|s| s.last()) {
                Some(v) => Label(&format!("{v:.1}"), Fs::Xl, Weight::Semibold, None, None),
                None => Label(SOON, Fs::Sm, Weight::Regular, Some(paint(p.muted)), None),
            }
        });
    });
}

/// One row of the process table. A member's agent process opens its session (FR-L21).
fn ProcessRowView(row: &ProcessRow, mut on_open: impl FnMut(String) + 'static) {
    let p = ember_theme().palette;
    let session = row.session.clone();
    let agent_row = session.is_some();
    let fill = if agent_row { p.blue.soft } else { p.surface };
    cu::Row()
        .alignment(Alignment::CenterStart)
        .space_role(SpaceRole::Sm)
        .modifier(Modifier::new().fill_max_width().background(paint(fill)).corner_radius(CORNER_MD))
        .content(|| {
            if agent_row {
                Character(&row.agent, row.leader, 24.0, &row.name, "", CharState::default(), None);
            }
            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                let label = if row.leader { format!("{} (리더)", row.name) } else { row.name.clone() };
                cu::Button(&label)
                    .variant(ButtonVariant::Text)
                    .color(paint(p.text))
                    .enabled(agent_row)
                    .modifier(Modifier::new().height(H_BTN).fill_max_width())
                    .on_click(move || {
                        if let Some(id) = session.clone() {
                            on_open(id);
                        }
                    });
            });
            Label(&row.pid.map_or_else(|| "-".to_string(), |v| v.to_string()), Fs::Xs, Weight::Regular, Some(paint(p.text_2)), None);
            Label(&cpu_text(row.cpu), Fs::Xs, Weight::Regular, Some(paint(p.text_2)), None);
            Label(&mem_text(row.mem), Fs::Xs, Weight::Regular, Some(paint(p.text_2)), None);
        });
}

/// A review window (`EMBER_CR_ROUTE=monitor`): the monitor sheet of a demo computer, chosen by
/// `EMBER_CR_COMPUTER` (an index, default the first).
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
    cu::Box()
        .modifier(Modifier::new().fill_max_size().background(paint(ember_theme().palette.bg)))
        .content(|| MonitorSheet(chips.get(index), &boxes, || {}));
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
        for want in ["맥미니", "오프라인", "연결이 끊겼습니다", "프로세스", SOON, "할당된 프로젝트"] {
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
