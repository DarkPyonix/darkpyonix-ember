//! Mention parsing for channel messages (SPEC FR-T12, FR-T13).
//!
//! In a channel a mention is `@<agent>` ([provisional]; the session composer keeps `@@`).
//! A token is `@` followed by name characters (letters, digits, `_`, `-`, and `.` only when a
//! name character follows it, so a closing full stop is not part of the name). The `@` must
//! start a word: not after a letter, digit or `_` `-` `.` (an email address) and not next to
//! another `@` (`@@` belongs to the session composer). A name matches a member agent's name
//! exactly, ignoring case; a prefix never matches. Names with spaces cannot be mentioned.

use serde::Serialize;

use super::Member;

/// The result of reading a message for mentions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Mentions {
    /// Member agents named, each once, in order of first mention.
    pub mentioned: Vec<Member>,
    /// Names that match no member agent, each once (case-insensitive), as written.
    pub unknown: Vec<String>,
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// Find `@name` tokens in `text` and resolve them against the agents among `members`.
pub fn parse_mentions(text: &str, members: &[Member]) -> Mentions {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Mentions::default();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '@' {
            i += 1;
            continue;
        }
        let start_ok = i == 0 || !(is_name_char(chars[i - 1]) || chars[i - 1] == '.' || chars[i - 1] == '@');
        let mut j = i + 1;
        while j < chars.len() {
            let c = chars[j];
            if is_name_char(c) || (c == '.' && chars.get(j + 1).is_some_and(|n| is_name_char(*n))) {
                j += 1;
            } else {
                break;
            }
        }
        // `@@` and a lone `@` carry no name; skip past the `@` only so `@@name` stays unread.
        if start_ok && j > i + 1 {
            let name: String = chars[i + 1..j].iter().collect();
            let lower = name.to_lowercase();
            let hits: Vec<&Member> =
                members.iter().filter(|m| m.is_agent && m.name.to_lowercase() == lower).collect();
            if hits.is_empty() {
                if !out.unknown.iter().any(|u| u.to_lowercase() == lower) {
                    out.unknown.push(name);
                }
            } else {
                for m in hits {
                    if !out.mentioned.iter().any(|x| x.id == m.id) {
                        out.mentioned.push(m.clone());
                    }
                }
            }
        }
        i = j.max(i + 1);
    }
    out
}
