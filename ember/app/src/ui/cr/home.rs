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
use compose_rust::{Alignment, ButtonVariant, DrawList, IconRole, ShapeRole};

pub use crate::ui::home::{
    apply_suggestion, can_send, computers_line, default_mode, effective_message, filter_models,
    harness_block, kpis, mode_after_harness_change, model_catalog, modes_for, needs_approve_all,
    project_options, project_tile_line, resolve_cwd, shortcut_label, split_chip_rows,
    success_percent, title_from_message, CrewMember, Dash, Harness, HarnessBar, Kpi, ModelOption,
    PermMode, ProjectTile, Selection, StatusCounts, COMPOSER_PLACEHOLDER, DEFAULT_PROJECT,
    EMPTY_CREW, EMPTY_HARNESS_USAGE, EMPTY_PROJECTS, GREETING, SUGGESTIONS, UNAVAILABLE,
};
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
use crate::ui::shell::{NavMenu, ShellMode, DEMO_USER};

type Part<'a> = Box<dyn FnOnce() + 'a>;

/// The width of the option popups of the composer's chips.
const POPUP_WIDTH: f32 = 280.0;

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

/// Estimated text width in dp: Hangul and other wide glyphs one em, ASCII 0.55 em.
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

/// Widths for chips whose natural widths do not all fit in `avail`: every chip gives up the same
/// share of its width (the mockup's `flex: 0 1 auto` chips shrink in proportion to their basis),
/// never below `min`.
pub fn shrink_widths(natural: &[f32], avail: f32, min: f32) -> Vec<f32> {
    let total: f32 = natural.iter().sum();
    if total <= avail || total <= 0.0 {
        return natural.to_vec();
    }
    let k = avail / total;
    natural.iter().map(|w| (w * k).max(min.min(*w))).collect()
}

/// A composer chip's natural width: the toolkit button's own padding and icon slot around the
/// text (`label_min_width`), at the 13 px chip size.
pub fn chip_natural(text: &str) -> f32 {
    super::components::label_min_width(text, Fs::Sm.size(), true)
}

/// The text a chip of width `w` can show.
pub fn chip_text(text: &str, w: f32) -> String {
    fit_label(text, (w - (chip_natural("") )).max(0.0), Fs::Sm.size())
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
fn content_width(window_dp: f32, pad: f32) -> f32 {
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

fn ring_image(size: f32, stroke: f32, parts: &[(u32, f32)]) {
    let p = ember_theme().palette;
    let doc = ring_svg(size, stroke, p.track.to_argb(), parts);
    cu::Image(compose_rust::asset(compose_rust::AssetKind::Svg, shell::leak_doc(doc)))
        .modifier(Modifier::new().size(size, size));
}

/// `n` equal columns of `items`, `gap` apart; the last line is padded with empty columns.
fn Cols(n: usize, gap: f32, items: Vec<Part<'_>>) {
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
fn Tile(content: impl FnOnce()) {
    super::components::EmberCard(false, true, None, None, None, || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(content);
    });
}

/// `.hm-ch`: the 30 px outlined icon, the title, and what goes at the right.
fn CardHead(icon: &'static str, title: &str, trailing: impl FnOnce()) {
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

fn Dot(color: compose_rust::Color) {
    cu::Spacer().modifier(Modifier::new().size(8.0, 8.0).background(paint(color)).shape_role(ShapeRole::Full));
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

/// `EMBER_CR_HOME_PICK=project|model|mode|computer` opens one list at the start, and
/// `EMBER_CR_HOME_SUGGEST=0..2` takes a suggestion, for review captures.
fn initial_pick() -> Option<Pick> {
    match std::env::var("EMBER_CR_HOME_PICK").ok().as_deref() {
        Some("project") => Some(Pick::Project),
        Some("model") => Some(Pick::Model),
        Some("mode") => Some(Pick::Mode),
        Some("computer") => Some(Pick::Computer),
        _ => None,
    }
}

fn initial_suggestion() -> Option<String> {
    let i = std::env::var("EMBER_CR_HOME_SUGGEST").ok()?.parse::<usize>().ok()?;
    SUGGESTIONS.get(i).map(|s| s.to_string())
}

/// `EMBER_CR_SCROLL=<dp|bottom>` for review captures: the toolkit cannot scroll a column from
/// code (TODO(compose-rust#385)), so the blocks that would have scrolled out (by their estimated heights) are left out.
struct Skip(std::cell::Cell<f32>);

impl Skip {
    fn from_env(total_est: f32, window_h: f32) -> Skip {
        let v = match std::env::var("EMBER_CR_SCROLL").ok().as_deref() {
            Some("bottom") => (total_est - window_h).max(0.0),
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
    harness_filter: MutableState<Option<Harness>>,
    query: MutableState<String>,
}

impl Composer {
    fn new() -> Self {
        Composer {
            selection: mutable_state_of(Selection::initial()),
            typed: mutable_state_of(String::new()),
            suggestion: mutable_state_of(initial_suggestion()),
            busy: mutable_state_of(false),
            open: mutable_state_of(initial_pick()),
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
        let modes = modes_for(sel.option.harness);
        let mode_labels: Vec<String> = modes.iter().map(|m| m.label().to_string()).collect();
        {
            let selection = self.selection.clone();
            OptionSheet(
                open == Some(Pick::Mode),
                &mode_labels,
                modes.iter().position(|m| *m == sel.mode),
                POPUP_WIDTH,
                close.clone(),
                move |i| {
                    if let Some(m) = modes.get(i) {
                        selection.set(selection.get_untracked().with_mode(*m));
                    }
                },
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
        self.model_sheet(ui, &sel, open == Some(Pick::Model), close);
    }

    /// The model picker: harness filter, search and the rows with their reasons (FR-L15).
    fn model_sheet(&self, ui: &SharedUi, sel: &Selection, open: bool, close: impl FnMut() + Clone + 'static) {
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
        let filter = self.harness_filter.get();
        let query = self.query.get();
        let rows = filter_models(&all, filter, &query);
        EmberSheet("모델 선택", open, close.clone(), None, || {
            cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width().padding(SP[3])).content(|| {
                cu::ScrollRow().modifier(Modifier::new().fill_max_width().height(H_CTL + SP[0])).content(|| {
                    let f = self.harness_filter.clone();
                    FilterChip("전체", filter.is_none(), move || f.set(None));
                    for h in Harness::ALL {
                        cu::Spacer().modifier(Modifier::new().width(SP[0]));
                        let f = self.harness_filter.clone();
                        FilterChip(h.label(), filter == Some(h), move || f.set(Some(h)));
                    }
                });
                let q = self.query.clone();
                EmberField(FieldOptions {
                    fill_max_width: true,
                    placeholder: "모델 검색".to_string(),
                    on_value_change: Some(Box::new(move |v| q.set(v))),
                    ..Default::default()
                });
                if rows.is_empty() {
                    Label("맞는 모델이 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
                }
                cu::ScrollColumn().modifier(Modifier::new().fill_max_width().height(320.0)).content(|| {
                    cu::Column().spacing(SP[0]).modifier(Modifier::new().fill_max_width()).content(|| {
                        for (i, o) in rows.iter().copied().enumerate() {
                            let block = harness_block(o.harness, &agents, &limited);
                            let (selection, open, mut dismiss) = (self.selection.clone(), self.open.clone(), close.clone());
                            let fill = if o == sel.option { paint(p.sel) } else { paint(compose_rust::Color::argb(0)) };
                            let m = Modifier::new().fill_max_width().height(40.0).background(fill).corner_radius(CORNER_SM);
                            // A row: the name at the left, the harness and the shortcut in columns.
                            // TODO(compose-rust#372): a pointer-up `Clickable`.
                            Clickable(
                                if block.is_some() { m.alpha(0.5) } else { m },
                                Alignment::CenterStart,
                                move || {
                                    if block.is_none() {
                                        selection.set(selection.get_untracked().with_option(o));
                                        open.set(None);
                                        dismiss();
                                    }
                                },
                                || {
                                    cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                                        cu::Spacer().modifier(Modifier::new().width(SP[2]));
                                        Label(&o.row_title(), Fs::Md, Weight::Medium, None, Some(1));
                                        cu::Spacer().modifier(Modifier::new().weight(1.0));
                                        Label(o.harness.label(), Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
                                        cu::Spacer().modifier(Modifier::new().width(SP[2]));
                                        cu::Row().modifier(Modifier::new().width(56.0)).content(|| {
                                            if let Some(k) = shortcut_label(i) {
                                                Label(&k, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                                            }
                                        });
                                        cu::Spacer().modifier(Modifier::new().width(SP[2]));
                                    });
                                },
                            );
                            if let Some(reason) = block {
                                Label(reason, Fs::Xs, Weight::Regular, Some(paint(p.red.strong)), None);
                            }
                        }
                    });
                });
            });
        });
    }
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
            Label("오늘의 팀", Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
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

/// A composer chip (`.pickchip`): the toolkit button on a `surface-2` pill with its glyph and the
/// text cut to the width `w` the row gave it; `icon_only` chips are 32 px circles.
/// TODO(compose-rust#372, #373): the mockup's folder, star, shield and monitor line icons and the
/// chevron of the model and mode chips need a button that takes an SVG icon; the toolkit glyph
/// roles stand in.
fn ComposerChip(text: &str, icon: IconRole, selected: bool, icon_only: bool, w: f32, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let fill = if selected { p.sel } else { p.surface_2 };
    let (shown, width) = if icon_only { (String::new(), H_CTL) } else { (chip_text(text, w), w) };
    cu::Button(shown)
        .variant(ButtonVariant::Text)
        .color(paint(p.text_2))
        .icon(icon)
        .modifier(Modifier::new().width(width).height(H_CTL).background(paint(fill)).shape_role(ShapeRole::Full))
        .on_click(on_click);
}

fn ComposerCard(c: &Composer, ui: &SharedUi, w: f32, l: Layout) {
    let p = ember_theme().palette;
    let sel = c.selection.get();
    let (_, computers) = c.live(ui);
    let message = effective_message(&c.typed.get(), c.suggestion.get().as_deref());
    let sendable = can_send(&message, c.busy.get());
    let placeholder = c.suggestion.get().unwrap_or_else(|| COMPOSER_PLACEHOLDER.to_string());
    let computer_text = crate::ui::home::computer_chip_text(sel.computer.as_deref(), &computers);
    let computer_text = if sel.computer.is_none() { c.local_name(ui) } else { computer_text };
    let icon_only = l.compact;
    let pad = if l.compact { SP[2] } else { SP[3] };
    let gap = if l.compact { SP[0] } else { 6.0 };
    let inner = w - 2.0 * pad - 2.0;
    let model_text = sel.option.chip_label();
    let mode_text = sel.mode.label().to_string();
    // The row: add (32), the chips, a spacer, send (36), `gap` between all of them. The chips
    // share what is left and are cut to it.
    let (widths, project_w): (Vec<f32>, f32) = if icon_only {
        let project_avail = (inner - 32.0 - 3.0 * H_CTL - 36.0 - 6.0 * gap).max(60.0);
        let project_nat = chip_natural(&fit_label(&sel.project, 110.0, Fs::Sm.size()));
        (vec![], project_nat.min(project_avail))
    } else {
        let avail = inner - 32.0 - 36.0 - 6.0 * gap;
        let nat = [
            chip_natural(&fit_label(&sel.project, 180.0, Fs::Sm.size())),
            chip_natural(&fit_label(&model_text, 180.0, Fs::Sm.size())),
            chip_natural(&fit_label(&mode_text, 180.0, Fs::Sm.size())),
            chip_natural(&fit_label(&computer_text, 180.0, Fs::Sm.size())),
        ];
        let w4 = shrink_widths(&nat, avail, 56.0);
        (w4.clone(), w4[0])
    };
    let width_of = |i: usize| widths.get(i).copied().unwrap_or(0.0);
    let chip = |text: &str, icon: IconRole, pick: Pick, selected: bool, w: f32| {
        let open = c.open.clone();
        ComposerChip(text, icon, selected, icon_only && pick != Pick::Project, w, move || open.set(Some(pick)));
    };
    let add = || {
        // TODO(FR-L11): attachments need the file picker and an upload API.
        super::components::IconButton(IconRole::Add, true, || notice::post(UNAVAILABLE));
    };
    let send = || {
        let (c2, ui2) = (c.clone(), ui.clone());
        // `.sendround`: a 36 px `--ink` circle with the send glyph.
        // TODO(compose-rust#372): the toolkit glyph is not the mockup's paper plane.
        // TODO(compose-rust#387): a disabled button turns grey instead of keeping `--ink`.
        cu::Button("")
            .icon(IconRole::Send)
            .enabled(sendable)
            .variant(ButtonVariant::Text)
            .color(paint(p.ink_text))
            .modifier(Modifier::new().size(36.0, 36.0).background(paint(p.ink)).shape_role(ShapeRole::Full))
            .on_click(move || c2.send(&ui2));
    };

    // `.bigcomposer`: surface, 1 px line, the xl radius, 16 / 16 / 12 padding.
    cu::Box()
        .modifier(
            Modifier::new()
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
                    add();
                    chip(&sel.project, IconRole::Library, Pick::Project, false, if icon_only { project_w } else { width_of(0) });
                    chip(&model_text, IconRole::Compose, Pick::Model, false, width_of(1));
                    chip(&mode_text, IconRole::Settings, Pick::Mode, sel.mode.is_bypass(), width_of(2));
                    chip(&computer_text, IconRole::Sidebar, Pick::Computer, false, width_of(3));
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                    if c.busy.get() {
                        cu::IndeterminateProgressIndicator().circular(true).modifier(Modifier::new().size(18.0, 18.0));
                    }
                    send();
                });
            });
        });
}

/// `.sugg`: outlined chips, three a line (two up to 900 px), centred.
fn Suggestions(c: &Composer, width_dp: f32) {
    let n = suggestion_cols(width_dp);
    cu::Column().alignment(Alignment::Center).spacing(SP[1]).content(|| {
        for line in SUGGESTIONS.chunks(n) {
            cu::Row().spacing(SP[1]).content(|| {
                for text in line {
                    let s = c.suggestion.clone();
                    let t = apply_suggestion(&c.typed.get_untracked(), text);
                    // TODO(compose-rust#372): the mockup's sparkle, branch and bars icons.
                    let shown = fit_label(text, 240.0 - 52.0, Fs::Sm.size());
                    AppButton(&shown, None, ButtonKind::Default, true, true, false, move || s.set(Some(t.clone())));
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
                Label("오늘의 팀", if l.compact { Fs::Xl } else { Fs::Xl2 }, Weight::Semibold, None, None);
                cu::Spacer().modifier(Modifier::new().height(10.0));
                Label(&format!("{} · 서버 기준", date_line(&crate::ui::home::today_utc())), Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
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
        if skip.show(EST_USAGE) {
            Weighted(stacked, GAP_CARD, vec![
                (7.0, Box::new(|| HarnessTile(&d.harness, &week, ui))),
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
                (1.0, Box::new(|| Unavailable(ICON_CLOCK, "승인 대기 시간"))),
                (1.0, Box::new(|| Unavailable(shell::ICON_CPU_SVG, "컴퓨터 부하"))),
            ]);
        } else {
            Weighted(!l.pair_mid, GAP_CARD, vec![
                (1.0, Box::new(|| SuccessTile(&d.counts)) as Part),
                (1.0, Box::new(|| Unavailable(ICON_CLOCK, "승인 대기 시간"))),
            ]);
            Unavailable(shell::ICON_CPU_SVG, "컴퓨터 부하");
        }
        ProjectsTile(&d.projects, ui, l);
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
    let p = ember_theme().palette;
    Tile(|| {
        CardHead(icon, title, || {});
        Label(UNAVAILABLE, Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
    });
}

/// Token bars per day for the ChatGPT plan accounts, 7 or 14 days (FR-U7). Cost and the other
/// harnesses are not reported yet and say so.
/// The hatched bars (Ember #190) are a `Canvas` at the card's width; the cost line needs a path
/// and the cost is not reported yet.
fn TokenTrendTile(ui: &SharedUi, chart_w: f32) {
    let p = ember_theme().palette;
    let days = remember(|| mutable_state_of(7usize));
    let rows = ui.live.usage.get();
    let ids: Vec<String> = ui.live.chatgpt.get().iter().map(|a| a.id.clone()).collect();
    if ids.is_empty() {
        return Unavailable(shell::ICON_GAUGE_SVG, "비용과 토큰 추이");
    }
    let series = crate::ui::home::chatgpt_token_days(&rows, &ids, &crate::ui::home::today_utc(), days.get());
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
            for (x1, y1, x2, y2) in crate::ui::home::hatch_lines(x, y, w, h, 5.0) {
                list = list.line(color, x1, y1, x2, y2, 1.0);
            }
        }
    }
    let commands = list.build();
    let total: u64 = values.iter().sum();
    let first = series.first().map(|(d, _)| d.clone()).unwrap_or_default();
    let last = series.last().map(|(d, _)| d.clone()).unwrap_or_default();
    Tile(|| {
        CardHead(shell::ICON_GAUGE_SVG, "비용과 토큰 추이", || {
            for n in [7usize, 14] {
                let d = days.clone();
                AppButton(&format!("{n}일"), None, if days.get() == n { ButtonKind::Primary } else { ButtonKind::Default }, true, true, false, move || d.set(n));
                cu::Spacer().modifier(Modifier::new().width(SP[0]));
            }
        });
        cu::Canvas(commands).modifier(Modifier::new().size(chart_w, H + 1.0));
        cu::Row().modifier(Modifier::new().fill_max_width()).content(|| {
            Label(&first, Fs::Xs2, Weight::Regular, Some(paint(p.muted)), Some(1));
            cu::Spacer().modifier(Modifier::new().weight(1.0));
            Label(&last, Fs::Xs2, Weight::Regular, Some(paint(p.muted)), Some(1));
        });
        Label(&format!("ChatGPT 토큰 합계 {}", crate::ui::agents::format_tokens(total)), Fs::Sm, Weight::Regular, None, None);
        Label(&format!("비용과 다른 하네스의 토큰은 {UNAVAILABLE}"), Fs::Xs2, Weight::Regular, Some(paint(p.muted)), Some(2));
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

/// 하네스별 사용량 (`.hm-usage`): the day's and the week's sessions, a legend, weekday stacked
/// bars per harness, one ring per harness with its share, and a "자세히" button to 에이전트.
fn HarnessTile(bars: &[HarnessBar], week: &[WeekDay], ui: &SharedUi) {
    let p = ember_theme().palette;
    let colors = [p.orange.strong, p.blue.strong, p.sky.strong];
    let totals: Vec<u64> = week.iter().map(|d| d.counts.iter().sum::<usize>() as u64).collect();
    let today = totals.last().copied().unwrap_or(0);
    let all: u64 = totals.iter().sum();
    const H: f32 = 100.0;
    let heights = bar_heights(&totals, H);
    Tile(|| {
        CardHead(ICON_CHART, "하네스별 사용량", || {
            let u = ui.clone();
            AppButton("자세히", None, ButtonKind::Default, true, true, false, move || shell::choose(&u, NavMenu::Agents));
        });
        if bars.is_empty() {
            Label(EMPTY_HARNESS_USAGE, Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
            return;
        }
        cu::Row().spacing(SP[5]).modifier(Modifier::new().fill_max_width()).content(|| {
            for (label, v) in [("오늘 세션", today), ("7일 합계", all)] {
                cu::Column().content(|| {
                    Label(label, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                    Label(&v.to_string(), Fs::Xl2, Weight::Regular, None, Some(1));
                });
            }
        });
        cu::Row().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
            for (h, color) in Harness::ALL.iter().zip(colors.iter()) {
                cu::Row().spacing(SP[0]).alignment(Alignment::CenterStart).content(|| {
                    Dot(*color);
                    Label(h.label(), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                });
            }
        });
        // Stacked bars: each day's column is split by harness; the stack's height follows the day's total.
        cu::Row().spacing(SP[1]).modifier(Modifier::new().fill_max_width().height(H)).content(|| {
            for (day, total_h) in week.iter().zip(heights.iter()) {
                cu::Column().modifier(Modifier::new().weight(1.0).height(H)).content(|| {
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                    let n: usize = day.counts.iter().sum();
                    for (count, color) in day.counts.iter().zip(colors.iter()).rev() {
                        if *count > 0 && n > 0 {
                            let h = total_h * *count as f32 / n as f32;
                            cu::Spacer().modifier(Modifier::new().fill_max_width().height(h).background(paint(*color)).corner_radius(CORNER_2XS));
                        }
                    }
                });
            }
        });
        cu::Row().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
            for (i, day) in week.iter().enumerate() {
                cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().weight(1.0)).content(|| {
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                    let today = i + 1 == week.len();
                    Label(day.label, Fs::Xs2, if today { Weight::Semibold } else { Weight::Regular }, Some(paint(if today { p.text } else { p.muted })), Some(1));
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                });
            }
        });
        // `.hm-rings`: one 52 px ring per harness with its share of the sessions.
        // TODO(server, FR-U7): rings per account with their usage share.
        let total: usize = bars.iter().map(|b| b.sessions).sum::<usize>().max(1);
        cu::Spacer().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
        cu::Row().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
            for (h, color) in Harness::ALL.iter().zip(colors.iter()) {
                let n = bars.iter().find(|b| b.label == h.label()).map(|b| b.sessions).unwrap_or(0);
                cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).modifier(Modifier::new().weight(1.0)).content(|| {
                    cu::Box().alignment(Alignment::Center).modifier(Modifier::new().size(52.0, 52.0)).content(|| {
                        ring_image(52.0, 6.0, &[(color.to_argb(), n as f32 / total as f32)]);
                        Label(&n.to_string(), Fs::Sm, Weight::Semibold, None, None);
                    });
                    Label(h.label(), Fs::Xs, Weight::Medium, None, Some(1));
                });
            }
        });
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
                    Label(&format!("{ended}건"), Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
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
            Label("프로젝트", Fs::Lg, Weight::Semibold, None, None);
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
                                // `.botrow`: up to five 20 px team members in their state.
                                let members = services().client.read(|s| project_members(s, &t.name, 5));
                                cu::Row().spacing(2.0).alignment(Alignment::CenterStart).content(|| {
                                    for (agent, status, title) in members.iter() {
                                        super::characters::Character(agent, false, 20.0, title, &t.name, super::characters::CharState::from_status(*status), None);
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
    fn fr_l11_composer_chips_shrink_to_the_row_and_end_in_an_ellipsis() {
        assert_eq!(suggestion_cols(1440.0), 3);
        assert_eq!(suggestion_cols(768.0), 2);
        assert_eq!(composer_width(1158.0), 720.0);
        assert_eq!(composer_width(340.0), 340.0);
        let nat = [150.0, 300.0, 120.0, 90.0];
        let w = shrink_widths(&nat, 480.0, 56.0);
        assert!(w.iter().sum::<f32>() <= 480.5, "{w:?}");
        // Proportional: every chip keeps 480/660 of its width.
        assert!((w[0] - 109.1).abs() < 0.2 && (w[1] - 218.2).abs() < 0.2, "{w:?}");
        assert!((w[3] - 65.5).abs() < 0.2, "{w:?}");
        assert_eq!(shrink_widths(&nat, 1000.0, 56.0), nat.to_vec());
        let cut = fit_label("Claude Code · Opus 5.5 · High", 120.0, 13.0);
        assert!(cut.ends_with('\u{2026}') && text_px(&cut, 13.0) <= 120.0, "{cut}");
        assert_eq!(fit_label("이 서버", 120.0, 13.0), "이 서버");
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
}
