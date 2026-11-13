//! compose-rust version of the computer monitor sheet (#220 phase 4; FR-L21,
//! `docs/DESIGN.md` section 6.9).
//!
//! While the sheet is open it asks the server for the computer's metrics every few seconds
//! (FR-X7, `GET /api/computers/{id}/metrics`) and draws them: the pure feed, formats and sort
//! are `ui::monitor`'s. An offline computer is not asked and says so; a failed request keeps the
//! last values dimmed (the cards at 55 percent opacity). The temperature meter runs on a fixed
//! 0 to 100 degree Celsius scale (hottest sensor). Demo mode takes its samples from `demo::metrics_sample`.

#![allow(non_snake_case)]

use compose_rust::runtime::{delay, launched_effect, mutable_state_of, remember, MutableState};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use ember_client::state::Reachability;
use compose_rust::{Alignment, SpaceRole};

pub use crate::ui::monitor::{
    agent_rows, computer_members, cpu_text, fold_samples, mem_text, offline_notice,
    sort_rows, toggle_sort, ComputerMetrics, Metric, MonitorFeed, NoMetrics, ProcessRow, SortKey,
    SortState,
};
use crate::ui::monitor::{
    bar_fraction, bar_max, card_view, meter_fraction, empty_text, health_score, process_footer, ram_badge, score_tone,
    score_word, status_line, storage_line, CardView, FanMode, FeedStatus, FAN_MODE_AVAILABLE, FAN_MODE_REASON, POLL_SECS,
    TOP_PROCESSES,
};
use super::characters::{Character, CharState};
use super::components::{
    AppButton, ButtonKind, EmberCard, EmberSheetSized, Label, Pill, PillTone, SHEET_WIDTH_MONITOR,
};
use super::theme::*;
use crate::ui::coexist::shared_ui;
use crate::ui::project::{display_name, reach_pill, ComputerChip, ProjectBox};
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

/// Column widths of the process table (dp): the name takes the rest.
const PID_W: f32 = 72.0;
const PWR_W: f32 = 72.0;
const CPU_W: f32 = 96.0;
const MEM_W: f32 = 96.0;
const ROW_H: f32 = 36.0;

/// One metrics request: the demo source, or the server through the client API on the runtime.
async fn fetch_metrics(id: String) -> Result<ember_client::api::MetricsSample, String> {
    let s = crate::services::services();
    if s.demo.is_some() || crate::demo::active_from_env() {
        return Ok(crate::demo::metrics_sample(crate::demo::now_ms().max(0) as u64));
    }
    let job = s.rt.spawn(async move {
        crate::services::services().client.api().metrics(&id, TOP_PROCESSES).await.map_err(|e| e.to_string())
    });
    job.await.unwrap_or_else(|e| Err(e.to_string()))
}

/// The monitor sheet of one computer. It polls while it is composed (the effect is cancelled
/// when the sheet closes) unless the computer is offline.
pub fn MonitorSheet(chip: Option<&ComputerChip>, boxes: &[ProjectBox], on_close: impl FnMut() + Clone + 'static) {
    let p = ember_theme().palette;
    let sort = remember(|| mutable_state_of(SortState::default()));
    let feed_state = remember(|| mutable_state_of(MonitorFeed::default()));
    {
        let poll = chip.is_some_and(|c| !matches!(c.reachability, Reachability::Offline));
        let id = chip.map(|c| c.id.clone()).unwrap_or_default();
        let feed_w = feed_state.clone();
        let demo = crate::services::services().demo.is_some() || crate::demo::active_from_env();
        launched_effect((id.clone(), poll), move || async move {
            if !poll || id.is_empty() {
                return;
            }
            if demo {
                // Two samples at once, so the network card has a rate on the first frame.
                feed_w.update(|f| f.push(crate::demo::metrics_previous(crate::demo::now_ms().max(0) as u64)));
            }
            loop {
                match fetch_metrics(id.clone()).await {
                    Ok(sample) => feed_w.update(|f| f.push(sample)),
                    Err(e) => feed_w.update(|f| f.fail(e)),
                }
                delay(std::time::Duration::from_secs(POLL_SECS)).await;
            }
        });
    }
    let feed = feed_state.get();
    let title = chip.map(|c| c.name.clone()).unwrap_or_else(|| "컴퓨터".to_string());
    let window = compose_rust::runtime::current_window_size();
    let columns = mon_columns(window.width_dp);
    let body_height = super::components::sheet_body_max_height(window.width_dp, window.height_dp, true, 60.0).min(1200.0);
    let state = chip.map(|c| reach_pill(c.reachability));

    // The state pill sits in the header, beside the close button (`.mon-state`).
    let mut actions = || {
        if let Some((text, tone)) = state {
            Pill(text, tone.into(), false);
            cu::Spacer().modifier(Modifier::new().width(SP[2]));
        }
    };
    EmberSheetSized(&title, true, SHEET_WIDTH_MONITOR, true, on_close.clone(), Some(&mut actions), || {
        cu::ScrollColumn()
            .modifier(Modifier::new().fill_max_width().height(body_height).background(paint(p.bg)))
            .content(|| {
                cu::Column().space_role(SpaceRole::Md).modifier(Modifier::new().fill_max_width().padding(SP[4])).content(|| {
                    let Some(c) = chip else {
                        cu::Box().alignment(Alignment::Center).modifier(Modifier::new().fill_max_width().height(240.0)).content(|| {
                            Label("컴퓨터를 찾을 수 없습니다", Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
                        });
                        return;
                    };
                    let online = matches!(c.reachability, Reachability::Online);
                    let members = computer_members(&c.projects, boxes);
                    let mut rows = feed.processes().unwrap_or_else(|| agent_rows(&members));
                    let cur = sort.get();
                    sort_rows(&mut rows, cur);
                    if let Some(n) = offline_notice(c.reachability) {
                        OfflineBanner(n);
                    } else if let (FeedStatus::Failed, Some(e)) = (feed.status(), feed.error()) {
                        FailureNotice(e);
                    }
                    // `.mon-wrap.stale`: a computer that is not online, or whose last request failed
                    // (the old values stay), is greyed out.
                    let stale = !online || matches!(feed.status(), FeedStatus::Failed);
                    let wrap = if stale { Modifier::new().alpha(0.55) } else { Modifier::new() };
                    cu::Column().space_role(SpaceRole::Md).modifier(wrap.fill_max_width()).content(|| {
                        Grid(columns, c, &feed, online);
                        ProcessTable(&rows, cur, &sort, &on_close);
                        Label(
                            &process_footer(&feed),
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

/// A red soft notice for a failed request; the cards keep the last values.
fn FailureNotice(detail: &str) {
    let p = ember_theme().palette;
    cu::Column()
        .spacing(2.0)
        .modifier(Modifier::new().fill_max_width().background(paint(p.red.soft)).corner_radius(CORNER_MD).padding(SP[2]))
        .content(|| {
            Label("지표를 읽지 못했습니다", Fs::Md, Weight::Semibold, Some(paint(p.red.strong)), Some(1));
            Label(detail, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(2));
        });
}

/// The metric cards: the status card first, then CPU, GPU, memory, temperature, disk, network
/// and fans, `columns` to a row (`.mon-grid`). Cards are 200 high in a grid (`.mon-card`
/// `min-height: 200px`) and as high as their content in one column.
fn Grid(columns: usize, c: &ComputerChip, feed: &MonitorFeed, online: bool) {
    let cards: Vec<Card> = std::iter::once(Card::Status)
        .chain(Metric::ALL.iter().copied().map(Card::Metric))
        .collect();
    let height = card_height(columns);
    for chunk in cards.chunks(columns) {
        cu::Row().space_role(SpaceRole::Md).modifier(Modifier::new().fill_max_width()).content(|| {
            for card in chunk.iter().copied() {
                cu::Column().modifier(Modifier::new().weight(1.0)).content(|| match card {
                    Card::Status => StatusCard(c, feed, online, height),
                    Card::Metric(Metric::Fans) => FanCard(height),
                    Card::Metric(m) => MetricCard(m, card_view(m, feed), empty_text(m, online, feed.status()), meter_fraction(m, feed), height),
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

/// The inner height of a card: 200 less the card padding in a grid, free in one column.
pub fn card_height(columns: usize) -> Option<f32> {
    (columns > 1).then_some(200.0 - 2.0 * PAD_CARD_V)
}

/// `.mon-card`: a 16 px icon, a 13 px semibold `--text-2` label and an optional badge on the
/// right (`.mon-badge`).
fn MonCard(icon: &'static str, label: &str, badge: Option<(&str, PillTone)>, height: Option<f32>, body: impl FnOnce()) {
    let p = ember_theme().palette;
    EmberCard(false, true, None, None, None, || {
        let m = Modifier::new().fill_max_width();
        let m = match height {
            Some(h) => m.height(h),
            None => m,
        };
        cu::Column().spacing(SP[2]).modifier(m).content(|| {
            cu::Row().alignment(Alignment::CenterStart).space_role(SpaceRole::Xs).modifier(Modifier::new().fill_max_width()).content(|| {
                super::shell::NavIcon(icon, p.muted.to_argb(), 16.0);
                // TODO(compose-rust#443): no overflow_wrap on Text, so a long label clips with an ellipsis.
                Label(label, Fs::Sm, Weight::Semibold, Some(paint(p.text_2)), Some(1));
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                if let Some((text, tone)) = badge {
                    Pill(text, tone, true);
                }
            });
            body();
        });
    });
}

/// The status card (`.mon-score`): the health score with its word, the main disk's free space,
/// the score ring in its tone, the RAM badge, and the assigned projects. Without a score the
/// card says why over an empty track (FR-L21, #389).
fn StatusCard(c: &ComputerChip, feed: &MonitorFeed, online: bool, height: Option<f32>) {
    let p = ember_theme().palette;
    let latest = feed.latest();
    let score = latest.and_then(health_score);
    let badge = latest.and_then(ram_badge);
    let storage = latest.and_then(storage_line);
    MonCard(super::shell::ICON_GAUGE_SVG, "상태", badge.as_deref().map(|b| (b, PillTone::Grey)), height, || {
        cu::Row().alignment(Alignment::CenterStart).space_role(SpaceRole::Md).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Column().spacing(4.0).modifier(Modifier::new().weight(1.0)).content(|| {
                match score {
                    Some(n) => {
                        cu::Row().alignment(Alignment::BottomStart).space_role(SpaceRole::Sm).content(|| {
                            Label(&n.to_string(), Fs::Xl4, Weight::Semibold, None, Some(1));
                            Label(score_word(n), Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
                        });
                        if let Some(line) = &storage {
                            Label(line, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(2));
                        }
                    }
                    None => Label(&status_line(feed, online), Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(3)),
                }
            });
            let fill = score.map(|n| {
                let tone = match score_tone(n) {
                    crate::ui::components::PillTone::Green => p.green.strong,
                    crate::ui::components::PillTone::Orange => p.orange.strong,
                    _ => p.red.strong,
                };
                (tone.to_argb(), n as f32 / 100.0)
            });
            super::home::ring_image(72.0, 8.0, fill.as_slice());
        });
        if c.projects.is_empty() {
            Label("할당된 프로젝트가 없습니다", Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(3));
        } else {
            cu::Row().space_role(SpaceRole::Xs).modifier(Modifier::new().fill_max_width()).content(|| {
                for pr in c.projects.iter() {
                    Pill(&display_name(pr), PillTone::Grey, true);
                }
            });
        }
        if score.is_some() {
            Label(&status_line(feed, online), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
        }
    });
}

/// The fan card: the speed stays 준비 중입니다 and the mode control (자동, 냉각, 최대) is shown
/// disabled with the reason, because the node neither reports fans nor sets a mode
/// (FR-X7, TODO(#389)). TODO(compose-rust#473): no radio role or checked state on the buttons.
fn FanCard(height: Option<f32>) {
    let p = ember_theme().palette;
    MonCard(metric_icon(Metric::Fans), Metric::Fans.title(), None, height, || {
        cu::Row().alignment(Alignment::BottomStart).space_role(SpaceRole::Xs).content(|| {
            Label("-", Fs::Xl4, Weight::Semibold, Some(paint(p.muted)), Some(1));
            Label("RPM", Fs::Lg, Weight::Medium, Some(paint(p.muted)), Some(1));
            Label(empty_text(Metric::Fans, true, FeedStatus::Live), Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
        });
        cu::Row().space_role(SpaceRole::Xs).modifier(Modifier::new().fill_max_width()).content(|| {
            for mode in FanMode::ALL {
                let kind = if mode == FanMode::Auto { ButtonKind::Primary } else { ButtonKind::Default };
                AppButton(mode.label(), None, kind, true, FAN_MODE_AVAILABLE, false, || {});
            }
        });
        Label(FAN_MODE_REASON, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
    });
}

fn MetricCard(metric: Metric, view: Option<CardView>, empty: &str, meter: Option<f32>, height: Option<f32>) {
    let p = ember_theme().palette;
    MonCard(metric_icon(metric), metric.title(), None, height, || match view.as_ref() {
        Some(v) => {
            Label(&v.headline, Fs::Xl4, Weight::Semibold, None, Some(1));
            for line in v.lines.iter().take(4) {
                Label(line, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(1));
            }
            Bars(metric, &v.bars);
            if let Some(f) = meter {
                Meter(f);
            }
        }
        None => Label(empty, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(3)),
    });
}

/// One value against a track: the track in surface-2, the fill in the accent, both rounded.
fn Meter(fraction: f32) {
    let p = ember_theme().palette;
    let f = bar_fraction(fraction, 1.0);
    cu::Row()
        .modifier(Modifier::new().fill_max_width().height(8.0).background(paint(p.surface_2)).corner_radius(4.0).clip(true))
        .content(move || {
            if f > 0.0 {
                cu::Spacer().modifier(Modifier::new().weight(f).height(8.0).background(paint(p.blue.strong)).corner_radius(4.0));
            }
            if f < 1.0 {
                cu::Spacer().modifier(Modifier::new().weight(1.0 - f).height(8.0));
            }
        });
}

/// A row of thin bars, one per core (CPU) or per recent sample, drawn from the bottom.
fn Bars(metric: Metric, bars: &[f32]) {
    if bars.is_empty() {
        return;
    }
    let p = ember_theme().palette;
    let max = bar_max(metric, bars);
    const H: f32 = 32.0;
    cu::Row().alignment(Alignment::BottomStart).spacing(2.0).modifier(Modifier::new().fill_max_width().height(H)).content(|| {
        for v in bars.iter().copied() {
            let h = (bar_fraction(v, max) * H).max(1.0);
            cu::Spacer().modifier(Modifier::new().weight(1.0).height(h).background(paint(p.blue.strong)));
        }
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
                            Label("전력", Fs::Xs, Weight::Medium, Some(paint(p.muted)), Some(1));
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
                    Label("이 컴퓨터에서 실행 중인 팀원이 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
                });
            }
        });
    });
}

/// One row of the process table. A member's agent process (`tr.mine`) has a 3 px blue bar, a
/// 12 % blue wash and the member name in blue, and opens its session (FR-L21).
/// The row is the container's `on_click` (compose-rust#372), named by its text.
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
                    Label(&text, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(1));
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
#[compose_rust::runtime::Composable]
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
    use crate::ui::project::SOON;

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

    fn live_feed() -> MonitorFeed {
        let mk = |at_ms: u64, rx: u64| -> ember_client::api::MetricsSample {
            serde_json::from_value(serde_json::json!({
                "at_ms": at_ms,
                "cpu": {"total_percent": 25.0, "cores": [10.0, 40.0]},
                "memory": {"total_bytes": 17179869184u64, "used_bytes": 8589934592u64,
                           "swap_total_bytes": 4294967296u64, "swap_used_bytes": 1073741824u64},
                "disks": [{"name": "d", "mount": "/", "total_bytes": 100, "available_bytes": 40}],
                "network": [{"interface": "en0", "rx_bytes": rx, "tx_bytes": 0}],
                "gpu": {"devices": [], "reason": "GPU를 읽을 수 없습니다"},
                "processes": [{"pid": 7, "name": "node", "cpu_percent": 3.0, "memory_bytes": 1048576}],
                "process_count": 321
            }))
            .unwrap()
        };
        let mut f = MonitorFeed::default();
        f.push(mk(1000, 0));
        f.push(mk(2000, 2048));
        f
    }

    fn composed_text(f: fn()) -> String {
        let mut host = Host::new(move || Box::new(Recomposer::new(move || f())));
        let batch = compose_rust::protocol::decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        batch.iter().map(|m| format!("{m:?}")).collect()
    }

    #[test]
    fn fr_l21_the_cards_draw_live_values() {
        let _guard = crate::services::test_guard();
        fn ui() {
            Grid(1, &chip(), &live_feed(), true);
        }
        let text = composed_text(ui);
        for want in ["25.0%", "50.0%", "스왑 1.0 GB / 4.0 GB", "60.0%", "2.0 KB/s", "GPU를 읽을 수 없습니다", "프로세스 321개 실행 중입니다", "준비 중입니다"] {
            assert!(text.contains(want), "missing {want}");
        }
    }

    #[test]
    fn fr_l21_cards_say_why_they_are_empty() {
        let _guard = crate::services::test_guard();
        fn waiting() {
            Grid(1, &chip(), &MonitorFeed::default(), true);
        }
        fn failed() {
            let mut f = MonitorFeed::default();
            f.fail("server answered 502");
            Grid(1, &chip(), &f, true);
        }
        fn offline() {
            Grid(1, &chip(), &MonitorFeed::default(), false);
        }
        assert!(composed_text(waiting).contains("불러오는 중입니다"));
        assert!(composed_text(failed).contains("지표를 읽지 못했습니다"));
        assert!(composed_text(offline).contains("연결이 끊겨 값을 읽을 수 없습니다"));
    }

    fn has_text_with_max_lines_1(batch: &[compose_rust::protocol::Mutation<'_>], prefix: &str) -> bool {
        use compose_rust::protocol::{Mutation, PropertyValue};
        use compose_rust::PropertyKind;
        let mut target_nodes = Vec::new();
        for m in batch {
            if let Mutation::SetProp { node_id, property: PropertyKind::Text, value: PropertyValue::String(s) } = m {
                if s.starts_with(prefix) {
                    target_nodes.push(*node_id);
                }
            }
        }
        for node_id in target_nodes {
            for m in batch {
                if let Mutation::SetProp { node_id: nid, property: PropertyKind::MaxLines, value: PropertyValue::Integer(n) } = m {
                    if *nid == node_id && *n == 1 {
                        return true;
                    }
                }
            }
        }
        false
    }

    #[test]
    fn fr_l10_1_table_headers_and_cells_emit_single_line_max_lines() {
        let _guard = crate::services::test_guard();
        let c = chip();
        let mut host = Host::new(move || {
            let c = c.clone();
            Box::new(Recomposer::new(move || MonitorSheet(Some(&c), &[], || {})))
        });
        let batch = compose_rust::protocol::decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        for label in ["전력", "메모리", "CPU", "PID", "이름 (0)"] {
            assert!(
                has_text_with_max_lines_1(&batch, label),
                "header text '{label}' should emit MaxLines(1)"
            );
        }
    }

    #[test]
    fn fr_l10_1_process_row_cells_emit_single_line_max_lines() {
        let mut host = Host::new(|| {
            Box::new(Recomposer::new(|| {
                let row = ProcessRow {
                    pid: Some(1234),
                    name: "ember-agent".into(),
                    agent: "claude".into(),
                    leader: false,
                    cpu: Some(12.5),
                    mem: Some(256),
                    session: None,
                };
                ProcessRowView(&row, |_| {});
            }))
        });
        let batch = compose_rust::protocol::decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        assert!(has_text_with_max_lines_1(&batch, "1234"));
        assert!(has_text_with_max_lines_1(&batch, "ember-agent"));
    }
    #[test]
    fn fr_l21_health_status_card_shows_the_score_word_ram_and_storage() {
        let _guard = crate::services::test_guard();
        fn ui() {
            Grid(1, &chip(), &live_feed(), true);
        }
        let text = composed_text(ui);
        // 25 % CPU, 50 % memory, no sensor, 40 % free: 100 - 6.25 = 94.
        for want in ["\"94\"", "아주 좋습니다", "저장 공간이 넉넉합니다", "16.0 GB", "프로세스 321개 실행 중입니다"] {
            assert!(text.contains(want), "missing {want}");
        }
    }

    #[test]
    fn fr_l21_health_status_card_without_a_sample_says_why() {
        let _guard = crate::services::test_guard();
        fn ui() {
            Grid(1, &chip(), &MonitorFeed::default(), true);
        }
        let text = composed_text(ui);
        assert!(text.contains("불러오는 중입니다"));
        assert!(!text.contains("아주 좋습니다"));
    }

    #[test]
    fn fr_l21_health_cards_are_200_high_in_a_grid_and_free_in_one_column() {
        assert_eq!(card_height(1), None);
        assert_eq!(card_height(2), Some(200.0 - 2.0 * PAD_CARD_V));
        assert_eq!(card_height(4), Some(200.0 - 2.0 * PAD_CARD_V));
    }

    #[test]
    fn fr_l21_fan_mode_buttons_are_named_and_disabled_with_the_reason() {
        use compose_rust::protocol::{Mutation, PropertyValue};
        use compose_rust::PropertyKind;
        let _guard = crate::services::test_guard();
        let mut host = Host::new(|| Box::new(Recomposer::new(|| FanCard(None))));
        let batch = compose_rust::protocol::decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let text: String = batch.iter().map(|m| format!("{m:?}")).collect();
        assert!(text.contains(FAN_MODE_REASON));
        assert!(text.contains("준비 중입니다"));
        assert!(text.contains("RPM"));
        for mode in FanMode::ALL {
            let node = batch
                .iter()
                .find_map(|m| match m {
                    Mutation::SetProp { node_id, property: PropertyKind::Text, value: PropertyValue::String(s) } if *s == mode.label() => {
                        Some(*node_id)
                    }
                    Mutation::SetText { node_id, text, .. } if *text == mode.label() => Some(*node_id),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no button named {}", mode.label()));
            let enabled = batch.iter().find_map(|m| match m {
                Mutation::SetProp { node_id, property: PropertyKind::Enabled, value: PropertyValue::Bool(b) } if *node_id == node => Some(*b),
                _ => None,
            });
            assert_eq!(enabled, Some(false), "{} must be disabled", mode.label());
        }
    }
}
