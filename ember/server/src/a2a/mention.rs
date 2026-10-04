//! Mentions from the composer (SPEC FR-T6).
//!
//! A user message may mention other sessions as `@@<id>`, `@@<title>` (one word) or
//! `@@"<title with spaces>"`. The message itself goes to the session it was typed in, unchanged;
//! each mentioned session additionally gets a copy as an A2A message **from that session**, so
//! the mentioned agent sees who mentioned it and can answer the origin with one call (FR-T3).
//!
//! Why a copy and not a redirect: the user typed in this conversation and expects it to carry
//! on here; the mention asks another session to look, it does not move the conversation. Both
//! agents see the same words, and nothing is lost if a mention cannot be resolved.
//!
//! Mentions follow the messaging rules (switches, team scope: [`A2a::targets`]) but not loop
//! protection's limits, since a person sent them; they are still stored, so they count toward
//! the window for later agent sends. The outcome of every mention is a notice in the origin
//! session.

use std::sync::Arc;

use serde::Serialize;

use super::{A2a, A2aError, Receipt, Target};
use crate::events::AgentEvent;

/// Default and maximum number of mention candidates.
pub const CANDIDATES: usize = 20;
pub const CANDIDATES_MAX: usize = 200;

/// Mention tokens in `text`, in order, without duplicates: the part after `@@`, quotes removed.
/// `@@` counts only at the start or after whitespace or an opening bracket, so `a@@b` is not a
/// mention; trailing punctuation is not part of an unquoted token.
pub fn parse_mentions(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i + 1 < chars.len() {
        let starts = chars[i] == '@'
            && chars[i + 1] == '@'
            && (i == 0 || chars[i - 1].is_whitespace() || matches!(chars[i - 1], '(' | '[' | '{'));
        if !starts {
            i += 1;
            continue;
        }
        let mut j = i + 2;
        let token: String = if chars.get(j) == Some(&'"') {
            j += 1;
            let start = j;
            while j < chars.len() && chars[j] != '"' && chars[j] != '\n' {
                j += 1;
            }
            let t: String = chars[start..j].iter().collect();
            if j < chars.len() && chars[j] == '"' {
                j += 1;
            }
            t.trim().to_string()
        } else {
            let start = j;
            while j < chars.len() && !chars[j].is_whitespace() {
                j += 1;
            }
            let t: String = chars[start..j].iter().collect();
            t.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}', '\'', '"'])
                .to_string()
        };
        if !token.is_empty() && !out.contains(&token) {
            out.push(token);
        }
        i = j.max(i + 2);
    }
    out
}

/// Pick the candidate `token` names: exact id, then a unique id prefix (4+ characters), then a
/// unique case-insensitive title.
pub fn resolve<'a>(token: &str, candidates: &'a [Target]) -> Result<&'a Target, String> {
    if let Some(t) = candidates.iter().find(|c| c.id == token) {
        return Ok(t);
    }
    if token.chars().count() >= 4 {
        let by_prefix: Vec<&Target> = candidates.iter().filter(|c| c.id.starts_with(token)).collect();
        if let [t] = by_prefix.as_slice() {
            return Ok(t);
        }
    }
    let by_title: Vec<&Target> =
        candidates.iter().filter(|c| c.title.trim().eq_ignore_ascii_case(token)).collect();
    match by_title.as_slice() {
        [t] => Ok(t),
        [] => Err(format!("no session you can message matches @@{token}")),
        many => Err(format!(
            "@@{token} matches {} sessions ({}); mention one by id",
            many.len(),
            many.iter().map(|t| t.id.as_str()).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// What happened to one mention.
#[derive(Debug, Clone, Serialize)]
pub struct MentionOutcome {
    pub token: String,
    /// The resolved session, when it resolved.
    pub session_id: Option<String>,
    pub receipt: Option<Receipt>,
    pub error: Option<String>,
}

impl A2a {
    /// Sessions the user may mention from `session_id` (FR-T6): the sessions it may message,
    /// filtered by `query` (title substring or id prefix, case-insensitive), most recently
    /// active first.
    pub fn mention_candidates(
        &self,
        session_id: &str,
        query: &str,
        limit: Option<usize>,
    ) -> Result<Vec<Target>, A2aError> {
        self.session(session_id)?;
        let q = query.trim().trim_start_matches('@').trim_matches('"').to_lowercase();
        let limit = limit.unwrap_or(CANDIDATES).clamp(1, CANDIDATES_MAX);
        Ok(self
            .targets(session_id)?
            .into_iter()
            .filter(|t| q.is_empty() || t.id.starts_with(&q) || t.title.to_lowercase().contains(&q))
            .take(limit)
            .collect())
    }

    /// Deliver the mentions in a user message typed in `from` (FR-T6). Each outcome is also
    /// recorded as a notice in `from`. Returns nothing to do for a message without mentions.
    pub async fn deliver_mentions(self: &Arc<Self>, from: &str, text: &str) -> Vec<MentionOutcome> {
        let tokens = parse_mentions(text);
        if tokens.is_empty() {
            return Vec::new();
        }
        let candidates = match self.targets(from) {
            Ok(c) => c,
            Err(e) => {
                let reason = format!("{e:#}");
                self.notice(from, &format!("Mentions were not delivered: {reason}"));
                return tokens
                    .into_iter()
                    .map(|token| MentionOutcome { token, session_id: None, receipt: None, error: Some(reason.clone()) })
                    .collect();
            }
        };
        let origin_title = self
            .sessions
            .store()
            .session(from)
            .ok()
            .flatten()
            .map(|s| s.title)
            .unwrap_or_default();
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for token in tokens {
            let target = match resolve(&token, &candidates) {
                Ok(t) => t.clone(),
                Err(e) => {
                    self.notice(from, &format!("Mention not delivered: {e}."));
                    out.push(MentionOutcome { token, session_id: None, receipt: None, error: Some(e) });
                    continue;
                }
            };
            if !seen.insert(target.id.clone()) {
                continue;
            }
            let body = format!(
                "The user mentioned this session in session {from} (\"{origin_title}\") and \
                 wrote:\n\n{text}"
            );
            let result = match self.store.insert_message(from, &target.id, None, &body) {
                Ok(msg) => self.deliver(&msg).await,
                Err(e) => Err(A2aError::Other(e)),
            };
            match result {
                Ok(receipt) => {
                    let how = match receipt.status {
                        super::Delivery::Delivered => "delivered",
                        super::Delivery::Queued => "queued until its turn ends",
                    };
                    self.notice(
                        from,
                        &format!(
                            "Mentioned session \"{}\" ({}): message {} {how}.",
                            target.title, target.id, receipt.id
                        ),
                    );
                    out.push(MentionOutcome {
                        token,
                        session_id: Some(target.id),
                        receipt: Some(receipt),
                        error: None,
                    });
                }
                Err(e) => {
                    let reason = format!("{e:#}");
                    self.notice(from, &format!("Mention of \"{}\" not delivered: {reason}", target.title));
                    out.push(MentionOutcome { token, session_id: Some(target.id), receipt: None, error: Some(reason) });
                }
            }
        }
        out
    }

    fn notice(&self, session: &str, message: &str) {
        let event = AgentEvent::Notice { message: message.to_string() };
        if let Err(e) = self.sessions.record_event(session, &event) {
            tracing::warn!(session = %session, "recording a mention notice failed: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::SessionStatus;

    fn t(id: &str, title: &str) -> Target {
        Target {
            id: id.into(),
            title: title.into(),
            project: "p".into(),
            agent: "scripted".into(),
            status: SessionStatus::Idle,
        }
    }

    #[test]
    fn parses_mentions() {
        assert_eq!(parse_mentions("hi @@beta, look"), vec!["beta"]);
        assert_eq!(
            parse_mentions("@@\"Deploy fixes\" and (@@1234abcd) and @@beta again @@beta"),
            vec!["Deploy fixes", "1234abcd", "beta"]
        );
        assert!(parse_mentions("mail a@@b or @@ alone or @@\"\"").is_empty());
        assert_eq!(parse_mentions("@@\"unterminated title\nnext"), vec!["unterminated title"]);
        assert_eq!(parse_mentions("ask @@한글세션?"), vec!["한글세션"]);
    }

    #[test]
    fn resolves_by_id_prefix_and_title() {
        let c = vec![t("abcd-1111", "Alpha"), t("abce-2222", "beta"), t("ffff-3333", "Beta")];
        assert_eq!(resolve("abcd-1111", &c).unwrap().id, "abcd-1111");
        assert_eq!(resolve("abce", &c).unwrap().id, "abce-2222");
        assert!(resolve("abc", &c).is_err(), "prefixes need four characters");
        assert_eq!(resolve("alpha", &c).unwrap().id, "abcd-1111");
        let e = resolve("BETA", &c).unwrap_err();
        assert!(e.contains("matches 2 sessions"), "{e}");
        assert!(resolve("gamma", &c).unwrap_err().contains("no session"));
    }
}
