//! Channel invites (SPEC FR-T14): the store half. An owner creates a single-use, channel-scoped
//! invite; only the SHA-256 of its token is stored, and the plaintext is returned once. Accepting
//! an invite makes a guest with one credential (`dpg_...`, also stored as a hash only); the guest
//! routes are in [`super::guest`]. All of it is [provisional] except the channel-scoped link itself
//! (`INTENT.md` D22).

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
/// Guest credential prefix.
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

/// A fresh secret: `prefix` plus 244 random bits in hex (two v4 UUIDs, 122 random bits each, from
/// the OS CSPRNG, as the server token).
pub fn new_secret(prefix: &str) -> String {
    format!("{prefix}{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// Longest guest name in characters.
pub const MAX_NAME_CHARS: usize = 64;

/// Names a guest may not take, compared trimmed and lowercased: they are the labels of the owner,
/// the server and agents in the log.
const RESERVED_NAMES: [&str; 4] = ["user", "나", "system", "agent"];

/// True for a character a name may not contain: Unicode categories Cc (controls), Cf (format:
/// zero-width and bidi controls), Zl and Zp. No general-category crate is a dependency, so the Cf
/// ranges are listed here (Unicode 15).
pub fn is_forbidden_name_char(c: char) -> bool {
    c.is_control() || is_format_char(c) || is_line_separator(c)
}

/// U+2028 (Zl) and U+2029 (Zp).
pub fn is_line_separator(c: char) -> bool {
    matches!(c as u32, 0x2028 | 0x2029)
}

/// An invisible Unicode format character (category Cf), from a hand-listed set because no
/// general-category crate is a dependency: soft hyphen, Arabic marks, zero-width space, joiner
/// and non-joiner, bidi controls, U+2060..U+2064, U+FEFF, tag characters and the like.
pub fn is_format_char(c: char) -> bool {
    let u = c as u32;
    matches!(u,
        0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x0890..=0x0891 | 0x08E2 | 0x180E
        | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F
        | 0xFEFF | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD | 0x13430..=0x1343F | 0x1BCA0..=0x1BCA3
        | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F)
}

/// Check a guest display name (already trimmed) against the shape rules and the reserved names.
/// `member_names` are the names of the channel's members; a name equal to one (case-insensitive)
/// is refused so a guest cannot pass as an agent. No Unicode normaliser is a dependency, so the
/// comparison is plain lowercase.
pub fn check_guest_name(name: &str, member_names: &[String]) -> Result<(), ChannelError> {
    let chars = name.chars().count();
    if chars == 0 || chars > MAX_NAME_CHARS || name.chars().any(is_forbidden_name_char) {
        return Err(ChannelError::Coded(
            "invalid_name",
            format!("name must be 1 to {MAX_NAME_CHARS} characters with no control, format or line-separator character"),
        ));
    }
    let lower = name.to_lowercase();
    if RESERVED_NAMES.contains(&lower.as_str()) || member_names.iter().any(|m| m.trim().to_lowercase() == lower) {
        return Err(ChannelError::Coded("invalid_name", "that name is reserved in this channel".into()));
    }
    Ok(())
}

/// A guest as the server knows it. Never carries the credential or its hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Guest {
    pub id: String,
    pub channel_id: String,
    pub invite_id: String,
    pub name: String,
    pub created_at: i64,
    /// Unix milliseconds; the credential follows the invite's expiry.
    pub expires_at: i64,
    pub revoked_at: Option<i64>,
    pub allow_until: Option<i64>,
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

    /// Accept an invite (FR-T14): checks that the invite is unexpired, unused and unrevoked, marks
    /// it used, and makes the guest with a fresh credential whose expiry is the invite's. Returns
    /// the guest and the plaintext credential, which exists nowhere else. `name` is trimmed and
    /// must pass [`check_guest_name`] (else 400 `invalid_name`, and the invite stays usable). Every other failure is the same `NotFound`, so a caller cannot tell an
    /// unknown token from a used, expired or revoked one.
    pub fn accept_invite(&self, token: &str, name: &str) -> Result<(Guest, String), ChannelError> {
        self.accept_invite_at(token, name, now_ms())
    }

    /// As [`Store::accept_invite`] with the clock given.
    pub fn accept_invite_at(&self, token: &str, name: &str, now: i64) -> Result<(Guest, String), ChannelError> {
        let name = name.trim();
        // Shape rules first, so a bad name is 400 whatever the token is (no oracle on the token).
        check_guest_name(name, &[])?;
        // Then the channel's member names, before the invite is burned. A failure here means the
        // token named a live channel; the secret is 244 bits, so that leaks nothing usable.
        let channel_of: Option<String> = self
            .conn()
            .query_row(
                "SELECT channel_id FROM channel_invites WHERE token_hash = ?1",
                params![hash_secret(token)],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(cid) = channel_of {
            let names: Vec<String> =
                self.channel_members(&cid).map(|m| m.into_iter().map(|x| x.name).collect()).unwrap_or_default();
            check_guest_name(name, &names)?;
        }
        let gone = || ChannelError::NotFound("invite".into());
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        // The single-use gate: only one caller can flip used_at from NULL.
        let hash = hash_secret(token);
        let marked = tx.execute(
            "UPDATE channel_invites SET used_at = ?1
             WHERE token_hash = ?2 AND used_at IS NULL AND revoked_at IS NULL AND expires_at > ?1",
            params![now, hash],
        )?;
        if marked != 1 {
            return Err(gone());
        }
        let (invite_id, channel_id, expires_at): (String, String, i64) = tx.query_row(
            "SELECT id, channel_id, expires_at FROM channel_invites WHERE token_hash = ?1",
            params![hash],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let credential = new_secret(CREDENTIAL_PREFIX);
        let guest = Guest {
            id: uuid::Uuid::new_v4().to_string(),
            channel_id,
            invite_id,
            name: name.to_string(),
            created_at: now,
            expires_at,
            revoked_at: None,
            allow_until: None,
        };
        tx.execute(
            "INSERT INTO channel_guests (id, channel_id, invite_id, name, credential_hash, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![guest.id, guest.channel_id, guest.invite_id, guest.name, hash_secret(&credential), now, expires_at],
        )?;
        tx.commit()?;
        Ok((guest, credential))
    }

    /// The guest a plaintext credential belongs to: `None` when it is unknown, expired, revoked, or
    /// its invite is revoked. Every request does this lookup, so revocation is immediate. A
    /// database error is also `None` (fail closed).
    pub fn guest_by_credential(&self, credential: &str) -> Option<Guest> {
        self.guest_by_credential_at(credential, now_ms())
    }

    /// As [`Store::guest_by_credential`] with the clock given.
    pub fn guest_by_credential_at(&self, credential: &str, now: i64) -> Option<Guest> {
        if !credential.starts_with(CREDENTIAL_PREFIX) {
            return None;
        }
        let hash = hash_secret(credential);
        let (guest, stored): (Guest, String) = self
            .conn()
            .query_row(
                "SELECT g.id, g.channel_id, g.invite_id, g.name, g.created_at, g.expires_at, g.revoked_at,
                        g.allow_until, g.credential_hash
                 FROM channel_guests g JOIN channel_invites i ON i.id = g.invite_id
                 WHERE g.credential_hash = ?1 AND g.revoked_at IS NULL AND i.revoked_at IS NULL
                   AND g.expires_at > ?2",
                params![hash, now],
                |r| {
                    Ok((
                        Guest {
                            id: r.get(0)?,
                            channel_id: r.get(1)?,
                            invite_id: r.get(2)?,
                            name: r.get(3)?,
                            created_at: r.get(4)?,
                            expires_at: r.get(5)?,
                            revoked_at: r.get(6)?,
                            allow_until: r.get(7)?,
                        },
                        r.get(8)?,
                    ))
                },
            )
            .optional()
            .ok()
            .flatten()?;
        crate::auth::token_eq(&stored, &hash).then_some(guest)
    }
}
