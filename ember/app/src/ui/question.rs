//! The question card's pure view-model (FR-T11, FR-A5): the draft, its gating, the reply, the
//! answered lines and the needs-you count. No UI dependency.
//!
//! A secret question is never typed into the app: compose-rust has no masked field, so the card
//! says the question is answered on the computer's own terminal and offers only a skip. The
//! draft therefore never holds a secret, and its `Debug` shows counts only all the same.

use std::collections::HashMap;

use ember_client::api::QuestionReply;
use ember_client::transcript::{Transcript, TranscriptItem};
use ember_client::wire::{Question, QuestionAnswer};

/// The limits the server enforces (`ANSWER_VALUES_MAX`, `ANSWER_VALUE_CHARS_MAX` in
/// `ember/server/src/events.rs`).
pub const VALUES_MAX: usize = 8;
pub const VALUE_CHARS_MAX: usize = 1000;

pub const SECRET_ON_TERMINAL: &str =
    "비밀 입력은 이 앱에서 받지 않습니다. 컴퓨터의 터미널에서 답해야 합니다";
pub const SECRET_ONLY_SKIP: &str = "여기서는 건너뛸 수만 있습니다";
pub const SAMPLE_NOTE: &str = "예시 질문입니다. 어디에도 보내지 않습니다";
pub const OTHER_PLACEHOLDER: &str = "직접 입력";
pub const SENT_WAITING: &str = "답을 보냈습니다. 에이전트의 응답을 기다리고 있습니다";
pub const SKIPPED_LINE: &str = "건너뜀";
pub const ABANDONED_LINE: &str = "답하지 않고 끝났습니다";
pub const SECRET_STARS: &str = "***";

/// What the person has picked or typed so far, per question id.
#[derive(Clone, Default, PartialEq)]
pub struct QuestionDraft {
    picks: HashMap<String, Vec<String>>,
    other: HashMap<String, String>,
}

impl std::fmt::Debug for QuestionDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "QuestionDraft({} picks, {} texts, values hidden)", self.picks.len(), self.other.len())
    }
}

impl QuestionDraft {
    pub fn picked(&self, qid: &str, label: &str) -> bool {
        self.picks.get(qid).is_some_and(|v| v.iter().any(|l| l == label))
    }

    pub fn other_text(&self, qid: &str) -> &str {
        self.other.get(qid).map(String::as_str).unwrap_or("")
    }

    /// Forget everything (on send and on leaving the card).
    pub fn clear(&mut self) {
        self.picks.clear();
        self.other.clear();
    }
}

/// Pick `label` of `q`. A single-select question takes exactly one value, so the pick replaces
/// the earlier one and the typed text; a multi-select question toggles it. A label the question
/// does not offer is ignored.
pub fn toggle(mut d: QuestionDraft, q: &Question, label: &str) -> QuestionDraft {
    if q.secret || !q.options.iter().any(|o| o.label == label) {
        return d;
    }
    if q.multi_select {
        let v = d.picks.entry(q.id.clone()).or_default();
        match v.iter().position(|l| l == label) {
            Some(i) => {
                v.remove(i);
            }
            None => v.push(label.to_string()),
        }
        if v.is_empty() {
            d.picks.remove(&q.id);
        }
    } else {
        d.picks.insert(q.id.clone(), vec![label.to_string()]);
        d.other.remove(&q.id);
    }
    d
}

/// Set the free text of `q`. Ignored unless the question allows it. In a single-select question
/// non-blank text replaces the picked label; in a multi-select one it is one more value.
pub fn set_other(mut d: QuestionDraft, q: &Question, text: &str) -> QuestionDraft {
    if q.secret || !q.allow_other {
        return d;
    }
    if text.trim().is_empty() {
        d.other.remove(&q.id);
        return d;
    }
    d.other.insert(q.id.clone(), text.to_string());
    if !q.multi_select {
        d.picks.remove(&q.id);
    }
    d
}

/// The values the draft holds for `q`: picked labels in the order the question offers them,
/// then the trimmed free text.
pub fn values_of(q: &Question, d: &QuestionDraft) -> Vec<String> {
    if q.secret {
        return Vec::new();
    }
    let mut out: Vec<String> = q
        .options
        .iter()
        .filter(|o| d.picked(&q.id, &o.label))
        .map(|o| o.label.clone())
        .collect();
    let text = d.other_text(&q.id).trim();
    if q.allow_other && !text.is_empty() {
        out.push(text.to_string());
    }
    out
}

fn fits(q: &Question, values: &[String]) -> bool {
    !values.is_empty()
        && values.len() <= VALUES_MAX
        && (q.multi_select || values.len() == 1)
        && values.iter().all(|v| (1..=VALUE_CHARS_MAX).contains(&v.trim().chars().count()))
}

/// Whether every question has an answer the server will take. False when any question is
/// secret: those cannot be answered here.
pub fn can_submit(questions: &[Question], d: &QuestionDraft) -> bool {
    !questions.is_empty() && questions.iter().all(|q| !q.secret && fits(q, &values_of(q, d)))
}

/// The reply for the draft, one answer per question in question order, or `None` while
/// `can_submit` is false.
pub fn reply(questions: &[Question], d: &QuestionDraft) -> Option<QuestionReply> {
    if !can_submit(questions, d) {
        return None;
    }
    Some(QuestionReply::Answers(
        questions
            .iter()
            .map(|q| QuestionAnswer { question_id: q.id.clone(), values: values_of(q, d) })
            .collect(),
    ))
}

/// Whether any question asks for a secret.
pub fn has_secret(questions: &[Question]) -> bool {
    questions.iter().any(|q| q.secret)
}

/// One `•` per character, for showing that something was typed without showing it.
pub fn mask(text: &str) -> String {
    text.chars().map(|_| '\u{2022}').collect()
}

/// One line per answered question: `header or text: labels`. A secret question shows `***`
/// whatever the stored values are.
pub fn answered_lines(questions: &[Question], answers: &[QuestionAnswer]) -> Vec<String> {
    questions
        .iter()
        .map(|q| {
            let title = q.header.as_deref().filter(|h| !h.trim().is_empty()).unwrap_or(&q.text);
            let shown = if q.secret {
                SECRET_STARS.to_string()
            } else {
                answers
                    .iter()
                    .find(|a| a.question_id == q.id)
                    .map(|a| a.values.join(", "))
                    .unwrap_or_default()
            };
            format!("{title}: {shown}")
        })
        .collect()
}

/// What needs a person in a transcript: approvals and questions.
pub fn needs_you_count(t: &Transcript) -> (usize, usize) {
    (t.pending_approvals().count(), t.pending_questions().count())
}

/// The badge text for those counts, or `None` when nothing waits.
pub fn needs_you_label((approvals, questions): (usize, usize)) -> Option<String> {
    match (approvals, questions) {
        (0, 0) => None,
        (a, 0) => Some(format!("승인 대기 {a}")),
        (0, q) => Some(format!("답 필요 {q}")),
        (a, q) => Some(format!("확인 필요 {}", a + q)),
    }
}

/// Estimated height (dp) of a question card, for the tail anchoring of the transcript.
pub fn estimate_height(questions: &[Question], pending: bool) -> f32 {
    if pending {
        let rows: usize = questions
            .iter()
            .map(|q| q.options.len() + usize::from(q.allow_other && !q.secret) + 1)
            .sum();
        120.0 + 44.0 * rows as f32
    } else {
        28.0 * questions.len() as f32 + 40.0
    }
}

/// The id of the capture sample; answering it calls nothing.
pub const DEBUG_SAMPLE_ID: &str = "demo-question-debug";

/// `EMBER_CR_QUESTION=single|multi|secret|answered`: a sample question item for review
/// captures, in debug builds only. Marked 예시 and never sent anywhere.
pub fn debug_sample(hook: Option<&str>, debug_build: bool, seq: i64) -> Option<TranscriptItem> {
    use ember_client::transcript::QuestionState;
    use ember_client::wire::QuestionOption;
    if !debug_build {
        return None;
    }
    let kind = hook?;
    let opt = |l: &str, d: &str| QuestionOption { label: l.into(), description: Some(d.into()) };
    let mut q = Question {
        id: "q0".into(),
        header: Some("예시".into()),
        text: "(예시) 어떤 방식으로 진행할까요?".into(),
        options: vec![opt("빠르게", "검사를 줄이고 먼저 끝냅니다."), opt("꼼꼼하게", "모든 검사를 실행합니다.")],
        multi_select: false,
        secret: false,
        allow_other: true,
    };
    let mut state = QuestionState::Pending;
    match kind {
        "single" => {}
        "multi" => q.multi_select = true,
        "secret" => {
            q.text = "(예시) 배포 비밀번호를 입력해 주세요.".into();
            q.options.clear();
            q.secret = true;
        }
        "answered" => {
            state = QuestionState::Answered(vec![QuestionAnswer { question_id: "q0".into(), values: vec!["꼼꼼하게".into()] }]);
        }
        _ => return None,
    }
    Some(TranscriptItem::Question { seq, question_id: DEBUG_SAMPLE_ID.into(), questions: vec![q], state })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_client::wire::{AgentEvent, QuestionOption, StoredEvent, TurnOutcome};

    fn q(id: &str, multi: bool, secret: bool, other: bool) -> Question {
        Question {
            id: id.into(),
            header: Some("색".into()),
            text: "Colour?".into(),
            options: ["red", "blue", "green"]
                .iter()
                .map(|l| QuestionOption { label: (*l).into(), description: None })
                .collect(),
            multi_select: multi,
            secret,
            allow_other: other,
        }
    }

    #[test]
    fn fr_t11_question_single_select_replaces_choice() {
        let q0 = q("q0", false, false, false);
        let d = toggle(toggle(QuestionDraft::default(), &q0, "red"), &q0, "blue");
        assert_eq!(values_of(&q0, &d), vec!["blue"]);
        assert!(!d.picked("q0", "red"));
        // A label the question does not offer is ignored.
        assert_eq!(values_of(&q0, &toggle(d, &q0, "pink")), vec!["blue"]);
    }

    #[test]
    fn fr_t11_question_multi_select_toggles_labels() {
        let q0 = q("q0", true, false, false);
        let d = toggle(toggle(QuestionDraft::default(), &q0, "green"), &q0, "red");
        assert_eq!(values_of(&q0, &d), vec!["red", "green"]);
        let d = toggle(d, &q0, "red");
        assert_eq!(values_of(&q0, &d), vec!["green"]);
        let d = toggle(d, &q0, "green");
        assert!(!can_submit(&[q0], &d));
    }

    #[test]
    fn fr_t11_question_other_text_only_when_allowed() {
        let no = q("q0", false, false, false);
        assert!(values_of(&no, &set_other(QuestionDraft::default(), &no, "x")).is_empty());
        let yes = q("q0", true, false, true);
        let d = set_other(toggle(QuestionDraft::default(), &yes, "red"), &yes, "  teal  ");
        assert_eq!(values_of(&yes, &d), vec!["red", "teal"]);
    }

    #[test]
    fn fr_t11_question_other_text_is_trimmed_and_replaces_single_choice() {
        let s = q("q0", false, false, true);
        let d = set_other(toggle(QuestionDraft::default(), &s, "red"), &s, "  teal ");
        assert_eq!(values_of(&s, &d), vec!["teal"]);
        // Picking a label again drops the text; blank text is no value.
        let d = toggle(d, &s, "blue");
        assert_eq!(values_of(&s, &d), vec!["blue"]);
        assert_eq!(values_of(&s, &set_other(d, &s, "   ")), vec!["blue"]);
    }

    #[test]
    fn fr_t11_question_submit_needs_every_question_answered() {
        let qs = vec![q("q0", false, false, false), q("q1", true, false, true)];
        let d = toggle(QuestionDraft::default(), &qs[0], "red");
        assert!(!can_submit(&qs, &d));
        assert!(reply(&qs, &d).is_none());
        let d = set_other(d, &qs[1], "teal");
        assert!(can_submit(&qs, &d));
        assert!(!can_submit(&[], &d));
    }

    #[test]
    fn fr_t11_question_reply_matches_server_limits() {
        let qs = vec![q("q0", false, false, false), q("q1", true, false, true)];
        let d = toggle(toggle(QuestionDraft::default(), &qs[0], "red"), &qs[1], "blue");
        let Some(QuestionReply::Answers(a)) = reply(&qs, &d) else { panic!() };
        assert_eq!(a[0], QuestionAnswer { question_id: "q0".into(), values: vec!["red".into()] });
        assert_eq!(a[1].values, vec!["blue"]);
        // 1000 characters are fine, 1001 are not, counted after trimming.
        let long = |n: usize| format!(" {} ", "가".repeat(n));
        let ok = set_other(QuestionDraft::default(), &qs[1], &long(VALUE_CHARS_MAX));
        assert!(can_submit(&qs[1..], &ok));
        let bad = set_other(QuestionDraft::default(), &qs[1], &long(VALUE_CHARS_MAX + 1));
        assert!(!can_submit(&qs[1..], &bad));
        // At most 8 values: 3 labels plus text is 4, so the cap is not reachable here, but a
        // question with 9 options picked all is refused.
        let mut many = q("m", true, false, false);
        many.options = (0..9).map(|i| QuestionOption { label: format!("o{i}"), description: None }).collect();
        let d = (0..9).fold(QuestionDraft::default(), |d, i| toggle(d, &many, &format!("o{i}")));
        assert!(!can_submit(&[many], &d));
    }

    #[test]
    fn fr_t11_question_answered_shows_labels() {
        let qs = vec![q("q0", true, false, false)];
        let a = vec![QuestionAnswer { question_id: "q0".into(), values: vec!["red".into(), "blue".into()] }];
        assert_eq!(answered_lines(&qs, &a), vec!["색: red, blue"]);
    }

    #[test]
    fn fr_t11_question_answered_secret_shows_stars() {
        let qs = vec![q("q0", false, true, false)];
        // Even if a server sent the clear value, the card shows stars.
        let a = vec![QuestionAnswer { question_id: "q0".into(), values: vec!["hunter2".into()] }];
        let lines = answered_lines(&qs, &a);
        assert_eq!(lines, vec!["색: ***"]);
        assert!(!lines[0].contains("hunter2"));
    }

    #[test]
    fn fr_t11_question_secret_is_masked_and_debug_never_prints_it() {
        assert_eq!(mask("abc가"), "\u{2022}\u{2022}\u{2022}\u{2022}");
        let s = q("q0", false, true, true);
        // A secret question takes no pick and no text, and cannot be submitted from here.
        let d = set_other(toggle(QuestionDraft::default(), &s, "red"), &s, "hunter2");
        assert!(values_of(&s, &d).is_empty() && !can_submit(&[s.clone()], &d));
        assert!(has_secret(&[s]));
        // The draft of an ordinary question never prints a value either.
        let o = q("q1", false, false, true);
        let d = set_other(QuestionDraft::default(), &o, "hunter2");
        assert!(!format!("{d:?}").contains("hunter2"));
    }

    fn ev(seq: i64, event: AgentEvent) -> StoredEvent {
        StoredEvent { session_id: "s".into(), seq, at: 0, event }
    }

    #[test]
    fn fr_t11_question_counts_in_needs_you_badge() {
        let mut t = Transcript::new();
        assert_eq!(needs_you_label(needs_you_count(&t)), None);
        t.apply(&ev(1, AgentEvent::QuestionAsked { question_id: "r".into(), questions: vec![q("q0", false, false, false)] }));
        assert_eq!(needs_you_label(needs_you_count(&t)).as_deref(), Some("답 필요 1"));
        t.apply(&ev(2, AgentEvent::ApprovalRequested { approval_id: "a".into(), tool: "Bash".into(), input: serde_json::json!({}) }));
        assert_eq!(needs_you_label(needs_you_count(&t)).as_deref(), Some("확인 필요 2"));
        t.apply(&ev(3, AgentEvent::TurnEnded { outcome: TurnOutcome::Interrupted }));
        assert_eq!(needs_you_label(needs_you_count(&t)), None);
        assert_eq!(needs_you_label((2, 0)).as_deref(), Some("승인 대기 2"));
    }

    #[test]
    fn fr_t11_question_debug_hook_is_debug_only_and_known_values_only() {
        assert!(debug_sample(Some("single"), false, 1).is_none());
        assert!(debug_sample(Some("nope"), true, 1).is_none());
        assert!(debug_sample(None, true, 1).is_none());
        for k in ["single", "multi", "secret", "answered"] {
            assert!(matches!(debug_sample(Some(k), true, 9), Some(TranscriptItem::Question { seq: 9, .. })));
        }
    }

    #[test]
    fn fr_t11_question_estimate_grows_with_options_only_while_pending() {
        let qs = vec![q("q0", false, false, true)];
        assert!(estimate_height(&qs, true) > estimate_height(&qs, false));
        assert_eq!(estimate_height(&qs, false), 68.0);
    }
}
