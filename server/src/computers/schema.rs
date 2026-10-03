//! Schema for computers and each session's current computer (store migration 3).

/// Migration from `user_version` 2 to 3. See [`crate::store`] for the mechanism.
///
/// `IF NOT EXISTS` because development databases from the first interception branch already
/// have these tables (it created them on its own connection, outside the migration list).
/// `session_computer` has no foreign key to `sessions`, so the registry can be used on its own.
pub const MIGRATION: &str = "
CREATE TABLE IF NOT EXISTS computers (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    url         TEXT NOT NULL,
    -- Bearer token for the node API. Never returned to clients.
    token       TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);

-- FR-X3: a session's current computer. No row = never switched (local, no environment block).
CREATE TABLE IF NOT EXISTS session_computer (
    session_id  TEXT PRIMARY KEY,
    computer_id TEXT NOT NULL,
    -- The computer's /v1/env at the time of the switch (JSON).
    env_json    TEXT,
    -- FR-S7 v0 notice still to be delivered with the next message.
    notice      TEXT,
    switched_at INTEGER NOT NULL
);
";
