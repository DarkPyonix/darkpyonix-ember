//! Schema for accounts, routing and API-key providers (store migration 2).

/// Migration from `user_version` 1 to 2. See [`crate::store`] for the mechanism.
pub const MIGRATION: &str = "
CREATE TABLE accounts (
    id            TEXT PRIMARY KEY,
    agent         TEXT NOT NULL,
    label         TEXT NOT NULL,
    -- The agent's isolated config/credential directory (CLAUDE_CONFIG_DIR / CODEX_HOME).
    config_dir    TEXT NOT NULL,
    -- Login status as last checked: unknown | logged_in | logged_out.
    status        TEXT NOT NULL DEFAULT 'unknown',
    is_default    INTEGER NOT NULL DEFAULT 0,
    -- Rate/usage limited until this Unix time (ms); NULL or past means available.
    limited_until INTEGER,
    limit_reason  TEXT,
    created_at    INTEGER NOT NULL
);
CREATE UNIQUE INDEX accounts_one_default_per_agent ON accounts(agent) WHERE is_default = 1;

-- FR-U2: a session's account is fixed at creation. NULL = the server's own agent login
-- (every session created before accounts existed).
ALTER TABLE sessions ADD COLUMN account_id TEXT;
ALTER TABLE sessions ADD COLUMN account_reason TEXT;
CREATE INDEX sessions_by_account ON sessions(account_id);

-- FR-U3: usage is aggregated from the stored `usage` events; this keeps the per-day scan cheap.
CREATE INDEX events_usage_by_time ON events(at) WHERE json_extract(event, '$.kind') = 'usage';

-- FR-U5: API keys, XChaCha20-Poly1305 encrypted (see accounts::secrets).
CREATE TABLE api_providers (
    id             TEXT PRIMARY KEY,
    label          TEXT NOT NULL,
    kind           TEXT NOT NULL,
    base_url       TEXT,
    key_nonce      BLOB NOT NULL,
    key_ciphertext BLOB NOT NULL,
    created_at     INTEGER NOT NULL
);
";
