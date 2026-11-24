//! The 루틴 (Routines) screen (FR-L25, `docs/DESIGN.md` section 6.5): a dashboard on top (KPIs,
//! success ring, weekday bars), the routines grouped by project below it, a detail panel and the
//! routine form.
//!
//! The decisions are plain functions and types in the first half of this file, unit-tested
//! without a renderer. The components in the second half only hand them to dioxus-compose.
//!
//! The routines live on the server's schedule API (FR-A8, `ember/server/src/schedules/`),
//! wrapped by `ember-client`: list, create, edit, pause and resume, run now and run history.
//! The server has no name, model or permission mode per schedule and no event trigger, so the
//! name is the first line of the prompt (also the session title), model and mode show the
//! harness defaults, and event routines stay on this device. [`LocalStore`] keeps those, and
//! every routine while the server cannot be reached, in memory for this app run; it starts
//! empty and nothing here is sample data.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use ember_client::wire::{
    self, EventOn, NewSchedule, Schedule, SchedulePatch, ScheduleKind, ScheduleRun, ScheduleTarget,
};

use super::home::{
    DEFAULT_PROJECT, Harness, Selection, default_mode, default_option,
};
use super::shell::NavMenu;


/// Minutes since 1970-01-01 00:00 UTC. Local time is this plus the offset the caller passes.
pub type Minutes = i64;

/// Offset for the dashboard and run history, which display UTC.
pub const LOCAL_OFFSET_MIN: i64 = 0;

// ---------------------------------------------------------------------------------------------
// Calendar
// ---------------------------------------------------------------------------------------------

/// Days since 1970-01-01 of a civil date.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `(year, month, day)` of a day count since 1970-01-01.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 0 = Sunday .. 6 = Saturday.
pub fn weekday(days: i64) -> u32 {
    (days + 4).rem_euclid(7) as u32
}

/// Minutes since the epoch of a civil date and time.
pub fn minutes_at(y: i64, mo: u32, d: u32, h: u32, mi: u32) -> Minutes {
    days_from_civil(y, mo, d) * 1440 + h as i64 * 60 + mi as i64
}

const WEEKDAY_SHORT: [&str; 7] = ["일", "월", "화", "수", "목", "금", "토"];

// ---------------------------------------------------------------------------------------------
// Cron
// ---------------------------------------------------------------------------------------------

/// The five fields as bit sets over their values.
struct Cron {
    minute: u64,
    hour: u64,
    dom: u64,
    month: u64,
    dow: u64,
    dom_any: bool,
    dow_any: bool,
}

/// One field: comma list of `*`, `a`, `a-b`, each with an optional `/step`.
fn parse_field(field: &str, lo: u32, hi: u32) -> Option<u64> {
    let mut mask = 0u64;
    for part in field.split(',') {
        let (base, step) = match part.split_once('/') {
            Some((b, s)) => (b, s.parse::<u32>().ok().filter(|n| *n > 0)?),
            None => (part, 1),
        };
        let (from, to): (u32, u32) = if base == "*" {
            (lo, hi)
        } else if let Some((a, b)) = base.split_once('-') {
            (a.parse().ok()?, b.parse().ok()?)
        } else {
            let a: u32 = base.parse().ok()?;
            if part.contains('/') { (a, hi) } else { (a, a) }
        };
        if from < lo || to > hi || from > to {
            return None;
        }
        let mut v = from;
        while v <= to {
            mask |= 1u64 << v;
            v += step;
        }
    }
    Some(mask)
}

fn parse_cron(expr: &str) -> Option<Cron> {
    let f: Vec<&str> = expr.split_whitespace().collect();
    if f.len() != 5 {
        return None;
    }
    let mut dow = parse_field(f[4], 0, 7)?;
    if dow & (1u64 << 7) != 0 {
        dow = (dow & !(1u64 << 7)) | 1;
    }
    Some(Cron {
        minute: parse_field(f[0], 0, 59)?,
        hour: parse_field(f[1], 0, 23)?,
        dom: parse_field(f[2], 1, 31)?,
        month: parse_field(f[3], 1, 12)?,
        dow,
        dom_any: f[2].starts_with('*'),
        dow_any: f[4].starts_with('*'),
    })
}

/// Whether `expr` is a five-field cron expression this screen understands: `*`, `*/n`, `a`,
/// `a-b`, `a-b/n` and comma lists, per field (minute hour day-of-month month day-of-week).
pub fn cron_valid(expr: &str) -> bool {
    parse_cron(expr).is_some()
}

const INVALID_CRON: &str = "올바르지 않은 cron입니다";

/// "오전 9시", "오후 2시 30분", "자정", "정오".
fn time_phrase(h: u32, m: u32) -> String {
    if h == 0 && m == 0 {
        return "자정".into();
    }
    if h == 12 && m == 0 {
        return "정오".into();
    }
    let period = if h < 12 { "오전" } else { "오후" };
    let h12 = if h % 12 == 0 { 12 } else { h % 12 };
    if m == 0 {
        format!("{period} {h12}시")
    } else {
        format!("{period} {h12}시 {m}분")
    }
}

/// The schedule as a Korean phrase: "매일 오전 9시", "평일 오전 9시", "15분마다". An expression
/// outside the small subset reads "사용자 지정 일정"; an invalid one "올바르지 않은 cron입니다".
pub fn describe_cron(expr: &str) -> String {
    if !cron_valid(expr) {
        return INVALID_CRON.into();
    }
    let f: Vec<&str> = expr.split_whitespace().collect();
    let num = |s: &str| s.parse::<u32>().ok();
    let step = |s: &str| s.strip_prefix("*/").and_then(|n| n.parse::<u32>().ok());
    let rest_any = f[2] == "*" && f[3] == "*" && f[4] == "*";
    let custom = || "사용자 지정 일정".to_string();

    if f.iter().all(|x| *x == "*") {
        return "매분".into();
    }
    if f[1] == "*" && rest_any {
        if f[0] == "0" {
            return "매시 정각".into();
        }
        return match step(f[0]) {
            Some(n) => format!("{n}분마다"),
            None => custom(),
        };
    }
    if f[0] == "0" && rest_any {
        if let Some(n) = step(f[1]) {
            return format!("{n}시간마다");
        }
    }
    let (Some(m), Some(h)) = (num(f[0]), num(f[1])) else {
        return custom();
    };
    let t = time_phrase(h, m);
    match (f[2], f[3], f[4]) {
        ("*", "*", "*") => format!("매일 {t}"),
        ("*", "*", "1-5") => format!("평일 {t}"),
        ("*", "*", d) => match num(d) {
            Some(n) if n <= 7 => format!("매주 {}요일 {t}", WEEKDAY_SHORT[(n % 7) as usize]),
            _ => custom(),
        },
        (d, "*", "*") => match num(d) {
            Some(n) => format!("매월 {n}일 {t}"),
            None => custom(),
        },
        _ => custom(),
    }
}

/// The time shown large: "09:00" for a fixed minute and hour, else the phrase of [`describe_cron`].
pub fn big_time(expr: &str) -> String {
    if cron_valid(expr) {
        let f: Vec<&str> = expr.split_whitespace().collect();
        if let (Ok(m), Ok(h)) = (f[0].parse::<u32>(), f[1].parse::<u32>()) {
            return format!("{h:02}:{m:02}");
        }
    }
    describe_cron(expr)
}

fn day_matches(c: &Cron, day: i64) -> bool {
    let (_, month, dom) = civil_from_days(day);
    if c.month & (1u64 << month) == 0 {
        return false;
    }
    let dom_ok = c.dom & (1u64 << dom) != 0;
    let dow_ok = c.dow & (1u64 << weekday(day)) != 0;
    match (c.dom_any, c.dow_any) {
        (true, true) => true,
        (true, false) => dow_ok,
        (false, true) => dom_ok,
        (false, false) => dom_ok || dow_ok,
    }
}

/// The first trigger strictly after `after` (local minutes), looking up to four years ahead.
/// `None` for an invalid expression or one that never matches (30 February).
pub fn next_run(expr: &str, after: Minutes) -> Option<Minutes> {
    let c = parse_cron(expr)?;
    let first = after.div_euclid(1440);
    for day in first..first + 366 * 4 {
        if !day_matches(&c, day) {
            continue;
        }
        for h in (0..24).filter(|h| c.hour & (1u64 << *h) != 0) {
            for m in (0..60).filter(|m| c.minute & (1u64 << *m) != 0) {
                let t = day * 1440 + h * 60 + m;
                if t > after {
                    return Some(t);
                }
            }
        }
    }
    None
}

/// The next cron trigger as UTC minutes, using the saved IANA time zone.
fn next_zoned_run(expr: &str, tz: Tz, after: Minutes) -> Option<Minutes> {
    let cron = croner::Cron::new(expr).parse().ok()?;
    let mut from = DateTime::<Utc>::from_timestamp(after.checked_mul(60)?, 0)?.with_timezone(&tz);
    let previous_local = from.naive_local();
    // The scheduler skips the repeated wall time when clocks go back.
    for _ in 0..4 {
        let next = cron.find_next_occurrence(&from, false).ok()?;
        if next.naive_local() != previous_local {
            return Some(next.timestamp().div_euclid(60));
        }
        from = next;
    }
    None
}

fn zone_minutes(utc: Minutes, tz: Tz) -> Option<Minutes> {
    let time = DateTime::<Utc>::from_timestamp(utc.checked_mul(60)?, 0)?.with_timezone(&tz);
    Some(time.naive_local().and_utc().timestamp().div_euclid(60))
}

fn format_zoned_next(next: Minutes, now: Minutes, tz: Tz) -> Option<String> {
    Some(format_next(zone_minutes(next, tz)?, zone_minutes(now, tz)?))
}

/// "오늘 09:00", "내일 09:00" or "10월 28일 09:00", both times in local minutes.
pub fn format_next(next: Minutes, now: Minutes) -> String {
    let rem = next.rem_euclid(1440);
    let clock = format!("{:02}:{:02}", rem / 60, rem % 60);
    let next_day = next.div_euclid(1440);
    match next_day - now.div_euclid(1440) {
        0 => format!("오늘 {clock}"),
        1 => format!("내일 {clock}"),
        _ => {
            let (_, m, d) = civil_from_days(next_day);
            format!("{m}월 {d}일 {clock}")
        }
    }
}

/// "방금 전", "5분 전", "3시간 전" or "2일 전".
pub fn ago(now: Minutes, at: Minutes) -> String {
    let d = now - at;
    if d < 1 {
        "방금 전".into()
    } else if d < 60 {
        format!("{d}분 전")
    } else if d < 1440 {
        format!("{}시간 전", d / 60)
    } else {
        format!("{}일 전", d / 1440)
    }
}

// ---------------------------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// Fires on a cron schedule.
    Time,
    /// Fires on an event; `Routine::cron` holds its description.
    Event,
}

impl Trigger {
    pub const ALL: [Trigger; 2] = [Trigger::Time, Trigger::Event];

    pub fn label(self) -> &'static str {
        match self {
            Trigger::Time => "시간",
            Trigger::Event => "이벤트",
        }
    }
}

/// One routine. The profile (account) is not stored: it follows the harness and the project
/// (FR-L17), so there is no account field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routine {
    pub id: String,
    pub name: String,
    pub trigger: Trigger,
    /// A cron expression for [`Trigger::Time`], the event description for [`Trigger::Event`].
    pub cron: String,
    /// IANA time zone for a cron routine. Intervals, one-offs and events have no zone.
    pub time_zone: Option<String>,
    pub prompt: String,
    pub enabled: bool,
    /// Project, model and effort, mode and computer, the same choices as the Home composer.
    pub sel: Selection,
    /// Session id when this routine continues an existing session (`ScheduleTarget::Continue`).
    pub continue_session: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Done,
    Failed,
    Running,
    /// The target session was busy, so nothing was sent.
    Skipped,
    /// The trigger passed while the server was down.
    Missed,
}

/// Size of the weekday bar chart as `(width, height, gap)` in dp (mockup `.bars2`): 150 tall with
/// 24 dp bars at 10 dp gaps, and 120 tall with 16 dp bars at 6 dp gaps on a narrow layout.
pub fn weekday_chart_size(compact: bool) -> (f32, f32, f32) {
    if compact {
        (148.0, 94.0, 6.0)
    } else {
        (228.0, 126.0, 10.0)
    }
}

/// Height of the weekday card content in dp: heading (16) + gap (4) + chart (+1 baseline) + the
/// 24 dp day-label row. The card's own 16 dp padding and 1 dp border come on top, so the card is
/// 205 dp high at 1440 and 173 at 390, as the mockup's `.rt-topwrap` cards (FR-L25, #848).
pub fn weekday_card_height(narrow: bool) -> f32 {
    let (_, chart_h, _) = weekday_chart_size(narrow);
    16.0 + 4.0 + chart_h + 1.0 + 24.0
}

/// Corner radius of a weekday bar: a pill (half the width), never more than half the height.
pub fn weekday_bar_radius(w: f32, h: f32) -> f32 {
    (w / 2.0).min(h / 2.0).max(0.0)
}

/// The gap between a weekday bar's failed and successful parts (`.bar { gap: 2px }`).
pub const WEEKDAY_BAR_GAP: f32 = 2.0;

/// `(fail_h, gap, ok_h)` of one joined weekday bar `h` dp high (mockup `.bar`: one pill,
/// `overflow: hidden`, the failed part on top of the successful part, 2 dp apart). The parts
/// add up to `h`; the gap is there only when the day has both (FR-L25, #848).
pub fn weekday_bar_parts(h: f32, ok: usize, fail: usize) -> (f32, f32, f32) {
    let total = ok + fail;
    if total == 0 || h <= 0.0 {
        return (0.0, 0.0, 0.0);
    }
    let gap = if ok > 0 && fail > 0 { WEEKDAY_BAR_GAP.min(h) } else { 0.0 };
    let room = (h - gap).max(0.0);
    let fail_h = room * fail as f32 / total as f32;
    (fail_h, gap, room - fail_h)
}

impl RunStatus {
    pub fn label(self) -> &'static str {
        match self {
            RunStatus::Done => "완료",
            RunStatus::Failed => "실패",
            RunStatus::Running => "실행 중",
            RunStatus::Skipped => "건너뜀",
            RunStatus::Missed => "놓침",
        }
    }
}

/// One run of a routine; each run is a session reachable from the history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub at: Minutes,
    pub status: RunStatus,
    pub session: Option<String>,
}

/// A routine with its runs, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub routine: Routine,
    pub runs: Vec<Run>,
    /// The server's next planned trigger; `None` for routines that live on this device.
    pub next_at: Option<Minutes>,
}

pub(crate) fn entry_next_run(entry: &Entry, now: Minutes) -> Option<Minutes> {
    entry.next_at.filter(|t| *t >= now).or_else(|| {
        let tz = entry.routine.time_zone.as_deref()?.parse().ok()?;
        next_zoned_run(&entry.routine.cron, tz, now)
    })
}

/// The next trigger in the routine's own wall time. Other triggers display in UTC.
pub(crate) fn entry_next_text(entry: &Entry, now: Minutes) -> Option<String> {
    let next = entry_next_run(entry, now)?;
    match entry.routine.time_zone.as_deref() {
        Some(tz) => format_zoned_next(next, now, tz.parse().ok()?),
        None => Some(format_next(next, now)),
    }
}

/// The schedule sentence: "매일 오전 9시", with the zone for non-UTC cron, or the event text.
pub fn schedule_line(r: &Routine) -> String {
    let mut line = match r.trigger {
        Trigger::Time if cron_valid(&r.cron) => describe_cron(&r.cron),
        _ => r.cron.clone(),
    };
    if let Some(tz) = r
        .time_zone
        .as_deref()
        .filter(|tz| r.trigger == Trigger::Time && *tz != DEFAULT_TIME_ZONE)
    {
        line.push_str(" · ");
        line.push_str(tz);
    }
    line
}

/// "실행 기록 없음", or "완료 · 3시간 전" from the newest run.
pub fn last_run_text(runs: &[Run], now: Minutes) -> String {
    match runs.first() {
        None => "실행 기록 없음".into(),
        Some(r) => format!("{} · {}", r.status.label(), ago(now, r.at)),
    }
}

// ---------------------------------------------------------------------------------------------
// Grouping and filtering
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub project: String,
    pub routines: Vec<Routine>,
}

impl Group {
    /// "ember (2)".
    pub fn title(&self) -> String {
        format!("{} ({})", self.project, self.routines.len())
    }
}

fn matches_query(r: &Routine, q: &str) -> bool {
    q.is_empty()
        || format!("{} {} {}", r.name, schedule_line(r), r.prompt)
            .to_lowercase()
            .contains(q)
}

/// One group per project, projects by name with 기본 프로젝트 last, routines in the given
/// order. `project` limits to one project; `query` matches name, schedule and prompt without
/// regard to case. A project with no match has no group.
pub fn group_by_project(all: &[Routine], project: Option<&str>, query: &str) -> Vec<Group> {
    let q = query.trim().to_lowercase();
    let mut groups: Vec<Group> = Vec::new();
    for r in all {
        if project.is_some_and(|p| p != r.sel.project) || !matches_query(r, &q) {
            continue;
        }
        match groups.iter_mut().find(|g| g.project == r.sel.project) {
            Some(g) => g.routines.push(r.clone()),
            None => groups.push(Group {
                project: r.sel.project.clone(),
                routines: vec![r.clone()],
            }),
        }
    }
    groups.sort_by(|a, b| {
        (a.project == DEFAULT_PROJECT, &a.project).cmp(&(b.project == DEFAULT_PROJECT, &b.project))
    });
    groups
}

/// The projects that have a routine, for the filter pills, in the order of the groups.
pub fn filter_projects(all: &[Routine]) -> Vec<String> {
    group_by_project(all, None, "")
        .into_iter()
        .map(|g| g.project)
        .collect()
}

/// What the screen shows at a window size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneMode {
    Both,
    ListOnly,
    DetailOnly,
}

/// Wide: list and detail side by side. Compact: one of them, the detail once a routine is open
/// (with the back button), else the list.
pub fn pane_mode(compact: bool, detail_open: bool) -> PaneMode {
    match (compact, detail_open) {
        (false, _) => PaneMode::Both,
        (true, true) => PaneMode::DetailOnly,
        (true, false) => PaneMode::ListOnly,
    }
}

/// Where the routine form opens (FR-L25, #511).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormLayout {
    /// In the detail area, in place of the detail (mockup v8), with 취소 and 저장 in its header.
    Inline,
    /// A modal sheet: the compact layouts have no detail area beside the list.
    Sheet,
}

/// The form's fields go two to a row from this field width (mockup `.form`, a two-column grid
/// at every width: about 133 per column at 390), and one to a row below it, so a column never
/// gets narrower than about 122 (FR-L25, #848, #596).
pub const FORM_TWO_COLUMNS_FROM: f32 = 260.0;

/// Whether the form lays its fields out in two columns: wide enough, inline or in the phone's
/// sheet alike, as the mockup's 390 form pairs 프로젝트/트리거 in selects (#596).
pub fn form_two_columns(_layout: FormLayout, field_w: f32) -> bool {
    field_w >= FORM_TWO_COLUMNS_FROM
}

/// The form's rows, each one or two fields, in `FORM_FIELDS` order. In two columns the mockup's
/// pairs share a row: 프로젝트 and 트리거, 일정 설명 and cron, model and 권한 모드 (#848).
pub fn form_rows(two_columns: bool) -> Vec<Vec<FormField>> {
    if !two_columns {
        return FORM_FIELDS.iter().map(|f| vec![*f]).collect();
    }
    vec![
        vec![FormField::Name],
        vec![FormField::Project, FormField::Trigger],
        vec![FormField::ScheduleDescription, FormField::Schedule],
        vec![FormField::TimeZone],
        vec![FormField::Model, FormField::Mode],
        vec![FormField::Computer],
        vec![FormField::Prompt],
    ]
}

/// Wide layouts (list and detail side by side) open the form in the detail area; the compact
/// ones keep the sheet.
pub fn form_layout(mode: PaneMode) -> FormLayout {
    match mode {
        PaneMode::Both => FormLayout::Inline,
        PaneMode::ListOnly | PaneMode::DetailOnly => FormLayout::Sheet,
    }
}

/// A route out of the open routine form, or into another form (FR-L25, #511, #596).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Leave {
    /// 취소.
    Cancel,
    /// A list row was tapped.
    OpenRoutine(String),
    /// 새 루틴.
    NewForm,
    /// 편집 of a routine.
    EditForm(String),
    /// Another sidebar, rail or bottom-tab menu was tapped in the shell.
    NavMenu(NavMenu),
    /// The shell's 검색 tool was tapped.
    Search,
}

/// What a route out does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaveDecision {
    /// Go at once: the form is closed or has no changes.
    Go,
    /// Ask "변경 사항을 버릴까요?" first.
    Ask,
    /// A save is in flight: do nothing.
    Block,
}

/// Whether the form holds changes: any field differs from the draft it opened with (name,
/// project, trigger, cron or event, model, permission, computer, prompt).
pub fn routine_form_dirty(initial: &FormDraft, current: &FormDraft) -> bool {
    initial != current
}

/// The one rule for every route out. A save closes the form at once (it hands the draft to the
/// save path and sets the panel to view), so `saving` stays false today.
pub fn leave_decision(form_open: bool, dirty: bool, saving: bool) -> LeaveDecision {
    if !form_open {
        LeaveDecision::Go
    } else if saving {
        LeaveDecision::Block
    } else if dirty {
        LeaveDecision::Ask
    } else {
        LeaveDecision::Go
    }
}

/// The question of the confirm.
pub const ROUTINE_DISCARD_QUESTION: &str = "변경 사항을 버릴까요?";

/// The fields of the routine form, in mockup v8 order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormField {
    Name,
    Project,
    Trigger,
    /// A read-only line derived from the schedule: the server stores no description text.
    ScheduleDescription,
    Schedule,
    TimeZone,
    /// One picker for harness, model and effort.
    Model,
    Mode,
    Computer,
    Prompt,
}

pub const FORM_FIELDS: [FormField; 10] = [
    FormField::Name,
    FormField::Project,
    FormField::Trigger,
    FormField::ScheduleDescription,
    FormField::Schedule,
    FormField::TimeZone,
    FormField::Model,
    FormField::Mode,
    FormField::Computer,
    FormField::Prompt,
];

/// The 일정 설명 line: the cron in words and the next run for a time routine, a fixed sentence
/// for an event routine. It is computed from the schedule, never stored or typed.
pub fn schedule_description(trigger: Trigger, cron: &str, time_zone: &str, now: Minutes) -> String {
    match trigger {
        Trigger::Time => {
            let cron = cron.trim();
            let words = describe_cron(cron);
            if !cron_valid(cron) {
                return words;
            }
            let next = time_zone.trim().parse::<Tz>().ok().and_then(|tz| {
                let next = next_zoned_run(cron, tz, now)?;
                format_zoned_next(next, now, tz)
            });
            match next {
                Some(t) => format!("{words} · 다음 실행 {t}"),
                None => words,
            }
        }
        Trigger::Event => "이벤트가 일어날 때 실행됩니다".to_string(),
    }
}

/// The rows of the one model picker: every harness that can run (`allowed`), or only the
/// current harness for a server routine, which cannot change it.
pub fn model_choices(
    catalog: &[super::home::ModelOption],
    allowed: &[Harness],
    current: Harness,
    server: bool,
) -> Vec<super::home::ModelOption> {
    catalog
        .iter()
        .filter(|o| {
            if server {
                o.harness == current
            } else {
                o.harness == current || allowed.contains(&o.harness)
            }
        })
        .copied()
        .collect()
}

/// The lines the big time heading may take: one for a time ("오전 9시"), three for an event
/// name such as `event: pull_request.opened, synchronize`, which would otherwise be cut.
pub fn when_big_max_lines(trigger: Trigger) -> u32 {
    match trigger {
        Trigger::Time => 1,
        Trigger::Event => 3,
    }
}

// ---------------------------------------------------------------------------------------------
// Form
// ---------------------------------------------------------------------------------------------

const DEFAULT_CRON: &str = "0 9 * * *";
pub(crate) const DEFAULT_TIME_ZONE: &str = "UTC";

/// Parses an event-routine input (`owner/name`, `owner/name#branch`, or a pasted
/// `github.com/owner/name(.git)` URL with optional `#branch`) into `(repository, branch)`.
/// The ` (branch)` display form is not accepted as input (FR-L23, FR-L25, #759).
pub fn parse_ci_event(text: &str) -> Option<(String, Option<String>)> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let (repo_raw, branch) = match t.split_once('#') {
        Some((r, b)) => {
            let b = b.trim();
            if b.is_empty() || b.contains('#') || b.chars().any(char::is_whitespace) {
                return None;
            }
            (r, Some(b.to_string()))
        }
        None => (t, None),
    };
    let r = repo_raw.trim().trim_end_matches('/');
    let r = r
        .strip_prefix("https://")
        .or_else(|| r.strip_prefix("http://"))
        .unwrap_or(r);
    let r = r.strip_prefix("github.com/").unwrap_or(r);
    let r = r.strip_suffix(".git").unwrap_or(r);
    let mut parts = r.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    let ok = |p: &str| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    (parts.next().is_none() && ok(owner) && ok(name))
        .then(|| (format!("{owner}/{name}"), branch))
}

/// Converts a routine's stored event display string (`CI 실패 시 · owner/name` or
/// `CI 실패 시 · owner/name (branch)`) back into the edit form's input shape (`owner/name` or
/// `owner/name#branch`), leaving any other string unchanged.
pub fn event_field_text(cron: &str) -> String {
    let t = cron.trim();
    let Some(rest) = t.strip_prefix("CI 실패 시 · ") else {
        return cron.to_string();
    };
    let rest = rest.trim();
    if let Some((repo, branch)) = rest
        .strip_suffix(')')
        .and_then(|s| s.rsplit_once(" ("))
    {
        let repo = repo.trim();
        let branch = branch.trim();
        if !repo.is_empty() && !branch.is_empty() {
            return format!("{repo}#{branch}");
        }
    }
    rest.to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormDraft {
    pub name: String,
    pub trigger: Trigger,
    pub cron: String,
    pub time_zone: String,
    pub prompt: String,
    pub sel: Selection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormError {
    NameEmpty,
    ScheduleInvalid,
    TimeZoneInvalid,
    EventEmpty,
    EventInvalid,
    PromptEmpty,
}

impl FormError {
    pub fn message(self) -> &'static str {
        match self {
            FormError::NameEmpty => "이름을 입력하셔야 합니다",
            FormError::ScheduleInvalid => "일정이 올바른 cron 형식이 아닙니다",
            FormError::TimeZoneInvalid => "UTC 또는 Asia/Seoul 같은 시간대를 입력하셔야 합니다",
            FormError::EventEmpty => "이벤트 설명을 입력하셔야 합니다",
            FormError::EventInvalid => {
                "저장소는 owner/name 또는 owner/name#branch 형식으로 입력하셔야 합니다"
            }
            FormError::PromptEmpty => "프롬프트를 입력하셔야 합니다",
        }
    }
}

impl FormDraft {
    /// A new routine: every day at 09:00, the stored new-member defaults (or Claude Code's default, ask mode), the main server.
    pub fn new(project: &str) -> FormDraft {
        let base = crate::services::try_services()
            .map(|s| Selection::initial_with_defaults(&s.ui.live.agent_defaults.get_untracked()))
            .unwrap_or_else(Selection::initial);
        FormDraft {
            name: String::new(),
            trigger: Trigger::Time,
            cron: DEFAULT_CRON.into(),
            time_zone: DEFAULT_TIME_ZONE.into(),
            prompt: String::new(),
            sel: Selection {
                project: project.into(),
                ..base
            },
        }
    }

    pub fn from_routine(r: &Routine) -> FormDraft {
        FormDraft {
            name: r.name.clone(),
            trigger: r.trigger,
            cron: match r.trigger {
                Trigger::Time => r.cron.clone(),
                Trigger::Event => event_field_text(&r.cron),
            },
            time_zone: r.time_zone.as_deref().unwrap_or(DEFAULT_TIME_ZONE).into(),
            prompt: r.prompt.clone(),
            sel: r.sel.clone(),
        }
    }

    /// Switching trigger replaces the text with that trigger's starting value.
    pub fn with_trigger(&self, t: Trigger) -> FormDraft {
        if t == self.trigger {
            return self.clone();
        }
        FormDraft {
            trigger: t,
            cron: match t {
                Trigger::Time => DEFAULT_CRON.into(),
                Trigger::Event => String::new(),
            },
            ..self.clone()
        }
    }

    /// The problems that block saving, in field order. Empty: the draft can be saved.
    pub fn errors(&self) -> Vec<FormError> {
        self.errors_with_initial(None)
    }

    /// The problems that block saving when editing `initial` (if `Some`), in field order.
    /// Leaving an existing non-empty event schedule unchanged does not block saving a name or
    /// prompt edit even if its stored repository string is not `owner/name` (#759).
    pub fn errors_with_initial(&self, initial: Option<&FormDraft>) -> Vec<FormError> {
        let mut out = Vec::new();
        if self.name.trim().is_empty() {
            out.push(FormError::NameEmpty);
        }
        let unchanged_event = initial.is_some_and(|i| {
            i.trigger == Trigger::Event
                && self.trigger == Trigger::Event
                && i.cron.trim() == self.cron.trim()
        });
        match self.trigger {
            Trigger::Time => {
                if !cron_valid(self.cron.trim()) {
                    out.push(FormError::ScheduleInvalid);
                }
                if self.time_zone.trim().parse::<Tz>().is_err() {
                    out.push(FormError::TimeZoneInvalid);
                }
            }
            Trigger::Event if self.cron.trim().is_empty() => out.push(FormError::EventEmpty),
            Trigger::Event if !unchanged_event && parse_ci_event(&self.cron).is_none() => {
                out.push(FormError::EventInvalid)
            }
            _ => {}
        }
        if self.prompt.trim().is_empty() {
            out.push(FormError::PromptEmpty);
        }
        out
    }

    /// The routine to store. Text is trimmed, and valid event inputs format as [`kind_text`].
    pub fn into_routine(&self, id: &str, enabled: bool) -> Routine {
        let cron = match self.trigger {
            Trigger::Time => self.cron.trim().into(),
            Trigger::Event => match parse_ci_event(&self.cron) {
                Some((repository, branch)) => kind_text(&ScheduleKind::Event {
                    on: EventOn::CiFailed,
                    repository,
                    branch,
                }),
                None => self.cron.trim().into(),
            },
        };
        Routine {
            id: id.into(),
            name: self.name.trim().into(),
            trigger: self.trigger,
            cron,
            time_zone: (self.trigger == Trigger::Time).then(|| self.time_zone.trim().into()),
            prompt: self.prompt.trim().into(),
            enabled,
            sel: self.sel.clone(),
            continue_session: None,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineKpi {
    pub label: &'static str,
    pub value: String,
    pub note: Option<String>,
}

/// Success rate of the runs that ended: done over done and failed. `None` while none ended.
pub fn success_percent(runs: &[Run]) -> Option<u32> {
    let done = runs.iter().filter(|r| r.status == RunStatus::Done).count();
    let failed = runs
        .iter()
        .filter(|r| r.status == RunStatus::Failed)
        .count();
    let ended = done + failed;
    (ended > 0).then(|| ((done as f64 / ended as f64) * 100.0).round() as u32)
}

/// The rate over every run of every routine.
pub fn overall_success(entries: &[Entry]) -> Option<u32> {
    let all: Vec<Run> = entries.iter().flat_map(|e| e.runs.clone()).collect();
    success_percent(&all)
}

/// 켜진 루틴, 지금 실행 중, 이번 주 실행 (the last seven days) and 다음 실행. `now` is local
/// minutes; run times are shifted by `offset` to local before they are compared.
pub fn kpis(entries: &[Entry], now: Minutes, offset: i64) -> [RoutineKpi; 4] {
    let on = entries.iter().filter(|e| e.routine.enabled).count();
    let runs = || entries.iter().flat_map(|e| e.runs.iter());
    let running = runs().filter(|r| r.status == RunStatus::Running).count();
    let week: Vec<&Run> = runs()
        .filter(|r| r.at + offset > now - 7 * 1440 && r.at + offset <= now)
        .collect();
    let failed = week
        .iter()
        .filter(|r| r.status == RunStatus::Failed)
        .count();
    let next = entries
        .iter()
        .filter(|e| e.routine.enabled && e.routine.trigger == Trigger::Time)
        .filter_map(|e| entry_next_run(e, now - offset).map(|t| (t + offset, &e.routine.name)))
        .min_by_key(|(t, _)| *t);
    [
        RoutineKpi {
            label: "켜진 루틴",
            value: on.to_string(),
            note: Some(format!("전체 {}", entries.len())),
        },
        RoutineKpi {
            label: "지금 실행 중",
            value: running.to_string(),
            note: (running > 0).then(|| "진행 중".to_string()),
        },
        RoutineKpi {
            label: "이번 주 실행",
            value: week.len().to_string(),
            note: Some(format!("실패 {failed}")),
        },
        match next {
            Some((t, name)) => RoutineKpi {
                label: "다음 실행",
                value: format_next(t, now),
                note: Some(name.clone()),
            },
            None => RoutineKpi {
                label: "다음 실행",
                value: "없음".into(),
                note: None,
            },
        },
    ]
}

/// One weekday of the bar chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DayBar {
    pub label: &'static str,
    pub ok: usize,
    pub fail: usize,
    pub today: bool,
}

/// The seven days ending today, oldest first, with the runs that ended on each.
pub fn weekday_bars(entries: &[Entry], now: Minutes, offset: i64) -> [DayBar; 7] {
    let today = now.div_euclid(1440);
    std::array::from_fn(|i| {
        let day = today - 6 + i as i64;
        let on_day = |status: RunStatus| {
            entries
                .iter()
                .flat_map(|e| e.runs.iter())
                .filter(|r| r.status == status && (r.at + offset).div_euclid(1440) == day)
                .count()
        };
        DayBar {
            label: WEEKDAY_SHORT[weekday(day) as usize],
            ok: on_day(RunStatus::Done),
            fail: on_day(RunStatus::Failed),
            today: day == today,
        }
    })
}

// ---------------------------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    /// A routine on this device cannot run: only the server runs them.
    Unavailable,
    NotFound,
}

/// Where routines live. The server's schedule API (FR-A8) is the real one; the screen only
/// talks to this trait.
pub trait RoutineStore {
    fn entries(&self) -> Vec<Entry>;
    fn save(&mut self, routine: Routine);
    fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), StoreError>;
    fn run_now(&mut self, id: &str) -> Result<(), StoreError>;
    /// A routine id that is not in use.
    fn next_id(&self) -> String;
    /// `true` while routines live only in this app run (the screen says so).
    fn is_local(&self) -> bool;
}

/// In-memory routines for this app run. Starts empty, never invents a run.
#[derive(Debug, Default)]
pub struct LocalStore {
    routines: Vec<Routine>,
}

impl RoutineStore for LocalStore {
    fn entries(&self) -> Vec<Entry> {
        self.routines
            .iter()
            .map(|r| Entry {
                routine: r.clone(),
                runs: Vec::new(),
                next_at: None,
            })
            .collect()
    }

    fn save(&mut self, routine: Routine) {
        match self.routines.iter_mut().find(|r| r.id == routine.id) {
            Some(slot) => *slot = routine,
            None => self.routines.push(routine),
        }
    }

    fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), StoreError> {
        let r = self
            .routines
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or(StoreError::NotFound)?;
        r.enabled = enabled;
        Ok(())
    }

    fn run_now(&mut self, id: &str) -> Result<(), StoreError> {
        if self.routines.iter().any(|r| r.id == id) {
            Err(StoreError::Unavailable)
        } else {
            Err(StoreError::NotFound)
        }
    }

    fn next_id(&self) -> String {
        (1..)
            .map(|n| format!("r{n}"))
            .find(|id| self.routines.iter().all(|r| &r.id != id))
            .expect("an unused id exists")
    }

    fn is_local(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------------------------
// Server schedules (FR-A8)
// ---------------------------------------------------------------------------------------------

/// Runs fetched per routine for the history and the dashboard.
const RUNS_SHOWN: usize = 20;
/// The longest routine name taken from a prompt.
const NAME_MAX: usize = 28;

/// Server time (Unix ms) as [`Minutes`].
pub fn ms_to_minutes(ms: i64) -> Minutes {
    ms.div_euclid(60_000) + LOCAL_OFFSET_MIN
}

/// The server keeps no name, so a routine is named by its prompt's first line.
pub fn name_from_prompt(prompt: &str) -> String {
    let line = prompt.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    if line.chars().count() <= NAME_MAX {
        return line.to_string();
    }
    let cut: String = line.chars().take(NAME_MAX).collect();
    format!("{cut}...")
}

/// The sentence for a schedule that is not a cron expression.
pub fn kind_text(kind: &ScheduleKind) -> String {
    match kind {
        ScheduleKind::Cron { expr, .. } => expr.clone(),
        ScheduleKind::Interval { seconds } => match *seconds {
            s if s % 3600 == 0 => format!("{}시간마다", s / 3600),
            s if s % 60 == 0 => format!("{}분마다", s / 60),
            s => format!("{s}초마다"),
        },
        ScheduleKind::Once { .. } => "한 번만 실행".to_string(),
        ScheduleKind::Event { repository, branch, .. } => match branch {
            Some(b) => format!("CI 실패 시 · {repository} ({b})"),
            None => format!("CI 실패 시 · {repository}"),
        },
    }
}

fn run_status(s: wire::RunStatus) -> RunStatus {
    match s {
        wire::RunStatus::Running => RunStatus::Running,
        wire::RunStatus::Completed => RunStatus::Done,
        wire::RunStatus::Skipped => RunStatus::Skipped,
        wire::RunStatus::Missed => RunStatus::Missed,
        wire::RunStatus::Failed | wire::RunStatus::Interrupted | wire::RunStatus::Unknown => {
            RunStatus::Failed
        }
    }
}

pub fn run_from_server(r: &ScheduleRun) -> Run {
    Run {
        at: ms_to_minutes(r.started_at.unwrap_or(r.scheduled_at)),
        status: run_status(r.status),
        session: r.session_id.clone(),
    }
}

/// A server schedule as a routine. Model and mode are the harness defaults, since the server
/// stores neither; the routine's name comes from `ScheduleTarget::New { title }` when present and
/// non-empty, falling back to [`name_from_prompt`].
pub fn schedule_to_routine(s: &Schedule) -> Routine {
    let harness = Harness::from_kind(&s.agent).unwrap_or(Harness::ClaudeCode);
    let (name, continue_session) = match &s.target {
        ScheduleTarget::New { title: Some(t) } if !t.trim().is_empty() => {
            (t.trim().to_string(), None)
        }
        ScheduleTarget::New { .. } => (name_from_prompt(&s.prompt), None),
        ScheduleTarget::Continue { session_id } => {
            (name_from_prompt(&s.prompt), Some(session_id.clone()))
        }
    };
    Routine {
        id: s.id.clone(),
        name,
        trigger: match &s.kind {
            ScheduleKind::Event { .. } => Trigger::Event,
            _ => Trigger::Time,
        },
        cron: kind_text(&s.kind),
        time_zone: match &s.kind {
            ScheduleKind::Cron { tz, .. } => Some(tz.clone()),
            _ => None,
        },
        prompt: s.prompt.clone(),
        enabled: !s.paused,
        sel: Selection {
            project: s.project.clone(),
            option: default_option(harness),
            mode: default_mode(harness),
            computer: s.computer_id.clone().filter(|c| c != "local"),
        },
        continue_session,
    }
}

/// A server schedule with its runs (newest first, as the server answers).
pub fn schedule_to_entry(s: &Schedule, runs: &[ScheduleRun]) -> Entry {
    Entry {
        routine: schedule_to_routine(s),
        runs: runs.iter().map(run_from_server).collect(),
        next_at: s.next_run_at.map(ms_to_minutes),
    }
}

/// Builds the server `ScheduleKind` from a form draft (zoned cron for [`Trigger::Time`], or
/// `ci_failed` for [`Trigger::Event`]).
pub fn draft_schedule_kind(d: &FormDraft) -> ScheduleKind {
    match d.trigger {
        Trigger::Time => ScheduleKind::Cron {
            expr: d.cron.trim().to_string(),
            tz: d.time_zone.trim().into(),
        },
        Trigger::Event => {
            let (repository, branch) =
                parse_ci_event(&d.cron).unwrap_or_else(|| (d.cron.trim().to_string(), None));
            ScheduleKind::Event {
                on: EventOn::CiFailed,
                repository,
                branch,
            }
        }
    }
}

/// The create body for a routine. Cron uses the selected zone. Event routines watch
/// `ci_failed` on the parsed repository and optional branch. Each run starts a session
/// titled with the routine's name, in `cwd`.
pub fn new_schedule(d: &FormDraft, cwd: &str) -> NewSchedule {
    NewSchedule {
        project: d.sel.project.clone(),
        agent: Some(d.sel.option.harness.kind().to_string()),
        account: None,
        computer: d.sel.computer.clone(),
        cwd: Some(cwd.to_string()),
        prompt: d.prompt.trim().to_string(),
        kind: draft_schedule_kind(d),
        target: ScheduleTarget::New {
            title: Some(d.name.trim().to_string()),
        },
        catch_up: false,
        paused: false,
    }
}

/// The edit body when no previous routine is cached: sends all draft fields that the server stores.
pub fn schedule_patch(d: &FormDraft) -> SchedulePatch {
    SchedulePatch {
        project: Some(d.sel.project.clone()),
        agent: Some(d.sel.option.harness.kind().to_string()),
        computer: Some(d.sel.computer.clone().unwrap_or_default()),
        cwd: None,
        target: Some(ScheduleTarget::New {
            title: Some(d.name.trim().to_string()),
        }),
        prompt: Some(d.prompt.trim().to_string()),
        kind: Some(draft_schedule_kind(d)),
        catch_up: None,
    }
}

/// Whether the draft changes a field beyond prompt and schedule.
pub fn changes_frozen_fields(d: &FormDraft, old: &Routine) -> bool {
    d.sel.project != old.sel.project
        || d.sel.option.harness != old.sel.option.harness
        || d.sel.computer != old.sel.computer
}

/// Shown while the server cannot be reached.
pub const OFFLINE_NOTE: &str =
    "서버에 연결되지 않아 루틴이 이 앱을 실행하는 동안만 이 기기에 보관됩니다";
/// Shown when routines that only this device keeps are listed beside the server's (or in sample mode).
pub const LOCAL_NOTE: &str =
    "이 기기에만 있는 루틴은 이 앱을 실행하는 동안만 보관됩니다";
pub const RUN_NOTICE: &str = "서버에 연결되지 않아 지금 실행할 수 없습니다";
pub const NO_CWD_NOTICE: &str =
    "이 프로젝트의 작업 폴더를 알 수 없습니다. 프로젝트에서 세션을 한 번 시작하셔야 합니다";

/// The `PATCH` body for an edit: only what differs from the stored routine (FR-L25, #493).
/// Includes `project`, `agent`, `computer` (empty string when cleared to `None`), `target` (`title`),
/// `prompt`, and `kind`. A `ScheduleTarget::Continue` routine keeps its target on the server and
/// does not send `ScheduleTarget::New`. An empty patch means nothing to send.
pub fn changed_patch(d: &FormDraft, old: &Routine) -> SchedulePatch {
    let name = d.name.trim();
    let prompt = d.prompt.trim();
    let kind = draft_schedule_kind(d);
    let initial = FormDraft::from_routine(old);
    let schedule_changed = d.trigger != old.trigger
        || (d.cron.trim() != initial.cron.trim() && kind_text(&kind) != old.cron.trim())
        || (d.trigger == Trigger::Time && old.time_zone.is_some()
            && d.time_zone.trim() != initial.time_zone);
    let cont = old.continue_session.is_some();
    SchedulePatch {
        project: (!cont && d.sel.project != old.sel.project).then(|| d.sel.project.clone()),
        agent: (!cont && d.sel.option.harness != old.sel.option.harness)
            .then(|| d.sel.option.harness.kind().to_string()),
        computer: (!cont && d.sel.computer != old.sel.computer)
            .then(|| d.sel.computer.clone().unwrap_or_default()),
        cwd: None,
        target: (!cont && name != old.name.trim()).then(|| ScheduleTarget::New {
            title: Some(name.to_string()),
        }),
        prompt: (prompt != old.prompt.trim()).then(|| prompt.to_string()),
        kind: schedule_changed.then_some(kind),
        catch_up: None,
    }
}

/// The fields of a draft that differ from `old` and that the server cannot store, in form order.
/// `Schedule` stores the harness (`agent`), not a per-schedule model or permission mode; a
/// `Continue`-target routine also keeps its session's title, project, harness and computer.
pub fn unsaved_fields(d: &FormDraft, old: &Routine) -> Vec<&'static str> {
    let mut out = Vec::new();
    let cont = old.continue_session.is_some();
    if cont && d.name.trim() != old.name.trim() {
        out.push("이름");
    }
    if cont && d.sel.project != old.sel.project {
        out.push("프로젝트");
    }
    if cont && d.sel.option.harness != old.sel.option.harness {
        out.push("하네스");
    }
    if d.sel.option != default_option(d.sel.option.harness) {
        out.push("모델");
    }
    if d.sel.mode != default_mode(d.sel.option.harness) {
        out.push("권한 모드");
    }
    if cont && d.sel.computer != old.sel.computer {
        out.push("컴퓨터");
    }
    out
}

/// The sentence for an edit whose patch holds nothing the server takes: it names what was not
/// saved instead of saying nothing changed. `None` when no such field changed.
pub fn not_saved_sentence(fields: &[&str]) -> Option<String> {
    if fields.is_empty() {
        return None;
    }
    Some(format!("{}은(는) 서버가 아직 바꿀 수 없어 저장하지 않았습니다", fields.join(", ")))
}

/// One row of a run history, as plain parts (the screen draws them as pills and a button).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRowText {
    pub status: &'static str,
    pub flag: Option<&'static str>,
    pub ago: String,
    /// The run's time as the mockup's `.runtile` heads it: "오늘 09:00" (#848).
    pub when: String,
    /// The tile's second line: the session the run made, or that it made none.
    pub sub: &'static str,
    pub opens_session: bool,
}

/// The run tile's second line when the run's session is not known to the client, and when the
/// run has none. A known session names its member (`run_tile_sub`).
pub const RUN_TILE_SESSION: &str = "세션";
pub const RUN_TILE_NO_SESSION: &str = "세션 없음";

/// The run tile's second line (`.runtile` `mname(session)`): the member whose session the run
/// is, when the client knows that session, else what the tile opens (#848, review r3).
pub fn run_tile_sub(r: &Run, member: Option<&str>) -> String {
    match (&r.session, member.map(str::trim).filter(|m| !m.is_empty())) {
        (None, _) => RUN_TILE_NO_SESSION.to_string(),
        (Some(_), Some(m)) => m.to_string(),
        (Some(_), None) => RUN_TILE_SESSION.to_string(),
    }
}

pub fn run_row_text(r: &Run, now: Minutes, simulated: bool) -> RunRowText {
    RunRowText {
        status: r.status.label(),
        flag: simulated.then_some(SAMPLE_FLAG),
        ago: ago(now, r.at),
        when: format_next(r.at, now),
        sub: if r.session.is_some() { RUN_TILE_SESSION } else { RUN_TILE_NO_SESSION },
        opens_session: r.session.is_some(),
    }
}

/// Whether a patch carries no change.
pub fn patch_is_empty(p: &SchedulePatch) -> bool {
    p.project.is_none()
        && p.agent.is_none()
        && p.computer.is_none()
        && p.cwd.is_none()
        && p.target.is_none()
        && p.prompt.is_none()
        && p.kind.is_none()
        && p.catch_up.is_none()
}

// ---------------------------------------------------------------------------------------------
// Simulated run in the demo (FR-L25, #494)
// ---------------------------------------------------------------------------------------------

/// How long the simulated run shows "실행 중" before it finishes.
pub const DEMO_RUN_MS: u64 = 2500;
/// How often the Routines screen polls the server while a schedule run is `Running` (FR-L25, #494).
pub const REMOTE_RUN_POLL_MS: u64 = 1000;
/// The id prefix the demo gives a session it creates; a run linked to one is a simulation.
pub const DEMO_SESSION_PREFIX: &str = "demo-new-";
pub const DEMO_RUN_STARTED: &str = "예시 실행을 시작했습니다. 데모에서 시뮬레이션하는 실행입니다";
pub const DEMO_RUN_DONE: &str = "예시 실행이 끝났습니다. 예시 세션이 목록에 추가되었습니다";
pub const DEMO_RUN_BUSY: &str = "이 루틴은 이미 실행 중입니다";
pub const REMOTE_RUN_STARTED: &str = "루틴을 지금 실행했습니다";
/// The flag shown beside a simulated run.
pub const SAMPLE_FLAG: &str = "예시";

/// Maps a demo routine's session id (`s1`..`s9` or `demo-s1`..`demo-s9` or `demo-new-*`) to the
/// client session id in demo mode (`demo-s1`..`demo-s9`), or leaves real server session ids as-is.
pub fn resolve_run_session_id(id: &str, demo_active: bool) -> String {
    if demo_active
        && id.starts_with('s')
        && id.len() > 1
        && id[1..].chars().all(|c| c.is_ascii_digit())
    {
        format!("demo-{id}")
    } else {
        id.to_string()
    }
}

/// Applies the server's `POST /api/schedules/{id}/run` response (`ScheduleRun`) to `entry`:
/// replaces the optimistic `Running` placeholder (if any) or inserts the server run at the top
/// of the history, and returns the user-facing notice string.
pub fn apply_server_run_started(e: &mut Entry, run: &ScheduleRun) -> String {
    let mapped = run_from_server(run);
    if let Some(slot) = e.runs.iter_mut().find(|r| r.status == RunStatus::Running && r.session.is_none()) {
        *slot = mapped;
    } else if let Some(slot) = e.runs.iter_mut().find(|r| r.session.is_some() && r.session == mapped.session) {
        *slot = mapped;
    } else {
        e.runs.insert(0, mapped);
    }
    match &run.error {
        Some(err) => format!("실행하지 못했습니다: {err}"),
        None => REMOTE_RUN_STARTED.to_string(),
    }
}

/// Whether `entries` has any run still in `RunStatus::Running` that should be polled on the server.
pub fn has_running_remote_run(sample_mode: bool, online: bool, entries: &[Entry]) -> bool {
    !sample_mode
        && online
        && entries
            .iter()
            .any(|e| e.runs.iter().any(|r| r.status == RunStatus::Running))
}

/// Whether 지금 실행 is busy for this routine: a run is in progress, either the optimistic
/// placeholder that `begin_demo_run` put first or a server run still `Running` (the runs
/// `has_running_remote_run` polls for). The button is disabled while this holds (FR-L25, #494).
pub fn run_now_busy(e: &Entry) -> bool {
    e.runs.iter().any(|r| r.status == RunStatus::Running)
}

/// Whether the simulated run is allowed: only in demo mode, never with a server.
pub fn demo_run_allowed(demo_active: bool, sample_entry: bool) -> bool {
    demo_active && sample_entry
}

/// A run is a simulation while it is running in the demo, or when it links a demo session.
pub fn run_is_simulated(r: &Run, demo_active: bool) -> bool {
    demo_active
        && (r.status == RunStatus::Running
            || r.session.as_deref().is_some_and(|s| s.starts_with(DEMO_SESSION_PREFIX)))
}

/// Puts a "실행 중" run first in the history. Refuses when one is already running.
pub fn begin_demo_run(e: &mut Entry, now: Minutes) -> Result<(), &'static str> {
    if e.runs.iter().any(|r| r.status == RunStatus::Running) {
        return Err(DEMO_RUN_BUSY);
    }
    e.runs.insert(0, Run { at: now, status: RunStatus::Running, session: None });
    Ok(())
}

/// Turns the running run into a finished one that links `session`. `false` when none runs.
pub fn finish_demo_run(e: &mut Entry, session: Option<String>) -> bool {
    match e.runs.iter_mut().find(|r| r.status == RunStatus::Running) {
        Some(r) => {
            r.status = RunStatus::Done;
            r.session = session;
            true
        }
        None => false,
    }
}

/// Removes an optimistic `RunStatus::Running` placeholder (`session: None`) from `e` when a
/// server `run_schedule` request fails before creating a run.
pub fn rollback_optimistic_run(e: &mut Entry) -> bool {
    if let Some(pos) = e
        .runs
        .iter()
        .position(|r| r.status == RunStatus::Running && r.session.is_none())
    {
        e.runs.remove(pos);
        true
    } else {
        false
    }
}

/// The title of the sample session a simulated run creates.
pub fn demo_session_title(routine_name: &str) -> String {
    format!("{} ({SAMPLE_FLAG})", routine_name.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::home::{PermMode, default_mode, default_option};

    fn at(y: i64, mo: u32, d: u32, h: u32, mi: u32) -> Minutes {
        minutes_at(y, mo, d, h, mi)
    }

    fn routine(id: &str, name: &str, project: &str, cron: &str) -> Routine {
        Routine {
            id: id.into(),
            name: name.into(),
            trigger: Trigger::Time,
            cron: cron.into(),
            time_zone: Some(DEFAULT_TIME_ZONE.into()),
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
        // Sunday noon: the weekday schedule fires on Monday.
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
        // Sunday is 0 and 7.
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
        assert_eq!(ember, ["1", "4"], "given order inside a group");
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

        // Case-insensitive, and the prompt counts.
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
            "a new draft needs a name and a prompt"
        );
        d.name = "  ".into();
        assert!(d.errors().contains(&FormError::NameEmpty), "blank is empty");
        d.name = "CI 요약".into();
        d.prompt = "실패를 요약합니다".into();
        assert!(d.errors().is_empty());

        d.cron = "0 25 * * *".into();
        assert_eq!(d.errors(), [FormError::ScheduleInvalid]);

        let mut e = d.with_trigger(Trigger::Event);
        assert_eq!(
            e.errors(),
            [FormError::EventEmpty],
            "the cron text is replaced"
        );
        e.cron = "PR이 열릴 때".into();
        assert_eq!(e.errors(), [FormError::EventInvalid]);
        e.cron = "DarkPyonix/darkpyonix-ember#develop".into();
        assert!(e.errors().is_empty());
        let back = e.with_trigger(Trigger::Time);
        assert_eq!(back.cron, "0 9 * * *");
        assert_eq!(
            back.with_trigger(Trigger::Time),
            back,
            "same trigger keeps the text"
        );
    }

    #[test]
    fn fr_l25_form_error_messages_are_polite() {
        for e in [
            FormError::NameEmpty,
            FormError::ScheduleInvalid,
            FormError::EventEmpty,
            FormError::EventInvalid,
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
        assert_eq!(k[2].value, "3", "the nine-day-old run is outside the week");
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
        // Sunday 2026-10-25, so the bars run Monday to Sunday.
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
        assert_eq!(
            (bars[6].ok, bars[6].fail),
            (1, 1),
            "a running run is not counted"
        );
        assert_eq!((bars[5].ok, bars[5].fail), (1, 0));
        assert_eq!(
            (bars[0].ok, bars[0].fail),
            (1, 0),
            "six days back is the first bar"
        );
        let total: usize = bars.iter().map(|b| b.ok + b.fail).sum();
        assert_eq!(total, 4, "the run seven days back is outside the bars");
    }

    #[test]
    fn fr_l25_weekday_bars_follow_the_offset() {
        // 23:30 UTC on Saturday is already Sunday in UTC+9.
        let run_at = at(2026, 10, 24, 23, 30);
        let now = at(2026, 10, 25, 12, 0);
        let e = [entry(
            routine("1", "a", "p", "0 9 * * *"),
            vec![run(run_at, RunStatus::Done)],
        )];
        let utc = weekday_bars(&e, now, 0);
        assert_eq!(utc[5].ok, 1, "Saturday in UTC");
        let kst = weekday_bars(&e, now + 540, 540);
        assert_eq!(kst[6].ok, 1, "Sunday in UTC+9");
    }

    // ---- store ------------------------------------------------------------------------------

    #[test]
    fn fr_l25_the_local_store_starts_empty_and_says_it_is_local() {
        let s = LocalStore::default();
        assert!(s.entries().is_empty(), "no sample data");
        assert!(s.is_local());
    }

    #[test]
    fn fr_l25_save_creates_then_replaces_by_id() {
        let mut s = LocalStore::default();
        let id = s.next_id();
        s.save(routine(&id, "첫째", "ember", "0 9 * * *"));
        assert_eq!(s.entries().len(), 1);
        assert_ne!(s.next_id(), id, "the next id is free");
        let mut edited = routine(&id, "고친 이름", "ember", "0 9 * * *");
        edited.enabled = false;
        s.save(edited);
        let e = s.entries();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].routine.name, "고친 이름");
        assert!(
            e[0].runs.is_empty(),
            "a saved routine has no invented history"
        );
    }

    #[test]
    fn fr_l25_toggle_and_run_now() {
        let mut s = LocalStore::default();
        let id = s.next_id();
        s.save(routine(&id, "a", "ember", "0 9 * * *"));
        assert_eq!(s.set_enabled(&id, false), Ok(()));
        assert!(!s.entries()[0].routine.enabled);
        assert_eq!(s.set_enabled("nope", true), Err(StoreError::NotFound));
        // Running needs the server: nothing is started and no run is made up.
        assert_eq!(s.run_now(&id), Err(StoreError::Unavailable));
        assert!(s.entries()[0].runs.is_empty());
    }

    // ---- server schedules -------------------------------------------------------------------

    fn schedule(kind: ScheduleKind, paused: bool) -> Schedule {
        Schedule {
            id: "sch1".into(),
            project: "ember".into(),
            agent: "codex".into(),
            account_id: None,
            computer_id: Some("local".into()),
            cwd: Some("/w".into()),
            prompt: "\n  Summarise the open PRs\nthen post it".into(),
            kind,
            target: ScheduleTarget::New { title: None },
            paused,
            catch_up: false,
            next_run_at: Some(120_000),
            last_run_at: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn server_run(status: wire::RunStatus) -> ScheduleRun {
        ScheduleRun {
            id: "r1".into(),
            schedule_id: "sch1".into(),
            scheduled_at: 60_000,
            status,
            session_id: Some("s9".into()),
            started_at: Some(180_000),
            finished_at: None,
            error: None,
        }
    }

    fn cron_kind(expr: &str) -> ScheduleKind {
        ScheduleKind::Cron {
            expr: expr.into(),
            tz: "UTC".into(),
        }
    }

    #[test]
    fn fr_a8_editing_cron_keeps_its_saved_time_zone() {
        let old = schedule_to_routine(&schedule(
            ScheduleKind::Cron {
                expr: "0 9 * * *".into(),
                tz: "Europe/Berlin".into(),
            },
            false,
        ));
        let mut draft = FormDraft::from_routine(&old);
        draft.cron = "15 9 * * *".into();
        assert_eq!(
            changed_patch(&draft, &old).kind,
            Some(ScheduleKind::Cron {
                expr: "15 9 * * *".into(),
                tz: "Europe/Berlin".into(),
            }),
        );
    }

    #[test]
    fn fr_a8_dashboard_compares_cron_triggers_in_absolute_time() {
        let entry = |name: &str, tz: &str| {
            let mut s = schedule(
                ScheduleKind::Cron {
                    expr: "0 9 * * *".into(),
                    tz: tz.into(),
                },
                false,
            );
            s.target = ScheduleTarget::New { title: Some(name.into()) };
            s.next_run_at = None;
            schedule_to_entry(&s, &[])
        };
        let entries = [entry("UTC routine", "UTC"), entry("Seoul routine", "Asia/Seoul")];
        let next = &kpis(&entries, at(2026, 6, 1, 23, 0), 0)[3];
        assert_eq!(next.value, "내일 00:00");
        assert_eq!(next.note.as_deref(), Some("Seoul routine"));
    }

    #[test]
    fn fr_a8_cron_create_and_zone_only_patch_round_trip() {
        let mut draft = FormDraft::new("ember");
        draft.name = "Daily routine".into();
        draft.prompt = "Summarize".into();
        draft.time_zone = " Europe/Berlin ".into();
        assert!(draft.errors().is_empty());
        let body = new_schedule(&draft, "/work");
        assert_eq!(
            body.kind,
            ScheduleKind::Cron { expr: "0 9 * * *".into(), tz: "Europe/Berlin".into() },
        );

        let old = schedule_to_routine(&schedule(body.kind, false));
        let loaded = FormDraft::from_routine(&old);
        assert_eq!(loaded.time_zone, "Europe/Berlin");
        assert!(patch_is_empty(&changed_patch(&loaded, &old)));

        let mut edited = loaded;
        edited.time_zone = "Asia/Seoul".into();
        let patch = changed_patch(&edited, &old);
        assert_eq!(patch.kind, Some(ScheduleKind::Cron {
            expr: draft.cron,
            tz: "Asia/Seoul".into(),
        }));
        assert!(patch.prompt.is_none());
    }

    #[test]
    fn fr_a8_unknown_time_zones_only_block_cron_forms() {
        let mut draft = FormDraft::new("ember");
        draft.name = "Daily routine".into();
        draft.prompt = "Summarize".into();
        for tz in ["", "Mars/Olympus"] {
            draft.time_zone = tz.into();
            assert_eq!(draft.errors(), [FormError::TimeZoneInvalid]);
        }
        let mut event = draft.with_trigger(Trigger::Event);
        event.cron = "owner/repository".into();
        assert!(event.errors().is_empty());
        assert!(matches!(draft_schedule_kind(&event), ScheduleKind::Event { .. }));
        assert!(event.into_routine("event", true).time_zone.is_none());
    }

    #[test]
    fn fr_a8_preview_uses_daylight_saving_in_the_selected_zone() {
        let zone: Tz = "Europe/Berlin".parse().unwrap();
        let december = at(2026, 12, 1, 7, 30);
        let june = at(2026, 6, 1, 7, 30);
        assert_eq!(next_zoned_run("0 9 * * *", zone, december), Some(at(2026, 12, 1, 8, 0)));
        assert_eq!(next_zoned_run("0 9 * * *", zone, june), Some(at(2026, 6, 2, 7, 0)));
        assert_eq!(
            schedule_description(Trigger::Time, "0 9 * * *", "Europe/Berlin", june),
            "매일 오전 9시 · 다음 실행 내일 09:00",
        );
    }

    #[test]
    fn fr_a8_a_schedule_reads_as_a_routine() {
        let e = schedule_to_entry(
            &schedule(cron_kind("0 9 * * 1-5"), false),
            &[server_run(wire::RunStatus::Completed)],
        );
        let r = &e.routine;
        assert_eq!(r.id, "sch1");
        assert_eq!(r.name, "Summarise the open PRs");
        assert_eq!(r.trigger, Trigger::Time);
        assert_eq!(r.cron, "0 9 * * 1-5");
        assert!(r.enabled);
        assert_eq!(r.sel.project, "ember");
        assert_eq!(r.sel.option.harness, Harness::Codex);
        assert_eq!(r.sel.computer, None, "local is the main server");
        assert_eq!(e.next_at, Some(2), "server ms become minutes");
        assert_eq!(e.runs.len(), 1);
        assert_eq!(e.runs[0].at, 3, "the start time, not the trigger time");
        assert_eq!(e.runs[0].status, RunStatus::Done);
        assert_eq!(e.runs[0].session.as_deref(), Some("s9"));
    }

    #[test]
    fn fr_a8_a_paused_schedule_is_off() {
        let e = schedule_to_entry(&schedule(cron_kind("0 9 * * *"), true), &[]);
        assert!(!e.routine.enabled);
    }

    #[test]
    fn fr_a8_interval_and_one_off_schedules_keep_their_own_sentence() {
        let every = |s| schedule_to_routine(&schedule(ScheduleKind::Interval { seconds: s }, false));
        assert_eq!(schedule_line(&every(1800)), "30분마다");
        assert_eq!(schedule_line(&every(7200)), "2시간마다");
        assert_eq!(schedule_line(&every(90 + 1)), "91초마다");
        let once = schedule_to_routine(&schedule(ScheduleKind::Once { at: 1 }, false));
        assert_eq!(schedule_line(&once), "한 번만 실행");
    }

    #[test]
    fn fr_a8_run_statuses_map_to_the_screens() {
        use wire::RunStatus as W;
        let cases = [
            (W::Running, RunStatus::Running),
            (W::Completed, RunStatus::Done),
            (W::Failed, RunStatus::Failed),
            (W::Interrupted, RunStatus::Failed),
            (W::Unknown, RunStatus::Failed),
            (W::Skipped, RunStatus::Skipped),
            (W::Missed, RunStatus::Missed),
        ];
        for (w, want) in cases {
            assert_eq!(run_from_server(&server_run(w)).status, want);
        }
        let waiting = ScheduleRun {
            started_at: None,
            ..server_run(W::Missed)
        };
        assert_eq!(run_from_server(&waiting).at, 1, "falls back to the trigger time");
    }

    #[test]
    fn fr_a8_names_come_from_the_first_line_of_the_prompt() {
        assert_eq!(name_from_prompt("  \n daily report \nmore"), "daily report");
        assert_eq!(name_from_prompt(""), "");
        let long = "가".repeat(40);
        let name = name_from_prompt(&long);
        assert_eq!(name.chars().count(), NAME_MAX + 3);
        assert!(name.ends_with("..."));
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
        assert_eq!(body.agent.as_deref(), Some("claude-code"));
        assert_eq!(body.computer.as_deref(), Some("c1"));
        assert_eq!(body.cwd.as_deref(), Some("/w/ember"));
        assert_eq!(body.prompt, "Summarise");
        assert_eq!(body.kind, cron_kind("30 8 * * 1"));
        assert_eq!(
            body.target,
            ScheduleTarget::New {
                title: Some("아침 요약".into())
            }
        );
        assert!(!body.paused);
        let patch = schedule_patch(&d);
        assert_eq!(patch.prompt.as_deref(), Some("Summarise"));
        assert_eq!(patch.kind, Some(cron_kind("30 8 * * 1")));
        assert_eq!(patch.catch_up, None);
    }

    #[test]
    fn fr_a8_only_prompt_and_schedule_can_be_edited_on_the_server() {
        let old = schedule_to_routine(&schedule(cron_kind("0 9 * * *"), false));
        let same = FormDraft::from_routine(&old);
        assert!(!changes_frozen_fields(&same, &old));
        let mut moved = same.clone();
        moved.sel.project = "other".into();
        assert!(changes_frozen_fields(&moved, &old));
        let mut other_computer = same;
        other_computer.sel.computer = Some("c2".into());
        assert!(changes_frozen_fields(&other_computer, &old));
    }

    // ---- text -------------------------------------------------------------------------------

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
            FormError::EventInvalid,
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

    // ---- edit patch and simulated run (#493, #494) --------------------------------------------

    #[test]
    fn fr_l25_edit_patch_carries_only_changed_fields() {
        let old = routine("r1", "a", "ember", "0 9 * * *");
        let mut d = FormDraft::from_routine(&old);
        assert!(patch_is_empty(&changed_patch(&d, &old)), "nothing changed");
        d.prompt = " 새 지시 ".into();
        let p = changed_patch(&d, &old);
        assert_eq!(p.prompt.as_deref(), Some("새 지시"));
        assert!(p.kind.is_none());
        assert!(p.project.is_none() && p.agent.is_none() && p.computer.is_none() && p.target.is_none());
        d.prompt = old.prompt.clone();
        d.cron = "30 8 * * 1".into();
        let p = changed_patch(&d, &old);
        assert!(p.prompt.is_none());
        assert_eq!(p.kind, Some(cron_kind("30 8 * * 1")));
    }

    #[test]
    fn fr_l25_edit_patch_carries_name_project_harness_and_computer() {
        let mut old = routine("r1", "CI 요약", "ember", "0 9 * * *");
        old.sel.computer = Some("c1".into());
        let mut d = FormDraft::from_routine(&old);
        d.name = "새 이름".into();
        d.sel.project = "core".into();
        d.sel = d.sel.with_harness(Harness::Codex);
        d.sel.computer = None;
        let p = changed_patch(&d, &old);
        assert!(!patch_is_empty(&p));
        assert_eq!(
            p.target,
            Some(ScheduleTarget::New {
                title: Some("새 이름".into())
            })
        );
        assert_eq!(p.project.as_deref(), Some("core"));
        assert_eq!(p.agent.as_deref(), Some("codex"));
        assert_eq!(p.computer.as_deref(), Some(""), "clearing computer sends empty string");
        assert!(unsaved_fields(&d, &old).is_empty(), "server stores all four fields");

        // Non-default model or mode on a server routine is reported in unsaved_fields.
        let mut custom = FormDraft::from_routine(&old);
        if let Some(other_opt) = crate::ui::home::model_catalog()
            .iter()
            .copied()
            .find(|o| o.harness == old.sel.option.harness && *o != default_option(old.sel.option.harness))
        {
            custom.sel = custom.sel.with_option(other_opt);
        }
        custom.sel = custom.sel.with_mode(PermMode::Plan);
        let unsaved = unsaved_fields(&custom, &old);
        assert_eq!(unsaved, vec!["모델", "권한 모드"]);

        // Continue-target routine preserves its Continue target and reports unsupported edits.
        let mut cont_sched = schedule(cron_kind("0 9 * * *"), false);
        cont_sched.target = ScheduleTarget::Continue {
            session_id: "sess_1".into(),
        };
        let cont_old = schedule_to_routine(&cont_sched);
        assert_eq!(cont_old.continue_session.as_deref(), Some("sess_1"));
        let mut cont_draft = FormDraft::from_routine(&cont_old);
        cont_draft.name = "바꾼 이름".into();
        cont_draft.prompt = "바꾼 프롬프트".into();
        let cont_patch = changed_patch(&cont_draft, &cont_old);
        assert!(cont_patch.target.is_none(), "Continue target must not become New");
        assert_eq!(cont_patch.prompt.as_deref(), Some("바꾼 프롬프트"));
        assert_eq!(unsaved_fields(&cont_draft, &cont_old), vec!["이름"]);

        // Round-trip: schedule_to_routine reads title from ScheduleTarget::New { title }.
        let mut s = schedule(cron_kind("0 9 * * *"), false);
        s.target = ScheduleTarget::New {
            title: Some("새 이름".into()),
        };
        assert_eq!(schedule_to_routine(&s).name, "새 이름");
    }

    #[test]
    fn fr_l25_not_saved_sentence_names_the_fields() {
        assert_eq!(not_saved_sentence(&[]), None);
        let s = not_saved_sentence(&["이름", "프로젝트"]).unwrap();
        assert!(s.starts_with("이름, 프로젝트"), "{s}");
        assert!(s.ends_with("저장하지 않았습니다"));
        assert!(!s.contains("바뀐 내용이 없습니다"));
    }

    #[test]
    fn fr_l25_run_history_row_text_for_running_and_done() {
        let mut e = entry(routine("r1", "a", "ember", "0 9 * * *"), vec![run(10, RunStatus::Done)]);
        begin_demo_run(&mut e, 100).unwrap();
        let running = run_row_text(&e.runs[0], 100, true);
        assert_eq!(running.status, "실행 중");
        assert_eq!(running.flag, Some("예시"));
        assert_eq!(running.ago, "방금 전");
        assert!(!running.opens_session);
        finish_demo_run(&mut e, Some("demo-new-1".into()));
        let done = run_row_text(&e.runs[0], 103, true);
        assert_eq!(done.ago, "3분 전");
        assert!(done.opens_session);
        let old = run_row_text(&e.runs[1], 103, false);
        assert_eq!(old.flag, None);
    }

    #[test]
    fn fr_l25_edit_form_is_prefilled_from_the_routine() {
        let mut old = routine("r1", "CI 요약", "ember", "0 9 * * *");
        old.sel.computer = Some("c1".into());
        let d = FormDraft::from_routine(&old);
        assert_eq!(d.name, "CI 요약");
        assert_eq!(d.cron, "0 9 * * *");
        assert_eq!(d.prompt, old.prompt);
        assert_eq!(d.sel, old.sel, "project, harness, model, mode and computer");
        // Project, agent and computer can change and travel with the draft.
        let mut d2 = d.clone();
        d2.sel.project = "core".into();
        d2.sel.computer = None;
        assert_eq!(d2.into_routine("r1", true).sel.project, "core");
        assert!(changes_frozen_fields(&{
            let mut x = d.clone();
            x.sel.computer = None;
            x
        }, &old));
    }

    #[test]
    fn fr_l25_demo_run_goes_running_then_done_with_a_session() {
        let mut e = entry(routine("r1", "a", "ember", "0 9 * * *"), vec![run(10, RunStatus::Done)]);
        assert!(begin_demo_run(&mut e, 100).is_ok());
        assert_eq!(e.runs.len(), 2);
        assert_eq!(e.runs[0].status, RunStatus::Running);
        assert_eq!(e.runs[0].at, 100);
        assert!(run_is_simulated(&e.runs[0], true));
        assert!(finish_demo_run(&mut e, Some("demo-new-1".into())));
        assert_eq!(e.runs[0].status, RunStatus::Done);
        assert_eq!(e.runs[0].session.as_deref(), Some("demo-new-1"));
        assert!(run_is_simulated(&e.runs[0], true));
        assert!(!run_is_simulated(&e.runs[1], true), "recorded history is not flagged");
        assert!(!finish_demo_run(&mut e, None), "nothing is running any more");
    }

    #[test]
    fn fr_l25_demo_run_refuses_a_second_run_while_one_is_running() {
        let mut e = entry(routine("r1", "a", "ember", "0 9 * * *"), vec![]);
        begin_demo_run(&mut e, 1).unwrap();
        assert_eq!(begin_demo_run(&mut e, 2), Err(DEMO_RUN_BUSY));
        assert_eq!(e.runs.len(), 1);
    }

    #[test]
    fn fr_l25_demo_run_is_never_reachable_outside_demo_mode() {
        assert!(demo_run_allowed(true, true));
        assert!(!demo_run_allowed(false, true));
        assert!(!demo_run_allowed(true, false));
        let r = Run { at: 1, status: RunStatus::Running, session: None };
        assert!(!run_is_simulated(&r, false), "no flag, and no simulation, without the demo");
    }

    #[test]
    fn fr_l25_demo_run_session_is_flagged_as_a_sample() {
        assert_eq!(demo_session_title(" 아침 요약 "), "아침 요약 (예시)");
        for s in [DEMO_RUN_STARTED, DEMO_RUN_DONE, DEMO_RUN_BUSY, REMOTE_RUN_STARTED] {
            assert!(!s.contains('\u{2014}'));
            assert!(s.ends_with("니다"), "{s}");
        }
        assert!(DEMO_RUN_STARTED.contains("시뮬레이션"));
    }

    #[test]
    fn fr_l25_resolve_run_session_id_maps_demo_ids_only_in_demo_mode() {
        assert_eq!(resolve_run_session_id("s9", true), "demo-s9");
        assert_eq!(resolve_run_session_id("s4", true), "demo-s4");
        assert_eq!(resolve_run_session_id("demo-s9", true), "demo-s9");
        assert_eq!(resolve_run_session_id("demo-new-1", true), "demo-new-1");
        assert_eq!(resolve_run_session_id("s9", false), "s9");
        assert_eq!(resolve_run_session_id("ses_1234", true), "ses_1234");
    }

    #[test]
    fn fr_l25_apply_server_run_started_replaces_optimistic_placeholder_and_tracks_running() {
        let mut e = entry(routine("sch1", "a", "ember", "0 9 * * *"), vec![run(10, RunStatus::Done)]);
        assert!(!has_running_remote_run(false, true, std::slice::from_ref(&e)));
        begin_demo_run(&mut e, 100).unwrap();
        assert!(has_running_remote_run(false, true, std::slice::from_ref(&e)));
        assert!(!has_running_remote_run(true, true, std::slice::from_ref(&e)));
        assert!(!has_running_remote_run(false, false, std::slice::from_ref(&e)));

        let srv_running = server_run(wire::RunStatus::Running);
        let msg = apply_server_run_started(&mut e, &srv_running);
        assert_eq!(msg, REMOTE_RUN_STARTED);
        assert_eq!(e.runs.len(), 2, "optimistic placeholder is replaced, not duplicated");
        assert_eq!(e.runs[0].status, RunStatus::Running);
        assert_eq!(e.runs[0].session.as_deref(), Some("s9"));

        let mut srv_failed = server_run(wire::RunStatus::Failed);
        srv_failed.error = Some("cwd missing".into());
        let fail_msg = apply_server_run_started(&mut e, &srv_failed);
        assert_eq!(fail_msg, "실행하지 못했습니다: cwd missing");
        assert_eq!(e.runs[0].status, RunStatus::Failed);
        assert!(!has_running_remote_run(false, true, std::slice::from_ref(&e)));
    }

    #[test]
    fn fr_l25_run_now_is_busy_from_the_optimistic_row_until_the_server_run_ends() {
        let mut e = entry(routine("sch1", "a", "ember", "0 9 * * *"), vec![run(10, RunStatus::Done)]);
        assert!(!run_now_busy(&e), "finished history leaves 지금 실행 enabled");
        begin_demo_run(&mut e, 100).unwrap();
        assert!(run_now_busy(&e), "the optimistic row disables 지금 실행 before the server answers");
        apply_server_run_started(&mut e, &server_run(wire::RunStatus::Running));
        assert!(run_now_busy(&e), "a server run still Running keeps it disabled");
        assert!(has_running_remote_run(false, true, std::slice::from_ref(&e)));
        apply_server_run_started(&mut e, &server_run(wire::RunStatus::Completed));
        assert!(!run_now_busy(&e), "the run ended, so 지금 실행 is enabled again");

        let mut rolled = entry(routine("sch2", "b", "ember", "0 9 * * *"), vec![]);
        begin_demo_run(&mut rolled, 1).unwrap();
        assert!(rollback_optimistic_run(&mut rolled));
        assert!(!run_now_busy(&rolled), "a failed call rolls back and enables it again");
    }

    #[test]
    fn fr_l25_routine_run_pill_says_done() {
        assert_eq!(RunStatus::Done.label(), "완료");
        assert_eq!(RunStatus::Failed.label(), "실패");
        assert!(!RunStatus::Done.label().contains("성공"));
    }

    #[test]
    fn fr_l25_weekday_card_fits_chart_and_labels() {
        for narrow in [false, true] {
            let (_, chart_h, _) = weekday_chart_size(narrow);
            assert_eq!(weekday_card_height(narrow), 16.0 + 4.0 + chart_h + 1.0 + 24.0);
        }
        // With the card's 16 dp padding and 1 dp border: the mockup's 205 at 1440, 173 at 390.
        assert_eq!(weekday_card_height(false) + 2.0 * 16.0 + 2.0, 205.0);
        assert_eq!(weekday_card_height(true) + 2.0 * 16.0 + 2.0, 173.0);
    }

    #[test]
    fn fr_l25_form_pairs_fields_in_two_columns_when_inline_and_wide() {
        assert!(form_two_columns(FormLayout::Inline, 686.0));
        assert!(form_two_columns(FormLayout::Inline, 400.0));
        // The phone's sheet (390 less 20 on each side) pairs them too, as the mockup's 390 form.
        assert!(form_two_columns(FormLayout::Sheet, 350.0));
        assert!(!form_two_columns(FormLayout::Sheet, 240.0));
        assert!(!form_two_columns(FormLayout::Inline, 250.0));
        let one: Vec<FormField> = form_rows(false).into_iter().flatten().collect();
        let two: Vec<FormField> = form_rows(true).into_iter().flatten().collect();
        assert_eq!(one, FORM_FIELDS.to_vec());
        assert_eq!(two, FORM_FIELDS.to_vec(), "same fields, same order");
        assert!(form_rows(false).iter().all(|r| r.len() == 1));
        let rows = form_rows(true);
        assert!(rows.contains(&vec![FormField::Project, FormField::Trigger]));
        assert!(rows.contains(&vec![FormField::ScheduleDescription, FormField::Schedule]));
        assert!(rows.contains(&vec![FormField::Model, FormField::Mode]));
    }

    #[test]
    fn fr_l25_run_tile_heads_with_the_time_and_names_the_session() {
        let now = 20_000 * 1440 + 10 * 60;
        let with = Run { at: 20_000 * 1440 + 9 * 60, status: RunStatus::Done, session: Some("s1".into()) };
        let t = run_row_text(&with, now, false);
        assert_eq!(t.when, "오늘 09:00");
        assert_eq!(t.sub, RUN_TILE_SESSION);
        let without = Run { session: None, ..with };
        assert_eq!(run_row_text(&without, now, false).sub, RUN_TILE_NO_SESSION);
    }

    #[test]
    fn fr_l25_weekday_bars_geometry() {
        assert_eq!(weekday_chart_size(false), (228.0, 126.0, 10.0));
        assert_eq!(weekday_chart_size(true), (148.0, 94.0, 6.0));
        // 24 dp and 16 dp bars, as in the mockup.
        let bar_w = |(w, _, g): (f32, f32, f32)| (w - 6.0 * g) / 7.0;
        assert_eq!(bar_w(weekday_chart_size(false)), 24.0);
        assert_eq!(bar_w(weekday_chart_size(true)), 16.0);
        assert_eq!(weekday_bar_radius(24.0, 100.0), 12.0);
        assert_eq!(weekday_bar_radius(24.0, 10.0), 5.0);
        assert_eq!(weekday_bar_radius(24.0, 0.0), 0.0);
    }

    #[test]
    fn fr_l25_weekday_bar_joins_failures_on_top_of_successes() {
        // `.bar` stacks its parts with `gap: 2px` inside one pill (review r3: two separate
        // pills with a dot on top).
        assert_eq!(WEEKDAY_BAR_GAP, 2.0);
        let (fail, gap, ok) = weekday_bar_parts(102.0, 3, 1);
        assert_eq!(gap, 2.0);
        assert_eq!(fail, 25.0);
        assert_eq!(ok, 75.0);
        assert_eq!(fail + gap + ok, 102.0, "the parts fill the bar exactly");
        assert_eq!(weekday_bar_parts(50.0, 4, 0), (0.0, 0.0, 50.0), "no gap without failures");
        assert_eq!(weekday_bar_parts(50.0, 0, 2), (50.0, 0.0, 0.0), "all failed is all orange");
        assert_eq!(weekday_bar_parts(0.0, 1, 1), (0.0, 0.0, 0.0));
        assert_eq!(weekday_bar_parts(40.0, 0, 0), (0.0, 0.0, 0.0));
        let (f, g, o) = weekday_bar_parts(1.0, 1, 1);
        assert!(f >= 0.0 && o >= 0.0 && f + g + o <= 1.0 + f32::EPSILON, "a tiny bar never overflows");
    }

    #[test]
    fn fr_l25_run_tile_names_the_member_when_the_session_is_known() {
        let with = Run { at: 0, status: RunStatus::Done, session: Some("s1".into()) };
        assert_eq!(run_tile_sub(&with, Some("ember-구현")), "ember-구현");
        assert_eq!(run_tile_sub(&with, Some("  ")), RUN_TILE_SESSION, "a blank name falls back");
        assert_eq!(run_tile_sub(&with, None), RUN_TILE_SESSION);
        let without = Run { session: None, ..with };
        assert_eq!(run_tile_sub(&without, Some("ember-구현")), RUN_TILE_NO_SESSION);
    }

    #[test]
    fn fr_l23_ci_event_parses_one_input_shape_and_strips_github_urls() {
        assert_eq!(
            parse_ci_event("DarkPyonix/darkpyonix-ember"),
            Some(("DarkPyonix/darkpyonix-ember".into(), None))
        );
        assert_eq!(
            parse_ci_event("  DarkPyonix/darkpyonix-ember#develop  "),
            Some((
                "DarkPyonix/darkpyonix-ember".into(),
                Some("develop".into())
            ))
        );
        assert_eq!(
            parse_ci_event("github.com/DarkPyonix/darkpyonix-ember#feat/ci"),
            Some((
                "DarkPyonix/darkpyonix-ember".into(),
                Some("feat/ci".into())
            ))
        );
        assert_eq!(
            parse_ci_event("https://github.com/DarkPyonix/darkpyonix-ember.git/#main"),
            Some(("DarkPyonix/darkpyonix-ember".into(), Some("main".into())))
        );
        assert_eq!(
            parse_ci_event("http://github.com/owner/repo/"),
            Some(("owner/repo".into(), None))
        );
        // The ` (branch)` display form is not accepted as input.
        assert_eq!(parse_ci_event("owner/repo (develop)"), None);
        assert_eq!(parse_ci_event(""), None);
        assert_eq!(parse_ci_event("owner"), None);
        assert_eq!(parse_ci_event("owner/repo/extra"), None);
        assert_eq!(parse_ci_event("owner/repo#"), None);
        assert_eq!(parse_ci_event("owner/repo#a#b"), None);
        assert_eq!(parse_ci_event("owner/repo#bad branch"), None);
    }

    #[test]
    fn fr_l23_ci_event_routine_round_trips_between_server_and_form() {
        let ev_kind = ScheduleKind::Event {
            on: EventOn::CiFailed,
            repository: "DarkPyonix/darkpyonix-ember".into(),
            branch: Some("develop".into()),
        };
        let mut s = schedule(ev_kind.clone(), false);
        s.target = ScheduleTarget::New {
            title: Some("CI 고치기".into()),
        };
        s.next_run_at = None;
        let r = schedule_to_routine(&s);
        assert_eq!(r.trigger, Trigger::Event);
        assert_eq!(r.cron, "CI 실패 시 · DarkPyonix/darkpyonix-ember (develop)");
        assert_eq!(
            schedule_line(&r),
            "CI 실패 시 · DarkPyonix/darkpyonix-ember (develop)"
        );

        let d = FormDraft::from_routine(&r);
        assert_eq!(d.trigger, Trigger::Event);
        assert_eq!(d.cron, "DarkPyonix/darkpyonix-ember#develop");
        assert!(d.errors().is_empty());
        assert!(
            patch_is_empty(&changed_patch(&d, &r)),
            "unedited event routine produces an empty patch"
        );

        let created = new_schedule(&d, "/w/ember");
        assert_eq!(created.kind, ev_kind);
        assert_eq!(schedule_patch(&d).kind, Some(ev_kind));

        let mut d_no_branch = d.clone();
        d_no_branch.cron = "https://github.com/DarkPyonix/darkpyonix-ember".into();
        let p = changed_patch(&d_no_branch, &r);
        assert_eq!(
            p.kind,
            Some(ScheduleKind::Event {
                on: EventOn::CiFailed,
                repository: "DarkPyonix/darkpyonix-ember".into(),
                branch: None,
            })
        );
        let stored = d_no_branch.into_routine("r-ev", true);
        assert_eq!(stored.cron, "CI 실패 시 · DarkPyonix/darkpyonix-ember");
        assert_eq!(
            FormDraft::from_routine(&stored).cron,
            "DarkPyonix/darkpyonix-ember"
        );
    }

    #[test]
    fn fr_l25_existing_event_routine_with_non_repo_schedule_allows_name_only_edit() {
        let mut s = schedule(
            ScheduleKind::Event {
                on: EventOn::CiFailed,
                repository: "legacy-repo".into(),
                branch: None,
            },
            false,
        );
        s.target = ScheduleTarget::New {
            title: Some("PR 리뷰".into()),
        };
        s.next_run_at = None;
        let r = schedule_to_routine(&s);
        let initial = FormDraft::from_routine(&r);
        assert_eq!(initial.cron, "legacy-repo");
        assert_eq!(initial.errors(), vec![FormError::EventInvalid]);
        assert!(
            initial.errors_with_initial(Some(&initial)).is_empty(),
            "unchanged event schedule from an existing routine does not block saving"
        );

        let mut edited = initial.clone();
        edited.name = "PR 리뷰 2".into();
        assert!(edited.errors_with_initial(Some(&initial)).is_empty());
        let p = changed_patch(&edited, &r);
        assert_eq!(
            p.target,
            Some(ScheduleTarget::New {
                title: Some("PR 리뷰 2".into())
            })
        );
        assert_eq!(p.kind, None, "unchanged event schedule is omitted from PATCH");

        // Changing the event field to another invalid value still requires owner/name.
        edited.cron = "still-bad".into();
        assert_eq!(
            edited.errors_with_initial(Some(&initial)),
            vec![FormError::EventInvalid]
        );

        // LOCAL_NOTE does not claim a sample-mode device routine was made offline.
        assert!(!LOCAL_NOTE.contains("오프라인"));
    }
}
