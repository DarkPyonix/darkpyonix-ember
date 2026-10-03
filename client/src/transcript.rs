//! One session's conversation, reduced from its stored events (FR-L5).
//!
//! Pure: no I/O. Events are applied strictly in sequence order. An event at or below the last
//! applied sequence is a duplicate and ignored; one beyond the next expected sequence is held
//! back and reported as a gap, so the sync layer can fetch what is missing. Applying the same
//! event list in any chunking and with any duplication therefore yields the same transcript.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::wire::{AgentEvent, ApprovalDecision, StoredEvent, TurnOutcome};

/// A rendered entry of the conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptItem {
    User { seq: i64, text: String },
    /// Assistant text. While `streaming`, `text` is assembled from deltas; the final message
    /// replaces it.
    Assistant { seq: i64, text: String, streaming: bool },
    /// A tool-call card; `result` fills in when the tool finishes.
    ToolCall { seq: i64, call_id: String, name: String, input: serde_json::Value, result: Option<ToolOutput> },
    Approval { seq: i64, approval_id: String, tool: String, input: serde_json::Value, state: ApprovalState },
    TurnEnded { seq: i64, outcome: TurnOutcome },
    Error { seq: i64, message: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolOutput {
    pub output: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "decision", rename_all = "snake_case")]
pub enum ApprovalState {
    Pending,
    Resolved(ApprovalDecision),
    /// The turn ended without an answer (e.g. interrupted); it can no longer be answered.
    Abandoned,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Outcome of offering one event to a transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// Applied (possibly along with held-back events it unblocked).
    Changed,
    /// Already applied; ignored.
    Duplicate,
    /// Arrived ahead of a missing one; held until the gap is filled.
    Gap { expected: i64 },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    pub items: Vec<TranscriptItem>,
    /// Sum of all usage reports.
    pub usage: Usage,
    /// Highest sequence number applied.
    pub last_seq: i64,
    #[serde(skip)]
    held: BTreeMap<i64, AgentEvent>,
    #[serde(skip)]
    tool_index: HashMap<String, usize>,
    #[serde(skip)]
    approval_index: HashMap<String, usize>,
    /// Index of the assistant item currently streaming, if any.
    #[serde(skip)]
    streaming: Option<usize>,
}

impl Transcript {
    pub fn new() -> Transcript {
        Transcript::default()
    }

    /// Approvals still waiting for an answer, oldest first.
    pub fn pending_approvals(&self) -> impl Iterator<Item = &TranscriptItem> {
        self.items
            .iter()
            .filter(|i| matches!(i, TranscriptItem::Approval { state: ApprovalState::Pending, .. }))
    }

    /// Whether events past `last_seq` are being held for a missing one.
    pub fn has_gap(&self) -> bool {
        !self.held.is_empty()
    }

    pub fn apply(&mut self, ev: &StoredEvent) -> Applied {
        self.apply_seq(ev.seq, &ev.event)
    }

    pub fn apply_seq(&mut self, seq: i64, event: &AgentEvent) -> Applied {
        if seq <= self.last_seq || self.held.contains_key(&seq) {
            return Applied::Duplicate;
        }
        if seq != self.last_seq + 1 {
            self.held.insert(seq, event.clone());
            return Applied::Gap { expected: self.last_seq + 1 };
        }
        self.reduce(seq, event);
        while let Some(next) = self.held.remove(&(self.last_seq + 1)) {
            self.reduce(self.last_seq + 1, &next);
        }
        Applied::Changed
    }

    fn reduce(&mut self, seq: i64, event: &AgentEvent) {
        self.last_seq = seq;
        match event {
            AgentEvent::UserMessage { text } => {
                self.end_stream();
                self.items.push(TranscriptItem::User { seq, text: text.clone() });
            }
            AgentEvent::AssistantDelta { text } => match self.streaming {
                Some(i) => {
                    if let TranscriptItem::Assistant { text: t, .. } = &mut self.items[i] {
                        t.push_str(text);
                    }
                }
                None => {
                    self.streaming = Some(self.items.len());
                    self.items.push(TranscriptItem::Assistant { seq, text: text.clone(), streaming: true });
                }
            },
            AgentEvent::AssistantMessage { text } => match self.streaming.take() {
                Some(i) => {
                    // The final message replaces the assembled deltas, in place.
                    self.items[i] = TranscriptItem::Assistant { seq, text: text.clone(), streaming: false };
                }
                None => self.items.push(TranscriptItem::Assistant { seq, text: text.clone(), streaming: false }),
            },
            AgentEvent::ToolCall { call_id, name, input } => {
                self.end_stream();
                self.tool_index.insert(call_id.clone(), self.items.len());
                self.items.push(TranscriptItem::ToolCall {
                    seq,
                    call_id: call_id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                    result: None,
                });
            }
            AgentEvent::ToolResult { call_id, output, is_error } => {
                let out = ToolOutput { output: output.clone(), is_error: *is_error };
                match self.tool_index.get(call_id).and_then(|&i| self.items.get_mut(i)) {
                    Some(TranscriptItem::ToolCall { result, .. }) => *result = Some(out),
                    _ => {
                        // A result for a call we never saw: still show it, as its own card.
                        self.tool_index.insert(call_id.clone(), self.items.len());
                        self.items.push(TranscriptItem::ToolCall {
                            seq,
                            call_id: call_id.clone(),
                            name: String::new(),
                            input: serde_json::Value::Null,
                            result: Some(out),
                        });
                    }
                }
            }
            AgentEvent::ApprovalRequested { approval_id, tool, input } => {
                self.end_stream();
                self.approval_index.insert(approval_id.clone(), self.items.len());
                self.items.push(TranscriptItem::Approval {
                    seq,
                    approval_id: approval_id.clone(),
                    tool: tool.clone(),
                    input: input.clone(),
                    state: ApprovalState::Pending,
                });
            }
            AgentEvent::ApprovalResolved { approval_id, decision } => {
                if let Some(TranscriptItem::Approval { state, .. }) =
                    self.approval_index.get(approval_id).and_then(|&i| self.items.get_mut(i))
                {
                    *state = ApprovalState::Resolved(*decision);
                }
            }
            AgentEvent::Usage { input_tokens, output_tokens } => {
                self.usage.input_tokens += input_tokens;
                self.usage.output_tokens += output_tokens;
            }
            AgentEvent::TurnEnded { outcome } => {
                self.end_stream();
                for item in &mut self.items {
                    if let TranscriptItem::Approval { state: s @ ApprovalState::Pending, .. } = item {
                        *s = ApprovalState::Abandoned;
                    }
                }
                self.items.push(TranscriptItem::TurnEnded { seq, outcome: *outcome });
            }
            AgentEvent::Error { message } => {
                self.items.push(TranscriptItem::Error { seq, message: message.clone() });
            }
            AgentEvent::NativeSession { .. } | AgentEvent::Unknown => {}
        }
    }

    /// Something other than a delta arrived: a streaming message without a final one stays as
    /// assembled.
    fn end_stream(&mut self) {
        if let Some(i) = self.streaming.take() {
            if let TranscriptItem::Assistant { streaming, .. } = &mut self.items[i] {
                *streaming = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(seq: i64, event: AgentEvent) -> StoredEvent {
        StoredEvent { session_id: "s".into(), seq, at: 0, event }
    }

    fn delta(t: &str) -> AgentEvent {
        AgentEvent::AssistantDelta { text: t.into() }
    }

    #[test]
    fn deltas_assemble_and_final_message_replaces_them() {
        let mut t = Transcript::new();
        t.apply(&ev(1, AgentEvent::UserMessage { text: "hi".into() }));
        t.apply(&ev(2, delta("Hel")));
        t.apply(&ev(3, delta("lo")));
        assert_eq!(
            t.items[1],
            TranscriptItem::Assistant { seq: 2, text: "Hello".into(), streaming: true }
        );
        t.apply(&ev(4, AgentEvent::AssistantMessage { text: "Hello!".into() }));
        assert_eq!(t.items.len(), 2);
        assert_eq!(
            t.items[1],
            TranscriptItem::Assistant { seq: 4, text: "Hello!".into(), streaming: false }
        );
        // A new stream after a final message starts a new item.
        t.apply(&ev(5, delta("More")));
        assert_eq!(t.items.len(), 3);
    }

    #[test]
    fn tool_call_card_gets_its_result() {
        let mut t = Transcript::new();
        t.apply(&ev(1, AgentEvent::ToolCall { call_id: "c".into(), name: "Read".into(), input: json!({"p": 1}) }));
        t.apply(&ev(2, AgentEvent::ToolResult { call_id: "c".into(), output: "ok".into(), is_error: false }));
        assert_eq!(t.items.len(), 1);
        let TranscriptItem::ToolCall { result, .. } = &t.items[0] else { panic!() };
        assert_eq!(result, &Some(ToolOutput { output: "ok".into(), is_error: false }));
    }

    #[test]
    fn approval_lifecycle() {
        let mut t = Transcript::new();
        let req = |id: &str| AgentEvent::ApprovalRequested { approval_id: id.into(), tool: "Bash".into(), input: json!({}) };
        t.apply(&ev(1, req("a1")));
        assert_eq!(t.pending_approvals().count(), 1);
        t.apply(&ev(2, AgentEvent::ApprovalResolved { approval_id: "a1".into(), decision: ApprovalDecision::Deny }));
        assert_eq!(t.pending_approvals().count(), 0);
        let TranscriptItem::Approval { state, .. } = &t.items[0] else { panic!() };
        assert_eq!(*state, ApprovalState::Resolved(ApprovalDecision::Deny));
        // An unanswered approval is abandoned when the turn ends.
        t.apply(&ev(3, req("a2")));
        t.apply(&ev(4, AgentEvent::TurnEnded { outcome: TurnOutcome::Interrupted }));
        assert_eq!(t.pending_approvals().count(), 0);
        let TranscriptItem::Approval { state, .. } = &t.items[1] else { panic!() };
        assert_eq!(*state, ApprovalState::Abandoned);
    }

    #[test]
    fn usage_accumulates() {
        let mut t = Transcript::new();
        t.apply(&ev(1, AgentEvent::Usage { input_tokens: 3, output_tokens: 1 }));
        t.apply(&ev(2, AgentEvent::Usage { input_tokens: 2, output_tokens: 4 }));
        assert_eq!(t.usage, Usage { input_tokens: 5, output_tokens: 5 });
    }

    #[test]
    fn duplicates_ignored_and_gaps_held_until_filled() {
        let all: Vec<_> = vec![
            ev(1, AgentEvent::UserMessage { text: "a".into() }),
            ev(2, delta("x")),
            ev(3, delta("y")),
            ev(4, AgentEvent::AssistantMessage { text: "xy".into() }),
            ev(5, AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }),
        ];
        let mut reference = Transcript::new();
        for e in &all {
            reference.apply(e);
        }

        let mut t = Transcript::new();
        assert_eq!(t.apply(&all[0]), Applied::Changed);
        assert_eq!(t.apply(&all[3]), Applied::Gap { expected: 2 });
        assert_eq!(t.apply(&all[4]), Applied::Gap { expected: 2 });
        assert!(t.has_gap());
        assert_eq!(t.apply(&all[0]), Applied::Duplicate);
        // The refetch fills the gap and replays overlapping events.
        for e in &all {
            t.apply(e);
        }
        assert!(!t.has_gap());
        assert_eq!(t, reference);
    }
}
