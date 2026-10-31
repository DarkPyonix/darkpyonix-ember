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
    current_window_size, launched_effect, mutable_state_of, remember, remember_with_key,
};
use compose_rust::ui::{self as cu, Modifier};
use compose_rust::{Alignment, Color, IconRole, SpaceRole};

use ember_client::LauncherStatus;

pub use crate::ui::session::{
    INFO_PANEL_W, IdeChoice, InfoCard, KanbanColumn, PANE_H, PaneLayout, PaneMove, PaneTab,
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
use crate::ui::project::{Member, display_name, project_boxes, status_pill};
use crate::ui::shell::NavMenu;

use super::characters::{CharState, Character};
use super::components::{
    AppButton, ButtonKind, Clickable, EmberCard, EmberField, EmberSheet, FieldOptions, FilterChip,
    IconButton, Label, OptionSheet, PickChip, Pill, PillTone,
};
use super::conversation::{ConversationView, TranscriptRow, item_key};
use super::notice;
use super::theme::*;

const NO_PROJECT: &str = "프로젝트가 없습니다";
const NO_MEMBERS: &str = "표시할 팀원이 없습니다";
const PICK_FROM_LIST: &str = "목록에서 팀원을 고르면 이 탭에 추가됩니다";
const NO_CONVERSATION: &str = "아직 대화가 없습니다";
const TERMINAL_SOON: &str = "터미널은 준비 중입니다";
const MODEL_LOCKED: &str = "실행 중인 세션의 모델은 바꿀 수 없습니다";
const WEB_SOON: &str = "웹 페이지 목록은 준비 중입니다";
const NO_SESSION: &str = "세션이 없습니다";
const PR_TITLE: &str = "PR과 CI";
const TIMELINE_TITLE: &str = "에이전트 소통 타임라인";

const POPUP_W: f32 = 280.0;

fn ide_label(kind: IdeKind) -> &'static str {
    match kind {
        IdeKind::Ember => "Ember 에디터",
        IdeKind::VsCode => "VS Code",
        IdeKind::Gateway => "JetBrains Gateway",
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
            .unwrap_or_default()
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
        .unwrap_or_default()
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
            let (project, folder) = IdeLaunch::editor_target(launch.as_ref());
            ui.open_editor(project, folder);
        }
        IdeAction::Unavailable(why) => {
            notice::post(format!("{label}: 사용할 수 없습니다: {why}"));
        }
    }
}

/// The model and mode chips of a composer (FR-L15, FR-L16).
pub fn ComposerChips(session: &str) {
    let ui = shared_ui().clone();
    let sid = session.to_string();
    let model_menu = remember_with_key(sid.clone(), || mutable_state_of(false));
    let mode_menu = remember_with_key(sid.clone(), || mutable_state_of(false));

    let (info, approve_all) = {
        let _ = ui.live.launcher.get();
        services().client.read(|s| {
            (
                s.session(&sid)
                    .map(|v| (v.record.agent.clone(), v.record.model.clone())),
                s.approve_all(&sid),
            )
        })
    };
    let Some((agent, model)) = info else {
        return;
    };

    let model_text = model_chip_label(&agent, model.as_deref());
    let mode_text = mode_chip_label(approve_all);
    let warns = mode_chip_warns(approve_all);
    let rows = mode_rows(&agent);

    cu::Box().modifier(Modifier::new().fill_max_width()).content(|| {
        cu::Row()
            .alignment(Alignment::CenterStart)
            .space_role(SpaceRole::Xs)
            .modifier(Modifier::new().fill_max_width())
            .content(|| {
                let mm = model_menu.clone();
                PickChip(&model_text, Some(IconRole::Settings), false, false, move || {
                    mm.set(true);
                });
                let md = mode_menu.clone();
                PickChip(mode_text, None, warns, false, move || {
                    md.set(true);
                });
            });

        // Model info OptionSheet (read-only notice for running session)
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

        // Mode switcher OptionSheet
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

/// "IDE 열기" with the three-entry chooser (FR-L7).
pub fn IdeMenu(session: &str) {
    let ui = shared_ui().clone();
    let sid = session.to_string();
    let ide = remember_with_key(sid.clone(), || {
        mutable_state_of(None::<Result<IdeLaunch, String>>)
    });
    let open = remember_with_key(sid.clone(), || mutable_state_of(false));

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

    cu::Box().content(|| {
        let o = open.clone();
        AppButton(
            "IDE 열기",
            Some(IconRole::Forward),
            ButtonKind::Primary,
            true,
            true,
            false,
            move || o.set(true),
        );

        let labels: Vec<String> = choices.iter().map(|c| c.label.to_string()).collect();
        let o_d = open.clone();
        let choices_c = choices.clone();
        let ui_c = ui.clone();
        let launch_now = ide.get();
        OptionSheet(
            open.get(),
            &labels,
            Some(0),
            POPUP_W,
            move || o_d.set(false),
            move |idx| {
                if let Some(c) = choices_c.get(idx) {
                    ui_c.update_prefs(|p| p.ide_target = Some(c.kind.key().to_string()));
                    launch_ide_shared(&ui_c, c.kind, launch_now.clone());
                }
            },
        );
    });
}

/// The side list: search, then a section per project (FR-L18).
pub fn SideList(
    sections: &[Section],
    current: &str,
    active: Option<&str>,
    on_query: impl FnMut(String) + 'static,
    on_open: impl Fn(String, String) + Clone + 'static,
) {
    let p = ember_theme().palette;
    let cur_proj = current.to_string();
    let active_id = active.map(str::to_string);
    let sections_vec = sections.to_vec();

    cu::Column()
        .space_role(SpaceRole::Sm)
        .modifier(
            Modifier::new()
                .fill_max_width()
                .fill_max_height()
                .padding_role(SpaceRole::Sm),
        )
        .content(move || {
            EmberField(FieldOptions {
                fill_max_width: true,
                placeholder: "세션 검색".to_string(),
                on_value_change: Some(Box::new(on_query)),
                ..Default::default()
            });

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

                                cu::Column()
                                    .space_role(SpaceRole::Xs)
                                    .modifier(Modifier::new().fill_max_width())
                                    .content(move || {
                                        cu::Row()
                                            .alignment(Alignment::CenterStart)
                                            .space_role(SpaceRole::Sm)
                                            .modifier(Modifier::new().fill_max_width())
                                            .content(|| {
                                                Label(
                                                    &sec_name,
                                                    Fs::Sm,
                                                    Weight::Semibold,
                                                    Some(paint(if is_cur_proj {
                                                        p.text
                                                    } else {
                                                        p.text_2
                                                    })),
                                                    Some(1),
                                                );
                                                Pill(
                                                    &sec_members.len().to_string(),
                                                    PillTone::Grey,
                                                    true,
                                                );
                                            });

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
                                            let m_status = m.status;
                                            let s_proj = sec_proj.clone();
                                            let (pill_txt, pill_tone) = status_pill(m_status);

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
                                                        .space_role(SpaceRole::Xs)
                                                        .modifier(Modifier::new().fill_max_width())
                                                        .content(move || {
                                                            Character(
                                                                &m_agent,
                                                                m_leader,
                                                                24.0,
                                                                &m_name,
                                                                &s_proj,
                                                                CharState::from_status(m_status),
                                                                None,
                                                            );
                                                            cu::Column()
                                                                .space_role(SpaceRole::Xs)
                                                                .modifier(Modifier::new().weight(1.0))
                                                                .content(move || {
                                                                    Label(
                                                                        &m_acc,
                                                                        Fs::Sm,
                                                                        Weight::Medium,
                                                                        Some(paint(p.text)),
                                                                        Some(1),
                                                                    );
                                                                    Pill(
                                                                        pill_txt,
                                                                        pill_tone.into(),
                                                                        true,
                                                                    );
                                                                });
                                                        });
                                                },
                                            );
                                        }
                                    });
                            }
                        });
                });
        });
}

/// The transcript of one pane, with approvals (reuses `ConversationView`'s `TranscriptRow`).
pub fn PaneTranscript(session: &str, agent: &str) {
    let ui = shared_ui().clone();
    let p = ember_theme().palette;
    let sid = session.to_string();
    let agent_name = agent.to_string();
    let expanded = remember_with_key(sid.clone(), || mutable_state_of(HashSet::<String>::new()));
    let answering = remember_with_key(sid.clone(), || mutable_state_of(HashSet::<String>::new()));

    let (items, keys) = {
        let _ = ui.live.transcripts.get();
        services().client.read(|st| match st.transcript(&sid) {
            Some(t) => (
                t.items.clone(),
                t.items.iter().map(item_key).collect::<Vec<_>>(),
            ),
            None => (Vec::new(), Vec::new()),
        })
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
                let items_c = items.clone();
                let keys_c = keys.clone();
                let sid_c = sid.clone();
                let agent_c = agent_name.clone();
                let exp_c = expanded.clone();
                let ans_c = answering.clone();
                cu::LazyColumn()
                    .modifier(Modifier::new().fill_max_width().fill_max_height())
                    .content(move |scope| {
                        scope.items_keyed(
                            count,
                            move |i| keys_c[i].clone(),
                            move |i| {
                                TranscriptRow(&sid_c, &agent_c, &items_c[i], &exp_c, &ans_c);
                            },
                        );
                    });
            }
        });
}

/// A pane's own composer: Enter sends, a message while the agent is busy is queued (FR-L6).
pub fn PaneComposer(session: &str, busy: bool) {
    let ui = shared_ui().clone();
    let p = ember_theme().palette;
    let sid = session.to_string();
    let draft = remember_with_key(sid.clone(), || mutable_state_of(String::new()));
    let generation = remember_with_key(sid.clone(), || mutable_state_of(0_u64));

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
        .space_role(SpaceRole::Xs)
        .modifier(
            Modifier::new()
                .fill_max_width()
                .padding_role(SpaceRole::Xs),
        )
        .content(move || {
            if queued > 0 {
                Pill(&format!("{queued}개 대기 중입니다"), PillTone::Blue, true);
            }
            ComposerChips(&sid);
            cu::Row()
                .alignment(Alignment::CenterStart)
                .space_role(SpaceRole::Xs)
                .modifier(
                    Modifier::new()
                        .fill_max_width()
                        .background(paint(p.surface))
                        .border(1.0, paint(p.line))
                        .corner_radius(CORNER_XL)
                        .padding_role(SpaceRole::Xs),
                )
                .content(|| {
                    let draft_ch = draft.clone();
                    let send_sub = send.clone();
                    EmberField(FieldOptions {
                        bare: true,
                        weight: Some(1.0),
                        multiline: true,
                        placeholder: if busy {
                            "다음 턴에 보낼 메시지를 입력하세요".to_string()
                        } else {
                            "메시지를 입력하세요".to_string()
                        },
                        on_value_change: Some(Box::new(move |v| draft_ch.set(v))),
                        on_submit: Some(Box::new(move |v| send_sub(v))),
                        ..Default::default()
                    });
                    if busy {
                        let sid_int = sid.clone();
                        AppButton(
                            "중단",
                            Some(IconRole::Close),
                            ButtonKind::Default,
                            true,
                            true,
                            false,
                            move || {
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
                            },
                        );
                    }
                    let send_btn = send.clone();
                    let draft_btn = draft.clone();
                    IconButton(IconRole::Send, true, move || {
                        send_btn(draft_btn.get_untracked());
                    });
                });
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
    let p = ember_theme().palette;
    let id = member.session_id.clone();
    let proj_owned = project.to_string();
    let member_owned = member.clone();
    let all_owned = all.to_vec();
    let order_owned = order.to_vec();

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
                        .padding_role(SpaceRole::Sm),
                )
                .content(|| {
                    // Header row
                    cu::Row()
                        .alignment(Alignment::CenterStart)
                        .space_role(SpaceRole::Xs)
                        .modifier(Modifier::new().fill_max_width())
                        .content(|| {
                            Character(
                                &member_owned.agent,
                                member_owned.leader,
                                28.0,
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
                                    cu::Row()
                                        .alignment(Alignment::CenterStart)
                                        .space_role(SpaceRole::Xs)
                                        .modifier(Modifier::new().fill_max_width())
                                        .content(|| {
                                            Pill(label, tone.into(), true);
                                            Label(
                                                &member_owned.task,
                                                Fs::Xs,
                                                Weight::Regular,
                                                Some(paint(p.text_2)),
                                                Some(1),
                                            );
                                        });
                                });
                            let enl = on_enlarge.clone();
                            let id_enl = id.clone();
                            IconButton(IconRole::Forward, true, move || enl(id_enl.clone()));

                            let m_open = menu.clone();
                            IconButton(IconRole::More, true, move || m_open.set(true));

                            let ui_close = ui.clone();
                            let proj_close = proj_owned.clone();
                            let all_close = all_owned.clone();
                            let id_close = id.clone();
                            IconButton(IconRole::Close, true, move || {
                                let (all_c, id_c) = (all_close.clone(), id_close.clone());
                                edit_panes(&ui_close, &proj_close, move |pp| {
                                    close_pane(pp.active_tab_mut(), &all_c, &id_c);
                                });
                            });
                        });

                    // Conversation / Terminal toggle chips
                    cu::Row()
                        .space_role(SpaceRole::Xs)
                        .modifier(Modifier::new().fill_max_width())
                        .content(|| {
                            for t in PaneToggle::ALL {
                                let tg = toggle.clone();
                                PickChip(t.label(), None, cur_toggle == t, false, move || {
                                    tg.set(t);
                                });
                            }
                        });

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
    let layout = pane_layout(window.class);
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
                    // Tab strip
                    cu::ScrollRow()
                        .modifier(Modifier::new().fill_max_width())
                        .content(|| {
                            cu::Row()
                                .alignment(Alignment::CenterStart)
                                .space_role(SpaceRole::Xs)
                                .content(|| {
                                    for (i, t) in panes.tabs.iter().enumerate() {
                                        let pg = page.clone();
                                        let u = ui.clone();
                                        let pr = proj_owned.clone();
                                        PickChip(
                                            &t.name,
                                            None,
                                            i == panes.active,
                                            false,
                                            move || {
                                                pg.set(0);
                                                edit_panes(&u, &pr, move |pp| pp.select(i));
                                            },
                                        );
                                    }
                                    {
                                        let pg = page.clone();
                                        let u = ui.clone();
                                        let pr = proj_owned.clone();
                                        let vis_add = vis.clone();
                                        PickChip(
                                            "새 탭",
                                            Some(IconRole::Add),
                                            false,
                                            false,
                                            move || {
                                                pg.set(0);
                                                let v = vis_add.clone();
                                                edit_panes(&u, &pr, move |pp| pp.add_tab(&v));
                                            },
                                        );
                                    }
                                    if panes.tabs.len() > 1 {
                                        let pg = page.clone();
                                        let u = ui.clone();
                                        let pr = proj_owned.clone();
                                        let act = panes.active;
                                        PickChip(
                                            "탭 닫기",
                                            Some(IconRole::Close),
                                            false,
                                            false,
                                            move || {
                                                pg.set(0);
                                                edit_panes(&u, &pr, move |pp| pp.remove_tab(act));
                                            },
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

                    if n == 0 {
                        cu::Box()
                            .alignment(Alignment::Center)
                            .modifier(Modifier::new().fill_max_width().weight(1.0))
                            .content(|| {
                                cu::Column()
                                    .alignment(Alignment::Center)
                                    .space_role(SpaceRole::Sm)
                                    .content(|| {
                                        Label(NO_MEMBERS, Fs::Lg, Weight::Medium, None, None);
                                        Label(
                                            PICK_FROM_LIST,
                                            Fs::Md,
                                            Weight::Regular,
                                            Some(paint(p.muted)),
                                            None,
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
                                                                        &pr_p, &m, &al_p, &vs_p,
                                                                        enl_p,
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

/// The right panel of the single view (environment, context, outputs, changes).
pub fn InfoPanel(session: &str) {
    let ui = shared_ui().clone();
    let p = ember_theme().palette;
    let sid = session.to_string();
    let computer = remember_with_key(sid.clone(), || mutable_state_of(None::<String>));

    {
        let sid_load = sid.clone();
        let comp_load = computer.clone();
        launched_effect(sid.clone(), move || async move {
            if services().demo.is_some() {
                comp_load.set(Some("mac-studio".to_string()));
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

    cu::ScrollColumn()
        .modifier(
            Modifier::new()
                .fill_max_width()
                .fill_max_height()
                .padding_role(SpaceRole::Sm),
        )
        .content(move || {
            cu::Column()
                .space_role(SpaceRole::Sm)
                .modifier(Modifier::new().fill_max_width())
                .content(move || {
                    for card in InfoCard::ALL {
                        let rows_c = rows.clone();
                        EmberCard(false, true, None, None, None, move || {
                            cu::Column()
                                .space_role(SpaceRole::Xs)
                                .modifier(Modifier::new().fill_max_width())
                                .content(move || {
                                    Label(card.label(), Fs::Md, Weight::Semibold, None, Some(1));
                                    if card.ready() {
                                        for (k, v) in rows_c.iter() {
                                            cu::Row()
                                                .space_role(SpaceRole::Sm)
                                                .modifier(Modifier::new().fill_max_width())
                                                .content(|| {
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
                                                                Some(2),
                                                            );
                                                        });
                                                });
                                        }
                                    } else {
                                        Label(
                                            SOON,
                                            Fs::Sm,
                                            Weight::Regular,
                                            Some(paint(p.muted)),
                                            None,
                                        );
                                    }
                                });
                        });
                    }
                });
        });
}

/// One member's conversation with the right panel (inline on expanded, a sheet elsewhere).
pub fn SingleView(session: &str, focus: Option<String>) {
    let window = current_window_size();
    let sid = session.to_string();
    let env_info = std::env::var("EMBER_CR_SESSION_INFO").ok();
    let info_open = remember_with_key(sid.clone(), || {
        mutable_state_of(env_info.as_deref() == Some("1"))
    });
    use_leases_cr(std::slice::from_ref(&sid), focus.as_deref());
    let wide = window.class == compose_rust::WindowSizeClass::Expanded
        && env_info.as_deref() != Some("0");

    cu::Box()
        .modifier(Modifier::new().fill_max_width().fill_max_height())
        .content(move || {
            let sid_row = sid.clone();
            let sid_p = sid.clone();
            let sid_sh = sid.clone();
            let info_btn = info_open.clone();
            cu::Row()
                .modifier(Modifier::new().fill_max_width().fill_max_height())
                .content(move || {
                    cu::Column()
                        .modifier(Modifier::new().weight(1.0).fill_max_height())
                        .content(move || {
                            if !wide {
                                cu::Row()
                                    .alignment(Alignment::CenterEnd)
                                    .modifier(
                                        Modifier::new()
                                            .fill_max_width()
                                            .padding_role(SpaceRole::Sm),
                                    )
                                    .content(move || {
                                        let io = info_btn.clone();
                                        AppButton(
                                            "정보",
                                            Some(IconRole::Sidebar),
                                            ButtonKind::Default,
                                            true,
                                            true,
                                            false,
                                            move || io.set(true),
                                        );
                                    });
                            }
                            ConversationView(&sid_row, true);
                        });
                    if wide {
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
                    "정보",
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

/// The kanban of member status, with the PR list and timeline not built yet.
pub fn StatusView(
    project: &str,
    members: &[Member],
    on_open: impl Fn(String) + Clone + 'static,
) {
    let p = ember_theme().palette;
    let proj_owned = project.to_string();
    let members_vec = members.to_vec();
    let columns = kanban(&members_vec);
    let count = |s: LauncherStatus| members_vec.iter().filter(|m| m.status == s).count();
    let total_cnt = members_vec.len();
    let run_cnt = count(LauncherStatus::Running);
    let wait_cnt = count(LauncherStatus::WaitingForApproval);
    let fail_cnt = count(LauncherStatus::Failed);

    cu::ScrollColumn()
        .modifier(Modifier::new().fill_max_width().fill_max_height())
        .content(move || {
            cu::Column()
                .space_role(SpaceRole::Md)
                .modifier(
                    Modifier::new()
                        .fill_max_width()
                        .padding_role(SpaceRole::Sm),
                )
                .content(move || {
                    // Top status summary pills
                    cu::ScrollRow()
                        .modifier(Modifier::new().fill_max_width())
                        .content(move || {
                            cu::Row()
                                .alignment(Alignment::CenterStart)
                                .space_role(SpaceRole::Xs)
                                .content(move || {
                                    Pill(&format!("팀원 {total_cnt}"), PillTone::Grey, false);
                                    Pill(&format!("실행 중 {run_cnt}"), PillTone::Blue, false);
                                    Pill(&format!("승인 대기 {wait_cnt}"), PillTone::Orange, false);
                                    Pill(&format!("실패 {fail_cnt}"), PillTone::Red, false);
                                });
                        });

                    // 5-column Kanban
                    let cols_c = columns.clone();
                    let proj_k = proj_owned.clone();
                    let open_k = on_open.clone();
                    cu::ScrollRow()
                        .modifier(Modifier::new().fill_max_width())
                        .content(move || {
                            cu::Row()
                                .alignment(Alignment::TopStart)
                                .space_role(SpaceRole::Sm)
                                .content(move || {
                                    for (col, ms) in cols_c.iter() {
                                        let col_lbl = col.label();
                                        let ms_c = ms.clone();
                                        let pr_c = proj_k.clone();
                                        let op_c = open_k.clone();
                                        cu::Column()
                                            .space_role(SpaceRole::Xs)
                                            .modifier(
                                                Modifier::new()
                                                    .width(240.0)
                                                    .background(paint(p.surface_2))
                                                    .border(1.0, paint(p.line))
                                                    .corner_radius(CORNER_LG)
                                                    .padding(SP[2]),
                                            )
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
                                                        cu::Spacer()
                                                            .modifier(Modifier::new().weight(1.0));
                                                        Pill(
                                                            &ms_c.len().to_string(),
                                                            PillTone::Grey,
                                                            true,
                                                        );
                                                    });
                                                if ms_c.is_empty() {
                                                    Label(
                                                        "없음",
                                                        Fs::Xs,
                                                        Weight::Regular,
                                                        Some(paint(p.muted)),
                                                        Some(1),
                                                    );
                                                }
                                                for m in ms_c.iter() {
                                                    let m_id = m.session_id.clone();
                                                    let m_agent = m.agent.clone();
                                                    let m_leader = m.leader;
                                                    let m_name = m.name.clone();
                                                    let m_acc = m.accessible_name();
                                                    let m_task = m.task.clone();
                                                    let m_status = m.status;
                                                    let pr_card = pr_c.clone();
                                                    let op_card = op_c.clone();
                                                    EmberCard(
                                                        false,
                                                        true,
                                                        None,
                                                        Some(SP[2]),
                                                        None,
                                                        move || {
                                                            let op_click = op_card.clone();
                                                            let id_click = m_id.clone();
                                                            Clickable(
                                                                Modifier::new().fill_max_width(),
                                                                Alignment::CenterStart,
                                                                move || op_click(id_click.clone()),
                                                                move || {
                                                                    cu::Row()
                                                                        .alignment(
                                                                            Alignment::CenterStart,
                                                                        )
                                                                        .space_role(SpaceRole::Xs)
                                                                        .modifier(
                                                                            Modifier::new()
                                                                                .fill_max_width(),
                                                                        )
                                                                        .content(move || {
                                                                            Character(
                                                                                &m_agent,
                                                                                m_leader,
                                                                                28.0,
                                                                                &m_name,
                                                                                &pr_card,
                                                                                CharState::from_status(
                                                                                    m_status,
                                                                                ),
                                                                                None,
                                                                            );
                                                                            cu::Column()
                                                                                .space_role(
                                                                                    SpaceRole::Xs,
                                                                                )
                                                                                .modifier(
                                                                                    Modifier::new()
                                                                                        .weight(
                                                                                            1.0,
                                                                                        ),
                                                                                )
                                                                                .content(
                                                                                    move || {
                                                                                        Label(
                                                                                            &m_acc,
                                                                                            Fs::Sm,
                                                                                            Weight::Semibold,
                                                                                            Some(paint(p.text)),
                                                                                            Some(2),
                                                                                        );
                                                                                        Label(
                                                                                            &m_task,
                                                                                            Fs::Xs,
                                                                                            Weight::Regular,
                                                                                            Some(paint(p.text_2)),
                                                                                            Some(2),
                                                                                        );
                                                                                    },
                                                                                );
                                                                        });
                                                                },
                                                            );
                                                        },
                                                    );
                                                }
                                            });
                                    }
                                });
                        });

                    // PR & CI placeholder card
                    EmberCard(false, true, None, None, None, || {
                        cu::Column()
                            .space_role(SpaceRole::Xs)
                            .modifier(Modifier::new().fill_max_width())
                            .content(|| {
                                Label(PR_TITLE, Fs::Md, Weight::Semibold, None, Some(1));
                                Label(
                                    SOON,
                                    Fs::Sm,
                                    Weight::Regular,
                                    Some(paint(p.muted)),
                                    None,
                                );
                            });
                    });

                    // Agent communication timeline placeholder card
                    EmberCard(false, true, None, None, None, || {
                        cu::Column()
                            .space_role(SpaceRole::Xs)
                            .modifier(Modifier::new().fill_max_width())
                            .content(|| {
                                Label(TIMELINE_TITLE, Fs::Md, Weight::Semibold, None, Some(1));
                                Label(
                                    SOON,
                                    Fs::Sm,
                                    Weight::Regular,
                                    Some(paint(p.muted)),
                                    None,
                                );
                            });
                    });
                });
        });
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
    let project_menu = remember(|| mutable_state_of(false));
    let more_menu = remember(|| mutable_state_of(false));

    let boxes = {
        let _ = ui.live.launcher.get();
        services().client.read(project_boxes)
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
                        Label(NO_PROJECT, Fs::Xl, Weight::Medium, None, None);
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
    let mode = side_list_mode(window.class);
    let chips_below = view_chips_below(window.width_dp);
    let fold_level = toolbar_fold_level(toolbar_width(
        window.width_dp,
        mode == SideListMode::Column,
    ));

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

    let project_name = display_name(&current);
    let view_now = view.get();

    cu::Box()
        .modifier(Modifier::new().fill_max_width().fill_max_height())
        .content(move || {
            let sections_col = sections.clone();
            let current_col = current.clone();
            let target_col = target.clone();
            let query_col = query.clone();
            let open_col = open_member.clone();

            cu::Row()
                .modifier(Modifier::new().fill_max_width().fill_max_height())
                .content(|| {
                    if mode == SideListMode::Column {
                        cu::Column()
                            .modifier(
                                Modifier::new()
                                    .width(SIDE_LIST_W)
                                    .fill_max_height()
                                    .background(paint(p.surface)),
                            )
                            .content(move || {
                                let q_st = query_col.clone();
                                SideList(
                                    &sections_col,
                                    &current_col,
                                    target_col.as_deref(),
                                    move |q| q_st.set(q),
                                    open_col.clone(),
                                );
                            });
                        cu::Spacer()
                            .modifier(Modifier::new().width(1.0).fill_max_height().background(paint(p.line)));
                    }

                    cu::Column()
                        .modifier(Modifier::new().weight(1.0).fill_max_height())
                        .content(|| {
                            // Top toolbar
                            cu::ScrollRow()
                                .modifier(
                                    Modifier::new()
                                        .fill_max_width()
                                        .padding_role(SpaceRole::Sm),
                                )
                                .content(|| {
                                    cu::Row()
                                        .alignment(Alignment::CenterStart)
                                        .space_role(SpaceRole::Xs)
                                        .content(|| {
                                            if mode == SideListMode::Drawer {
                                                let dr = drawer.clone();
                                                AppButton(
                                                    "목록",
                                                    Some(IconRole::List),
                                                    ButtonKind::Default,
                                                    true,
                                                    true,
                                                    false,
                                                    move || dr.set(true),
                                                );
                                            }
                                            let pm = project_menu.clone();
                                            AppButton(
                                                &project_name,
                                                Some(IconRole::Library),
                                                ButtonKind::Default,
                                                true,
                                                true,
                                                false,
                                                move || pm.set(true),
                                            );
                                            if !chips_below {
                                                for v in SessionView::ALL {
                                                    let sv = set_view.clone();
                                                    FilterChip(
                                                        v.label(),
                                                        view_now == v,
                                                        move || sv(v),
                                                    );
                                                }
                                            }
                                            if fold_level < 3 {
                                                let u = ui.clone();
                                                let c = current.clone();
                                                AppButton(
                                                    "새 팀원",
                                                    Some(IconRole::Add),
                                                    ButtonKind::Default,
                                                    true,
                                                    true,
                                                    false,
                                                    move || u.open_new_session(Some(c.clone())),
                                                );
                                            }
                                            if fold_level < 2 {
                                                let u = ui.clone();
                                                let c = current.clone();
                                                AppButton(
                                                    "버전 관리",
                                                    None,
                                                    ButtonKind::Default,
                                                    true,
                                                    true,
                                                    false,
                                                    move || {
                                                        u.set_selected_project(Some(c.clone()));
                                                        u.set_menu(NavMenu::VersionControl);
                                                    },
                                                );
                                            }
                                            if fold_level < 1 {
                                                AppButton(
                                                    "웹 페이지",
                                                    None,
                                                    ButtonKind::Default,
                                                    true,
                                                    true,
                                                    false,
                                                    move || notice::post(WEB_SOON),
                                                );
                                            }
                                            if fold_level > 0 {
                                                let mm = more_menu.clone();
                                                AppButton(
                                                    "⋯",
                                                    None,
                                                    ButtonKind::Default,
                                                    true,
                                                    true,
                                                    false,
                                                    move || mm.set(true),
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
                                        });
                                });

                            if chips_below {
                                cu::ScrollRow()
                                    .modifier(
                                        Modifier::new()
                                            .fill_max_width()
                                            .padding_role(SpaceRole::Sm),
                                    )
                                    .content(|| {
                                        cu::Row()
                                            .alignment(Alignment::CenterStart)
                                            .space_role(SpaceRole::Xs)
                                            .content(|| {
                                                for v in SessionView::ALL {
                                                    let sv = set_view.clone();
                                                    FilterChip(
                                                        v.label(),
                                                        view_now == v,
                                                        move || sv(v),
                                                    );
                                                }
                                            });
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
                                        Some(t) => SingleView(&t, focus.clone()),
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

            // Drawer sheet on compact/medium
            if mode == SideListMode::Drawer {
                let dr_d = drawer.clone();
                let sec_d = sections.clone();
                let cur_d = current.clone();
                let tgt_d = target.clone();
                let q_d = query.clone();
                let op_d = open_member.clone();
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
                                    move |q| q_st.set(q),
                                    op_d.clone(),
                                );
                            });
                    },
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

            // Folded toolbar actions OptionSheet
            if fold_level > 0 {
                let mut more_opts = Vec::new();
                let mut more_kinds = Vec::new();
                if fold_level >= 3 {
                    more_opts.push("새 팀원".to_string());
                    more_kinds.push(0usize);
                }
                if fold_level >= 2 {
                    more_opts.push("버전 관리".to_string());
                    more_kinds.push(1usize);
                }
                more_opts.push("웹 페이지".to_string());
                more_kinds.push(2usize);

                let mm_d = more_menu.clone();
                let ui_m = ui.clone();
                let cur_m = current.clone();
                OptionSheet(
                    more_menu.get(),
                    &more_opts,
                    None,
                    POPUP_W,
                    move || mm_d.set(false),
                    move |idx| match more_kinds.get(idx).copied() {
                        Some(0) => ui_m.open_new_session(Some(cur_m.clone())),
                        Some(1) => {
                            ui_m.set_selected_project(Some(cur_m.clone()));
                            ui_m.set_menu(NavMenu::VersionControl);
                        }
                        Some(2) => notice::post(WEB_SOON),
                        _ => {}
                    },
                );
            }
        });
}

#[cfg(test)]
mod tests {
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
        for want in ["멀티", "싱글", "상태", "탭 1", "새 탭", "IDE 열기"] {
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
