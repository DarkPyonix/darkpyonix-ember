//! The channel feed (SPEC FR-T11): what a channel's member agents request from or report to the
//! person, derived from the session events and status, with no store of its own.
//!
//! Two kinds of item, newest first:
//! - `approval`: a tool approval a member session asked for and nobody has answered or withdrawn
//!   (`FR-A5`). It leaves the feed when answered, interrupted or ended.
//! - `report`: how a member's last turn ended (`completed`, `failed`, `interrupted`; `FR-T8`),
//!   shown while that session is `finished` or `failed`. A new turn clears it.
//!
//! Not built: questions (no agent event carries one yet), stall reports, and dismissing a report
//! without starting another turn.

use serde::Serialize;

use super::{ChannelError, Member, Store};
use crate::events::{AgentEvent, SessionStatus, TurnOutcome};

/// Characters of the last reply kept in a report's summary.
const SUMMARY_CHARS: usize = 500;

/// The member agent an item is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FeedAgent {
    pub id: String,
    pub name: String,
    pub harness: String,
}

/// One feed entry. Fields not meant for the kind are absent.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FeedItem {
    /// `approval:<approval_id>` or `report:<session id>:<seq>`.
    pub id: String,
    /// `approval` or `report`.
    pub kind: &'static str,
    pub agent: FeedAgent,
    pub session_id: String,
    /// The session's route, for the client to open.
    pub link: String,
    /// Unix seconds.
    pub at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    /// `completed`, `failed` or `interrupted` (reports).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Store {
    /// The feed of a channel (FR-T11): items of its member agents only.
    pub fn channel_feed(&self, channel: &str) -> Result<Vec<FeedItem>, ChannelError> {
        let row = self.channel_row(channel)?;
        let members: Vec<Member> = self.members_of(&row)?.into_iter().filter(|m| m.is_agent).collect();
        let mut items = Vec::new();
        for m in members {
            let Some(rec) = self.session(&m.id)? else { continue };
            let agent = FeedAgent { id: m.id.clone(), name: m.name.clone(), harness: m.harness.clone() };
            let base = |id: String, kind: &'static str, at_ms: i64| FeedItem {
                id,
                kind,
                agent: agent.clone(),
                session_id: m.id.clone(),
                link: format!("/sessions/{}", m.id),
                at: at_ms / 1000,
                approval_id: None,
                tool: None,
                input: None,
                outcome: None,
                summary: None,
                error: None,
            };
            let mut pending: Vec<FeedItem> = Vec::new();
            let mut summary: Option<String> = None;
            let mut error: Option<String> = None;
            let mut ended: Option<(i64, i64, TurnOutcome, Option<String>, Option<String>)> = None;
            for e in self.events_after(&m.id, 0)? {
                match e.event {
                    AgentEvent::ApprovalRequested { approval_id, tool, input } => {
                        let mut it = base(format!("approval:{approval_id}"), "approval", e.at);
                        it.approval_id = Some(approval_id);
                        it.tool = Some(tool);
                        it.input = Some(input);
                        pending.push(it);
                    }
                    AgentEvent::ApprovalResolved { approval_id, .. } => {
                        pending.retain(|p| p.approval_id.as_deref() != Some(approval_id.as_str()))
                    }
                    AgentEvent::AssistantMessage { text } => summary = Some(text),
                    AgentEvent::Error { message } => error = Some(message),
                    AgentEvent::TurnEnded { outcome } => {
                        pending.clear();
                        ended = Some((e.seq, e.at, outcome, summary.take(), error.take()));
                    }
                    _ => {}
                }
            }
            items.extend(pending);
            if matches!(rec.status, SessionStatus::Finished | SessionStatus::Failed) {
                if let Some((seq, at, outcome, summary, error)) = ended {
                    let mut it = base(format!("report:{}:{seq}", m.id), "report", at);
                    it.outcome = Some(match outcome {
                        TurnOutcome::Completed => "completed",
                        TurnOutcome::Interrupted => "interrupted",
                        TurnOutcome::Failed => "failed",
                    });
                    it.summary = summary.map(|s| s.chars().take(SUMMARY_CHARS).collect());
                    it.error = error;
                    items.push(it);
                }
            }
        }
        items.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.id.cmp(&b.id)));
        Ok(items)
    }

    /// The pending approval `approval_id` in the feed of `channel`, if there is one.
    pub fn channel_feed_approval(&self, channel: &str, approval_id: &str) -> Result<FeedItem, ChannelError> {
        self.channel_feed(channel)?
            .into_iter()
            .find(|i| i.kind == "approval" && i.approval_id.as_deref() == Some(approval_id))
            .ok_or_else(|| ChannelError::NotFound(format!("pending approval {approval_id} in channel {channel}")))
    }
}
