//! Delivery of a person's channel message to a mentioned agent (SPEC FR-T12, FR-T13).
//!
//! The channel message itself is stored by the channels module; this is the A2A leg. It follows
//! the messaging rules (the server-wide and per-session switches of FR-T6, the team rule of
//! FR-T7) but not loop protection's limits, since a person typed it. The row is stored so it
//! counts toward the window for later agent sends. Only a person's message is delivered this
//! way: a message authored by an agent never reaches this function, so no loop can start here.

use std::sync::Arc;

use std::time::{Duration, Instant};

use super::{store::now_ms, team, A2a, A2aError, Receipt, CHANNEL_MENTION_WINDOW, CHANNEL_POSTS_PER_MINUTE};
use crate::channels::ChannelError;

impl A2a {
    /// Deliver `text`, which `person` typed in channel `channel_id`, to session `to`. A refusal
    /// carries the reason the channel shows (FR-T13).
    pub async fn send_from_person(
        self: &Arc<Self>,
        channel_id: &str,
        person: &str,
        to: &str,
        text: &str,
    ) -> Result<Receipt, A2aError> {
        if text.trim().is_empty() {
            return Err(A2aError::BadRequest("message text is empty".into()));
        }
        if !self.enabled()? {
            return Err(A2aError::Disabled(
                "agent-to-agent messaging is turned off on this server".into(),
            ));
        }
        self.session(to)?;
        if !self.store.session_enabled(to)? {
            return Err(A2aError::Disabled(format!(
                "agent-to-agent messaging is turned off for the target session ({to})"
            )));
        }
        let target = self.sessions.store().team_member(to)?.filter(team::MemberRow::active);
        if let Some(why) = Self::team_scope_denial(None, target.as_ref(), to) {
            return Err(A2aError::Forbidden(why));
        }
        let msg = self.store.insert_channel_message(channel_id, person, to, text)?;
        self.deliver(&msg).await
    }

    /// May `session` post into `channel` now (FR-T12, `ember-a2a channel post`)? The caller is
    /// already authenticated. In order: the session is a member (else `Forbidden`), a person
    /// mentioned it in this channel within [`CHANNEL_MENTION_WINDOW`] (else `Forbidden`), and it
    /// made fewer than [`CHANNEL_POSTS_PER_MINUTE`] posts here in the last minute (else
    /// `RateLimited`). The session's own A2A switch is not consulted: it may answer a mention
    /// even with A2A off. An allowed call counts toward the rate limit.
    pub fn check_channel_post(&self, session: &str, channel: &str) -> Result<(), A2aError> {
        let members = self.sessions.store().channel_members(channel).map_err(|e| match e {
            ChannelError::NotFound(m) => A2aError::NotFound(m),
            ChannelError::Forbidden(m) | ChannelError::BadRequest(m) => A2aError::Forbidden(m),
            other => A2aError::Other(anyhow::anyhow!("{other}")),
        })?;
        if !members.iter().any(|m| m.id == session) {
            return Err(A2aError::Forbidden(format!("{session} is not a member of channel {channel}")));
        }
        let since = now_ms() - CHANNEL_MENTION_WINDOW.as_millis() as i64;
        if !self.store.channel_mention_since(session, channel, since)? {
            return Err(A2aError::Forbidden(format!(
                "no one has mentioned you in channel {channel} in the last {} minutes; \
                 an agent answers a mention and does not post unprompted",
                CHANNEL_MENTION_WINDOW.as_secs() / 60
            )));
        }
        let now = Instant::now();
        let mut posts = self.channel_posts.lock().unwrap();
        let q = posts.entry((session.to_string(), channel.to_string())).or_default();
        while q.front().is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60)) {
            q.pop_front();
        }
        if q.len() as u32 >= CHANNEL_POSTS_PER_MINUTE {
            return Err(A2aError::RateLimited(format!(
                "at most {CHANNEL_POSTS_PER_MINUTE} posts a minute in one channel; wait and post once"
            )));
        }
        q.push_back(now);
        Ok(())
    }
}
