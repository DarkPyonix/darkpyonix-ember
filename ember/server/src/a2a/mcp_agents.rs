//! `ember-a2a mcp agents`: the `ember` MCP server with `list_agents` and `send_message` (SPEC
//! FR-T2, INTENT D5, `docs/design/A2A-NATIVE.md` direction (B), #1045).
//!
//! The agent reaches Ember A2A through tools of its own kind instead of a shell command: the
//! tools carry the native argument names (`to`, `message`, as Claude Code's `SendMessage` and
//! Codex's `send_message`), their descriptions open with the user's verbs, and every tool asks
//! Claude Code to load it upfront (`_meta["anthropic/alwaysLoad"]`, code.claude.com/docs/en/mcp,
//! "Mark a tool to load upfront or stay deferred", read 2026-11-22). The built-in peer tools stay
//! enabled; these tools win by name, description and loading alone.
//!
//! The protocol is the one `ember-a2a mcp` speaks (newline-delimited JSON-RPC 2.0, hand-written,
//! framing shared with [`super::mcp`]). The tools call the same routes as `ember-a2a list` and
//! `ember-a2a send` with the session's runtime token; authorization, team scope and loop
//! protection stay on the server. Diagnostics go to stderr and never contain the token.

use std::io::{self, BufRead, Write};

use serde_json::{json, Value};

use super::client::{format_targets, receipt_text, Client};
use super::mcp::{
    error_response, non_loopback_host, non_loopback_warning, ok_response, read_frame, tool_result, write_frame,
    Frame, SUPPORTED_VERSIONS,
};
use super::A2A_MCP_SERVER;

/// The tool that lists the sessions the caller may message.
pub const LIST_AGENTS: &str = "list_agents";
/// The tool that sends one message.
pub const SEND_MESSAGE: &str = "send_message";
/// Longest `message`. The A2A route has no cap of its own; one frame is at most
/// [`super::mcp::MAX_LINE_BYTES`].
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// Longest server `error` text passed on to the model.
const MAX_ERROR_CHARS: usize = 500;

/// The server instructions, sent in `initialize`. Claude Code shows them to the model even when
/// tools are deferred (code.claude.com/docs/en/mcp, "Tool search for MCP server authors").
pub const INSTRUCTIONS: &str = "Ember runs several agent sessions for the user (Claude Code, Codex and others, on any \
computer and with any model). Use list_agents to see them and send_message to ask, consult or tell one of them. \
These tools reach every Ember session; built-in agent tools reach only your own sub-agents. A message reaches the \
other session as a user message; replies come back to you the same way, so do not wait or poll.";

const NOT_IN_SESSION: &str = "This tool only works inside an Ember agent session (EMBER_URL and EMBER_RUNTIME_TOKEN are not set).";
const BAD_URL: &str = "The ember tools cannot reach the Ember server: EMBER_URL is not a plain http://host[:port] address.";
const UNREACHABLE: &str = "Could not reach the Ember server.";

/// Why a call did not happen. `reason` is short and safe to show to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub reason: String,
}

impl Refusal {
    pub fn new(reason: impl Into<String>) -> Refusal {
        Refusal { reason: reason.into() }
    }
}

/// The A2A API the tools call; the real one is [`HttpAgents`], tests inject a fake.
pub trait AgentsApi {
    /// `GET /api/a2a/targets`: an array of `{id, title, project, agent, status}`.
    fn targets(&self) -> Result<Value, Refusal>;
    /// `POST /api/a2a/messages`: the receipt `{id, to, status}`.
    fn send(&self, to: &str, message: &str, reply_to: Option<&str>) -> Result<Value, Refusal>;
}

/// The A2A API over HTTP with a session's runtime token. Without one, every call is refused with
/// a sentence saying so, and `initialize` and `tools/list` still work.
pub struct HttpAgents {
    base: Option<(String, String)>,
}

impl HttpAgents {
    pub fn new(url: &str, token: &str) -> HttpAgents {
        HttpAgents::from_parts(Some(url.to_string()), Some(token.to_string()))
    }

    /// `None` or an empty value for either part means "not in an Ember session".
    pub fn from_parts(url: Option<String>, token: Option<String>) -> HttpAgents {
        let base = match (url.filter(|u| !u.is_empty()), token.filter(|t| !t.is_empty())) {
            (Some(u), Some(t)) => Some((u, t)),
            _ => None,
        };
        HttpAgents { base }
    }

    pub fn from_env() -> HttpAgents {
        HttpAgents::from_parts(std::env::var("EMBER_URL").ok(), std::env::var("EMBER_RUNTIME_TOKEN").ok())
    }

    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, Refusal> {
        let Some((url, token)) = &self.base else { return Err(Refusal::new(NOT_IN_SESSION)) };
        let client = Client::new(url, token).map_err(|_| Refusal::new(BAD_URL))?;
        // The error text is dropped on purpose: it may carry the URL.
        let (status, v) = client.request(method, path, body).map_err(|_| Refusal::new(UNREACHABLE))?;
        if (200..300).contains(&status) {
            Ok(v)
        } else {
            Err(refusal_from_response(status, &v))
        }
    }
}

impl AgentsApi for HttpAgents {
    fn targets(&self) -> Result<Value, Refusal> {
        self.call("GET", "/api/a2a/targets", None)
    }

    fn send(&self, to: &str, message: &str, reply_to: Option<&str>) -> Result<Value, Refusal> {
        let body = json!({ "to": to, "text": message, "reply_to": reply_to });
        self.call("POST", "/api/a2a/messages", Some(&body))
    }
}

/// The refusal for a non-2xx answer. The server's `error` text is passed on (cut to
/// [`MAX_ERROR_CHARS`]) only under one of the codes the A2A API returns (`A2aError` in
/// `api.rs`), because only then is it Ember's own sentence (a missing session, a rate limit, a
/// team rule). Anything else, a proxy page included, gets a fixed sentence by status.
pub fn refusal_from_response(status: u16, body: &Value) -> Refusal {
    const CODES: [&str; 7] = ["unauthorized", "disabled", "rate_limited", "not_found", "bad_request", "forbidden", "internal"];
    let code = body.get("code").and_then(Value::as_str).filter(|c| CODES.contains(c));
    let error = body.get("error").and_then(Value::as_str).map(str::trim).filter(|e| !e.is_empty());
    if let (Some(_), Some(error)) = (code, error) {
        let cut: String = error.chars().take(MAX_ERROR_CHARS).collect();
        return Refusal::new(format!("The message was not sent: {cut}"));
    }
    Refusal::new(match status {
        401 => "The message was not sent: the runtime token was not accepted, so this session cannot use Ember A2A.",
        400 => "The message was not sent: the server rejected the request as malformed.",
        403 => "The message was not sent: this session may not message that session.",
        404 => "The message was not sent: no such session. Use list_agents for the ids.",
        429 => "The message was not sent: too many messages. Wait, then send once.",
        500..=599 => "The message was not sent: the Ember server had an error. Try again later.",
        _ => "The message was not sent: the Ember server refused it.",
    })
}

/// The session id for `to`: `to` itself when it is a target's id, else the id of the one target
/// whose title equals `to` ignoring letter case (Claude Code's `SendMessage` addresses a peer by
/// name), else `to` as given, so the server answers an unknown or ambiguous name.
pub fn resolve_target(to: &str, targets: &Value) -> String {
    let rows = targets.as_array().map(Vec::as_slice).unwrap_or_default();
    let field = |r: &Value, k: &str| r.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    if rows.iter().any(|r| field(r, "id") == to) {
        return to.to_string();
    }
    let mut named = rows.iter().filter(|r| field(r, "title").eq_ignore_ascii_case(to));
    match (named.next(), named.next()) {
        (Some(one), None) => field(one, "id"),
        _ => to.to_string(),
    }
}

/// Entry point of `ember-a2a mcp agents`; returns the process exit code.
pub fn run_stdio() -> i32 {
    if let Some(host) = std::env::var("EMBER_URL").ok().as_deref().and_then(non_loopback_host) {
        eprintln!("{}", non_loopback_warning(&host));
    }
    let stdin = io::stdin();
    let stdout = io::stdout();
    match serve(stdin.lock(), stdout.lock(), &HttpAgents::from_env()) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("ember-a2a mcp agents: i/o error ({:?})", e.kind());
            1
        }
    }
}

/// Handle frames from `input` until EOF, one at a time, answering on `output`.
pub fn serve<R: BufRead, W: Write>(mut input: R, mut output: W, api: &dyn AgentsApi) -> io::Result<()> {
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
                if let Some(answer) = handle(&line, api) {
                    write_frame(&mut output, &answer)?;
                }
            }
        }
    }
}

/// The answer to one line, or `None` for a notification.
fn handle(line: &[u8], api: &dyn AgentsApi) -> Option<Value> {
    let Ok(frame) = serde_json::from_slice::<Value>(line) else {
        return Some(error_response(&Value::Null, -32700, "parse error"));
    };
    let Some(method) = frame.as_object().and_then(|o| o.get("method")).and_then(Value::as_str) else {
        return Some(error_response(&Value::Null, -32600, "not a JSON-RPC request"));
    };
    let id = frame.get("id")?; // a notification gets no answer, known or not
    if !(id.is_string() || id.is_number() || id.is_null()) {
        return Some(error_response(&Value::Null, -32600, "invalid request id"));
    }
    let null = Value::Null;
    let params = frame.get("params").unwrap_or(&null);
    let answer = match method {
        "initialize" => Ok(initialize_result(params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => call_tool(params, api),
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
        "serverInfo": { "name": A2A_MCP_SERVER, "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

fn tool_definitions() -> Value {
    let upfront = json!({ "anthropic/alwaysLoad": true });
    json!([
        {
            "name": LIST_AGENTS,
            "description": "List the other agents you can ask, consult or message: every agent session Ember runs for the user (Claude Code, Codex and others), on any computer and with any model. Shows each session's id, agent, status, project and title; use the id or the title with send_message.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "_meta": upfront,
        },
        {
            "name": SEND_MESSAGE,
            "description": "Ask, consult or tell another agent: send a message to any agent session Ember runs for the user (Claude Code, Codex and others), on any computer and with any model. It arrives as a user message; a busy session gets it when its current turn ends and a sleeping one is woken. Its answer comes back to you the same way.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to": {
                        "type": "string",
                        "description": "The session to message: its id or its title from list_agents, or the sender session id from a message you received."
                    },
                    "message": {
                        "type": "string",
                        "description": "The message, written for that agent: say what you need and include the context it lacks."
                    },
                    "reply_to": {
                        "type": "string",
                        "description": "Optional: the id of a message you received, when this message answers it."
                    }
                },
                "required": ["to", "message"],
                "additionalProperties": false
            },
            "_meta": upfront,
        }
    ])
}

fn call_tool(params: &Value, api: &dyn AgentsApi) -> Result<Value, (i64, &'static str)> {
    let Some(p) = params.as_object() else {
        return Err((-32602, "invalid params"));
    };
    let null = Value::Null;
    let arguments = p.get("arguments").unwrap_or(&null);
    let outcome = match p.get("name").and_then(Value::as_str) {
        Some(LIST_AGENTS) => list_agents(arguments, api),
        Some(SEND_MESSAGE) => send_message(arguments, api),
        _ => return Err((-32602, "unknown tool")),
    };
    Ok(match outcome {
        Ok(text) => tool_result(&text, false),
        Err(reason) => tool_result(&reason, true),
    })
}

fn list_agents(arguments: &Value, api: &dyn AgentsApi) -> Result<String, String> {
    let empty = arguments.is_null() || arguments.as_object().is_some_and(|o| o.is_empty());
    if !empty {
        return Err("list_agents takes no arguments.".into());
    }
    api.targets().map(|v| format_targets(&v)).map_err(|r| r.reason)
}

/// The checked `(to, message, reply_to)` of a `send_message` call.
fn send_arguments(arguments: &Value) -> Result<(&str, &str, Option<&str>), &'static str> {
    let Some(args) = arguments.as_object() else {
        return Err("arguments must be an object with to and message.");
    };
    if args.keys().any(|k| !matches!(k.as_str(), "to" | "message" | "reply_to")) {
        return Err("only the arguments to, message and reply_to are accepted.");
    }
    let Some(to) = args.get("to").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty()) else {
        return Err("to is required: a session id or title from list_agents.");
    };
    let Some(message) = args.get("message").and_then(Value::as_str) else {
        return Err("message is required and must be a string.");
    };
    if message.trim().is_empty() {
        return Err("message must not be empty.");
    }
    if message.len() > MAX_MESSAGE_BYTES {
        return Err("message is too long (at most 65536 bytes).");
    }
    let reply_to = match args.get("reply_to") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim()),
        Some(_) => return Err("reply_to must be the id of a message you received."),
    };
    Ok((to, message, reply_to))
}

fn send_message(arguments: &Value, api: &dyn AgentsApi) -> Result<String, String> {
    let (to, message, reply_to) = send_arguments(arguments).map_err(String::from)?;
    // A title is resolved against the caller's own targets; if they cannot be read, the server
    // judges `to` as given.
    let to = match api.targets() {
        Ok(targets) => resolve_target(to, &targets),
        Err(_) => to.to_string(),
    };
    let receipt = api.send(&to, message, reply_to).map_err(|r| r.reason)?;
    Ok(receipt_text(&to, &receipt))
}
