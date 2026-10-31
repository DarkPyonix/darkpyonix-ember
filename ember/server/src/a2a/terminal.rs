//! Terminal tools between sessions (SPEC FR-T9, INTENT D21), apart from team messages.
//!
//! Models: herdr `pane read`, `pane send-text`, `pane send-keys`. A session reads another
//! session's recent or live output, sends it text, or interrupts its turn. Deny by default: the
//! caller needs a grant from the user (per reader and target, revocable), or must be the active
//! leader of the target's team (team config). Every use is logged as a notice in the target's
//! transcript.

use std::time::{Duration, Instant};

use serde::Serialize;

use super::{A2a, A2aError};
use crate::events::AgentEvent;

const DEFAULT_LINES: usize = 50;
const MAX_LINES: usize = 1000;
const MAX_WAIT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize)]
pub struct OutputLine {
    pub seq: i64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TerminalRead {
    pub lines: Vec<OutputLine>,
    /// Pass as `since` to follow on from here.
    pub last_seq: i64,
}

/// One line of terminal-style text per event; `None` for events that are not output.
fn render(event: &AgentEvent) -> Option<String> {
    Some(match event {
        AgentEvent::UserMessage { text } => format!("> {text}"),
        AgentEvent::SystemNotice { text } => format!("[system] {text}"),
        AgentEvent::AssistantDelta { text } | AgentEvent::AssistantMessage { text } => text.clone(),
        AgentEvent::ToolCall { name, input, .. } => format!("$ {name} {input}"),
        AgentEvent::ToolResult { output, is_error, .. } => {
            if *is_error { format!("! {output}") } else { output.clone() }
        }
        AgentEvent::ApprovalRequested { tool, .. } => format!("[waiting for approval: {tool}]"),
        AgentEvent::ApprovalResolved { decision, .. } => format!("[approval: {decision:?}]"),
        AgentEvent::TurnEnded { outcome } => format!("[turn ended: {outcome:?}]"),
        AgentEvent::Error { message } => format!("error: {message}"),
        AgentEvent::RateLimited { message, .. } => format!("rate limited: {message}"),
        _ => return None,
    })
}

impl A2a {
    pub fn set_terminal_grant(&self, reader: &str, target: &str, allowed: bool) -> Result<(), A2aError> {
        self.session(reader)?;
        self.session(target)?;
        Ok(self.store.set_terminal_grant(reader, target, allowed)?)
    }

    /// Rejects unless `reader` may use terminal tools on `target`.
    fn check_terminal(&self, reader: &str, target: &str) -> Result<(), A2aError> {
        self.check_caller(reader)?;
        self.session(target)?;
        if reader == target {
            return Err(A2aError::BadRequest("a session cannot use terminal tools on itself".into()));
        }
        if self.store.terminal_granted(reader, target)? {
            return Ok(());
        }
        let members = self.sessions.store().active_team_members()?;
        if let (Some(r), Some(t)) = (members.get(reader), members.get(target)) {
            if r.role == super::team::Role::Leader && t.is_active_teammate() && r.team_id == t.team_id {
                return Ok(());
            }
        }
        Err(A2aError::Forbidden(format!(
            "no terminal permission for session {target}; the user must grant {reader} access to it"
        )))
    }

    fn log_use(&self, reader: &str, target: &str, what: &str) {
        let notice = AgentEvent::Notice {
            message: format!("[terminal] session {reader} {what}"),
        };
        if let Err(e) = self.sessions.record_event(target, &notice) {
            tracing::warn!("could not log terminal use in {target}: {e:#}");
        }
    }

    /// Output lines of `target`: the last `lines` (default 50), or everything after `since`. With
    /// `since` and `wait_ms`, waits up to that long (max 30 s) for new output (follow).
    pub async fn terminal_read(
        &self,
        reader: &str,
        target: &str,
        lines: Option<usize>,
        since: Option<i64>,
        wait_ms: Option<u64>,
    ) -> Result<TerminalRead, A2aError> {
        self.check_terminal(reader, target)?;
        let limit = lines.unwrap_or(DEFAULT_LINES).clamp(1, MAX_LINES);
        let deadline = Instant::now() + Duration::from_millis(wait_ms.unwrap_or(0)).min(MAX_WAIT);
        loop {
            let events = self.sessions.store().events_after(target, since.unwrap_or(0))?;
            let mut out: Vec<OutputLine> = events
                .iter()
                .filter_map(|e| render(&e.event).map(|text| OutputLine { seq: e.seq, text }))
                .collect();
            let last = events.last().map(|e| e.seq).unwrap_or(since.unwrap_or(0));
            if since.is_none() && out.len() > limit {
                out.drain(..out.len() - limit);
            }
            if !out.is_empty() || since.is_none() || Instant::now() >= deadline {
                if since.is_some() && out.len() > limit {
                    out.truncate(limit);
                }
                let last_seq = out.last().map(|l| l.seq).unwrap_or(last);
                self.log_use(reader, target, "read your output");
                return Ok(TerminalRead { lines: out, last_seq });
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Send `text` into `target` as input (herdr `send-text`).
    pub async fn terminal_send_text(&self, reader: &str, target: &str, text: &str) -> Result<(), A2aError> {
        self.check_terminal(reader, target)?;
        if text.is_empty() {
            return Err(A2aError::BadRequest("text is empty".into()));
        }
        self.log_use(reader, target, &format!("sent you text ({} chars)", text.chars().count()));
        self.sessions
            .send(target, text)
            .await
            .map_err(|e| A2aError::Other(anyhow::anyhow!("{e}")))?;
        Ok(())
    }

    /// Interrupt `target`'s running turn (herdr `send-keys` for Ctrl-C).
    pub async fn terminal_interrupt(&self, reader: &str, target: &str) -> Result<(), A2aError> {
        self.check_terminal(reader, target)?;
        self.log_use(reader, target, "interrupted your turn");
        self.sessions
            .interrupt(target)
            .await
            .map_err(|e| A2aError::Other(anyhow::anyhow!("{e}")))?;
        Ok(())
    }
}
