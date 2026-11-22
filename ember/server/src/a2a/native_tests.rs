//! FR-T2 (#1045): every harness Ember starts gets the `ember` MCP server with `list_agents` and
//! `send_message`, loaded upfront, beside the built-ins, and its instructions name the tools first.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::native::*;
use super::*;
use crate::agents::claude_code::{mcp_config, ClaudeCodeAdapter, PrivateConfigFile};
use crate::agents::scripted::ScriptedAdapter;
use crate::agents::AgentAdapter;
use crate::store::Store;

const CLI: &str = "/opt/ember/ember-a2a";

fn world(cli: Option<&str>) -> Arc<A2a> {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store, vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let mut config = A2aConfig::new("http://127.0.0.1:8740");
    config.cli_path = cli.map(PathBuf::from);
    let a = A2a::new(sessions, A2aStore::open_in_memory().unwrap(), config);
    // Hermetic: never read the real home or managed files.
    *a.claude_sources.lock().unwrap() = ClaudeConfigSources::default();
    a
}

fn rec(a: &A2a, agent: AgentKind) -> SessionRecord {
    a.sessions.store().create_session("core", agent, "/tmp", None, "t").unwrap()
}

fn acp() -> AgentKind {
    AgentKind::Acp(crate::agents::AcpName::register("acp-t2-tool").unwrap())
}

/// A request as the hooks leave it, in their order: runtime env, instructions, the channel tool,
/// then the A2A tools.
fn started(a: &A2a, r: &SessionRecord) -> StartRequest {
    let mut req = StartRequest {
        cwd: "/nonexistent-ember-test/proj".into(),
        env: a.runtime_env(r),
        instructions: Some(a.instructions_for(r)),
        ..Default::default()
    };
    a.channel_tool_config(r, &mut req);
    a.a2a_tool_config(r, &mut req);
    req
}

fn env_of(req: &StartRequest, name: &str) -> String {
    req.env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()).unwrap()
}

fn ember_entry(req: &StartRequest) -> Option<&McpServer> {
    req.mcp_servers.iter().find(|s| s.name == A2A_MCP_SERVER)
}

fn move_to(a: &A2a, r: &SessionRecord, computer: &str) {
    a.sessions
        .store()
        .conn()
        .execute(
            "INSERT INTO session_computer (session_id, computer_id, switched_at) VALUES (?1, ?2, 0)",
            rusqlite::params![r.id, computer],
        )
        .unwrap();
}

fn put(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn with_server(key: &str) -> String {
    format!(r#"{{"mcpServers":{{"{key}":{{"command":"x"}}}}}}"#)
}

#[test]
fn fr_t2_the_constants_are_built_from_each_other() {
    assert_eq!(A2A_MCP_SERVER, "ember");
    assert_eq!(LIST_AGENTS_TOOL, format!("mcp__{A2A_MCP_SERVER}__{}", mcp_agents::LIST_AGENTS));
    assert_eq!(SEND_MESSAGE_TOOL, format!("mcp__{A2A_MCP_SERVER}__{}", mcp_agents::SEND_MESSAGE));
}

#[test]
fn fr_t2_claude_code_launch_config_includes_the_ember_server() {
    let a = world(Some(CLI));
    let r = rec(&a, AgentKind::ClaudeCode);
    let req = started(&a, &r);
    let token = env_of(&req, "EMBER_RUNTIME_TOKEN");
    let names: Vec<&str> = req.mcp_servers.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["ember-channels", "ember"]);
    let config: serde_json::Value = serde_json::from_str(&mcp_config(&req.mcp_servers).unwrap()).unwrap();
    assert_eq!(
        config["mcpServers"]["ember"],
        serde_json::json!({
            "type": "stdio",
            "command": CLI,
            "args": ["mcp", "agents"],
            "env": { "EMBER_URL": "http://127.0.0.1:8740", "EMBER_RUNTIME_TOKEN": token },
            "alwaysLoad": true,
        })
    );
    assert!(config["mcpServers"]["ember-channels"].get("alwaysLoad").is_none(), "the channel tool is unchanged");
    // The same token the command uses: no second one was issued.
    assert_eq!(a.store.session_for_token(&token).unwrap().as_deref(), Some(r.id.as_str()));
    // Through the private file, with one allow rule per tool, and the token in no argument.
    let dir = tempfile::tempdir().unwrap();
    let file = PrivateConfigFile::create(dir.path(), &mcp_config(&req.mcp_servers).unwrap()).unwrap();
    let args = ClaudeCodeAdapter::args_with(&req, Some(file.path()));
    let i = args.iter().position(|x| x == "--allowedTools").unwrap();
    assert_eq!(args[i + 1..i + 4], [CHANNEL_POST_TOOL, LIST_AGENTS_TOOL, SEND_MESSAGE_TOOL]);
    assert!(args[i + 4].starts_with("--mcp-config="), "{args:?}");
    assert!(!args.join("\n").contains(&token));
}

#[test]
fn fr_t2_codex_launch_config_includes_the_ember_server() {
    let a = world(Some(CLI));
    let r = rec(&a, AgentKind::Codex);
    let req = started(&a, &r);
    let token = env_of(&req, "EMBER_RUNTIME_TOKEN");
    let args = crate::agents::codex::app_server_args(&req);
    assert_eq!(
        args,
        [
            "app-server",
            "-c",
            "mcp_servers.ember-channels.command=\"/opt/ember/ember-a2a\"",
            "-c",
            "mcp_servers.ember-channels.args=[\"mcp\"]",
            "-c",
            "mcp_servers.ember-channels.startup_timeout_sec=10",
            "-c",
            "mcp_servers.ember-channels.env_vars=[\"EMBER_URL\", \"EMBER_RUNTIME_TOKEN\"]",
            "-c",
            "mcp_servers.ember.command=\"/opt/ember/ember-a2a\"",
            "-c",
            "mcp_servers.ember.args=[\"mcp\", \"agents\"]",
            "-c",
            "mcp_servers.ember.startup_timeout_sec=10",
            "-c",
            "mcp_servers.ember.env_vars=[\"EMBER_URL\", \"EMBER_RUNTIME_TOKEN\"]",
            "-c",
            "mcp_servers.ember-channels.tools.channel_post.approval_mode=\"approve\"",
            "-c",
            "mcp_servers.ember.tools.list_agents.approval_mode=\"approve\"",
            "-c",
            "mcp_servers.ember.tools.send_message.approval_mode=\"approve\"",
        ]
    );
    assert!(!args.join("\n").contains(&token), "the token is never an argument");
    // Both servers read the same two variables, so one process environment carries them.
    let (env, skipped) = crate::agents::codex::mcp_process_env(&req);
    assert!(skipped.is_empty(), "{skipped:?}");
    assert!(env.contains(&("EMBER_RUNTIME_TOKEN".to_string(), token)));
}

#[test]
fn fr_t2_antigravity_and_acp_session_config_include_the_ember_server() {
    let a = world(Some(CLI));
    let r = rec(&a, AgentKind::Antigravity);
    let req = started(&a, &r);
    let token = env_of(&req, "EMBER_RUNTIME_TOKEN");
    let cfg = crate::agents::antigravity::mcp_config(&req.mcp_servers).unwrap();
    assert_eq!(
        cfg["mcpServers"]["ember"],
        serde_json::json!({
            "command": CLI,
            "args": ["mcp", "agents"],
            "env": { "EMBER_URL": "http://127.0.0.1:8740", "EMBER_RUNTIME_TOKEN": token },
        })
    );
    assert!(req.allowed_tools.is_empty(), "Antigravity asks through Ember's hook: {:?}", req.allowed_tools);
    let r = rec(&a, acp());
    let req = started(&a, &r);
    let param = crate::agents::acp::mcp_servers_param(&req.mcp_servers);
    let names: Vec<&str> = param.as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["ember-channels", "ember"]);
    assert!(req.allowed_tools.is_empty(), "an ACP agent asks through session/request_permission");
}

/// D5: nothing is denied or intercepted. The only rules Ember adds are exact allow rules for its
/// own tools; the built-ins (`ListAgents`, `SendMessage`, `Agent`, Codex `multi_agent`) are never
/// named.
#[test]
fn fr_t2_the_built_ins_stay_enabled() {
    let a = world(Some(CLI));
    for agent in [AgentKind::ClaudeCode, AgentKind::Codex] {
        let r = rec(&a, agent);
        let req = started(&a, &r);
        for rule in &req.allowed_tools {
            assert!(rule.starts_with("mcp__ember"), "{rule}");
        }
        let args = match agent {
            AgentKind::ClaudeCode => ClaudeCodeAdapter::args_with(&req, None),
            _ => crate::agents::codex::app_server_args(&req),
        };
        let joined = args.join("\n");
        for word in ["disallowed", "ListAgents", "SendMessage", "multi_agent", "--deny", "features."] {
            assert!(!joined.contains(word), "{agent:?}: {word} in {args:?}");
        }
    }
}

#[test]
fn fr_t2_hook_skips_where_the_channel_tool_skips() {
    // Another computer.
    let a = world(Some(CLI));
    let r = rec(&a, AgentKind::ClaudeCode);
    move_to(&a, &r, "pi-1");
    assert!(ember_entry(&started(&a, &r)).is_none());
    // The scripted agent.
    let r = rec(&a, AgentKind::Scripted);
    assert!(started(&a, &r).mcp_servers.is_empty());
    // A request that already runs elsewhere.
    let r = rec(&a, AgentKind::Codex);
    let mut req = StartRequest { cwd: ".".into(), env: a.runtime_env(&r), ..Default::default() };
    req.remote = Some(crate::agents::RemoteExec { environment_id: "ember-x".into(), exec_server_url: "ws://x".into() });
    a.a2a_tool_config(&r, &mut req);
    assert!(req.mcp_servers.is_empty() && req.allowed_tools.is_empty());
    // Missing credentials.
    for drop in ["EMBER_RUNTIME_TOKEN", "EMBER_URL"] {
        let mut req = StartRequest { cwd: ".".into(), env: a.runtime_env(&r), ..Default::default() };
        req.env.retain(|(k, _)| k != drop);
        a.a2a_tool_config(&r, &mut req);
        assert!(req.mcp_servers.is_empty() && req.allowed_tools.is_empty(), "{drop}");
    }
    // No binary.
    let b = world(None);
    let r = rec(&b, AgentKind::ClaudeCode);
    assert!(started(&b, &r).mcp_servers.is_empty());
}

#[test]
fn fr_t2_hook_steps_aside_for_a_server_named_ember_in_the_request() {
    let a = world(Some(CLI));
    let r = rec(&a, AgentKind::ClaudeCode);
    let mut req = StartRequest { cwd: ".".into(), env: a.runtime_env(&r), ..Default::default() };
    req.mcp_servers.push(McpServer { name: "Ember".into(), command: "/usr/bin/other".into(), ..Default::default() });
    a.a2a_tool_config(&r, &mut req);
    assert_eq!(req.mcp_servers.len(), 1);
    assert_eq!(req.mcp_servers[0].command, "/usr/bin/other");
    assert!(req.allowed_tools.is_empty(), "no allow rule for a server we did not register");
}

#[test]
fn fr_t2_hook_steps_aside_for_a_server_named_ember_in_the_users_configuration() {
    let d = tempfile::tempdir().unwrap();
    let a = world(Some(CLI));
    *a.claude_sources.lock().unwrap() = ClaudeConfigSources { home: Some(d.path().to_path_buf()), ..Default::default() };
    put(&d.path().join(".claude.json"), &with_server("ember"));
    put(&d.path().join(".codex/config.toml"), "[mcp_servers.ember]\ncommand = \"y\"\n");
    put(&d.path().join(".gemini/config/mcp_config.json"), &with_server("ember"));
    for agent in [AgentKind::ClaudeCode, AgentKind::Codex, AgentKind::Antigravity] {
        let r = rec(&a, agent);
        let req = started(&a, &r);
        assert!(ember_entry(&req).is_none(), "{agent:?}");
        assert!(!req.allowed_tools.iter().any(|t| t.starts_with("mcp__ember__")), "{agent:?}");
        // The prompt keeps the command.
        assert_eq!(req.instructions.unwrap(), a.instructions_for(&r), "{agent:?}");
    }
}

#[test]
fn fr_t2_name_checks_read_each_harness_file() {
    let d = tempfile::tempdir().unwrap();
    let cwd = d.path().join("proj/sub");
    std::fs::create_dir_all(&cwd).unwrap();
    let home = d.path().join("home");
    // Claude Code: exact keys, so `ember-channels` is another server.
    assert_eq!(claude_name_check(&cwd, Some(home.as_path()), None, &[]), NameCheck::Free);
    put(&d.path().join("proj/.mcp.json"), &with_server("ember-channels"));
    assert_eq!(claude_name_check(&cwd, Some(home.as_path()), None, &[]), NameCheck::Free);
    put(&d.path().join("proj/.mcp.json"), &with_server("EMBER"));
    assert_eq!(
        claude_name_check(&cwd, Some(home.as_path()), None, &[]),
        NameCheck::Taken { kind: "project .mcp.json" }
    );
    put(&d.path().join("proj/.mcp.json"), "{ not json");
    assert!(matches!(claude_name_check(&cwd, None, None, &[]), NameCheck::Unsure { .. }), "fail closed");
    std::fs::remove_file(d.path().join("proj/.mcp.json")).unwrap();
    put(&home.join(".claude.json"), &format!(r#"{{"projects":{{"{}":{{"mcpServers":{{"ember":{{}}}}}}}}}}"#, cwd.display()));
    assert_eq!(
        claude_name_check(&cwd, Some(home.as_path()), None, &[]),
        NameCheck::Taken { kind: "local config" }
    );
    // Codex and Antigravity: a text search for the name as a key.
    let codex_home = d.path().join("codex-home");
    put(&codex_home.join("config.toml"), "[projects.\"/Users/me/darkpyonix-ember\"]\ntrust_level = \"trusted\"\n[mcp_servers.ember-channels]\ncommand = \"x\"\n");
    assert_eq!(codex_name_check(&cwd, Some(codex_home.as_path()), &[]), NameCheck::Free, "a path and ember-channels are not the key");
    put(&codex_home.join("config.toml"), "[mcp_servers]\nember = { command = \"x\" }\n");
    assert_eq!(codex_name_check(&cwd, Some(codex_home.as_path()), &[]), NameCheck::Taken { kind: "user config.toml" });
    put(&home.join(".gemini/config/mcp_config.json"), &with_server("ember-browser"));
    assert_eq!(agy_name_check(&cwd, Some(home.as_path())), NameCheck::Free);
    put(&d.path().join("proj/.agents/mcp_config.json"), &with_server("ember"));
    assert_eq!(agy_name_check(&cwd, None), NameCheck::Taken { kind: "project .agents/mcp_config.json" });
}

#[test]
fn fr_t2_the_text_search_finds_ember_only_as_a_key() {
    for hit in [
        "[mcp_servers.ember]\n",
        "[mcp_servers.ember.env]\n",
        "[mcp_servers.\"ember\"]\n",
        "mcp_servers.ember.command = \"x\"\n",
        "[mcp_servers]\nEmber = {}\n",
        "mcp_servers = { ember = { command = \"x\" } }\n",
        "{\"mcpServers\": {\"ember\": {}}}",
        "[mcp_servers.\"\\u0065mber\"]\n",
        "# ember is mine\n",
    ] {
        assert!(mentions_as_key(hit.as_bytes(), A2A_MCP_SERVER), "{hit:?}");
    }
    for miss in [
        "[mcp_servers.ember-channels]\n",
        "[projects.\"/Volumes/x/darkpyonix-ember\"]\n",
        "path = \"/Users/me/ember/bin\"\n",
        "[mcp_servers.embers]\n",
        "[mcp_servers.my_ember]\n",
        "model = \"x\"\n",
    ] {
        assert!(!mentions_as_key(miss.as_bytes(), A2A_MCP_SERVER), "{miss:?}");
    }
}

/// A Codex layer the name check does not read may define its own `ember`: Codex would then ask
/// through an elicitation Ember cannot answer for this server, so the session keeps the command.
#[test]
fn fr_t2_codex_with_an_unread_layer_keeps_the_command() {
    let d = tempfile::tempdir().unwrap();
    let a = world(Some(CLI));
    let codex_home = d.path().join("codex-home");
    put(&codex_home.join("work.config.toml"), "model = \"x\"\n");
    *a.claude_sources.lock().unwrap() =
        ClaudeConfigSources { codex_home: Some(codex_home.clone()), ..Default::default() };
    let r = rec(&a, AgentKind::Codex);
    let req = started(&a, &r);
    assert!(ember_entry(&req).is_none());
    assert_eq!(req.instructions.unwrap(), a.instructions_for(&r));
}

#[test]
fn fr_t2_claude_instructions_name_the_mcp_tools_first_and_keep_the_cli_fallback() {
    let a = world(Some(CLI));
    let r = rec(&a, AgentKind::ClaudeCode);
    let text = started(&a, &r).instructions.unwrap();
    let tools = text.find(SEND_MESSAGE_TOOL).expect("the tool is named");
    let cli = text.find("`ember-a2a send <session-id>").expect("the command stays");
    assert!(tools < cli, "the tools come first: {text}");
    assert!(text.contains(LIST_AGENTS_TOOL), "{text}");
    assert!(text.contains("`to`") && text.contains("`message`") && text.contains("`reply_to`"), "{text}");
    assert!(text.contains("If those tools are not available, use the `ember-a2a` shell command"), "{text}");
    assert!(text.contains("every Ember session"), "{text}");
    // Only the opening of the messaging section changed: the rest is the shared text.
    let shared = a.instructions_for(&r);
    let tail = "\n\n- `ember-a2a list`";
    assert_eq!(text[text.find(tail).unwrap()..], shared[shared.find(tail).unwrap()..]);
}

#[test]
fn fr_t2_other_harnesses_are_told_the_server_and_the_tool_names() {
    let a = world(Some(CLI));
    for agent in [AgentKind::Codex, AgentKind::Antigravity, acp()] {
        let r = rec(&a, agent);
        let text = started(&a, &r).instructions.unwrap();
        assert!(
            text.contains("Use the `list_agents` and `send_message` tools of the MCP server `ember` first"),
            "{agent:?}: {text}"
        );
        assert!(!text.contains(SEND_MESSAGE_TOOL), "no combined name is claimed: {agent:?}");
        assert!(text.contains("If those tools are not available, use the `ember-a2a` shell command"));
    }
}

#[test]
fn fr_t2_the_command_only_prompt_is_unchanged() {
    // The opening sentence of the command-only text is the one the prompt had before FR-T2.
    let a = world(Some(CLI));
    assert!(a.instructions().contains(
        "for the user. You can message those sessions with the `ember-a2a` shell command (also at `/opt/ember/ember-a2a`, or `$EMBER_A2A_BIN`):\n\n- `ember-a2a list`"
    ));
    let b = world(None);
    assert!(b
        .instructions()
        .contains("for the user. You can message those sessions with the `ember-a2a` shell command:\n\n- `ember-a2a list`"));
}
