//! compose-rust version of the Project screen (#220 phase 5; FR-L1 to FR-L4, FR-L24, FR-L26,
//! `docs/DESIGN.md` section 6.2): the computer strip, a grid of project boxes with their team
//! members, the full view of one project and its settings sheet.
//!
//! The decisions (boxes, members, ordering, filters, deck, settings rules) are `ui::project`'s
//! plain functions, used as they are. The server saves only computer assignment (FR-L26), so
//! the other settings fields are shown disabled with "준비 중입니다".

#![allow(non_snake_case)]

use compose_rust::runtime::{mutable_state_of, remember, remember_with_key, MutableState};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{Alignment, ButtonVariant, IconRole, ShapeRole, SpaceRole};

use ember_client::LauncherStatus;

pub use crate::ui::project::{
    chip_line, computer_chips, danger_allowed, deck_count, deck_label, display_name, filter_members,
    is_default_project, leader_choice_visible, part_supported, project_boxes, reach_pill, status_pill,
    validate_settings, visible_tiles, ComputerChip, Member, MemberFilter, ProjectBox, SettingsDraft,
    SettingsPart, MAX_TILES, SETTINGS_SAVE_AVAILABLE, SOON,
};
use super::characters::{CharState, Character, StylePicker};
use super::components::{
    AppButton, ButtonKind, Clickable, EmberField, EmberSheet, EmberSheetSized, FieldOptions, FilterChip,
    IconButton, Label, OptionSheet, Pill, PillTone, SheetFooter,
};
use super::home::{content_width, fit_label, ring_image, text_px, Cols, Part};
use super::notice;
use super::shell::{self, NavIcon};
use super::theme::*;
use crate::model::status_rank;
use crate::ide::{open_external, plan, IdeAction, IdeKind};
use crate::server::IdeLaunch;
use crate::services::{run_shared, services};
use crate::ui::coexist::{shared_ui, SharedUi};
use crate::ui::shell::NavMenu;

// ---------------------------------------------------------------------------------------------
// Layout decisions (project.css breakpoints, on the window width)
// ---------------------------------------------------------------------------------------------

/// Project box columns: 3 above 1279, 2 above 900, 1 below (`.pgrid`).
pub fn grid_cols(width_dp: f32) -> usize {
    if width_dp > 1279.0 {
        3
    } else if width_dp > 900.0 {
        2
    } else {
        1
    }
}

/// Member tile columns inside a box: 2, 1 on a phone (`.pbox-team`).
pub fn team_cols(width_dp: f32) -> usize {
    if width_dp > 560.0 {
        2
    } else {
        1
    }
}

/// The full view puts the routine and channel cards beside the members above 900 px.
pub fn full_two_columns(width_dp: f32) -> bool {
    width_dp > 900.0
}

/// The colour of a status in the donut and the stats (`teamSegs`).
fn status_color(s: LauncherStatus, p: &Palette) -> compose_rust::Color {
    match s {
        LauncherStatus::Running => p.blue.strong,
        LauncherStatus::WaitingForApproval => p.orange.strong,
        LauncherStatus::FinishedUnread => p.sky.strong,
        LauncherStatus::Finished => p.grey.strong,
        LauncherStatus::Failed => p.red.strong,
        _ => p.grey.strong,
    }
}

/// The five segments of a project's team, in the mockup's order: (label, status, count).
pub fn team_segments(members: &[Member]) -> Vec<(&'static str, LauncherStatus, usize)> {
    let n = |s: LauncherStatus| members.iter().filter(|m| m.status == s).count();
    vec![
        ("실행", LauncherStatus::Running, n(LauncherStatus::Running)),
        ("승인 대기", LauncherStatus::WaitingForApproval, n(LauncherStatus::WaitingForApproval)),
        ("새 결과", LauncherStatus::FinishedUnread, n(LauncherStatus::FinishedUnread)),
        ("완료", LauncherStatus::Finished, n(LauncherStatus::Finished)),
        ("실패", LauncherStatus::Failed, n(LauncherStatus::Failed)),
    ]
}

/// The members of one status in the full view's list, in the mockup's group order (`memberList`).
pub fn grouped(members: &[Member]) -> Vec<(LauncherStatus, Vec<Member>)> {
    let mut groups: Vec<(LauncherStatus, Vec<Member>)> = Vec::new();
    for m in members {
        match groups.iter_mut().find(|(s, _)| *s == m.status) {
            Some((_, v)) => v.push(m.clone()),
            None => groups.push((m.status, vec![m.clone()])),
        }
    }
    groups.sort_by(|a, b| status_rank(b.0).cmp(&status_rank(a.0)));
    groups
}

/// Whether a project box matches the search text: its name or one of its members.
pub fn box_matches(b: &ProjectBox, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    q.is_empty()
        || b.name.to_lowercase().contains(&q)
        || b.members.iter().any(|m| m.name.to_lowercase().contains(&q) || m.task.to_lowercase().contains(&q))
}

/// Greedy line breaking for the stats: the indices of `widths` per line, `gap` apart, in `avail`.
pub fn wrap_lines(widths: &[f32], gap: f32, avail: f32) -> Vec<Vec<usize>> {
    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut used = 0.0;
    for (i, w) in widths.iter().enumerate() {
        match lines.last_mut() {
            Some(line) if used + gap + w <= avail => {
                line.push(i);
                used += gap + w;
            }
            _ => {
                lines.push(vec![i]);
                used = *w;
            }
        }
    }
    lines
}

/// The width of one stat: the 8 px mark, the number and the label, 4 px apart.
pub fn stat_width(n: usize, label: &str) -> f32 {
    8.0 + 4.0 + text_px(&n.to_string(), Fs::Md.size()) + 4.0 + text_px(label, Fs::Md.size())
}

/// The computer chip's second line: the status, the running members and the project count
/// (`OS · 실행 2 · 프로젝트 3`; TODO(FR-X7): the OS joins when ember node reports it).
pub fn chip_sub(c: &ComputerChip) -> String {
    let mut s = reach_pill(c.reachability).0.to_string();
    if c.running > 0 {
        s.push_str(&format!(" \u{b7} 실행 {}", c.running));
    }
    if !c.projects.is_empty() {
        s.push_str(&format!(" \u{b7} 프로젝트 {}", c.projects.len()));
    }
    s
}

// ---------------------------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------------------------

/// What the screen has open and typed. `EMBER_CR_ROUTE=project-detail` opens the first
/// project's full view (`EMBER_CR_PROJECT=<id>` another), `EMBER_CR_PROJECT_SHEET=settings|
/// computer|new` a sheet, `EMBER_CR_PROJECT_EXPAND=1` expands every box's deck.
#[derive(Clone)]
struct Ui {
    full: MutableState<Option<String>>,
    settings: MutableState<Option<String>>,
    computer: MutableState<Option<String>>,
    new_project: MutableState<bool>,
    new_name: MutableState<String>,
    query: MutableState<String>,
    expanded: MutableState<Vec<String>>,
    filter: MutableState<MemberFilter>,
    member_query: MutableState<String>,
}

impl Ui {
    fn new(boxes: &[ProjectBox], chips: &[ComputerChip]) -> Ui {
        let first = boxes.first().map(|b| b.id.clone());
        let full = match std::env::var("EMBER_CR_PROJECT").ok() {
            Some(id) if !id.is_empty() => Some(id),
            _ if std::env::var("EMBER_CR_ROUTE").as_deref() == Ok("project-detail") => first.clone(),
            _ => None,
        };
        let sheet = std::env::var("EMBER_CR_PROJECT_SHEET").ok();
        let expand = std::env::var("EMBER_CR_PROJECT_EXPAND").as_deref() == Ok("1");
        Ui {
            settings: mutable_state_of((sheet.as_deref() == Some("settings")).then(|| full.clone().or(first.clone())).flatten()),
            computer: mutable_state_of((sheet.as_deref() == Some("computer")).then(|| chips.first().map(|c| c.id.clone())).flatten()),
            new_project: mutable_state_of(sheet.as_deref() == Some("new")),
            full: mutable_state_of(full),
            new_name: mutable_state_of(String::new()),
            query: mutable_state_of(String::new()),
            expanded: mutable_state_of(if expand { boxes.iter().map(|b| b.id.clone()).collect() } else { Vec::new() }),
            filter: mutable_state_of(MemberFilter::All),
            member_query: mutable_state_of(String::new()),
        }
    }
}

/// Assign or unassign a computer on the main server (FR-L4); say so when it fails.
fn set_assignment(project: String, computer_id: String, assign: bool) {
    run_shared(
        async move {
            let c = &services().client;
            let r = if assign { c.assign_computer(&project, &computer_id).await } else { c.unassign_computer(&project, &computer_id).await };
            r.map(|_| ()).map_err(|e| e.to_string())
        },
        |r| {
            if let Err(e) = r {
                notice::post(format!("컴퓨터 할당을 바꾸지 못했습니다: {e}"));
            }
        },
    );
}

// ---------------------------------------------------------------------------------------------
// The screen
// ---------------------------------------------------------------------------------------------

pub fn ProjectScreen() {
    let ui = shared_ui().clone();
    let _ = ui.live.launcher.get();
    let (boxes, chips) = services().client.read(|s| {
        let boxes = project_boxes(s);
        let chips = computer_chips(s, &boxes);
        (boxes, chips)
    });
    let st = remember(|| Ui::new(&boxes, &chips));
    let window = compose_rust::runtime::current_window_size();
    let w = window.width_dp;
    let compact = w <= 560.0;
    let pad = if compact { SP[3] } else { SP[6] };
    let full = st.full.get();

    cu::Box().modifier(Modifier::new().fill_max_width().fill_max_height()).content(|| {
        cu::ScrollColumn().modifier(Modifier::new().fill_max_width().fill_max_height()).content(|| {
            cu::Column().spacing(SP[4]).modifier(Modifier::new().fill_max_width().padding(pad)).content(|| {
                super::launcher::ConnectionBanner();
                match full.as_ref().and_then(|id| boxes.iter().find(|b| &b.id == id)) {
                    Some(b) => FullProject(b, &chips, &st, &ui, w),
                    None if full.is_some() => {
                        Label("프로젝트를 찾을 수 없습니다", Fs::Md, Weight::Regular, Some(paint(ember_theme().palette.muted)), None)
                    }
                    None => Grid(&boxes, &chips, &st, &ui, w),
                }
            });
        });
        Sheets(&boxes, &chips, &st, &ui);
    });
}

/// The page header: the title and subtitle, and the actions at the right; on a phone the actions
/// are a row below the title (and the title leaves room for the menu button at the top right).
fn Header(title: &str, sub: &str, compact: bool, trailing: impl FnOnce()) {
    let p = ember_theme().palette;
    let titles = || {
        cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
            Label(title, if compact { Fs::Xl2 } else { Fs::Xl3 }, Weight::Medium, None, Some(1));
            if !sub.is_empty() {
                Label(sub, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
            }
        });
    };
    if compact {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Row().modifier(Modifier::new().fill_max_width()).content(|| {
                titles();
                cu::Spacer().modifier(Modifier::new().width(PHONE_MENU_ROOM));
            });
            cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).content(trailing);
        });
    } else {
        cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
            titles();
            trailing();
        });
    }
}

/// The room the shell's floating menu button takes at the top right of a phone.
const PHONE_MENU_ROOM: f32 = 52.0;

/// The theme toggle of the page header: a 40 px round button with a sun or a moon.
/// TODO(compose-rust#372): a pointer-up `Clickable`; the toolkit has no sun or moon glyph.
fn ThemeButton(ui: &SharedUi) {
    let p = ember_theme().palette;
    let dark = ember_theme().scheme == Scheme::Dark;
    let u = ui.clone();
    Clickable(
        Modifier::new().size(H_BTN, H_BTN).background(paint(p.surface)).border(1.0, paint(p.line)).shape_role(ShapeRole::Full),
        Alignment::Center,
        move || u.set_theme_pref(u.theme_pref.get_untracked().next()),
        || NavIcon(if dark { shell::ICON_SUN_SVG } else { shell::ICON_MOON_SVG }, p.text_2.to_argb(), 18.0),
    );
}

/// The list header's actions: search, 새 프로젝트, the theme toggle and 새 팀원. Up to 900 px
/// the search is a button, up to 900 the labels of the first two drop, and on a phone 새 팀원
/// is a round button too (`hide-md`).
fn ListActions(st: &Ui, ui: &SharedUi, w: f32) {
    if w > 900.0 {
        let q = st.query.clone();
        // TODO(compose-rust#376, #374): the field frame, the magnifier and the "/" key hint.
        cu::Column().modifier(Modifier::new().width(240.0)).content(|| {
            EmberField(FieldOptions {
                fill_max_width: true,
                placeholder: "팀원 검색".to_string(),
                on_value_change: Some(Box::new(move |v| q.set(v))),
                ..Default::default()
            });
        });
    } else {
        let u = ui.clone();
        IconButton(IconRole::Search, false, move || u.go(crate::ui::Route::Search));
    }
    let n = st.new_project.clone();
    // Enabled in the demo only: it makes a local project there (#218).
    let can_create = services().demo.is_some();
    let open_new = move || {
        if can_create {
            n.set(true)
        } else {
            notice::post(SOON)
        }
    };
    if w > 900.0 {
        AppButton("새 프로젝트", Some(IconRole::Library), ButtonKind::Default, false, can_create, false, open_new);
    } else {
        IconButton(IconRole::Library, false, open_new);
    }
    ThemeButton(ui);
    NewMemberButton(ui, None, w);
}

/// 새 팀원: a primary button; a 40 px ink circle on a phone.
fn NewMemberButton(ui: &SharedUi, project: Option<String>, w: f32) {
    let p = ember_theme().palette;
    let u = ui.clone();
    if w > 560.0 {
        AppButton("새 팀원", Some(IconRole::Add), ButtonKind::Primary, false, true, false, move || u.open_new_session(project.clone()));
    } else {
        cu::Button("")
            .icon(IconRole::Add)
            .variant(ButtonVariant::Text)
            .color(paint(p.ink_text))
            .modifier(Modifier::new().size(H_BTN, H_BTN).background(paint(p.ink)).shape_role(ShapeRole::Full))
            .on_click(move || u.open_new_session(project.clone()));
    }
}

fn Grid(boxes: &[ProjectBox], chips: &[ComputerChip], st: &Ui, ui: &SharedUi, w: f32) {
    let p = ember_theme().palette;
    let query = st.query.get();
    let shown: Vec<&ProjectBox> = boxes.iter().filter(|b| box_matches(b, &query)).collect();
    Header("프로젝트", "프로젝트를 누르면 세션으로 들어가 팀원들의 대화를 함께 봅니다.", w <= 560.0, || ListActions(st, ui, w));
    ComputerStrip(chips, st);
    if shown.is_empty() {
        let text = if boxes.is_empty() { "아직 프로젝트가 없습니다" } else { "일치하는 프로젝트가 없습니다" };
        Label(text, Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
    }
    let cols = grid_cols(w);
    let pad = if w <= 560.0 { SP[3] } else { SP[6] };
    // The box's inner width: its share of the content, less the 20 px side padding.
    let box_inner = ((content_width(w, pad) - (cols as f32 - 1.0) * SP[4]) / cols as f32 - 2.0 * SP[4]).max(200.0);
    let parts: Vec<Part> = shown
        .into_iter()
        .map(|b| -> Part { Box::new(move || ProjectBoxView(b, st, ui, w, box_inner)) })
        .collect();
    Cols(cols, SP[4], parts);
}

// --- computers -------------------------------------------------------------------------------

/// `.cstrip`: a row of computer chips, then the add chip.
fn ComputerStrip(chips: &[ComputerChip], st: &Ui) {
    let p = ember_theme().palette;
    if chips.is_empty() {
        Label("등록된 컴퓨터가 없습니다", Fs::Md, Weight::Regular, Some(paint(p.text_2)), None);
        return;
    }
    cu::ScrollRow().modifier(Modifier::new().fill_max_width().height(64.0)).content(|| {
        for c in chips {
            let (open, id) = (st.computer.clone(), c.id.clone());
            ComputerChipView(c, move || open.set(Some(id.clone())));
            cu::Spacer().modifier(Modifier::new().width(SP[1]));
        }
        // TODO(FR-X7): pairing a computer needs the hub's pairing code.
        let add = Modifier::new().height(56.0).border(1.0, paint(p.line)).corner_radius(CORNER_MD);
        Clickable(add, Alignment::CenterStart, || notice::post(SOON), || {
            cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).content(|| {
                cu::Spacer().modifier(Modifier::new().width(SP[2]));
                NavIcon(shell::ICON_PLUS_SVG, p.text_2.to_argb(), 18.0);
                Label("컴퓨터 추가", Fs::Sm, Weight::Medium, Some(paint(p.text_2)), Some(1));
                cu::Spacer().modifier(Modifier::new().width(SP[3]));
            });
        });
    });
}

/// `.cchip`: 56 px, an icon tile with a status dot, the name and the status line.
/// TODO(compose-rust#372): a pointer-up `Clickable`.
fn ComputerChipView(c: &ComputerChip, on_open: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let (_, tone) = reach_pill(c.reachability);
    let dot = super::components::pill_colors(tone.into(), p).0;
    let offline = !matches!(c.reachability, ember_client::state::Reachability::Online);
    let m = Modifier::new().height(56.0).background(paint(p.surface)).border(1.0, paint(p.line)).corner_radius(CORNER_MD);
    Clickable(if offline { m.alpha(0.6) } else { m }, Alignment::CenterStart, on_open, || {
        cu::Row().spacing(10.0).alignment(Alignment::CenterStart).content(|| {
            cu::Spacer().modifier(Modifier::new().width(1.0));
            cu::Box().modifier(Modifier::new().size(38.0, 38.0)).content(|| {
                cu::Box()
                    .alignment(Alignment::Center)
                    .modifier(Modifier::new().size(38.0, 38.0).background(paint(p.surface_2)).corner_radius(CORNER_SM))
                    .content(|| NavIcon(shell::ICON_CPU_SVG, p.text_2.to_argb(), 18.0));
                cu::Box().alignment(Alignment::BottomEnd).modifier(Modifier::new().size(38.0, 38.0)).content(|| {
                    cu::Spacer().modifier(Modifier::new().size(10.0, 10.0).background(paint(dot)).border(2.0, paint(p.surface)).shape_role(ShapeRole::Full));
                });
            });
            cu::Column().content(|| {
                Label(&fit_label(&c.name, 180.0, Fs::Sm.size()), Fs::Sm, Weight::Semibold, None, Some(1));
                Label(&fit_label(&chip_sub(c), 180.0, Fs::Xs.size()), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
            });
            cu::Spacer().modifier(Modifier::new().width(4.0));
        });
    });
}

// --- project boxes ---------------------------------------------------------------------------

/// `.donut`: the team ring, the member count and "팀원".
fn TeamDonut(members: &[Member], size: f32) {
    let p = ember_theme().palette;
    let total = members.len().max(1) as f32;
    let parts: Vec<(u32, f32)> = team_segments(members)
        .into_iter()
        .map(|(_, s, n)| (status_color(s, p).to_argb(), n as f32 / total))
        .collect();
    cu::Box().alignment(Alignment::Center).modifier(Modifier::new().size(size, size)).content(|| {
        ring_image(size, 6.0, &parts);
        cu::Column().alignment(Alignment::Center).content(|| {
            Label(&members.len().to_string(), Fs::Lg, Weight::Semibold, None, None);
            Label("팀원", Fs::Xs2, Weight::Regular, Some(paint(p.muted)), None);
        });
    });
}

/// `.pbox`: the header (ring, name and repository, actions, stats) and, below a rule, the
/// full-width `surface-2` team area.
/// TODO(compose-rust): per-corner radii and clipping; the team area is a rounded box over the
/// card's bottom corners, so its top corners are rounded too.
fn ProjectBoxView(b: &ProjectBox, st: &Ui, ui: &SharedUi, w: f32, inner: f32) {
    let p = ember_theme().palette;
    let expanded = st.expanded.get().contains(&b.id);
    let total = b.members.len();
    let shown = visible_tiles(total, expanded);
    let deck = deck_count(total, expanded);
    cu::Box()
        .modifier(Modifier::new().fill_max_width().background(paint(p.surface)).border(1.0, paint(p.line)).corner_radius(CORNER_XL))
        .content(|| {
            cu::Column().modifier(Modifier::new().fill_max_width()).content(|| {
                cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width().padding(SP[4])).content(|| {
                    cu::Row().spacing(SP[2]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                        TeamDonut(&b.members, 60.0);
                        let (full, id) = (st.full.clone(), b.id.clone());
                        // TODO(compose-rust#372): the name opens the full view; a pointer-up `Clickable`.
                        Clickable(Modifier::new().weight(1.0), Alignment::CenterStart, move || full.set(Some(id.clone())), || {
                            cu::Column().content(|| {
                                Label(&b.name, Fs::Lg, Weight::Semibold, None, Some(1));
                                Label(&b.repo_line(), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                            });
                        });
                        // `.pbox-act`: version control, settings, and the primary "세션 열기".
                        cu::Row().spacing(SP[0]).alignment(Alignment::CenterStart).content(|| {
                            let (u, id) = (ui.clone(), b.id.clone());
                            IconButton(IconRole::History, true, move || {
                                u.set_selected_project(Some(id.clone()));
                                shell::choose(&u, NavMenu::VersionControl);
                            });
                            let (settings, id) = (st.settings.clone(), b.id.clone());
                            IconButton(IconRole::Settings, true, move || settings.set(Some(id.clone())));
                            OpenSessionButton(b, ui, true);
                        });
                    });
                    Stats(b, inner);
                });
                cu::Spacer().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
                cu::Box()
                    .modifier(Modifier::new().fill_max_width().background(paint(p.surface_2)).corner_radius(CORNER_XL).padding(SP[3]))
                    .content(|| {
                        cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                            if total == 0 {
                                Label("아직 팀원이 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                            }
                            let tiles: Vec<Part> = b
                                .members
                                .iter()
                                .take(shown)
                                .map(|m| -> Part { Box::new(move || MemberTile(m, &b.id, ui)) })
                                .collect();
                            Cols(team_cols(w), SP[1], tiles);
                            if deck > 0 {
                                let e = st.expanded.clone();
                                let id = b.id.clone();
                                AppButton(&deck_label(deck), None, ButtonKind::Default, true, true, true, move || {
                                    let mut v = e.get_untracked();
                                    v.push(id.clone());
                                    e.set(v);
                                });
                            } else if total > MAX_TILES {
                                let e = st.expanded.clone();
                                let id = b.id.clone();
                                AppButton("접기", None, ButtonKind::Default, true, true, true, move || {
                                    let mut v = e.get_untracked();
                                    v.retain(|x| x != &id);
                                    e.set(v);
                                });
                            }
                        });
                    });
            });
        });
}

/// 세션 열기: the first member's conversation, or a new session when the project has none.
/// TODO(compose-rust#372): the mockup's grid glyph; the toolkit's forward arrow.
fn OpenSessionButton(b: &ProjectBox, ui: &SharedUi, small: bool) {
    let p = ember_theme().palette;
    let size = if small { H_CTL } else { H_BTN };
    let (u, first, id) = (ui.clone(), b.members.first().map(|m| m.session_id.clone()), b.id.clone());
    cu::Button("")
        .icon(IconRole::Forward)
        .variant(ButtonVariant::Text)
        .color(paint(p.ink_text))
        .modifier(Modifier::new().size(size, size).background(paint(p.ink)).shape_role(ShapeRole::Full))
        .on_click(move || match &first {
            Some(s) => u.open_conversation(s),
            None => u.open_new_session(Some(id.clone())),
        });
}

/// `.pbox-stats`: the non-empty segments and the computer count flow left to right, 14 px apart,
/// and wrap; the computer count sits at the right end of the last line.
fn Stats(b: &ProjectBox, inner: f32) {
    let p = ember_theme().palette;
    let mut items: Vec<(String, usize, Option<compose_rust::Color>)> = team_segments(&b.members)
        .into_iter()
        .filter(|(_, _, n)| *n > 0)
        .map(|(label, s, n)| (label.to_string(), n, Some(status_color(s, p))))
        .collect();
    items.push(("컴퓨터".to_string(), b.computers.len(), None));
    let widths: Vec<f32> = items.iter().map(|(l, n, _)| stat_width(*n, l)).collect();
    let lines = wrap_lines(&widths, 14.0, inner);
    let last = items.len() - 1;
    cu::Column().spacing(SP[0]).modifier(Modifier::new().fill_max_width()).content(|| {
        for line in lines {
            cu::Row().spacing(14.0).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                for i in line {
                    let (label, n, color) = &items[i];
                    if i == last {
                        cu::Spacer().modifier(Modifier::new().weight(1.0));
                    }
                    cu::Row().spacing(4.0).alignment(Alignment::CenterStart).content(|| {
                        match color {
                            // The mark is an 8 px rounded square (radius 3).
                            Some(c) => {
                                cu::Spacer().modifier(Modifier::new().size(8.0, 8.0).background(paint(*c)).corner_radius(3.0));
                            }
                            None => NavIcon(shell::ICON_CPU_SVG, p.muted.to_argb(), 14.0),
                        }
                        Label(&n.to_string(), Fs::Md, Weight::Semibold, None, None);
                        Label(label, Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
                    });
                }
            });
        }
    });
}

/// `.mtile`: the character, the name, the task and the status; a waiting member's border takes
/// the orange. TODO(compose-rust#372): a pointer-up `Clickable`.
fn MemberTile(m: &Member, project_id: &str, ui: &SharedUi) {
    let p = ember_theme().palette;
    let (label, tone) = status_pill(m.status);
    let border = if m.status == LauncherStatus::WaitingForApproval {
        super::compat::mix(p.orange.strong, p.line, 0.55)
    } else {
        p.line
    };
    let (u, id) = (ui.clone(), m.session_id.clone());
    Clickable(
        Modifier::new().fill_max_width().background(paint(p.surface)).border(1.0, paint(border)).corner_radius(CORNER_MD).padding(10.0),
        Alignment::CenterStart,
        move || u.open_conversation(&id),
        || {
            cu::Column().spacing(6.0).modifier(Modifier::new().fill_max_width()).content(|| {
                cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                    Character(&m.agent, m.leader, 34.0, &m.name, project_id, CharState::from_status(m.status), None);
                    Label(&m.accessible_name(), Fs::Sm, Weight::Semibold, None, Some(2));
                });
                Label(&m.task, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(2));
                Pill(label, tone.into(), true);
            });
        },
    );
}

// --- full project view -----------------------------------------------------------------------

fn FullProject(b: &ProjectBox, chips: &[ComputerChip], st: &Ui, ui: &SharedUi, w: f32) {
    let p = ember_theme().palette;
    let compact = w <= 560.0;
    let assigned: Vec<ComputerChip> = chips.iter().filter(|c| b.computers.contains(&c.id)).cloned().collect();
    let two = full_two_columns(w);
    // The top bar: back at the left; version control, settings and "세션 열기" at the right
    // (round icon buttons on a phone, leaving room for the menu button).
    cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
        let full = st.full.clone();
        AppButton("프로젝트", Some(IconRole::Back), ButtonKind::Default, true, true, false, move || full.set(None));
        cu::Spacer().modifier(Modifier::new().weight(1.0));
        let (u, id) = (ui.clone(), b.id.clone());
        let vcs = move || {
            u.set_selected_project(Some(id.clone()));
            shell::choose(&u, NavMenu::VersionControl);
        };
        let (settings, id) = (st.settings.clone(), b.id.clone());
        let open_settings = move || settings.set(Some(id.clone()));
        if compact {
            IconButton(IconRole::History, true, vcs);
            IconButton(IconRole::Settings, true, open_settings);
            OpenSessionButton(b, ui, true);
            cu::Spacer().modifier(Modifier::new().width(PHONE_MENU_ROOM));
        } else {
            AppButton("버전 관리", Some(IconRole::History), ButtonKind::Default, true, true, false, vcs);
            AppButton("설정", Some(IconRole::Settings), ButtonKind::Default, true, true, false, open_settings);
            let (u, first, id) = (ui.clone(), b.members.first().map(|m| m.session_id.clone()), b.id.clone());
            AppButton("세션 열기", Some(IconRole::Forward), ButtonKind::Primary, true, true, false, move || match &first {
                Some(s) => u.open_conversation(s),
                None => u.open_new_session(Some(id.clone())),
            });
        }
    });
    Header(&b.name, &b.repo_line(), compact, || {
        IdeMenu(b, ui, compact);
        ThemeButton(ui);
        NewMemberButton(ui, Some(b.id.clone()), w);
    });
    if assigned.is_empty() {
        Label("할당된 컴퓨터가 없습니다", Fs::Md, Weight::Regular, Some(paint(p.text_2)), None);
    } else {
        ComputerStrip(&assigned, st);
    }
    KpiStrip(b, ui, w);
    let members = || MembersCard(b, st, ui);
    let side = || {
        cu::Column().spacing(SP[4]).modifier(Modifier::new().fill_max_width()).content(|| {
            // TODO(routines, channels screens, phase 7): they list the project's items here.
            for (icon, title, empty) in [(shell::ICON_GAUGE_SVG, "루틴", "루틴이 없습니다"), (shell::ICON_HASH_SVG, "채널", "채널이 없습니다")] {
                super::home::Tile(|| {
                    super::home::CardHead(icon, title, || {});
                    Label(&format!("{empty}. {SOON}"), Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(2));
                });
            }
        });
    };
    if two {
        cu::Row().spacing(SP[4]).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Column().modifier(Modifier::new().weight(2.0)).content(members);
            cu::Column().modifier(Modifier::new().weight(1.0)).content(side);
        });
    } else {
        cu::Column().spacing(SP[4]).modifier(Modifier::new().fill_max_width()).content(|| {
            members();
            side();
        });
    }
}

// --- IDE 열기 ---------------------------------------------------------------------------------

/// The name a chooser entry shows.
fn ide_name(kind: IdeKind) -> &'static str {
    match kind {
        IdeKind::Ember => "Ember 에디터",
        IdeKind::VsCode => "VS Code",
        IdeKind::Gateway => "JetBrains Gateway",
    }
}

/// Hand an IDE choice to the OS, or open the Ember editor (FR-L7, FR-L31). The same steps as
/// the Dioxus conversation view's `launch_ide` (D32: exactly the three IDEs).
fn launch_ide(ui: &SharedUi, kind: IdeKind, launch: Option<Result<IdeLaunch, String>>) {
    let target = match &launch {
        None => {
            notice::post("IDE 목록을 받아오는 중입니다");
            return;
        }
        Some(Err(e)) => {
            notice::post(format!("IDE 목록을 받지 못했습니다: {e}"));
            return;
        }
        Some(Ok(l)) => l.target(kind.key()).cloned(),
    };
    let label = ide_name(kind);
    match plan(kind, target.as_ref()) {
        IdeAction::OpenUrl(url) => run_shared(
            async move {
                tokio::task::spawn_blocking(move || open_external(&url))
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r.map_err(|e| e.to_string()))
            },
            move |r| match r {
                Ok(()) => notice::post(format!("{label}: 여는 중입니다")),
                Err(e) => notice::post(format!("{label}: 열지 못했습니다: {e}")),
            },
        ),
        IdeAction::EmberPlaceholder => {
            let (project, folder) = IdeLaunch::editor_target(launch.as_ref());
            ui.open_editor(project, folder);
        }
        IdeAction::Unavailable(why) => notice::post(format!("{label}: 사용할 수 없습니다: {why}")),
    }
}

/// "IDE 열기" with the three-entry chooser. It asks the server for the targets of the project's
/// first session when it appears (the demo serves the list itself); a project without a session
/// has nothing to open.
fn IdeMenu(b: &ProjectBox, ui: &SharedUi, compact: bool) {
    let session = b.members.first().map(|m| m.session_id.clone());
    let key = session.clone().unwrap_or_default();
    let open = remember_with_key(key.clone(), || mutable_state_of(false));
    let ide = remember_with_key(key.clone(), || mutable_state_of(None::<Result<IdeLaunch, String>>));
    let started = remember_with_key(key, || mutable_state_of(false));
    if let (Some(sid), false) = (session.clone(), started.get_untracked()) {
        started.set(true);
        if services().demo.is_some() {
            let (project, cwd) = services()
                .client
                .read(|s| s.session(&sid).map(|v| (v.record.project.clone(), v.record.cwd.clone())))
                .unwrap_or_default();
            ide.set(Some(Ok(crate::demo::ide_launch(&sid, &project, &cwd))));
        } else {
            let ide = ide.clone();
            run_shared(
                async move { services().server.ide_targets(&sid, None).await.map_err(|e| e.to_string()) },
                move |r| ide.set(Some(r)),
            );
        }
    }
    let remembered = ui.prefs.get().ide_target.as_deref().and_then(IdeKind::from_key);
    let choices = crate::ui::session::ide_choices(remembered);
    let labels: Vec<String> = choices.iter().map(|c| c.label.to_string()).collect();
    let has_session = session.is_some();
    cu::Box().content(|| {
        let o = open.clone();
        AppButton(if compact { "IDE" } else { "IDE 열기" }, Some(IconRole::Forward), ButtonKind::Default, false, true, false, move || {
            if has_session {
                o.set(true)
            } else {
                notice::post("세션이 없어 IDE를 열 수 없습니다")
            }
        });
        let (close, ui2, ide2) = (open.clone(), ui.clone(), ide.clone());
        OptionSheet(open.get(), &labels, None, 220.0, move || close.set(false), move |i| {
            if let Some(c) = choices.get(i) {
                let kind = c.kind;
                ui2.update_prefs(|p| p.ide_target = Some(kind.key().to_string()));
                launch_ide(&ui2, kind, ide2.get_untracked());
            }
        });
    });
}

/// `.strip`: members, waiting, running and the week's tokens, with the pinned sessions as chips.
fn KpiStrip(b: &ProjectBox, ui: &SharedUi, w: f32) {
    let p = ember_theme().palette;
    let items = [
        ("멤버", b.members.len().to_string()),
        ("승인 대기", b.waiting.to_string()),
        ("실행 중", b.running.to_string()),
        ("이번 주 토큰", SOON.to_string()),
    ];
    let pinned: Vec<&Member> = services().client.read(|s| {
        b.members.iter().filter(|m| s.session(&m.session_id).map_or(false, |v| v.record.pinned)).collect::<Vec<_>>()
    });
    let kpis = || {
        let cells: Vec<Part> = items
            .iter()
            .map(|(label, v)| -> Part {
                Box::new(move || {
                    cu::Column().spacing(SP[0]).content(|| {
                        Label(label, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
                        if *v == SOON {
                            Label(v, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
                        } else {
                            Label(v, Fs::Xl2, Weight::Regular, None, Some(1));
                        }
                    });
                })
            })
            .collect();
        Cols(if w > 560.0 { 4 } else { 2 }, SP[4], cells);
    };
    let pins = || {
        if pinned.is_empty() {
            return;
        }
        cu::Column().spacing(6.0).content(|| {
            Label("고정", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(1));
            for m in pinned.iter() {
                let (u, id) = (ui.clone(), m.session_id.clone());
                // `.pins .chip`: an outlined chip with the status mark and the title.
                Clickable(
                    Modifier::new().height(H_CTL).background(paint(p.surface)).border(1.0, paint(p.line)).shape_role(ShapeRole::Full),
                    Alignment::CenterStart,
                    move || u.open_conversation(&id),
                    || {
                        cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).content(|| {
                            cu::Spacer().modifier(Modifier::new().width(SP[1]));
                            super::home::Dot(status_color(m.status, p));
                            Label(&fit_label(&m.task, 220.0, Fs::Sm.size()), Fs::Sm, Weight::Regular, None, Some(1));
                            cu::Spacer().modifier(Modifier::new().width(SP[1]));
                        });
                    },
                );
            }
        });
    };
    cu::Box()
        .modifier(Modifier::new().fill_max_width().background(paint(p.surface)).border(1.0, paint(p.line)).corner_radius(CORNER_LG).padding(SP[3]))
        .content(|| {
            if w > 1279.0 && !pinned.is_empty() {
                cu::Row().spacing(SP[5]).modifier(Modifier::new().fill_max_width()).content(|| {
                    cu::Column().modifier(Modifier::new().weight(4.0)).content(kpis);
                    cu::Column().modifier(Modifier::new().weight(2.0)).content(pins);
                });
            } else {
                cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                    kpis();
                    pins();
                });
            }
        });
}

/// 멤버: the search, the status filters and the members grouped by status.
fn MembersCard(b: &ProjectBox, st: &Ui, ui: &SharedUi) {
    let p = ember_theme().palette;
    let filter = st.filter.get();
    let list = filter_members(&b.members, filter, &st.member_query.get());
    super::components::EmberCard(false, true, None, None, None, || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
            super::home::CardHead(shell::ICON_USERS_SVG, "멤버", || {
                Pill(&format!("{}명", b.members.len()), PillTone::Grey, false);
            });
            let q = st.member_query.clone();
            EmberField(FieldOptions {
                fill_max_width: true,
                placeholder: "멤버 검색".to_string(),
                on_value_change: Some(Box::new(move |v| q.set(v))),
                ..Default::default()
            });
            cu::ScrollRow().modifier(Modifier::new().fill_max_width().height(H_CTL + SP[0])).content(|| {
                for f in MemberFilter::ALL {
                    let s = st.filter.clone();
                    FilterChip(f.label(), f == filter, move || s.set(f));
                    cu::Spacer().modifier(Modifier::new().width(SP[0]));
                }
            });
            if list.is_empty() {
                Label("일치하는 멤버가 없습니다", Fs::Md, Weight::Regular, Some(paint(p.text_2)), None);
            }
            for (status, members) in grouped(&list) {
                // `.grp`: the status and its count.
                Label(&format!("{} \u{b7} {}", status_pill(status).0, members.len()), Fs::Xs, Weight::Semibold, Some(paint(p.muted)), Some(1));
                for (i, m) in members.iter().enumerate() {
                    MemberRow(m, &b.id, ui, i > 0);
                }
            }
        });
    });
}

/// `.member`: a 36 px character, the name with its status pill, and the task line.
/// TODO(compose-rust#372): a pointer-up `Clickable`.
fn MemberRow(m: &Member, project_id: &str, ui: &SharedUi, rule: bool) {
    let p = ember_theme().palette;
    let (label, tone) = status_pill(m.status);
    let (u, id) = (ui.clone(), m.session_id.clone());
    if rule {
        cu::Spacer().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
    }
    // TODO(FR-X7): the computer the member runs on and the pending approval's request line need
    // the server (the client knows neither).
    Clickable(Modifier::new().fill_max_width().padding(SP[0]), Alignment::CenterStart, move || u.open_conversation(&id), || {
        cu::Row().spacing(SP[2]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
            Character(&m.agent, m.leader, 36.0, &m.name, project_id, CharState::from_status(m.status), None);
            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).content(|| {
                    Label(&fit_label(&m.accessible_name(), 220.0, Fs::Md.size()), Fs::Md, Weight::Medium, None, Some(1));
                    Pill(label, tone.into(), true);
                });
                let line = format!("{} \u{b7} {}", m.task, m.agent);
                Label(&fit_label(&line, 520.0, Fs::Xs.size()), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
            });
        });
    });
}

// ---------------------------------------------------------------------------------------------
// Sheets
// ---------------------------------------------------------------------------------------------

fn Sheets(boxes: &[ProjectBox], chips: &[ComputerChip], st: &Ui, ui: &SharedUi) {
    // New project (demo only).
    {
        let close = {
            let n = st.new_project.clone();
            move || n.set(false)
        };
        EmberSheet("새 프로젝트", st.new_project.get(), close.clone(), None, || {
            cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width().padding(SP[4])).content(|| {
                let name = st.new_name.clone();
                // TODO(compose-rust#374): the field is uncontrolled with no initial value.
                EmberField(FieldOptions {
                    fill_max_width: true,
                    placeholder: "프로젝트 이름".to_string(),
                    on_value_change: Some(Box::new(move |v| name.set(v))),
                    ..Default::default()
                });
            });
            SheetFooter(|| {
                AppButton("취소", None, ButtonKind::Default, true, true, false, close.clone());
                let (name, n) = (st.new_name.clone(), st.new_project.clone());
                AppButton("만들기", None, ButtonKind::Primary, true, true, false, move || {
                    let s = services();
                    let Some(d) = &s.demo else { return };
                    match d.create_project(&s.client, &name.get_untracked()) {
                        Ok(_) => n.set(false),
                        Err(e) => notice::post(e),
                    }
                });
            });
        });
    }
    // A computer's monitor.
    if let Some(id) = st.computer.get() {
        let c = st.computer.clone();
        super::monitor::MonitorSheet(chips.iter().find(|x| x.id == id), boxes, move || c.set(None));
    }
    // The project's settings.
    if let Some(id) = st.settings.get() {
        if let Some(b) = boxes.iter().find(|b| b.id == id) {
            let s = st.settings.clone();
            SettingsSheet(b, chips, move || s.set(None));
        }
        let _ = ui;
    }
}

/// A settings section: a rule above (except the first), the 16 px title, its hint, the content.
fn Section(title: &str, hint: &str, first: bool, content: impl FnOnce()) {
    let p = ember_theme().palette;
    cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
        if !first {
            cu::Spacer().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
            cu::Spacer().modifier(Modifier::new().height(SP[1]));
        }
        Label(title, Fs::Lg, Weight::Semibold, None, Some(1));
        if !hint.is_empty() {
            Label(hint, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(3));
        }
        content();
    });
}

/// `.ps-field`: a small label above its field.
fn FieldRow(label: &str, content: impl FnOnce()) {
    let p = ember_theme().palette;
    cu::Column().spacing(4.0).modifier(Modifier::new().fill_max_width()).content(|| {
        Label(label, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
        content();
    });
}

/// The settings sheet: 760 wide, with 취소 and 저장 in the header beside the close button.
fn SettingsSheet(b: &ProjectBox, chips: &[ComputerChip], on_close: impl FnMut() + Clone + 'static) {
    let p = ember_theme().palette;
    let default_project = is_default_project(&b.id);
    let draft = remember_with_key(b.id.clone(), || {
        mutable_state_of(SettingsDraft {
            name: b.name.clone(),
            repo: b.repo.clone().unwrap_or_default(),
            branch: b.branch.clone().unwrap_or_default(),
        })
    });
    let d = draft.get();
    let errors = validate_settings(&d, &b.id);
    let can_save = SETTINGS_SAVE_AVAILABLE && errors.is_empty();
    let window = compose_rust::runtime::current_window_size();
    let body_height = (window.height_dp - 32.0 - 60.0).clamp(240.0, 900.0);
    let width = (window.width_dp - 32.0).clamp(280.0, 760.0);
    let mut actions = {
        let close = on_close.clone();
        move || {
            AppButton("취소", None, ButtonKind::Default, true, true, false, close.clone());
            cu::Spacer().modifier(Modifier::new().width(SP[0]));
            AppButton("저장", None, ButtonKind::Primary, true, can_save, false, || {});
            cu::Spacer().modifier(Modifier::new().width(SP[1]));
        }
    };
    EmberSheetSized(&format!("{} 설정", b.name), true, width, on_close.clone(), Some(&mut actions), || {
        cu::ScrollColumn().modifier(Modifier::new().fill_max_width().height(body_height)).content(|| {
            cu::Column().spacing(SP[3]).modifier(Modifier::new().fill_max_width().padding(SP[4])).content(|| {
                // TODO(server): rename and repository need a project settings API (FR-L26); the
                // fields are shown disabled until then.
                Section("기본", "프로젝트 이름과 저장소", true, || {
                    cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                        let (d1, d2, d3) = (draft.clone(), draft.clone(), draft.clone());
                        FieldRow("이름", || {
                            EmberField(FieldOptions {
                                fill_max_width: true,
                                placeholder: b.name.clone(),
                                enabled: Some(part_supported(SettingsPart::Name) && !default_project),
                                on_value_change: Some(Box::new(move |v| d1.update(|s| s.name = v))),
                                ..Default::default()
                            });
                        });
                        cu::Row().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                                FieldRow("저장소", || {
                                    EmberField(FieldOptions {
                                        fill_max_width: true,
                                        placeholder: b.repo.clone().unwrap_or_else(|| "github.com/...".to_string()),
                                        enabled: Some(part_supported(SettingsPart::Repository)),
                                        on_value_change: Some(Box::new(move |v| d2.update(|s| s.repo = v))),
                                        ..Default::default()
                                    });
                                });
                            });
                            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                                FieldRow("기본 브랜치", || {
                                    EmberField(FieldOptions {
                                        fill_max_width: true,
                                        placeholder: b.branch.clone().unwrap_or_else(|| "develop".to_string()),
                                        enabled: Some(part_supported(SettingsPart::Repository)),
                                        on_value_change: Some(Box::new(move |v| d3.update(|s| s.branch = v))),
                                        ..Default::default()
                                    });
                                });
                            });
                        });
                        if !part_supported(SettingsPart::Name) {
                            Label(SOON, Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                        }
                        for e in errors.iter() {
                            Label(e.message(), Fs::Xs, Weight::Regular, Some(paint(p.red.strong)), None);
                        }
                    });
                });
                Section("컴퓨터", "이 프로젝트의 팀원이 일할 수 있는 컴퓨터입니다", false, || {
                    cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                        if chips.is_empty() {
                            Label("등록된 컴퓨터가 없습니다", Fs::Md, Weight::Regular, Some(paint(p.text_2)), None);
                        }
                        for c in chips {
                            let on = b.computers.contains(&c.id);
                            cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                                Label(&c.name, Fs::Md, Weight::Regular, None, Some(1));
                                Label(reach_pill(c.reachability).0, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(1));
                                cu::Spacer().modifier(Modifier::new().weight(1.0));
                                let (pr, id) = (b.id.clone(), c.id.clone());
                                // TODO(compose-rust): a switch; a toggle button stands in.
                                AppButton(if on { "할당됨" } else { "할당" }, None, if on { ButtonKind::Primary } else { ButtonKind::Default }, true, true, false, move || {
                                    set_assignment(pr.clone(), id.clone(), !on)
                                });
                            });
                        }
                    });
                });
                Section("새 팀원 기본값", "하네스, 모델, 권한 모드는 새 팀원에게만 적용됩니다. 준비 중입니다", false, || {
                    AppButton("기본값 고르기", None, ButtonKind::Default, false, part_supported(SettingsPart::MemberDefaults), false, || {});
                });
                Section("팀 캐릭터", "팀원이 모두 고른 모양으로 보입니다. 이 기기에만 저장됩니다", false, || {
                    // TODO(server): the style is kept in this device's prefs until the server stores it.
                    StylePicker(&b.id, &b.name);
                });
                if leader_choice_visible(&b.id) {
                    Section("리더 (오케스트레이터)", "리더 지정은 준비 중입니다", false, || match b.leader() {
                        Some(l) => Label(&format!("현재 리더: {}", l.name), Fs::Md, Weight::Regular, None, None),
                        None => Label("리더가 없습니다", Fs::Md, Weight::Regular, Some(paint(p.text_2)), None),
                    });
                } else {
                    Section("리더 (오케스트레이터)", "기본 프로젝트는 리더를 두지 않습니다", false, || {
                        Label("리더가 필요하면 프로젝트를 새로 만들어 옮기세요.", Fs::Md, Weight::Regular, Some(paint(p.text_2)), None);
                    });
                }
                Section("프로필 덮어쓰기", "선택 사항입니다. 이 프로젝트에서만 하네스에 다른 프로필을 씁니다. 준비 중입니다", false, || {
                    AppButton("프로필 고르기", None, ButtonKind::Default, false, part_supported(SettingsPart::ProfileOverride), false, || {});
                });
                Section("연결", "이 프로젝트에 묶인 채널, 루틴, 버전 관리는 준비 중입니다", false, || {
                    Label("연결된 항목이 없습니다", Fs::Md, Weight::Regular, Some(paint(p.text_2)), None);
                });
                Section("위험 영역", if default_project { "기본 프로젝트는 보관하거나 지울 수 없습니다" } else { "되돌리기 어려운 작업입니다. 준비 중입니다" }, false, || {
                    cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                        AppButton("프로젝트 보관", None, ButtonKind::Destructive, false, danger_allowed(&b.id, SettingsPart::Archive), false, || {});
                        AppButton("프로젝트 삭제", None, ButtonKind::Destructive, false, danger_allowed(&b.id, SettingsPart::Delete), false, || {});
                    });
                });
            });
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use compose_rust::protocol::{decode_batch, HostEvent, Mutation, PropertyValue};
    use compose_rust::{Host, Recomposer};

    fn member(id: &str, status: LauncherStatus) -> Member {
        Member {
            session_id: id.into(),
            name: format!("m-{id}"),
            task: "task".into(),
            agent: "claude-code".into(),
            status,
            leader: false,
            updated_at: 0,
        }
    }

    #[test]
    fn fr_l1_the_grid_follows_the_mockup_breakpoints() {
        assert_eq!((grid_cols(1440.0), grid_cols(1024.0), grid_cols(768.0), grid_cols(390.0)), (3, 2, 1, 1));
        assert_eq!((team_cols(768.0), team_cols(390.0)), (2, 1));
        assert!(full_two_columns(1024.0) && !full_two_columns(768.0));
    }

    #[test]
    fn fr_l2_the_team_segments_count_each_status_and_the_groups_put_waiting_first() {
        let ms = vec![
            member("a", LauncherStatus::Running),
            member("b", LauncherStatus::WaitingForApproval),
            member("c", LauncherStatus::Running),
            member("d", LauncherStatus::Failed),
        ];
        let segs = team_segments(&ms);
        assert_eq!(segs.iter().map(|s| s.2).collect::<Vec<_>>(), vec![2, 1, 0, 0, 1]);
        let g = grouped(&ms);
        assert_eq!(g[0].0, LauncherStatus::WaitingForApproval);
        assert_eq!(g[1].1.len(), 2);
    }

    #[test]
    fn fr_l1_a_box_matches_its_name_and_its_members() {
        let b = ProjectBox {
            id: "p".into(),
            name: "darkpyonix-ember".into(),
            members: vec![member("a", LauncherStatus::Running)],
            running: 1,
            waiting: 0,
            computers: vec![],
            top: None,
            repo: None,
            branch: None,
        };
        assert!(box_matches(&b, ""));
        assert!(box_matches(&b, "EMBER"));
        assert!(box_matches(&b, "m-a"));
        assert!(!box_matches(&b, "zzz"));
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
    fn fr_l1_the_project_screen_shows_its_title_the_strip_and_the_boxes() {
        use compose_rust::{EventPayload, WindowHeightClass, WindowSizeClass};
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| Box::new(Recomposer::new(ProjectScreen)));
        let mut all = texts(&decode_batch(host.rebuild().expect("rebuild")).expect("decode"));
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
        all.extend(texts(&decode_batch(bytes).expect("decode")));
        let has = |want: &str| all.iter().any(|t| t.contains(want));
        for want in ["프로젝트", "새 프로젝트", "새 팀원"] {
            assert!(has(want), "missing {want}: {all:?}");
        }
        compose_rust::window::reset_window_size();
    }
}
