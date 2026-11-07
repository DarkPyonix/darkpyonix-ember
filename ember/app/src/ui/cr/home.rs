//! compose-rust version of the Home screen (#220 phase 5; FR-L11, FR-L15, FR-L16, FR-L23,
//! `docs/DESIGN.md` section 6.1): the chat-first top (greeting, the large composer with its
//! project, model, mode and computer choices, suggestion chips) and "오늘의 팀" below it.
//!
//! The decisions (harnesses, models, modes, the selection rules, KPIs, counts, ring and bar
//! arithmetic) are `ui::home`'s plain functions, used as they are. Only numbers the client
//! holds are shown; what the server cannot report yet (approval wait, per-member efficiency,
//! computer load) is a tile that says "준비 중입니다", never an estimate.

#![allow(non_snake_case)]

use compose_rust::runtime::{mutable_state_of, remember, MutableState};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{Alignment, Color, DrawList, Paint, IconRole, ShapeRole};

pub use crate::ui::home::{
    apply_suggestion, can_send, computers_line, default_mode, effective_message, filter_models,
    harness_block, kpis, mode_after_harness_change, composer_chip_budget, composer_chip_rows, composer_chip_width, mode_header, mode_picker_rows_pending, mode_tap, popover_width, model_catalog, modes_for, needs_approve_all,
    project_options, project_tile_line, resolve_cwd, shortcut_label, split_chip_rows,
    success_percent, title_from_message, CrewMember, Dash, Harness, HarnessBar, Kpi, ModelOption,
    ModeRow, PermMode, ProjectTile, Selection, StatusCounts, ATTACH_UNSUPPORTED, COMPOSER_PLACEHOLDER, DEFAULT_PROJECT,
    EMPTY_CREW, EMPTY_HARNESS_USAGE, CiResult, GithubCard, GithubPill, GithubSampleKind, INTEGRATIONS_TITLE, LATER_INTEGRATIONS, LATER_LINE, LATER_PILL, GITHUB_NAME, EMPTY_PROJECTS, GREETING, SUGGESTIONS, UNAVAILABLE,
};
use crate::ui::home::{model_active_row, model_enter, model_row_detail, model_view, ModelEnter};
use super::components::{
    AppButton, ButtonKind, Clickable, EmberField, EmberSheet, FieldOptions, FilterChip, Label,
    OptionSheet, PickChip, Pill, PillTone,
};
use super::notice;
use super::shell::{self, NavIcon};
use super::theme::*;
use crate::model;
use crate::prefs::LastUsed;
use crate::server::CreateSession;
use crate::services::{run_shared, services};
use crate::ui::coexist::{shared_ui, SharedUi};
use crate::ui::usage;
use crate::ui::shell::{NavMenu, ShellMode, DEMO_USER};

pub type Part<'a> = Box<dyn FnOnce() + 'a>;

/// The width of the option popups of the composer's chips.
const POPUP_WIDTH: f32 = 280.0;
/// The permission popover (mockup `.mp-pop.mp-mode`).
const MODE_POPOVER_WIDTH: f32 = 340.0;
/// The model popover: a rail of harnesses beside the search and the rows.
const MODEL_POPOVER_WIDTH: f32 = 560.0;
const MODEL_RAIL_WIDTH: f32 = 128.0;

/// The permission-mode rows (FR-L16, mockup v8 `.md-row`): a header "권한 모드 · harness", then
/// one row per mode with a shield (a warning triangle on the danger row), the title, a
/// one-line description and a check on the chosen row. The bypass row is red. Tapping calls
/// `on_tap(i)`; closing and the bypass confirmation belong to the caller.
/// Keyboard: each row is a `Clickable`, which the renderer makes a focusable button (Tab, Enter,
/// Space), named by its title and description. Not built: arrow keys between rows and focus on
/// the chosen row when the picker opens (TODO(compose-rust#504): only Enter reaches the Host,
/// compose-rust#100; request focus landed after the pinned v0.0.6), and the selected state for screen readers
/// (TODO(compose-rust#460): `Modifier.selected` landed after v0.0.6).
fn ModeBody(header: &str, rows: &[ModeRow], width: f32, on_tap: impl FnMut(usize) + Clone + 'static) {
    let p = ember_theme().palette;
    cu::Column().modifier(Modifier::new().width(width)).content(|| {
        if !header.is_empty() {
            cu::Row().modifier(Modifier::new().width(width).padding(SP[2])).content(|| {
                Label(header, Fs::Xs, Weight::Semibold, Some(paint(p.muted)), Some(1));
            });
        }
        for (i, r) in rows.iter().enumerate() {
            let mut tap = on_tap.clone();
            let bg = if r.chosen { paint(p.sel) } else { Paint::Literal(Color::argb(0)) };
            let tone = if r.danger { p.red.strong } else { p.muted };
            let title = if r.danger { p.red.strong } else { p.text };
            let icon = if r.danger { ICON_WARN } else { ICON_SHIELD };
            Clickable(
                Modifier::new().width(width).background(bg).corner_radius(CORNER_SM).padding(SP[2]),
                Alignment::CenterStart,
                move || tap(i),
                || {
                    cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                        NavIcon(icon, tone.to_argb(), 16.0);
                        cu::Spacer().modifier(Modifier::new().width(SP[2]));
                        cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                            Label(r.title, Fs::Sm, Weight::Medium, Some(paint(title)), Some(1));
                            Label(r.description, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
                        });
                        if r.chosen {
                            cu::Spacer().modifier(Modifier::new().width(SP[2]));
                            NavIcon(ICON_CHECK, p.text.to_argb(), 16.0);
                        }
                    });
                },
            );
        }
    });
}

/// The permission picker as a bottom sheet at 560 and below (the shared sheet, FR-L32), with
/// the same header, rows and bypass confirmation as the popover.
fn ModeSheet(
    open: bool,
    header: &str,
    rows: &[ModeRow],
    on_dismiss: impl FnMut() + Clone + 'static,
    on_tap: impl FnMut(usize) + Clone + 'static,
) {
    let width = (compose_rust::runtime::current_window_size().width_dp - 2.0 * SP[3]).max(240.0);
    EmberSheet(header, open, on_dismiss, None, || {
        cu::Box().modifier(Modifier::new().fill_max_width().padding(SP[1])).content(|| ModeBody("", rows, width - 2.0 * SP[1], on_tap));
    });
}

macro_rules! line_icon {
    ($d:literal) => {
        concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d=""#,
            $d,
            r#""/></svg>"#
        )
    };
}

const ICON_ARROW: &str = line_icon!("M5 12h14M13 6l6 6-6 6");
const ICON_SHIELD: &str = include_str!("../../../assets/nav/shield.svg");
const ICON_FOLDER: &str = include_str!("../../../assets/nav/folder.svg");
const ICON_PC: &str = include_str!("../../../assets/nav/pc.svg");
// TODO(item 6): the model chip's harness glyph; the terminal stands in.
const ICON_TERMINAL: &str = include_str!("../../../assets/nav/terminal.svg");
const ICON_CODE: &str = include_str!("../../../assets/nav/code.svg");
const ICON_GIT: &str = include_str!("../../../assets/nav/git.svg");
const ICON_CHART_NAV: &str = include_str!("../../../assets/nav/chart.svg");
const ICON_WARN: &str = line_icon!("M12 9.5v4M12 16.75h.01M10.3 3.9L2.4 17.6a2 2 0 001.7 3h15.8a2 2 0 001.7-3L13.7 3.9a2 2 0 00-3.4 0z");
const ICON_CHECK: &str = line_icon!("M5 12.5l4.5 4.5L19 7.5");
const ICON_CLOCK: &str = line_icon!("M12 7v5l3 2M21 12a9 9 0 11-18 0 9 9 0 0118 0z");
const ICON_ZAP: &str = line_icon!("M13 3L5 13.5h6L10 21l8-10.5h-6z");
const ICON_CHART: &str = line_icon!("M5 20V11M12 20V5M19 20v-7");

// ---------------------------------------------------------------------------------------------
// Layout decisions (home.css breakpoints, on the window width)
// ---------------------------------------------------------------------------------------------

/// How the dashboard is laid out at a window width: the 12-column grid of the mockup becomes
/// two or one column at 1279, 900 and 560 px.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// Above 1279: trend 8 + status 4, usage 7 + crew 5, three small tiles in a row.
    pub wide: bool,
    pub kpi_cols: usize,
    /// Success and wait side by side (above 900); otherwise stacked.
    pub pair_mid: bool,
    pub eff_cols: usize,
    pub proj_cols: usize,
    /// 560 and below: phone paddings, icon-only chips.
    pub compact: bool,
}

pub fn layout(width_dp: f32) -> Layout {
    let wide = width_dp > 1279.0;
    let mid = width_dp > 900.0;
    Layout {
        wide,
        kpi_cols: if mid { 4 } else { 2 },
        pair_mid: mid,
        eff_cols: if wide { 3 } else if mid { 2 } else { 1 },
        proj_cols: if wide {
            5
        } else if mid {
            3
        } else if width_dp > 560.0 {
            2
        } else {
            1
        },
        compact: width_dp <= 560.0,
    }
}

/// Estimated text width in dp: Hangul and other wide glyphs one em, ASCII 0.55 em. Used to lay out
/// rows of chips and cards, never to cut or wrap a label.
pub fn text_px(text: &str, font_px: f32) -> f32 {
    text.chars().map(|c| if c.is_ascii() { 0.55 } else { 1.0 }).sum::<f32>() * font_px
}

/// `text` cut to fit `max_px` at `font_px`, ending in an ellipsis when it was cut.
pub fn fit_label(text: &str, max_px: f32, font_px: f32) -> String {
    if text_px(text, font_px) <= max_px {
        return text.to_string();
    }
    let mut out = String::new();
    for ch in text.chars() {
        let next = format!("{out}{ch}\u{2026}");
        if text_px(&next, font_px) > max_px {
            break;
        }
        out.push(ch);
    }
    format!("{out}\u{2026}")
}

/// "10월 25일 토요일" for `YYYY-MM-DD`.
pub fn date_line(ymd: &str) -> String {
    let n = crate::ui::home::day_number(ymd);
    let parts: Vec<&str> = ymd.split('-').collect();
    match (n, parts.get(1).and_then(|m| m.parse::<u32>().ok()), parts.get(2).and_then(|d| d.parse::<u32>().ok())) {
        (Some(n), Some(m), Some(d)) => {
            let wd = ["일", "월", "화", "수", "목", "금", "토"][((n + 4).rem_euclid(7)) as usize];
            format!("{m}월 {d}일 {wd}요일")
        }
        _ => String::new(),
    }
}

/// The suggestion chips per line: three, two up to 900 px.
pub fn suggestion_cols(width_dp: f32) -> usize {
    if width_dp > 900.0 {
        3
    } else {
        2
    }
}

/// The width the main column has: the window minus the navigation and the page padding.
pub fn content_width(window_dp: f32, pad: f32) -> f32 {
    let nav = match shell::current_mode(false) {
        ShellMode::BottomTabs => 0.0,
        ShellMode::Rail => RAIL_W,
        ShellMode::Sidebar => SIDE_W,
    };
    (window_dp - nav - 2.0 * pad).max(280.0)
}

/// The composer's width: `min(720px, 100%)`.
pub fn composer_width(content_dp: f32) -> f32 {
    content_dp.min(720.0)
}

// ---------------------------------------------------------------------------------------------
// Drawing arithmetic (pure)
// ---------------------------------------------------------------------------------------------

/// A ring of `parts` `(colour, fraction)` over a full-circle track, clockwise from 12 o'clock.
/// One arc path per non-empty part; a part that fills the circle is a circle.
pub fn ring_svg(size: f32, stroke: f32, track: u32, parts: &[(u32, f32)]) -> String {
    let c = size / 2.0;
    let r = (size - stroke) / 2.0;
    let hex = |v: u32| format!("#{:06x}", v & 0x00ff_ffff);
    let mut s = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{size}\" height=\"{size}\" viewBox=\"0 0 {size} {size}\"><circle cx=\"{c}\" cy=\"{c}\" r=\"{r}\" fill=\"none\" stroke=\"{}\" stroke-width=\"{stroke}\"/>",
        hex(track)
    );
    let mut at = 0.0_f32;
    for (color, frac) in parts {
        let f = frac.clamp(0.0, 1.0 - at);
        if f <= 0.0005 {
            continue;
        }
        if f >= 0.999 {
            s.push_str(&format!(
                "<circle cx=\"{c}\" cy=\"{c}\" r=\"{r}\" fill=\"none\" stroke=\"{}\" stroke-width=\"{stroke}\"/>",
                hex(*color)
            ));
        } else {
            let point = |t: f32| {
                let a = (t * 360.0 - 90.0).to_radians();
                (c + r * a.cos(), c + r * a.sin())
            };
            let (x0, y0) = point(at);
            let (x1, y1) = point(at + f);
            let large = if f > 0.5 { 1 } else { 0 };
            s.push_str(&format!(
                "<path d=\"M{x0:.2} {y0:.2}A{r} {r} 0 {large} 1 {x1:.2} {y1:.2}\" fill=\"none\" stroke=\"{}\" stroke-width=\"{stroke}\"/>",
                hex(*color)
            ));
        }
        at += f;
    }
    s.push_str("</svg>");
    s
}

/// Bar heights in dp for `values` against the largest, at least 2 dp for a non-zero value.
pub fn bar_heights(values: &[u64], max_h: f32) -> Vec<f32> {
    let max = values.iter().copied().max().unwrap_or(0);
    values
        .iter()
        .map(|v| if max == 0 || *v == 0 { 0.0 } else { (max_h * (*v as f32 / max as f32)).max(2.0) })
        .collect()
}

/// The tone of a KPI's note: a waiting count is good news, failures and waiting-now are not
/// when they are above zero.
pub fn note_tone(index: usize, counts: &StatusCounts) -> PillTone {
    match index {
        2 if counts.failed > 0 => PillTone::Red,
        3 if counts.waiting > 0 => PillTone::Red,
        _ => PillTone::Green,
    }
}

// ---------------------------------------------------------------------------------------------
// Small pieces
// ---------------------------------------------------------------------------------------------

pub fn ring_image(size: f32, stroke: f32, parts: &[(u32, f32)]) {
    let p = ember_theme().palette;
    let doc = ring_svg(size, stroke, p.track.to_argb(), parts);
    cu::Image(compose_rust::asset(compose_rust::AssetKind::Svg, shell::leak_doc(doc)))
        .modifier(Modifier::new().size(size, size));
}

/// `n` equal columns of `items`, `gap` apart; the last line is padded with empty columns.
pub fn Cols(n: usize, gap: f32, items: Vec<Part<'_>>) {
    let n = n.max(1);
    cu::Column().spacing(gap).modifier(Modifier::new().fill_max_width()).content(|| {
        let mut it = items.into_iter().peekable();
        while it.peek().is_some() {
            let row: Vec<Part> = it.by_ref().take(n).collect();
            let pad = n - row.len();
            cu::Row().spacing(gap).modifier(Modifier::new().fill_max_width()).content(|| {
                for part in row {
                    cu::Column().modifier(Modifier::new().weight(1.0)).content(part);
                }
                for _ in 0..pad {
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                }
            });
        }
    });
}

/// Columns of unequal weights (the 12-column grid), or stacked when `stacked`.
fn Weighted(stacked: bool, gap: f32, parts: Vec<(f32, Part<'_>)>) {
    if stacked {
        cu::Column().spacing(gap).modifier(Modifier::new().fill_max_width()).content(|| {
            for (_, part) in parts {
                cu::Column().modifier(Modifier::new().fill_max_width()).content(part);
            }
        });
    } else {
        cu::Row().spacing(gap).modifier(Modifier::new().fill_max_width()).content(|| {
            for (w, part) in parts {
                cu::Column().modifier(Modifier::new().weight(w)).content(part);
            }
        });
    }
}

/// A dashboard tile: a card with its content in a column.
pub fn Tile(content: impl FnOnce()) {
    TileH(None, content);
}

/// The inner height of the success tile (donut 132 + head 30 + gap 8 + the card's 16 px padding
/// each side is outside it): tiles beside it take the same, since the toolkit rows do not
/// stretch their children. TODO(compose-rust#389): equal-height cards in a row; this number is
/// fragile when the success tile's content changes.
const ROW_TILE_H: f32 = 171.0;

fn TileH(height: Option<f32>, content: impl FnOnce()) {
    super::components::EmberCard(false, true, None, None, None, || {
        let m = Modifier::new().fill_max_width();
        cu::Column().spacing(SP[2]).modifier(match height { Some(h) => m.height(h), None => m }).content(content);
    });
}

/// `.hm-ch`: the 30 px outlined icon, the title, and what goes at the right.
pub fn CardHead(icon: &'static str, title: &str, trailing: impl FnOnce()) {
    let p = ember_theme().palette;
    cu::Row().spacing(10.0).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width().height(30.0)).content(|| {
        cu::Box()
            .alignment(Alignment::Center)
            .modifier(Modifier::new().size(30.0, 30.0).border(1.0, paint(p.line)).corner_radius(CORNER_SM))
            .content(|| NavIcon(icon, p.text_2.to_argb(), 16.0));
        Label(title, Fs::Lg, Weight::Semibold, None, Some(1));
        cu::Spacer().modifier(Modifier::new().weight(1.0));
        trailing();
    });
}

/// `.hm-go`: the round 30 px arrow that opens the tile's screen.
/// TODO(compose-rust#372): a pointer-up `Clickable` with no keyboard focus or role.
fn GoButton(on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    Clickable(
        Modifier::new().size(30.0, 30.0).border(1.0, paint(p.line)).shape_role(ShapeRole::Full),
        Alignment::Center,
        on_click,
        || NavIcon(ICON_ARROW, p.muted.to_argb(), 16.0),
    );
}

pub fn Dot(color: compose_rust::Color) {
    cu::Spacer().modifier(Modifier::new().size(8.0, 8.0).background(paint(color)).shape_role(ShapeRole::Full));
}

/// A legend dot: solid in `solid`, or an 8 px hatched square in `hatch` (the bars' hatch, FR-L23).
fn DotOrHatch(hatched: bool, hatch: compose_rust::Color, solid: compose_rust::Color) {
    if !hatched {
        Dot(solid);
        return;
    }
    // The bars' pattern: same colour for fill and lines, same step (`legend_hatch_style`).
    // TODO(compose-rust#428, #429): the dots have no accessible label; the Label beside them is the text.
    let (step, _alpha) = usage::legend_hatch_style();
    let mut list = DrawList::builder().round_rect(paint(hatch), 0.0, 0.0, 8.0, 8.0, 2.0, 1.0);
    for (x1, y1, x2, y2) in crate::ui::home::hatch_lines(0.0, 0.0, 8.0, 8.0, step) {
        list = list.line(paint(hatch), x1, y1, x2, y2, 1.0);
    }
    cu::Canvas(list.build()).modifier(Modifier::new().size(8.0, 8.0));
}

// ---------------------------------------------------------------------------------------------
// The screen
// ---------------------------------------------------------------------------------------------

/// Which of the composer's lists is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pick {
    Project,
    Model,
    Mode,
    Computer,
}

/// `EMBER_CR_HOME_PICK=project|model|mode|computer|mode_armed` opens one list at the start
/// (`mode_armed` opens the permission picker with the bypass confirmation armed, so its row
/// reads "다시 눌러 바이패스 적용"), `EMBER_CR_HOME_MODE=bypass` presets the permission mode so the
/// composer shows the warning chip, and `EMBER_CR_HOME_SUGGEST=0..2` takes a suggestion, for
/// review captures.
fn initial_pick() -> Option<Pick> {
    pick_from(std::env::var("EMBER_CR_HOME_PICK").ok().as_deref()).0
}

/// The list a `EMBER_CR_HOME_PICK` value opens and whether it also arms the bypass confirm.
fn pick_from(value: Option<&str>) -> (Option<Pick>, bool) {
    match value {
        Some("project") => (Some(Pick::Project), false),
        Some("model") => (Some(Pick::Model), false),
        Some("mode") => (Some(Pick::Mode), false),
        Some("mode_armed") => (Some(Pick::Mode), true),
        Some("computer") => (Some(Pick::Computer), false),
        _ => (None, false),
    }
}

/// The permission mode a `EMBER_CR_HOME_MODE` value presets.
fn mode_from(value: Option<&str>) -> Option<PermMode> {
    match value {
        Some("bypass") => Some(PermMode::Bypass),
        Some("edits") => Some(PermMode::Edits),
        Some("plan") => Some(PermMode::Plan),
        Some("ask") => Some(PermMode::Ask),
        _ => None,
    }
}

fn initial_suggestion() -> Option<String> {
    let i = std::env::var("EMBER_CR_HOME_SUGGEST").ok()?.parse::<usize>().ok()?;
    SUGGESTIONS.get(i).map(|s| s.to_string())
}

/// `EMBER_CR_SCROLL=<dp|bottom|status|usage|efficiency|small>` for review captures: the toolkit cannot scroll a column from
/// code (TODO(compose-rust#385)), so the blocks that would have scrolled out (by their estimated heights) are left out.
struct Skip(std::cell::Cell<f32>);

impl Skip {
    fn from_env(total_est: f32, window_h: f32) -> Skip {
        let v = match std::env::var("EMBER_CR_SCROLL").ok().as_deref() {
            Some("bottom") => (total_est - window_h).max(0.0),
            // Named starts: the first block in view. `status` shows the 팀원 상태 card with its
            // legend at the top, `usage` the 하네스별 사용량 and 지금 일하는 팀원 row.
            Some("status") => EST_HERO + EST_HEAD,
            Some("usage") => EST_HERO + EST_HEAD + EST_TREND,
            Some("efficiency") => EST_HERO + EST_HEAD + EST_TREND + EST_USAGE,
            Some("small") => EST_HERO + EST_HEAD + EST_TREND + EST_USAGE + EST_EFF,
            Some(n) => n.parse::<f32>().unwrap_or(0.0),
            None => 0.0,
        };
        Skip(std::cell::Cell::new(v))
    }

    /// Whether a block of about `est` dp is still in view.
    fn show(&self, est: f32) -> bool {
        let left = self.0.get();
        if left >= est {
            self.0.set(left - est);
            false
        } else {
            self.0.set(0.0);
            true
        }
    }
}

/// Estimated heights of the blocks, wide layout, for [`Skip`].
const EST_HERO: f32 = 560.0;
const EST_HEAD: f32 = 190.0;
const EST_TREND: f32 = 380.0;
const EST_USAGE: f32 = 340.0;
const EST_SMALL: f32 = 340.0;
const EST_EFF: f32 = 120.0;
const EST_PROJ: f32 = 330.0;

/// The Home screen: the chat-first top, then the dashboard.
pub fn HomeScreen() {
    let window = compose_rust::runtime::current_window_size();
    let l = layout(window.width_dp);
    let pad = if l.compact { SP[3] } else { SP[6] };
    let content_w = content_width(window.width_dp, pad);
    let ui = shared_ui().clone();
    let _ = ui.live.launcher.get();
    let dash = services().client.read(crate::ui::home::read_dash);

    // Initial fetch when HomeScreen mounts directly (and `shell()` also triggers on route transitions to Home, FR-L35, #621).
    let _requested = remember(|| {
        crate::bridge::request_github_summary();
        0u8
    });

    // The lists are modals; they sit beside the scroller so no parent's spacing counts them.
    let composer = remember(|| Composer::new());
    let skip = Skip::from_env(EST_HERO + EST_HEAD + EST_TREND + EST_USAGE + EST_SMALL + EST_EFF + EST_PROJ, window.height_dp);
    cu::Box().modifier(Modifier::new().fill_max_width().fill_max_height()).content(|| {
        cu::ScrollColumn().modifier(Modifier::new().fill_max_width().fill_max_height()).content(|| {
            cu::Column().alignment(Alignment::Center).modifier(Modifier::new().fill_max_width().padding(pad)).content(|| {
                if skip.show(EST_HERO) {
                    Hero(&composer, &ui, window.width_dp, window.height_dp, content_w, l);
                    cu::Spacer().modifier(Modifier::new().height(SP[5]));
                }
                Dashboard(&dash, &ui, l, content_w, &skip);
            });
        });
        composer.lists(&ui);
    });
}

/// The composer's state.
#[derive(Clone)]
struct Composer {
    selection: MutableState<Selection>,
    typed: MutableState<String>,
    suggestion: MutableState<Option<String>>,
    busy: MutableState<bool>,
    open: MutableState<Option<Pick>>,
    /// The mode that waits for its confirming tap (bypass), if any.
    mode_pending: MutableState<crate::ui::home::ModePending>,
    harness_filter: MutableState<Option<Harness>>,
    query: MutableState<String>,
}

impl Composer {
    fn new() -> Self {
        Composer {
            selection: mutable_state_of(match mode_from(std::env::var("EMBER_CR_HOME_MODE").ok().as_deref()) {
                Some(m) => Selection::initial().with_mode(m),
                None => Selection::initial(),
            }),
            typed: mutable_state_of(String::new()),
            suggestion: mutable_state_of(initial_suggestion()),
            busy: mutable_state_of(false),
            open: mutable_state_of(initial_pick()),
            mode_pending: mutable_state_of(
                pick_from(std::env::var("EMBER_CR_HOME_PICK").ok().as_deref()).1.then(|| (PermMode::Bypass, now_ms())),
            ),
            harness_filter: mutable_state_of(None),
            query: mutable_state_of(String::new()),
        }
    }

    /// What the lists show: projects and computers from the client.
    fn live(&self, ui: &SharedUi) -> (Vec<String>, Vec<(String, String)>) {
        let _ = ui.live.launcher.get();
        services().client.read(|s| {
            let projects = model::project_cards(s).into_iter().map(|c| c.name).collect();
            let computers = s
                .computers()
                .iter()
                .filter(|c| c.id != "local")
                .map(|c| (c.id.clone(), c.name.clone()))
                .collect();
            (projects, computers)
        })
    }

    /// The name the main server's computer goes by (`이 서버` until the registry names it).
    fn local_name(&self, ui: &SharedUi) -> String {
        let _ = ui.live.launcher.get();
        services()
            .client
            .read(|s| s.computers().iter().find(|c| c.id == "local").map(|c| c.name.clone()))
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "이 서버".to_string())
    }

    fn send(&self, ui: &SharedUi) {
        let message = effective_message(&self.typed.get_untracked(), self.suggestion.get_untracked().as_deref());
        if !can_send(&message, self.busy.get_untracked()) {
            return;
        }
        let sel = self.selection.get_untracked();
        let prefs = ui.prefs.get_untracked();
        let remembered = prefs.last_used.get(&sel.project).map(|l| l.cwd.clone());
        let (latest_in_project, latest_any) = services().client.read(|s| {
            let any = model::recent_sessions(s, &model::RowContext { account_labels: &Default::default() }, 1)
                .first()
                .map(|r| r.cwd.clone());
            (model::latest_cwd(s, &sel.project), any)
        });
        let Some(cwd) = resolve_cwd(remembered.as_deref(), latest_in_project.as_deref(), latest_any.as_deref()) else {
            // Home has no folder field; the new-session dialog asks for one.
            notice::post("작업 폴더를 알 수 없습니다. 새 세션 창에서 폴더를 정해 주세요");
            ui.open_new_session(Some(sel.project.clone()));
            return;
        };
        self.busy.set(true);
        let body = CreateSession {
            project: sel.project.clone(),
            agent: sel.option.harness.kind().to_string(),
            cwd: cwd.clone(),
            model: Some(sel.option.model.to_string()),
            title: Some(title_from_message(&message)),
            account: None,
        };
        let computer_id = sel.computer.clone();
        let bypass = needs_approve_all(sel.mode);
        let remember = LastUsed {
            agent: body.agent.clone(),
            account: None,
            computer: computer_id.clone(),
            model: body.model.clone(),
            cwd,
        };
        let project_name = sel.project.clone();
        let (ui, busy) = (ui.clone(), self.busy.clone());
        run_shared(
            async move {
                let s = services();
                let rec = s.create_session(&body).await?;
                let mut note = None;
                // The demo (#199) has one computer and no node to switch.
                if let Some(cid) = computer_id.filter(|_| s.demo.is_none()) {
                    if let Err(e) = s.server.switch_computer(&rec.id, &cid).await {
                        note = Some(format!("메인 서버에서 시작했습니다. 컴퓨터를 바꾸지 못했습니다({e})"));
                    }
                }
                if bypass {
                    if let Err(e) = s.set_approve_all(&rec.id, true).await {
                        note = Some(format!("모두 허용을 켜지 못했습니다({e})"));
                    }
                }
                Ok::<_, String>((rec, note))
            },
            move |r| {
                busy.set(false);
                match r {
                    Ok((rec, note)) => {
                        ui.update_prefs(|p| {
                            p.last_used.insert(project_name.clone(), remember);
                            p.last_project = Some(project_name);
                        });
                        let s = services();
                        s.outbox.lock().unwrap().push(&rec.id, message);
                        s.bump_outbox();
                        ui.open_conversation(&rec.id);
                        s.drive_outbox(&rec.id);
                        if let Some(n) = note {
                            notice::post(n);
                        }
                    }
                    Err(e) => notice::post(format!("세션을 시작하지 못했습니다: {e}")),
                }
            },
        );
    }

    /// The four lists the chips open. One node, so the closed dialogs add no gap anywhere.
    fn lists(&self, ui: &SharedUi) {
        let (projects, computers) = self.live(ui);
        let sel = self.selection.get();
        let open = self.open.get();
        let close = {
            let o = self.open.clone();
            move || o.set(None)
        };
        let projects = project_options(&projects);
        let chosen = projects.iter().position(|p| *p == sel.project);
        {
            let (selection, list) = (self.selection.clone(), projects.clone());
            OptionSheet(open == Some(Pick::Project), &projects, chosen, POPUP_WIDTH, close.clone(), move |i| {
                if let Some(p) = list.get(i) {
                    let mut s = selection.get_untracked();
                    s.project = p.clone();
                    selection.set(s);
                }
            });
        }
        // Compact widths keep the modal; wider ones use the popover under the chip, drawn in
        // the composer card (`mode_popover`).
        let compact = layout(compose_rust::runtime::current_window_size().width_dp).compact;
        if compact {
            let me = self.clone();
            let dismiss = self.dismiss_mode();
            ModeSheet(
                open == Some(Pick::Mode),
                &mode_header(sel.option.harness),
                &mode_picker_rows_pending(sel.option.harness, sel.mode, self.mode_pending.get().map(|(m, _)| m)),
                dismiss,
                move |i| me.tap_mode(i),
            );
        }
        let mut labels = vec![self.local_name(ui)];
        labels.extend(computers.iter().map(|(_, n)| n.clone()));
        {
            let (selection, list) = (self.selection.clone(), computers.clone());
            let at = sel.computer.as_ref().and_then(|id| computers.iter().position(|(c, _)| c == id)).map(|i| i + 1).unwrap_or(0);
            OptionSheet(open == Some(Pick::Computer), &labels, Some(at), POPUP_WIDTH, close.clone(), move |i| {
                let mut s = selection.get_untracked();
                s.computer = if i == 0 { None } else { list.get(i - 1).map(|(id, _)| id.clone()) };
                selection.set(s);
            });
        }
        // Compact widths keep the modal (a bottom sheet in the spec); wider ones use the popover
        // under the chip, drawn in the composer card.
        let compact = layout(compose_rust::runtime::current_window_size().width_dp).compact;
        self.model_sheet(ui, &sel, compact && open == Some(Pick::Model), close);
    }

    /// Closing the permission picker also withdraws a waiting bypass confirmation.
    fn dismiss_mode(&self) -> impl FnMut() + Clone + 'static {
        let (o, pending) = (self.open.clone(), self.mode_pending.clone());
        move || {
            pending.set(None);
            if o.get_untracked() == Some(Pick::Mode) {
                o.set(None);
            }
        }
    }

    /// A tap on row `i` of the permission picker: bypass needs a second tap (`mode_tap`).
    fn tap_mode(&self, i: usize) {
        let sel = self.selection.get_untracked();
        let Some(m) = modes_for(sel.option.harness).get(i).copied() else { return };
        let (pending, applied) = mode_tap(sel.mode, self.mode_pending.get_untracked(), m, now_ms());
        self.mode_pending.set(pending);
        if let Some(m) = applied {
            self.selection.set(sel.with_mode(m));
            self.open.set(None);
        }
    }

    /// The permission picker as a popover under the mode chip (mockup `.mp-pop.mp-mode`, 340 dp).
    fn mode_popover(&self, sel: &Selection, anchor: impl FnOnce()) {
        let theme = ember_theme();
        let p = theme.palette;
        let open = self.open.get() == Some(Pick::Mode);
        let width = MODE_POPOVER_WIDTH;
        let rows = mode_picker_rows_pending(sel.option.harness, sel.mode, self.mode_pending.get().map(|(m, _)| m));
        let header = mode_header(sel.option.harness);
        cu::Menu(open).on_dismiss(self.dismiss_mode()).anchor(anchor).content(|| {
            let me = self.clone();
            cu::Box()
                .modifier(
                    with_shadows(Modifier::new(), theme.shadow_lg())
                        .width(width)
                        .background(paint(p.surface))
                        .border(1.0, paint(p.line))
                        .corner_radius(CORNER_MD)
                        .padding(SP[1]),
                )
                .content(|| ModeBody(&header, &rows, width - 2.0 * SP[1], move |i| me.tap_mode(i)));
        });
    }

    /// Closing the model picker: the next opening starts on the chosen harness with an empty
    /// search, as the mockup's `open()` does (the search field is new each time it opens).
    fn close_model(&self) -> impl FnMut() + Clone + 'static {
        let (o, f, q) = (self.open.clone(), self.harness_filter.clone(), self.query.clone());
        move || {
            f.set(None);
            q.set(String::new());
            if o.get_untracked() == Some(Pick::Model) {
                o.set(None);
            }
        }
    }

    /// The model picker as a modal (compact widths, where the spec asks for a bottom sheet):
    /// harness chips on top, search and the rows with their reasons (FR-L15).
    fn model_sheet(&self, ui: &SharedUi, sel: &Selection, open: bool, close: impl FnMut() + Clone + 'static) {
        let mut reset = self.close_model();
        let mut close = close;
        let dismiss = move || {
            reset();
            close();
        };
        EmberSheet("모델 선택", open, dismiss.clone(), None, || {
            self.model_body(ui, sel, false, dismiss);
        });
    }

    /// The model picker as a popover hanging under the model chip (FR-L15, mockup v8 `.mpick`):
    /// a left rail of harnesses, then search and the rows. The toolkit `Menu` places its popup
    /// below its anchor since compose-rust v0.0.5 (#383), with any content as its entries; the
    /// popup is a focusable platform popup, so Escape and a click outside call `on_dismiss`.
    /// Not built: the rail's favourites group and the favourite stars (no place stores them),
    /// profile names (FR-L17 profiles are not built), arrow keys and `Ctrl+number`: the pinned
    /// toolkit delivers only Enter to the Host (TODO(compose-rust#504) for arrows and focus in
    /// the popup, TODO(compose-rust#100) for `Ctrl+number`).
    fn model_popover(&self, ui: &SharedUi, sel: &Selection, anchor: impl FnOnce(), close: impl FnMut() + Clone + 'static) {
        let open = self.open.get() == Some(Pick::Model);
        let mut reset = self.close_model();
        let mut close = close;
        let dismiss = move || {
            reset();
            close();
        };
        cu::Menu(open).on_dismiss(dismiss.clone()).anchor(anchor).content(|| {
            self.model_body(ui, sel, true, dismiss);
        });
    }

    /// The picker's contents. With `rail` the harness switch is a vertical rail at the left
    /// and the list is a fixed-width column beside it; without, chips run along the top. Either
    /// way it opens on the chosen model's harness and has no "all" entry (mockup v8).
    fn model_body(&self, ui: &SharedUi, sel: &Selection, rail: bool, close: impl FnMut() + Clone + 'static) {
        let p = ember_theme().palette;
        let agents: Vec<(String, bool)> = ui.live.agents.get().iter().map(|a| (a.kind.clone(), a.installed)).collect();
        let limited: Vec<String> = {
            let accounts = ui.live.accounts.get();
            Harness::ALL
                .iter()
                .map(|h| h.kind())
                .filter(|k| {
                    let of_kind: Vec<_> = accounts.iter().filter(|a| a.agent == *k).collect();
                    !of_kind.is_empty() && of_kind.iter().all(|a| a.limited)
                })
                .map(String::from)
                .collect()
        };
        let all = model_catalog();
        let view = model_view(self.harness_filter.get(), sel.option.harness);
        let query = self.query.get();
        let rows = filter_models(&all, Some(view), &query);
        let active = model_active_row(&rows, sel.option, &query);
        let blocks: Vec<Option<&'static str>> = rows.iter().map(|o| harness_block(o.harness, &agents, &limited)).collect();
        // The search field keeps what was typed on its own (it is uncontrolled), so a rail tap
        // that clears the query gives the field a new identity, which starts empty.
        let field_gen = remember(|| mutable_state_of(0u32));
        let list = || {
            cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                let q = self.query.clone();
                // Enter takes the active row (FR-L15): the chosen row, or the first match once
                // something is typed. A blocked row says why and the picker stays open.
                let enter = {
                    let (rows, current, query) = (rows.clone(), sel.option, query.clone());
                    let (agents, limited) = (agents.clone(), limited.clone());
                    let (selection, mut done) = (self.selection.clone(), close.clone());
                    move |_: String| match model_enter(&rows, current, &query, |h| harness_block(h, &agents, &limited)) {
                        ModelEnter::Choose(o) => {
                            selection.set(selection.get_untracked().with_option(o));
                            done();
                        }
                        ModelEnter::Blocked(reason) => notice::post(reason),
                        ModelEnter::Nothing => {}
                    }
                };
                compose_rust::runtime::key(field_gen.get(), || {
                    EmberField(FieldOptions {
                        fill_max_width: true,
                        placeholder: "모델 검색".to_string(),
                        text: Some(query.clone()),
                        on_value_change: Some(Box::new(move |v| q.set(v))),
                        on_submit: Some(Box::new(enter)),
                        ..Default::default()
                    })
                });
                if rows.is_empty() {
                    Label("일치하는 모델이 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
                }
                cu::ScrollColumn().modifier(Modifier::new().fill_max_width().height(320.0)).content(|| {
                    cu::Column().spacing(SP[0]).modifier(Modifier::new().fill_max_width()).content(|| {
                        for (i, o) in rows.iter().copied().enumerate() {
                            let block = blocks[i];
                            let (selection, mut done) = (self.selection.clone(), close.clone());
                            // `.mp-row.on` is the chosen row, `.mp-row.act` the one Enter takes.
                            let fill = if o == sel.option {
                                paint(p.sel)
                            } else if active == Some(i) && !query.trim().is_empty() {
                                paint(p.hover)
                            } else {
                                paint(compose_rust::Color::argb(0))
                            };
                            let m = Modifier::new().fill_max_width().height(56.0).background(fill).corner_radius(CORNER_SM);
                            // A row: the name over the harness (and why it cannot be chosen),
                            // the shortcut at the right. A blocked row says why when tapped.
                            Clickable(
                                if block.is_some() { m.alpha(0.5) } else { m },
                                Alignment::CenterStart,
                                move || match block {
                                    Some(reason) => notice::post(reason),
                                    None => {
                                        selection.set(selection.get_untracked().with_option(o));
                                        done();
                                    }
                                },
                                || {
                                    cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                                        cu::Spacer().modifier(Modifier::new().width(SP[3]));
                                        cu::Column().spacing(2.0).modifier(Modifier::new().weight(1.0)).content(|| {
                                            Label(&o.row_title(), Fs::Md, Weight::Semibold, None, Some(1));
                                            cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).content(|| {
                                                NavIcon(harness_glyph(o.harness), harness_tint(o.harness), 14.0);
                                                let tone = if block.is_some() { p.red.strong } else { p.muted };
                                                Label(&model_row_detail(&o, block), Fs::Sm, Weight::Regular, Some(paint(tone)), Some(1));
                                            });
                                        });
                                        cu::Spacer().modifier(Modifier::new().width(SP[2]));
                                        cu::Row().modifier(Modifier::new().width(56.0)).content(|| {
                                            if let Some(k) = shortcut_label(i) {
                                                Label(&k, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
                                            }
                                        });
                                        cu::Spacer().modifier(Modifier::new().width(SP[2]));
                                    });
                                },
                            );
                        }
                    });
                });
            });
        };
        if rail {
            let surface = with_shadows(Modifier::new(), ember_theme().shadow_lg())
                .width(popover_width(MODEL_POPOVER_WIDTH, compose_rust::runtime::current_window_size().width_dp))
                .background(paint(p.surface))
                .border(1.0, paint(p.line))
                .corner_radius(CORNER_MD);
            // `equal_height`: the rail's `surface-2` runs the popover's full height.
            cu::Row().equal_height(true).modifier(surface).content(|| {
                // `.mp-rail`: `surface-2`, one entry per harness, the open one marked.
                // TODO(compose-rust#460): icon-only 36 dp entries in a 52 dp rail as in the
                // mockup once the pinned toolkit has the label modifier; until then each entry
                // keeps its harness name as text, which is what names it to a screen reader.
                cu::Column()
                    .spacing(SP[0])
                    .modifier(Modifier::new().width(MODEL_RAIL_WIDTH).fill_max_height().background(paint(p.surface_2)).padding(SP[1]))
                    .content(|| {
                        for h in Harness::ALL {
                            let (f, q, g) = (self.harness_filter.clone(), self.query.clone(), field_gen.clone());
                            RailEntry(h, view == h && query.trim().is_empty(), move || {
                                if !q.get_untracked().is_empty() {
                                    q.set(String::new());
                                    g.set(g.get_untracked().wrapping_add(1));
                                }
                                f.set(Some(h));
                            });
                        }
                    });
                cu::Box().modifier(Modifier::new().weight(1.0).padding(SP[2])).content(list);
            });
        } else {
            cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width().padding(SP[3])).content(|| {
                cu::ScrollRow().modifier(Modifier::new().fill_max_width().height(H_CTL + SP[0])).content(|| {
                    for h in Harness::ALL {
                        cu::Spacer().modifier(Modifier::new().width(SP[0]).height(SP[0]));
                        let f = self.harness_filter.clone();
                        FilterChip(h.label(), view == h, move || f.set(Some(h)));
                    }
                });
                list();
            });
        }
    }
}

/// One entry of the model popover's rail (mockup `.mp-rail-b`): the harness glyph in its vendor
/// colour and the harness name, on `sel` with the 3 dp blue bar at the right edge when open.
fn RailEntry(h: Harness, on: bool, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let bg = if on { paint(p.sel) } else { Paint::Literal(Color::argb(0)) };
    Clickable(
        Modifier::new().fill_max_width().height(36.0).background(bg).corner_radius(CORNER_SM),
        Alignment::CenterStart,
        on_click,
        || {
            cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                cu::Spacer().modifier(Modifier::new().width(SP[2]));
                NavIcon(harness_glyph(h), harness_tint(h), 20.0);
                cu::Spacer().modifier(Modifier::new().width(SP[1]));
                Label(h.label(), Fs::Sm, if on { Weight::Semibold } else { Weight::Regular }, None, Some(1));
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                if on {
                    cu::Spacer().modifier(Modifier::new().size(3.0, 20.0).background(paint(p.blue.strong)).corner_radius(2.0));
                }
            });
        },
    );
}

/// The harness glyphs of mockup v8 (`core.js` `GLYPH`), drawn in `currentColor`.
const GLYPH_CLAUDE: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24"><path d="M12 3v18M3 12h18M5.6 5.6l12.8 12.8M18.4 5.6 5.6 18.4" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" fill="none"/></svg>"#;
const GLYPH_CODEX: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24"><path d="M12 2.8 20 7.4v9.2l-8 4.6-8-4.6V7.4z" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" fill="none"/><path d="M8.5 10l2.5 2-2.5 2M12.5 14.5h3" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" fill="none"/></svg>"#;
const GLYPH_GEMINI: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24"><path d="M12 2.5c.6 5.2 4.3 8.9 9.5 9.5-5.2.6-8.9 4.3-9.5 9.5-.6-5.2-4.3-8.9-9.5-9.5 5.2-.6 8.9-4.3 9.5-9.5z" fill="currentColor"/></svg>"#;

fn harness_glyph(h: Harness) -> &'static str {
    match h {
        Harness::ClaudeCode => GLYPH_CLAUDE,
        Harness::Codex => GLYPH_CODEX,
        Harness::Gemini => GLYPH_GEMINI,
    }
}

/// `--v-claude`, `--v-codex`, `--v-gemini`: orange, blue and sky.
fn harness_tint(h: Harness) -> u32 {
    let p = ember_theme().palette;
    match h {
        Harness::ClaudeCode => p.orange.strong,
        Harness::Codex => p.blue.strong,
        Harness::Gemini => p.sky.strong,
    }
    .to_argb()
}

// ---------------------------------------------------------------------------------------------
// Hero: greeting, composer, suggestions
// ---------------------------------------------------------------------------------------------

fn Hero(c: &Composer, ui: &SharedUi, width_dp: f32, height_dp: f32, content_w: f32, l: Layout) {
    let p = ember_theme().palette;
    // `.home .hero{padding:10vh 0 ...}`, less the 30 dp the capture measured over the mockup.
    let top = if l.compact { 48.0 } else { (height_dp * 0.10 - 30.0).clamp(8.0, 96.0) };
    cu::Column().alignment(Alignment::Center).spacing(SP[4]).modifier(Modifier::new().fill_max_width()).content(|| {
        cu::Spacer().modifier(Modifier::new().height(top));
        // `.hero-bots`: three characters on their baseline, the middle one raised 6 px.
        let bot = if l.compact { 32.0 } else { 40.0 };
        cu::Row().spacing(10.0).alignment(Alignment::BottomStart).content(|| {
            HeroBot("claude", "done", bot);
            cu::Column().content(|| {
                HeroBot("codex", "run", bot);
                cu::Spacer().modifier(Modifier::new().height(6.0));
            });
            HeroBot("gemini", "wait", bot);
        });
        // The mockup greets the signed-in user by name.
        let greeting = format!("{}, {}", DEMO_USER.0, GREETING);
        Label(&greeting, if l.compact { Fs::Xl2 } else { Fs::Xl3 }, Weight::Medium, None, Some(2));
        ComposerCard(c, ui, composer_width(content_w), l);
        Suggestions(c, width_dp);
        cu::Row().spacing(SP[0]).alignment(Alignment::CenterStart).content(|| {
            Label("오늘의 팀", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
            NavIcon(super::shell::ICON_CHEVRON_DOWN_SVG, p.muted.to_argb(), 14.0);
        });
    });
}

/// The plain vendor character of `core.js` `bot()`: one shape per harness in its colour.
pub fn hero_bot_svg(vendor: &str, status: &str, px: f32, p: &Palette) -> String {
    let hex = |c: compose_rust::Color| format!("#{:06x}", c.to_argb() & 0x00ff_ffff);
    let (fill, shape) = match vendor {
        "codex" => (hex(p.blue.strong), "M13 8h14c3.9 0 6 2.1 6 6v13c0 3.9-2.1 6-6 6H13c-3.9 0-6-2.1-6-6V14c0-3.9 2.1-6 6-6z"),
        "gemini" => (hex(p.sky.strong), "M20 5.5c2.2 6.4 6.3 10.6 13.5 13.6-7.2 3-11.3 7.4-13.5 14.4-2.2-7-6.3-11.4-13.5-14.4C13.7 16.1 17.8 11.9 20 5.5z"),
        _ => (hex(p.orange.strong), "M20 6.5c8.2 0 13.8 5.6 13.8 14.2S28.4 34.5 20 34.5 6.2 29.3 6.2 20.7 11.8 6.5 20 6.5z"),
    };
    let ink = if vendor == "gemini" { "#26374f" } else { "#2a2522" };
    let mouth = match status {
        "run" => "M17 25.5h6",
        "wait" => "M18 25a2 2 0 0 0 4 0",
        "fail" => "M16.5 27q3.5-3 7 0",
        _ => "M16.5 24.5q3.5 3.2 7 0",
    };
    let size = px.round() as i32;
    let mut s = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{size}\" height=\"{size}\" viewBox=\"0 0 40 40\"><ellipse cx=\"20\" cy=\"37.6\" rx=\"9\" ry=\"1.5\" fill=\"#1c1b19\" fill-opacity=\".12\"/><path d=\"{shape}\" fill=\"{fill}\"/><path d=\"M11.5 23.5h2.5M26 23.5h2.5\" fill=\"none\" stroke=\"#ffffff\" stroke-opacity=\".35\" stroke-width=\"2.2\" stroke-linecap=\"round\"/><ellipse cx=\"15.5\" cy=\"19\" rx=\"1.9\" ry=\"2.5\" fill=\"{ink}\"/><ellipse cx=\"24.5\" cy=\"19\" rx=\"1.9\" ry=\"2.5\" fill=\"{ink}\"/><path d=\"{mouth}\" fill=\"none\" stroke=\"{ink}\" stroke-width=\"1.6\" stroke-linecap=\"round\"/>"
    );
    if status == "wait" {
        s.push_str(&format!("<circle cx=\"35\" cy=\"15\" r=\"3\" fill=\"{fill}\"/>"));
    }
    if status == "run" {
        s.push_str(&format!("<circle cx=\"31\" cy=\"7\" r=\"1.5\" fill=\"{0}\"/><circle cx=\"35\" cy=\"5\" r=\"1.2\" fill=\"{0}\"/>", hex(p.blue.strong)));
    }
    s.push_str("</svg>");
    s
}

fn HeroBot(vendor: &str, status: &str, px: f32) {
    let doc = hero_bot_svg(vendor, status, px, ember_theme().palette);
    cu::Image(compose_rust::asset(compose_rust::AssetKind::Svg, shell::leak_doc(doc))).modifier(Modifier::new().size(px, px));
}

/// A composer chip (`.pickchip`): a 32 px `surface-2` pill built like `WarnChip`: 16 px glyph,
/// 13 px weight 500 label cut with an ellipsis, and the 14 px chevron of a picker. `icon_only`
/// chips are 32 px circles with the glyph centred.
/// TODO(compose-rust#372): a pointer-up `Clickable` with no keyboard focus or role.
fn ComposerChip(
    text: &str,
    icon: &'static str,
    chevron: bool,
    selected: bool,
    icon_only: bool,
    weighted: bool,
    on_click: impl FnMut() + 'static,
) {
    let p = ember_theme().palette;
    let fill = if selected { p.sel } else { p.surface_2 };
    let modifier = Modifier::new().height(H_CTL).background(paint(fill)).shape_role(ShapeRole::Full);
    let modifier = if icon_only { modifier.width(H_CTL) } else if weighted { modifier.weight(1.0) } else { modifier };
    Clickable(modifier, Alignment::Center, on_click, || {
        if icon_only {
            NavIcon(icon, p.text_2.to_argb(), 16.0);
            return;
        }
        cu::Row().alignment(Alignment::CenterStart).content(|| {
            cu::Spacer().modifier(Modifier::new().width(SP[2]));
            NavIcon(icon, p.text_2.to_argb(), 16.0);
            cu::Spacer().modifier(Modifier::new().width(SP[1]));
            if weighted {
                cu::Box().modifier(Modifier::new().weight(1.0)).content(|| {
                    Label(text, Fs::Sm, Weight::Medium, Some(paint(p.text_2)), Some(1));
                });
            } else {
                Label(text, Fs::Sm, Weight::Medium, Some(paint(p.text_2)), Some(1));
            }
            if chevron {
                cu::Spacer().modifier(Modifier::new().width(SP[1]));
                NavIcon(super::shell::ICON_CHEVRON_DOWN_SVG, p.muted.to_argb(), 14.0);
            }
            cu::Spacer().modifier(Modifier::new().width(SP[2]));
        });
    });
}

/// A suggestion chip (`.chip.outline`): 32 px, `surface` with a 1 px line, an 18 px glyph and
/// the label at 13 px weight 500.
fn SuggestionChip(text: &str, icon: &'static str, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    Clickable(
        Modifier::new()
            .height(H_CTL)
            .background(paint(p.surface))
            .border(1.0, paint(p.line))
            .corner_radius(H_CTL / 2.0),
        Alignment::Center,
        on_click,
        || {
            cu::Row().alignment(Alignment::CenterStart).content(|| {
                cu::Spacer().modifier(Modifier::new().width(SP[2]));
                NavIcon(icon, p.text_2.to_argb(), 18.0);
                cu::Spacer().modifier(Modifier::new().width(SP[1]));
                Label(text, Fs::Sm, Weight::Medium, Some(paint(p.text_2)), Some(1));
                cu::Spacer().modifier(Modifier::new().width(SP[2]));
            });
        },
    );
}

/// The side of the Home send button (`.sendround`).
pub const SEND_BUTTON_DP: f32 = 36.0;

/// Whether a composer chip draws the picker chevron: the project, model and mode pickers do, the
/// computer chip does not (mockup `.mp-chev`, FR-L11).
fn chip_has_chevron(pick: Pick) -> bool {
    pick != Pick::Computer
}

/// The composer's mode chip while bypass is on: `red-soft` fill, red text, the warning triangle.
fn WarnChip(text: &str, icon_only: bool, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    Clickable(
        Modifier::new().height(H_CTL).background(paint(p.red.soft)).shape_role(ShapeRole::Full),
        Alignment::Center,
        on_click,
        || {
            cu::Row().alignment(Alignment::CenterStart).content(|| {
                cu::Spacer().modifier(Modifier::new().width(if icon_only { SP[1] } else { SP[2] }));
                NavIcon(ICON_WARN, p.red.strong.to_argb(), 16.0);
                if !icon_only {
                    cu::Spacer().modifier(Modifier::new().width(SP[0]));
                    Label(text, Fs::Sm, Weight::Medium, Some(paint(p.red.strong)), Some(1));
                }
                cu::Spacer().modifier(Modifier::new().width(if icon_only { SP[1] } else { SP[2] }));
            });
        },
    );
}

fn ComposerCard(c: &Composer, ui: &SharedUi, w: f32, l: Layout) {
    let theme = ember_theme();
    let p = theme.palette;
    let sel = c.selection.get();
    let (_, computers) = c.live(ui);
    let message = effective_message(&c.typed.get(), c.suggestion.get().as_deref());
    let sendable = can_send(&message, c.busy.get());
    let placeholder = c.suggestion.get().unwrap_or_else(|| COMPOSER_PLACEHOLDER.to_string());
    let computer_text = crate::ui::home::computer_chip_text(sel.computer.as_deref(), &computers);
    let computer_text = if sel.computer.is_none() { c.local_name(ui) } else { computer_text };
    let icon_only = l.compact;
    let gap = if l.compact { SP[0] } else { 6.0 };
    let model_text = sel.option.chip_label();
    let mode_text = sel.mode.label().to_string();
    let chip = |text: &str, icon: &'static str, pick: Pick, selected: bool| {
        let open = c.open.clone();
        // Wide chips are content-sized, their text cut to the budget; only the compact project
        // chip takes the rest of the row.
        ComposerChip(text, icon, chip_has_chevron(pick), selected, icon_only && pick != Pick::Project, icon_only && pick == Pick::Project, move || open.set(Some(pick)));
    };
    let add = || {
        // TODO(compose-rust#483, #500): attachments need the file picker (planned for v0.0.8)
        // and an upload API. Until then the button is dimmed and says why when tapped.
        // TODO(compose-rust#429): the dimmed wrapper has no disabled accessible name.
        cu::Box().modifier(Modifier::new().alpha(0.5)).content(|| {
            super::components::IconButton(IconRole::Add, true, || notice::post(ATTACH_UNSUPPORTED));
        });
    };
    let send = || {
        let (c2, ui2) = (c.clone(), ui.clone());
        // `.sendround`: a 36 px `--ink` circle with the paper plane, always ink. Composer::send
        // guards `can_send`, so an empty input does nothing.
        // TODO(compose-rust#428, #429): the circle has no accessible name ("보내기").
        super::session::SendCircleButton(SEND_BUTTON_DP, move || c2.send(&ui2));
    };

    // `.bigcomposer`: surface, 1 px line, the xl radius, 16 / 16 / 12 padding.
    cu::Box()
        .modifier(
            with_shadows(Modifier::new(), theme.shadow_lg())
                .width(w)
                .background(paint(p.surface))
                .border(1.0, paint(p.line))
                .corner_radius(CORNER_XL)
                .padding(if l.compact { SP[2] } else { SP[3] }),
        )
        .content(|| {
            cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                let (typed, ui2, c2) = (c.typed.clone(), ui.clone(), c.clone());
                // TODO(compose-rust#374, #376): the field is uncontrolled with no initial value
                // and carries the toolkit frame, so a suggestion shows as the placeholder.
                EmberField(FieldOptions {
                    fill_max_width: true,
                    multiline: true,
                    bare: true,
                    height: Some(if l.compact { 64.0 } else { 84.0 }),
                    placeholder,
                    on_value_change: Some(Box::new(move |v| typed.set(v))),
                    on_submit: Some(Box::new(move |_| c2.send(&ui2))),
                    ..Default::default()
                });
                if c.suggestion.get().is_some() {
                    cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                        Label("제안을 고르셨습니다. 입력하지 않으면 이 내용으로 시작합니다", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
                        cu::Spacer().modifier(Modifier::new().weight(1.0));
                        let s = c.suggestion.clone();
                        PickChip("취소", None, false, false, move || s.set(None));
                    });
                }
                cu::Row().spacing(gap).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                    cu::Box().modifier(Modifier::new().size(36.0, 36.0)).content(add);
                    cu::Column().spacing(SP[0]).modifier(Modifier::new().weight(1.0).clip(true)).content(|| {
                        // Bypass draws the chip in the warning state (mockup `.mdpick.warn`).
                        let chip_at = |i: usize, text: &str| match i {
                            0 => chip(text, ICON_FOLDER, Pick::Project, false),
                            1 => {
                                let close = {
                                    let o = c.open.clone();
                                    move || o.set(None)
                                };
                                let one = || chip(text, ICON_TERMINAL, Pick::Model, false);
                                if l.compact {
                                    one();
                                } else {
                                    c.model_popover(ui, &sel, one, close);
                                }
                            }
                            2 => {
                                let one = || {
                                    if sel.mode.is_bypass() {
                                        let open = c.open.clone();
                                        WarnChip(text, icon_only, move || open.set(Some(Pick::Mode)));
                                    } else {
                                        chip(text, ICON_SHIELD, Pick::Mode, false);
                                    }
                                };
                                if l.compact {
                                    one();
                                } else {
                                    c.mode_popover(&sel, one);
                                }
                            }
                            _ => chip(text, ICON_PC, Pick::Computer, false),
                        };
                        let texts = [sel.project.clone(), model_text.clone(), mode_text.clone(), computer_text.clone()];
                        if l.compact {
                            cu::Row().spacing(gap).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                                for (i, t) in texts.iter().enumerate() {
                                    chip_at(i, t);
                                }
                            });
                        } else {
                            let fitted: Vec<String> = texts
                                .iter()
                                .zip(composer_chip_budget(w))
                                .map(|(t, b)| crate::ui::home::composer_fit_text(t, b))
                                .collect();
                            let widths: Vec<f32> = fitted.iter().map(|t| composer_chip_width(t)).collect();
                            // A line per row of chips: a narrow card wraps rather than drops a chip.
                            for row in composer_chip_rows(&widths, w) {
                                cu::Row().spacing(gap).alignment(Alignment::CenterStart).content(|| {
                                    for i in row {
                                        chip_at(i, &fitted[i]);
                                    }
                                });
                            }
                        }
                    });
                    if c.busy.get() {
                        cu::IndeterminateProgressIndicator().circular(true).modifier(Modifier::new().size(18.0, 18.0));
                    }
                    send();
                });
            });
        });
}

/// The glyph of the `i`th suggestion in a line. TODO(item 6): the mockup's spark star needs the
/// harness glyph asset; `code.svg` stands in until it lands.
fn suggestion_icon(i: usize) -> &'static str {
    [ICON_CODE, ICON_GIT, ICON_CHART_NAV][i % 3]
}

/// `.sugg`: outlined chips, three a line (two up to 900 px), centred.
fn Suggestions(c: &Composer, width_dp: f32) {
    let n = suggestion_cols(width_dp);
    cu::Column().alignment(Alignment::Center).spacing(SP[1]).content(|| {
        for line in SUGGESTIONS.chunks(n) {
            cu::Row().spacing(SP[1]).content(|| {
                for (i, text) in line.iter().enumerate() {
                    let s = c.suggestion.clone();
                    let t = apply_suggestion(&c.typed.get_untracked(), text);
                    SuggestionChip(text, suggestion_icon(i), move || s.set(Some(t.clone())));
                }
            });
        }
    });
}

// ---------------------------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------------------------

fn Dashboard(d: &Dash, ui: &SharedUi, l: Layout, content_w: f32, skip: &Skip) {
    let week = services().client.read(|s| weekday_usage(s, now_ms()));
    let p = ember_theme().palette;
    let k = kpis(&d.counts, d.online, d.computers);
    cu::Column().spacing(SP[4]).modifier(Modifier::new().fill_max_width()).content(|| {
        if skip.show(EST_HEAD) {
            cu::Column().content(|| {
                Label("오늘의 팀", if l.compact { Fs::Xl } else { Fs::Xl2 }, Weight::Semibold, None, Some(1));
                cu::Spacer().modifier(Modifier::new().height(10.0));
                Label(&format!("{} · 서버 기준", date_line(&crate::ui::home::today_utc())), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                cu::Spacer().modifier(Modifier::new().height(12.0));
            });
            KpiStrip(&k, &d.counts, l);
        }
        let stacked = !l.wide;
        // The trend's chart is a fixed-width canvas: the card's inner width.
        let chart_w = if l.wide { content_w * 8.0 / 12.0 - GAP_CARD - 2.0 * PAD_CARD_H } else { content_w - 2.0 * PAD_CARD_H }.max(200.0);
        if skip.show(EST_TREND) {
            Weighted(stacked, GAP_CARD, vec![
                (8.0, Box::new(|| TokenTrendTile(ui, chart_w)) as Part),
                (4.0, Box::new(|| StatusTile(&d.counts, ui, if l.wide { (content_w - GAP_CARD) * 4.0 / 12.0 } else { content_w }, l.compact))),
            ]);
        }
        let usage_w = if l.wide { (content_w - GAP_CARD) * 7.0 / 12.0 - 2.0 * PAD_CARD_H } else { content_w - 2.0 * PAD_CARD_H }.max(200.0);
        if skip.show(EST_USAGE) {
            Weighted(stacked, GAP_CARD, vec![
                (7.0, Box::new(|| HarnessTile(&d.harness, &week, ui, usage_w))),
                (5.0, Box::new(|| CrewTile(&d.crew, d.online, d.computers, ui, content_w * 5.0 / 12.0))),
            ]);
        }
        if skip.show(EST_EFF) {
            Unavailable(ICON_ZAP, "팀원별 효율");
        }
        // Three small tiles: in a row when wide, success beside wait up to 900, else stacked.
        if !skip.show(EST_SMALL) {
        } else if l.wide {
            Weighted(false, GAP_CARD, vec![
                (1.0, Box::new(|| SuccessTile(&d.counts)) as Part),
                (1.0, Box::new(|| UnavailableH(ICON_CLOCK, "승인 대기 시간", Some(ROW_TILE_H)))),
                (1.0, Box::new(|| UnavailableH(shell::ICON_CPU_SVG, "컴퓨터 부하", Some(ROW_TILE_H)))),
            ]);
        } else {
            Weighted(!l.pair_mid, GAP_CARD, vec![
                (1.0, Box::new(|| SuccessTile(&d.counts)) as Part),
                (1.0, Box::new(|| UnavailableH(ICON_CLOCK, "승인 대기 시간", if l.pair_mid { Some(ROW_TILE_H) } else { None }))),
            ]);
            Unavailable(shell::ICON_CPU_SVG, "컴퓨터 부하");
        }
        ProjectsTile(&d.projects, ui, l);
        IntegrationsArea();
    });
}

/// `연동` (FR-L23, #498): the GitHub card, then one small 준비 중 card per later service. The
/// card reads `GET /api/integrations/github` through the live slot (FR-L35). Before the first
/// answer, or when it failed, it says 준비 중입니다; a null number reads 알 수 없음. In the demo
/// the card shows a fixed sample and its header carries (예시). No number is drawn that the
/// server did not send.
fn IntegrationsArea() {
    let p = ember_theme().palette;
    let s = services();
    let demo = s.demo.is_some();
    let fetched = s.ui.live.github.get();
    let hook_sample = crate::ui::home::github_sample_from_env();
    let is_sample = demo || hook_sample.is_some();
    let sample = hook_sample.or_else(|| demo.then(crate::demo::github_state));
    let card = GithubCard::from_state(if is_sample { sample.as_ref() } else { fetched.state() });
    let header_line = if is_sample { crate::ui::home::sample_header(&card.header_line) } else { card.header_line.clone() };
    let guidance = remember(|| mutable_state_of(false));
    cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
        Label(INTEGRATIONS_TITLE, Fs::Lg, Weight::Semibold, None, Some(1));
        Label(&header_line, Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
        Tile(|| {
            cu::Row().spacing(10.0).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                cu::Box()
                    .alignment(Alignment::Center)
                    .modifier(Modifier::new().size(30.0, 30.0).border(1.0, paint(p.line)).corner_radius(CORNER_SM))
                    .content(|| Label("G", Fs::Md, Weight::Semibold, None, Some(1)));
                Label(GITHUB_NAME, Fs::Lg, Weight::Semibold, None, Some(1));
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                let tone = match card.tone {
                    GithubPill::Soon => PillTone::Grey,
                    GithubPill::Off => PillTone::Grey,
                    GithubPill::Warn => PillTone::Orange,
                    GithubPill::On => PillTone::Green,
                };
                Pill(card.pill, tone, true);
            });
            if let Some(account) = &card.account {
                Label(account, Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
            }
            if let Some(note) = &card.note {
                Label(note, Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
            }
            if let Some(action) = &card.action {
                // Pressing only shows how to log in; Ember runs nothing and stores no token.
                let g = guidance.clone();
                AppButton(action.label, None, ButtonKind::Default, true, true, false, move || g.set(true));
                if guidance.get() {
                    Label(&action.guidance, Fs::Md, Weight::Regular, None, None);
                }
            }
            for (label, value) in &card.kpis {
                cu::Row().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                    Label(label, Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                    Label(value, Fs::Md, Weight::Regular, None, Some(1));
                });
            }
            for r in &card.repos {
                cu::Column().spacing(4.0).modifier(Modifier::new().fill_max_width()).content(|| {
                    cu::Row().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                        Label(&r.name, Fs::Md, Weight::Medium, None, Some(1));
                        cu::Spacer().modifier(Modifier::new().weight(1.0));
                        if r.error.is_none() {
                            Label(&format!("PR {} · 리뷰 {}", r.open_prs, r.review_requested), Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
                        }
                    });
                    if let Some(e) = &r.error {
                        Label(e, Fs::Md, Weight::Regular, Some(paint(p.red.strong)), None);
                    } else if !r.ci.is_empty() {
                        CiBar(&r.ci);
                    }
                });
            }
            if let Some(more) = &card.more_repos {
                Label(more, Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
            }
            // The server sends no list of what the integration did, so none is drawn.
            Label(card.activity_empty, Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
        });
        for name in LATER_INTEGRATIONS {
            Tile(|| {
                cu::Row().spacing(10.0).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                    Label(name, Fs::Md, Weight::Semibold, None, Some(1));
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                    Pill(LATER_PILL, PillTone::Grey, true);
                });
                Label(LATER_LINE, Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
            });
        }
    });
}

/// Recent CI results of one repository: small cells, newest first (left).
fn CiBar(results: &[CiResult]) {
    let p = ember_theme().palette;
    cu::Row().spacing(2.0).content(|| {
        for r in results {
            let c = match r {
                CiResult::Success => p.green.strong,
                CiResult::Failure => p.red.strong,
                CiResult::Cancelled => p.grey.strong,
                CiResult::Pending => p.track,
            };
            cu::Spacer().modifier(Modifier::new().size(8.0, 14.0).background(paint(c)).corner_radius(2.0));
        }
    });
}

/// `.hm-kpis`: label, the number with its unit and note pill; a 1 px rule between the columns.
fn KpiStrip(k: &[Kpi; 4], counts: &StatusCounts, l: Layout) {
    let cells: Vec<Part> = k
        .iter()
        .enumerate()
        .map(|(i, kpi)| -> Part {
            let first_in_line = i % l.kpi_cols == 0;
            Box::new(move || KpiCell(kpi, note_tone(i, counts), !first_in_line))
        })
        .collect();
    Cols(l.kpi_cols, SP[4], cells);
}

fn KpiCell(kpi: &Kpi, tone: PillTone, rule: bool) {
    let p = ember_theme().palette;
    cu::Row().modifier(Modifier::new().fill_max_width().height(64.0)).content(|| {
        if rule {
            cu::Spacer().modifier(Modifier::new().width(1.0).height(64.0).background(paint(p.line)));
            cu::Spacer().modifier(Modifier::new().width(SP[3]));
        }
        cu::Column().spacing(6.0).modifier(Modifier::new().weight(1.0)).content(|| {
            Label(kpi.label, Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
            if kpi.available {
                cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).content(|| {
                    // The unit hugs the number (`small{margin-left:2px}`).
                    cu::Row().spacing(0.0).alignment(Alignment::CenterStart).content(|| {
                        Label(&kpi.value, Fs::Xl2, Weight::Regular, None, Some(1));
                        if !kpi.unit.is_empty() {
                            Label(kpi.unit, Fs::Md, Weight::Medium, Some(paint(p.muted)), Some(1));
                        }
                    });
                    if let Some(note) = &kpi.note {
                        Pill(note, tone, true);
                    }
                });
            } else {
                Label(&kpi.value, Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
                if let Some(note) = &kpi.note {
                    Label(note, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                }
            }
        });
    });
}

/// A tile for what the server does not report yet: the title and "준비 중입니다".
/// TODO(server, FR-U7, FR-X7): approval wait, per-member efficiency and computer load.
fn Unavailable(icon: &'static str, title: &str) {
    UnavailableH(icon, title, None);
}

fn UnavailableH(icon: &'static str, title: &str, height: Option<f32>) {
    let p = ember_theme().palette;
    TileH(height, || {
        CardHead(icon, title, || {});
        Label(UNAVAILABLE, Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
    });
}

/// Token bars per day for every harness, 7 or 14 UTC days (FR-U7, #332), from
/// `GET /api/usage/summary`. The cost is not priced (no table in the SPEC) and the tile says so.
/// The hatched bars (Ember #190) are a `Canvas` at the card's width.
fn TokenTrendTile(ui: &SharedUi, chart_w: f32) {
    let p = ember_theme().palette;
    let days = remember(|| mutable_state_of(usage::DEFAULT_WINDOW));
    let _requested = remember(|| {
        crate::bridge::request_usage_summary();
        0u8
    });
    let load = ui.live.usage_summary.get();
    let series = usage::trend_days(&load, days.get());
    let values: Vec<u64> = series.iter().map(|(_, t)| *t).collect();
    const H: f32 = 160.0;
    let usize_values: Vec<usize> = values.iter().map(|v| *v as usize).collect();
    let rects = crate::ui::home::bar_rects(&usize_values, chart_w, H, 6.0);
    let now = rects.len().saturating_sub(1);
    let mut list = DrawList::builder().rect(paint(p.line), 0.0, H, chart_w, 1.0, 0.0);
    for (i, (x, y, w, h)) in rects.into_iter().enumerate() {
        if h > 0.0 {
            let color = paint(if i == now { p.orange.strong } else { p.grey.strong });
            list = list.round_rect(color, x, y, w, h, 3.0, 1.0);
            for (x1, y1, x2, y2) in crate::ui::home::hatch_lines(x, y, w, h, usage::legend_hatch_style().0) {
                list = list.line(color, x1, y1, x2, y2, 1.0);
            }
        }
    }
    let commands = list.build();
    let legend = usage::cost_legend(
        usage::period_cost(&load, days.get()).is_some(),
        usage::window_total(&load, days.get()).is_some(),
        layout(compose_rust::runtime::current_window_size().width_dp).compact,
    );
    let first = series.first().map(|(d, _)| d.clone()).unwrap_or_default();
    let last = series.last().map(|(d, _)| d.clone()).unwrap_or_default();
    Tile(|| {
        CardHead(shell::ICON_GAUGE_SVG, "비용과 토큰 추이", || {
            for e in &legend {
                DotOrHatch(e.hatched, p.grey.strong, p.text);
                Label(e.label, Fs::Xs2, Weight::Regular, Some(paint(p.muted)), Some(1));
                cu::Spacer().modifier(Modifier::new().width(SP[2]));
            }
            for n in [7usize, 14] {
                let d = days.clone();
                // The mockup's toggle pill is 42 x 26.
                let fill = if days.get() == n { p.sel } else { p.surface_2 };
                let on = days.get() == n;
                Clickable(
                    Modifier::new().size(42.0, 26.0).background(paint(fill)).shape_role(ShapeRole::Full),
                    Alignment::Center,
                    move || d.set(n),
                    || {
                        Label(
                            &format!("{n}일"),
                            Fs::Xs,
                            Weight::Medium,
                            Some(paint(if on { p.text } else { p.text_2 })),
                            Some(1),
                        );
                    },
                );
                cu::Spacer().modifier(Modifier::new().width(SP[0]));
            }
        });
        cu::Canvas(commands).modifier(Modifier::new().size(chart_w, H + 1.0));
        cu::Row().modifier(Modifier::new().fill_max_width()).content(|| {
            Label(&first, Fs::Xs2, Weight::Regular, Some(paint(p.muted)), Some(1));
            cu::Spacer().modifier(Modifier::new().weight(1.0));
            Label(&last, Fs::Xs2, Weight::Regular, Some(paint(p.muted)), Some(1));
        });
        Label(&usage::trend_line(&load, days.get()), Fs::Sm, Weight::Regular, None, None);
        // Four stats from the real data; a missing one reads "데이터 없음". Cache hits stay without
        // data: the usage event has no cache tokens (FR-U7).
        cu::Row().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
            for (label, value) in usage::cost_stats(&load, days.get()) {
                cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                    Label(label, Fs::Xs2, Weight::Regular, Some(paint(p.muted)), Some(1));
                    Label(&value, Fs::Sm, Weight::Semibold, None, Some(1));
                });
            }
        });
        // TODO(compose-rust): a tooltip or per-bar tap needs hover or a click on the Canvas; the
        // toolkit has neither, so no day is selectable.
    });
}

/// 팀원 상태 (`.hm-status`): the waiting alert, four coloured blocks sized by count, a legend.
fn StatusTile(c: &StatusCounts, ui: &SharedUi, tile_w: f32, compact: bool) {
    let p = ember_theme().palette;
    let parts = [
        ("실행", c.running, p.blue.strong),
        ("승인 대기", c.waiting, p.orange.strong),
        ("새 결과", c.unread, p.sky.strong),
        ("완료", c.done, p.grey.strong),
    ];
    Tile(|| {
        CardHead(shell::ICON_USERS_SVG, "팀원 상태", || {
            let u = ui.clone();
            GoButton(move || shell::choose(&u, NavMenu::Sessions));
        });
        // `.hm-alert`: a `surface-2` pill with the waiting count and the failures.
        cu::Row()
            .alignment(Alignment::CenterStart)
            .modifier(Modifier::new().fill_max_width().height(36.0).background(paint(p.surface_2)).shape_role(ShapeRole::Full))
            .content(|| {
                cu::Spacer().modifier(Modifier::new().width(14.0));
                Label(&format!("승인 대기 {}명", c.waiting), Fs::Sm, Weight::Regular, None, Some(1));
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                Pill(&format!("실패 {}", c.failed), if c.failed > 0 { PillTone::Red } else { PillTone::Green }, true);
                cu::Spacer().modifier(Modifier::new().width(SP[0]));
            });
        // `.hm-blocks`: coloured blocks sized by count, each with the 18 % white stripes. The
        // stripes are a canvas at the block's estimated width (the card's inner width shared by
        // the weights, less 8 dp so an estimate that is a little high stops short rather than
        // spilling past the block); TODO(compose-rust#388): size-aware drawing so the stripes follow their box.
        let bh = if compact { 88.0 } else { 116.0 };
        let weights: Vec<f32> = parts.iter().map(|(_, n, _)| (*n as f32).max(0.6)).collect();
        let sum: f32 = weights.iter().sum();
        let inner = (tile_w - 2.0 * PAD_CARD_H - 3.0 * 6.0).max(60.0);
        cu::Row().spacing(6.0).modifier(Modifier::new().fill_max_width().height(bh)).content(|| {
            for ((_, _, color), wt) in parts.iter().zip(weights.iter()) {
                let w = inner * wt / sum;
                cu::Box()
                    .modifier(Modifier::new().weight(*wt).height(bh).background(paint(*color)).corner_radius(CORNER_MD))
                    .content(|| {
                        let stripe = paint(compose_rust::Color::argb(0x2eff_ffff));
                        let mut list = DrawList::builder();
                        for (x1, y1, x2, y2) in crate::ui::home::hatch_lines(0.0, 0.0, (w - 8.0).max(1.0), bh, 7.0) {
                            list = list.line(stripe, x1, y1, x2, y2, 1.0);
                        }
                        cu::Canvas(list.build()).modifier(Modifier::new().size((w - 8.0).max(1.0), bh));
                    });
            }
        });
        Cols(2, SP[2], parts.iter().map(|(label, n, color)| -> Part {
            Box::new(move || {
                cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                    Dot(*color);
                    Label(label, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                    Label(&n.to_string(), Fs::Xl, Weight::Regular, None, None);
                });
            })
        }).collect());
    });
}

/// One day of the usage bars: its weekday and the sessions touched that day per harness
/// (Claude Code, Codex, Gemini).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeekDay {
    pub label: &'static str,
    pub counts: [usize; 3],
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The last seven UTC days ending at `now_ms`, oldest first, with the sessions last updated on
/// each day per harness. A session count, not tokens (TODO(server, FR-U7): token usage).
pub fn weekday_usage(state: &ember_client::State, now_ms: i64) -> Vec<WeekDay> {
    const DAY: i64 = 86_400_000;
    let today = now_ms.div_euclid(DAY);
    let names = ["일", "월", "화", "수", "목", "금", "토"];
    let mut days: Vec<WeekDay> = (0..7)
        .map(|i| WeekDay { label: names[((today - 6 + i + 4).rem_euclid(7)) as usize], counts: [0; 3] })
        .collect();
    for p in state.projects() {
        for id in &p.sessions {
            let Some(v) = state.session(id).filter(|v| !v.record.archived) else { continue };
            let age = today - v.record.updated_at.div_euclid(DAY);
            if !(0..7).contains(&age) {
                continue;
            }
            let Some(h) = Harness::from_kind(&v.record.agent) else { continue };
            let i = Harness::ALL.iter().position(|x| *x == h).unwrap_or(0);
            days[(6 - age) as usize].counts[i] += 1;
        }
    }
    days
}

/// Greedy rows of legend entries: the indices of `widths` (dp, each entry with its dot) that fit
/// `max_w` per row with `gap` between. An entry wider than the row gets a row of its own.
pub fn legend_rows(widths: &[f32], max_w: f32, gap: f32) -> Vec<Vec<usize>> {
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut used = 0.0f32;
    for (i, w) in widths.iter().enumerate() {
        match rows.last_mut() {
            Some(row) if used + gap + w <= max_w => {
                row.push(i);
                used += gap + w;
            }
            _ => {
                rows.push(vec![i]);
                used = *w;
            }
        }
    }
    rows
}

fn vendor_color(v: usage::Vendor, p: &Palette) -> compose_rust::Color {
    match v {
        usage::Vendor::Claude => p.orange.strong,
        usage::Vendor::Codex => p.blue.strong,
        usage::Vendor::Gemini => p.sky.strong,
        usage::Vendor::Other => p.grey.strong,
    }
}

/// 하네스별 사용량 (`.hm-usage`, FR-L23, #360): today's tokens, a 7-day stacked chart of the
/// tokens each harness reported (`days[].by_agent`) with the average as a dashed line, a legend
/// that wraps, and the session counts underneath. Unreported harnesses are not drawn.
/// `chart_w` is the card's inner width.
fn HarnessTile(bars: &[HarnessBar], week: &[WeekDay], ui: &SharedUi, chart_w: f32) {
    let p = ember_theme().palette;
    let _requested = remember(|| {
        crate::bridge::request_usage_summary();
        0u8
    });
    let load = ui.live.usage_summary.get();
    let model = usage::home_stack(&load, 7);
    let sessions_today = week.last().map(|d| d.counts.iter().sum::<usize>()).unwrap_or(0);
    let sessions_week: usize = week.iter().map(|d| d.counts.iter().sum::<usize>()).sum();
    const H: f32 = 100.0;
    const GAP: f32 = 6.0;
    let totals: Vec<usize> = model.days.iter().map(|d| d.total as usize).collect();
    let rects = crate::ui::home::bar_rects(&totals, chart_w, H, GAP);
    let max = totals.iter().copied().max().unwrap_or(0);
    let mut list = DrawList::builder().rect(paint(p.line), 0.0, H, chart_w, 1.0, 0.0);
    for (day, (x, y, w, h)) in model.days.iter().zip(rects.iter().copied()) {
        let mut top = y + h;
        for seg in &day.segs {
            let sh = h * seg.fraction;
            top -= sh;
            list = list.rect(paint(vendor_color(seg.vendor, &p)), x, top, w, sh, 0.0);
        }
    }
    if max > 0 {
        // The average as a dashed line across the chart.
        let ay = H - H * (model.average_tokens as f32 / max as f32);
        let mut x = 0.0;
        while x < chart_w {
            list = list.rect(paint(p.text), x, ay, 4.0f32.min(chart_w - x), 1.0, 0.0);
            x += 8.0;
        }
    }
    let commands = list.build();
    let bar_w = rects.first().map(|r| r.2).unwrap_or(0.0);
    let widths: Vec<f32> = model.legend.iter().map(|e| 8.0 + 4.0 + text_px(&e.label, 12.0)).collect();
    let rows = legend_rows(&widths, chart_w, 16.0);
    Tile(|| {
        CardHead(ICON_CHART, "하네스별 사용량", || {
            let u = ui.clone();
            AppButton("자세히", None, ButtonKind::Default, true, true, false, move || shell::choose(&u, NavMenu::Agents));
        });
        Label(&model.today, Fs::Xl2, Weight::Regular, None, Some(1));
        if let Some(notice) = &model.notice {
            Label(notice, Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
            if model.retry {
                AppButton(usage::RETRY, None, ButtonKind::Default, true, true, false, crate::bridge::request_usage_summary);
            }
        } else {
            Label(&model.average, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
            cu::Canvas(commands).modifier(Modifier::new().size(chart_w, H + 1.0));
            cu::Row().spacing(GAP).modifier(Modifier::new().fill_max_width()).content(|| {
                for (i, day) in model.days.iter().enumerate() {
                    let today = i + 1 == model.days.len();
                    cu::Box().alignment(Alignment::Center).modifier(Modifier::new().width(bar_w)).content(|| {
                        Label(day.weekday, Fs::Xs2, if today { Weight::Semibold } else { Weight::Regular }, Some(paint(if today { p.text } else { p.muted })), Some(1));
                    });
                }
            });
            Label(model.axis_caption, Fs::Xs2, Weight::Regular, Some(paint(p.muted)), Some(1));
            for row in &rows {
                cu::Row().spacing(16.0).content(|| {
                    for &i in row {
                        let e = &model.legend[i];
                        cu::Row().spacing(SP[0]).alignment(Alignment::CenterStart).content(|| {
                            Dot(vendor_color(e.vendor, &p));
                            Label(&e.label, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                        });
                    }
                });
            }
            // The dashed average line has its own legend entry.
            cu::Row().spacing(SP[0]).alignment(Alignment::CenterStart).content(|| {
                cu::Canvas(DrawList::builder().rect(paint(p.text), 0.0, 0.0, 3.0, 1.0, 0.0).rect(paint(p.text), 6.0, 0.0, 3.0, 1.0, 0.0).rect(paint(p.text), 12.0, 0.0, 3.0, 1.0, 0.0).build()).modifier(Modifier::new().size(15.0, 1.0));
                Label(model.average_legend, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
            });
        }
        Label(usage::COST_UNKNOWN, Fs::Xs2, Weight::Regular, Some(paint(p.muted)), Some(1));
        // The session counts stay, below the chart.
        let total: usize = bars.iter().map(|b| b.sessions).sum();
        if bars.is_empty() {
            Label(EMPTY_HARNESS_USAGE, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
        } else {
            Label(&format!("오늘 세션 {sessions_today} · 7일 세션 {sessions_week} · 전체 {total}"), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
        }
    });
}

/// 지금 일하는 팀원 (`.b-crew`): the working members, each opening its conversation.
fn CrewTile(crew: &[CrewMember], online: usize, computers: usize, ui: &SharedUi, card_w: f32) {
    let p = ember_theme().palette;
    let running = crew.iter().filter(|m| !m.waiting).count();
    let waiting = crew.len() - running;
    let cols = (((card_w - 2.0 * PAD_CARD_H) / 96.0) as usize).clamp(2, 6);
    Tile(|| {
        cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width().height(30.0)).content(|| {
            Label("지금 일하는 팀원", Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
            cu::Spacer().modifier(Modifier::new().weight(1.0));
            Pill(&format!("실행 {running}"), PillTone::Blue, false);
            if waiting > 0 {
                Pill(&format!("승인 대기 {waiting}"), PillTone::Orange, false);
            }
            let u = ui.clone();
            GoButton(move || shell::choose(&u, NavMenu::Sessions));
        });
        if crew.is_empty() {
            Label(EMPTY_CREW, Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
        }
        Cols(cols, SP[2], crew.iter().map(|m| -> Part {
            Box::new(move || {
                let (u, id) = (ui.clone(), m.id.clone());
                Clickable(Modifier::new().fill_max_width(), Alignment::Center, move || u.open_conversation(&id), || {
                    cu::Column().alignment(Alignment::Center).spacing(SP[0]).modifier(Modifier::new().fill_max_width()).content(|| {
                        let state = if m.waiting { super::characters::CharState::Wait } else { super::characters::CharState::Run };
                        super::characters::Character(&m.agent, false, 52.0, &m.title, &m.project, state, None);
                        Label(&model::one_line(&m.title, 24), Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(2));
                    });
                });
            })
        }).collect());
        cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).content(|| {
            NavIcon(shell::ICON_CPU_SVG, p.muted.to_argb(), 14.0);
            Label(&computers_line(online, computers), Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
        });
    });
}

/// 작업 성공률 (`.hm-succ`): the donut of finished against failed sessions and its legend.
fn SuccessTile(c: &StatusCounts) {
    let p = ember_theme().palette;
    let ok = c.done + c.unread;
    let ended = ok + c.failed;
    let pct = success_percent(ok, c.failed);
    Tile(|| {
        CardHead(ICON_CHECK, "작업 성공률", || {});
        cu::Row().spacing(SP[4]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Box().alignment(Alignment::Center).modifier(Modifier::new().size(132.0, 132.0)).content(|| {
                let frac = |n: usize| if ended == 0 { 0.0 } else { n as f32 / ended as f32 };
                ring_image(132.0, 16.0, &[(p.green.strong.to_argb(), frac(ok)), (p.red.strong.to_argb(), frac(c.failed))]);
                cu::Column().alignment(Alignment::Center).content(|| {
                    let text = pct.map(|v| format!("{v}%")).unwrap_or_else(|| "집계 전".to_string());
                    Label(&text, Fs::Xl2, Weight::Regular, None, Some(1));
                    Label(&format!("{ended}건"), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                });
            });
            cu::Column().spacing(SP[1]).modifier(Modifier::new().weight(1.0)).content(|| {
                for (label, n, color) in [("성공", ok, p.green.strong), ("실패", c.failed, p.red.strong)] {
                    cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                        Dot(color);
                        Label(label, Fs::Sm, Weight::Regular, None, Some(1));
                        cu::Spacer().modifier(Modifier::new().weight(1.0));
                        Label(&n.to_string(), Fs::Sm, Weight::Semibold, None, None);
                    });
                }
                Label("CI 결과는 GitHub 연동에서 옵니다", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
            });
        });
    });
}

/// The first `max` sessions of a project as (agent, status, title), for the tile's robot row.
pub fn project_members(state: &ember_client::State, project: &str, max: usize) -> Vec<(String, ember_client::LauncherStatus, String)> {
    state
        .projects()
        .into_iter()
        .find(|p| p.name == project)
        .map(|p| {
            p.sessions
                .iter()
                .filter_map(|id| state.session(id).filter(|v| !v.record.archived))
                .take(max)
                .map(|v| (v.record.agent.clone(), v.status, v.record.title.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// 프로젝트 (`.b-proj`): each project as a tile that opens it in 프로젝트.
fn ProjectsTile(tiles: &[ProjectTile], ui: &SharedUi, l: Layout) {
    let p = ember_theme().palette;
    Tile(|| {
        cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width().height(32.0)).content(|| {
            Label("프로젝트", Fs::Lg, Weight::Semibold, None, Some(1));
            cu::Spacer().modifier(Modifier::new().weight(1.0));
            let u = ui.clone();
            AppButton("모두 보기", None, ButtonKind::Default, true, true, false, move || {
                u.set_selected_project(None);
                shell::choose(&u, NavMenu::Projects);
            });
        });
        cu::Spacer().modifier(Modifier::new().height(SP[0]));
        if tiles.is_empty() {
            Label(EMPTY_PROJECTS, Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
        }
        Cols(l.proj_cols, SP[1], tiles.iter().map(|t| -> Part {
            Box::new(move || {
                let (u, name) = (ui.clone(), t.name.clone());
                // `.ptile`: a `surface-2` row, a 40 px ring with the member count, the name and
                // the counts. TODO(compose-rust#372): a pointer-up `Clickable`.
                Clickable(
                    Modifier::new().fill_max_width().background(paint(p.surface_2)).corner_radius(CORNER_MD).padding(10.0),
                    Alignment::CenterStart,
                    move || {
                        u.set_selected_project(Some(name.clone()));
                        shell::choose(&u, NavMenu::Projects);
                    },
                    || {
                        cu::Row().spacing(10.0).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                            cu::Box().alignment(Alignment::Center).modifier(Modifier::new().size(40.0, 40.0)).content(|| {
                                let n = t.total.max(1) as f32;
                                let rest = t.total.saturating_sub(t.running + t.waiting);
                                ring_image(40.0, 5.0, &[
                                    (p.blue.strong.to_argb(), t.running as f32 / n),
                                    (p.orange.strong.to_argb(), t.waiting as f32 / n),
                                    (p.sky.strong.to_argb(), rest as f32 / n),
                                ]);
                                Label(&t.total.to_string(), Fs::Sm, Weight::Semibold, None, None);
                            });
                            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                                Label(&crate::ui::project::display_name(&t.name), Fs::Md, Weight::Semibold, None, Some(1));
                                // `.botrow`: up to five team members in their state, drawn at 28 dp so the body is about the 20 px the mockup shows (the drawing has margin in its box).
                                let members = services().client.read(|s| project_members(s, &t.name, 5));
                                cu::Row().spacing(2.0).alignment(Alignment::CenterStart).content(|| {
                                    for (agent, status, title) in members.iter() {
                                        super::characters::Character(agent, false, 28.0, title, &t.name, super::characters::CharState::from_status(*status), None);
                                    }
                                });
                            });
                        });
                    },
                );
            })
        }).collect());
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn fr_l16_capture_hooks_map_env_values() {
        assert_eq!(pick_from(Some("mode")), (Some(Pick::Mode), false));
        assert_eq!(pick_from(Some("mode_armed")), (Some(Pick::Mode), true));
        assert_eq!(pick_from(Some("model")), (Some(Pick::Model), false));
        assert_eq!(pick_from(Some("zzz")), (None, false));
        assert_eq!(pick_from(None), (None, false));
        assert_eq!(mode_from(Some("bypass")), Some(PermMode::Bypass));
        assert_eq!(mode_from(Some("plan")), Some(PermMode::Plan));
        assert_eq!(mode_from(Some("nope")), None);
        assert_eq!(mode_from(None), None);
    }

    use super::*;
    use compose_rust::protocol::{decode_batch, HostEvent, Mutation, PropertyValue};
    use compose_rust::{Host, Recomposer};

    #[test]
    fn fr_l23_the_dashboard_grid_follows_the_mockup_breakpoints() {
        let w = layout(1440.0);
        assert!(w.wide && w.pair_mid);
        assert_eq!((w.kpi_cols, w.eff_cols, w.proj_cols), (4, 3, 5));
        let m = layout(1024.0);
        assert!(!m.wide && m.pair_mid);
        assert_eq!((m.kpi_cols, m.eff_cols, m.proj_cols), (4, 2, 3));
        let t = layout(768.0);
        assert!(!t.pair_mid && !t.compact);
        assert_eq!((t.kpi_cols, t.eff_cols, t.proj_cols), (2, 1, 2));
        let p = layout(390.0);
        assert!(p.compact);
        assert_eq!(p.proj_cols, 1);
    }

    #[test]
    fn fr_l11_composer_layout() {
        assert_eq!(suggestion_cols(1440.0), 3);
        assert_eq!(suggestion_cols(768.0), 2);
        assert_eq!(composer_width(1158.0), 720.0);
        assert_eq!(composer_width(340.0), 340.0);

    }

    #[test]
    fn fr_l11_long_chips_leave_the_send_button_its_width() {
        use compose_rust::schema::PropertyKind;
        let _guard = crate::services::test_guard();
        for width in [1440.0, 390.0] {
            let mut host = Host::new(move || Box::new(Recomposer::new(move || {
                let c = Composer::new();
                let mut selection = Selection::initial();
                selection.project = "아주 긴 프로젝트 이름".repeat(20);
                c.selection.set(selection);
                ComposerCard(&c, shared_ui(), composer_width(width - 50.0), layout(width));
            })));
            let bytes = host.rebuild().expect("rebuild");
            let batch = decode_batch(bytes).expect("decode");
            let w36: Vec<_> = batch.iter().filter_map(|m| match m {
                Mutation::SetModifier { node_id, modifier: compose_rust::Modifier::Width(w), .. } if *w == SEND_BUTTON_DP => Some(*node_id),
                _ => None,
            }).collect();
            let weighted = |parent| batch.iter().any(|m| match m {
                Mutation::Insert { parent_id, node_id, .. } if *parent_id == parent => batch.iter().any(|n| matches!(n,
                    Mutation::SetModifier { node_id: id, modifier: compose_rust::Modifier::Weight(w), .. }
                    if id == node_id && *w == 1.0)),
                _ => false,
            });
            // The add box and the send circle are both 36 wide beside the weighted chip column.
            let beside = w36.iter().filter(|n| batch.iter().any(|m| matches!(m,
                Mutation::Insert { parent_id, node_id, .. } if node_id == *n && weighted(*parent_id)))).count();
            assert!(beside >= 2, "chip row must take the remaining width at {width} dp");
        }
    }

    #[test]
    fn fr_l10_home_send_button_is_the_ink_paper_plane_circle() {
        assert_eq!(SEND_BUTTON_DP, 36.0);
        let production = include_str!("home.rs").split("#[cfg(test)]").next().unwrap();
        assert!(production.contains("super::session::SendCircleButton(SEND_BUTTON_DP"));
        assert!(!production.contains("IconRole::Send"), "the toolkit arrow glyph is gone");
    }

    #[test]
    fn fr_l10_home_send_button_chips_are_not_toolkit_buttons() {
        let production = include_str!("home.rs").split("#[cfg(test)]").next().unwrap();
        assert_eq!(production.matches("cu::Button(").count(), 0);
        assert!(chip_has_chevron(Pick::Project) && chip_has_chevron(Pick::Model) && chip_has_chevron(Pick::Mode));
        assert!(!chip_has_chevron(Pick::Computer));
    }

    #[test]
    fn fr_l23_the_date_line_names_the_weekday() {
        assert_eq!(date_line("2026-10-25"), "10월 25일 일요일");
        assert_eq!(date_line("1970-01-01"), "1월 1일 목요일");
        assert_eq!(date_line("nope"), "");
    }

    #[test]
    fn fr_l11_hero_bots_use_the_vendor_colours() {
        let svg = hero_bot_svg("codex", "run", 40.0, &LIGHT);
        assert!(svg.contains("#3f74d4") && svg.contains("width=\"40\""));
        assert!(hero_bot_svg("gemini", "wait", 32.0, &DARK).contains("#8fb1df"));
    }

    #[test]
    fn fr_l23_legend_rows_wrap_at_the_card_width() {
        assert_eq!(legend_rows(&[80.0, 80.0, 80.0], 390.0, 16.0), vec![vec![0, 1, 2]]);
        assert_eq!(legend_rows(&[100.0, 100.0, 100.0], 230.0, 16.0), vec![vec![0, 1], vec![2]]);
        assert_eq!(legend_rows(&[300.0, 50.0], 200.0, 16.0), vec![vec![0], vec![1]]);
        assert!(legend_rows(&[], 200.0, 16.0).is_empty());
    }

    #[test]
    fn fr_l23_the_week_has_seven_days_ending_today() {
        let _guard = crate::services::test_guard();
        let days = services().client.read(|s| weekday_usage(s, 1_761_350_400_000));
        assert_eq!(days.len(), 7);
        // 2025-10-25 is a Saturday.
        assert_eq!(days.last().map(|d| d.label), Some("토"));
        assert_eq!(days[0].label, "일");
    }

    #[test]
    fn capture_scroll_skips_whole_blocks() {
        let s = Skip(std::cell::Cell::new(600.0));
        assert!(!s.show(560.0));
        assert!(s.show(190.0));
        assert!(s.show(380.0));
    }

    #[test]
    fn fr_l23_a_ring_draws_one_arc_per_non_empty_part() {
        let svg = ring_svg(40.0, 5.0, 0x00ffffff, &[(0xff0000, 0.25), (0x00ff00, 0.0), (0x0000ff, 0.5)]);
        assert_eq!(svg.matches("<path").count(), 2);
        assert!(svg.contains("stroke=\"#ff0000\"") && svg.contains("stroke=\"#0000ff\""));
        assert!(!svg.contains("#00ff00"));
        // A part that fills the circle is a second circle, not a degenerate arc.
        let full = ring_svg(40.0, 5.0, 0, &[(0x123456, 1.0)]);
        assert_eq!(full.matches("<circle").count(), 2);
        assert_eq!(full.matches("<path").count(), 0);
    }

    #[test]
    fn fr_l23_bar_heights_are_relative_and_keep_small_values_visible() {
        assert_eq!(bar_heights(&[0, 50, 100], 100.0), vec![0.0, 50.0, 100.0]);
        assert_eq!(bar_heights(&[1, 1000], 100.0)[0], 2.0);
        assert_eq!(bar_heights(&[0, 0], 100.0), vec![0.0, 0.0]);
    }

    #[test]
    fn fr_l23_kpi_notes_turn_red_only_for_failures_and_waiting() {
        let none = StatusCounts::default();
        assert_eq!(note_tone(2, &none), PillTone::Green);
        let bad = StatusCounts { failed: 1, waiting: 2, ..Default::default() };
        assert_eq!(note_tone(0, &bad), PillTone::Green);
        assert_eq!(note_tone(2, &bad), PillTone::Red);
        assert_eq!(note_tone(3, &bad), PillTone::Red);
    }

    fn texts(batch: &[Mutation<'_>]) -> Vec<String> {
        batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetProp { value: PropertyValue::String(s), .. } => Some(s.to_string()),
                _ => None,
            })
            .collect()
    }

    /// What the tests read off the wire, kept across batches: node ids stay the same.
    #[derive(Default)]
    struct Tree {
        text: std::collections::HashMap<u32, String>,
        placeholder: std::collections::HashMap<u32, String>,
        /// (node, property) to handler id: `PropertyKind` is not `Hash`, so a list.
        handler: Vec<(u32, PropertyKind, u64)>,
        parent: std::collections::HashMap<u32, u32>,
        /// Texts in the order they were last sent.
        sent: Vec<String>,
    }

    use compose_rust::schema::PropertyKind;

    impl Tree {
        fn read(&mut self, bytes: &[u8]) {
            self.sent.clear();
            for m in decode_batch(bytes).expect("decode") {
                match m {
                    Mutation::SetProp { node_id, property: PropertyKind::Text, value: PropertyValue::String(s) } => {
                        self.text.insert(node_id, s.to_string());
                        self.sent.push(s.to_string());
                    }
                    Mutation::SetProp { node_id, property: PropertyKind::Placeholder, value: PropertyValue::String(s) } => {
                        self.placeholder.insert(node_id, s.to_string());
                    }
                    Mutation::SetProp { node_id, property, value: PropertyValue::Integer(h) } => {
                        self.handler.retain(|(n, k, _)| !(*n == node_id && *k == property));
                        self.handler.push((node_id, property, h as u64));
                    }
                    Mutation::Insert { parent_id, node_id, .. } => {
                        self.parent.insert(node_id, parent_id);
                    }
                    _ => {}
                }
            }
        }

        fn has(&self, want: &str) -> bool {
            self.text.values().any(|t| t == want)
        }

        /// The clickable container around the text `label`: its node and click handler.
        fn click_target(&self, label: &str) -> (u32, u64) {
            let mut node = *self.text.iter().find(|(_, t)| t.as_str() == label).unwrap_or_else(|| panic!("no text {label}")).0;
            loop {
                if let Some(h) = self.handler_of(node, PropertyKind::OnClick) {
                    return (node, h);
                }
                node = *self.parent.get(&node).unwrap_or_else(|| panic!("{label} has no clickable ancestor"));
            }
        }

        /// The text field with this placeholder and its handler for `property`.
        fn field(&self, placeholder: &str, property: PropertyKind) -> (u32, u64) {
            let node = *self.placeholder.iter().find(|(_, t)| t.as_str() == placeholder).expect("field").0;
            (node, self.handler_of(node, property).expect("handler"))
        }

        fn handler_of(&self, node: u32, property: PropertyKind) -> Option<u64> {
            self.handler.iter().find(|(n, k, _)| *n == node && *k == property).map(|(_, _, h)| *h)
        }
    }

    thread_local! {
        static PICKER: std::cell::RefCell<Option<Composer>> = const { std::cell::RefCell::new(None) };
    }

    /// The composer card at 1440 dp with the model picker open, and the composer it draws.
    fn model_picker_host() -> (Host, Tree, Composer) {
        let mut host = Host::new(|| {
            Box::new(Recomposer::new(|| {
                let c = remember(|| {
                    let c = Composer::new();
                    c.open.set(Some(Pick::Model));
                    c
                });
                PICKER.with(|p| *p.borrow_mut() = Some(c.clone()));
                ComposerCard(&c, shared_ui(), composer_width(1390.0), layout(1440.0));
            }))
        });
        let mut tree = Tree::default();
        tree.read(host.rebuild().expect("rebuild"));
        let c = PICKER.with(|p| p.borrow().clone()).expect("composed");
        (host, tree, c)
    }

    fn click(host: &mut Host, tree: &mut Tree, label: &str) {
        let (node_id, handler_id) = tree.click_target(label);
        let (bytes, _) = host.dispatch(HostEvent { node_id, handler_id, payload: compose_rust::EventPayload::Clicked }).expect("click");
        tree.read(bytes);
    }

    #[test]
    fn fr_l15_model_popover_opens_on_the_chosen_harness_with_a_harness_rail() {
        let _guard = crate::services::test_guard();
        let (_host, tree, c) = model_picker_host();
        assert_eq!(c.selection.get_untracked().option.harness, Harness::ClaudeCode);
        for h in Harness::ALL {
            assert!(tree.has(h.label()), "rail entry {}", h.label());
        }
        assert!(!tree.has("전체"), "the rail has no all entry (mockup v8)");
        assert!(tree.has("Opus 5.5 (높음)") && tree.has("Haiku 5.5 (낮음)"));
        assert!(!tree.has("GPT-5 (높음)"), "only the chosen harness is listed");
        assert!(tree.has("Ctrl+1"));
    }

    #[test]
    fn fr_l15_rail_tap_lists_that_harness_and_a_row_tap_chooses_and_closes() {
        let _guard = crate::services::test_guard();
        let (mut host, mut tree, c) = model_picker_host();
        click(&mut host, &mut tree, "Codex");
        assert!(tree.has("GPT-5 mini (보통)"), "codex rows after the rail tap: {:?}", tree.sent);
        click(&mut host, &mut tree, "GPT-5 mini (보통)");
        let sel = c.selection.get_untracked();
        assert_eq!((sel.option.harness, sel.option.model, sel.option.effort), (Harness::Codex, "gpt-5-mini", Some("Medium")));
        assert_eq!(c.open.get_untracked(), None, "choosing closes the picker");
        assert_eq!(c.harness_filter.get_untracked(), None, "the next opening starts on the chosen harness");
    }

    #[test]
    fn fr_l15_enter_in_the_search_chooses_the_first_match() {
        let _guard = crate::services::test_guard();
        let (mut host, mut tree, c) = model_picker_host();
        let (node_id, handler_id) = tree.field("모델 검색", PropertyKind::OnValueChange);
        let (bytes, _) = host
            .dispatch(HostEvent { node_id, handler_id, payload: compose_rust::EventPayload::TextChanged("sonnet 보통") })
            .expect("type");
        tree.read(bytes);
        let (node_id, handler_id) = tree.field("모델 검색", PropertyKind::OnSubmit);
        let (bytes, _) = host
            .dispatch(HostEvent { node_id, handler_id, payload: compose_rust::EventPayload::TextSubmitted("sonnet 보통") })
            .expect("enter");
        tree.read(bytes);
        let sel = c.selection.get_untracked();
        assert_eq!((sel.option.model, sel.option.effort), ("sonnet", Some("Medium")));
        assert_eq!(c.open.get_untracked(), None);
        assert_eq!(c.query.get_untracked(), "", "the search starts empty next time");
    }

    #[test]
    fn fr_l15_enter_on_a_blocked_model_keeps_the_picker_open() {
        let _guard = crate::services::test_guard();
        let (mut host, mut tree, c) = model_picker_host();
        let before = c.selection.get_untracked();
        let (node_id, handler_id) = tree.field("모델 검색", PropertyKind::OnValueChange);
        let (bytes, _) = host
            .dispatch(HostEvent { node_id, handler_id, payload: compose_rust::EventPayload::TextChanged("gemini flash") })
            .expect("type");
        tree.read(bytes);
        assert!(tree.has("Gemini · 준비 중입니다"), "the row says why: {:?}", tree.sent);
        let (node_id, handler_id) = tree.field("모델 검색", PropertyKind::OnSubmit);
        host.dispatch(HostEvent { node_id, handler_id, payload: compose_rust::EventPayload::TextSubmitted("gemini flash") })
            .expect("enter");
        assert_eq!(c.selection.get_untracked(), before);
        assert_eq!(c.open.get_untracked(), Some(Pick::Model));
    }

    #[test]
    fn fr_l16_mode_popover_lists_titles_and_descriptions_and_a_tap_applies() {
        let _guard = crate::services::test_guard();
        let mut host = Host::new(|| {
            Box::new(Recomposer::new(|| {
                let c = remember(|| {
                    let c = Composer::new();
                    c.open.set(Some(Pick::Mode));
                    c
                });
                PICKER.with(|p| *p.borrow_mut() = Some(c.clone()));
                ComposerCard(&c, shared_ui(), composer_width(1390.0), layout(1440.0));
            }))
        });
        let mut tree = Tree::default();
        tree.read(host.rebuild().expect("rebuild"));
        let c = PICKER.with(|p| p.borrow().clone()).expect("composed");
        assert!(tree.has("권한 모드 \u{b7} Claude Code"));
        for m in modes_for(Harness::ClaudeCode) {
            assert!(tree.has(m.label()) && tree.has(m.description()), "{m:?}");
        }
        // Edits applies at once and closes the picker.
        click(&mut host, &mut tree, PermMode::Edits.label());
        assert_eq!(c.selection.get_untracked().mode, PermMode::Edits);
        assert_eq!(c.open.get_untracked(), None);
    }

    #[test]
    fn fr_l11_home_shows_the_greeting_the_composer_and_the_dashboard() {
        use compose_rust::{EventPayload, WindowHeightClass, WindowSizeClass};
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| Box::new(Recomposer::new(HomeScreen)));
        let first = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let mut all = texts(&first);
        let event = HostEvent {
            node_id: 0,
            handler_id: 0,
            payload: EventPayload::WindowSizeChanged {
                width_dp: 1440.0,
                height_dp: 900.0,
                class: WindowSizeClass::Expanded,
                height_class: WindowHeightClass::Medium,
                safe_area_top: 0.0,
                safe_area_leading: 0.0,
                safe_area_trailing: 0.0,
            },
        };
        let (bytes, _) = host.dispatch(event).expect("dispatch");
        let batch = decode_batch(bytes).expect("decode");
        // The resize only re-emits what changed, so the greeting is in the first batch.
        all.extend(texts(&batch));
        let has = |want: &str| all.iter().any(|t| t.contains(want));
        for want in ["무엇을 도와줄까요?", "기본 프로젝트", "오늘의 팀", "실행 중 팀원", "팀원 상태", "프로젝트", "모두 보기"] {
            assert!(has(want), "missing {want}: {all:?}");
        }
        for s in SUGGESTIONS {
            assert!(has(s), "missing suggestion {s}");
        }
        compose_rust::window::reset_window_size();
    }

    #[test]
    fn fr_l35_home_requests_github_summary_on_mount_and_on_returning_to_home() {
        let _guard = crate::services::test_guard();
        let ui = shared_ui();
        ui.set_menu(crate::ui::shell::NavMenu::Home);
        let before = crate::bridge::github_request_count();
        let mut host = Host::new(|| Box::new(Recomposer::new(HomeScreen)));
        host.rebuild().expect("first home mount");
        let after_mount = crate::bridge::github_request_count();
        assert_eq!(after_mount, before + 1);

        // Staying on Home and rebuilding does not re-request.
        host.rebuild().expect("still on home");
        assert_eq!(crate::bridge::github_request_count(), after_mount);

        // Switching to another menu does not request; returning to Home requests immediately.
        ui.set_menu(crate::ui::shell::NavMenu::Projects);
        assert_eq!(crate::bridge::github_request_count(), after_mount);
        ui.set_menu(crate::ui::shell::NavMenu::Home);
        assert_eq!(crate::bridge::github_request_count(), after_mount + 1);
    }
}
