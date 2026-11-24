//! compose-rust version of `ui::session` (#220 phase 6; FR-L5 to FR-L7, FR-L15, FR-L16, FR-L18,
//! FR-L24, `docs/DESIGN.md` section 6.3).
//!
//! Left, the side list (search, one section per project, the leader first); on the right a top
//! bar (project, view switcher, new member, version control, web page, IDE) over one of three
//! views: **멀티** (panes side by side, pane tabs per project), **싱글** (one member's
//! conversation plus a right panel) and **상태** (kanban).
//!
//! The pure decisions (`SessionView`, `PaneLayout`, `SideListMode`, `KanbanColumn`, `IdeChoice`,
//! `PaneMove`, `PaneTab`, `ProjectPanes`, `PaneToggle`, `InfoCard`, `side_sections`, `kanban`,
//! `lease_plan`, `model_chip_label`, `mode_chip_label`, `environment_rows`, etc.) live in
//! `crate::ui::session` and are re-exported here unchanged.

#![allow(non_snake_case)]

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use compose_rust::runtime::{
    MutableState, delay, disposable_effect, launched_effect, mutable_state_of, on_dispose, remember,
    remember_with_key,
};
// Ember's reading: a desktop window that has not reported its size yet reads its opened size
// (FR-L10.3).
use crate::ui::window_size::current_window_size;
use compose_rust::ui as cu;
use compose_rust::ui::{Modifier, Side};
use compose_rust::{Alignment, AssetKind, Color, IconRole, ShapeRole, Snap, SpaceRole, TypeRole};

use ember_client::LauncherStatus;

pub use crate::ui::session::{
    INFO_PANEL_W, IdeChoice, status_card_width, status_grid_columns, status_page_pad, token_tile, ComputerRow, NodeStatus, PrRow, StatusTone, SubNode, TokenRow,
    COMPUTERS_CAPTION, COMPUTERS_EMPTY, LOAD_CARD_TITLE, PR_CARD_TITLE, PR_EMPTY, SUBAGENT_CARD_TITLE,
    SUBAGENT_EMPTY, TOKENS_CAPTION, TOKENS_EMPTY, computer_fill_pct, computer_rows, computer_value_text,
    cpu_fill_tone, pr_count_text, pr_rows, session_count_rows, subagent_tree, token_rows,
    token_value_text, token_width_pct, vendor_accent, InfoCard, KanbanColumn, PANE_H, PaneLayout, PaneMove, PaneTab,
    PaneToggle, ProjectPanes, SIDE_LIST_W, SOON, Section, SessionView, SideListMode, UI_STRINGS,
    add_pane, can_move, close_pane, current_mode, environment_rows, ide_choices, kanban,
    kanban_column, lease_plan, mode_chip_label, mode_chip_warns, mode_rows, model_chip_label,
    move_pane, multi_pane_layout, pane_rows, swipe_pane_width, SWIPE_GAP, reorder_pane, resolve_project, side_list_mode,
    side_sections, single_target, toolbar_fold_level, toolbar_width, view_chips_below,
    view_from_saved, visible_panes,
};

use crate::ide::{IdeAction, IdeKind, open_external, open_ide_window, plan};
use crate::server::{CurrentComputer, IdeLaunch};
use crate::services::{run_shared, services};
use crate::ui::coexist::{SharedUi, shared_ui};
use crate::ui::project::{Member, display_name, project_boxes, project_boxes_with_archived, status_pill};

use super::characters::{CharState, Character};
use super::compat::Spinner;
use super::home::{CardHead, Dot as HomeDot};
use super::components::{
    AppButton, BackButton, ButtonKind, Clickable, CloseButton, NamedClickable, NamedClickableOn, NameThenPill, EmberCard, EmberCardSized, TrackBar, EmberField, EmberSheet,
    FieldOptions, IconButton, Label, OptionSheet, PickChip, Pill, PillTone, PopExtra, PopKind, PopMenu, PopRow, PopoverMenu, PopoverMenuWith, SearchField, Select, SheetFooter, Txt,
    SEARCH_FIELD_H_COMPACT,
};
use super::conversation::{ConversationView, ICON_SEND_SVG, ICON_TERMINAL_SVG, TranscriptRow, tail_start_index};
use super::notice;
use super::shell::{
    ICON_CHEVRON_DOWN_SVG, ICON_CLOSE_SVG, ICON_CPU_SVG, ICON_PLUS_SVG, ICON_USERS_SVG, NavIcon,
    RedBadge, leak_doc,
};
use super::theme::*;

pub const ICON_GRID_SVG: &str = include_str!("../../../assets/nav/grid.svg");
pub const ICON_USER_SVG: &str = include_str!("../../../assets/nav/user.svg");
pub const ICON_CHART_SVG: &str = include_str!("../../../assets/nav/chart.svg");
pub const ICON_PC_SVG: &str = include_str!("../../../assets/nav/pc.svg");
pub const ICON_FOLDER_SVG: &str = include_str!("../../../assets/nav/folder.svg");
pub const ICON_GIT_SVG: &str = include_str!("../../../assets/nav/git.svg");
pub const ICON_SHIELD_SVG: &str = include_str!("../../../assets/nav/shield.svg");
pub const ICON_GLOBE_SVG: &str = include_str!("../../../assets/nav/globe.svg");
pub const ICON_CHEVRON_RIGHT_SVG: &str = include_str!("../../../assets/nav/chevron-right.svg");
pub const ICON_GRIP_SVG: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round"><path d="M9 6h.01M15 6h.01M9 12h.01M15 12h.01M9 18h.01M15 18h.01"/></svg>"#;
pub const ICON_SIDEBAR_CLOSE_SVG: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="m18.75 4.5-7.5 7.5 7.5 7.5m-6-15L5.25 12l7.5 7.5"/></svg>"#;
pub const ICON_SIDEBAR_OPEN_SVG: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="m5.25 4.5 7.5 7.5-7.5 7.5m6-15 7.5 7.5-7.5 7.5"/></svg>"#;
pub const ICON_CODE_SVG: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M17.25 6.75 22.5 12l-5.25 5.25m-10.5 0L1.5 12l5.25-5.25m7.5-3-4.5 16.5"/></svg>"#;
pub const ICON_CHECK_SVG: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M9 12.75 11.25 15 15 9.75M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0Z"/></svg>"#;
pub const ICON_PLAY_SVG: &str = r#"<svg viewBox="0 0 24 24" fill="currentColor"><path d="M5.25 5.65c0-1.42 1.52-2.31 2.76-1.62l11.54 6.35c1.29.71 1.29 2.53 0 3.24L8.01 19.97c-1.24.69-2.76-.2-2.76-1.62V5.65Z"/></svg>"#;
pub const ICON_PAUSE_SVG: &str = r#"<svg viewBox="0 0 24 24" fill="currentColor"><path d="M6.75 5.25a.75.75 0 0 1 .75-.75H9a.75.75 0 0 1 .75.75v13.5a.75.75 0 0 1-.75.75H7.5a.75.75 0 0 1-.75-.75V5.25Zm7.5 0a.75.75 0 0 1 .75-.75h1.5a.75.75 0 0 1 .75.75v13.5a.75.75 0 0 1-.75.75H15a.75.75 0 0 1-.75-.75V5.25Z"/></svg>"#;
pub const ICON_REPEAT_SVG: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M19.5 12c0-1.23-.05-2.45-.14-3.66a4.11 4.11 0 0 0-3.7-3.7 48.66 48.66 0 0 0-7.32 0 4.11 4.11 0 0 0-3.7 3.7c-.09 1.21-.14 2.43-.14 3.66 0 1.23.05 2.45.14 3.66a4.11 4.11 0 0 0 3.7 3.7c2.43.2 4.89.2 7.32 0a4.11 4.11 0 0 0 3.7-3.7c.09-1.21.14-2.43.14-3.66Z"/></svg>"#;
pub const ICON_EDIT_SVG: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="m16.86 4.49 1.69-1.69a1.88 1.88 0 1 1 2.65 2.65L10.58 16.07a4.5 4.5 0 0 1-1.9 1.13L6 18l.8-2.68a4.5 4.5 0 0 1 1.13-1.9l8.93-8.93ZM19.5 7.13 16.86 4.49M18 14v4.75A2.25 2.25 0 0 1 15.75 21H5.25A2.25 2.25 0 0 1 3 18.75V8.25A2.25 2.25 0 0 1 5.25 6H10"/></svg>"#;
pub const ICON_EXPAND_SVG: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M3.75 3.75v4.5m0-4.5h4.5m-4.5 0L9 9M3.75 20.25v-4.5m0 4.5h4.5m-4.5 0L9 15M20.25 3.75h-4.5m4.5 0v4.5m0-4.5L15 9m5.25 11.25h-4.5m4.5 0v-4.5m0 4.5L15 15"/></svg>"#;
pub const ICON_LIST_SVG: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M3.75 6.75h16.5M3.75 12h16.5m-16.5 5.25h16.5"/></svg>"#;
pub const ICON_MORE_H_SVG: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M6.75 12a.75.75 0 1 1-1.5 0 .75.75 0 0 1 1.5 0ZM12.75 12a.75.75 0 1 1-1.5 0 .75.75 0 0 1 1.5 0ZM18.75 12a.75.75 0 1 1-1.5 0 .75.75 0 0 1 1.5 0Z"/></svg>"#;

const NO_PROJECT: &str = "프로젝트가 없습니다";
const NO_MEMBERS: &str = "표시할 팀원이 없습니다";
const PICK_FROM_LIST: &str = "목록에서 팀원을 고르면 이 탭에 추가됩니다";
const NO_CONVERSATION: &str = "아직 대화가 없습니다";
const ICON_CHAT_SVG: &str = include_str!("../../../assets/nav/chat.svg");
const MODEL_LOCKED: &str = "실행 중인 세션의 모델은 바꿀 수 없습니다";
const NO_SESSION: &str = "세션이 없습니다";
const TIMELINE_TITLE: &str = "에이전트 간 통신";
use crate::ui::session::{
    ComputerHost, TimelinePlayback, WebPagesLoad, WebTap, computer_host, timeline_summary,
    web_pages_from, web_tap,
};

const POPUP_W: f32 = 280.0;

fn ide_label(kind: IdeKind) -> &'static str {
    match kind {
        IdeKind::Ember => "Ember 에디터",
        IdeKind::VsCode => "VS Code",
        IdeKind::Gateway => "JetBrains Gateway",
    }
}

fn default_panes_for(project: &str) -> ProjectPanes {
    if services().demo.is_some() {
        ProjectPanes {
            tabs: vec![
                PaneTab::all("탭 1"),
                PaneTab::all("리더와 검토"),
            ],
            active: 0,
        }
    } else {
        let _ = project;
        ProjectPanes::default()
    }
}

/// Change the saved panes of a project in [`SharedUi`].
fn edit_panes(ui: &SharedUi, project: &str, f: impl FnOnce(&mut ProjectPanes)) {
    let project = project.to_string();
    ui.update_prefs(move |p| {
        let mut cur = p
            .panes
            .get(&project)
            .cloned()
            .unwrap_or_else(|| default_panes_for(&project))
            .normalized();
        f(&mut cur);
        p.panes.insert(project, cur);
    });
}

fn panes_of(ui: &SharedUi, project: &str) -> ProjectPanes {
    ui.prefs
        .get()
        .panes
        .get(project)
        .cloned()
        .unwrap_or_else(|| default_panes_for(project))
        .normalized()
}

/// Keep `wanted` sessions leased in the client while the calling composable is active.
fn use_leases_cr(wanted: &[String], focus: Option<&str>) {
    let mine = remember(|| Rc::new(RefCell::new(HashSet::<String>::new())));
    let (open, close) = {
        let m = mine.borrow();
        lease_plan(
            wanted,
            |id| services().client.read(|s| s.is_open(id)),
            &m,
            focus,
        )
    };
    for id in open {
        services().open_session(&id);
        mine.borrow_mut().insert(id);
    }
    for id in close {
        services().close_session(&id);
        mine.borrow_mut().remove(&id);
    }
}

/// Hand an IDE choice to the OS or open the Ember editor screen (`FR-L7`).
pub fn launch_ide_shared(
    ui: &SharedUi,
    kind: IdeKind,
    launch: Option<Result<IdeLaunch, String>>,
) {
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
    let label = ide_label(kind);
    match plan(kind, target.as_ref()) {
        IdeAction::OpenIdeWindow(url) => run_shared(
            async move {
                tokio::task::spawn_blocking(move || open_ide_window(&url))
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r)
            },
            move |r| match r {
                Ok(()) => notice::post(format!("{label}: 여는 중입니다")),
                Err(e) => notice::post(format!("{label}: 열지 못했습니다: {e}")),
            },
        ),
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
            let c = IdeLaunch::editor_connection(launch.as_ref());
            // The relay route alone is enough to reach the OSE (FR-N9.1).
            if c.endpoint.as_deref().map_or(true, str::is_empty) && c.relay.as_deref().map_or(true, str::is_empty) {
                notice::post("Ember 에디터: 서버가 OSE 런타임을 시작하지 못했습니다");
                return;
            }
            ui.open_editor_connection(c.project, c.folder, c.endpoint, c.token, c.server_url, c.computer, c.relay, c.vscode_url);
        }
        IdeAction::Unavailable(why) => {
            notice::post(format!("{label}: 사용할 수 없습니다: {why}"));
        }
    }
}

/// Red circular stop button (`.stop` in mockup: 30x30 circle, `--red`, 10x10 white square).
pub fn StopCircleButton(on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    Clickable(
        Modifier::new()
            .size(30.0, 30.0)
            .background(paint(p.red.strong))
            .shape_role(ShapeRole::Full),
        Alignment::Center,
        on_click,
        || {
            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                    .size(10.0, 10.0)
                    .background(paint(Color::rgb(0xffffff)))
                    .corner_radius(2.0)) };
        },
    );
}

/// Circular send button (`.sendround` in mockup: `size` circle, `--ink`, send glyph).
pub fn SendCircleButton(size: f32, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    // Named "보내기" for assistive technology (compose-rust v0.0.7, #429).
    NamedClickableOn(
        super::a11y::SEND,
        Modifier::new()
            .size(size, size)
            .background(paint(p.ink))
            .shape_role(ShapeRole::Full),
        Some(p.ink),
        Alignment::Center,
        on_click,
        || NavIcon(ICON_SEND_SVG, p.ink_text.to_argb(), 15.0),
    );
}

fn harness_dot_color(agent: &str, p: &Palette) -> Color {
    match agent {
        "codex" => p.green.strong,
        "antigravity" => p.blue.strong,
        _ => p.orange.strong,
    }
}

/// The one row of the session model chip's menu: a running session's model cannot change.
pub fn model_chip_rows() -> Vec<PopRow> {
    vec![PopRow {
        icon: Some(ICON_SHIELD_SVG),
        title: MODEL_LOCKED.to_string(),
        badge: Some((SOON.to_string(), PillTone::Grey)),
        ..Default::default()
    }]
}

/// The model chip of a composer (`.pickchip.modelchip`). Its menu hangs from the chip (FR-L10.4,
/// #888), in the permission picker's look with the harness and model as its head; at 560 and
/// below it is the mockup's bottom sheet (`plain_sheet`). The mockup opens the model picker here
/// (review SE20); a running session's model cannot change yet, so the menu holds one row saying
/// so. `EMBER_CR_SESSION_MENU=model` opens it at start, for captures.
pub fn ModelChip(session: &str, compact_label: bool) {
    let ui = shared_ui().clone();
    let p = ember_theme().palette;
    let sid = session.to_string();
    let model_menu = remember_with_key(format!("{sid}:model"), || {
        mutable_state_of(std::env::var("EMBER_CR_SESSION_MENU").as_deref() == Ok("model"))
    });

    let info = {
        let _ = ui.live.launcher.get();
        services().client.read(|s| {
            s.session(&sid)
                .map(|v| (v.record.agent.clone(), v.record.model.clone()))
        })
    };
    let Some((agent, model)) = info else {
        return;
    };

    let base_text = model_chip_label(&agent, model.as_deref());
    let shown_text = if compact_label {
        base_text
            .split(" · ")
            .next()
            .unwrap_or(&base_text)
            .to_string()
    } else {
        format!("{base_text} · 높음")
    };
    let dot_col = harness_dot_color(&agent, p);

    let rows = model_chip_rows();
    let (mm, mm_d) = (model_menu.clone(), model_menu.clone());
    let expanded = model_menu.get();
    PopoverMenuWith(
        model_menu.get(),
        PopMenu {
            name: super::a11y::MODEL_PICKER,
            kind: PopKind::Mode,
            head: &base_text,
            sub: None,
            rows: &rows,
            align: compose_rust::PopoverAlign::Start,
        },
        PopExtra { plain_sheet: true, ..Default::default() },
        move || mm_d.set(false),
        |_| {},
        move || {
            Clickable(
                Modifier::new()
                    .height(30.0)
                    .background(paint(p.surface_2))
                    .shape_role(ShapeRole::Full)
                    .expanded(expanded),
                Alignment::Center,
                move || mm.set(!mm.get_untracked()),
                move || {
                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 6.0, {
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(4.0)) };
                            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                                    .size(8.0, 8.0)
                                    .background(paint(dot_col))
                                    .corner_radius(4.0)) };
                            Label(
                                &shown_text,
                                Fs::Xs,
                                Weight::Medium,
                                Some(paint(p.text_2)),
                                Some(1),
                            );
                            NavIcon(ICON_CHEVRON_DOWN_SVG, p.muted.to_argb(), 12.0);
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(4.0)) };
                        }) };
                },
            );
        },
        None,
    );
}

/// The permission mode chip of a composer (`.pickchip.modechip`). It opens the permission
/// picker as a popover under the chip (FR-L16, FR-L10.4, #888, mockup `.mp-pop.mp-mode`): the
/// "권한 모드 · harness" head, then one row per mode with its shield (a warning triangle in red on
/// bypass), title, description and a check on the current one; at 560 and below a bottom sheet.
/// A running session can switch only between ask and bypass; the other rows carry a 준비 중입니다
/// pill and say so when tapped. `EMBER_CR_SESSION_MENU=mode` opens it at start, for captures.
pub fn ModeChip(session: &str, _compact_label: bool) {
    let ui = shared_ui().clone();
    let p = ember_theme().palette;
    let sid = session.to_string();
    let mode_menu = remember_with_key(format!("{sid}:mode"), || {
        mutable_state_of(std::env::var("EMBER_CR_SESSION_MENU").as_deref() == Ok("mode"))
    });

    let (agent, approve_all) = {
        let _ = ui.live.launcher.get();
        services().client.read(|s| {
            (
                s.session(&sid)
                    .map(|v| v.record.agent.clone())
                    .unwrap_or_default(),
                s.approve_all(&sid),
            )
        })
    };
    if agent.is_empty() {
        return;
    }

    let mode_text = mode_chip_label(approve_all);
    let warns = mode_chip_warns(approve_all);
    let rows = mode_rows(&agent);
    let (bg, fg) = if warns {
        (p.orange.soft, p.orange.strong)
    } else {
        (p.surface_2, p.text_2)
    };
    let now = current_mode(approve_all);
    let pop_rows: Vec<PopRow> = rows
        .iter()
        .map(|(m, settable)| PopRow {
            icon: Some(if m.is_bypass() { super::home::ICON_WARN } else { ICON_SHIELD_SVG }),
            title: m.label().to_string(),
            description: Some(m.description().to_string()),
            badge: (!*settable).then(|| (SOON.to_string(), PillTone::Grey)),
            chosen: *m == now,
            danger: m.is_bypass(),
            ..Default::default()
        })
        .collect();
    let head = crate::ui::home::Harness::from_kind(&agent)
        .map(crate::ui::home::mode_header)
        .unwrap_or_else(|| format!("권한 모드 \u{b7} {agent}"));

    let md_d = mode_menu.clone();
    let sid_m = sid.clone();
    let rows_c = rows.clone();
    let md = mode_menu.clone();
    let expanded = mode_menu.get();
    PopoverMenu(
        mode_menu.get(),
        PopMenu {
            name: super::a11y::MODE_PICKER,
            kind: PopKind::Mode,
            head: &head,
            sub: None,
            rows: &pop_rows,
            align: compose_rust::PopoverAlign::Start,
        },
        move || md_d.set(false),
        move |idx| {
            let Some((m, settable)) = rows_c.get(idx).copied() else {
                return;
            };
            if !settable {
                notice::post(SOON);
                return;
            }
            let s_id = sid_m.clone();
            let enable = m.is_bypass();
            run_shared(
                async move { services().set_approve_all(&s_id, enable).await },
                |r| {
                    if let Err(e) = r {
                        notice::post(format!("모드를 바꾸지 못했습니다: {e}"));
                    }
                },
            );
        },
        move || {
            Clickable(
                Modifier::new()
                    .height(30.0)
                    .background(paint(bg))
                    .shape_role(ShapeRole::Full)
                    .expanded(expanded),
                Alignment::Center,
                move || md.set(!md.get_untracked()),
                move || {
                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 5.0, {
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(4.0)) };
                            NavIcon(ICON_SHIELD_SVG, fg.to_argb(), 13.0);
                            Label(
                                mode_text,
                                Fs::Xs,
                                Weight::Medium,
                                Some(paint(fg)),
                                Some(1),
                            );
                            NavIcon(ICON_CHEVRON_DOWN_SVG, p.muted.to_argb(), 12.0);
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(4.0)) };
                        }) };
                },
            );
        },
        None,
    );
}

/// The model and mode chips of a composer (FR-L15, FR-L16).
pub fn ComposerChips(session: &str) {
    let sid = session.to_string();
    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 6.0, content = move || {
            ModelChip(&sid, false);
            ModeChip(&sid, false);
        }) };
}

/// What the main part of an "IDE 열기" button does (FR-L7): launch the remembered IDE directly,
/// or open the chooser when none is remembered yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdeClick {
    Launch(IdeKind),
    Choose,
}

/// The decision behind every header's "IDE 열기" button (session, project, conversation, phone).
pub fn ide_click(remembered: Option<IdeKind>) -> IdeClick {
    match remembered {
        Some(kind) => IdeClick::Launch(kind),
        None => IdeClick::Choose,
    }
}

/// Run the main-button click: `launch` for a remembered IDE, otherwise open the chooser.
pub fn ide_main_click(
    remembered: Option<IdeKind>,
    open: &MutableState<bool>,
    launch: impl FnOnce(IdeKind),
) {
    match ide_click(remembered) {
        IdeClick::Launch(kind) => launch(kind),
        IdeClick::Choose => open.set(true),
    }
}

/// The colours of the "IDE 열기" split button (`.btn.sm` in the mockup): `surface` fill and a
/// `line` border with `text` ink, the same default button as every other header action. It was
/// drawn with the `ink` fill, which is a light inverted button in the dark scheme (NFR-L3, #510).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdeSplitStyle {
    pub fill: Color,
    pub ink: Color,
    pub border: Color,
    pub divider: Color,
}

/// The `IdeSplitStyle` of the current palette.
pub fn ide_split_style(p: &Palette) -> IdeSplitStyle {
    IdeSplitStyle { fill: p.surface, ink: p.text, border: p.line, divider: p.line }
}

/// The corners of the leading half of a split pill `height` high: the pill's own arc at the
/// start, square where it meets the divider (`[top_start, top_end, bottom_end, bottom_start]`).
pub fn split_start_radii(height: f32) -> [f32; 4] {
    [height / 2.0, 0.0, 0.0, height / 2.0]
}

/// The corners of the trailing half of a split pill: square at the divider, the pill's arc at
/// the end.
pub fn split_end_radii(height: f32) -> [f32; 4] {
    [0.0, height / 2.0, height / 2.0, 0.0]
}

/// The chevron beside a header's "IDE 열기" that opens the chooser even when a default is set
/// (FR-L7). `on_ink` draws it inside the dark split button; otherwise as a bordered square.
/// Its accessible name is "다른 IDE 선택" (compose-rust v0.0.7, #460), and it says whether the
/// chooser is open. Drawn 26x32 (`height`), with a 44 dp hit area that does not grow the node
/// (`Modifier::min_touch_target`, compose-rust#472, v0.0.11; FR-L34.3, #323).
pub fn IdeChevron(open: MutableState<bool>, on_ink: bool) {
    IdeChevronSized(open, on_ink, 32.0);
}

/// [`IdeChevron`] `height` high, to stand as high as the button beside it: the project header's
/// 40 dp 열기 button, so its menu hangs 6 below both (review r8: 10 below the 32 chevron).
pub fn IdeChevronSized(open: MutableState<bool>, on_ink: bool, height: f32) {
    let p = ember_theme().palette;
    let (tint, m) = if on_ink {
        // The trailing end of the split pill: its hover layer, press tint and focus ring take
        // the pill's rounded end, not a square that crosses it (FR-L34.2, #251).
        (ide_split_style(p).ink.to_argb(), Modifier::new().size(26.0, height).corner_radii(split_end_radii(height)))
    } else {
        (
            p.text_2.to_argb(),
            Modifier::new()
                .size(26.0, height)
                .background(paint(p.surface))
                .border(1.0, paint(p.line))
                .shape_role(ShapeRole::Full),
        )
    };
    NamedClickable(
        super::a11y::OTHER_IDE,
        m.min_touch_target(super::vcs::ICON_TOUCH_TARGET).expanded(open.get()),
        Alignment::Center,
        move || open.set(true),
        move || NavIcon(ICON_CHEVRON_DOWN_SVG, tint, 13.0),
    );
}

/// The IDE menu's rows (session and project): the code glyph, the name, what the client does
/// with it, and the fill and check on the current IDE ([`crate::ui::session::current_ide`]: Ember
/// when none is remembered).
pub fn ide_pop_rows(choices: &[crate::ui::session::IdeChoice], remembered: Option<IdeKind>) -> Vec<PopRow> {
    let current = crate::ui::session::current_ide(remembered);
    choices
        .iter()
        .map(|c| PopRow {
            icon: Some(ICON_CODE_SVG),
            title: c.label.to_string(),
            description: Some(crate::ui::session::ide_description(c.kind).to_string()),
            chosen: c.kind == current,
            ..Default::default()
        })
        .collect()
}

/// [`ide_pop_rows`] with the Ember row carrying the chooser's advance notice
/// ([`crate::ui::session::ember_ide_notice`]): the notice replaces its description and a warning
/// badge marks it. The row stays selectable, so choosing it still opens the editor window, which
/// shows the fallback. Without a notice the rows come back as they are.
pub fn with_ember_notice(
    mut rows: Vec<PopRow>,
    choices: &[crate::ui::session::IdeChoice],
    launch: Option<&Result<IdeLaunch, String>>,
) -> Vec<PopRow> {
    if let Some(notice) = crate::ui::session::ember_ide_notice(launch) {
        for (row, c) in rows.iter_mut().zip(choices) {
            if c.kind == IdeKind::Ember {
                row.description = Some(notice.clone());
                row.badge = Some(("지원 안 함".to_string(), PillTone::Orange));
            }
        }
    }
    rows
}

/// The demo's own computer, as the mockup names it.
pub(crate) const DEMO_COMPUTER: &str = "mac-mini";

/// Reads the name of the session's computer into `into` (the demo's own computer in the demo),
/// for the IDE menu's `computer:~/project` line. A failed read leaves it unset.
pub(crate) fn load_session_computer(session: String, into: MutableState<Option<String>>) {
    if services().demo.is_some() {
        into.set(Some(DEMO_COMPUTER.to_string()));
        return;
    }
    run_shared(
        async move { services().server.session_computer(&session).await.map_err(|e| e.to_string()) },
        move |r: Result<CurrentComputer, String>| {
            if let Ok(c) = r {
                into.set(Some(c.computer.name));
            }
        },
    );
}

/// "IDE 열기" split button with the three-entry chooser (`.idesplit`, FR-L7), a menu under the
/// split button (FR-L10.4, #888, mockup `ideMenu`): "어느 IDE로 열까요" over the session's folder,
/// one row per IDE with the code glyph, its name, what it does and a check on the remembered one.
/// Not built: the VS Code runtime switch and the OSE pill (no runtime preference exists).
pub fn IdeMenu(session: &str) {
    let ui = shared_ui().clone();
    let window = current_window_size();
    let p = ember_theme().palette;
    let split = ide_split_style(p);
    let sid = session.to_string();
    let ide = remember_with_key(sid.clone(), || {
        mutable_state_of(None::<Result<IdeLaunch, String>>)
    });
    // `EMBER_CR_SESSION_MENU=ide` opens the chooser at start, for captures.
    let open = remember_with_key(sid.clone(), || {
        mutable_state_of(std::env::var("EMBER_CR_SESSION_MENU").as_deref() == Ok("ide"))
    });
    let is_phone = window.width_dp <= 560.0;
    let show_cur = window.width_dp >= 1280.0;

    let computer = remember_with_key(sid.clone(), || mutable_state_of(None::<String>));
    {
        let sid_load = sid.clone();
        let ide_load = ide.clone();
        let computer_load = computer.clone();
        launched_effect(sid.clone(), move || async move {
            load_session_computer(sid_load.clone(), computer_load);
            if services().demo.is_some() {
                let (project, cwd) = services()
                    .client
                    .read(|s| {
                        s.session(&sid_load)
                            .map(|v| (v.record.project.clone(), v.record.cwd.clone()))
                    })
                    .unwrap_or_default();
                ide_load.set(Some(Ok(crate::demo::ide_launch(&sid_load, &project, &cwd))));
                return;
            }
            let s_id = sid_load.clone();
            let ide_st = ide_load.clone();
            run_shared(
                async move {
                    services()
                        .server
                        .ide_targets(&s_id, None)
                        .await
                        .map_err(|e| e.to_string())
                },
                move |r| ide_st.set(Some(r)),
            );
        });
    }

    let remembered = ui
        .prefs
        .get()
        .ide_target
        .as_deref()
        .and_then(IdeKind::from_key);
    let choices = ide_choices(remembered);
    let cur_label = remembered.map(ide_label);

    let cwd = services()
        .client
        .read(|s| s.session(&sid).map(|v| v.record.cwd.clone()))
        .unwrap_or_default();
    // `computer:~/project` (mockup `ideMenu`), not the raw folder (review r8).
    let ide_sub = crate::ui::session::ide_menu_subtitle(crate::ui::session::ide_menu_computer(ide.get().as_ref(), computer.get().as_deref()).as_deref(), &cwd);
    let rows = ide_pop_rows(&choices, remembered);
    let rows = with_ember_notice(rows, &choices, ide.get().as_ref());
    let o_d = open.clone();
    let choices_c = choices.clone();
    let ui_c = ui.clone();
    let launch_now = ide.get();
    // The menu hangs from the whole split button, lined up with its end (`.sdrop{right:0}`).
    PopoverMenu(
        open.get(),
        PopMenu {
            name: super::a11y::IDE_PICKER,
            kind: PopKind::Drop,
            head: crate::ui::session::IDE_MENU_HEAD,
            sub: ide_sub.as_deref(),
            rows: &rows,
            align: compose_rust::PopoverAlign::End,
        },
        move || o_d.set(false),
        move |idx| {
            if let Some(c) = choices_c.get(idx) {
                launch_ide_shared(&ui_c, c.kind, launch_now.clone());
            }
        },
        move || {
            let o = open.clone();
            let o_main = open.clone();
            let ui_main = ui.clone();
            let launch_main = ide.get();
            if is_phone {
                let o_chev = open.clone();
                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::Center, spacing = 4.0, content = move || {
                        IdeChevron(o_chev, false);
                        Clickable(
                            Modifier::new()
                                .size(32.0, 32.0)
                                .background(paint(split.fill))
                                .border(1.0, paint(split.border))
                                .shape_role(ShapeRole::Full),
                            Alignment::Center,
                            move || {
                                ide_main_click(remembered, &o, |kind| {
                                    launch_ide_shared(&ui_main, kind, launch_main.clone())
                                })
                            },
                            || NavIcon(ICON_CODE_SVG, split.ink.to_argb(), 15.0),
                        );
                    }) };
            } else {
                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::Center, modifier = Modifier::new()
                            .height(32.0)
                            .background(paint(split.fill))
                            .border(1.0, paint(split.border))
                            .shape_role(ShapeRole::Full), content = move || {
                        Clickable(
                            // The leading half of the 32 dp pill (FR-L34.2, #251).
                            Modifier::new().fill_max_height().corner_radii(split_start_radii(32.0)),
                            Alignment::Center,
                            move || {
                                ide_main_click(remembered, &o_main, |kind| {
                                    launch_ide_shared(&ui_main, kind, launch_main.clone())
                                })
                            },
                            move || {
                                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 6.0, {
                                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(6.0)) };
                                        NavIcon(ICON_CODE_SVG, split.ink.to_argb(), 15.0);
                                        Label(
                                            "IDE 열기",
                                            Fs::Sm,
                                            Weight::Medium,
                                            Some(paint(split.ink)),
                                            Some(1),
                                        );
                                        if show_cur {
                                            if let Some(cur_label) = cur_label {
                                                Label(
                                                    cur_label,
                                                    Fs::Xs,
                                                    Weight::Regular,
                                                    Some(paint(split.ink)),
                                                    Some(1),
                                                );
                                            }
                                        }
                                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(4.0)) };
                                    }) };
                            },
                        );
                        { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                                .size(1.0, 18.0)
                                .background(paint(split.divider))) };
                        IdeChevron(o, true);
                    }) };
            }

        },
        None,
    );
}

fn status_dot_color(s: LauncherStatus, p: &Palette) -> Color {
    match s {
        LauncherStatus::Running => p.blue.strong,
        LauncherStatus::WaitingForApproval => p.orange.strong,
        LauncherStatus::FinishedUnread => p.sky.strong,
        LauncherStatus::Finished => p.green.strong,
        LauncherStatus::Failed => p.red.strong,
        _ => p.grey.strong,
    }
}

fn StatusDot(s: LauncherStatus) {
    let p = ember_theme().palette;
    let col = status_dot_color(s, p);
    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
            .size(8.0, 8.0)
            .background(paint(col))
            .corner_radius(4.0)) };
}

/// The side list: search, then a section per project (FR-L18).
pub fn SideList(
    sections: &[Section],
    current: &str,
    active: Option<&str>,
    is_multi: bool,
    on_collapse: Option<Box<dyn FnMut()>>,
    on_query: impl FnMut(String) + 'static,
    on_open: impl Fn(String, String) + Clone + 'static,
) {
    let p = ember_theme().palette;
    let cur_proj = current.to_string();
    let active_id = active.map(str::to_string);
    let sections_vec = sections.to_vec();
    let closed_secs = remember(|| mutable_state_of(HashSet::<String>::new()));

    { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new()
                .fill_max_width()
                .fill_max_height()
                .background(paint(p.surface_2)), content = move || {
            // .st-top header (height 60dp, bottom 1px line)
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Sm, modifier = Modifier::new()
                        .fill_max_width()
                        .height(SHEET_HEADER_H)
                        .padding(SP[2]), content = move || {
                    Label("세션", Fs::Lg, Weight::Semibold, Some(paint(p.text)), Some(1));
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                    if let Some(mut collapse_cb) = on_collapse {
                        Clickable(
                            Modifier::new()
                                .size(32.0, 32.0)
                                .corner_radius(CORNER_SM),
                            Alignment::Center,
                            move || collapse_cb(),
                            || NavIcon(ICON_SIDEBAR_CLOSE_SVG, p.muted.to_argb(), 16.0),
                        );
                    }
                }) };
            { use ::compose_rust::ui::Separator; Separator!(Some(paint(p.line))) };

            { use ::compose_rust::ui::Column; Column!(space_role = SpaceRole::Sm, modifier = Modifier::new()
                        .fill_max_width()
                        .weight(1.0)
                        .padding(SP[2]), content = move || {
                    SearchField("세션 검색", SEARCH_FIELD_H_COMPACT, on_query);

                    if is_multi {
                        Label(
                            "팀원을 끌어 칸에 놓을 수 있습니다",
                            Fs::Xs,
                            Weight::Regular,
                            Some(paint(p.muted)),
                            Some(1),
                        );
                    }

                    { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().fill_max_width().weight(1.0), content = move || {
                            { use ::compose_rust::ui::Column; Column!(space_role = SpaceRole::Sm, modifier = Modifier::new().fill_max_width(), content = move || {
                                    for s in sections_vec.iter() {
                                        let sec_proj = s.project.clone();
                                        let sec_name = s.name.clone();
                                        let sec_members = s.members.clone();
                                        let is_cur_proj = sec_proj == cur_proj;
                                        let active_in_sec = active_id.clone();
                                        let open_cb = on_open.clone();
                                        let is_closed = closed_secs.get().contains(&sec_proj);
                                        let cs_toggle = closed_secs.clone();
                                        let sec_proj_toggle = sec_proj.clone();
                                        let wait_in_sec = sec_members
                                            .iter()
                                            .filter(|m| m.status == LauncherStatus::WaitingForApproval)
                                            .count();
                                        let sec_len = sec_members.len();

                                        { use ::compose_rust::ui::Column; Column!(space_role = SpaceRole::Xs, modifier = Modifier::new().fill_max_width(), content = move || {
                                                Clickable(
                                                    Modifier::new()
                                                        .fill_max_width()
                                                        .padding(SP[0]),
                                                    Alignment::CenterStart,
                                                    move || {
                                                        cs_toggle.update(|set| {
                                                            if !set.remove(&sec_proj_toggle) {
                                                                set.insert(sec_proj_toggle.clone());
                                                            }
                                                        });
                                                    },
                                                    move || {
                                                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Xs, modifier = Modifier::new().fill_max_width(), {
                                                                NavIcon(
                                                                    if is_closed {
                                                                        ICON_CHEVRON_RIGHT_SVG
                                                                    } else {
                                                                        ICON_CHEVRON_DOWN_SVG
                                                                    },
                                                                    p.muted.to_argb(),
                                                                    12.0,
                                                                );
                                                                Label(
                                                                    &sec_name,
                                                                    Fs::Xs,
                                                                    Weight::Semibold,
                                                                    Some(paint(if is_cur_proj {
                                                                        p.text
                                                                    } else {
                                                                        p.muted
                                                                    })),
                                                                    Some(1),
                                                                );
                                                                Label(
                                                                    &sec_len.to_string(),
                                                                    Fs::Xs,
                                                                    Weight::Medium,
                                                                    Some(paint(p.muted)),
                                                                    Some(1),
                                                                );
                                                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                                                                if wait_in_sec > 0 {
                                                                    RedBadge(wait_in_sec);
                                                                }
                                                            }) };
                                                    },
                                                );

                                                if !is_closed {
                                                    for m in sec_members.iter() {
                                                        let is_active = active_in_sec.as_deref()
                                                            == Some(m.session_id.as_str());
                                                        let bg = if is_active {
                                                            paint(p.sel)
                                                        } else {
                                                            paint(Color::argb(0))
                                                        };
                                                        let (proj_click, id_click) =
                                                            (sec_proj.clone(), m.session_id.clone());
                                                        let cb = open_cb.clone();
                                                        let m_agent = m.agent.clone();
                                                        let m_leader = m.leader;
                                                        let m_name = m.name.clone();
                                                        let m_acc = m.accessible_name();
                                                        let m_task = m.task.clone();
                                                        let m_status = m.status;
                                                        let s_proj = sec_proj.clone();

                                                        Clickable(
                                                            Modifier::new()
                                                                .fill_max_width()
                                                                .background(bg)
                                                                .corner_radius(CORNER_SM)
                                                                .padding(SP[1]),
                                                            Alignment::CenterStart,
                                                            move || cb(proj_click.clone(), id_click.clone()),
                                                            move || {
                                                                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Sm, modifier = Modifier::new().fill_max_width(), content = move || {
                                                                        Character(
                                                                            &m_agent,
                                                                            m_leader,
                                                                            26.0,
                                                                            &m_name,
                                                                            &s_proj,
                                                                            CharState::from_status(m_status),
                                                                            None,
                                                                        );
                                                                        { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().weight(1.0), content = move || {
                                                                                Label(
                                                                                    &m_acc,
                                                                                    Fs::Sm,
                                                                                    Weight::Medium,
                                                                                    Some(paint(p.text)),
                                                                                    Some(1),
                                                                                );
                                                                                Label(
                                                                                    &m_task,
                                                                                    Fs::Xs,
                                                                                    Weight::Regular,
                                                                                    Some(paint(p.muted)),
                                                                                    Some(1),
                                                                                );
                                                                            }) };
                                                                        if m_status == LauncherStatus::Running {
                                                                            Spinner(12.0);
                                                                        } else {
                                                                            StatusDot(m_status);
                                                                        }
                                                                    }) };
                                                            },
                                                        );
                                                    }
                                                }
                                            }) };
                                    }
                                }) };
                        }) };
                }) };
        }) };
}

/// Collapsed 64 dp avatar rail (`.srail`) shown at tablet widths (`560 < width <= 900`) or when
/// the user collapses the side list column on desktop.
pub fn SideListRail(
    sections: &[Section],
    active: Option<&str>,
    on_expand: impl FnMut() + 'static,
    on_open: impl Fn(String, String) + Clone + 'static,
) {
    let p = ember_theme().palette;
    let active_id = active.map(str::to_string);
    let sections_vec = sections.to_vec();

    { use ::compose_rust::ui::Column; Column!(alignment = Alignment::TopCenter, modifier = Modifier::new()
                .width(64.0)
                .fill_max_height()
                .background(paint(p.surface_2)), content = move || {
            { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().fill_max_width().height(SHEET_HEADER_H), content = move || {
                    Clickable(
                        Modifier::new().size(36.0, 36.0).corner_radius(CORNER_SM),
                        Alignment::Center,
                        on_expand,
                        || NavIcon(ICON_SIDEBAR_OPEN_SVG, p.muted.to_argb(), 16.0),
                    );
                }) };
            { use ::compose_rust::ui::Separator; Separator!(Some(paint(p.line))) };

            { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().fill_max_width().weight(1.0), content = move || {
                    { use ::compose_rust::ui::Column; Column!(alignment = Alignment::TopCenter, spacing = SP[1], modifier = Modifier::new().fill_max_width().padding(SP[1]), content = move || {
                            for (sec_idx, s) in sections_vec.iter().enumerate() {
                                if sec_idx > 0 {
                                    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                                            .size(28.0, 1.0)
                                            .background(paint(p.line))) };
                                }
                                for m in s.members.iter() {
                                    let is_active =
                                        active_id.as_deref() == Some(m.session_id.as_str());
                                    let bg = if is_active {
                                        paint(p.sel)
                                    } else {
                                        paint(Color::argb(0))
                                    };
                                    let (proj_click, id_click) =
                                        (s.project.clone(), m.session_id.clone());
                                    let cb = on_open.clone();
                                    let m_agent = m.agent.clone();
                                    let m_leader = m.leader;
                                    let m_name = m.name.clone();
                                    let m_status = m.status;
                                    let s_proj = s.project.clone();

                                    Clickable(
                                        Modifier::new()
                                            .size(44.0, 44.0)
                                            .background(bg)
                                            .corner_radius(CORNER_MD),
                                        Alignment::Center,
                                        move || cb(proj_click.clone(), id_click.clone()),
                                        move || {
                                            { use ::compose_rust::ui::Box; Box!(alignment = Alignment::BottomEnd, modifier = Modifier::new().size(30.0, 30.0), content = move || {
                                                    Character(
                                                        &m_agent,
                                                        m_leader,
                                                        28.0,
                                                        &m_name,
                                                        &s_proj,
                                                        CharState::from_status(m_status),
                                                        None,
                                                    );
                                                    StatusDot(m_status);
                                                }) };
                                        },
                                    );
                                }
                            }
                        }) };
                }) };
        }) };
}

/// How close to the end still counts as "at the end", in dp: about one line of the transcript.
pub const FOLLOW_NEAR_END_DP: f32 = 24.0;

/// Where a pane's transcript scrolls to when its scrollable distance changes, the usual chat
/// rule (NFR-L3, #770): to the end on the first layout, and when new content arrives only if
/// the view was at the end (within [`FOLLOW_NEAR_END_DP`]). A reader who scrolled up keeps the
/// position. `prev_max` and `prev_offset` are the distance and the offset before the change
/// (both 0 before the first layout). `None`: leave the position alone. Pure, for tests.
///
/// The first-layout half is what makes the screen the same on every OS. The tail slice below
/// is cut by the window height, and the toolkit reports a height of 0 before its first size
/// event and runs a composable again only when the size class changes. A window that opens
/// compact (macOS at 360 dp) therefore kept the whole transcript and showed its oldest
/// message, while a phone, whose size is known first, showed the newest.
pub fn follow_newest(max_offset: f32, prev_max: f32, prev_offset: f32) -> Option<f32> {
    if max_offset <= 0.0 || max_offset == prev_max {
        return None;
    }
    let first_layout = prev_max <= 0.0;
    let was_at_end = prev_offset >= prev_max - FOLLOW_NEAR_END_DP;
    (first_layout || was_at_end).then_some(max_offset)
}

/// Applies [`follow_newest`] to `scroll`. It draws nothing; it is a composable of its own so
/// that reading the offset runs only this function again while the user scrolls.
#[compose_rust::runtime::Composable]
fn FollowNewest(scroll: cu::ScrollState, key: String) {
    let offset = scroll.offset();
    let max_offset = scroll.max_offset();
    // (distance, offset) as of the last pass.
    let seen = remember_with_key(key, || mutable_state_of((0.0f32, 0.0f32)));
    compose_rust::runtime::side_effect(move || {
        let (prev_max, prev_offset) = seen.get_untracked();
        let to = follow_newest(max_offset, prev_max, prev_offset);
        seen.set((max_offset, to.unwrap_or(offset)));
        if let Some(to) = to {
            scroll.scroll_to(to);
        }
    });
}

/// The transcript of one pane, with approvals (reuses `ConversationView`'s `TranscriptRow`).
pub fn PaneTranscript(session: &str, agent: &str) {
    let ui = shared_ui().clone();
    let window = current_window_size();
    let p = ember_theme().palette;
    let sid = session.to_string();
    let agent_name = agent.to_string();
    let expanded = remember_with_key(sid.clone(), || mutable_state_of(HashSet::<String>::new()));
    let answering = remember_with_key(sid.clone(), || mutable_state_of(HashSet::<String>::new()));
    // The name of the member's computer, for the approval card's title (#536: a pane's card
    // read 이 컴퓨터에서 because no name was passed).
    let computer = remember_with_key(format!("{sid}:pane-computer"), || mutable_state_of(None::<String>));
    {
        let (sid_load, computer_load) = (sid.clone(), computer.clone());
        launched_effect(sid.clone(), move || async move {
            load_session_computer(sid_load, computer_load);
        });
    }
    let computer_name = computer.get();

    let items = {
        let _ = ui.live.transcripts.get();
        services()
            .client
            .read(|st| st.transcript(&sid).map(|t| t.items.clone()).unwrap_or_default())
    };
    let count = items.len();

    // Follow the newest message with the toolkit's scroll state (compose-rust#385, closed).
    let scroll = cu::remember_scroll_state();
    FollowNewest(scroll.clone(), sid.clone());

    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().weight(1.0), content = move || {
            if count == 0 {
                { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().fill_max_width().fill_max_height(), {
                        Label(
                            NO_CONVERSATION,
                            Fs::Sm,
                            Weight::Regular,
                            Some(paint(p.muted)),
                            None,
                        );
                    }) };
            } else {
                let avail_h = window.height_dp
                    - 28.0
                    - 60.0
                    - 44.0
                    - 48.0
                    - 56.0
                    - (if window.width_dp <= 560.0 { 164.0 } else { 0.0 });
                let exp_set = expanded.get();
                let start_idx = tail_start_index(&items, &exp_set, avail_h);
                let items_c = items[start_idx..].to_vec();
                let sid_c = sid.clone();
                let agent_c = agent_name.clone();
                let exp_c = expanded.clone();
                let ans_c = answering.clone();
                let computer_c = computer_name.clone();
                { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(scroll_state = &scroll, modifier = Modifier::new().fill_max_width().fill_max_height(), content = move || {
                        { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().fill_max_width().padding(SP[1]), content = move || {
                                for item in items_c.iter() {
                                    TranscriptRow(&sid_c, &agent_c, item, &exp_c, &ans_c, computer_c.as_deref(), true);
                                }
                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(12.0)) };
                            }) };
                    }) };
            }
        }) };
}

/// The Terminal side of a pane: the member's terminal, read-only, in the mockup's `.term` colours
/// (FR-L18). Outside the demo the client has no stream to attach to for a member, so it says so.
///
/// v0.0.7: #5 fixed (`SelectionContainer`); workaround kept until checked on a display (the
/// terminal rows are a styled grid, and a selection over them must be tried before wrapping it).
/// New output is followed with the toolkit's scroll state, as the transcript is
/// (compose-rust#385, `FollowNewest`): the pane opens at its newest line and stays there unless
/// the reader scrolled up.
/// Outside the demo it attaches as a read-only viewer to the project's one running agent terminal on
/// the session's computer, and detaches when the pane leaves the screen (the terminal keeps running).
pub fn PaneTerminal(member: &Member, project: &str) {
    use crate::terminal_stream::{start_view_with, TermReach, TerminalHandle};
    use crate::ui::term_pane::{self as tp, TermRole, TermView};
    let p = ember_theme().palette;
    let demo = services().demo.is_some();
    let sid = member.session_id.clone();
    let computer = remember_with_key(sid.clone(), || mutable_state_of(None::<String>));
    let revision = remember_with_key(sid.clone(), || mutable_state_of(0u64));
    let handle = remember_with_key(sid.clone(), || Rc::new(RefCell::new(None::<TerminalHandle>)));
    if !demo {
        {
            let (sid, computer) = (sid.clone(), computer.clone());
            launched_effect(sid.clone(), move || async move {
                let (s_id, c_st) = (sid.clone(), computer.clone());
                run_shared(
                    async move { services().server.session_computer(&s_id).await.map_err(|e| e.to_string()) },
                    move |r: Result<CurrentComputer, String>| {
                        if let Ok(c) = r {
                            c_st.set(Some(c.computer.id));
                        }
                    },
                );
            });
        }
        let computer_id = computer.get();
        let (handle_start, project_start) = (handle.clone(), project.to_string());
        disposable_effect((sid.clone(), computer_id.clone()), move || {
            if let Some(id) = &computer_id {
                // Over the transport when the server is a peer (FR-N9, #867), else over HTTP as before.
                *handle_start.borrow_mut() = Some(start_view_with(TermReach::of_app(), id, &project_start, None));
            }
            let handle_stop = handle_start.clone();
            on_dispose(move || {
                if let Some(h) = handle_stop.borrow_mut().take() {
                    h.detach();
                }
            })
        });
        // The stream runs on a worker; read its revision here, on the UI thread, and recompose on change.
        let (handle_poll, revision_poll) = (handle.clone(), revision.clone());
        launched_effect(sid.clone(), move || async move {
            loop {
                delay(std::time::Duration::from_millis(250)).await;
                let current = handle_poll.borrow().as_ref().map(TerminalHandle::revision).unwrap_or(0);
                if current != revision_poll.get() {
                    revision_poll.set(current);
                }
            }
        });
    }
    let _ = revision.get();
    let attached = handle.borrow().clone();
    let state = attached.as_ref().map(TerminalHandle::snapshot);
    let pick = attached.as_ref().and_then(TerminalHandle::pick);
    let view = if demo {
        tp::view_for_member(true, None, &member.agent, &member.agent, project, &member.task)
    } else if computer.get().is_none() {
        TermView::Connecting
    } else {
        tp::view_for_attached(state.as_ref(), pick.as_ref())
    };
    let note = |text: &str| {
        let text = text.to_string();
        { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().fill_max_width().weight(1.0), content = move || {
                Label(&text, Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
            }) };
    };
    // Follow the newest line with the toolkit's scroll state (compose-rust#385), as `PaneTranscript` does.
    let scroll = cu::remember_scroll_state();
    FollowNewest(scroll.clone(), format!("{sid}:terminal"));
    match view {
        TermView::Connecting => note(tp::CONNECTING),
        TermView::NoTerminal => note(tp::NO_TERMINAL),
        TermView::Note(text) => note(text),
        TermView::Failed(text) => note(text),
        TermView::Lines { rows, notice } => {
            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                        .fill_max_width()
                        .weight(1.0)
                        .background(paint(p.surface_2))
                        .border(1.0, paint(p.line))
                        .corner_radius(CORNER_SM), content = move || {
                    { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(scroll_state = &scroll, modifier = Modifier::new().fill_max_width().fill_max_height(), content = move || {
                            { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width().padding(SP[2]), content = move || {
                                    for (role, line) in rows.iter() {
                                        let (color, weight) = match role {
                                            TermRole::Banner | TermRole::Warn => (p.orange.strong, None),
                                            TermRole::User => (p.text, Some(600.0)),
                                            TermRole::Agent => (p.text, None),
                                            TermRole::Tool | TermRole::Selected => (p.blue.strong, None),
                                            TermRole::Add => (p.green.strong, None),
                                            TermRole::Del => (p.red.strong, None),
                                            TermRole::Muted => (p.muted, None),
                                            TermRole::Plain => (p.text_2, None),
                                        };
                                        // An empty line still takes a row.
                                        let text = if line.is_empty() { " " } else { line.as_str() };
                                        Txt(text, TypeRole::Mono, Some(paint(color)), None, None, weight, None);
                                    }
                                    if let Some(n) = &notice {
                                        Txt(n, TypeRole::Mono, Some(paint(p.muted)), None, None, None, None);
                                    }
                                }) };
                        }) };
                }) };
        }
    }
}

/// A pane's own composer: Enter sends, a message while the agent is busy is queued (FR-L6).
pub fn PaneComposer(session: &str, busy: bool) {
    let ui = shared_ui().clone();
    let window = current_window_size();
    let p = ember_theme().palette;
    let sid = session.to_string();
    let draft = remember_with_key(sid.clone(), || mutable_state_of(String::new()));
    let generation = remember_with_key(sid.clone(), || mutable_state_of(0_u64));
    let hide_chips = window.width_dp < 1150.0;
    let short_model = window.width_dp < 1560.0;

    let send = {
        let sid = sid.clone();
        let draft = draft.clone();
        let generation = generation.clone();
        move |text: String| {
            let text = text.trim().to_string();
            if text.is_empty() {
                return;
            }
            let s = services();
            s.outbox.lock().unwrap().push(&sid, text);
            s.bump_outbox();
            s.drive_outbox(&sid);
            draft.set(String::new());
            generation.update(|g| *g += 1);
        }
    };

    let queued = {
        let _ = ui.live.outbox.get();
        services().outbox.lock().unwrap().queued(&sid)
    };

    let _gen = generation.get();
    { use ::compose_rust::ui::Column; Column!(spacing = SP[0], modifier = Modifier::new().fill_max_width(), content = move || {
            { use ::compose_rust::ui::Separator; Separator!(Some(paint(p.line))) };
            if queued > 0 {
                { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width().padding(SP[0]), {
                        Pill(&format!("{queued}개 대기 중입니다"), PillTone::Blue, true);
                    }) };
            }
            // 1-row .pane-c footer: [ModelChip] [ModeChip] [Input] [Stop/Send circle]
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width().padding(SP[0]), {
                    if !hide_chips {
                        ModelChip(&sid, short_model);
                        ModeChip(&sid, short_model);
                    }
                    let draft_ch = draft.clone();
                    let send_sub = send.clone();
                    EmberField(FieldOptions {
                        weight: Some(1.0),
                        multiline: false,
                        height: Some(34.0),
                        background: Some(paint(p.surface_2)),
                        border_width: Some(1.0),
                        border_color: Some(paint(p.line)),
                        corner_radius: Some(17.0),
                        placeholder: if busy {
                            "대기열에 넣기".to_string()
                        } else {
                            "메시지".to_string()
                        },
                        on_value_change: Some(Box::new(move |v| draft_ch.set(v))),
                        on_submit: Some(Box::new(move |v| send_sub(v))),
                        ..Default::default()
                    });
                    if busy {
                        let sid_int = sid.clone();
                        StopCircleButton(move || {
                            let id_c = sid_int.clone();
                            run_shared(
                                async move {
                                    services()
                                        .client
                                        .interrupt(&id_c)
                                        .await
                                        .map_err(|e| e.to_string())
                                },
                                |r| {
                                    if let Err(e) = r {
                                        notice::post(format!("중단하지 못했습니다: {e}"));
                                    }
                                },
                            );
                        });
                    } else {
                        let send_btn = send.clone();
                        let draft_btn = draft.clone();
                        SendCircleButton(30.0, move || {
                            send_btn(draft_btn.get_untracked());
                        });
                    }
                }) };
        }) };
}

/// Width of one pane in the multi view: the window less the nav, the page padding on both sides
/// and the gaps between `columns` panes. An unknown window (0 dp on this toolkit version at 390)
/// gives 0, which counts as narrow (FR-L18, #585).
pub fn pane_width(window_dp: f32, columns: usize, nav_dp: f32) -> f32 {
    if window_dp <= 0.0 {
        return 0.0;
    }
    let n = columns.max(1) as f32;
    let body = window_dp - nav_dp - 2.0 * status_page_pad(window_dp);
    ((body - SP[2] * (n - 1.0)) / n).max(0.0)
}

/// Whether the pane header shows the single icon toggle (`.ph-mode`) instead of the 대화/터미널
/// segment (`.ph-seg`): at 420 dp and below, or when the width is unknown (mockup
/// `@container pane (max-width:420px)`, FR-L18, #585).
pub fn pane_header_uses_icon_toggle(pane_w: f32) -> bool {
    pane_w <= 0.0 || pane_w <= 420.0
}

/// Compact segmented switcher inside a pane header (`.ph-seg`).
fn MiniSeg(cur: PaneToggle, on_pick: impl Fn(PaneToggle) + Clone + 'static) {
    let p = ember_theme().palette;
    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::Center, modifier = Modifier::new()
                .height(26.0)
                .background(paint(p.surface))
                .border(1.0, paint(p.line))
                .corner_radius(13.0)
                .padding(2.0), content = move || {
            for t in PaneToggle::ALL {
                let selected = cur == t;
                let (bg, fg) = if selected {
                    (paint(p.ink), p.ink_text)
                } else {
                    (paint(Color::argb(0)), p.muted)
                };
                let cb = on_pick.clone();
                Clickable(
                    Modifier::new().height(22.0).background(bg).corner_radius(11.0),
                    Alignment::Center,
                    move || cb(t),
                    move || {
                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::Center, {
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(8.0)) };
                            Label(
                                t.label(),
                                Fs::Xs,
                                if selected { Weight::Semibold } else { Weight::Regular },
                                Some(paint(fg)),
                                Some(1),
                            );
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(8.0)) };
                        }) };
                    },
                );
            }
        }) };
}

/// One pane of the multi view: header, transcript with approvals, composer.
pub fn Pane(
    project: &str,
    member: &Member,
    all: &[String],
    order: &[String],
    on_enlarge: impl Fn(String) + Clone + 'static,
) {
    let ui = shared_ui().clone();
    let window = current_window_size();
    let p = ember_theme().palette;
    let id = member.session_id.clone();
    let proj_owned = project.to_string();
    let member_owned = member.clone();
    let all_owned = all.to_vec();
    let order_owned = order.to_vec();
    let narrow_pane = window.width_dp < 1150.0;
    let pane_cols = if window.width_dp >= 700.0 { 2 } else { 1 };
    let nav_dp = match crate::ui::shell::nav_mode(window.width_dp, false) {
        crate::ui::shell::ShellMode::BottomTabs => 0.0,
        crate::ui::shell::ShellMode::Rail => RAIL_W,
        crate::ui::shell::ShellMode::Sidebar => SIDE_W,
    };
    let icon_toggle =
        pane_header_uses_icon_toggle(pane_width(window.width_dp, pane_cols, nav_dp));

    let toggle = remember_with_key(id.clone(), || mutable_state_of(PaneToggle::Conversation));
    let menu = remember_with_key(id.clone(), || mutable_state_of(false));

    {
        let id_load = id.clone();
        launched_effect(id.clone(), move || async move {
            run_shared(
                async move {
                    services()
                        .client
                        .load_approve_all(&id_load)
                        .await
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                },
                |r| {
                    if let Err(e) = r {
                        tracing::debug!("approve-all: {e}");
                    }
                },
            );
        });
    }

    let (label, tone) = status_pill(member.status);
    let busy = crate::model::is_busy(member.status);
    let can_left = can_move(&order_owned, &id, PaneMove::Left);
    let can_right = can_move(&order_owned, &id, PaneMove::Right);

    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().fill_max_height(), content = move || {
            let cur_toggle = toggle.get();
            { use ::compose_rust::ui::Column; Column!(space_role = SpaceRole::Xs, modifier = Modifier::new()
                        .fill_max_width()
                        .fill_max_height()
                        .background(paint(p.surface))
                        .border(1.0, paint(p.line))
                        .corner_radius(CORNER_LG)
                        .padding(SP[1]), {
                    // Header row (.pane-h)
                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Xs, modifier = Modifier::new().fill_max_width(), {
                            let m_open = menu.clone();
                            Clickable(
                                Modifier::new().size(18.0, 28.0),
                                Alignment::Center,
                                move || m_open.set(true),
                                || NavIcon(ICON_GRIP_SVG, p.muted.to_argb(), 14.0),
                            );
                            Character(
                                &member_owned.agent,
                                member_owned.leader,
                                if narrow_pane { 24.0 } else { 28.0 },
                                &member_owned.name,
                                &proj_owned,
                                CharState::from_status(member_owned.status),
                                None,
                            );
                            { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().weight(1.0), {
                                    Label(
                                        &member_owned.accessible_name(),
                                        Fs::Sm,
                                        Weight::Semibold,
                                        None,
                                        Some(1),
                                    );
                                    Label(
                                        &member_owned.task,
                                        Fs::Xs,
                                        Weight::Regular,
                                        Some(paint(p.muted)),
                                        Some(1),
                                    );
                                }) };
                            if narrow_pane {
                                StatusDot(member_owned.status);
                            } else {
                                Pill(label, tone.into(), true);
                            }
                            let tg_seg = toggle.clone();
                            if icon_toggle {
                                // One icon button (`.ph-mode`): shows the view it opens, so the
                                // title keeps the remaining width. Its accessible name is
                                // `switch_label()`, a fixed set (compose-rust v0.0.7, #460).
                                let to = cur_toggle.other();
                                NamedClickable(
                                    to.switch_label(),
                                    Modifier::new().size(28.0, 28.0).corner_radius(CORNER_SM),
                                    Alignment::Center,
                                    move || tg_seg.set(to),
                                    move || {
                                        let icon = match to {
                                            PaneToggle::Terminal => ICON_TERMINAL_SVG,
                                            PaneToggle::Conversation => ICON_CHAT_SVG,
                                        };
                                        NavIcon(icon, p.muted.to_argb(), 14.0)
                                    },
                                );
                            } else {
                                MiniSeg(cur_toggle, move |t| tg_seg.set(t));
                            }

                            let enl = on_enlarge.clone();
                            let id_enl = id.clone();
                            Clickable(
                                Modifier::new().size(28.0, 28.0).corner_radius(CORNER_SM),
                                Alignment::Center,
                                move || enl(id_enl.clone()),
                                || NavIcon(ICON_EXPAND_SVG, p.muted.to_argb(), 14.0),
                            );

                            let ui_close = ui.clone();
                            let proj_close = proj_owned.clone();
                            let all_close = all_owned.clone();
                            let id_close = id.clone();
                            Clickable(
                                Modifier::new().size(28.0, 28.0).corner_radius(CORNER_SM),
                                Alignment::Center,
                                move || {
                                    let (all_c, id_c) = (all_close.clone(), id_close.clone());
                                    edit_panes(&ui_close, &proj_close, move |pp| {
                                        close_pane(pp.active_tab_mut(), &all_c, &id_c);
                                    });
                                },
                                || NavIcon(ICON_CLOSE_SVG, p.muted.to_argb(), 14.0),
                            );
                        }) };
                    { use ::compose_rust::ui::Separator; Separator!(Some(paint(p.line))) };

                    if cur_toggle == PaneToggle::Conversation {
                        PaneTranscript(&id, &member_owned.agent);
                        PaneComposer(&id, busy);
                    } else {
                        PaneTerminal(&member_owned, &proj_owned);
                    }
                }) };

            // Reorder OptionSheet
            let mut move_opts = Vec::new();
            let mut move_dirs = Vec::new();
            if can_left {
                move_opts.push("왼쪽으로 이동".to_string());
                move_dirs.push(PaneMove::Left);
            }
            if can_right {
                move_opts.push("오른쪽으로 이동".to_string());
                move_dirs.push(PaneMove::Right);
            }
            if move_opts.is_empty() {
                move_opts.push("이동할 수 있는 위치가 없습니다".to_string());
            }
            let m_d = menu.clone();
            let ui_mv = ui.clone();
            let proj_mv = proj_owned.clone();
            let all_mv = all_owned.clone();
            let id_mv = id.clone();
            OptionSheet(
                menu.get(),
                &move_opts,
                None,
                POPUP_W,
                move || m_d.set(false),
                move |idx| {
                    if let Some(&dir) = move_dirs.get(idx) {
                        let (all_c, id_c) = (all_mv.clone(), id_mv.clone());
                        edit_panes(&ui_mv, &proj_mv, move |pp| {
                            reorder_pane(pp.active_tab_mut(), &all_c, &id_c, dir);
                        });
                    }
                },
            );
        }) };
}
/// The corners of a pane tab's click area (`[top_start, top_end, bottom_end, bottom_start]`),
/// so the hover layer, the press tint and the keyboard focus ring follow the tab and not a
/// square inside it (FR-L34.4, #251). Alone, the click area is the whole tab and takes its
/// `CORNER_MD` at all four corners. With a close button after it, the click area ends where the
/// close button starts, inside the tab's straight part, so it is rounded at the start only and
/// square at that end.
pub fn pane_tab_click_radii(closable: bool) -> [f32; 4] {
    if closable {
        [CORNER_MD, 0.0, 0.0, CORNER_MD]
    } else {
        [CORNER_MD; 4]
    }
}

/// One tab of the multi view's tab strip (`.stab`): 34 dp high with the `CORNER_MD` corner, its
/// name and pane count, and a 24 dp round close button when there is more than one tab. The
/// current tab is `surface` inside a 1 dp `line` outline.
fn PaneTabChip(
    name: &str,
    count: usize,
    selected: bool,
    closable: bool,
    on_select: impl FnMut() + 'static,
    on_close: impl FnMut() + 'static,
) {
    let p = ember_theme().palette;
    let (bg, fg, wt) = if selected {
        (paint(p.surface), p.text, Weight::Semibold)
    } else {
        (paint(Color::argb(0)), p.text_2, Weight::Medium)
    };
    let mut tab_mod = Modifier::new().height(34.0).background(bg).corner_radius(CORNER_MD);
    if selected {
        tab_mod = tab_mod.border(1.0, paint(p.line));
    }
    let tab_name = name.to_string();
    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::Center, modifier = tab_mod, content = move || {
        Clickable(
            Modifier::new().fill_max_height().corner_radii(pane_tab_click_radii(closable)),
            Alignment::Center,
            on_select,
            move || {
                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 6.0, {
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(10.0)) };
                    Label(&tab_name, Fs::Sm, wt, Some(paint(fg)), Some(1));
                    { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().size(20.0, 18.0).background(paint(p.surface_2)).corner_radius(9.0), {
                            Label(&count.to_string(), Fs::Xs2, Weight::Medium, Some(paint(p.muted)), Some(1));
                        }) };
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(if closable { 2.0 } else { 10.0 })) };
                }) };
            },
        );
        if closable {
            Clickable(Modifier::new().size(24.0, 24.0).shape_role(ShapeRole::Full), Alignment::Center, on_close, || {
                NavIcon(ICON_CLOSE_SVG, p.muted.to_argb(), 11.0)
            });
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(4.0)) };
        }
    }) };
}

/// Panes of the project's active tab, with the tab strip (FR-L18).
pub fn MultiView(
    project: &str,
    members: &[Member],
    focus: Option<String>,
    on_enlarge: impl Fn(String) + Clone + 'static,
) {
    let ui = shared_ui().clone();
    let window = current_window_size();
    let p = ember_theme().palette;
    let layout = multi_pane_layout(window.width_dp, window.class);
    let proj_owned = project.to_string();
    let members_vec = members.to_vec();

    let page = remember_with_key(proj_owned.clone(), || mutable_state_of(0_usize));
    let add_menu = remember_with_key(proj_owned.clone(), || mutable_state_of(false));

    let all: Vec<String> = members_vec.iter().map(|m| m.session_id.clone()).collect();
    let panes = panes_of(&ui, &proj_owned);
    let vis = visible_panes(panes.active_tab(), &all);
    use_leases_cr(&vis, focus.as_deref());

    let hidden: Vec<Member> = members_vec
        .iter()
        .filter(|m| !vis.contains(&m.session_id))
        .cloned()
        .collect();
    let n = vis.len();
    let show_help = window.width_dp >= 1280.0;

    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().fill_max_height(), content = move || {
            { use ::compose_rust::ui::Column; Column!(space_role = SpaceRole::Sm, modifier = Modifier::new()
                        .fill_max_width()
                        .fill_max_height()
                        .padding_role(SpaceRole::Sm), {
                    // Tab strip (.stabs)
                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Xs, modifier = Modifier::new().fill_max_width(), {
                            { use ::compose_rust::ui::ScrollRow; ScrollRow!(modifier = Modifier::new().weight(1.0), {
                                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 6.0, {
                                            let multi_tabs = panes.tabs.len() > 1;
                                            for (i, t) in panes.tabs.iter().enumerate() {
                                                let pg = page.clone();
                                                let u = ui.clone();
                                                let pr = proj_owned.clone();
                                                let (pg_s, u_s, pr_s) = (pg.clone(), u.clone(), pr.clone());
                                                PaneTabChip(
                                                    &t.name,
                                                    visible_panes(t, &all).len(),
                                                    i == panes.active,
                                                    multi_tabs,
                                                    move || {
                                                        pg_s.set(0);
                                                        edit_panes(&u_s, &pr_s, move |pp| {
                                                            pp.select(i)
                                                        });
                                                    },
                                                    move || {
                                                        pg.set(0);
                                                        edit_panes(&u, &pr, move |pp| {
                                                            pp.remove_tab(i)
                                                        });
                                                    },
                                                );
                                            }
                                            // Circular .stab-add "+" button (30x30)
                                            {
                                                let pg = page.clone();
                                                let u = ui.clone();
                                                let pr = proj_owned.clone();
                                                let vis_add = vis.clone();
                                                Clickable(
                                                    Modifier::new()
                                                        .size(30.0, 30.0)
                                                        .border(1.0, paint(p.line))
                                                        .shape_role(ShapeRole::Full),
                                                    Alignment::Center,
                                                    move || {
                                                        pg.set(0);
                                                        let v = vis_add.clone();
                                                        edit_panes(&u, &pr, move |pp| pp.add_tab(&v));
                                                    },
                                                    || NavIcon(ICON_PLUS_SVG, p.muted.to_argb(), 15.0),
                                                );
                                            }
                                            if !hidden.is_empty() {
                                                let am = add_menu.clone();
                                                PickChip(
                                                    "팀원 추가",
                                                    Some(IconRole::Add),
                                                    false,
                                                    false,
                                                    move || am.set(true),
                                                );
                                            }
                                        }) };
                                }) };
                            if show_help {
                                Label(
                                    "칸 머리를 끌어 옮길 수 있습니다. 가운데에 놓으면 바꾸고, 가장자리에 놓으면 나눕니다",
                                    Fs::Xs,
                                    Weight::Regular,
                                    Some(paint(p.muted)),
                                    Some(1),
                                );
                            }
                        }) };

                    if n == 0 {
                        { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().fill_max_width().weight(1.0), {
                                { use ::compose_rust::ui::Column; Column!(alignment = Alignment::Center, space_role = SpaceRole::Sm, {
                                        Label(NO_MEMBERS, Fs::Lg, Weight::Medium, None, Some(1));
                                        Label(
                                            PICK_FROM_LIST,
                                            Fs::Md,
                                            Weight::Regular,
                                            Some(paint(p.muted)),
                                            Some(1),
                                        );
                                    }) };
                            }) };
                    } else if layout.paged {
                        // Sideways strip, one pane per screen width (mockup `.mv` at 560 px,
                        // `scroll-snap-type:x mandatory`), instead of a previous / next pager.
                        // Each slot is a direct child of the ScrollRow and as wide as the strip,
                        // so `Snap::ItemStart` (compose-rust#547) brings a swipe to rest on one
                        // whole pane. Half a gap on each side of a slot keeps panes a gap apart.
                        let pane_w = swipe_pane_width(window.width_dp);
                        { use ::compose_rust::ui::ScrollRow; ScrollRow!(snap = Snap::ItemStart, modifier = Modifier::new().fill_max_width().weight(1.0), {
                                for sid in &vis {
                                    if let Some(m) = members_vec.iter().find(|m| &m.session_id == sid)
                                    {
                                        let m = m.clone();
                                        let pr = proj_owned.clone();
                                        let all_s = all.clone();
                                        let vis_s = vis.clone();
                                        let enl = on_enlarge.clone();
                                        { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().width(pane_w).fill_max_height(), content = move || {
                                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SWIPE_GAP / 2.0)) };
                                                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(1.0).fill_max_height(), content = move || {
                                                        Pane(&pr, &m, &all_s, &vis_s, enl.clone());
                                                    }) };
                                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SWIPE_GAP / 2.0)) };
                                            }) };
                                    }
                                }
                            }) };
                    } else {
                        let rows_idx = pane_rows(n, layout.per_row);
                        let proj_grid = proj_owned.clone();
                        let members_grid = members_vec.clone();
                        let all_grid = all.clone();
                        let vis_grid = vis.clone();
                        let enl_grid = on_enlarge.clone();
                        if rows_idx.len() <= 2 {
                            // Full-height flex grid filling the remaining vertical space (.sgrid-multi)
                            { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width().weight(1.0), content = move || {
                                    for row in rows_idx.iter() {
                                        let row_c = row.clone();
                                        let pr = proj_grid.clone();
                                        let ms = members_grid.clone();
                                        let al = all_grid.clone();
                                        let vs = vis_grid.clone();
                                        let enl = enl_grid.clone();
                                        { use ::compose_rust::ui::Row; Row!(spacing = SP[2], modifier = Modifier::new().fill_max_width().weight(1.0), content = move || {
                                                for idx in row_c.iter().copied() {
                                                    let pr_p = pr.clone();
                                                    let al_p = al.clone();
                                                    let vs_p = vs.clone();
                                                    let enl_p = enl.clone();
                                                    let m_opt = ms
                                                        .iter()
                                                        .find(|m| m.session_id == vs[idx])
                                                        .cloned();
                                                    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                                                                .weight(1.0)
                                                                .fill_max_height(), content = move || {
                                                            if let Some(m) = m_opt {
                                                                Pane(
                                                                    &pr_p, &m, &al_p, &vs_p, enl_p,
                                                                );
                                                            }
                                                        }) };
                                                }
                                                for _ in 0..(layout.per_row - row_c.len()) {
                                                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                                                }
                                            }) };
                                    }
                                }) };
                        } else {
                            { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().fill_max_width().weight(1.0), content = move || {
                                    { use ::compose_rust::ui::Column; Column!(space_role = SpaceRole::Sm, modifier = Modifier::new().fill_max_width(), content = move || {
                                            for row in rows_idx.iter() {
                                                let row_c = row.clone();
                                                let pr = proj_grid.clone();
                                                let ms = members_grid.clone();
                                                let al = all_grid.clone();
                                                let vs = vis_grid.clone();
                                                let enl = enl_grid.clone();
                                                { use ::compose_rust::ui::Row; Row!(space_role = SpaceRole::Sm, modifier = Modifier::new().fill_max_width(), content = move || {
                                                        for idx in row_c.iter().copied() {
                                                            let pr_p = pr.clone();
                                                            let al_p = al.clone();
                                                            let vs_p = vs.clone();
                                                            let enl_p = enl.clone();
                                                            let m_opt = ms
                                                                .iter()
                                                                .find(|m| m.session_id == vs[idx])
                                                                .cloned();
                                                            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                                                                        .weight(1.0)
                                                                        .height(PANE_H), content = move || {
                                                                    if let Some(m) = m_opt {
                                                                        Pane(
                                                                            &pr_p, &m, &al_p,
                                                                            &vs_p, enl_p,
                                                                        );
                                                                    }
                                                                }) };
                                                        }
                                                        for _ in 0..(layout.per_row - row_c.len()) {
                                                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                                                        }
                                                    }) };
                                            }
                                        }) };
                                }) };
                        }
                    }
                }) };

            // Add-member OptionSheet
            if !hidden.is_empty() {
                let labels: Vec<String> = hidden.iter().map(|m| m.accessible_name()).collect();
                let am_d = add_menu.clone();
                let hidden_c = hidden.clone();
                let u_add = ui.clone();
                let pr_add = proj_owned.clone();
                let all_add = all.clone();
                OptionSheet(
                    add_menu.get(),
                    &labels,
                    None,
                    POPUP_W,
                    move || am_d.set(false),
                    move |idx| {
                        if let Some(m) = hidden_c.get(idx) {
                            let (al, id) = (all_add.clone(), m.session_id.clone());
                            edit_panes(&u_add, &pr_add, move |pp| {
                                add_pane(pp.active_tab_mut(), &al, &id);
                            });
                        }
                    },
                );
            }
        }) };
}

fn env_row_icon(key: &str) -> &'static str {
    match key {
        "컴퓨터" => ICON_PC_SVG,
        "작업 경로" => ICON_FOLDER_SVG,
        "에이전트" | "모델" | "계정" => ICON_GIT_SVG,
        _ => ICON_SHIELD_SVG,
    }
}

/// The right panel of the single view (environment, context, outputs, changes).
pub fn InfoPanel(session: &str) {
    let ui = shared_ui().clone();
    let p = ember_theme().palette;
    let sid = session.to_string();
    let computer = remember_with_key(sid.clone(), || mutable_state_of(None::<String>));
    let collapsed = remember_with_key(sid.clone(), || mutable_state_of(HashSet::<usize>::new()));
    let is_demo = services().demo.is_some();

    {
        let sid_load = sid.clone();
        let comp_load = computer.clone();
        launched_effect(sid.clone(), move || async move {
            if services().demo.is_some() {
                comp_load.set(Some("mac-mini".to_string()));
                return;
            }
            let s_id = sid_load.clone();
            let c_st = comp_load.clone();
            run_shared(
                async move {
                    services()
                        .server
                        .session_computer(&s_id)
                        .await
                        .map_err(|e| e.to_string())
                },
                move |r: Result<CurrentComputer, String>| {
                    if let Ok(c) = r {
                        c_st.set(Some(c.computer.name));
                    }
                },
            );
        });
    }

    let (rec, approve_all) = {
        let _ = ui.live.launcher.get();
        services().client.read(|s| {
            (
                s.session(&sid).map(|v| v.record.clone()),
                s.approve_all(&sid),
            )
        })
    };
    let comp_now = computer.get();
    let rows = rec
        .as_ref()
        .map(|r| environment_rows(r, comp_now.as_deref(), approve_all))
        .unwrap_or_default();
    let agent_str = rec
        .as_ref()
        .map(|r| r.agent.clone())
        .unwrap_or_else(|| "claude-code".to_string());
    let model_str = model_chip_label(
        &agent_str,
        rec.as_ref().and_then(|r| r.model.as_deref()),
    );
    let mode_str = mode_chip_label(approve_all);
    let comp_str = comp_now.unwrap_or_else(|| "mac-mini".to_string());

    { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new()
                .fill_max_width()
                .fill_max_height()
                .background(paint(p.bg))
                .padding_role(SpaceRole::Sm), content = move || {
            { use ::compose_rust::ui::Column; Column!(space_role = SpaceRole::Sm, modifier = Modifier::new().fill_max_width(), content = move || {
                    for (idx, card) in InfoCard::ALL.into_iter().enumerate() {
                        let rows_c = rows.clone();
                        let is_off = collapsed.get().contains(&idx);
                        let col_toggle = collapsed.clone();
                        let ag_c = agent_str.clone();
                        let md_c = model_str.clone();
                        let comp_c = comp_str.clone();
                        let right_summary = match card {
                            InfoCard::Environment => "",
                            InfoCard::Context if is_demo => "31%",
                            InfoCard::Outputs if is_demo => "2",
                            InfoCard::Changes if is_demo => "+23 -6",
                            _ => "",
                        };
                        let title_text = match card {
                            InfoCard::Context if is_demo => "컨텍스트 구성",
                            _ => card.label(),
                        };

                        EmberCard(false, true, None, Some(SP[2]), None, move || {
                            { use ::compose_rust::ui::Column; Column!(spacing = 6.0, modifier = Modifier::new().fill_max_width(), content = move || {
                                    Clickable(
                                        Modifier::new().fill_max_width(),
                                        Alignment::CenterStart,
                                        move || {
                                            col_toggle.update(|set| {
                                                if !set.remove(&idx) {
                                                    set.insert(idx);
                                                }
                                            });
                                        },
                                        move || {
                                            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 6.0, modifier = Modifier::new().fill_max_width(), {
                                                    Label(
                                                        title_text,
                                                        Fs::Sm,
                                                        Weight::Semibold,
                                                        None,
                                                        Some(1),
                                                    );
                                                    NavIcon(
                                                        if is_off {
                                                            ICON_CHEVRON_RIGHT_SVG
                                                        } else {
                                                            ICON_CHEVRON_DOWN_SVG
                                                        },
                                                        p.muted.to_argb(),
                                                        12.0,
                                                    );
                                                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                                                    if !right_summary.is_empty() {
                                                        Label(
                                                            right_summary,
                                                            Fs::Xs,
                                                            Weight::Medium,
                                                            Some(paint(p.muted)),
                                                            Some(1),
                                                        );
                                                    }
                                                }) };
                                        },
                                    );
                                    if !is_off {
                                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(2.0)) };
                                        match card {
                                            InfoCard::Environment if is_demo => {
                                                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width().padding(4.0), content = move || {
                                                        NavIcon(ICON_PC_SVG, p.muted.to_argb(), 16.0);
                                                        Label(&comp_c, Fs::Sm, Weight::Regular, Some(paint(p.text)), Some(1));
                                                        Label("· macOS 26", Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
                                                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                                                        Label("192.168.0.10", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                                                    }) };
                                                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width().padding(4.0), {
                                                        NavIcon(ICON_FOLDER_SVG, p.muted.to_argb(), 16.0);
                                                        Label("DarkPyonix/darkpyonix-ember", Fs::Sm, Weight::Regular, Some(paint(p.text)), Some(1));
                                                    }) };
                                                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width().padding(4.0), {
                                                        NavIcon(ICON_GIT_SVG, p.muted.to_argb(), 16.0);
                                                        Label("feat/shim-wiring", Fs::Sm, Weight::Regular, Some(paint(p.text)), Some(1));
                                                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                                                        Pill("worktree", PillTone::Grey, true);
                                                    }) };
                                                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width().padding(4.0), content = move || {
                                                        Character(&ag_c, false, 18.0, "에이전트", "demo", CharState::Run, None);
                                                        Label(&md_c, Fs::Sm, Weight::Regular, Some(paint(p.text)), Some(1));
                                                    }) };
                                                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width().padding(4.0), content = move || {
                                                        NavIcon(ICON_SHIELD_SVG, p.muted.to_argb(), 16.0);
                                                        Label(&format!("{mode_str} · Claude 개인"), Fs::Sm, Weight::Regular, Some(paint(p.text)), Some(1));
                                                    }) };
                                            }
                                            InfoCard::Environment => {
                                                for (k, v) in rows_c.iter() {
                                                    let ico = env_row_icon(k);
                                                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width().padding(4.0), content = move || {
                                                            NavIcon(ico, p.muted.to_argb(), 16.0);
                                                            Label(
                                                                k,
                                                                Fs::Sm,
                                                                Weight::Regular,
                                                                Some(paint(p.muted)),
                                                                Some(1),
                                                            );
                                                            { use ::compose_rust::ui::Column; Column!(alignment = Alignment::CenterEnd, modifier = Modifier::new().weight(1.0), {
                                                                    Label(
                                                                        v,
                                                                        Fs::Sm,
                                                                        Weight::Regular,
                                                                        None,
                                                                        Some(1),
                                                                    );
                                                                }) };
                                                        }) };
                                                }
                                            }
                                            InfoCard::Context if is_demo => {
                                                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width(), {
                                                        Label("31.2% 사용됨", Fs::Sm, Weight::Semibold, Some(paint(p.text)), Some(1));
                                                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                                                        Label("312.0k / 1M", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                                                    }) };
                                                { use ::compose_rust::ui::Row; Row!(spacing = 1.0, modifier = Modifier::new()
                                                            .fill_max_width()
                                                            .height(6.0)
                                                            .background(paint(p.surface_2))
                                                            .corner_radius(3.0), {
                                                        { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(24.4).fill_max_height().background(paint(p.blue.strong))) };
                                                        { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(2.8).fill_max_height().background(paint(p.sky.strong))) };
                                                        { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(4.0).fill_max_height().background(paint(p.orange.strong))) };
                                                        { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(68.8).fill_max_height()) };
                                                    }) };
                                                for (col, lbl, tok, pct) in [
                                                    (p.blue.strong, "메시지", "244.0k", "24.4%"),
                                                    (p.sky.strong, "시스템 도구", "18.0k", "1.8%"),
                                                    (p.green.strong, "스킬", "6.0k", "0.6%"),
                                                    (p.orange.strong, "자동 압축 버퍼", "33.0k", "3.3%"),
                                                    (p.line, "남은 공간", "688.0k", "68.8%"),
                                                ] {
                                                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 6.0, modifier = Modifier::new().fill_max_width().padding(2.0), content = move || {
                                                            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().size(10.0, 10.0).background(paint(col)).corner_radius(3.0)) };
                                                            Label(lbl, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(1));
                                                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                                                            Label(tok, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                                                            Label(pct, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
                                                        }) };
                                                }
                                            }
                                            InfoCard::Outputs if is_demo => {
                                                for (ico, lbl, kind) in [
                                                    (ICON_GIT_SVG, "#139 Wire shim into proxy", "PR"),
                                                    (ICON_GLOBE_SVG, "http://192.168.0.10:3000", "웹 페이지"),
                                                ] {
                                                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width().padding(4.0), content = move || {
                                                            NavIcon(ico, p.muted.to_argb(), 15.0);
                                                            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(1.0), content = move || {
                                                                Label(lbl, Fs::Sm, Weight::Regular, Some(paint(p.text)), Some(1));
                                                            }) };
                                                            Label(kind, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                                                        }) };
                                                }
                                            }
                                            InfoCard::Changes if is_demo => {
                                                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 6.0, modifier = Modifier::new().fill_max_width().padding(2.0), {
                                                        NavIcon(ICON_EDIT_SVG, p.muted.to_argb(), 15.0);
                                                        Label("+23", Fs::Sm, Weight::Medium, Some(paint(p.green.strong)), Some(1));
                                                        Label("-6", Fs::Sm, Weight::Medium, Some(paint(p.red.strong)), Some(1));
                                                        Label("· 파일 3개", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                                                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                                                        Pill("PR #139 보기", PillTone::Grey, true);
                                                    }) };
                                                for (f_path, add_s, del_s) in [
                                                    ("ember/proxy/src/shim.rs", "+14", "-3"),
                                                    ("ember/proxy/src/router.rs", "+6", "-2"),
                                                    ("docs/SPEC.md", "+3", "-1"),
                                                ] {
                                                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 6.0, modifier = Modifier::new().fill_max_width().padding(2.0), content = move || {
                                                            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(1.0), content = move || {
                                                                Label(f_path, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(1));
                                                            }) };
                                                            Label(add_s, Fs::Xs, Weight::Medium, Some(paint(p.green.strong)), Some(1));
                                                            Label(del_s, Fs::Xs, Weight::Medium, Some(paint(p.red.strong)), Some(1));
                                                        }) };
                                                }
                                            }
                                            _ => {
                                                Label(
                                                    SOON,
                                                    Fs::Sm,
                                                    Weight::Regular,
                                                    Some(paint(p.muted)),
                                                    None,
                                                );
                                            }
                                        }
                                    }
                                }) };
                        });
                    }
                }) };
        }) };
}

/// One member's conversation with the right panel (inline on expanded, a sheet elsewhere).
pub fn SingleView(session: &str, focus: Option<String>, on_back: impl Fn() + Clone + 'static) {
    let ui = shared_ui().clone();
    let window = current_window_size();
    let p = ember_theme().palette;
    let sid = session.to_string();
    let env_info = std::env::var("EMBER_CR_SESSION_INFO").ok();
    let info_open = remember_with_key(sid.clone(), || {
        mutable_state_of(env_info.as_deref() == Some("1"))
    });
    let panel_visible = remember_with_key(sid.clone(), || {
        mutable_state_of(env_info.as_deref() != Some("0"))
    });
    use_leases_cr(std::slice::from_ref(&sid), focus.as_deref());
    let wide = window.class == compose_rust::WindowSizeClass::Expanded;

    let member_info = {
        let _ = ui.live.launcher.get();
        services().client.read(|s| {
            let boxes = project_boxes(s);
            boxes
                .into_iter()
                .flat_map(|b| {
                    let pid = b.id.clone();
                    b.members.into_iter().map(move |m| (pid.clone(), m))
                })
                .find(|(_, m)| m.session_id == sid)
        })
    };

    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().fill_max_height(), content = move || {
            let sid_row = sid.clone();
            let sid_p = sid.clone();
            let sid_sh = sid.clone();
            let info_btn = info_open.clone();
            let pv_btn = panel_visible.clone();
            let show_inline = wide && panel_visible.get();
            let back_cb = on_back.clone();
            let m_opt = member_info.clone();

            { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width().fill_max_height(), content = move || {
                    { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().weight(1.0).fill_max_height(), content = move || {
                            // `.topbar` in `.detail` (`grid-template-rows:56px`): 56 high, 20 in
                            // from each side, nothing above or below. The 52 dp row with 12 dp
                            // padding all round left 28 dp, so the 32 dp back square drew 32x28
                            // at x 12 (review r7, conversation-single-390).
                            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new()
                                        .fill_max_width()
                                        .height(super::theme::TOPBAR_H)
                                        .background(paint(p.surface)), content = move || {
                                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(super::theme::TOPBAR_PAD_X)) };
                                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Sm, modifier = Modifier::new().weight(1.0), content = move || {
                                    let b_cb = back_cb.clone();
                                    BackButton(move || b_cb());
                                    if let Some((proj_id, m)) = m_opt {
                                        let (st_txt, st_tone) = status_pill(m.status);
                                        Character(
                                            &m.agent,
                                            m.leader,
                                            24.0,
                                            &m.name,
                                            &proj_id,
                                            CharState::from_status(m.status),
                                            None,
                                        );
                                        // The status pill sits right after the name, as the
                                        // mockup's; a long name is cut with an ellipsis so the
                                        // pill and the panel button keep their room (a long name
                                        // pushed the button past the edge at 390; review r8: a
                                        // name filling the row pushed the pill to the far end).
                                        NameThenPill(
                                            &m.accessible_name(),
                                            Fs::Md,
                                            Weight::Semibold,
                                            Some(paint(p.text)),
                                            SP[1],
                                            || Pill(st_txt, st_tone.into(), true),
                                        );
                                    } else {
                                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                                    }
                                    let io = info_btn.clone();
                                    let pv = pv_btn.clone();
                                    IconButton(IconRole::Sidebar, true, move || {
                                        if wide {
                                            pv.update(|v| *v = !*v);
                                        } else {
                                            io.set(true);
                                        }
                                    });
                                        }) };
                                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(super::theme::TOPBAR_PAD_X)) };
                                }) };
                            { use ::compose_rust::ui::Separator; Separator!(Some(paint(p.line))) };
                            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(1.0).fill_max_width(), content = move || {
                                    ConversationView(&sid_row, true);
                                }) };
                        }) };
                    if show_inline {
                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new()
                                .width(1.0)
                                .fill_max_height()
                                .background(paint(p.line))) };
                        { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().width(INFO_PANEL_W).fill_max_height(), content = move || {
                                InfoPanel(&sid_p);
                            }) };
                    }
                }) };

            if !wide {
                let io_d = info_open.clone();
                EmberSheet(
                    "세션 정보",
                    info_open.get(),
                    move || io_d.set(false),
                    None,
                    move || {
                        { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().height(PANE_H), content = move || {
                                InfoPanel(&sid_sh);
                            }) };
                    },
                );
            }
        }) };
}

fn KpiCard(title: &str, value: &str, sub: &str, tone: Option<Color>) {
    let p = ember_theme().palette;
    let t = title.to_string();
    let v = value.to_string();
    let s = sub.to_string();
    let v_col = tone.unwrap_or(p.text);
    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                .weight(1.0)
                .height(100.0)
                .background(paint(p.surface))
                .border(1.0, paint(p.line))
                .corner_radius(CORNER_LG)
                .padding(SP[2]), content = move || {
            { use ::compose_rust::ui::Column; Column!(spacing = 4.0, modifier = Modifier::new().fill_max_width(), content = move || {
                    Label(&t, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(1));
                    Label(&v, Fs::Xl2, Weight::Regular, Some(paint(v_col)), Some(1));
                    Label(&s, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                }) };
        }) };
}

fn KanbanColView(
    col_lbl: &'static str,
    ms: Vec<Member>,
    proj: String,
    width_opt: Option<f32>,
    weight_opt: Option<f32>,
    on_open: impl Fn(String) + Clone + 'static,
) {
    let p = ember_theme().palette;
    let mut m = Modifier::new()
        .background(paint(p.surface_2))
        .border(1.0, paint(p.line))
        .corner_radius(CORNER_LG)
        .padding(SP[2]);
    if let Some(w) = width_opt {
        m = m.width(w);
    }
    if let Some(wg) = weight_opt {
        m = m.weight(wg);
    }
    { use ::compose_rust::ui::Column; Column!(space_role = SpaceRole::Xs, modifier = m, content = move || {
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Sm, modifier = Modifier::new().fill_max_width(), {
                    Label(
                        col_lbl,
                        Fs::Sm,
                        Weight::Semibold,
                        Some(paint(p.text_2)),
                        Some(1),
                    );
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                    Pill(&ms.len().to_string(), PillTone::Grey, true);
                }) };
            if ms.is_empty() {
                Label(
                    "없음",
                    Fs::Xs,
                    Weight::Regular,
                    Some(paint(p.muted)),
                    Some(1),
                );
            }
            for m in ms.iter() {
                let m_id = m.session_id.clone();
                let m_agent = m.agent.clone();
                let m_leader = m.leader;
                let m_name = m.name.clone();
                let m_acc = m.accessible_name();
                let m_task = m.task.clone();
                let m_status = m.status;
                let m_meta = format!("{} · 이 컴퓨터", m.agent);
                let pr_card = proj.clone();
                let op_card = on_open.clone();
                EmberCard(false, true, None, Some(SP[2]), None, move || {
                    let op_click = op_card.clone();
                    let id_click = m_id.clone();
                    Clickable(
                        Modifier::new().fill_max_width(),
                        Alignment::CenterStart,
                        move || op_click(id_click.clone()),
                        move || {
                            { use ::compose_rust::ui::Column; Column!(space_role = SpaceRole::Xs, modifier = Modifier::new().fill_max_width(), content = move || {
                                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Xs, modifier = Modifier::new().fill_max_width(), content = move || {
                                            Character(
                                                &m_agent,
                                                m_leader,
                                                22.0,
                                                &m_name,
                                                &pr_card,
                                                CharState::from_status(m_status),
                                                None,
                                            );
                                            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(1.0), content = move || {
                                                    Label(
                                                        &m_acc,
                                                        Fs::Sm,
                                                        Weight::Semibold,
                                                        Some(paint(p.text)),
                                                        Some(1),
                                                    );
                                                }) };
                                            StatusDot(m_status);
                                        }) };
                                    Label(
                                        &m_task,
                                        Fs::Xs,
                                        Weight::Regular,
                                        Some(paint(p.text_2)),
                                        Some(2),
                                    );
                                    Label(
                                        &m_meta,
                                        Fs::Xs,
                                        Weight::Regular,
                                        Some(paint(p.muted)),
                                        Some(1),
                                    );
                                }) };
                        },
                    );
                });
            }
        }) };
}

#[derive(Clone, Copy)]
enum TlSegKind {
    Gap,
    Think,
    Tool,
    Wait,
    Fail,
}

struct TlLane {
    name: &'static str,
    leader: bool,
    agent: &'static str,
    sub: &'static str,
    status: LauncherStatus,
    tone: fn(&Palette) -> (Color, Color),
    segs: &'static [(f32, TlSegKind)],
}

const DEMO_TL_LANES: [TlLane; 5] = [
    TlLane {
        name: "리더",
        leader: true,
        agent: "claude-code",
        sub: "claude-code · opus-5.5",
        status: LauncherStatus::Running,
        tone: |p| (p.blue.strong, p.blue.soft),
        segs: &[
            (2.2, TlSegKind::Think),
            (1.6, TlSegKind::Tool),
            (2.8, TlSegKind::Wait),
            (2.0, TlSegKind::Think),
            (1.4, TlSegKind::Tool),
            (3.4, TlSegKind::Wait),
            (2.0, TlSegKind::Think),
            (1.0, TlSegKind::Tool),
        ],
    },
    TlLane {
        name: "포트 설정 추가",
        leader: false,
        agent: "claude-code",
        sub: "claude-code · sonnet-5.0",
        status: LauncherStatus::WaitingForApproval,
        tone: |p| (p.orange.strong, p.orange.soft),
        segs: &[
            (2.4, TlSegKind::Gap),
            (2.4, TlSegKind::Think),
            (3.6, TlSegKind::Tool),
            (1.8, TlSegKind::Think),
            (6.2, TlSegKind::Wait),
        ],
    },
    TlLane {
        name: "README 정리",
        leader: false,
        agent: "codex",
        sub: "codex · gpt-5",
        status: LauncherStatus::Finished,
        tone: |p| (p.green.strong, p.green.soft),
        segs: &[
            (2.8, TlSegKind::Gap),
            (2.0, TlSegKind::Think),
            (4.2, TlSegKind::Tool),
            (2.2, TlSegKind::Think),
            (2.4, TlSegKind::Tool),
            (2.8, TlSegKind::Gap),
        ],
    },
    TlLane {
        name: "라우터 점검",
        leader: false,
        agent: "claude-code",
        sub: "claude-code · sonnet-5.0",
        status: LauncherStatus::Running,
        tone: |p| (p.sky.strong, p.sky.soft),
        segs: &[
            (3.8, TlSegKind::Gap),
            (2.6, TlSegKind::Think),
            (3.8, TlSegKind::Tool),
            (2.4, TlSegKind::Think),
            (3.8, TlSegKind::Tool),
        ],
    },
    TlLane {
        name: "빌드 검증",
        leader: false,
        agent: "codex",
        sub: "codex · gpt-5",
        status: LauncherStatus::Idle,
        tone: |p| (p.grey.strong, p.grey.soft),
        segs: &[
            (6.8, TlSegKind::Gap),
            (2.2, TlSegKind::Think),
            (3.0, TlSegKind::Tool),
            (1.6, TlSegKind::Fail),
            (2.8, TlSegKind::Think),
        ],
    },
];

fn hex_rgb(c: Color) -> String {
    format!("#{:06x}", c.to_argb() & 0x00ff_ffff)
}

fn tl_plot_svg(w: f32, p: &Palette) -> String {
    let pw = w.max(200.0).round() as i32;
    let ph = 280;
    let t_max = 16.4_f32;
    let x_of = |t: f32| -> f32 { (t / t_max) * (pw as f32) };

    let surf = hex_rgb(p.surface);
    let line = hex_rgb(p.line);
    let red_s = hex_rgb(p.red.strong);
    let red_bg = hex_rgb(p.red.soft);

    let mut s = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{pw}" height="{ph}" viewBox="0 0 {pw} {ph}" fill="none">"#
    );

    // Hatched diagonal patterns per lane color
    s.push_str("<defs>");
    for (idx, lane) in DEMO_TL_LANES.iter().enumerate() {
        let (st, _) = (lane.tone)(p);
        let col = hex_rgb(st);
        s.push_str(&format!(
            r#"<pattern id="hw{idx}" width="6" height="6" patternTransform="rotate(45 0 0)" patternUnits="userSpaceOnUse"><line x1="0" y1="0" x2="0" y2="6" stroke="{col}" stroke-width="1.8" stroke-opacity="0.55"/></pattern>"#
        ));
    }
    s.push_str("</defs>");

    // Vertical dashed grid lines at 0s, 5s, 10s, 15s
    for tick_t in [0.0_f32, 5.0, 10.0, 15.0] {
        let tx = x_of(tick_t).clamp(0.5, (pw as f32) - 0.5);
        s.push_str(&format!(
            r#"<line x1="{tx:.1}" y1="0" x2="{tx:.1}" y2="{ph}" stroke="{line}" stroke-width="1" stroke-dasharray="3 3"/>"#
        ));
    }

    // Horizontal row dividers between lanes
    for row in 1..5 {
        let ry = row * 56;
        s.push_str(&format!(
            r#"<line x1="0" y1="{ry}" x2="{pw}" y2="{ry}" stroke="{line}" stroke-width="1"/>"#
        ));
    }

    // Segments per lane
    for (idx, lane) in DEMO_TL_LANES.iter().enumerate() {
        let (st_col, sf_col) = (lane.tone)(p);
        let st_hex = hex_rgb(st_col);
        let sf_hex = hex_rgb(sf_col);
        let y_top = (idx as i32) * 56 + 17;
        let mut cur_t = 0.0_f32;
        for &(dur, kind) in lane.segs.iter() {
            let x0 = x_of(cur_t) + 1.0;
            let seg_w = (x_of(cur_t + dur) - x0 - 1.0).max(4.0);
            cur_t += dur;
            match kind {
                TlSegKind::Gap => {}
                TlSegKind::Think => {
                    s.push_str(&format!(
                        r#"<rect x="{x0:.1}" y="{y_top}" width="{seg_w:.1}" height="22" rx="7" fill="{sf_hex}" stroke="{st_hex}" stroke-width="1"/>"#
                    ));
                }
                TlSegKind::Tool => {
                    s.push_str(&format!(
                        r#"<rect x="{x0:.1}" y="{y_top}" width="{seg_w:.1}" height="22" rx="7" fill="{st_hex}"/>"#
                    ));
                }
                TlSegKind::Wait => {
                    s.push_str(&format!(
                        r#"<rect x="{x0:.1}" y="{y_top}" width="{seg_w:.1}" height="22" rx="7" fill="url(#hw{idx})" stroke="{st_hex}" stroke-width="1" stroke-opacity="0.65"/>"#
                    ));
                }
                TlSegKind::Fail => {
                    let cx = x0 + seg_w * 0.5;
                    let cy = (y_top as f32) + 11.0;
                    s.push_str(&format!(
                        r#"<rect x="{x0:.1}" y="{y_top}" width="{seg_w:.1}" height="22" rx="7" fill="{red_bg}" stroke="{red_s}" stroke-width="1"/>"#
                    ));
                    s.push_str(&format!(
                        r#"<path d="M{:.1} {:.1}L{:.1} {:.1}M{:.1} {:.1}L{:.1} {:.1}" stroke="{red_s}" stroke-width="1.8" stroke-linecap="round"/>"#,
                        cx - 3.5,
                        cy - 3.5,
                        cx + 3.5,
                        cy + 3.5,
                        cx + 3.5,
                        cy - 3.5,
                        cx - 3.5,
                        cy + 3.5
                    ));
                }
            }
        }
    }

    // Handoff connector lines between leader (lane 0) and member lanes (1..=4)
    let handoffs: [(usize, f32); 8] = [
        (1, 2.4),
        (2, 2.8),
        (3, 3.8),
        (4, 6.8),
        (1, 10.2),
        (2, 13.6),
        (4, 13.6),
        (3, 16.2),
    ];
    for (target_lane, t_sec) in handoffs {
        let hx = x_of(t_sec).clamp(4.0, (pw as f32) - 4.0);
        let y0 = 28.0 + 9.0;
        let y1 = (target_lane as f32) * 56.0 + 28.0 - 9.0;
        let (st_col, _) = (DEMO_TL_LANES[target_lane].tone)(p);
        let col = hex_rgb(st_col);
        s.push_str(&format!(
            r#"<line x1="{hx:.1}" y1="{y0:.1}" x2="{hx:.1}" y2="{y1:.1}" stroke="{col}" stroke-width="1.5"/>"#
        ));
        s.push_str(&format!(
            r#"<circle cx="{hx:.1}" cy="{y0:.1}" r="3.5" fill="{col}" stroke="{surf}" stroke-width="1.5"/>"#
        ));
        s.push_str(&format!(
            r#"<circle cx="{hx:.1}" cy="{y1:.1}" r="3.5" fill="{col}" stroke="{surf}" stroke-width="1.5"/>"#
        ));
    }

    s.push_str("</svg>");
    s
}

/// Deterministic agent-to-agent communication Gantt chart (`.tlcard` in mockup).
fn TimelineCard(project: &str) {
    let window = current_window_size();
    let p = ember_theme().palette;
    let proj_owned = project.to_string();
    let speed = remember(|| mutable_state_of(1_u8));
    let playback = remember(|| mutable_state_of(TimelinePlayback::new()));
    let pb = playback.get();
    let cur_spd = speed.get();
    let total_events = DEMO_TL_LANES.len();
    let active_lane = pb.active_index(total_events);
    if pb.playing {
        let pb_tick = playback.clone();
        let step_ms = if cur_spd >= 2 { 450 } else { 900 };
        launched_effect((pb.playing, pb.cursor, cur_spd), move || async move {
            delay(std::time::Duration::from_millis(step_ms)).await;
            let mut next = pb_tick.get_untracked();
            if next.playing {
                next.step_next(total_events);
                pb_tick.set(next);
            }
        });
    }
    let is_phone = window.width_dp <= 560.0;
    let label_w = if is_phone {
        56.0
    } else if window.width_dp < 1280.0 {
        170.0
    } else {
        220.0
    };
    let outer_w = if is_phone {
        window.width_dp - 24.0
    } else if window.width_dp <= 900.0 {
        window.width_dp - 64.0 - 40.0
    } else if window.width_dp < 1280.0 {
        window.width_dp - 72.0 - 280.0 - 40.0
    } else {
        window.width_dp - 234.0 - 280.0 - 40.0
    };
    let plot_w = (outer_w - label_w - 24.0).max(220.0);
    let plot_svg = tl_plot_svg(plot_w, p);
    let plot_id = compose_rust::asset(AssetKind::Svg, leak_doc(plot_svg));

    let wait_swatch_svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="18" height="10" viewBox="0 0 18 10" fill="none"><defs><pattern id="ws" width="5" height="5" patternTransform="rotate(45 0 0)" patternUnits="userSpaceOnUse"><line x1="0" y1="0" x2="0" y2="5" stroke="{}" stroke-width="1.5" stroke-opacity="0.6"/></pattern></defs><rect x="0.5" y="0.5" width="17" height="9" rx="2.5" fill="url(#ws)" stroke="{}" stroke-width="1" stroke-opacity="0.65"/></svg>"#,
        hex_rgb(p.blue.strong),
        hex_rgb(p.blue.strong)
    );
    let wait_swatch_id = compose_rust::asset(AssetKind::Svg, leak_doc(wait_swatch_svg));

    let hand_swatch_svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10" viewBox="0 0 10 10" fill="none"><path d="M1.5 1V6.5C1.5 7.6 2.4 8.5 3.5 8.5H9" stroke="{}" stroke-width="1.5" stroke-linecap="round"/></svg>"#,
        hex_rgb(p.text_2)
    );
    let hand_swatch_id = compose_rust::asset(AssetKind::Svg, leak_doc(hand_swatch_svg));

    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                .fill_max_width()
                .background(paint(p.surface))
                .border(1.0, paint(p.line))
                .corner_radius(CORNER_LG), content = move || {
            { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), content = move || {
                    // 1. Header (.tl-head: ~73dp high)
                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[2], modifier = Modifier::new().fill_max_width().height(72.0).padding(SP[3]), {
                            { use ::compose_rust::ui::Column; Column!(spacing = 4.0, modifier = Modifier::new().weight(1.0), {
                                    Label(
                                        TIMELINE_TITLE,
                                        Fs::Lg,
                                        Weight::Semibold,
                                        Some(paint(p.text)),
                                        Some(1),
                                    );
                                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 6.0, {
                                            NavIcon(ICON_CHECK_SVG, p.green.strong.to_argb(), 15.0);
                                            Label(
                                                timeline_summary(services().demo.is_some()),
                                                Fs::Xs,
                                                Weight::Regular,
                                                Some(paint(p.muted)),
                                                Some(1),
                                            );
                                        }) };
                                }) };
                        }) };
                    { use ::compose_rust::ui::Separator; Separator!(Some(paint(p.line))) };

                    // 2. Body (.tl-body): left labels column + right SVG Gantt plot + time axis
                    { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), content = move || {
                            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::TopStart, modifier = Modifier::new().fill_max_width().height(280.0), content = move || {
                                    // Left labels column (.tl-labs)
                                    { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().width(label_w).fill_max_height(), content = move || {
                                            for (lane_idx, lane) in DEMO_TL_LANES.iter().enumerate()
                                            {
                                                let pr_l = proj_owned.clone();
                                                let (strong_col, _) = (lane.tone)(p);
                                                let row_mod = if active_lane == Some(lane_idx) {
                                                    Modifier::new()
                                                        .fill_max_width()
                                                        .height(55.0)
                                                        .background(paint(p.surface_2))
                                                        .padding(SP[1])
                                                } else {
                                                    Modifier::new()
                                                        .fill_max_width()
                                                        .height(55.0)
                                                        .padding(SP[1])
                                                };
                                                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 8.0, modifier = row_mod, content = move || {
                                                        Character(
                                                            lane.agent,
                                                            lane.leader,
                                                            28.0,
                                                            lane.name,
                                                            &pr_l,
                                                            CharState::from_status(lane.status),
                                                            None,
                                                        );
                                                        if !is_phone {
                                                            { use ::compose_rust::ui::Column; Column!(spacing = 2.0, modifier = Modifier::new().weight(1.0), content = move || {
                                                                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 5.0, content = move || {
                                                                            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                                                                                    .size(8.0, 8.0)
                                                                                    .background(
                                                                                        paint(
                                                                                            strong_col,
                                                                                        ),
                                                                                    )
                                                                                    .corner_radius(
                                                                                        4.0,
                                                                                    )) };
                                                                            Label(
                                                                                lane.name,
                                                                                Fs::Sm,
                                                                                Weight::Semibold,
                                                                                Some(paint(p.text)),
                                                                                Some(1),
                                                                            );
                                                                            if lane.leader {
                                                                                Pill(
                                                                                    "오케스트레이터",
                                                                                    PillTone::Grey,
                                                                                    true,
                                                                                );
                                                                            }
                                                                        }) };
                                                                    Label(
                                                                        lane.sub,
                                                                        Fs::Xs,
                                                                        Weight::Regular,
                                                                        Some(paint(p.muted)),
                                                                        Some(1),
                                                                    );
                                                                }) };
                                                        }
                                                    }) };
                                                if lane_idx + 1 < DEMO_TL_LANES.len() {
                                                    { use ::compose_rust::ui::Separator; Separator!(Some(paint(p.line))) };
                                                }
                                            }
                                        }) };
                                    // Vertical separator between label and plot
                                    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                                            .width(1.0)
                                            .fill_max_height()
                                            .background(paint(p.line))) };
                                    // Right Gantt plot area (.tl-lanes + .tl-h + .tl-grid)
                                    { use ::compose_rust::ui::Box; Box!(alignment = Alignment::CenterStart, modifier = Modifier::new()
                                                .weight(1.0)
                                                .fill_max_height()
                                                .padding(SP[0]), content = move || {
                                            { use ::compose_rust::ui::Image; Image!(plot_id, modifier = Modifier::new().size(plot_w, 280.0)) };
                                        }) };
                                }) };
                            { use ::compose_rust::ui::Separator; Separator!(Some(paint(p.line))) };
                            // Time axis row (.tl-axis)
                            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width().height(26.0), content = move || {
                                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(label_w)) };
                                    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                                            .width(1.0)
                                            .fill_max_height()
                                            .background(paint(p.line))) };
                                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new()
                                                .weight(1.0)
                                                .fill_max_height()
                                                .padding(SP[0]), {
                                            for tick in ["0s", "5s", "10s", "15s"] {
                                                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(1.0), content = move || {
                                                        Label(
                                                            tick,
                                                            Fs::Xs2,
                                                            Weight::Regular,
                                                            Some(paint(p.muted)),
                                                            Some(1),
                                                        );
                                                    }) };
                                            }
                                            Label(
                                                "16.4s",
                                                Fs::Xs2,
                                                Weight::Regular,
                                                Some(paint(p.muted)),
                                                Some(1),
                                            );
                                        }) };
                                }) };
                        }) };
                    { use ::compose_rust::ui::Separator; Separator!(Some(paint(p.line))) };

                    // 3. Footer (.tl-foot: ~72dp high): play (40x40), restart (32x32), speed 1x/2x, time, legend
                    let spd_st = speed.clone();
                    let pb_play = playback.clone();
                    let pb_rst = playback.clone();
                    let play_icon = if pb.playing { ICON_PAUSE_SVG } else { ICON_PLAY_SVG };
                    let play_label = pb.action_label();
                    let step_label = format!(
                        "{}/{} \u{b7} 16.4s",
                        active_lane.map(|i| i + 1).unwrap_or(0),
                        total_events
                    );
                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[2], modifier = Modifier::new()
                                .fill_max_width()
                                .height(72.0)
                                .background(paint(p.surface_2))
                                .padding(SP[3]), content = move || {
                            NamedClickable(
                                play_label,
                                Modifier::new()
                                    .size(40.0, 40.0)
                                    .background(paint(p.ink))
                                    .shape_role(ShapeRole::Full),
                                Alignment::Center,
                                move || {
                                    let mut next = pb_play.get_untracked();
                                    next.toggle_play(total_events);
                                    pb_play.set(next);
                                },
                                move || NavIcon(play_icon, p.ink_text.to_argb(), 16.0),
                            );
                            NamedClickable(
                                "처음부터 재생",
                                Modifier::new()
                                    .size(32.0, 32.0)
                                    .background(paint(p.surface))
                                    .border(1.0, paint(p.line))
                                    .shape_role(ShapeRole::Full),
                                Alignment::Center,
                                move || {
                                    let mut next = pb_rst.get_untracked();
                                    next.restart();
                                    pb_rst.set(next);
                                },
                                || NavIcon(ICON_REPEAT_SVG, p.text_2.to_argb(), 15.0),
                            );
                            // 1x / 2x mini segmented control
                            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::Center, modifier = Modifier::new()
                                        .height(26.0)
                                        .background(paint(p.surface))
                                        .border(1.0, paint(p.line))
                                        .corner_radius(13.0)
                                        .padding(2.0), content = move || {
                                    for (s_val, s_lbl) in [(1_u8, "1x"), (2_u8, "2x")] {
                                        let sel = cur_spd == s_val;
                                        let (bg, fg) = if sel {
                                            (paint(p.ink), p.ink_text)
                                        } else {
                                            (paint(Color::argb(0)), p.muted)
                                        };
                                        let sp_c = spd_st.clone();
                                        Clickable(
                                            Modifier::new()
                                                .height(22.0)
                                                .background(bg)
                                                .corner_radius(11.0),
                                            Alignment::Center,
                                            move || sp_c.set(s_val),
                                            move || {
                                                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::Center, {
                                                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(8.0)) };
                                                        Label(
                                                            s_lbl,
                                                            Fs::Xs,
                                                            Weight::Semibold,
                                                            Some(paint(fg)),
                                                            Some(1),
                                                        );
                                                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(8.0)) };
                                                    }) };
                                            },
                                        );
                                    }
                                }) };
                            Label(
                                &step_label,
                                Fs::Xs,
                                Weight::Medium,
                                Some(paint(p.muted)),
                                Some(1),
                            );
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                            if !is_phone {
                                // 5-item legend (.tl-legend)
                                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[3], content = move || {
                                        // 1. 생각
                                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 6.0, {
                                                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                                                        .size(18.0, 10.0)
                                                        .background(paint(p.blue.soft))
                                                        .border(1.0, paint(p.blue.strong))
                                                        .corner_radius(3.0)) };
                                                Label(
                                                    "생각",
                                                    Fs::Xs,
                                                    Weight::Regular,
                                                    Some(paint(p.text_2)),
                                                    Some(1),
                                                );
                                            }) };
                                        // 2. 도구 호출
                                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 6.0, {
                                                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                                                        .size(18.0, 10.0)
                                                        .background(paint(p.blue.strong))
                                                        .corner_radius(3.0)) };
                                                Label(
                                                    "도구 호출",
                                                    Fs::Xs,
                                                    Weight::Regular,
                                                    Some(paint(p.text_2)),
                                                    Some(1),
                                                );
                                            }) };
                                        // 3. 대기 (hatched)
                                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 6.0, content = move || {
                                                { use ::compose_rust::ui::Image; Image!(wait_swatch_id, modifier = Modifier::new().size(18.0, 10.0)) };
                                                Label(
                                                    "대기",
                                                    Fs::Xs,
                                                    Weight::Regular,
                                                    Some(paint(p.text_2)),
                                                    Some(1),
                                                );
                                            }) };
                                        // 4. 핸드오프 (L-shaped)
                                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 6.0, content = move || {
                                                { use ::compose_rust::ui::Image; Image!(hand_swatch_id, modifier = Modifier::new().size(10.0, 10.0)) };
                                                Label(
                                                    "핸드오프",
                                                    Fs::Xs,
                                                    Weight::Regular,
                                                    Some(paint(p.text_2)),
                                                    Some(1),
                                                );
                                            }) };
                                        // 5. 실패 (12x12 circle)
                                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 6.0, {
                                                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                                                        .size(12.0, 12.0)
                                                        .background(paint(p.red.soft))
                                                        .border(1.0, paint(p.red.strong))
                                                        .corner_radius(6.0)) };
                                                Label(
                                                    "실패",
                                                    Fs::Xs,
                                                    Weight::Regular,
                                                    Some(paint(p.text_2)),
                                                    Some(1),
                                                );
                                            }) };
                                    }) };
                            }
                        }) };
                }) };
        }) };
}

fn tone_pill(t: StatusTone) -> PillTone {
    match t {
        StatusTone::Green => PillTone::Green,
        StatusTone::Red => PillTone::Red,
        StatusTone::Blue => PillTone::Blue,
        StatusTone::Orange => PillTone::Orange,
        StatusTone::Sky => PillTone::Sky,
    }
}

fn tone_color(t: StatusTone, p: &Palette) -> Color {
    match t {
        StatusTone::Green => p.green.strong,
        StatusTone::Red => p.red.strong,
        StatusTone::Blue => p.blue.strong,
        StatusTone::Orange => p.orange.strong,
        StatusTone::Sky => p.sky.strong,
    }
}

/// Everything the three bottom cards draw, from the pure model.
#[derive(Clone)]
struct StatusCardsData {
    project: String,
    prs: Vec<PrRow>,
    tree: Vec<SubNode>,
    tokens: Vec<TokenRow>,
    computers: Vec<ComputerRow>,
    counts: Vec<String>,
    compact: bool,
}

/// `.card` with a `.card-h` (icon tile, title, trailing, 1 px divider) and a padded body.
/// `weighted` is true inside a row, where the card takes an equal share.
fn StatusCardShell(
    weighted: bool,
    ico: &'static str,
    title: &str,
    trailing: impl FnOnce(),
    body: impl FnOnce(),
) {
    let p = ember_theme().palette;
    let title = title.to_string();
    let (fill, weight) = if weighted { (false, Some(1.0)) } else { (true, None) };
    // A weighted card sits in a multi-column row, which is `equal_height`; it fills that height.
    EmberCardSized(false, fill, weighted, weight, Some(0.0), None, move || {
        { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), content = move || {
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(13.0)) };
            { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width(), content = move || {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[4])) };
                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(1.0), content = move || {
                    CardHead(ico, &title, trailing);
                }) };
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[4])) };
            }) };
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(13.0)) };
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().fill_max_width().height(1.0).background(paint(p.line))) };
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(SP[3])) };
            { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width(), content = move || {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[4])) };
                { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().weight(1.0), content = body) };
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[4])) };
            }) };
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(SP[3])) };
        }) };
    });
}

fn EmptyText(text: &str) {
    let p = ember_theme().palette;
    Label(text, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
}

/// `.list-row`: the 36 px git tile, `#num title` over `member · branch`, and the CI pill over
/// the review state. The row divider is a 1 dp line inside the card body's padding, where the
/// mockup's `.list-row` top border reaches the card edges, and the right column is not
/// right-aligned. Neither waits on the toolkit any more (one-sided borders arrived with
/// compose-rust#464, v0.0.9); moving the divider out of the padding means laying the body out
/// without `StatusCardShell`'s inset, which is left for a display check.
fn PrRowView(row: PrRow, first: bool) {
    let p = ember_theme().palette;
    // The divider sits between rows with 12 dp above and below, clear of the right column.
    { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), content = move || {
        if !first {
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(SP[2])) };
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().fill_max_width().height(1.0).background(paint(p.line))) };
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(SP[2])) };
        }
        { use ::compose_rust::ui::Row; Row!(spacing = SP[2], alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width(), content = move || {
            { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new()
                        .size(36.0, 36.0)
                        .background(paint(p.surface_2))
                        .border(1.0, paint(p.line))
                        .corner_radius(CORNER_SM), content = || NavIcon(ICON_GIT_SVG, p.text_2.to_argb(), 18.0)) };
            { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().weight(1.0), {
                Label(&row.headline(), Fs::Md, Weight::Medium, None, Some(1));
                Label(&row.sub_line(), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
            }) };
            { use ::compose_rust::ui::Column; Column!(spacing = SP[0], {
                Pill(row.ci.label(), tone_pill(row.ci.tone()), false);
                Label(&row.review, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
            }) };
        }) };
    }) };
}

/// `.tnode`: an avatar for a member, a spinner or a status dot for a thread, then name and task.
/// Children are indented 14 dp and sit 12 dp right of a 1 dp guide line, the left border of the
/// children's column (`.tree ul` `margin-left:14px; padding-left:12px; border-left`), drawn with
/// `Modifier::border_side` (compose-rust#464, v0.0.9).
/// `.tree ul` `margin-left`: where the guide line stands from the parent's left edge.
const TREE_GUIDE_INSET: f32 = 14.0;
/// `.tree ul` `padding-left`: the gap from the guide line to the children.
const TREE_CHILD_PAD: f32 = 12.0;

fn TreeNodeView(node: SubNode, project: String) {
    let p = ember_theme().palette;
    { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), content = move || {
        { use ::compose_rust::ui::Row; Row!(spacing = SP[1], alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width(), {
            match &node.member {
                Some(m) => Character(
                    &m.agent,
                    m.leader,
                    22.0,
                    &m.name,
                    &project,
                    CharState::from_status(m.status),
                    None,
                ),
                None => match node.status {
                    NodeStatus::Run => Spinner(12.0),
                    NodeStatus::Unknown => HomeDot(p.grey.strong),
                    st => HomeDot(tone_color(st.dot_tone().unwrap_or(StatusTone::Blue), p)),
                },
            }
            Label(&node.name, Fs::Sm, Weight::Semibold, None, Some(1));
            // The status as text too, so it does not rest on the marker's colour alone.
            if !node.status.label().is_empty() {
                Label(node.status.label(), Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(1));
            }
            Label(&node.task, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
        }) };
        if !node.children.is_empty() {
            { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width(), {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(TREE_GUIDE_INSET)) };
                { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new()
                            .weight(1.0)
                            .border_side(Side::Start, 1.0, paint(p.line)), {
                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(TREE_CHILD_PAD)) };
                        { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().weight(1.0), {
                            for c in node.children.iter() {
                                TreeNodeView(c.clone(), project.clone());
                            }
                        }) };
                    }) };
            }) };
        }
    }) };
}

/// `.sl-row`: a lead, a fixed-width name, a track and a value.
fn UsageRow(lead: impl FnOnce(), name: &str, name_w: f32, pct: f32, fill: Color, value: &str) {
    let p = ember_theme().palette;
    { use ::compose_rust::ui::Row; Row!(spacing = 10.0, alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width(), {
        lead();
        { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().width(name_w), {
            Label(name, Fs::Sm, Weight::Regular, None, Some(1));
        }) };
        { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(1.0), content = || TrackBar(pct, 6.0, fill)) };
        Label(value, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
    }) };
}

fn PrCard(weighted: bool, d: StatusCardsData) {
    let count = pr_count_text(&d.prs);
    StatusCardShell(
        weighted,
        ICON_GIT_SVG,
        PR_CARD_TITLE,
        move || Pill(&count, PillTone::Grey, false),
        move || {
            if d.prs.is_empty() {
                EmptyText(PR_EMPTY);
            }
            for (i, r) in d.prs.iter().enumerate() {
                PrRowView(r.clone(), i == 0);
            }
        },
    );
}

fn SubagentCard(weighted: bool, d: StatusCardsData) {
    StatusCardShell(weighted, ICON_USERS_SVG, SUBAGENT_CARD_TITLE, || {}, move || {
        if d.tree.is_empty() {
            EmptyText(SUBAGENT_EMPTY);
        }
        for n in d.tree.iter() {
            TreeNodeView(n.clone(), d.project.clone());
        }
    });
}

fn LoadCard(weighted: bool, d: StatusCardsData) {
    let p = ember_theme().palette;
    let name_w = if d.compact { 90.0 } else { 118.0 };
    StatusCardShell(weighted, ICON_CPU_SVG, LOAD_CARD_TITLE, || {}, move || {
        { use ::compose_rust::ui::Column; Column!(spacing = SP[0], modifier = Modifier::new().fill_max_width(), {
            Label(TOKENS_CAPTION, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
            if d.tokens.is_empty() {
                EmptyText(TOKENS_EMPTY);
            }
            let max = d.tokens.iter().map(|t| t.k).max().unwrap_or(0);
            for t in d.tokens.iter() {
                let m = t.member.clone();
                let project = d.project.clone();
                UsageRow(
                    move || {
                        Character(
                            &m.agent,
                            m.leader,
                            22.0,
                            &m.name,
                            &project,
                            CharState::from_status(m.status),
                            None,
                        )
                    },
                    &t.member.name,
                    name_w,
                    token_width_pct(t.k, max),
                    tone_color(vendor_accent(&t.member.agent), p),
                    &token_value_text(t.k),
                );
            }
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(SP[1])) };
            Label(COMPUTERS_CAPTION, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
            if d.computers.is_empty() {
                EmptyText(COMPUTERS_EMPTY);
            }
            for c in d.computers.iter() {
                let fill = tone_color(cpu_fill_tone(c.cpu.unwrap_or(0)), p);
                let name = match &c.ip {
                    Some(ip) => format!("{} \u{00b7} {ip}", c.name),
                    None => c.name.clone(),
                };
                UsageRow(
                    || NavIcon(ICON_CPU_SVG, p.muted.to_argb(), 18.0),
                    &name,
                    name_w,
                    computer_fill_pct(c),
                    fill,
                    &computer_value_text(c),
                );
            }
            // The real session counts, not in the mockup, kept under the computers.
            if !d.counts.is_empty() {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(SP[1])) };
            }
            for r in d.counts.iter() {
                Label(r, Fs::Sm, Weight::Regular, None, Some(1));
            }
        }) };
    });
}

/// Whether a layout of `columns` columns stretches its cards to the tallest (FR-L18): only the
/// multi-column rows do; the single column keeps each card's own height.
fn status_cards_equal_height(columns: usize) -> bool {
    columns >= 2
}

/// The bottom grid (`.grid.g3.sgrid`), gap 20: three columns from 1280 dp, two from 900 dp
/// (the third card below), one under that. Every card fills its share of the page width
/// (`status_card_width`); under 900 dp the width is set outright, because a column sized to its
/// content shrank the cards to about 308 dp at 390 dp.
/// The rows of the multi-column layouts use `equal_height(true)` and the cards `fill_max_height`, so
/// the cards stretch to the tallest, as in the mockup (FR-L18, compose-rust#389).
fn StatusCardsGrid(width_dp: f32, d: StatusCardsData) {
    let (d1, d2, d3) = (d.clone(), d.clone(), d);
    match status_grid_columns(width_dp) {
        3 => {
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::TopStart, spacing = SP[4], intrinsic_size = status_cards_equal_height(3), modifier = Modifier::new().fill_max_width(), content = move || {
                PrCard(true, d1);
                SubagentCard(true, d2);
                LoadCard(true, d3);
            }) };
        }
        2 => {
            { use ::compose_rust::ui::Column; Column!(spacing = SP[4], modifier = Modifier::new().fill_max_width(), content = move || {
                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::TopStart, spacing = SP[4], intrinsic_size = status_cards_equal_height(2), modifier = Modifier::new().fill_max_width(), content = move || {
                    PrCard(true, d1);
                    SubagentCard(true, d2);
                }) };
                LoadCard(false, d3);
            }) };
        }
        _ => {
            let w = status_card_width(width_dp, 1, SP[4]);
            { use ::compose_rust::ui::Column; Column!(spacing = SP[4], modifier = Modifier::new().width(w), content = move || {
                PrCard(false, d1);
                SubagentCard(false, d2);
                LoadCard(false, d3);
            }) };
        }
    }
}

/// Whether the stat tiles, the timeline card and the kanban card are drawn above the status cards.
/// `EMBER_CR_SCROLL=cards` hides them for review captures: the toolkit cannot scroll a column from
/// code before v0.0.5 (v0.0.7: #385 fixed with `ScrollState::scroll_to`; workaround kept until
/// checked on a display, because the capture script relies on it), so the blocks that would have scrolled out are left out and the
/// cards (PR과 CI, 서브 에이전트 스레드, 부하) start at the top at 390 dp.
fn status_blocks_above_cards(scroll: Option<&str>) -> bool {
    scroll != Some("cards")
}

/// The kanban of member status, with the cards fed by the view-model (#324).
pub fn StatusView(
    project: &str,
    members: &[Member],
    on_open: impl Fn(String) + Clone + 'static,
) {
    let window = current_window_size();
    let p = ember_theme().palette;
    let proj_owned = project.to_string();
    let members_vec = members.to_vec();
    let members_for_cards = members_vec.clone();
    let columns = kanban(&members_vec);
    let count = |s: LauncherStatus| members_vec.iter().filter(|m| m.status == s).count();
    let total_cnt = members_vec.len();
    let lead_cnt = members_vec.iter().filter(|m| m.leader).count();
    let sub_cnt = total_cnt.saturating_sub(lead_cnt);
    let run_cnt = count(LauncherStatus::Running);
    let wait_cnt = count(LauncherStatus::WaitingForApproval);
    let fail_cnt = count(LauncherStatus::Failed);
    let ok_pct = if total_cnt == 0 {
        100
    } else {
        ((total_cnt.saturating_sub(fail_cnt)) * 100) / total_cnt
    };

    let kpis: Vec<(String, String, String, Option<Color>)> = vec![
        (
            "팀원".to_string(),
            total_cnt.to_string(),
            format!("리더 {lead_cnt} · 팀원 {sub_cnt}"),
            None,
        ),
        (
            "실행 중".to_string(),
            run_cnt.to_string(),
            "활발한 세션".to_string(),
            Some(p.blue.strong),
        ),
        (
            "승인 대기".to_string(),
            wait_cnt.to_string(),
            if wait_cnt > 0 {
                "응답 필요".to_string()
            } else {
                "없음".to_string()
            },
            Some(p.orange.strong),
        ),
        (
            "실패".to_string(),
            fail_cnt.to_string(),
            if fail_cnt > 0 {
                "점검 필요".to_string()
            } else {
                "없음".to_string()
            },
            Some(p.red.strong),
        ),
        {
            crate::bridge::request_project_day(&proj_owned);
            let load = services().ui.live.project_day.get().get(&proj_owned).cloned().unwrap_or_default();
            let (value, sub) = token_tile(&load, &crate::ui::home::today_utc());
            ("오늘 토큰".to_string(), value, sub, None)
        },
        (
            "성공률".to_string(),
            format!("{ok_pct}%"),
            "최근 세션".to_string(),
            Some(p.green.strong),
        ),
    ];

    let kpi_per_row = if window.width_dp >= 1280.0 {
        6
    } else if window.width_dp >= 900.0 {
        3
    } else {
        2
    };
    let wide_kanban = window.width_dp >= 1280.0;

    { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().fill_max_width().fill_max_height(), content = move || {
            { use ::compose_rust::ui::Column; Column!(space_role = SpaceRole::Md, modifier = Modifier::new()
                        .fill_max_width()
                        .padding(status_page_pad(window.width_dp)), content = move || {
                    // Review captures: `EMBER_CR_SCROLL=cards` leaves out the blocks above the cards.
                    if status_blocks_above_cards(std::env::var("EMBER_CR_SCROLL").ok().as_deref()) {
                        // 1. 6 KPI cards grid (.skpis)
                        let kpi_rows: Vec<Vec<(String, String, String, Option<Color>)>> = kpis
                            .chunks(kpi_per_row)
                            .map(|c| c.to_vec())
                            .collect();
                        for row in kpi_rows {
                            { use ::compose_rust::ui::Row; Row!(space_role = SpaceRole::Sm, modifier = Modifier::new().fill_max_width(), content = move || {
                                    for (t, v, s, tone) in row {
                                        KpiCard(&t, &v, &s, tone);
                                    }
                                }) };
                        }

                        // 2. Agent-to-agent communication timeline card (.tlcard)
                        TimelineCard(&proj_owned);

                        // 3. Outer Kanban card (.skan) with 5 columns
                        let cols_c = columns.clone();
                        let proj_k = proj_owned.clone();
                        let open_k = on_open.clone();
                        EmberCard(false, true, None, None, None, move || {
                            { use ::compose_rust::ui::Column; Column!(space_role = SpaceRole::Sm, modifier = Modifier::new().fill_max_width(), content = move || {
                                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Sm, modifier = Modifier::new().fill_max_width(), {
                                            NavIcon(ICON_GRID_SVG, p.text_2.to_argb(), 16.0);
                                            Label("칸반", Fs::Md, Weight::Semibold, None, Some(1));
                                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                                            Label(
                                                &format!("팀원 {total_cnt}"),
                                                Fs::Xs,
                                                Weight::Regular,
                                                Some(paint(p.muted)),
                                                Some(1),
                                            );
                                        }) };

                                    if wide_kanban {
                                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::TopStart, space_role = SpaceRole::Sm, modifier = Modifier::new().fill_max_width(), content = move || {
                                                for (col, ms) in cols_c.iter() {
                                                    KanbanColView(
                                                        col.label(),
                                                        ms.clone(),
                                                        proj_k.clone(),
                                                        None,
                                                        Some(1.0),
                                                        open_k.clone(),
                                                    );
                                                }
                                            }) };
                                    } else {
                                        { use ::compose_rust::ui::ScrollRow; ScrollRow!(modifier = Modifier::new().fill_max_width(), content = move || {
                                                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::TopStart, space_role = SpaceRole::Sm, content = move || {
                                                        for (col, ms) in cols_c.iter() {
                                                            KanbanColView(
                                                                col.label(),
                                                                ms.clone(),
                                                                proj_k.clone(),
                                                                Some(220.0),
                                                                None,
                                                                open_k.clone(),
                                                            );
                                                        }
                                                    }) };
                                            }) };
                                    }
                                }) };
                        });
                    }

                    // 4. Bottom 3-column grid (.sgrid): PR과 CI, 서브 에이전트 스레드, 부하
                    // Rows come from the pure view-model; no data shows an honest empty state (#324).
                    let demo_cards = services().demo.is_some();
                    let computers = services().client.read(|st| st.computers().to_vec());
                    StatusCardsGrid(
                        window.width_dp,
                        StatusCardsData {
                            project: proj_owned.clone(),
                            prs: pr_rows(demo_cards),
                            tree: subagent_tree(demo_cards, &members_for_cards),
                            tokens: token_rows(demo_cards, &members_for_cards),
                            computers: computer_rows(&computers, &proj_owned, demo_cards),
                            counts: session_count_rows(&members_for_cards),
                            compact: window.width_dp <= 560.0,
                        },
                    );

                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(24.0)) };
                }) };
        }) };
}

fn view_icon_svg(v: SessionView) -> &'static str {
    match v {
        SessionView::Multi => ICON_GRID_SVG,
        SessionView::Single => ICON_USER_SVG,
        SessionView::Status => ICON_CHART_SVG,
    }
}

/// Unified segmented view switcher (`.seg.viewseg`): 멀티 / 싱글 / 상태 with icons.
fn ViewSeg(
    cur: SessionView,
    hide_label: bool,
    full_width: bool,
    on_pick: impl Fn(SessionView) + Clone + 'static,
) {
    let p = ember_theme().palette;
    let mut m = Modifier::new()
        .height(32.0)
        .background(paint(p.surface_2))
        .border(1.0, paint(p.line))
        .shape_role(ShapeRole::Full)
        .padding(2.0);
    if full_width {
        m = m.fill_max_width();
    }
    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::Center, modifier = m, content = move || {
            for v in SessionView::ALL {
                let selected = cur == v;
                let (bg, fg) = if selected {
                    (paint(p.ink), p.ink_text)
                } else {
                    (paint(Color::argb(0)), p.text_2)
                };
                let cb = on_pick.clone();
                let mut item_m = Modifier::new()
                    .height(28.0)
                    .background(bg)
                    .shape_role(ShapeRole::Full);
                if full_width {
                    item_m = item_m.weight(1.0);
                }
                Clickable(
                    item_m,
                    Alignment::Center,
                    move || cb(v),
                    move || {
                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::Center, spacing = 5.0, {
                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(10.0)) };
                                NavIcon(view_icon_svg(v), fg.to_argb(), 14.0);
                                if !hide_label {
                                    Label(
                                        v.label(),
                                        Fs::Xs,
                                        if selected {
                                            Weight::Semibold
                                        } else {
                                            Weight::Medium
                                        },
                                        Some(paint(fg)),
                                        Some(1),
                                    );
                                }
                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(10.0)) };
                            }) };
                    },
                );
            }
        }) };
}

/// The head of the session project menu: what is chosen now.
pub fn project_switch_head(current: &str) -> String {
    format!("프로젝트 \u{b7} {}", display_name(current))
}

/// The session project menu's rows: one per project with the folder glyph, its name, its member
/// count and a check on the current one.
pub fn project_switch_rows(boxes: &[crate::ui::project::ProjectBox], current: &str) -> Vec<PopRow> {
    boxes
        .iter()
        .map(|b| PopRow {
            icon: Some(ICON_FOLDER_SVG),
            title: display_name(&b.id),
            description: Some(format!("팀원 {}명", b.members.len())),
            chosen: b.id == current,
            ..Default::default()
        })
        .collect()
}

/// The session screen's project switcher menu (FR-L10.4, #888): the shared menu in the
/// permission-mode look under `anchor`, lined up with its start; at 560 and below a bottom sheet
/// with the menu's own head, for which the caller passes an empty `anchor` and draws the control
/// itself. The mockup draws a native `<select>` (`.projsw`). A row switches the project and
/// remembers it.
fn ProjectSwitchMenu(
    open: &MutableState<bool>,
    boxes: &[crate::ui::project::ProjectBox],
    current: &str,
    project: &MutableState<Option<String>>,
    ui: &SharedUi,
    anchor: impl FnOnce(),
) {
    let rows = project_switch_rows(boxes, current);
    let head = project_switch_head(current);
    let ids: Vec<String> = boxes.iter().map(|b| b.id.clone()).collect();
    let (dismiss, proj_st, ui_p) = (open.clone(), project.clone(), ui.clone());
    PopoverMenuWith(
        open.get(),
        PopMenu {
            name: super::a11y::PROJECT_PICKER,
            kind: PopKind::Mode,
            head: &head,
            sub: None,
            rows: &rows,
            align: compose_rust::PopoverAlign::Start,
        },
        PopExtra { plain_sheet: true, ..Default::default() },
        move || dismiss.set(false),
        move |idx| {
            if let Some(id) = ids.get(idx).cloned() {
                proj_st.set(Some(id.clone()));
                ui_p.update_prefs(|p| p.last_project = Some(id));
            }
        },
        anchor,
        None,
    );
}

/// The phone header's first row (title, new member): `.sesshead{height:auto;padding:8px ...}`
/// around the title block, which is taller than the 32 dp controls: the name line (Md), 1 dp,
/// the subtitle line (Xs2) and the block's 2 dp padding. A fixed 48 cut the subtitle's last
/// 3 to 4 px (FR-L28.6, review and2).
const PHONE_HEAD_ROW_H: f32 =
    2.0 * SP[1] + 2.0 * 2.0 + Fs::Md.line_height() + 1.0 + Fs::Xs2.line_height();

/// Pill-shaped toolbar action button (`.btn.sm` in `.sesshead`).
/// Session header actions carry no text label (owner placement rule 2026-11-13, rule 6, design
/// review B6); the label is the tooltip and the accessible name.
pub const SESSION_HEADER_LABELS: bool = false;

/// The 웹 페이지 button and its menu (FR-P7, FR-L10.4, #888, mockup `webMenu`): the globe with a
/// chevron (`.btn.sm` with `I("globe")` and `.chev`, review SE3), and under it, lined up with its
/// end, "웹 페이지 열기" and one row per listening port: the address in the mono face, the process
/// (and why a port cannot be opened) below, and a green 감지됨 pill on a port that opens. A tap
/// opens the address in the browser or says why it cannot. `sub` is the line under the head,
/// `computer · ip` ([`crate::ui::session::web_menu_subtitle`]), read with the ports. Not built:
/// the member character on a row (ports carry no member), the port field and the tunnel hint
/// (no direct-port open).
fn WebMenu(open: bool, load: &WebPagesLoad, sub: Option<&str>, on_dismiss: impl FnMut() + Clone + 'static, on_open: impl FnMut() + 'static) {
    let items = crate::ui::session::web_menu_items(load);
    let rows: Vec<PopRow> = items
        .iter()
        .map(|w| PopRow {
            title: w.title.clone(),
            description: w.detail.clone(),
            badge: w.detected.then(|| ("감지됨".to_string(), PillTone::Green)),
            mono: w.mono,
            spinner: w.loading,
            ..Default::default()
        })
        .collect();
    let load_now = load.clone();
    PopoverMenu(
        open,
        PopMenu {
            name: super::a11y::WEB_MENU,
            kind: PopKind::Drop,
            head: crate::ui::session::WEB_MENU_HEAD,
            sub,
            rows: &rows,
            align: compose_rust::PopoverAlign::End,
        },
        on_dismiss,
        move |idx| match web_tap(&load_now, idx) {
            WebTap::Open(url) => {
                if crate::ide::open_external(&url).is_err() {
                    notice::post("브라우저를 열지 못했습니다");
                }
            }
            // An address that is a guess: open it, and say it may not open (FR-L18, #519).
            WebTap::OpenSaying(url, text) => {
                if crate::ide::open_external(&url).is_err() {
                    notice::post("브라우저를 열지 못했습니다");
                } else {
                    notice::post(text);
                }
            }
            WebTap::Say(text) => notice::post(text),
            WebTap::Nothing => {}
        },
        move || ToolbarBtn(ICON_GLOBE_SVG, "웹 페이지", SESSION_HEADER_LABELS, true, Some(open), on_open),
        None,
    );
}

/// A header action: a named button (`label` is its name and its tooltip), and, when it opens a
/// menu, whether that menu is open (`expanded`, FR-L10.4).
fn ToolbarBtn(
    icon_svg: &'static str,
    label: &'static str,
    show_label: bool,
    chevron: bool,
    expanded: Option<bool>,
    on_click: impl FnMut() + 'static,
) {
    let p = ember_theme().palette;
    let lbl = label.to_string();
    let m = if show_label || chevron {
        Modifier::new()
            .height(32.0)
            .background(paint(p.surface))
            .border(1.0, paint(p.line))
            .shape_role(ShapeRole::Full)
    } else {
        Modifier::new()
            .size(32.0, 32.0)
            .background(paint(p.surface))
            .border(1.0, paint(p.line))
            .shape_role(ShapeRole::Full)
    };
    // The label is the tooltip (`title` in the mockup) and the button's name. The tooltip sits
    // inside the button: around it, the toolkit's `Tooltip` box carries the text as its own
    // description and macOS read the control as static text (review r8; compose-rust
    // `renderer/desktop/src/renderer/HostOverlay.kt` `HostTooltip`).
    let tip = lbl.clone();
    let m = match expanded {
        Some(open) => m.expanded(open),
        None => m,
    };
    NamedClickable(label, m, Alignment::Center, on_click, move || { use ::compose_rust::ui::Tooltip; Tooltip!(tip, content = move || {
        if show_label || chevron {
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 5.0, {
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(8.0)) };
                    NavIcon(icon_svg, p.text_2.to_argb(), 14.0);
                    if show_label {
                        Label(
                            &lbl,
                            Fs::Xs,
                            Weight::Medium,
                            Some(paint(p.text)),
                            Some(1),
                        );
                    }
                    if chevron {
                        NavIcon(ICON_CHEVRON_DOWN_SVG, p.muted.to_argb(), 12.0);
                    }
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(8.0)) };
                }) };
        } else {
            NavIcon(icon_svg, p.text_2.to_argb(), 15.0);
        }
    }) });
}

/// New member sheet (`#snew` in mockup `session.js` lines 386-395).
pub(super) fn NewMemberSheet(
    project: &str,
    open: bool,
    on_close: impl FnMut() + Clone + Send + 'static,
) {
    if !open {
        return;
    }
    let ui = shared_ui().clone();
    let p = ember_theme().palette;
    let proj_owned = project.to_string();
    let proj_disp = display_name(project);
    let role_name = remember_with_key(format!("{proj_owned}:snew_role"), || {
        mutable_state_of(String::new())
    });
    let prompt_txt = remember_with_key(format!("{proj_owned}:snew_prompt"), || {
        mutable_state_of(String::new())
    });
    let comp_idx = remember_with_key(format!("{proj_owned}:snew_comp"), || {
        mutable_state_of(0_usize)
    });
    let model_opt = remember_with_key(format!("{proj_owned}:snew_model"), || {
        mutable_state_of(crate::ui::home::default_option(
            crate::ui::home::Harness::ClaudeCode,
        ))
    });
    let perm_mode = remember_with_key(format!("{proj_owned}:snew_mode"), || {
        mutable_state_of(crate::ui::home::default_mode(
            crate::ui::home::Harness::ClaudeCode,
        ))
    });
    let model_open = remember_with_key(format!("{proj_owned}:snew_model_open"), || {
        mutable_state_of(false)
    });
    let mode_open = remember_with_key(format!("{proj_owned}:snew_mode_open"), || {
        mutable_state_of(false)
    });
    let busy = remember_with_key(format!("{proj_owned}:snew_busy"), || mutable_state_of(false));

    let comp_opts = vec!["mac-mini".to_string(), "mac-studio".to_string(), "이 컴퓨터".to_string()];
    let c_idx = comp_idx.get();

    let close_guarded = {
        let b = busy.clone();
        let mut cl = on_close.clone();
        move || {
            if !b.get_untracked() {
                cl();
            }
        }
    };
    let close_hdr = close_guarded.clone();
    let close_f = close_guarded.clone();
    let create_cb = {
        let ui = ui.clone();
        let proj_c = proj_owned.clone();
        let role_c = role_name.clone();
        let model_c = model_opt.clone();
        let mode_c = perm_mode.clone();
        let busy_c = busy.clone();
        let mut close_done = on_close.clone();
        move || {
            if busy_c.get_untracked() {
                return;
            }
            let r_title = {
                let t = role_c.get_untracked().trim().to_string();
                if t.is_empty() {
                    "새 팀원".to_string()
                } else {
                    t
                }
            };
            if services().demo.is_some() {
                notice::post(format!("{r_title} 팀원을 추가했습니다"));
                close_done();
                return;
            }
            let chosen_opt = model_c.get_untracked();
            let chosen_mode = mode_c.get_untracked();
            let cwd = services()
                .client
                .read(|s| crate::model::latest_cwd(s, &proj_c))
                .unwrap_or_else(|| "/work".to_string());
            let body = crate::server::CreateSession {
                project: proj_c.clone(),
                agent: chosen_opt.harness.kind().to_string(),
                cwd,
                model: Some(chosen_opt.model.to_string()),
                title: Some(r_title),
                account: None,
            };
            let bypass = crate::ui::home::needs_approve_all(chosen_mode);
            busy_c.set(true);
            let b_done = busy_c.clone();
            let u_done = ui.clone();
            let mut cl_done = close_done.clone();
            run_shared(
                async move {
                    let s = services();
                    let rec = s.create_session(&body).await?;
                    let mut note = None;
                    if bypass {
                        if let Err(e) = s.set_approve_all(&rec.id, true).await {
                            note = Some(format!("모두 허용을 켜지 못했습니다({e})"));
                        }
                    }
                    Ok::<_, String>((rec, note))
                },
                move |res| {
                    b_done.set(false);
                    match res {
                        Ok((rec, note)) => {
                            if let Some(m) = note {
                                notice::post(m);
                            }
                            cl_done();
                            u_done.open_conversation(&rec.id);
                        }
                        Err(e) => notice::post(format!("팀원을 만들지 못했습니다: {e}")),
                    }
                },
            );
        }
    };

    let modifier = with_shadows(Modifier::new(), ember_theme().shadow_sm())
        .background(paint(p.surface))
        .border(1.0, paint(p.line))
        .corner_radius(CORNER_XL);
    let is_compact = current_window_size().is_compact();
    let body = move || {
        let mut cl_h = close_hdr.clone();
        { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), {
                // Header (.sheet-h): title + project name on the left, close button on the right
                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width().height(SHEET_HEADER_H), content = move || {
                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[4])) };
                        Label("새 팀원", Fs::Xl, Weight::Semibold, None, Some(1));
                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
                        Label(
                            &proj_disp,
                            Fs::Md,
                            Weight::Regular,
                            Some(paint(p.muted)),
                            Some(1),
                        );
                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                        CloseButton(move || cl_h());
                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[4])) };
                    }) };
                { use ::compose_rust::ui::Separator; Separator!(Some(paint(p.line))) };

                { use ::compose_rust::ui::Column; Column!(spacing = 16.0, modifier = Modifier::new().fill_max_width().padding(SP[4]), {
                        // 1. 역할 이름 (.field: 40dp, 1px line border, surface fill)
                        { use ::compose_rust::ui::Column; Column!(spacing = 8.0, modifier = Modifier::new().fill_max_width(), {
                                Label(
                                    "역할 이름",
                                    Fs::Sm,
                                    Weight::Regular,
                                    Some(paint(p.text_2)),
                                    Some(1),
                                );
                                let val_r = role_name.get();
                                { use ::compose_rust::ui::Box; Box!(alignment = Alignment::CenterStart, modifier = Modifier::new()
                                            .fill_max_width()
                                            .height(H_BTN)
                                            .background(paint(p.surface))
                                            .border(1.0, paint(p.line))
                                            .corner_radius(CORNER_SM)
                                            .padding(SP[2]), content = move || {
                                        if val_r.is_empty() {
                                            Label(
                                                "예: 구현, 리뷰, 문서",
                                                Fs::Md,
                                                Weight::Regular,
                                                Some(paint(p.muted)),
                                                Some(1),
                                            );
                                        } else {
                                            Label(
                                                &val_r,
                                                Fs::Md,
                                                Weight::Regular,
                                                Some(paint(p.text)),
                                                Some(1),
                                            );
                                        }
                                    }) };
                            }) };

                        // 2. 첫 지시 (선택) (textarea.field: 118dp, 1px line border, surface fill)
                        { use ::compose_rust::ui::Column; Column!(spacing = 8.0, modifier = Modifier::new().fill_max_width(), {
                                Label(
                                    "첫 지시 (선택)",
                                    Fs::Sm,
                                    Weight::Regular,
                                    Some(paint(p.text_2)),
                                    Some(1),
                                );
                                let val_p = prompt_txt.get();
                                { use ::compose_rust::ui::Box; Box!(alignment = Alignment::TopStart, modifier = Modifier::new()
                                            .fill_max_width()
                                            .height(118.0)
                                            .background(paint(p.surface))
                                            .border(1.0, paint(p.line))
                                            .corner_radius(CORNER_SM)
                                            .padding(SP[2]), content = move || {
                                        if val_p.is_empty() {
                                            Label(
                                                "이 팀원이 할 일을 적으세요",
                                                Fs::Md,
                                                Weight::Regular,
                                                Some(paint(p.muted)),
                                                Some(3),
                                            );
                                        } else {
                                            Label(
                                                &val_p,
                                                Fs::Md,
                                                Weight::Regular,
                                                Some(paint(p.text)),
                                                Some(4),
                                            );
                                        }
                                    }) };
                            }) };

                        // 3. 하네스, 모델, 권한 모드 (.snew-picks: 40dp height, CORNER_SM, FR-L15 / FR-L16)
                        let cur_model = model_opt.get();
                        let cur_mode = perm_mode.get();
                        let catalog = crate::ui::home::model_catalog();
                        let model_rows: Vec<PopRow> = catalog
                            .iter()
                            .map(|opt| PopRow {
                                title: opt.chip_label(),
                                chosen: *opt == cur_model,
                                ..Default::default()
                            })
                            .collect();
                        let model_chip_text = cur_model.chip_label();
                        let model_dot = harness_dot_color(cur_model.harness.kind(), p);
                        let mo_expanded = model_open.get();
                        let (mo_btn, mo_dismiss, mo_pick, mo_close_mode) = (
                            model_open.clone(),
                            model_open.clone(),
                            model_open.clone(),
                            mode_open.clone(),
                        );
                        let (m_state, md_sync) = (model_opt.clone(), perm_mode.clone());
                        let cat_pick = catalog.clone();

                        let mode_list = crate::ui::home::modes_for(cur_model.harness);
                        let mode_head = crate::ui::home::mode_header(cur_model.harness);
                        let mode_pop_rows: Vec<PopRow> =
                            crate::ui::home::mode_picker_rows(cur_model.harness, cur_mode)
                                .into_iter()
                                .map(|r| PopRow {
                                    icon: Some(if r.danger {
                                        super::home::ICON_WARN
                                    } else {
                                        ICON_SHIELD_SVG
                                    }),
                                    title: r.title.to_string(),
                                    description: Some(r.description.to_string()),
                                    chosen: r.chosen,
                                    danger: r.danger,
                                    ..Default::default()
                                })
                                .collect();
                        let (mode_bg, mode_fg) = if cur_mode.is_bypass() {
                            (p.orange.soft, p.orange.strong)
                        } else {
                            (p.surface_2, p.text)
                        };
                        let mode_icon = if cur_mode.is_bypass() {
                            super::home::ICON_WARN
                        } else {
                            ICON_SHIELD_SVG
                        };
                        let mode_chip_text = cur_mode.label();
                        let md_expanded = mode_open.get();
                        let (md_btn, md_dismiss, md_pick, md_close_model) = (
                            mode_open.clone(),
                            mode_open.clone(),
                            mode_open.clone(),
                            model_open.clone(),
                        );
                        let mode_state = perm_mode.clone();

                        { use ::compose_rust::ui::Column; Column!(spacing = 8.0, modifier = Modifier::new().fill_max_width(), {
                                Label(
                                    "하네스, 모델, 권한 모드",
                                    Fs::Sm,
                                    Weight::Regular,
                                    Some(paint(p.text_2)),
                                    Some(1),
                                );
                                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                                        PopoverMenuWith(
                                            mo_expanded,
                                            PopMenu {
                                                name: super::a11y::MODEL_PICKER,
                                                kind: PopKind::Mode,
                                                head: "하네스 · 모델",
                                                sub: None,
                                                rows: &model_rows,
                                                align: compose_rust::PopoverAlign::Start,
                                            },
                                            PopExtra { plain_sheet: true, ..Default::default() },
                                            move || mo_dismiss.set(false),
                                            move |idx| {
                                                if let Some(opt) = cat_pick.get(idx).copied() {
                                                    m_state.set(opt);
                                                    let synced =
                                                        crate::ui::home::mode_after_harness_change(
                                                            opt.harness,
                                                            md_sync.get_untracked(),
                                                        );
                                                    md_sync.set(synced);
                                                }
                                                mo_pick.set(false);
                                            },
                                            move || {
                                                Clickable(
                                                    Modifier::new()
                                                        .height(H_BTN)
                                                        .background(paint(p.surface_2))
                                                        .border(1.0, paint(p.line))
                                                        .corner_radius(CORNER_SM)
                                                        .expanded(mo_expanded),
                                                    Alignment::CenterStart,
                                                    move || {
                                                        mo_close_mode.set(false);
                                                        mo_btn.set(!mo_btn.get_untracked());
                                                    },
                                                    move || {
                                                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 6.0, {
                                                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(10.0)) };
                                                                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                                                                        .size(8.0, 8.0)
                                                                        .background(paint(model_dot))
                                                                        .corner_radius(4.0)) };
                                                                Label(
                                                                    &model_chip_text,
                                                                    Fs::Sm,
                                                                    Weight::Medium,
                                                                    Some(paint(p.text)),
                                                                    Some(1),
                                                                );
                                                                NavIcon(
                                                                    ICON_CHEVRON_DOWN_SVG,
                                                                    p.muted.to_argb(),
                                                                    12.0,
                                                                );
                                                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(10.0)) };
                                                            }) };
                                                    },
                                                );
                                            },
                                            None,
                                        );
                                        PopoverMenu(
                                            md_expanded,
                                            PopMenu {
                                                name: super::a11y::MODE_PICKER,
                                                kind: PopKind::Mode,
                                                head: &mode_head,
                                                sub: None,
                                                rows: &mode_pop_rows,
                                                align: compose_rust::PopoverAlign::Start,
                                            },
                                            move || md_dismiss.set(false),
                                            move |idx| {
                                                if let Some(m) = mode_list.get(idx).copied() {
                                                    mode_state.set(m);
                                                }
                                                md_pick.set(false);
                                            },
                                            move || {
                                                Clickable(
                                                    Modifier::new()
                                                        .height(H_BTN)
                                                        .background(paint(mode_bg))
                                                        .border(1.0, paint(p.line))
                                                        .corner_radius(CORNER_SM)
                                                        .expanded(md_expanded),
                                                    Alignment::CenterStart,
                                                    move || {
                                                        md_close_model.set(false);
                                                        md_btn.set(!md_btn.get_untracked());
                                                    },
                                                    move || {
                                                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 6.0, {
                                                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(10.0)) };
                                                                NavIcon(
                                                                    mode_icon,
                                                                    if cur_mode.is_bypass() {
                                                                        p.orange.strong.to_argb()
                                                                    } else {
                                                                        p.text_2.to_argb()
                                                                    },
                                                                    14.0,
                                                                );
                                                                Label(
                                                                    mode_chip_text,
                                                                    Fs::Sm,
                                                                    Weight::Medium,
                                                                    Some(paint(mode_fg)),
                                                                    Some(1),
                                                                );
                                                                NavIcon(
                                                                    ICON_CHEVRON_DOWN_SVG,
                                                                    p.muted.to_argb(),
                                                                    12.0,
                                                                );
                                                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(10.0)) };
                                                            }) };
                                                    },
                                                );
                                            },
                                            None,
                                        );
                                    }) };
                                Label(
                                    &format!("하네스 {}", cur_model.harness.label()),
                                    Fs::Xs,
                                    Weight::Regular,
                                    Some(paint(p.muted)),
                                    Some(1),
                                );
                            }) };

                        // 4. 컴퓨터
                        { use ::compose_rust::ui::Column; Column!(spacing = 8.0, modifier = Modifier::new().fill_max_width(), {
                                Label(
                                    "컴퓨터",
                                    Fs::Sm,
                                    Weight::Regular,
                                    Some(paint(p.text_2)),
                                    Some(1),
                                );
                                let ci = comp_idx.clone();
                                Select(&comp_opts[c_idx.min(comp_opts.len() - 1)], &comp_opts, c_idx, 440.0, move |i| ci.set(i));
                            }) };
                    }) };
                SheetFooter(move || {
                    let mut cl = close_f.clone();
                    Clickable(
                        Modifier::new()
                            .height(H_CTL)
                            .background(paint(p.surface))
                            .border(1.0, paint(p.line))
                            .shape_role(ShapeRole::Full),
                        Alignment::Center,
                        move || cl(),
                        || {
                            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::Center, {
                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(12.0)) };
                                Label(
                                    "취소",
                                    Fs::Sm,
                                    Weight::Medium,
                                    Some(paint(p.text)),
                                    Some(1),
                                );
                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(12.0)) };
                            }) };
                        },
                    );
                    let mut cr = create_cb.clone();
                    let en = !busy.get();
                    Clickable(
                        Modifier::new()
                            .height(H_CTL)
                            .background(paint(p.ink))
                            .shape_role(ShapeRole::Full),
                        Alignment::Center,
                        move || {
                            if en {
                                cr();
                            }
                        },
                        || {
                            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::Center, {
                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(12.0)) };
                                Label(
                                    "만들기",
                                    Fs::Sm,
                                    Weight::Medium,
                                    Some(paint(p.ink_text)),
                                    Some(1),
                                );
                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(12.0)) };
                            }) };
                        },
                    );
                });
            }) };
    };

    if is_compact {
        { use ::compose_rust::ui::Sheet; Sheet!(true, close_guarded, modifier = modifier.fill_max_width(), content = body) };
    } else {
        { use ::compose_rust::ui::Dialog; Dialog!(true, close_guarded, modifier = modifier.width(758.0), content = body) };
    }
}

/// The 세션 screen (`Screen::Conversation(focus)` and `Screen::Placeholder(NavMenu::Sessions)`).
pub fn SessionScreen(focus: Option<String>) {
    let ui = shared_ui().clone();
    let window = current_window_size();
    let p = ember_theme().palette;

    let focus_project = focus.as_ref().and_then(|id| {
        let _ = ui.live.launcher.get();
        services()
            .client
            .read(|s| s.session(id).map(|v| v.record.project.clone()))
    });

    let view = remember(|| {
        if let Ok(env_v) = std::env::var("EMBER_CR_SESSION_VIEW") {
            if let Some(v) = SessionView::from_key(env_v.trim()) {
                return mutable_state_of(v);
            }
        }
        mutable_state_of(view_from_saved(
            ui.prefs.get_untracked().session_view.as_deref(),
        ))
    });

    let project = remember(|| {
        if let Ok(env_p) = std::env::var("EMBER_CR_SESSION_PROJECT") {
            if !env_p.trim().is_empty() {
                return mutable_state_of(Some(env_p.trim().to_string()));
            }
        }
        mutable_state_of(
            focus_project
                .clone()
                .or_else(|| ui.selected_project.get_untracked()),
        )
    });

    let query = remember(|| mutable_state_of(String::new()));
    let drawer = remember(|| {
        mutable_state_of(std::env::var("EMBER_CR_SESSION_DRAWER").as_deref() == Ok("1"))
    });
    let rail_collapsed = remember(|| mutable_state_of(false));
    let new_member_open = remember(|| {
        mutable_state_of(std::env::var("EMBER_CR_SESSION_NEW_MEMBER").as_deref() == Ok("1"))
    });
    // `EMBER_CR_SESSION_MENU=project` opens the project menu at start, for captures.
    let project_menu = remember(|| mutable_state_of(std::env::var("EMBER_CR_SESSION_MENU").as_deref() == Ok("project")));
    let web_menu = remember(|| mutable_state_of(false));
    // 버전 관리 opens as a sheet over the conversation (FR-L18, #495); `EMBER_CR_SESSION_VCS=1`
    // opens it at start for captures.
    let vcs_open = remember(|| mutable_state_of(std::env::var("EMBER_CR_SESSION_VCS").as_deref() == Ok("1")));
    let web_pages = remember(|| mutable_state_of(WebPagesLoad::Loading));
    // The 웹 페이지 menu's `computer · ip` line, read with the ports.
    let web_sub = remember(|| mutable_state_of(None::<String>));
    // The detail flag value this screen last wrote or saw (FR-L28, #885).
    let detail_wrote = remember(|| mutable_state_of(false));

    let show_archived = ui.show_archived.get();
    let boxes = {
        let _ = ui.live.launcher.get();
        services().client.read(|s| project_boxes_with_archived(s, show_archived))
    };

    let Some(current) = resolve_project(None, project.get().as_deref(), &boxes) else {
        { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().fill_max_width().fill_max_height(), {
                { use ::compose_rust::ui::Column; Column!(alignment = Alignment::Center, space_role = SpaceRole::Sm, {
                        Label(NO_PROJECT, Fs::Xl, Weight::Medium, None, Some(1));
                        let u = ui.clone();
                        AppButton(
                            "새 세션",
                            Some(IconRole::Compose),
                            ButtonKind::Primary,
                            false,
                            true,
                            false,
                            move || u.open_new_session(None),
                        );
                    }) };
            }) };
        return;
    };

    let members = boxes
        .iter()
        .find(|b| b.id == current)
        .map(|b| b.members.clone())
        .unwrap_or_default();
    let target = single_target(&members, focus.as_deref());
    let sections = side_sections(&boxes, &current, &query.get());
    let is_phone = window.width_dp <= 560.0;
    let is_tablet_rail = window.width_dp > 560.0 && window.width_dp <= 900.0;
    let show_rail = is_tablet_rail || (!is_phone && rail_collapsed.get());
    let show_column = !is_phone && !show_rail;
    // The drawer state resets once the list is a column again, so shrinking later does not
    // reopen it (FR-L18). Written only while it is true, so there is no write loop.
    if show_column && drawer.get() {
        drawer.set(false);
    }
    let chips_below = view_chips_below(window.width_dp);
    let hide_view_lbl = crate::ui::session::view_seg_icon_only(window.width_dp);

    // Opening the 웹 페이지 list reads the listening ports of the session's computer (FR-P7).
    let open_web_menu = {
        let web_menu = web_menu.clone();
        let web_pages = web_pages.clone();
        let web_sub = web_sub.clone();
        let sid = target.clone().or_else(|| members.first().map(|m| m.session_id.clone()));
        move || {
            web_menu.set(true);
            web_pages.set(WebPagesLoad::Loading);
            let done = web_pages.clone();
            let sub = web_sub.clone();
            // The line of the last open is not shown while another session's ports load, or
            // after a read that fails.
            sub.set(None);
            if services().demo.is_some() {
                let host = ComputerHost { host: Some(crate::demo::DEMO_HOST.to_string()), ..Default::default() };
                sub.set(crate::ui::session::web_menu_subtitle(Some(DEMO_COMPUTER), &host));
                done.set(web_pages_from(Ok(crate::demo::ports()), &host));
                return;
            }
            let Some(sid) = sid.clone() else {
                done.set(WebPagesLoad::Failed(NO_SESSION.to_string()));
                return;
            };
            run_shared(
                async move {
                    let c = services().server.session_computer(&sid).await.map_err(|e| e.to_string())?;
                    let host = computer_host(&c.computer, &services().config.server_url);
                    let ports = services().client.api().computer_ports(&c.computer.id).await.map_err(|e| e.to_string())?;
                    Ok::<_, String>((ports, host, c.computer.name))
                },
                move |r: Result<(ember_client::api::Ports, ComputerHost, String), String>| match r {
                    Ok((ports, host, name)) => {
                        sub.set(crate::ui::session::web_menu_subtitle(Some(&name), &host));
                        done.set(web_pages_from(Ok(ports), &host));
                    }
                    Err(e) => {
                        // The list shows one fixed sentence; the cause goes to the log only.
                        tracing::debug!("web page list: {e}");
                        done.set(web_pages_from(Err(e), &ComputerHost::default()));
                    }
                },
            );
        }
    };

    // `EMBER_CR_SESSION_WEB=1` opens the 웹 페이지 list at start, for captures without clicks.
    if std::env::var("EMBER_CR_SESSION_WEB").as_deref() == Ok("1") {
        let open = open_web_menu.clone();
        launched_effect((), move || {
            let open = open.clone();
            async move { open() }
        });
    }

    let set_view = {
        let view = view.clone();
        let ui = ui.clone();
        move |v: SessionView| {
            view.set(v);
            ui.update_prefs(|p| p.session_view = Some(v.key().to_string()));
        }
    };

    let open_member = {
        let drawer = drawer.clone();
        let current = current.clone();
        let members = members.clone();
        let project = project.clone();
        let ui = ui.clone();
        let view = view.clone();
        let set_view = set_view.clone();
        move |proj: String, id: String| {
            drawer.set(false);
            let same = proj == current;
            project.set(Some(proj.clone()));
            ui.update_prefs(|p| p.last_project = Some(proj.clone()));
            let now = view.get_untracked();
            match now {
                SessionView::Multi if same => {
                    let all: Vec<String> = members.iter().map(|m| m.session_id.clone()).collect();
                    edit_panes(&ui, &proj, move |pp| {
                        add_pane(pp.active_tab_mut(), &all, &id);
                    });
                }
                SessionView::Status => {
                    set_view(SessionView::Single);
                    ui.open_conversation(&id);
                }
                _ => ui.open_conversation(&id),
            }
        }
    };

    let to_single = {
        let current = current.clone();
        let project = project.clone();
        let ui = ui.clone();
        let set_view = set_view.clone();
        move |id: String| {
            set_view(SessionView::Single);
            ui.open_conversation(&id);
            project.set(Some(current.clone()));
        }
    };

    let to_multi = {
        let set_view = set_view.clone();
        move || {
            set_view(SessionView::Multi);
        }
    };

    let project_name = display_name(&current);
    let view_now = view.get();

    // A member's 싱글 view at phone width is a full-screen detail view: the shell hides its
    // bottom bar and menu (`body.view-detail`, FR-L28, #885). The system back clears the shared
    // flag (`BackStep::CloseDetail`), which takes this screen back to the 멀티 view.
    {
        let want = crate::ui::session::is_detail_view(window.width_dp, view_now, target.is_some());
        let wrote = detail_wrote.clone();
        match crate::ui::session::detail_sync(want, wrote.get_untracked(), ui.detail_open.get()) {
            crate::ui::session::DetailSync::Keep => {
                if wrote.get_untracked() != want {
                    wrote.set(want);
                }
            }
            crate::ui::session::DetailSync::Write(v) => {
                wrote.set(v);
                ui.detail_open.set(v);
            }
            crate::ui::session::DetailSync::Back => {
                wrote.set(false);
                set_view(SessionView::Multi);
            }
        }
    }
    let total_m = members.len();
    let wait_m = members
        .iter()
        .filter(|m| m.status == LauncherStatus::WaitingForApproval)
        .count();
    let run_m = members
        .iter()
        .filter(|m| m.status == LauncherStatus::Running)
        .count();
    let mut sub_parts = vec![format!("세션 · 팀원 {total_m}")];
    if wait_m > 0 {
        sub_parts.push(format!("승인 대기 {wait_m}"));
    }
    if run_m > 0 {
        sub_parts.push(format!("실행 중 {run_m}"));
    }
    let sub_line = sub_parts.join(" · ");

    { use ::compose_rust::ui::Box; Box!(alignment = Alignment::TopStart, modifier = Modifier::new().fill_max_width().fill_max_height(), content = move || {
            let sections_col = sections.clone();
            let current_col = current.clone();
            let target_col = target.clone();
            let query_col = query.clone();
            let open_col = open_member.clone();
            let rc_col = rail_collapsed.clone();
            let dr_rail = drawer.clone();

            { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width().fill_max_height(), {
                    if show_column {
                        { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new()
                                    .width(SIDE_LIST_W)
                                    .fill_max_height()
                                    .background(paint(p.surface_2)), content = move || {
                                let q_st = query_col.clone();
                                let rc_btn = rc_col.clone();
                                SideList(
                                    &sections_col,
                                    &current_col,
                                    target_col.as_deref(),
                                    view_now == SessionView::Multi,
                                    Some(Box::new(move || rc_btn.set(true))),
                                    move |q| q_st.set(q),
                                    open_col.clone(),
                                );
                            }) };
                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new()
                                .width(1.0)
                                .fill_max_height()
                                .background(paint(p.line))) };
                    } else if show_rail {
                        let sec_r = sections_col.clone();
                        let tgt_r = target_col.clone();
                        let op_r = open_col.clone();
                        let rc_exp = rc_col.clone();
                        let dr_exp = dr_rail.clone();
                        SideListRail(
                            &sec_r,
                            tgt_r.as_deref(),
                            move || {
                                if is_tablet_rail {
                                    dr_exp.set(true);
                                } else {
                                    rc_exp.set(false);
                                }
                            },
                            op_r,
                        );
                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new()
                                .width(1.0)
                                .fill_max_height()
                                .background(paint(p.line))) };
                    }

                    { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().weight(1.0).fill_max_height(), {
                            // Top toolbar (.sesshead)
                            // The version control, web page and IDE controls. At 560 and below the
                            // mockup's header wraps (`.sesshead{flex-wrap:wrap;padding-right:60px}`):
                            // they go to a second row, so the title keeps its room and nothing
                            // runs under the floating menu button (FR-L28.2, FR-L28.6).
                            let trailing = || {
                                let vo = vcs_open.clone();
                                ToolbarBtn(ICON_GIT_SVG, "버전 관리", SESSION_HEADER_LABELS, false, None, move || vo.set(true));
                                let wm = open_web_menu.clone();
                                let wm_d = web_menu.clone();
                                #[allow(clippy::redundant_closure)]
                                WebMenu(web_menu.get(), &web_pages.get(), web_sub.get().as_deref(), move || wm_d.set(false), move || wm());
                                if let Some(t) = target.clone() {
                                    IdeMenu(&t);
                                } else {
                                    AppButton(
                                        "IDE 열기",
                                        Some(IconRole::Forward),
                                        ButtonKind::Default,
                                        true,
                                        false,
                                        false,
                                        || {},
                                    );
                                }
                            };
                            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 6.0, modifier = Modifier::new()
                                        .fill_max_width()
                                        .height(if is_phone { PHONE_HEAD_ROW_H } else { 60.0 })
                                        .background(paint(p.surface))
                                        // Phone: 8 around the 32 dp controls (one padding for all
                                        // sides; the mockup's start is 12) and 60 at the end with
                                        // the menu room.
                                        .padding(if is_phone { SP[1] } else { SP[2] }), {
                                    if is_phone {
                                        let dr = drawer.clone();
                                        NamedClickable(
                                            super::a11y::MENU,
                                            Modifier::new()
                                                .size(32.0, 32.0)
                                                .background(paint(p.surface))
                                                .border(1.0, paint(p.line))
                                                .shape_role(ShapeRole::Full)
                                                .expanded(drawer.get()),
                                            Alignment::Center,
                                            move || dr.set(true),
                                            || NavIcon(ICON_LIST_SVG, p.text_2.to_argb(), 15.0),
                                        );
                                    }
                                    let pm = project_menu.clone();
                                    let p_name = project_name.clone();
                                    let s_line = sub_line.clone();
                                    let mut proj_mod = Modifier::new().padding(2.0);
                                    if is_phone {
                                        // FR-L36 (#770): keep the + button right after the title
                                        // while capping long project names so + stays clear of the
                                        // floating menu button.
                                        proj_mod = proj_mod.max_width(phone_header_title_max_w(current_window_size().width_dp));
                                    }
                                    // The control says whether its menu is open; its text is its name.
                                    let proj_open = project_menu.get();
                                    let control = move || {
                                    Clickable(
                                        proj_mod.expanded(proj_open),
                                        Alignment::CenterStart,
                                        move || pm.set(!pm.get_untracked()),
                                        move || {
                                            { use ::compose_rust::ui::Column; Column!(spacing = 1.0, content = move || {
                                                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 4.0, {
                                                            Label(
                                                                &p_name,
                                                                if is_phone {
                                                                    Fs::Md
                                                                } else {
                                                                    Fs::Lg
                                                                },
                                                                Weight::Semibold,
                                                                Some(paint(p.text)),
                                                                Some(1),
                                                            );
                                                            NavIcon(
                                                                ICON_CHEVRON_DOWN_SVG,
                                                                p.muted.to_argb(),
                                                                13.0,
                                                            );
                                                        }) };
                                                    Label(
                                                        &s_line,
                                                        Fs::Xs2,
                                                        Weight::Regular,
                                                        Some(paint(p.muted)),
                                                        Some(1),
                                                    );
                                                }) };
                                        },
                                    );
                                    };
                                    if is_phone {
                                        // The phone's bottom sheet is drawn apart from the control
                                        // (below), so the control keeps its weight in this row.
                                        control();
                                    } else {
                                        ProjectSwitchMenu(&project_menu, &boxes, &current, &project, &ui, control);
                                    }
                                    if !is_phone {
                                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                                    }
                                    if !chips_below {
                                        let sv = set_view.clone();
                                        ViewSeg(view_now, hide_view_lbl, false, move |v| sv(v));
                                    }
                                    {
                                        // Icon-only 32 px buttons with tooltips at every width, as the
                                        // mockup's `.sesshead` (owner placement rule 2026-11-13, rule 6).
                                        let nm = new_member_open.clone();
                                        ToolbarBtn(ICON_PLUS_SVG, "새 팀원", SESSION_HEADER_LABELS, false, None, move || nm.set(true));
                                    }
                                    if is_phone {
                                        // Fill the remaining row after + and keep clear of the
                                        // shell's floating menu button.
                                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(super::project::PHONE_MENU_ROOM)) };
                                    } else {
                                        trailing();
                                    }
                                }) };
                            if is_phone {
                                // The wrapped second row: version control, web pages, IDE.
                                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 6.0, modifier = Modifier::new()
                                            .fill_max_width()
                                            .background(paint(p.surface))
                                            .padding(SP[1]), {
                                        trailing();
                                    }) };
                            }

                            if chips_below {
                                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new()
                                            .fill_max_width()
                                            .background(paint(p.surface))
                                            .padding(SP[1]), {
                                        let sv = set_view.clone();
                                        ViewSeg(view_now, hide_view_lbl, true, move |v| sv(v));
                                    }) };
                            }

                            { use ::compose_rust::ui::Separator; Separator!(Some(paint(p.line))) };

                            // Body view
                            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(1.0).fill_max_width(), content = || match view_now {
                                    SessionView::Multi => {
                                        MultiView(
                                            &current,
                                            &members,
                                            focus.clone(),
                                            to_single.clone(),
                                        );
                                    }
                                    SessionView::Single => match target.clone() {
                                        Some(t) => SingleView(&t, focus.clone(), to_multi.clone()),
                                        None => {
                                            { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new()
                                                        .fill_max_width()
                                                        .fill_max_height(), {
                                                    Label(
                                                        NO_SESSION,
                                                        Fs::Md,
                                                        Weight::Regular,
                                                        Some(paint(p.muted)),
                                                        None,
                                                    );
                                                }) };
                                        }
                                    },
                                    SessionView::Status => {
                                        StatusView(&current, &members, to_single.clone());
                                    }
                                }) };
                        }) };
                }) };

            // Left drawer with a scrim on compact/tablet (#582). The toolkit's Sheet picks its
            // own edge and Dialog centres, so this is an in-tree overlay stacked in the screen's
            // Box. It covers the session content area, not the phone tab bar or title strip
            // (the shell is outside this screen).
            // v0.0.7: #476 fixed (containers take `on_key_down`, and `Batch::request_focus`
            // exists); workaround kept until checked on a display (the drawer has to take focus
            // when it opens before Escape can reach it); the scrim and the close button close it.
            // The toggle, the scrim and the drawer are named (compose-rust v0.0.7, #429).
            if crate::ui::session::drawer_open_effective(show_column, drawer.get()) {
                let dr_s = drawer.clone();
                let dr_x = drawer.clone();
                let sec_d = sections.clone();
                let cur_d = current.clone();
                let tgt_d = target.clone();
                let q_d = query.clone();
                let op_d = open_member.clone();
                let is_m = view_now == SessionView::Multi;
                let theme = ember_theme();
                let pal = theme.palette;
                let w = crate::ui::session::drawer_width(window.width_dp);
                NamedClickable(
                    super::a11y::CLOSE,
                    Modifier::new()
                        .fill_max_width()
                        .fill_max_height()
                        .background(paint(Color::argb(0x40000000))),
                    Alignment::TopStart,
                    move || dr_s.set(false),
                    || {},
                );
                { use ::compose_rust::ui::Row; Row!(modifier = with_shadows(Modifier::new(), theme.shadow_sm())
                            .width(w)
                            .fill_max_height()
                            .role(compose_rust::ui::Role::Navigation)
                            .label(super::a11y::SESSION_MENU), content = move || {
                        { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(1.0).fill_max_height(), content = move || {
                                SideList(
                                    &sec_d,
                                    &cur_d,
                                    tgt_d.as_deref(),
                                    is_m,
                                    Some(Box::new(move || dr_x.set(false))),
                                    move |q| q_d.set(q),
                                    op_d.clone(),
                                );
                            }) };
                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(1.0).fill_max_height().background(paint(pal.line))) };
                    }) };
            }

            // New member sheet (#snew)
            {
                let nm_d = new_member_open.clone();
                NewMemberSheet(&current, new_member_open.get(), move || nm_d.set(false));
            }

            // 버전 관리 sheet over the conversation (#495)
            {
                let vo_d = vcs_open.clone();
                super::vcs::VcsSheet(&current, vcs_open.get(), move || vo_d.set(false));
            }

            // The project menu's phone sheet (FR-L10.4): apart from the control in the bar.
            if is_phone {
                ProjectSwitchMenu(&project_menu, &boxes, &current, &project, &ui, || {});
            }

        }) };
}

/// Maximum width of the project title button in the phone session header (FR-L36, #770):
/// reserves room for the drawer button, the + button right after the title, row padding, gaps,
/// and `PHONE_MENU_ROOM` so long names ellipsize instead of pushing + under the floating menu.
pub fn phone_header_title_max_w(window_w: f32) -> f32 {
    (window_w - 150.0).max(80.0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn fr_l18_phone_header_keeps_icon_buttons_instead_of_a_folded_menu() {
        let src = include_str!("session.rs").split("mod tests {").next().unwrap();
        let head = src.split("// Top toolbar (.sesshead)").nth(1).unwrap();
        let head = head.split("if chips_below {").next().unwrap();
        assert!(!head.contains("ICON_MORE_H_SVG"));
        // Owner placement rule 2026-11-13 (rule 6): icon only at every width.
        assert!(!super::SESSION_HEADER_LABELS);
        assert_eq!(head.matches(", SESSION_HEADER_LABELS, false, None, move ||").count(), 2);
        assert!(!head.contains("show_btn_lbl"));
        // The globe is the 웹 페이지 menu's control, a split with a chevron (review SE3, #888).
        let web = src.split("fn WebMenu(").nth(1).unwrap().split("\nfn ").next().unwrap();
        assert!(web.contains("ToolbarBtn(ICON_GLOBE_SVG, \"웹 페이지\", SESSION_HEADER_LABELS, true, Some(open), on_open)"));
        for icon in ["ICON_PLUS_SVG", "ICON_GIT_SVG", "WebMenu(", "IdeMenu(&t)"] {
            assert!(head.contains(icon), "{icon}");
        }
        assert!(!src.contains("more_menu"));
    }

    fn globe_semantics(expanded: Option<bool>) -> Vec<String> {
        use compose_rust::protocol::{decode_batch, Mutation};
        use compose_rust::{Host, Recomposer};
        thread_local!(static EXPANDED: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) });
        EXPANDED.with(|e| e.set(expanded));
        fn globe() {
            ToolbarBtn(ICON_GLOBE_SVG, "웹 페이지", false, true, EXPANDED.with(|e| e.get()), || {});
        }
        let mut host = Host::new(|| Box::new(Recomposer::new(globe)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetModifier { modifier, .. } => Some(format!("{modifier:?}")),
                Mutation::SetProp { property, value, .. } => Some(format!("{property:?}={value:?}")),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn fr_l10_4_the_globe_is_a_named_button_that_says_whether_its_menu_is_open() {
        let _guard = crate::services::test_guard();
        let (closed, open, plain) = (globe_semantics(Some(false)), globe_semantics(Some(true)), globe_semantics(None));
        let sem = |all: &[String]| all.iter().find(|m| m.starts_with("Semantics")).cloned().expect("semantics");
        assert!(sem(&closed).contains("Button"), "{closed:?}");
        assert!(closed.iter().any(|p| p.contains("웹 페이지") && !p.starts_with("Text=")), "named: {closed:?}");
        // Collapsed, expanded and (for a plain action) neither are three different states.
        assert_ne!(sem(&closed), sem(&open));
        assert_ne!(sem(&closed), sem(&plain));
        assert_ne!(sem(&open), sem(&plain));
    }

    #[test]
    fn fr_e10_chooser_rows_carry_the_notice_on_the_ember_row_only() {
        use crate::ide::IdeKind;
        use crate::ui::session::ide_choices;
        let launch_with = |commit: &str| {
            Ok(crate::demo::ide_launch_with(
                "s",
                "p",
                "/w",
                Some(ember_client::wire::EmberOseConnection {
                    endpoint: "http://127.0.0.1:9000".into(),
                    relay: None,
                    connection_token: String::new(),
                    ose_commit: Some(commit.to_string()),
                }),
            ))
        };
        for remembered in [None, Some(IdeKind::VsCode)] {
            let choices = ide_choices(remembered);
            let plain = ide_pop_rows(&choices, remembered);
            let other = launch_with("0036dcb6c18a603d2168c00fa578be600987ac7b");
            let rows = with_ember_notice(ide_pop_rows(&choices, remembered), &choices, Some(&other));
            for ((row, base), c) in rows.iter().zip(&plain).zip(&choices) {
                if c.kind == IdeKind::Ember {
                    assert!(row.description.as_deref().unwrap().contains("0036dcb6c18a"));
                    assert!(row.badge.is_some());
                    assert_eq!(row.title, base.title);
                } else {
                    assert_eq!(row.description, base.description);
                    assert!(row.badge.is_none());
                }
                assert_eq!(row.chosen, base.chosen);
            }
            let pinned = launch_with(ember_editor_conn::PINNED_COMMIT);
            let same = with_ember_notice(ide_pop_rows(&choices, remembered), &choices, Some(&pinned));
            assert_eq!(
                same.iter().map(|r| (&r.title, &r.description, r.badge.is_some())).collect::<Vec<_>>(),
                plain.iter().map(|r| (&r.title, &r.description, r.badge.is_some())).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn fr_l10_4_the_ide_menu_always_marks_the_current_ide() {
        use crate::ide::IdeKind;
        use crate::ui::session::{current_ide, ide_choices};
        // A fresh profile: Ember's own editor is the current one (mockup `on = s.ide || "ember"`).
        assert_eq!(current_ide(None), IdeKind::Ember);
        let rows = ide_pop_rows(&ide_choices(None), None);
        let chosen: Vec<&str> = rows.iter().filter(|r| r.chosen).map(|r| r.title.as_str()).collect();
        assert_eq!(chosen, ["Ember 에디터"]);
        // A remembered IDE is first and the only one marked.
        let rows = ide_pop_rows(&ide_choices(Some(IdeKind::VsCode)), Some(IdeKind::VsCode));
        assert_eq!((rows[0].title.as_str(), rows[0].chosen), ("VS Code", true));
        assert_eq!(rows.iter().filter(|r| r.chosen).count(), 1);
        // Both IDE menus build their rows here.
        let project = include_str!("project.rs").split("#[cfg(test)]").next().unwrap();
        assert!(project.contains("crate::ui::cr::session::ide_pop_rows(&choices, remembered)"));
    }

    fn project_switch_sample() {
        let _ = services();
        let open = remember(|| mutable_state_of(true));
        let project = remember(|| mutable_state_of(None::<String>));
        let boxes = services().client.read(|s| project_boxes(s));
        let current = boxes.first().map(|b| b.id.clone()).unwrap_or_default();
        ProjectSwitchMenu(&open, &boxes, &current, &project, shared_ui(), || {
            Label("프로젝트 컨트롤", Fs::Lg, Weight::Semibold, None, Some(1));
        });
    }

    #[test]
    fn fr_l10_4_session_project_menu_is_the_shared_menu() {
        use compose_rust::protocol::{decode_batch, Mutation};
        let _guard = crate::services::test_guard();
        crate::ui::window_size::set_opened_for_test(Some((1440.0, 900.0)));
        let mut host = compose_rust::Host::new(|| Box::new(compose_rust::Recomposer::new(project_switch_sample)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        crate::ui::window_size::set_opened_for_test(None);
        let mut nodes: Vec<(u32, String, Vec<String>, Vec<String>)> = Vec::new();
        for m in batch.iter() {
            match m {
                Mutation::Create { node_id, widget } => nodes.push((*node_id, format!("{widget:?}"), Vec::new(), Vec::new())),
                Mutation::SetModifier { node_id, modifier, .. } => {
                    if let Some(n) = nodes.iter_mut().find(|n| n.0 == *node_id) {
                        n.2.push(format!("{modifier:?}"));
                    }
                }
                Mutation::SetProp { node_id, property, value } => {
                    if let Some(n) = nodes.iter_mut().find(|n| n.0 == *node_id) {
                        n.3.push(format!("{property:?}={value:?}"));
                    }
                }
                _ => {}
            }
        }
        assert!(!nodes.iter().any(|n| n.1 == "Dialog" || n.1 == "Sheet"), "no centred dialog at 1440");
        let pops: Vec<_> = nodes.iter().filter(|n| n.1 == "Popover").collect();
        assert_eq!(pops.len(), 1);
        for want in ["Open=Bool(true)", "Placement=Integer(2)", "OffsetDp=Float(8.0)"] {
            assert!(pops[0].3.iter().any(|p| p == want), "{want} in {:?}", pops[0].3);
        }
        assert!(!pops[0].3.iter().any(|p| p == "Align=Integer(3)"));
        assert!(pops[0].2.iter().any(|m| m == "Width(340.0)"), "{:?}", pops[0].2);
        let boxes = services().client.read(|s| project_boxes(s));
        let current = boxes.first().map(|b| b.id.clone()).unwrap_or_default();
        let rows = project_switch_rows(&boxes, &current);
        assert!(!boxes.is_empty(), "the demo has projects, so the rows below are checked");
        assert_eq!(rows.len(), boxes.len());
        assert_eq!(rows.iter().filter(|r| r.chosen).count(), usize::from(!boxes.is_empty()));
        assert!(rows.iter().all(|r| r.icon.is_some() && r.description.as_deref().is_some_and(|d| d.starts_with("팀원 "))));
        let says = |t: &str| nodes.iter().any(|n| n.3.iter().any(|p| p.contains(t)));
        assert!(says(&project_switch_head(&current)) && says("프로젝트 컨트롤"));
        for r in &rows {
            assert!(says(&r.title), "missing {}", r.title);
        }
        let items = nodes.iter().filter(|n| n.2.iter().any(|m| m.starts_with("Semantics") && m.contains("MenuItem"))).count();
        assert_eq!(items, rows.len());
    }

    #[test]
    fn fr_l10_4_session_pickers_hang_from_their_controls() {
        let src = include_str!("session.rs").split("mod tests {").next().unwrap();
        let body = |name: &str| -> String {
            let rest = src.split(&format!("fn {name}(")).nth(1).unwrap_or_else(|| panic!("no fn {name}"));
            rest.split("\nfn ").next().unwrap().split("\npub fn ").next().unwrap().to_string()
        };
        let mode = body("ModeChip");
        assert!(mode.contains("PopoverMenu(") && mode.contains("PopKind::Mode"), "the permission chip opens a popover");
        assert!(mode.contains("super::home::ICON_WARN") && mode.contains("danger: m.is_bypass()"), "bypass is the red warning row");
        assert!(mode.contains(".expanded(expanded)"), "the chip says whether the menu is open");
        let ide = body("IdeMenu");
        assert!(ide.contains("PopoverMenu(") && ide.contains("PopKind::Drop") && ide.contains("PopoverAlign::End"));
        assert!(ide.contains("IDE_MENU_HEAD") && ide.contains("ide_pop_rows(&choices, remembered)"));
        let web = body("WebMenu");
        assert!(web.contains("PopoverMenu(") && web.contains("WEB_MENU_HEAD") && web.contains("PillTone::Green"));
        // The `computer · ip` line under the head is passed in and sent as the menu's subtitle.
        assert!(web.contains("sub: Option<&str>") && web.contains("            sub,\n"), "{web}");
        assert!(src.contains("sub.set(crate::ui::session::web_menu_subtitle(Some(&name), &host));"));
        assert!(src.contains("web_sub.get().as_deref()"));
        // The globe is a named button that says whether its menu is open (review r8).
        assert!(web.contains("ToolbarBtn(ICON_GLOBE_SVG, \"웹 페이지\", SESSION_HEADER_LABELS, true, Some(open), on_open)"), "{web}");
        // The model chip's one line hangs from the chip too, with the mockup's own sheet on a phone.
        let model = body("ModelChip");
        assert!(model.contains("PopoverMenuWith(") && model.contains("PopKind::Mode") && model.contains("plain_sheet: true"));
        assert!(model.contains(".expanded(expanded)") && model.contains("model_chip_rows()"));
        let rows = super::model_chip_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "실행 중인 세션의 모델은 바꿀 수 없습니다");
        assert_eq!(rows[0].badge, Some((crate::ui::session::SOON.to_string(), PillTone::Grey)));
        // The project switcher (a native select in the mockup) is the shared menu too.
        let switch = body("ProjectSwitchMenu");
        assert!(switch.contains("PopoverMenuWith(") && switch.contains("PopKind::Mode") && switch.contains("plain_sheet: true"));
        assert!(switch.contains("PopoverAlign::Start") && switch.contains("a11y::PROJECT_PICKER"));
        assert!(src.contains("proj_mod.expanded(proj_open)"), "the control says whether the menu is open");
        assert!(src.contains("ProjectSwitchMenu(&project_menu, &boxes, &current, &project, &ui, control);"));
        assert!(src.contains("ProjectSwitchMenu(&project_menu, &boxes, &current, &project, &ui, || {});"), "the phone sheet");
        assert!(src.contains("EMBER_CR_SESSION_MENU\").as_deref() == Ok(\"project\")"));
        assert!(!src.contains("// Project picker OptionSheet"));
        for b in [&mode, &ide, &web, &model] {
            assert!(!b.contains("OptionSheet("), "no centred option sheet is left in {b:.40}");
        }
        assert!(!src.contains("// Web pages OptionSheet"));
    }

    #[test]
    fn fr_l36_phone_header_places_plus_right_after_title_and_clears_menu() {
        assert_eq!(super::phone_header_title_max_w(390.0), 240.0);
        assert_eq!(super::phone_header_title_max_w(360.0), 210.0);
        assert_eq!(super::phone_header_title_max_w(200.0), 80.0);
        let src = include_str!("session.rs").split("mod tests {").next().unwrap();
        assert!(src.contains("proj_mod = proj_mod.max_width(phone_header_title_max_w(current_window_size().width_dp));"));
        assert!(!src.contains("proj_mod = proj_mod.weight(1.0);"));
    }

    #[test]
    fn fr_l18_subagent_tree_children_hang_from_a_guide_line() {
        // compose-rust#464: the children's column has a one-sided left border, as `.tree ul`.
        let src = include_str!("session.rs").split("mod tests {").next().unwrap();
        let tree = src.split("fn TreeNodeView(").nth(1).unwrap();
        let tree = tree.split("\nfn ").next().unwrap();
        assert!(tree.contains(".border_side(Side::Start, 1.0, paint(p.line))"));
        assert!(tree.contains("Modifier::new().width(TREE_GUIDE_INSET)"));
        assert!(tree.contains("Modifier::new().width(TREE_CHILD_PAD)"));
        assert_eq!((super::TREE_GUIDE_INSET, super::TREE_CHILD_PAD), (14.0, 12.0));
        assert!(!src.contains(concat!("TODO(compose-rust#", "464)")));
    }

    #[test]
    fn fr_l18_one_pane_layout_is_a_sideways_strip_not_a_pager() {
        let src = include_str!("session.rs").split("mod tests {").next().unwrap();
        let mv = src.split("pub fn MultiView(").nth(1).unwrap();
        let mv = mv.split("\npub fn ").next().unwrap();
        let paged = mv.split("} else if layout.paged {").nth(1).unwrap();
        let paged = paged.split("let rows_idx = pane_rows(").next().unwrap();
        assert!(paged.contains("ScrollRow!("));
        assert!(paged.contains(".width(pane_w)"));
        // compose-rust#547: the strip snaps to whole panes, so no peek stand-in is left.
        assert!(paged.contains("snap = Snap::ItemStart"));
        assert!(!paged.contains("TODO(compose-rust#547)"));
        assert!(!paged.contains("spacing = SWIPE_GAP"));
        assert!(mv.contains("multi_pane_layout(window.width_dp, window.class)"));
        assert!(!paged.contains("\"이전\""));
        assert!(!paged.contains("\"다음\""));
    }

    #[test]
    fn fr_l18_drawer_state_resets_and_scrim_and_navigation_close_it() {
        let src = include_str!("session.rs").split("mod tests {").next().unwrap();
        assert!(src.contains("if show_column && drawer.get() {\n        drawer.set(false);"));
        assert!(src.contains("drawer_open_effective(show_column, drawer.get())"));
        // The scrim's Clickable closes the drawer.
        assert!(src.contains("move || dr_s.set(false),"));
        // open_member closes it.
        let om = src.split("let open_member = {").nth(1).unwrap();
        assert!(om.split("let same = proj == current;").next().unwrap().contains("drawer.set(false);"));
        // compose-rust#429 (v0.0.7): the toggle, the scrim and the drawer are named.
        assert!(src.contains("super::a11y::MENU,"));
        assert!(src.contains(".label(super::a11y::SESSION_MENU)"));
        assert!(!src.contains("TODO(compose-rust#429)"));
    }

    #[test]
    fn fr_l18_pane_header_icon_toggle_at_420_and_below_segment_above() {
        assert!(super::pane_header_uses_icon_toggle(360.0));
        assert!(super::pane_header_uses_icon_toggle(420.0));
        assert!(!super::pane_header_uses_icon_toggle(421.0));
        assert!(!super::pane_header_uses_icon_toggle(600.0));
        assert!(super::pane_header_uses_icon_toggle(0.0));
    }

    #[test]
    fn fr_l18_pane_width_unknown_window_is_narrow_and_columns_split_the_body() {
        assert_eq!(super::pane_width(0.0, 2, 72.0), 0.0);
        assert!(super::pane_header_uses_icon_toggle(super::pane_width(0.0, 1, 0.0)));
        assert_eq!(super::pane_width(390.0, 1, 0.0), 366.0);
        assert!(!super::pane_header_uses_icon_toggle(super::pane_width(1440.0, 2, 234.0)));
    }

    #[test]
    fn fr_l18_status_cards_equal_height_only_when_multi_column() {
        assert!(!super::status_cards_equal_height(super::status_grid_columns(390.0)));
        assert!(super::status_cards_equal_height(super::status_grid_columns(900.0)));
        assert!(super::status_cards_equal_height(super::status_grid_columns(1280.0)));
        let src = include_str!("session.rs").split("mod tests {").next().unwrap();
        assert!(src.contains("intrinsic_size = status_cards_equal_height(3)"));
        assert!(src.contains("intrinsic_size = status_cards_equal_height(2)"));

    }

    #[test]
    fn fr_l18_status_capture_scroll_cards_hides_the_blocks_above() {
        assert!(super::status_blocks_above_cards(None));
        assert!(!super::status_blocks_above_cards(Some("cards")));
        assert!(super::status_blocks_above_cards(Some("other")));
    }

    use super::*;

    #[test]
    fn pane_terminal_follows_its_newest_line_with_the_toolkit_scroll_state() {
        // compose-rust#385 (#673): the terminal pane hands its ScrollColumn the scroll state that
        // `FollowNewest` moves, so no stand-in marker for scrolling to the bottom is left.
        let src = include_str!("session.rs");
        let src = &src[..src.find("#[cfg(test)]").unwrap()];
        let doc = src.split("pub fn PaneTerminal(").next().unwrap();
        assert!(!doc.contains(concat!("TODO(compose-rust): scroll to the ", "bottom")));
        let term = src.split("pub fn PaneTerminal(").nth(1).unwrap();
        let term = term.split("\npub fn ").next().unwrap();
        assert!(term.contains("let scroll = cu::remember_scroll_state();"));
        assert!(term.contains("FollowNewest(scroll.clone(), format!(\"{sid}:terminal\"));"));
        assert!(term.contains("ScrollColumn!(scroll_state = &scroll,"));

    }

    #[test]
    fn nfr_l3_pane_transcript_opens_at_the_newest_message() {
        // First layout: the whole transcript was kept (height unknown), go to the end.
        assert_eq!(follow_newest(412.0, 0.0, 0.0), Some(412.0));
        // Nothing changed: no second request for the same distance.
        assert_eq!(follow_newest(412.0, 412.0, 412.0), None);
        // The content fits the pane: nothing to scroll.
        assert_eq!(follow_newest(0.0, 0.0, 0.0), None);
    }

    #[test]
    fn nfr_l3_pane_transcript_follows_new_content_only_from_the_end() {
        // At the end, or within about a line of it: a new message is followed.
        assert_eq!(follow_newest(520.0, 412.0, 412.0), Some(520.0));
        assert_eq!(follow_newest(520.0, 412.0, 412.0 - FOLLOW_NEAR_END_DP), Some(520.0));
        // Scrolled up to read: the position is left alone.
        assert_eq!(follow_newest(520.0, 412.0, 412.0 - FOLLOW_NEAR_END_DP - 1.0), None);
        assert_eq!(follow_newest(520.0, 412.0, 0.0), None);
    }
    use compose_rust::protocol::{HostEvent, Mutation, PropertyValue, decode_batch};
    use compose_rust::{
        EventPayload, Host, Recomposer, WindowHeightClass, WindowSizeClass, Composable,
    };

    fn texts(batch: &[Mutation<'_>]) -> Vec<String> {
        batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetProp {
                    value: PropertyValue::String(s),
                    ..
                } => Some(s.to_string()),
                _ => None,
            })
            .collect()
    }

    thread_local! {
        static CHEVRON_OPEN: RefCell<Option<MutableState<bool>>> = const { RefCell::new(None) };
    }

    #[Composable]
    fn host_ide_chevron() {
        let open = remember(|| mutable_state_of(false));
        CHEVRON_OPEN.with(|c| *c.borrow_mut() = Some(open.clone()));
        IdeChevron(open.clone(), false);
    }

    /// FR-L7, #270: with an IDE remembered, every header's main button launches it and leaves
    /// the chooser closed; with none, it opens the chooser and launches nothing.
    #[test]
    fn fr_l7_header_main_button_launches_the_remembered_ide_and_opens_the_chooser_when_unset() {
        assert_eq!(ide_click(Some(IdeKind::VsCode)), IdeClick::Launch(IdeKind::VsCode));
        assert_eq!(ide_click(None), IdeClick::Choose);

        let _guard = crate::services::test_guard();
        for kind in IdeKind::ALL {
            let open = mutable_state_of(false);
            let mut launched = Vec::new();
            ide_main_click(Some(kind), &open, |k| launched.push(k));
            assert_eq!(launched, vec![kind]);
            assert!(!open.get_untracked(), "a remembered IDE opens without the chooser");
        }
        let open = mutable_state_of(false);
        let mut launched = Vec::new();
        ide_main_click(None, &open, |k| launched.push(k));
        assert!(launched.is_empty(), "nothing is launched without a default");
        assert!(open.get_untracked(), "an unset default opens the chooser");
    }

    #[Composable]
    fn host_ide_chevron_in_the_pill() {
        let open = remember(|| mutable_state_of(false));
        IdeChevron(open.clone(), true);
    }

    #[Composable]
    fn host_pane_tab_alone() {
        PaneTabChip("작업 1", 2, true, false, || {}, || {});
    }

    #[Composable]
    fn host_pane_tab_with_close() {
        PaneTabChip("작업 1", 2, true, true, || {}, || {});
    }

    /// What a composed pane tab sends: the nodes with a click handler in the order sent, every
    /// node's parent and every node's modifiers as debug text.
    struct PaneTabSent {
        clicked: Vec<u32>,
        parents: Vec<(u32, u32)>,
        modifiers: Vec<(u32, String)>,
    }

    impl PaneTabSent {
        fn of(content: fn()) -> Self {
            let mut host = Host::new(move || Box::new(Recomposer::new(content)));
            let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
            let mut sent = PaneTabSent { clicked: Vec::new(), parents: Vec::new(), modifiers: Vec::new() };
            for m in batch.iter() {
                match m {
                    Mutation::SetProp { node_id, property: compose_rust::PropertyKind::OnClick, .. } => sent.clicked.push(*node_id),
                    Mutation::Insert { parent_id, node_id, .. } => sent.parents.push((*node_id, *parent_id)),
                    Mutation::SetModifier { node_id, modifier, .. } => sent.modifiers.push((*node_id, format!("{modifier:?}"))),
                    _ => {}
                }
            }
            sent
        }

        fn mods(&self, id: u32) -> Vec<String> {
            self.modifiers.iter().filter(|(n, _)| *n == id).map(|(_, m)| m.clone()).collect()
        }

        fn parent(&self, id: u32) -> Option<u32> {
            self.parents.iter().find(|(n, _)| *n == id).map(|(_, p)| *p)
        }
    }

    /// FR-L34.4, #251: a pane tab's click area carries the tab's corners, so hover, press and the
    /// focus ring are not a square inside the 12 dp rounded tab. Alone it is the whole tab; with
    /// a close button after it, it is rounded at the start only.
    #[test]
    fn fr_l34_4_a_pane_tab_click_area_carries_the_tab_corners() {
        assert_eq!(CORNER_MD, 12.0);
        assert_eq!(pane_tab_click_radii(false), [12.0, 12.0, 12.0, 12.0]);
        assert_eq!(pane_tab_click_radii(true), [12.0, 0.0, 0.0, 12.0]);
        let _guard = crate::services::test_guard();
        let shape = |r: [f32; 4]| {
            format!("{:?}", compose_rust::schema::Modifier::Shape { top_start: r[0], top_end: r[1], bottom_end: r[2], bottom_start: r[3] })
        };
        let tab_shape = shape([12.0; 4]);
        // Alone: one click area, the whole tab, with the tab's four corners.
        let alone = PaneTabSent::of(host_pane_tab_alone);
        assert_eq!(alone.clicked.len(), 1, "{:?}", alone.clicked);
        let mods = alone.mods(alone.clicked[0]);
        assert!(mods.contains(&tab_shape), "{mods:?}");
        assert!(mods.iter().any(|m| m == "FillMaxHeight"), "{mods:?}");
        // Its parent is the tab itself: 34 high with the same corner.
        let tab = alone.mods(alone.parent(alone.clicked[0]).expect("the tab"));
        assert!(tab.iter().any(|m| m == "Height(34.0)") && tab.contains(&tab_shape), "{tab:?}");
        // With a close button: the name's click area is rounded at the start and square where
        // the close button follows; the close button keeps its circle.
        let both = PaneTabSent::of(host_pane_tab_with_close);
        assert_eq!(both.clicked.len(), 2, "{:?}", both.clicked);
        let (name, close) = (both.mods(both.clicked[0]), both.mods(both.clicked[1]));
        assert!(name.contains(&shape([12.0, 0.0, 0.0, 12.0])), "{name:?}");
        assert!(!name.contains(&tab_shape), "{name:?}");
        assert!(close.iter().any(|m| m.starts_with("ShapeRole(Full")), "{close:?}");
        assert!(close.iter().any(|m| m == "Width(24.0)") && close.iter().any(|m| m == "Height(24.0)"), "{close:?}");
        assert!(both.parent(both.clicked[0]).is_some());
        assert_eq!(both.parent(both.clicked[0]), both.parent(both.clicked[1]), "both are children of the tab");
        let tab = both.mods(both.parent(both.clicked[0]).expect("the tab"));
        assert!(tab.contains(&tab_shape), "the tab keeps its own four corners: {tab:?}");
        // The tab strip draws its tabs with it, and no tab is left with a bare click area.
        let src = include_str!("session.rs").split("#[cfg(test)]").next().unwrap();
        assert_eq!(src.matches("PaneTabChip(").count(), 2, "the definition and the tab strip");
        assert!(src.contains("Modifier::new().fill_max_height().corner_radii(pane_tab_click_radii(closable))"));
    }

    /// FR-L34.2, #251: the two halves of the "IDE 열기" split pill take the pill's rounded ends,
    /// so the hover layer, the press tint and the keyboard focus ring stay inside the pill.
    #[test]
    fn fr_l34_2_the_split_pill_halves_carry_the_pill_ends_as_their_shape() {
        assert_eq!(split_start_radii(32.0), [16.0, 0.0, 0.0, 16.0]);
        assert_eq!(split_end_radii(32.0), [0.0, 16.0, 16.0, 0.0]);
        assert_eq!(split_end_radii(40.0), [0.0, 20.0, 20.0, 0.0]);
        let _guard = crate::services::test_guard();
        let mut host = Host::new(|| Box::new(Recomposer::new(host_ide_chevron_in_the_pill)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let clicked: Vec<u32> = batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetProp { node_id, property: compose_rust::PropertyKind::OnClick, .. } => Some(*node_id),
                _ => None,
            })
            .collect();
        assert_eq!(clicked.len(), 1, "{clicked:?}");
        let mods: Vec<String> = batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetModifier { node_id, modifier, .. } if *node_id == clicked[0] => Some(format!("{modifier:?}")),
                _ => None,
            })
            .collect();
        let want = format!(
            "{:?}",
            compose_rust::schema::Modifier::Shape { top_start: 0.0, top_end: 16.0, bottom_end: 16.0, bottom_start: 0.0 }
        );
        assert!(mods.contains(&want), "{mods:?}");
        // The main half is sent with the other end, in the desktop header.
        let src = include_str!("session.rs").split("#[cfg(test)]").next().unwrap();
        assert!(src.contains("Modifier::new().fill_max_height().corner_radii(split_start_radii(32.0))"));
    }

    /// FR-L7, #271: the chevron offered beside the default IDE carries a click handler that
    /// opens the chooser, as the renderer would click it.
    #[test]
    fn fr_l7_ide_chevron_beside_the_default_opens_the_chooser_when_clicked() {
        let _guard = crate::services::test_guard();
        CHEVRON_OPEN.with(|c| *c.borrow_mut() = None);
        let mut host = Host::new(|| Box::new(Recomposer::new(host_ide_chevron)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let clicks: Vec<(u32, u64)> = batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetProp {
                    node_id,
                    property: compose_rust::PropertyKind::OnClick,
                    value: PropertyValue::Integer(h),
                } => Some((*node_id, *h as u64)),
                _ => None,
            })
            .collect();
        assert_eq!(clicks.len(), 1, "the chevron is one clickable: {clicks:?}");
        // compose-rust#472 (v0.0.11): the 26x32 chevron carries MinTouchTarget(44.0) without
        // growing its drawn Width(26.0) / Height(32.0).
        let mods: Vec<String> = batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetModifier { node_id, modifier, .. } if *node_id == clicks[0].0 => Some(format!("{modifier:?}")),
                _ => None,
            })
            .collect();
        for want in ["Width(26.0)", "Height(32.0)", "MinTouchTarget { size: 44.0 }"] {
            assert!(mods.iter().any(|m| m == want), "{want}: {mods:?}");
        }
        let open = CHEVRON_OPEN.with(|c| c.borrow().clone()).expect("state captured");
        assert!(!open.get_untracked());
        host.dispatch(HostEvent::new(clicks[0].0, clicks[0].1, EventPayload::Clicked))
        .expect("dispatch");
        assert!(open.get_untracked(), "the chevron opened the chooser");
        let src = include_str!("session.rs").split("#[cfg(test)]").next().unwrap();
        assert!(!src.contains(concat!("TODO(compose-rust#", "472")));
    }

    /// What a composed screen sends: each node's parent, string props and modifiers.
    struct Tree {
        parents: std::collections::HashMap<u32, u32>,
        strings: Vec<(u32, String)>,
        modifiers: Vec<(u32, String)>,
    }

    impl Tree {
        fn of(content: fn()) -> Self {
            let mut host = Host::new(move || Box::new(Recomposer::new(content)));
            let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
            let mut t = Tree { parents: Default::default(), strings: Vec::new(), modifiers: Vec::new() };
            for m in batch.iter() {
                match m {
                    Mutation::Insert { parent_id, node_id, .. } => {
                        t.parents.insert(*node_id, *parent_id);
                    }
                    Mutation::SetProp { node_id, value: PropertyValue::String(v), .. } => t.strings.push((*node_id, v.to_string())),
                    Mutation::SetModifier { node_id, modifier, .. } => t.modifiers.push((*node_id, format!("{modifier:?}"))),
                    _ => {}
                }
            }
            t
        }

        fn ancestors(&self, mut id: u32) -> Vec<u32> {
            let mut out = vec![id];
            while let Some(p) = self.parents.get(&id) {
                out.push(*p);
                id = *p;
            }
            out
        }

        fn nodes_with(&self, text: &str) -> Vec<u32> {
            self.strings.iter().filter(|(_, s)| s == text).map(|(n, _)| *n).collect()
        }

        /// The nearest node that holds both texts.
        fn common(&self, a: &str, b: &str) -> u32 {
            let a = self.ancestors(*self.nodes_with(a).first().unwrap_or_else(|| panic!("no {a}")));
            let b = self.ancestors(*self.nodes_with(b).first().unwrap_or_else(|| panic!("no {b}")));
            *a.iter().find(|n| b.contains(n)).expect("one tree")
        }

        fn children_mods(&self, parent: u32) -> Vec<String> {
            self.modifiers
                .iter()
                .filter(|(n, _)| self.parents.get(n) == Some(&parent))
                .map(|(_, m)| m.clone())
                .collect()
        }
    }

    #[test]
    fn fr_l28_6_the_phone_head_row_holds_the_title_and_the_whole_subtitle() {
        // Review and2: at 48 dp the subtitle lost its bottom 3 to 4 px.
        let inner = Fs::Md.line_height() + 1.0 + Fs::Xs2.line_height() + 2.0 * 2.0;
        assert_eq!(PHONE_HEAD_ROW_H, 2.0 * SP[1] + inner);
        assert!(PHONE_HEAD_ROW_H >= 48.0 && PHONE_HEAD_ROW_H - 2.0 * SP[1] >= 32.0);
        assert_eq!(PHONE_HEAD_ROW_H, 59.0);
    }

    /// Flip 4 review 3.2 and 3.3: at 360 dp the five header controls shared the title's row, so
    /// the title shrank to "demo-..." and the IDE button ran under the floating menu button. The
    /// mockup's phone header wraps: the first row is the drawer, the title and 새 팀원, kept
    /// 60 dp from the right edge; version control, web pages and the IDE go to the next row.
    #[test]
    fn fr_l28_2_the_phone_session_header_wraps_and_keeps_the_menu_button_room_at_360() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        for (view, host) in [("multi", host_session_multi as fn()), ("single", host_session_single as fn())] {
            shared_ui().update_prefs(|p| p.session_view = Some(view.to_string()));
            crate::ui::window_size::set_opened_for_test(Some((360.0, 880.0)));
            let t = Tree::of(host);
            crate::ui::window_size::set_opened_for_test(None);
            let first_row = t.common(super::super::a11y::MENU, "새 팀원");
            for later in ["버전 관리", "웹 페이지"] {
                for n in t.nodes_with(later) {
                    assert!(!t.ancestors(n).contains(&first_row), "{view}: {later} must leave the title row at 360");
                }
            }
            let row = t.children_mods(first_row);
            assert!(row.iter().any(|m| m == "Width(52.0)"), "{view}: the floating menu room ends the title row: {row:?}");
            assert!(t.modifiers.iter().any(|(n, m)| *n == first_row && m == "Height(59.0)"), "{view}: a 59 dp first row (title and subtitle)");
        }
        shared_ui().update_prefs(|p| p.session_view = None);
        compose_rust::window::reset_window_size();
    }

    #[Composable]
    fn host_session_multi() {
        SessionScreen(None);
    }

    #[Composable]
    fn host_session_single() {
        SessionScreen(Some("demo-s1".to_string()));
    }

    #[test]
    fn fr_l18_session_screen_renders_multi_single_and_status_in_compose_rust() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();

        // 1. Multi view
        shared_ui().update_prefs(|p| p.session_view = Some("multi".to_string()));
        let mut host = Host::new(|| Box::new(Recomposer::new(host_session_multi)));
        let mut all = texts(&decode_batch(host.rebuild().expect("rebuild")).expect("decode"));
        let resize = HostEvent::new(0, 0, EventPayload::WindowSizeChanged {
                width_dp: 1440.0,
                height_dp: 900.0,
                class: WindowSizeClass::Expanded,
                height_class: WindowHeightClass::Medium,
                safe_area_top: 0.0,
                safe_area_leading: 0.0,
                safe_area_trailing: 0.0,
                safe_area_bottom: 0.0,
            });
        let (bytes, _) = host.dispatch(resize.clone()).expect("dispatch");
        all.extend(texts(&decode_batch(bytes).expect("decode")));
        for want in ["멀티", "싱글", "상태", "탭 1", "IDE 열기"] {
            assert!(
                all.iter().any(|t| t.contains(want)),
                "missing {want} in MultiView: {all:?}"
            );
        }

        // 2. Single view
        shared_ui().update_prefs(|p| p.session_view = Some("single".to_string()));
        let mut host_s = Host::new(|| Box::new(Recomposer::new(host_session_single)));
        let mut all_s = texts(&decode_batch(host_s.rebuild().expect("rebuild")).expect("decode"));
        let (bytes_s, _) = host_s.dispatch(resize.clone()).expect("dispatch");
        all_s.extend(texts(&decode_batch(bytes_s).expect("decode")));
        for want in ["환경", "컨텍스트", "결과물", "변경 사항"] {
            assert!(
                all_s.iter().any(|t| t.contains(want)),
                "missing {want} in SingleView: {all_s:?}"
            );
        }

        // 3. Status view
        shared_ui().update_prefs(|p| p.session_view = Some("status".to_string()));
        let mut host_st = Host::new(|| Box::new(Recomposer::new(host_session_multi)));
        let mut all_st =
            texts(&decode_batch(host_st.rebuild().expect("rebuild")).expect("decode"));
        let (bytes_st, _) = host_st.dispatch(resize).expect("dispatch");
        all_st.extend(texts(&decode_batch(bytes_st).expect("decode")));
        for want in ["대기", "실행", "승인 대기", "검토", "완료", "PR과 CI"] {
            assert!(
                all_st.iter().any(|t| t.contains(want)),
                "missing {want} in StatusView: {all_st:?}"
            );
        }

        compose_rust::window::reset_window_size();
    }
}
