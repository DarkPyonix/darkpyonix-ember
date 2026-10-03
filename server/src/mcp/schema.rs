//! Schema for the central MCP server registry (FR-A7), store migration 11.

/// Migration from `user_version` 10 to 11: MCP servers handed to every agent session.
///
/// `args` and `env_keys` are JSON arrays of strings. The environment's values are sealed
/// together as one JSON object (`{name: value}`) with the server's at-rest key
/// (`accounts::secrets::SecretBox`, AAD `ember/mcp-env/v1:<id>`): `env_nonce` (24 bytes) and
/// `env_ciphertext`, both NULL when the server has no environment. Only the names are stored in
/// the clear (`env_keys`), so listing never decrypts. `scope` is `all` or `project:<name>`.
pub const MIGRATION: &str = "
CREATE TABLE mcp_servers (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL UNIQUE,
    command         TEXT NOT NULL,
    args            TEXT NOT NULL,
    env_keys        TEXT NOT NULL,
    env_nonce       BLOB,
    env_ciphertext  BLOB,
    enabled         INTEGER NOT NULL DEFAULT 1,
    scope           TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);
";
