//! Schema for computers and each session's current computer (store migrations 3 and 5).

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
    -- The computer's /env at the time of the switch (JSON).
    env_json    TEXT,
    -- FR-S7 v0 notice still to be delivered with the next message.
    notice      TEXT,
    switched_at INTEGER NOT NULL
);
";

/// Migration from `user_version` 4 to 5: a computer may be addressed by transport peer
/// (`FR-N1`) instead of an HTTP URL. `peer_json` is an `ember_transport::PeerAddr` as JSON;
/// such rows have an empty `url`.
pub const PEER_MIGRATION: &str = "
ALTER TABLE computers ADD COLUMN peer_json TEXT;
";

/// Migration for FR-C2c (#633): extracted transport peer id column with a partial UNIQUE index,
/// so duplicate peer ids are refused by an O(log n) index lookup and enforced by SQLite.
pub const PEER_ID_UNIQUE_MIGRATION: &str = "
ALTER TABLE computers ADD COLUMN peer_id TEXT;
UPDATE computers
SET peer_id = lower(json_extract(peer_json, '$.peer'))
WHERE peer_json IS NOT NULL
  AND json_valid(peer_json)
  AND typeof(json_extract(peer_json, '$.peer')) = 'text'
  AND length(json_extract(peer_json, '$.peer')) = 64;
CREATE UNIQUE INDEX IF NOT EXISTS computers_peer_id_idx
ON computers(peer_id)
WHERE peer_id IS NOT NULL;
";

/// Migration for FR-X3: the computer a user designated for a session. The session falls back to
/// the main server when it is lost and returns when it is detected again.
pub const DESIGNATED_MIGRATION: &str = "
CREATE TABLE IF NOT EXISTS session_designated (
    session_id  TEXT PRIMARY KEY,
    computer_id TEXT NOT NULL
);
";
