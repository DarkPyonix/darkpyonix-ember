//! The real `ember-a2a mcp ember` binary lists sessions and sends a message over stdio (SPEC
//! FR-T2, #1045).
//!
//! An in-process server listens on loopback with the auth layer in front; the child gets
//! `EMBER_URL` and a session's runtime token, as an agent process would, and is driven with the
//! frames Claude Code sends (`initialize`, `tools/list`, `tools/call`). The message must arrive in
//! the target session as a user message under the A2A header naming the sender. Every read has a
//! timeout so a hang fails instead of blocking.

use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ember_server::a2a::{A2a, A2aConfig, A2aStore};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::auth::{self, ServerAuth};
use ember_server::events::AgentEvent;
use ember_server::session::{NewSession, Sessions};
use ember_server::store::Store;
use serde_json::{json, Value};

const WAIT: Duration = Duration::from_secs(15);

struct World {
    sessions: Arc<Sessions>,
    a2a: Arc<A2a>,
    url: String,
    alpha: String,
    beta: String,
    _dir: tempfile::TempDir,
}

fn new_session(s: &Sessions, title: &str) -> String {
    s.create(NewSession {
        project: "core".into(),
        agent: AgentKind::Scripted,
        cwd: std::env::temp_dir(),
        model: None,
        title: title.into(),
    })
    .unwrap()
    .id
}

async fn world() -> World {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store, vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let a2a = A2a::new(sessions.clone(), A2aStore::open_in_memory().unwrap(), A2aConfig::new("http://127.0.0.1:1"));
    a2a.install();
    let dir = tempfile::tempdir().unwrap();
    let app = ember_server::api::router(sessions.clone()).merge(ember_server::a2a::api::router(a2a.clone()));
    let server_token = auth::load_or_create_token(dir.path()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let guarded = auth::protect(app, ServerAuth::new(server_token, addr, []));
    tokio::spawn(async move { axum::serve(listener, guarded).await.unwrap() });
    let (alpha, beta) = (new_session(&sessions, "alpha"), new_session(&sessions, "beta"));
    World { sessions, a2a, url: format!("http://{addr}"), alpha, beta, _dir: dir }
}

fn user_messages(s: &Sessions, id: &str) -> Vec<String> {
    s.store()
        .events_after(id, 0)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            AgentEvent::UserMessage { text } => Some(text),
            _ => None,
        })
        .collect()
}

struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
}

impl Mcp {
    fn spawn(url: Option<&str>, token: Option<&str>) -> Mcp {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ember-a2a"));
        cmd.args(["mcp", "ember"]).env_remove("EMBER_URL").env_remove("EMBER_RUNTIME_TOKEN");
        if let Some(u) = url {
            cmd.env("EMBER_URL", u);
        }
        if let Some(t) = token {
            cmd.env("EMBER_RUNTIME_TOKEN", t);
        }
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Mcp { child, stdin, lines }
    }

    fn send(&mut self, frame: Value) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        writeln!(stdin, "{frame}").unwrap();
        stdin.flush().unwrap();
    }

    fn rpc(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let line = self.lines.recv_timeout(WAIT).expect("the server answered in time");
        let v: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("stdout line is not JSON ({e}): {line}"));
        assert_eq!(v["jsonrpc"], "2.0", "{line}");
        assert_eq!(v["id"], id, "{v}");
        v
    }

    /// The Claude Code opening (measured on 2.1.319 for `ember-a2a mcp`): discover, initialize,
    /// initialized, tools/list.
    fn open_like_claude_code(&mut self) -> Value {
        let v = self.rpc(0, "server/discover", json!({ "_meta": { "protocolVersion": "2026-07-28" } }));
        assert_eq!(v["error"]["code"], -32601);
        let v = self.rpc(1, "initialize", json!({
            "protocolVersion": "2025-11-25",
            "capabilities": { "roots": { "listChanged": true }, "elicitation": {} },
            "clientInfo": { "name": "claude-code", "version": "2.1.332" }
        }));
        assert_eq!(v["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(v["result"]["serverInfo"]["name"], "ember");
        assert!(v["result"]["instructions"].as_str().unwrap().contains("use these tools first"));
        self.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        self.rpc(2, "tools/list", json!({}))
    }

    fn finish(mut self) -> Option<i32> {
        drop(self.stdin.take());
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.code();
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                panic!("the child did not exit after stdin closed");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn text(v: &Value) -> &str {
    v["result"]["content"][0]["text"].as_str().unwrap_or_else(|| panic!("no text content: {v}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fr_t2_mcp_round_trip_lists_and_the_sent_message_arrives() {
    let w = world().await;
    let token = w.a2a.store().issue_token(&w.alpha).unwrap();
    let mut mcp = Mcp::spawn(Some(&w.url), Some(&token));
    let list = mcp.open_like_claude_code();
    let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["list_agents", "send_message"]);

    let v = mcp.rpc(3, "tools/call", json!({ "name": "list_agents", "arguments": {} }));
    assert!(v["result"]["isError"].is_null(), "{v}");
    let rows = text(&v);
    assert!(rows.contains(&w.beta) && rows.contains("beta"), "{rows}");
    assert!(!rows.contains(&w.alpha), "the caller is not its own target: {rows}");

    // The call as Claude Code sends it, with `_meta`.
    let v = mcp.rpc(4, "tools/call", json!({
        "name": "send_message",
        "arguments": { "to": w.beta, "message": "can you check the tests?" },
        "_meta": { "claudecode/toolUseId": "toolu_x", "progressToken": 4 }
    }));
    assert!(v["result"]["isError"].is_null(), "{v}");
    let sent = text(&v);
    assert!(sent.starts_with("Sent message ") && sent.contains(&format!(" to {}: ", w.beta)), "{sent}");

    // The message arrives in beta as a user message from alpha.
    let deadline = Instant::now() + WAIT;
    let got = loop {
        let got = user_messages(&w.sessions, &w.beta);
        if !got.is_empty() {
            break got;
        }
        assert!(Instant::now() < deadline, "the message never arrived in beta");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].starts_with("[ember a2a] Message "), "{}", got[0]);
    assert!(got[0].contains(&format!("from session {} \"alpha\"", w.alpha)), "{}", got[0]);
    assert!(got[0].ends_with("\n\ncan you check the tests?"), "{}", got[0]);
    let id = sent.strip_prefix("Sent message ").unwrap().split(' ').next().unwrap();
    assert!(got[0].contains(id), "the receipt names the delivered message: {id} / {}", got[0]);

    // A refusal from the server is a tool error with a fixed sentence; the loop goes on.
    let v = mcp.rpc(5, "tools/call", json!({ "name": "send_message", "arguments": { "to": "no-such-session", "message": "x" } }));
    assert_eq!(v["result"]["isError"], true, "{v}");
    assert!(text(&v).starts_with("Refused: no such session"), "{v}");
    assert_eq!(mcp.rpc(6, "ping", json!({}))["result"], json!({}));
    assert_eq!(mcp.finish(), Some(0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_t2_mcp_outside_a_session_answers_the_protocol_and_refuses_the_tools() {
    let mut mcp = Mcp::spawn(None, None);
    let list = mcp.rpc(1, "tools/list", json!({}));
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 2);
    let v = mcp.rpc(2, "tools/call", json!({ "name": "list_agents", "arguments": {} }));
    assert_eq!(v["result"]["isError"], true);
    assert!(text(&v).contains("only work inside an Ember agent session"), "{v}");
    assert_eq!(mcp.finish(), Some(0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr_t2_mcp_a_bad_token_is_refused_without_echoing_it() {
    let w = world().await;
    const CANARY: &str = "TOKEN-CANARY-1045";
    let mut mcp = Mcp::spawn(Some(&w.url), Some(CANARY));
    let v = mcp.rpc(1, "tools/call", json!({ "name": "send_message", "arguments": { "to": w.beta, "message": "x" } }));
    assert_eq!(v["result"]["isError"], true, "{v}");
    assert!(text(&v).contains("runtime token was not accepted"), "{v}");
    assert!(!v.to_string().contains(CANARY));
    assert!(user_messages(&w.sessions, &w.beta).is_empty());
    assert_eq!(mcp.finish(), Some(0));
}

#[test]
fn fr_t2_mcp_unknown_server_argument_is_a_usage_error() {
    let out = Command::new(env!("CARGO_BIN_EXE_ember-a2a")).args(["mcp", "other"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty(), "stdout carries protocol frames only");
}
