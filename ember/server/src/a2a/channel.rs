//! Delivery of a person's channel message to a mentioned agent (SPEC FR-T12, FR-T13).
//!
//! The channel message itself is stored by the channels module; this is the A2A leg. It follows
//! the messaging rules (the server-wide and per-session switches of FR-T6, the team rule of
//! FR-T7) but not loop protection's limits, since a person typed it. The row is stored so it
//! counts toward the window for later agent sends. Only a person's message is delivered this
//! way: a message authored by an agent never reaches this function, so no loop can start here.

use std::sync::Arc;

use super::{team, A2a, A2aError, Receipt};

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
}
