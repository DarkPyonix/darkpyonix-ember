//! Schema for "Sign in with ChatGPT" accounts (store migration 4, SPEC FR-U4).

/// Migration from `user_version` 3 to 4. See [`crate::store`] for the mechanism.
pub const MIGRATION: &str = "
-- One row per signed-in ChatGPT account. The OAuth client is registered per user by OpenAI
-- (`dynamic_agent_client`), so the issued `client_id` belongs to the account, not to Ember.
CREATE TABLE chatgpt_accounts (
    id                  TEXT PRIMARY KEY,
    label               TEXT NOT NULL,
    client_id           TEXT NOT NULL,
    -- The ID token's `sub`; with `client_id` it identifies the ChatGPT user.
    subject             TEXT NOT NULL,
    email               TEXT,
    -- Space-separated scopes as granted.
    scopes              TEXT NOT NULL,
    -- 1 when `chatgpt.tokens.use.direct` was granted (ChatGPT plan usage allowed).
    plan_usage          INTEGER NOT NULL,
    -- signed_in | signed_out | not_eligible
    status              TEXT NOT NULL DEFAULT 'signed_in',
    -- Unix ms.
    access_expires_at   INTEGER,
    refresh_expires_at  INTEGER,
    earliest_refresh_at INTEGER,
    -- XChaCha20-Poly1305 of the JSON {access_token, refresh_token, id_token}
    -- (see accounts::secrets); NULL after sign-out or a failed refresh.
    token_nonce         BLOB,
    token_ciphertext    BLOB,
    limited_until       INTEGER,
    limit_reason        TEXT,
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL
);
CREATE UNIQUE INDEX chatgpt_accounts_identity ON chatgpt_accounts(client_id, subject);

-- This server's stable `ext_agent_host_id` (one row).
CREATE TABLE chatgpt_host (
    id      INTEGER PRIMARY KEY CHECK (id = 1),
    host_id TEXT NOT NULL
);

-- Tokens reported by `response.completed` events, per account and UTC day (FR-U3 usage page).
CREATE TABLE chatgpt_usage (
    account_id    TEXT NOT NULL,
    day           TEXT NOT NULL,
    input_tokens  INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    reports       INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (account_id, day)
);
";
