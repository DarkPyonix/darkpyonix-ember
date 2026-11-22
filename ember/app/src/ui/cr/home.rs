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
use compose_rust::ui::{Modifier, Role};
use compose_rust::{Alignment, Color, DrawList, Paint, IconRole, ShapeRole};

pub use crate::ui::home::{
    apply_suggestion, can_send, computers_line, default_mode, effective_message, filter_models,
    harness_block, kpis, mode_after_harness_change, composer_chip_rows, composer_chip_width, composer_chip_line, ChipLine, mode_header, mode_picker_rows_pending, mode_tap, popover_width, model_catalog, modes_for, needs_approve_all,
    project_options, project_tile_line, resolve_cwd, shortcut_label, split_chip_rows,
    success_percent, title_from_message, CrewMember, Dash, Harness, HarnessBar, Kpi, ModelOption,
    ModeRow, PermMode, ProjectTile, Selection, StatusCounts, ComposerAttachment, ATTACH_HINT, COMPOSER_PLACEHOLDER, DEFAULT_PROJECT,
    EMPTY_CREW, EMPTY_HARNESS_USAGE, CiResult, GithubCard, GithubPill, GithubSampleKind, INTEGRATIONS_TITLE, LATER_INTEGRATIONS, integrations_layout, CI_RULE_WHEN, CI_RULE_THEN, CI_RULE_NOTE, CI_RULE_LINK, LATER_LINE, LATER_PILL, GITHUB_NAME, EMPTY_PROJECTS, GREETING, SUGGESTIONS, UNAVAILABLE,
};
use crate::ui::home::{
    composer_pop_offset, mode_hover, model_active_row, model_key, model_pick, model_popover_height, model_row_detail,
    model_sheet_height, model_view, ModelEnter, ModelKey,
};
use super::components::{
    pop_anchor_weight, pop_chosen_fill, pop_metrics, AppButton, ButtonKind, Clickable, EmberField, EmberSheet,
    FieldOptions, Label, NamedClickable, PickChip, Pill, PillTone, PopExtra, PopKind, PopMenu, PopRow, PopoverMenuWith,
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

/// The model popover (mockup `.mp-pop{width:400px}`, review K4): the harness rail beside the
/// search and the rows.
const MODEL_POPOVER_WIDTH: f32 = 400.0;
/// `.mp-rail{width:52px}`, its 1 dp right line included.
const MODEL_RAIL_WIDTH: f32 = 52.0;
/// `.mp-rail-b{width:36px;height:36px}`.
const MODEL_RAIL_BUTTON: f32 = 36.0;
/// `.mp-k`: a 12/18 label with 2 above and below.
const MODEL_KEY_CAP_H: f32 = 22.0;
/// `.mp-k{min-width:52px}`.
const MODEL_KEY_CAP_W: f32 = 52.0;
/// `.mp-search{height:40px}`, its 1 dp blue underline included.
const MODEL_SEARCH_H: f32 = 40.0;

/// The composer card's padding above 560 (`.bigcomposer`), which also places its pickers.
const COMPOSER_PAD: f32 = SP[3];

/// The permission picker's rows as the shared option menu draws them (FR-L16, mockup
/// `.md-row`, review r8): a shield (the warning triangle on the bypass row), the title at
/// 14/600 over the one-line description, the current row filled with a check, the bypass row
/// red. The row is a menu item that says whether it is the current one
/// (`components::PopRowView`).
fn mode_pop_rows(rows: &[ModeRow]) -> Vec<PopRow> {
    rows.iter()
        .map(|r| PopRow {
            icon: Some(if r.danger { ICON_WARN } else { ICON_SHIELD }),
            title: r.title.to_string(),
            description: Some(r.description.to_string()),
            chosen: r.chosen,
            danger: r.danger,
            ..Default::default()
        })
        .collect()
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

/// `.hm-go`'s `I("arrow")`: the up-right arrow of the mockup (review 2026-11-14 H6, #849).
const ICON_ARROW: &str = line_icon!("m4.5 19.5 15-15m0 0H8.25m11.25 0v11.25");
/// `I("team")`, the group of three on the mockup's 팀원 상태 head (Heroicons user-group).
const ICON_TEAM: &str = line_icon!("M18 18.72a9.094 9.094 0 0 0 3.741-.479 3 3 0 0 0-4.682-2.72m.94 3.198.001.031c0 .225-.012.447-.037.666A11.944 11.944 0 0 1 12 21c-2.17 0-4.207-.576-5.963-1.584A6.062 6.062 0 0 1 6 18.719m12 0a5.971 5.971 0 0 0-.941-3.197m0 0A5.995 5.995 0 0 0 12 12.75a5.995 5.995 0 0 0-5.058 2.772m0 0a3 3 0 0 0-4.681 2.72 8.986 8.986 0 0 0 3.74.477m.94-3.197a5.971 5.971 0 0 0-.94 3.197M15 6.75a3 3 0 1 1-6 0 3 3 0 0 1 6 0Zm6 3a2.25 2.25 0 1 1-4.5 0 2.25 2.25 0 0 1 4.5 0Zm-13.5 0a2.25 2.25 0 1 1-4.5 0 2.25 2.25 0 0 1 4.5 0Z");
const ICON_SHIELD: &str = include_str!("../../../assets/nav/shield.svg");
const ICON_FOLDER: &str = include_str!("../../../assets/nav/folder.svg");
const ICON_PC: &str = include_str!("../../../assets/nav/pc.svg");
// TODO(item 6): the model chip's harness glyph; the terminal stands in.
const ICON_TERMINAL: &str = include_str!("../../../assets/nav/terminal.svg");
const ICON_GIT: &str = include_str!("../../../assets/nav/git.svg");
const ICON_CHART_NAV: &str = include_str!("../../../assets/nav/chart.svg");
/// Heroicons 2.2.0 `sparkles` (MIT), the mockup's `spark` (`icons.js`).
const ICON_SPARK: &str = line_icon!("M9.813 15.904 9 18.75l-.813-2.846a4.5 4.5 0 0 0-3.09-3.09L2.25 12l2.846-.813a4.5 4.5 0 0 0 3.09-3.09L9 5.25l.813 2.846a4.5 4.5 0 0 0 3.09 3.09L15.75 12l-2.846.813a4.5 4.5 0 0 0-3.09 3.09ZM18.259 8.715 18 9.75l-.259-1.035a3.375 3.375 0 0 0-2.455-2.456L14.25 6l1.036-.259a3.375 3.375 0 0 0 2.455-2.456L18 2.25l.259 1.035a3.375 3.375 0 0 0 2.456 2.456L21.75 6l-1.035.259a3.375 3.375 0 0 0-2.456 2.456ZM16.894 20.567 16.5 21.75l-.394-1.183a2.25 2.25 0 0 0-1.423-1.423L13.5 18.75l1.183-.394a2.25 2.25 0 0 0 1.423-1.423l.394-1.183.394 1.183a2.25 2.25 0 0 0 1.423 1.423l1.183.394-1.183.394a2.25 2.25 0 0 0-1.423 1.423Z");
pub(crate) const ICON_WARN: &str = line_icon!("M12 9.5v4M12 16.75h.01M10.3 3.9L2.4 17.6a2 2 0 001.7 3h15.8a2 2 0 001.7-3L13.7 3.9a2 2 0 00-3.4 0z");
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
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100%\" height=\"100%\" viewBox=\"0 0 {size} {size}\"><circle cx=\"{c}\" cy=\"{c}\" r=\"{r}\" fill=\"none\" stroke=\"{}\" stroke-width=\"{stroke}\"/>",
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
    { use ::compose_rust::ui::Image; Image!(compose_rust::asset(compose_rust::AssetKind::Svg, shell::leak_doc(doc)), modifier = Modifier::new().size(size, size)) };
}

/// `n` equal columns of `items`, `gap` apart; the last line is padded with empty columns.
pub fn Cols(n: usize, gap: f32, items: Vec<Part<'_>>) {
    let n = n.max(1);
    { use ::compose_rust::ui::Column; Column!(spacing = gap, modifier = Modifier::new().fill_max_width(), {
        let mut it = items.into_iter().peekable();
        while it.peek().is_some() {
            let row: Vec<Part> = it.by_ref().take(n).collect();
            let pad = n - row.len();
            { use ::compose_rust::ui::Row; Row!(spacing = gap, modifier = Modifier::new().fill_max_width(), {
                for part in row {
                    { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().weight(1.0), content = part) };
                }
                for _ in 0..pad {
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                }
            }) };
        }
    }) };
}

/// Columns of unequal weights (the 12-column grid), or stacked when `stacked`.
fn Weighted(stacked: bool, gap: f32, parts: Vec<(f32, Part<'_>)>) {
    if stacked {
        { use ::compose_rust::ui::Column; Column!(spacing = gap, modifier = Modifier::new().fill_max_width(), {
            for (_, part) in parts {
                { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), content = part) };
            }
        }) };
    } else {
        { use ::compose_rust::ui::Row; Row!(spacing = gap, modifier = Modifier::new().fill_max_width(), {
            for (w, part) in parts {
                { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().weight(w), content = part) };
            }
        }) };
    }
}

/// A dashboard tile: a card with its content in a column.
pub fn Tile(content: impl FnOnce()) {
    TileH(None, content);
}

/// The inner height of the success tile (donut 132 + head 30 + gap 8 + the card's 16 px padding
/// each side is outside it): tiles beside it take the same, since the toolkit rows do not
/// stretch their children. v0.0.7: #389 fixed (`Row::equal_height` with `fill_max_height`
/// children, as `EmberCardSized` uses); workaround kept until checked on a display (the tiles
/// here are not `EmberCardSized` yet). This number is fragile when the success tile's content changes.
const ROW_TILE_H: f32 = 171.0;

fn TileH(height: Option<f32>, content: impl FnOnce()) {
    super::components::EmberCard(false, true, None, None, None, || {
        let m = Modifier::new().fill_max_width();
        { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = match height { Some(h) => m.height(h), None => m }, content = content) };
    });
}

/// `.hm-ch`: the 30 px outlined icon, the title, and what goes at the right.
pub fn CardHead(icon: &'static str, title: &str, trailing: impl FnOnce()) {
    let p = ember_theme().palette;
    { use ::compose_rust::ui::Row; Row!(spacing = 10.0, alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width().height(30.0), {
        { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().size(30.0, 30.0).border(1.0, paint(p.line)).corner_radius(CORNER_SM), content = || NavIcon(icon, p.text_2.to_argb(), 16.0)) };
        Label(title, Fs::Lg, Weight::Semibold, None, Some(1));
        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
        trailing();
    }) };
}

/// `.hm-go`: the round 30 px arrow that opens the tile's screen, named "화면 열기" for
/// assistive technology (compose-rust v0.0.7, #372, #460).
fn GoButton(on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    super::components::NamedClickable(
        super::a11y::OPEN_SCREEN,
        Modifier::new().size(30.0, 30.0).border(1.0, paint(p.line)).shape_role(ShapeRole::Full),
        Alignment::Center,
        on_click,
        || NavIcon(ICON_ARROW, p.muted.to_argb(), 16.0),
    );
}

pub fn Dot(color: compose_rust::Color) {
    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().size(8.0, 8.0).background(paint(color)).shape_role(ShapeRole::Full)) };
}

/// A legend dot: solid in `solid`, or an 8 px hatched square in `hatch` (the bars' hatch, FR-L23).
fn DotOrHatch(hatched: bool, hatch: compose_rust::Color, solid: compose_rust::Color) {
    if !hatched {
        Dot(solid);
        return;
    }
    // The bars' pattern: same colour for fill and lines, same step (`legend_hatch_style`).
    // The square is decoration: the Label beside it is the text, so it is hidden from
    // assistive technology (compose-rust v0.0.7, #429).
    let (step, _alpha) = usage::legend_hatch_style();
    let mut list = DrawList::builder().round_rect(paint(hatch), 0.0, 0.0, 8.0, 8.0, 2.0, 1.0);
    for (x1, y1, x2, y2) in crate::ui::home::hatch_lines(0.0, 0.0, 8.0, 8.0, step) {
        list = list.line(paint(hatch), x1, y1, x2, y2, 1.0);
    }
    { use ::compose_rust::ui::Canvas; Canvas!(list.build(), modifier = Modifier::new().size(8.0, 8.0).hidden(true)) };
}

// ---------------------------------------------------------------------------------------------
// The screen
// ---------------------------------------------------------------------------------------------

/// Which of the composer's lists is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Pick {
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
/// code before v0.0.5 (v0.0.7: #385 fixed with `ScrollState::scroll_to`; workaround kept until
/// checked on a display, because the capture script relies on it), so the blocks that would have scrolled out (by their estimated heights) are left out.
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
    let window = crate::ui::window_size::current_window_size();
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
    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().fill_max_height(), {
        { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().fill_max_width().fill_max_height(), {
            { use ::compose_rust::ui::Column; Column!(alignment = Alignment::Center, modifier = Modifier::new().fill_max_width().padding(pad), {
                if skip.show(EST_HERO) {
                    Hero(&composer, &ui, window.width_dp, window.height_dp, content_w, l);
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(SP[5])) };
                }
                Dashboard(&dash, &ui, l, content_w, &skip);
            }) };
        }) };
        composer.lists(&ui);
    }) };
}

/// The composer's state.
#[derive(Clone)]
pub(super) struct Composer {
    pub(super) selection: MutableState<Selection>,
    typed: MutableState<String>,
    attachments: MutableState<Vec<ComposerAttachment>>,
    attachment_open: MutableState<bool>,
    attachment_path: MutableState<String>,
    attachment_error: MutableState<Option<String>>,
    suggestion: MutableState<Option<String>>,
    busy: MutableState<bool>,
    pub(super) open: MutableState<Option<Pick>>,
    /// The mode that waits for its confirming tap (bypass), if any.
    mode_pending: MutableState<crate::ui::home::ModePending>,
    harness_filter: MutableState<Option<Harness>>,
    query: MutableState<String>,
    /// The row the arrow keys moved to in the model picker (`st.act`); `None` is the default
    /// row ([`model_active_row`]).
    model_act: MutableState<Option<usize>>,
    /// The search field keeps what was typed on its own (it is uncontrolled), so a rail tap
    /// that clears the query gives the field a new identity, which starts empty.
    model_field_gen: MutableState<u32>,
}

impl Composer {
    pub(super) fn new() -> Self {
        let base_sel = crate::services::try_services()
            .map(|s| Selection::initial_with_defaults(&s.ui.live.agent_defaults.get_untracked()))
            .unwrap_or_else(Selection::initial);
        Composer {
            selection: mutable_state_of(match mode_from(std::env::var("EMBER_CR_HOME_MODE").ok().as_deref()) {
                Some(m) => base_sel.with_mode(m),
                None => base_sel,
            }),
            typed: mutable_state_of(String::new()),
            attachments: mutable_state_of(Vec::new()),
            attachment_open: mutable_state_of(false),
            attachment_path: mutable_state_of(String::new()),
            attachment_error: mutable_state_of(None),
            suggestion: mutable_state_of(initial_suggestion()),
            busy: mutable_state_of(false),
            open: mutable_state_of(initial_pick()),
            mode_pending: mutable_state_of(
                pick_from(std::env::var("EMBER_CR_HOME_PICK").ok().as_deref()).1.then(|| (PermMode::Bypass, now_ms())),
            ),
            harness_filter: mutable_state_of(None),
            query: mutable_state_of(String::new()),
            model_act: mutable_state_of(None),
            model_field_gen: mutable_state_of(0),
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

    fn add_attachment(&self) {
        let mut items = self.attachments.get_untracked();
        match crate::ui::home::stage_attachment(&mut items, &self.attachment_path.get_untracked()) {
            Ok(()) => {
                self.attachments.set(items);
                self.attachment_error.set(None);
                self.attachment_open.set(false);
            }
            Err(e) => self.attachment_error.set(Some(e.to_string())),
        }
    }

    fn send(&self, ui: &SharedUi) {
        let message = effective_message(&self.typed.get_untracked(), self.suggestion.get_untracked().as_deref());
        let staged = self.attachments.get_untracked();
        let message = crate::ui::home::attachment_message(&message, &staged);
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
        let (ui, busy, attachments) = (ui.clone(), self.busy.clone(), self.attachments.clone());
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
                        let mut remaining = attachments.get_untracked();
                        remaining.retain(|item| !staged.contains(item));
                        attachments.set(remaining);
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
    pub(super) fn lists(&self, ui: &SharedUi) {
        let sel = self.selection.get();
        let open = self.open.get();
        let close = {
            let o = self.open.clone();
            move || o.set(None)
        };
        // Wider than 560 the project, model, mode and computer lists are popovers under the
        // composer, drawn in the composer card (`project_menu`, `model_popover`, `mode_menu`,
        // `computer_menu`); at 560 and below they are bottom sheets, drawn here so a sheet does
        // not wrap the chip that takes the rest of the row.
        let compact = layout(crate::ui::window_size::current_window_size().width_dp).compact;
        if compact {
            self.project_menu(ui, None, || {});
            self.computer_menu(ui, None, || {});
            self.mode_menu(&sel, None, || {});
        }
        self.model_sheet(ui, &sel, compact && open == Some(Pick::Model), close);
    }

    /// Closing a chip's list: only the open one, so a late dismiss does not close another.
    fn close_pick(&self, pick: Pick) -> impl FnMut() + Clone + 'static {
        let o = self.open.clone();
        move || {
            if o.get_untracked() == Some(pick) {
                o.set(None);
            }
        }
    }

    /// What the composer's pickers ask of the shared menu: the box 8 below the composer card
    /// (review r8) and the weight the chip takes when the chips share their line (#573).
    fn pop_extra(share: Option<f32>) -> PopExtra {
        PopExtra { offset: Some(composer_pop_offset(COMPOSER_PAD)), anchor_weight: share, ..Default::default() }
    }

    /// The project chip's list (FR-L10.4, #888, review K10): a popover under the composer, or a
    /// bottom sheet at 560 and below; one row per project with the folder glyph and a check on
    /// the chosen one.
    fn project_menu(&self, ui: &SharedUi, share: Option<f32>, anchor: impl FnOnce()) {
        let (projects, _) = self.live(ui);
        let sel = self.selection.get();
        let options = project_options(&projects);
        let rows: Vec<PopRow> = options
            .iter()
            .map(|p| PopRow { icon: Some(ICON_FOLDER), title: p.clone(), chosen: *p == sel.project, ..Default::default() })
            .collect();
        let (selection, list) = (self.selection.clone(), options.clone());
        PopoverMenuWith(
            self.open.get() == Some(Pick::Project),
            PopMenu { name: super::a11y::PROJECT_PICKER, kind: PopKind::Mode, head: "", sub: None, rows: &rows, align: compose_rust::PopoverAlign::Start },
            Self::pop_extra(share),
            self.close_pick(Pick::Project),
            move |i| {
                if let Some(p) = list.get(i) {
                    let mut s = selection.get_untracked();
                    s.project = p.clone();
                    selection.set(s);
                }
            },
            anchor,
            None,
        );
    }

    /// The computer chip's list, as [`Composer::project_menu`]: this server's own computer first,
    /// then the registered ones.
    fn computer_menu(&self, ui: &SharedUi, share: Option<f32>, anchor: impl FnOnce()) {
        let (_, computers) = self.live(ui);
        let sel = self.selection.get();
        let mut labels = vec![self.local_name(ui)];
        labels.extend(computers.iter().map(|(_, n)| n.clone()));
        let at = sel.computer.as_ref().and_then(|id| computers.iter().position(|(c, _)| c == id)).map(|i| i + 1).unwrap_or(0);
        let rows: Vec<PopRow> = labels
            .iter()
            .enumerate()
            .map(|(i, n)| PopRow { icon: Some(ICON_PC), title: n.clone(), chosen: i == at, ..Default::default() })
            .collect();
        let (selection, list) = (self.selection.clone(), computers.clone());
        PopoverMenuWith(
            self.open.get() == Some(Pick::Computer),
            PopMenu { name: super::a11y::COMPUTER_PICKER, kind: PopKind::Mode, head: "", sub: None, rows: &rows, align: compose_rust::PopoverAlign::Start },
            Self::pop_extra(share),
            self.close_pick(Pick::Computer),
            move |i| {
                let mut s = selection.get_untracked();
                s.computer = if i == 0 { None } else { list.get(i - 1).map(|(id, _)| id.clone()) };
                selection.set(s);
            },
            anchor,
            None,
        );
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

    /// The pointer came over row `i` of the permission picker: pointing at another row withdraws
    /// a waiting bypass confirmation (FR-L16, `mode_hover`, #574).
    fn hover_mode(&self, i: usize) {
        let Some(m) = modes_for(self.selection.get_untracked().option.harness).get(i).copied() else { return };
        let now = self.mode_pending.get_untracked();
        let next = mode_hover(now, m);
        if next != now {
            self.mode_pending.set(next);
        }
    }

    /// The permission picker (FR-L16, review r8): the shared option menu of the session's
    /// permission chip (`PopoverMenu`, `PopKind::Mode`, mockup `.mp-pop.mp-mode`), so the two
    /// read as one family: 340 wide, the 12/600 head "권한 모드 · harness", 56 dp rows with
    /// 14/600 titles, the current row on `sel` with a check, and no focus ring on the current
    /// row when it opens. It opens 8 below the composer card. A row tap does not close the menu
    /// by itself (`keep_open`): [`Composer::tap_mode`] closes it once a mode is applied, and
    /// the first tap on 바이패스 only arms that row. At 560 and below it is a bottom sheet with
    /// the same head and rows and no title bar (`plain_sheet`, review K8).
    /// The arrow keys move between the rows and the current row has focus when the popover
    /// opens, the toolkit's menu keyboard ([`PopoverMenuWith`], compose-rust#810).
    fn mode_menu(&self, sel: &Selection, share: Option<f32>, anchor: impl FnOnce()) {
        let rows = mode_pop_rows(&mode_picker_rows_pending(sel.option.harness, sel.mode, self.mode_pending.get().map(|(m, _)| m)));
        let header = mode_header(sel.option.harness);
        let (me, hovered) = (self.clone(), self.clone());
        PopoverMenuWith(
            self.open.get() == Some(Pick::Mode),
            PopMenu { name: super::a11y::MODE_PICKER, kind: PopKind::Mode, head: &header, sub: None, rows: &rows, align: compose_rust::PopoverAlign::Start },
            PopExtra {
                keep_open: true,
                plain_sheet: true,
                on_hover: Some(std::rc::Rc::new(move |i: usize| hovered.hover_mode(i))),
                ..Self::pop_extra(share)
            },
            self.dismiss_mode(),
            move |i| me.tap_mode(i),
            anchor,
            None,
        );
    }

    /// Closing the model picker: the next opening starts on the chosen harness with an empty
    /// search and the default active row, as the mockup's `open()` does (the search field is
    /// new each time it opens).
    fn close_model(&self) -> impl FnMut() + Clone + 'static {
        let (o, f, q, a) = (self.open.clone(), self.harness_filter.clone(), self.query.clone(), self.model_act.clone());
        move || {
            f.set(None);
            q.set(String::new());
            a.set(None);
            if o.get_untracked() == Some(Pick::Model) {
                o.set(None);
            }
        }
    }

    /// A tap on a rail entry: that harness's models, with an empty search and the default
    /// active row (mockup `data-mpv`: `st.view=...; st.q=""; st.act=-1`).
    fn rail_tap(&self, h: Harness) {
        if !self.query.get_untracked().is_empty() {
            self.query.set(String::new());
            self.model_field_gen.set(self.model_field_gen.get_untracked().wrapping_add(1));
        }
        self.model_act.set(None);
        self.harness_filter.set(Some(h));
    }

    /// The model picker at 560 and below (review K7, mockup `.mp-pop` under `@media
    /// (max-width:560px)`): the same rail, search and rows as a bottom sheet with no title bar,
    /// four fifths of the window high at most, and no key caps.
    pub(super) fn model_sheet(&self, ui: &SharedUi, sel: &Selection, open: bool, close: impl FnMut() + Clone + 'static) {
        let mut reset = self.close_model();
        let mut close = close;
        let dismiss = move || {
            reset();
            close();
        };
        let height = model_sheet_height(crate::ui::window_size::current_window_size().height_dp);
        EmberSheet("", open, dismiss.clone(), None, || {
            { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width().height(height).label(super::a11y::MODEL_PICKER), content = || self.model_body(ui, sel, true, open, dismiss)) };
        });
    }

    /// The model picker as a popover (FR-L15, mockup v8 `.mp-pop`, review r8): the toolkit
    /// `Popover` with the box of the shared option menu (`surface`, a 1 dp `line`, radius 14,
    /// `shadow-lg`, the toolkit's own elevation off), 400 wide, 8 below the composer card and
    /// lined up with the chip's start. Inside: the 52 dp harness rail, then the search and the
    /// rows ([`Composer::model_body`]). Escape and a press outside call `on_dismiss`.
    /// It is a searchable list, not a menu ([leader, 2026-11-21], #888): no node in it declares
    /// a menu or a menu item, so the toolkit's menu keyboard (compose-rust#810) does not take
    /// it over, and the search keeps the keys.
    /// Not built: the rail's favourites group and the favourite stars (no place stores them),
    /// and profile names (FR-L17 profiles are not built).
    pub(super) fn model_popover(&self, ui: &SharedUi, sel: &Selection, share: Option<f32>, anchor: impl FnOnce(), close: impl FnMut() + Clone + 'static) {
        let open = self.open.get() == Some(Pick::Model);
        let mut reset = self.close_model();
        let mut close = close;
        let dismiss = move || {
            reset();
            close();
        };
        let theme = ember_theme();
        let p = theme.palette;
        let window = crate::ui::window_size::current_window_size();
        { use ::compose_rust::ui::Popover; Popover!(open, dismiss.clone(), placement = compose_rust::PopoverPlacement::Bottom, align = compose_rust::PopoverAlign::Start, offset = composer_pop_offset(COMPOSER_PAD), modifier = pop_anchor_weight(
                share,
                // A fixed height, not `equal_height` (#849, toolkit bug compose-rust#766): the
                // toolkit sizes an `equal_height` row by its children's intrinsic heights and
                // counts a child holding a scrolling list as 0, so the row took the rail's
                // height and the list showed one row. `elevation(0)` as in `PopoverMenu`: the
                // box casts its own shadow.
                with_shadows(Modifier::new(), theme.shadow_lg())
                    .elevation(0.0)
                    .width(popover_width(MODEL_POPOVER_WIDTH, window.width_dp))
                    .height(model_popover_height(window.height_dp))
                    .background(paint(p.surface))
                    .border(1.0, paint(p.line))
                    .corner_radius(CORNER_LG)
                    .label(super::a11y::MODEL_PICKER),
            ), anchor = Some(anchor), {
                { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width().fill_max_height(), content = || self.model_body(ui, sel, false, open, dismiss)) };
            }) };
    }

    /// The picker's contents, the children of a row with a fixed height: the harness rail
    /// (`.mp-rail`), its 1 dp line, then the search over the rows (`.mp-main`). It opens on the
    /// chosen model's harness and has no "all" entry (mockup v8). `phone` is the bottom sheet,
    /// which hides the key caps (`.mp-k{display:none}` at 560 and below).
    ///
    /// Keyboard, in the search field: Enter takes the active row, the arrows move it, Home and
    /// End jump to the ends while nothing is typed and `Ctrl+1` to `Ctrl+9` choose a row
    /// (`model_key`, #574). The field consumes the keys it acts on. The search takes the focus
    /// when the picker opens, as the mockup's `open()` focuses `[data-mpq]` ([leader,
    /// 2026-11-21], #888). The rows are a list named 모델, each a list item that says it is
    /// selected when it is the active row Ember draws, the honest stand-in for the mockup's
    /// listbox, options and `aria-activedescendant`, which the toolkit cannot express (no
    /// combobox, listbox or option role and no relation from the field to a row). Not built:
    /// scrolling the active row into view (a `ScrollColumn` cannot be scrolled to a child from
    /// the Host).
    fn model_body(&self, ui: &SharedUi, sel: &Selection, phone: bool, shown: bool, close: impl FnMut() + Clone + 'static) {
        let p = ember_theme().palette;
        // The search takes the focus when the picker opens. The request runs as an effect,
        // after the batch that shows the field; one made earlier is dropped (compose-rust#410).
        // Only the body on screen asks (`shown`: the popover's wider than 560, the sheet's at
        // 560 and below), so a closed sheet's field never takes the focus from the popover's.
        let search_focus = cu::use_focus_handle();
        {
            let (focus, is_open) = (search_focus.clone(), shown);
            compose_rust::runtime::launched_effect(is_open, move || async move {
                if is_open {
                    focus.request();
                }
            });
        }
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
        // `st.act`: where the arrows left it, else the chosen row (the first match once
        // something is typed).
        let active = self.model_act.get().filter(|i| *i < rows.len()).or_else(|| model_active_row(&rows, sel.option, &query));
        let blocks: Vec<Option<&'static str>> = rows.iter().map(|o| harness_block(o.harness, &agents, &limited)).collect();
        // Choosing row `i` by a tap, Enter or `Ctrl+number`: a blocked row says why and the
        // picker stays open.
        let pick = {
            let (rows, agents, limited) = (rows.clone(), agents.clone(), limited.clone());
            let (selection, done) = (self.selection.clone(), close.clone());
            move |i: usize| match model_pick(&rows, i, |h| harness_block(h, &agents, &limited)) {
                ModelEnter::Choose(o) => {
                    selection.set(selection.get_untracked().with_option(o));
                    let mut done = done.clone();
                    done();
                }
                ModelEnter::Blocked(reason) => notice::post(reason),
                ModelEnter::Nothing => {}
            }
        };

        // `.mp-rail`: `surface-2`, 8 above the first entry, 4 between entries, one icon-only
        // entry per harness named for assistive technology (review K3).
        { use ::compose_rust::ui::Column; Column!(spacing = SP[0], modifier = Modifier::new()
                    .width(MODEL_RAIL_WIDTH - 1.0)
                    .fill_max_height()
                    .background(paint(p.surface_2))
                    // The box's own corners, inside its 1 dp line; the sheet's top is `r-xl`.
                    .corner_radii(if phone { [CORNER_XL, 0.0, 0.0, 0.0] } else { [CORNER_LG - 1.0, 0.0, 0.0, CORNER_LG - 1.0] }), {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(SP[0])) };
                for h in Harness::ALL {
                    let me = self.clone();
                    RailEntry(h, view == h && query.trim().is_empty(), move || me.rail_tap(h));
                }
            }) };
        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(1.0).fill_max_height().background(paint(p.line))) };
        // `.mp-main`. Zero wide and weighted: it holds a scrolling list.
        { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().width(0.0).weight(1.0).fill_max_height(), {
            // `.mp-top{padding:8px 12px}` around `.mp-search`: 40 high, a magnifier, the field
            // at 14 and a 1 dp blue underline (review K4).
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(SP[1])) };
            { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width(), {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
                { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().width(0.0).weight(1.0), {
                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width().height(MODEL_SEARCH_H - 1.0), {
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[0])) };
                            NavIcon(super::shell::ICON_SEARCH_SVG, p.muted.to_argb(), 18.0);
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[1])) };
                            let (q, act) = (self.query.clone(), self.model_act.clone());
                            let enter = {
                                let pick = pick.clone();
                                move |_: String| {
                                    if let Some(i) = active {
                                        pick(i);
                                    }
                                }
                            };
                            let keys = {
                                let (pick, act) = (pick.clone(), self.model_act.clone());
                                let (n, query_empty) = (rows.len(), query.trim().is_empty());
                                move |k: compose_rust::KeyEvent| {
                                    let name = match k.key() {
                                        compose_rust::Key::ArrowDown => "ArrowDown",
                                        compose_rust::Key::ArrowUp => "ArrowUp",
                                        compose_rust::Key::Home => "Home",
                                        compose_rust::Key::End => "End",
                                        _ => k.text(),
                                    };
                                    match model_key(name, k.code(), k.ctrl_key(), n, active, query_empty) {
                                        ModelKey::Move(i) => {
                                            k.consume();
                                            act.set(Some(i));
                                        }
                                        ModelKey::Pick(i) => {
                                            k.consume();
                                            pick(i);
                                        }
                                        ModelKey::Nothing => {}
                                    }
                                }
                            };
                            compose_rust::runtime::key(self.model_field_gen.get(), || {
                                EmberField(FieldOptions {
                                    bare: true,
                                    height: Some(MODEL_SEARCH_H - 1.0),
                                    weight: Some(1.0),
                                    placeholder: "모델 검색".to_string(),
                                    text: Some(query.clone()),
                                    // Typing starts again from the first match (`st.act=0`).
                                    on_value_change: Some(Box::new(move |v| {
                                        act.set(None);
                                        q.set(v);
                                    })),
                                    on_submit: Some(Box::new(enter)),
                                    on_key_down: Some(Box::new(keys)),
                                    focus: Some(search_focus.clone()),
                                    ..Default::default()
                                })
                            });
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[0])) };
                        }) };
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().fill_max_width().height(1.0).background(paint(p.blue.strong))) };
                }) };
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
            }) };
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(SP[0])) };
            // `.mp-body{flex:1 1 auto;overflow:auto;padding:4px 8px 8px}`: the list takes what
            // the fixed height leaves under the search.
            { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().fill_max_width().weight(1.0), {
                { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width().padding(SP[1]).role(Role::List).label(super::a11y::MODEL_LIST), {
                    if rows.is_empty() {
                        // `.mp-empty`.
                        { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().fill_max_width().padding(SP[4]), {
                            Label("일치하는 모델이 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
                        }) };
                    }
                    for (i, o) in rows.iter().copied().enumerate() {
                        let block = blocks[i];
                        let pick = pick.clone();
                        ModelRow(&o, block, o == sel.option, active == Some(i), if phone { None } else { shortcut_label(i) }, move || pick(i));
                    }
                }) };
            }) };
        }) };
    }
}

/// The fill of a model row: `sel` on the chosen one (`.mp-row.on`), `hover` on the one the keys
/// are on (`.mp-row.act`), none otherwise. The chosen fill is the shared option menu's.
fn model_row_fill(chosen: bool, active: bool, p: &Palette) -> Option<Color> {
    if chosen {
        Some(pop_chosen_fill(PopKind::Mode, p))
    } else if active {
        Some(p.hover)
    } else {
        None
    }
}

/// One row of the model picker (mockup `.mp-row`), with the shared option menu's row values
/// (`pop_metrics(PopKind::Mode)`: 56 high at least, radius 8): 12 before the text, the name at
/// 14/600 over the harness glyph, the harness and, in red, why the row cannot be chosen; then
/// the shortcut in a `surface-2` key cap 52 wide (review K5) and 8 after it. A row that cannot
/// be chosen is drawn at half strength and says why when tapped. It is a list item, not a menu
/// item, and it says it is selected when it is the active row, the one Enter takes ([leader,
/// 2026-11-21], #888): a menu item would hand the popover to the toolkit's menu keyboard.
fn ModelRow(o: &ModelOption, block: Option<&'static str>, chosen: bool, active: bool, shortcut: Option<String>, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let m = pop_metrics(PopKind::Mode);
    let fill = match model_row_fill(chosen, active, p) {
        Some(c) => paint(c),
        None => Paint::Literal(Color::argb(0)),
    };
    let modifier = Modifier::new()
        .fill_max_width()
        .height(m.row_min_h)
        .background(fill)
        .corner_radius(m.row_radius)
        .role(Role::ListItem)
        .selected(active)
        .click_action(true);
    Clickable(if block.is_some() { modifier.alpha(0.5) } else { modifier }, Alignment::CenterStart, on_click, || {
        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width(), {
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(m.row_pad.1)) };
            // Zero wide and weighted, so a long name ends in an ellipsis.
            { use ::compose_rust::ui::Column; Column!(spacing = 2.0, modifier = Modifier::new().width(0.0).weight(1.0), {
                Label(&o.row_title(), Fs::Md, Weight::Semibold, None, Some(1));
                { use ::compose_rust::ui::Row; Row!(spacing = SP[0], alignment = Alignment::CenterStart, {
                    NavIcon(harness_glyph(o.harness), harness_tint(o.harness), 16.0);
                    let tone = if block.is_some() { p.red.strong } else { p.muted };
                    Label(&model_row_detail(o, block), Fs::Sm, Weight::Regular, Some(paint(tone)), Some(1));
                }) };
            }) };
            if let Some(k) = shortcut {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[1])) };
                { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().size(MODEL_KEY_CAP_W, MODEL_KEY_CAP_H).background(paint(p.surface_2)).corner_radius(CORNER_2XS), content = || Label(&k, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1))) };
            }
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[1])) };
        }) };
    });
}

/// One entry of the model popover's rail (mockup `.mp-rail-b`, review K3): a 36 dp button with
/// only the harness glyph, 22 dp in its vendor colour, named by the harness for assistive
/// technology, and the 3 dp blue bar at the rail's right edge beside the open one
/// (`.mp-rail-b.on::after`). The open entry has no fill of its own in the mockup.
fn RailEntry(h: Harness, on: bool, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let side = (MODEL_RAIL_WIDTH - 1.0 - MODEL_RAIL_BUTTON) / 2.0;
    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, {
        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(side.floor())) };
        NamedClickable(
            h.label(),
            Modifier::new().size(MODEL_RAIL_BUTTON, MODEL_RAIL_BUTTON).corner_radius(CORNER_SM).selected(on),
            Alignment::Center,
            on_click,
            || NavIcon(harness_glyph(h), harness_tint(h), 22.0),
        );
        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(MODEL_RAIL_WIDTH - 1.0 - side.floor() - MODEL_RAIL_BUTTON - 3.0)) };
        let bar = Modifier::new().size(3.0, MODEL_RAIL_BUTTON - 2.0 * SP[1]).corner_radius(2.0);
        { use ::compose_rust::ui::Spacer; Spacer!(modifier = if on { bar.background(paint(p.blue.strong)) } else { bar }) };
    }) };
}

/// The harness glyphs of mockup v8 (`core.js` `GLYPH`), drawn in `currentColor`.
const GLYPH_CLAUDE: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24"><path d="M12 3v18M3 12h18M5.6 5.6l12.8 12.8M18.4 5.6 5.6 18.4" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" fill="none"/></svg>"#;
const GLYPH_CODEX: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24"><path d="M12 2.8 20 7.4v9.2l-8 4.6-8-4.6V7.4z" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" fill="none"/><path d="M8.5 10l2.5 2-2.5 2M12.5 14.5h3" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" fill="none"/></svg>"#;
const GLYPH_GEMINI: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24"><path d="M12 2.5c.6 5.2 4.3 8.9 9.5 9.5-5.2.6-8.9 4.3-9.5 9.5-.6-5.2-4.3-8.9-9.5-9.5 5.2-.6 8.9-4.3 9.5-9.5z" fill="currentColor"/></svg>"#;

/// A composer chip's glyph and tint: the model chip takes the harness glyph in the harness colour,
/// every other chip keeps its icon in `text-2`.
fn composer_chip_icon(pick: Pick, icon: &'static str, harness: Harness) -> (&'static str, Option<u32>) {
    if pick == Pick::Model { (harness_glyph(harness), Some(harness_tint(harness))) } else { (icon, None) }
}

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
    { use ::compose_rust::ui::Column; Column!(alignment = Alignment::Center, spacing = SP[4], modifier = Modifier::new().fill_max_width(), {
        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(top)) };
        // `.hero-bots`: three characters on their baseline, the middle one raised 6 px.
        let bot = if l.compact { 32.0 } else { 40.0 };
        { use ::compose_rust::ui::Row; Row!(spacing = 10.0, alignment = Alignment::BottomStart, {
            HeroBot("claude", "done", bot);
            { use ::compose_rust::ui::Column; Column!({
                HeroBot("codex", "run", bot);
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(6.0)) };
            }) };
            HeroBot("gemini", "wait", bot);
        }) };
        // The mockup greets the signed-in user by name.
        let greeting = format!("{}, {}", DEMO_USER.0, GREETING);
        Label(&greeting, if l.compact { Fs::Xl2 } else { Fs::Xl3 }, GREETING_WEIGHT, None, Some(2));
        ComposerCard(c, ui, composer_width(content_w), l);
        Suggestions(c, width_dp);
        { use ::compose_rust::ui::Row; Row!(spacing = SP[0], alignment = Alignment::CenterStart, {
            Label("오늘의 팀", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
            NavIcon(super::shell::ICON_CHEVRON_DOWN_SVG, p.muted.to_argb(), 14.0);
        }) };
    }) };
}

/// The plain vendor character of `core.js` `bot()`: one shape per harness in its colour.
pub fn hero_bot_svg(vendor: &str, status: &str, _px: f32, p: &Palette) -> String {
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
    let mut s = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100%\" height=\"100%\" viewBox=\"0 0 40 40\"><ellipse cx=\"20\" cy=\"37.6\" rx=\"9\" ry=\"1.5\" fill=\"#1c1b19\" fill-opacity=\".12\"/><path d=\"{shape}\" fill=\"{fill}\"/><path d=\"M11.5 23.5h2.5M26 23.5h2.5\" fill=\"none\" stroke=\"#ffffff\" stroke-opacity=\".35\" stroke-width=\"2.2\" stroke-linecap=\"round\"/><ellipse cx=\"15.5\" cy=\"19\" rx=\"1.9\" ry=\"2.5\" fill=\"{ink}\"/><ellipse cx=\"24.5\" cy=\"19\" rx=\"1.9\" ry=\"2.5\" fill=\"{ink}\"/><path d=\"{mouth}\" fill=\"none\" stroke=\"{ink}\" stroke-width=\"1.6\" stroke-linecap=\"round\"/>"
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
    { use ::compose_rust::ui::Image; Image!(compose_rust::asset(compose_rust::AssetKind::Svg, shell::leak_doc(doc)), modifier = Modifier::new().size(px, px)) };
}

/// How wide a composer chip is.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ChipWidth {
    /// As wide as its content.
    Content,
    /// A 32 dp circle with only the glyph (560 and below, `.hm-pick .pickchip{width:32px}`).
    Circle,
    /// The rest of the row (the project chip at 560 and below).
    Rest,
    /// A share of the line by this weight, the label cut by the toolkit (`ChipLine::Shared`,
    /// #573). The weight itself goes on the popover the chip is the anchor of, which is the
    /// row's direct child (`PopExtra::anchor_weight`); the chip is zero wide so the share, not
    /// its label, decides its width.
    Share(f32),
}

/// What a composer chip shows.
struct ChipSpec<'a> {
    text: &'a str,
    icon: &'static str,
    /// The glyph's own colour (the harness glyph); else the chip's ink.
    tint: Option<u32>,
    chevron: bool,
    /// The mode chip while bypass is on (`.mdpick.warn`).
    warn: bool,
    width: ChipWidth,
    /// Whether the chip's list is open (FR-L10.4).
    expanded: bool,
    /// The name of an icon-only chip for assistive technology ([`chip_name`]).
    name: Option<String>,
}

/// A composer chip's fill, its ink (glyph and label) and its chevron (mockup `.pickchip`):
/// `surface-2` with `text-2` and a `muted` chevron; `hover` with `text` while its list is open
/// (`.pickchip[aria-expanded=true]`); `red-soft` with red ink and a red chevron while bypass is
/// on, open or not (`.mdpick.warn`, `.mdpick.warn .mp-chev{color:inherit}`).
fn chip_colors(warn: bool, expanded: bool, p: &Palette) -> (Color, Color, Color) {
    if warn {
        (p.red.soft, p.red.strong, p.red.strong)
    } else if expanded {
        (p.hover, p.text, p.muted)
    } else {
        (p.surface_2, p.text_2, p.muted)
    }
}

/// What an icon-only composer chip is called (#639; the mockup's `aria-label`s "하네스와 모델:
/// ..." and "권한 모드: ..." and its `title`s 프로젝트 and 컴퓨터). The model and mode names carry
/// the choice, which comes from a fixed list; a project or computer name is data, and the
/// toolkit keeps every distinct name for the life of the program, so those chips say only what
/// they are.
fn chip_name(pick: Pick, text: &str) -> String {
    match pick {
        Pick::Project => "프로젝트".to_string(),
        Pick::Model => format!("하네스와 모델: {text}"),
        Pick::Mode => format!("권한 모드: {text}"),
        Pick::Computer => "컴퓨터".to_string(),
    }
}

/// A composer chip (`.pickchip`): a 32 dp pill, 12 at each side, a 16 dp glyph, 8, the label at
/// 13/500 on one line ending in an ellipsis when the chip is narrower than it, and for a picker
/// 8 and the 14 dp chevron. A [`ChipWidth::Circle`] chip is a 32 dp circle with the glyph
/// centred and a name. The chip is the container's `on_click` (compose-rust#372) and says
/// whether its list is open.
fn ComposerChip(c: ChipSpec<'_>, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let (fill, ink, chev) = chip_colors(c.warn, c.expanded, p);
    // Every composer chip opens a list, so it says whether that list is open (FR-L10.4).
    let base = Modifier::new().height(H_CTL).background(paint(fill)).shape_role(ShapeRole::Full).selected(false).expanded(c.expanded);
    let modifier = match c.width {
        ChipWidth::Content => base,
        ChipWidth::Circle => base.width(H_CTL),
        ChipWidth::Rest => base.weight(1.0),
        ChipWidth::Share(_) => base.width(0.0),
    };
    let modifier = match (&c.name, c.width) {
        (Some(name), ChipWidth::Circle) => modifier.label(name).role(Role::Button).click_action(true),
        _ => modifier,
    };
    let icon_color = c.tint.unwrap_or(ink.to_argb());
    Clickable(modifier, Alignment::Center, on_click, || {
        if c.width == ChipWidth::Circle {
            NavIcon(c.icon, icon_color, 16.0);
            return;
        }
        // A chip that is given its width hands the label what the glyph and chevron leave.
        let given = matches!(c.width, ChipWidth::Rest | ChipWidth::Share(_));
        let row_modifier = if given { Modifier::new().fill_max_width() } else { Modifier::new() };
        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = row_modifier, {
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
            NavIcon(c.icon, icon_color, 16.0);
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[1])) };
            if given {
                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().width(0.0).weight(1.0), {
                    Label(c.text, Fs::Sm, Weight::Medium, Some(paint(ink)), Some(1));
                }) };
            } else {
                Label(c.text, Fs::Sm, Weight::Medium, Some(paint(ink)), Some(1));
            }
            if c.chevron {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[1])) };
                NavIcon(super::shell::ICON_CHEVRON_DOWN_SVG, chev.to_argb(), 14.0);
            }
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
        }) };
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
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
                NavIcon(icon, p.text_2.to_argb(), 18.0);
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[1])) };
                Label(text, Fs::Sm, Weight::Medium, Some(paint(p.text_2)), Some(1));
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
            }) };
        },
    );
}

/// The side of the Home send button (`.sendround`).
pub const SEND_BUTTON_DP: f32 = 36.0;

/// Whether a composer chip draws the picker chevron: the model and mode pickers do
/// (`.mp-chev`); the project and computer chips are the mockup's `select.pickchip`, which has
/// none (`home.js` `.hm-sel`, #573).
fn chip_has_chevron(pick: Pick) -> bool {
    matches!(pick, Pick::Model | Pick::Mode)
}

fn ComposerCard(c: &Composer, ui: &SharedUi, w: f32, l: Layout) {
    let theme = ember_theme();
    let p = theme.palette;
    let sel = c.selection.get();
    let (_, computers) = c.live(ui);
    let message = effective_message(&c.typed.get(), c.suggestion.get().as_deref());
    let sendable = can_send(&crate::ui::home::attachment_message(&message, &c.attachments.get()), c.busy.get());
    let placeholder = c.suggestion.get().unwrap_or_else(|| COMPOSER_PLACEHOLDER.to_string());
    let computer_text = crate::ui::home::computer_chip_text(sel.computer.as_deref(), &computers);
    let computer_text = if sel.computer.is_none() { c.local_name(ui) } else { computer_text };
    let gap = if l.compact { SP[0] } else { 6.0 };
    let model_text = sel.option.chip_label();
    let mode_text = sel.mode.label().to_string();
    let chip = |text: &str, icon: &'static str, pick: Pick, width: ChipWidth| {
        let open = c.open.clone();
        // The harness chip shows the harness's own glyph in its colour (`--v-claude` and so on),
        // at every width (design review B11; it was a generic terminal glyph).
        let (icon, tint) = composer_chip_icon(pick, icon, sel.option.harness);
        let expanded = open.get() == Some(pick);
        // Bypass draws the mode chip in the warning state (mockup `.mdpick.warn`): red on
        // `red-soft` with the warning triangle, and the same chevron, open state and name as
        // the other chips (#574, #639).
        let warn = pick == Pick::Mode && sel.mode.is_bypass();
        let spec = ChipSpec {
            text,
            icon: if warn { ICON_WARN } else { icon },
            tint: if warn { None } else { tint },
            chevron: chip_has_chevron(pick),
            warn,
            width,
            expanded,
            name: (width == ChipWidth::Circle).then(|| chip_name(pick, text)),
        };
        ComposerChip(spec, move || open.set(Some(pick)));
    };
    let add = || {
        let open = c.attachment_open.clone();
        super::components::IconButton(IconRole::Add, true, move || open.set(!open.get_untracked()));
    };
    let send = || {
        let (c2, ui2) = (c.clone(), ui.clone());
        // `.sendround`: a 36 px `--ink` circle with the paper plane, always ink. Composer::send
        // guards `can_send`, so an empty input does nothing. The circle is named "보내기".
        super::session::SendCircleButton(SEND_BUTTON_DP, move || c2.send(&ui2));
    };

    // `.bigcomposer`: surface, 1 px line, the xl radius, 16 / 16 / 12 padding.
    { use ::compose_rust::ui::Box; Box!(modifier = with_shadows(Modifier::new(), theme.shadow_lg())
                .width(w)
                .background(paint(p.surface))
                .border(1.0, paint(p.line))
                .corner_radius(CORNER_XL)
                .padding(if l.compact { SP[2] } else { COMPOSER_PAD }), {
            { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                for item in c.attachments.get() {
                    let attachments = c.attachments.clone();
                    let path = item.path.clone();
                    { use ::compose_rust::ui::Row; Row!(spacing = SP[1], alignment = Alignment::CenterStart, {
                        Label(&item.name, Fs::Sm, Weight::Medium, Some(paint(p.text_2)), Some(1));
                        PickChip("×", None, false, false, move || {
                            let mut items = attachments.get_untracked();
                            items.retain(|item| item.path != path);
                            attachments.set(items);
                        });
                    }) };
                }
                if c.attachment_open.get() {
                    Label(ATTACH_HINT, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
                    let path = c.attachment_path.clone();
                    let add = c.clone();
                    EmberField(FieldOptions {
                        fill_max_width: true,
                        placeholder: "파일의 절대 경로".to_string(),
                        on_value_change: Some(Box::new(move |v| path.set(v))),
                        on_submit: Some(Box::new(move |_| add.add_attachment())),
                        ..Default::default()
                    });
                    if let Some(error) = c.attachment_error.get() {
                        Label(&error, Fs::Xs, Weight::Regular, Some(paint(p.red.strong)), Some(2));
                    }
                    let add = c.clone();
                    PickChip("첨부", None, false, false, move || add.add_attachment());
                }
                let (typed, ui2, c2) = (c.typed.clone(), ui.clone(), c.clone());
                // v0.0.7: #374 and #376 fixed (`TextField::text`, and the field takes the node's
                // frame); workaround kept until checked on a display (a controlled value must not
                // move the caret while typing), so a suggestion shows as the placeholder.
                EmberField(FieldOptions {
                    fill_max_width: true,
                    multiline: true,
                    bare: true,
                    inset: Some((2.0, 2.0)),
                    height: Some(if l.compact { 64.0 } else { 84.0 }),
                    placeholder,
                    on_value_change: Some(Box::new(move |v| typed.set(v))),
                    on_submit: Some(Box::new(move |_| c2.send(&ui2))),
                    ..Default::default()
                });
                if c.suggestion.get().is_some() {
                    { use ::compose_rust::ui::Row; Row!(spacing = SP[1], alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width(), {
                        Label("제안을 고르셨습니다. 입력하지 않으면 이 내용으로 시작합니다", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                        let s = c.suggestion.clone();
                        PickChip("취소", None, false, false, move || s.set(None));
                    }) };
                }
                { use ::compose_rust::ui::Row; Row!(spacing = gap, alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width(), {
                    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().size(36.0, 36.0), content = add) };
                    { use ::compose_rust::ui::Column; Column!(spacing = SP[0], modifier = Modifier::new().weight(1.0).clip(true), {
                        // A chip with a share is one of four that share the line by weight; at
                        // 560 and below the project chip takes the rest and the others are
                        // circles. Above 560 each chip is the anchor of its own picker.
                        let chip_at = |i: usize, text: &str, share: Option<f32>| {
                            let width = match share {
                                Some(weight) => ChipWidth::Share(weight),
                                None if !l.compact => ChipWidth::Content,
                                None if i == 0 => ChipWidth::Rest,
                                None => ChipWidth::Circle,
                            };
                            match i {
                                0 => {
                                    if l.compact {
                                        chip(text, ICON_FOLDER, Pick::Project, width);
                                    } else {
                                        c.project_menu(ui, share, || chip(text, ICON_FOLDER, Pick::Project, width));
                                    }
                                }
                                1 => {
                                    let close = {
                                        let o = c.open.clone();
                                        move || o.set(None)
                                    };
                                    let one = || chip(text, ICON_TERMINAL, Pick::Model, width);
                                    if l.compact {
                                        one();
                                    } else {
                                        c.model_popover(ui, &sel, share, one, close);
                                    }
                                }
                                2 => {
                                    let one = || chip(text, ICON_SHIELD, Pick::Mode, width);
                                    if l.compact {
                                        one();
                                    } else {
                                        c.mode_menu(&sel, share, one);
                                    }
                                }
                                _ => {
                                    if l.compact {
                                        chip(text, ICON_PC, Pick::Computer, width);
                                    } else {
                                        c.computer_menu(ui, share, || chip(text, ICON_PC, Pick::Computer, width));
                                    }
                                }
                            }
                        };
                        let texts = [sel.project.clone(), model_text.clone(), mode_text.clone(), computer_text.clone()];
                        if l.compact {
                            { use ::compose_rust::ui::Row; Row!(spacing = gap, alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width(), {
                                for (i, t) in texts.iter().enumerate() {
                                    chip_at(i, t, None);
                                }
                            }) };
                        } else {
                            // One line as the mockup (#849 review H1, #573): the labels go to
                            // the toolkit whole (cut only at their caps). With room every chip
                            // is as wide as its content; without, the four share the line by
                            // weight and the toolkit ends each label in an ellipsis by its
                            // measured width; the row wraps only when even the shortest labels
                            // do not fit, rather than dropping a chip.
                            let fitted = crate::ui::home::composer_chip_fit(&texts, w);
                            match composer_chip_line(&fitted, w) {
                                ChipLine::Natural => {
                                    { use ::compose_rust::ui::Row; Row!(spacing = gap, alignment = Alignment::CenterStart, {
                                        for (i, t) in fitted.iter().enumerate() {
                                            chip_at(i, t, None);
                                        }
                                    }) };
                                }
                                ChipLine::Shared(weights) => {
                                    { use ::compose_rust::ui::Row; Row!(spacing = gap, alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width(), {
                                        for (i, t) in fitted.iter().enumerate() {
                                            chip_at(i, t, Some(weights[i]));
                                        }
                                    }) };
                                }
                                ChipLine::Wrapped(rows) => {
                                    for row in rows {
                                        { use ::compose_rust::ui::Row; Row!(spacing = gap, alignment = Alignment::CenterStart, {
                                            for i in row {
                                                chip_at(i, &fitted[i], None);
                                            }
                                        }) };
                                    }
                                }
                            }
                        }
                    }) };
                    if c.busy.get() {
                        { use ::compose_rust::ui::IndeterminateProgressIndicator; IndeterminateProgressIndicator!(circular = true, modifier = Modifier::new().size(18.0, 18.0)) };
                    }
                    send();
                }) };
            }) };
        }) };
}

/// The glyph of the `i`th suggestion in a line. TODO(item 6): the mockup's spark star needs the
/// harness glyph asset; `code.svg` stands in until it lands.
/// The glyph of the `i`th suggestion of [`SUGGESTIONS`], counted over all lines: spark, branch,
/// chart (`home.js:9`). Design review 2026-11-13 B11: the index restarted on every line, so at 390
/// the first and the third chip both showed the code glyph.
fn suggestion_icon(i: usize) -> &'static str {
    [ICON_SPARK, ICON_GIT, ICON_CHART_NAV][i % 3]
}

/// `.sugg`: outlined chips, three a line (two up to 900 px). The block is centred under the
/// composer and a short last line starts at its left edge, as the mockup's wrapped flex row
/// (review 2026-11-14 H8, #849).
fn Suggestions(c: &Composer, width_dp: f32) {
    let n = suggestion_cols(width_dp);
    { use ::compose_rust::ui::Column; Column!(alignment = Alignment::TopStart, spacing = SP[1], {
        for (row, line) in SUGGESTIONS.chunks(n).enumerate() {
            { use ::compose_rust::ui::Row; Row!(spacing = SP[1], {
                for (i, text) in line.iter().enumerate() {
                    let s = c.suggestion.clone();
                    let t = apply_suggestion(&c.typed.get_untracked(), text);
                    SuggestionChip(text, suggestion_icon(row * n + i), move || s.set(Some(t.clone())));
                }
            }) };
        }
    }) };
}

// ---------------------------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------------------------

fn Dashboard(d: &Dash, ui: &SharedUi, l: Layout, content_w: f32, skip: &Skip) {
    let week = services().client.read(|s| weekday_usage(s, now_ms()));
    let p = ember_theme().palette;
    let k = kpis(&d.counts, d.online, d.computers);
    { use ::compose_rust::ui::Column; Column!(spacing = SP[4], modifier = Modifier::new().fill_max_width(), {
        if skip.show(EST_HEAD) {
            { use ::compose_rust::ui::Column; Column!({
                // `.hm-sh h2` is 28/600 at every width (review 2026-11-14 H7, #849).
                Label("오늘의 팀", Fs::Xl2, Weight::Semibold, None, Some(1));
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(10.0)) };
                Label(&format!("{} · 서버 기준", date_line(&crate::ui::home::today_utc())), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(12.0)) };
            }) };
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
    }) };
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
    let ui = shared_ui().clone();
    let il = integrations_layout(crate::ui::window_size::current_window_size().width_dp);
    let github = || Tile(|| {
            { use ::compose_rust::ui::Row; Row!(spacing = 10.0, alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width(), {
                { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().size(30.0, 30.0).border(1.0, paint(p.line)).corner_radius(CORNER_SM), content = || Label("G", Fs::Md, Weight::Semibold, None, Some(1))) };
                Label(GITHUB_NAME, Fs::Lg, Weight::Semibold, None, Some(1));
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                let tone = match card.tone {
                    GithubPill::Soon => PillTone::Grey,
                    GithubPill::Off => PillTone::Grey,
                    GithubPill::Warn => PillTone::Orange,
                    GithubPill::On => PillTone::Green,
                };
                Pill(card.pill, tone, true);
            }) };
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
                { use ::compose_rust::ui::Row; Row!(spacing = SP[2], modifier = Modifier::new().fill_max_width(), {
                    Label(label, Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                    Label(value, Fs::Md, Weight::Regular, None, Some(1));
                }) };
            }
            for r in &card.repos {
                { use ::compose_rust::ui::Column; Column!(spacing = 4.0, modifier = Modifier::new().fill_max_width(), {
                    { use ::compose_rust::ui::Row; Row!(spacing = SP[2], modifier = Modifier::new().fill_max_width(), {
                        Label(&r.name, Fs::Md, Weight::Medium, None, Some(1));
                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                        if r.error.is_none() {
                            Label(&format!("PR {} · 리뷰 {}", r.open_prs, r.review_requested), Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
                        }
                    }) };
                    if let Some(e) = &r.error {
                        Label(e, Fs::Md, Weight::Regular, Some(paint(p.red.strong)), None);
                    } else if !r.ci.is_empty() {
                        CiBar(&r.ci);
                    }
                }) };
            }
            if let Some(more) = &card.more_repos {
                Label(more, Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
            }
            // The server sends no list of what the integration did, so none is drawn.
            Label(card.activity_empty, Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
            CiRuleRow(&ui);
        });
    let later = || {
        let cards: Vec<Part> = LATER_INTEGRATIONS
            .into_iter()
            .map(|name: &'static str| -> Part {
                Box::new(move || {
                    let p = ember_theme().palette;
                    Tile(|| {
                        { use ::compose_rust::ui::Row; Row!(spacing = 10.0, alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width(), {
                            Label(name, Fs::Md, Weight::Semibold, None, Some(1));
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                            Pill(LATER_PILL, PillTone::Grey, true);
                        }) };
                        Label(LATER_LINE, Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
                    })
                })
            })
            .collect();
        Cols(il.later_cols, GAP_CARD, cards);
    };
    { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width(), {
        Label(INTEGRATIONS_TITLE, Fs::Lg, Weight::Semibold, None, Some(1));
        Label(&header_line, Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
        // `.int-grid`: GitHub span 6 beside the span-3 cards above 1279, else stacked.
        Weighted(!il.github_beside, GAP_CARD, vec![(1.0, Box::new(github) as Part), (1.0, Box::new(later) as Part)]);
    }) };
}

/// `.int-rule`: CI 실패 -> 루틴, with the note and a link to 루틴 (FR-L23, #498, #759). This row
/// only opens the 루틴 screen; the server's CI watcher runs event routines only when connected to a
/// server (never in demo mode or while offline).
fn CiRuleRow(ui: &SharedUi) {
    let p = ember_theme().palette;
    { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().fill_max_width().background(paint(p.surface_2)).corner_radius(CORNER_SM).padding(SP[2]), {
            { use ::compose_rust::ui::Row; Row!(spacing = 6.0, alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width(), {
                Label(CI_RULE_WHEN, Fs::Xs, Weight::Semibold, None, Some(1));
                Label("\u{2192}", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                Label(CI_RULE_THEN, Fs::Xs, Weight::Regular, None, Some(1));
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                let u = ui.clone();
                AppButton(CI_RULE_LINK, None, ButtonKind::Default, true, true, false, move || shell::choose(&u, NavMenu::Routines));
            }) };
            Label(CI_RULE_NOTE, Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
        }) };
}

/// Recent CI results of one repository: small cells, newest first (left).
fn CiBar(results: &[CiResult]) {
    let p = ember_theme().palette;
    { use ::compose_rust::ui::Row; Row!(spacing = 2.0, {
        for r in results {
            let c = match r {
                CiResult::Success => p.green.strong,
                CiResult::Failure => p.red.strong,
                CiResult::Cancelled => p.grey.strong,
                CiResult::Pending => p.track,
            };
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().size(8.0, 14.0).background(paint(c)).corner_radius(2.0)) };
        }
    }) };
}

/// The unit or denominator after a KPI number: `.hm-kv small`, half the 28 px number, 500.
pub const KPI_SMALL: (Fs, Weight) = (Fs::Md, Weight::Medium);
/// `.hm-kv small{margin-left:2px}`.
pub const KPI_SMALL_GAP: f32 = 2.0;

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
    { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width().height(64.0), {
        if rule {
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(1.0).height(64.0).background(paint(p.line))) };
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[3])) };
        }
        { use ::compose_rust::ui::Column; Column!(spacing = 6.0, modifier = Modifier::new().weight(1.0), {
            Label(kpi.label, Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
            if kpi.available {
                { use ::compose_rust::ui::Row; Row!(spacing = SP[1], alignment = Alignment::CenterStart, {
                    // The unit or the denominator hugs the number (`small{margin-left:2px}`), small
                    // and muted (design review B9).
                    let (number, small) = crate::ui::home::kpi_value_parts(kpi);
                    { use ::compose_rust::ui::Row; Row!(spacing = 0.0, alignment = Alignment::CenterStart, {
                        Label(&number, Fs::Xl2, Weight::Regular, None, Some(1));
                        if !small.is_empty() {
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(KPI_SMALL_GAP)) };
                            Label(&small, KPI_SMALL.0, KPI_SMALL.1, Some(paint(p.muted)), Some(1));
                        }
                    }) };
                    if let Some(note) = &kpi.note {
                        Pill(note, tone, true);
                    }
                }) };
            } else {
                Label(&kpi.value, Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
                if let Some(note) = &kpi.note {
                    Label(note, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                }
            }
        }) };
    }) };
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
    const H: f32 = TREND_PLOT_H;
    let commands = trend_draw_list(&values, &usage::trend_costs(&load, days.get()), chart_w, p);
    let narrow = layout(crate::ui::window_size::current_window_size().width_dp).compact;
    let legend = usage::cost_legend(
        usage::period_cost(&load, days.get()).is_some(),
        usage::window_total(&load, days.get()).is_some(),
        narrow,
    );
    let first = series.first().map(|(d, _)| d.clone()).unwrap_or_default();
    let last = series.last().map(|(d, _)| d.clone()).unwrap_or_default();
    // The day under the pointer (FR-L23.1, compose-rust#417): the plot's box takes pointer input
    // and the line under the plot reads that day. Leaving the plot shows the window's total again.
    let picked = remember(|| mutable_state_of(None::<usize>));
    let day_line = picked
        .get()
        .and_then(|i| series.get(i))
        .map(|(day, tokens)| usage::trend_day_line(day, *tokens))
        .unwrap_or_else(|| usage::trend_line(&load, days.get()));
    let day_count = series.len();
    Tile(|| {
        // `.hm-ico` is the bar chart (`I("chart")`), as on the mockup.
        CardHead(ICON_CHART_NAV, "비용과 토큰 추이", || {
            for e in &legend {
                DotOrHatch(e.hatched, p.grey.strong, p.ink);
                Label(e.label, Fs::Xs2, Weight::Regular, Some(paint(p.muted)), Some(1));
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
            }
            for n in [7usize, 14] {
                RangeToggle(n, days.get() == n, {
                    let d = days.clone();
                    move || d.set(n)
                });
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[0])) };
            }
        });
        // `.hm-sub`: the four stats above the plot, 28/600 values under small muted labels, 32
        // apart; a missing one reads "데이터 없음". Cache hits stay without data: the usage
        // event has no cache tokens (FR-U7). `.hide-sm` drops 캐시 적중 on a narrow card.
        { use ::compose_rust::ui::Row; Row!(spacing = SP[6], alignment = Alignment::TopStart, modifier = Modifier::new().fill_max_width(), {
            for (i, (label, value)) in usage::cost_stats(&load, days.get()).into_iter().enumerate() {
                if narrow && i == 3 {
                    continue;
                }
                { use ::compose_rust::ui::Column; Column!(spacing = 2.0, {
                    Label(label, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                    if value == usage::NO_DATA {
                        Label(&value, Fs::Sm, Weight::Medium, Some(paint(p.muted)), Some(1));
                    } else {
                        Label(&value, Fs::Xl2, Weight::Semibold, None, Some(1));
                    }
                }) };
            }
        }) };
        let pick = picked.clone();
        { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().size(chart_w, H + 1.0), on_pointer = Some((TREND_POINTER_KINDS, move |e: ::compose_rust::PointerEvent| {
                let day = match e.kind() {
                    compose_rust::PointerKind::Leave | compose_rust::PointerKind::Cancel => None,
                    _ => usage::trend_day_at(e.x(), day_count, chart_w, TREND_BAR_GAP),
                };
                if pick.get_untracked() != day {
                    pick.set(day);
                }
            })), {
                { use ::compose_rust::ui::Canvas; Canvas!(commands, modifier = Modifier::new().size(chart_w, H + 1.0)) };
            }) };
        { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width(), {
            Label(&first, Fs::Xs2, Weight::Regular, Some(paint(p.muted)), Some(1));
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
            Label(&last, Fs::Xs2, Weight::Regular, Some(paint(p.muted)), Some(1));
        }) };
        Label(&day_line, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
    });
}

/// The trend plot's height.
const TREND_PLOT_H: f32 = 160.0;

/// The gap between two bars of the trend plot.
const TREND_BAR_GAP: f32 = 6.0;

/// The pointer input the trend plot asks for: a press or a move names the day, leaving clears it.
const TREND_POINTER_KINDS: compose_rust::PointerKinds = compose_rust::PointerKinds::DOWN
    .union(compose_rust::PointerKinds::MOVE)
    .union(compose_rust::PointerKinds::LEAVE)
    .union(compose_rust::PointerKinds::CANCEL);

/// The paint of the area under the cost line: `ink` at 5 % (`.hm-area`).
fn cost_area_paint(p: &Palette) -> Paint {
    Paint::Literal(Color::argb((usage::COST_AREA_ALPHA << 24) | (p.ink.to_argb() & 0x00ff_ffff)))
}

/// What the trend plot draws, `chart_w` wide and [`TREND_PLOT_H`] high over a 1 dp baseline:
/// one hatched bar per day (the last in orange), and over them the cost of the days the server
/// priced (mockup `.hm-area`, `.hm-line`, `.hm-pt`, #849): the 5 % ink area under the line, the
/// 2 dp ink line, and a ringed point on the last costed day. With no cost in the window (the
/// server sends none today) only the bars are drawn, never a line at zero.
fn trend_draw_list(values: &[u64], costs: &[Option<f64>], chart_w: f32, p: &Palette) -> DrawList {
    const H: f32 = TREND_PLOT_H;
    let usize_values: Vec<usize> = values.iter().map(|v| *v as usize).collect();
    let rects = crate::ui::home::bar_rects(&usize_values, chart_w, H, TREND_BAR_GAP);
    let centers: Vec<f32> = rects.iter().map(|(x, _, w, _)| x + w / 2.0).collect();
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
    let points = usage::cost_line_points(costs, &centers, H);
    // The toolkit's `DrawList` has no filled path, so the area is columns of the line's height.
    for (x, y) in usage::cost_area_columns(&points, usage::COST_AREA_STEP) {
        list = list.line(cost_area_paint(p), x, y, x, H, usage::COST_AREA_STEP);
    }
    for pair in points.windows(2) {
        list = list.line(paint(p.ink), pair[0].0, pair[0].1, pair[1].0, pair[1].1, 2.0);
    }
    if let Some((x, y)) = points.last() {
        list = list.circle(paint(p.surface), *x, *y, 6.0, 0.0).circle(paint(p.ink), *x, *y, 4.5, 3.0);
    }
    list.build()
}

/// `.hm-seg button`: the 7일 / 14일 toggle, 28 dp pills (`.hm-seg button{height:28px}`) with a
/// 1 dp `line` outline on `surface`; the chosen one is `ink` with `ink-text` (`.seg button.on`,
/// review 2026-11-14 H4, #849). The pill keeps the 42 dp width of #509.
fn RangeToggle(n: usize, on: bool, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let (fill, edge, text) = range_toggle_colors(on, &p);
    Clickable(
        Modifier::new().size(42.0, 28.0).background(paint(fill)).border(1.0, paint(edge)).shape_role(ShapeRole::Full),
        Alignment::Center,
        on_click,
        || {
            Label(&format!("{n}일"), Fs::Xs, Weight::Medium, Some(paint(text)), Some(1));
        },
    );
}

/// The toggle's `(fill, outline, text)`: `ink` / `ink` / `ink-text` when chosen, else
/// `surface` / `line` / `text-2`.
fn range_toggle_colors(on: bool, p: &Palette) -> (Color, Color, Color) {
    if on { (p.ink, p.ink, p.ink_text) } else { (p.surface, p.line, p.text_2) }
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
        CardHead(ICON_TEAM, "팀원 상태", || {
            let u = ui.clone();
            GoButton(move || shell::choose(&u, NavMenu::Sessions));
        });
        // `.hm-alert`: a `surface-2` pill with the waiting count and the failures.
        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width().height(36.0).background(paint(p.surface_2)).shape_role(ShapeRole::Full), {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(14.0)) };
                Label(&format!("승인 대기 {}명", c.waiting), Fs::Sm, Weight::Regular, None, Some(1));
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                Pill(&format!("실패 {}", c.failed), if c.failed > 0 { PillTone::Red } else { PillTone::Green }, true);
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[0])) };
            }) };
        // `.hm-blocks`: coloured blocks sized by count, each with the 18 % white stripes. The
        // stripes are a canvas at the block's estimated width (the card's inner width shared by
        // the weights, less 8 dp so an estimate that is a little high stops short rather than
        // spilling past the block). v0.0.7: #388 fixed (`Canvas::view_box` and `fractional`);
        // workaround kept until checked on a display (the stripes must follow the weighted box).
        let bh = if compact { 88.0 } else { 116.0 };
        let weights: Vec<f32> = parts.iter().map(|(_, n, _)| (*n as f32).max(0.6)).collect();
        let sum: f32 = weights.iter().sum();
        let inner = (tile_w - 2.0 * PAD_CARD_H - 3.0 * 6.0).max(60.0);
        { use ::compose_rust::ui::Row; Row!(spacing = 6.0, modifier = Modifier::new().fill_max_width().height(bh), {
            for ((_, _, color), wt) in parts.iter().zip(weights.iter()) {
                let w = inner * wt / sum;
                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(*wt).height(bh).background(paint(*color)).corner_radius(CORNER_MD), {
                        let stripe = paint(compose_rust::Color::argb(0x2eff_ffff));
                        let mut list = DrawList::builder();
                        for (x1, y1, x2, y2) in crate::ui::home::hatch_lines(0.0, 0.0, (w - 8.0).max(1.0), bh, 7.0) {
                            list = list.line(stripe, x1, y1, x2, y2, 1.0);
                        }
                        { use ::compose_rust::ui::Canvas; Canvas!(list.build(), modifier = Modifier::new().size((w - 8.0).max(1.0), bh)) };
                    }) };
            }
        }) };
        Cols(2, SP[2], parts.iter().map(|(label, n, color)| -> Part {
            Box::new(move || {
                { use ::compose_rust::ui::Row; Row!(spacing = SP[1], alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width(), {
                    Dot(*color);
                    Label(label, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                    Label(&n.to_string(), Fs::Xl, Weight::Regular, None, None);
                }) };
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
        if crate::ui::project::project_archived(state, &p.name) {
            continue;
        }
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
            { use ::compose_rust::ui::Canvas; Canvas!(commands, modifier = Modifier::new().size(chart_w, H + 1.0)) };
            { use ::compose_rust::ui::Row; Row!(spacing = GAP, modifier = Modifier::new().fill_max_width(), {
                for (i, day) in model.days.iter().enumerate() {
                    let today = i + 1 == model.days.len();
                    { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().width(bar_w), {
                        Label(day.weekday, Fs::Xs2, if today { Weight::Semibold } else { Weight::Regular }, Some(paint(if today { p.text } else { p.muted })), Some(1));
                    }) };
                }
            }) };
            Label(model.axis_caption, Fs::Xs2, Weight::Regular, Some(paint(p.muted)), Some(1));
            for row in &rows {
                { use ::compose_rust::ui::Row; Row!(spacing = 16.0, {
                    for &i in row {
                        let e = &model.legend[i];
                        { use ::compose_rust::ui::Row; Row!(spacing = SP[0], alignment = Alignment::CenterStart, {
                            Dot(vendor_color(e.vendor, &p));
                            Label(&e.label, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                        }) };
                    }
                }) };
            }
            // The dashed average line has its own legend entry.
            { use ::compose_rust::ui::Row; Row!(spacing = SP[0], alignment = Alignment::CenterStart, {
                { use ::compose_rust::ui::Canvas; Canvas!(DrawList::builder().rect(paint(p.text), 0.0, 0.0, 3.0, 1.0, 0.0).rect(paint(p.text), 6.0, 0.0, 3.0, 1.0, 0.0).rect(paint(p.text), 12.0, 0.0, 3.0, 1.0, 0.0).build(), modifier = Modifier::new().size(15.0, 1.0)) };
                Label(model.average_legend, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
            }) };
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
        { use ::compose_rust::ui::Row; Row!(spacing = SP[1], alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width().height(30.0), {
            Label("지금 일하는 팀원", Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
            Pill(&format!("실행 {running}"), PillTone::Blue, false);
            if waiting > 0 {
                Pill(&format!("승인 대기 {waiting}"), PillTone::Orange, false);
            }
            let u = ui.clone();
            GoButton(move || shell::choose(&u, NavMenu::Sessions));
        }) };
        if crew.is_empty() {
            Label(EMPTY_CREW, Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
        }
        Cols(cols, SP[2], crew.iter().map(|m| -> Part {
            Box::new(move || {
                let (u, id) = (ui.clone(), m.id.clone());
                Clickable(Modifier::new().fill_max_width(), Alignment::Center, move || u.open_conversation(&id), || {
                    { use ::compose_rust::ui::Column; Column!(alignment = Alignment::Center, spacing = SP[0], modifier = Modifier::new().fill_max_width(), {
                        let state = if m.waiting { super::characters::CharState::Wait } else { super::characters::CharState::Run };
                        super::characters::Character(&m.agent, false, 52.0, &m.title, &m.project, state, None);
                        Label(&model::one_line(&m.title, 24), Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(2));
                    }) };
                });
            })
        }).collect());
        { use ::compose_rust::ui::Row; Row!(spacing = SP[1], alignment = Alignment::CenterStart, {
            NavIcon(shell::ICON_CPU_SVG, p.muted.to_argb(), 14.0);
            Label(&computers_line(online, computers), Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
        }) };
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
        { use ::compose_rust::ui::Row; Row!(spacing = SP[4], alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width(), {
            { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().size(132.0, 132.0), {
                let frac = |n: usize| if ended == 0 { 0.0 } else { n as f32 / ended as f32 };
                ring_image(132.0, 16.0, &[(p.green.strong.to_argb(), frac(ok)), (p.red.strong.to_argb(), frac(c.failed))]);
                { use ::compose_rust::ui::Column; Column!(alignment = Alignment::Center, {
                    let text = pct.map(|v| format!("{v}%")).unwrap_or_else(|| "집계 전".to_string());
                    Label(&text, Fs::Xl2, Weight::Regular, None, Some(1));
                    Label(&format!("{ended}건"), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                }) };
            }) };
            { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().weight(1.0), {
                for (label, n, color) in [("성공", ok, p.green.strong), ("실패", c.failed, p.red.strong)] {
                    { use ::compose_rust::ui::Row; Row!(spacing = SP[1], alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width(), {
                        Dot(color);
                        Label(label, Fs::Sm, Weight::Regular, None, Some(1));
                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                        Label(&n.to_string(), Fs::Sm, Weight::Semibold, None, None);
                    }) };
                }
                Label("CI 결과는 GitHub 연동에서 옵니다", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
            }) };
        }) };
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
        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width().height(32.0), {
            Label("프로젝트", Fs::Lg, Weight::Semibold, None, Some(1));
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
            let u = ui.clone();
            AppButton("모두 보기", None, ButtonKind::Default, true, true, false, move || {
                u.set_selected_project(None);
                shell::choose(&u, NavMenu::Projects);
            });
        }) };
        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(SP[0])) };
        if tiles.is_empty() {
            Label(EMPTY_PROJECTS, Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
        }
        Cols(l.proj_cols, SP[1], tiles.iter().map(|t| -> Part {
            Box::new(move || {
                let (u, name) = (ui.clone(), t.name.clone());
                // `.ptile`: a `surface-2` row, a 40 px ring with the member count, the name and
                // the counts; the container's `on_click` (compose-rust#372).
                Clickable(
                    Modifier::new().fill_max_width().background(paint(p.surface_2)).corner_radius(CORNER_MD).padding(10.0),
                    Alignment::CenterStart,
                    move || {
                        u.set_selected_project(Some(name.clone()));
                        shell::choose(&u, NavMenu::Projects);
                    },
                    || {
                        { use ::compose_rust::ui::Row; Row!(spacing = 10.0, alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width(), {
                            { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().size(40.0, 40.0), {
                                let n = t.total.max(1) as f32;
                                let rest = t.total.saturating_sub(t.running + t.waiting);
                                ring_image(40.0, 5.0, &[
                                    (p.blue.strong.to_argb(), t.running as f32 / n),
                                    (p.orange.strong.to_argb(), t.waiting as f32 / n),
                                    (p.sky.strong.to_argb(), rest as f32 / n),
                                ]);
                                Label(&t.total.to_string(), Fs::Sm, Weight::Semibold, None, None);
                            }) };
                            { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().weight(1.0), {
                                Label(&crate::ui::project::display_name(&t.name), Fs::Md, Weight::Semibold, None, Some(1));
                                // `.botrow`: up to five team members in their state, drawn at 28 dp so the body is about the 20 px the mockup shows (the drawing has margin in its box).
                                let members = services().client.read(|s| project_members(s, &t.name, 5));
                                { use ::compose_rust::ui::Row; Row!(spacing = 2.0, alignment = Alignment::CenterStart, {
                                    for (agent, status, title) in members.iter() {
                                        super::characters::Character(agent, false, 28.0, title, &t.name, super::characters::CharState::from_status(*status), None);
                                    }
                                }) };
                            }) };
                        }) };
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
    fn fr_l11_attachment_bar_and_chip_render() {
        let _guard = crate::services::test_guard();
        let mut host = Host::new(|| Box::new(Recomposer::new(|| {
            let c = Composer::new();
            c.attachment_open.set(true);
            c.attachments.set(vec![ComposerAttachment { path: "/sample/file.png".into(), name: "file.png".into() }]);
            ComposerCard(&c, shared_ui(), 720.0, layout(1440.0));
        })));
        let bytes = host.rebuild().expect("rebuild");
        let batch = decode_batch(bytes).expect("decode");
        let all = texts(&batch);
        for expected in ["file.png", "×", ATTACH_HINT, "파일의 절대 경로"] {
            assert!(all.iter().any(|text| text == expected), "missing {expected}: {all:?}");
        }
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
        assert_eq!(production.matches("ui::Button; Button!(").count(), 0);

        // The pickers carry the chevron; the project and computer chips are the mockup's
        // `select.pickchip`, which has none (#573).
        assert!(chip_has_chevron(Pick::Model) && chip_has_chevron(Pick::Mode));
        assert!(!chip_has_chevron(Pick::Project) && !chip_has_chevron(Pick::Computer));
    }

    #[test]
    fn fr_l23_1_the_trend_plot_takes_the_pointer_and_reads_the_day_under_it() {
        // compose-rust#417 (#673): `on_pointer` on the plot's box; the model picks the day
        // (`usage::trend_day_at`, tested there) and the line under the plot reads it.
        let production = include_str!("home.rs").split("#[cfg(test)]").next().unwrap();
        let tile = production.split("fn TokenTrendTile(").nth(1).unwrap().split("\nfn ").next().unwrap();
        assert!(tile.contains("on_pointer = Some((TREND_POINTER_KINDS, move |e: ::compose_rust::PointerEvent| {"));
        assert!(tile.contains("usage::trend_day_at(e.x(), day_count, chart_w, TREND_BAR_GAP)"));
        assert!(tile.contains("compose_rust::PointerKind::Leave | compose_rust::PointerKind::Cancel => None"));
        assert!(tile.contains("Label(&day_line, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);"));
        assert!(!tile.contains("TODO(compose-rust#417"));
        // The plot and the model agree on the bars: the same gap lays them out and finds them.
        assert_eq!(TREND_BAR_GAP, 6.0);
        assert!(production.contains("bar_rects(&usize_values, chart_w, H, TREND_BAR_GAP)"));
        let rects = crate::ui::home::bar_rects(&[1, 2, 3, 4, 5, 6, 7], 400.0, TREND_PLOT_H, TREND_BAR_GAP);
        for (i, (x, _, w, _)) in rects.iter().enumerate() {
            assert_eq!(usage::trend_day_at(x + w / 2.0, 7, 400.0, TREND_BAR_GAP), Some(i), "the middle of bar {i}");
        }
        for kind in [compose_rust::PointerKind::Down, compose_rust::PointerKind::Move, compose_rust::PointerKind::Leave, compose_rust::PointerKind::Cancel] {
            assert!(TREND_POINTER_KINDS.contains(kind), "{kind:?}");
        }
        assert!(!TREND_POINTER_KINDS.contains(compose_rust::PointerKind::Wheel), "the page still scrolls over the plot");
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
        assert!(svg.contains("#3f74d4") && svg.contains("width=\"100%\""));
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
        /// The last `Width` and `Weight` modifiers of each node.
        width: std::collections::HashMap<u32, f32>,
        weight: std::collections::HashMap<u32, f32>,
        /// Every mutation as its debug text, for properties the tests read by name.
        log: Vec<String>,
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
                    Mutation::SetModifier { node_id, modifier: compose_rust::Modifier::Width(w), .. } => {
                        self.width.insert(node_id, w);
                    }
                    Mutation::SetModifier { node_id, modifier: compose_rust::Modifier::Weight(w), .. } => {
                        self.weight.insert(node_id, w);
                    }
                    _ => {}
                }
            }
            self.log.extend(decode_batch(bytes).expect("decode").iter().map(|m| format!("{m:?}")));
        }

        /// The node of the text `label`.
        fn text_node(&self, label: &str) -> u32 {
            *self.text.iter().find(|(_, t)| t.as_str() == label).unwrap_or_else(|| panic!("no text {label}")).0
        }

        /// The debug texts of the mutations that set `property` (by its name) on `node`.
        fn props(&self, node: u32, property: &str) -> Vec<&String> {
            let (id, name) = (format!("node_id: {node},"), format!("property: {property},"));
            self.log.iter().filter(|m| m.starts_with("SetProp") && m.contains(&id) && m.contains(&name)).collect()
        }

        /// The debug texts of every mutation that sets `property`.
        fn all_props(&self, property: &str) -> Vec<&String> {
            let name = format!("property: {property},");
            self.log.iter().filter(|m| m.starts_with("SetProp") && m.contains(&name)).collect()
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

        /// How many nodes are `side` wide and take a click (a square button of that side).
        fn clickable_squares(&self, side: f32) -> usize {
            self.width.iter().filter(|(n, w)| **w == side && self.handler_of(**n, PropertyKind::OnClick).is_some()).count()
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
        let (bytes, _) = host.dispatch(HostEvent::new(node_id, handler_id, compose_rust::EventPayload::Clicked)).expect("click");
        tree.read(bytes);
    }

    #[test]
    fn fr_l15_model_popover_opens_on_the_chosen_harness_with_a_harness_rail() {
        let _guard = crate::services::test_guard();
        let (_host, tree, c) = model_picker_host();
        assert_eq!(c.selection.get_untracked().option.harness, Harness::ClaudeCode);
        // The rail is icon-only (review K3): three 36 dp buttons beside the send circle, none
        // of them a text.
        assert!(!tree.has(Harness::Codex.label()) && !tree.has(Harness::Gemini.label()), "rail entries are glyphs: {:?}", tree.sent);
        assert!(tree.clickable_squares(MODEL_RAIL_BUTTON) >= Harness::ALL.len() + 1, "three rail buttons and the send circle");
        assert!(!tree.has("전체"), "the rail has no all entry (mockup v8)");
        assert!(tree.has("Opus 5.5 (높음)") && tree.has("Haiku 5.5 (낮음)"));
        assert!(!tree.has("GPT-5 (높음)"), "only the chosen harness is listed");
        assert!(tree.has("Ctrl+1"));
    }

    #[test]
    fn fr_l15_rail_tap_lists_that_harness_and_a_row_tap_chooses_and_closes() {
        let _guard = crate::services::test_guard();
        let (mut host, mut tree, c) = model_picker_host();
        c.rail_tap(Harness::Codex);
        tree.read(host.render_frame(1).expect("frame"));
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
            .dispatch(HostEvent::new(node_id, handler_id, compose_rust::EventPayload::TextChanged("sonnet 보통")))
            .expect("type");
        tree.read(bytes);
        let (node_id, handler_id) = tree.field("모델 검색", PropertyKind::OnSubmit);
        let (bytes, _) = host
            .dispatch(HostEvent::new(node_id, handler_id, compose_rust::EventPayload::TextSubmitted("sonnet 보통")))
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
            .dispatch(HostEvent::new(node_id, handler_id, compose_rust::EventPayload::TextChanged("gemini flash")))
            .expect("type");
        tree.read(bytes);
        assert!(tree.has("Gemini · 준비 중입니다"), "the row says why: {:?}", tree.sent);
        let (node_id, handler_id) = tree.field("모델 검색", PropertyKind::OnSubmit);
        host.dispatch(HostEvent::new(node_id, handler_id, compose_rust::EventPayload::TextSubmitted("gemini flash")))
            .expect("enter");
        assert_eq!(c.selection.get_untracked(), before);
        assert_eq!(c.open.get_untracked(), Some(Pick::Model));
    }

    fn chip_semantics(expanded: bool) -> String {
        use compose_rust::protocol::{decode_batch, Mutation};
        let mut host = Host::new(move || {
            Box::new(Recomposer::new(move || {
                let spec = ChipSpec { text: "demo-project", icon: ICON_FOLDER, tint: None, chevron: false, warn: false, width: ChipWidth::Content, expanded, name: None };
                ComposerChip(spec, || {});
            }))
        });
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        batch
            .iter()
            .find_map(|m| match m {
                Mutation::SetModifier { modifier, .. } if format!("{modifier:?}").starts_with("Semantics") => Some(format!("{modifier:?}")),
                _ => None,
            })
            .expect("the chip's semantics")
    }

    #[test]
    fn fr_l10_4_composer_chips_say_whether_their_list_is_open() {
        let _guard = crate::services::test_guard();
        assert_ne!(chip_semantics(true), chip_semantics(false), "expanded and collapsed differ");
        let src = include_str!("home.rs").split("#[cfg(test)]").next().unwrap();
        assert!(src.contains("let expanded = open.get() == Some(pick);"));
    }

    #[test]
    fn fr_l10_4_project_and_computer_chips_open_popovers_under_them() {
        let src = include_str!("home.rs").split("#[cfg(test)]").next().unwrap();
        assert!(src.contains("c.project_menu(ui, share, || chip(text, ICON_FOLDER, Pick::Project, width));"));
        assert!(src.contains("c.computer_menu(ui, share, || chip(text, ICON_PC, Pick::Computer, width));"));
        // At 560 and below the same rows are a bottom sheet, drawn without wrapping the chip.
        let lists = src.split("fn lists(").nth(1).unwrap().split("\n    fn ").next().unwrap();
        assert!(lists.contains("self.project_menu(ui, None, || {});") && lists.contains("self.computer_menu(ui, None, || {});"));
        assert!(lists.contains("self.mode_menu(&sel, None, || {});"), "the permission sheet is the same menu");
        assert!(!src.contains("OptionSheet("), "no centred option sheet is left on Home");
    }

    #[test]
    fn fr_l10_4_project_popover_lists_the_projects_with_a_check_and_a_tap_chooses() {
        let _guard = crate::services::test_guard();
        let mut host = Host::new(|| {
            Box::new(Recomposer::new(|| {
                let c = remember(|| {
                    let c = Composer::new();
                    c.open.set(Some(Pick::Project));
                    c
                });
                PICKER.with(|p| *p.borrow_mut() = Some(c.clone()));
                ComposerCard(&c, shared_ui(), composer_width(1390.0), layout(1440.0));
            }))
        });
        let mut tree = Tree::default();
        tree.read(host.rebuild().expect("rebuild"));
        let c = PICKER.with(|p| p.borrow().clone()).expect("composed");
        let options = project_options(&c.live(shared_ui()).0);
        let other = options.iter().find(|p| **p != c.selection.get_untracked().project).cloned();
        if let Some(other) = other {
            click(&mut host, &mut tree, &other);
            assert_eq!(c.selection.get_untracked().project, other);
            assert_eq!(c.open.get_untracked(), None, "choosing closes the list");
        }
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

    /// The composer card at 1440 dp with `pick` open and `mode` chosen, and the composer.
    fn picker_host(pick: Option<Pick>, mode: PermMode) -> (Host, Tree, Composer) {
        let mut host = Host::new(move || {
            Box::new(Recomposer::new(move || {
                let c = remember(|| {
                    let c = Composer::new();
                    c.open.set(pick);
                    c.selection.set(c.selection.get_untracked().with_mode(mode));
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

    fn key_down(host: &mut Host, tree: &mut Tree, key: compose_rust::Key, ctrl_key: bool, code: u32, text: &'static str) -> i64 {
        let (node_id, handler_id) = tree.field("모델 검색", PropertyKind::OnKeyDown);
        let payload = compose_rust::EventPayload::KeyDown { key, shift_key: false, ctrl_key, alt_key: false, meta_key: false, repeat: false, code, text };
        let (bytes, consumed) = host.dispatch(HostEvent::new(node_id, handler_id, payload)).expect("key");
        let bytes = bytes.to_vec();
        tree.read(&bytes);
        consumed
    }

    #[test]
    fn fr_l11_the_four_chips_share_the_line_by_weight_at_1440() {
        // #573: at 1440 the demo labels need more than the 604 dp the chips have, so each
        // picker node carries its chip's weight and the toolkit cuts the labels itself. The
        // labels reach it whole: no estimate cut them.
        let _guard = crate::services::test_guard();
        let (_host, tree, c) = picker_host(None, PermMode::Ask);
        let sel = c.selection.get_untracked();
        assert!(tree.has(&sel.option.chip_label()), "the model label is whole: {:?}", tree.sent);
        assert!(tree.has(PermMode::Ask.label()) && tree.has(&sel.project));
        let model = composer_chip_width(1, &sel.option.chip_label());
        let mode = composer_chip_width(2, PermMode::Ask.label());
        let shares: Vec<f32> = tree.weight.values().copied().filter(|w| *w > 1.0).collect();
        assert_eq!(shares.len(), 4, "one share per chip: {shares:?}");
        // A share is the chip's frame plus the part of its label the line has room for.
        assert!(shares.iter().all(|w| *w > 48.0 && *w < model), "{shares:?} against {model}");
        assert!(shares.iter().any(|w| *w > 70.0 && *w < mode), "{shares:?} against {mode}");
        // A chip with a share is zero wide, so the share and not its label decides its width.
        assert!(tree.width.values().filter(|w| **w == 0.0).count() >= 4);
    }

    #[test]
    fn fr_l10_4_the_four_pickers_are_popovers_8_below_the_composer_card() {
        // Review r8: the menus opened 8 below the chip and overlapped the composer's edge.
        let _guard = crate::services::test_guard();
        let (_host, tree, _c) = picker_host(None, PermMode::Ask);
        let popovers = tree.log.iter().filter(|m| m.starts_with("Create") && m.contains("widget: Popover")).count();
        assert_eq!(popovers, 4, "project, model, mode and computer");
        assert_eq!(tree.log.iter().filter(|m| m.starts_with("Create") && m.contains("widget: Menu")).count(), 0, "no toolkit Menu is left");
        let offsets = tree.all_props("OffsetDp");
        assert_eq!(offsets.len(), 4);
        assert!(offsets.iter().all(|m| m.contains("Float(26.0)")), "{offsets:?}");
    }

    #[test]
    fn fr_l16_the_mode_menu_reads_as_the_session_permission_menu() {
        // Review r8: the home mode popover had 13 px medium titles; the shared menu's are
        // 14/600 over a 12 px description, under the 12/600 head.
        let _guard = crate::services::test_guard();
        let (_host, tree, _c) = picker_host(Some(Pick::Mode), PermMode::Ask);
        let title = tree.text_node(PermMode::Edits.label());
        assert!(tree.props(title, "FontSize").iter().any(|m| m.contains("(14")), "{:?}", tree.props(title, "FontSize"));
        assert!(tree.props(title, "FontWeight").iter().any(|m| m.contains("(600")), "{:?}", tree.props(title, "FontWeight"));
        let head = tree.text_node("권한 모드 \u{b7} Claude Code");
        assert!(tree.props(head, "FontSize").iter().any(|m| m.contains("(12")));
        assert!(tree.props(head, "FontWeight").iter().any(|m| m.contains("(600")));
        let rows = mode_pop_rows(&mode_picker_rows_pending(Harness::ClaudeCode, PermMode::Ask, None));
        assert_eq!(rows.len(), 4);
        assert!(rows[0].chosen && !rows[0].danger && rows[0].icon == Some(ICON_SHIELD));
        assert!(rows[2].danger && rows[2].icon == Some(ICON_WARN));
        assert_eq!(rows[1].description.as_deref(), Some(PermMode::Edits.description()));
    }

    #[test]
    fn fr_l16_the_first_bypass_tap_arms_the_row_and_the_menu_stays_open() {
        // #574: the armed state. The shared menu closes on a row tap unless asked not to.
        let _guard = crate::services::test_guard();
        let (mut host, mut tree, c) = picker_host(Some(Pick::Mode), PermMode::Ask);
        click(&mut host, &mut tree, PermMode::Bypass.label());
        assert_eq!(c.open.get_untracked(), Some(Pick::Mode), "the menu is still open");
        assert_eq!(c.mode_pending.get_untracked().map(|(m, _)| m), Some(PermMode::Bypass));
        assert_eq!(c.selection.get_untracked().mode, PermMode::Ask, "nothing is applied yet");
        assert!(tree.has(crate::ui::home::BYPASS_CONFIRM_TITLE), "the row asks for the second tap: {:?}", tree.sent);
        assert!(tree.has(crate::ui::home::BYPASS_CONFIRM_TEXT));
    }

    #[test]
    fn fr_l16_pointing_at_another_row_withdraws_the_bypass_confirmation() {
        // #574: `mode_hover` is wired. Every row hears the pointer come over it.
        let _guard = crate::services::test_guard();
        let (mut host, mut tree, c) = picker_host(Some(Pick::Mode), PermMode::Ask);
        assert_eq!(tree.all_props("OnPointer").len(), modes_for(Harness::ClaudeCode).len(), "one subscription per row");
        click(&mut host, &mut tree, PermMode::Bypass.label());
        let bypass = modes_for(Harness::ClaudeCode).iter().position(|m| m.is_bypass()).unwrap();
        c.hover_mode(bypass);
        assert!(c.mode_pending.get_untracked().is_some(), "the armed row itself keeps the confirmation");
        c.hover_mode(0);
        assert_eq!(c.mode_pending.get_untracked(), None);
        tree.read(host.render_frame(1).expect("frame"));
        assert!(tree.has(PermMode::Bypass.label()), "the row reads as before: {:?}", tree.sent);
        assert_eq!(c.open.get_untracked(), Some(Pick::Mode));
    }

    #[test]
    fn fr_l16_bypass_draws_the_mode_chip_in_the_warning_state() {
        // #574, #639: red on `red-soft` with a red chevron, open or not; a plain chip turns to
        // `hover` with `text` while its list is open (`.pickchip[aria-expanded=true]`).
        let p = ember_theme().palette;
        assert_eq!(chip_colors(true, false, p), (p.red.soft, p.red.strong, p.red.strong));
        assert_eq!(chip_colors(true, true, p), (p.red.soft, p.red.strong, p.red.strong));
        assert_eq!(chip_colors(false, false, p), (p.surface_2, p.text_2, p.muted));
        assert_eq!(chip_colors(false, true, p), (p.hover, p.text, p.muted));
        // The warning chip is the same chip as the others, so it carries the chevron, the open
        // state and the name. Its label is on the wire twice: the chip and the picker's row.
        let _guard = crate::services::test_guard();
        let (_host, tree, _c) = picker_host(None, PermMode::Bypass);
        assert_eq!(tree.text.values().filter(|t| t.as_str() == PermMode::Bypass.label()).count(), 2, "{:?}", tree.sent);
        let src = include_str!("home.rs").split("#[cfg(test)]").next().unwrap();
        assert!(src.contains("icon: if warn { ICON_WARN } else { icon },"));
        assert!(src.contains("let warn = pick == Pick::Mode && sel.mode.is_bypass();"));
        assert!(!src.contains("fn WarnChip("), "no second chip implementation is left");
    }

    #[test]
    fn fr_l11_icon_only_chips_have_names() {
        // #639: at 560 and below three chips are circles with no text.
        assert_eq!(chip_name(Pick::Mode, PermMode::Bypass.label()), "권한 모드: 바이패스(모두 허용)");
        assert_eq!(chip_name(Pick::Model, "Claude Code \u{b7} Opus 5.5 \u{b7} 높음"), "하네스와 모델: Claude Code \u{b7} Opus 5.5 \u{b7} 높음");
        // A project or computer name is data, so those names say only what the chip is.
        assert_eq!(chip_name(Pick::Project, "demo"), "프로젝트");
        assert_eq!(chip_name(Pick::Computer, "스튜디오"), "컴퓨터");
        let semantics = |name: Option<String>| {
            let mut host = Host::new(move || {
                let name = name.clone();
                Box::new(Recomposer::new(move || {
                    let spec = ChipSpec { text: "x", icon: ICON_PC, tint: None, chevron: false, warn: false, width: ChipWidth::Circle, expanded: false, name: name.clone() };
                    ComposerChip(spec, || {});
                }))
            });
            let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
            batch.iter().filter(|m| matches!(m, Mutation::SetModifier { .. })).map(|m| format!("{m:?}")).filter(|m| m.contains("Semantics")).collect::<Vec<_>>()
        };
        assert_ne!(semantics(Some("컴퓨터".to_string())), semantics(None), "the name reaches the toolkit");
    }

    #[test]
    fn fr_l15_the_model_popover_is_400_wide_with_the_shared_row_values() {
        // #849, review r8: 400 wide with the 52 dp rail (it was 560 and 128), rows as the
        // shared menu's (56 high, 14/600 titles), the shortcut in a 52 dp key cap.
        let _guard = crate::services::test_guard();
        let (_host, tree, _c) = model_picker_host();
        assert_eq!((MODEL_POPOVER_WIDTH, MODEL_RAIL_WIDTH), (400.0, 52.0));
        assert_eq!(popover_width(MODEL_POPOVER_WIDTH, 1440.0), 400.0);
        assert_eq!(popover_width(MODEL_POPOVER_WIDTH, 561.0), 400.0, "it fits beside the rail at every popover width");
        // The box is as wide as the window in this test allows (no window size is reported).
        let boxed = popover_width(MODEL_POPOVER_WIDTH, crate::ui::window_size::current_window_size().width_dp);
        assert!(tree.width.values().any(|w| *w == boxed), "the box takes the popover width");
        assert!(!tree.width.values().any(|w| *w == 560.0 || *w == 128.0));
        assert!(tree.width.values().any(|w| *w == MODEL_RAIL_WIDTH - 1.0), "the rail, beside its 1 dp line");
        assert_eq!(pop_metrics(PopKind::Mode).row_min_h, 56.0);
        let title = tree.text_node("Opus 5.5 (보통)");
        assert!(tree.props(title, "FontSize").iter().any(|m| m.contains("14")));
        assert!(tree.props(title, "FontWeight").iter().any(|m| m.contains("600")));
        let cap = tree.text_node("Ctrl+2");
        assert_eq!(tree.width.get(tree.parent.get(&cap).unwrap()), Some(&MODEL_KEY_CAP_W));
        let p = ember_theme().palette;
        assert_eq!(model_row_fill(true, true, p), Some(p.sel));
        assert_eq!(model_row_fill(false, true, p), Some(p.hover));
        assert_eq!(model_row_fill(false, false, p), None);
        // The key cap is `surface-2` in both themes, which the mockup's dark tokens match
        // (#252523 on the #1f1f1d surface), so #849's "no visible fill" needed no token change.
        assert_eq!((DARK.surface_2, DARK.surface), (Color::rgb(0x252523), Color::rgb(0x1f1f1d)));
    }

    #[test]
    fn fr_l15_arrow_keys_move_the_active_row_and_enter_takes_it() {
        // #574: the keys in the search field.
        let _guard = crate::services::test_guard();
        let (mut host, mut tree, c) = model_picker_host();
        assert_eq!(key_down(&mut host, &mut tree, compose_rust::Key::ArrowDown, false, 0x51, "ArrowDown"), 1, "the field consumes the arrow");
        assert_eq!(c.model_act.get_untracked(), Some(1));
        key_down(&mut host, &mut tree, compose_rust::Key::ArrowDown, false, 0x51, "ArrowDown");
        key_down(&mut host, &mut tree, compose_rust::Key::ArrowUp, false, 0x52, "ArrowUp");
        assert_eq!(c.model_act.get_untracked(), Some(1));
        assert_eq!(key_down(&mut host, &mut tree, compose_rust::Key::Other, false, 0x04, "a"), 0, "a letter stays the field's");
        // The active row is drawn on `hover` (`.mp-row.act`); the fill is `model_row_fill`'s.
        let (node_id, handler_id) = tree.field("모델 검색", PropertyKind::OnSubmit);
        host.dispatch(HostEvent::new(node_id, handler_id, compose_rust::EventPayload::TextSubmitted(""))).expect("enter");
        let sel = c.selection.get_untracked();
        assert_eq!((sel.option.model, sel.option.effort), ("opus", Some("Medium")), "the second row, Opus 5.5 (보통)");
        assert_eq!(c.open.get_untracked(), None);
        assert_eq!(c.model_act.get_untracked(), None, "the next opening starts on the chosen row");
    }

    /// The last `Semantics` modifier sent for `node`, as its debug text.
    fn semantics_of(tree: &Tree, node: u32) -> Option<String> {
        let id = format!("node_id: {node},");
        tree.log.iter().rev().find(|m| m.starts_with("SetModifier") && m.contains(&id) && m.contains("Semantics")).cloned()
    }

    /// The model rows on the wire, by title: the semantics (role and state) of each row's pressable node.
    fn model_row_semantics(tree: &Tree, titles: &[&str]) -> Vec<String> {
        titles
            .iter()
            .map(|t| {
                let (node, _) = tree.click_target(t);
                let m = semantics_of(tree, node).unwrap_or_else(|| panic!("{t} row has no semantics"));
                // Only the role and the state, not the node id.
                m.split("modifier: ").nth(1).unwrap_or(&m).to_string()
            })
            .collect()
    }

    const OPUS_ROWS: [&str; 3] = ["Opus 5.5 (높음)", "Opus 5.5 (보통)", "Opus 5.5 (낮음)"];

    #[test]
    fn fr_l15_the_model_popover_is_a_searchable_list_not_a_menu() {
        // [leader, 2026-11-21], #888: the search and a list of options, so the toolkit's menu
        // keyboard (compose-rust#810) does not take the popover over. No combobox, listbox or
        // option role exists in the toolkit, so the rows are list items in a list named 모델,
        // and the selected one is the active row Ember draws (the chosen one when it opens).
        let _guard = crate::services::test_guard();
        let (_host, tree, _c) = model_picker_host();
        // The picker's box is the node named 하네스와 모델 선택; nothing in it is a menu.
        let node_of = |m: &String| m.split("node_id: ").nth(1).and_then(|r| r.split(|ch: char| !ch.is_ascii_digit()).next()).and_then(|d| d.parse::<u32>().ok());
        let named: Vec<u32> = tree.log.iter().filter(|m| m.starts_with("Set") && m.contains(super::super::a11y::MODEL_PICKER)).filter_map(node_of).collect();
        let (first, _) = tree.click_target(OPUS_ROWS[0]);
        let picker = {
            let mut at = Some(first);
            loop {
                match at {
                    Some(n) if named.contains(&n) => break n,
                    Some(n) => at = tree.parent.get(&n).copied(),
                    None => panic!("the rows are in no box named {}", super::super::a11y::MODEL_PICKER),
                }
            }
        };
        let inside = |mut n: u32| loop {
            if n == picker {
                return true;
            }
            match tree.parent.get(&n) {
                Some(p) => n = *p,
                None => return false,
            }
        };
        let menu_like: Vec<&String> = tree
            .log
            .iter()
            .filter(|m| m.starts_with("SetModifier") && m.contains("Semantics") && (m.contains("MenuItem") || m.contains("role: Menu,")))
            .filter(|m| node_of(m).is_some_and(inside))
            .collect();
        assert!(menu_like.is_empty(), "no menu or menu item in the picker: {menu_like:?}");
        let rows = model_row_semantics(&tree, &OPUS_ROWS);
        assert!(rows.iter().all(|s| s.contains("role: ListItem,")), "{rows:?}");
        assert_ne!(rows[0], rows[1], "the active row (the chosen 높음) is selected");
        assert_eq!(rows[1], rows[2], "the others are not: {rows:?}");
        // The rows sit in one list named 모델.
        let (row, _) = tree.click_target(OPUS_ROWS[0]);
        let mut at = tree.parent.get(&row).copied();
        let mut list = None;
        while let Some(id) = at {
            if semantics_of(&tree, id).is_some_and(|s| s.contains("role: List,")) {
                list = Some(id);
                break;
            }
            at = tree.parent.get(&id).copied();
        }
        let list = list.expect("the rows are in a list");
        assert!(tree.log.iter().any(|m| m.contains(&format!("node_id: {list},")) && m.contains(super::super::a11y::MODEL_LIST)), "the list is named 모델");
        // The search is a text field, the closest the toolkit has to a combobox.
        let (field, _) = tree.field("모델 검색", PropertyKind::OnKeyDown);
        assert!(!semantics_of(&tree, field).is_some_and(|s| s.contains("Menu")), "the field claims no menu role");
    }

    #[test]
    fn fr_l15_the_model_popover_opens_in_the_search_and_enter_takes_the_drawn_row() {
        // [leader, 2026-11-21], #888: the search has the focus when the picker opens (the
        // mockup's `open()` focuses `[data-mpq]`), the arrows move the drawn active row, which
        // is the selected one, and Enter takes that row.
        let _guard = crate::services::test_guard();
        let (mut host, mut tree, c) = model_picker_host();
        tree.read(host.render_frame(1).expect("frame"));
        let (field, _) = tree.field("모델 검색", PropertyKind::OnKeyDown);
        assert!(tree.log.iter().any(|m| m == &format!("RequestFocus {{ node_id: {field} }}")), "the search asks for the focus: {:?}", tree.log.iter().filter(|m| m.starts_with("RequestFocus")).collect::<Vec<_>>());
        key_down(&mut host, &mut tree, compose_rust::Key::ArrowDown, false, 0x51, "ArrowDown");
        let rows = model_row_semantics(&tree, &OPUS_ROWS);
        assert_ne!(rows[1], rows[0], "the drawn active row moved to 보통 and is the selected one");
        assert_eq!(rows[0], rows[2], "{rows:?}");
        let (node_id, handler_id) = tree.field("모델 검색", PropertyKind::OnSubmit);
        host.dispatch(HostEvent::new(node_id, handler_id, compose_rust::EventPayload::TextSubmitted(""))).expect("enter");
        let sel = c.selection.get_untracked();
        assert_eq!((sel.option.model, sel.option.effort), ("opus", Some("Medium")), "Enter took the drawn row, Opus 5.5 (보통)");
        assert_eq!(c.open.get_untracked(), None);
    }

    #[test]
    fn fr_l15_ctrl_number_chooses_the_row_with_that_key_cap() {
        let _guard = crate::services::test_guard();
        let (mut host, mut tree, c) = model_picker_host();
        assert!(tree.has("Ctrl+3"));
        assert_eq!(key_down(&mut host, &mut tree, compose_rust::Key::Other, true, 0x20, "3"), 1);
        let sel = c.selection.get_untracked();
        assert_eq!((sel.option.model, sel.option.effort), ("opus", Some("Low")), "the third row, Opus 5.5 (낮음)");
        assert_eq!(c.open.get_untracked(), None);
    }

    #[test]
    fn fr_l23_the_trend_draws_the_cost_line_its_area_and_the_last_point_once_a_cost_arrives() {
        // #849: what the plot hands the toolkit. Four days, three of them priced.
        use compose_rust::DrawCommand;
        let p = ember_theme().palette;
        let values = [10_u64, 20, 30, 40];
        let ink = |list: &DrawList| {
            let cmds = list.decode().expect("decode");
            let line = cmds.iter().filter(|c| matches!(c, DrawCommand::Line { paint: pt, stroke_width, .. } if *pt == paint(p.ink) && *stroke_width == 2.0)).count();
            let area = cmds.iter().filter(|c| matches!(c, DrawCommand::Line { paint: pt, .. } if *pt == cost_area_paint(p))).count();
            let ring = cmds.iter().filter(|c| matches!(c, DrawCommand::Circle { .. })).count();
            (line, area, ring)
        };
        let none = trend_draw_list(&values, &[None, None, None, None], 400.0, p);
        assert_eq!(ink(&none), (0, 0, 0), "no cost, no line");
        let priced = trend_draw_list(&values, &[Some(1.0), None, Some(3.0), Some(2.0)], 400.0, p);
        let (line, area, ring) = ink(&priced);
        assert_eq!(line, 2, "one segment between each pair of costed days");
        assert_eq!(ring, 2, "the surface disc and the ink ring of `.hm-pt`");
        // The area spans the first to the last costed day, which are three bar pitches apart
        // of the four that fill 400 dp, in 2 dp columns.
        assert!((140..=160).contains(&area), "{area} columns");
        // The area is `ink` at 5 %, not opaque.
        assert_eq!(cost_area_paint(p), Paint::Literal(Color::argb(0x0D00_0000 | (p.ink.to_argb() & 0x00ff_ffff))));
        let one = trend_draw_list(&values, &[None, Some(1.0), None, None], 400.0, p);
        assert_eq!(ink(&one), (0, 0, 2), "a single costed day is a point");
    }

    #[test]
    fn fr_l11_home_shows_the_greeting_the_composer_and_the_dashboard() {
        use compose_rust::{EventPayload, WindowHeightClass, WindowSizeClass};
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| Box::new(Recomposer::new(HomeScreen)));
        let first = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let mut all = texts(&first);
        let event = HostEvent::new(0, 0, EventPayload::WindowSizeChanged {
                width_dp: 1440.0,
                height_dp: 900.0,
                class: WindowSizeClass::Expanded,
                height_class: WindowHeightClass::Medium,
                safe_area_top: 0.0,
                safe_area_leading: 0.0,
                safe_area_trailing: 0.0,
                safe_area_bottom: 0.0,
            });
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

/// Design review 2026-11-13 (B8, B9, B11): the home screen against the mockup.
#[cfg(test)]
mod design_review_tests {
    use super::*;

    /// This file without this module, so a source check never finds its own literal.
    fn source() -> &'static str {
        let src = include_str!("home.rs");
        &src[..src.find("mod design_review_tests {").unwrap()]
    }

    fn kpi(value: &str, unit: &'static str) -> Kpi {
        Kpi { label: "x", value: value.to_string(), unit, note: None, available: true }
    }

    #[test]
    fn design_b9_kpi_unit_and_denominator_are_the_small_muted_part() {
        assert_eq!(crate::ui::home::kpi_value_parts(&kpi("4", "명")), ("4".to_string(), "명".to_string()));
        assert_eq!(crate::ui::home::kpi_value_parts(&kpi("3/5", "")), ("3".to_string(), "/5".to_string()));
        assert_eq!(crate::ui::home::kpi_value_parts(&kpi("87", "%")), ("87".to_string(), "%".to_string()));
        assert_eq!(KPI_SMALL, (Fs::Md, Weight::Medium));
        assert_eq!(KPI_SMALL_GAP, 2.0);
        let src = source();
        assert!(src.contains("Label(&small, KPI_SMALL.0, KPI_SMALL.1, Some(paint(p.muted)), Some(1));"));
    }

    #[test]
    fn design_b8_greeting_is_32_500() {
        assert_eq!(GREETING_WEIGHT, Weight::Medium);
        assert!(source().contains("Label(&greeting, if l.compact { Fs::Xl2 } else { Fs::Xl3 }, GREETING_WEIGHT, None, Some(2));"));
    }

    #[test]
    fn design_b11_suggestions_are_spark_branch_chart_on_every_line() {
        assert_eq!(suggestion_icon(0), ICON_SPARK);
        assert_eq!(suggestion_icon(1), ICON_GIT);
        assert_eq!(suggestion_icon(2), ICON_CHART_NAV);
        assert!(source().contains("suggestion_icon(row * n + i)"), "the index restarts on every line");
    }

    #[test]
    fn design_b11_harness_chip_shows_the_harness_glyph_in_its_colour() {
        for h in [Harness::ClaudeCode, Harness::Codex, Harness::Gemini] {
            let (glyph, tint) = composer_chip_icon(Pick::Model, ICON_TERMINAL, h);
            assert_eq!(glyph, harness_glyph(h));
            assert!(tint.is_some());
        }
        assert_eq!(composer_chip_icon(Pick::Project, ICON_FOLDER, Harness::Codex), (ICON_FOLDER, None));
    }

    #[test]
    fn fr_l15_model_popover_has_a_fixed_height_not_an_equal_height_row() {
        // #849: an `equal_height` row counted the list (a scrolling child) as 0 high, so the
        // popover took the rail's height and showed one row. The popover keeps a fixed height.
        let src = include_str!("home.rs");
        let body = &src[src.find("fn model_popover").expect("model_popover")..src.find("fn RailEntry").expect("RailEntry")];
        assert!(!body.contains(concat!("equal_height", "(true)")));
        assert!(body.contains("model_popover_height("));
    }

    #[test]
    fn fr_l23_range_toggle_chosen_is_ink_and_the_other_outlined() {
        let p = ember_theme().palette;
        assert_eq!(range_toggle_colors(true, &p), (p.ink, p.ink, p.ink_text));
        assert_eq!(range_toggle_colors(false, &p), (p.surface, p.line, p.text_2));
    }

    #[test]
    fn fr_l23_trend_and_status_heads_use_the_mockup_icons() {
        // Review 2026-11-14 H3 and H6 (#849): bar chart on the trend, the group on 팀원 상태, and
        // the up-right arrow on the go button.
        let src = include_str!("home.rs");
        assert!(src.contains(concat!("CardHead(ICON_CHART_NAV, ", "\"비용과 토큰 추이\"")));
        assert!(src.contains(concat!("CardHead(ICON_TEAM, ", "\"팀원 상태\"")));
        assert!(ICON_ARROW.contains("m4.5 19.5 15-15"));
    }

    /// #327: the home weekly usage chart skips sessions belonging to archived projects.
    #[test]
    fn fr_l26_weekday_usage_skips_archived_projects() {
        let src = include_str!("home.rs");
        let start = src.find("pub fn weekday_usage(").expect("weekday_usage");
        let body = &src[start..start + 900];
        assert!(body.contains("crate::ui::project::project_archived(state, &p.name)"), "{body}");
    }
}
