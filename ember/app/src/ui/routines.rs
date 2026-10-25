//! The 루틴 (Routines) screen (FR-L25, `docs/DESIGN.md` section 6.5): a dashboard on top (KPIs,
//! success ring, weekday bars), the routines grouped by project below it, a detail panel and the
//! routine form.
//!
//! The decisions are plain functions and types in the first half of this file, unit-tested
//! without a renderer. The components in the second half only hand them to dioxus-compose.
//!
//! The server has a schedule API (FR-A8, `ember/server/src/schedules/`) but `ember-client` does
//! not wrap it yet, so the screen runs on [`RoutineStore`]. [`LocalStore`] keeps routines in
//! memory for this app run and starts empty: nothing here is sample data. Run history and
//! "지금 실행" need the server, so they say [`UNAVAILABLE`].

use super::home::{DEFAULT_PROJECT, Harness, PermMode, Selection, default_mode, default_option};

/// What a part shows or says while the server does not provide it.
pub const UNAVAILABLE: &str = "준비 중입니다";

/// Minutes since 1970-01-01 00:00 UTC. Local time is this plus the offset the caller passes.
pub type Minutes = i64;

/// TODO(server): no time-zone source in the app yet; the server evaluates cron in UTC by default.
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
    pub prompt: String,
    pub enabled: bool,
    /// Project, model and effort, mode and computer, the same choices as the Home composer.
    pub sel: Selection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Done,
    Failed,
    Running,
}

impl RunStatus {
    pub fn label(self) -> &'static str {
        match self {
            RunStatus::Done => "성공",
            RunStatus::Failed => "실패",
            RunStatus::Running => "실행 중",
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
}

/// The schedule sentence: "매일 오전 9시", or the event text.
pub fn schedule_line(r: &Routine) -> String {
    match r.trigger {
        Trigger::Time => describe_cron(&r.cron),
        Trigger::Event => r.cron.clone(),
    }
}

/// "실행 기록 없음", or "성공 · 3시간 전" from the newest run.
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

// ---------------------------------------------------------------------------------------------
// Form
// ---------------------------------------------------------------------------------------------

const DEFAULT_CRON: &str = "0 9 * * *";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormDraft {
    pub name: String,
    pub trigger: Trigger,
    pub cron: String,
    pub prompt: String,
    pub sel: Selection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormError {
    NameEmpty,
    ScheduleInvalid,
    EventEmpty,
    PromptEmpty,
}

impl FormError {
    pub fn message(self) -> &'static str {
        match self {
            FormError::NameEmpty => "이름을 입력하셔야 합니다",
            FormError::ScheduleInvalid => "일정이 올바른 cron 형식이 아닙니다",
            FormError::EventEmpty => "이벤트 설명을 입력하셔야 합니다",
            FormError::PromptEmpty => "프롬프트를 입력하셔야 합니다",
        }
    }
}

impl FormDraft {
    /// A new routine: every day at 09:00, Claude Code's default, ask mode, the main server.
    pub fn new(project: &str) -> FormDraft {
        FormDraft {
            name: String::new(),
            trigger: Trigger::Time,
            cron: DEFAULT_CRON.into(),
            prompt: String::new(),
            sel: Selection {
                project: project.into(),
                ..Selection::initial()
            },
        }
    }

    pub fn from_routine(r: &Routine) -> FormDraft {
        FormDraft {
            name: r.name.clone(),
            trigger: r.trigger,
            cron: r.cron.clone(),
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
        let mut out = Vec::new();
        if self.name.trim().is_empty() {
            out.push(FormError::NameEmpty);
        }
        match self.trigger {
            Trigger::Time if !cron_valid(self.cron.trim()) => out.push(FormError::ScheduleInvalid),
            Trigger::Event if self.cron.trim().is_empty() => out.push(FormError::EventEmpty),
            _ => {}
        }
        if self.prompt.trim().is_empty() {
            out.push(FormError::PromptEmpty);
        }
        out
    }

    /// The routine to store. Text is trimmed.
    pub fn into_routine(&self, id: &str, enabled: bool) -> Routine {
        Routine {
            id: id.into(),
            name: self.name.trim().into(),
            trigger: self.trigger,
            cron: self.cron.trim().into(),
            prompt: self.prompt.trim().into(),
            enabled,
            sel: self.sel.clone(),
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
        .filter_map(|e| next_run(&e.routine.cron, now).map(|t| (t, &e.routine.name)))
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
    /// The server cannot do it yet: shown as [`UNAVAILABLE`].
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

#[cfg(test)]
mod tests {
    use super::*;

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
        Entry { routine: r, runs }
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

    // ---- text -------------------------------------------------------------------------------

    #[test]
    fn nfr_l4_routine_text_has_no_plain_da_endings() {
        let mut strings: Vec<String> = vec![UNAVAILABLE.into()];
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
                !s.ends_with('다') || s.ends_with("입니다") || s.ends_with("합니다"),
                "{s}"
            );
        }
    }
}
