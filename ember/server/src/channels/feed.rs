//! The channel feed (SPEC FR-T11): what a channel's member agents request from or report to the
//! person, derived from the session events and status, with no store of its own.
//!
//! Two kinds of item, newest first:
//! - `approval`: a tool approval a member session asked for and nobody has answered or withdrawn
//!   (`FR-A5`). It leaves the feed when answered, interrupted or ended.
//! - `report`: how a member's last turn ended (`completed`, `failed`, `interrupted`; `FR-T8`),
//!   shown while that session is `finished` or `failed`. A new turn clears it.
//!
//! An approval for `AskUserQuestion` also carries `questions` (the question text and option
//! labels), because the person must answer it and no automatic mode may.
//!
//! Not built: a question event of its own (no agent event carries one yet), stall reports, and dismissing a report
//! without starting another turn.

use serde::Serialize;

use super::{ChannelError, Member, Store};
use crate::events::{is_user_question_tool, AgentEvent, SessionStatus, TurnOutcome};

/// Characters of the last reply kept in a report's summary.
const SUMMARY_CHARS: usize = 500;

/// Most questions, options per question and characters per string a feed item carries.
const QUESTIONS_MAX: usize = 4;
const OPTIONS_MAX: usize = 8;
const QUESTION_CHARS: usize = 200;

/// One question an agent asks the person, read from an `AskUserQuestion` approval (FR-T11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FeedQuestion {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    pub text: String,
    /// Option labels only.
    pub options: Vec<String>,
    pub multi_select: bool,
}

/// At most [`QUESTION_CHARS`] characters of `s`, ending in an ellipsis when cut. Counts chars, so
/// a cut never splits a UTF-8 sequence.
fn clip(s: &str) -> String {
    if s.chars().count() <= QUESTION_CHARS {
        return s.to_string();
    }
    let mut out: String = s.chars().take(QUESTION_CHARS - 1).collect();
    out.push('\u{2026}');
    out
}

/// The readable questions of an approval for a question tool, or `None` for any other tool or an
/// input that does not parse (`questions` an array with a string `question` each, at least one).
pub fn feed_questions(tool: &str, input: &serde_json::Value) -> Option<Vec<FeedQuestion>> {
    if !is_user_question_tool(tool) {
        return None;
    }
    let list = input.get("questions")?.as_array()?;
    let mut out = Vec::new();
    for q in list.iter().take(QUESTIONS_MAX) {
        let text = q.get("question")?.as_str()?;
        let header = q.get("header").and_then(|h| h.as_str()).map(clip);
        let options = q
            .get("options")
            .and_then(|o| o.as_array())
            .map(|o| {
                o.iter()
                    .filter_map(|o| o.get("label").and_then(|l| l.as_str()))
                    .take(OPTIONS_MAX)
                    .map(clip)
                    .collect()
            })
            .unwrap_or_default();
        let multi_select = q.get("multiSelect").and_then(|m| m.as_bool()).unwrap_or(false);
        out.push(FeedQuestion { header, text: clip(text), options, multi_select });
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

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
    /// The readable form of an approval for a question tool (`AskUserQuestion`); absent for any
    /// other approval and for an input that does not parse.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub questions: Option<Vec<FeedQuestion>>,
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
                questions: None,
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
                        it.questions = feed_questions(&tool, &input);
                        it.tool = Some(tool);
                        it.input = Some(input);
                        pending.push(it);
                    }
                    AgentEvent::ApprovalResolved { approval_id, .. } => {
                        pending.retain(|p| p.approval_id.as_deref() != Some(approval_id.as_str()))
                    }
                    AgentEvent::AssistantMessage { text } => summary = Some(text),
                    AgentEvent::Error { message } | AgentEvent::TurnError { message, .. } => error = Some(message),
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fr_t11_questions_are_read_from_an_ask_user_question_input() {
        let input = json!({"questions": [
            {"question": "Which db?", "header": "DB", "multiSelect": true,
             "options": [{"label": "pg", "description": "x"}, {"label": "sqlite"}]},
            {"question": "Ok?", "options": []}
        ]});
        let q = feed_questions("AskUserQuestion", &input).unwrap();
        assert_eq!(q.len(), 2);
        assert_eq!(q[0].header.as_deref(), Some("DB"));
        assert_eq!(q[0].text, "Which db?");
        assert_eq!(q[0].options, vec!["pg", "sqlite"]);
        assert!(q[0].multi_select);
        assert_eq!(q[1].header, None);
        assert!(!q[1].multi_select);
        let v = serde_json::to_value(&q[1]).unwrap();
        assert_eq!(v, json!({"text": "Ok?", "options": [], "multi_select": false}));
    }

    #[test]
    fn fr_t11_questions_are_capped_and_truncated_on_char_boundaries() {
        let long = "\u{d55c}".repeat(300);
        let opts: Vec<_> = (0..12).map(|i| json!({"label": format!("o{i}")})).collect();
        let qs: Vec<_> = (0..6).map(|_| json!({"question": long, "header": long, "options": opts})).collect();
        let q = feed_questions("AskUserQuestion", &json!({"questions": qs})).unwrap();
        assert_eq!(q.len(), QUESTIONS_MAX);
        assert_eq!(q[0].options.len(), OPTIONS_MAX);
        assert_eq!(q[0].text.chars().count(), QUESTION_CHARS);
        assert!(q[0].text.ends_with('\u{2026}'));
        assert_eq!(q[0].header.as_ref().unwrap().chars().count(), QUESTION_CHARS);
        let exact = "a".repeat(QUESTION_CHARS);
        let q = feed_questions("AskUserQuestion", &json!({"questions": [{"question": exact}]})).unwrap();
        assert_eq!(q[0].text, exact);
    }

    #[test]
    fn fr_t11_malformed_question_input_and_other_tools_have_no_questions() {
        for input in [
            json!({}),
            json!({"questions": "x"}),
            json!({"questions": []}),
            json!({"questions": [{"header": "no text"}]}),
            json!({"questions": [{"question": 5}]}),
            json!(null),
        ] {
            assert_eq!(feed_questions("AskUserQuestion", &input), None, "{input}");
        }
        let ok = json!({"questions": [{"question": "q"}]});
        assert_eq!(feed_questions("Bash", &ok), None);
        assert_eq!(feed_questions("mcp__x__AskUserQuestion", &ok), None);
    }
}
