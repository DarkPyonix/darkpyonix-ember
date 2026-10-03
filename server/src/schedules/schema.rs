//! Schema for scheduled tasks (FR-A8), store migration 12.

/// Migration from `user_version` 11 to 12: schedules and their runs.
///
/// `schedules.kind` is JSON: `{"type":"cron","expr","tz"}`, `{"type":"interval","seconds"}` or
/// `{"type":"once","at"}` (`at` in Unix ms). `target` is JSON: `{"type":"continue","session_id"}`
/// or `{"type":"new","title"}`. `next_run_at` (Unix ms) is the next planned trigger; NULL when
/// paused or when a one-off has fired. `anchor_at` is where interval triggers are counted from.
///
/// `schedule_runs.status`: `running` (prompt sent, turn not over), `completed`, `failed`,
/// `interrupted`, `skipped` (the target session was busy), `missed` (the trigger passed while
/// the server was down or behind). `scheduled_at` is the trigger time the run belongs to.
pub const MIGRATION: &str = "
CREATE TABLE schedules (
    id                  TEXT PRIMARY KEY,
    project             TEXT NOT NULL,
    agent               TEXT NOT NULL,
    account_id          TEXT,
    computer_id         TEXT,
    cwd                 TEXT,
    prompt              TEXT NOT NULL,
    kind                TEXT NOT NULL,
    target              TEXT NOT NULL,
    paused              INTEGER NOT NULL DEFAULT 0,
    catch_up            INTEGER NOT NULL DEFAULT 0,
    anchor_at           INTEGER NOT NULL,
    next_run_at         INTEGER,
    last_run_at         INTEGER,
    created_by_session  TEXT,
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL
);
CREATE INDEX schedules_by_project ON schedules(project);
CREATE INDEX schedules_due ON schedules(paused, next_run_at);

CREATE TABLE schedule_runs (
    id            TEXT PRIMARY KEY,
    schedule_id   TEXT NOT NULL,
    scheduled_at  INTEGER NOT NULL,
    status        TEXT NOT NULL,
    session_id    TEXT,
    started_at    INTEGER,
    finished_at   INTEGER,
    error         TEXT
);
CREATE INDEX schedule_runs_by_schedule ON schedule_runs(schedule_id, scheduled_at);
CREATE INDEX schedule_runs_running ON schedule_runs(status);
";
