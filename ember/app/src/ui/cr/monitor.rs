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
use compose_rust::ui::Modifier;
use ember_client::state::Reachability;
use compose_rust::{Alignment, SpaceRole};

pub use crate::ui::monitor::{
    agent_rows, computer_members, cpu_text, fold_samples, mem_text, offline_notice,
    sort_rows, toggle_sort, ComputerMetrics, Metric, MonitorFeed, NoMetrics, ProcessRow, SortKey,
    SortState,
};
use crate::ui::monitor::{
    bar_fraction, bar_max, card_view, chip_badge, hero_parts, max_process_cpu, member_note, member_rows,
    meter_fraction, metric_badge, net_graph_svg, empty_text, health_score, power_text, process_footer,
    score_tone, sparkline_svg, status_badges, table_layout, temperature_readings, title_os,
    CORE_BARS_GAP, CORE_BARS_H, CORE_BAR_ALPHA, CORE_BAR_RADIUS, GRAPH_HEIGHT,
    score_word, status_line, storage_line, CardView, FanMode, FeedStatus, TableLayout,
    FAN_MODE_AVAILABLE, FAN_MODE_REASON, POLL_SECS, TOP_PROCESSES,
};
use super::characters::{Character, CharState};
use super::components::{
    Choice, ChoiceButton, EmberCard, EmberSheetSized, Label, Pill, PillTone, SheetTitle, SHEET_WIDTH_MONITOR,
    WrapLabel,
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
        SortKey::Power => "전력",
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
        let feed_w = feed_state;
        let demo = crate::services::services().demo.is_some() || crate::demo::active_from_env();
        launched_effect((id.clone(), poll), move || async move {
            if !poll || id.is_empty() {
                return;
            }
            if demo {
                // Seed a short history so the memory area graph and network dual-area graph
                // render full curves on the first frame.
                let now = crate::demo::now_ms().max(0) as u64;
                for step in (1..=6u64).rev() {
                    let at = now.saturating_sub(step * POLL_SECS * 1000);
                    feed_w.update(|f| f.push(crate::demo::metrics_sample(at)));
                }
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
    let name = chip.map(|c| c.name.clone()).unwrap_or_else(|| "컴퓨터".to_string());
    let window = crate::ui::window_size::current_window_size();
    // `.mon-os`: the OS the node reports, beside the title (#389), not at 560 and below.
    let os = title_os(&feed, window.width_dp);
    let title = SheetTitle { text: &name, subtitle: os.as_deref() };
    let columns = mon_columns(window.width_dp);
    // The header (60), the 1 dp line under it and, where the sheet is the whole window, the
    // strip above it (FR-L33).
    let top_inset = super::components::full_screen_sheet_top_inset(window.width_dp, true, crate::ui::safe_area::safe_area());
    let body_height = super::components::sheet_body_max_height(window.width_dp, window.height_dp, true, MON_CHROME_H + top_inset).min(1200.0);
    let scroll_pad = mon_scroll_pad(window.width_dp);
    let state = chip.map(|c| reach_pill(c.reachability));

    // The state pill sits in the header, beside the close button (`.mon-state`).
    let mut actions = || {
        if let Some((text, tone)) = state {
            Pill(text, tone.into(), false);
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
        }
    };
    #[allow(clippy::needless_borrows_for_generic_args)]
    EmberSheetSized(&title, true, SHEET_WIDTH_MONITOR, true, on_close.clone(), Some(&mut actions), || {
        { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().fill_max_width().height(body_height).background(paint(p.bg)), {
                { use ::compose_rust::ui::Column; Column!(space_role = SpaceRole::Md, modifier = Modifier::new().fill_max_width().padding(scroll_pad), {
                    let Some(c) = chip else {
                        { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().fill_max_width().height(240.0), {
                            Label("컴퓨터를 찾을 수 없습니다", Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
                        }) };
                        return;
                    };
                    let online = matches!(c.reachability, Reachability::Online);
                    let members = computer_members(&c.projects, boxes);
                    // The live rows with the members' agent processes marked by pid (FR-X7,
                    // #389); before a sample, the members alone.
                    let sessions = feed.latest().map(|s| s.sessions.clone()).unwrap_or_default();
                    let mut rows = match feed.processes() {
                        Some(live) => member_rows(live, &sessions, &members),
                        None => agent_rows(&members),
                    };
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
                    { use ::compose_rust::ui::Column; Column!(space_role = SpaceRole::Md, modifier = wrap.fill_max_width(), {
                        Grid(columns, c, &feed, online);
                        ProcessTable(&rows, cur, &sort, window.width_dp, &on_close);
                        Label(
                            &process_footer(&feed),
                            Fs::Xs,
                            Weight::Regular,
                            Some(paint(p.text_2)),
                            None,
                        );
                        // `.mon-note`: a member row opens the session.
                        if let Some(note) = member_note(&rows) {
                            Label(note, Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                        }
                    }) };
                }) };
            }) };
    });
}

/// What the monitor sheet draws above its scrolling body: the 60 dp header and the 1 dp line
/// under it. The body was sized for the header alone, so it ran 1 dp past the dialog's edge
/// (review sf A: the 1440 footer note was cut at the dialog's bottom edge).
pub const MON_CHROME_H: f32 = 60.0 + 1.0;

/// The scrolling body's padding, the room under the last line when it is scrolled into view:
/// `.mon-scroll{padding:var(--sp5)}` (20), 16 at 560 and below (`.mon-scroll{padding:var(--sp4)}`).
pub fn mon_scroll_pad(width_dp: f32) -> f32 {
    if width_dp <= 560.0 { 16.0 } else { 20.0 }
}

/// `.mon-off`: a red soft banner with the notice, for a computer that is not online.
fn OfflineBanner(title: &str) {
    let p = ember_theme().palette;
    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::TopStart, space_role = SpaceRole::Sm, modifier = Modifier::new()
                .fill_max_width()
                .background(paint(p.red.soft))
                .corner_radius(CORNER_MD)
                .padding(SP[2]), {
            // The glyph's circle fills 18 of 24 units, so 24 px draws the mockup's 20 px mark.
            super::shell::NavIcon(super::shell::ICON_INFO_SVG, p.red.strong.to_argb(), 24.0);
            { use ::compose_rust::ui::Column; Column!(spacing = 2.0, {
                Label(title, Fs::Md, Weight::Semibold, Some(paint(p.red.strong)), None);
                Label(
                    "마지막 연결 시각은 아직 알 수 없습니다. 아래는 마지막으로 받은 값입니다.",
                    Fs::Sm,
                    Weight::Regular,
                    Some(paint(p.text_2)),
                    None,
                );
            }) };
        }) };
}

/// A red soft notice for a failed request; the cards keep the last values.
fn FailureNotice(detail: &str) {
    let p = ember_theme().palette;
    { use ::compose_rust::ui::Column; Column!(spacing = 2.0, modifier = Modifier::new().fill_max_width().background(paint(p.red.soft)).corner_radius(CORNER_MD).padding(SP[2]), {
            Label("지표를 읽지 못했습니다", Fs::Md, Weight::Semibold, Some(paint(p.red.strong)), Some(1));
            Label(detail, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(2));
        }) };
}

/// The metric cards: the status card first, then CPU, GPU, memory, temperature, disk, network
/// and fans, `columns` to a row (`.mon-grid`). In a grid a row is at least 200 high (`.mon-card`
/// `min-height: 200px`) and as high as its tallest card, and every card in it fills that height;
/// in one column a card is as high as its content (`min-height:0`). A fixed 200 cut the 상태
/// card's last line at 1440 (review r6).
fn Grid(columns: usize, c: &ComputerChip, feed: &MonitorFeed, online: bool) {
    let cards: Vec<Card> = std::iter::once(Card::Status)
        .chain(Metric::ALL.iter().copied().map(Card::Metric))
        .collect();
    let min = card_min_height(columns);
    let fill = min.is_some();
    for chunk in cards.chunks(columns) {
        { use ::compose_rust::ui::Row; Row!(space_role = SpaceRole::Md, intrinsic_size = fill, modifier = Modifier::new().fill_max_width(), {
            for card in chunk.iter().copied() {
                let cell = Modifier::new().weight(1.0);
                // A Box stacks the card on a zero-width floor as high as the card minimum, so
                // the cell is max(200, its card) and the row takes its tallest cell.
                { use ::compose_rust::ui::Box; Box!(modifier = if fill { cell.fill_max_height() } else { cell }, {
                    if let Some(h) = min {
                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(0.0).height(h)) };
                    }
                    match card {
                        Card::Status => StatusCard(c, feed, online, fill),
                        Card::Metric(Metric::Fans) => FanCard(fill),
                        Card::Metric(Metric::Temperature) => TemperatureCard(feed, online, fill),
                        Card::Metric(m) => {
                            let badge = feed.latest().and_then(|s| metric_badge(m, s));
                            MetricCard(
                                m,
                                card_view(m, feed),
                                badge,
                                empty_text(m, online, feed.status()),
                                meter_fraction(m, feed),
                                feed.net_tx_series(),
                                fill,
                            )
                        }
                    }
                }) };
            }
            for _ in chunk.len()..columns {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
            }
        }) };
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

/// A card's minimum height: 200 in a grid (`.mon-card{min-height:200px}`), none in one column.
pub fn card_min_height(columns: usize) -> Option<f32> {
    (columns > 1).then_some(200.0)
}

/// `.mon-card`: a 16 px icon, a 13 px semibold `--text-2` label and its badges on the right
/// (`.mon-badge`, the status card has up to three). With `fill` the card takes its grid row's
/// height.
fn MonCard(icon: &'static str, label: &str, badges: &[(&str, PillTone)], fill: bool, body: impl FnOnce()) {
    let p = ember_theme().palette;
    super::components::EmberCardSized(false, true, fill, None, None, None, || {
        let m = Modifier::new().fill_max_width();
        { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = m, {
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Xs, modifier = Modifier::new().fill_max_width(), {
                super::shell::NavIcon(icon, p.muted.to_argb(), 16.0);
                // A long label wraps to a second line, breaking a long token inside itself
                // (compose-rust#443, v0.0.8), instead of clipping with an ellipsis.
                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(1.0), {
                    WrapLabel(label, Fs::Sm, Weight::Semibold, Some(paint(p.text_2)), Some(2));
                }) };
                for (text, tone) in badges.iter().copied() {
                    Pill(text, tone, true);
                }
            }) };
            body();
        }) };
    });
}

/// The status card (`.mon-score`): the health score with its word, the main disk's free space,
/// the score ring in its tone, the chip, RAM and OS badges, and the assigned projects. Without a score the
/// card says why over an empty track (FR-L21, #389).
fn StatusCard(c: &ComputerChip, feed: &MonitorFeed, online: bool, fill: bool) {
    let p = ember_theme().palette;
    let latest = feed.latest();
    let score = latest.and_then(health_score);
    // Chip, RAM and OS, as reported (#389).
    let badges = latest.map(status_badges).unwrap_or_default();
    let header: Vec<(&str, PillTone)> = badges.iter().map(|b| (b.as_str(), PillTone::Grey)).collect();
    let storage = latest.and_then(storage_line);
    MonCard(super::shell::ICON_GAUGE_SVG, "상태", &header, fill, || {
        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Md, modifier = Modifier::new().fill_max_width(), {
            { use ::compose_rust::ui::Column; Column!(spacing = 4.0, modifier = Modifier::new().weight(1.0), {
                match score {
                    Some(n) => {
                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::BottomStart, space_role = SpaceRole::Sm, {
                            Label(&n.to_string(), Fs::Xl4, Weight::Semibold, None, Some(1));
                            Label(score_word(n), Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
                        }) };
                        if let Some(line) = &storage {
                            Label(line, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(2));
                        }
                    }
                    None => Label(&status_line(feed, online), Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(3)),
                }
            }) };
            let fill = score.map(|n| {
                let tone = match score_tone(n) {
                    crate::ui::components::PillTone::Green => p.green.strong,
                    crate::ui::components::PillTone::Orange => p.orange.strong,
                    _ => p.red.strong,
                };
                (tone.to_argb(), n as f32 / 100.0)
            });
            super::home::ring_image(72.0, 8.0, fill.as_slice());
        }) };
        if c.projects.is_empty() {
            Label("할당된 프로젝트가 없습니다", Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(3));
        } else {
            { use ::compose_rust::ui::Row; Row!(space_role = SpaceRole::Xs, modifier = Modifier::new().fill_max_width(), {
                for pr in c.projects.iter() {
                    Pill(&display_name(pr), PillTone::Grey, true);
                }
            }) };
        }
        if score.is_some() {
            Label(&status_line(feed, online), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
        }
    });
}

/// The fan card: the speed stays 준비 중입니다 and the mode control (자동, 냉각, 최대) is shown
/// disabled with the reason, because the node neither reports fans nor sets a mode
/// (FR-X7, TODO(#389)). The mode buttons are radio buttons to assistive technology, with 자동
/// checked (`Choice::Radio`, compose-rust#473, v0.0.9). `Row::radio_group` (#545) moves focus
/// between toolkit `RadioButton`s only, so the row does not claim it for pill buttons.
fn FanCard(fill: bool) {
    let p = ember_theme().palette;
    MonCard(metric_icon(Metric::Fans), Metric::Fans.title(), &[], fill, || {
        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::BottomStart, space_role = SpaceRole::Xs, {
            Label("-", Fs::Xl4, Weight::Semibold, Some(paint(p.muted)), Some(1));
            Label("RPM", Fs::Lg, Weight::Medium, Some(paint(p.muted)), Some(1));
            Label(empty_text(Metric::Fans, true, FeedStatus::Live), Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
        }) };
        { use ::compose_rust::ui::Row; Row!(space_role = SpaceRole::Xs, modifier = Modifier::new().fill_max_width(), {
            for mode in FanMode::ALL {
                ChoiceButton(mode.label(), Choice::Radio, mode == FanMode::Auto, true, FAN_MODE_AVAILABLE, || {});
            }
        }) };
        Label(FAN_MODE_REASON, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
    });
}

/// The temperature card: CPU and GPU readings side by side (`.mon-temps`) with one
/// tone-colored meter bar per sensor (`.mon-tbars`), and the chip name as the footer.
fn TemperatureCard(feed: &MonitorFeed, online: bool, fill: bool) {
    let p = ember_theme().palette;
    let badge = feed.latest().and_then(|s| metric_badge(Metric::Temperature, s));
    let header_badge: Vec<(&str, PillTone)> = badge.iter().map(|(text, tone)| (text.as_str(), PillTone::from(*tone))).collect();
    MonCard(metric_icon(Metric::Temperature), Metric::Temperature.title(), &header_badge, fill, || {
        let Some(s) = feed.latest() else {
            Label(empty_text(Metric::Temperature, online, feed.status()), Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(3));
            return;
        };
        let readings = temperature_readings(s);
        if readings.is_empty() {
            HeroValue("-", false);
            Label("이 컴퓨터가 온도를 알려주지 않습니다", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
            return;
        }
        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::BottomStart, spacing = SP[5], modifier = Modifier::new().fill_max_width(), {
            for r in &readings {
                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::BottomStart, spacing = SP[0], {
                    Label(&r.label, Fs::Xs, Weight::Medium, Some(paint(p.muted)), Some(1));
                    Label(&format!("{}", r.celsius.round() as i64), Fs::Xl2, Weight::Semibold, None, Some(1));
                    Label("\u{00B0}C", Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
                }) };
            }
        }) };
        { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
            for r in &readings {
                let color = match r.tone {
                    crate::ui::components::PillTone::Red => p.red.strong,
                    crate::ui::components::PillTone::Orange => p.orange.strong,
                    crate::ui::components::PillTone::Green => p.green.strong,
                    _ => p.blue.strong,
                };
                TonedMeter(bar_fraction(r.celsius, 100.0), color);
            }
        }) };
        if let Some(chip) = chip_badge(s) {
            Label(&chip, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
        }
    });
}

fn MetricCard(
    metric: Metric,
    view: Option<CardView>,
    badge: Option<(String, crate::ui::components::PillTone)>,
    empty: &str,
    meter: Option<f32>,
    net_tx: &[f32],
    fill: bool,
) {
    let p = ember_theme().palette;
    let header_badge: Vec<(&str, PillTone)> = badge.iter().map(|(text, tone)| (text.as_str(), PillTone::from(*tone))).collect();
    MonCard(metric_icon(metric), metric.title(), &header_badge, fill, || match view.as_ref() {
        Some(v) => {
            HeroValue(&v.headline, !fill);
            match metric {
                Metric::Cpu => Bars(metric, &v.bars),
                Metric::Gpu if !v.bars.is_empty() => {
                    SvgGraph(sparkline_svg(&v.bars, 100.0, p.orange.strong.to_argb(), false));
                }
                Metric::Memory if v.bars.len() >= 2 => {
                    SvgGraph(sparkline_svg(&v.bars, 100.0, p.blue.strong.to_argb(), true));
                }
                Metric::Network if v.bars.len() >= 2 && net_tx.len() >= 2 => {
                    SvgGraph(net_graph_svg(
                        net_tx,
                        &v.bars,
                        p.green.strong.to_argb(),
                        p.blue.strong.to_argb(),
                        p.line.to_argb(),
                    ));
                }
                _ => {
                    Bars(metric, &v.bars);
                    if let Some(f) = meter {
                        Meter(f);
                    }
                }
            }
            for line in v.lines.iter().take(4) {
                Label(line, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
            }
        }
        None => Label(empty, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(3)),
    });
}

fn SvgGraph(doc: String) {
    if doc.is_empty() {
        return;
    }
    { use ::compose_rust::ui::Image; Image!(compose_rust::asset(compose_rust::AssetKind::Svg, super::shell::leak_doc(doc)), modifier = Modifier::new().fill_max_width().height(GRAPH_HEIGHT)) };
}

/// `.mon-big`: the number at 40/600 (32/600 on a phone) and its unit at 15/500 `muted` beside
/// it, bottoms aligned, 4 apart; whole numbers for a percent or a temperature.
fn HeroValue(headline: &str, compact: bool) {
    let p = ember_theme().palette;
    let (number, unit) = hero_parts(headline);
    let num_fs = if compact { Fs::Xl3 } else { Fs::Xl4 };
    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::BottomStart, spacing = SP[0], {
        Label(&number, num_fs, Weight::Semibold, None, Some(1));
        if !unit.is_empty() {
            Label(&unit, Fs::Lg, Weight::Medium, Some(paint(p.muted)), Some(1));
        }
    }) };
}

/// One value against a track (`.mon-bar`): the track in `track`, the fill in the accent, both
/// rounded.
fn Meter(fraction: f32) {
    TonedMeter(fraction, ember_theme().palette.blue.strong);
}

fn TonedMeter(fraction: f32, color: compose_rust::Color) {
    let p = ember_theme().palette;
    let f = bar_fraction(fraction, 1.0);
    { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width().height(8.0).background(paint(p.track)).corner_radius(4.0).clip(true), content = move || {
            if f > 0.0 {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(f).height(8.0).background(paint(color)).corner_radius(4.0)) };
            }
            if f < 1.0 {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0 - f).height(8.0)) };
            }
        }) };
}

/// A row of thin bars drawn from the bottom: the CPU's cores as the mockup's `.mon-cores` (44
/// high, 3 apart, green with 2 dp top corners at 85 percent; review M2), the recent samples of
/// the other cards 32 high in blue.
fn Bars(metric: Metric, bars: &[f32]) {
    if bars.is_empty() {
        return;
    }
    let p = ember_theme().palette;
    let max = bar_max(metric, bars);
    let cores = metric == Metric::Cpu;
    let (h_all, gap, tone) = if cores { (CORE_BARS_H, CORE_BARS_GAP, p.green.strong) } else { (32.0, 2.0, p.blue.strong) };
    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::BottomStart, spacing = gap, modifier = Modifier::new().fill_max_width().height(h_all), {
        for v in bars.iter().copied() {
            let h = (bar_fraction(v, max) * h_all).max(1.0);
            let bar = Modifier::new().weight(1.0).height(h).background(paint(tone));
            let bar = if cores {
                bar.corner_radii([CORE_BAR_RADIUS, CORE_BAR_RADIUS, 0.0, 0.0]).alpha(CORE_BAR_ALPHA)
            } else {
                bar
            };
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = bar) };
        }
    }) };
}

/// `.mon-tablebox`: a card with a sticky header and 1 px row dividers.
fn ProcessTable(rows: &[ProcessRow], cur: SortState, sort: &MutableState<SortState>, width_dp: f32, on_close: &(impl FnMut() + Clone + 'static)) {
    let p = ember_theme().palette;
    let ui = shared_ui().clone();
    let layout = table_layout(width_dp);
    let max_cpu = max_process_cpu(rows);
    EmberCard(false, true, None, Some(0.0), None, || {
        { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), {
            // Header: 12 px muted, the sorted one in `--text`, numbers right-aligned.
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width().height(H_CTL), {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[3])) };
                // The mockup's columns (`COLS`): 이름, PID, CPU, 전력, 메모리.
                for key in [SortKey::Name, SortKey::Pid, SortKey::Cpu, SortKey::Power, SortKey::Memory] {
                    let sort = *sort;
                    let (width, end) = match key {
                        SortKey::Name => (None, false),
                        SortKey::Pid => (Some(layout.pid_w), true),
                        SortKey::Cpu => (Some(layout.cpu_w), true),
                        SortKey::Power => (Some(layout.pwr_w), true),
                        SortKey::Memory => (Some(layout.mem_w), true),
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
                }
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[3])) };
            }) };
            for (i, r) in rows.iter().enumerate() {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().fill_max_width().height(1.0).background(paint(p.line))) };
                let _ = i;
                let (ui, mut close) = (ui.clone(), on_close.clone());
                ProcessRowViewWith(r, layout, max_cpu, move |id: String| {
                    close();
                    ui.go(Route::Conversation(id));
                });
            }
            if rows.is_empty() {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().fill_max_width().height(1.0).background(paint(p.line))) };
                { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().fill_max_width().height(64.0), {
                    Label("이 컴퓨터에서 실행 중인 팀원이 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
                }) };
            }
        }) };
    });
}

/// One row of the process table using the desktop column layout.
#[cfg(test)]
fn ProcessRowView(row: &ProcessRow, on_open: impl FnMut(String) + 'static) {
    ProcessRowViewWith(row, table_layout(1440.0), 100.0, on_open);
}

/// One row of the process table. A member's agent process (`tr.mine`) has a 3 px blue bar, a
/// 12 % blue wash and the member name in blue, and opens its session.
fn ProcessRowViewWith(row: &ProcessRow, layout: TableLayout, max_cpu: f32, mut on_open: impl FnMut(String) + 'static) {
    let p = ember_theme().palette;
    let session = row.session.clone();
    let mine = session.is_some();
    let fill = if mine { paint(super::compat::mix(p.surface, p.blue.strong, 0.12)) } else { paint(p.surface) };
    let content = || {
        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width().height(ROW_H), {
            if mine {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(3.0).height(ROW_H).background(paint(p.blue.strong))) };
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[3] - 3.0)) };
            } else {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[3])) };
            }
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Sm, modifier = Modifier::new().weight(1.0), {
                if mine {
                    // `.mon-who`: the member's character and name in blue, then the process
                    // (the OS process name of a matched row, else the agent).
                    Character(&row.agent, row.leader, 22.0, &row.name, "", CharState::default(), None);
                    let who = if row.leader { format!("{} (리더)", row.name) } else { row.name.clone() };
                    Label(&who, Fs::Xs, Weight::Medium, Some(paint(p.blue.strong)), Some(1));
                    Label(row.process.as_deref().unwrap_or(row.agent.as_str()), Fs::Sm, Weight::Medium, Some(paint(p.text)), Some(1));
                } else {
                    // `.mon-pi`: 22 x 22 rounded icon tile aligned with the 22 dp member avatar.
                    { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().size(22.0, 22.0).background(paint(p.surface_2)).corner_radius(CORNER_XS).border(1.0, paint(p.line)), {
                        super::shell::NavIcon(super::shell::ICON_CPU_SVG, p.muted.to_argb(), 14.0);
                    }) };
                    Label(&row.name, Fs::Sm, Weight::Medium, Some(paint(p.text)), Some(1));
                }
            }) };
            let cell = |w: f32, text: String| {
                { use ::compose_rust::ui::Box; Box!(alignment = Alignment::CenterEnd, modifier = Modifier::new().width(w), {
                    Label(&text, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(1));
                }) };
            };
            cell(layout.pid_w, row.pid.map_or_else(|| "-".to_string(), |v| v.to_string()));
            // In-cell CPU bar beside the numeric percent: blue for member rows, muted track fill
            // for other processes, scaled against the busiest visible process.
            { use ::compose_rust::ui::Box; Box!(alignment = Alignment::CenterEnd, modifier = Modifier::new().width(layout.cpu_w), {
                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterEnd, spacing = SP[1], {
                    if let (Some(bar_w), Some(pct)) = (layout.cpu_bar_w, row.cpu) {
                        let f = bar_fraction(pct, max_cpu);
                        let bar_color = if mine { p.blue.strong } else { p.muted };
                        let r = layout.cpu_bar_h / 2.0;
                        { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().width(bar_w).height(layout.cpu_bar_h).background(paint(p.track)).corner_radius(r).clip(true), content = move || {
                                if f > 0.0 {
                                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(f).height(layout.cpu_bar_h).background(paint(bar_color)).corner_radius(r)) };
                                }
                                if f < 1.0 {
                                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0 - f).height(layout.cpu_bar_h)) };
                                }
                            }) };
                    }
                    Label(&cpu_text(row.cpu), Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(1));
                }) };
            }) };
            cell(layout.pwr_w, power_text(row.power));
            cell(layout.mem_w, mem_text(row.mem));
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[3])) };
        }) };
    };
    let box_modifier = Modifier::new().fill_max_width().background(fill);
    match session {
        Some(id) => super::components::Clickable(box_modifier, Alignment::CenterStart, move || on_open(id.clone()), content),
        None => { use ::compose_rust::ui::Box; Box!(modifier = box_modifier, content = content) },
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
    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_size(), {
        super::shell::shell();
        MonitorSheet(chips.get(index), &boxes, || {});
    }) };
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

    /// What the monitor sheet sends before its title after a window report at `width` with the
    /// safe area `top` x `leading`.
    fn monitor_head_after_report(width: f32, top: f32, leading: f32) -> String {
        let text = monitor_after_report(width, 844.0, top, leading);
        // The delta may not resend the title node; what precedes it, when it is resent.
        match text.find("이 컴퓨터") {
            Some(title) => text[..title].to_string(),
            None => text,
        }
    }

    /// Everything the monitor sheet sends after a window report of `width` x `height` with the
    /// safe area `top` x `leading`.
    fn monitor_after_report(width: f32, height: f32, top: f32, leading: f32) -> String {
        use compose_rust::protocol::HostEvent;
        use compose_rust::{EventPayload, WindowHeightClass, WindowSizeClass};
        compose_rust::window::reset_window_size();
        let c = ComputerChip { name: "이 컴퓨터".into(), ..chip() };
        let mut host = Host::new(move || {
            let c = c.clone();
            Box::new(Recomposer::new(move || MonitorSheet(Some(&c), &[], || {})))
        });
        host.rebuild().expect("rebuild");
        let payload = EventPayload::WindowSizeChanged {
            width_dp: width,
            height_dp: height,
            class: WindowSizeClass::from_width_dp(width),
            height_class: WindowHeightClass::from_height_dp(height),
            safe_area_top: top,
            safe_area_leading: leading,
            safe_area_trailing: 0.0,
            safe_area_bottom: 0.0,
        };
        let (bytes, _) = host.dispatch(HostEvent::new(0, 0, payload)).expect("dispatch");
        let text: String = compose_rust::protocol::decode_batch(bytes).expect("decode").iter().map(|m| format!("{m:?}")).collect();
        compose_rust::window::reset_window_size();
        text
    }

    #[test]
    fn fr_l33_the_monitor_dialog_starts_below_the_window_buttons() {
        // Review sf: from 561 up (and at 1440) the monitor dialog filled the window less 16 above
        // and below, so its header sat under the traffic lights. A dialog now leaves the lights'
        // strip above and below; centred, its top edge is below the strip.
        use crate::ui::safe_area::{safe_area_for, Platform, SafeArea};
        use super::super::components::dialog_max_height;
        let lights = safe_area_for(Platform::MacOs, SafeArea { top: 66.0, leading: 98.0, trailing: 0.0 });
        assert_eq!(dialog_max_height(900.0, lights), 768.0, "66 above and below");
        assert_eq!(dialog_max_height(900.0, SafeArea::default()), 868.0, "16 above and below with no report");
        assert_eq!(dialog_max_height(844.0, SafeArea { top: 8.0, leading: 40.0, trailing: 0.0 }), 812.0, "never under 16");
        assert_eq!(dialog_max_height(844.0, SafeArea { top: 32.0, leading: 0.0, trailing: 138.0 }), 780.0, "a Windows caption report: its 32 strip above and below");
        let _guard = crate::services::test_guard();
        // The body is the dialog less its 60 header and the 1 dp line under it:
        // 900 - 2 x 66 - 61 = 707 with the lights, 900 - 32 - 61 = 807 with a zero report.
        for width in [561.0, 1440.0] {
            let sent = monitor_after_report(width, 900.0, 66.0, 98.0);
            assert!(sent.contains("Height(707.0)") && !sent.contains("Height(807.0)"), "the dialog below the reported strip at {width}: {sent}");
            let sent = monitor_after_report(width, 900.0, 0.0, 0.0);
            assert!(sent.contains("Height(807.0)") && !sent.contains("Height(707.0)"), "no change with a zero safe area at {width}: {sent}");
        }
    }

    #[test]
    fn fr_l33_the_full_screen_monitor_header_starts_below_the_window_buttons() {
        // Review ba D: at 390 the computer monitor (and the project screen's computer sheet,
        // the same `MonitorSheet`) put "이 컴퓨터" under the macOS traffic lights.
        use crate::ui::safe_area::{safe_area_for, Platform, SafeArea};
        use super::super::components::full_screen_sheet_top_inset;
        // The pure rule names macOS, so it holds on every OS CI runs (as `cr/shell.rs` does).
        let lights = safe_area_for(Platform::MacOs, SafeArea { top: 28.0, leading: 72.0, trailing: 0.0 });
        assert_eq!(full_screen_sheet_top_inset(390.0, true, lights), 28.0);
        assert_eq!(full_screen_sheet_top_inset(560.0, true, lights), 28.0, "full screen up to 560");
        assert_eq!(full_screen_sheet_top_inset(561.0, true, lights), 0.0, "a dialog from 561");
        assert_eq!(full_screen_sheet_top_inset(390.0, false, lights), 0.0, "a bottom sheet clears them");
        assert_eq!(full_screen_sheet_top_inset(390.0, true, SafeArea::default()), 0.0, "nothing to clear");
        let _guard = crate::services::test_guard();
        // An odd footprint (31) so no other height in the sheet can stand in for the strip.
        // The composed sheet reads the report as it is on every system (FR-L33).
        let head = monitor_head_after_report(390.0, 31.0, 72.0);
        assert!(head.contains("Height(31.0)"), "the 31 strip before the title: {head}");
        let head = monitor_head_after_report(390.0, 0.0, 0.0);
        assert!(!head.contains("Height(31.0)") && !head.contains("Height(28.0)"), "no strip with a zero safe area: {head}");
    }

    #[test]
    fn fr_l21_the_monitor_body_ends_at_the_dialog_edge_with_the_mockup_padding() {
        // Review sf A: at 1440 the footer note under the process table was cut at the dialog's
        // bottom edge. The body is sized for the header and its line (61), so it ends at the
        // dialog's edge, and the scroll keeps the mockup's padding under the last line.
        assert_eq!(MON_CHROME_H, 61.0);
        assert_eq!(mon_scroll_pad(1440.0), 20.0);
        assert_eq!(mon_scroll_pad(561.0), 20.0);
        assert_eq!(mon_scroll_pad(560.0), 16.0);
        assert_eq!(mon_scroll_pad(390.0), 16.0);
        let _guard = crate::services::test_guard();
        let wide = monitor_after_report(1440.0, 900.0, 0.0, 0.0);
        assert!(wide.contains("Height(807.0)") && wide.contains("Padding(20.0)"), "{wide}");
        let phone = monitor_after_report(390.0, 844.0, 0.0, 0.0);
        // The report only sends what changed: the phone keeps the 16 it started with, so no
        // Padding(20.0) is sent, and the body is the window less the 61 dp chrome.
        assert!(phone.contains("Height(783.0)") && !phone.contains("Padding(20.0)"), "{phone}");
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
        // The big value is split into the number and its unit, a percent as a whole number
        // (`hero_parts`, review M1), so each is its own quoted text.
        for want in ["\"25\"", "\"50\"", "스왑 1.0 GB / 4.0 GB", "\"60\"", "\"2.0\"", "KB/s", "GPU를 읽을 수 없습니다", "프로세스 321개 실행 중입니다", "준비 중입니다"] {
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
                    power: None,
                    process: None,
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
        assert_eq!(card_min_height(1), None);
        assert_eq!(card_min_height(2), Some(200.0));
        assert_eq!(card_min_height(4), Some(200.0));
    }

    #[test]
    fn fr_l21_a_grid_row_is_at_least_200_and_grows_with_its_tallest_card() {
        // Review r6: the 상태 card at 1440 was a fixed 200 and cut its last line; `.mon-card`
        // is `min-height:200px` in a grid row that stretches its cards.
        use compose_rust::protocol::{decode_batch, Mutation};
        use compose_rust::{Host, Recomposer};
        let _guard = crate::services::test_guard();
        fn ui() {
            Grid(4, &chip(), &MonitorFeed::default(), true);
        }
        let mut host = Host::new(|| Box::new(Recomposer::new(ui)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let all: String = batch.iter().map(|m| format!("{m:?}")).collect();
        assert!(!all.contains(&format!("Height({:?})", 200.0 - 2.0 * PAD_CARD_V)), "no fixed inner height");
        // Every card of the two rows (4 + 4 cards) stands on the 200 floor in an equal-height row.
        let floors = batch
            .iter()
            .filter(|m| matches!(m, Mutation::SetModifier { modifier, .. } if format!("{modifier:?}") == "Height(200.0)"))
            .count();
        assert_eq!(floors, 8, "{all}");
        let production = include_str!("monitor.rs").split("#[cfg(test)]").next().unwrap();
        let grid = production.split("fn Grid(").nth(1).unwrap().split("\n}\n").next().unwrap();
        assert!(grid.contains("intrinsic_size = fill"), "{grid}");

        assert!(grid.contains("cell.fill_max_height()"), "{grid}");
        let card = production.split("fn MonCard(").nth(1).unwrap().split("\n}\n").next().unwrap();
        assert!(card.contains("EmberCardSized(false, true, fill,"), "{card}");
        assert!(!card.contains(".height(h)"), "{card}");
    }

    fn cpu_card() {
        let view = CardView { headline: "23.1%".into(), lines: vec!["코어 8개".into()], bars: vec![50.0, 100.0] };
        MetricCard(Metric::Cpu, Some(view), None, "", None, &[], false);
    }

    /// Review M1, M2: the big value is the whole number with its unit beside it in a smaller
    /// muted label, and the cores are green bars with rounded tops, 44 high.
    #[test]
    fn m1_m2_cpu_card_splits_the_number_and_draws_green_core_bars() {
        use compose_rust::protocol::{decode_batch, Mutation, PropertyValue};
        use compose_rust::PropertyKind;
        let _guard = crate::services::test_guard();
        let mut host = Host::new(|| Box::new(Recomposer::new(cpu_card)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let texts: Vec<String> = batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetProp { property: PropertyKind::Text, value: PropertyValue::String(s), .. } => Some(s.to_string()),
                Mutation::SetText { text, .. } => Some(text.to_string()),
                _ => None,
            })
            .collect();
        assert!(texts.contains(&"23".to_string()), "{texts:?}");
        assert!(texts.contains(&"%".to_string()), "{texts:?}");
        assert!(!texts.contains(&"23.1%".to_string()), "no one-label value with a decimal");
        let p = ember_theme().palette;
        let green = format!("{:?}", compose_rust::schema::Modifier::Background(paint(p.green.strong)));
        let mods_of = |id: u32| -> Vec<String> {
            batch
                .iter()
                .filter_map(|m| match m {
                    Mutation::SetModifier { node_id, modifier, .. } if *node_id == id => Some(format!("{modifier:?}")),
                    _ => None,
                })
                .collect()
        };
        let bars: Vec<u32> = batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetModifier { node_id, modifier, .. } if format!("{modifier:?}") == green => Some(*node_id),
                _ => None,
            })
            .collect();
        assert_eq!(bars.len(), 2, "one green bar per core");
        for id in bars {
            let mods = mods_of(id);
            assert!(mods.iter().any(|m| m == "Alpha(0.85)"), "{mods:?}");
            assert!(mods.iter().any(|m| m.starts_with("CornerEach") || m.starts_with("Shape")), "rounded top: {mods:?}");
        }
        let all: String = batch.iter().map(|m| format!("{m:?}")).collect();
        assert!(all.contains("Height(44.0)"), "the core row is 44 high");
    }

    #[test]
    fn fr_l21_fan_mode_buttons_are_named_and_disabled_with_the_reason() {
        use compose_rust::protocol::{Mutation, PropertyValue};
        use compose_rust::PropertyKind;
        let _guard = crate::services::test_guard();
        let mut host = Host::new(|| Box::new(Recomposer::new(|| FanCard(false))));
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
            // The choice is Ember's pill (review r5): a disabled one has no click handler on its
            // label or any node above it.
            let parent = |id: u32| {
                batch.iter().find_map(|m| match m {
                    Mutation::Insert { parent_id, node_id, .. } if *node_id == id => Some(*parent_id),
                    _ => None,
                })
            };
            let clickable = |id: u32| {
                batch.iter().any(|m| matches!(m, Mutation::SetProp { node_id, property: PropertyKind::OnClick, .. } if *node_id == id))
            };
            let mut at = Some(node);
            while let Some(id) = at {
                assert!(!clickable(id), "{} must be disabled", mode.label());
                at = parent(id);
            }
        }
    }

    // ---- #389: chip and OS badges, member rows and power ----

    fn telemetry_feed() -> MonitorFeed {
        let s: ember_client::api::MetricsSample = serde_json::from_value(serde_json::json!({
            "at_ms": 1000,
            "chip": "Apple M2 Pro",
            "os_name": "macOS",
            "os_version": "15.2",
            "cpu": {"total_percent": 25.0, "cores": [10.0, 40.0]},
            "memory": {"total_bytes": 17179869184u64, "used_bytes": 8589934592u64,
                       "swap_total_bytes": 0, "swap_used_bytes": 0},
            "processes": [{"pid": 7, "name": "claude", "cpu_percent": 3.0, "memory_bytes": 1048576, "power_watts": 2.34}],
            "sessions": [{"pid": 7, "session": "s1"}],
            "process_count": 9
        }))
        .unwrap();
        let mut f = MonitorFeed::default();
        f.push(s);
        f
    }

    #[test]
    fn fr_l21_the_status_card_draws_chip_ram_and_os_badges() {
        let _guard = crate::services::test_guard();
        fn ui() {
            StatusCard(&chip(), &telemetry_feed(), true, false);
        }
        let text = composed_text(ui);
        let at = |want: &str| text.find(&format!("\"{want}\"")).unwrap_or_else(|| panic!("missing {want}: {text}"));
        // The mockup's order: chip, RAM, OS.
        assert!(at("Apple M2 Pro") < at("16.0 GB") && at("16.0 GB") < at("macOS 15.2"));
    }

    #[test]
    fn fr_l21_a_member_row_shows_the_member_the_process_and_the_power() {
        let _guard = crate::services::test_guard();
        fn ui() {
            let f = telemetry_feed();
            let s = f.latest().unwrap();
            let members = [crate::ui::project::Member {
                session_id: "s1".into(),
                name: "ember-리더".into(),
                task: String::new(),
                agent: "claude-code".into(),
                status: ember_client::LauncherStatus::Running,
                leader: true,
                updated_at: 0,
            }];
            let rows = crate::ui::monitor::member_rows(crate::ui::monitor::process_rows(s), &s.sessions, &members);
            assert!(rows[0].session.is_some());
            ProcessRowView(&rows[0], |_| {});
        }
        let text = composed_text(ui);
        for want in ["\"ember-리더 (리더)\"", "\"claude\"", "\"2.3\"", "\"7\""] {
            assert!(text.contains(want), "missing {want}: {text}");
        }
        // The columns follow the mockup: 이름, PID, CPU, 전력, 메모리.
        let production = include_str!("monitor.rs").split("#[cfg(test)]").next().unwrap();
        assert!(production.contains("[SortKey::Name, SortKey::Pid, SortKey::Cpu, SortKey::Power, SortKey::Memory]"));
    }

    #[test]
    fn fr_l21_temperature_card_draws_cpu_and_gpu_side_by_side_with_toned_bars() {
        let _guard = crate::services::test_guard();
        fn ui() {
            let s: ember_client::api::MetricsSample = serde_json::from_value(serde_json::json!({
                "at_ms": 1000,
                "chip": "Apple M4 Pro",
                "temperatures": [
                    {"label": "CPU die", "celsius": 56.0},
                    {"label": "GPU die", "celsius": 74.0}
                ]
            }))
            .unwrap();
            let mut f = MonitorFeed::default();
            f.push(s);
            TemperatureCard(&f, true, false);
        }
        let text = composed_text(ui);
        for want in ["\"CPU\"", "\"56\"", "\"GPU\"", "\"74\"", "\"Apple M4 Pro\""] {
            assert!(text.contains(want), "missing {want}: {text}");
        }
        let p = ember_theme().palette;
        let green = format!("{:?}", compose_rust::schema::Modifier::Background(paint(p.green.strong)));
        let orange = format!("{:?}", compose_rust::schema::Modifier::Background(paint(p.orange.strong)));
        assert!(text.contains(&green), "CPU 56C bar should be green");
        assert!(text.contains(&orange), "GPU 74C bar should be orange");
    }

    #[test]
    fn fr_l21_gpu_memory_and_network_cards_emit_svg_graphs_when_series_has_two_points() {
        let _guard = crate::services::test_guard();
        fn ui() {
            let mk = |at_ms: u64, rx: u64, tx: u64, gpu: f32, used: u64| -> ember_client::api::MetricsSample {
                serde_json::from_value(serde_json::json!({
                    "at_ms": at_ms,
                    "cpu": {"total_percent": 20.0, "cores": [20.0]},
                    "memory": {"total_bytes": 1000u64, "used_bytes": used, "swap_total_bytes": 0, "swap_used_bytes": 0},
                    "network": [{"interface": "en0", "rx_bytes": rx, "tx_bytes": tx}],
                    "gpu": {"devices": [{"name": "Apple M4", "utilization_percent": gpu}]}
                }))
                .unwrap()
            };
            let mut f = MonitorFeed::default();
            f.push(mk(1000, 0, 0, 10.0, 500));
            f.push(mk(2000, 2000, 400, 15.0, 520));
            f.push(mk(3000, 5000, 900, 12.0, 540));
            for m in [Metric::Gpu, Metric::Memory, Metric::Network] {
                MetricCard(m, card_view(m, &f), None, "", meter_fraction(m, &f), f.net_tx_series(), false);
            }
        }
        let text = composed_text(ui);
        assert!(text.matches("Height(44.0)").count() >= 3, "GPU, memory and network each emit a 44 dp SVG graph: {text}");
    }
}
