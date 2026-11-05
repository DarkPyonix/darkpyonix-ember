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
    MutableState, current_window_size, launched_effect, mutable_state_of, remember,
    remember_with_key,
};
use compose_rust::ui::{self as cu, Modifier};
use compose_rust::{Alignment, AssetKind, Color, IconRole, ShapeRole, SpaceRole};

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
    move_pane, page_step, pane_layout, pane_rows, reorder_pane, resolve_project, side_list_mode,
    side_sections, single_target, toolbar_fold_level, toolbar_width, view_chips_below,
    view_from_saved, visible_panes,
};

use crate::ide::{IdeAction, IdeKind, open_external, plan};
use crate::server::{CurrentComputer, IdeLaunch};
use crate::services::{run_shared, services};
use crate::ui::coexist::{SharedUi, shared_ui};
use crate::ui::project::{Member, display_name, project_boxes, project_boxes_with_archived, status_pill};
use crate::ui::shell::NavMenu;

use super::characters::{CharState, Character};
use super::compat::Spinner;
use super::home::{CardHead, Dot as HomeDot};
use super::components::{
    AppButton, ButtonKind, Clickable, CloseButton, EmberCard, TrackBar, EmberField, EmberSheet,
    FieldOptions, IconButton, Label, OptionSheet, PickChip, Pill, PillTone, Select, SheetFooter,
};
use super::conversation::{ConversationView, ICON_SEND_SVG, TranscriptRow, tail_start_index};
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
pub const ICON_REPEAT_SVG: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M19.5 12c0-1.23-.05-2.45-.14-3.66a4.11 4.11 0 0 0-3.7-3.7 48.66 48.66 0 0 0-7.32 0 4.11 4.11 0 0 0-3.7 3.7c-.09 1.21-.14 2.43-.14 3.66 0 1.23.05 2.45.14 3.66a4.11 4.11 0 0 0 3.7 3.7c2.43.2 4.89.2 7.32 0a4.11 4.11 0 0 0 3.7-3.7c.09-1.21.14-2.43.14-3.66Z"/></svg>"#;
pub const ICON_EDIT_SVG: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="m16.86 4.49 1.69-1.69a1.88 1.88 0 1 1 2.65 2.65L10.58 16.07a4.5 4.5 0 0 1-1.9 1.13L6 18l.8-2.68a4.5 4.5 0 0 1 1.13-1.9l8.93-8.93ZM19.5 7.13 16.86 4.49M18 14v4.75A2.25 2.25 0 0 1 15.75 21H5.25A2.25 2.25 0 0 1 3 18.75V8.25A2.25 2.25 0 0 1 5.25 6H10"/></svg>"#;
pub const ICON_EXPAND_SVG: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M3.75 3.75v4.5m0-4.5h4.5m-4.5 0L9 9M3.75 20.25v-4.5m0 4.5h4.5m-4.5 0L9 15M20.25 3.75h-4.5m4.5 0v4.5m0-4.5L15 9m5.25 11.25h-4.5m4.5 0v-4.5m0 4.5L15 15"/></svg>"#;
pub const ICON_LIST_SVG: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M3.75 6.75h16.5M3.75 12h16.5m-16.5 5.25h16.5"/></svg>"#;
pub const ICON_MORE_H_SVG: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M6.75 12a.75.75 0 1 1-1.5 0 .75.75 0 0 1 1.5 0ZM12.75 12a.75.75 0 1 1-1.5 0 .75.75 0 0 1 1.5 0ZM18.75 12a.75.75 0 1 1-1.5 0 .75.75 0 0 1 1.5 0Z"/></svg>"#;

const NO_PROJECT: &str = "프로젝트가 없습니다";
const NO_MEMBERS: &str = "표시할 팀원이 없습니다";
const PICK_FROM_LIST: &str = "목록에서 팀원을 고르면 이 탭에 추가됩니다";
const NO_CONVERSATION: &str = "아직 대화가 없습니다";
const TERMINAL_SOON: &str = "터미널은 준비 중입니다";
const MODEL_LOCKED: &str = "실행 중인 세션의 모델은 바꿀 수 없습니다";
const WEB_SOON: &str = "웹 페이지 목록은 준비 중입니다";
const NO_SESSION: &str = "세션이 없습니다";
const TIMELINE_TITLE: &str = "에이전트 간 통신";
use crate::ui::session::timeline_summary;

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
            if c.endpoint.as_deref().map_or(true, str::is_empty) {
                notice::post("Ember 에디터: 서버가 OSE 런타임을 시작하지 못했습니다");
                return;
            }
            ui.open_editor_connection(c.project, c.folder, c.endpoint, c.token, c.server_url, c.computer);
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
            cu::Box().modifier(
                Modifier::new()
                    .size(10.0, 10.0)
                    .background(paint(Color::rgb(0xffffff)))
                    .corner_radius(2.0),
            );
        },
    );
}

/// Circular send button (`.sendround` in mockup: `size` circle, `--ink`, send glyph).
pub fn SendCircleButton(size: f32, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    Clickable(
        Modifier::new()
            .size(size, size)
            .background(paint(p.ink))
            .shape_role(ShapeRole::Full),
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

/// The model chip of a composer (`.pickchip.modelchip`).
pub fn ModelChip(session: &str, compact_label: bool) {
    let ui = shared_ui().clone();
    let p = ember_theme().palette;
    let sid = session.to_string();
    let model_menu = remember_with_key(format!("{sid}:model"), || mutable_state_of(false));

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

    cu::Box().content(|| {
        let mm = model_menu.clone();
        Clickable(
            Modifier::new()
                .height(30.0)
                .background(paint(p.surface_2))
                .shape_role(ShapeRole::Full),
            Alignment::Center,
            move || mm.set(true),
            move || {
                cu::Row()
                    .alignment(Alignment::CenterStart)
                    .spacing(6.0)
                    .content(|| {
                        cu::Spacer().modifier(Modifier::new().width(4.0));
                        cu::Box().modifier(
                            Modifier::new()
                                .size(8.0, 8.0)
                                .background(paint(dot_col))
                                .corner_radius(4.0),
                        );
                        Label(
                            &shown_text,
                            Fs::Xs,
                            Weight::Medium,
                            Some(paint(p.text_2)),
                            Some(1),
                        );
                        NavIcon(ICON_CHEVRON_DOWN_SVG, p.muted.to_argb(), 12.0);
                        cu::Spacer().modifier(Modifier::new().width(4.0));
                    });
            },
        );

        let model_opts = vec![format!("{MODEL_LOCKED} ({SOON})")];
        let mm_d = model_menu.clone();
        let mm_c = model_menu.clone();
        OptionSheet(
            model_menu.get(),
            &model_opts,
            None,
            POPUP_W,
            move || mm_d.set(false),
            move |_| mm_c.set(false),
        );
    });
}

/// The permission mode chip of a composer (`.pickchip.modechip`).
pub fn ModeChip(session: &str, _compact_label: bool) {
    let ui = shared_ui().clone();
    let p = ember_theme().palette;
    let sid = session.to_string();
    let mode_menu = remember_with_key(format!("{sid}:mode"), || mutable_state_of(false));

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

    cu::Box().content(|| {
        let md = mode_menu.clone();
        Clickable(
            Modifier::new()
                .height(30.0)
                .background(paint(bg))
                .shape_role(ShapeRole::Full),
            Alignment::Center,
            move || md.set(true),
            move || {
                cu::Row()
                    .alignment(Alignment::CenterStart)
                    .spacing(5.0)
                    .content(|| {
                        cu::Spacer().modifier(Modifier::new().width(4.0));
                        NavIcon(ICON_SHIELD_SVG, fg.to_argb(), 13.0);
                        Label(
                            mode_text,
                            Fs::Xs,
                            Weight::Medium,
                            Some(paint(fg)),
                            Some(1),
                        );
                        NavIcon(ICON_CHEVRON_DOWN_SVG, p.muted.to_argb(), 12.0);
                        cu::Spacer().modifier(Modifier::new().width(4.0));
                    });
            },
        );

        let mode_labels: Vec<String> = rows
            .iter()
            .map(|(m, settable)| {
                if *settable {
                    m.label().to_string()
                } else {
                    format!("{} ({SOON})", m.label())
                }
            })
            .collect();
        let chosen = rows
            .iter()
            .position(|(m, _)| *m == current_mode(approve_all));
        let md_d = mode_menu.clone();
        let sid_m = sid.clone();
        let rows_c = rows.clone();
        OptionSheet(
            mode_menu.get(),
            &mode_labels,
            chosen,
            POPUP_W,
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
        );
    });
}

/// The model and mode chips of a composer (FR-L15, FR-L16).
pub fn ComposerChips(session: &str) {
    let sid = session.to_string();
    cu::Row()
        .alignment(Alignment::CenterStart)
        .spacing(6.0)
        .content(move || {
            ModelChip(&sid, false);
            ModeChip(&sid, false);
        });
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

/// The chevron beside a header's "IDE 열기" that opens the chooser even when a default is set
/// (FR-L7). `on_ink` draws it inside the dark split button; otherwise as a bordered square.
/// TODO(compose-rust#460): the accessible name "다른 IDE 선택" cannot be set; `Clickable` gives a
/// button role (compose-rust#372) but no label. Touch target 26x32, under 44 px: #323.
pub fn IdeChevron(open: MutableState<bool>, on_ink: bool) {
    let p = ember_theme().palette;
    let (tint, m) = if on_ink {
        (p.ink_text.to_argb(), Modifier::new().size(26.0, 32.0))
    } else {
        (
            p.text_2.to_argb(),
            Modifier::new()
                .size(26.0, 32.0)
                .background(paint(p.surface))
                .border(1.0, paint(p.line))
                .shape_role(ShapeRole::Full),
        )
    };
    Clickable(
        m,
        Alignment::Center,
        move || open.set(true),
        move || NavIcon(ICON_CHEVRON_DOWN_SVG, tint, 13.0),
    );
}

/// "IDE 열기" split button with the three-entry chooser (`.idesplit`, FR-L7).
pub fn IdeMenu(session: &str) {
    let ui = shared_ui().clone();
    let window = current_window_size();
    let p = ember_theme().palette;
    let sid = session.to_string();
    let ide = remember_with_key(sid.clone(), || {
        mutable_state_of(None::<Result<IdeLaunch, String>>)
    });
    let open = remember_with_key(sid.clone(), || mutable_state_of(false));
    let is_phone = window.width_dp <= 560.0;
    let show_cur = window.width_dp >= 1280.0;

    {
        let sid_load = sid.clone();
        let ide_load = ide.clone();
        launched_effect(sid.clone(), move || async move {
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

    cu::Box().content(|| {
        let o = open.clone();
        let o_main = open.clone();
        let ui_main = ui.clone();
        let launch_main = ide.get();
        if is_phone {
            let o_chev = open.clone();
            cu::Row()
                .alignment(Alignment::Center)
                .spacing(4.0)
                .content(move || {
                    IdeChevron(o_chev, false);
                    Clickable(
                        Modifier::new()
                            .size(32.0, 32.0)
                            .background(paint(p.ink))
                            .shape_role(ShapeRole::Full),
                        Alignment::Center,
                        move || {
                            ide_main_click(remembered, &o, |kind| {
                                launch_ide_shared(&ui_main, kind, launch_main.clone())
                            })
                        },
                        || NavIcon(ICON_CODE_SVG, p.ink_text.to_argb(), 15.0),
                    );
                });
        } else {
            cu::Row()
                .alignment(Alignment::Center)
                .modifier(
                    Modifier::new()
                        .height(32.0)
                        .background(paint(p.ink))
                        .shape_role(ShapeRole::Full),
                )
                .content(move || {
                    Clickable(
                        Modifier::new().fill_max_height(),
                        Alignment::Center,
                        move || {
                            ide_main_click(remembered, &o_main, |kind| {
                                launch_ide_shared(&ui_main, kind, launch_main.clone())
                            })
                        },
                        move || {
                            cu::Row()
                                .alignment(Alignment::CenterStart)
                                .spacing(6.0)
                                .content(|| {
                                    cu::Spacer().modifier(Modifier::new().width(6.0));
                                    NavIcon(ICON_CODE_SVG, p.ink_text.to_argb(), 15.0);
                                    Label(
                                        "IDE 열기",
                                        Fs::Sm,
                                        Weight::Medium,
                                        Some(paint(p.ink_text)),
                                        Some(1),
                                    );
                                    if show_cur {
                                        if let Some(cur_label) = cur_label {
                                            Label(
                                                cur_label,
                                                Fs::Xs,
                                                Weight::Regular,
                                                Some(paint(p.ink_text)),
                                                Some(1),
                                            );
                                        }
                                    }
                                    cu::Spacer().modifier(Modifier::new().width(4.0));
                                });
                        },
                    );
                    cu::Box().modifier(
                        Modifier::new()
                            .size(1.0, 18.0)
                            .background(paint(p.muted)),
                    );
                    IdeChevron(o, true);
                });
        }

        let labels: Vec<String> = choices.iter().map(|c| c.label.to_string()).collect();
        let o_d = open.clone();
        let choices_c = choices.clone();
        let ui_c = ui.clone();
        let launch_now = ide.get();
        OptionSheet(
            open.get(),
            &labels,
            remembered.map(|_| 0),
            POPUP_W,
            move || o_d.set(false),
            move |idx| {
                if let Some(c) = choices_c.get(idx) {
                    launch_ide_shared(&ui_c, c.kind, launch_now.clone());
                }
            },
        );
    });
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
    cu::Box().modifier(
        Modifier::new()
            .size(8.0, 8.0)
            .background(paint(col))
            .corner_radius(4.0),
    );
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

    cu::Column()
        .modifier(
            Modifier::new()
                .fill_max_width()
                .fill_max_height()
                .background(paint(p.surface_2)),
        )
        .content(move || {
            // .st-top header (height 60dp, bottom 1px line)
            cu::Row()
                .alignment(Alignment::CenterStart)
                .space_role(SpaceRole::Sm)
                .modifier(
                    Modifier::new()
                        .fill_max_width()
                        .height(SHEET_HEADER_H)
                        .padding(SP[2]),
                )
                .content(move || {
                    Label("세션", Fs::Lg, Weight::Semibold, Some(paint(p.text)), Some(1));
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
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
                });
            cu::Separator(Some(paint(p.line)));

            cu::Column()
                .space_role(SpaceRole::Sm)
                .modifier(
                    Modifier::new()
                        .fill_max_width()
                        .weight(1.0)
                        .padding(SP[2]),
                )
                .content(move || {
                    EmberField(FieldOptions {
                        fill_max_width: true,
                        placeholder: "세션 검색".to_string(),
                        on_value_change: Some(Box::new(on_query)),
                        ..Default::default()
                    });

                    if is_multi {
                        Label(
                            "팀원을 끌어 칸에 놓을 수 있습니다",
                            Fs::Xs,
                            Weight::Regular,
                            Some(paint(p.muted)),
                            Some(1),
                        );
                    }

                    cu::ScrollColumn()
                        .modifier(Modifier::new().fill_max_width().weight(1.0))
                        .content(move || {
                            cu::Column()
                                .space_role(SpaceRole::Sm)
                                .modifier(Modifier::new().fill_max_width())
                                .content(move || {
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

                                        cu::Column()
                                            .space_role(SpaceRole::Xs)
                                            .modifier(Modifier::new().fill_max_width())
                                            .content(move || {
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
                                                        cu::Row()
                                                            .alignment(Alignment::CenterStart)
                                                            .space_role(SpaceRole::Xs)
                                                            .modifier(Modifier::new().fill_max_width())
                                                            .content(|| {
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
                                                                cu::Spacer()
                                                                    .modifier(Modifier::new().weight(1.0));
                                                                if wait_in_sec > 0 {
                                                                    RedBadge(wait_in_sec);
                                                                }
                                                            });
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
                                                                cu::Row()
                                                                    .alignment(Alignment::CenterStart)
                                                                    .space_role(SpaceRole::Sm)
                                                                    .modifier(Modifier::new().fill_max_width())
                                                                    .content(move || {
                                                                        Character(
                                                                            &m_agent,
                                                                            m_leader,
                                                                            26.0,
                                                                            &m_name,
                                                                            &s_proj,
                                                                            CharState::from_status(m_status),
                                                                            None,
                                                                        );
                                                                        cu::Column()
                                                                            .modifier(Modifier::new().weight(1.0))
                                                                            .content(move || {
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
                                                                            });
                                                                        if m_status == LauncherStatus::Running {
                                                                            Spinner(12.0);
                                                                        } else {
                                                                            StatusDot(m_status);
                                                                        }
                                                                    });
                                                            },
                                                        );
                                                    }
                                                }
                                            });
                                    }
                                });
                        });
                });
        });
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

    cu::Column()
        .alignment(Alignment::TopCenter)
        .modifier(
            Modifier::new()
                .width(64.0)
                .fill_max_height()
                .background(paint(p.surface_2)),
        )
        .content(move || {
            cu::Box()
                .alignment(Alignment::Center)
                .modifier(Modifier::new().fill_max_width().height(SHEET_HEADER_H))
                .content(move || {
                    Clickable(
                        Modifier::new().size(36.0, 36.0).corner_radius(CORNER_SM),
                        Alignment::Center,
                        on_expand,
                        || NavIcon(ICON_SIDEBAR_OPEN_SVG, p.muted.to_argb(), 16.0),
                    );
                });
            cu::Separator(Some(paint(p.line)));

            cu::ScrollColumn()
                .modifier(Modifier::new().fill_max_width().weight(1.0))
                .content(move || {
                    cu::Column()
                        .alignment(Alignment::TopCenter)
                        .spacing(SP[1])
                        .modifier(Modifier::new().fill_max_width().padding(SP[1]))
                        .content(move || {
                            for (sec_idx, s) in sections_vec.iter().enumerate() {
                                if sec_idx > 0 {
                                    cu::Box().modifier(
                                        Modifier::new()
                                            .size(28.0, 1.0)
                                            .background(paint(p.line)),
                                    );
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
                                            cu::Box()
                                                .alignment(Alignment::BottomEnd)
                                                .modifier(Modifier::new().size(30.0, 30.0))
                                                .content(move || {
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
                                                });
                                        },
                                    );
                                }
                            }
                        });
                });
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

    let items = {
        let _ = ui.live.transcripts.get();
        services()
            .client
            .read(|st| st.transcript(&sid).map(|t| t.items.clone()).unwrap_or_default())
    };
    let count = items.len();

    cu::Box()
        .modifier(Modifier::new().fill_max_width().weight(1.0))
        .content(move || {
            if count == 0 {
                cu::Box()
                    .alignment(Alignment::Center)
                    .modifier(Modifier::new().fill_max_width().fill_max_height())
                    .content(|| {
                        Label(
                            NO_CONVERSATION,
                            Fs::Sm,
                            Weight::Regular,
                            Some(paint(p.muted)),
                            None,
                        );
                    });
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
                cu::ScrollColumn()
                    .modifier(Modifier::new().fill_max_width().fill_max_height())
                    .content(move || {
                        cu::Column()
                            .spacing(SP[1])
                            .modifier(Modifier::new().fill_max_width().padding(SP[1]))
                            .content(move || {
                                for item in items_c.iter() {
                                    TranscriptRow(&sid_c, &agent_c, item, &exp_c, &ans_c);
                                }
                                cu::Spacer().modifier(Modifier::new().height(12.0));
                            });
                    });
            }
        });
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
    cu::Column()
        .spacing(SP[0])
        .modifier(Modifier::new().fill_max_width())
        .content(move || {
            cu::Separator(Some(paint(p.line)));
            if queued > 0 {
                cu::Row()
                    .modifier(Modifier::new().fill_max_width().padding(SP[0]))
                    .content(|| {
                        Pill(&format!("{queued}개 대기 중입니다"), PillTone::Blue, true);
                    });
            }
            // 1-row .pane-c footer: [ModelChip] [ModeChip] [Input] [Stop/Send circle]
            cu::Row()
                .alignment(Alignment::CenterStart)
                .spacing(SP[1])
                .modifier(Modifier::new().fill_max_width().padding(SP[0]))
                .content(|| {
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
                });
        });
}

/// Compact segmented switcher inside a pane header (`.mini-seg`).
fn MiniSeg(cur: PaneToggle, on_pick: impl Fn(PaneToggle) + Clone + 'static) {
    let p = ember_theme().palette;
    cu::Row()
        .alignment(Alignment::Center)
        .modifier(
            Modifier::new()
                .height(26.0)
                .background(paint(p.surface))
                .border(1.0, paint(p.line))
                .corner_radius(13.0)
                .padding(2.0),
        )
        .content(move || {
            for t in PaneToggle::ALL {
                let selected = cur == t;
                let (bg, fg) = if selected {
                    (paint(p.ink), p.ink_text)
                } else {
                    (paint(Color::argb(0)), p.muted)
                };
                let cb = on_pick.clone();
                Clickable(
                    Modifier::new()
                        .height(22.0)
                        .background(bg)
                        .corner_radius(11.0),
                    Alignment::Center,
                    move || cb(t),
                    move || {
                        cu::Row().alignment(Alignment::Center).content(|| {
                            cu::Spacer().modifier(Modifier::new().width(8.0));
                            Label(
                                t.label(),
                                Fs::Xs,
                                if selected { Weight::Semibold } else { Weight::Regular },
                                Some(paint(fg)),
                                Some(1),
                            );
                            cu::Spacer().modifier(Modifier::new().width(8.0));
                        });
                    },
                );
            }
        });
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

    cu::Box()
        .modifier(Modifier::new().fill_max_width().fill_max_height())
        .content(move || {
            let cur_toggle = toggle.get();
            cu::Column()
                .space_role(SpaceRole::Xs)
                .modifier(
                    Modifier::new()
                        .fill_max_width()
                        .fill_max_height()
                        .background(paint(p.surface))
                        .border(1.0, paint(p.line))
                        .corner_radius(CORNER_LG)
                        .padding(SP[1]),
                )
                .content(|| {
                    // Header row (.pane-h)
                    cu::Row()
                        .alignment(Alignment::CenterStart)
                        .space_role(SpaceRole::Xs)
                        .modifier(Modifier::new().fill_max_width())
                        .content(|| {
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
                            cu::Column()
                                .modifier(Modifier::new().weight(1.0))
                                .content(|| {
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
                                });
                            if narrow_pane {
                                StatusDot(member_owned.status);
                            } else {
                                Pill(label, tone.into(), true);
                            }
                            let tg_seg = toggle.clone();
                            MiniSeg(cur_toggle, move |t| tg_seg.set(t));

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
                        });
                    cu::Separator(Some(paint(p.line)));

                    if cur_toggle == PaneToggle::Conversation {
                        PaneTranscript(&id, &member_owned.agent);
                        PaneComposer(&id, busy);
                    } else {
                        cu::Box()
                            .alignment(Alignment::Center)
                            .modifier(Modifier::new().fill_max_width().weight(1.0))
                            .content(|| {
                                Label(
                                    TERMINAL_SOON,
                                    Fs::Sm,
                                    Weight::Regular,
                                    Some(paint(p.muted)),
                                    None,
                                );
                            });
                    }
                });

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
        });
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
    let layout = if window.width_dp >= 700.0 {
        PaneLayout {
            per_row: 2,
            paged: false,
        }
    } else {
        pane_layout(window.class)
    };
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
    let cur = page.get().min(n.saturating_sub(1));
    let show_help = window.width_dp >= 1280.0;

    cu::Box()
        .modifier(Modifier::new().fill_max_width().fill_max_height())
        .content(move || {
            cu::Column()
                .space_role(SpaceRole::Sm)
                .modifier(
                    Modifier::new()
                        .fill_max_width()
                        .fill_max_height()
                        .padding_role(SpaceRole::Sm),
                )
                .content(|| {
                    // Tab strip (.stabs)
                    cu::Row()
                        .alignment(Alignment::CenterStart)
                        .space_role(SpaceRole::Xs)
                        .modifier(Modifier::new().fill_max_width())
                        .content(|| {
                            cu::ScrollRow()
                                .modifier(Modifier::new().weight(1.0))
                                .content(|| {
                                    cu::Row()
                                        .alignment(Alignment::CenterStart)
                                        .spacing(6.0)
                                        .content(|| {
                                            let multi_tabs = panes.tabs.len() > 1;
                                            for (i, t) in panes.tabs.iter().enumerate() {
                                                let pg = page.clone();
                                                let u = ui.clone();
                                                let pr = proj_owned.clone();
                                                let tab_vis = visible_panes(t, &all);
                                                let tab_name = t.name.clone();
                                                let tab_cnt = tab_vis.len();
                                                let selected = i == panes.active;
                                                let (bg, border_w, fg, wt) = if selected {
                                                    (
                                                        paint(p.surface),
                                                        1.0,
                                                        p.text,
                                                        Weight::Semibold,
                                                    )
                                                } else {
                                                    (
                                                        paint(Color::argb(0)),
                                                        0.0,
                                                        p.text_2,
                                                        Weight::Medium,
                                                    )
                                                };
                                                let mut tab_mod = Modifier::new()
                                                    .height(34.0)
                                                    .background(bg)
                                                    .corner_radius(CORNER_MD);
                                                if border_w > 0.0 {
                                                    tab_mod = tab_mod.border(border_w, paint(p.line));
                                                }
                                                cu::Row()
                                                    .alignment(Alignment::Center)
                                                    .modifier(tab_mod)
                                                    .content(move || {
                                                        let pg_s = pg.clone();
                                                        let u_s = u.clone();
                                                        let pr_s = pr.clone();
                                                        Clickable(
                                                            Modifier::new().fill_max_height(),
                                                            Alignment::Center,
                                                            move || {
                                                                pg_s.set(0);
                                                                edit_panes(&u_s, &pr_s, move |pp| {
                                                                    pp.select(i)
                                                                });
                                                            },
                                                            move || {
                                                                cu::Row()
                                                                    .alignment(Alignment::CenterStart)
                                                                    .spacing(6.0)
                                                                    .content(|| {
                                                                        cu::Spacer().modifier(
                                                                            Modifier::new().width(10.0),
                                                                        );
                                                                        Label(
                                                                            &tab_name,
                                                                            Fs::Sm,
                                                                            wt,
                                                                            Some(paint(fg)),
                                                                            Some(1),
                                                                        );
                                                                        cu::Box()
                                                                            .alignment(Alignment::Center)
                                                                            .modifier(
                                                                                Modifier::new()
                                                                                    .size(20.0, 18.0)
                                                                                    .background(paint(
                                                                                        p.surface_2,
                                                                                    ))
                                                                                    .corner_radius(9.0),
                                                                            )
                                                                            .content(|| {
                                                                                Label(
                                                                                    &tab_cnt.to_string(),
                                                                                    Fs::Xs2,
                                                                                    Weight::Medium,
                                                                                    Some(paint(p.muted)),
                                                                                    Some(1),
                                                                                );
                                                                            });
                                                                        cu::Spacer().modifier(
                                                                            Modifier::new().width(
                                                                                if multi_tabs {
                                                                                    2.0
                                                                                } else {
                                                                                    10.0
                                                                                },
                                                                            ),
                                                                        );
                                                                    });
                                                            },
                                                        );
                                                        if multi_tabs {
                                                            let pg_x = pg.clone();
                                                            let u_x = u.clone();
                                                            let pr_x = pr.clone();
                                                            Clickable(
                                                                Modifier::new()
                                                                    .size(24.0, 24.0)
                                                                    .shape_role(ShapeRole::Full),
                                                                Alignment::Center,
                                                                move || {
                                                                    pg_x.set(0);
                                                                    edit_panes(&u_x, &pr_x, move |pp| {
                                                                        pp.remove_tab(i)
                                                                    });
                                                                },
                                                                || {
                                                                    NavIcon(
                                                                        ICON_CLOSE_SVG,
                                                                        p.muted.to_argb(),
                                                                        11.0,
                                                                    )
                                                                },
                                                            );
                                                            cu::Spacer().modifier(Modifier::new().width(4.0));
                                                        }
                                                    });
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
                                        });
                                });
                            if show_help {
                                Label(
                                    "칸 머리를 끌어 옮길 수 있습니다. 가운데에 놓으면 바꾸고, 가장자리에 놓으면 나눕니다",
                                    Fs::Xs,
                                    Weight::Regular,
                                    Some(paint(p.muted)),
                                    Some(1),
                                );
                            }
                        });

                    if n == 0 {
                        cu::Box()
                            .alignment(Alignment::Center)
                            .modifier(Modifier::new().fill_max_width().weight(1.0))
                            .content(|| {
                                cu::Column()
                                    .alignment(Alignment::Center)
                                    .space_role(SpaceRole::Sm)
                                    .content(|| {
                                        Label(NO_MEMBERS, Fs::Lg, Weight::Medium, None, Some(1));
                                        Label(
                                            PICK_FROM_LIST,
                                            Fs::Md,
                                            Weight::Regular,
                                            Some(paint(p.muted)),
                                            Some(1),
                                        );
                                    });
                            });
                    } else if layout.paged {
                        cu::Row()
                            .alignment(Alignment::CenterStart)
                            .space_role(SpaceRole::Sm)
                            .modifier(Modifier::new().fill_max_width())
                            .content(|| {
                                let pg1 = page.clone();
                                AppButton(
                                    "이전",
                                    Some(IconRole::Back),
                                    ButtonKind::Default,
                                    true,
                                    cur > 0,
                                    false,
                                    move || pg1.set(page_step(cur, n, false)),
                                );
                                Label(
                                    &format!("{} / {}", cur + 1, n),
                                    Fs::Sm,
                                    Weight::Regular,
                                    Some(paint(p.text_2)),
                                    Some(1),
                                );
                                let pg2 = page.clone();
                                AppButton(
                                    "다음",
                                    Some(IconRole::Forward),
                                    ButtonKind::Default,
                                    true,
                                    cur + 1 < n,
                                    false,
                                    move || pg2.set(page_step(cur, n, true)),
                                );
                            });
                        cu::Box()
                            .modifier(Modifier::new().fill_max_width().weight(1.0))
                            .content(|| {
                                if let Some(m) =
                                    members_vec.iter().find(|m| m.session_id == vis[cur])
                                {
                                    Pane(&proj_owned, m, &all, &vis, on_enlarge.clone());
                                }
                            });
                    } else {
                        let rows_idx = pane_rows(n, layout.per_row);
                        let proj_grid = proj_owned.clone();
                        let members_grid = members_vec.clone();
                        let all_grid = all.clone();
                        let vis_grid = vis.clone();
                        let enl_grid = on_enlarge.clone();
                        if rows_idx.len() <= 2 {
                            // Full-height flex grid filling the remaining vertical space (.sgrid-multi)
                            cu::Column()
                                .spacing(SP[2])
                                .modifier(Modifier::new().fill_max_width().weight(1.0))
                                .content(move || {
                                    for row in rows_idx.iter() {
                                        let row_c = row.clone();
                                        let pr = proj_grid.clone();
                                        let ms = members_grid.clone();
                                        let al = all_grid.clone();
                                        let vs = vis_grid.clone();
                                        let enl = enl_grid.clone();
                                        cu::Row()
                                            .spacing(SP[2])
                                            .modifier(Modifier::new().fill_max_width().weight(1.0))
                                            .content(move || {
                                                for idx in row_c.iter().copied() {
                                                    let pr_p = pr.clone();
                                                    let al_p = al.clone();
                                                    let vs_p = vs.clone();
                                                    let enl_p = enl.clone();
                                                    let m_opt = ms
                                                        .iter()
                                                        .find(|m| m.session_id == vs[idx])
                                                        .cloned();
                                                    cu::Box()
                                                        .modifier(
                                                            Modifier::new()
                                                                .weight(1.0)
                                                                .fill_max_height(),
                                                        )
                                                        .content(move || {
                                                            if let Some(m) = m_opt {
                                                                Pane(
                                                                    &pr_p, &m, &al_p, &vs_p, enl_p,
                                                                );
                                                            }
                                                        });
                                                }
                                                for _ in 0..(layout.per_row - row_c.len()) {
                                                    cu::Spacer()
                                                        .modifier(Modifier::new().weight(1.0));
                                                }
                                            });
                                    }
                                });
                        } else {
                            cu::ScrollColumn()
                                .modifier(Modifier::new().fill_max_width().weight(1.0))
                                .content(move || {
                                    cu::Column()
                                        .space_role(SpaceRole::Sm)
                                        .modifier(Modifier::new().fill_max_width())
                                        .content(move || {
                                            for row in rows_idx.iter() {
                                                let row_c = row.clone();
                                                let pr = proj_grid.clone();
                                                let ms = members_grid.clone();
                                                let al = all_grid.clone();
                                                let vs = vis_grid.clone();
                                                let enl = enl_grid.clone();
                                                cu::Row()
                                                    .space_role(SpaceRole::Sm)
                                                    .modifier(Modifier::new().fill_max_width())
                                                    .content(move || {
                                                        for idx in row_c.iter().copied() {
                                                            let pr_p = pr.clone();
                                                            let al_p = al.clone();
                                                            let vs_p = vs.clone();
                                                            let enl_p = enl.clone();
                                                            let m_opt = ms
                                                                .iter()
                                                                .find(|m| m.session_id == vs[idx])
                                                                .cloned();
                                                            cu::Box()
                                                                .modifier(
                                                                    Modifier::new()
                                                                        .weight(1.0)
                                                                        .height(PANE_H),
                                                                )
                                                                .content(move || {
                                                                    if let Some(m) = m_opt {
                                                                        Pane(
                                                                            &pr_p, &m, &al_p,
                                                                            &vs_p, enl_p,
                                                                        );
                                                                    }
                                                                });
                                                        }
                                                        for _ in 0..(layout.per_row - row_c.len()) {
                                                            cu::Spacer()
                                                                .modifier(Modifier::new().weight(1.0));
                                                        }
                                                    });
                                            }
                                        });
                                });
                        }
                    }
                });

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
        });
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

    cu::ScrollColumn()
        .modifier(
            Modifier::new()
                .fill_max_width()
                .fill_max_height()
                .background(paint(p.bg))
                .padding_role(SpaceRole::Sm),
        )
        .content(move || {
            cu::Column()
                .space_role(SpaceRole::Sm)
                .modifier(Modifier::new().fill_max_width())
                .content(move || {
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
                            cu::Column()
                                .spacing(6.0)
                                .modifier(Modifier::new().fill_max_width())
                                .content(move || {
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
                                            cu::Row()
                                                .alignment(Alignment::CenterStart)
                                                .spacing(6.0)
                                                .modifier(Modifier::new().fill_max_width())
                                                .content(|| {
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
                                                    cu::Spacer()
                                                        .modifier(Modifier::new().weight(1.0));
                                                    if !right_summary.is_empty() {
                                                        Label(
                                                            right_summary,
                                                            Fs::Xs,
                                                            Weight::Medium,
                                                            Some(paint(p.muted)),
                                                            Some(1),
                                                        );
                                                    }
                                                });
                                        },
                                    );
                                    if !is_off {
                                        cu::Spacer().modifier(Modifier::new().height(2.0));
                                        match card {
                                            InfoCard::Environment if is_demo => {
                                                cu::Row()
                                                    .alignment(Alignment::CenterStart)
                                                    .spacing(SP[1])
                                                    .modifier(Modifier::new().fill_max_width().padding(4.0))
                                                    .content(move || {
                                                        NavIcon(ICON_PC_SVG, p.muted.to_argb(), 16.0);
                                                        Label(&comp_c, Fs::Sm, Weight::Regular, Some(paint(p.text)), Some(1));
                                                        Label("· macOS 26", Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
                                                        cu::Spacer().modifier(Modifier::new().weight(1.0));
                                                        Label("192.168.0.10", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                                                    });
                                                cu::Row()
                                                    .alignment(Alignment::CenterStart)
                                                    .spacing(SP[1])
                                                    .modifier(Modifier::new().fill_max_width().padding(4.0))
                                                    .content(|| {
                                                        NavIcon(ICON_FOLDER_SVG, p.muted.to_argb(), 16.0);
                                                        Label("DarkPyonix/darkpyonix-ember", Fs::Sm, Weight::Regular, Some(paint(p.text)), Some(1));
                                                    });
                                                cu::Row()
                                                    .alignment(Alignment::CenterStart)
                                                    .spacing(SP[1])
                                                    .modifier(Modifier::new().fill_max_width().padding(4.0))
                                                    .content(|| {
                                                        NavIcon(ICON_GIT_SVG, p.muted.to_argb(), 16.0);
                                                        Label("feat/shim-wiring", Fs::Sm, Weight::Regular, Some(paint(p.text)), Some(1));
                                                        cu::Spacer().modifier(Modifier::new().weight(1.0));
                                                        Pill("worktree", PillTone::Grey, true);
                                                    });
                                                cu::Row()
                                                    .alignment(Alignment::CenterStart)
                                                    .spacing(SP[1])
                                                    .modifier(Modifier::new().fill_max_width().padding(4.0))
                                                    .content(move || {
                                                        Character(&ag_c, false, 18.0, "에이전트", "demo", CharState::Run, None);
                                                        Label(&md_c, Fs::Sm, Weight::Regular, Some(paint(p.text)), Some(1));
                                                    });
                                                cu::Row()
                                                    .alignment(Alignment::CenterStart)
                                                    .spacing(SP[1])
                                                    .modifier(Modifier::new().fill_max_width().padding(4.0))
                                                    .content(move || {
                                                        NavIcon(ICON_SHIELD_SVG, p.muted.to_argb(), 16.0);
                                                        Label(&format!("{mode_str} · Claude 개인"), Fs::Sm, Weight::Regular, Some(paint(p.text)), Some(1));
                                                    });
                                            }
                                            InfoCard::Environment => {
                                                for (k, v) in rows_c.iter() {
                                                    let ico = env_row_icon(k);
                                                    cu::Row()
                                                        .alignment(Alignment::CenterStart)
                                                        .spacing(SP[1])
                                                        .modifier(Modifier::new().fill_max_width().padding(4.0))
                                                        .content(move || {
                                                            NavIcon(ico, p.muted.to_argb(), 16.0);
                                                            Label(
                                                                k,
                                                                Fs::Sm,
                                                                Weight::Regular,
                                                                Some(paint(p.muted)),
                                                                Some(1),
                                                            );
                                                            cu::Column()
                                                                .alignment(Alignment::CenterEnd)
                                                                .modifier(Modifier::new().weight(1.0))
                                                                .content(|| {
                                                                    Label(
                                                                        v,
                                                                        Fs::Sm,
                                                                        Weight::Regular,
                                                                        None,
                                                                        Some(1),
                                                                    );
                                                                });
                                                        });
                                                }
                                            }
                                            InfoCard::Context if is_demo => {
                                                cu::Row()
                                                    .alignment(Alignment::CenterStart)
                                                    .modifier(Modifier::new().fill_max_width())
                                                    .content(|| {
                                                        Label("31.2% 사용됨", Fs::Sm, Weight::Semibold, Some(paint(p.text)), Some(1));
                                                        cu::Spacer().modifier(Modifier::new().weight(1.0));
                                                        Label("312.0k / 1M", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                                                    });
                                                cu::Row()
                                                    .spacing(1.0)
                                                    .modifier(
                                                        Modifier::new()
                                                            .fill_max_width()
                                                            .height(6.0)
                                                            .background(paint(p.surface_2))
                                                            .corner_radius(3.0),
                                                    )
                                                    .content(|| {
                                                        cu::Box().modifier(Modifier::new().weight(24.4).fill_max_height().background(paint(p.blue.strong)));
                                                        cu::Box().modifier(Modifier::new().weight(2.8).fill_max_height().background(paint(p.sky.strong)));
                                                        cu::Box().modifier(Modifier::new().weight(4.0).fill_max_height().background(paint(p.orange.strong)));
                                                        cu::Box().modifier(Modifier::new().weight(68.8).fill_max_height());
                                                    });
                                                for (col, lbl, tok, pct) in [
                                                    (p.blue.strong, "메시지", "244.0k", "24.4%"),
                                                    (p.sky.strong, "시스템 도구", "18.0k", "1.8%"),
                                                    (p.green.strong, "스킬", "6.0k", "0.6%"),
                                                    (p.orange.strong, "자동 압축 버퍼", "33.0k", "3.3%"),
                                                    (p.line, "남은 공간", "688.0k", "68.8%"),
                                                ] {
                                                    cu::Row()
                                                        .alignment(Alignment::CenterStart)
                                                        .spacing(6.0)
                                                        .modifier(Modifier::new().fill_max_width().padding(2.0))
                                                        .content(move || {
                                                            cu::Box().modifier(Modifier::new().size(10.0, 10.0).background(paint(col)).corner_radius(3.0));
                                                            Label(lbl, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(1));
                                                            cu::Spacer().modifier(Modifier::new().weight(1.0));
                                                            Label(tok, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                                                            Label(pct, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
                                                        });
                                                }
                                            }
                                            InfoCard::Outputs if is_demo => {
                                                for (ico, lbl, kind) in [
                                                    (ICON_GIT_SVG, "#139 Wire shim into proxy", "PR"),
                                                    (ICON_GLOBE_SVG, "http://192.168.0.10:3000", "웹 페이지"),
                                                ] {
                                                    cu::Row()
                                                        .alignment(Alignment::CenterStart)
                                                        .spacing(SP[1])
                                                        .modifier(Modifier::new().fill_max_width().padding(4.0))
                                                        .content(move || {
                                                            NavIcon(ico, p.muted.to_argb(), 15.0);
                                                            cu::Box().modifier(Modifier::new().weight(1.0)).content(move || {
                                                                Label(lbl, Fs::Sm, Weight::Regular, Some(paint(p.text)), Some(1));
                                                            });
                                                            Label(kind, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                                                        });
                                                }
                                            }
                                            InfoCard::Changes if is_demo => {
                                                cu::Row()
                                                    .alignment(Alignment::CenterStart)
                                                    .spacing(6.0)
                                                    .modifier(Modifier::new().fill_max_width().padding(2.0))
                                                    .content(|| {
                                                        NavIcon(ICON_EDIT_SVG, p.muted.to_argb(), 15.0);
                                                        Label("+23", Fs::Sm, Weight::Medium, Some(paint(p.green.strong)), Some(1));
                                                        Label("-6", Fs::Sm, Weight::Medium, Some(paint(p.red.strong)), Some(1));
                                                        Label("· 파일 3개", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                                                        cu::Spacer().modifier(Modifier::new().weight(1.0));
                                                        Pill("PR #139 보기", PillTone::Grey, true);
                                                    });
                                                for (f_path, add_s, del_s) in [
                                                    ("ember/proxy/src/shim.rs", "+14", "-3"),
                                                    ("ember/proxy/src/router.rs", "+6", "-2"),
                                                    ("docs/SPEC.md", "+3", "-1"),
                                                ] {
                                                    cu::Row()
                                                        .alignment(Alignment::CenterStart)
                                                        .spacing(6.0)
                                                        .modifier(Modifier::new().fill_max_width().padding(2.0))
                                                        .content(move || {
                                                            cu::Box().modifier(Modifier::new().weight(1.0)).content(move || {
                                                                Label(f_path, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(1));
                                                            });
                                                            Label(add_s, Fs::Xs, Weight::Medium, Some(paint(p.green.strong)), Some(1));
                                                            Label(del_s, Fs::Xs, Weight::Medium, Some(paint(p.red.strong)), Some(1));
                                                        });
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
                                });
                        });
                    }
                });
        });
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

    cu::Box()
        .modifier(Modifier::new().fill_max_width().fill_max_height())
        .content(move || {
            let sid_row = sid.clone();
            let sid_p = sid.clone();
            let sid_sh = sid.clone();
            let info_btn = info_open.clone();
            let pv_btn = panel_visible.clone();
            let show_inline = wide && panel_visible.get();
            let back_cb = on_back.clone();
            let m_opt = member_info.clone();

            cu::Row()
                .modifier(Modifier::new().fill_max_width().fill_max_height())
                .content(move || {
                    cu::Column()
                        .modifier(Modifier::new().weight(1.0).fill_max_height())
                        .content(move || {
                            // Compact 1-row .topbar header
                            cu::Row()
                                .alignment(Alignment::CenterStart)
                                .space_role(SpaceRole::Sm)
                                .modifier(
                                    Modifier::new()
                                        .fill_max_width()
                                        .height(52.0)
                                        .background(paint(p.surface))
                                        .padding(SP[2]),
                                )
                                .content(move || {
                                    let b_cb = back_cb.clone();
                                    IconButton(IconRole::Back, true, move || b_cb());
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
                                        Label(
                                            &m.accessible_name(),
                                            Fs::Md,
                                            Weight::Semibold,
                                            Some(paint(p.text)),
                                            Some(1),
                                        );
                                        Pill(st_txt, st_tone.into(), true);
                                    }
                                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                                    let io = info_btn.clone();
                                    let pv = pv_btn.clone();
                                    IconButton(IconRole::Sidebar, true, move || {
                                        if wide {
                                            pv.update(|v| *v = !*v);
                                        } else {
                                            io.set(true);
                                        }
                                    });
                                });
                            cu::Separator(Some(paint(p.line)));
                            cu::Box()
                                .modifier(Modifier::new().weight(1.0).fill_max_width())
                                .content(move || {
                                    ConversationView(&sid_row, true);
                                });
                        });
                    if show_inline {
                        cu::Spacer().modifier(
                            Modifier::new()
                                .width(1.0)
                                .fill_max_height()
                                .background(paint(p.line)),
                        );
                        cu::Column()
                            .modifier(Modifier::new().width(INFO_PANEL_W).fill_max_height())
                            .content(move || {
                                InfoPanel(&sid_p);
                            });
                    }
                });

            if !wide {
                let io_d = info_open.clone();
                EmberSheet(
                    "세션 정보",
                    info_open.get(),
                    move || io_d.set(false),
                    None,
                    move || {
                        cu::Box()
                            .modifier(Modifier::new().fill_max_width().height(PANE_H))
                            .content(move || {
                                InfoPanel(&sid_sh);
                            });
                    },
                );
            }
        });
}

fn KpiCard(title: &str, value: &str, sub: &str, tone: Option<Color>) {
    let p = ember_theme().palette;
    let t = title.to_string();
    let v = value.to_string();
    let s = sub.to_string();
    let v_col = tone.unwrap_or(p.text);
    cu::Box()
        .modifier(
            Modifier::new()
                .weight(1.0)
                .height(100.0)
                .background(paint(p.surface))
                .border(1.0, paint(p.line))
                .corner_radius(CORNER_LG)
                .padding(SP[2]),
        )
        .content(move || {
            cu::Column()
                .spacing(4.0)
                .modifier(Modifier::new().fill_max_width())
                .content(move || {
                    Label(&t, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(1));
                    Label(&v, Fs::Xl2, Weight::Regular, Some(paint(v_col)), Some(1));
                    Label(&s, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                });
        });
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
    cu::Column()
        .space_role(SpaceRole::Xs)
        .modifier(m)
        .content(move || {
            cu::Row()
                .alignment(Alignment::CenterStart)
                .space_role(SpaceRole::Sm)
                .modifier(Modifier::new().fill_max_width())
                .content(|| {
                    Label(
                        col_lbl,
                        Fs::Sm,
                        Weight::Semibold,
                        Some(paint(p.text_2)),
                        Some(1),
                    );
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                    Pill(&ms.len().to_string(), PillTone::Grey, true);
                });
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
                            cu::Column()
                                .space_role(SpaceRole::Xs)
                                .modifier(Modifier::new().fill_max_width())
                                .content(move || {
                                    cu::Row()
                                        .alignment(Alignment::CenterStart)
                                        .space_role(SpaceRole::Xs)
                                        .modifier(Modifier::new().fill_max_width())
                                        .content(move || {
                                            Character(
                                                &m_agent,
                                                m_leader,
                                                22.0,
                                                &m_name,
                                                &pr_card,
                                                CharState::from_status(m_status),
                                                None,
                                            );
                                            cu::Box()
                                                .modifier(Modifier::new().weight(1.0))
                                                .content(move || {
                                                    Label(
                                                        &m_acc,
                                                        Fs::Sm,
                                                        Weight::Semibold,
                                                        Some(paint(p.text)),
                                                        Some(1),
                                                    );
                                                });
                                            StatusDot(m_status);
                                        });
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
                                });
                        },
                    );
                });
            }
        });
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

    cu::Box()
        .modifier(
            Modifier::new()
                .fill_max_width()
                .background(paint(p.surface))
                .border(1.0, paint(p.line))
                .corner_radius(CORNER_LG),
        )
        .content(move || {
            cu::Column()
                .modifier(Modifier::new().fill_max_width())
                .content(move || {
                    // 1. Header (.tl-head: ~73dp high)
                    cu::Row()
                        .alignment(Alignment::CenterStart)
                        .spacing(SP[2])
                        .modifier(Modifier::new().fill_max_width().height(72.0).padding(SP[3]))
                        .content(|| {
                            cu::Column()
                                .spacing(4.0)
                                .modifier(Modifier::new().weight(1.0))
                                .content(|| {
                                    Label(
                                        TIMELINE_TITLE,
                                        Fs::Lg,
                                        Weight::Semibold,
                                        Some(paint(p.text)),
                                        Some(1),
                                    );
                                    cu::Row()
                                        .alignment(Alignment::CenterStart)
                                        .spacing(6.0)
                                        .content(|| {
                                            NavIcon(ICON_CHECK_SVG, p.green.strong.to_argb(), 15.0);
                                            Label(
                                                timeline_summary(services().demo.is_some()),
                                                Fs::Xs,
                                                Weight::Regular,
                                                Some(paint(p.muted)),
                                                Some(1),
                                            );
                                        });
                                });
                        });
                    cu::Separator(Some(paint(p.line)));

                    // 2. Body (.tl-body): left labels column + right SVG Gantt plot + time axis
                    cu::Column()
                        .modifier(Modifier::new().fill_max_width())
                        .content(move || {
                            cu::Row()
                                .alignment(Alignment::TopStart)
                                .modifier(Modifier::new().fill_max_width().height(280.0))
                                .content(move || {
                                    // Left labels column (.tl-labs)
                                    cu::Column()
                                        .modifier(Modifier::new().width(label_w).fill_max_height())
                                        .content(move || {
                                            for (lane_idx, lane) in DEMO_TL_LANES.iter().enumerate()
                                            {
                                                let pr_l = proj_owned.clone();
                                                let (strong_col, _) = (lane.tone)(p);
                                                cu::Row()
                                                    .alignment(Alignment::CenterStart)
                                                    .spacing(8.0)
                                                    .modifier(
                                                        Modifier::new()
                                                            .fill_max_width()
                                                            .height(55.0)
                                                            .padding(SP[1]),
                                                    )
                                                    .content(move || {
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
                                                            cu::Column()
                                                                .spacing(2.0)
                                                                .modifier(
                                                                    Modifier::new().weight(1.0),
                                                                )
                                                                .content(move || {
                                                                    cu::Row()
                                                                        .alignment(
                                                                            Alignment::CenterStart,
                                                                        )
                                                                        .spacing(5.0)
                                                                        .content(move || {
                                                                            cu::Box().modifier(
                                                                                Modifier::new()
                                                                                    .size(8.0, 8.0)
                                                                                    .background(
                                                                                        paint(
                                                                                            strong_col,
                                                                                        ),
                                                                                    )
                                                                                    .corner_radius(
                                                                                        4.0,
                                                                                    ),
                                                                            );
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
                                                                        });
                                                                    Label(
                                                                        lane.sub,
                                                                        Fs::Xs,
                                                                        Weight::Regular,
                                                                        Some(paint(p.muted)),
                                                                        Some(1),
                                                                    );
                                                                });
                                                        }
                                                    });
                                                if lane_idx + 1 < DEMO_TL_LANES.len() {
                                                    cu::Separator(Some(paint(p.line)));
                                                }
                                            }
                                        });
                                    // Vertical separator between label and plot
                                    cu::Box().modifier(
                                        Modifier::new()
                                            .width(1.0)
                                            .fill_max_height()
                                            .background(paint(p.line)),
                                    );
                                    // Right Gantt plot area (.tl-lanes + .tl-h + .tl-grid)
                                    cu::Box()
                                        .alignment(Alignment::CenterStart)
                                        .modifier(
                                            Modifier::new()
                                                .weight(1.0)
                                                .fill_max_height()
                                                .padding(SP[0]),
                                        )
                                        .content(move || {
                                            cu::Image(plot_id)
                                                .modifier(Modifier::new().size(plot_w, 280.0));
                                        });
                                });
                            cu::Separator(Some(paint(p.line)));
                            // Time axis row (.tl-axis)
                            cu::Row()
                                .alignment(Alignment::CenterStart)
                                .modifier(Modifier::new().fill_max_width().height(26.0))
                                .content(move || {
                                    cu::Spacer().modifier(Modifier::new().width(label_w));
                                    cu::Box().modifier(
                                        Modifier::new()
                                            .width(1.0)
                                            .fill_max_height()
                                            .background(paint(p.line)),
                                    );
                                    cu::Row()
                                        .alignment(Alignment::CenterStart)
                                        .modifier(
                                            Modifier::new()
                                                .weight(1.0)
                                                .fill_max_height()
                                                .padding(SP[0]),
                                        )
                                        .content(|| {
                                            for tick in ["0s", "5s", "10s", "15s"] {
                                                cu::Box()
                                                    .modifier(Modifier::new().weight(1.0))
                                                    .content(move || {
                                                        Label(
                                                            tick,
                                                            Fs::Xs2,
                                                            Weight::Regular,
                                                            Some(paint(p.muted)),
                                                            Some(1),
                                                        );
                                                    });
                                            }
                                            Label(
                                                "16.4s",
                                                Fs::Xs2,
                                                Weight::Regular,
                                                Some(paint(p.muted)),
                                                Some(1),
                                            );
                                        });
                                });
                        });
                    cu::Separator(Some(paint(p.line)));

                    // 3. Footer (.tl-foot: ~72dp high): play (40x40), restart (32x32), speed 1x/2x, time, legend
                    let cur_spd = speed.get();
                    let spd_st = speed.clone();
                    cu::Row()
                        .alignment(Alignment::CenterStart)
                        .spacing(SP[2])
                        .modifier(
                            Modifier::new()
                                .fill_max_width()
                                .height(72.0)
                                .background(paint(p.surface_2))
                                .padding(SP[3]),
                        )
                        .content(move || {
                            Clickable(
                                Modifier::new()
                                    .size(40.0, 40.0)
                                    .background(paint(p.ink))
                                    .shape_role(ShapeRole::Full),
                                Alignment::Center,
                                || notice::post("타임라인 재생 데모입니다"),
                                || NavIcon(ICON_PLAY_SVG, p.ink_text.to_argb(), 16.0),
                            );
                            Clickable(
                                Modifier::new()
                                    .size(32.0, 32.0)
                                    .background(paint(p.surface))
                                    .border(1.0, paint(p.line))
                                    .shape_role(ShapeRole::Full),
                                Alignment::Center,
                                || notice::post("처음으로 되돌렸습니다"),
                                || NavIcon(ICON_REPEAT_SVG, p.text_2.to_argb(), 15.0),
                            );
                            // 1x / 2x mini segmented control
                            cu::Row()
                                .alignment(Alignment::Center)
                                .modifier(
                                    Modifier::new()
                                        .height(26.0)
                                        .background(paint(p.surface))
                                        .border(1.0, paint(p.line))
                                        .corner_radius(13.0)
                                        .padding(2.0),
                                )
                                .content(move || {
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
                                                cu::Row()
                                                    .alignment(Alignment::Center)
                                                    .content(|| {
                                                        cu::Spacer()
                                                            .modifier(Modifier::new().width(8.0));
                                                        Label(
                                                            s_lbl,
                                                            Fs::Xs,
                                                            Weight::Semibold,
                                                            Some(paint(fg)),
                                                            Some(1),
                                                        );
                                                        cu::Spacer()
                                                            .modifier(Modifier::new().width(8.0));
                                                    });
                                            },
                                        );
                                    }
                                });
                            Label(
                                "16.4s",
                                Fs::Xs,
                                Weight::Medium,
                                Some(paint(p.muted)),
                                Some(1),
                            );
                            cu::Spacer().modifier(Modifier::new().weight(1.0));
                            if !is_phone {
                                // 5-item legend (.tl-legend)
                                cu::Row()
                                    .alignment(Alignment::CenterStart)
                                    .spacing(SP[3])
                                    .content(move || {
                                        // 1. 생각
                                        cu::Row()
                                            .alignment(Alignment::CenterStart)
                                            .spacing(6.0)
                                            .content(|| {
                                                cu::Box().modifier(
                                                    Modifier::new()
                                                        .size(18.0, 10.0)
                                                        .background(paint(p.blue.soft))
                                                        .border(1.0, paint(p.blue.strong))
                                                        .corner_radius(3.0),
                                                );
                                                Label(
                                                    "생각",
                                                    Fs::Xs,
                                                    Weight::Regular,
                                                    Some(paint(p.text_2)),
                                                    Some(1),
                                                );
                                            });
                                        // 2. 도구 호출
                                        cu::Row()
                                            .alignment(Alignment::CenterStart)
                                            .spacing(6.0)
                                            .content(|| {
                                                cu::Box().modifier(
                                                    Modifier::new()
                                                        .size(18.0, 10.0)
                                                        .background(paint(p.blue.strong))
                                                        .corner_radius(3.0),
                                                );
                                                Label(
                                                    "도구 호출",
                                                    Fs::Xs,
                                                    Weight::Regular,
                                                    Some(paint(p.text_2)),
                                                    Some(1),
                                                );
                                            });
                                        // 3. 대기 (hatched)
                                        cu::Row()
                                            .alignment(Alignment::CenterStart)
                                            .spacing(6.0)
                                            .content(move || {
                                                cu::Image(wait_swatch_id)
                                                    .modifier(Modifier::new().size(18.0, 10.0));
                                                Label(
                                                    "대기",
                                                    Fs::Xs,
                                                    Weight::Regular,
                                                    Some(paint(p.text_2)),
                                                    Some(1),
                                                );
                                            });
                                        // 4. 핸드오프 (L-shaped)
                                        cu::Row()
                                            .alignment(Alignment::CenterStart)
                                            .spacing(6.0)
                                            .content(move || {
                                                cu::Image(hand_swatch_id)
                                                    .modifier(Modifier::new().size(10.0, 10.0));
                                                Label(
                                                    "핸드오프",
                                                    Fs::Xs,
                                                    Weight::Regular,
                                                    Some(paint(p.text_2)),
                                                    Some(1),
                                                );
                                            });
                                        // 5. 실패 (12x12 circle)
                                        cu::Row()
                                            .alignment(Alignment::CenterStart)
                                            .spacing(6.0)
                                            .content(|| {
                                                cu::Box().modifier(
                                                    Modifier::new()
                                                        .size(12.0, 12.0)
                                                        .background(paint(p.red.soft))
                                                        .border(1.0, paint(p.red.strong))
                                                        .corner_radius(6.0),
                                                );
                                                Label(
                                                    "실패",
                                                    Fs::Xs,
                                                    Weight::Regular,
                                                    Some(paint(p.text_2)),
                                                    Some(1),
                                                );
                                            });
                                    });
                            }
                        });
                });
        });
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
    EmberCard(false, fill, weight, Some(0.0), None, move || {
        cu::Column().modifier(Modifier::new().fill_max_width()).content(move || {
            cu::Spacer().modifier(Modifier::new().height(13.0));
            cu::Row().modifier(Modifier::new().fill_max_width()).content(move || {
                cu::Spacer().modifier(Modifier::new().width(SP[4]));
                cu::Box().modifier(Modifier::new().weight(1.0)).content(move || {
                    CardHead(ico, &title, trailing);
                });
                cu::Spacer().modifier(Modifier::new().width(SP[4]));
            });
            cu::Spacer().modifier(Modifier::new().height(13.0));
            cu::Spacer().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
            cu::Spacer().modifier(Modifier::new().height(SP[3]));
            cu::Row().modifier(Modifier::new().fill_max_width()).content(move || {
                cu::Spacer().modifier(Modifier::new().width(SP[4]));
                cu::Column().modifier(Modifier::new().weight(1.0)).content(body);
                cu::Spacer().modifier(Modifier::new().width(SP[4]));
            });
            cu::Spacer().modifier(Modifier::new().height(SP[3]));
        });
    });
}

fn EmptyText(text: &str) {
    let p = ember_theme().palette;
    Label(text, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
}

/// `.list-row`: the 36 px git tile, `#num title` over `member · branch`, and the CI pill over
/// the review state. TODO(compose-rust): the row divider is inside the card padding, and the
/// right column is not right-aligned (no column cross-axis alignment in the pinned toolkit).
fn PrRowView(row: PrRow, first: bool) {
    let p = ember_theme().palette;
    cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(move || {
        if !first {
            cu::Spacer().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
        }
        cu::Row().spacing(SP[2]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(move || {
            cu::Box()
                .alignment(Alignment::Center)
                .modifier(
                    Modifier::new()
                        .size(36.0, 36.0)
                        .background(paint(p.surface_2))
                        .border(1.0, paint(p.line))
                        .corner_radius(CORNER_SM),
                )
                .content(|| NavIcon(ICON_GIT_SVG, p.text_2.to_argb(), 18.0));
            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                Label(&row.headline(), Fs::Md, Weight::Medium, None, Some(1));
                Label(&row.sub_line(), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
            });
            cu::Column().spacing(SP[0]).content(|| {
                Pill(row.ci.label(), tone_pill(row.ci.tone()), false);
                Label(&row.review, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
            });
        });
    });
}

/// `.tnode`: an avatar for a member, a spinner or a status dot for a thread, then name and task.
/// Children are indented. TODO(compose-rust): the 1 px guide line (a left border) needs a
/// one-sided border or a stretching child; indentation alone for now.
fn TreeNodeView(node: SubNode, project: String) {
    let p = ember_theme().palette;
    cu::Column().modifier(Modifier::new().fill_max_width()).content(move || {
        cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
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
            Label(&node.task, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
        });
        if !node.children.is_empty() {
            cu::Row().modifier(Modifier::new().fill_max_width()).content(|| {
                cu::Spacer().modifier(Modifier::new().width(SP[3] + 2.0));
                cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                    for c in node.children.iter() {
                        TreeNodeView(c.clone(), project.clone());
                    }
                });
            });
        }
    });
}

/// `.sl-row`: a lead, a fixed-width name, a track and a value.
fn UsageRow(lead: impl FnOnce(), name: &str, name_w: f32, pct: f32, fill: Color, value: &str) {
    let p = ember_theme().palette;
    cu::Row().spacing(10.0).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
        lead();
        cu::Box().modifier(Modifier::new().width(name_w)).content(|| {
            Label(name, Fs::Sm, Weight::Regular, None, Some(1));
        });
        cu::Box().modifier(Modifier::new().weight(1.0)).content(|| TrackBar(pct, 6.0, fill));
        Label(value, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
    });
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
        cu::Column().spacing(SP[0]).modifier(Modifier::new().fill_max_width()).content(|| {
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
            cu::Spacer().modifier(Modifier::new().height(SP[1]));
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
                cu::Spacer().modifier(Modifier::new().height(SP[1]));
            }
            for r in d.counts.iter() {
                Label(r, Fs::Sm, Weight::Regular, None, Some(1));
            }
        });
    });
}

/// The bottom grid (`.grid.g3.sgrid`), gap 20: three columns from 1280 dp, two from 900 dp
/// (the third card below), one under that. Every card fills its share of the page width
/// (`status_card_width`); under 900 dp the width is set outright, because a column sized to its
/// content shrank the cards to about 308 dp at 390 dp.
fn StatusCardsGrid(width_dp: f32, d: StatusCardsData) {
    let (d1, d2, d3) = (d.clone(), d.clone(), d);
    match status_grid_columns(width_dp) {
        3 => {
            cu::Row().alignment(Alignment::TopStart).spacing(SP[4]).modifier(Modifier::new().fill_max_width()).content(move || {
                PrCard(true, d1);
                SubagentCard(true, d2);
                LoadCard(true, d3);
            });
        }
        2 => {
            cu::Column().spacing(SP[4]).modifier(Modifier::new().fill_max_width()).content(move || {
                cu::Row().alignment(Alignment::TopStart).spacing(SP[4]).modifier(Modifier::new().fill_max_width()).content(move || {
                    PrCard(true, d1);
                    SubagentCard(true, d2);
                });
                LoadCard(false, d3);
            });
        }
        _ => {
            let w = status_card_width(width_dp, 1, SP[4]);
            cu::Column().spacing(SP[4]).modifier(Modifier::new().width(w)).content(move || {
                PrCard(false, d1);
                SubagentCard(false, d2);
                LoadCard(false, d3);
            });
        }
    }
}

/// Whether the stat tiles, the timeline card and the kanban card are drawn above the status cards.
/// `EMBER_CR_SCROLL=cards` hides them for review captures: the toolkit cannot scroll a column from
/// code (TODO(compose-rust#385)), so the blocks that would have scrolled out are left out and the
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

    cu::ScrollColumn()
        .modifier(Modifier::new().fill_max_width().fill_max_height())
        .content(move || {
            cu::Column()
                .space_role(SpaceRole::Md)
                .modifier(
                    Modifier::new()
                        .fill_max_width()
                        .padding(status_page_pad(window.width_dp)),
                )
                .content(move || {
                    // Review captures: `EMBER_CR_SCROLL=cards` leaves out the blocks above the cards.
                    if status_blocks_above_cards(std::env::var("EMBER_CR_SCROLL").ok().as_deref()) {
                        // 1. 6 KPI cards grid (.skpis)
                        let kpi_rows: Vec<Vec<(String, String, String, Option<Color>)>> = kpis
                            .chunks(kpi_per_row)
                            .map(|c| c.to_vec())
                            .collect();
                        for row in kpi_rows {
                            cu::Row()
                                .space_role(SpaceRole::Sm)
                                .modifier(Modifier::new().fill_max_width())
                                .content(move || {
                                    for (t, v, s, tone) in row {
                                        KpiCard(&t, &v, &s, tone);
                                    }
                                });
                        }

                        // 2. Agent-to-agent communication timeline card (.tlcard)
                        TimelineCard(&proj_owned);

                        // 3. Outer Kanban card (.skan) with 5 columns
                        let cols_c = columns.clone();
                        let proj_k = proj_owned.clone();
                        let open_k = on_open.clone();
                        EmberCard(false, true, None, None, None, move || {
                            cu::Column()
                                .space_role(SpaceRole::Sm)
                                .modifier(Modifier::new().fill_max_width())
                                .content(move || {
                                    cu::Row()
                                        .alignment(Alignment::CenterStart)
                                        .space_role(SpaceRole::Sm)
                                        .modifier(Modifier::new().fill_max_width())
                                        .content(|| {
                                            NavIcon(ICON_GRID_SVG, p.text_2.to_argb(), 16.0);
                                            Label("칸반", Fs::Md, Weight::Semibold, None, Some(1));
                                            cu::Spacer().modifier(Modifier::new().weight(1.0));
                                            Label(
                                                &format!("팀원 {total_cnt}"),
                                                Fs::Xs,
                                                Weight::Regular,
                                                Some(paint(p.muted)),
                                                Some(1),
                                            );
                                        });

                                    if wide_kanban {
                                        cu::Row()
                                            .alignment(Alignment::TopStart)
                                            .space_role(SpaceRole::Sm)
                                            .modifier(Modifier::new().fill_max_width())
                                            .content(move || {
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
                                            });
                                    } else {
                                        cu::ScrollRow()
                                            .modifier(Modifier::new().fill_max_width())
                                            .content(move || {
                                                cu::Row()
                                                    .alignment(Alignment::TopStart)
                                                    .space_role(SpaceRole::Sm)
                                                    .content(move || {
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
                                                    });
                                            });
                                    }
                                });
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

                    cu::Spacer().modifier(Modifier::new().height(24.0));
                });
        });
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
    cu::Row()
        .alignment(Alignment::Center)
        .modifier(m)
        .content(move || {
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
                        cu::Row()
                            .alignment(Alignment::Center)
                            .spacing(5.0)
                            .content(|| {
                                cu::Spacer().modifier(Modifier::new().width(10.0));
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
                                cu::Spacer().modifier(Modifier::new().width(10.0));
                            });
                    },
                );
            }
        });
}

/// Pill-shaped toolbar action button (`.btn.sm` in `.sesshead`).
fn ToolbarBtn(
    icon_svg: &'static str,
    label: &str,
    show_label: bool,
    chevron: bool,
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
    Clickable(m, Alignment::Center, on_click, move || {
        if show_label || chevron {
            cu::Row()
                .alignment(Alignment::CenterStart)
                .spacing(5.0)
                .content(|| {
                    cu::Spacer().modifier(Modifier::new().width(8.0));
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
                    cu::Spacer().modifier(Modifier::new().width(8.0));
                });
        } else {
            NavIcon(icon_svg, p.text_2.to_argb(), 15.0);
        }
    });
}

/// New member sheet (`#snew` in mockup `session.js` lines 386-395).
fn NewMemberSheet(
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
    let busy = remember_with_key(format!("{proj_owned}:snew_busy"), || mutable_state_of(false));

    let comp_opts = vec!["mac-mini".to_string(), "mac-studio".to_string(), "이 컴퓨터".to_string()];
    let c_idx = comp_idx.get();

    let close_hdr = on_close.clone();
    let close_f = on_close.clone();
    let create_cb = {
        let ui = ui.clone();
        let proj_c = proj_owned.clone();
        let role_c = role_name.clone();
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
            let cwd = services()
                .client
                .read(|s| crate::model::latest_cwd(s, &proj_c))
                .unwrap_or_else(|| "/work".to_string());
            let body = crate::server::CreateSession {
                project: proj_c.clone(),
                agent: "claude-code".to_string(),
                cwd,
                model: None,
                title: Some(r_title),
                account: None,
            };
            busy_c.set(true);
            let b_done = busy_c.clone();
            let u_done = ui.clone();
            let mut cl_done = close_done.clone();
            run_shared(
                async move { services().create_session(&body).await },
                move |res| {
                    b_done.set(false);
                    match res {
                        Ok(rec) => {
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
        cu::Column()
            .modifier(Modifier::new().fill_max_width())
            .content(|| {
                // Header (.sheet-h): title + project name on the left, close button on the right
                cu::Row()
                    .alignment(Alignment::CenterStart)
                    .modifier(Modifier::new().fill_max_width().height(SHEET_HEADER_H))
                    .content(move || {
                        cu::Spacer().modifier(Modifier::new().width(SP[4]));
                        Label("새 팀원", Fs::Xl, Weight::Semibold, None, Some(1));
                        cu::Spacer().modifier(Modifier::new().width(SP[2]));
                        Label(
                            &proj_disp,
                            Fs::Md,
                            Weight::Regular,
                            Some(paint(p.muted)),
                            Some(1),
                        );
                        cu::Spacer().modifier(Modifier::new().weight(1.0));
                        CloseButton(move || cl_h());
                        cu::Spacer().modifier(Modifier::new().width(SP[4]));
                    });
                cu::Separator(Some(paint(p.line)));

                cu::Column()
                    .spacing(16.0)
                    .modifier(Modifier::new().fill_max_width().padding(SP[4]))
                    .content(|| {
                        // 1. 역할 이름 (.field: 40dp, 1px line border, surface fill)
                        cu::Column()
                            .spacing(8.0)
                            .modifier(Modifier::new().fill_max_width())
                            .content(|| {
                                Label(
                                    "역할 이름",
                                    Fs::Sm,
                                    Weight::Regular,
                                    Some(paint(p.text_2)),
                                    Some(1),
                                );
                                let val_r = role_name.get();
                                cu::Box()
                                    .alignment(Alignment::CenterStart)
                                    .modifier(
                                        Modifier::new()
                                            .fill_max_width()
                                            .height(H_BTN)
                                            .background(paint(p.surface))
                                            .border(1.0, paint(p.line))
                                            .corner_radius(CORNER_SM)
                                            .padding(SP[2]),
                                    )
                                    .content(move || {
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
                                    });
                            });

                        // 2. 첫 지시 (선택) (textarea.field: 118dp, 1px line border, surface fill)
                        cu::Column()
                            .spacing(8.0)
                            .modifier(Modifier::new().fill_max_width())
                            .content(|| {
                                Label(
                                    "첫 지시 (선택)",
                                    Fs::Sm,
                                    Weight::Regular,
                                    Some(paint(p.text_2)),
                                    Some(1),
                                );
                                let val_p = prompt_txt.get();
                                cu::Box()
                                    .alignment(Alignment::TopStart)
                                    .modifier(
                                        Modifier::new()
                                            .fill_max_width()
                                            .height(118.0)
                                            .background(paint(p.surface))
                                            .border(1.0, paint(p.line))
                                            .corner_radius(CORNER_SM)
                                            .padding(SP[2]),
                                    )
                                    .content(move || {
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
                                    });
                            });

                        // 3. 하네스, 모델, 권한 모드 (.snew-picks: 40dp height, CORNER_SM)
                        cu::Column()
                            .spacing(8.0)
                            .modifier(Modifier::new().fill_max_width())
                            .content(|| {
                                Label(
                                    "하네스, 모델, 권한 모드",
                                    Fs::Sm,
                                    Weight::Regular,
                                    Some(paint(p.text_2)),
                                    Some(1),
                                );
                                cu::Row()
                                    .alignment(Alignment::CenterStart)
                                    .spacing(SP[1])
                                    .modifier(Modifier::new().fill_max_width())
                                    .content(|| {
                                        Clickable(
                                            Modifier::new()
                                                .height(H_BTN)
                                                .background(paint(p.surface_2))
                                                .border(1.0, paint(p.line))
                                                .corner_radius(CORNER_SM),
                                            Alignment::CenterStart,
                                            || notice::post("데모에서는 기본 모델을 사용합니다"),
                                            || {
                                                cu::Row()
                                                    .alignment(Alignment::CenterStart)
                                                    .spacing(6.0)
                                                    .content(|| {
                                                        cu::Spacer()
                                                            .modifier(Modifier::new().width(10.0));
                                                        cu::Box().modifier(
                                                            Modifier::new()
                                                                .size(8.0, 8.0)
                                                                .background(paint(p.orange.strong))
                                                                .corner_radius(4.0),
                                                        );
                                                        Label(
                                                            "Claude Code · Opus 5.5 · 높음",
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
                                                        cu::Spacer()
                                                            .modifier(Modifier::new().width(10.0));
                                                    });
                                            },
                                        );
                                        Clickable(
                                            Modifier::new()
                                                .height(H_BTN)
                                                .background(paint(p.surface_2))
                                                .border(1.0, paint(p.line))
                                                .corner_radius(CORNER_SM),
                                            Alignment::CenterStart,
                                            || notice::post("권한 모드는 세션에서도 바꿀 수 있습니다"),
                                            || {
                                                cu::Row()
                                                    .alignment(Alignment::CenterStart)
                                                    .spacing(6.0)
                                                    .content(|| {
                                                        cu::Spacer()
                                                            .modifier(Modifier::new().width(10.0));
                                                        NavIcon(
                                                            ICON_SHIELD_SVG,
                                                            p.text_2.to_argb(),
                                                            14.0,
                                                        );
                                                        Label(
                                                            "명령마다 확인",
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
                                                        cu::Spacer()
                                                            .modifier(Modifier::new().width(10.0));
                                                    });
                                            },
                                        );
                                    });
                                Label(
                                    "프로필 Claude 개인",
                                    Fs::Xs,
                                    Weight::Regular,
                                    Some(paint(p.muted)),
                                    Some(1),
                                );
                            });

                        // 4. 컴퓨터
                        cu::Column()
                            .spacing(8.0)
                            .modifier(Modifier::new().fill_max_width())
                            .content(|| {
                                Label(
                                    "컴퓨터",
                                    Fs::Sm,
                                    Weight::Regular,
                                    Some(paint(p.text_2)),
                                    Some(1),
                                );
                                let ci = comp_idx.clone();
                                Select(
                                    &comp_opts[c_idx.min(comp_opts.len() - 1)],
                                    &comp_opts,
                                    c_idx,
                                    440.0,
                                    move |i| ci.set(i),
                                );
                            });
                    });
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
                            cu::Row().alignment(Alignment::Center).content(|| {
                                cu::Spacer().modifier(Modifier::new().width(12.0));
                                Label(
                                    "취소",
                                    Fs::Sm,
                                    Weight::Medium,
                                    Some(paint(p.text)),
                                    Some(1),
                                );
                                cu::Spacer().modifier(Modifier::new().width(12.0));
                            });
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
                            cu::Row().alignment(Alignment::Center).content(|| {
                                cu::Spacer().modifier(Modifier::new().width(12.0));
                                Label(
                                    "만들기",
                                    Fs::Sm,
                                    Weight::Medium,
                                    Some(paint(p.ink_text)),
                                    Some(1),
                                );
                                cu::Spacer().modifier(Modifier::new().width(12.0));
                            });
                        },
                    );
                });
            });
    };

    if is_compact {
        cu::Sheet(true)
            .modifier(modifier.fill_max_width())
            .on_dismiss(on_close)
            .content(body);
    } else {
        cu::Dialog(true)
            .modifier(modifier.width(758.0))
            .on_dismiss(on_close)
            .content(body);
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
    let project_menu = remember(|| mutable_state_of(false));
    let more_menu = remember(|| mutable_state_of(false));
    let web_menu = remember(|| mutable_state_of(false));

    let show_archived = ui.show_archived.get();
    let boxes = {
        let _ = ui.live.launcher.get();
        services().client.read(|s| project_boxes_with_archived(s, show_archived))
    };

    let Some(current) = resolve_project(None, project.get().as_deref(), &boxes) else {
        cu::Box()
            .alignment(Alignment::Center)
            .modifier(Modifier::new().fill_max_width().fill_max_height())
            .content(|| {
                cu::Column()
                    .alignment(Alignment::Center)
                    .space_role(SpaceRole::Sm)
                    .content(|| {
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
                    });
            });
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
    let chips_below = view_chips_below(window.width_dp);
    let hide_view_lbl = window.width_dp < 1280.0 && !chips_below;
    let show_btn_lbl = window.width_dp >= 1400.0;
    let show_web_lbl = window.width_dp >= 1400.0;

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

    cu::Box()
        .modifier(Modifier::new().fill_max_width().fill_max_height())
        .content(move || {
            let sections_col = sections.clone();
            let current_col = current.clone();
            let target_col = target.clone();
            let query_col = query.clone();
            let open_col = open_member.clone();
            let rc_col = rail_collapsed.clone();
            let dr_rail = drawer.clone();

            cu::Row()
                .modifier(Modifier::new().fill_max_width().fill_max_height())
                .content(|| {
                    if show_column {
                        cu::Column()
                            .modifier(
                                Modifier::new()
                                    .width(SIDE_LIST_W)
                                    .fill_max_height()
                                    .background(paint(p.surface_2)),
                            )
                            .content(move || {
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
                            });
                        cu::Spacer().modifier(
                            Modifier::new()
                                .width(1.0)
                                .fill_max_height()
                                .background(paint(p.line)),
                        );
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
                        cu::Spacer().modifier(
                            Modifier::new()
                                .width(1.0)
                                .fill_max_height()
                                .background(paint(p.line)),
                        );
                    }

                    cu::Column()
                        .modifier(Modifier::new().weight(1.0).fill_max_height())
                        .content(|| {
                            // Top toolbar (.sesshead)
                            cu::Row()
                                .alignment(Alignment::CenterStart)
                                .spacing(6.0)
                                .modifier(
                                    Modifier::new()
                                        .fill_max_width()
                                        .height(60.0)
                                        .background(paint(p.surface))
                                        .padding(SP[2]),
                                )
                                .content(|| {
                                    if is_phone {
                                        let dr = drawer.clone();
                                        Clickable(
                                            Modifier::new()
                                                .size(32.0, 32.0)
                                                .background(paint(p.surface))
                                                .border(1.0, paint(p.line))
                                                .shape_role(ShapeRole::Full),
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
                                        proj_mod = proj_mod.weight(1.0);
                                    }
                                    Clickable(
                                        proj_mod,
                                        Alignment::CenterStart,
                                        move || pm.set(true),
                                        move || {
                                            cu::Column()
                                                .spacing(1.0)
                                                .content(move || {
                                                    cu::Row()
                                                        .alignment(Alignment::CenterStart)
                                                        .spacing(4.0)
                                                        .content(|| {
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
                                                        });
                                                    Label(
                                                        &s_line,
                                                        Fs::Xs2,
                                                        Weight::Regular,
                                                        Some(paint(p.muted)),
                                                        Some(1),
                                                    );
                                                });
                                        },
                                    );
                                    if !is_phone {
                                        cu::Spacer().modifier(Modifier::new().weight(1.0));
                                    }
                                    if !chips_below {
                                        let sv = set_view.clone();
                                        ViewSeg(view_now, hide_view_lbl, false, move |v| sv(v));
                                    }
                                    if !is_phone {
                                        let nm = new_member_open.clone();
                                        ToolbarBtn(
                                            ICON_PLUS_SVG,
                                            "새 팀원",
                                            show_btn_lbl,
                                            false,
                                            move || nm.set(true),
                                        );
                                        let u = ui.clone();
                                        let c = current.clone();
                                        ToolbarBtn(
                                            ICON_GIT_SVG,
                                            "버전 관리",
                                            show_btn_lbl,
                                            false,
                                            move || {
                                                u.set_selected_project(Some(c.clone()));
                                                u.set_menu(NavMenu::VersionControl);
                                            },
                                        );
                                        let wm = web_menu.clone();
                                        ToolbarBtn(
                                            ICON_GLOBE_SVG,
                                            "웹 페이지",
                                            show_web_lbl,
                                            true,
                                            move || wm.set(true),
                                        );
                                    } else {
                                        let mm = more_menu.clone();
                                        Clickable(
                                            Modifier::new()
                                                .size(32.0, 32.0)
                                                .background(paint(p.surface))
                                                .border(1.0, paint(p.line))
                                                .shape_role(ShapeRole::Full),
                                            Alignment::Center,
                                            move || mm.set(true),
                                            || NavIcon(ICON_MORE_H_SVG, p.text_2.to_argb(), 16.0),
                                        );
                                    }
                                    if let Some(t) = target.clone() {
                                        IdeMenu(&t);
                                    } else {
                                        AppButton(
                                            "IDE 열기",
                                            Some(IconRole::Forward),
                                            ButtonKind::Primary,
                                            true,
                                            false,
                                            false,
                                            || {},
                                        );
                                    }
                                    if is_phone {
                                        // Keep the trailing group clear of the shell's floating menu button.
                                        cu::Spacer().modifier(
                                            Modifier::new().width(super::project::PHONE_MENU_ROOM),
                                        );
                                    }
                                });

                            if chips_below {
                                cu::Row()
                                    .alignment(Alignment::CenterStart)
                                    .modifier(
                                        Modifier::new()
                                            .fill_max_width()
                                            .background(paint(p.surface))
                                            .padding(SP[1]),
                                    )
                                    .content(|| {
                                        let sv = set_view.clone();
                                        ViewSeg(view_now, false, true, move |v| sv(v));
                                    });
                            }

                            cu::Separator(Some(paint(p.line)));

                            // Body view
                            cu::Box()
                                .modifier(Modifier::new().weight(1.0).fill_max_width())
                                .content(|| match view_now {
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
                                            cu::Box()
                                                .alignment(Alignment::Center)
                                                .modifier(
                                                    Modifier::new()
                                                        .fill_max_width()
                                                        .fill_max_height(),
                                                )
                                                .content(|| {
                                                    Label(
                                                        NO_SESSION,
                                                        Fs::Md,
                                                        Weight::Regular,
                                                        Some(paint(p.muted)),
                                                        None,
                                                    );
                                                });
                                        }
                                    },
                                    SessionView::Status => {
                                        StatusView(&current, &members, to_single.clone());
                                    }
                                });
                        });
                });

            // Drawer sheet on compact/tablet
            if !show_column {
                let dr_d = drawer.clone();
                let sec_d = sections.clone();
                let cur_d = current.clone();
                let tgt_d = target.clone();
                let q_d = query.clone();
                let op_d = open_member.clone();
                let is_m = view_now == SessionView::Multi;
                EmberSheet(
                    "세션 목록",
                    drawer.get(),
                    move || dr_d.set(false),
                    None,
                    move || {
                        let q_st = q_d.clone();
                        cu::Box()
                            .modifier(Modifier::new().fill_max_width().height(PANE_H))
                            .content(move || {
                                SideList(
                                    &sec_d,
                                    &cur_d,
                                    tgt_d.as_deref(),
                                    is_m,
                                    None,
                                    move |q| q_st.set(q),
                                    op_d.clone(),
                                );
                            });
                    },
                );
            }

            // New member sheet (#snew)
            {
                let nm_d = new_member_open.clone();
                NewMemberSheet(&current, new_member_open.get(), move || nm_d.set(false));
            }

            // Web pages OptionSheet
            {
                let web_opts = vec![
                    "http://192.168.0.10:3000 (Ember 웹 미리보기)".to_string(),
                    "http://192.168.0.10:6006 (컴포넌트 카탈로그)".to_string(),
                ];
                let wm_d = web_menu.clone();
                OptionSheet(
                    web_menu.get(),
                    &web_opts,
                    None,
                    320.0,
                    move || wm_d.set(false),
                    move |_| notice::post(WEB_SOON),
                );
            }

            // Project picker OptionSheet
            let proj_labels: Vec<String> = boxes.iter().map(|b| display_name(&b.id)).collect();
            let chosen_proj = boxes.iter().position(|b| b.id == current);
            let pm_d = project_menu.clone();
            let boxes_c = boxes.clone();
            let proj_st = project.clone();
            let ui_p = ui.clone();
            OptionSheet(
                project_menu.get(),
                &proj_labels,
                chosen_proj,
                POPUP_W,
                move || pm_d.set(false),
                move |idx| {
                    if let Some(b) = boxes_c.get(idx) {
                        let id = b.id.clone();
                        proj_st.set(Some(id.clone()));
                        ui_p.update_prefs(|p| p.last_project = Some(id));
                    }
                },
            );

            // Phone folded toolbar actions OptionSheet
            if is_phone {
                let more_opts = vec![
                    "새 팀원".to_string(),
                    "버전 관리".to_string(),
                    "웹 페이지".to_string(),
                ];
                let mm_d = more_menu.clone();
                let nm_m = new_member_open.clone();
                let ui_m = ui.clone();
                let cur_m = current.clone();
                OptionSheet(
                    more_menu.get(),
                    &more_opts,
                    None,
                    POPUP_W,
                    move || mm_d.set(false),
                    move |idx| match idx {
                        0 => nm_m.set(true),
                        1 => {
                            ui_m.set_selected_project(Some(cur_m.clone()));
                            ui_m.set_menu(NavMenu::VersionControl);
                        }
                        2 => notice::post(WEB_SOON),
                        _ => {}
                    },
                );
            }
        });
}

#[cfg(test)]
mod tests {
    #[test]
    fn fr_l18_status_capture_scroll_cards_hides_the_blocks_above() {
        assert!(super::status_blocks_above_cards(None));
        assert!(!super::status_blocks_above_cards(Some("cards")));
        assert!(super::status_blocks_above_cards(Some("other")));
    }

    use super::*;
    use compose_rust::protocol::{HostEvent, Mutation, PropertyValue, decode_batch};
    use compose_rust::{
        EventPayload, Host, Recomposer, WindowHeightClass, WindowSizeClass, composable,
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

    #[composable]
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
        let open = CHEVRON_OPEN.with(|c| c.borrow().clone()).expect("state captured");
        assert!(!open.get_untracked());
        host.dispatch(HostEvent {
            node_id: clicks[0].0,
            handler_id: clicks[0].1,
            payload: EventPayload::Clicked,
        })
        .expect("dispatch");
        assert!(open.get_untracked(), "the chevron opened the chooser");
    }

    #[composable]
    fn host_session_multi() {
        SessionScreen(None);
    }

    #[composable]
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
        let resize = HostEvent {
            node_id: 0,
            handler_id: 0,
            payload: EventPayload::WindowSizeChanged {
                width_dp: 1440.0,
                height_dp: 900.0,
                class: WindowSizeClass::Expanded,
                height_class: WindowHeightClass::Medium,
            },
        };
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
