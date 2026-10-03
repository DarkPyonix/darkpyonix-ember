//! An in-process ACP agent for tests: speaks ACP v1 over a byte stream, scripted by the prompt.
//!
//! Prompts (first word):
//! - `hello`: two message chunks of one message, then `end_turn` with usage.
//! - `setup?`: answers which method opened the session (`new`, `load` or `resume`).
//! - `echo <text>`: answers with the whole prompt text.
//! - `mcp?`: answers with the MCP server names given to `session/new`.
//! - `edit <abs path>`: a tool call, a permission request, then (if allowed) `fs/read_text_file`
//!   and `fs/write_text_file` appending `beta\n`; answers `chosen <optionId|cancelled>`.
//! - `readlines <abs path>`: `fs/read_text_file` with `line: 2, limit: 1`; answers the content.
//! - `run`: `terminal/create` (`sh -c`, output on both streams, exit 3), embeds the terminal in the
//!   tool call, waits, reads the output, releases; answers `exit <code> output <output>`.
//! - `kill`: a long `sleep` killed with `terminal/kill`; answers `signal <name>`.
//! - `plan`: a `plan` update.
//! - `slow`: a pending tool call, then waits for `session/cancel` and stops with `cancelled`.
//! - `slow-permission`: a permission request it waits on; answers `permission <outcome>` and
//!   stops with `cancelled` if the turn was cancelled.
//! - `elicit`: an `elicitation/create` request; answers `elicitation error <code>`.
//! - `exit`: the agent goes away mid-turn.

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf};

#[derive(Debug, Clone)]
pub struct FakeOptions {
    /// Advertise `loadSession`.
    pub load: bool,
    /// Advertise `sessionCapabilities.resume`.
    pub resume: bool,
    /// The protocol version to answer `initialize` with.
    pub version: u64,
}

impl Default for FakeOptions {
    fn default() -> FakeOptions {
        FakeOptions { load: false, resume: false, version: 1 }
    }
}

struct Fake {
    lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
    out: WriteHalf<DuplexStream>,
    opts: FakeOptions,
    next_id: i64,
    session: String,
    setup: &'static str,
    mcp: Vec<String>,
    cancelled: bool,
}

pub async fn run(io: DuplexStream, opts: FakeOptions) {
    let (r, w) = tokio::io::split(io);
    let mut fake = Fake {
        lines: BufReader::new(r).lines(),
        out: w,
        opts,
        next_id: 1000,
        session: String::new(),
        setup: "",
        mcp: Vec::new(),
        cancelled: false,
    };
    fake.serve().await;
}

impl Fake {
    async fn next(&mut self) -> Option<Value> {
        loop {
            let line = self.lines.next_line().await.ok()??;
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                assert_eq!(v["jsonrpc"], "2.0", "client message without jsonrpc 2.0: {line}");
                return Some(v);
            }
        }
    }

    async fn send(&mut self, v: Value) {
        let mut line = serde_json::to_vec(&v).unwrap();
        line.push(b'\n');
        let _ = self.out.write_all(&line).await;
        let _ = self.out.flush().await;
    }

    async fn reply(&mut self, id: Value, result: Value) {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "result": result })).await;
    }

    async fn update(&mut self, update: Value) {
        let session = self.session.clone();
        self.send(json!({ "jsonrpc": "2.0", "method": "session/update",
                          "params": { "sessionId": session, "update": update } }))
            .await;
    }

    async fn say(&mut self, text: &str) {
        self.update(json!({ "sessionUpdate": "agent_message_chunk",
                            "content": { "type": "text", "text": text } }))
            .await;
    }

    /// Call the client and wait for its response, noting a `session/cancel` meanwhile.
    async fn call(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).await;
        loop {
            let msg = self.next().await.expect("client closed during a call");
            if msg.get("method").is_none() && msg["id"] == json!(id) {
                return msg;
            }
            if msg["method"] == "session/cancel" {
                self.cancelled = true;
            }
        }
    }

    async fn serve(&mut self) {
        while let Some(msg) = self.next().await {
            let id = msg.get("id").cloned().unwrap_or(Value::Null);
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            match msg["method"].as_str() {
                Some("initialize") => {
                    assert_eq!(params["protocolVersion"], 1);
                    assert_eq!(params["clientCapabilities"]["fs"]["readTextFile"], true);
                    assert_eq!(params["clientCapabilities"]["fs"]["writeTextFile"], true);
                    assert_eq!(params["clientCapabilities"]["terminal"], true);
                    let session_caps = if self.opts.resume { json!({ "resume": {} }) } else { json!({}) };
                    let result = json!({
                        "protocolVersion": self.opts.version,
                        "agentCapabilities": { "loadSession": self.opts.load, "sessionCapabilities": session_caps },
                        "agentInfo": { "name": "fake", "version": "0" },
                        "authMethods": [],
                    });
                    self.reply(id, result).await;
                }
                Some("session/new") => {
                    assert!(params["cwd"].as_str().is_some_and(|c| c.starts_with('/')));
                    self.mcp = params["mcpServers"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|m| m["name"].as_str().unwrap().to_string())
                        .collect();
                    self.session = "fake-session-1".into();
                    self.setup = "new";
                    self.reply(id, json!({ "sessionId": "fake-session-1" })).await;
                }
                Some("session/load") => {
                    assert!(self.opts.load, "session/load without loadSession");
                    self.session = params["sessionId"].as_str().unwrap().to_string();
                    self.setup = "load";
                    // Replay, as the spec requires.
                    self.update(json!({ "sessionUpdate": "user_message_chunk",
                                        "content": { "type": "text", "text": "old question" } }))
                        .await;
                    self.say("REPLAYED").await;
                    self.update(json!({ "sessionUpdate": "tool_call", "toolCallId": "old",
                                        "title": "old tool", "status": "completed" }))
                        .await;
                    self.reply(id, json!({})).await;
                }
                Some("session/resume") => {
                    assert!(self.opts.resume, "session/resume without the capability");
                    self.session = params["sessionId"].as_str().unwrap().to_string();
                    self.setup = "resume";
                    self.reply(id, json!({})).await;
                }
                Some("session/prompt") => {
                    assert_eq!(params["sessionId"], json!(self.session));
                    let text = params["prompt"][0]["text"].as_str().unwrap_or_default().to_string();
                    if !self.prompt(id, &text).await {
                        return;
                    }
                }
                Some("session/cancel") => self.cancelled = true,
                Some(other) => {
                    let other = other.to_string();
                    self.send(json!({ "jsonrpc": "2.0", "id": id,
                                      "error": { "code": -32601, "message": other } }))
                        .await;
                }
                None => {}
            }
        }
    }

    /// Run one prompt turn; `false` = the agent goes away.
    async fn prompt(&mut self, id: Value, text: &str) -> bool {
        self.cancelled = false;
        let session = self.session.clone();
        let mut words = text.split_whitespace();
        let first = words.next().unwrap_or_default();
        let arg = words.next().unwrap_or_default().to_string();
        let mut usage = Value::Null;
        match first {
            "hello" => {
                for t in ["Hi ", "there"] {
                    self.update(json!({ "sessionUpdate": "agent_message_chunk", "messageId": "m1",
                                        "content": { "type": "text", "text": t } }))
                        .await;
                }
                usage = json!({ "totalTokens": 15, "inputTokens": 10, "outputTokens": 5 });
            }
            "setup?" => {
                let s = format!("setup={}", self.setup);
                self.say(&s).await;
            }
            "echo" => self.say(text).await,
            "mcp?" => {
                let s = format!("mcp={}", self.mcp.join(","));
                self.say(&s).await;
            }
            "edit" => {
                self.say("Editing.").await;
                self.update(json!({ "sessionUpdate": "tool_call", "toolCallId": "t1",
                                    "title": format!("Edit {arg}"), "kind": "edit", "status": "pending",
                                    "rawInput": { "path": arg }, "locations": [{ "path": arg }] }))
                    .await;
                let resp = self
                    .call(
                        "session/request_permission",
                        json!({ "sessionId": session, "toolCall": { "toolCallId": "t1" }, "options": [
                            { "optionId": "allow-once", "name": "Allow once", "kind": "allow_once" },
                            { "optionId": "allow-always", "name": "Always allow", "kind": "allow_always" },
                            { "optionId": "reject-once", "name": "Reject", "kind": "reject_once" },
                            { "optionId": "reject-always", "name": "Always reject", "kind": "reject_always" },
                        ] }),
                    )
                    .await;
                let outcome = &resp["result"]["outcome"];
                let chosen = if outcome["outcome"] == "selected" {
                    outcome["optionId"].as_str().unwrap().to_string()
                } else {
                    "cancelled".to_string()
                };
                if chosen.starts_with("allow") {
                    self.update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1",
                                        "status": "in_progress" }))
                        .await;
                    let read = self
                        .call("fs/read_text_file", json!({ "sessionId": session, "path": arg }))
                        .await;
                    let old = read["result"]["content"].as_str().expect("read result").to_string();
                    let new = format!("{old}beta\n");
                    let wrote = self
                        .call("fs/write_text_file", json!({ "sessionId": session, "path": arg, "content": new }))
                        .await;
                    assert_eq!(wrote["result"], json!({}));
                    self.update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1",
                                        "status": "completed", "content": [
                                            { "type": "diff", "path": arg, "oldText": old, "newText": new } ] }))
                        .await;
                } else {
                    self.update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1",
                                        "status": "failed", "content": [
                                            { "type": "content", "content": { "type": "text", "text": "rejected" } } ] }))
                        .await;
                }
                self.say(&format!("chosen {chosen}")).await;
            }
            "readlines" => {
                let read = self
                    .call("fs/read_text_file", json!({ "sessionId": session, "path": arg, "line": 2, "limit": 1 }))
                    .await;
                let content = read["result"]["content"].as_str().unwrap_or("ERROR").to_string();
                self.say(&content).await;
            }
            "run" => {
                self.update(json!({ "sessionUpdate": "tool_call", "toolCallId": "t2", "title": "sh",
                                    "kind": "execute", "status": "pending",
                                    "rawInput": { "command": "printf out; printf err 1>&2; exit 3" } }))
                    .await;
                let created = self
                    .call("terminal/create", json!({ "sessionId": session, "command": "sh",
                        "args": ["-c", "printf out; printf err 1>&2; exit 3"], "outputByteLimit": 1000 }))
                    .await;
                let term = created["result"]["terminalId"].as_str().expect("terminalId").to_string();
                self.update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t2", "status": "in_progress",
                                    "content": [{ "type": "terminal", "terminalId": term }] }))
                    .await;
                let waited = self
                    .call("terminal/wait_for_exit", json!({ "sessionId": session, "terminalId": term }))
                    .await;
                let code = waited["result"]["exitCode"].clone();
                let output = self
                    .call("terminal/output", json!({ "sessionId": session, "terminalId": term }))
                    .await;
                assert_eq!(output["result"]["exitStatus"]["exitCode"], code);
                assert_eq!(output["result"]["truncated"], false);
                let out = output["result"]["output"].as_str().unwrap().to_string();
                self.call("terminal/release", json!({ "sessionId": session, "terminalId": term })).await;
                self.update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t2", "status": "completed",
                                    "content": [{ "type": "terminal", "terminalId": term }] }))
                    .await;
                self.say(&format!("exit {code} output {out}")).await;
            }
            "kill" => {
                let created = self
                    .call("terminal/create", json!({ "sessionId": session, "command": "sleep", "args": ["30"] }))
                    .await;
                let term = created["result"]["terminalId"].as_str().unwrap().to_string();
                self.call("terminal/kill", json!({ "sessionId": session, "terminalId": term })).await;
                let waited = self
                    .call("terminal/wait_for_exit", json!({ "sessionId": session, "terminalId": term }))
                    .await;
                self.call("terminal/release", json!({ "sessionId": session, "terminalId": term })).await;
                let gone = self
                    .call("terminal/output", json!({ "sessionId": session, "terminalId": term }))
                    .await;
                assert!(gone.get("error").is_some(), "a released terminal id stays valid");
                let signal = waited["result"]["signal"].as_str().unwrap_or("none").to_string();
                self.say(&format!("signal {signal}")).await;
            }
            "plan" => {
                self.update(json!({ "sessionUpdate": "plan", "entries": [
                    { "content": "Read", "priority": "high", "status": "completed" },
                    { "content": "Edit", "priority": "medium", "status": "pending" } ] }))
                    .await;
            }
            "slow" => {
                self.update(json!({ "sessionUpdate": "tool_call", "toolCallId": "t3", "title": "wait",
                                    "kind": "other", "status": "in_progress" }))
                    .await;
                while !self.cancelled {
                    match self.next().await {
                        Some(m) if m["method"] == "session/cancel" => self.cancelled = true,
                        Some(_) => {}
                        None => return false,
                    }
                }
            }
            "slow-permission" => {
                let resp = self
                    .call(
                        "session/request_permission",
                        json!({ "sessionId": session,
                                "toolCall": { "toolCallId": "t4", "title": "rm -rf build", "kind": "delete" },
                                "options": [{ "optionId": "ok", "name": "OK", "kind": "allow_once" }] }),
                    )
                    .await;
                let outcome = resp["result"]["outcome"]["outcome"].as_str().unwrap_or("?").to_string();
                self.say(&format!("permission {outcome}")).await;
            }
            "elicit" => {
                let resp = self.call("elicitation/create", json!({ "sessionId": session })).await;
                let code = resp["error"]["code"].clone();
                self.say(&format!("elicitation error {code}")).await;
            }
            "exit" => {
                self.say("bye").await;
                return false;
            }
            _ => self.say("?").await,
        }
        let stop = if self.cancelled { "cancelled" } else { "end_turn" };
        let mut result = json!({ "stopReason": stop });
        if !usage.is_null() {
            result["usage"] = usage;
        }
        self.reply(id, result).await;
        true
    }
}
