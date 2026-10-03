//! Claude Code session files: `~/.claude/projects/<slug>/<session-id>.jsonl`.
//!
//! One JSON object per line. Conversation lines are `{"type":"user"|"assistant",
//! "message":{"role","content"}, "sessionId", "cwd", "isSidechain", "isMeta", …}`, where
//! `content` is a string or a list of `text` / `thinking` / `tool_use` / `tool_result` blocks.
//! Title lines are `{"type":"custom-title","customTitle"}` and `{"type":"summary","summary"}`.
//! Everything else (snapshots, attachments, system lines, …) is skipped.

use serde_json::Value;

use super::{input_text, join_text_blocks, json_lines, str_field, Builder, Role, Transcript};

pub fn parse(text: &str) -> Transcript {
    let mut out = Transcript::default();
    let mut summary: Option<String> = None;
    let mut custom_title: Option<String> = None;
    let mut b = Builder::default();

    for line in json_lines(text) {
        if out.native_id.is_none() {
            out.native_id = str_field(&line, "sessionId");
        }
        if out.cwd.is_none() {
            out.cwd = str_field(&line, "cwd");
        }
        let kind = line.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "custom-title" => {
                if let Some(t) = str_field(&line, "customTitle") {
                    custom_title = Some(t);
                }
                continue;
            }
            "summary" => {
                if summary.is_none() {
                    summary = str_field(&line, "summary");
                }
                continue;
            }
            "user" | "assistant" => {}
            _ => continue,
        }
        let flag = |k: &str| line.get(k).and_then(Value::as_bool).unwrap_or(false);
        if flag("isSidechain") || flag("isMeta") || flag("isCompactSummary") {
            continue;
        }
        let Some(message) = line.get("message").and_then(Value::as_object) else { continue };
        let role = match message.get("role").and_then(Value::as_str).unwrap_or(kind) {
            "assistant" => Role::Assistant,
            _ => Role::User,
        };
        match message.get("content") {
            Some(Value::String(s)) => b.text(role, s.clone()),
            Some(Value::Array(blocks)) => {
                b.text(role, join_text_blocks(blocks, &["text"]));
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("tool_use") => {
                            let id = block.get("id").and_then(Value::as_str).unwrap_or("");
                            let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                            let input = block.get("input").cloned().unwrap_or(Value::Null);
                            b.call(name, id, input_text(&input));
                        }
                        Some("tool_result") => {
                            let id = block.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                            let output = match block.get("content") {
                                Some(Value::String(s)) => s.clone(),
                                Some(Value::Array(parts)) => join_text_blocks(parts, &["text"]),
                                _ => String::new(),
                            };
                            let is_error =
                                block.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                            b.result(id, output, is_error);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    out.title = custom_title.or(summary);
    out.messages = b.messages;
    out
}
