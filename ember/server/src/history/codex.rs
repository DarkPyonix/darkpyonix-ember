//! Codex rollout files: `~/.codex/sessions/YYYY/MM/DD/rollout-<time>-<id>.jsonl`.
//!
//! One `{"timestamp","type","payload"}` object per line. `session_meta` carries the thread id
//! and cwd; the transcript is the `response_item` lines (`message`, `function_call`,
//! `custom_tool_call` and their outputs). `event_msg` lines repeat the same content for the UI,
//! and `compacted` lines repeat earlier history, so both are skipped. The format changes often:
//! anything unrecognised is skipped, never an error.

use serde_json::Value;

use super::{input_text, join_text_blocks, json_lines, str_field, Builder, Role, Transcript};

/// Context Codex injects as user input blocks; not something the user typed.
const INJECTED: &[&str] = &[
    "<environment_context>",
    "<user_instructions>",
    "<recommended_plugins>",
    "<permissions instructions>",
    "# AGENTS.md instructions",
];

pub fn parse(text: &str) -> Transcript {
    let mut out = Transcript::default();
    let mut turn_cwd: Option<String> = None;
    let mut b = Builder::default();

    for line in json_lines(text) {
        let Some(p) = line.get("payload").and_then(Value::as_object) else { continue };
        match line.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                if out.native_id.is_none() {
                    out.native_id = str_field(p, "id");
                }
                if out.cwd.is_none() {
                    out.cwd = str_field(p, "cwd");
                }
                continue;
            }
            Some("turn_context") => {
                if turn_cwd.is_none() {
                    turn_cwd = str_field(p, "cwd");
                }
                continue;
            }
            Some("response_item") => {}
            _ => continue,
        }
        let call_id = p.get("call_id").and_then(Value::as_str).unwrap_or("");
        let name = p.get("name").and_then(Value::as_str).unwrap_or("");
        match p.get("type").and_then(Value::as_str) {
            Some("message") => {
                let Some(blocks) = p.get("content").and_then(Value::as_array) else { continue };
                match p.get("role").and_then(Value::as_str) {
                    Some("user") => {
                        let typed: Vec<Value> = blocks
                            .iter()
                            .filter(|b| {
                                let t = b.get("text").and_then(Value::as_str).unwrap_or("");
                                !INJECTED.iter().any(|prefix| t.starts_with(prefix))
                            })
                            .cloned()
                            .collect();
                        b.text(Role::User, join_text_blocks(&typed, &["input_text"]));
                    }
                    Some("assistant") => {
                        b.text(Role::Assistant, join_text_blocks(blocks, &["output_text"]));
                    }
                    _ => {}
                }
            }
            Some("function_call") => {
                let args = p.get("arguments").and_then(Value::as_str).unwrap_or("");
                let input = match serde_json::from_str::<Value>(args) {
                    Ok(v) => input_text(&v),
                    Err(_) => args.to_string(),
                };
                b.call(name, call_id, input);
            }
            Some("custom_tool_call") => {
                let input = p.get("input").cloned().unwrap_or(Value::Null);
                b.call(name, call_id, input_text(&input));
            }
            Some("function_call_output") | Some("custom_tool_call_output") => {
                let (output, is_error) = tool_output(p.get("output"));
                b.result(call_id, output, is_error);
            }
            _ => {}
        }
    }
    if out.cwd.is_none() {
        out.cwd = turn_cwd;
    }
    out.messages = b.messages;
    out
}

/// A tool output: a block list's text, the older `{"output","metadata":{"exit_code"}}` exec
/// wrapper unwrapped, or a plain string as-is.
fn tool_output(output: Option<&Value>) -> (String, bool) {
    match output {
        Some(Value::Array(blocks)) => (join_text_blocks(blocks, &["input_text", "output_text"]), false),
        Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Object(wrapper)) if wrapper.get("output").is_some_and(Value::is_string) => {
                let text = wrapper["output"].as_str().unwrap_or("").to_string();
                let exit = wrapper.get("metadata").and_then(|m| m.get("exit_code"));
                let failed = exit.and_then(Value::as_f64).is_some_and(|c| c != 0.0);
                (text, failed)
            }
            _ => (s.clone(), false),
        },
        _ => (String::new(), false),
    }
}
