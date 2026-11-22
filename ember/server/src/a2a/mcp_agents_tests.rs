//! Unit tests of the `ember` MCP server (FR-T2, #1045) against a fake A2A API.

use std::cell::RefCell;
use std::io::Cursor;

use serde_json::{json, Value};

use super::mcp_agents::*;

type Sent = (String, String, Option<String>);

struct Fake {
    targets: Value,
    sent: RefCell<Vec<Sent>>,
    listed: RefCell<usize>,
    refuse: Option<Refusal>,
}

impl Fake {
    fn new() -> Fake {
        Fake {
            targets: json!([
                { "id": "s-beta", "title": "beta", "project": "acme", "agent": "codex", "status": "idle" },
                { "id": "s-gamma", "title": "reviewer", "project": "acme", "agent": "claude-code", "status": "running" },
                { "id": "s-delta", "title": "twin", "project": "acme", "agent": "codex", "status": "idle" },
                { "id": "s-eps", "title": "twin", "project": "acme", "agent": "codex", "status": "idle" },
            ]),
            sent: RefCell::new(Vec::new()),
            listed: RefCell::new(0),
            refuse: None,
        }
    }

    fn refusing(reason: &str) -> Fake {
        Fake { refuse: Some(Refusal::new(reason)), ..Fake::new() }
    }
}

impl AgentsApi for Fake {
    fn targets(&self) -> Result<Value, Refusal> {
        *self.listed.borrow_mut() += 1;
        match &self.refuse {
            Some(r) => Err(r.clone()),
            None => Ok(self.targets.clone()),
        }
    }

    fn send(&self, to: &str, message: &str, reply_to: Option<&str>) -> Result<Value, Refusal> {
        if let Some(r) = &self.refuse {
            return Err(r.clone());
        }
        self.sent.borrow_mut().push((to.into(), message.into(), reply_to.map(String::from)));
        Ok(json!({ "id": "m-1", "to": to, "status": "delivered" }))
    }
}

fn run(frames: &[Value], api: &dyn AgentsApi) -> Vec<Value> {
    let mut input = Vec::new();
    for f in frames {
        input.extend_from_slice(f.to_string().as_bytes());
        input.push(b'\n');
    }
    let mut out = Vec::new();
    serve(Cursor::new(input), &mut out, api).unwrap();
    String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn req(id: i64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

fn call(id: i64, tool: &str, args: Value) -> Value {
    req(id, "tools/call", json!({ "name": tool, "arguments": args }))
}

fn tools() -> Vec<Value> {
    let out = run(&[req(1, "tools/list", json!({}))], &Fake::new());
    out[0]["result"]["tools"].as_array().unwrap().clone()
}

fn text(v: &Value) -> String {
    v["result"]["content"][0]["text"].as_str().unwrap().to_string()
}

fn is_error(v: &Value) -> bool {
    v["result"]["isError"] == true
}

#[test]
fn fr_t2_mcp_tools_list_has_list_agents_and_send_message_in_the_native_shapes() {
    let tools = tools();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, [LIST_AGENTS, SEND_MESSAGE]);
    assert_eq!((LIST_AGENTS, SEND_MESSAGE), ("list_agents", "send_message"));
    let list = &tools[0]["inputSchema"];
    assert_eq!(list["type"], "object");
    assert_eq!(list["properties"], json!({}));
    assert!(list.get("required").is_none(), "list_agents takes no arguments");
    let send = &tools[1]["inputSchema"];
    let mut props: Vec<&String> = send["properties"].as_object().unwrap().keys().collect();
    props.sort();
    assert_eq!(props, ["message", "reply_to", "to"], "the native names `to` and `message`");
    assert_eq!(send["required"], json!(["to", "message"]));
    assert_eq!(send["additionalProperties"], false);
    for t in &tools {
        assert_eq!(t["_meta"]["anthropic/alwaysLoad"], true, "{t}");
    }
}

/// The tools win by name and description alone (D5): the descriptions open with the user's verbs
/// and say they reach every Ember session, and they stay under Claude Code's 4,096-character cut.
#[test]
fn fr_t2_mcp_descriptions_open_with_the_users_verbs_and_name_every_session() {
    let tools = tools();
    let list = tools[0]["description"].as_str().unwrap();
    let send = tools[1]["description"].as_str().unwrap();
    assert!(list.starts_with("List the other agents"), "{list}");
    assert!(send.starts_with("Ask, consult or tell another agent"), "{send}");
    for d in [list, send] {
        assert!(d.contains("any computer") && d.contains("any model"), "{d}");
        assert!(d.len() < 1024, "{d}");
    }
}

#[test]
fn fr_t2_mcp_initialize_negotiates_and_carries_the_server_instructions() {
    for (asked, want) in [("2025-11-25", "2025-11-25"), ("2025-06-18", "2025-06-18"), ("1999-01-01", "2025-11-25")] {
        let out = run(&[req(1, "initialize", json!({ "protocolVersion": asked }))], &Fake::new());
        let r = &out[0]["result"];
        assert_eq!(r["protocolVersion"], want);
        assert_eq!(r["serverInfo"]["name"], "ember");
        assert!(r["capabilities"]["tools"].is_object());
        let instructions = r["instructions"].as_str().unwrap();
        assert_eq!(instructions, INSTRUCTIONS);
    }
    assert!(INSTRUCTIONS.contains("every Ember session") && INSTRUCTIONS.len() < 4096);
}

#[test]
fn fr_t2_mcp_notifications_get_no_answer_and_ping_answers() {
    let out = run(
        &[json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }), req(7, "ping", json!({}))],
        &Fake::new(),
    );
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["id"], 7);
    assert_eq!(out[0]["result"], json!({}));
}

#[test]
fn fr_t2_mcp_list_agents_shows_each_session() {
    let fake = Fake::new();
    let out = run(&[call(1, LIST_AGENTS, json!({}))], &fake);
    assert!(!is_error(&out[0]));
    let t = text(&out[0]);
    assert!(t.contains("s-beta  codex  idle  project=acme  beta"), "{t}");
    assert!(t.contains("s-gamma  claude-code  running  project=acme  reviewer"), "{t}");
    // Missing arguments are the same as `{}`.
    let out = run(&[req(2, "tools/call", json!({ "name": LIST_AGENTS }))], &fake);
    assert!(!is_error(&out[0]), "{}", out[0]);
    let empty = Fake { targets: json!([]), ..Fake::new() };
    let out = run(&[call(3, LIST_AGENTS, json!({}))], &empty);
    assert_eq!(text(&out[0]), "No other sessions to message.");
}

#[test]
fn fr_t2_mcp_send_message_passes_to_message_and_reply_to() {
    let fake = Fake::new();
    let out = run(
        &[
            call(1, SEND_MESSAGE, json!({ "to": "s-beta", "message": "hello" })),
            call(2, SEND_MESSAGE, json!({ "to": "s-beta", "message": "the answer", "reply_to": "m-0" })),
        ],
        &fake,
    );
    assert_eq!(text(&out[0]), "Sent message m-1 to s-beta: delivered.");
    assert!(!is_error(&out[1]));
    assert_eq!(
        *fake.sent.borrow(),
        [("s-beta".into(), "hello".into(), None), ("s-beta".into(), "the answer".into(), Some("m-0".into()))]
    );
}

/// Claude Code's `SendMessage` addresses a peer by name: a unique session title (any letter
/// case) is taken for its id; an ambiguous or unknown name goes to the server as given.
#[test]
fn fr_t2_mcp_send_message_resolves_a_unique_title() {
    let fake = Fake::new();
    run(
        &[
            call(1, SEND_MESSAGE, json!({ "to": "Reviewer", "message": "a" })),
            call(2, SEND_MESSAGE, json!({ "to": "twin", "message": "b" })),
            call(3, SEND_MESSAGE, json!({ "to": "nobody", "message": "c" })),
            call(4, SEND_MESSAGE, json!({ "to": "s-beta", "message": "d" })),
        ],
        &fake,
    );
    let to: Vec<String> = fake.sent.borrow().iter().map(|s| s.0.clone()).collect();
    assert_eq!(to, ["s-gamma", "twin", "nobody", "s-beta"]);
    assert_eq!(resolve_target("REVIEWER", &fake.targets), "s-gamma");
    assert_eq!(resolve_target("s-eps", &fake.targets), "s-eps");
}

#[test]
fn fr_t2_mcp_bad_arguments_are_tool_errors_and_never_send() {
    let fake = Fake::new();
    let long = "x".repeat(MAX_MESSAGE_BYTES + 1);
    let cases = [
        json!(null),
        json!("s-beta hello"),
        json!({ "message": "no target" }),
        json!({ "to": "s-beta" }),
        json!({ "to": "", "message": "x" }),
        json!({ "to": "s-beta", "message": "   " }),
        json!({ "to": 5, "message": "x" }),
        json!({ "to": "s-beta", "message": "x", "reply_to": 3 }),
        json!({ "to": "s-beta", "message": "x", "cc": "s-gamma" }),
        json!({ "to": "s-beta", "message": long }),
    ];
    for (i, args) in cases.iter().enumerate() {
        let out = run(&[call(i as i64, SEND_MESSAGE, args.clone())], &fake);
        assert!(is_error(&out[0]), "{args}: {}", out[0]);
        assert!(out[0].get("error").is_none());
    }
    assert!(fake.sent.borrow().is_empty());
    // At the cap is fine.
    let out = run(&[call(99, SEND_MESSAGE, json!({ "to": "s-beta", "message": "y".repeat(MAX_MESSAGE_BYTES) }))], &fake);
    assert!(!is_error(&out[0]), "{}", out[0]);
    // list_agents takes nothing.
    let out = run(&[call(100, LIST_AGENTS, json!({ "all": true }))], &fake);
    assert!(is_error(&out[0]));
}

#[test]
fn fr_t2_mcp_unknown_tool_is_invalid_params_and_unknown_methods_are_not_found() {
    let out = run(
        &[call(1, "channel_post", json!({})), req(2, "resources/list", json!({})), req(3, "ping", json!({}))],
        &Fake::new(),
    );
    assert_eq!(out[0]["error"]["code"], -32602);
    assert_eq!(out[1]["error"]["code"], -32601);
    assert_eq!(out[2]["result"], json!({}));
}

#[test]
fn fr_t2_mcp_a_refusal_is_a_tool_error_and_the_loop_continues() {
    let fake = Fake::refusing("The message was not sent: too many messages.");
    let out = run(
        &[call(1, SEND_MESSAGE, json!({ "to": "s-beta", "message": "x" })), call(2, LIST_AGENTS, json!({})), req(3, "ping", json!({}))],
        &fake,
    );
    assert!(is_error(&out[0]) && is_error(&out[1]));
    assert_eq!(text(&out[0]), "The message was not sent: too many messages.");
    assert_eq!(out[2]["result"], json!({}));
}

/// The server's own `error` text is passed on only under one of the A2A API's codes, cut to a
/// bounded length; anything else (a proxy page, an unknown code) gets a sentence by status.
#[test]
fn fr_t2_mcp_refusal_text_comes_from_known_codes_only() {
    let r = refusal_from_response(404, &json!({ "error": "session s-x not found", "code": "not_found" }));
    assert_eq!(r.reason, "The message was not sent: session s-x not found");
    let r = refusal_from_response(429, &json!({ "error": "slow down", "code": "rate_limited" }));
    assert_eq!(r.reason, "The message was not sent: slow down");
    let r = refusal_from_response(502, &json!({ "error": "<html>proxy</html>", "code": "gateway" }));
    assert!(!r.reason.contains("proxy"), "{}", r.reason);
    assert!(r.reason.starts_with("The message was not sent"), "{}", r.reason);
    let r = refusal_from_response(401, &Value::Null);
    assert!(r.reason.contains("runtime token"), "{}", r.reason);
    let long = "e".repeat(5000);
    let r = refusal_from_response(400, &json!({ "error": long, "code": "bad_request" }));
    assert!(r.reason.len() <= 600, "{}", r.reason.len());
}

#[test]
fn fr_t2_mcp_without_the_environment_the_tools_say_so() {
    let api = HttpAgents::from_parts(None, None);
    let out = run(&[call(1, LIST_AGENTS, json!({})), call(2, SEND_MESSAGE, json!({ "to": "a", "message": "b" }))], &api);
    for v in &out {
        assert!(is_error(v));
        assert!(text(v).contains("only works inside an Ember agent session"), "{v}");
    }
}
