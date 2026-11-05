//! Allowances for guests (SPEC FR-T15, FR-T17) [provisional]: the owner lets one guest's
//! mentions reach the channel's agents for the next N whole hours. The end time is stored in
//! `channel_guests.allow_until` and compared with the clock at delivery time, so an allowance
//! ends by itself and no job has to clear it. It never outlives the guest credential.

use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use super::ChannelError;
use crate::store::{now_ms, Store};

/// Shortest allowance in hours.
pub const MIN_HOURS: i64 = 1;
/// Longest allowance in hours.
pub const MAX_HOURS: i64 = 168;

/// A guest as the owner's list shows it. Never carries the credential or its hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GuestEntry {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub revoked_at: Option<i64>,
    /// Unix ms the allowance ends; `None` when there is none (or it was revoked).
    pub allow_until: Option<i64>,
}

/// A guest in the channel as members see it: who, and until when their mentions are allowed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelGuest {
    pub id: String,
    pub name: String,
    /// Unix ms the allowance ends, or `None` when no allowance is active now.
    pub allow_until: Option<i64>,
}

/// The result of setting an allowance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Allowance {
    pub allow_until: i64,
    /// True when the credential's own expiry cut the allowance short.
    pub capped: bool,
}

/// The whole hours in a request value: an integer from 1 to 168, else 400 `invalid_hours`. A
/// fraction (even `1.0`), a string, a boolean or a missing value are refused.
pub fn check_hours(value: Option<&serde_json::Value>) -> Result<i64, ChannelError> {
    let bad = || {
        ChannelError::Coded(
            "invalid_hours",
            format!("hours must be a whole number from {MIN_HOURS} to {MAX_HOURS}"),
        )
    };
    let n = value.and_then(|v| v.as_i64()).ok_or_else(bad)?;
    if (MIN_HOURS..=MAX_HOURS).contains(&n) {
        Ok(n)
    } else {
        Err(bad())
    }
}

/// SQL condition for a guest whose credential is usable at `?2`: not revoked, its invite not
/// revoked, not expired. Same rule as `guest_by_credential_at`.
const USABLE: &str = "g.revoked_at IS NULL AND g.expires_at > ?2
    AND NOT EXISTS (SELECT 1 FROM channel_invites i WHERE i.id = g.invite_id AND i.revoked_at IS NOT NULL)";

impl Store {
    /// Allow `guest_id`'s mentions for `hours` whole hours from now (1..=168), never beyond the
    /// credential's expiry. 404 for an unknown channel, a guest of another channel, or a guest
    /// whose credential is revoked or expired.
    pub fn set_guest_allowance(&self, channel_id: &str, guest_id: &str, hours: i64) -> Result<Allowance, ChannelError> {
        self.set_guest_allowance_at(channel_id, guest_id, hours, now_ms())
    }

    /// As [`Store::set_guest_allowance`] with the clock given.
    pub fn set_guest_allowance_at(
        &self,
        channel_id: &str,
        guest_id: &str,
        hours: i64,
        now: i64,
    ) -> Result<Allowance, ChannelError> {
        if !(MIN_HOURS..=MAX_HOURS).contains(&hours) {
            return Err(ChannelError::Coded("invalid_hours", format!("hours must be a whole number from {MIN_HOURS} to {MAX_HOURS}")));
        }
        self.channel_row(channel_id)?;
        let conn = self.conn();
        let expires: Option<i64> = conn
            .query_row(
                &format!("SELECT g.expires_at FROM channel_guests g WHERE g.id = ?1 AND g.channel_id = ?3 AND {USABLE}"),
                params![guest_id, now, channel_id],
                |r| r.get(0),
            )
            .optional()?;
        let expires = expires.ok_or_else(|| ChannelError::NotFound(format!("guest {guest_id}")))?;
        let wanted = now + hours * 3_600_000;
        let allow_until = wanted.min(expires);
        conn.execute(
            "UPDATE channel_guests SET allow_until = ?1 WHERE id = ?2 AND channel_id = ?3",
            params![allow_until, guest_id, channel_id],
        )?;
        Ok(Allowance { allow_until, capped: allow_until < wanted })
    }

    /// End a guest's allowance at once. 404 for an unknown channel or a guest of another one.
    pub fn clear_guest_allowance(&self, channel_id: &str, guest_id: &str) -> Result<(), ChannelError> {
        self.channel_row(channel_id)?;
        let n = self.conn().execute(
            "UPDATE channel_guests SET allow_until = NULL WHERE id = ?1 AND channel_id = ?2",
            params![guest_id, channel_id],
        )?;
        if n == 0 {
            return Err(ChannelError::NotFound(format!("guest {guest_id}")));
        }
        Ok(())
    }

    /// Revoke a guest's credential at once (and with it the allowance). Revoking twice keeps the
    /// first time.
    pub fn revoke_guest(&self, channel_id: &str, guest_id: &str) -> Result<(), ChannelError> {
        self.channel_row(channel_id)?;
        let n = self.conn().execute(
            "UPDATE channel_guests SET revoked_at = COALESCE(revoked_at, ?1), allow_until = NULL
             WHERE id = ?2 AND channel_id = ?3",
            params![now_ms(), guest_id, channel_id],
        )?;
        if n == 0 {
            return Err(ChannelError::NotFound(format!("guest {guest_id}")));
        }
        Ok(())
    }

    /// The channel's guests, newest first, with no credential or hash.
    pub fn list_guests(&self, channel_id: &str) -> Result<Vec<GuestEntry>, ChannelError> {
        self.channel_row(channel_id)?;
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, created_at, expires_at, revoked_at, allow_until FROM channel_guests
             WHERE channel_id = ?1 ORDER BY created_at DESC, rowid DESC",
        )?;
        let rows = stmt
            .query_map(params![channel_id], |r| {
                Ok(GuestEntry {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    created_at: r.get(2)?,
                    expires_at: r.get(3)?,
                    revoked_at: r.get(4)?,
                    allow_until: r.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The end of `guest_id`'s allowance if it is in force at `now`: the guest is not revoked,
    /// neither is its invite, the credential is unexpired and `allow_until > now`. Read from the
    /// database each time, so a revocation counts at once; an error is `None` (fail closed).
    pub fn guest_allowance_at(&self, guest_id: &str, now: i64) -> Option<i64> {
        self.conn()
            .query_row(
                &format!("SELECT g.allow_until FROM channel_guests g WHERE g.id = ?1 AND {USABLE}"),
                params![guest_id, now],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()
            .ok()
            .flatten()
            .flatten()
            .filter(|until| *until > now)
    }

    /// The guests present in the channel now (credential usable), with the allowance end if one
    /// is in force.
    pub fn channel_guests(&self, channel_id: &str) -> Result<Vec<ChannelGuest>, ChannelError> {
        self.channel_guests_at(channel_id, now_ms())
    }

    /// As [`Store::channel_guests`] with the clock given.
    pub fn channel_guests_at(&self, channel_id: &str, now: i64) -> Result<Vec<ChannelGuest>, ChannelError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT g.id, g.name, g.allow_until FROM channel_guests g
             WHERE g.channel_id = ?1 AND {USABLE} ORDER BY g.created_at, g.rowid"
        ))?;
        let rows = stmt
            .query_map(params![channel_id, now], |r| {
                let until: Option<i64> = r.get(2)?;
                Ok(ChannelGuest { id: r.get(0)?, name: r.get(1)?, allow_until: until.filter(|u| *u > now) })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}
