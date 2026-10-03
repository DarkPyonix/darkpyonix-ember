//! How agents get the browser (FR-R3) without patching them (E2).
//!
//! Agents run on the ember server, next to the browser. Each is given an off-the-shelf
//! DevTools-protocol browser MCP server pointed at ember's CDP relay,
//! `ws://<server>/api/v1/browsers/<project>/cdp`:
//!
//! - **chrome-devtools-mcp** (default): `npx -y chrome-devtools-mcp@latest --wsEndpoint=<relay>`
//! - **Playwright MCP**: `npx -y @playwright/mcp@latest --cdp-endpoint=<relay>`
//!
//! Both connect to an existing browser instead of launching their own, so the agent drives the
//! project's browser — same profile, same egress, visible in the viewer. Going through the relay
//! (rather than Chrome's port) is what lets ember show "agent is active" and pause the agent while
//! the user has taken over.
//!
//! Claude Code takes the servers via `--mcp-config <json>`; Codex via `-c mcp_servers.<name>.…`
//! overrides (or the `config` map on app-server `thread/start`). Both are plain, documented CLI
//! configuration — the agent itself is unchanged.

use serde::Serialize;
use serde_json::{json, Value};

use crate::session::StartHook;

/// MCP server name the agents see.
pub const MCP_NAME: &str = "ember-browser";

/// Which browser MCP server to hand to agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BrowserMcp {
    ChromeDevtools,
    Playwright,
}

impl BrowserMcp {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "chrome-devtools" | "chrome-devtools-mcp" => Some(BrowserMcp::ChromeDevtools),
            "playwright" | "playwright-mcp" => Some(BrowserMcp::Playwright),
            _ => None,
        }
    }

    /// `(command, args)` for the MCP server's stdio process.
    pub fn command(self, cdp_ws: &str) -> (String, Vec<String>) {
        match self {
            BrowserMcp::ChromeDevtools => (
                "npx".into(),
                vec!["-y".into(), "chrome-devtools-mcp@latest".into(), format!("--wsEndpoint={cdp_ws}")],
            ),
            BrowserMcp::Playwright => (
                "npx".into(),
                vec!["-y".into(), "@playwright/mcp@latest".into(), format!("--cdp-endpoint={cdp_ws}")],
            ),
        }
    }
}

/// The relay URL for a project, given the server's base (`http://127.0.0.1:8740`).
pub fn cdp_ws_url(base: &str, project: &str) -> String {
    let ws = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_string()
    };
    format!("{}/api/v1/browsers/{project}/cdp", ws.trim_end_matches('/'))
}

/// Ready-to-use configuration for each agent CLI.
#[derive(Debug, Clone, Serialize)]
pub struct AgentBrowserConfig {
    pub cdp_ws: String,
    pub mcp: BrowserMcp,
    /// `claude --mcp-config '<this JSON>'`.
    pub claude_code_mcp_config: Value,
    /// `codex -c <each>` (also valid as `thread/start` `config` entries, key = before `=`).
    pub codex_overrides: Vec<String>,
    /// The same as a `~/.codex/config.toml` section.
    pub codex_toml: String,
}

pub fn agent_config(cdp_ws: &str, mcp: BrowserMcp) -> AgentBrowserConfig {
    let (cmd, args) = mcp.command(cdp_ws);
    let toml_args = format!(
        "[{}]",
        args.iter().map(|a| toml_str(a)).collect::<Vec<_>>().join(", ")
    );
    AgentBrowserConfig {
        cdp_ws: cdp_ws.to_string(),
        mcp,
        claude_code_mcp_config: json!({
            "mcpServers": { MCP_NAME: { "type": "stdio", "command": cmd, "args": args } }
        }),
        codex_overrides: vec![
            format!("mcp_servers.{MCP_NAME}.command={}", toml_str(&cmd)),
            format!("mcp_servers.{MCP_NAME}.args={toml_args}"),
        ],
        codex_toml: format!(
            "[mcp_servers.{MCP_NAME}]\ncommand = {}\nargs = {toml_args}\n",
            toml_str(&cmd)
        ),
    }
}

/// A TOML basic string.
fn toml_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A session start hook exporting the project's browser to every agent process:
/// - `EMBER_BROWSER_CDP_WS` — the relay endpoint;
/// - `EMBER_BROWSER_MCP_CONFIG` — the Claude Code `--mcp-config` JSON.
///
/// Adapters (or a wrapper) turn these into the CLI flags above; the agents themselves are not
/// changed.
pub fn start_hook(base: String, mcp: BrowserMcp) -> StartHook {
    std::sync::Arc::new(move |rec| {
        if super::check_project(&rec.project).is_err() {
            return Vec::new();
        }
        let cfg = agent_config(&cdp_ws_url(&base, &rec.project), mcp);
        vec![
            ("EMBER_BROWSER_CDP_WS".into(), cfg.cdp_ws),
            ("EMBER_BROWSER_MCP_CONFIG".into(), cfg.claude_code_mcp_config.to_string()),
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configs() {
        let ws = cdp_ws_url("http://127.0.0.1:8740/", "acme");
        assert_eq!(ws, "ws://127.0.0.1:8740/api/v1/browsers/acme/cdp");
        let c = agent_config(&ws, BrowserMcp::ChromeDevtools);
        assert_eq!(
            c.claude_code_mcp_config["mcpServers"][MCP_NAME]["args"][2],
            format!("--wsEndpoint={ws}")
        );
        assert_eq!(c.codex_overrides[0], "mcp_servers.ember-browser.command=\"npx\"");
        assert!(c.codex_toml.contains("args = [\"-y\", \"chrome-devtools-mcp@latest\""));
    }
}
