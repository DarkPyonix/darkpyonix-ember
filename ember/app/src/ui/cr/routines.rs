//! compose-rust version of the Routines screen (#220 phase 7; FR-L25, `docs/DESIGN.md` section
//! 6.5): a dashboard on top (KPIs, success ring, weekday bars), routines grouped by project
//! below it, a detail panel, and the routine create/edit form.
//!
//! Pure decisions (cron parser, civil calendar, grouping, validation, schedule API mapping)
//! stay in [`crate::ui::routines`] and are re-exported. Only the drawing and orchestration
//! are rewritten here with compose-rust primitives (`MutableState`, `cu::*`, `EmberCard`).

#![allow(non_snake_case)]

use std::future::Future;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use compose_rust::runtime::{
    current_window_size, launched_effect, mutable_state_of, remember, MutableState,
};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{Alignment, ButtonVariant, Color, DrawList, IconRole, Paint, ShapeRole};

use ember_client::api::ApiError;

pub use crate::ui::routines::{
    ago, big_time, changes_frozen_fields, civil_from_days, cron_valid, days_from_civil,
    describe_cron, filter_projects, format_next, group_by_project, kind_text, kpis,
    last_run_text, minutes_at, ms_to_minutes, name_from_prompt, new_schedule, next_run,
    overall_success, pane_mode, run_from_server, schedule_line, schedule_patch,
    schedule_to_entry, schedule_to_routine, success_percent, weekday, weekday_bars, DayBar,
    Entry, FormDraft, FormError, Group, LocalStore, Minutes, PaneMode, Routine, RoutineKpi,
    RoutineStore, Run, RunStatus, StoreError, Trigger, LOCAL_NOTE, LOCAL_OFFSET_MIN, OFFLINE_NOTE,
};
use crate::ui::home::{
    bar_rects, computer_chip_text, default_mode, default_option, filter_models, harness_block,
    model_catalog, modes_for, project_options, DEFAULT_PROJECT, Harness, Selection,
};
use super::components::{
    pill_colors, pill_metrics, AppButton, ButtonKind, EmberCard, EmberField, EmberSheet, InsetPanel,
    FieldOptions, Label, PickChip, Pill, PillTone, SheetFooter,
};
use super::project::PHONE_MENU_ROOM;
use super::shell::NavIcon;
use super::theme::*;
use crate::services::{run_shared, services, try_services};
use crate::ui::cr::notice;

// ---------------------------------------------------------------------------------------------
// Orchestration helpers
// ---------------------------------------------------------------------------------------------

const RUN_NOTICE: &str = "서버에 연결되지 않아 지금 실행할 수 없습니다";
const NO_CWD_NOTICE: &str =
    "이 프로젝트의 작업 폴더를 알 수 없습니다. 프로젝트에서 세션을 한 번 시작하셔야 합니다";
const FROZEN_NOTICE: &str =
    "프로젝트, 에이전트, 컴퓨터는 만든 뒤에 바꿀 수 없어 프롬프트와 일정만 저장했습니다";
const RUNS_SHOWN: usize = 20;

fn store() -> &'static Mutex<LocalStore> {
    static STORE: OnceLock<Mutex<LocalStore>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(LocalStore::default()))
}

/// Runs `f` on the device-local store.
pub fn with_store<R>(f: impl FnOnce(&mut LocalStore) -> R) -> R {
    let mut guard = store().lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

/// What the server last answered: `online` is false until a load succeeds.
#[derive(Default)]
pub struct Remote {
    pub online: bool,
    pub entries: Vec<Entry>,
}

fn remote() -> &'static Mutex<Remote> {
    static REMOTE: OnceLock<Mutex<Remote>> = OnceLock::new();
    REMOTE.get_or_init(|| Mutex::new(Remote::default()))
}

pub fn with_remote<R>(f: impl FnOnce(&mut Remote) -> R) -> R {
    let mut guard = remote().lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

/// Whether `id` is a server schedule (else it is on this device).
pub fn is_remote(id: &str) -> bool {
    with_remote(|r| r.online && r.entries.iter().any(|e| e.routine.id == id))
}

/// The server's schedules with their recent runs.
pub async fn load_remote() -> Result<Vec<Entry>, ApiError> {
    let api = services().client.api();
    let mut out = Vec::new();
    for s in api.schedules(None).await? {
        let runs = api.schedule_runs(&s.id, Some(RUNS_SHOWN)).await?;
        out.push(schedule_to_entry(&s, &runs));
    }
    Ok(out)
}

fn unreachable_err(e: &ApiError) -> bool {
    e.status().is_none()
}

pub fn apply_load(r: Result<Vec<Entry>, ApiError>) {
    with_remote(|st| match r {
        Ok(entries) => {
            st.online = true;
            st.entries = entries;
        }
        Err(e) => {
            tracing::warn!("loading routines failed: {e}");
            if unreachable_err(&e) {
                st.online = false;
                st.entries.clear();
            }
        }
    });
}

fn refresh(tick: MutableState<u32>) {
    if try_services().is_none() {
        return;
    }
    run_shared(
        async move { load_remote().await },
        move |r| {
            apply_load(r);
            tick.update(|v| *v += 1);
        },
    );
}

fn remote_action<F>(tick: MutableState<u32>, work: F)
where
    F: Future<Output = Result<Option<String>, ApiError>> + Send + 'static,
{
    if try_services().is_none() {
        return;
    }
    run_shared(
        async move {
            let done = work.await;
            (done, load_remote().await)
        },
        move |(done, list)| {
            apply_load(list);
            match done {
                Ok(Some(text)) => notice::post(text),
                Ok(None) => {}
                Err(e) => notice::post(format!("서버가 요청을 처리하지 못했습니다: {e}")),
            }
            tick.update(|v| *v += 1);
        },
    );
}

/// Local minutes now.
pub fn now_local() -> Minutes {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    secs / 60 + LOCAL_OFFSET_MIN
}

/// `(id, name)` of registered computers and existing project names.
fn live_choices() -> (Vec<(String, String)>, Vec<String>) {
    let Some(svc) = try_services() else {
        return (Vec::new(), Vec::new());
    };
    let _ = svc.ui.live.launcher.get();
    svc.client.read(|s| {
        let computers = s
            .computers()
            .iter()
            .filter(|c| c.id != "local")
            .map(|c| (c.id.clone(), c.name.clone()))
            .collect();
        let projects = crate::model::project_cards(s)
            .into_iter()
            .map(|c| c.name)
            .collect();
        (computers, projects)
    })
}

fn live_agents() -> Vec<(String, bool)> {
    let Some(svc) = try_services() else {
        return Vec::new();
    };
    svc.ui
        .live
        .agents
        .get()
        .iter()
        .map(|a| (a.kind.clone(), a.installed))
        .collect()
}

fn status_tone(s: RunStatus) -> PillTone {
    match s {
        RunStatus::Done => PillTone::Green,
        RunStatus::Failed => PillTone::Red,
        RunStatus::Running => PillTone::Blue,
        RunStatus::Skipped | RunStatus::Missed => PillTone::Orange,
    }
}

fn detail_state() -> Option<MutableState<bool>> {
    try_services().map(|s| s.ui.detail_open.clone())
}

/// The list panel's width: 400 above 1279 dp, 320 up to 1279, 300 up to 900.
pub fn list_pane_width(width_dp: f32) -> f32 {
    if width_dp > 1279.0 {
        400.0
    } else if width_dp > 900.0 {
        320.0
    } else {
        300.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Panel {
    #[default]
    View,
    Edit(String),
    New,
}

// ---------------------------------------------------------------------------------------------
// Public screen entry points
// ---------------------------------------------------------------------------------------------

/// 루틴 (Routines) screen on compose-rust (FR-L25).
pub fn RoutinesScreen() {
    let tick = remember(|| mutable_state_of(0_u32));
    let is_demo = std::env::var("EMBER_CR_ROUTE").as_deref() == Ok("routines");
    if !is_demo {
        let tick_refresh = tick.clone();
        launched_effect((), move || async move {
            refresh(tick_refresh);
        });
    }
    let _ = tick.get();
    let (entries, online) = if is_demo {
        (sample_entries(), true)
    } else {
        let (online, mut entries) = with_remote(|r| (r.online, r.entries.clone()));
        let device = with_store(|s| s.entries());
        entries.extend(device);
        (entries, online)
    };
    RoutinesScreenWith(entries, online);
}

fn project_rank(p: &str) -> usize {
    match p {
        "ember" => 0,
        "core" => 1,
        "pyx" => 2,
        "site" => 3,
        DEFAULT_PROJECT => 4,
        _ => 5,
    }
}

fn ordered_filter_projects(all: &[Routine]) -> Vec<String> {
    let mut projects: Vec<String> = vec![
        "ember".into(),
        "core".into(),
        "pyx".into(),
        "site".into(),
        DEFAULT_PROJECT.into(),
    ];
    for r in all {
        if !projects.contains(&r.sel.project) {
            projects.push(r.sel.project.clone());
        }
    }
    projects.sort_by_key(|p| project_rank(p));
    projects
}

/// Routines screen with explicit entries, for review captures and previews.
pub fn RoutinesScreenWith(entries: Vec<Entry>, online: bool) {
    let theme = ember_theme();
    let p = theme.palette;
    let window = current_window_size();
    let compact = window.is_compact();

    let tick = remember(|| mutable_state_of(0_u32));
    let selected = remember(|| {
        let preset = std::env::var("EMBER_CR_ROUTINE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .and_then(|v| {
                v.parse::<usize>()
                    .ok()
                    .and_then(|i| entries.get(i))
                    .map(|e| e.routine.id.clone())
                    .or(Some(v))
            });
        mutable_state_of(preset)
    });
    let panel = remember(|| mutable_state_of(Panel::View));
    let filter = remember(|| mutable_state_of(None::<String>));
    let query = remember(|| mutable_state_of(String::new()));
    let entries_state = remember(|| mutable_state_of(entries));
    let online_state = remember(|| mutable_state_of(online));

    let _ = tick.get();
    let cur_entries = entries_state.get();
    let cur_online = online_state.get();
    let device_only = with_store(|s| !s.entries().is_empty());
    let now = now_local();
    let (computers, existing_projects) = live_choices();

    let routines: Vec<Routine> = cur_entries.iter().map(|e| e.routine.clone()).collect();
    let mut groups = group_by_project(&routines, filter.get().as_deref(), &query.get());
    groups.sort_by_key(|g| project_rank(&g.project));
    let pills = ordered_filter_projects(&routines);
    let cur_sel = selected.get();
    let current = cur_sel.as_ref().filter(|id| cur_entries.iter().any(|e| e.routine.id == **id));
    let shown = if compact {
        current.cloned()
    } else {
        current.cloned().or_else(|| {
            groups
                .first()
                .and_then(|g| g.routines.first())
                .map(|r| r.id.clone())
        })
    };
    let detail = shown
        .as_ref()
        .and_then(|id| cur_entries.iter().find(|e| e.routine.id == *id))
        .cloned();
    let mode = pane_mode(compact, current.is_some());
    if let Some(d) = detail_state() {
        d.set(compact && current.is_some());
    }
    let pad = if compact { SP[3] } else { SP[6] };

    let on_toggle = {
        let entries_st = entries_state.clone();
        let tick_ref = tick.clone();
        move |id: String, on: bool| {
            if is_remote(&id) {
                remote_action(tick_ref.clone(), async move {
                    let api = services().client.api();
                    if on {
                        api.resume_schedule(&id).await?;
                    } else {
                        api.pause_schedule(&id).await?;
                    }
                    Ok(None)
                });
            } else {
                let _ = with_store(|s| s.set_enabled(&id, on));
                entries_st.update(|list| {
                    if let Some(e) = list.iter_mut().find(|e| e.routine.id == id) {
                        e.routine.enabled = on;
                    }
                });
                tick_ref.update(|v| *v += 1);
            }
        }
    };

    let on_run_now = {
        let tick_ref = tick.clone();
        move |id: String| {
            if is_remote(&id) {
                remote_action(tick_ref.clone(), async move {
                    let run = services().client.api().run_schedule(&id).await?;
                    Ok(run.error.map(|e| format!("실행하지 못했습니다: {e}")))
                });
                return;
            }
            match with_store(|s| s.run_now(&id)) {
                Err(StoreError::Unavailable) => notice::post(RUN_NOTICE),
                Err(StoreError::NotFound) => notice::post("루틴을 찾을 수 없습니다"),
                Ok(()) => {}
            }
        }
    };

    let on_save = {
        let entries_st = entries_state.clone();
        let online_st = online_state.clone();
        let tick_ref = tick.clone();
        let panel_ref = panel.clone();
        let selected_ref = selected.clone();
        move |(draft, editing): (FormDraft, Option<String>)| {
            if let Some(id) = editing.clone().filter(|id| is_remote(id)) {
                let old = with_remote(|r| {
                    r.entries
                        .iter()
                        .find(|e| e.routine.id == id)
                        .map(|e| e.routine.clone())
                });
                let frozen = old.is_some_and(|o| changes_frozen_fields(&draft, &o));
                let patch = schedule_patch(&draft);
                remote_action(tick_ref.clone(), async move {
                    services().client.api().update_schedule(&id, &patch).await?;
                    Ok(frozen.then(|| FROZEN_NOTICE.to_string()))
                });
                panel_ref.set(Panel::View);
                return;
            }
            if editing.is_none() && draft.trigger == Trigger::Time && with_remote(|r| r.online) {
                let cwd = try_services().and_then(|svc| {
                    svc.client.read(|s| crate::model::latest_cwd(s, &draft.sel.project))
                });
                let Some(cwd) = cwd else {
                    notice::post(NO_CWD_NOTICE);
                    return;
                };
                let body = new_schedule(&draft, &cwd);
                let tick_spawn = tick_ref.clone();
                let sel_spawn = selected_ref.clone();
                let ent_spawn = entries_st.clone();
                let onl_spawn = online_st.clone();
                run_shared(
                    async move {
                        let made = services().client.api().create_schedule(&body).await;
                        (made, load_remote().await)
                    },
                    move |(made, list)| {
                        apply_load(list);
                        match made {
                            Ok(s) => {
                                sel_spawn.set(Some(s.id));
                            }
                            Err(e) if unreachable_err(&e) => {
                                let id = with_store(|st| st.next_id());
                                with_store(|st| st.save(draft.into_routine(&id, true)));
                                sel_spawn.set(Some(id));
                                notice::post(OFFLINE_NOTE);
                            }
                            Err(e) => notice::post(format!("서버가 루틴을 만들지 못했습니다: {e}")),
                        }
                        let (on, mut remote_entries) = with_remote(|r| (r.online, r.entries.clone()));
                        remote_entries.extend(with_store(|s| s.entries()));
                        onl_spawn.set(on);
                        ent_spawn.set(remote_entries);
                        tick_spawn.update(|v| *v += 1);
                    },
                );
                panel_ref.set(Panel::View);
                return;
            }
            let id = editing
                .clone()
                .unwrap_or_else(|| with_store(|s| s.next_id()));
            let enabled = with_store(|s| {
                s.entries()
                    .iter()
                    .find(|e| e.routine.id == id)
                    .map(|e| e.routine.enabled)
            })
            .unwrap_or(true);
            let saved = draft.into_routine(&id, enabled);
            with_store(|s| s.save(saved.clone()));
            entries_st.update(|list| {
                match list.iter_mut().find(|e| e.routine.id == id) {
                    Some(slot) => slot.routine = saved,
                    None => list.push(Entry {
                        routine: saved,
                        runs: Vec::new(),
                        next_at: None,
                    }),
                }
            });
            selected_ref.set(Some(id));
            panel_ref.set(Panel::View);
            tick_ref.update(|v| *v += 1);
        }
    };

    cu::ScrollColumn()
        .modifier(Modifier::new().fill_max_width().fill_max_height())
        .content(|| {
            cu::Column()
                .spacing(SP[4])
                .modifier(Modifier::new().fill_max_width().padding(pad))
                .content(|| {
                    if mode != PaneMode::DetailOnly {
                        let panel_new = panel.clone();
                        RoutineHeader(compact, move || {
                            if compact {
                                cu::Box()
                                    .modifier(
                                        Modifier::new()
                                            .width(52.0)
                                            .height(40.0)
                                            .background(paint(p.ink))
                                            .border(1.0, paint(p.ink))
                                            .shape_role(ShapeRole::Full),
                                    )
                                    .alignment(Alignment::Center)
                                    .content(move || {
                                        NavIcon(super::shell::ICON_PLUS_SVG, p.ink_text.to_argb(), 18.0);
                                        FocusOverlay("새 루틴", move || panel_new.set(Panel::New));
                                    });
                            } else {
                                let label = "새 루틴";
                                let btn_w = (18.0 + 12.0 + super::home::text_px(label, Fs::Sm.size()) + 36.0)
                                    .ceil()
                                    .max(116.0);
                                cu::Button(label)
                                    .icon(IconRole::Add)
                                    .variant(ButtonVariant::Text)
                                    .color(paint(p.ink_text))
                                    .modifier(
                                        Modifier::new()
                                            .width(btn_w)
                                            .height(40.0)
                                            .background(paint(p.ink))
                                            .border(1.0, paint(p.ink))
                                            .shape_role(ShapeRole::Full),
                                    )
                                    .on_click(move || panel_new.set(Panel::New));
                            }
                        });
                    }
                    if !cur_online {
                        Label(OFFLINE_NOTE, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(3));
                    } else if device_only {
                        Label(LOCAL_NOTE, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(3));
                    }
                    if mode != PaneMode::DetailOnly {
                        Dashboard(&cur_entries, now, compact, window.width_dp);
                    }
                    match mode {
                        PaneMode::Both => {
                            cu::Row().spacing(SP[4]).modifier(Modifier::new().fill_max_width()).content(|| {
                                cu::Column().modifier(Modifier::new().width(list_pane_width(window.width_dp))).content(|| {
                                    ListPane(
                                        &cur_entries,
                                        &groups,
                                        &pills,
                                        &filter,
                                        &query,
                                        shown.as_deref(),
                                        now,
                                        &computers,
                                        {
                                            let sel = selected.clone();
                                            move |id| {
                                                sel.set(Some(id));
                                                if let Some(d) = detail_state() {
                                                    d.set(true);
                                                }
                                            }
                                        },
                                        on_toggle.clone(),
                                    );
                                });
                                cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                                    DetailPane(
                                        detail.as_ref(),
                                        now,
                                        false,
                                        &computers,
                                        cur_entries.is_empty(),
                                        {
                                            let sel = selected.clone();
                                            move || {
                                                sel.set(None);
                                                if let Some(d) = detail_state() {
                                                    d.set(false);
                                                }
                                            }
                                        },
                                        {
                                            let pan = panel.clone();
                                            move |id| pan.set(Panel::Edit(id))
                                        },
                                        on_run_now.clone(),
                                        on_toggle.clone(),
                                    );
                                });
                            });
                        }
                        PaneMode::ListOnly => {
                            ListPane(
                                &cur_entries,
                                &groups,
                                &pills,
                                &filter,
                                &query,
                                None,
                                now,
                                &computers,
                                {
                                    let sel = selected.clone();
                                    move |id| {
                                        sel.set(Some(id));
                                        if let Some(d) = detail_state() {
                                            d.set(true);
                                        }
                                    }
                                },
                                on_toggle.clone(),
                            );
                        }
                        PaneMode::DetailOnly => {
                            DetailPane(
                                detail.as_ref(),
                                now,
                                true,
                                &computers,
                                cur_entries.is_empty(),
                                {
                                    let sel = selected.clone();
                                    move || {
                                        sel.set(None);
                                        if let Some(d) = detail_state() {
                                            d.set(false);
                                        }
                                    }
                                },
                                {
                                    let pan = panel.clone();
                                    move |id| pan.set(Panel::Edit(id))
                                },
                                on_run_now.clone(),
                                on_toggle.clone(),
                            );
                        }
                    }
                });
        });

    let form_panel = panel.get();
    let form_initial = match &form_panel {
        Panel::View => None,
        Panel::New => Some((
            FormDraft::new(filter.get().as_deref().unwrap_or(DEFAULT_PROJECT)),
            None,
        )),
        Panel::Edit(id) => cur_entries
            .iter()
            .find(|e| e.routine.id == *id)
            .map(|e| (FormDraft::from_routine(&e.routine), Some(id.clone()))),
    };

    if let Some((initial, editing)) = form_initial {
        let panel_close = panel.clone();
        let save_cb = on_save.clone();
        RoutineForm(
            initial,
            editing.clone(),
            &computers,
            &existing_projects,
            move |d| save_cb((d, editing.clone())),
            move || panel_close.set(Panel::View),
        );
    }
}

/// A review window (`EMBER_CR_ROUTE=routines`): the routines screen with sample routines.
#[compose_rust::runtime::composable]
pub fn preview() {
    let p = ember_theme().palette;
    cu::Box()
        .modifier(Modifier::new().fill_max_size().background(paint(p.bg)))
        .content(|| {
            RoutinesScreenWith(sample_entries(), true);
        });
}

// ---------------------------------------------------------------------------------------------
// Component helpers (Header, Switch, FilterSegButton)
// ---------------------------------------------------------------------------------------------

const ICON_CLOCK_SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"24\" fill=\"none\" viewBox=\"0 0 24 24\" stroke-width=\"1.5\" stroke=\"currentColor\"><path stroke-linecap=\"round\" stroke-linejoin=\"round\" d=\"M12 6v6h4.5m4.5 0a9 9 0 1 1-18 0 9 9 0 0 1 18 0Z\"/></svg>";
const ICON_ZAP_SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"24\" fill=\"none\" viewBox=\"0 0 24 24\" stroke-width=\"1.5\" stroke=\"currentColor\"><path stroke-linecap=\"round\" stroke-linejoin=\"round\" d=\"m3.75 13.5 10.5-11.25L12 10.5h8.25L9.75 21.75 12 13.5H3.75Z\"/></svg>";
const ICON_SEARCH_SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"24\" fill=\"none\" viewBox=\"0 0 24 24\" stroke-width=\"1.5\" stroke=\"currentColor\"><path stroke-linecap=\"round\" stroke-linejoin=\"round\" d=\"m21 21-5.197-5.197m0 0A7.5 7.5 0 1 0 5.196 5.196a7.5 7.5 0 0 0 10.607 10.607Z\"/></svg>";
const ICON_EDIT_SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"24\" fill=\"none\" viewBox=\"0 0 24 24\" stroke-width=\"1.5\" stroke=\"currentColor\"><path stroke-linecap=\"round\" stroke-linejoin=\"round\" d=\"m16.862 4.487 1.687-1.688a1.875 1.875 0 1 1 2.652 2.652L6.832 19.82a4.5 4.5 0 0 1-1.897 1.13l-2.685.8.8-2.685a4.5 4.5 0 0 1 1.13-1.897L16.863 4.487Zm0 0L19.5 7.125\"/></svg>";
const ICON_PLAY_SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"24\" fill=\"none\" viewBox=\"0 0 24 24\" stroke-width=\"1.5\" stroke=\"currentColor\"><path stroke-linecap=\"round\" stroke-linejoin=\"round\" d=\"M5.25 5.653c0-.856.917-1.398 1.667-.986l11.54 6.347a1.125 1.125 0 0 1 0 1.972l-11.54 6.347a1.125 1.125 0 0 1-1.667-.986V5.653Z\"/></svg>";

fn TriggerPill(trigger: Trigger, small: bool) {
    let theme = ember_theme();
    let tone = match trigger {
        Trigger::Time => PillTone::Blue,
        Trigger::Event => PillTone::Sky,
    };
    let (fg, bg) = pill_colors(tone, theme.palette);
    let (height, fs, weight) = pill_metrics(small);
    let icon_svg = match trigger {
        Trigger::Time => ICON_CLOCK_SVG,
        Trigger::Event => ICON_ZAP_SVG,
    };
    cu::Row()
        .alignment(Alignment::CenterStart)
        .modifier(
            Modifier::new()
                .height(height)
                .background(paint(bg))
                .shape_role(ShapeRole::Full),
        )
        .content(|| {
            cu::Spacer().modifier(Modifier::new().width(SP[1]));
            NavIcon(icon_svg, fg.to_argb(), if small { 12.0 } else { 14.0 });
            cu::Spacer().modifier(Modifier::new().width(4.0));
            Label(trigger.label(), fs, weight, Some(paint(fg)), Some(1));
            cu::Spacer().modifier(Modifier::new().width(SP[1]));
        });
}

/// The page header for Routines: 32px regular title, Korean subtitle, and action button.
/// On compact screens (phones), reserves room for the floating phone menu and stacks actions below.
fn RoutineHeader(compact: bool, trailing: impl FnOnce()) {
    super::project::Header(
        "루틴",
        "팀원이 일정이나 이벤트에 맞춰 스스로 하는 일",
        compact,
        trailing,
    );
}

/// An accessible invisible button overlay for a painted row or pill.
/// Gives assistive tech the item's label and provides keyboard focus and click handling.
fn FocusOverlay(label: &str, on_click: impl FnMut() + 'static) {
    cu::Button(label)
        .variant(ButtonVariant::Text)
        .color(Paint::Literal(Color::argb(0)))
        .modifier(Modifier::new().fill_max_size())
        .on_click(on_click);
}

/// A 36x20 toggle switch matching the mockup (`.switch.on` ink, off `track`, knob `surface`).
///
/// TODO(compose-rust#372): the toolkit `Switch` draws 54x32 green (#2ec04f) and cannot be
/// styled to the mockup's 36x20 ink track; built from a toolkit Button with a painted
/// track and knob so keyboard focus and button semantics work.
fn RoutineSwitch(routine_name: &str, checked: bool, mut on_change: impl FnMut(bool) + 'static) {
    let p = ember_theme().palette;
    let track_color = if checked { p.ink } else { p.track };
    let knob_x = if checked { 26.0 } else { 10.0 };
    let list = DrawList::builder()
        .round_rect(paint(track_color), 0.0, 0.0, 36.0, 20.0, 10.0, 0.0)
        .circle(paint(p.surface), knob_x, 10.0, 8.0, 0.0)
        .build();
    let switch_label = if checked {
        format!("{routine_name} 루틴 끄기")
    } else {
        format!("{routine_name} 루틴 켜기")
    };
    cu::Box()
        .modifier(Modifier::new().size(36.0, 20.0))
        .content(move || {
            cu::Canvas(list).modifier(Modifier::new().size(36.0, 20.0));
            FocusOverlay(&switch_label, move || on_change(!checked));
        });
}

/// A filter segment pill button matching the mockup (`.seg button`): 32px height, pill shape,
/// ink fill when selected, surface with line border when unselected.
fn FilterSegButton(text: &str, selected: bool, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let (bg, border, fg) = if selected {
        (p.ink, p.ink, p.ink_text)
    } else {
        (p.surface, p.line, p.text_2)
    };
    let advance: f32 = text
        .chars()
        .map(|c| if c.is_ascii() { 8.0 } else { 13.0 })
        .sum::<f32>();
    let width = (advance + 28.0).max(44.0);
    cu::Box()
        .modifier(
            Modifier::new()
                .width(width)
                .height(H_CTL)
                .background(paint(bg))
                .border(1.0, paint(border))
                .shape_role(ShapeRole::Full),
        )
        .alignment(Alignment::Center)
        .content(move || {
            Label(text, Fs::Sm, Weight::Medium, Some(paint(fg)), Some(1));
            FocusOverlay(text, on_click);
        });
}

// ---------------------------------------------------------------------------------------------
// Dashboard (KPI cards + Hero card with success ring and weekday bars)
// ---------------------------------------------------------------------------------------------

fn Dashboard(entries: &[Entry], now: Minutes, compact: bool, width_dp: f32) {
    let p = ember_theme().palette;
    let k = kpis(entries, now, LOCAL_OFFSET_MIN);
    let rate = overall_success(entries);
    let bars = weekday_bars(entries, now, LOCAL_OFFSET_MIN);

    let mut ring = DrawList::builder().circle(paint(p.track), 38.0, 38.0, 30.0, 8.0);
    if let Some(r) = rate.filter(|r| *r > 0) {
        ring = ring.arc(
            paint(if r < 60 {
                p.orange.strong
            } else {
                p.blue.strong
            }),
            38.0,
            38.0,
            30.0,
            -90.0,
            (359.9_f32).min(360.0 * r as f32 / 100.0),
            8.0,
        );
    }
    let ring_cmd = ring.build();

    let chart_w = if compact { 148.0 } else { 228.0 };
    let chart_h = if compact { 96.0 } else { 100.0 };
    let chart_gap = if compact { 6.0 } else { 10.0 };
    let totals: Vec<usize> = bars.iter().map(|b| b.ok + b.fail).collect();
    let mut chart = DrawList::builder().rect(paint(p.line), 0.0, chart_h, chart_w, 1.0, 0.0);
    for (b, (x, y, w, h)) in bars.iter().zip(bar_rects(&totals, chart_w, chart_h, chart_gap)) {
        if h <= 0.0 {
            continue;
        }
        let total = (b.ok + b.fail) as f32;
        let fail_h = h * b.fail as f32 / total;
        chart = chart.round_rect(paint(p.blue.strong), x, y + fail_h, w, h - fail_h, 4.0, 0.0);
        if fail_h > 0.0 {
            chart = chart.round_rect(paint(p.orange.strong), x, y, w, fail_h, 4.0, 0.0);
        }
    }
    let chart_cmd = chart.build();
    let busy = totals.iter().any(|t| *t > 0);

    let kpis_content = move || {
        cu::Column()
            .spacing(SP[2])
            .modifier(Modifier::new().fill_max_width().height(174.0))
            .content(|| {
                cu::Row().modifier(Modifier::new().fill_max_width()).content(|| {
                    RoutineKpiView(&k[0], 0);
                    RoutineKpiView(&k[1], 1);
                });
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                cu::Row().modifier(Modifier::new().fill_max_width()).content(|| {
                    RoutineKpiView(&k[2], 2);
                    RoutineKpiView(&k[3], 3);
                });
            });
    };

    let hero_content = move || {
        HeroCardContent(&ring_cmd, rate, &chart_cmd, &bars, busy, chart_w, chart_h);
    };

    if width_dp > 1279.0 {
        cu::Row().spacing(SP[4]).modifier(Modifier::new().fill_max_width()).content(|| {
            EmberCard(false, true, Some(1.1), None, None, kpis_content);
            EmberCard(false, true, Some(1.0), None, None, hero_content);
        });
    } else {
        cu::Column().spacing(SP[3]).modifier(Modifier::new().fill_max_width()).content(|| {
            EmberCard(false, true, None, None, None, kpis_content);
            EmberCard(false, true, None, None, None, hero_content);
        });
    }
}

fn HeroCardContent(
    ring_cmd: &DrawList,
    rate: Option<u32>,
    chart_cmd: &DrawList,
    bars: &[DayBar; 7],
    busy: bool,
    chart_w: f32,
    chart_h: f32,
) {
    let p = ember_theme().palette;
    cu::Row()
        .spacing(SP[3])
        .alignment(Alignment::CenterStart)
        .modifier(Modifier::new().fill_max_width().height(174.0))
        .content(|| {
            cu::Column().spacing(SP[1]).modifier(Modifier::new().weight(1.0)).content(|| {
                Label("성공률 (전체 기록)", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                cu::Spacer().modifier(Modifier::new().height(SP[1]));
                cu::Row().spacing(SP[2]).alignment(Alignment::CenterStart).content(|| {
                    cu::Box()
                        .modifier(Modifier::new().size(76.0, 76.0))
                        .alignment(Alignment::Center)
                        .content(|| {
                            cu::Canvas(ring_cmd.clone()).modifier(Modifier::new().size(76.0, 76.0));
                            Label(
                                &rate.map(|r| format!("{r}%")).unwrap_or_else(|| "-".into()),
                                Fs::Md,
                                Weight::Semibold,
                                None,
                                Some(1),
                            );
                        });
                    cu::Column().spacing(SP[1]).alignment(Alignment::CenterStart).content(|| {
                        cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
                            cu::Box().modifier(Modifier::new().size(8.0, 8.0).background(paint(p.blue.strong)).shape_role(ShapeRole::Full));
                            Label("성공", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                        });
                        cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
                            cu::Box().modifier(Modifier::new().size(8.0, 8.0).background(paint(p.orange.strong)).shape_role(ShapeRole::Full));
                            Label("실패", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                        });
                    });
                });
                if !busy {
                    Label("최근 7일 동안 끝난 실행이 없습니다", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                }
            });
            cu::Column().spacing(SP[0]).modifier(Modifier::new().width(chart_w)).content(|| {
                Label("요일별 실행", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                cu::Spacer().modifier(Modifier::new().height(SP[0]));
                cu::Canvas(chart_cmd.clone()).modifier(Modifier::new().size(chart_w, chart_h + 1.0));
                cu::Row().modifier(Modifier::new().width(chart_w).height(24.0)).content(|| {
                    for b in bars {
                        cu::Column().modifier(Modifier::new().weight(1.0)).alignment(Alignment::Center).content(|| {
                            Label(
                                b.label,
                                Fs::Xs,
                                if b.today { Weight::Semibold } else { Weight::Regular },
                                Some(paint(if b.today { p.text } else { p.muted })),
                                Some(1),
                            );
                        });
                    }
                });
            });
        });
}

fn RoutineKpiView(kpi: &RoutineKpi, index: usize) {
    let p = ember_theme().palette;
    cu::Column().modifier(Modifier::new().weight(1.0)).spacing(SP[0]).content(|| {
        Label(kpi.label, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
        if index < 3 {
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
                Label(&kpi.value, Fs::Xl2, Weight::Semibold, None, Some(1));
                if let Some(note) = &kpi.note {
                    let tone = match index {
                        1 => PillTone::Blue,
                        2 => PillTone::Orange,
                        _ => PillTone::Grey,
                    };
                    Pill(note, tone, true);
                }
            });
        } else {
            Label(&kpi.value, Fs::Xl2, Weight::Semibold, None, Some(1));
            if let Some(note) = &kpi.note {
                Label(note, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
            }
        }
    });
}

// ---------------------------------------------------------------------------------------------
// List pane & Routine row
// ---------------------------------------------------------------------------------------------

fn ListPane(
    entries: &[Entry],
    groups: &[Group],
    pills: &[String],
    filter: &MutableState<Option<String>>,
    query: &MutableState<String>,
    selected_id: Option<&str>,
    now: Minutes,
    computers: &[(String, String)],
    on_open: impl Fn(String) + Clone + 'static,
    on_toggle: impl Fn(String, bool) + Clone + 'static,
) {
    let p = ember_theme().palette;
    cu::Box()
        .modifier(
            Modifier::new()
                .fill_max_width()
                .background(paint(p.surface_2))
                .border(1.0, paint(p.line))
                .corner_radius(CORNER_LG)
                .padding(13.0),
        )
        .content(|| {
            cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                cu::ScrollRow()
                    .modifier(Modifier::new().fill_max_width().height(H_CTL + SP[0]))
                    .content(|| {
                        let f_all = filter.clone();
                        FilterSegButton("전체", filter.get().is_none(), move || f_all.set(None));
                        cu::Spacer().modifier(Modifier::new().width(SP[1]));
                        for name in pills {
                            let f_proj = filter.clone();
                            let n = name.clone();
                            let is_sel = filter.get().as_deref() == Some(name.as_str());
                            FilterSegButton(name, is_sel, move || f_proj.set(Some(n.clone())));
                            cu::Spacer().modifier(Modifier::new().width(SP[1]));
                        }
                    });
                let q_set = query.clone();
                cu::Row()
                    .alignment(Alignment::CenterStart)
                    .modifier(
                        Modifier::new()
                            .fill_max_width()
                            .height(H_BTN)
                            .background(paint(p.surface))
                            .border(1.0, paint(p.line))
                            .corner_radius(CORNER_SM),
                    )
                    .content(move || {
                        cu::Spacer().modifier(Modifier::new().width(12.0));
                        NavIcon(ICON_SEARCH_SVG, p.muted.to_argb(), 18.0);
                        cu::Spacer().modifier(Modifier::new().width(SP[1]));
                        EmberField(FieldOptions {
                            bare: true,
                            background: Some(Paint::Literal(Color::argb(0))),
                            border_width: Some(0.0),
                            height: Some(H_BTN),
                            weight: Some(1.0),
                            placeholder: "루틴 검색".to_string(),
                            on_value_change: Some(Box::new(move |v| q_set.set(v))),
                            ..Default::default()
                        });
                        cu::Spacer().modifier(Modifier::new().width(SP[1]));
                    });
                if groups.is_empty() {
                    Label(
                        if entries.is_empty() {
                            "아직 루틴이 없습니다. 새 루틴을 만들어 주세요"
                        } else {
                            "맞는 루틴이 없습니다"
                        },
                        Fs::Md,
                        Weight::Regular,
                        Some(paint(p.muted)),
                        Some(2),
                    );
                }
                for g in groups {
                    cu::Column().spacing(SP[0]).modifier(Modifier::new().fill_max_width()).content(|| {
                        Label(&g.title(), Fs::Xs, Weight::Semibold, Some(paint(p.muted)), Some(1));
                        for r in &g.routines {
                            let runs = entries
                                .iter()
                                .find(|e| e.routine.id == r.id)
                                .map(|e| e.runs.clone())
                                .unwrap_or_default();
                            let is_selected = selected_id == Some(r.id.as_str());
                            RoutineRow(
                                r,
                                &runs,
                                is_selected,
                                now,
                                computers,
                                on_open.clone(),
                                on_toggle.clone(),
                            );
                        }
                    });
                }
            });
        });
}

fn RoutineRow(
    routine: &Routine,
    runs: &[Run],
    selected: bool,
    now: Minutes,
    computers: &[(String, String)],
    on_open: impl Fn(String) + 'static,
    on_toggle: impl Fn(String, bool) + 'static,
) {
    let p = ember_theme().palette;
    let open_id = routine.id.clone();
    let toggle_id = routine.id.clone();
    let routine_name = routine.name.clone();
    let routine_enabled = routine.enabled;
    let routine_trigger = routine.trigger;
    let routine_line = schedule_line(routine);
    let routine_harness = routine.sel.option.harness.label().to_string();
    let computer = computer_chip_text(routine.sel.computer.as_deref(), computers);
    let last_run = runs.first();

    let vendor_color = match routine.sel.option.harness {
        Harness::ClaudeCode => p.orange.strong,
        Harness::Codex => p.blue.strong,
        Harness::Gemini => p.sky.strong,
    };

    let row_border = if selected { p.ink } else { p.line };
    let row_alpha = if routine_enabled { 1.0 } else { 0.6 };

    // Accessible focusable row container using toolkit Button
    cu::Box()
        .modifier(
            Modifier::new()
                .fill_max_width()
                .height(103.0)
                .background(paint(p.surface))
                .border(1.0, paint(row_border))
                .corner_radius(CORNER_MD)
                .alpha(row_alpha),
        )
        .content(move || {
            FocusOverlay(&routine_name, move || on_open(open_id.clone()));
            cu::Column()
                .spacing(SP[1])
                .modifier(
                    Modifier::new()
                        .fill_max_width()
                        .height(103.0)
                        .padding(SP[2]),
                )
                .content(move || {
                    cu::Row()
                        .alignment(Alignment::CenterStart)
                        .modifier(Modifier::new().fill_max_width())
                        .content(move || {
                            Label(&routine_name, Fs::Md, Weight::Semibold, Some(paint(p.text)), Some(1));
                            cu::Spacer().modifier(Modifier::new().weight(1.0));
                            let mut toggle_fn = on_toggle;
                            RoutineSwitch(&routine_name, routine_enabled, move |v| toggle_fn(toggle_id.clone(), v));
                        });
                    cu::Row()
                        .alignment(Alignment::CenterStart)
                        .spacing(SP[2])
                        .modifier(Modifier::new().fill_max_width())
                        .content(move || {
                            TriggerPill(routine_trigger, true);
                            Label(&routine_line, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(1));
                        });
                    cu::Row()
                        .alignment(Alignment::CenterStart)
                        .spacing(SP[1])
                        .modifier(Modifier::new().fill_max_width())
                        .content(move || {
                            cu::Box()
                                .modifier(
                                    Modifier::new()
                                        .size(8.0, 8.0)
                                        .background(paint(vendor_color))
                                        .shape_role(ShapeRole::Full),
                                );
                            cu::Spacer().modifier(Modifier::new().width(SP[0]));
                            Label(
                                &format!("{routine_harness} · {computer}"),
                                Fs::Xs,
                                Weight::Regular,
                                Some(paint(p.muted)),
                                Some(1),
                            );
                            cu::Spacer().modifier(Modifier::new().weight(1.0));
                            if let Some(r) = last_run {
                                Pill(r.status.label(), status_tone(r.status), true);
                                cu::Spacer().modifier(Modifier::new().width(SP[1]));
                                Label(&ago(now, r.at), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                            } else {
                                Label("실행 기록 없음", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                            }
                        });
                });
        });
}

// ---------------------------------------------------------------------------------------------
// Detail pane
// ---------------------------------------------------------------------------------------------

fn DetailPane(
    entry: Option<&Entry>,
    now: Minutes,
    compact: bool,
    computers: &[(String, String)],
    empty: bool,
    on_back: impl Fn() + 'static,
    on_edit: impl Fn(String) + 'static,
    on_run: impl Fn(String) + 'static,
    on_toggle: impl Fn(String, bool) + 'static,
) {
    let p = ember_theme().palette;
    let Some(entry) = entry else {
        EmberCard(false, true, None, None, None, || {
            Label(
                if empty {
                    "루틴을 만들면 여기에 자세한 내용이 표시됩니다"
                } else {
                    "루틴을 골라 주세요"
                },
                Fs::Md,
                Weight::Regular,
                Some(paint(p.muted)),
                Some(2),
            );
        });
        return;
    };

    let r = &entry.routine;
    let edit_id = r.id.clone();
    let run_id = r.id.clone();
    let toggle_id = r.id.clone();
    let routine_name = r.name.clone();
    let routine_enabled = r.enabled;
    let routine_trigger = r.trigger;
    let routine_cron = r.cron.clone();
    let routine_line = schedule_line(r);
    let project_name = r.sel.project.clone();
    let harness_label = r.sel.option.harness.label().to_string();
    let model_title = r.sel.option.row_title();
    let mode_label = r.sel.mode.label().to_string();
    let prompt_text = r.prompt.clone();
    let computer = computer_chip_text(r.sel.computer.as_deref(), computers);

    let rate = success_percent(&entry.runs);
    let when_big = match routine_trigger {
        Trigger::Time if !cron_valid(&routine_cron) => routine_cron.clone(),
        Trigger::Time => big_time(&routine_cron),
        Trigger::Event => routine_cron.clone(),
    };
    let next = match routine_trigger {
        _ if !routine_enabled => "꺼져 있습니다".to_string(),
        Trigger::Event => "이벤트가 있을 때 실행됩니다".to_string(),
        Trigger::Time => match entry.next_at.or_else(|| next_run(&routine_cron, now)) {
            Some(t) => format!("다음 실행 {}", format_next(t, now)),
            None => "다음 실행 없음".to_string(),
        },
    };

    let mut l = DrawList::builder().circle(paint(p.track), 38.0, 38.0, 32.0, 7.0);
    if let Some(v) = rate.filter(|v| *v > 0) {
        l = l.arc(
            paint(if v < 60 { p.orange.strong } else { p.blue.strong }),
            38.0,
            38.0,
            32.0,
            -90.0,
            (359.9_f32).min(360.0 * v as f32 / 100.0),
            7.0,
        );
    }
    let ring_cmd = l.build();

    let on_toggle = std::sync::Arc::new(on_toggle);
    let on_edit = std::sync::Arc::new(on_edit);
    let on_run = std::sync::Arc::new(on_run);
    let routine_name_c = routine_name.clone();
    let toggle_id_c = toggle_id.clone();
    let edit_id_c = edit_id.clone();
    let run_id_c = run_id.clone();
    let on_toggle_c = std::sync::Arc::clone(&on_toggle);
    let on_edit_c = std::sync::Arc::clone(&on_edit);
    let on_run_c = std::sync::Arc::clone(&on_run);

    EmberCard(false, true, None, Some(0.0), None, move || {
        cu::Column().modifier(Modifier::new().fill_max_width()).content(move || {
            // Header: Topbar with bottom border
            if compact {
                let back_cmd = DrawList::builder()
                    .line(paint(p.text_2), 20.0, 16.0, 11.0, 16.0, 2.0)
                    .line(paint(p.text_2), 16.0, 11.0, 11.0, 16.0, 2.0)
                    .line(paint(p.text_2), 16.0, 21.0, 11.0, 16.0, 2.0)
                    .build();
                let on_back_fn = on_back;
                let toggle_fn = on_toggle_c;
                let edit_fn = on_edit_c;
                let run_fn = on_run_c;
                cu::Row()
                    .alignment(Alignment::CenterStart)
                    .spacing(SP[1])
                    .modifier(Modifier::new().fill_max_width().height(56.0).padding(12.0))
                    .content(move || {
                        cu::Box()
                            .modifier(Modifier::new().size(32.0, 32.0))
                            .alignment(Alignment::Center)
                            .content(move || {
                                cu::Canvas(back_cmd).modifier(Modifier::new().size(32.0, 32.0));
                                FocusOverlay("목록", move || on_back_fn());
                            });
                        cu::Column()
                            .modifier(Modifier::new().weight(1.0))
                            .alignment(Alignment::CenterStart)
                            .content(|| {
                                Label(&routine_name_c, Fs::Md, Weight::Semibold, None, Some(1));
                            });
                        TriggerPill(routine_trigger, true);
                        RoutineSwitch(&routine_name_c, routine_enabled, move |v| toggle_fn(toggle_id_c.clone(), v));
                        cu::Box()
                            .modifier(
                                Modifier::new()
                                    .size(32.0, 32.0)
                                    .background(paint(p.surface))
                                    .border(1.0, paint(p.line))
                                    .corner_radius(CORNER_SM),
                            )
                            .alignment(Alignment::Center)
                            .content(move || {
                                NavIcon(ICON_EDIT_SVG, p.text_2.to_argb(), 16.0);
                                FocusOverlay("편집", move || edit_fn(edit_id_c.clone()));
                            });
                        cu::Box()
                            .modifier(
                                Modifier::new()
                                    .size(32.0, 32.0)
                                    .background(paint(p.ink))
                                    .border(1.0, paint(p.ink))
                                    .corner_radius(CORNER_SM),
                            )
                            .alignment(Alignment::Center)
                            .content(move || {
                                NavIcon(ICON_PLAY_SVG, p.ink_text.to_argb(), 16.0);
                                FocusOverlay("지금 실행", move || run_fn(run_id_c.clone()));
                            });
                    });
            } else {
                let toggle_fn = on_toggle;
                let edit_fn = on_edit;
                let run_fn = on_run;
                cu::Row()
                    .alignment(Alignment::CenterStart)
                    .spacing(SP[2])
                    .modifier(Modifier::new().fill_max_width().height(64.0).padding(16.0))
                    .content(move || {
                        Label(&routine_name, Fs::Xl, Weight::Semibold, None, Some(1));
                        TriggerPill(routine_trigger, true);
                        cu::Spacer().modifier(Modifier::new().weight(1.0));
                        RoutineSwitch(&routine_name, routine_enabled, move |v| toggle_fn(toggle_id.clone(), v));
                        AppButton("편집", None, ButtonKind::Default, true, true, false, move || edit_fn(edit_id.clone()));
                        AppButton("지금 실행", None, ButtonKind::Primary, true, true, false, move || run_fn(run_id.clone()));
                    });
            }
            cu::Spacer().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));

            cu::Column()
                .spacing(SP[4])
                .modifier(Modifier::new().fill_max_width().padding(if compact { 12.0 } else { 16.0 }))
                .content(move || {
                    // Schedule & Ring
                    cu::Row().alignment(Alignment::CenterStart).spacing(SP[3]).modifier(Modifier::new().fill_max_width()).content(move || {
                        cu::Column().modifier(Modifier::new().weight(1.0)).spacing(SP[0]).content(move || {
                            Label(&when_big, Fs::Xl4, Weight::Semibold, None, Some(1));
                            Label(&routine_line, Fs::Sm, Weight::Medium, None, Some(1));
                            if routine_trigger == Trigger::Time && cron_valid(&routine_cron) {
                                Label(&routine_cron, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                            }
                            Label(&next, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                        });
                        cu::Box()
                            .modifier(Modifier::new().size(76.0, 76.0))
                            .alignment(Alignment::Center)
                            .content(move || {
                                cu::Canvas(ring_cmd).modifier(Modifier::new().size(76.0, 76.0));
                                Label(
                                    &rate.map(|v| format!("{v}%")).unwrap_or_else(|| "-".to_string()),
                                    Fs::Md,
                                    Weight::Semibold,
                                    None,
                                    None,
                                );
                            });
                    });

                    // Inset cards: 3 cards in one row on wide
                    if compact {
                        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(move || {
                            KeyValue("프로젝트", &project_name, "");
                            KeyValue("에이전트", &harness_label, &model_title);
                            KeyValue("컴퓨터 · 프로필", &computer, "프로필은 하네스를 따릅니다");
                            KeyValue("권한 모드", &mode_label, "");
                        });
                    } else {
                        cu::Row().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(move || {
                            cu::Column().modifier(Modifier::new().weight(1.0)).content(move || {
                                KeyValue("프로젝트", &project_name, "");
                            });
                            cu::Column().modifier(Modifier::new().weight(1.0)).content(move || {
                                KeyValue("에이전트", &harness_label, &model_title);
                            });
                            cu::Column().modifier(Modifier::new().weight(1.0)).content(move || {
                                KeyValue("컴퓨터 · 프로필", &computer, "프로필은 하네스를 따릅니다");
                            });
                        });
                    }
                    InsetPanel(true, move || {
                        cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width().padding(SP[2])).content(move || {
                            Label("프롬프트", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                            Label(&prompt_text, Fs::Sm, Weight::Regular, None, None);
                        });
                    });
                    Label("실행 기록", Fs::Lg, Weight::Semibold, None, Some(1));
                    if entry.runs.is_empty() {
                        Label("아직 실행하지 않았습니다", Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
                    }
                    for run in &entry.runs {
                        RunTile(run, now);
                    }
                });
        });
    });
}

fn KeyValue(title: &str, value: &str, sub: &str) {
    let p = ember_theme().palette;
    let t = title.to_string();
    let v = value.to_string();
    let s = sub.to_string();
    InsetPanel(true, move || {
        cu::Column()
            .spacing(SP[0])
            .modifier(Modifier::new().fill_max_width().padding(SP[2]))
            .content(move || {
                Label(&t, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                Label(&v, Fs::Sm, Weight::Medium, None, Some(1));
                if !s.is_empty() {
                    Label(&s, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
                }
            });
    });
}

fn RunTile(run: &Run, now: Minutes) {
    let p = ember_theme().palette;
    let tone = status_tone(run.status);
    let session = run.session.clone();
    let status_lbl = run.status.label();
    let ago_lbl = ago(now, run.at);
    InsetPanel(true, move || {
        cu::Row()
            .alignment(Alignment::CenterStart)
            .spacing(SP[2])
            .modifier(Modifier::new().fill_max_width().padding(SP[2]))
            .content(move || {
                Pill(status_lbl, tone, true);
                Label(&ago_lbl, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(1));
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                if let Some(id) = session {
                    AppButton("세션 열기", None, ButtonKind::Default, true, true, false, move || {
                        if let Some(svc) = try_services() {
                            svc.ui.open_conversation(&id);
                        }
                    });
                }
            });
    });
}

// ---------------------------------------------------------------------------------------------
// Routine form
// ---------------------------------------------------------------------------------------------

fn RoutineForm(
    initial: FormDraft,
    editing: Option<String>,
    computers: &[(String, String)],
    projects: &[String],
    mut on_save: impl FnMut(FormDraft) + Clone + 'static,
    mut on_close: impl FnMut() + Clone + 'static,
) {
    let p = ember_theme().palette;
    let draft = remember(|| mutable_state_of(initial.clone()));
    let tried = remember(|| mutable_state_of(false));

    let agents = live_agents();
    let d = draft.get();
    let errors = d.errors();
    let mut project_choices = project_options(projects);
    if !project_choices.contains(&d.sel.project) {
        project_choices.push(d.sel.project.clone());
    }
    let models = filter_models(&model_catalog(), Some(d.sel.option.harness), "");
    let title = if editing.is_some() {
        "루틴 편집"
    } else {
        "새 루틴"
    };
    let schedule_hint = match d.trigger {
        Trigger::Time => {
            let now = now_local();
            match next_run(d.cron.trim(), now) {
                Some(t) => format!(
                    "{} · 다음 실행 {}",
                    describe_cron(d.cron.trim()),
                    format_next(t, now)
                ),
                None => describe_cron(d.cron.trim()),
            }
        }
        Trigger::Event => "이벤트가 일어날 때 실행됩니다".to_string(),
    };

    let body_height = (current_window_size().height_dp - 32.0 - 60.0 - 65.0).clamp(240.0, 600.0);
    let comp_list = computers.to_vec();

    EmberSheet(title, true, on_close.clone(), None, move || {
        cu::ScrollColumn()
            .modifier(Modifier::new().fill_max_width().height(body_height))
            .content(|| {
                cu::Column()
                    .spacing(SP[3])
                    .modifier(Modifier::new().fill_max_width().padding(SP[4]))
                    .content(|| {
                        if editing.is_some() {
                            Label(
                                "입력하지 않은 항목은 지금 값을 유지합니다",
                                Fs::Xs,
                                Weight::Regular,
                                Some(paint(p.muted)),
                                None,
                            );
                        }
                        FormLabel("이름");
                        let draft_name = draft.clone();
                        EmberField(FieldOptions {
                            fill_max_width: true,
                            placeholder: if initial.name.is_empty() {
                                "예: develop CI 실패 요약".to_string()
                            } else {
                                initial.name.clone()
                            },
                            on_value_change: Some(Box::new(move |v| {
                                draft_name.update(|d| d.name = v);
                            })),
                            ..Default::default()
                        });
                        FormLabel("프로젝트");
                        cu::ScrollRow()
                            .modifier(Modifier::new().fill_max_width().height(H_CTL + SP[0]))
                            .content(|| {
                                for name in &project_choices {
                                    let sel_proj = draft.clone();
                                    let n = name.clone();
                                    let selected = d.sel.project == *name;
                                    PickChip(name, None, selected, false, move || {
                                        sel_proj.update(|d| d.sel.project = n.clone());
                                    });
                                    cu::Spacer().modifier(Modifier::new().width(SP[1]));
                                }
                            });
                        FormLabel("트리거");
                        cu::Row().spacing(SP[1]).content(|| {
                            for t in Trigger::ALL {
                                let sel_trig = draft.clone();
                                PickChip(t.label(), None, d.trigger == t, false, move || {
                                    sel_trig.update(|d| *d = d.with_trigger(t));
                                });
                            }
                        });
                        FormLabel(if d.trigger == Trigger::Time { "일정 (cron)" } else { "이벤트" });
                        let sel_cron = draft.clone();
                        let ph = if initial.trigger == d.trigger && !initial.cron.is_empty() {
                            initial.cron.clone()
                        } else if d.trigger == Trigger::Time {
                            "0 9 * * *".to_string()
                        } else {
                            "예: PR이 열릴 때".to_string()
                        };
                        EmberField(FieldOptions {
                            fill_max_width: true,
                            placeholder: ph,
                            on_value_change: Some(Box::new(move |v| {
                                sel_cron.update(|d| d.cron = v);
                            })),
                            ..Default::default()
                        });
                        Label(&schedule_hint, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(2));
                        FormLabel("하네스");
                        cu::ScrollRow()
                            .modifier(Modifier::new().fill_max_width().height(H_CTL + SP[0]))
                            .content(|| {
                                for h in Harness::ALL {
                                    if harness_block(h, &agents, &[]).is_none() {
                                        let sel_h = draft.clone();
                                        PickChip(h.label(), None, d.sel.option.harness == h, false, move || {
                                            sel_h.update(|d| d.sel = d.sel.with_harness(h));
                                        });
                                        cu::Spacer().modifier(Modifier::new().width(SP[1]));
                                    }
                                }
                            });
                        FormLabel("모델과 추론 강도");
                        cu::ScrollRow()
                            .modifier(Modifier::new().fill_max_width().height(H_CTL + SP[0]))
                            .content(|| {
                                for o in &models {
                                    let sel_o = draft.clone();
                                    let opt = *o;
                                    PickChip(&o.row_title(), None, d.sel.option == *o, false, move || {
                                        sel_o.update(|d| d.sel = d.sel.with_option(opt));
                                    });
                                    cu::Spacer().modifier(Modifier::new().width(SP[1]));
                                }
                            });
                        Label("프로필은 하네스를 따릅니다. 계정을 따로 고르지 않습니다", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
                        FormLabel("권한 모드");
                        cu::ScrollRow()
                            .modifier(Modifier::new().fill_max_width().height(H_CTL + SP[0]))
                            .content(|| {
                                for m in modes_for(d.sel.option.harness).iter().copied() {
                                    let sel_m = draft.clone();
                                    PickChip(m.label(), None, d.sel.mode == m, false, move || {
                                        sel_m.update(|d| d.sel = d.sel.with_mode(m));
                                    });
                                    cu::Spacer().modifier(Modifier::new().width(SP[1]));
                                }
                            });
                        FormLabel("컴퓨터");
                        cu::ScrollRow()
                            .modifier(Modifier::new().fill_max_width().height(H_CTL + SP[0]))
                            .content(|| {
                                let sel_c = draft.clone();
                                PickChip("이 서버", None, d.sel.computer.is_none(), false, move || {
                                    sel_c.update(|d| d.sel.computer = None);
                                });
                                cu::Spacer().modifier(Modifier::new().width(SP[1]));
                                for (cid, cname) in &comp_list {
                                    let sel_c = draft.clone();
                                    let cid_clone = cid.clone();
                                    let selected = d.sel.computer.as_deref() == Some(cid.as_str());
                                    PickChip(cname, None, selected, false, move || {
                                        sel_c.update(|d| d.sel.computer = Some(cid_clone.clone()));
                                    });
                                    cu::Spacer().modifier(Modifier::new().width(SP[1]));
                                }
                            });
                        FormLabel("프롬프트");
                        let sel_p = draft.clone();
                        EmberField(FieldOptions {
                            fill_max_width: true,
                            height: Some(120.0),
                            multiline: true,
                            placeholder: if initial.prompt.is_empty() {
                                "에이전트에게 매번 줄 지시를 적어 주세요".to_string()
                            } else {
                                initial.prompt.clone()
                            },
                            on_value_change: Some(Box::new(move |v| {
                                sel_p.update(|d| d.prompt = v);
                            })),
                            ..Default::default()
                        });
                        if tried.get() {
                            for e in errors {
                                Label(e.message(), Fs::Xs, Weight::Regular, Some(paint(p.red.strong)), None);
                            }
                        }
                    });
            });
        SheetFooter(move || {
            let mut close = on_close.clone();
            AppButton("취소", None, ButtonKind::Default, true, true, false, move || close());
            let mut save = on_save.clone();
            let draft_save = draft.clone();
            let tried_set = tried.clone();
            AppButton("저장", None, ButtonKind::Primary, true, true, false, move || {
                let cur = draft_save.get();
                if cur.errors().is_empty() {
                    save(cur);
                } else {
                    tried_set.set(true);
                }
            });
        });
    });
}

fn FormLabel(text: &str) {
    let p = ember_theme().palette;
    Label(text, Fs::Sm, Weight::Medium, Some(paint(p.text_2)), Some(1));
}

// ---------------------------------------------------------------------------------------------
// Sample data
// ---------------------------------------------------------------------------------------------

/// Realistic demo routines and runs used for captures and previews.
pub fn sample_entries() -> Vec<Entry> {
    let now = now_local();
    vec![
        Entry {
            routine: Routine {
                id: "rt1".into(),
                name: "develop CI 실패 요약".into(),
                trigger: Trigger::Time,
                cron: "0 9 * * *".into(),
                prompt: "develop 브랜치에서 지난 24시간 동안 실패한 CI 체크를 모아 원인별로 요약하고, ember 개발방 채널에 올립니다.".into(),
                enabled: true,
                sel: Selection {
                    project: "ember".into(),
                    option: default_option(Harness::ClaudeCode),
                    mode: default_mode(Harness::ClaudeCode),
                    computer: Some("mac-mini".into()),
                },
            },
            runs: vec![
                Run {
                    at: now - 180,
                    status: RunStatus::Done,
                    session: Some("s9".into()),
                },
                Run {
                    at: now - 1440 - 180,
                    status: RunStatus::Done,
                    session: Some("s9".into()),
                },
                Run {
                    at: now - 2 * 1440,
                    status: RunStatus::Failed,
                    session: Some("s6".into()),
                },
            ],
            next_at: next_run("0 9 * * *", now),
        },
        Entry {
            routine: Routine {
                id: "rt2".into(),
                name: "PR 열리면 리뷰".into(),
                trigger: Trigger::Event,
                cron: "event: pull_request.opened, synchronize".into(),
                prompt: "새 PR의 diff를 읽고 SPEC ID와 테스트가 있는지 확인한 뒤, 문제점을 PR 코멘트로 남깁니다.".into(),
                enabled: true,
                sel: Selection {
                    project: "ember".into(),
                    option: default_option(Harness::Codex),
                    mode: default_mode(Harness::Codex),
                    computer: Some("mac-mini".into()),
                },
            },
            runs: vec![
                Run {
                    at: now - 60,
                    status: RunStatus::Done,
                    session: Some("s4".into()),
                },
                Run {
                    at: now - 1440 + 60,
                    status: RunStatus::Done,
                    session: Some("s5".into()),
                },
            ],
            next_at: None,
        },
        Entry {
            routine: Routine {
                id: "rt3".into(),
                name: "주간 의존성 점검".into(),
                trigger: Trigger::Time,
                cron: "0 10 * * 1".into(),
                prompt: "각 크레이트의 Cargo.lock에서 보안 권고와 오래된 의존성을 찾아 표로 정리하고, 갱신 PR 초안을 만듭니다.".into(),
                enabled: false,
                sel: Selection {
                    project: "ember".into(),
                    option: default_option(Harness::ClaudeCode),
                    mode: default_mode(Harness::ClaudeCode),
                    computer: Some("ws-linux-01".into()),
                },
            },
            runs: vec![Run {
                at: now - 5 * 1440,
                status: RunStatus::Done,
                session: Some("s5".into()),
            }],
            next_at: None,
        },
        Entry {
            routine: Routine {
                id: "rt4".into(),
                name: "OSE 야간 빌드".into(),
                trigger: Trigger::Time,
                cron: "0 2 * * *".into(),
                prompt: "Windows에서 OSE 빌드를 돌리고 실패하면 로그 마지막 200줄과 함께 CI 알림 채널에 올립니다.".into(),
                enabled: true,
                sel: Selection {
                    project: "ember".into(),
                    option: default_option(Harness::Codex),
                    mode: default_mode(Harness::Codex),
                    computer: Some("win-desktop".into()),
                },
            },
            runs: vec![
                Run {
                    at: now - 420,
                    status: RunStatus::Failed,
                    session: Some("s6".into()),
                },
                Run {
                    at: now - 1440 - 420,
                    status: RunStatus::Failed,
                    session: Some("s6".into()),
                },
            ],
            next_at: next_run("0 2 * * *", now),
        },
        Entry {
            routine: Routine {
                id: "rt5".into(),
                name: "hub API 회귀 테스트".into(),
                trigger: Trigger::Event,
                cron: "event: push(develop)".into(),
                prompt: "kernel, manager, hub API 통합 테스트를 돌리고 실패하면 원인을 찾아 수정 PR을 엽니다.".into(),
                enabled: true,
                sel: Selection {
                    project: "core".into(),
                    option: default_option(Harness::ClaudeCode),
                    mode: default_mode(Harness::ClaudeCode),
                    computer: Some("ws-linux-01".into()),
                },
            },
            runs: vec![
                Run {
                    at: now - 10,
                    status: RunStatus::Running,
                    session: Some("s3".into()),
                },
                Run {
                    at: now - 1440,
                    status: RunStatus::Done,
                    session: Some("s7".into()),
                },
            ],
            next_at: None,
        },
        Entry {
            routine: Routine {
                id: "rt6".into(),
                name: "벤치마크 리포트".into(),
                trigger: Trigger::Time,
                cron: "0 18 * * 5".into(),
                prompt: "텍스트 레이아웃 벤치마크를 돌려 지난주와 비교한 표를 docs/bench.md에 갱신합니다.".into(),
                enabled: true,
                sel: Selection {
                    project: "pyx".into(),
                    option: default_option(Harness::Gemini),
                    mode: default_mode(Harness::Gemini),
                    computer: Some("win-desktop".into()),
                },
            },
            runs: vec![Run {
                at: now - 3 * 1440,
                status: RunStatus::Done,
                session: Some("s8".into()),
            }],
            next_at: next_run("0 18 * * 5", now),
        },
        Entry {
            routine: Routine {
                id: "rt7".into(),
                name: "아침 브리핑".into(),
                trigger: Trigger::Time,
                cron: "30 8 * * 1-5".into(),
                prompt: "모든 프로젝트의 밤사이 세션 결과, 승인 대기, 사용량을 한 화면으로 요약합니다.".into(),
                enabled: true,
                sel: Selection {
                    project: DEFAULT_PROJECT.into(),
                    option: default_option(Harness::ClaudeCode),
                    mode: default_mode(Harness::ClaudeCode),
                    computer: Some("mac-mini".into()),
                },
            },
            runs: vec![Run {
                at: now - 1440,
                status: RunStatus::Done,
                session: Some("s9".into()),
            }],
            next_at: next_run("30 8 * * 1-5", now),
        },
    ]
}

// ---------------------------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use compose_rust::protocol::{decode_batch, HostEvent, Mutation, PropertyValue};
    use compose_rust::{
        EventPayload, Host, PropertyKind, Recomposer, WidgetKind, WindowHeightClass,
        WindowSizeClass,
    };
    use crate::ui::home::PermMode;

    fn at(y: i64, mo: u32, d: u32, h: u32, mi: u32) -> Minutes {
        minutes_at(y, mo, d, h, mi)
    }

    fn routine(id: &str, name: &str, project: &str, cron: &str) -> Routine {
        Routine {
            id: id.into(),
            name: name.into(),
            trigger: Trigger::Time,
            cron: cron.into(),
            prompt: "CI 실패를 요약합니다".into(),
            enabled: true,
            sel: Selection {
                project: project.into(),
                ..Selection::initial()
            },
        }
    }

    fn run(at: Minutes, status: RunStatus) -> Run {
        Run {
            at,
            status,
            session: None,
        }
    }

    fn entry(r: Routine, runs: Vec<Run>) -> Entry {
        Entry {
            routine: r,
            runs,
            next_at: None,
        }
    }

    // ---- calendar ---------------------------------------------------------------------------

    #[test]
    fn fr_l25_calendar_round_trips_and_knows_the_weekday() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        let d = days_from_civil(2026, 10, 25);
        assert_eq!(civil_from_days(d), (2026, 10, 25));
        assert_eq!(weekday(d), 0, "2026-10-25 is a Sunday");
        assert_eq!(weekday(0), 4, "1970-01-01 was a Thursday");
        assert_eq!(civil_from_days(days_from_civil(2028, 2, 29)), (2028, 2, 29));
        assert_eq!(at(1970, 1, 2, 1, 30), 1440 + 90);
    }

    // ---- cron to sentence -------------------------------------------------------------------

    #[test]
    fn fr_l25_cron_reads_as_a_sentence() {
        let cases = [
            ("0 9 * * *", "매일 오전 9시"),
            ("30 14 * * *", "매일 오후 2시 30분"),
            ("0 0 * * *", "매일 자정"),
            ("0 12 * * *", "매일 정오"),
            ("0 9 * * 1-5", "평일 오전 9시"),
            ("0 9 * * 1", "매주 월요일 오전 9시"),
            ("0 9 * * 0", "매주 일요일 오전 9시"),
            ("0 9 1 * *", "매월 1일 오전 9시"),
            ("*/15 * * * *", "15분마다"),
            ("0 */2 * * *", "2시간마다"),
            ("* * * * *", "매분"),
            ("0 * * * *", "매시 정각"),
        ];
        for (cron, want) in cases {
            assert_eq!(describe_cron(cron), want, "{cron}");
        }
    }

    #[test]
    fn fr_l25_cron_outside_the_subset_or_invalid_says_so() {
        assert_eq!(describe_cron("5 4 * 6 *"), "사용자 지정 일정");
        assert_eq!(describe_cron("abc"), "올바르지 않은 cron입니다");
        assert_eq!(describe_cron("0 9 * *"), "올바르지 않은 cron입니다");
        assert_eq!(describe_cron("61 9 * * *"), "올바르지 않은 cron입니다");
    }

    #[test]
    fn fr_l25_cron_validity() {
        for ok in [
            "0 9 * * *",
            "*/5 * * * *",
            "0 9,18 * * 1-5",
            "0 0 1 1 *",
            "0 9 * * 7",
        ] {
            assert!(cron_valid(ok), "{ok}");
        }
        for bad in [
            "",
            "0 9 * *",
            "0 25 * * *",
            "0 9 32 * *",
            "0 9 * 13 *",
            "*/0 * * * *",
            "a b c d e",
        ] {
            assert!(!cron_valid(bad), "{bad}");
        }
    }

    #[test]
    fn fr_l25_big_time_is_hh_mm_for_a_fixed_time() {
        assert_eq!(big_time("0 9 * * *"), "09:00");
        assert_eq!(big_time("30 14 * * 1-5"), "14:30");
        assert_eq!(big_time("*/15 * * * *"), "15분마다");
    }

    // ---- next run ---------------------------------------------------------------------------

    #[test]
    fn fr_l25_next_run_is_strictly_after() {
        let nine = at(2026, 10, 25, 9, 0);
        assert_eq!(next_run("0 9 * * *", at(2026, 10, 25, 8, 0)), Some(nine));
        assert_eq!(next_run("0 9 * * *", nine), Some(at(2026, 10, 26, 9, 0)));
    }

    #[test]
    fn fr_l25_next_run_rolls_over_month_and_year() {
        assert_eq!(
            next_run("0 9 * * *", at(2026, 10, 31, 10, 0)),
            Some(at(2026, 11, 1, 9, 0))
        );
        assert_eq!(
            next_run("0 9 * * *", at(2026, 12, 31, 10, 0)),
            Some(at(2027, 1, 1, 9, 0))
        );
    }

    #[test]
    fn fr_l25_next_run_follows_weekdays_and_lists_and_steps() {
        assert_eq!(
            next_run("0 9 * * 1-5", at(2026, 10, 25, 12, 0)),
            Some(at(2026, 10, 26, 9, 0))
        );
        assert_eq!(
            next_run("0 9,18 * * *", at(2026, 10, 25, 10, 0)),
            Some(at(2026, 10, 25, 18, 0))
        );
        assert_eq!(
            next_run("*/15 * * * *", at(2026, 10, 25, 10, 7)),
            Some(at(2026, 10, 25, 10, 15))
        );
        let want = Some(at(2026, 11, 1, 9, 0));
        assert_eq!(next_run("0 9 * * 0", at(2026, 10, 25, 9, 0)), want);
        assert_eq!(next_run("0 9 * * 7", at(2026, 10, 25, 9, 0)), want);
    }

    #[test]
    fn fr_l25_next_run_skips_months_without_the_day_and_gives_up_on_never() {
        assert_eq!(
            next_run("0 9 31 * *", at(2026, 11, 1, 0, 0)),
            Some(at(2026, 12, 31, 9, 0))
        );
        assert_eq!(next_run("0 0 30 2 *", at(2026, 1, 1, 0, 0)), None);
        assert_eq!(next_run("nonsense", at(2026, 1, 1, 0, 0)), None);
    }

    #[test]
    fn fr_l25_next_run_text() {
        let now = at(2026, 10, 25, 8, 0);
        assert_eq!(format_next(at(2026, 10, 25, 9, 0), now), "오늘 09:00");
        assert_eq!(format_next(at(2026, 10, 26, 9, 5), now), "내일 09:05");
        assert_eq!(
            format_next(at(2026, 10, 28, 18, 30), now),
            "10월 28일 18:30"
        );
    }

    #[test]
    fn fr_l25_ago_text() {
        let now = at(2026, 10, 25, 12, 0);
        assert_eq!(ago(now, now), "방금 전");
        assert_eq!(ago(now, now - 5), "5분 전");
        assert_eq!(ago(now, now - 180), "3시간 전");
        assert_eq!(ago(now, now - 2 * 1440), "2일 전");
    }

    // ---- lines ------------------------------------------------------------------------------

    #[test]
    fn fr_l25_schedule_line_and_last_run_text() {
        let mut r = routine("a", "아침 요약", "ember", "0 9 * * 1-5");
        assert_eq!(schedule_line(&r), "평일 오전 9시");
        r.trigger = Trigger::Event;
        r.cron = "PR이 열릴 때".into();
        assert_eq!(schedule_line(&r), "PR이 열릴 때");

        let now = at(2026, 10, 25, 12, 0);
        assert_eq!(last_run_text(&[], now), "실행 기록 없음");
        assert_eq!(
            last_run_text(&[run(now - 180, RunStatus::Done)], now),
            "성공 · 3시간 전"
        );
        assert_eq!(
            last_run_text(&[run(now - 5, RunStatus::Failed)], now),
            "실패 · 5분 전"
        );
        assert_eq!(
            last_run_text(&[run(now, RunStatus::Running)], now),
            "실행 중 · 방금 전"
        );
    }

    // ---- grouping ---------------------------------------------------------------------------

    fn sample() -> Vec<Routine> {
        vec![
            routine("1", "CI 요약", "ember", "0 9 * * *"),
            routine("2", "잡담 정리", DEFAULT_PROJECT, "0 18 * * *"),
            routine("3", "릴리스 점검", "core", "0 10 * * 1"),
            routine("4", "의존성 확인", "ember", "0 8 * * 1"),
        ]
    }

    #[test]
    fn fr_l25_routines_group_by_project_with_the_default_last() {
        let g = group_by_project(&sample(), None, "");
        let names: Vec<&str> = g.iter().map(|g| g.project.as_str()).collect();
        assert_eq!(names, ["core", "ember", DEFAULT_PROJECT]);
        let ember: Vec<&str> = g[1].routines.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ember, ["1", "4"]);
        assert_eq!(g[1].title(), "ember (2)");
    }

    #[test]
    fn fr_l25_grouping_filters_by_project_and_query() {
        let g = group_by_project(&sample(), Some("ember"), "");
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].routines.len(), 2);

        let g = group_by_project(&sample(), None, "릴리스");
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].project, "core");

        let mut all = sample();
        all[0].name = "CI Summary".into();
        let g = group_by_project(&all, None, "ci summary");
        assert_eq!(g[0].routines[0].id, "1");

        assert!(group_by_project(&sample(), None, "없는 이름").is_empty());
        assert!(group_by_project(&[], None, "").is_empty());
    }

    #[test]
    fn fr_l25_filter_pills_follow_the_groups() {
        assert_eq!(
            filter_projects(&sample()),
            ["core", "ember", DEFAULT_PROJECT]
        );
        assert!(filter_projects(&[]).is_empty());
    }

    #[test]
    fn fr_l25_compact_swaps_list_and_detail() {
        assert_eq!(pane_mode(false, false), PaneMode::Both);
        assert_eq!(pane_mode(false, true), PaneMode::Both);
        assert_eq!(pane_mode(true, false), PaneMode::ListOnly);
        assert_eq!(pane_mode(true, true), PaneMode::DetailOnly);
    }

    // ---- form -------------------------------------------------------------------------------

    #[test]
    fn fr_l25_a_new_draft_starts_at_nine_with_the_composer_defaults() {
        let d = FormDraft::new("ember");
        assert_eq!(d.trigger, Trigger::Time);
        assert_eq!(d.cron, "0 9 * * *");
        assert_eq!(d.sel.project, "ember");
        assert_eq!(d.sel.option, default_option(Harness::ClaudeCode));
        assert_eq!(d.sel.mode, default_mode(Harness::ClaudeCode));
        assert_eq!(d.sel.computer, None);
    }

    #[test]
    fn fr_l25_validation_lists_what_blocks_saving_in_field_order() {
        let mut d = FormDraft::new("ember");
        assert_eq!(
            d.errors(),
            [FormError::NameEmpty, FormError::PromptEmpty],
        );
        d.name = "  ".into();
        assert!(d.errors().contains(&FormError::NameEmpty));
        d.name = "CI 요약".into();
        d.prompt = "실패를 요약합니다".into();
        assert!(d.errors().is_empty());

        d.cron = "0 25 * * *".into();
        assert_eq!(d.errors(), [FormError::ScheduleInvalid]);

        let mut e = d.with_trigger(Trigger::Event);
        assert_eq!(e.errors(), [FormError::EventEmpty]);
        e.cron = "PR이 열릴 때".into();
        assert!(e.errors().is_empty());
        let back = e.with_trigger(Trigger::Time);
        assert_eq!(back.cron, "0 9 * * *");
        assert_eq!(back.with_trigger(Trigger::Time), back);
    }

    #[test]
    fn fr_l25_form_error_messages_are_polite() {
        for e in [
            FormError::NameEmpty,
            FormError::ScheduleInvalid,
            FormError::EventEmpty,
            FormError::PromptEmpty,
        ] {
            assert!(e.message().ends_with("니다"), "{}", e.message());
        }
    }

    #[test]
    fn fr_l25_harness_change_moves_the_mode_like_the_composer() {
        let mut d = FormDraft::new("ember");
        d.sel = d.sel.with_mode(PermMode::Plan);
        assert_eq!(d.sel.mode, PermMode::Plan);
        d.sel = d.sel.with_harness(Harness::Codex);
        assert_eq!(d.sel.mode, default_mode(Harness::Codex));
    }

    #[test]
    fn fr_l25_draft_round_trips_a_routine_and_trims() {
        let mut d = FormDraft::new("ember");
        d.name = "  CI 요약 ".into();
        d.prompt = " 실패를 요약합니다\n".into();
        let r = d.into_routine("r1", true);
        assert_eq!((r.id.as_str(), r.name.as_str()), ("r1", "CI 요약"));
        assert_eq!(r.prompt, "실패를 요약합니다");
        assert!(r.enabled);
        let again = FormDraft::from_routine(&r);
        assert_eq!(again.name, "CI 요약");
        assert_eq!(again.sel, r.sel);
    }

    // ---- dashboard --------------------------------------------------------------------------

    #[test]
    fn fr_l25_success_percent_counts_ended_runs_only() {
        assert_eq!(success_percent(&[]), None);
        assert_eq!(success_percent(&[run(1, RunStatus::Running)]), None);
        let runs = [
            run(1, RunStatus::Done),
            run(2, RunStatus::Done),
            run(3, RunStatus::Done),
            run(4, RunStatus::Failed),
            run(5, RunStatus::Running),
        ];
        assert_eq!(success_percent(&runs), Some(75));
        let e = [
            entry(routine("1", "a", "p", "0 9 * * *"), runs.to_vec()),
            entry(
                routine("2", "b", "p", "0 9 * * *"),
                vec![run(6, RunStatus::Failed)],
            ),
        ];
        assert_eq!(overall_success(&e), Some(60));
        assert_eq!(overall_success(&[]), None);
    }

    #[test]
    fn fr_l25_kpis_of_an_empty_list_are_zero_and_honest() {
        let k = kpis(&[], at(2026, 10, 25, 12, 0), 0);
        assert_eq!(k[0].label, "켜진 루틴");
        assert_eq!(k[0].value, "0");
        assert_eq!(k[0].note.as_deref(), Some("전체 0"));
        assert_eq!(k[1].value, "0");
        assert_eq!(k[2].value, "0");
        assert_eq!(k[3].label, "다음 실행");
        assert_eq!(k[3].value, "없음");
        assert_eq!(k[3].note, None);
    }

    #[test]
    fn fr_l25_kpis_count_enabled_running_week_and_next() {
        let now = at(2026, 10, 25, 8, 0);
        let mut off = routine("2", "꺼진 것", "p", "0 7 * * *");
        off.enabled = false;
        let e = [
            entry(
                routine("1", "CI 요약", "p", "0 9 * * *"),
                vec![
                    run(now - 10, RunStatus::Running),
                    run(now - 1440, RunStatus::Failed),
                    run(now - 3 * 1440, RunStatus::Done),
                    run(now - 9 * 1440, RunStatus::Done),
                ],
            ),
            entry(off, vec![]),
        ];
        let k = kpis(&e, now, 0);
        assert_eq!(
            (k[0].value.as_str(), k[0].note.as_deref()),
            ("1", Some("전체 2"))
        );
        assert_eq!(k[1].value, "1");
        assert_eq!(k[2].value, "3");
        assert_eq!(k[2].note.as_deref(), Some("실패 1"));
        assert_eq!(k[3].value, "오늘 09:00");
        assert_eq!(k[3].note.as_deref(), Some("CI 요약"));
    }

    #[test]
    fn fr_l25_next_kpi_skips_disabled_and_event_routines() {
        let now = at(2026, 10, 25, 8, 0);
        let mut ev = routine("1", "이벤트", "p", "PR이 열릴 때");
        ev.trigger = Trigger::Event;
        let mut off = routine("2", "꺼짐", "p", "30 8 * * *");
        off.enabled = false;
        let later = routine("3", "저녁", "p", "0 18 * * *");
        let sooner = routine("4", "아침", "p", "15 8 * * *");
        let e = [ev, off, later, sooner].map(|r| entry(r, vec![]));
        let k = kpis(&e, now, 0);
        assert_eq!(k[3].value, "오늘 08:15");
        assert_eq!(k[3].note.as_deref(), Some("아침"));
    }

    #[test]
    fn fr_l25_weekday_bars_cover_seven_days_ending_today() {
        let now = at(2026, 10, 25, 12, 0);
        let e = [entry(
            routine("1", "a", "p", "0 9 * * *"),
            vec![
                run(now - 60, RunStatus::Done),
                run(now - 120, RunStatus::Failed),
                run(now - 1440, RunStatus::Done),
                run(now - 6 * 1440, RunStatus::Done),
                run(now - 7 * 1440, RunStatus::Done),
                run(now, RunStatus::Running),
            ],
        )];
        let bars = weekday_bars(&e, now, 0);
        let labels: Vec<&str> = bars.iter().map(|b| b.label).collect();
        assert_eq!(labels, ["월", "화", "수", "목", "금", "토", "일"]);
        assert!(bars[6].today && !bars[5].today);
        assert_eq!((bars[6].ok, bars[6].fail), (1, 1));
        assert_eq!((bars[5].ok, bars[5].fail), (1, 0));
        assert_eq!((bars[0].ok, bars[0].fail), (1, 0));
        let total: usize = bars.iter().map(|b| b.ok + b.fail).sum();
        assert_eq!(total, 4);
    }

    #[test]
    fn fr_l25_weekday_bars_follow_the_offset() {
        let run_at = at(2026, 10, 24, 23, 30);
        let now = at(2026, 10, 25, 12, 0);
        let e = [entry(
            routine("1", "a", "p", "0 9 * * *"),
            vec![run(run_at, RunStatus::Done)],
        )];
        let utc = weekday_bars(&e, now, 0);
        assert_eq!(utc[5].ok, 1);
        let kst = weekday_bars(&e, now + 540, 540);
        assert_eq!(kst[6].ok, 1);
    }

    // ---- store ------------------------------------------------------------------------------

    #[test]
    fn fr_l25_the_local_store_starts_empty_and_says_it_is_local() {
        let s = LocalStore::default();
        assert!(s.entries().is_empty());
        assert!(s.is_local());
    }

    #[test]
    fn fr_l25_save_creates_then_replaces_by_id() {
        let mut s = LocalStore::default();
        let id = s.next_id();
        s.save(routine(&id, "첫째", "ember", "0 9 * * *"));
        assert_eq!(s.entries().len(), 1);
        assert_ne!(s.next_id(), id);
        let mut edited = routine(&id, "고친 이름", "ember", "0 9 * * *");
        edited.enabled = false;
        s.save(edited);
        let e = s.entries();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].routine.name, "고친 이름");
        assert!(e[0].runs.is_empty());
    }

    #[test]
    fn fr_l25_toggle_and_run_now() {
        let mut s = LocalStore::default();
        let id = s.next_id();
        s.save(routine(&id, "a", "ember", "0 9 * * *"));
        assert_eq!(s.set_enabled(&id, false), Ok(()));
        assert!(!s.entries()[0].routine.enabled);
        assert_eq!(s.set_enabled("nope", true), Err(StoreError::NotFound));
        assert_eq!(s.run_now(&id), Err(StoreError::Unavailable));
        assert!(s.entries()[0].runs.is_empty());
    }

    // ---- server schedules -------------------------------------------------------------------

    #[test]
    fn fr_a8_names_come_from_the_first_line_of_the_prompt() {
        assert_eq!(name_from_prompt("  \n daily report \nmore"), "daily report");
        assert_eq!(name_from_prompt(""), "");
    }

    #[test]
    fn fr_a8_a_draft_becomes_the_create_and_patch_bodies() {
        let mut d = FormDraft::new("ember");
        d.name = " 아침 요약 ".into();
        d.prompt = " Summarise ".into();
        d.cron = " 30 8 * * 1 ".into();
        d.sel.computer = Some("c1".into());
        let body = new_schedule(&d, "/w/ember");
        assert_eq!(body.project, "ember");
        assert_eq!(body.prompt, "Summarise");
        let patch = schedule_patch(&d);
        assert_eq!(patch.prompt.as_deref(), Some("Summarise"));
    }

    // ---- sample entries ---------------------------------------------------------------------

    #[test]
    fn fr_l25_sample_entries_are_realistic_and_cover_all_projects() {
        let samples = sample_entries();
        assert!(samples.len() >= 7);
        for s in &samples {
            assert!(!s.routine.name.is_empty());
            assert!(!s.routine.prompt.is_empty());
        }
        let rate = overall_success(&samples);
        assert!(rate.is_some());
        let k = kpis(&samples, now_local(), LOCAL_OFFSET_MIN);
        assert_ne!(k[0].value, "0");
    }

    // ---- Host composable rendering tests ----------------------------------------------------

    #[test]
    fn fr_l25_host_wide_routines_screen_renders_dashboard_and_list() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| Box::new(Recomposer::new(|| RoutinesScreenWith(sample_entries(), true))));
        let mut text: String = decode_batch(host.rebuild().expect("rebuild"))
            .expect("decode")
            .iter()
            .map(|m| format!("{m:?}"))
            .collect();
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
        text.extend(decode_batch(bytes).expect("decode").iter().map(|m| format!("{m:?}")));
        for want in [
            "루틴",
            "새 루틴",
            "켜진 루틴",
            "지금 실행 중",
            "이번 주 실행",
            "다음 실행",
            "성공률",
            "요일별 실행",
            "develop CI 실패 요약",
            "PR 열리면 리뷰",
            "실행 기록",
        ] {
            assert!(text.contains(want), "missing {want}");
        }
        compose_rust::window::reset_window_size();
    }

    #[test]
    fn fr_l25_host_compact_swaps_list_and_detail_views() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| Box::new(Recomposer::new(|| RoutinesScreenWith(sample_entries(), true))));
        let mut text: String = decode_batch(host.rebuild().expect("rebuild"))
            .expect("decode")
            .iter()
            .map(|m| format!("{m:?}"))
            .collect();
        let event = HostEvent {
            node_id: 0,
            handler_id: 0,
            payload: EventPayload::WindowSizeChanged {
                width_dp: 390.0,
                height_dp: 844.0,
                class: WindowSizeClass::Compact,
                height_class: WindowHeightClass::Medium,
            },
        };
        let (bytes, _) = host.dispatch(event).expect("dispatch");
        text.extend(decode_batch(bytes).expect("decode").iter().map(|m| format!("{m:?}")));
        for want in ["루틴", "새 루틴", "develop CI 실패 요약"] {
            assert!(text.contains(want), "missing {want}");
        }
        compose_rust::window::reset_window_size();
    }

    #[test]
    fn fr_l25_host_empty_routines_shows_guidance_and_offline_note() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| Box::new(Recomposer::new(|| RoutinesScreenWith(vec![], false))));
        let text: String = decode_batch(host.rebuild().expect("rebuild"))
            .expect("decode")
            .iter()
            .map(|m| format!("{m:?}"))
            .collect();
        assert!(text.contains("아직 루틴이 없습니다"));
        assert!(text.contains(OFFLINE_NOTE));
        compose_rust::window::reset_window_size();
    }

    #[test]
    fn fr_l25_list_rows_are_accessible_buttons_with_focus_role() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| Box::new(Recomposer::new(|| RoutinesScreenWith(sample_entries(), true))));
        let mutations = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let button_count = mutations
            .iter()
            .filter(|m| matches!(m, Mutation::Create { widget: WidgetKind::Button, .. }))
            .count();
        assert!(button_count >= 10, "expected accessible button widgets in tree, found {button_count}");
        compose_rust::window::reset_window_size();
    }

    #[test]
    fn fr_l25_list_pane_width_on_wide_is_400() {
        assert_eq!(list_pane_width(1440.0), 400.0);
        assert_eq!(list_pane_width(1280.0), 400.0);
        assert_eq!(list_pane_width(1200.0), 320.0);
        assert_eq!(list_pane_width(900.0), 300.0);
    }

    // ---- polite ending & no em-dash tests ---------------------------------------------------

    #[test]
    fn nfr_l4_routine_text_has_no_plain_da_endings() {
        let mut strings: Vec<String> = vec![
            OFFLINE_NOTE.into(),
            LOCAL_NOTE.into(),
            RUN_NOTICE.into(),
            NO_CWD_NOTICE.into(),
            FROZEN_NOTICE.into(),
        ];
        strings.extend(Trigger::ALL.iter().map(|t| t.label().to_string()));
        for e in [
            FormError::NameEmpty,
            FormError::ScheduleInvalid,
            FormError::EventEmpty,
            FormError::PromptEmpty,
        ] {
            strings.push(e.message().into());
        }
        for c in ["0 9 * * *", "0 9 * * 1-5", "*/15 * * * *", "5 4 * 6 *", "x"] {
            strings.push(describe_cron(c));
        }
        strings.push(last_run_text(&[], 0));
        for s in strings {
            assert!(
                !s.ends_with('다')
                    || s.ends_with("입니다")
                    || s.ends_with("합니다")
                    || s.ends_with("니다")
                    || s.ends_with("마다"),
                "{s}"
            );
        }
    }

    #[test]
    fn nfr_no_em_dash_in_routines_screen() {
        assert!(!include_str!("routines.rs").contains('\u{2014}'));
    }

    #[test]
    fn focus_overlay_emits_button_with_text_and_fill_max_size() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| {
            Box::new(Recomposer::new(|| {
                cu::Box().content(|| {
                    FocusOverlay("테스트 행", || {});
                });
            }))
        });
        let mutations = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let has_button = mutations.iter().any(|m| match m {
            Mutation::Create {
                widget: WidgetKind::Button,
                ..
            } => true,
            _ => false,
        });
        assert!(has_button, "expected Button widget");

        let has_text = mutations.iter().any(|m| match m {
            Mutation::SetProp {
                property,
                value: PropertyValue::String(s),
                ..
            } => *property == PropertyKind::Text && *s == "테스트 행",
            _ => false,
        });
        assert!(has_text, "expected text prop matching label");

        let has_fill_width = mutations.iter().any(|m| match m {
            Mutation::SetModifier {
                modifier: compose_rust::Modifier::FillMaxWidth,
                ..
            } => true,
            _ => false,
        });
        let has_fill_height = mutations.iter().any(|m| match m {
            Mutation::SetModifier {
                modifier: compose_rust::Modifier::FillMaxHeight,
                ..
            } => true,
            _ => false,
        });
        assert!(
            has_fill_width && has_fill_height,
            "expected fill_max_size modifier"
        );
        compose_rust::window::reset_window_size();
    }

    fn extract_buttons<'a>(mutations: &'a [Mutation<'a>]) -> Vec<(u32, Option<&'a str>)> {
        use std::collections::HashMap;
        let mut buttons: HashMap<u32, Option<&'a str>> = HashMap::new();
        for m in mutations {
            if let Mutation::Create { node_id, widget: WidgetKind::Button } = m {
                buttons.insert(*node_id, None);
            }
        }
        for m in mutations {
            match m {
                Mutation::SetProp { node_id, property: PropertyKind::Text, value: PropertyValue::String(s) } => {
                    if let Some(slot) = buttons.get_mut(node_id) {
                        *slot = Some(s);
                    }
                }
                Mutation::SetText { node_id, text, .. } => {
                    if let Some(slot) = buttons.get_mut(node_id) {
                        *slot = Some(text);
                    }
                }
                _ => {}
            }
        }
        buttons.into_iter().collect()
    }

    fn check_no_empty_buttons(mutations: &[Mutation<'_>], context: &str) {
        let buttons = extract_buttons(mutations);
        for (node_id, text) in &buttons {
            let label = text.unwrap_or("");
            assert!(
                !label.trim().is_empty(),
                "Button node {node_id} has empty Text prop in {context}"
            );
        }
    }

    #[test]
    fn fr_l25_no_button_has_empty_text_at_wide_and_compact() {
        let _guard = crate::services::test_guard();
        for (w, h, class) in [
            (1440.0, 900.0, WindowSizeClass::Expanded),
            (390.0, 844.0, WindowSizeClass::Compact),
        ] {
            compose_rust::window::reset_window_size();
            let mut host = Host::new(|| Box::new(Recomposer::new(|| RoutinesScreenWith(sample_entries(), true))));
            let initial = host.rebuild().expect("rebuild");
            let first = decode_batch(initial).expect("decode");
            check_no_empty_buttons(&first, &format!("initial width {w}"));

            let event = HostEvent {
                node_id: 0,
                handler_id: 0,
                payload: EventPayload::WindowSizeChanged {
                    width_dp: w,
                    height_dp: h,
                    class,
                    height_class: WindowHeightClass::Medium,
                },
            };
            let (bytes, _) = host.dispatch(event).expect("dispatch");
            let m = decode_batch(bytes).expect("decode");
            check_no_empty_buttons(&m, &format!("width {w}"));
        }

        compose_rust::window::reset_window_size();
    }

    #[test]
    fn fr_l25_routine_rows_and_switches_expose_routine_names() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let entries = sample_entries();
        let mut host = Host::new(|| Box::new(Recomposer::new(|| RoutinesScreenWith(sample_entries(), true))));
        let mutations = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let buttons = extract_buttons(&mutations);
        let button_labels: Vec<&str> = buttons.iter().filter_map(|(_, t)| *t).collect();

        for e in &entries {
            let name = &e.routine.name;
            let has_row_button = button_labels.iter().any(|lbl| *lbl == name);
            assert!(
                has_row_button,
                "expected routine row button with exact name '{name}', found labels: {button_labels:?}"
            );

            let has_switch_button = button_labels.iter().any(|lbl| lbl.contains(name) && (lbl.contains("루틴 끄기") || lbl.contains("루틴 켜기")));
            assert!(
                has_switch_button,
                "expected switch button containing routine name '{name}', found labels: {button_labels:?}"
            );
        }
        compose_rust::window::reset_window_size();
    }
}
