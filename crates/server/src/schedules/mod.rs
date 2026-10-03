//! Scheduled tasks (SPEC FR-A8).
//!
//! A schedule sends a prompt to an agent on a cron expression (in a time zone), a fixed
//! interval, or once. Each run either continues an existing session (`target: continue`) or
//! starts a new one in the schedule's project (`target: new`), then sends the prompt through
//! [`Sessions::send`] exactly like a user message, behind a `[ember schedule]` header line. Runs
//! are recorded in `schedule_runs` (status, session, started/finished, error) and pushed.
//!
//! The [`Scheduler`] keeps each schedule's next planned trigger in the store. A trigger found
//! more than [`GRACE_MS`] in the past — the server was down, or asleep — is not silently run:
//! it is recorded as `missed`, with a notice (in the target session for `continue` schedules, and
//! always as a pushed `missed` run). With `catch_up: true` only the latest missed trigger is run;
//! the earlier ones stay `missed`. Never more than one run per schedule per tick.
//!
//! Agents create schedules from a conversation with `ember-a2a schedule add|list|rm`, which
//! calls the token-authenticated routes in [`api`] scoped to the caller's project.
//!
//! Time comes from a [`Clock`], so tests drive the scheduler with a [`ManualClock`] and
//! [`Scheduler::tick`].

pub mod api;
pub mod schema;
pub mod timing;

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use futures::future::BoxFuture;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Deserializer, Serialize};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::Notify;

use crate::agents::AgentKind;
use crate::events::{AgentEvent, SessionStatus, TurnOutcome};
use crate::session::{NewSession, Push, SessionError, Sessions, PUSH_VERSION};

/// A trigger later than this is treated as missed rather than run.
pub const GRACE_MS: i64 = 60_000;
/// At most this many `missed` rows are written for one gap (the notice gives the full count).
pub const MAX_MISSED_ROWS: usize = 20;
/// Schedules one project may have.
pub const MAX_SCHEDULES_PER_PROJECT: usize = 100;
/// Longest the scheduler sleeps between checks (it is also woken by changes).
pub const MAX_SLEEP: Duration = Duration::from_secs(30);
/// Marks the first line of every scheduled prompt.
pub const HEADER_TAG: &str = "[ember schedule]";

/// Wall-clock time in Unix milliseconds.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}

/// The system clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        crate::store::now_ms()
    }
}

/// A clock tests set by hand.
#[derive(Debug, Default)]
pub struct ManualClock(AtomicI64);

impl ManualClock {
    pub fn new(ms: i64) -> Arc<ManualClock> {
        Arc::new(ManualClock(AtomicI64::new(ms)))
    }
    pub fn set(&self, ms: i64) {
        self.0.store(ms, Ordering::SeqCst);
    }
    pub fn advance(&self, ms: i64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// When a schedule fires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScheduleKind {
    /// A five-field cron expression evaluated in `tz` (IANA name; default `UTC`).
    Cron {
        expr: String,
        #[serde(default = "utc")]
        tz: String,
    },
    /// Every `seconds` (at least [`timing::MIN_INTERVAL_SECS`]), counted from creation/resume.
    Interval { seconds: u64 },
    /// Once, at `at`: Unix ms, or an RFC 3339 string on input.
    Once {
        #[serde(deserialize_with = "de_time")]
        at: i64,
    },
}

fn utc() -> String {
    "UTC".into()
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TimeIn {
    Ms(i64),
    Text(String),
}

fn de_time<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    match TimeIn::deserialize(d)? {
        TimeIn::Ms(v) => Ok(v),
        TimeIn::Text(s) => timing::parse_rfc3339(&s).map_err(serde::de::Error::custom),
    }
}

/// What each run does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScheduleTarget {
    /// Send the prompt into this existing session (skipped while it is mid-turn).
    Continue { session_id: String },
    /// Start a new session in the schedule's project and send the prompt there.
    New {
        #[serde(default)]
        title: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Schedule {
    pub id: String,
    pub project: String,
    pub agent: AgentKind,
    pub account_id: Option<String>,
    pub computer_id: Option<String>,
    /// Working directory for `new` sessions.
    pub cwd: Option<String>,
    pub prompt: String,
    pub kind: ScheduleKind,
    pub target: ScheduleTarget,
    pub paused: bool,
    /// After missed triggers, run the latest one (`true`) or only report them (`false`).
    pub catch_up: bool,
    /// Next planned trigger (Unix ms); `None` when paused or a one-off is done.
    pub next_run_at: Option<i64>,
    pub last_run_at: Option<i64>,
    /// The session whose agent created it (`ember-a2a schedule add`), if any.
    pub created_by_session: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(skip)]
    anchor_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// The prompt was sent; the turn has not ended.
    Running,
    Completed,
    Failed,
    Interrupted,
    /// The target session was mid-turn, so nothing was sent.
    Skipped,
    /// The trigger passed while the server was down (or behind); nothing was sent.
    Missed,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Running => "running",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
            RunStatus::Interrupted => "interrupted",
            RunStatus::Skipped => "skipped",
            RunStatus::Missed => "missed",
        }
    }

    pub fn parse(s: &str) -> RunStatus {
        match s {
            "running" => RunStatus::Running,
            "completed" => RunStatus::Completed,
            "interrupted" => RunStatus::Interrupted,
            "skipped" => RunStatus::Skipped,
            "missed" => RunStatus::Missed,
            _ => RunStatus::Failed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScheduleRun {
    pub id: String,
    pub schedule_id: String,
    /// The trigger time this run belongs to (Unix ms).
    pub scheduled_at: i64,
    pub status: RunStatus,
    pub session_id: Option<String>,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    /// Why it failed or was skipped, or the missed-trigger notice.
    pub error: Option<String>,
}

/// `POST /api/v1/schedules`.
#[derive(Debug, Clone, Deserialize)]
pub struct NewSchedule {
    pub project: String,
    /// Required for `new` targets; a `continue` target uses its session's agent.
    #[serde(default)]
    pub agent: Option<String>,
    /// Account for `new` sessions (FR-U2); omitted = the router chooses.
    #[serde(default)]
    pub account: Option<String>,
    /// Computer for `new` sessions (FR-X3); omitted = this server.
    #[serde(default)]
    pub computer: Option<String>,
    /// Working directory for `new` sessions (required for them).
    #[serde(default)]
    pub cwd: Option<String>,
    pub prompt: String,
    pub kind: ScheduleKind,
    pub target: ScheduleTarget,
    #[serde(default)]
    pub catch_up: bool,
    #[serde(default)]
    pub paused: bool,
}

/// `PATCH /api/v1/schedules/{id}`: `None` leaves a field alone.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SchedulePatch {
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub kind: Option<ScheduleKind>,
    #[serde(default)]
    pub catch_up: Option<bool>,
}

#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    #[error("schedule {0} not found")]
    NotFound(String),
    #[error("{0}")]
    BadRequest(String),
    #[error("missing or invalid runtime token")]
    Unauthorized,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<rusqlite::Error> for ScheduleError {
    fn from(e: rusqlite::Error) -> Self {
        ScheduleError::Other(e.into())
    }
}

/// Puts a new session on a computer before its first message (`crate::computers` switch).
pub type ComputerPlacer =
    Arc<dyn Fn(String, String) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

const COLUMNS: &str = "id, project, agent, account_id, computer_id, cwd, prompt, kind, target, paused, catch_up, \
                       anchor_at, next_run_at, last_run_at, created_by_session, created_at, updated_at";

fn row_to_schedule(r: &rusqlite::Row<'_>) -> rusqlite::Result<Schedule> {
    let agent: String = r.get(2)?;
    let kind: String = r.get(7)?;
    let target: String = r.get(8)?;
    let bad = |i: usize, e: serde_json::Error| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
    };
    Ok(Schedule {
        id: r.get(0)?,
        project: r.get(1)?,
        agent: AgentKind::parse(&agent).unwrap_or(AgentKind::Scripted),
        account_id: r.get(3)?,
        computer_id: r.get(4)?,
        cwd: r.get(5)?,
        prompt: r.get(6)?,
        kind: serde_json::from_str(&kind).map_err(|e| bad(7, e))?,
        target: serde_json::from_str(&target).map_err(|e| bad(8, e))?,
        paused: r.get(9)?,
        catch_up: r.get(10)?,
        anchor_at: r.get(11)?,
        next_run_at: r.get(12)?,
        last_run_at: r.get(13)?,
        created_by_session: r.get(14)?,
        created_at: r.get(15)?,
        updated_at: r.get(16)?,
    })
}

const RUN_COLUMNS: &str = "id, schedule_id, scheduled_at, status, session_id, started_at, finished_at, error";

fn row_to_run(r: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduleRun> {
    let status: String = r.get(3)?;
    Ok(ScheduleRun {
        id: r.get(0)?,
        schedule_id: r.get(1)?,
        scheduled_at: r.get(2)?,
        status: RunStatus::parse(&status),
        session_id: r.get(4)?,
        started_at: r.get(5)?,
        finished_at: r.get(6)?,
        error: r.get(7)?,
    })
}

fn bad(msg: impl Into<String>) -> ScheduleError {
    ScheduleError::BadRequest(msg.into())
}

/// Why a run sent nothing.
enum RunFailure {
    Skipped(String),
    Failed(String),
}

pub struct Scheduler {
    sessions: Arc<Sessions>,
    clock: Arc<dyn Clock>,
    placer: StdMutex<Option<ComputerPlacer>>,
    wake: Notify,
    /// Serialises ticks (the loop and tests).
    tick_lock: tokio::sync::Mutex<()>,
}

impl Scheduler {
    pub fn new(sessions: Arc<Sessions>, clock: Arc<dyn Clock>) -> Arc<Scheduler> {
        Arc::new(Scheduler {
            sessions,
            clock,
            placer: StdMutex::new(None),
            wake: Notify::new(),
            tick_lock: tokio::sync::Mutex::new(()),
        })
    }

    /// How `new` sessions with a `computer` get there.
    pub fn set_computer_placer(&self, placer: ComputerPlacer) {
        *self.placer.lock().unwrap() = Some(placer);
    }

    pub fn sessions(&self) -> &Arc<Sessions> {
        &self.sessions
    }

    fn now(&self) -> i64 {
        self.clock.now_ms()
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, rusqlite::Connection> {
        self.sessions.store().conn()
    }

    fn publish(&self, push: Push) {
        self.sessions.publish(push);
    }

    /// Settle what the last server left behind, then run the scheduler loop in the background.
    /// The first tick reports (and, with `catch_up`, runs) triggers missed while it was down.
    pub fn start(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        if let Err(e) = self.recover() {
            tracing::error!("recovering schedule runs failed: {e:#}");
        }
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                this.tick().await;
                let wait = match this.next_due() {
                    Ok(Some(at)) => Duration::from_millis((at - this.now()).max(0) as u64).min(MAX_SLEEP),
                    Ok(None) => MAX_SLEEP,
                    Err(e) => {
                        tracing::error!("reading schedules failed: {e:#}");
                        MAX_SLEEP
                    }
                };
                tokio::select! {
                    _ = tokio::time::sleep(wait.max(Duration::from_millis(200))) => {}
                    _ = this.wake.notified() => {}
                }
            }
        })
    }

    /// Runs still `running` from before a restart can no longer be followed: mark them failed.
    pub fn recover(&self) -> anyhow::Result<usize> {
        Ok(self.conn().execute(
            "UPDATE schedule_runs SET status = 'failed', finished_at = ?1,
                 error = 'the server stopped before this run''s turn ended'
             WHERE status = 'running'",
            params![self.now()],
        )?)
    }

    fn next_due(&self) -> anyhow::Result<Option<i64>> {
        Ok(self.conn().query_row(
            "SELECT MIN(next_run_at) FROM schedules WHERE paused = 0 AND next_run_at IS NOT NULL",
            [],
            |r| r.get(0),
        )?)
    }

    // -----------------------------------------------------------------------------------------
    // Schedules

    pub fn list(&self, project: Option<&str>) -> anyhow::Result<Vec<Schedule>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM schedules WHERE (?1 IS NULL OR project = ?1) ORDER BY created_at, id"
        ))?;
        let rows = stmt.query_map(params![project], row_to_schedule)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn get(&self, id: &str) -> Result<Schedule, ScheduleError> {
        self.conn()
            .query_row(&format!("SELECT {COLUMNS} FROM schedules WHERE id = ?1"), params![id], row_to_schedule)
            .optional()?
            .ok_or_else(|| ScheduleError::NotFound(id.to_string()))
    }

    /// Check a kind against the clock: valid, and fires at least once from `now`.
    fn first_trigger(&self, kind: &ScheduleKind, anchor: i64) -> Result<i64, ScheduleError> {
        timing::validate(kind).map_err(bad)?;
        let now = self.now();
        if let ScheduleKind::Once { at } = kind {
            // A moment in the past (clock skew, typing) still fires right away.
            if *at < now - GRACE_MS {
                return Err(bad(format!("time {} is in the past", timing::rfc3339(*at))));
            }
            return Ok((*at).max(now));
        }
        timing::next_after(kind, anchor, now)
            .map_err(bad)?
            .ok_or_else(|| bad("this schedule would never fire"))
    }

    /// Create a schedule (FR-A8). `created_by` is the agent session that asked for it, if any.
    pub fn create(&self, new: NewSchedule, created_by: Option<&str>) -> Result<Schedule, ScheduleError> {
        let project = new.project.trim().to_string();
        if project.is_empty() {
            return Err(bad("project must not be empty"));
        }
        if new.prompt.trim().is_empty() {
            return Err(bad("prompt must not be empty"));
        }
        let agent = match (&new.target, new.agent.as_deref()) {
            (ScheduleTarget::Continue { session_id }, requested) => {
                let rec = self
                    .sessions
                    .store()
                    .session(session_id)?
                    .ok_or_else(|| bad(format!("session {session_id} not found")))?;
                if rec.project != project {
                    return Err(bad(format!("session {session_id} is not in project {project:?}")));
                }
                if let Some(a) = requested {
                    if AgentKind::parse(a) != Some(rec.agent) {
                        return Err(bad(format!(
                            "session {session_id} runs {}, not {a}",
                            rec.agent.as_str()
                        )));
                    }
                }
                if new.computer.is_some() || new.account.is_some() || new.cwd.is_some() {
                    return Err(bad(
                        "account, computer and cwd apply to new sessions only; a continued session keeps its own",
                    ));
                }
                rec.agent
            }
            (ScheduleTarget::New { .. }, Some(a)) => {
                let kind = AgentKind::parse(a).ok_or_else(|| bad(format!("unknown agent {a}")))?;
                if new.cwd.as_deref().is_none_or(|c| c.trim().is_empty()) {
                    return Err(bad("cwd is required for schedules that start new sessions"));
                }
                kind
            }
            (ScheduleTarget::New { .. }, None) => return Err(bad("agent is required for schedules that start new sessions")),
        };
        let count: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM schedules WHERE project = ?1",
            params![project],
            |r| r.get(0),
        )?;
        if count as usize >= MAX_SCHEDULES_PER_PROJECT {
            return Err(bad(format!("project {project:?} already has {MAX_SCHEDULES_PER_PROJECT} schedules")));
        }

        let now = self.now();
        let next = self.first_trigger(&new.kind, now)?;
        let id = format!("sch_{}", uuid::Uuid::new_v4().simple());
        self.conn().execute(
            &format!(
                "INSERT INTO schedules ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, NULL, ?14, ?15, ?15)"
            ),
            params![
                id,
                project,
                agent.as_str(),
                new.account,
                new.computer,
                new.cwd,
                new.prompt,
                serde_json::to_string(&new.kind).map_err(anyhow::Error::from)?,
                serde_json::to_string(&new.target).map_err(anyhow::Error::from)?,
                new.paused,
                new.catch_up,
                now,
                (!new.paused).then_some(next),
                created_by,
                now
            ],
        )?;
        let schedule = self.get(&id)?;
        self.publish(Push::ScheduleCreated { v: PUSH_VERSION, schedule: schedule.clone() });
        self.wake.notify_one();
        Ok(schedule)
    }

    pub fn update(&self, id: &str, patch: SchedulePatch) -> Result<Schedule, ScheduleError> {
        let current = self.get(id)?;
        if patch.prompt.as_deref().is_some_and(|p| p.trim().is_empty()) {
            return Err(bad("prompt must not be empty"));
        }
        let now = self.now();
        let (kind, anchor, next) = match &patch.kind {
            Some(kind) => {
                let next = self.first_trigger(kind, now)?;
                (kind.clone(), now, (!current.paused).then_some(next))
            }
            None => (current.kind.clone(), current.anchor_at, current.next_run_at),
        };
        self.conn().execute(
            "UPDATE schedules SET prompt = COALESCE(?2, prompt), kind = ?3, catch_up = COALESCE(?4, catch_up),
                 anchor_at = ?5, next_run_at = ?6, updated_at = ?7
             WHERE id = ?1",
            params![
                id,
                patch.prompt,
                serde_json::to_string(&kind).map_err(anyhow::Error::from)?,
                patch.catch_up,
                anchor,
                next,
                now
            ],
        )?;
        self.updated(id)
    }

    fn updated(&self, id: &str) -> Result<Schedule, ScheduleError> {
        let schedule = self.get(id)?;
        self.publish(Push::ScheduleUpdated { v: PUSH_VERSION, schedule: schedule.clone() });
        self.wake.notify_one();
        Ok(schedule)
    }

    /// Stop firing until resumed. Triggers while paused are not "missed".
    pub fn pause(&self, id: &str) -> Result<Schedule, ScheduleError> {
        self.get(id)?;
        self.conn().execute(
            "UPDATE schedules SET paused = 1, next_run_at = NULL, updated_at = ?2 WHERE id = ?1",
            params![id, self.now()],
        )?;
        self.updated(id)
    }

    /// Fire again from now on: intervals restart their count, cron resumes at its next match, a
    /// one-off still ahead keeps its time (one already past stays done).
    pub fn resume(&self, id: &str) -> Result<Schedule, ScheduleError> {
        let s = self.get(id)?;
        let now = self.now();
        let next = match &s.kind {
            ScheduleKind::Once { at } => (s.last_run_at.is_none() && *at >= now - GRACE_MS).then_some((*at).max(now)),
            kind => timing::next_after(kind, now, now).map_err(bad)?,
        };
        self.conn().execute(
            "UPDATE schedules SET paused = 0, anchor_at = ?2, next_run_at = ?3, updated_at = ?2 WHERE id = ?1",
            params![id, now, next],
        )?;
        self.updated(id)
    }

    pub fn delete(&self, id: &str) -> Result<(), ScheduleError> {
        let s = self.get(id)?;
        {
            let mut conn = self.conn();
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM schedule_runs WHERE schedule_id = ?1", params![id])?;
            tx.execute("DELETE FROM schedules WHERE id = ?1", params![id])?;
            tx.commit()?;
        }
        self.publish(Push::ScheduleDeleted { v: PUSH_VERSION, id: id.to_string(), project: s.project });
        Ok(())
    }

    /// A schedule's runs, newest first.
    pub fn runs(&self, id: &str, limit: usize) -> Result<Vec<ScheduleRun>, ScheduleError> {
        self.get(id)?;
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM schedule_runs WHERE schedule_id = ?1
             ORDER BY scheduled_at DESC, COALESCE(started_at, 0) DESC, rowid DESC LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![id, limit as i64], row_to_run)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn run(&self, id: &str) -> Result<ScheduleRun, ScheduleError> {
        self.conn()
            .query_row(&format!("SELECT {RUN_COLUMNS} FROM schedule_runs WHERE id = ?1"), params![id], row_to_run)
            .optional()?
            .ok_or_else(|| ScheduleError::NotFound(id.to_string()))
    }

    // -----------------------------------------------------------------------------------------
    // Runs

    fn insert_run(
        &self,
        schedule_id: &str,
        scheduled_at: i64,
        status: RunStatus,
        started_at: Option<i64>,
        error: Option<&str>,
    ) -> anyhow::Result<ScheduleRun> {
        let id = format!("run_{}", uuid::Uuid::new_v4().simple());
        let finished_at = (status != RunStatus::Running).then(|| self.now());
        self.conn().execute(
            &format!("INSERT INTO schedule_runs ({RUN_COLUMNS}) VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7)"),
            params![id, schedule_id, scheduled_at, status.as_str(), started_at, finished_at, error],
        )?;
        let run = self.run(&id).map_err(|e| anyhow::anyhow!("{e}"))?;
        self.publish(Push::ScheduleRun { v: PUSH_VERSION, run: run.clone() });
        Ok(run)
    }

    /// Update a run and push it. Only a `running` run changes status (a finished one stays).
    fn finish_run(&self, run_id: &str, status: RunStatus, error: Option<&str>) -> anyhow::Result<()> {
        let n = self.conn().execute(
            "UPDATE schedule_runs SET status = ?2, finished_at = ?3, error = ?4 WHERE id = ?1 AND status = 'running'",
            params![run_id, status.as_str(), self.now(), error],
        )?;
        if n > 0 {
            if let Ok(run) = self.run(run_id) {
                self.publish(Push::ScheduleRun { v: PUSH_VERSION, run });
            }
        }
        Ok(())
    }

    /// Run `id` now, outside its schedule (`run-now`). Does not move its next trigger.
    pub async fn run_now(self: &Arc<Self>, id: &str) -> Result<ScheduleRun, ScheduleError> {
        let s = self.get(id)?;
        Ok(self.execute(&s, self.now()).await?)
    }

    /// Fire everything due. Returns the runs recorded (missed ones included), oldest first.
    pub async fn tick(self: &Arc<Self>) -> Vec<ScheduleRun> {
        let _guard = self.tick_lock.lock().await;
        let now = self.now();
        let due: Vec<Schedule> = match self.due(now) {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("listing due schedules failed: {e:#}");
                return Vec::new();
            }
        };
        let mut recorded = Vec::new();
        let mut to_run = Vec::new();
        for s in due {
            match self.plan(&s, now) {
                Ok((missed, run_at)) => {
                    recorded.extend(missed);
                    if let Some(at) = run_at {
                        to_run.push((s, at));
                    }
                }
                Err(e) => tracing::error!(schedule = %s.id, "scheduling failed: {e:#}"),
            }
        }
        let runs = futures::future::join_all(to_run.iter().map(|(s, at)| self.execute(s, *at))).await;
        for r in runs {
            match r {
                Ok(run) => recorded.push(run),
                Err(e) => tracing::error!("recording a schedule run failed: {e:#}"),
            }
        }
        recorded
    }

    fn due(&self, now: i64) -> anyhow::Result<Vec<Schedule>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM schedules WHERE paused = 0 AND next_run_at IS NOT NULL AND next_run_at <= ?1
             ORDER BY next_run_at"
        ))?;
        let rows = stmt.query_map(params![now], row_to_schedule)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Advance a due schedule past `now` and decide what happens to the triggers it passed:
    /// returns the `missed` runs recorded and the trigger time to run, if any.
    fn plan(&self, s: &Schedule, now: i64) -> anyhow::Result<(Vec<ScheduleRun>, Option<i64>)> {
        let first = s.next_run_at.expect("due schedules have a next trigger");
        let due = timing::occurrences_between(&s.kind, s.anchor_at, first, now, MAX_MISSED_ROWS + 1)
            .map_err(|e| anyhow::anyhow!(e))?;
        let next = timing::next_after(&s.kind, s.anchor_at, now).map_err(|e| anyhow::anyhow!(e))?;
        let on_time = now - due.latest <= GRACE_MS;
        let run_at = (on_time || s.catch_up).then_some(due.latest);
        self.conn().execute(
            "UPDATE schedules SET next_run_at = ?2, last_run_at = COALESCE(?3, last_run_at) WHERE id = ?1",
            params![s.id, next, run_at],
        )?;

        let missed_count = due.count - usize::from(run_at.is_some());
        if missed_count == 0 {
            return Ok((Vec::new(), run_at));
        }
        let mut missed_times: Vec<i64> = due.recent.clone();
        if run_at.is_some() {
            missed_times.pop();
        }
        if missed_times.len() > MAX_MISSED_ROWS {
            missed_times.drain(..missed_times.len() - MAX_MISSED_ROWS);
        }
        let last_missed = *missed_times.last().unwrap_or(&due.first);
        let what = if run_at.is_some() {
            format!("the latest, {}, is run now (catch_up)", timing::rfc3339(due.latest))
        } else {
            "none was run (catch_up is off)".to_string()
        };
        let notice = format!(
            "{HEADER_TAG} Schedule {} missed {missed_count} trigger(s) between {} and {} while the \
             server was down or behind; {what}.",
            s.id,
            timing::rfc3339(due.first),
            timing::rfc3339(last_missed),
        );
        tracing::warn!(schedule = %s.id, missed = missed_count, "{notice}");
        let mut runs = Vec::new();
        for at in missed_times {
            runs.push(self.insert_run(&s.id, at, RunStatus::Missed, None, Some(&notice))?);
        }
        if let ScheduleTarget::Continue { session_id } = &s.target {
            let event = AgentEvent::Notice { message: notice.clone() };
            if let Err(e) = self.sessions.record_event(session_id, &event) {
                tracing::warn!(session = %session_id, "recording the missed-schedule notice failed: {e:#}");
            }
        }
        Ok((runs, run_at))
    }

    /// One run of `s` for the trigger at `scheduled_at`: record it, send the prompt, and follow
    /// the turn in the background.
    async fn execute(self: &Arc<Self>, s: &Schedule, scheduled_at: i64) -> anyhow::Result<ScheduleRun> {
        let run = self.insert_run(&s.id, scheduled_at, RunStatus::Running, Some(self.now()), None)?;
        // Subscribe before sending so the turn's end cannot be missed.
        let rx = self.sessions.subscribe();
        match self.send(s, &run.id).await {
            Ok((session_id, before)) => {
                self.conn().execute(
                    "UPDATE schedule_runs SET session_id = ?2 WHERE id = ?1",
                    params![run.id, session_id],
                )?;
                if let Ok(r) = self.run(&run.id) {
                    self.publish(Push::ScheduleRun { v: PUSH_VERSION, run: r });
                }
                tokio::spawn(follow_turn(Arc::downgrade(self), run.id.clone(), session_id, before, rx));
            }
            Err(RunFailure::Skipped(why)) => self.finish_run(&run.id, RunStatus::Skipped, Some(&why))?,
            Err(RunFailure::Failed(why)) => {
                tracing::warn!(schedule = %s.id, "schedule run failed: {why}");
                self.finish_run(&run.id, RunStatus::Failed, Some(&why))?
            }
        }
        Ok(self.run(&run.id).map_err(|e| anyhow::anyhow!("{e}"))?)
    }

    /// The text sent for a run.
    fn render(&self, s: &Schedule, run_id: &str) -> String {
        format!(
            "{HEADER_TAG} Scheduled task {} (run {run_id}). This message was sent by a schedule, not \
             typed by the user just now.\n\n{}",
            s.id, s.prompt
        )
    }

    /// Send the prompt; returns the session and its last event seq before the send.
    async fn send(&self, s: &Schedule, run_id: &str) -> Result<(String, i64), RunFailure> {
        let fail = |e: &dyn std::fmt::Display| RunFailure::Failed(format!("{e:#}"));
        let text = self.render(s, run_id);
        match &s.target {
            ScheduleTarget::Continue { session_id } => {
                let rec = self
                    .sessions
                    .store()
                    .session(session_id)
                    .map_err(|e| fail(&e))?
                    .ok_or_else(|| RunFailure::Failed(format!("session {session_id} no longer exists")))?;
                if matches!(rec.status, SessionStatus::Running | SessionStatus::WaitingForApproval) {
                    return Err(RunFailure::Skipped(format!("session {session_id} was mid-turn")));
                }
                self.sessions.send(session_id, &text).await.map_err(|e| fail(&e))?;
                Ok((session_id.clone(), rec.last_seq))
            }
            ScheduleTarget::New { title } => {
                let title = title.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| {
                    let mut t: String = s.prompt.chars().take(40).collect();
                    if s.prompt.chars().count() > 40 {
                        t.push('\u{2026}');
                    }
                    format!("Scheduled: {t}")
                });
                let rec = self
                    .sessions
                    .create_with_account(
                        NewSession {
                            project: s.project.clone(),
                            agent: s.agent,
                            cwd: s.cwd.clone().unwrap_or_default().into(),
                            model: None,
                            title,
                        },
                        s.account_id.as_deref(),
                    )
                    .map_err(|e: SessionError| fail(&e))?;
                if let Some(computer) = &s.computer_id {
                    let placer = self.placer.lock().unwrap().clone();
                    match placer {
                        Some(place) => place(rec.id.clone(), computer.clone()).await.map_err(|e| {
                            RunFailure::Failed(format!("putting session {} on computer {computer}: {e:#}", rec.id))
                        })?,
                        None => {
                            return Err(RunFailure::Failed(format!(
                                "computer {computer} requested but computers are not enabled"
                            )))
                        }
                    }
                }
                self.sessions.send(&rec.id, &text).await.map_err(|e| fail(&e))?;
                Ok((rec.id, 0))
            }
        }
    }
}

/// Finish run `run_id` when `session_id`'s turn after `before` ends.
async fn follow_turn(
    scheduler: std::sync::Weak<Scheduler>,
    run_id: String,
    session_id: String,
    before: i64,
    mut rx: tokio::sync::broadcast::Receiver<Push>,
) {
    let outcome_status = |o: TurnOutcome| match o {
        TurnOutcome::Completed => RunStatus::Completed,
        TurnOutcome::Interrupted => RunStatus::Interrupted,
        TurnOutcome::Failed => RunStatus::Failed,
    };
    loop {
        let ended: Option<TurnOutcome> = match rx.recv().await {
            Ok(Push::Event { event, .. }) if event.session_id == session_id && event.seq > before => match event.event {
                AgentEvent::TurnEnded { outcome } => Some(outcome),
                _ => None,
            },
            Ok(_) => None,
            Err(RecvError::Lagged(_)) => {
                // Missed pushes: look in the store instead.
                let Some(this) = scheduler.upgrade() else { return };
                this.sessions.store().events_after(&session_id, before).ok().and_then(|evs| {
                    evs.into_iter().find_map(|e| match e.event {
                        AgentEvent::TurnEnded { outcome } => Some(outcome),
                        _ => None,
                    })
                })
            }
            Err(RecvError::Closed) => return,
        };
        if let Some(outcome) = ended {
            let Some(this) = scheduler.upgrade() else { return };
            let status = outcome_status(outcome);
            let error = (status != RunStatus::Completed).then(|| format!("the turn ended {}", status.as_str()));
            if let Err(e) = this.finish_run(&run_id, status, error.as_deref()) {
                tracing::warn!(run = %run_id, "recording the end of a schedule run failed: {e:#}");
            }
            return;
        }
    }
}
