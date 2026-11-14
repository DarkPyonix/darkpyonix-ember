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
    current_window_size, launched_effect, mutable_state_of, remember, remember_with_key,
    MutableState,
};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{Alignment, ButtonVariant, Color, DrawList, IconRole, Paint, ShapeRole};

use ember_client::api::ApiError;

pub use crate::ui::routines::{
    apply_server_run_started, begin_demo_run, changed_patch, demo_run_allowed,
    demo_session_title, finish_demo_run, has_running_remote_run, patch_is_empty,
    resolve_run_session_id, rollback_optimistic_run, run_is_simulated, run_now_busy, DEMO_RUN_BUSY, DEMO_RUN_DONE,
    DEMO_RUN_MS, DEMO_RUN_STARTED, DEMO_SESSION_PREFIX, REMOTE_RUN_POLL_MS,
    REMOTE_RUN_STARTED, SAMPLE_FLAG,
    not_saved_sentence, run_row_text, unsaved_fields,
    ago, big_time, civil_from_days, cron_valid, days_from_civil,
    describe_cron, filter_projects, format_next, group_by_project, kind_text, kpis,
    last_run_text, minutes_at, ms_to_minutes, name_from_prompt, new_schedule, next_run,
    overall_success, pane_mode, run_from_server, schedule_line, schedule_patch,
    schedule_to_entry, schedule_to_routine, success_percent, weekday, weekday_bar_radius,
    weekday_bars, weekday_card_height, weekday_chart_size, DayBar,
    Entry, FormDraft, FormError, FormField, Leave, LeaveDecision, leave_decision, routine_form_dirty,
    ROUTINE_DISCARD_QUESTION, FormLayout, Group, FORM_FIELDS, form_layout,
    model_choices, schedule_description, when_big_max_lines, LocalStore, Minutes, PaneMode, Routine, RoutineKpi,
    RoutineStore, Run, RunStatus, StoreError, Trigger, LOCAL_NOTE, LOCAL_OFFSET_MIN, OFFLINE_NOTE,
};
use crate::ui::home::{
    bar_rects, computer_chip_text, default_mode, default_option, filter_models, harness_block,
    model_catalog, modes_for, project_options, DEFAULT_PROJECT, Harness, Selection,
};
use super::components::{
    pill_colors, pill_metrics, AppButton, ButtonKind, EmberCard, EmberField, EmberSheet, InsetPanel, SearchField,
    FieldOptions, Label, Pill, PillTone, Select, SheetFooter,
};
use super::compat::Spinner;
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

/// Clears the device-local store.
pub fn clear_local_store() {
    with_store(|s| *s = LocalStore::default());
}

/// Whether Routines should display sample demo entries instead of querying the server.
pub fn should_use_sample_entries(route_env: Option<&str>, demo_active: bool) -> bool {
    route_env == Some("routines") || demo_active
}

fn sample_mode() -> bool {
    let has_server = std::env::var("EMBER_SERVER_URL")
        .ok()
        .is_some_and(|v| !v.trim().is_empty());
    !has_server
        && should_use_sample_entries(
            std::env::var("EMBER_CR_ROUTE").ok().as_deref(),
            crate::demo::active_from_env() || try_services().is_some_and(|s| s.demo.is_some()),
        )
}

/// Whether the demo is on. The simulated run needs this, not just the sample route (#494).
fn demo_active() -> bool {
    crate::demo::active_from_env() || try_services().is_some_and(|s| s.demo.is_some())
}

/// The simulated run of the demo (#494): a "실행 중" entry now, then a finished one linking a new
/// sample session. Called only when [`demo_run_allowed`] says so.
fn demo_run(id: &str, entries_st: &MutableState<Vec<Entry>>, tick: &MutableState<u32>) {
    let mut list = entries_st.get();
    let Some(e) = list.iter_mut().find(|e| e.routine.id == id) else {
        notice::post("루틴을 찾을 수 없습니다");
        return;
    };
    if let Err(msg) = begin_demo_run(e, now_local()) {
        notice::post(msg);
        return;
    }
    let r = e.routine.clone();
    entries_st.set(list);
    tick.update(|v| *v += 1);
    notice::post(DEMO_RUN_STARTED);
    if try_services().is_none() {
        return;
    }
    let (entries_st, tick) = (entries_st.clone(), tick.clone());
    let id = id.to_string();
    run_shared(
        async move {
            tokio::time::sleep(std::time::Duration::from_millis(DEMO_RUN_MS)).await;
            let body = crate::server::CreateSession {
                project: r.sel.project.clone(),
                agent: r.sel.option.harness.kind().to_string(),
                cwd: String::new(),
                model: Some(r.sel.option.model.to_string()),
                title: Some(demo_session_title(&r.name)),
                account: None,
            };
            let svc = services();
            let sid = svc.create_session(&body).await.ok().map(|rec| rec.id);
            if let (Some(id), Some(demo)) = (&sid, &svc.demo) {
                demo.send_message(&svc.client, id, &r.prompt);
            }
            sid
        },
        move |session| {
            let mut list = entries_st.get();
            if let Some(e) = list.iter_mut().find(|e| e.routine.id == id) {
                finish_demo_run(e, session);
            }
            entries_st.set(list);
            tick.update(|v| *v += 1);
            notice::post(DEMO_RUN_DONE);
        },
    );
}

/// Capture-only hook, debug builds only: `EMBER_CR_ROUTINE_EDIT=1` opens the edit form of the
/// routine selected by `EMBER_CR_ROUTINE` at start.
fn capture_edit_panel(selected: Option<&str>) -> Panel {
    if cfg!(debug_assertions) && std::env::var("EMBER_CR_ROUTINE_EDIT").as_deref() == Ok("1") {
        if let Some(id) = selected {
            return Panel::Edit(id.to_string());
        }
    }
    Panel::View
}

/// Capture-only hook, debug builds and demo mode only: `EMBER_CR_ROUTINE_RUN=running` (or `1`)
/// starts the simulated run of the selected routine at start and holds the "실행 중" state
/// without finishing, `=done` shows the finished entry. It needs no timer, so a screenshot
/// always catches the state (`EMBER_CR_ROUTINE_RUN_AT` is therefore not needed).
fn capture_demo_run(entries: &mut [Entry], selected: Option<&str>) {
    if !cfg!(debug_assertions) || !demo_active() {
        return;
    }
    let mode = std::env::var("EMBER_CR_ROUTINE_RUN").unwrap_or_default();
    if !matches!(mode.as_str(), "1" | "running" | "done") {
        return;
    }
    let fallback = entries.first().map(|e| e.routine.id.clone());
    let Some(id) = selected.map(str::to_string).or(fallback) else { return };
    let Some(e) = entries.iter_mut().find(|e| e.routine.id == id) else { return };
    if !demo_run_allowed(true, sample_entries().iter().any(|s| s.routine.id == id)) {
        return;
    }
    if begin_demo_run(e, now_local()).is_ok() && mode == "done" {
        finish_demo_run(e, Some(format!("{DEMO_SESSION_PREFIX}capture")));
    }
}

/// Sync `entries_st` and `online_st` from [`Remote`] and [`LocalStore`].
pub fn sync_from_remote(entries_st: &MutableState<Vec<Entry>>, online_st: &MutableState<bool>) {
    let (on, mut remote_entries) = with_remote(|r| (r.online, r.entries.clone()));
    remote_entries.extend(with_store(|s| s.entries()));
    online_st.set(on);
    entries_st.set(remote_entries);
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

fn remote_schedule_allowed(sample: bool, online: bool) -> bool {
    !sample && online
}

/// Whether `id` is a server schedule (else it is on this device).
pub fn is_remote(id: &str) -> bool {
    with_remote(|r| remote_schedule_allowed(sample_mode(), r.online) && r.entries.iter().any(|e| e.routine.id == id))
}

/// The server's schedules with their recent runs.
pub async fn load_remote() -> Result<Vec<Entry>, ApiError> {
    load_remote_from(services().client.api()).await
}

/// Loads all schedules and their recent runs from `api` (FR-L25, #494).
pub async fn load_remote_from(api: &ember_client::api::Api) -> Result<Vec<Entry>, ApiError> {
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

fn remote_action<F>(
    tick: MutableState<u32>,
    entries_st: MutableState<Vec<Entry>>,
    online_st: MutableState<bool>,
    work: F,
) where
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
            sync_from_remote(&entries_st, &online_st);
            match done {
                Ok(Some(text)) => notice::post(text),
                Ok(None) => {}
                Err(e) => notice::post(format!("서버가 요청을 처리하지 못했습니다: {e}")),
            }
            tick.update(|v| *v += 1);
        },
    );
}

/// Inserts the optimistic `RunStatus::Running` placeholder into `entries_st` and `REMOTE` for
/// `id` before calling `POST /api/schedules/{id}/run` (FR-L25, #494).
pub fn begin_remote_run_now(
    id: &str,
    now: Minutes,
    entries_st: &MutableState<Vec<Entry>>,
) -> Result<(), &'static str> {
    let mut list = entries_st.get();
    let Some(e) = list.iter_mut().find(|e| e.routine.id == id) else {
        return Err("루틴을 찾을 수 없습니다");
    };
    begin_demo_run(e, now)?;
    entries_st.set(list);
    with_remote(|r| {
        if let Some(re) = r.entries.iter_mut().find(|e| e.routine.id == id) {
            let _ = begin_demo_run(re, now);
        }
    });
    Ok(())
}

/// Applies the result of `POST /api/schedules/{id}/run` (`run_res`) and the refreshed
/// schedule list (`list`) to `REMOTE`, `entries_st` and `online_st`, rolling back any
/// optimistic placeholder on error and returning the user-facing notice text (FR-L25, #494).
pub fn finish_remote_run_now(
    id: &str,
    run_res: Result<ember_client::wire::ScheduleRun, ApiError>,
    list: Result<Vec<Entry>, ApiError>,
    entries_st: &MutableState<Vec<Entry>>,
    online_st: &MutableState<bool>,
) -> String {
    apply_load(list);
    match run_res {
        Ok(run) => {
            let msg = with_remote(|r| {
                r.entries
                    .iter_mut()
                    .find(|e| e.routine.id == id)
                    .map(|e| apply_server_run_started(e, &run))
                    .unwrap_or_else(|| match &run.error {
                        Some(err) => format!("실행하지 못했습니다: {err}"),
                        None => REMOTE_RUN_STARTED.to_string(),
                    })
            });
            sync_from_remote(entries_st, online_st);
            msg
        }
        Err(e) => {
            with_remote(|r| {
                if let Some(re) = r.entries.iter_mut().find(|e| e.routine.id == id) {
                    rollback_optimistic_run(re);
                }
            });
            sync_from_remote(entries_st, online_st);
            let mut cur = entries_st.get();
            if let Some(le) = cur.iter_mut().find(|e| e.routine.id == id) {
                if rollback_optimistic_run(le) {
                    entries_st.set(cur);
                }
            }
            format!("서버가 요청을 처리하지 못했습니다: {e}")
        }
    }
}

/// Start a remote schedule run (`POST /api/schedules/{id}/run`), immediately marking the
/// routine as `"실행 중"` (`RunStatus::Running`), refreshing the client's session list when a
/// session is created, and syncing the updated run list from the server (FR-L25, #494).
fn remote_run_now(
    id: &str,
    entries_st: &MutableState<Vec<Entry>>,
    online_st: &MutableState<bool>,
    tick: &MutableState<u32>,
) {
    if try_services().is_none() {
        notice::post(RUN_NOTICE);
        return;
    }
    if let Err(msg) = begin_remote_run_now(id, now_local(), entries_st) {
        notice::post(msg);
        return;
    }
    tick.update(|v| *v += 1);
    let id = id.to_string();
    let entries_st = entries_st.clone();
    let online_st = online_st.clone();
    let tick = tick.clone();
    run_shared(
        async move {
            let svc = services();
            let api = svc.client.api();
            let run_res = api.run_schedule(&id).await;
            if let Ok(run) = &run_res {
                if run.session_id.is_some() {
                    if let Ok(sessions) = api.sessions(None).await {
                        svc.client.apply(ember_client::Input::SessionsLoaded(sessions));
                    }
                }
            }
            let list = load_remote().await;
            (id, run_res, list)
        },
        move |(id, run_res, list)| {
            let msg = finish_remote_run_now(&id, run_res, list, &entries_st, &online_st);
            notice::post(msg);
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

/// The room a phone keeps below the content so the floating tab bar (`TABBAR_H` plus its
/// `TABBAR_BOTTOM_GAP`, `shell.rs`) never covers the last row: the bar, its gap and one `SP[3]`.
pub fn compact_bottom_clearance() -> f32 {
    TABBAR_H + TABBAR_BOTTOM_GAP + SP[3]
}

/// The search field's width in the list pane, from the layout rules in `ListPane`: the screen
/// content (window minus the page padding twice) minus the list box's padding and border twice,
/// minus the search row's fixed parts (12 leading, 18 icon, `SP[1]` after the icon and `SP[1]`
/// trailing). The field is laid out by weight, so this is the share it receives, not a width it
/// is given.
pub fn search_field_width(width_dp: f32, compact: bool) -> f32 {
    let page_pad = if compact { SP[3] } else { SP[6] };
    let pane = if compact { width_dp - 2.0 * page_pad } else { list_pane_width(width_dp) };
    (pane - 2.0 * (13.0 + 1.0) - (12.0 + 18.0 + SP[1] + SP[1])).max(0.0)
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

/// How the detail header lays out its actions (FR-L25, #492).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailHeader {
    /// Title, trigger pill and the three actions in one row.
    Inline,
    /// The actions move under the title because the panel is too narrow for one row.
    Stacked,
    /// Phone: a back button and icon-only actions.
    Icons,
}

/// The detail panel is narrower than this and its actions move under the title.
pub const DETAIL_STACK_BELOW: f32 = 420.0;
/// The three tiles sit in one row from this panel width, and stack below it.
pub const DETAIL_TILES_ROW_FROM: f32 = 520.0;
/// The inset panel's own chrome on both sides: its padding, the 4 dp rails and the 12 dp inner.
pub const INSET_CHROME: f32 = 56.0;

/// The width of the detail panel: the content column, less the list column and the gap beside it
/// when both show.
pub fn detail_panel_width(window_dp: f32, content_dp: f32, compact: bool) -> f32 {
    if compact {
        content_dp
    } else {
        (content_dp - list_pane_width(window_dp) - SP[4]).max(0.0)
    }
}

pub fn detail_header_layout(panel_w: f32, compact: bool) -> DetailHeader {
    if compact {
        DetailHeader::Icons
    } else if panel_w < DETAIL_STACK_BELOW {
        DetailHeader::Stacked
    } else {
        DetailHeader::Inline
    }
}

pub fn detail_tile_columns(panel_w: f32) -> usize {
    if panel_w >= DETAIL_TILES_ROW_FROM {
        3
    } else {
        1
    }
}

/// The width the prompt text has inside the detail panel.
pub fn detail_prompt_width(panel_w: f32, compact: bool) -> f32 {
    let pad = if compact { SP[2] } else { SP[3] };
    (panel_w - 2.0 * pad - INSET_CHROME).max(0.0)
}

/// The detail header's height: the mockup's `.rt-dh` (`min-height: 56px`), for every layout
/// but the stacked one.
pub const DETAIL_HEADER_H: f32 = 56.0;
/// The ring beside the big time.
pub const DETAIL_RING: f32 = 76.0;
/// One run row: the inset's 12 dp above and below a row as tall as the 32 dp 세션 열기 button.
pub const RUN_TILE_H: f32 = PAD_ROW_V * 2.0 + H_CTL;

/// The detail body's padding: 14 on a wide panel, 12 on a phone.
pub fn detail_body_pad(compact: bool) -> f32 {
    if compact {
        12.0
    } else {
        14.0
    }
}

/// The gap between the detail body's sections.
pub const DETAIL_GAP: f32 = SP[3];

/// The big time's type step: 40 on a wide panel, 28 on a phone (mockup `.rt-hero-when .when`,
/// `--fs-4xl`, and `--fs-2xl` at 560 px and below).
pub fn detail_when_fs(compact: bool) -> Fs {
    if compact {
        Fs::Xl2
    } else {
        Fs::Xl4
    }
}

/// One key-value tile: the inset's padding, the label, the value, and `sub_lines` of the muted
/// line under it. The inset already pads 12 16 (`.inset`), so the tile adds none of its own.
fn kv_tile_h(sub_lines: u32) -> f32 {
    let sub = if sub_lines > 0 { SP[0] + sub_lines as f32 * Fs::Xs.line_height() } else { 0.0 };
    PAD_ROW_V * 2.0 + Fs::Xs.line_height() + SP[0] + Fs::Sm.line_height() + sub
}

/// How far below the top of the detail card the first 실행 기록 row ends, from the same
/// constants the drawing uses (FR-L25, #494). `prompt_lines` is how many lines the prompt
/// takes, `computer_sub_lines` how many the 컴퓨터 tile's note takes (2 on a phone). A time
/// routine's big time is one line; an event's may take two and is not covered here.
pub fn detail_first_run_bottom(panel_w: f32, compact: bool, prompt_lines: u32, computer_sub_lines: u32) -> f32 {
    let header = match detail_header_layout(panel_w, compact) {
        DetailHeader::Stacked => 2.0 * SP[3] + Fs::Lg.line_height() * 2.0 + SP[2] + H_CTL,
        _ => DETAIL_HEADER_H,
    };
    let when_col = detail_when_fs(compact).line_height()
        + SP[0]
        + Fs::Sm.line_height()
        + SP[0]
        + Fs::Xs.line_height();
    let when = when_col.max(DETAIL_RING);
    let tiles = if detail_tile_columns(panel_w) >= 3 {
        kv_tile_h(1).max(kv_tile_h(computer_sub_lines))
    } else {
        kv_tile_h(1) + SP[2] + kv_tile_h(computer_sub_lines).max(kv_tile_h(0))
    };
    let prompt = PAD_ROW_V * 2.0 + Fs::Xs.line_height() + SP[0] + prompt_lines as f32 * Fs::Sm.line_height();
    let history = Fs::Md.line_height() + SP[1] + RUN_TILE_H;
    header + 1.0 + detail_body_pad(compact) + when + DETAIL_GAP + tiles + DETAIL_GAP + prompt + DETAIL_GAP + history
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
    let _ = try_services().map(|s| s.ui.live.launcher.get());
    let is_demo = sample_mode();
    if !is_demo {
        let tick_refresh = tick.clone();
        launched_effect((), move || async move {
            refresh(tick_refresh);
        });
    }
    let _ = tick.get();
    let (entries, online) = if is_demo {
        let mut sample = sample_entries();
        let local = with_store(|s| s.entries());
        for entry in local {
            if let Some(slot) = sample.iter_mut().find(|e| e.routine.id == entry.routine.id) {
                let runs = if entry.runs.is_empty() {
                    std::mem::take(&mut slot.runs)
                } else {
                    entry.runs
                };
                let next_at = entry.next_at.or(slot.next_at);
                *slot = Entry {
                    routine: entry.routine,
                    runs,
                    next_at,
                };
            } else {
                sample.push(entry);
            }
        }
        (sample, true)
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

    let launch_tick = try_services().map(|s| s.ui.live.launcher.get()).unwrap_or(0);
    let tick = remember(|| mutable_state_of(0_u32));
    let entries_for_preset = entries.clone();
    let entries_for_selected = entries.clone();
    let selected = remember_with_key(launch_tick, move || {
        let preset = std::env::var("EMBER_CR_ROUTINE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .and_then(|v| {
                v.parse::<usize>()
                    .ok()
                    .and_then(|i| entries_for_selected.get(i))
                    .map(|e| e.routine.id.clone())
                    .or(Some(v))
            });
        mutable_state_of(preset)
    });
    let panel_preset = capture_edit_panel(selected.get().as_deref());
    let preset_initial = match &panel_preset {
        Panel::Edit(id) => entries_for_preset
            .iter()
            .find(|e| e.routine.id == *id)
            .map(|e| FormDraft::from_routine(&e.routine)),
        _ => None,
    }
    .unwrap_or_else(|| FormDraft::new(DEFAULT_PROJECT));
    let panel = remember_with_key(launch_tick, move || mutable_state_of(panel_preset));
    // The form's draft and the draft it opened with live here, so every route out of the form
    // can ask whether it holds changes (FR-L25, #511).
    let form_draft = remember_with_key(launch_tick, {
        let d = preset_initial.clone();
        move || mutable_state_of(d)
    });
    let form_init = remember_with_key(launch_tick, move || mutable_state_of(preset_initial));
    let pending_leave = remember_with_key(launch_tick, || mutable_state_of(None::<Leave>));
    let filter = remember_with_key(launch_tick, || mutable_state_of(None::<String>));
    let query = remember_with_key(launch_tick, || mutable_state_of(String::new()));
    let capture_sel = selected.get();
    let entries_state = remember_with_key(launch_tick, move || {
        let mut entries = entries;
        capture_demo_run(&mut entries, capture_sel.as_deref());
        mutable_state_of(entries)
    });
    let online_state = remember_with_key(launch_tick, move || mutable_state_of(online));

    let _ = tick.get();
    if !sample_mode() {
        let (remote_online, mut remote_entries) = with_remote(|r| (r.online, r.entries.clone()));
        if remote_online || !remote_entries.is_empty() {
            remote_entries.extend(with_store(|s| s.entries()));
            if online_state.get_untracked() != remote_online {
                online_state.set(remote_online);
            }
            if entries_state.get_untracked() != remote_entries {
                entries_state.set(remote_entries);
            }
        }
    }
    let cur_entries = entries_state.get();
    let cur_online = online_state.get();
    let any_running = has_running_remote_run(sample_mode(), cur_online, &cur_entries);
    if any_running {
        let tick_poll = tick.clone();
        let entries_poll = entries_state.clone();
        let online_poll = online_state.clone();
        launched_effect((any_running, tick.get()), move || async move {
            if try_services().is_none() {
                return;
            }
            run_shared(
                async move {
                    tokio::time::sleep(std::time::Duration::from_millis(REMOTE_RUN_POLL_MS)).await;
                    let list = load_remote().await;
                    if let Ok(sessions) = services().client.api().sessions(None).await {
                        services().client.apply(ember_client::Input::SessionsLoaded(sessions));
                    }
                    list
                },
                move |list| {
                    apply_load(list);
                    sync_from_remote(&entries_poll, &online_poll);
                    tick_poll.update(|v| *v += 1);
                },
            );
        });
    }
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
    let panel_w = detail_panel_width(
        window.width_dp,
        super::home::content_width(window.width_dp, pad),
        compact,
    );
    // The form's field widths, worked out here where the window is known: inside the sheet's
    // own content the window size reads 0 (#493).
    let form_w_inline = form_field_width(window.width_dp, panel_w, FormLayout::Inline);
    let form_w_sheet = form_field_width(window.width_dp, panel_w, FormLayout::Sheet);

    let on_toggle = {
        let entries_st = entries_state.clone();
        let online_st = online_state.clone();
        let tick_ref = tick.clone();
        move |id: String, on: bool| {
            if is_remote(&id) {
                remote_action(
                    tick_ref.clone(),
                    entries_st.clone(),
                    online_st.clone(),
                    async move {
                        let api = services().client.api();
                        if on {
                            api.resume_schedule(&id).await?;
                        } else {
                            api.pause_schedule(&id).await?;
                        }
                        Ok(None)
                    },
                );
            } else {
                if with_store(|s| s.set_enabled(&id, on)).is_err() {
                    if let Some(e) = entries_st.get().iter().find(|e| e.routine.id == id) {
                        let mut r = e.routine.clone();
                        r.enabled = on;
                        with_store(|s| s.save(r));
                    }
                }
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
        let entries_st = entries_state.clone();
        let online_st = online_state.clone();
        move |id: String| {
            if is_remote(&id) {
                remote_run_now(&id, &entries_st, &online_st, &tick_ref);
                return;
            }
            let sample = sample_entries().iter().any(|e| e.routine.id == id);
            if demo_run_allowed(demo_active(), sample) {
                demo_run(&id, &entries_st, &tick_ref);
                return;
            }
            if sample_mode() && sample {
                notice::post(RUN_NOTICE);
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
                let unsaved = old.as_ref().map(|o| unsaved_fields(&draft, o)).unwrap_or_default();
                let mut patch = match &old {
                    Some(o) => changed_patch(&draft, o),
                    None => schedule_patch(&draft),
                };
                if patch.project.is_some() {
                    let cwd = try_services().and_then(|svc| {
                        svc.client.read(|s| crate::model::latest_cwd(s, &draft.sel.project))
                    });
                    let Some(cwd) = cwd else {
                        notice::post(NO_CWD_NOTICE);
                        return;
                    };
                    patch.cwd = Some(cwd);
                }
                if patch_is_empty(&patch) {
                    // Nothing the server can store changed; do not send an empty PATCH.
                    panel_ref.set(Panel::View);
                    notice::post(
                        not_saved_sentence(&unsaved)
                            .unwrap_or_else(|| "바뀐 내용이 없습니다".to_string()),
                    );
                    return;
                }
                remote_action(
                    tick_ref.clone(),
                    entries_st.clone(),
                    online_st.clone(),
                    async move {
                        services().client.api().update_schedule(&id, &patch).await?;
                        Ok(not_saved_sentence(&unsaved).map(|n| format!("{n}. 나머지 변경 사항은 저장했습니다")))
                    },
                );
                panel_ref.set(Panel::View);
                return;
            }
            if editing.is_none() && draft.trigger == Trigger::Time && with_remote(|r| remote_schedule_allowed(sample_mode(), r.online)) {
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
            .or_else(|| {
                entries_st
                    .get()
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

    let form_panel = panel.get();
    let form_initial = match &form_panel {
        Panel::View => None,
        Panel::New => Some((form_init.get(), None)),
        Panel::Edit(id) => cur_entries
            .iter()
            .any(|e| e.routine.id == *id)
            .then(|| (form_init.get(), Some(id.clone()))),
    };
    let form_dirty = form_initial.is_some() && routine_form_dirty(&form_init.get(), &form_draft.get());

    // Opening a form starts it from the stored routine (or an empty draft) and remembers that
    // starting point, which is what "changed" is measured against.
    let perform = {
        let (panel, selected, form_draft, form_init) =
            (panel.clone(), selected.clone(), form_draft.clone(), form_init.clone());
        let (entries_st, filter) = (entries_state.clone(), filter.clone());
        move |leave: Leave| {
            let open_form = |p: Panel| {
                let init = match &p {
                    Panel::Edit(id) => entries_st
                        .get()
                        .iter()
                        .find(|e| e.routine.id == *id)
                        .map(|e| FormDraft::from_routine(&e.routine)),
                    _ => None,
                }
                .unwrap_or_else(|| FormDraft::new(filter.get().as_deref().unwrap_or(DEFAULT_PROJECT)));
                form_init.set(init.clone());
                form_draft.set(init);
                panel.set(p);
            };
            match leave {
                Leave::Cancel => panel.set(Panel::View),
                Leave::OpenRoutine(id) => {
                    panel.set(Panel::View);
                    selected.set(Some(id));
                    if let Some(d) = detail_state() {
                        d.set(true);
                    }
                }
                Leave::NewForm => open_form(Panel::New),
                Leave::EditForm(id) => open_form(Panel::Edit(id)),
            }
        }
    };
    let request_leave = {
        let (perform, pending) = (perform.clone(), pending_leave.clone());
        let open = form_initial.is_some();
        move |leave: Leave| match leave_decision(open, form_dirty, false) {
            LeaveDecision::Go => perform(leave),
            LeaveDecision::Ask => pending.set(Some(leave)),
            LeaveDecision::Block => {}
        }
    };

    let inline_form = form_initial.is_some() && form_layout(mode) == FormLayout::Inline;

    cu::ScrollColumn()
        .modifier(Modifier::new().fill_max_width().fill_max_height())
        .content(|| {
            cu::Column()
                .spacing(SP[4])
                .modifier(Modifier::new().fill_max_width().padding(pad))
                .content(|| {
                    if mode != PaneMode::DetailOnly {
                        let rl_new = request_leave.clone();
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
                                        FocusOverlay("새 루틴", move || rl_new(Leave::NewForm));
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
                                    .on_click(move || rl_new(Leave::NewForm));
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
                                            // Opening another routine leaves the inline form, asking first.
                                            let rl = request_leave.clone();
                                            move |id| rl(Leave::OpenRoutine(id))
                                        },
                                        on_toggle.clone(),
                                    );
                                });
                                cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                                    if let (true, Some((initial, editing))) = (inline_form, form_initial.as_ref()) {
                                        let rl_close = request_leave.clone();
                                        let save_cb = on_save.clone();
                                        let editing_save = editing.clone();
                                        RoutineForm(
                                            initial.clone(),
                                            editing.clone(),
                                            editing.as_deref().is_some_and(is_remote),
                                            &computers,
                                            &existing_projects,
                                            FormLayout::Inline,
                                            form_w_inline,
                                            &form_draft,
                                            move |d| save_cb((d, editing_save.clone())),
                                            move || rl_close(Leave::Cancel),
                                        );
                                        return;
                                    }
                                    DetailPane(
                                        detail.as_ref(),
                                        now,
                                        false,
                                        panel_w,
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
                                            let rl = request_leave.clone();
                                            move |id| rl(Leave::EditForm(id))
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
                                panel_w,
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
                                    let rl = request_leave.clone();
                                    move |id| rl(Leave::EditForm(id))
                                },
                                on_run_now.clone(),
                                on_toggle.clone(),
                            );
                        }
                    }
                    if compact {
                        cu::Spacer().modifier(Modifier::new().height(compact_bottom_clearance()));
                    }
                });
        });

    if let (false, Some((initial, editing))) = (inline_form, form_initial) {
        let rl_close = request_leave.clone();
        let save_cb = on_save.clone();
        RoutineForm(
            initial,
            editing.clone(),
            editing.as_deref().is_some_and(is_remote),
            &computers,
            &existing_projects,
            FormLayout::Sheet,
            form_w_sheet,
            &form_draft,
            move |d| save_cb((d, editing.clone())),
            move || rl_close(Leave::Cancel),
        );
    }
    // The unsaved-changes question: 버리기 goes where the user was going, 계속 편집 stays.
    {
        let keep = {
            let c = pending_leave.clone();
            move || c.set(None)
        };
        let asked = pending_leave.get();
        EmberSheet("변경 사항", asked.is_some(), keep.clone(), None, || {
            cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width().padding(SP[4])).content(|| {
                Label(ROUTINE_DISCARD_QUESTION, Fs::Md, Weight::Regular, None, Some(3));
            });
            SheetFooter(|| {
                AppButton("계속 편집", None, ButtonKind::Default, true, true, false, keep.clone());
                let (c, perform) = (pending_leave.clone(), perform.clone());
                AppButton("버리기", None, ButtonKind::Primary, true, true, false, move || {
                    if let Some(l) = c.get() {
                        c.set(None);
                        perform(l);
                    }
                });
            });
        });
    }
}

/// A review window (`EMBER_CR_ROUTE=routines`): the routines screen with sample routines.
#[compose_rust::runtime::Composable]
pub fn preview_routines() {
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

// v0.0.7: #425 and #429 fixed (`Button::content`, `Modifier::label`); workaround kept until
// checked on a display (moving each painted row into a `Button` content slot changes its press
// feedback and padding). The overlay names itself with `accessible_name` (owned), never
// `Modifier::label`: a routine name is data, and the toolkit keeps every label for good (#455).
/// An accessible invisible button overlay for a painted row or pill.
/// Gives assistive tech the item's label and provides keyboard focus and click handling.
fn FocusOverlay(label: &str, on_click: impl FnMut() + 'static) {
    NamedOverlay(label, true, on_click);
}

/// The one direct button behind both overlays: invisible text, the whole box to click.
fn NamedOverlay(label: &str, enabled: bool, on_click: impl FnMut() + 'static) {
    cu::Button(label)
        .accessible_name(label)
        .enabled(enabled)
        .variant(ButtonVariant::Text)
        .color(Paint::Literal(Color::argb(0)))
        .modifier(Modifier::new().fill_max_size())
        .on_click(on_click);
}

/// What 지금 실행 reads while its routine has a run in progress.
pub const RUN_NOW_BUSY_LABEL: &str = "실행 중";

/// The invisible overlay of a control that cannot act now: named, focusable by assistive tech as
/// a disabled button, and never clickable.
fn DisabledOverlay(label: &str) {
    NamedOverlay(label, false, || {});
}

/// The header's 지금 실행. While a run is in progress it is disabled, reads 실행 중 and has a
/// spinner beside it, as a CI "run" button does while its job runs (FR-L25, #494).
fn RunNowButton(busy: bool, on_click: impl FnMut() + 'static) {
    if busy {
        cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
            Spinner(12.0);
            AppButton(RUN_NOW_BUSY_LABEL, None, ButtonKind::Primary, true, false, false, || {});
        });
    } else {
        AppButton("지금 실행", None, ButtonKind::Primary, true, true, false, on_click);
    }
}

/// A 36x20 toggle switch matching the mockup (`.switch.on` ink, off `track`, thumb `surface`).
/// The toolkit `Switch` takes the size, the colours and the accessible name since compose-rust
/// v0.0.7 (#426, #372). It announces its own on or off state, so its name is
/// "<routine> 루틴", passed with `accessible_name` because a routine name is data (#455).
fn RoutineSwitch(routine_name: &str, checked: bool, on_change: impl FnMut(bool) + 'static) {
    let p = ember_theme().palette;
    let track_color = if checked { p.ink } else { p.track };
    cu::Switch(checked)
        .size(36.0, 20.0)
        .track_color(paint(track_color))
        .thumb_color(paint(p.surface))
        .accessible_name(routine_switch_name(routine_name))
        .on_change(on_change);
}

/// The accessible name of a routine's switch.
pub fn routine_switch_name(routine_name: &str) -> String {
    format!("{routine_name} 루틴")
}

/// The segment's type step: the mockup's `.seg button` is `--fs-sm` (13) at 500.
pub const SEG_FS: Fs = Fs::Sm;

/// `(fill, border, text)` of a segment: `ink` with `ink-text` when chosen (`.seg button.on`),
/// `surface` on a `line` border with `text-2` when not.
pub fn seg_colors(selected: bool, p: &Palette) -> (Color, Color, Color) {
    if selected {
        (p.ink, p.ink, p.ink_text)
    } else {
        (p.surface, p.line, p.text_2)
    }
}

/// A segment's width, erring wide: its label at `SEG_FS` with a Latin character at 0.62 em
/// (the measure-style `text_px` uses 0.55) and a Hangul syllable at one em, 12 on each side,
/// the 1 dp border twice and 4 to spare, at least 44.
pub fn seg_width(text: &str) -> f32 {
    let em = SEG_FS.size();
    let advance: f32 = text.chars().map(|c| if c.is_ascii() { 0.62 * em } else { em }).sum();
    (advance + 2.0 * 12.0 + 2.0 + 4.0).ceil().max(44.0)
}

/// What a line of segments may fill: the row's width less one gap, so a line that the estimate
/// gets slightly wrong still does not touch the edge.
pub fn seg_line_room(avail: f32) -> f32 {
    (avail - SP[1]).max(0.0)
}

/// The segments of a choice row packed into lines no wider than `avail`, as the mockup's `.seg`
/// wraps (`flex-wrap: wrap`, `gap: 8px`), so every option and the chosen one stay in view.
/// v0.0.7: #521 fixed (`FlowRow`); workaround kept until checked on a display (the toolkit's
/// line breaking must match the mockup's `gap: 8px` wrap at 390 and 1440), so the packing is by
/// estimated width. Each line
/// holds indices into `labels`, in order; a segment wider than `avail` gets a line of its own.
pub fn seg_lines<S: AsRef<str>>(labels: &[S], avail: f32) -> Vec<Vec<usize>> {
    let avail = seg_line_room(avail);
    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut used = 0.0_f32;
    for (i, l) in labels.iter().enumerate() {
        let w = seg_width(l.as_ref());
        match lines.last_mut() {
            Some(line) if used + SP[1] + w <= avail => {
                line.push(i);
                used += SP[1] + w;
            }
            _ => {
                lines.push(vec![i]);
                used = w;
            }
        }
    }
    lines
}

/// The fields' width in the 390 phone's sheet: the window less the sheet's 20 on each side.
pub const NARROW_SHEET_FIELD_W: f32 = 390.0 - 2.0 * SP[4];

/// The width the form's fields get: the detail panel less its 1 dp border and 16 padding on
/// each side inline, or the sheet less its 20 padding on each side.
pub fn form_field_width(window_w: f32, panel_w: f32, layout: FormLayout) -> f32 {
    match layout {
        FormLayout::Inline => panel_w - 2.0 - 2.0 * 16.0,
        // An unknown window (0 before the first size event) packs against the narrowest sheet,
        // the 390 phone's, so nothing runs off it and the lines are not one option each.
        FormLayout::Sheet if window_w <= 0.0 => NARROW_SHEET_FIELD_W,
        FormLayout::Sheet => {
            super::components::sheet_width(window_w, super::components::SHEET_WIDTH_DEFAULT) - 2.0 * SP[4]
        }
    }
    .max(0.0)
}

/// Choice segments in wrapped lines (`seg_lines`): `labels[i]` is chosen when `selected(i)`,
/// and a tap calls `on_pick(i)`.
fn SegChoices(
    labels: &[String],
    avail: f32,
    selected: impl Fn(usize) -> bool,
    on_pick: impl Fn(usize) + Clone + 'static,
) {
    let lines = seg_lines(labels, avail);
    cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
        for line in &lines {
            cu::Row().spacing(SP[1]).content(|| {
                for &i in line {
                    let pick = on_pick.clone();
                    FilterSegButton(&labels[i], selected(i), move || pick(i));
                }
            });
        }
    });
}

/// A segment pill button matching the mockup (`.seg button`): 32px height, pill shape, 13/500
/// text, ink fill when selected, surface with line border when unselected. The list's project
/// filter and the form's one-of choices (프로젝트, 트리거, 컴퓨터) both use it: the shared
/// `PickChip` is the composer's dropdown chip (`.pickchip`), whose label takes the toolkit's
/// button type and whose chosen state is `sel` on `surface-2`, too faint to read as chosen
/// (#493).
fn FilterSegButton(text: &str, selected: bool, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let (bg, border, fg) = seg_colors(selected, p);
    let width = seg_width(text);
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
            Label(text, SEG_FS, Weight::Medium, Some(paint(fg)), Some(1));
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

    let (chart_w, chart_h, chart_gap) = weekday_chart_size(compact);
    let totals: Vec<usize> = bars.iter().map(|b| b.ok + b.fail).collect();
    let mut chart = DrawList::builder().rect(paint(p.line), 0.0, chart_h, chart_w, 1.0, 0.0);
    for (b, (x, y, w, h)) in bars.iter().zip(bar_rects(&totals, chart_w, chart_h, chart_gap)) {
        if h <= 0.0 {
            continue;
        }
        let total = (b.ok + b.fail) as f32;
        let fail_h = h * b.fail as f32 / total;
        let r = weekday_bar_radius(w, h);
        chart = chart.round_rect(paint(p.blue.strong), x, y + fail_h, w, h - fail_h, r, 0.0);
        if fail_h > 0.0 {
            chart = chart.round_rect(paint(p.orange.strong), x, y, w, fail_h, r, 0.0);
        }
    }
    let chart_cmd = chart.build();
    let card_h = weekday_card_height(compact).max(174.0);
    let busy = totals.iter().any(|t| *t > 0);

    let kpis_content = move || {
        // The two KPI rows sit together at the top of the card (`.rt-topwrap .kpis` row gap
        // `--sp4`), not spread over the height the chart card beside it gives (design review B5).
        cu::Column()
            .spacing(ROUTINE_KPI_ROW_GAP)
            .modifier(Modifier::new().fill_max_width().height(card_h))
            .content(|| {
                cu::Row().modifier(Modifier::new().fill_max_width()).content(|| {
                    RoutineKpiView(&k[0], 0);
                    RoutineKpiView(&k[1], 1);
                });
                cu::Row().modifier(Modifier::new().fill_max_width()).content(|| {
                    RoutineKpiView(&k[2], 2);
                    RoutineKpiView(&k[3], 3);
                });
            });
    };

    let hero_content = move || {
        HeroCardContent(&ring_cmd, rate, &chart_cmd, &bars, busy, chart_w, chart_h, card_h);
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
    card_h: f32,
) {
    let p = ember_theme().palette;
    cu::Row()
        .spacing(SP[3])
        .alignment(Alignment::CenterStart)
        .modifier(Modifier::new().fill_max_width().height(card_h))
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

/// A routine KPI number: `.kpi .v` 28/400 (`style.css:110`, `routines.css:64`), design review B5.
pub const ROUTINE_KPI_WEIGHT: Weight = Weight::Regular;
/// The gap between the two KPI rows (`.rt-topwrap .kpis` gap `--sp4`).
pub const ROUTINE_KPI_ROW_GAP: f32 = 16.0;
/// The detail pane's title: 15/600 (`DESIGN.md` 6.5), design review B5.
pub const ROUTINE_DETAIL_TITLE: (Fs, Weight) = (Fs::Lg, Weight::Semibold);

// TODO(compose-rust#443): `Text` has no `overflow_wrap`, so a KPI label stays on one line with
// `max_lines(Some(1))` instead of being allowed to wrap in a narrow column.
fn RoutineKpiView(kpi: &RoutineKpi, index: usize) {
    let p = ember_theme().palette;
    cu::Column().modifier(Modifier::new().weight(1.0)).spacing(SP[0]).content(|| {
        Label(kpi.label, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
        if index < 3 {
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
                Label(&kpi.value, Fs::Xl2, ROUTINE_KPI_WEIGHT, None, Some(1));
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
            Label(&kpi.value, Fs::Xl2, ROUTINE_KPI_WEIGHT, None, Some(1));
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
                // v0.0.7: #328 fixed (labels stay on one line, and a weighted child keeps its
                // longest word, #427); workaround kept until checked on a display (whether the
                // sixth filter chip still clips at 1440; it is reachable by scrolling).
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
                SearchField("루틴 검색", H_BTN, move |v| q_set.set(v));
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
    panel_w: f32,
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
    // A run in progress (the optimistic row or a server run still Running) disables 지금 실행,
    // so a second click cannot start a second run (FR-L25, #494).
    let busy = run_now_busy(entry);

    let layout = detail_header_layout(panel_w, compact);
    let tile_cols = detail_tile_columns(panel_w);
    let demo = demo_active();
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
    let sched_text = if routine_trigger == Trigger::Time && cron_valid(&routine_cron) {
        format!("{routine_line} · {routine_cron}")
    } else {
        routine_line
    };

    let on_toggle = std::sync::Arc::new(on_toggle);
    let on_edit = std::sync::Arc::new(on_edit);
    let on_run = std::sync::Arc::new(on_run);
    let routine_name_c = routine_name.clone();
    let routine_name_w = routine_name.clone();
    let toggle_id_c = toggle_id.clone();
    let edit_id_c = edit_id.clone();
    let run_id_c = run_id.clone();
    let on_toggle_c = std::sync::Arc::clone(&on_toggle);
    let on_edit_c = std::sync::Arc::clone(&on_edit);
    let on_run_c = std::sync::Arc::clone(&on_run);

    EmberCard(false, true, None, Some(0.0), None, move || {
        cu::Column().modifier(Modifier::new().fill_max_width()).content(move || {
            // Header: Topbar with bottom border
            if layout == DetailHeader::Icons {
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
                    .modifier(Modifier::new().fill_max_width().height(DETAIL_HEADER_H).padding(PAD_ROW_V))
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
                        // Busy: the muted fill of a disabled primary button (`button_style_for`)
                        // with a spinner in place of the play glyph.
                        let run_fill = if busy { p.muted } else { p.ink };
                        cu::Box()
                            .modifier(
                                Modifier::new()
                                    .size(32.0, 32.0)
                                    .background(paint(run_fill))
                                    .border(1.0, paint(run_fill))
                                    .corner_radius(CORNER_SM),
                            )
                            .alignment(Alignment::Center)
                            .content(move || {
                                if busy {
                                    Spinner(14.0);
                                    DisabledOverlay(RUN_NOW_BUSY_LABEL);
                                } else {
                                    NavIcon(ICON_PLAY_SVG, p.ink_text.to_argb(), 16.0);
                                    FocusOverlay("지금 실행", move || run_fn(run_id_c.clone()));
                                }
                            });
                    });
            } else if layout == DetailHeader::Stacked {
                // Narrow panel (768): the title and pill on top, the actions on their own row,
                // so none of them is clipped.
                let toggle_fn = on_toggle;
                let edit_fn = on_edit;
                let run_fn = on_run;
                let title_name = routine_name.clone();
                cu::Column()
                    .spacing(SP[2])
                    .modifier(Modifier::new().fill_max_width().padding(SP[3]))
                    .content(move || {
                        cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(move || {
                            cu::Column().modifier(Modifier::new().weight(1.0)).content(move || {
                                Label(&title_name, Fs::Lg, Weight::Semibold, None, Some(2));
                            });
                            TriggerPill(routine_trigger, true);
                        });
                        cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(move || {
                            RoutineSwitch(&routine_name, routine_enabled, move |v| toggle_fn(toggle_id.clone(), v));
                            cu::Spacer().modifier(Modifier::new().weight(1.0));
                            AppButton("편집", None, ButtonKind::Default, true, true, false, move || edit_fn(edit_id.clone()));
                            RunNowButton(busy, move || run_fn(run_id.clone()));
                        });
                    });
            } else {
                let toggle_fn = on_toggle;
                let edit_fn = on_edit;
                let run_fn = on_run;
                // 56 high as the mockup's `.rt-dh`: 12 above and below the 32 dp buttons, and
                // 4 more on each side for the card's 16.
                cu::Row()
                    .alignment(Alignment::CenterStart)
                    .spacing(SP[2])
                    .modifier(Modifier::new().fill_max_width().height(DETAIL_HEADER_H).padding(PAD_ROW_V))
                    .content(move || {
                        cu::Spacer().modifier(Modifier::new().width(SP[0]));
                        // The title takes what the actions leave and ellipsizes, so the actions
                        // never leave the panel.
                        cu::Column().modifier(Modifier::new().weight(1.0)).content(move || {
                            Label(&routine_name, ROUTINE_DETAIL_TITLE.0, ROUTINE_DETAIL_TITLE.1, None, Some(1));
                        });
                        TriggerPill(routine_trigger, true);
                        RoutineSwitch(&routine_name_w, routine_enabled, move |v| toggle_fn(toggle_id.clone(), v));
                        AppButton("편집", None, ButtonKind::Default, true, true, false, move || edit_fn(edit_id.clone()));
                        RunNowButton(busy, move || run_fn(run_id.clone()));
                        cu::Spacer().modifier(Modifier::new().width(SP[0]));
                    });
            }
            cu::Spacer().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));

            cu::Column()
                .spacing(DETAIL_GAP)
                .modifier(Modifier::new().fill_max_width().padding(detail_body_pad(compact)))
                .content(move || {
                    // Schedule & Ring
                    cu::Row().alignment(Alignment::CenterStart).spacing(SP[3]).modifier(Modifier::new().fill_max_width()).content(move || {
                        cu::Column().modifier(Modifier::new().weight(1.0)).spacing(SP[0]).content(move || {
                            Label(&when_big, if routine_trigger == Trigger::Event { Fs::Xl2 } else { detail_when_fs(compact) }, Weight::Semibold, None, Some(when_big_max_lines(routine_trigger)));
                            // The cron sits on the schedule's line, as the mockup's `.rt-sched`
                            // ("매일 오전 9시 · 0 9 * * *"), not on a line of its own.
                            Label(&sched_text, Fs::Sm, Weight::Medium, None, Some(1));
                            Label(&next, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                        });
                        cu::Box()
                            .modifier(Modifier::new().size(DETAIL_RING, DETAIL_RING))
                            .alignment(Alignment::Center)
                            .content(move || {
                                cu::Canvas(ring_cmd).modifier(Modifier::new().size(DETAIL_RING, DETAIL_RING));
                                Label(
                                    &rate.map(|v| format!("{v}%")).unwrap_or_else(|| "-".to_string()),
                                    Fs::Md,
                                    Weight::Semibold,
                                    None,
                                    None,
                                );
                            });
                    });

                    // Inset cards: 3 cards in one row on wide, 2x2 grid on narrow so the run
                    // history below stays visible above the fold on 390 dp phone screens.
                    if tile_cols < 3 {
                        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(move || {
                            cu::Row().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(move || {
                                cu::Column().modifier(Modifier::new().weight(1.0)).content(move || {
                                    KeyValue("프로젝트", &project_name, "");
                                });
                                cu::Column().modifier(Modifier::new().weight(1.0)).content(move || {
                                    KeyValue("에이전트", &harness_label, &model_title);
                                });
                            });
                            cu::Row().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(move || {
                                cu::Column().modifier(Modifier::new().weight(1.0)).content(move || {
                                    KeyValue("컴퓨터 · 프로필", &computer, "프로필은 하네스를 따릅니다");
                                });
                                cu::Column().modifier(Modifier::new().weight(1.0)).content(move || {
                                    KeyValue("권한 모드", &mode_label, "");
                                });
                            });
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
                    // The inset pads 12 16 itself (`.inset`); no second padding inside it.
                    InsetPanel(true, move || {
                        cu::Column().spacing(SP[0]).modifier(Modifier::new().fill_max_width()).content(move || {
                            Label("프롬프트", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                            Label(&prompt_text, Fs::Sm, Weight::Regular, None, None);
                        });
                    });
                    // The heading sits 8 above its rows (mockup `.runtiles { margin-top: 8px }`),
                    // at the body's 14 semibold (`<b>`).
                    cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(move || {
                        Label("실행 기록", Fs::Md, Weight::Semibold, None, Some(1));
                        if entry.runs.is_empty() {
                            Label("아직 실행하지 않았습니다", Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
                        }
                        for run in &entry.runs {
                            RunTile(run, now, run_is_simulated(run, demo));
                        }
                    });
                });
        });
    });
}

fn KeyValue(title: &str, value: &str, sub: &str) {
    let p = ember_theme().palette;
    let t = title.to_string();
    let v = value.to_string();
    let s = sub.to_string();
    // The inset pads 12 16 itself (`.inset`); a second padding inside doubled it (#494).
    InsetPanel(true, move || {
        cu::Column()
            .spacing(SP[0])
            .modifier(Modifier::new().fill_max_width())
            .content(move || {
                Label(&t, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                // Two lines, then an ellipsis: the tile is at least a third of the panel.
                Label(&v, Fs::Sm, Weight::Medium, None, Some(2));
                if !s.is_empty() {
                    Label(&s, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
                }
            });
    });
}

fn RunTile(run: &Run, now: Minutes, simulated: bool) {
    let p = ember_theme().palette;
    let tone = status_tone(run.status);
    let session = run.session.clone();
    let row = run_row_text(run, now, simulated);
    let (status_lbl, ago_lbl) = (row.status, row.ago);
    // `RUN_TILE_H` high: the inset's own 12 above and below a row the button's height.
    InsetPanel(true, move || {
        cu::Row()
            .alignment(Alignment::CenterStart)
            .spacing(SP[2])
            .modifier(Modifier::new().fill_max_width().height(H_CTL))
            .content(move || {
                Pill(status_lbl, tone, true);
                if simulated {
                    Pill(SAMPLE_FLAG, PillTone::Grey, true);
                }
                Label(&ago_lbl, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(1));
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                if let Some(id) = session {
                    AppButton("세션 열기", None, ButtonKind::Default, true, true, false, move || {
                        if let Some(svc) = try_services() {
                            let target = resolve_run_session_id(&id, demo_active());
                            svc.ui.open_conversation(&target);
                        }
                    });
                }
            });
    });
}

// ---------------------------------------------------------------------------------------------
// Routine form
// ---------------------------------------------------------------------------------------------

/// The state and handlers both form shells (inline panel, sheet) share, so the save path is
/// written once (FR-L25, #511).
#[derive(Clone)]
struct FormCtx {
    /// The fields' width, for wrapping the choice segments (`form_field_width`). The screen
    /// passes it in: inside the sheet's content the window size reads 0 (#493).
    field_w: f32,
    draft: MutableState<FormDraft>,
    tried: MutableState<bool>,
    initial: FormDraft,
    editing: bool,
    computers: Vec<(String, String)>,
    projects: Vec<String>,
}

/// The 저장 handler of either shell: validate, then hand the draft to the one save path.
fn form_save_handler(
    draft: MutableState<FormDraft>,
    tried: MutableState<bool>,
    mut on_save: impl FnMut(FormDraft) + 'static,
) -> impl FnMut() + 'static {
    move || {
        let cur = draft.get();
        if cur.errors().is_empty() {
            on_save(cur);
        } else {
            tried.set(true);
        }
    }
}

fn RoutineForm(
    initial: FormDraft,
    editing: Option<String>,
    _server: bool,
    computers: &[(String, String)],
    projects: &[String],
    layout: FormLayout,
    field_w: f32,
    draft: &MutableState<FormDraft>,
    on_save: impl FnMut(FormDraft) + Clone + 'static,
    on_close: impl FnMut() + Clone + 'static,
) {
    let p = ember_theme().palette;
    let draft = draft.clone();
    let tried = remember(|| mutable_state_of(false));
    let ctx = FormCtx {
        field_w,
        draft,
        tried,
        initial: initial.clone(),
        editing: editing.is_some(),
        computers: computers.to_vec(),
        projects: projects.to_vec(),
    };
    let title = if editing.is_some() {
        "루틴 편집"
    } else {
        "새 루틴"
    };

    if layout == FormLayout::Inline {
        // The detail area: a card with the title, 취소 and 저장 in its header, then the fields
        // in the page's own scroll (mockup v8: the form replaces the detail panel).
        let (close, save) = (on_close.clone(), form_save_handler(ctx.draft.clone(), ctx.tried.clone(), on_save.clone()));
        EmberCard(false, true, None, Some(0.0), None, move || {
            cu::Column().modifier(Modifier::new().fill_max_width()).content(move || {
                cu::Row()
                    .alignment(Alignment::CenterStart)
                    .spacing(SP[2])
                    .modifier(Modifier::new().fill_max_width().height(64.0).padding(16.0))
                    .content(move || {
                        cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                            Label(title, Fs::Xl, Weight::Semibold, None, Some(1));
                        });
                        let mut close = close;
                        AppButton("취소", None, ButtonKind::Default, true, true, false, move || close());
                        AppButton("저장", None, ButtonKind::Primary, true, true, false, save);
                    });
                cu::Spacer().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
                // The first error also sits under the header buttons once a save was tried, so
                // it is seen without scrolling to the end of the form.
                if ctx.tried.get() {
                    if let Some(e) = ctx.draft.get().errors().first() {
                        cu::Column().modifier(Modifier::new().fill_max_width().padding(16.0)).content(|| {
                            Label(e.message(), Fs::Sm, Weight::Medium, Some(paint(p.red.strong)), Some(2));
                        });
                    }
                }
                cu::Column()
                    .spacing(SP[3])
                    .modifier(Modifier::new().fill_max_width().padding(16.0))
                    .content(move || {
                        FormFields(&ctx);
                        // The last field clears the page bottom.
                        cu::Spacer().modifier(Modifier::new().height(SP[4]));
                    });
            });
        });
        return;
    }

    let body_height = {
        let w = current_window_size();
        super::components::sheet_body_max_height(w.width_dp, w.height_dp, false, 60.0 + 65.0).min(600.0)
    };
    EmberSheet(title, true, on_close.clone(), None, move || {
        cu::ScrollColumn()
            .modifier(Modifier::new().fill_max_width().height(body_height))
            .content(|| {
                cu::Column()
                    .spacing(SP[3])
                    .modifier(Modifier::new().fill_max_width().padding(SP[4]))
                    .content(|| FormFields(&ctx));
            });
        SheetFooter(move || {
            let mut close = on_close.clone();
            AppButton("취소", None, ButtonKind::Default, true, true, false, move || close());
            AppButton("저장", None, ButtonKind::Primary, true, true, false, form_save_handler(ctx.draft.clone(), ctx.tried.clone(), on_save.clone()));
        });
    });
}

/// The fields, in `FORM_FIELDS` order, for both shells.
fn FormFields(ctx: &FormCtx) {
    let p = ember_theme().palette;
    let field_w = ctx.field_w;
    let (draft, tried, initial) = (&ctx.draft, &ctx.tried, &ctx.initial);
    let editing = ctx.editing;
    let agents = live_agents();
    let d = draft.get();
    let errors = d.errors();
    let mut project_choices = project_options(&ctx.projects);
    if !project_choices.contains(&d.sel.project) {
        project_choices.push(d.sel.project.clone());
    }
    let allowed: Vec<Harness> = Harness::ALL
        .iter()
        .copied()
        .filter(|h| harness_block(*h, &agents, &[]).is_none())
        .collect();
    let models = model_choices(&model_catalog(), &allowed, d.sel.option.harness, false);
    let model_labels: Vec<String> = models.iter().map(|o| o.chip_label()).collect();
    let model_at = models.iter().position(|o| *o == d.sel.option).unwrap_or(0);
    let modes: Vec<crate::ui::home::PermMode> = modes_for(d.sel.option.harness).to_vec();
    let mode_labels: Vec<String> = modes.iter().map(|m| m.label().to_string()).collect();
    let mode_at = modes.iter().position(|m| *m == d.sel.mode).unwrap_or(0);
    let schedule_hint = schedule_description(d.trigger, &d.cron, now_local());

    if editing {
        Label(
            "바꾸지 않은 항목은 지금 값을 유지합니다",
            Fs::Xs,
            Weight::Regular,
            Some(paint(p.muted)),
            None,
        );
    }
    for f in FORM_FIELDS {
        match f {
            FormField::Name => {
                FormLabel("이름");
                let draft_name = draft.clone();
                EmberField(FieldOptions {
                    fill_max_width: true,
                    text: Some(d.name.clone()),
                    placeholder: if editing {
                        "바꾸려면 새 이름을 입력하세요".to_string()
                    } else {
                        "예: develop CI 실패 요약".to_string()
                    },
                    on_value_change: Some(Box::new(move |v| {
                        draft_name.update(|d| d.name = v);
                    })),
                    ..Default::default()
                });
            }
            FormField::Project => {
                FormLabel("프로젝트");
                // Wrapped, never scrolled sideways, so the chosen one is always in view (#493).
                let (sel_proj, names) = (draft.clone(), project_choices.clone());
                SegChoices(&project_choices, field_w, |i| d.sel.project == project_choices[i], move |i| {
                    let n = names[i].clone();
                    sel_proj.update(|d| d.sel.project = n);
                });
            }
            FormField::Trigger => {
                FormLabel("트리거");
                let labels: Vec<String> = Trigger::ALL.iter().map(|t| t.label().to_string()).collect();
                let sel_trig = draft.clone();
                SegChoices(&labels, field_w, |i| d.trigger == Trigger::ALL[i], move |i| {
                    let t = Trigger::ALL[i];
                    sel_trig.update(|d| *d = d.with_trigger(t));
                });
            }
            FormField::ScheduleDescription => {
                // Read only: the server stores no description, so this is the schedule in words.
                FormLabel("일정 설명");
                Label(&schedule_hint, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(2));
            }
            FormField::Schedule => {
                FormLabel(if d.trigger == Trigger::Time { "cron" } else { "이벤트" });
                let sel_cron = draft.clone();
                let ph = if editing && initial.trigger == d.trigger {
                    "바꾸려면 새 값을 입력하세요".to_string()
                } else if d.trigger == Trigger::Time {
                    "0 9 * * *".to_string()
                } else {
                    "예: PR이 열릴 때".to_string()
                };
                EmberField(FieldOptions {
                    fill_max_width: true,
                    text: Some(d.cron.clone()),
                    placeholder: ph,
                    on_value_change: Some(Box::new(move |v| {
                        sel_cron.update(|d| d.cron = v);
                    })),
                    ..Default::default()
                });
            }
            FormField::Model => {
                FormLabel("하네스 · 모델 · 추론 강도");
                let (pick, choices) = (draft.clone(), models.clone());
                Select(&model_labels[model_at.min(model_labels.len().saturating_sub(1))], &model_labels, model_at, 320.0, move |i| {
                    if let Some(o) = choices.get(i).copied() {
                        pick.update(|d| {
                            d.sel = d.sel.with_harness(o.harness).with_option(o);
                        });
                    }
                });
                Label("프로필은 하네스를 따릅니다. 계정을 따로 고르지 않습니다", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
            }
            FormField::Mode => {
                FormLabel("권한 모드");
                let (pick, choices) = (draft.clone(), modes.clone());
                Select(&mode_labels[mode_at], &mode_labels, mode_at, 300.0, move |i| {
                    if let Some(m) = choices.get(i).copied() {
                        pick.update(|d| d.sel = d.sel.with_mode(m));
                    }
                });
            }
            FormField::Computer => {
                FormLabel("컴퓨터");
                // "이 서버" first, then the computers; `None` is this server.
                let ids: Vec<Option<String>> =
                    std::iter::once(None).chain(ctx.computers.iter().map(|(id, _)| Some(id.clone()))).collect();
                let labels: Vec<String> = std::iter::once("이 서버".to_string())
                    .chain(ctx.computers.iter().map(|(_, name)| name.clone()))
                    .collect();
                let (sel_c, pick_ids) = (draft.clone(), ids.clone());
                SegChoices(&labels, field_w, |i| d.sel.computer == ids[i], move |i| {
                    let c = pick_ids[i].clone();
                    sel_c.update(|d| d.sel.computer = c);
                });
            }
            FormField::Prompt => {
                FormLabel("프롬프트");
                let sel_p = draft.clone();
                EmberField(FieldOptions {
                    fill_max_width: true,
                    height: Some(120.0),
                    multiline: true,
                    text: Some(d.prompt.clone()),
                    placeholder: if editing {
                        "바꾸려면 새 지시를 입력하세요".to_string()
                    } else {
                        "에이전트에게 매번 줄 지시를 적어 주세요".to_string()
                    },
                    on_value_change: Some(Box::new(move |v| {
                        sel_p.update(|d| d.prompt = v);
                    })),
                    ..Default::default()
                });
            }
        }
    }
    if tried.get() {
        for e in errors {
            Label(e.message(), Fs::Xs, Weight::Regular, Some(paint(p.red.strong)), None);
        }
    }
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
                continue_session: None,
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
                continue_session: None,
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
                continue_session: None,
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
                continue_session: None,
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
                continue_session: None,
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
                continue_session: None,
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
                continue_session: None,
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
            continue_session: None,
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
    fn fr_l25_search_field_fits_the_content_width_at_390() {
        let content = 390.0 - 2.0 * SP[3];
        let w = search_field_width(390.0, true);
        assert!(w > 0.0 && w <= content, "search field {w} exceeds content {content}");
        for width in [320.0, 360.0, 390.0, 430.0] {
            assert!(search_field_width(width, true) <= width - 2.0 * SP[3]);
        }
        assert!(compact_bottom_clearance() >= TABBAR_H + TABBAR_BOTTOM_GAP);
    }

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
            "완료 · 3시간 전"
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

    // ---- detail layout by width (#492) -------------------------------------------------------

    /// The content width the screen has at a window width: the window less the navigation (234
    /// sidebar from 1280, 72 rail from 600, none on a phone) and the page padding.
    fn content_at(w: f32) -> (f32, bool) {
        let compact = w < 600.0;
        let nav = if w >= 1280.0 { 234.0 } else if compact { 0.0 } else { 72.0 };
        let pad = if compact { SP[3] } else { SP[6] };
        (w - nav - 2.0 * pad, compact)
    }

    #[test]
    fn fr_l25_form_is_inline_where_the_detail_area_exists_and_a_sheet_when_compact() {
        for (w, expect) in [
            (1440.0, FormLayout::Inline),
            (1024.0, FormLayout::Inline),
            (768.0, FormLayout::Inline),
            (390.0, FormLayout::Sheet),
        ] {
            let (_, compact) = content_at(w);
            // New is pressed from the list, edit from an open detail: both layouts agree.
            for open in [false, true] {
                assert_eq!(form_layout(pane_mode(compact, open)), expect, "width {w}, open {open}");
            }
        }
    }

    #[test]
    fn fr_l25_form_is_dirty_when_any_field_differs_from_where_it_opened() {
        let init = FormDraft::new("ember");
        assert!(!routine_form_dirty(&init, &init.clone()));
        let mut d = init.clone();
        d.name = "a".into();
        assert!(routine_form_dirty(&init, &d));
        d.name.clear();
        assert!(!routine_form_dirty(&init, &d), "typing and deleting again is clean");
        for edit in [
            |d: &mut FormDraft| d.cron = "0 10 * * *".into(),
            |d: &mut FormDraft| d.prompt = "x".into(),
            |d: &mut FormDraft| d.sel.project = "core".into(),
            |d: &mut FormDraft| d.sel.computer = Some("c1".into()),
            |d: &mut FormDraft| d.sel = d.sel.with_harness(Harness::Codex),
            |d: &mut FormDraft| *d = d.with_trigger(Trigger::Event),
        ] {
            let mut d = init.clone();
            edit(&mut d);
            assert!(routine_form_dirty(&init, &d));
        }
        let mut m = init.clone();
        m.sel = m.sel.with_mode(PermMode::Plan);
        assert!(routine_form_dirty(&init, &m));
    }

    #[test]
    fn fr_l25_every_route_out_of_a_dirty_form_asks_and_a_clean_one_goes() {
        assert_eq!(leave_decision(true, true, false), LeaveDecision::Ask);
        assert_eq!(leave_decision(true, false, false), LeaveDecision::Go);
        assert_eq!(leave_decision(false, true, false), LeaveDecision::Go, "no form, nothing to lose");
        assert_eq!(leave_decision(true, true, true), LeaveDecision::Block);
        assert_eq!(leave_decision(true, false, true), LeaveDecision::Block);
    }

    #[test]
    fn fr_l25_form_routes_all_go_through_request_leave() {
        let src = include_str!("routines.rs");
        let screen = src.split("pub fn RoutinesScreenWith").nth(1).unwrap().split("pub fn preview_routines").next().unwrap();
        for route in ["Leave::NewForm", "Leave::OpenRoutine", "Leave::Cancel", "Leave::EditForm"] {
            assert!(screen.contains(route), "{route}");
        }
        assert!(!screen.contains("pan.set(Panel::Edit"));
        assert!(!screen.contains("panel_new.set"));
        assert_eq!(ROUTINE_DISCARD_QUESTION, "변경 사항을 버릴까요?");
    }

    /// The form's one-of choices are the mockup's `.seg` buttons: 13/500 text, and the chosen
    /// one in `ink`, not the composer's dropdown chip with its faint `sel` fill (#493).
    #[test]
    fn fr_l25_form_choices_are_segments_with_an_ink_chosen_state() {
        assert_eq!((SEG_FS, SEG_FS.size()), (Fs::Sm, 13.0));
        for p in [&LIGHT, &DARK] {
            assert_eq!(seg_colors(true, p), (p.ink, p.ink, p.ink_text));
            assert_eq!(seg_colors(false, p), (p.surface, p.line, p.text_2));
        }
        let src = include_str!("routines.rs");
        let form = src.split("fn FormFields(").nth(1).unwrap().split("\nfn FormLabel(").next().unwrap();
        for field in ["FormField::Project", "FormField::Trigger", "FormField::Computer"] {
            let arm = form.split(field).nth(1).unwrap().split("FormField::").next().unwrap();
            assert!(arm.contains("SegChoices("), "{field} uses the wrapped segments");
            assert!(!arm.contains("ScrollRow("), "{field} never scrolls sideways");
            assert!(!arm.contains("PickChip("), "{field} does not use the dropdown chip");
        }
    }

    /// At 390 the 프로젝트 segments wrap onto more lines instead of scrolling, so every option
    /// and the chosen one are in view; no line is wider than the sheet's fields (#493).
    #[test]
    fn fr_l25_form_segments_wrap_to_the_field_width() {
        let projects = ["기본 프로젝트", "demo-project", "second-project", "ember"];
        let phone = form_field_width(390.0, 0.0, FormLayout::Sheet);
        assert_eq!(phone, 350.0);
        let (content, _) = content_at(1440.0);
        let wide = form_field_width(1440.0, detail_panel_width(1440.0, content, false), FormLayout::Inline);
        for avail in [phone, wide] {
            let lines = seg_lines(&projects, avail);
            let flat: Vec<usize> = lines.iter().flatten().copied().collect();
            assert_eq!(flat, vec![0, 1, 2, 3], "every option once, in order, at {avail}");
            for line in &lines {
                let w: f32 = line.iter().map(|&i| seg_width(projects[i])).sum::<f32>()
                    + SP[1] * (line.len() as f32 - 1.0);
                assert!(w <= avail, "a line of {w} fits {avail}");
            }
        }
        assert!(seg_lines(&projects, phone).len() >= 2, "the phone wraps");
        // The 390 sheet: 기본 프로젝트 and demo-project share a line; second-project and the
        // chosen ember go below, so ember is in view (r3 capture of #720 had all on one line).
        assert_eq!(seg_lines(&projects, phone), vec![vec![0, 1], vec![2, 3]]);
        for line in seg_lines(&projects, phone) {
            let w: f32 = line.iter().map(|&i| seg_width(projects[i])).sum::<f32>() + SP[1] * (line.len() as f32 - 1.0);
            assert!(w <= phone - SP[1], "a 390 line of {w} keeps a gap from the sheet's edge");
        }
        assert_eq!(
            form_field_width(0.0, 0.0, FormLayout::Sheet),
            NARROW_SHEET_FIELD_W,
            "an unknown window packs against the 390 sheet"
        );
        assert_eq!(seg_lines(&projects, NARROW_SHEET_FIELD_W), vec![vec![0, 1], vec![2, 3]]);
        // The screen works the width out before the sheet opens and hands it down.
        let src = include_str!("routines.rs");
        let fields = src.split("fn FormFields(").nth(1).unwrap().split("\nfn FormLabel(").next().unwrap();
        assert!(!fields.contains("current_window_size()"), "the fields never read the window inside the sheet");
        assert!(src.contains("let form_w_sheet = form_field_width(window.width_dp, panel_w, FormLayout::Sheet);"));
        assert!(src.contains("            form_w_sheet,\n            &form_draft,"), "the sheet gets the screen's width");
        assert_eq!(seg_lines(&projects, wide).len(), 1, "the wide inline form keeps one line");
        // A segment wider than the room still gets its own line rather than vanishing.
        assert_eq!(seg_lines(&["a-very-long-project-name-indeed"], 100.0), vec![vec![0]]);
    }

    #[test]
    fn fr_l25_form_fields_follow_the_mockup_order_with_one_model_picker() {
        assert_eq!(
            FORM_FIELDS,
            [
                FormField::Name,
                FormField::Project,
                FormField::Trigger,
                FormField::ScheduleDescription,
                FormField::Schedule,
                FormField::Model,
                FormField::Mode,
                FormField::Computer,
                FormField::Prompt,
            ]
        );
        // One picker for harness, model and effort: no separate harness field.
        assert_eq!(FORM_FIELDS.iter().filter(|f| **f == FormField::Model).count(), 1);
    }

    #[test]
    fn fr_l25_model_picker_lists_every_runnable_harness_but_one_for_a_server_routine() {
        let all = model_catalog();
        let free = model_choices(&all, &[Harness::ClaudeCode, Harness::Codex], Harness::ClaudeCode, false);
        assert!(free.iter().any(|o| o.harness == Harness::ClaudeCode));
        assert!(free.iter().any(|o| o.harness == Harness::Codex));
        assert!(!free.iter().any(|o| o.harness == Harness::Gemini));
        let server = model_choices(&all, &[Harness::ClaudeCode, Harness::Codex], Harness::Codex, true);
        assert!(!server.is_empty() && server.iter().all(|o| o.harness == Harness::Codex));
        // The current harness stays listed even when it cannot run now, so the picker shows it.
        let kept = model_choices(&all, &[], Harness::Codex, false);
        assert!(kept.iter().all(|o| o.harness == Harness::Codex) && !kept.is_empty());
    }

    #[test]
    fn fr_l25_schedule_description_reads_the_cron_and_the_event() {
        let now = at(2026, 11, 6, 8, 0);
        let t = schedule_description(Trigger::Time, "0 9 * * *", now);
        assert!(t.starts_with("매일 오전 9시 · 다음 실행"), "{t}");
        assert_eq!(schedule_description(Trigger::Time, "nope", now), describe_cron("nope"));
        assert_eq!(
            schedule_description(Trigger::Event, "pull_request.opened", now),
            "이벤트가 일어날 때 실행됩니다"
        );
    }

    #[test]
    fn fr_l25_event_title_wraps_and_a_time_stays_on_one_line() {
        assert_eq!(when_big_max_lines(Trigger::Time), 1);
        assert!(when_big_max_lines(Trigger::Event) > 1);
    }

    #[test]
    fn fr_l25_detail_layout_follows_the_panel_width() {
        let cases = [
            (1440.0, 722.0, DetailHeader::Inline, 3),
            (1024.0, 548.0, DetailHeader::Inline, 3),
            (768.0, 312.0, DetailHeader::Stacked, 1),
            (390.0, 358.0, DetailHeader::Icons, 1),
        ];
        for (w, panel, header, cols) in cases {
            let (content, compact) = content_at(w);
            let pw = detail_panel_width(w, content, compact);
            assert_eq!(pw, panel, "panel width at {w}");
            assert_eq!(detail_header_layout(pw, compact), header, "header at {w}");
            assert_eq!(detail_tile_columns(pw), cols, "tiles at {w}");
        }
    }

    /// The first 실행 기록 row shows without scrolling at 1440x900 and 390x844 (FR-L25, #494).
    /// The detail card's top is measured on the r3 captures of PR #721 (`running-1440-*.png`:
    /// 386; `done-390-*.png`: 84), since the dashboard above it is laid out by the toolkit. The
    /// prompt is given one line more than the sample prompt takes at each width, so the check
    /// errs on the safe side.
    #[test]
    fn fr_l25_first_run_row_is_visible_without_scrolling_at_1440_and_390() {
        // (window w, window h, card top, prompt lines, 컴퓨터 note lines, room kept at the bottom)
        let cases = [
            (1440.0, 900.0, 386.0, 2, 1, SP[6]),
            (390.0, 844.0, 84.0, 3, 2, compact_bottom_clearance()),
        ];
        for (w, h, top, prompt_lines, sub_lines, room) in cases {
            let (content, compact) = content_at(w);
            let pw = detail_panel_width(w, content, compact);
            let bottom = top + detail_first_run_bottom(pw, compact, prompt_lines, sub_lines);
            assert!(
                bottom <= h - room,
                "first run row ends at {bottom} dp at {w}x{h}, past {} dp",
                h - room
            );
        }
        // The pieces the sum is made of are the mockup's.
        assert_eq!(DETAIL_HEADER_H, 56.0, "`.rt-dh` min-height");
        assert_eq!(RUN_TILE_H, 56.0, "12 + 32 + 12");
        assert_eq!(detail_when_fs(false), Fs::Xl4);
        assert_eq!(detail_when_fs(true), Fs::Xl2, "28 px at 560 px and below");
    }

    #[test]
    fn fr_l25_detail_tiles_and_run_rows_are_padded_once() {
        let src = include_str!("routines.rs");
        let kv = src.split("fn KeyValue(").nth(1).unwrap().split("\n}\n").next().unwrap();
        let tile = src.split("fn RunTile(").nth(1).unwrap().split("\n}\n").next().unwrap();
        for (name, body) in [("KeyValue", kv), ("RunTile", tile)] {
            assert!(body.contains("InsetPanel(true"), "{name} sits in an inset");
            assert!(!body.contains(".padding("), "{name} adds no padding inside the inset's own");
        }
        assert!(tile.contains(".height(H_CTL)"), "a run row is as tall as its button");
    }

    #[test]
    fn fr_l25_detail_prompt_has_paragraph_width_at_every_size() {
        for w in [1440.0, 1024.0, 768.0, 390.0] {
            let (content, compact) = content_at(w);
            let pw = detail_panel_width(w, content, compact);
            let text = detail_prompt_width(pw, compact);
            assert!(text >= 200.0, "the prompt is {text} dp wide at {w}, not a 60 dp column");
        }
    }

    #[test]
    fn fr_l25_detail_inset_panel_fills_its_width() {
        let src = include_str!("components.rs");
        let inset = src.split("pub fn InsetPanel(").nth(1).unwrap().split("\n}\n").next().unwrap();
        assert!(inset.contains("cu::Row().modifier(Modifier::new().fill_max_width_if(fill_max_width))"));
        assert!(inset.contains("cu::Column().modifier(Modifier::new().weight(1.0)).content(content)"));
    }

    #[test]
    fn fr_l25_demo_run_button_reaches_the_simulation_only_in_the_demo() {
        let src = include_str!("routines.rs");
        let handler = src.split("let on_run_now = {").nth(1).unwrap().split("let on_save = {").next().unwrap();
        let demo = handler.find("demo_run_allowed(demo_active(), sample)").expect("gated on demo mode");
        let honest = handler.find("notice::post(RUN_NOTICE)").expect("the honest toast stays");
        assert!(demo < honest, "the demo branch comes first and the toast stays for no server");
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
                safe_area_top: 0.0,
                safe_area_leading: 0.0,
                safe_area_trailing: 0.0,
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
                safe_area_top: 0.0,
                safe_area_leading: 0.0,
                safe_area_trailing: 0.0,
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
                    safe_area_top: 0.0,
                    safe_area_leading: 0.0,
                    safe_area_trailing: 0.0,
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
        let switches: Vec<u32> = mutations
            .iter()
            .filter_map(|m| match m {
                Mutation::Create { node_id, widget: WidgetKind::Switch } => Some(*node_id),
                _ => None,
            })
            .collect();
        let switch_names: Vec<&str> = mutations
            .iter()
            .filter_map(|m| match m {
                Mutation::SetProp { node_id, property: PropertyKind::AccessibleName, value: PropertyValue::String(s) }
                    if switches.contains(node_id) => Some(*s),
                _ => None,
            })
            .collect();

        for e in &entries {
            let name = &e.routine.name;
            let has_row_button = button_labels.iter().any(|lbl| *lbl == name);
            assert!(
                has_row_button,
                "expected routine row button with exact name '{name}', found labels: {button_labels:?}"
            );

            // compose-rust#426 (v0.0.7): the switch is the toolkit's, named "<routine> 루틴".
            let want = routine_switch_name(name);
            let has_switch = switch_names.iter().any(|n| *n == want);
            assert!(
                has_switch,
                "expected a Switch named '{want}', found switches: {switch_names:?}"
            );
        }
        compose_rust::window::reset_window_size();
    }

    #[test]
    fn fr_l25_demo_routines_do_not_use_stale_remote_state() {
        assert!(!remote_schedule_allowed(true, true));
        assert!(!remote_schedule_allowed(true, false));
        assert!(remote_schedule_allowed(false, true));
        assert!(!remote_schedule_allowed(false, false));
    }

    #[test]
    fn fr_l25_demo_mode_uses_sample_entries_and_reset_clears_local_store() {
        assert!(should_use_sample_entries(Some("routines"), false));
        assert!(should_use_sample_entries(Some("routines"), true));
        assert!(should_use_sample_entries(None, true));
        assert!(should_use_sample_entries(Some("other"), true));
        assert!(!should_use_sample_entries(None, false));
        assert!(!should_use_sample_entries(Some("other"), false));

        let _guard = crate::services::test_guard();
        assert!(crate::services::test_services().demo.is_some());

        let prev_route = std::env::var("EMBER_CR_ROUTE").ok();
        unsafe {
            std::env::remove_var("EMBER_CR_ROUTE");
        }

        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| Box::new(Recomposer::new(|| RoutinesScreen())));
        let initial = host.rebuild().expect("rebuild");
        let mutations = decode_batch(initial).expect("decode");

        let buttons = extract_buttons(&mutations);
        let button_labels: Vec<&str> = buttons.iter().filter_map(|(_, t)| *t).collect();
        let has_ci = button_labels.iter().any(|lbl| *lbl == "develop CI 실패 요약")
            || mutations.iter().any(|m| match m {
                Mutation::SetProp {
                    property: PropertyKind::Text,
                    value: PropertyValue::String(s),
                    ..
                } => *s == "develop CI 실패 요약",
                Mutation::SetText { text, .. } => *text == "develop CI 실패 요약",
                _ => false,
            });
        assert!(
            has_ci,
            "expected demo routine 'develop CI 실패 요약' to be rendered, buttons: {button_labels:?}"
        );

        with_store(|s| {
            s.save(Routine {
                id: "test-rt-local".into(),
                name: "로컬 테스트 루틴".into(),
                trigger: Trigger::Time,
                cron: "0 0 * * *".into(),
                prompt: "로컬 프롬프트".into(),
                enabled: true,
                sel: Selection::initial(),
                continue_session: None,
            });
        });
        assert!(with_store(|s| s.entries().iter().any(|e| e.routine.id == "test-rt-local")));
        clear_local_store();
        assert!(with_store(|s| s.entries().is_empty()));

        with_store(|s| {
            s.save(Routine {
                id: "test-rt-local-2".into(),
                name: "로컬 테스트 루틴 2".into(),
                trigger: Trigger::Time,
                cron: "0 0 * * *".into(),
                prompt: "로컬 프롬프트 2".into(),
                enabled: true,
                sel: Selection::initial(),
                continue_session: None,
            });
        });
        assert!(with_store(|s| s.entries().iter().any(|e| e.routine.id == "test-rt-local-2")));
        crate::services::services().reset_demo();
        assert!(with_store(|s| s.entries().is_empty()));

        unsafe {
            if let Some(r) = prev_route {
                std::env::set_var("EMBER_CR_ROUTE", r);
            } else {
                std::env::remove_var("EMBER_CR_ROUTE");
            }
        }
        compose_rust::window::reset_window_size();
    }

    #[test]
    fn fr_l25_kpi_labels_set_max_lines_one_against_narrow_column_wrapping() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| Box::new(Recomposer::new(|| RoutinesScreenWith(sample_entries(), true))));
        let mutations = decode_batch(host.rebuild().expect("rebuild")).expect("decode");

        let kpi_labels = ["켜진 루틴", "지금 실행 중", "이번 주 실행", "다음 실행"];
        for label in kpi_labels {
            let node_ids: Vec<u32> = mutations
                .iter()
                .filter_map(|m| match m {
                    Mutation::SetProp {
                        node_id,
                        property: PropertyKind::Text,
                        value: PropertyValue::String(s),
                    } if *s == label => Some(*node_id),
                    _ => None,
                })
                .collect();

            assert!(
                !node_ids.is_empty(),
                "expected at least one node with Text '{label}'"
            );

            for node_id in node_ids {
                let has_max_lines_one = mutations.iter().any(|m| match m {
                    Mutation::SetProp {
                        node_id: n,
                        property: PropertyKind::MaxLines,
                        value: PropertyValue::Integer(1),
                    } if *n == node_id => true,
                    _ => false,
                });

                assert!(
                    has_max_lines_one,
                    "expected node {node_id} with Text '{label}' to have PropertyKind::MaxLines set to PropertyValue::Integer(1)"
                );
            }
        }
        compose_rust::window::reset_window_size();
    }

    #[test]
    fn fr_l25_host_edit_form_prefills_fields_and_allows_editing() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let entries = sample_entries();
        let target = &entries[0].routine;
        let initial = FormDraft::from_routine(target);
        let draft = mutable_state_of(initial.clone());
        let computers = vec![("mac-mini".to_string(), "Mac mini".to_string())];
        let projects = vec!["ember".to_string(), "core".to_string()];

        let mut host = Host::new({
            let initial = initial.clone();
            let draft = draft.clone();
            move || {
                let initial = initial.clone();
                let draft = draft.clone();
                let computers = computers.clone();
                let projects = projects.clone();
                Box::new(Recomposer::new(move || {
                    RoutineForm(
                        initial.clone(),
                        Some("rt1".to_string()),
                        true,
                        &computers,
                        &projects,
                        FormLayout::Inline,
                        form_field_width(1440.0, 742.0, FormLayout::Inline),
                        &draft,
                        |_| {},
                        || {},
                    );
                }))
            }
        });

        let mutations = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let text_fields: Vec<u32> = mutations
            .iter()
            .filter_map(|m| match m {
                Mutation::Create {
                    node_id,
                    widget: WidgetKind::TextField,
                } => Some(*node_id),
                _ => None,
            })
            .collect();
        assert_eq!(text_fields.len(), 3, "expected Name, Schedule, and Prompt TextFields");

        let field_texts: Vec<&str> = mutations
            .iter()
            .filter_map(|m| match m {
                Mutation::SetProp {
                    node_id,
                    property: PropertyKind::Text,
                    value: PropertyValue::String(s),
                } if text_fields.contains(node_id) => Some(*s),
                _ => None,
            })
            .collect();
        assert!(
            field_texts.contains(&target.name.as_str()),
            "expected Name TextField pre-filled with '{}', got {field_texts:?}",
            target.name
        );
        assert!(
            field_texts.contains(&target.cron.as_str()),
            "expected Schedule TextField pre-filled with '{}', got {field_texts:?}",
            target.cron
        );
        assert!(
            field_texts.contains(&target.prompt.as_str()),
            "expected Prompt TextField pre-filled with '{}', got {field_texts:?}",
            target.prompt
        );

        let any_disabled = mutations.iter().any(|m| match m {
            Mutation::SetProp {
                node_id,
                property: PropertyKind::Enabled,
                value: PropertyValue::Bool(false),
            } if text_fields.contains(node_id) => true,
            _ => false,
        });
        assert!(!any_disabled, "none of the edit form text fields should be disabled");
        compose_rust::window::reset_window_size();
    }

    #[test]
    fn fr_l25_host_open_session_button_resolves_demo_session_and_opens_conversation() {
        let _guard = crate::services::test_guard();
        let svc = crate::services::test_services();
        svc.reset_demo();
        compose_rust::window::reset_window_size();

        // The shown routine keeps only its newest run (session s9), so the detail has exactly one
        // 세션 열기: with all three (s9, s9, s6) the test took whichever a HashMap gave first and
        // opened demo-s6 in some runs.
        let entries = || {
            let mut all = sample_entries();
            all[0].runs.truncate(1);
            assert_eq!(all[0].runs[0].session.as_deref(), Some("s9"));
            all
        };
        let mut host = Host::new(move || Box::new(Recomposer::new(move || RoutinesScreenWith(entries(), true))));
        let mut texts: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
        let mut clickables: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
        let mut scan = |batch: &[Mutation<'_>]| {
            for m in batch {
                match m {
                    Mutation::SetProp { node_id, property: PropertyKind::Text, value: PropertyValue::String(s) } => {
                        texts.insert(*node_id, s.to_string());
                    }
                    Mutation::SetProp { node_id, property: PropertyKind::OnClick, value: PropertyValue::Integer(h) } => {
                        clickables.insert(*node_id, *h as u64);
                    }
                    _ => {}
                }
            }
        };
        scan(&decode_batch(host.rebuild().expect("rebuild")).expect("decode"));
        let resize = HostEvent {
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
        let (bytes, _) = host.dispatch(resize).expect("dispatch resize");
        scan(&decode_batch(bytes).expect("decode"));

        let open: Vec<u32> = texts
            .iter()
            .filter(|(id, t)| *t == "세션 열기" && clickables.contains_key(id))
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(open.len(), 1, "one run, one 세션 열기");
        let open_btn = open[0];
        let handler_id = clickables[&open_btn];
        host.dispatch(HostEvent {
            node_id: open_btn,
            handler_id,
            payload: EventPayload::Clicked,
        })
        .expect("dispatch click on 세션 열기");

        assert_eq!(
            svc.ui.route.get(),
            crate::ui::Route::Conversation("demo-s9".to_string()),
            "clicking '세션 열기' on sample run 's9' in demo mode should open conversation 'demo-s9'"
        );
        let is_open = svc.client.read(|st| {
            st.session("demo-s9")
                .is_some_and(|v| v.open && st.transcript("demo-s9").is_some())
        });
        assert!(is_open, "opening 'demo-s9' must mark it open and load its demo transcript");
        compose_rust::window::reset_window_size();
    }

    #[test]
    fn fr_l25_host_run_now_click_starts_running_state_and_posts_notice() {
        let _guard = crate::services::test_guard();
        let svc = crate::services::test_services();
        svc.reset_demo();
        compose_rust::window::reset_window_size();

        let mut host = Host::new(|| Box::new(Recomposer::new(|| RoutinesScreenWith(sample_entries(), true))));
        let mut texts: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
        let mut clickables: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
        let mut scan = |batch: &[Mutation<'_>]| {
            for m in batch {
                match m {
                    Mutation::SetProp { node_id, property: PropertyKind::Text, value: PropertyValue::String(s) } => {
                        texts.insert(*node_id, s.to_string());
                    }
                    Mutation::SetProp { node_id, property: PropertyKind::OnClick, value: PropertyValue::Integer(h) } => {
                        clickables.insert(*node_id, *h as u64);
                    }
                    _ => {}
                }
            }
        };
        scan(&decode_batch(host.rebuild().expect("rebuild")).expect("decode"));
        let resize = HostEvent {
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
        let (bytes, _) = host.dispatch(resize).expect("dispatch resize");
        scan(&decode_batch(bytes).expect("decode"));

        let run_btn = texts
            .iter()
            .find(|(id, t)| *t == "지금 실행" && clickables.contains_key(id))
            .map(|(id, _)| *id)
            .expect("expected clickable '지금 실행' button in detail header");
        let handler_id = clickables[&run_btn];
        let (after_click, _) = host
            .dispatch(HostEvent {
                node_id: run_btn,
                handler_id,
                payload: EventPayload::Clicked,
            })
            .expect("dispatch click on 지금 실행");
        let after_mutations = decode_batch(after_click).expect("decode after click");
        let after_text: String = after_mutations.iter().map(|m| format!("{m:?}")).collect();
        assert!(
            after_text.contains("실행 중") && after_text.contains(SAMPLE_FLAG),
            "clicking '지금 실행' must immediately render '실행 중' and '{SAMPLE_FLAG}' in the detail run history"
        );
        assert_eq!(
            svc.ui.notice.get().map(|(_, s)| s).as_deref(),
            Some(DEMO_RUN_STARTED),
            "clicking '지금 실행' must post DEMO_RUN_STARTED notice"
        );
        compose_rust::window::reset_window_size();
    }

    /// What a test has seen of the tree so far: text, click handler and enabled flag per node.
    #[derive(Default)]
    struct Seen {
        texts: std::collections::HashMap<u32, String>,
        clickables: std::collections::HashMap<u32, u64>,
        enabled: std::collections::HashMap<u32, bool>,
    }

    impl Seen {
        fn scan(&mut self, batch: &[Mutation<'_>]) {
            for m in batch {
                match m {
                    Mutation::SetProp { node_id, property: PropertyKind::Text, value: PropertyValue::String(s) } => {
                        self.texts.insert(*node_id, s.to_string());
                    }
                    Mutation::SetProp { node_id, property: PropertyKind::OnClick, value: PropertyValue::Integer(h) } => {
                        self.clickables.insert(*node_id, *h as u64);
                    }
                    Mutation::SetProp { node_id, property: PropertyKind::Enabled, value: PropertyValue::Bool(b) } => {
                        self.enabled.insert(*node_id, *b);
                    }
                    Mutation::Remove { node_id } => {
                        self.texts.remove(node_id);
                        self.clickables.remove(node_id);
                        self.enabled.remove(node_id);
                    }
                    _ => {}
                }
            }
        }
    }

    /// While the run it started is in progress, 지금 실행 is disabled and reads 실행 중, at the
    /// wide header and the phone's icon header alike (FR-L25, #494).
    #[test]
    fn fr_l25_run_now_is_disabled_while_its_run_is_in_progress() {
        for (w, h, class) in [(1440.0, 900.0, WindowSizeClass::Expanded), (390.0, 844.0, WindowSizeClass::Compact)] {
            let _guard = crate::services::test_guard();
            let svc = crate::services::test_services();
            svc.reset_demo();
            compose_rust::window::reset_window_size();

            let mut host = Host::new(|| Box::new(Recomposer::new(|| RoutinesScreenWith(sample_entries(), true))));
            let mut seen = Seen::default();
            seen.scan(&decode_batch(host.rebuild().expect("rebuild")).expect("decode"));
            let resize = HostEvent {
                node_id: 0,
                handler_id: 0,
                payload: EventPayload::WindowSizeChanged {
                    width_dp: w,
                    height_dp: h,
                    class,
                    height_class: WindowHeightClass::Medium,
                    safe_area_top: 0.0,
                    safe_area_leading: 0.0,
                    safe_area_trailing: 0.0,
                },
            };
            let (bytes, _) = host.dispatch(resize).expect("dispatch resize");
            seen.scan(&decode_batch(bytes).expect("decode"));
            if class == WindowSizeClass::Compact {
                // The phone shows the list first and the detail only for a chosen routine (no
                // routine is chosen by default there), so open the first one as a user would:
                // its row is a button named by the routine.
                let name = sample_entries()[0].routine.name.clone();
                let row = seen
                    .texts
                    .iter()
                    .find(|(id, t)| **t == name && seen.clickables.contains_key(id))
                    .map(|(id, _)| *id)
                    .unwrap_or_else(|| panic!("a clickable row named {name}"));
                let (bytes, _) = host
                    .dispatch(HostEvent { node_id: row, handler_id: seen.clickables[&row], payload: EventPayload::Clicked })
                    .expect("dispatch click on the routine row");
                seen.scan(&decode_batch(bytes).expect("decode"));
            }

            let run_btn = seen.texts
                .iter()
                .find(|(id, t)| *t == "지금 실행" && seen.clickables.contains_key(id))
                .map(|(id, _)| *id)
                .unwrap_or_else(|| panic!("a clickable 지금 실행 at {w}"));
            assert_ne!(seen.enabled.get(&run_btn), Some(&false), "지금 실행 starts enabled at {w}");
            let (bytes, _) = host
                .dispatch(HostEvent { node_id: run_btn, handler_id: seen.clickables[&run_btn], payload: EventPayload::Clicked })
                .expect("dispatch click on 지금 실행");
            seen.scan(&decode_batch(bytes).expect("decode"));

            let live_run_now: Vec<u32> = seen.texts
                .iter()
                .filter(|(id, t)| *t == "지금 실행" && seen.enabled.get(id) != Some(&false))
                .map(|(id, _)| *id)
                .collect();
            assert!(live_run_now.is_empty(), "no enabled 지금 실행 while the run is in progress at {w}");
            assert!(
                seen.texts.iter().any(|(id, t)| t == RUN_NOW_BUSY_LABEL && seen.enabled.get(id) == Some(&false)),
                "a disabled {RUN_NOW_BUSY_LABEL} button stands in for it at {w}"
            );
            compose_rust::window::reset_window_size();
        }
    }

    #[test]
    fn fr_l25_remote_run_now_live_progress_and_error_rollback() {
        let _guard = crate::services::test_guard();
        let svc = crate::services::test_services();
        with_store(|s| *s = LocalStore::default());
        with_remote(|r| *r = Remote::default());

        let state = std::sync::Arc::new(std::sync::Mutex::new((
            None::<ember_client::wire::RunStatus>,
            false,
        )));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        listener.set_nonblocking(true).expect("nonblocking");
        let addr = listener.local_addr().expect("local addr");
        let state_srv = std::sync::Arc::clone(&state);

        svc.rt.spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::from_std(listener).expect("tokio listener");
            while let Ok((mut stream, _)) = listener.accept().await {
                let state_req = std::sync::Arc::clone(&state_srv);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let first_line = req.lines().next().unwrap_or("");
                    let (run_st, fail_run) = *state_req.lock().unwrap();
                    let status_str = match run_st {
                        Some(ember_client::wire::RunStatus::Completed) => "completed",
                        Some(ember_client::wire::RunStatus::Running) | None => "running",
                        _ => "failed",
                    };
                    let (code, body) = if first_line.starts_with("POST /api/schedules/sch-live/run") {
                        if fail_run {
                            ("500 Internal Server Error", r#"{"error":"runner down"}"#.to_string())
                        } else {
                            (
                                "200 OK",
                                format!(
                                    r#"{{"id":"run-1","schedule_id":"sch-live","scheduled_at":1700000000000,"started_at":1700000000000,"ended_at":null,"status":"{status_str}","session_id":"ses-live-1","error":null}}"#
                                ),
                            )
                        }
                    } else if first_line.starts_with("GET /api/schedules/sch-live/runs") {
                        let runs_json = match (fail_run, run_st) {
                            (true, _) | (_, None) => "[]".to_string(),
                            (false, Some(_)) => format!(
                                r#"[{{"id":"run-1","schedule_id":"sch-live","scheduled_at":1700000000000,"started_at":1700000000000,"ended_at":1700000005000,"status":"{status_str}","session_id":"ses-live-1","error":null}}]"#
                            ),
                        };
                        ("200 OK", runs_json)
                    } else if first_line.starts_with("GET /api/schedules") {
                        (
                            "200 OK",
                            r#"[{"id":"sch-live","project":"ember","agent":"claude-code","computer_id":null,"cwd":"/tmp","prompt":"CI 요약\n자세히","kind":{"type":"cron","expr":"0 9 * * *","tz":"UTC"},"target":{"type":"new","title":"CI 요약"},"catch_up":false,"paused":false,"next_run_at":1700003600000,"last_run_at":null,"created_at":1700000000000}]"#
                                .to_string(),
                        )
                    } else {
                        ("404 Not Found", r#"{"error":"not found"}"#.to_string())
                    };
                    let resp = format!(
                        "HTTP/1.1 {code}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                });
            }
        });

        let api = ember_client::api::Api::new(&format!("http://{addr}")).expect("api");

        // 1. Initial load from server populates REMOTE and entries_st with 0 runs.
        let initial_list = svc.rt.block_on(load_remote_from(&api)).expect("initial load");
        assert_eq!(initial_list.len(), 1);
        assert_eq!(initial_list[0].routine.id, "sch-live");
        assert!(initial_list[0].runs.is_empty());

        let entries_st = mutable_state_of(Vec::<Entry>::new());
        let online_st = mutable_state_of(false);
        apply_load(Ok(initial_list));
        sync_from_remote(&entries_st, &online_st);
        assert!(online_st.get());
        assert_eq!(entries_st.get().len(), 1);
        assert!(!has_running_remote_run(false, online_st.get(), &entries_st.get()));

        // 2. Error path first: if POST /api/schedules/{id}/run fails, the optimistic Running
        // placeholder is rolled back even if load_remote also fails.
        state.lock().unwrap().1 = true;
        begin_remote_run_now("sch-live", 100, &entries_st).expect("begin optimistic run");
        assert!(has_running_remote_run(false, online_st.get(), &entries_st.get()));
        let err_res = svc.rt.block_on(api.run_schedule("sch-live"));
        assert!(err_res.is_err());
        let err_list = svc.rt.block_on(load_remote_from(&api));
        let err_msg = finish_remote_run_now("sch-live", err_res, err_list, &entries_st, &online_st);
        assert!(
            err_msg.starts_with("서버가 요청을 처리하지 못했습니다:"),
            "unexpected error notice: {err_msg}"
        );
        assert!(
            entries_st.get()[0].runs.is_empty(),
            "optimistic Running placeholder must be rolled back when run_schedule fails"
        );
        assert!(!has_running_remote_run(false, online_st.get(), &entries_st.get()));

        // Also verify rollback when load_remote itself fails with a status error.
        begin_remote_run_now("sch-live", 101, &entries_st).expect("begin optimistic run 2");
        let err_res2 = svc.rt.block_on(api.run_schedule("sch-live"));
        let err_load = Err(ember_client::api::ApiError::Status {
            status: 500,
            message: "db locked".into(),
            code: None,
        });
        let _ = finish_remote_run_now("sch-live", err_res2, err_load, &entries_st, &online_st);
        assert!(
            entries_st.get()[0].runs.is_empty(),
            "optimistic placeholder must be rolled back even when load_remote fails"
        );

        // 3. Live progress path: Running -> polling -> Completed (완료).
        *state.lock().unwrap() = (Some(ember_client::wire::RunStatus::Running), false);
        begin_remote_run_now("sch-live", 102, &entries_st).expect("begin live run");
        assert_eq!(entries_st.get()[0].runs.len(), 1);
        assert_eq!(entries_st.get()[0].runs[0].status, RunStatus::Running);
        assert_eq!(entries_st.get()[0].runs[0].session, None);

        let ok_res = svc.rt.block_on(api.run_schedule("sch-live"));
        let list_running = svc.rt.block_on(load_remote_from(&api));
        let ok_msg = finish_remote_run_now("sch-live", ok_res, list_running, &entries_st, &online_st);
        assert_eq!(ok_msg, REMOTE_RUN_STARTED);
        assert_eq!(entries_st.get()[0].runs.len(), 1);
        assert_eq!(entries_st.get()[0].runs[0].status, RunStatus::Running);
        assert_eq!(
            entries_st.get()[0].runs[0].session.as_deref(),
            Some("ses-live-1")
        );
        assert!(
            has_running_remote_run(false, online_st.get(), &entries_st.get()),
            "while server run is Running, has_running_remote_run must stay true to keep polling"
        );

        // Server transitions run-1 to Completed; next poll syncs Done ("완료") and stops polling.
        state.lock().unwrap().0 = Some(ember_client::wire::RunStatus::Completed);
        let polled = svc.rt.block_on(load_remote_from(&api));
        apply_load(polled);
        sync_from_remote(&entries_st, &online_st);
        assert_eq!(entries_st.get()[0].runs.len(), 1);
        assert_eq!(entries_st.get()[0].runs[0].status, RunStatus::Done);
        assert_eq!(entries_st.get()[0].runs[0].status.label(), "완료");
        assert_eq!(
            entries_st.get()[0].runs[0].session.as_deref(),
            Some("ses-live-1")
        );
        assert!(
            !has_running_remote_run(false, online_st.get(), &entries_st.get()),
            "once server run is Completed, polling stops"
        );

        with_remote(|r| *r = Remote::default());
    }
}

/// Design review 2026-11-13 B5: the routine dashboard and detail against the mockup.
#[cfg(test)]
mod design_review_tests {
    use super::*;

    /// This file without this module, so a source check never finds its own literal.
    fn source() -> &'static str {
        let src = include_str!("routines.rs");
        &src[..src.find("mod design_review_tests {").unwrap()]
    }

    #[test]
    fn design_b5_kpi_numbers_are_28_400_and_the_rows_sit_together() {
        assert_eq!(ROUTINE_KPI_WEIGHT, Weight::Regular);
        assert_eq!(ROUTINE_KPI_ROW_GAP, 16.0);
        let src = source();
        assert_eq!(src.matches("Label(&kpi.value, Fs::Xl2, ROUTINE_KPI_WEIGHT, None, Some(1));").count(), 2);
        let dash = &src[src.find("let kpis_content = move || {").unwrap()..src.find("let hero_content = move || {").unwrap()];
        assert!(!dash.contains("weight(1.0)"), "the KPI rows are still spread over the card");
    }

    #[test]
    fn design_b5_detail_title_is_15_600() {
        assert_eq!(ROUTINE_DETAIL_TITLE, (Fs::Lg, Weight::Semibold));
    }
}
