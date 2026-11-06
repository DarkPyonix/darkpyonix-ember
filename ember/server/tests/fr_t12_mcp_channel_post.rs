//! The real `ember-a2a mcp` binary posts into a channel over stdio (SPEC FR-T12, #380).
//!
//! An in-process server listens on loopback with the auth layer in front; the child gets
//! `EMBER_URL` and a session's runtime token, as an agent process would, and is driven with the
//! frames Claude Code sends. Every read has a timeout so a hang fails instead of blocking.

use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::a2a::{A2a, A2aConfig, A2aStore};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::auth::{self, ServerAuth};
use ember_server::session::{NewSession, Sessions};
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const WAIT: Duration = Duration::from_secs(15);

struct World {
    a2a: Arc<A2a>,
    guarded: Router,
    server_token: String,
    url: String,
    channel: String,
    alpha: String,
    outsider: String,
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

async fn call(app: &Router, method: &str, uri: &str, auth: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri).header("host", "127.0.0.1:8740");
    if let Some(t) = auth {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// A channel `review` holding `alpha`; `outsider` is not a member. Both were mentioned by a
/// person, so only membership separates them.
async fn world() -> World {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let a2a = A2a::new(sessions.clone(), A2aStore::open_in_memory().unwrap(), A2aConfig::new("http://127.0.0.1:1"));
    a2a.install();
    let dir = tempfile::tempdir().unwrap();
    let app = ember_server::channels::api::router_with_a2a(store, sessions.clone(), dir.path(), a2a.clone());
    let server_token = auth::load_or_create_token(dir.path()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let guarded = auth::protect(app.clone(), ServerAuth::new(server_token.clone(), addr, []));
    let served = guarded.clone();
    tokio::spawn(async move { axum::serve(listener, served).await.unwrap() });
    let (alpha, outsider) = (new_session(&sessions, "alpha"), new_session(&sessions, "out"));
    let (_, c) = call(&app, "POST", "/api/channels", None, Some(json!({"project":"core","name":"review","agents":[alpha]}))).await;
    let channel = c["id"].as_str().unwrap().to_string();
    for s in [&alpha, &outsider] {
        a2a.store().insert_channel_message(&channel, "나", s, "please look").unwrap();
    }
    World { a2a, guarded, server_token, url: format!("http://{addr}"), channel, alpha, outsider, _dir: dir }
}

impl World {
    fn token(&self, session: &str) -> String {
        self.a2a.store().issue_token(session).unwrap()
    }

    async fn log(&self) -> Vec<Value> {
        let uri = format!("/api/channels/{}/messages", self.channel);
        let (_, all) = call(&self.guarded, "GET", &uri, Some(&self.server_token), None).await;
        all.as_array().unwrap().clone()
    }
}

struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
}

impl Mcp {
    fn spawn(url: Option<&str>, token: Option<&str>) -> Mcp {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ember-a2a"));
        cmd.arg("mcp").env_remove("EMBER_URL").env_remove("EMBER_RUNTIME_TOKEN");
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

    fn recv(&mut self) -> Value {
        let line = self.lines.recv_timeout(WAIT).expect("the server answered in time");
        let v: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("stdout line is not JSON ({e}): {line}"));
        assert_eq!(v["jsonrpc"], "2.0", "{line}");
        v
    }

    fn rpc(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let v = self.recv();
        assert_eq!(v["id"], id, "{v}");
        v
    }

    /// The Claude Code opening, measured on 2.1.319: discover, initialize, initialized, tools/list.
    fn open_like_claude_code(&mut self) {
        let v = self.rpc(0, "server/discover", json!({ "_meta": { "protocolVersion": "2026-07-28" } }));
        assert_eq!(v["error"]["code"], -32601);
        let v = self.rpc(1, "initialize", json!({
            "protocolVersion": "2025-11-25",
            "capabilities": { "roots": { "listChanged": true }, "elicitation": {} },
            "clientInfo": { "name": "claude-code", "version": "2.1.319" }
        }));
        assert_eq!(v["result"]["protocolVersion"], "2025-11-25");
        self.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        let v = self.rpc(2, "tools/list", json!({}));
        assert_eq!(v["result"]["tools"][0]["name"], "channel_post");
    }

    fn post(&mut self, id: i64, channel: &str, text: &str) -> Value {
        self.rpc(id, "tools/call", json!({ "name": "channel_post", "arguments": { "channel": channel, "text": text } }))
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fr_t12_mcp_member_posts_and_the_message_is_in_the_feed() {
    let w = world().await;
    let mut mcp = Mcp::spawn(Some(&w.url), Some(&w.token(&w.alpha)));
    mcp.open_like_claude_code();
    let v = mcp.post(3, &w.channel, "looks fine to me");
    assert!(v["result"]["isError"].is_null(), "{v}");
    let text = v["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("Posted message ") && text.ends_with(&format!("in channel {}.", w.channel)), "{text}");
    let log = w.log().await;
    let mine: Vec<_> = log.iter().filter(|m| m["from"] == w.alpha.as_str()).collect();
    assert_eq!(mine.len(), 1, "{log:?}");
    assert_eq!(mine[0]["text"], "looks fine to me");
    assert_eq!(mcp.finish(), Some(0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fr_t12_mcp_non_member_is_refused_as_a_tool_error() {
    let w = world().await;
    let mut mcp = Mcp::spawn(Some(&w.url), Some(&w.token(&w.outsider)));
    mcp.open_like_claude_code();
    let v = mcp.post(3, &w.channel, "let me in");
    assert_eq!(v["result"]["isError"], true, "{v}");
    assert!(v["result"]["content"][0]["text"].as_str().unwrap().contains("403"), "{v}");
    assert!(v.get("error").is_none());
    assert!(w.log().await.iter().all(|m| m["from"] != w.outsider.as_str()));
    // The server is still serving.
    assert_eq!(mcp.rpc(4, "ping", json!({}))["result"], json!({}));
    assert_eq!(mcp.finish(), Some(0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fr_t12_mcp_a_bad_token_is_refused_as_a_tool_error() {
    let w = world().await;
    let mut mcp = Mcp::spawn(Some(&w.url), Some("not-a-token"));
    let v = mcp.post(1, &w.channel, "hi");
    assert_eq!(v["result"]["isError"], true, "{v}");
    assert!(v["result"]["content"][0]["text"].as_str().unwrap().contains("401"), "{v}");
    assert_eq!(mcp.finish(), Some(0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fr_t12_mcp_the_seventh_post_in_a_minute_is_refused() {
    let w = world().await;
    let mut mcp = Mcp::spawn(Some(&w.url), Some(&w.token(&w.alpha)));
    mcp.open_like_claude_code();
    let limit = ember_server::a2a::CHANNEL_POSTS_PER_MINUTE as i64;
    for i in 0..limit {
        let v = mcp.post(10 + i, &w.channel, &format!("post {i}"));
        assert!(v["result"]["isError"].is_null(), "{v}");
    }
    let v = mcp.post(99, &w.channel, "one too many");
    assert_eq!(v["result"]["isError"], true, "{v}");
    assert!(v["result"]["content"][0]["text"].as_str().unwrap().contains("429"), "{v}");
    let mine = w.log().await.iter().filter(|m| m["from"] == w.alpha.as_str()).count();
    assert_eq!(mine as i64, limit);
    assert_eq!(mcp.finish(), Some(0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fr_t12_mcp_without_the_environment_listing_works_and_the_call_is_a_tool_error() {
    let mut mcp = Mcp::spawn(None, None);
    mcp.open_like_claude_code();
    let v = mcp.post(3, "some-channel", "hello");
    assert_eq!(v["result"]["isError"], true, "{v}");
    assert!(
        v["result"]["content"][0]["text"].as_str().unwrap().contains("only works inside an Ember agent session"),
        "{v}"
    );
    assert_eq!(mcp.finish(), Some(0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fr_t12_mcp_closing_stdin_ends_the_process_with_zero() {
    let mut mcp = Mcp::spawn(None, None);
    assert_eq!(mcp.rpc(1, "ping", json!({}))["result"], json!({}));
    assert_eq!(mcp.finish(), Some(0));
}
