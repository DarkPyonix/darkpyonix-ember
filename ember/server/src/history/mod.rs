//! Importing existing agent history (SPEC FR-S2, `INTENT.md` D13).
//!
//! Each agent keeps its own transcript files on disk (Claude Code under `~/.claude/projects`,
//! Codex under `~/.codex/sessions`). The importers here read one such file into a neutral
//! [`Transcript`], which [`to_events`] turns into the server's [`AgentEvent`]s so the history can
//! be stored like any live session. The format and its edge cases are pinned by the shared
//! vectors in `tests/vectors/transcripts/` (see its `SCHEMA.md`), which the Python adapters in
//! `web/proxy/dpx/agents/` are tested against too.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::agents::AgentKind;
use crate::events::{AgentEvent, TurnOutcome};

pub mod claude_code;
pub mod codex;

/// One native transcript, normalised.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    /// The agent's own session id (what its resume takes), if the file says.
    pub native_id: Option<String>,
    pub cwd: Option<String>,
    /// A title the file carries (Claude Code's custom title or summary). Never derived.
    pub title: Option<String>,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    /// Assistant text, or a tool call when `tool_name` is set.
    Assistant,
    /// A tool result.
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    /// Message text; for a tool call, its input (canonical JSON unless a plain string).
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

impl Message {
    fn text(role: Role, text: String) -> Message {
        Message { role, text, tool_name: None, tool_call_id: None, is_error: None }
    }

    fn call(name: &str, call_id: &str, input: String) -> Message {
        Message {
            role: Role::Assistant,
            text: input,
            tool_name: Some(name.to_string()),
            tool_call_id: Some(call_id.to_string()),
            is_error: None,
        }
    }

    fn is_tool_call(&self) -> bool {
        self.role == Role::Assistant && self.tool_name.is_some()
    }
}

/// Builds the message list, resolving tool-result names from earlier calls.
#[derive(Default)]
struct Builder {
    messages: Vec<Message>,
    names: std::collections::HashMap<String, String>,
}

impl Builder {
    /// Pushes a user/assistant text message unless it is blank.
    fn text(&mut self, role: Role, text: String) {
        if !text.trim().is_empty() {
            self.messages.push(Message::text(role, text));
        }
    }

    fn call(&mut self, name: &str, call_id: &str, input: String) {
        self.names.insert(call_id.to_string(), name.to_string());
        self.messages.push(Message::call(name, call_id, input));
    }

    fn result(&mut self, call_id: &str, output: String, is_error: bool) {
        self.messages.push(Message {
            role: Role::Tool,
            text: output,
            tool_name: self.names.get(call_id).cloned(),
            tool_call_id: Some(call_id.to_string()),
            is_error: Some(is_error),
        });
    }
}

/// Reads a native transcript file of the given agent.
pub fn import_file(agent: AgentKind, path: &Path) -> anyhow::Result<Transcript> {
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes);
    match agent {
        AgentKind::ClaudeCode => Ok(claude_code::parse(&text)),
        AgentKind::Codex => Ok(codex::parse(&text)),
        AgentKind::Antigravity => {
            anyhow::bail!("importing Antigravity history is not supported yet")
        }
        AgentKind::Scripted => anyhow::bail!("the scripted agent has no native history"),
        // ACP has no transcript file format; each agent stores sessions its own way.
        AgentKind::Acp(name) => anyhow::bail!("importing {} transcripts is not supported", name.as_str()),
    }
}

/// The JSON objects of a JSONL file, skipping anything else (blank, malformed, truncated lines).
/// Splits on `\n` only: U+2028 inside a JSON string is text, not a line break.
fn json_lines(text: &str) -> impl Iterator<Item = serde_json::Map<String, serde_json::Value>> + '_ {
    text.split('\n').filter_map(|line| {
        let line = line.trim();
        if !line.starts_with('{') {
            return None;
        }
        match serde_json::from_str(line) {
            Ok(serde_json::Value::Object(map)) => Some(map),
            _ => None,
        }
    })
}

/// A tool input as message text: a JSON string as-is, anything else as canonical JSON.
fn input_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => canonical_json(other),
    }
}

/// Compact JSON with object keys sorted, independent of serde_json's map ordering feature.
fn canonical_json(value: &serde_json::Value) -> String {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let fields: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", Value::String(k.clone()), canonical_json(&map[k])))
                .collect();
            format!("{{{}}}", fields.join(","))
        }
        Value::Array(items) => {
            format!("[{}]", items.iter().map(canonical_json).collect::<Vec<_>>().join(","))
        }
        scalar => scalar.to_string(),
    }
}

/// The `text` of the text-like blocks in `blocks` whose type is in `types`, non-blank ones joined
/// with `\n`.
fn join_text_blocks(blocks: &[serde_json::Value], types: &[&str]) -> String {
    blocks
        .iter()
        .filter(|b| b.get("type").and_then(|t| t.as_str()).is_some_and(|t| types.contains(&t)))
        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
        .filter(|t| !t.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn str_field(map: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    map.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string)
}

/// The events a live session would have stored for this transcript, for importing it.
///
/// Starts with [`AgentEvent::NativeSession`] when the native id is known, so the imported
/// session resumes natively. Each user message after some agent activity closes the previous
/// turn with `TurnEnded { Completed }`, and so does the end of the transcript.
pub fn to_events(transcript: &Transcript) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    if let Some(native_id) = &transcript.native_id {
        events.push(AgentEvent::NativeSession { native_id: native_id.clone() });
    }
    let mut turn_open = false;
    for m in &transcript.messages {
        let event = match m.role {
            Role::User => {
                if turn_open {
                    events.push(AgentEvent::TurnEnded { outcome: TurnOutcome::Completed });
                }
                AgentEvent::UserMessage { text: m.text.clone() }
            }
            Role::Assistant if m.is_tool_call() => AgentEvent::ToolCall {
                call_id: m.tool_call_id.clone().unwrap_or_default(),
                name: m.tool_name.clone().unwrap_or_default(),
                input: serde_json::from_str(&m.text)
                    .unwrap_or_else(|_| serde_json::Value::String(m.text.clone())),
            },
            Role::Assistant => AgentEvent::AssistantMessage { text: m.text.clone() },
            Role::Tool => AgentEvent::ToolResult {
                call_id: m.tool_call_id.clone().unwrap_or_default(),
                output: m.text.clone(),
                is_error: m.is_error.unwrap_or(false),
            },
        };
        turn_open = true;
        events.push(event);
    }
    if turn_open {
        events.push(AgentEvent::TurnEnded { outcome: TurnOutcome::Completed });
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn vectors_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/vectors/transcripts")
    }

    /// Runs `agent`'s importer over every vector under `tests/vectors/transcripts/<dir>/`.
    fn run_vectors(agent: AgentKind, dir: &str) {
        let mut cases: Vec<PathBuf> = std::fs::read_dir(vectors_dir().join(dir))
            .expect("vector dir")
            .map(|e| e.unwrap().path())
            .filter(|p| p.join("input.jsonl").is_file())
            .collect();
        cases.sort();
        assert!(!cases.is_empty(), "no vectors for {dir}");
        let mut failures = Vec::new();
        for case in &cases {
            let got = import_file(agent, &case.join("input.jsonl")).unwrap();
            let expected: Transcript = serde_json::from_str(
                &std::fs::read_to_string(case.join("expected.json")).unwrap(),
            )
            .unwrap();
            if got != expected {
                failures.push(format!(
                    "{}:\n  expected {}\n  got      {}",
                    case.display(),
                    serde_json::to_string(&expected).unwrap(),
                    serde_json::to_string(&got).unwrap()
                ));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn history_claude_code_vectors() {
        run_vectors(AgentKind::ClaudeCode, "claude-code");
    }

    #[test]
    fn history_codex_vectors() {
        run_vectors(AgentKind::Codex, "codex");
    }

    #[test]
    fn history_expected_json_round_trips() {
        // The expected files omit absent fields rather than writing null; keep it that way.
        let path = vectors_dir().join("claude-code/tool-use/expected.json");
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let parsed: Transcript = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), raw);
    }

    #[test]
    fn history_canonical_json_sorts_keys() {
        let v = serde_json::json!({"b": [1, {"d": "é", "c": null}], "a": "x\ny"});
        assert_eq!(canonical_json(&v), r#"{"a":"x\ny","b":[1,{"c":null,"d":"é"}]}"#);
    }

    #[test]
    fn history_to_events_closes_turns() {
        let t = import_file(
            AgentKind::ClaudeCode,
            &vectors_dir().join("claude-code/tool-use/input.jsonl"),
        )
        .unwrap();
        let events = to_events(&t);
        assert_eq!(
            events[0],
            AgentEvent::NativeSession { native_id: "22222222-2222-4222-8222-222222222222".into() }
        );
        assert_eq!(
            events[1],
            AgentEvent::UserMessage { text: "List the files and read the config.".into() }
        );
        assert_eq!(
            events[2],
            AgentEvent::ToolCall {
                call_id: "toolu_01".into(),
                name: "Bash".into(),
                input: serde_json::json!({"command": "ls", "description": "List files"}),
            }
        );
        assert!(events.contains(&AgentEvent::ToolResult {
            call_id: "toolu_03".into(),
            output: "Exit code 1\ncat: missing.log: No such file or directory".into(),
            is_error: true,
        }));
        assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }));
        let turn_ends =
            events.iter().filter(|e| matches!(e, AgentEvent::TurnEnded { .. })).count();
        assert_eq!(turn_ends, 1);

        // Two user turns → two TurnEnded, the first right before the second user message.
        let t = import_file(AgentKind::Codex, &vectors_dir().join("codex/plain-chat/input.jsonl"))
            .unwrap();
        let events = to_events(&t);
        let kinds: Vec<&str> = events
            .iter()
            .map(|e| match e {
                AgentEvent::NativeSession { .. } => "native",
                AgentEvent::UserMessage { .. } => "user",
                AgentEvent::AssistantMessage { .. } => "assistant",
                AgentEvent::TurnEnded { .. } => "end",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["native", "user", "assistant", "end", "user", "assistant", "end"]);
        // A non-JSON tool input stays a string.
        let t = import_file(AgentKind::Codex, &vectors_dir().join("codex/tool-use/input.jsonl"))
            .unwrap();
        assert!(to_events(&t).contains(&AgentEvent::ToolCall {
            call_id: "call_4".into(),
            name: "view_image".into(),
            input: serde_json::Value::String("not json at all".into()),
        }));
    }

    #[test]
    fn history_empty_transcript_has_no_events() {
        assert!(to_events(&Transcript::default()).is_empty());
    }

    #[test]
    fn history_imported_events_store_as_finished_session() {
        let store = crate::store::Store::open_in_memory().unwrap();
        let t = import_file(AgentKind::Codex, &vectors_dir().join("codex/tool-use/input.jsonl"))
            .unwrap();
        let s = store.create_session("p", AgentKind::Codex, "/work/project", None, "t").unwrap();
        for e in to_events(&t) {
            store.append(&s.id, &e).unwrap();
        }
        let s = store.session(&s.id).unwrap().unwrap();
        assert_eq!(s.native_id, t.native_id);
        assert_eq!(s.status, crate::events::SessionStatus::Finished);
    }
}
