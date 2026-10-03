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
//!
//! Opt-in per server: `EMBER_BROWSER_MCP=chrome-devtools|playwright|off` (default `off`). The MCP
//! server is fetched and run by a package runner found on `PATH` — `npx` (`npx -y <pkg>`), else
//! `bunx` (`bunx <pkg>`); `EMBER_BROWSER_MCP_RUNNER` names one explicitly. With no runner the
//! setting is ignored with a warning. When enabled, [`start_config_hook`] adds the server to every
//! agent start as a [`McpServer`] (the adapters turn it into the CLI flags) and exports
//! `EMBER_BROWSER_CDP_WS` / `EMBER_BROWSER_MCP_CONFIG`.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};

use crate::agents::{McpServer, StartRequest};
use crate::session::{StartConfigHook, StartHook};
use crate::store::SessionRecord;

/// How long agents should allow the MCP server to start: the runner may download it first.
pub const MCP_STARTUP_TIMEOUT_SECS: u32 = 90;

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

    /// `(command, args)` for the MCP server's stdio process, run with `npx`.
    pub fn command(self, cdp_ws: &str) -> (String, Vec<String>) {
        self.command_with(&Runner::Npx("npx".into()), cdp_ws)
    }

    /// `(command, args)` for the MCP server's stdio process, run with `runner`.
    pub fn command_with(self, runner: &Runner, cdp_ws: &str) -> (String, Vec<String>) {
        let (package, flag) = match self {
            BrowserMcp::ChromeDevtools => ("chrome-devtools-mcp@latest", format!("--wsEndpoint={cdp_ws}")),
            BrowserMcp::Playwright => ("@playwright/mcp@latest", format!("--cdp-endpoint={cdp_ws}")),
        };
        match runner {
            // `-y`: install without asking (no terminal to ask on).
            Runner::Npx(bin) => (bin.display().to_string(), vec!["-y".into(), package.into(), flag]),
            // bunx installs missing packages on its own.
            Runner::Bunx(bin) => (bin.display().to_string(), vec![package.into(), flag]),
        }
    }
}

/// The package runner that fetches and starts the MCP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Runner {
    Npx(PathBuf),
    Bunx(PathBuf),
}

impl Runner {
    /// `EMBER_BROWSER_MCP_RUNNER` (a path or name; `bunx`/`bun x` style is told apart by its file
    /// name), else `npx`, else `bunx` on `PATH`.
    pub fn find() -> Option<Runner> {
        if let Some(r) = std::env::var_os("EMBER_BROWSER_MCP_RUNNER").filter(|v| !v.is_empty()) {
            let p = PathBuf::from(r);
            let p = if p.components().count() == 1 { which(&p.to_string_lossy())? } else { p };
            return Some(Runner::of(p));
        }
        which("npx").map(Runner::Npx).or_else(|| which("bunx").map(Runner::Bunx))
    }

    fn of(p: PathBuf) -> Runner {
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if name.starts_with("bunx") {
            Runner::Bunx(p)
        } else {
            Runner::Npx(p)
        }
    }
}

/// `name` on `PATH`, as an absolute path (agents get it verbatim, whatever their own `PATH`).
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// `EMBER_BROWSER_MCP` → which server, if any. `None` = off (the default), or an unknown value,
/// or no runner found (each logged).
pub fn from_env() -> Option<(BrowserMcp, Runner)> {
    let v = std::env::var("EMBER_BROWSER_MCP").unwrap_or_default();
    let v = v.trim();
    if v.is_empty() || matches!(v, "off" | "0" | "false" | "no") {
        return None;
    }
    let Some(mcp) = BrowserMcp::parse(v) else {
        tracing::warn!("EMBER_BROWSER_MCP={v:?} is not chrome-devtools, playwright or off; agents get no browser");
        return None;
    };
    let Some(runner) = Runner::find() else {
        tracing::warn!("EMBER_BROWSER_MCP={v} needs npx or bunx on PATH (or EMBER_BROWSER_MCP_RUNNER); agents get no browser");
        return None;
    };
    Some((mcp, runner))
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
    agent_config_with(cdp_ws, mcp, &Runner::Npx("npx".into()))
}

pub fn agent_config_with(cdp_ws: &str, mcp: BrowserMcp, runner: &Runner) -> AgentBrowserConfig {
    let (cmd, args) = mcp.command_with(runner, cdp_ws);
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
pub(crate) fn toml_str(s: &str) -> String {
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

/// The browser MCP server for `project`, as the adapters take it.
pub fn mcp_server(base: &str, project: &str, mcp: BrowserMcp, runner: &Runner) -> McpServer {
    let (command, args) = mcp.command_with(runner, &cdp_ws_url(base, project));
    McpServer {
        name: MCP_NAME.into(),
        command,
        args,
        startup_timeout_secs: Some(MCP_STARTUP_TIMEOUT_SECS),
        env: Vec::new(),
    }
}

/// Give every agent the project's browser (FR-R3): adds the MCP server to the start request (the
/// Claude Code and Codex adapters turn it into `--mcp-config` / `-c mcp_servers.…`) and exports
/// `EMBER_BROWSER_CDP_WS` and `EMBER_BROWSER_MCP_CONFIG`. A session whose project is not a valid
/// browser name gets nothing.
pub fn start_config_hook(base: String, mcp: BrowserMcp, runner: Runner) -> StartConfigHook {
    std::sync::Arc::new(move |rec: &SessionRecord, req: &mut StartRequest| {
        if super::check_project(&rec.project).is_err() {
            return Ok(());
        }
        let cfg = agent_config_with(&cdp_ws_url(&base, &rec.project), mcp, &runner);
        req.env.push(("EMBER_BROWSER_CDP_WS".into(), cfg.cdp_ws));
        req.env.push(("EMBER_BROWSER_MCP_CONFIG".into(), cfg.claude_code_mcp_config.to_string()));
        req.mcp_servers.push(mcp_server(&base, &rec.project, mcp, &runner));
        Ok(())
    })
}

/// A session start hook exporting the project's browser to every agent process (environment
/// only; superseded by [`start_config_hook`], which also wires the adapters):
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

    #[test]
    fn runners_and_servers() {
        let ws = "ws://127.0.0.1:8740/api/v1/browsers/acme/cdp";
        let (cmd, args) = BrowserMcp::Playwright.command_with(&Runner::Bunx("/b/bunx".into()), ws);
        assert_eq!(cmd, "/b/bunx");
        assert_eq!(args, ["@playwright/mcp@latest".to_string(), format!("--cdp-endpoint={ws}")]);
        let s = mcp_server("http://127.0.0.1:8740", "acme", BrowserMcp::ChromeDevtools, &Runner::Npx("/n/npx".into()));
        assert_eq!(s.name, MCP_NAME);
        assert_eq!(s.command, "/n/npx");
        assert_eq!(s.args, ["-y".to_string(), "chrome-devtools-mcp@latest".into(), format!("--wsEndpoint={ws}")]);
        assert_eq!(Runner::of("/x/bunx".into()), Runner::Bunx("/x/bunx".into()));
        assert_eq!(Runner::of("/x/npx".into()), Runner::Npx("/x/npx".into()));
    }

    #[test]
    fn config_hook_adds_the_server_and_environment() {
        let hook = start_config_hook("http://127.0.0.1:8740".into(), BrowserMcp::ChromeDevtools, Runner::Npx("/n/npx".into()));
        let store = crate::store::Store::open_in_memory().unwrap();
        let rec = store.create_session("acme", crate::agents::AgentKind::ClaudeCode, "/tmp", None, "t").unwrap();
        let mut req = StartRequest::default();
        hook(&rec, &mut req).unwrap();
        assert_eq!(req.mcp_servers.len(), 1);
        assert_eq!(req.mcp_servers[0].command, "/n/npx");
        assert!(req.env.iter().any(|(k, v)| k == "EMBER_BROWSER_CDP_WS" && v.ends_with("/browsers/acme/cdp")));
        // The adapter flag carries it.
        let flag = crate::agents::claude_code::mcp_config(&req.mcp_servers).unwrap();
        assert!(flag.contains("chrome-devtools-mcp@latest"));

        let bad = store.create_session("a b", crate::agents::AgentKind::ClaudeCode, "/tmp", None, "t").unwrap();
        let mut req = StartRequest::default();
        hook(&bad, &mut req).unwrap();
        assert!(req.mcp_servers.is_empty() && req.env.is_empty());
    }
}
