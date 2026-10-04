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

/// Migration from `user_version` 10 to 11 (darkpyonix-core PR #34):
/// - the read-only **resolve token** (`dpr_...`, NFR-H2) that goes in the pkarr resolver's URL
///   instead of the device token, sealed like it (AAD `ember/hub-resolve-token/v1:<endpoint>`);
///   `NULL` for a registration made before the hub issued them (one is fetched on start);
/// - the **pending link** (`link_id`), so a server restarted while waiting for approval resumes
///   it with `GET /device-links/{link_id}` instead of starting over.
pub const RESOLVE_MIGRATION: &str = "
ALTER TABLE hub_registration ADD COLUMN resolve_nonce BLOB;
ALTER TABLE hub_registration ADD COLUMN resolve_ciphertext BLOB;
CREATE TABLE hub_pending_link (
    id         INTEGER PRIMARY KEY CHECK (id = 1),
    hub_url    TEXT NOT NULL,
    link_id    TEXT NOT NULL,
    expires_at INTEGER NOT NULL
);
";
