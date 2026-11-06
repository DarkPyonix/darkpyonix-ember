//! The normalised event model every agent adapter emits (SPEC FR-A3).
//!
//! Each agent speaks its own protocol (Claude Code stream-json, Codex app-server JSON-RPC, ACP…).
//! Adapters translate that into these events; storage, the push channel, the UI and A2A only ever
//! see this model, so adding an agent never touches them.

use serde::{Deserialize, Serialize};

/// One thing that happened in a session, in the order it happened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    /// The agent's own session identifier became known (used for native resume, FR-S2).
    NativeSession { native_id: String },
    /// A message the user (or an A2A sender) sent into the session.
    UserMessage { text: String },
    /// A note from Ember itself, delivered **to the agent** in front of the next user message
    /// (e.g. "the session's computer changed", FR-S7). Stored so the transcript shows what the
    /// agent was told. Unlike [`AgentEvent::Notice`], it is part of the agent's conversation.
    SystemNotice { text: String },
    /// Incremental assistant text, for live display. Not needed to reconstruct the transcript.
    AssistantDelta { text: String },
    /// A complete assistant message.
    AssistantMessage { text: String },
    /// The agent invoked a tool.
    ToolCall { call_id: String, name: String, input: serde_json::Value },
    /// A tool finished.
    ToolResult { call_id: String, output: String, is_error: bool },
    /// The agent is waiting for a tool approval (FR-A5). `needs_person` is the agent's own mark
    /// that only a person may answer (Claude Code's `requires_user_interaction`); absent in rows
    /// stored before it existed, and then false.
    ApprovalRequested {
        approval_id: String,
        tool: String,
        input: serde_json::Value,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        needs_person: bool,
    },
    /// An approval was answered.
    ApprovalResolved { approval_id: String, decision: ApprovalDecision },
    /// Token usage reported by the agent (FR-U3).
    Usage { input_tokens: u64, output_tokens: u64 },
    /// The agent's account hit a rate or usage limit (FR-U3). `resets_at` is a Unix time in
    /// milliseconds when the agent reported one. Usually followed by an `Error` and a failed turn.
    RateLimited { resets_at: Option<i64>, message: String },
    /// The current turn is over.
    TurnEnded { outcome: TurnOutcome },
    /// The agent process reported or hit an error.
    Error { message: String },
    /// The agent's turn failed for a reason the app renders specially (FR-A10b, FR-A10c): `code`
    /// is `auth_required` (sign-in or API key missing or rejected) or `quota_exceeded` (quota or
    /// rate limit). `message` is the text to show. Always followed by a failed `TurnEnded`.
    TurnError { code: String, message: String },
    /// Something Ember itself shows in this session, e.g. an A2A message refused by loop
    /// protection (FR-T5). Not part of the agent's conversation; leaves the status alone.
    Notice { message: String },
    /// The agent asks the person one or more questions and waits (FR-A5, FR-T11). `question_id`
    /// is the adapter's request id. Only a person answers it; no automatic mode may.
    QuestionAsked { question_id: String, questions: Vec<Question> },
    /// A question was answered, or skipped (`answers` empty). The values of a `secret` question
    /// are stored as `***`, never in clear.
    QuestionAnswered { question_id: String, answers: Vec<QuestionAnswer> },
}

/// One question of a [`AgentEvent::QuestionAsked`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    /// Stable within the request (`q0`, `q1`, ...); the answer names it.
    pub id: String,
    /// A short label (Claude Code's `header`).
    pub header: Option<String>,
    pub text: String,
    pub options: Vec<QuestionOption>,
    pub multi_select: bool,
    /// The answer must not be stored in clear.
    pub secret: bool,
    /// Free text besides the offered options is accepted.
    pub allow_other: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: Option<String>,
}

/// The answer to one question: the chosen labels or the free text, one value per entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionAnswer {
    pub question_id: String,
    pub values: Vec<String>,
}

/// What the automatic modes may do with an approval for one tool (FR-A5). One table, read by the
/// event pump, `set_approve_all` and the adapters' "allow always" memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApprovalPolicy {
    /// Approve-everything mode answers it with an allow.
    pub auto_in_approve_all: bool,
    /// "Allow always" is remembered for the rest of the run.
    pub remember_always: bool,
    /// Recorded as a [`AgentEvent::Notice`] when the mode answers it by itself.
    pub auto_notice: Option<&'static str>,
}

/// Shown when approve-everything leaves an agent's question for a person (FR-A5).
pub const QUESTION_WAITS_NOTICE: &str = "질문에는 자동으로 답하지 않습니다. 직접 답해 주세요.";
/// Recorded when approve-everything allows a plan by itself (FR-A5).
pub const PLAN_AUTO_APPROVED_NOTICE: &str = "계획을 자동 승인했습니다.";

/// The approval policy for `tool` (`needs_person` is the agent's own mark, see
/// [`AgentEvent::ApprovalRequested`]). Exact, case-sensitive tool names: a prefixed MCP tool such
/// as `mcp__x__AskUserQuestion` is an ordinary tool.
pub fn approval_policy(tool: &str, needs_person: bool) -> ApprovalPolicy {
    match tool {
        "AskUserQuestion" => ApprovalPolicy { auto_in_approve_all: false, remember_always: false, auto_notice: None },
        "ExitPlanMode" => ApprovalPolicy {
            auto_in_approve_all: true,
            remember_always: false,
            auto_notice: Some(PLAN_AUTO_APPROVED_NOTICE),
        },
        _ if needs_person => ApprovalPolicy { auto_in_approve_all: false, remember_always: false, auto_notice: None },
        _ => ApprovalPolicy { auto_in_approve_all: true, remember_always: true, auto_notice: None },
    }
}

/// Most values one answer may carry, and the longest value (characters, after trimming).
pub const ANSWER_VALUES_MAX: usize = 8;
pub const ANSWER_VALUE_CHARS_MAX: usize = 1000;

/// Why a set of answers does not fit its questions. The messages are fixed: they are returned to
/// API callers as they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AnswerInvalid {
    #[error("every question needs an answer")]
    MissingAnswer,
    #[error("an answer names a question that was not asked")]
    UnknownQuestion,
    #[error("a question was answered more than once")]
    DuplicateAnswer,
    #[error("an answer needs 1 to 8 values")]
    ValueCount,
    #[error("a value needs 1 to 1000 characters")]
    ValueLength,
    #[error("a question that is not multi-select takes exactly one value")]
    SingleSelect,
}

/// Whether `answers` answer `questions`: every question exactly once, 1 to
/// [`ANSWER_VALUES_MAX`] values each, each value 1 to [`ANSWER_VALUE_CHARS_MAX`] characters after
/// trimming, and exactly one value for a question that is not multi-select.
pub fn validate_question_answers(questions: &[Question], answers: &[QuestionAnswer]) -> Result<(), AnswerInvalid> {
    let mut seen: Vec<&str> = Vec::new();
    for a in answers {
        let q = questions
            .iter()
            .find(|q| q.id == a.question_id)
            .ok_or(AnswerInvalid::UnknownQuestion)?;
        if seen.contains(&q.id.as_str()) {
            return Err(AnswerInvalid::DuplicateAnswer);
        }
        seen.push(&q.id);
        if a.values.is_empty() || a.values.len() > ANSWER_VALUES_MAX {
            return Err(AnswerInvalid::ValueCount);
        }
        if a.values.iter().any(|v| !(1..=ANSWER_VALUE_CHARS_MAX).contains(&v.trim().chars().count())) {
            return Err(AnswerInvalid::ValueLength);
        }
        if !q.multi_select && a.values.len() != 1 {
            return Err(AnswerInvalid::SingleSelect);
        }
    }
    if questions.iter().any(|q| !seen.contains(&q.id.as_str())) {
        return Err(AnswerInvalid::MissingAnswer);
    }
    Ok(())
}

/// `answers` as they are stored: every value of a question marked `secret` in `questions`
/// becomes `***` (one per value, so the count stays). The one place that does this; the live
/// reply to the agent keeps the real values.
pub fn redact_secret_answers(questions: &[Question], answers: &[QuestionAnswer]) -> Vec<QuestionAnswer> {
    answers
        .iter()
        .map(|a| {
            let secret = questions.iter().any(|q| q.id == a.question_id && q.secret);
            QuestionAnswer {
                question_id: a.question_id.clone(),
                values: if secret { a.values.iter().map(|_| "***".to_string()).collect() } else { a.values.clone() },
            }
        })
        .collect()
}

/// Whether `tool` is Claude Code's built-in `AskUserQuestion`: the channel feed reads its
/// questions from the input (FR-T11). The automatic modes use [`approval_policy`], not this.
/// Exact, case-sensitive match.
pub fn is_user_question_tool(tool: &str) -> bool {
    tool == "AskUserQuestion"
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    AllowOnce,
    /// Allow this tool kind for the rest of the session.
    AllowAlways,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    Completed,
    Interrupted,
    Failed,
}

/// Session status shown in the launcher (FR-L2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Idle,
    Running,
    WaitingForApproval,
    Finished,
    Failed,
}

impl SessionStatus {
    /// Status after `event`, given the status before it. Approval outranks running (FR-L2).
    pub fn after(self, event: &AgentEvent) -> SessionStatus {
        match event {
            AgentEvent::UserMessage { .. } => SessionStatus::Running,
            AgentEvent::ApprovalRequested { .. } | AgentEvent::QuestionAsked { .. } => {
                SessionStatus::WaitingForApproval
            }
            AgentEvent::ApprovalResolved { .. } | AgentEvent::QuestionAnswered { .. } => SessionStatus::Running,
            AgentEvent::TurnEnded { outcome: TurnOutcome::Failed } => SessionStatus::Failed,
            AgentEvent::TurnEnded { .. } => SessionStatus::Finished,
            _ => self,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_outranks_running_until_resolved() {
        let s = SessionStatus::Idle.after(&AgentEvent::UserMessage { text: "hi".into() });
        assert_eq!(s, SessionStatus::Running);
        let s = s.after(&AgentEvent::AssistantDelta { text: "x".into() });
        assert_eq!(s, SessionStatus::Running);
        let s = s.after(&AgentEvent::ApprovalRequested {
            approval_id: "a".into(),
            tool: "Bash".into(),
            input: serde_json::json!({}),
            needs_person: false,
        });
        assert_eq!(s, SessionStatus::WaitingForApproval);
        let s = s.after(&AgentEvent::ToolCall {
            call_id: "c".into(),
            name: "Read".into(),
            input: serde_json::json!({}),
        });
        assert_eq!(s, SessionStatus::WaitingForApproval);
        let s = s.after(&AgentEvent::ApprovalResolved {
            approval_id: "a".into(),
            decision: ApprovalDecision::AllowOnce,
        });
        assert_eq!(s, SessionStatus::Running);
        let s = s.after(&AgentEvent::TurnEnded { outcome: TurnOutcome::Completed });
        assert_eq!(s, SessionStatus::Finished);
    }

    #[test]
    fn events_serialise_with_kind_tag() {
        let v = serde_json::to_value(AgentEvent::TurnEnded { outcome: TurnOutcome::Interrupted })
            .unwrap();
        assert_eq!(v, serde_json::json!({"kind": "turn_ended", "outcome": "interrupted"}));
    }

    #[test]
    fn fr_a5_user_question_tool_is_exactly_ask_user_question() {
        assert!(is_user_question_tool("AskUserQuestion"));
        for no in ["askuserquestion", "ASKUSERQUESTION", "askUserQuestion", "AskUserQuestion ", " AskUserQuestion",
            "AskUser", "AskUserQuestions", "mcp__x__AskUserQuestion", "Bash", "ExitPlanMode", ""]
        {
            assert!(!is_user_question_tool(no), "{no:?}");
        }
    }

    #[test]
    fn fr_a5_approval_policy_table() {
        let p = |t, n| approval_policy(t, n);
        let ask = ApprovalPolicy { auto_in_approve_all: false, remember_always: false, auto_notice: None };
        assert_eq!(p("AskUserQuestion", true), ask);
        assert_eq!(p("AskUserQuestion", false), ask);
        assert_eq!(
            p("ExitPlanMode", true),
            ApprovalPolicy {
                auto_in_approve_all: true,
                remember_always: false,
                auto_notice: Some("계획을 자동 승인했습니다.")
            }
        );
        assert_eq!(p("ExitPlanMode", false).auto_notice, Some("계획을 자동 승인했습니다."));
        // An unknown tool carrying the agent's own mark needs a person.
        assert_eq!(p("SomeNewTool", true), ask);
        // Ordinary tools: today's behaviour.
        let plain = ApprovalPolicy { auto_in_approve_all: true, remember_always: true, auto_notice: None };
        assert_eq!(p("Bash", false), plain);
        assert_eq!(p("Write", false), plain);
        // Exact names only.
        assert_eq!(p("askuserquestion", false), plain);
        assert_eq!(p("mcp__x__AskUserQuestion", false), plain);
        assert_eq!(p("exitplanmode", false), plain);
    }

    #[test]
    fn fr_a5_question_notice_texts_are_fixed() {
        assert_eq!(QUESTION_WAITS_NOTICE, "질문에는 자동으로 답하지 않습니다. 직접 답해 주세요.");
        assert_eq!(PLAN_AUTO_APPROVED_NOTICE, "계획을 자동 승인했습니다.");
    }

    #[test]
    fn fr_a5_an_old_approval_row_without_needs_person_still_parses_as_false() {
        let old = r#"{"kind":"approval_requested","approval_id":"a","tool":"Bash","input":{"command":"ls"}}"#;
        let ev: AgentEvent = serde_json::from_str(old).unwrap();
        assert_eq!(
            ev,
            AgentEvent::ApprovalRequested {
                approval_id: "a".into(),
                tool: "Bash".into(),
                input: serde_json::json!({"command": "ls"}),
                needs_person: false,
            }
        );
        // False is not written, so a stored row looks as it always did.
        assert_eq!(serde_json::to_string(&ev).unwrap(), old);
        let flagged = AgentEvent::ApprovalRequested {
            approval_id: "a".into(),
            tool: "ExitPlanMode".into(),
            input: serde_json::json!({}),
            needs_person: true,
        };
        let back: AgentEvent = serde_json::from_str(&serde_json::to_string(&flagged).unwrap()).unwrap();
        assert_eq!(back, flagged);
    }

    #[test]
    fn fr_a5_question_events_round_trip_and_set_the_status() {
        let asked = AgentEvent::QuestionAsked {
            question_id: "r1".into(),
            questions: vec![Question {
                id: "q0".into(),
                header: Some("Colour".into()),
                text: "Which?".into(),
                options: vec![QuestionOption { label: "Red".into(), description: None }],
                multi_select: false,
                secret: false,
                allow_other: true,
            }],
        };
        let back: AgentEvent = serde_json::from_str(&serde_json::to_string(&asked).unwrap()).unwrap();
        assert_eq!(back, asked);
        let s = SessionStatus::Running.after(&asked);
        assert_eq!(s, SessionStatus::WaitingForApproval);
        let answered = AgentEvent::QuestionAnswered { question_id: "r1".into(), answers: Vec::new() };
        assert_eq!(s.after(&answered), SessionStatus::Running);
    }

    #[test]
    fn fr_a5_secret_answers_are_stored_as_stars_and_others_as_given() {
        let q = |id: &str, secret| Question {
            id: id.into(),
            header: None,
            text: "t".into(),
            options: Vec::new(),
            multi_select: true,
            secret,
            allow_other: true,
        };
        let answers = vec![
            QuestionAnswer { question_id: "q0".into(), values: vec!["hunter2".into(), "x".into()] },
            QuestionAnswer { question_id: "q1".into(), values: vec!["blue".into()] },
        ];
        let stored = redact_secret_answers(&[q("q0", true), q("q1", false)], &answers);
        assert_eq!(stored[0].values, vec!["***", "***"]);
        assert_eq!(stored[1].values, vec!["blue"]);
        assert!(!serde_json::to_string(&stored).unwrap().contains("hunter2"));
    }

    fn q(id: &str, multi: bool) -> Question {
        Question {
            id: id.into(),
            header: None,
            text: format!("text {id}"),
            options: Vec::new(),
            multi_select: multi,
            secret: false,
            allow_other: true,
        }
    }

    fn a(id: &str, values: &[&str]) -> QuestionAnswer {
        QuestionAnswer { question_id: id.into(), values: values.iter().map(|v| v.to_string()).collect() }
    }

    #[test]
    fn fr_a5_answers_must_fit_their_questions() {
        let qs = [q("q0", false), q("q1", true)];
        assert_eq!(validate_question_answers(&qs, &[a("q0", &["x"]), a("q1", &["a", "b"])]), Ok(()));
        // Order does not matter.
        assert_eq!(validate_question_answers(&qs, &[a("q1", &["a"]), a("q0", &["x"])]), Ok(()));
        // A missing answer.
        assert_eq!(validate_question_answers(&qs, &[a("q0", &["x"])]), Err(AnswerInvalid::MissingAnswer));
        assert_eq!(validate_question_answers(&qs, &[]), Err(AnswerInvalid::MissingAnswer));
        // An extra id, and a repeated one.
        assert_eq!(
            validate_question_answers(&qs, &[a("q0", &["x"]), a("q1", &["a"]), a("q9", &["x"])]),
            Err(AnswerInvalid::UnknownQuestion)
        );
        assert_eq!(
            validate_question_answers(&qs, &[a("q0", &["x"]), a("q0", &["y"]), a("q1", &["a"])]),
            Err(AnswerInvalid::DuplicateAnswer)
        );
        // 0 and 9 values.
        assert_eq!(validate_question_answers(&qs, &[a("q0", &["x"]), a("q1", &[])]), Err(AnswerInvalid::ValueCount));
        let nine = ["v"; 9];
        assert_eq!(validate_question_answers(&qs, &[a("q0", &["x"]), a("q1", &nine)]), Err(AnswerInvalid::ValueCount));
        let eight = ["v"; 8];
        assert_eq!(validate_question_answers(&qs, &[a("q0", &["x"]), a("q1", &eight)]), Ok(()));
        // Value length, counted after trimming.
        let long = "x".repeat(1001);
        let edge = "x".repeat(1000);
        assert_eq!(validate_question_answers(&qs, &[a("q0", &[long.as_str()]), a("q1", &["a"])]), Err(AnswerInvalid::ValueLength));
        assert_eq!(validate_question_answers(&qs, &[a("q0", &[edge.as_str()]), a("q1", &["a"])]), Ok(()));
        assert_eq!(validate_question_answers(&qs, &[a("q0", &["   "]), a("q1", &["a"])]), Err(AnswerInvalid::ValueLength));
        assert_eq!(validate_question_answers(&qs, &[a("q0", &[""]), a("q1", &["a"])]), Err(AnswerInvalid::ValueLength));
        let padded = format!(" {edge} ");
        assert_eq!(validate_question_answers(&qs, &[a("q0", &[padded.as_str()]), a("q1", &["a"])]), Ok(()));
        // Two values on a single-select question.
        assert_eq!(
            validate_question_answers(&qs, &[a("q0", &["x", "y"]), a("q1", &["a"])]),
            Err(AnswerInvalid::SingleSelect)
        );
    }
}
