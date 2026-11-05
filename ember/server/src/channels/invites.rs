//! Channel invites (SPEC FR-T14): the store half. An owner creates a single-use, channel-scoped
//! invite; only the SHA-256 of its token is stored, and the plaintext is returned once. Accepting
//! an invite and the guest routes are not built here. All of it is [provisional] except the
//! channel-scoped link itself (`INTENT.md` D22).

use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::ChannelError;
use crate::store::{now_ms, Store};

/// Store migration (appended last to `store::MIGRATIONS`): invites and the guests made from them.
/// Token and credential columns hold SHA-256 hex, never the value. Times are Unix milliseconds.
pub const MIGRATION: &str = "
CREATE TABLE channel_invites (
    id          TEXT PRIMARY KEY,
    channel_id  TEXT NOT NULL REFERENCES channels(id),
    token_hash  TEXT NOT NULL UNIQUE,
    created_by  TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL,
    used_at     INTEGER,
    revoked_at  INTEGER
);
CREATE INDEX channel_invites_by_channel ON channel_invites(channel_id);
CREATE TABLE channel_guests (
    id              TEXT PRIMARY KEY,
    channel_id      TEXT NOT NULL REFERENCES channels(id),
    invite_id       TEXT NOT NULL REFERENCES channel_invites(id),
    name            TEXT NOT NULL,
    credential_hash TEXT NOT NULL UNIQUE,
    created_at      INTEGER NOT NULL,
    expires_at      INTEGER NOT NULL,
    revoked_at      INTEGER,
    allow_until     INTEGER
);
CREATE INDEX channel_guests_by_invite ON channel_guests(invite_id);
";

/// Invite token prefix; a leaked value is recognizable.
pub const INVITE_PREFIX: &str = "dpi_";
/// Guest credential prefix (used by the accept route, PR 2).
pub const CREDENTIAL_PREFIX: &str = "dpg_";
/// Default invite lifetime in hours [provisional].
pub const DEFAULT_TTL_HOURS: i64 = 24;
/// Longest invite lifetime in hours.
pub const MAX_TTL_HOURS: i64 = 168;

/// An invite as the owner sees it. Never carries the token or its hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Invite {
    pub id: String,
    pub channel_id: String,
    pub created_by: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub used_at: Option<i64>,
    pub revoked_at: Option<i64>,
    /// The name of the guest who accepted it, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guest_name: Option<String>,
}

/// SHA-256 of `secret` as lowercase hex.
pub fn hash_secret(secret: &str) -> String {
    Sha256::digest(secret.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// A fresh secret: `prefix` plus 256 random bits (two v4 UUIDs, as the server token) in hex.
pub fn new_secret(prefix: &str) -> String {
    format!("{prefix}{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

const INVITE_COLUMNS: &str = "i.id, i.channel_id, i.created_by, i.created_at, i.expires_at, i.used_at, i.revoked_at,
    (SELECT g.name FROM channel_guests g WHERE g.invite_id = i.id)";

fn invite_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Invite> {
    Ok(Invite {
        id: r.get(0)?,
        channel_id: r.get(1)?,
        created_by: r.get(2)?,
        created_at: r.get(3)?,
        expires_at: r.get(4)?,
        used_at: r.get(5)?,
        revoked_at: r.get(6)?,
        guest_name: r.get(7)?,
    })
}

impl Store {
    /// Create an invite for `channel_id`. `ttl_hours` defaults to 24 and must be 1..=168. Returns the
    /// invite and its plaintext token, which exists nowhere else.
    pub fn create_invite(
        &self,
        channel_id: &str,
        created_by: &str,
        ttl_hours: Option<i64>,
    ) -> Result<(Invite, String), ChannelError> {
        let ttl = ttl_hours.unwrap_or(DEFAULT_TTL_HOURS);
        if !(1..=MAX_TTL_HOURS).contains(&ttl) {
            return Err(ChannelError::BadRequest(format!("ttl_hours must be a whole number from 1 to {MAX_TTL_HOURS}")));
        }
        self.channel_row(channel_id)?;
        let token = new_secret(INVITE_PREFIX);
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_ms();
        let invite = Invite {
            id,
            channel_id: channel_id.to_string(),
            created_by: created_by.to_string(),
            created_at: now,
            expires_at: now + ttl * 3_600_000,
            used_at: None,
            revoked_at: None,
            guest_name: None,
        };
        self.conn().execute(
            "INSERT INTO channel_invites (id, channel_id, token_hash, created_by, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![invite.id, channel_id, hash_secret(&token), created_by, invite.created_at, invite.expires_at],
        )?;
        Ok((invite, token))
    }

    /// The channel's invites, newest first, without tokens.
    pub fn list_invites(&self, channel_id: &str) -> Result<Vec<Invite>, ChannelError> {
        self.channel_row(channel_id)?;
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {INVITE_COLUMNS} FROM channel_invites i WHERE i.channel_id = ?1 ORDER BY i.created_at DESC, i.rowid DESC"
        ))?;
        let rows = stmt.query_map(params![channel_id], invite_row)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Revoke an invite and, at once, the guest credential made from it. Revoking twice keeps the
    /// first time.
    pub fn revoke_invite(&self, channel_id: &str, invite_id: &str) -> Result<(), ChannelError> {
        self.channel_row(channel_id)?;
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let now = now_ms();
        let found = tx.execute(
            "UPDATE channel_invites SET revoked_at = COALESCE(revoked_at, ?1) WHERE id = ?2 AND channel_id = ?3",
            params![now, invite_id, channel_id],
        )?;
        if found == 0 {
            return Err(ChannelError::NotFound(format!("invite {invite_id}")));
        }
        tx.execute(
            "UPDATE channel_guests SET revoked_at = COALESCE(revoked_at, ?1), allow_until = NULL WHERE invite_id = ?2",
            params![now, invite_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The usable invite for a plaintext token: found by the hash, `None` when unknown, expired,
    /// used or revoked.
    pub fn invite_by_token(&self, token: &str) -> Result<Option<Invite>, ChannelError> {
        self.invite_by_token_at(token, now_ms())
    }

    /// As [`Store::invite_by_token`] with the clock given.
    pub fn invite_by_token_at(&self, token: &str, now: i64) -> Result<Option<Invite>, ChannelError> {
        let invite = self
            .conn()
            .query_row(
                &format!("SELECT {INVITE_COLUMNS} FROM channel_invites i WHERE i.token_hash = ?1"),
                params![hash_secret(token)],
                invite_row,
            )
            .optional()?;
        Ok(invite.filter(|i| i.used_at.is_none() && i.revoked_at.is_none() && i.expires_at > now))
    }
}
