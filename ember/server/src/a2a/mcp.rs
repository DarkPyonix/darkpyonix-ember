//! `ember-a2a mcp`: a stdio MCP server with one tool, `channel_post` (SPEC FR-T12, #380).
//!
//! Newline-delimited JSON-RPC 2.0 on stdin and stdout, hand-written (no SDK dependency). Stdout
//! carries protocol frames only, all written through [`write_frame`]. Diagnostics go to stderr
//! and never contain the token, a message text or the URL. The tool calls the same route as
//! `ember-a2a channel post`; authorization, the 30-minute mention window and the rate limit stay
//! on the server.
//!
//! Measured on 2026-11-06 (Claude Code 2.1.319, Codex 0.164.0): Claude Code sends
//! `server/discover` first and `initialize` with protocol 2025-11-25; Codex sends `initialize`
//! with 2025-06-18 and later probes `resources/list` and `resources/templates/list`. Every
//! unknown request is answered with `-32601` and the loop goes on.

use std::io::{self, BufRead, Write};

use serde_json::{json, Value};

use super::client::{channel_post_path, Client};

/// Protocol versions this server answers to, newest first.
pub const SUPPORTED_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
/// The tool's name.
pub const TOOL_NAME: &str = "channel_post";
/// A frame longer than this (without its newline) is discarded.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;
/// Longest `text`. The agent route has no cap of its own; this is the guest route's cap
/// (`channels::guest::MAX_TEXT_BYTES`).
pub const MAX_TEXT_BYTES: usize = 8192;
/// Longest channel id (the JSONL log refuses longer ones, `channels/jsonl.rs`).
pub const MAX_CHANNEL_ID: usize = 128;
/// Longest server error text carried into a tool result.
const MAX_REASON_CHARS: usize = 200;

const NOT_IN_SESSION: &str =
    "channel_post only works inside an Ember agent session (EMBER_URL and EMBER_RUNTIME_TOKEN are not set).";
const UNREACHABLE: &str = "Could not reach the Ember server.";

/// Why a post did not happen. `reason` is short and safe to show to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostRefusal {
    pub reason: String,
}

impl PostRefusal {
    pub fn new(reason: impl Into<String>) -> PostRefusal {
        PostRefusal { reason: reason.into() }
    }
}

/// Posts into a channel; the real one calls the server, tests inject a fake.
pub trait ChannelPoster {
    /// Returns the new message's id.
    fn post(&self, channel: &str, text: &str) -> Result<String, PostRefusal>;
}

/// The refusal for a failure of our own (connect, read, bad response). The error text is
/// dropped on purpose: it may carry the URL, and nothing in it helps the model.
pub fn refusal_from_failure(_error: &anyhow::Error) -> PostRefusal {
    PostRefusal::new(UNREACHABLE)
}

/// The refusal for a non-2xx answer. Only the status and the server's own `error` text are used,
/// and the text only when the body has the server's `{error, code}` shape, so a proxy's page or
/// any other field never reaches the model. Control characters are dropped; at most 200 chars.
pub fn refusal_from_response(status: u16, body: &Value) -> PostRefusal {
    let own_shape = body.get("code").is_some_and(Value::is_string);
    let text = body.get("error").and_then(Value::as_str).filter(|_| own_shape);
    match text {
        Some(t) => {
            let clean: String = t.chars().filter(|c| !c.is_control()).take(MAX_REASON_CHARS).collect();
            PostRefusal::new(format!("The post was refused (HTTP {status}): {clean}"))
        }
        None => PostRefusal::new(format!("The post was refused (HTTP {status}).")),
    }
}

/// The real poster: reads `EMBER_URL` and `EMBER_RUNTIME_TOKEN` at each call, so a missing
/// variable breaks the tool only, never `initialize` or `tools/list`.
pub struct EnvPoster;

impl ChannelPoster for EnvPoster {
    fn post(&self, channel: &str, text: &str) -> Result<String, PostRefusal> {
        let client = Client::from_env().map_err(|_| PostRefusal::new(NOT_IN_SESSION))?;
        let (status, body) = client
            .request("POST", &channel_post_path(channel), Some(&json!({ "text": text })))
            .map_err(|e| refusal_from_failure(&e))?;
        if (200..300).contains(&status) {
            Ok(body.get("id").and_then(Value::as_str).unwrap_or("?").to_string())
        } else {
            Err(refusal_from_response(status, &body))
        }
    }
}

/// Entry point of `ember-a2a mcp`; returns the process exit code.
pub fn run_stdio() -> i32 {
    let stdin = io::stdin();
    let stdout = io::stdout();
    match serve(stdin.lock(), stdout.lock(), &EnvPoster) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("ember-a2a mcp: i/o error ({:?})", e.kind());
            1
        }
    }
}

/// Handle frames from `input` until EOF, one at a time, answering on `output`.
pub fn serve<R: BufRead, W: Write>(mut input: R, mut output: W, poster: &dyn ChannelPoster) -> io::Result<()> {
    let mut line = Vec::new();
    loop {
        match read_frame(&mut input, &mut line)? {
            Frame::Eof => return Ok(()),
            Frame::TooLong => {
                write_frame(&mut output, &error_response(&Value::Null, -32600, "frame too large"))?;
            }
            Frame::Line => {
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if line.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                if let Some(answer) = handle(&line, poster) {
                    write_frame(&mut output, &answer)?;
                }
            }
        }
    }
}

enum Frame {
    Eof,
    Line,
    TooLong,
}

/// Read one line into `out` without ever holding more than [`MAX_LINE_BYTES`]; a longer line is
/// drained to its end and reported as `TooLong`.
fn read_frame<R: BufRead>(r: &mut R, out: &mut Vec<u8>) -> io::Result<Frame> {
    out.clear();
    let mut too_long = false;
    let mut any = false;
    loop {
        let buf = match r.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if buf.is_empty() {
            return Ok(if !any {
                Frame::Eof
            } else if too_long {
                Frame::TooLong
            } else {
                Frame::Line
            });
        }
        any = true;
        let newline = buf.iter().position(|b| *b == b'\n');
        let chunk_len = newline.unwrap_or(buf.len());
        if !too_long {
            if out.len() + chunk_len > MAX_LINE_BYTES {
                too_long = true;
                out.clear();
            } else {
                out.extend_from_slice(&buf[..chunk_len]);
            }
        }
        r.consume(chunk_len + usize::from(newline.is_some()));
        if newline.is_some() {
            return Ok(if too_long { Frame::TooLong } else { Frame::Line });
        }
    }
}

/// The one place that writes to the output: a frame, a newline, a flush.
fn write_frame<W: Write>(out: &mut W, frame: &Value) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(frame).map_err(io::Error::other)?;
    bytes.push(b'\n');
    out.write_all(&bytes)?;
    out.flush()
}

fn ok_response(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn tool_result(text: &str, is_error: bool) -> Value {
    let mut v = json!({ "content": [{ "type": "text", "text": text }] });
    if is_error {
        v["isError"] = json!(true);
    }
    v
}

/// The answer to one line, or `None` for a notification.
fn handle(line: &[u8], poster: &dyn ChannelPoster) -> Option<Value> {
    let Ok(frame) = serde_json::from_slice::<Value>(line) else {
        return Some(error_response(&Value::Null, -32700, "parse error"));
    };
    let Some(method) = frame.as_object().and_then(|o| o.get("method")).and_then(Value::as_str) else {
        return Some(error_response(&Value::Null, -32600, "not a JSON-RPC request"));
    };
    let Some(id) = frame.get("id") else {
        return None; // a notification gets no answer, known or not
    };
    if !(id.is_string() || id.is_number() || id.is_null()) {
        return Some(error_response(&Value::Null, -32600, "invalid request id"));
    }
    let null = Value::Null;
    let params = frame.get("params").unwrap_or(&null);
    let answer = match method {
        "initialize" => Ok(initialize_result(params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": [tool_definition()] })),
        "tools/call" => call_tool(params, poster),
        _ => Err((-32601, "method not found")),
    };
    Some(match answer {
        Ok(result) => ok_response(id, result),
        Err((code, message)) => error_response(id, code, message),
    })
}

fn initialize_result(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = SUPPORTED_VERSIONS.iter().find(|v| Some(**v) == asked).unwrap_or(&SUPPORTED_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "ember-channels", "version": env!("CARGO_PKG_VERSION") },
    })
}

fn tool_definition() -> Value {
    json!({
        "name": TOOL_NAME,
        "description": "Answer in a channel where a person mentioned you; allowed only within 30 minutes of a mention, at most 6 posts a minute.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "The channel id from the mention header (the text after \"id\")."
                },
                "text": {
                    "type": "string",
                    "description": "The message to post in the channel."
                }
            },
            "required": ["channel", "text"],
            "additionalProperties": false
        }
    })
}

fn call_tool(params: &Value, poster: &dyn ChannelPoster) -> Result<Value, (i64, &'static str)> {
    let Some(p) = params.as_object() else {
        return Err((-32602, "invalid params"));
    };
    if p.get("name").and_then(Value::as_str) != Some(TOOL_NAME) {
        return Err((-32602, "unknown tool"));
    }
    let (channel, text) = match validate_arguments(p.get("arguments")) {
        Ok(v) => v,
        Err(reason) => return Ok(tool_result(reason, true)),
    };
    Ok(match poster.post(channel, text) {
        Ok(id) => tool_result(&format!("Posted message {id} in channel {channel}."), false),
        Err(refusal) => tool_result(&refusal.reason, true),
    })
}

/// A channel id is a UUID or `default-<project>` (project names: letters, digits, `-`, `_`, `.`).
fn valid_channel(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_CHANNEL_ID
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && !s.bytes().all(|b| b == b'.') // `.` and `..` would climb the URL path
}

fn validate_arguments(arguments: Option<&Value>) -> Result<(&str, &str), &'static str> {
    let Some(args) = arguments.and_then(Value::as_object) else {
        return Err("arguments must be an object with channel and text.");
    };
    if args.keys().any(|k| k != "channel" && k != "text") {
        return Err("only the arguments channel and text are accepted.");
    }
    let Some(channel) = args.get("channel").and_then(Value::as_str) else {
        return Err("channel is required and must be a string.");
    };
    let Some(text) = args.get("text").and_then(Value::as_str) else {
        return Err("text is required and must be a string.");
    };
    if !valid_channel(channel) {
        return Err("channel must be the channel id from the mention (letters, digits, '-', '_', '.'; at most 128 characters).");
    }
    if text.trim().is_empty() {
        return Err("text must not be empty.");
    }
    if text.len() > MAX_TEXT_BYTES {
        return Err("text is too long (at most 8192 bytes).");
    }
    Ok((channel, text))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::io::Cursor;

    use super::*;

    struct Fake {
        calls: RefCell<Vec<(String, String)>>,
        answer: Result<String, PostRefusal>,
    }

    impl Fake {
        fn ok() -> Fake {
            Fake { calls: RefCell::new(vec![]), answer: Ok("m-1".into()) }
        }
        fn refusing(reason: &str) -> Fake {
            Fake { calls: RefCell::new(vec![]), answer: Err(PostRefusal::new(reason)) }
        }
    }

    impl ChannelPoster for Fake {
        fn post(&self, channel: &str, text: &str) -> Result<String, PostRefusal> {
            self.calls.borrow_mut().push((channel.into(), text.into()));
            self.answer.clone()
        }
    }

    /// Feed `input`, return the raw output after checking that every line is a JSON-RPC 2.0 frame.
    fn run_bytes(input: Vec<u8>, poster: &dyn ChannelPoster) -> Vec<Value> {
        let mut out = Vec::new();
        serve(Cursor::new(input), &mut out, poster).unwrap();
        if out.is_empty() {
            return vec![];
        }
        assert_eq!(out.last(), Some(&b'\n'), "every frame ends with a newline");
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| {
                let v: Value = serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}"));
                assert_eq!(v["jsonrpc"], "2.0", "{l}");
                v
            })
            .collect()
    }

    fn run(frames: &[Value], poster: &dyn ChannelPoster) -> Vec<Value> {
        let mut input = String::new();
        for f in frames {
            input.push_str(&f.to_string());
            input.push('\n');
        }
        run_bytes(input.into_bytes(), poster)
    }

    fn req(id: i64, method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    fn note(method: &str) -> Value {
        json!({ "jsonrpc": "2.0", "method": method })
    }

    fn call(id: i64, args: Value) -> Value {
        req(id, "tools/call", json!({ "name": "channel_post", "arguments": args }))
    }

    fn tool_error(v: &Value) -> bool {
        v["result"]["isError"] == json!(true)
    }

    #[test]
    fn fr_t12_mcp_initialize_negotiates_each_supported_version() {
        let p = Fake::ok();
        for v in SUPPORTED_VERSIONS {
            let out = run(&[req(1, "initialize", json!({ "protocolVersion": v }))], &p);
            assert_eq!(out.len(), 1);
            assert_eq!(out[0]["id"], 1);
            assert_eq!(out[0]["result"]["protocolVersion"], v);
        }
    }

    #[test]
    fn fr_t12_mcp_initialize_falls_back_to_the_newest() {
        let p = Fake::ok();
        for params in [json!({ "protocolVersion": "2030-01-01" }), json!({}), json!(null), json!({ "protocolVersion": 7 })] {
            let out = run(&[req(1, "initialize", params)], &p);
            assert_eq!(out[0]["result"]["protocolVersion"], "2025-11-25");
        }
    }

    #[test]
    fn fr_t12_mcp_initialize_answers_capabilities_and_server_info() {
        let out = run(&[req(1, "initialize", json!({ "protocolVersion": "2025-06-18" }))], &Fake::ok());
        let r = &out[0]["result"];
        assert_eq!(r["capabilities"], json!({ "tools": {} }));
        assert_eq!(r["serverInfo"]["name"], "ember-channels");
        assert_eq!(r["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn fr_t12_mcp_notifications_get_no_answer() {
        let p = Fake::ok();
        let out = run(
            &[note("notifications/initialized"), note("notifications/cancelled"), note("something/unknown")],
            &p,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn fr_t12_mcp_ping_answers_an_empty_object() {
        let out = run(&[req(5, "ping", json!(null))], &Fake::ok());
        assert_eq!(out[0], json!({ "jsonrpc": "2.0", "id": 5, "result": {} }));
    }

    #[test]
    fn fr_t12_mcp_string_ids_are_echoed() {
        let out = run(&[json!({ "jsonrpc": "2.0", "id": "abc", "method": "ping" })], &Fake::ok());
        assert_eq!(out[0]["id"], "abc");
    }

    #[test]
    fn fr_t12_mcp_tools_list_has_exactly_the_channel_post_tool() {
        let out = run(&[req(2, "tools/list", json!({}))], &Fake::ok());
        let tools = out[0]["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        let t = &tools[0];
        assert_eq!(t["name"], "channel_post");
        assert!(t["description"].as_str().unwrap().contains("30 minutes"));
        let schema = &t["inputSchema"];
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["channel", "text"]));
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"].as_object().unwrap().len(), 2);
        assert_eq!(schema["properties"]["channel"]["type"], "string");
        assert_eq!(schema["properties"]["text"]["type"], "string");
        assert!(schema["properties"]["channel"]["description"].is_string());
        assert!(schema["properties"]["text"]["description"].is_string());
    }

    #[test]
    fn fr_t12_mcp_call_posts_once_and_returns_the_message_id() {
        let p = Fake::ok();
        let out = run(&[call(3, json!({ "channel": "c1-x.y_z", "text": "looks fine" }))], &p);
        assert_eq!(*p.calls.borrow(), vec![("c1-x.y_z".to_string(), "looks fine".to_string())]);
        assert!(!tool_error(&out[0]), "{}", out[0]);
        assert_eq!(out[0]["result"]["content"][0]["type"], "text");
        assert_eq!(out[0]["result"]["content"][0]["text"], "Posted message m-1 in channel c1-x.y_z.");
    }

    #[test]
    fn fr_t12_mcp_text_is_posted_as_given_and_the_limit_is_inclusive() {
        let p = Fake::ok();
        let at_limit = "a".repeat(MAX_TEXT_BYTES);
        let out = run(&[call(1, json!({ "channel": "c", "text": at_limit }))], &p);
        assert!(!tool_error(&out[0]));
        let out = run(&[call(1, json!({ "channel": "c", "text": "  hi\n" }))], &p);
        assert!(!tool_error(&out[0]));
        assert_eq!(p.calls.borrow()[1].1, "  hi\n");
    }

    #[test]
    fn fr_t12_mcp_refusal_is_a_tool_error_and_the_loop_continues() {
        let p = Fake::refusing("The post was refused (HTTP 403): no recent mention");
        let out = run(&[call(1, json!({ "channel": "c", "text": "x" })), req(2, "ping", json!(null))], &p);
        assert_eq!(out.len(), 2);
        assert!(tool_error(&out[0]));
        assert_eq!(out[0]["result"]["content"][0]["text"], "The post was refused (HTTP 403): no recent mention");
        assert!(out[0].get("error").is_none());
        assert_eq!(out[1]["id"], 2);
        assert_eq!(out[1]["result"], json!({}));
    }

    #[test]
    fn fr_t12_mcp_bad_arguments_are_tool_errors_and_never_post() {
        let long_channel = "c".repeat(MAX_CHANNEL_ID + 1);
        let long_text = "a".repeat(MAX_TEXT_BYTES + 1);
        let bad = [
            json!({ "channel": "c" }),
            json!({ "text": "hi" }),
            json!({}),
            json!({ "channel": 1, "text": "hi" }),
            json!({ "channel": "c", "text": 5 }),
            json!({ "channel": null, "text": "hi" }),
            json!({ "channel": "c", "text": "hi", "extra": true }),
            json!({ "channel": "c", "text": "   \n\t" }),
            json!({ "channel": "c", "text": "" }),
            json!({ "channel": "c", "text": long_text }),
            json!({ "channel": "a/b", "text": "hi" }),
            json!({ "channel": "a b", "text": "hi" }),
            json!({ "channel": "..", "text": "hi" }),
            json!({ "channel": ".", "text": "hi" }),
            json!({ "channel": "", "text": "hi" }),
            json!({ "channel": "a%2Fb", "text": "hi" }),
            json!({ "channel": "a?b", "text": "hi" }),
            json!({ "channel": long_channel, "text": "hi" }),
            json!("not an object"),
            json!(null),
        ];
        let p = Fake::ok();
        for args in bad {
            let out = run(&[call(1, args.clone())], &p);
            assert_eq!(out.len(), 1);
            assert!(tool_error(&out[0]), "{args}: {}", out[0]);
            assert!(out[0].get("error").is_none(), "{args}");
        }
        let out = run(&[req(1, "tools/call", json!({ "name": "channel_post" }))], &p);
        assert!(tool_error(&out[0]), "missing arguments");
        assert!(p.calls.borrow().is_empty(), "the poster was never called");
    }

    #[test]
    fn fr_t12_mcp_unknown_tool_is_invalid_params() {
        let p = Fake::ok();
        let out = run(
            &[
                req(1, "tools/call", json!({ "name": "other", "arguments": {} })),
                req(2, "tools/call", json!({})),
                req(3, "tools/call", json!("x")),
            ],
            &p,
        );
        for o in &out {
            assert_eq!(o["error"]["code"], -32602, "{o}");
        }
        assert!(p.calls.borrow().is_empty());
    }

    #[test]
    fn fr_t12_mcp_other_requests_are_method_not_found_and_the_loop_continues() {
        let p = Fake::ok();
        for method in [
            "server/discover",
            "resources/list",
            "resources/templates/list",
            "prompts/list",
            "logging/setLevel",
            "completion/complete",
        ] {
            let out = run(&[req(1, method, json!({})), req(2, "ping", json!(null))], &p);
            assert_eq!(out.len(), 2, "{method}");
            assert_eq!(out[0]["error"]["code"], -32601, "{method}");
            assert_eq!(out[0]["id"], 1);
            assert_eq!(out[1]["result"], json!({}), "{method}: the server went on");
        }
    }

    #[test]
    fn fr_t12_mcp_claude_code_opening_is_served() {
        let p = Fake::ok();
        let out = run(
            &[
                req(0, "server/discover", json!({ "_meta": { "protocolVersion": "2026-07-28" } })),
                req(1, "initialize", json!({ "protocolVersion": "2025-11-25", "capabilities": { "roots": { "listChanged": true }, "elicitation": {} }, "clientInfo": { "name": "claude-code", "version": "2.1.319" } })),
                note("notifications/initialized"),
                req(2, "tools/list", json!({})),
            ],
            &p,
        );
        assert_eq!(out.len(), 3);
        assert_eq!(out[0]["error"]["code"], -32601);
        assert_eq!(out[1]["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(out[2]["result"]["tools"][0]["name"], "channel_post");
    }

    #[test]
    fn fr_t12_mcp_codex_opening_is_served() {
        let p = Fake::ok();
        let out = run(
            &[
                req(0, "initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": { "elicitation": {} }, "clientInfo": { "name": "codex-mcp-client", "version": "0.164.0" } })),
                note("notifications/initialized"),
                req(1, "tools/list", json!({ "_meta": { "progressToken": 0 } })),
                req(2, "resources/templates/list", json!({})),
                req(3, "resources/list", json!({})),
            ],
            &p,
        );
        assert_eq!(out.len(), 4);
        assert_eq!(out[0]["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(out[1]["result"]["tools"].as_array().unwrap().len(), 1);
        assert_eq!(out[2]["error"]["code"], -32601);
        assert_eq!(out[3]["error"]["code"], -32601);
    }

    #[test]
    fn fr_t12_mcp_malformed_json_is_a_parse_error_and_the_loop_continues() {
        let input = b"{not json\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n".to_vec();
        let out = run_bytes(input, &Fake::ok());
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["error"]["code"], -32700);
        assert_eq!(out[0]["id"], Value::Null);
        assert_eq!(out[1]["result"], json!({}));
    }

    #[test]
    fn fr_t12_mcp_non_request_frames_are_invalid_requests() {
        let input = b"[1,2]\n42\n\"s\"\n{\"jsonrpc\":\"2.0\",\"id\":3}\n{\"id\":1,\"method\":5}\n{\"id\":[1],\"method\":\"ping\"}\n".to_vec();
        let out = run_bytes(input, &Fake::ok());
        assert_eq!(out.len(), 6);
        for o in &out {
            assert_eq!(o["error"]["code"], -32600, "{o}");
            assert_eq!(o["id"], Value::Null);
        }
    }

    #[test]
    fn fr_t12_mcp_an_over_long_line_is_discarded_and_the_next_is_handled() {
        let mut input = vec![b'a'; MAX_LINE_BYTES + 1];
        input.push(b'\n');
        input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n");
        let out = run_bytes(input, &Fake::ok());
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["error"]["code"], -32600);
        assert_eq!(out[0]["id"], Value::Null);
        assert_eq!(out[1]["id"], 1);
    }

    #[test]
    fn fr_t12_mcp_a_line_of_exactly_the_cap_is_read_and_an_over_long_last_line_is_refused() {
        // Exactly the cap: read (and rejected as malformed JSON, not as too large).
        let mut input = vec![b'a'; MAX_LINE_BYTES];
        input.push(b'\n');
        let out = run_bytes(input, &Fake::ok());
        assert_eq!(out[0]["error"]["code"], -32700);
        // Over the cap and no newline before EOF.
        let out = run_bytes(vec![b'a'; MAX_LINE_BYTES + 5], &Fake::ok());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["error"]["code"], -32600);
    }

    #[test]
    fn fr_t12_mcp_the_cap_is_not_buffered() {
        // A reader with a small buffer: the frame reader must not collect the over-long line.
        let mut data = vec![b'a'; MAX_LINE_BYTES * 3];
        data.push(b'\n');
        let mut reader = io::BufReader::with_capacity(4096, Cursor::new(data));
        let mut out = Vec::new();
        assert!(matches!(read_frame(&mut reader, &mut out).unwrap(), Frame::TooLong));
        assert!(out.len() <= MAX_LINE_BYTES);
        assert!(matches!(read_frame(&mut reader, &mut out).unwrap(), Frame::Eof));
    }

    #[test]
    fn fr_t12_mcp_empty_lines_are_ignored_and_eof_ends_with_ok() {
        let input = b"\n\r\n   \n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\r\n\n".to_vec();
        let out = run_bytes(input, &Fake::ok());
        assert_eq!(out.len(), 1);
        assert!(run_bytes(vec![], &Fake::ok()).is_empty());
        // A final frame without a newline is still handled.
        let out = run_bytes(b"{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"ping\"}".to_vec(), &Fake::ok());
        assert_eq!(out[0]["id"], 9);
    }

    #[test]
    fn fr_t12_mcp_secrets_never_reach_the_output() {
        const CANARY: &str = "TOKEN-CANARY-123";
        // A failure whose chain holds the canary maps to a fixed reason.
        let chain = anyhow::anyhow!("Authorization: Bearer {CANARY}").context("connecting to http://host");
        let refusal = refusal_from_failure(&chain);
        assert!(!refusal.reason.contains(CANARY) && !refusal.reason.contains("http"), "{}", refusal.reason);
        // A refusal body with the canary in a field other than `error`.
        let body = json!({ "detail": CANARY, "token": CANARY, "code": "forbidden" });
        let refusal = refusal_from_response(403, &body);
        assert_eq!(refusal.reason, "The post was refused (HTTP 403).");
        // A body that lacks the server's shape is not trusted even in `error`.
        let refusal = refusal_from_response(502, &json!({ "error": format!("<html>{CANARY}</html>") }));
        assert_eq!(refusal.reason, "The post was refused (HTTP 502).");
        // Through the protocol.
        let p = Fake { calls: RefCell::new(vec![]), answer: Err(refusal_from_failure(&chain)) };
        let mut out = Vec::new();
        let frames = format!(
            "{}\n{}\n",
            call(1, json!({ "channel": "c", "text": "hello" })),
            req(2, "tools/list", json!({}))
        );
        serve(Cursor::new(frames.into_bytes()), &mut out, &p).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains(CANARY));
    }

    #[test]
    fn fr_t12_mcp_refusal_from_the_servers_own_error_is_capped_and_clean() {
        let r = refusal_from_response(403, &json!({ "error": "not a member\nof this channel", "code": "forbidden" }));
        assert_eq!(r.reason, "The post was refused (HTTP 403): not a memberof this channel");
        let long = "x".repeat(500);
        let r = refusal_from_response(429, &json!({ "error": long, "code": "rate_limited" }));
        assert_eq!(r.reason.chars().count(), "The post was refused (HTTP 429): ".len() + 200);
        let r = refusal_from_response(500, &Value::Null);
        assert_eq!(r.reason, "The post was refused (HTTP 500).");
    }

    #[test]
    fn fr_t12_mcp_missing_environment_is_a_tool_error_naming_the_agent_session() {
        // The only path that reads the process environment is EnvPoster; the unset case is
        // pinned end to end by the integration test. Here, the constant the model reads.
        assert!(NOT_IN_SESSION.contains("only works inside an Ember agent session"));
        assert!(!NOT_IN_SESSION.contains("http://"));
    }

    #[test]
    fn fr_t12_mcp_valid_channel_ids() {
        for ok in ["c", "6f1d2c1e-8f0a-4c52-9a43-2b1f0b7f9d11", "default-core", "default-my_proj.v2", &"a".repeat(128)] {
            assert!(valid_channel(ok), "{ok}");
        }
        for bad in ["", ".", "..", "...", "a/b", "a b", "a\n", "é", &"a".repeat(129)] {
            assert!(!valid_channel(bad), "{bad:?}");
        }
    }
}
