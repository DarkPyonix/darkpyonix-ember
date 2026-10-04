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
