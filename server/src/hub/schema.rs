//! Schema for the server's hub registration (store migration 10, FR-N2).

/// Migration from `user_version` 7 to 8. See [`crate::store`] for the mechanism.
pub const MIGRATION: &str = "
-- FR-N2: this server's registration with the darkpyonix.dev hub (one row). The device token is
-- sealed with the server's secret.key (the FR-U5 scheme, AAD ember/hub-device-token/v1:<endpoint>).
CREATE TABLE hub_registration (
    id               INTEGER PRIMARY KEY CHECK (id = 1),
    hub_url          TEXT NOT NULL,
    endpoint_id      TEXT NOT NULL,
    device_json      TEXT NOT NULL,
    token_nonce      BLOB NOT NULL,
    token_ciphertext BLOB NOT NULL,
    registered_at    INTEGER NOT NULL,
    revoked_at       INTEGER
);
-- FR-N3 rows synced from the hub account are 'hub'; rows added by hand are 'local'.
ALTER TABLE devices ADD COLUMN source TEXT NOT NULL DEFAULT 'local';
";
