//! The `ember` MCP server (`ember-a2a mcp agents`) lists sessions and sends A2A messages over
//! stdio (SPEC FR-T2, #1045).
//!
//! An in-process Ember server (sessions, A2A and its API) listens on loopback. The MCP server is
//! driven with the opening Claude Code or Codex sends, once in process ([`mcp_agents::serve`]
//! with an [`HttpAgents`] holding a session's runtime token) and once as the real binary. A
//! message is checked where it lands: as a user message in the target session's transcript.

use std::io::{Cursor, Write};
use std::net::SocketAddr;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use ember_server::a2a::mcp_agents::{self, HttpAgents};
use ember_server::a2a::{A2a, A2aConfig, A2aStore};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::events::AgentEvent;
use ember_server::session::{NewSession, Sessions};
use ember_server::store::Store;
use serde_json::{json, Value};

struct World {
    sessions: Arc<Sessions>,
    a2a: Arc<A2a>,
    url: String,
    alpha: String,
    beta: String,
}

fn new_session(s: &Sessions, title: &str) -> String {
    s.create(NewSession {
        project: "acme".into(),
        agent: AgentKind::Scripted,
        cwd: std::env::temp_dir(),
        model: None,
        title: title.into(),
    })
    .unwrap()
    .id
}

/// Two scripted sessions, `alpha` (the sender) and `beta`, behind a real HTTP listener.
async fn world() -> World {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store, vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let a2a = A2a::new(sessions.clone(), A2aStore::open_in_memory().unwrap(), A2aConfig::new("http://127.0.0.1:1"));
    a2a.install();
    let app = ember_server::api::router(sessions.clone()).merge(ember_server::a2a::api::router(a2a.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let alpha = new_session(&sessions, "alpha");
    let beta = new_session(&sessions, "beta");
    World { sessions, a2a, url: format!("http://{addr}"), alpha, beta }
}

impl World {
    fn token(&self, session: &str) -> String {
        self.a2a.store().issue_token(session).unwrap()
    }

    fn user_messages(&self, id: &str) -> Vec<String> {
        self.sessions
            .store()
            .events_after(id, 0)
            .unwrap()
            .into_iter()
            .filter_map(|e| match e.event {
                AgentEvent::UserMessage { text } => Some(text),
                _ => None,
            })
            .collect()
    }

    async fn wait_for_message(&self, id: &str, ending: &str) -> String {
        for _ in 0..2000 {
            if let Some(m) = self.user_messages(id).into_iter().find(|m| m.ends_with(ending)) {
                return m;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("no message ending with {ending:?} reached {id}: {:?}", self.user_messages(id));
    }
}

fn req(id: i64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

fn call(id: i64, tool: &str, arguments: Value) -> Value {
    req(id, "tools/call", json!({ "name": tool, "arguments": arguments }))
}

/// The opening Claude Code sends (measured on 2.1.319 for `ember-channels`): discover,
/// initialize with 2025-11-25, initialized, tools/list.
fn claude_code_opening() -> Vec<Value> {
    vec![
        req(0, "server/discover", json!({ "_meta": { "protocolVersion": "2026-07-28" } })),
        req(1, "initialize", json!({
            "protocolVersion": "2025-11-25",
            "capabilities": { "roots": { "listChanged": true }, "elicitation": {} },
            "clientInfo": { "name": "claude-code", "version": "2.1.330" }
        })),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        req(2, "tools/list", json!({})),
    ]
}

/// The opening Codex sends (measured on 0.164.0 for `ember-channels`): initialize with
/// 2025-06-18, initialized, tools/list.
fn codex_opening() -> Vec<Value> {
    vec![
        req(1, "initialize", json!({
            "protocolVersion": "2025-06-18",
            "capabilities": { "elicitation": {} },
            "clientInfo": { "name": "codex-mcp-client", "version": "0.167.0" }
        })),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        req(2, "tools/list", json!({})),
    ]
}

fn frames_bytes(frames: &[Value]) -> Vec<u8> {
    let mut input = Vec::new();
    for f in frames {
        writeln!(input, "{f}").unwrap();
    }
    input
}

/// The answers by id. Every stdout line must be a JSON-RPC 2.0 frame.
fn answers(output: &[u8]) -> std::collections::HashMap<i64, Value> {
    String::from_utf8(output.to_vec())
        .unwrap()
        .lines()
        .map(|line| {
            let v: Value = serde_json::from_str(line).unwrap_or_else(|e| panic!("stdout line is not JSON ({e}): {line}"));
            assert_eq!(v["jsonrpc"], "2.0", "{line}");
            (v["id"].as_i64().unwrap_or(-1), v)
        })
        .collect()
}

/// Serve `frames` in process for the session holding `token`.
async fn serve_in_process(url: &str, token: &str, frames: Vec<Value>) -> std::collections::HashMap<i64, Value> {
    let agents = HttpAgents::new(url, token);
    let out = tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        mcp_agents::serve(Cursor::new(frames_bytes(&frames)), &mut out, &agents).unwrap();
        out
    })
    .await
    .unwrap();
    answers(&out)
}

fn text(v: &Value) -> &str {
    v["result"]["content"][0]["text"].as_str().unwrap_or_else(|| panic!("no tool text: {v}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fr_t2_mcp_round_trip_lists_agents_and_the_message_arrives() {
    let w = world().await;
    let mut frames = claude_code_opening();
    frames.push(call(3, "list_agents", json!({})));
    frames.push(call(4, "send_message", json!({ "to": w.beta, "message": "can you review the schema?" })));
    let got = serve_in_process(&w.url, &w.token(&w.alpha), frames).await;

    assert_eq!(got[&0]["error"]["code"], -32601, "server/discover is not ours");
    let init = &got[&1]["result"];
    assert_eq!(init["protocolVersion"], "2025-11-25");
    assert_eq!(init["serverInfo"]["name"], "ember");
    assert!(init["capabilities"]["tools"].is_object());
    assert!(init["instructions"].as_str().unwrap().contains("every Ember session"), "{init}");

    let tools = got[&2]["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["list_agents", "send_message"]);
    for t in tools {
        assert_eq!(t["_meta"]["anthropic/alwaysLoad"], true, "loaded upfront: {t}");
    }
    let send_schema = &tools[1]["inputSchema"];
    assert_eq!(send_schema["required"], json!(["to", "message"]));

    let listed = text(&got[&3]);
    assert!(got[&3]["result"]["isError"].is_null(), "{}", got[&3]);
    assert!(listed.contains(&w.beta) && listed.contains("beta"), "{listed}");
    assert!(!listed.contains(&w.alpha), "the caller is not its own target: {listed}");

    assert!(got[&4]["result"]["isError"].is_null(), "{}", got[&4]);
    let sent = text(&got[&4]);
    assert!(sent.starts_with("Sent message ") && sent.contains(&format!("to {}", w.beta)), "{sent}");

    let arrived = w.wait_for_message(&w.beta, "\n\ncan you review the schema?").await;
    assert!(arrived.starts_with("[ember a2a] Message "), "{arrived}");
    assert!(arrived.contains(&format!("from session {} \"alpha\"", w.alpha)), "{arrived}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fr_t2_mcp_send_message_takes_a_session_title_as_to() {
    let w = world().await;
    let mut frames = codex_opening();
    frames.push(call(3, "send_message", json!({ "to": "Beta", "message": "ping from codex" })));
    let got = serve_in_process(&w.url, &w.token(&w.alpha), frames).await;
    assert_eq!(got[&1]["result"]["protocolVersion"], "2025-06-18");
    assert!(got[&3]["result"]["isError"].is_null(), "{}", got[&3]);
    assert!(text(&got[&3]).contains(&format!("to {}", w.beta)), "{}", got[&3]);
    w.wait_for_message(&w.beta, "\n\nping from codex").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fr_t2_mcp_refusals_are_tool_errors_and_send_nothing() {
    let w = world().await;
    let frames = vec![
        call(1, "send_message", json!({ "to": "no-such-session", "message": "hello?" })),
        call(2, "send_message", json!({ "to": w.alpha, "message": "talking to myself" })),
        call(3, "send_message", json!({ "to": w.beta })),
        req(4, "ping", json!({})),
    ];
    let got = serve_in_process(&w.url, &w.token(&w.alpha), frames).await;
    for id in [1, 2, 3] {
        assert_eq!(got[&id]["result"]["isError"], true, "{}", got[&id]);
        assert!(got[&id].get("error").is_none(), "a refusal is a tool result, not a protocol error");
    }
    assert!(text(&got[&1]).starts_with("The message was not sent"), "{}", got[&1]);
    assert_eq!(got[&4]["result"], json!({}), "the loop goes on");
    assert!(w.user_messages(&w.beta).is_empty());

    // A token the server does not know.
    let got = serve_in_process(&w.url, "not-a-token", vec![call(1, "list_agents", json!({}))]).await;
    assert_eq!(got[&1]["result"]["isError"], true, "{}", got[&1]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fr_t2_mcp_the_binary_serves_the_ember_tools_and_keeps_the_token_out_of_stdout() {
    let w = world().await;
    let token = w.token(&w.alpha);
    let mut frames = claude_code_opening();
    frames.push(call(3, "send_message", json!({ "to": w.beta, "message": "from the binary" })));
    let input = frames_bytes(&frames);
    let (url, tok) = (w.url.clone(), token.clone());
    let output = tokio::task::spawn_blocking(move || {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ember-a2a"))
            .args(["mcp", "agents"])
            .env("EMBER_URL", url)
            .env("EMBER_RUNTIME_TOKEN", tok)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&input).unwrap();
        child.wait_with_output().unwrap()
    })
    .await
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    let all = [output.stdout.as_slice(), output.stderr.as_slice()].concat();
    assert!(!String::from_utf8_lossy(&all).contains(&token), "the token is in no output");
    let got = answers(&output.stdout);
    assert_eq!(got[&2]["result"]["tools"][1]["name"], "send_message");
    assert!(got[&3]["result"]["isError"].is_null(), "{}", got[&3]);
    w.wait_for_message(&w.beta, "\n\nfrom the binary").await;
}
