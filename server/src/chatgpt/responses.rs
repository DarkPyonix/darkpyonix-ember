//! Request shaping and the streamed reply for Responses API calls billed to a ChatGPT plan.
//!
//! Constraints (developers.openai.com/siwc/token-sharing-open-source/models-and-inference and
//! /preview-limitations, read 2026-10-03):
//!
//! * `store: false` and `stream: true` are required, and `input` must be an array. Ember sets
//!   `store` and `stream` itself, whatever the caller passed.
//! * `previous_response_id` is not available over HTTP, and these fields must be omitted:
//!   `background`, `conversation`, `max_output_tokens`, `max_tool_calls`, `metadata`,
//!   `moderation`, `multi_agent`, `prompt`, `prompt_cache_retention`, `safety_identifier`,
//!   `temperature`, `top_logprobs`, `top_p`, `truncation`, `user`. A request with any of them is
//!   refused here with the field's name, instead of being trimmed silently.
//! * Unsupported tools: image generation, file search, Code Interpreter, native computer use,
//!   hosted MCP/connectors and `tool_search`. Function/custom tools and web search stay allowed
//!   (the docs ask for function/custom tools to be grouped in namespaces or supplied through
//!   `additional_tools` input items; that grouping is left to the caller).

use serde_json::Value;

/// Body fields the plan-usage flow does not accept.
pub const UNSUPPORTED_FIELDS: &[&str] = &[
    "previous_response_id",
    "background",
    "conversation",
    "max_output_tokens",
    "max_tool_calls",
    "metadata",
    "moderation",
    "multi_agent",
    "prompt",
    "prompt_cache_retention",
    "safety_identifier",
    "temperature",
    "top_logprobs",
    "top_p",
    "truncation",
    "user",
];

/// Tool `type`s the plan-usage flow does not support.
pub const UNSUPPORTED_TOOLS: &[(&str, &str)] = &[
    ("image_generation", "image generation"),
    ("file_search", "file search"),
    ("code_interpreter", "Code Interpreter"),
    ("computer_use", "native computer use"),
    ("computer_use_preview", "native computer use"),
    ("computer", "native computer use"),
    ("mcp", "hosted MCP/connectors"),
    ("tool_search", "Responses tool_search"),
];

/// Make `body` a valid plan-usage request, or say exactly why it cannot be one.
pub fn shape_request(body: Value) -> Result<Value, String> {
    let Value::Object(mut body) = body else {
        return Err("the request body must be a JSON object".into());
    };
    match body.get("model") {
        Some(Value::String(m)) if !m.is_empty() => {}
        _ => return Err("`model` is required (a slug from GET /v1/models)".into()),
    }
    match body.get("input") {
        Some(Value::Array(_)) => {}
        _ => {
            return Err("`input` must be an array of input items for ChatGPT plan usage".into())
        }
    }
    for f in UNSUPPORTED_FIELDS {
        if body.contains_key(*f) {
            return Err(format!(
                "`{f}` is not supported when the request uses a ChatGPT plan; remove it"
            ));
        }
    }
    if let Some(tools) = body.get("tools") {
        check_tools(tools)?;
    }
    if let Some(Value::Object(choice)) = body.get("tool_choice") {
        check_tool(&Value::Object(choice.clone()))?;
    }
    if let Some(Value::Array(items)) = body.get("input") {
        for item in items {
            if item["type"].as_str() == Some("additional_tools") {
                check_tools(&item["tools"])?;
            }
        }
    }
    body.insert("store".into(), Value::Bool(false));
    body.insert("stream".into(), Value::Bool(true));
    Ok(Value::Object(body))
}

fn check_tools(tools: &Value) -> Result<(), String> {
    match tools {
        Value::Array(list) => list.iter().try_for_each(check_tool),
        Value::Null => Ok(()),
        _ => Err("`tools` must be an array".into()),
    }
}

fn check_tool(tool: &Value) -> Result<(), String> {
    let ty = tool["type"].as_str().unwrap_or_default();
    if let Some((_, what)) = UNSUPPORTED_TOOLS.iter().find(|(t, _)| *t == ty) {
        return Err(format!(
            "tool type `{ty}` ({what}) is not available with ChatGPT plan usage; remove it or \
             use an API-key provider"
        ));
    }
    // Namespaces group other tools; check what they contain.
    if let Some(inner) = tool.get("tools") {
        check_tools(inner)?;
    }
    Ok(())
}

/// One server-sent event: its type (`response.output_text.delta`, …) and JSON data.
#[derive(Debug, Clone, PartialEq)]
pub struct SseEvent {
    pub kind: String,
    pub data: Value,
}

/// Incremental SSE parser: feed bytes, take complete events.
#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
}

impl SseParser {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next complete event, if one is buffered. Comments and `[DONE]` are skipped.
    pub fn next_event(&mut self) -> Option<SseEvent> {
        loop {
            let (end, sep) = find_blank_line(&self.buf)?;
            let raw: Vec<u8> = self.buf.drain(..end + sep).collect();
            if let Some(ev) = parse_event(&raw[..end]) {
                return Some(ev);
            }
        }
    }

    /// At end of stream: an event left without its trailing blank line.
    pub fn finish(&mut self) -> Option<SseEvent> {
        let raw = std::mem::take(&mut self.buf);
        parse_event(&raw)
    }
}

/// Position of the first blank line (`\n\n` or `\r\n\r\n`) and the separator's length.
fn find_blank_line(buf: &[u8]) -> Option<(usize, usize)> {
    let lf = buf.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2));
    let crlf = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| (i, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (a, b) => a.or(b),
    }
}

fn parse_event(raw: &[u8]) -> Option<SseEvent> {
    let text = String::from_utf8_lossy(raw);
    let mut event = None;
    let mut data = String::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(v) = line.strip_prefix("event:") {
            event = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(v.strip_prefix(' ').unwrap_or(v));
        }
    }
    if data.is_empty() || data == "[DONE]" {
        return None;
    }
    let data: Value = serde_json::from_str(&data).unwrap_or(Value::String(data));
    let kind = event
        .or_else(|| data["type"].as_str().map(str::to_string))
        .unwrap_or_else(|| "message".into());
    Some(SseEvent { kind, data })
}

/// `(input_tokens, output_tokens)` of a `response.completed` event.
pub fn completed_usage(data: &Value) -> Option<(u64, u64)> {
    let u = &data["response"]["usage"];
    Some((u["input_tokens"].as_u64()?, u["output_tokens"].as_u64()?))
}

/// The error object of a `response.failed` event, or of a JSON error body.
pub fn error_of(v: &Value) -> (Option<String>, String) {
    // `response.failed` nests it under `response`; HTTP error bodies under `error`; the stream's
    // `error` event carries `code`/`message` at the top level.
    let e = if v["response"]["error"].is_object() {
        &v["response"]["error"]
    } else if v["error"].is_object() {
        &v["error"]
    } else {
        v
    };
    let code = e["code"]
        .as_str()
        .or_else(|| e["type"].as_str())
        .map(str::to_string);
    let message = e["message"]
        .as_str()
        .unwrap_or("the request failed")
        .to_string();
    (code, message)
}

/// Keep only the visible models of `GET /v1/models` (`visibility: "list"`).
pub fn listed_models(v: &Value) -> Vec<Value> {
    let list = v["models"].as_array().or_else(|| v["data"].as_array());
    list.map(|l| {
        l.iter()
            .filter(|m| m["visibility"].as_str().is_none_or(|x| x == "list"))
            .cloned()
            .collect()
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base() -> Value {
        json!({"model": "gpt-x", "input": [{"role": "user", "content": "hi"}]})
    }

    #[test]
    fn store_and_stream_are_forced() {
        let mut b = base();
        b["store"] = json!(true);
        b["stream"] = json!(false);
        let out = shape_request(b).unwrap();
        assert_eq!(out["store"], json!(false));
        assert_eq!(out["stream"], json!(true));
        let out = shape_request(base()).unwrap();
        assert_eq!((out["store"].clone(), out["stream"].clone()), (json!(false), json!(true)));
    }

    #[test]
    fn unsupported_fields_and_shapes_are_refused_by_name() {
        for f in UNSUPPORTED_FIELDS {
            let mut b = base();
            b[*f] = json!("x");
            let err = shape_request(b).unwrap_err();
            assert!(err.contains(f), "{err}");
        }
        assert!(shape_request(json!({"model": "m", "input": "hi"})).is_err());
        assert!(shape_request(json!({"input": []})).unwrap_err().contains("model"));
        assert!(shape_request(json!([])).is_err());
    }

    #[test]
    fn unsupported_tools_are_refused_anywhere() {
        for (t, _) in UNSUPPORTED_TOOLS {
            let mut b = base();
            b["tools"] = json!([{"type": "function", "name": "f"}, {"type": t}]);
            let err = shape_request(b).unwrap_err();
            assert!(err.contains(t), "{err}");
        }
        // Inside a namespace, in tool_choice, and in an `additional_tools` input item.
        let mut b = base();
        b["tools"] = json!([{"type": "namespace", "name": "ns", "tools": [{"type": "file_search"}]}]);
        assert!(shape_request(b).is_err());
        let mut b = base();
        b["tool_choice"] = json!({"type": "code_interpreter"});
        assert!(shape_request(b).is_err());
        let mut b = base();
        b["input"] = json!([{"type": "additional_tools", "tools": [{"type": "mcp"}]}]);
        assert!(shape_request(b).is_err());

        // Function, custom and web search tools pass.
        let mut b = base();
        b["tools"] = json!([
            {"type": "function", "name": "f", "parameters": {}},
            {"type": "custom", "name": "c"},
            {"type": "web_search"}
        ]);
        assert!(shape_request(b).is_ok());
    }

    #[test]
    fn sse_parsing_handles_split_chunks_crlf_and_done() {
        let mut p = SseParser::default();
        p.push(b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",");
        assert!(p.next_event().is_none());
        p.push(b"\"delta\":\"Hi\"}\n\n: comment\n\ndata: {\"type\":\"response.completed\"}\r\n\r\n");
        let a = p.next_event().unwrap();
        assert_eq!(a.kind, "response.output_text.delta");
        assert_eq!(a.data["delta"], "Hi");
        let b = p.next_event().unwrap();
        assert_eq!(b.kind, "response.completed");
        p.push(b"data: [DONE]\n\n");
        assert!(p.next_event().is_none());
        p.push(b"data: {\"type\":\"tail\"}");
        assert_eq!(p.finish().unwrap().kind, "tail");
    }

    #[test]
    fn usage_errors_and_models() {
        let done = json!({"response": {"usage": {"input_tokens": 12, "output_tokens": 3}}});
        assert_eq!(completed_usage(&done), Some((12, 3)));
        let failed = json!({"response": {"error": {"code": "subscription_sharing_usage_limit_exceeded", "message": "cap"}}});
        assert_eq!(
            error_of(&failed),
            (Some("subscription_sharing_usage_limit_exceeded".into()), "cap".into())
        );
        let models = json!({"models": [
            {"slug": "a", "visibility": "list"}, {"slug": "b", "visibility": "hide"}
        ]});
        assert_eq!(listed_models(&models).len(), 1);
    }
}
