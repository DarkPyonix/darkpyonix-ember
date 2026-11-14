//! Schema for devices allowed to reach this server over the transport (store migration 6).

/// Migration from `user_version` 5 to 6. See [`crate::store`] for the mechanism.
pub const MIGRATION: &str = "
-- FR-N3: peers (client devices, by transport peer id) allowed to connect. Removing a row is
-- revocation.
CREATE TABLE devices (
    peer_id     TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);
";

/// #366, FR-N8: the device's scope (`full`, `read_only`, `chat_only`). Existing rows keep
/// today's behaviour (`full`). Appended last in [`crate::store`].
pub const SCOPE_MIGRATION: &str = "
ALTER TABLE devices ADD COLUMN scope TEXT NOT NULL DEFAULT 'full';
";
