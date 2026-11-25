//! Agent-to-agent messaging (SPEC §T, INTENT D5).
//!
//! Every agent process gets runtime credentials in its environment (`EMBER_URL`,
//! `EMBER_SESSION_ID`, `EMBER_RUNTIME_TOKEN`) and instructions for the `ember-a2a` command, which
//! calls the API in [`api`] (FR-T2). A message is stored first, then delivered through
//! [`Sessions::send`], so it enters the target exactly like a user message, behind a header
//! naming the sender (FR-T3). A target that is mid-turn gets its queued messages at the next turn
//! boundary; an idle or released target is woken by the delivery; undelivered messages survive a
//! restart (FR-T4). Sends are limited per session and per pair within a sliding window (FR-T5),
//! and can be switched off server-wide or per session (FR-T6).
//!
//! Users mention other sessions from the composer ([`mention`], FR-T6), and a leader session
//! runs a team of teammate sessions with a shared task list and mailbox ([`team`], FR-T7).

pub mod api;
pub mod channel;
pub mod client;
pub mod mcp;
pub mod mention;
pub mod report;
pub mod store;
pub mod team;
pub mod terminal;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use serde::Serialize;
use tokio::sync::broadcast::error::RecvError;

use crate::agents::{AgentKind, McpServer, StartRequest};
use crate::events::{AgentEvent, SessionStatus};
use crate::session::{Push, Sessions};
use crate::store::SessionRecord;
pub use channel::ChannelReply;
pub use store::{A2aMessage, A2aStore, TokenTimes};

/// Name of the agent-side command (a bin target of this crate).
pub const CLI_NAME: &str = "ember-a2a";

/// Name of the MCP server that carries the `channel_post` tool (FR-T12, #380).
pub const CHANNEL_MCP_SERVER: &str = "ember-channels";
/// The tool as Claude Code names it: `mcp__<server>__<tool>`. A literal, because constants cannot
/// be concatenated; a unit test builds it from [`CHANNEL_MCP_SERVER`] and [`mcp::TOOL_NAME`] and
/// compares, so the three cannot drift apart. It is the one tool the allow rule names.
pub const CHANNEL_POST_TOOL: &str = "mcp__ember-channels__channel_post";

/// The one notice a session gets when its start skipped the channel tool (FR-T12).
const CHANNEL_FALLBACK_NOTICE: &str = "The channel tool could not be registered because a MCP server named ember-channels is already configured; replies use the ember-a2a command.";

/// How many posts one session may make into one channel per minute (FR-T12, `channel post`).
pub const CHANNEL_POSTS_PER_MINUTE: u32 = 6;

/// How long after a person's mention an agent may post in that channel (FR-T12).
pub const CHANNEL_MENTION_WINDOW: Duration = Duration::from_secs(30 * 60);
/// How long a mention thread stays open while the agent has not answered it (FR-T12, #380,
/// INTENT D51): past the window, an agent may still post its first answer to its latest mention
/// in a channel for this long.
pub const CHANNEL_THREAD_MAX: Duration = Duration::from_secs(24 * 60 * 60);
/// Marks the first line of every delivered message.
pub const HEADER_TAG: &str = "[ember a2a]";
/// Marks the first line of every scheduled prompt (FR-A8).
const SCHEDULE_TAG: &str = crate::schedules::HEADER_TAG;

/// Loop protection limits (FR-T5).
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub window: Duration,
    /// Messages one session may send within `window`.
    pub per_session: u32,
    /// Messages between one pair of sessions, either direction, within `window`.
    pub per_pair: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            window: Duration::from_secs(600),
            per_session: 20,
            per_pair: 10,
        }
    }
}

#[derive(Debug, Clone)]
pub struct A2aConfig {
    /// Base URL agents use to reach this server, e.g. `http://127.0.0.1:8740`.
    pub base_url: String,
    /// Absolute path of the `ember-a2a` binary, if it is installed. Its directory is put first
    /// on the agents' `PATH`.
    pub cli_path: Option<PathBuf>,
    pub limits: Limits,
    /// Server-wide switch used until a user sets one through the API.
    pub enabled_by_default: bool,
    /// Timings of FR-T8 reports (#995).
    pub reports: report::ReportTimes,
    /// Runtime token lifetimes (FR-T2a, #102).
    pub tokens: TokenTimes,
}

impl A2aConfig {
    pub fn new(base_url: impl Into<String>) -> A2aConfig {
        A2aConfig {
            base_url: base_url.into(),
            cli_path: None,
            limits: Limits::default(),
            enabled_by_default: true,
            reports: report::ReportTimes::default(),
            tokens: TokenTimes::from_env(),
        }
    }

    /// `ember-a2a` next to the running executable, if present.
    pub fn cli_next_to_current_exe() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let cli = exe.parent()?.join(CLI_NAME);
        cli.is_file().then_some(cli)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum A2aError {
    #[error("missing or invalid runtime token")]
    Unauthorized,
    #[error("{0}")]
    Disabled(String),
    #[error("{0}")]
    RateLimited(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    BadRequest(String),
    /// Not allowed for this caller (team roles and scope, FR-T7).
    #[error("{0}")]
    Forbidden(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Moves a session to a computer: `(session id, computer id)`. Installed by `main.rs` from
/// `crate::computers::Computers::switch`, so a teammate can be spawned on a chosen computer.
pub type ComputerSetter =
    Arc<dyn Fn(String, String) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

/// Checks that a directory exists on a computer before a teammate is spawned there:
/// `(computer id, absolute path)`, `Err` with the reason when it does not (FR-T7, #82). Installed
/// by `main.rs` from the computer's node (`stat`).
pub type DirChecker =
    Arc<dyn Fn(String, String) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

/// Where a session runs: `(designated computer id, current computer id)`, each `None` when
/// nothing is recorded (the session runs on the main server). Installed by `main.rs` from
/// `crate::computers::Computers::placement`, so team views show where each member is (FR-T7).
pub type PlacementLookup = Arc<dyn Fn(&str) -> (Option<String>, Option<String>) + Send + Sync>;

/// A session the caller may message.
#[derive(Debug, Clone, Serialize)]
pub struct Target {
    pub id: String,
    pub title: String,
    pub project: String,
    pub agent: String,
    pub status: SessionStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// Entered the target session now.
    Delivered,
    /// The target is mid-turn (or could not be started); it gets the message at its next turn
    /// boundary.
    Queued,
}

#[derive(Debug, Clone, Serialize)]
pub struct Receipt {
    pub id: String,
    pub to: String,
    pub status: Delivery,
}

pub struct A2a {
    sessions: Arc<Sessions>,
    store: A2aStore,
    config: A2aConfig,
    /// Serialises the limit check and insert of sends.
    send_lock: tokio::sync::Mutex<()>,
    /// One delivery at a time per target.
    flush_locks: StdMutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Targets we delivered to whose turn has not ended yet, with their last event seq before
    /// the delivery. Guards against delivering twice before the store shows the turn running.
    awaiting: StdMutex<HashMap<String, i64>>,
    /// Last loop-protection notice per (from, to), so a retrying agent does not flood both
    /// transcripts.
    last_notice: StdMutex<HashMap<(String, String), Instant>>,
    computer_setter: StdMutex<Option<ComputerSetter>>,
    /// FR-T8 delegators, in-flight reports and what was already reported (#995).
    report_state: StdMutex<report::ReportState>,
    /// Computer and account label of a reporting session (#82 P3).
    report_context: StdMutex<Option<report::ContextLookup>>,
    dir_checker: StdMutex<Option<DirChecker>>,
    placement: StdMutex<Option<PlacementLookup>>,
    /// Recent channel posts per (session, channel), for the rate limit of `channel post`.
    channel_posts: StdMutex<HashMap<(String, String), std::collections::VecDeque<Instant>>>,
    /// Sessions whose last start skipped the `ember-channels` registration because the user's own
    /// configuration of the harness already names that server (FR-T12). The prompt builders read it
    /// through [`A2a::channel_reply`]. It lives in memory: after a server restart it is
    /// empty, so a surviving agent is again told "by tool" (its prompt already carries the CLI
    /// fallback sentence) until its next start runs the check again. An entry is removed when the
    /// session's start finds the name free, when the session's team membership ends (end hook)
    /// and when the session moves to another computer.
    channel_fallback: StdMutex<HashSet<String>>,
    /// Where the collision check looks outside the session directory.
    claude_sources: StdMutex<ClaudeConfigSources>,
}

/// Fixed places the collision check reads besides the session's directories (FR-T12).
#[derive(Debug, Clone, Default)]
pub struct ClaudeConfigSources {
    /// `$HOME`, whose `.claude.json` is Claude Code's user file.
    pub home: Option<PathBuf>,
    /// `CLAUDE_CONFIG_DIR` of this server's own environment (a value in the start request wins).
    pub config_dir: Option<PathBuf>,
    /// Managed MCP files.
    pub managed: Vec<PathBuf>,
    /// `CODEX_HOME` of this server's own environment (a value in the start request wins; without
    /// either it is `<home>/.codex`).
    pub codex_home: Option<PathBuf>,
    /// Codex configuration files outside the user's home.
    pub codex_system: Vec<PathBuf>,
    /// Codex managed preference locations ([`codex_managed_locations`]); presence alone makes
    /// the channel tool unverified (FR-T12).
    pub codex_managed: Vec<PathBuf>,
}

impl ClaudeConfigSources {
    pub fn from_env() -> ClaudeConfigSources {
        ClaudeConfigSources {
            home: std::env::var_os("HOME").filter(|v| !v.is_empty()).map(PathBuf::from),
            config_dir: std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()).map(PathBuf::from),
            managed: vec![
                PathBuf::from("/Library/Application Support/ClaudeCode/managed-mcp.json"),
                PathBuf::from("/etc/claude-code/managed-mcp.json"),
            ],
            codex_home: std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()).map(PathBuf::from),
            codex_system: vec![
                PathBuf::from("/etc/codex/config.toml"),
                PathBuf::from("/etc/codex/managed_config.toml"),
            ],
            codex_managed: codex_managed_locations(),
        }
    }
}

/// Largest configuration file the collision check reads; a bigger one counts as unreadable.
const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;

const KIND_PROJECT: &str = "project .mcp.json";
const KIND_USER: &str = "user config";
const KIND_LOCAL: &str = "local config";
const KIND_MANAGED: &str = "managed";
const KIND_REQUEST: &str = "start request";
const KIND_CODEX_USER: &str = "user config.toml";
const KIND_CODEX_PROJECT: &str = "project .codex/config.toml";
const KIND_CODEX_SYSTEM: &str = "system config.toml";
const KIND_AGY_USER: &str = "user mcp_config.json";
const KIND_AGY_PROJECT: &str = "project .agents/mcp_config.json";
const REASON_TAKEN: &str = "a server with this name is already configured";
const KIND_CODEX_PROFILE: &str = "profile <name>.config.toml";
const KIND_CODEX_PLUGIN: &str = "plugin .mcp.json or plugin.json";
const KIND_CODEX_MANAGED: &str = "managed preferences";
/// Logged when a Codex session gets the server without its auto-approval (FR-T12).
const REASON_UNVERIFIED: &str =
    "a Codex configuration layer the name check does not read is present, so the server name is unverified and the call is asked";

/// A Codex configuration layer that [`codex_channel_name_taken`] does not read (FR-T12). While
/// one may exist, which `ember-channels` entry wins is unknown, so `channel_post` is not
/// auto-approved. `unreadable` is true when the location could not be checked (fail closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnreadLayer {
    pub kind: &'static str,
    pub unreadable: bool,
}

/// Where [`codex_unread_layer`] looks. Plain paths, so tests use a temp dir.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexLayerPaths {
    /// Directories whose `*.config.toml` files other than `config.toml` are profile layers.
    pub profile_dirs: Vec<PathBuf>,
    /// Plugin roots searched for `.mcp.json` and `plugin.json`, up to three levels deep.
    pub plugin_roots: Vec<PathBuf>,
    /// Managed preference files or directories.
    pub managed: Vec<PathBuf>,
}

/// How deep below a plugin root the search for `.mcp.json` and `plugin.json` goes.
const PLUGIN_DEPTH: usize = 3;

/// The first Codex layer of `paths` that is present or cannot be checked. Presence only: no file
/// is opened or parsed. A missing location is fine; any other error counts as present.
pub fn codex_unread_layer(paths: &CodexLayerPaths) -> Option<UnreadLayer> {
    use std::io::ErrorKind::NotFound;
    let found = |kind| Some(UnreadLayer { kind, unreadable: false });
    let unreadable = |kind| Some(UnreadLayer { kind, unreadable: true });
    for dir in &paths.profile_dirs {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == NotFound => continue,
            Err(_) => return unreadable(KIND_CODEX_PROFILE),
        };
        for entry in entries {
            let Ok(entry) = entry else { return unreadable(KIND_CODEX_PROFILE) };
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".config.toml") && name != "config.toml" {
                return found(KIND_CODEX_PROFILE);
            }
        }
    }
    /// `Ok(true)` when `dir` holds a plugin file within `depth` levels. Symlinks are not followed.
    fn plugin_file(dir: &Path, depth: usize) -> Result<bool, ()> {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err(()),
        };
        for entry in entries {
            let entry = entry.map_err(|_| ())?;
            let name = entry.file_name();
            if name == ".mcp.json" || name == "plugin.json" {
                return Ok(true);
            }
            let ty = entry.file_type().map_err(|_| ())?;
            if ty.is_dir() && depth > 1 && plugin_file(&entry.path(), depth - 1)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    for root in &paths.plugin_roots {
        match plugin_file(root, PLUGIN_DEPTH) {
            Ok(true) => return found(KIND_CODEX_PLUGIN),
            Ok(false) => {}
            Err(()) => return unreadable(KIND_CODEX_PLUGIN),
        }
    }
    for path in &paths.managed {
        match std::fs::symlink_metadata(path) {
            Ok(_) => return found(KIND_CODEX_MANAGED),
            Err(e) if e.kind() == NotFound => {}
            Err(_) => return unreadable(KIND_CODEX_MANAGED),
        }
    }
    None
}

/// The locations [`codex_unread_layer`] checks for a session whose Codex home is `codex_home`:
/// the profile files and the plugins under it, and the `managed` locations.
pub fn codex_layer_paths(codex_home: Option<&Path>, managed: &[PathBuf]) -> CodexLayerPaths {
    CodexLayerPaths {
        profile_dirs: codex_home.map(Path::to_path_buf).into_iter().collect(),
        plugin_roots: codex_home.map(|h| h.join("plugins")).into_iter().collect(),
        managed: managed.to_vec(),
    }
}

/// Codex's managed preference locations on this OS: on macOS the `com.openai.codex` preference
/// files (machine-wide managed, per-user managed and machine-wide), on Windows
/// `%ProgramData%\OpenAI\Codex`, elsewhere none (the `/etc/codex` files are read by the name
/// check). The exact locations Codex reads are NOT MEASURED.
pub fn codex_managed_locations() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if cfg!(target_os = "macos") {
        let managed = PathBuf::from("/Library/Managed Preferences");
        out.push(managed.join("com.openai.codex.plist"));
        if let Some(user) = std::env::var_os("USER").filter(|u| !u.is_empty()) {
            out.push(managed.join(user).join("com.openai.codex.plist"));
        }
        out.push(PathBuf::from("/Library/Preferences/com.openai.codex.plist"));
    } else if cfg!(windows) {
        let base = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        out.push(base.join("OpenAI").join("Codex"));
    }
    out
}

/// The closed set of reasons a configuration file could not be checked. Static on purpose:
/// nothing read from a file reaches a log line or a notice.
const WHY_UNREADABLE: &str = "unreadable";
const WHY_NOT_JSON: &str = "not valid JSON";
const WHY_TOO_BIG: &str = "larger than 4 MiB";
const WHY_NOT_REGULAR: &str = "not a regular file";

/// What the collision check found (FR-T12). `Taken` and `Unsure` both skip the registration (the
/// check fails closed); they differ in what the session notice and the log say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameCheck {
    /// Sure that no configuration source names the server.
    Free,
    /// A source defines a server of that name. `kind` is the kind of source.
    Taken { kind: &'static str },
    /// A source exists but could not be checked. `why` is one of the `WHY_*` texts.
    Unsure { kind: &'static str, why: &'static str },
}

/// The notice for a start that skipped the registration because a file could not be checked.
#[cfg(test)]
fn unsure_notice(kind: &str, why: &str) -> String {
    unsure_notice_for(ToolHarness::Claude, kind, why)
}

/// [`unsure_notice`] naming the harness whose configuration could not be checked.
fn unsure_notice_for(harness: ToolHarness, kind: &str, why: &str) -> String {
    format!(
        "The channel tool was not registered because the {} configuration ({kind}) could not be checked ({why}); replies use the ember-a2a command.",
        harness.label()
    )
}

/// The harnesses that get the `channel_post` MCP tool (FR-T12), each through its own
/// session-scoped mechanism: Claude Code's private `--mcp-config` file, Codex's `-c` overrides,
/// the Antigravity session root's `mcp_config.json`, and `mcpServers` of an ACP `session/new`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolHarness {
    Claude,
    Codex,
    Antigravity,
    Acp,
}

impl ToolHarness {
    /// `None` for the scripted test agent, which has no MCP client.
    fn of(agent: AgentKind) -> Option<ToolHarness> {
        match agent {
            AgentKind::ClaudeCode => Some(ToolHarness::Claude),
            AgentKind::Codex => Some(ToolHarness::Codex),
            AgentKind::Antigravity => Some(ToolHarness::Antigravity),
            AgentKind::Acp(_) => Some(ToolHarness::Acp),
            AgentKind::Scripted => None,
        }
    }

    /// How the prompt names the tool. Only Claude Code's combined name is measured.
    fn reply(self) -> ChannelReply {
        match self {
            ToolHarness::Claude => ChannelReply::ClaudeTool,
            ToolHarness::Codex | ToolHarness::Antigravity | ToolHarness::Acp => ChannelReply::McpTool,
        }
    }

    fn label(self) -> &'static str {
        match self {
            ToolHarness::Claude => "Claude Code",
            ToolHarness::Codex => "Codex",
            ToolHarness::Antigravity => "Antigravity",
            ToolHarness::Acp => "ACP agent",
        }
    }

    /// Whether the start request carries the one allow rule. Claude Code denies the call without
    /// it (measured), and Codex could only ask through `mcpServer/elicitation/request`, which
    /// Ember does not answer. Antigravity's call goes through Ember's hook and an ACP agent's
    /// through `session/request_permission`, so the person is asked there and no rule is added.
    fn allow_rule(self) -> bool {
        matches!(self, ToolHarness::Claude | ToolHarness::Codex)
    }
}

/// `Ok(None)` for a missing file, `Ok(Some(json))` for a regular JSON file of at most
/// [`MAX_CONFIG_BYTES`], `Err(why)` for everything else (cannot be sure). The file is opened
/// without blocking and judged by `fstat` on the open handle, so a path swapped for a FIFO
/// between a check and the open cannot stall the session start. A symlink is followed on purpose
/// (dotfile managers link these files).
fn read_config_json(path: &Path) -> Result<Option<serde_json::Value>, &'static str> {
    read_config_bytes(path)?
        .map(|buf| serde_json::from_slice(&buf).map_err(|_| WHY_NOT_JSON))
        .transpose()
}

/// [`read_config_json`] without the parsing: the bytes of a regular file of at most
/// [`MAX_CONFIG_BYTES`], `Ok(None)` for a missing file.
fn read_config_bytes(path: &Path) -> Result<Option<Vec<u8>>, &'static str> {
    use std::io::Read;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NONBLOCK);
    }
    let file = match opts.open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(WHY_UNREADABLE),
    };
    let meta = file.metadata().map_err(|_| WHY_UNREADABLE)?;
    if !meta.is_file() {
        return Err(WHY_NOT_REGULAR);
    }
    if meta.len() > MAX_CONFIG_BYTES {
        return Err(WHY_TOO_BIG);
    }
    let mut buf = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut buf).map_err(|_| WHY_UNREADABLE)?;
    if buf.len() as u64 > MAX_CONFIG_BYTES {
        return Err(WHY_TOO_BIG);
    }
    Ok(Some(buf))
}

/// Whether `bytes` contain [`CHANNEL_MCP_SERVER`] anywhere, ignoring ASCII case. Used for the
/// configuration files of Codex (TOML) and Antigravity (JSON that may hold comments and trailing
/// commas): a plain text search needs no parser and fails closed, since a comment that mentions
/// the name counts as taken. The text is searched as written and again with its ASCII escapes
/// decoded ([`unescape_ascii`]), because a quoted key may spell a character as an escape.
fn mentions_channel_server(bytes: &[u8]) -> bool {
    let needle = CHANNEL_MCP_SERVER.as_bytes();
    let found = |text: &[u8]| text.windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle));
    found(bytes) || found(&unescape_ascii(bytes))
}

/// `bytes` with every `\uXXXX`, `\UXXXXXXXX` and `\xHH` escape of an ASCII character replaced by
/// that character. TOML and JSON accept such an escape in a quoted key, so
/// `"ember-channels"` is the key `ember-channels`. Everything else is kept as it is, also a
/// sequence that is no escape where it stands (a literal string, a path): decoding too much can
/// only report a name that is not there, which fails closed.
fn unescape_ascii(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let digits = match bytes[i + 1] {
                b'u' => 4,
                b'U' => 8,
                b'x' => 2,
                _ => 0,
            };
            let end = i + 2 + digits;
            if digits > 0 && end <= bytes.len() {
                let code = std::str::from_utf8(&bytes[i + 2..end]).ok().and_then(|hex| u32::from_str_radix(hex, 16).ok());
                if let Some(c) = code.filter(|c| *c < 0x80) {
                    out.push(c as u8);
                    i = end;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// The first file of `files` that mentions the server name or cannot be checked decides.
fn text_name_check(files: Vec<(PathBuf, &'static str)>) -> NameCheck {
    for (file, kind) in files {
        match read_config_bytes(&file) {
            Err(why) => return NameCheck::Unsure { kind, why },
            Ok(Some(bytes)) if mentions_channel_server(&bytes) => return NameCheck::Taken { kind },
            Ok(_) => {}
        }
    }
    NameCheck::Free
}

/// `cwd` (made absolute) and every ancestor, then the same for its canonical form.
fn session_dirs(cwd: &Path) -> Vec<PathBuf> {
    let cwd = if cwd.is_absolute() {
        cwd.to_path_buf()
    } else {
        std::env::current_dir().map(|d| d.join(cwd)).unwrap_or_else(|_| cwd.to_path_buf())
    };
    let mut dirs: Vec<PathBuf> = cwd.ancestors().map(Path::to_path_buf).collect();
    if let Ok(real) = cwd.canonicalize() {
        for a in real.ancestors() {
            if !dirs.iter().any(|d| d == a) {
                dirs.push(a.to_path_buf());
            }
        }
    }
    dirs
}

/// Whether the user's own Codex configuration mentions an MCP server named `ember-channels`
/// (FR-T12): `.codex/config.toml` in `cwd` and every ancestor, `<codex_home>/config.toml`, and
/// the `system` files. A `-c mcp_servers.ember-channels.…` override would merge into such an
/// entry, so the registration steps aside. Fails closed like [`channel_server_name_taken`].
pub fn codex_channel_name_taken(cwd: &Path, codex_home: Option<&Path>, system: &[PathBuf]) -> NameCheck {
    let mut files: Vec<(PathBuf, &'static str)> = session_dirs(cwd)
        .into_iter()
        .map(|d| (d.join(".codex").join("config.toml"), KIND_CODEX_PROJECT))
        .collect();
    if let Some(home) = codex_home {
        files.push((home.join("config.toml"), KIND_CODEX_USER));
    }
    files.extend(system.iter().map(|f| (f.clone(), KIND_CODEX_SYSTEM)));
    text_name_check(files)
}

/// Whether the user's own Antigravity configuration mentions an MCP server named
/// `ember-channels` (FR-T12): `.agents/mcp_config.json` in `cwd` and every ancestor, and under
/// `home` the user file `.gemini/config/mcp_config.json` and the legacy
/// `.gemini/antigravity/mcp_config.json` (agy has no variable that moves `~/.gemini`). Fails
/// closed like [`channel_server_name_taken`].
pub fn agy_channel_name_taken(cwd: &Path, home: Option<&Path>) -> NameCheck {
    let mut files: Vec<(PathBuf, &'static str)> = session_dirs(cwd)
        .into_iter()
        .map(|d| (d.join(".agents").join("mcp_config.json"), KIND_AGY_PROJECT))
        .collect();
    if let Some(home) = home {
        let gemini = home.join(".gemini");
        files.push((gemini.join("config").join("mcp_config.json"), KIND_AGY_USER));
        files.push((gemini.join("antigravity").join("mcp_config.json"), KIND_AGY_USER));
    }
    text_name_check(files)
}

/// Whether the `mcpServers` object `servers` has a key equal to [`CHANNEL_MCP_SERVER`], ignoring
/// ASCII case.
fn names_channel_server(servers: Option<&serde_json::Value>) -> bool {
    servers
        .and_then(|v| v.as_object())
        .is_some_and(|m| m.keys().any(|k| k.eq_ignore_ascii_case(CHANNEL_MCP_SERVER)))
}

/// Whether the user's own Claude Code configuration already defines an MCP server named
/// `ember-channels` (FR-T12). [`NameCheck::Free`] only when it is sure there is no such server. It
/// fails closed: a file that exists but cannot be read, is not valid JSON, is over 4 MiB or is
/// not a regular file gives [`NameCheck::Unsure`]; a missing file is fine.
///
/// Sources: `.mcp.json` in `cwd` and every ancestor; `<config_dir>/.claude.json` and
/// `<home>/.claude.json` (Claude Code keeps its user file at `$CLAUDE_CONFIG_DIR/.claude.json`
/// when that variable is set and at `$HOME/.claude.json` otherwise), key `mcpServers` (user
/// scope) and `projects.<dir>.mcpServers` (local scope) for `cwd` and every ancestor; the
/// `managed` files, key `mcpServers`. Claude Code's own precedence between these is not measured.
pub fn channel_server_name_taken(
    cwd: &Path,
    home: Option<&Path>,
    config_dir: Option<&Path>,
    managed: &[PathBuf],
) -> NameCheck {
    let dirs = session_dirs(cwd);
    for d in &dirs {
        match read_config_json(&d.join(".mcp.json")) {
            Err(why) => return NameCheck::Unsure { kind: KIND_PROJECT, why },
            Ok(Some(v)) if names_channel_server(v.get("mcpServers")) => {
                return NameCheck::Taken { kind: KIND_PROJECT };
            }
            Ok(_) => {}
        }
    }
    let user_files = config_dir.iter().chain(home.iter()).map(|d| d.join(".claude.json"));
    for file in user_files {
        match read_config_json(&file) {
            Err(why) => return NameCheck::Unsure { kind: KIND_USER, why },
            Ok(None) => {}
            Ok(Some(v)) => {
                if names_channel_server(v.get("mcpServers")) {
                    return NameCheck::Taken { kind: KIND_USER };
                }
                if let Some(projects) = v.get("projects").and_then(|p| p.as_object()) {
                    for d in &dirs {
                        let hit = d.to_str().and_then(|k| projects.get(k));
                        if names_channel_server(hit.and_then(|p| p.get("mcpServers"))) {
                            return NameCheck::Taken { kind: KIND_LOCAL };
                        }
                    }
                }
            }
        }
    }
    for file in managed {
        match read_config_json(file) {
            Err(why) => return NameCheck::Unsure { kind: KIND_MANAGED, why },
            Ok(Some(v)) if names_channel_server(v.get("mcpServers")) => {
                return NameCheck::Taken { kind: KIND_MANAGED };
            }
            Ok(_) => {}
        }
    }
    NameCheck::Free
}

const NOTICE_EVERY: Duration = Duration::from_secs(60);

fn is_busy(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::Running | SessionStatus::WaitingForApproval
    )
}

impl A2a {
    pub fn new(sessions: Arc<Sessions>, store: A2aStore, config: A2aConfig) -> Arc<A2a> {
        store.set_token_times(config.tokens);
        Arc::new(A2a {
            sessions,
            store,
            config,
            send_lock: tokio::sync::Mutex::new(()),
            flush_locks: StdMutex::new(HashMap::new()),
            awaiting: StdMutex::new(HashMap::new()),
            last_notice: StdMutex::new(HashMap::new()),
            computer_setter: StdMutex::new(None),
            report_state: StdMutex::new(report::ReportState::default()),
            report_context: StdMutex::new(None),
            dir_checker: StdMutex::new(None),
            placement: StdMutex::new(None),
            channel_posts: StdMutex::new(HashMap::new()),
            channel_fallback: StdMutex::new(HashSet::new()),
            claude_sources: StdMutex::new(ClaudeConfigSources::from_env()),
        })
    }

    /// Let `team spawn` put teammates on a chosen computer.
    pub fn set_computer_setter(&self, setter: ComputerSetter) {
        *self.computer_setter.lock().unwrap() = Some(setter);
    }

    /// Let `team spawn --computer` refuse a computer without the project directory (FR-T7, #82).
    pub fn set_dir_checker(&self, checker: DirChecker) {
        *self.dir_checker.lock().unwrap() = Some(checker);
    }

    /// Let FR-T8 reports name the computer and the account label (#82 P3).
    pub fn set_report_context(&self, lookup: report::ContextLookup) {
        *self.report_context.lock().unwrap() = Some(lookup);
    }

    /// Let team views name each member's designated and current computer (FR-T7, #82).
    pub fn set_placement_lookup(&self, lookup: PlacementLookup) {
        *self.placement.lock().unwrap() = Some(lookup);
    }

    /// Hook into the session manager: give every agent process its credentials and
    /// instructions, deliver queued messages at turn boundaries, and deliver whatever was still
    /// queued when the server last stopped. Call once, inside the runtime.
    ///
    /// It is [`A2a::install_hooks`] followed by [`A2a::start`]. The server binary calls the two
    /// apart, with `Sessions::recover_after_restart` between them (FR-S8).
    pub fn install(self: &Arc<Self>) {
        self.install_hooks();
        self.start();
    }

    /// The hooks alone: nothing is delivered and no task starts. A session started or rejoined
    /// after this gets its A2A credentials and instructions.
    pub fn install_hooks(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        self.sessions.add_start_hook(Arc::new(move |rec| {
            weak.upgrade()
                .map(|a| a.runtime_env(rec))
                .unwrap_or_default()
        }));
        let weak = Arc::downgrade(self);
        self.sessions.add_instructions_hook(Arc::new(move |rec: &SessionRecord| {
            weak.upgrade().map(|a| a.instructions_for(rec))
        }));
        // FR-T12 (#380): Claude Code, Codex, Antigravity and ACP agents answer a mention through
        // the `channel_post` MCP tool.
        let weak = Arc::downgrade(self);
        self.sessions.add_start_config_hook(Arc::new(move |rec: &SessionRecord, req: &mut StartRequest| {
            if let Some(a) = weak.upgrade() {
                a.channel_tool_config(rec, req);
            }
            Ok(())
        }));
        self.install_end_hook();
        // FR-T2a (#102): a token lives only as long as the agent run that got it.
        let weak = Arc::downgrade(self);
        self.sessions.add_run_end_hook(Arc::new(move |id: &str| {
            if let Some(a) = weak.upgrade() {
                a.revoke_tokens(id, "its agent run ended");
            }
        }));
        // Mentions in user messages (FR-T6).
        let weak = Arc::downgrade(self);
        self.sessions.add_user_message_hook(Arc::new(move |session: String, text: String| {
            let weak = weak.clone();
            Box::pin(async move {
                if let Some(a) = weak.upgrade() {
                    a.deliver_mentions(&session, &text).await;
                }
            }) as BoxFuture<'static, ()>
        }));
    }

    /// Deliver at turn boundaries from now on, and deliver what was still queued when the server
    /// last stopped. Call once, inside the runtime, after [`A2a::install_hooks`] and after the
    /// session statuses are settled (`Sessions::recover_after_restart`): the first flush reads
    /// them, and a turn that ended before this call is covered by that flush.
    pub fn start(self: &Arc<Self>) {
        // Subscribe before the initial flush so no turn end is missed in between.
        let rx = self.sessions.subscribe();
        tokio::spawn(watch_turns(Arc::downgrade(self), rx));
        let this = self.clone();
        tokio::spawn(async move { this.flush_all().await });
        // FR-T8 stall reports (#995).
        let weak = Arc::downgrade(self);
        let every = self.config.reports.stall_check;
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.tick().await;
            loop {
                tick.tick().await;
                let Some(this) = weak.upgrade() else { return };
                this.check_stalls().await;
            }
        });
        // FR-T2a (#102): live runs keep their tokens; the absolute lifetime rotates them.
        let weak = Arc::downgrade(self);
        let every = self.config.tokens.settled().check;
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let Some(this) = weak.upgrade() else { return };
                this.check_tokens().await;
            }
        });
    }

    /// Revoke every runtime token of `id` (FR-T2a). Never logs a token.
    fn revoke_tokens(&self, id: &str, why: &str) {
        match self.store.revoke_session_tokens(id) {
            Ok(0) => {}
            Ok(n) => tracing::debug!(session = %id, revoked = n, "A2A token revoked: {why}"),
            Err(e) => tracing::warn!(session = %id, "revoking A2A tokens failed: {e:#}"),
        }
    }

    /// Server start (FR-T2a, FR-S8): only an agent that outlived the previous server and was
    /// rejoined, or whose interrupted turn is being drained, can still hold a runtime token. Every
    /// other session's tokens are revoked; the kept ones count as just seen, so a long downtime
    /// does not idle-expire a surviving agent's token. Call after
    /// [`Sessions::recover_after_restart`] and before the listeners serve agents.
    pub fn settle_tokens_after_restart(&self, recovery: &crate::session::Recovery) {
        let mut keep = recovery.rejoined.clone();
        keep.extend(recovery.draining.iter().cloned());
        match self.store.revoke_tokens_except(&keep) {
            Ok(0) => {}
            Ok(n) => tracing::info!("revoked {n} A2A token(s) of agents that did not outlive the previous server"),
            Err(e) => tracing::warn!("revoking stale A2A tokens failed: {e:#}"),
        }
        let now = store::now_ms();
        for id in &keep {
            if let Err(e) = self.store.touch_session_tokens(id, now) {
                tracing::warn!(session = %id, "refreshing a surviving agent's A2A token failed: {e:#}");
            }
        }
    }

    /// One liveness check of the runtime tokens (FR-T2a), run every [`TokenTimes::check`]:
    ///
    /// - a session whose agent run is alive (or whose surviving turn is being drained) has its
    ///   tokens marked as seen, so the idle lifetime never ends a token under a live run;
    /// - a session whose shim died has its tokens revoked (the run is gone; the next start mints);
    /// - a live run past the absolute lifetime is released when it is between turns, so its next
    ///   start mints a fresh token; mid-turn its tokens are held for another idle lifetime instead,
    ///   so a turn is never cut off;
    /// - tokens that can no longer pass are deleted.
    pub async fn check_tokens(&self) {
        let times = self.store.token_times();
        let now = store::now_ms();
        let max_ms = times.max.as_millis().min(i64::MAX as u128) as i64;
        let idle_ms = times.idle.as_millis().min(i64::MAX as u128) as i64;
        let held = match self.store.token_sessions() {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("reading A2A tokens failed: {e:#}");
                return;
            }
        };
        for t in held {
            let id = t.session_id.as_str();
            let draining = self.sessions.is_draining(id);
            if !draining && !self.sessions.is_live(id).await {
                continue;
            }
            if !draining && crate::agents::proc::shim_died(id) {
                self.revoke_tokens(id, "its agent's shim died");
                continue;
            }
            if let Err(e) = self.store.touch_session_tokens(id, now) {
                tracing::warn!(session = %id, "refreshing A2A tokens failed: {e:#}");
            }
            if now < t.oldest_created_at.saturating_add(max_ms) {
                continue;
            }
            let busy = draining
                || self.sessions.store().session(id).ok().flatten().is_some_and(|r| is_busy(r.status));
            if busy {
                if let Err(e) = self.store.hold_session_tokens(id, now.saturating_add(idle_ms)) {
                    tracing::warn!(session = %id, "holding A2A tokens failed: {e:#}");
                }
            } else {
                tracing::info!(session = %id, "A2A token reached its absolute lifetime; releasing the idle agent so its next start gets a fresh one");
                if let Err(e) = self.sessions.release(id).await {
                    tracing::warn!(session = %id, "releasing for A2A token rotation failed: {e}");
                }
            }
        }
        if let Err(e) = self.store.sweep_expired_tokens(now) {
            tracing::warn!("sweeping expired A2A tokens failed: {e:#}");
        }
    }

    /// Drop a session's `channel_fallback` entry when the session ends (its team member is ended),
    /// so the set does not keep ids of sessions that are gone. The sessions API has no other
    /// end or delete path to hook. A later start decides again.
    fn install_end_hook(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        self.sessions.add_end_hook(Arc::new(move |id: &str| {
            if let Some(a) = weak.upgrade() {
                a.channel_fallback.lock().unwrap().remove(id);
                // FR-T2a: a session ended for good keeps no token.
                a.revoke_tokens(id, "its session ended");
            }
        }));
    }

    pub fn store(&self) -> &A2aStore {
        &self.store
    }

    /// How `rec` is told to answer a channel mention: through the `channel_post` MCP tool, under
    /// the name its harness is known to give it, or with the `ember-a2a channel post` command
    /// (FR-T12, FR-T13). Every harness with an MCP client gets the tool ([`ToolHarness`]), only
    /// with the `ember-a2a` binary installed, and only on this computer: on another computer the
    /// MCP child would need the binary there, so those sessions keep the command. The start hook
    /// and both prompt builders ask this one function, so they cannot disagree. A lookup error
    /// counts as another computer. A session whose last start found the server name taken in the
    /// user's own configuration of the harness gets the command (see `channel_fallback`).
    fn channel_reply(&self, rec: &SessionRecord) -> ChannelReply {
        match ToolHarness::of(rec.agent) {
            Some(harness)
                if self.channel_reply_eligible(rec)
                    && !self.channel_fallback.lock().unwrap().contains(&rec.id) =>
            {
                harness.reply()
            }
            _ => ChannelReply::Command,
        }
    }

    /// Whether [`A2a::channel_reply`] is a tool (test helper).
    #[cfg(test)]
    fn channel_reply_by_tool(&self, rec: &SessionRecord) -> bool {
        self.channel_reply(rec) != ChannelReply::Command
    }

    /// [`A2a::channel_reply`] before the name-collision fallback: what the harness, the
    /// binary and the computer allow. The start hook asks this one, then runs the collision check.
    fn channel_reply_eligible(&self, rec: &SessionRecord) -> bool {
        if ToolHarness::of(rec.agent).is_none() || self.config.cli_path.is_none() {
            return false;
        }
        match self.session_computer_id(&rec.id) {
            Ok(None) => true,
            Ok(Some(id)) => id == crate::computers::LOCAL,
            Err(e) => {
                tracing::warn!(session = %rec.id, "reading the session's computer failed: {e}");
                false
            }
        }
    }

    /// The computer id recorded for a session by `crate::computers` (`None` = never switched).
    fn session_computer_id(&self, session: &str) -> Result<Option<String>, rusqlite::Error> {
        use rusqlite::OptionalExtension;
        let conn = self.sessions.store().conn();
        conn.query_row(
            "SELECT computer_id FROM session_computer WHERE session_id = ?1",
            rusqlite::params![session],
            |r| r.get::<_, String>(0),
        )
        .optional()
    }

    /// Start-config hook (FR-T12): register the `ember-channels` MCP server for a session that
    /// [`A2a::channel_reply`], and its one allow rule where the harness needs one
    /// ([`ToolHarness::allow_rule`]). It reuses the `EMBER_URL` and `EMBER_RUNTIME_TOKEN` already
    /// in `req.env` (made by [`A2a::runtime_env`]), so no second token is issued and the child
    /// gets no wider scope than the CLI has. No adapter puts the entry's environment on a command
    /// line: Claude Code writes a private file, Codex forwards the two variables by name from its
    /// own process environment, Antigravity writes a private file in the session root, and an ACP
    /// agent reads `session/new` from its stdin. Never logs the token.
    fn channel_tool_config(&self, rec: &SessionRecord, req: &mut StartRequest) {
        if !self.channel_reply_eligible(rec) || req.remote.is_some() || req.computer.is_some() {
            self.channel_fallback.lock().unwrap().remove(&rec.id);
            return;
        }
        let Some(cli) = self.config.cli_path.as_ref() else { return };
        let Some(harness) = ToolHarness::of(rec.agent) else { return };
        let env_value = |name: &str| {
            req.env.iter().rev().find(|(k, v)| k == name && !v.is_empty()).map(|(_, v)| v.clone())
        };
        let (Some(url), Some(token)) = (env_value("EMBER_URL"), env_value("EMBER_RUNTIME_TOKEN")) else {
            return;
        };
        let sources = self.claude_sources.lock().unwrap().clone();
        let codex_home = env_value("CODEX_HOME")
            .map(PathBuf::from)
            .or(sources.codex_home.clone())
            .or(sources.home.as_ref().map(|h| h.join(".codex")));
        let check = if req.mcp_servers.iter().any(|s| s.name == CHANNEL_MCP_SERVER) {
            NameCheck::Taken { kind: KIND_REQUEST }
        } else {
            match harness {
                ToolHarness::Claude => {
                    let config_dir =
                        env_value("CLAUDE_CONFIG_DIR").map(PathBuf::from).or(sources.config_dir.clone());
                    channel_server_name_taken(&req.cwd, sources.home.as_deref(), config_dir.as_deref(), &sources.managed)
                }
                ToolHarness::Codex => {
                    codex_channel_name_taken(&req.cwd, codex_home.as_deref(), &sources.codex_system)
                }
                ToolHarness::Antigravity => agy_channel_name_taken(&req.cwd, sources.home.as_deref()),
                // Where an ACP agent keeps its own MCP servers is not known to Ember.
                ToolHarness::Acp => NameCheck::Free,
            }
        };
        // Never the file contents: `kind`, `why` and the reason are static strings.
        let skip_notice = match check {
            NameCheck::Free => None,
            NameCheck::Taken { kind } => {
                tracing::warn!(session = %rec.id, source = kind, "not registering {CHANNEL_MCP_SERVER}: {REASON_TAKEN}");
                Some(CHANNEL_FALLBACK_NOTICE.to_string())
            }
            NameCheck::Unsure { kind, why } => {
                tracing::warn!(session = %rec.id, source = kind, "not registering {CHANNEL_MCP_SERVER}: the configuration could not be checked ({why})");
                Some(unsure_notice_for(harness, kind, why))
            }
        };
        if let Some(notice) = skip_notice {
            let first = self.channel_fallback.lock().unwrap().insert(rec.id.clone());
            // The instructions hook ran before this one and named the tool: put the command back.
            if let Some(text) = req.instructions.as_mut() {
                *text = text
                    .replace(&self.instructions_with(harness.reply()), &self.instructions_with(ChannelReply::Command));
            }
            if first {
                let event = AgentEvent::Notice { message: notice };
                if let Err(e) = self.sessions.record_event(&rec.id, &event) {
                    tracing::warn!(session = %rec.id, "recording the channel fallback notice failed: {e:#}");
                }
            }
            return;
        }
        if self.channel_fallback.lock().unwrap().remove(&rec.id) {
            // The instructions hook ran with the fallback still set: name the tool again.
            if let Some(text) = req.instructions.as_mut() {
                *text = text
                    .replace(&self.instructions_with(ChannelReply::Command), &self.instructions_with(harness.reply()));
            }
        }
        req.mcp_servers.push(McpServer {
            name: CHANNEL_MCP_SERVER.to_string(),
            command: cli.to_string_lossy().into_owned(),
            args: vec!["mcp".to_string()],
            startup_timeout_secs: Some(10),
            env: vec![("EMBER_URL".to_string(), url), ("EMBER_RUNTIME_TOKEN".to_string(), token)],
        });
        // A Codex layer the name check does not read may define its own `ember-channels`: the call
        // is then asked (the Codex adapter puts the unverified note on that card), never
        // auto-approved.
        let unread = match harness {
            ToolHarness::Codex => codex_unread_layer(&codex_layer_paths(codex_home.as_deref(), &sources.codex_managed)),
            _ => None,
        };
        if let Some(layer) = unread {
            tracing::warn!(session = %rec.id, source = layer.kind, unreadable = layer.unreadable, "not auto-approving {CHANNEL_POST_TOOL}: {REASON_UNVERIFIED}");
            req.allowed_tools.retain(|t| t != CHANNEL_POST_TOOL);
        } else if harness.allow_rule() && !req.allowed_tools.iter().any(|t| t == CHANNEL_POST_TOOL) {
            req.allowed_tools.push(CHANNEL_POST_TOOL.to_string());
        }
    }

    /// Environment for an agent process: a fresh runtime token per agent run start (FR-T2,
    /// FR-T2a). It replaces the session's earlier tokens, unless the session's agent still runs
    /// under a live shim: then this start may rejoin that agent (FR-S8), which keeps the token in
    /// its environment, so the earlier ones stay until the run ends.
    fn runtime_env(&self, rec: &SessionRecord) -> Vec<(String, String)> {
        let minted = if crate::agents::proc::shim_alive(&rec.id) {
            self.store.issue_token_beside(&rec.id)
        } else {
            self.store.issue_token(&rec.id)
        };
        let token = match minted {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(session = %rec.id, "issuing A2A token failed: {e:#}");
                return Vec::new();
            }
        };
        let mut env = vec![
            ("EMBER_URL".to_string(), self.config.base_url.clone()),
            ("EMBER_SESSION_ID".to_string(), rec.id.clone()),
            ("EMBER_RUNTIME_TOKEN".to_string(), token),
        ];
        if let Some(cli) = &self.config.cli_path {
            env.push(("EMBER_A2A_BIN".into(), cli.to_string_lossy().into_owned()));
            if let Some(dir) = cli.parent() {
                let mut paths = vec![dir.to_path_buf()];
                if let Some(existing) = std::env::var_os("PATH") {
                    paths.extend(std::env::split_paths(&existing));
                }
                if let Ok(joined) = std::env::join_paths(paths) {
                    env.push(("PATH".into(), joined.to_string_lossy().into_owned()));
                }
            }
        }
        env
    }

    /// How the agent uses A2A. Stable across restarts (no per-process values), so it does not
    /// disturb prompt caching on resume.
    pub fn instructions(&self) -> String {
        self.instructions_with(ChannelReply::Command)
    }

    /// [`A2a::instructions`]; with a tool `how` it names the `channel_post` MCP tool as the way to
    /// answer a channel mention (with the command as a fallback) instead of the command alone.
    fn instructions_with(&self, how: ChannelReply) -> String {
        let channel_answer = match how {
            ChannelReply::ClaudeTool => format!(
                "call the tool `{CHANNEL_POST_TOOL}` with `channel` set to the channel id and \
                 `text` set to your answer; if the tool is not available, run `{CLI_NAME} channel \
                 post <channel-id> \"<text>\"`"
            ),
            ChannelReply::McpTool => format!(
                "call the `{}` tool of the MCP server `{CHANNEL_MCP_SERVER}` with `channel` set to \
                 the channel id and `text` set to your answer; if the tool is not available, run \
                 `{CLI_NAME} channel post <channel-id> \"<text>\"`",
                mcp::TOOL_NAME
            ),
            ChannelReply::Command => format!(
                "run `{CLI_NAME} channel post <channel-id> \"<text>\"` (use `-` to read the text \
                 from stdin)"
            ),
        };
        let path_note = match &self.config.cli_path {
            Some(p) => format!(" (also at `{}`, or `$EMBER_A2A_BIN`)", p.display()),
            None => String::new(),
        };
        format!(
            "# Messaging other agent sessions (Ember A2A)\n\
             \n\
             You are running inside Ember, which hosts several agent sessions (Claude Code, Codex, \
             ...) for the user. You can message those sessions with the `{CLI_NAME}` shell \
             command{path_note}:\n\
             \n\
             - `{CLI_NAME} list` shows the sessions you can message (id, agent, status, project, \
             title).\n\
             - `{CLI_NAME} send <session-id> \"message\"` sends a message. It reaches that \
             session as a user message; a busy session gets it when its current turn ends, a \
             sleeping one is woken up.\n\
             - `{CLI_NAME} send <session-id> --reply-to <message-id> \"reply\"` answers a \
             message you received.\n\
             \n\
             Terminal access is separate from messaging. Use `{CLI_NAME} terminal read <session-id> \
             [--lines <n>] [--since <seq>] [--wait-ms <ms>] [--json]` to inspect output; \
             use the returned last_seq with --since to follow new output. Use `{CLI_NAME} terminal \
             send-text <session-id> \"<text>\"` to send input and `{CLI_NAME} terminal interrupt \
             <session-id>` to interrupt a turn. Access requires a user grant, or an active team \
             leader accessing its own teammate; each use is recorded in the target transcript.\n\
             \n\
             Teams (only when the work splits into independent parts; each teammate is a full \
             agent session in this project):\n\
             \n\
             - `{CLI_NAME} team spawn <name> [--agent claude-code|codex] [--computer <id>] \
             \"<first prompt>\"` starts a teammate and makes you the team leader; the teammate \
             runs under its harness's profile, so do not choose an account. `{CLI_NAME} team \
             list` shows members and tasks; `{CLI_NAME} team end <name>` stops one. Only the \
             leader spawns and ends teammates.\n\
             - `{CLI_NAME} task add \"<title>\" [--detail <text>] [--assign <name>]`, \
             `{CLI_NAME} task update <n> [--status open|in_progress|blocked|done|cancelled] \
             [--assign <name>|none]`, `{CLI_NAME} task list`.\n\
             - `{CLI_NAME} mail send <name>|--all \"<text>\"` sends team mail (it reaches the \
             recipient like a message); `{CLI_NAME} mail read` shows the mailbox.\n\
             \n\
             Messages from other sessions arrive as user messages whose first line starts with \
             `{HEADER_TAG}` and names the sender session and the message id. They come from a \
             fellow agent, not from the user. Reply only when the message needs an answer: do \
             not send acknowledgements or thanks, and do not keep an exchange going for its own \
             sake. Sends are rate-limited and loops are stopped. The command talks to the local \
             Ember server over HTTP; if a sandbox blocks that, ask for permission to run it \
             outside the sandbox.\n\
             \n\
             # Scheduled prompts (Ember schedules)\n\
             \n\
             When the user asks for something to happen later or repeatedly (\"every morning\", \
             \"in two hours\", \"each Friday\"), create a schedule with the same command:\n\
             \n\
             - `{CLI_NAME} schedule add --cron \"0 9 * * 1-5\" --tz Asia/Seoul \"<prompt>\"` \
             (five-field cron in an IANA time zone), `--every 2h`, or `--at \
             2026-10-04T09:00:00+09:00` (once). By default the prompt is sent into this session; \
             add `--new [--title <title>]` to start a new session in this project each time, and \
             `--catch-up` to run the latest trigger missed while the server was down.\n\
             - `{CLI_NAME} schedule list` shows this project's schedules; `{CLI_NAME} schedule rm \
             <schedule-id>` deletes one.\n\
             \n\
             Scheduled prompts arrive as user messages whose first line starts with \
             `{SCHEDULE_TAG}`. Create schedules only when the user asked for one.\n\
             \n\
             # Channels (Ember channels)\n\
             \n\
             A person can mention you with `@name` in a channel. That message reaches you as a \
             user message whose first line starts with `{HEADER_TAG}` and names the person, the \
             channel and the channel id. A plain reply in your own session is not shown in the \
             channel. To answer there, {channel_answer}. Reply only when you were addressed, and keep \
             it short. You may post only in a channel you belong to, for 30 minutes after a \
             person mentioned you there, at most {CHANNEL_POSTS_PER_MINUTE} posts a minute. \
             Your post is shown to the people in the channel; `@name` in it does not reach \
             another agent."
        )
    }

    /// [`A2a::instructions`] plus, for an active teammate, its team role.
    pub fn instructions_for(&self, rec: &SessionRecord) -> String {
        let base = self.instructions_with(self.channel_reply(rec));
        match self.teammate_instructions(rec) {
            Some(team) => format!("{base}\n\n{team}"),
            None => base,
        }
    }

    /// The session a bearer token belongs to (FR-T2a): a token that is unknown, revoked or
    /// expired, or whose session no longer exists, is refused. A token of a deleted session is
    /// revoked here.
    pub fn authenticate(&self, token: &str) -> Result<String, A2aError> {
        let id = self.store.session_for_token(token)?.ok_or(A2aError::Unauthorized)?;
        if self.sessions.store().session(&id)?.is_none() {
            self.revoke_tokens(&id, "its session was deleted");
            return Err(A2aError::Unauthorized);
        }
        Ok(id)
    }

    pub fn enabled(&self) -> anyhow::Result<bool> {
        Ok(self
            .store
            .global_enabled()?
            .unwrap_or(self.config.enabled_by_default))
    }

    pub fn set_enabled(&self, enabled: bool) -> anyhow::Result<()> {
        self.store.set_global_enabled(enabled)
    }

    pub fn session_enabled(&self, id: &str) -> Result<bool, A2aError> {
        self.session(id)?;
        Ok(self.store.session_enabled(id)?)
    }

    pub fn set_session_enabled(&self, id: &str, enabled: bool) -> Result<(), A2aError> {
        self.session(id)?;
        Ok(self.store.set_session_enabled(id, enabled)?)
    }

    fn session(&self, id: &str) -> Result<SessionRecord, A2aError> {
        self.sessions
            .store()
            .session(id)?
            .ok_or_else(|| A2aError::NotFound(format!("session {id} not found")))
    }

    /// Rejects when A2A is off server-wide or for the calling session.
    fn check_caller(&self, caller: &str) -> Result<(), A2aError> {
        if !self.enabled()? {
            return Err(A2aError::Disabled(
                "agent-to-agent messaging is turned off on this server".into(),
            ));
        }
        if !self.store.session_enabled(caller)? {
            return Err(A2aError::Disabled(format!(
                "agent-to-agent messaging is turned off for this session ({caller})"
            )));
        }
        Ok(())
    }

    /// Why `caller` may not message `target` under the team rule (FR-T7), if it may not: an
    /// active teammate is reachable only from its own team, and reaches only its own team.
    fn team_scope_denial(
        caller: Option<&team::MemberRow>,
        target: Option<&team::MemberRow>,
        target_id: &str,
    ) -> Option<String> {
        let caller_team = caller.map(|m| m.team_id.as_str());
        let target_team = target.map(|m| m.team_id.as_str());
        if let Some(c) = caller.filter(|m| m.is_active_teammate()) {
            if target_team != Some(c.team_id.as_str()) {
                return Some(format!(
                    "you are teammate {} in team {}; you can message only your team (`{CLI_NAME} \
                     team list`)",
                    c.name, c.team_id
                ));
            }
        }
        if let Some(t) = target.filter(|m| m.is_active_teammate()) {
            if caller_team != Some(t.team_id.as_str()) {
                return Some(format!(
                    "session {target_id} is a teammate in team {}; only its team can message it",
                    t.team_id
                ));
            }
        }
        None
    }

    /// Sessions `caller` may message: every other session with A2A on, within the team rule.
    pub fn targets(&self, caller: &str) -> Result<Vec<Target>, A2aError> {
        self.check_caller(caller)?;
        let members = self.sessions.store().active_team_members()?;
        let me = members.get(caller);
        let mut out = Vec::new();
        for s in self.sessions.store().sessions(None)? {
            if s.id == caller || !self.store.session_enabled(&s.id)? {
                continue;
            }
            if Self::team_scope_denial(me, members.get(&s.id), &s.id).is_some() {
                continue;
            }
            out.push(Target {
                id: s.id,
                title: s.title,
                project: s.project,
                agent: s.agent.as_str().to_string(),
                status: s.status,
            });
        }
        Ok(out)
    }

    /// Send `text` from `from` to `to`. The message is stored before delivery is attempted.
    pub async fn send(
        self: &Arc<Self>,
        from: &str,
        to: &str,
        text: &str,
        reply_to: Option<&str>,
    ) -> Result<Receipt, A2aError> {
        if text.trim().is_empty() {
            return Err(A2aError::BadRequest("message text is empty".into()));
        }
        if from == to {
            return Err(A2aError::BadRequest(
                "a session cannot message itself".into(),
            ));
        }
        self.check_caller(from)?;
        self.session(to)?;
        if !self.store.session_enabled(to)? {
            return Err(A2aError::Disabled(format!(
                "agent-to-agent messaging is turned off for the target session ({to})"
            )));
        }
        {
            let store = self.sessions.store();
            let (me, them) = (store.team_member(from)?, store.team_member(to)?);
            let active = |m: Option<team::MemberRow>| m.filter(team::MemberRow::active);
            if let Some(why) =
                Self::team_scope_denial(active(me).as_ref(), active(them).as_ref(), to)
            {
                return Err(A2aError::Forbidden(why));
            }
        }
        if let Some(r) = reply_to {
            let orig = self
                .store
                .message(r)?
                .ok_or_else(|| A2aError::BadRequest(format!("unknown message {r} in reply_to")))?;
            if orig.from_session != from && orig.to_session != from {
                return Err(A2aError::BadRequest(format!(
                    "message {r} was not sent to or by this session"
                )));
            }
        }

        let msg = {
            let _guard = self.send_lock.lock().await;
            self.check_limits(from, &[to.to_string()])?;
            self.store.insert_message(from, to, reply_to, text)?
        };
        self.deliver(&msg).await
    }

    /// Try to deliver a stored message now and say whether it went in or stays queued.
    pub(crate) async fn deliver(self: &Arc<Self>, msg: &A2aMessage) -> Result<Receipt, A2aError> {
        self.flush_logged(&msg.to_session).await;
        let delivered = self
            .store
            .message(&msg.id)?
            .is_some_and(|m| m.delivered_at.is_some());
        let status = if delivered {
            Delivery::Delivered
        } else {
            Delivery::Queued
        };
        Ok(Receipt {
            id: msg.id.clone(),
            to: msg.to_session.clone(),
            status,
        })
    }

    /// Loop protection for one message from `from` to each of `tos` (a team broadcast counts
    /// once per recipient). All or nothing.
    fn check_limits(&self, from: &str, tos: &[String]) -> Result<(), A2aError> {
        let l = self.config.limits;
        let since = store::now_ms() - l.window.as_millis() as i64;
        let mins = l.window.as_secs().div_ceil(60);
        let mut refused: Option<(String, String)> = None;
        let sent = self.store.sent_since(from, since)?;
        if sent + tos.len() as u32 > l.per_session {
            refused = tos.first().map(|to| {
                (
                    to.clone(),
                    format!(
                        "session {from} has sent {} agent-to-agent messages in the last {mins} min \
                         (limit)",
                        l.per_session
                    ),
                )
            });
        } else {
            for to in tos {
                if self.store.pair_since(from, to, since)? >= l.per_pair {
                    refused = Some((
                        to.clone(),
                        format!(
                            "sessions {from} and {to} have exchanged {} agent-to-agent messages in \
                             the last {mins} min (limit)",
                            l.per_pair
                        ),
                    ));
                    break;
                }
            }
        }
        let Some((to, reason)) = refused else { return Ok(()) };
        let to = to.as_str();
        let message = format!(
            "A2A message from {from} to {to} was refused by loop protection: {reason}. Wait \
             before sending more, or ask the user."
        );
        self.notify_rejection(from, to, &message);
        Err(A2aError::RateLimited(message))
    }

    /// A visible notice in both sessions (FR-T5), at most once per pair per minute.
    fn notify_rejection(&self, from: &str, to: &str, message: &str) {
        let now = Instant::now();
        {
            let mut last = self.last_notice.lock().unwrap();
            let key = (from.to_string(), to.to_string());
            if last
                .get(&key)
                .is_some_and(|at| now.duration_since(*at) < NOTICE_EVERY)
            {
                return;
            }
            last.insert(key, now);
        }
        for id in [from, to] {
            let event = AgentEvent::Notice {
                message: message.to_string(),
            };
            if let Err(e) = self.sessions.record_event(id, &event) {
                tracing::warn!(session = %id, "recording A2A notice failed: {e:#}");
            }
        }
    }

    fn flush_lock(&self, target: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.flush_locks
            .lock()
            .unwrap()
            .entry(target.to_string())
            .or_default()
            .clone()
    }

    /// Deliver everything queued for `target` if it is between turns. Returns whether a
    /// delivery happened.
    pub async fn flush(self: &Arc<Self>, target: &str) -> anyhow::Result<bool> {
        let lock = self.flush_lock(target);
        let _guard = lock.lock().await;
        if !self.settle_awaiting(target) {
            return Ok(false);
        }
        let Some(rec) = self.sessions.store().session(target)? else {
            return Ok(false);
        };
        if is_busy(rec.status) {
            return Ok(false);
        }
        let pending = self.store.pending_for(target)?;
        if pending.is_empty() {
            return Ok(false);
        }
        let text = self.render(&rec, &pending)?;
        // Before sending: the turn could end before `send` returns.
        self.awaiting
            .lock()
            .unwrap()
            .insert(target.to_string(), rec.last_seq);
        if let Err(e) = self.sessions.send(target, &text).await {
            self.awaiting.lock().unwrap().remove(target);
            return Err(e.into());
        }
        let ids: Vec<String> = pending.iter().map(|m| m.id.clone()).collect();
        self.store.mark_delivered(&ids)?;
        self.note_delivered(target, &pending);
        Ok(true)
    }

    /// Whether `target`'s last delivered turn is over (or there is none). Checks the store, so
    /// it does not depend on the turn watcher having seen the turn end yet.
    fn settle_awaiting(&self, target: &str) -> bool {
        let Some(before) = self.awaiting.lock().unwrap().get(target).copied() else {
            return true;
        };
        let ended = self
            .sessions
            .store()
            .events_after(target, before)
            .is_ok_and(|evs| {
                evs.iter()
                    .any(|e| matches!(e.event, AgentEvent::TurnEnded { .. }))
            });
        if ended {
            let mut awaiting = self.awaiting.lock().unwrap();
            if awaiting.get(target) == Some(&before) {
                awaiting.remove(target);
            }
        }
        ended
    }

    async fn flush_logged(self: &Arc<Self>, target: &str) {
        if let Err(e) = self.flush(target).await {
            tracing::warn!(target_session = %target, "A2A delivery failed, kept queued: {e:#}");
        }
    }

    async fn flush_all(self: &Arc<Self>) {
        match self.store.sessions_with_pending() {
            Ok(ids) => {
                for id in ids {
                    self.flush_logged(&id).await;
                }
            }
            Err(e) => tracing::error!("listing queued A2A messages failed: {e:#}"),
        }
    }

    /// The user-message text for a batch of messages to one target (FR-T3).
    fn render(&self, rec: &SessionRecord, msgs: &[A2aMessage]) -> anyhow::Result<String> {
        let how = self.channel_reply(rec);
        let mut parts = Vec::new();
        for m in msgs {
            if let (Some(channel_id), Some(until)) = (&m.origin_channel, m.origin_guest_until) {
                // A guest in a channel (FR-T17): the text is untrusted and fenced.
                parts.push(channel::guest_message_text_for(
                    &m.id,
                    m.origin_label.as_deref().unwrap_or("guest"),
                    self.sessions.store().channel_name(channel_id).as_deref(),
                    channel_id,
                    until,
                    &m.text,
                    how,
                ));
                continue;
            }
            if let Some(channel_id) = &m.origin_channel {
                // A person in a channel (FR-T12): no sender session, so no A2A reply hint.
                let who = channel::sanitize_label(m.origin_label.as_deref().unwrap_or("a person"));
                let place = match self.sessions.store().channel_name(channel_id).map(|n| channel::sanitize_label(&n)) {
                    Some(name) => format!("channel #{name}, id {channel_id}"),
                    None => format!("channel {channel_id}"),
                };
                let reply_hint = channel::reply_hint(channel_id, how);
                parts.push(format!(
                    "{HEADER_TAG} Message {} from {who} ({place})\n\
                     This message is from a person in a channel, not from another agent.\n\
                     {reply_hint}\n\n{}",
                    m.id, m.text
                ));
                continue;
            }
            let sender = match self.sessions.store().session(&m.from_session)? {
                Some(s) => format!(
                    "\"{}\" (project {}, agent {})",
                    s.title,
                    s.project,
                    s.agent.as_str()
                ),
                None => "(deleted session)".into(),
            };
            if m.is_report() {
                // FR-T8 (#995): built by Ember, never answered automatically.
                parts.push(format!(
                    "{HEADER_TAG} Report {} from session {} {sender}\n\
                     Ember built this status report from that session's turn. No reply is needed; \
                     act on it only if your own work needs it.\n\n{}",
                    m.id, m.from_session, m.text
                ));
                continue;
            }
            let mut header = format!(
                "{HEADER_TAG} Message {} from session {} {sender}",
                m.id, m.from_session
            );
            if let Some(r) = &m.reply_to {
                header.push_str(&format!("\nIn reply to message {r}."));
            }
            header.push_str(&format!(
                "\nTo reply: {CLI_NAME} send {} --reply-to {} \"<your reply>\"",
                m.from_session, m.id
            ));
            parts.push(format!("{header}\n\n{}", m.text));
        }
        Ok(parts.join("\n\n---\n\n"))
    }
}

/// Delivers queued messages whenever a turn ends.
async fn watch_turns(a2a: Weak<A2a>, mut rx: tokio::sync::broadcast::Receiver<Push>) {
    loop {
        let push = rx.recv().await;
        let Some(this) = a2a.upgrade() else { return };
        match push {
            Ok(Push::Event { event, .. })
                if matches!(event.event, AgentEvent::ApprovalRequested { .. } | AgentEvent::QuestionAsked { .. }) =>
            {
                // FR-T8 needs input (#995).
                let (id, what) = match &event.event {
                    AgentEvent::ApprovalRequested { approval_id, tool, .. } => (approval_id.clone(), format!("approval for {tool}")),
                    AgentEvent::QuestionAsked { question_id, questions } => (
                        question_id.clone(),
                        format!("a question: {}", questions.first().map(|q| q.text.as_str()).unwrap_or("")),
                    ),
                    _ => unreachable!(),
                };
                let (sid, seq) = (event.session_id, event.seq);
                tokio::spawn(async move { this.on_prompt(&sid, &id, seq, what).await });
            }
            Ok(Push::Event { event, .. })
                if matches!(event.event, AgentEvent::TurnEnded { .. }) =>
            {
                let sid = event.session_id;
                if let AgentEvent::TurnEnded { outcome } = &event.event {
                    // FR-T8 turn-end report (#995).
                    let (this, sid, seq, outcome) = (this.clone(), sid.clone(), event.seq, *outcome);
                    tokio::spawn(async move { this.on_turn_ended(&sid, seq, outcome).await });
                }
                {
                    let mut awaiting = this.awaiting.lock().unwrap();
                    if awaiting.get(&sid).is_some_and(|before| event.seq > *before) {
                        awaiting.remove(&sid);
                    }
                }
                tokio::spawn(async move { this.flush_logged(&sid).await });
            }
            Ok(_) => {}
            Err(RecvError::Lagged(_)) => {
                // Turn ends may have been missed; `flush` settles them from the store.
                tokio::spawn(async move { this.flush_all().await });
            }
            Err(RecvError::Closed) => return,
        }
    }
}

#[cfg(test)]
mod channel_tool_tests {
    //! FR-T12 (#380 b, c): Claude Code, Codex, Antigravity and ACP agents answer a channel mention
    //! through the `channel_post` MCP tool.
    use super::*;
    use crate::agents::claude_code::{mcp_config, mcp_needs_file, ClaudeCodeAdapter, PrivateConfigFile};
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

    /// A request as the start hooks leave it: runtime env made by `runtime_env`.
    fn request(a: &A2a, r: &SessionRecord) -> StartRequest {
        StartRequest { cwd: "/nonexistent-ember-test/proj".into(), env: a.runtime_env(r), ..Default::default() }
    }

    fn env_of(req: &StartRequest, name: &str) -> String {
        req.env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()).unwrap()
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

    fn acp() -> AgentKind {
        AgentKind::Acp(crate::agents::AcpName::register("acp-t12-tool").unwrap())
    }

    #[test]
    fn fr_t12_the_constants_are_built_from_each_other() {
        assert_eq!(CHANNEL_MCP_SERVER, "ember-channels");
        assert_eq!(CHANNEL_POST_TOOL, "mcp__ember-channels__channel_post");
        assert_eq!(CHANNEL_POST_TOOL, format!("mcp__{CHANNEL_MCP_SERVER}__{}", mcp::TOOL_NAME));
    }

    #[test]
    fn fr_t12_hook_registers_the_server_and_the_one_allow_rule_for_local_claude_code() {
        let a = world(Some(CLI));
        let r = rec(&a, AgentKind::ClaudeCode);
        let mut req = request(&a, &r);
        a.channel_tool_config(&r, &mut req);
        assert_eq!(req.allowed_tools, vec![CHANNEL_POST_TOOL.to_string()]);
        assert_eq!(req.mcp_servers.len(), 1);
        let s = &req.mcp_servers[0];
        assert_eq!(s.name, "ember-channels");
        assert_eq!(s.command, CLI);
        assert_eq!(s.args, vec!["mcp".to_string()]);
        assert_eq!(s.startup_timeout_secs, Some(10));
        assert_eq!(s.env.len(), 2);
        assert!(s.env.contains(&("EMBER_URL".into(), "http://127.0.0.1:8740".into())));
    }

    #[test]
    fn fr_t12_hook_reuses_the_token_in_the_env_and_issues_no_second_one() {
        let a = world(Some(CLI));
        let r = rec(&a, AgentKind::ClaudeCode);
        let mut req = request(&a, &r);
        let token = env_of(&req, "EMBER_RUNTIME_TOKEN");
        a.channel_tool_config(&r, &mut req);
        let entry = req.mcp_servers[0].env.iter().find(|(k, _)| k == "EMBER_RUNTIME_TOKEN").unwrap();
        assert_eq!(entry.1, token);
        // A second `issue_token` would replace the stored hash: the env token would stop working.
        assert_eq!(a.store.session_for_token(&token).unwrap().as_deref(), Some(r.id.as_str()));
    }

    #[test]
    fn fr_t12_hook_treats_a_session_on_the_local_computer_row_as_local() {
        let a = world(Some(CLI));
        let r = rec(&a, AgentKind::ClaudeCode);
        move_to(&a, &r, crate::computers::LOCAL);
        let mut req = request(&a, &r);
        a.channel_tool_config(&r, &mut req);
        assert_eq!(req.mcp_servers.len(), 1);
    }

    #[test]
    fn fr_t12_hook_skips_a_claude_session_on_another_computer() {
        let a = world(Some(CLI));
        let r = rec(&a, AgentKind::ClaudeCode);
        move_to(&a, &r, "pi-1");
        let mut req = request(&a, &r);
        a.channel_tool_config(&r, &mut req);
        assert!(req.mcp_servers.is_empty() && req.allowed_tools.is_empty());
        assert!(!a.channel_reply_by_tool(&r));
        assert!(a.instructions_for(&r).contains("run `ember-a2a channel post <channel-id>"));
    }

    #[test]
    fn fr_t12_hook_skips_without_the_cli_binary() {
        let a = world(None);
        let r = rec(&a, AgentKind::ClaudeCode);
        let mut req = request(&a, &r);
        a.channel_tool_config(&r, &mut req);
        assert!(req.mcp_servers.is_empty() && req.allowed_tools.is_empty());
        assert!(!a.channel_reply_by_tool(&r));
    }

    #[test]
    fn fr_t12_hook_never_touches_the_scripted_agent() {
        let a = world(Some(CLI));
        let r = rec(&a, AgentKind::Scripted);
        let mut req = request(&a, &r);
        a.channel_tool_config(&r, &mut req);
        assert!(req.mcp_servers.is_empty());
        assert!(req.allowed_tools.is_empty());
        assert!(!a.channel_reply_by_tool(&r));
        assert_eq!(a.channel_reply(&r), ChannelReply::Command);
    }

    #[test]
    fn fr_t12_hook_skips_a_request_that_already_runs_elsewhere() {
        let a = world(Some(CLI));
        let r = rec(&a, AgentKind::ClaudeCode);
        let mut req = request(&a, &r);
        req.remote = Some(crate::agents::RemoteExec {
            environment_id: "ember-x".into(),
            exec_server_url: "ws://x".into(),
        });
        a.channel_tool_config(&r, &mut req);
        assert!(req.mcp_servers.is_empty() && req.allowed_tools.is_empty());
    }

    #[test]
    fn fr_t12_hook_skips_when_the_env_lacks_the_url_or_the_token() {
        let a = world(Some(CLI));
        let r = rec(&a, AgentKind::ClaudeCode);
        for drop in ["EMBER_RUNTIME_TOKEN", "EMBER_URL"] {
            let mut req = request(&a, &r);
            req.env.retain(|(k, _)| k != drop);
            a.channel_tool_config(&r, &mut req);
            assert!(req.mcp_servers.is_empty() && req.allowed_tools.is_empty(), "{drop}");
        }
        let mut req = request(&a, &r);
        for (k, v) in req.env.iter_mut() {
            if k == "EMBER_RUNTIME_TOKEN" {
                v.clear();
            }
        }
        a.channel_tool_config(&r, &mut req);
        assert!(req.mcp_servers.is_empty() && req.allowed_tools.is_empty(), "empty token");
        let mut req = StartRequest { cwd: ".".into(), ..Default::default() };
        a.channel_tool_config(&r, &mut req);
        assert!(req.mcp_servers.is_empty());
    }

    #[test]
    fn fr_t12_hook_does_not_add_a_second_server_of_the_same_name() {
        let a = world(Some(CLI));
        let r = rec(&a, AgentKind::ClaudeCode);
        let mut req = request(&a, &r);
        req.mcp_servers.push(McpServer {
            name: CHANNEL_MCP_SERVER.into(),
            command: "/usr/bin/other".into(),
            ..Default::default()
        });
        a.channel_tool_config(&r, &mut req);
        assert_eq!(req.mcp_servers.len(), 1);
        assert_eq!(req.mcp_servers[0].command, "/usr/bin/other");
        assert!(req.allowed_tools.is_empty(), "no allow rule for a server we did not register");
    }

    #[test]
    fn fr_t12_the_token_is_in_the_private_file_and_not_in_argv_or_debug() {
        let a = world(Some(CLI));
        let r = rec(&a, AgentKind::ClaudeCode);
        let mut req = request(&a, &r);
        let token = env_of(&req, "EMBER_RUNTIME_TOKEN");
        a.channel_tool_config(&r, &mut req);
        assert!(mcp_needs_file(&req), "the entry has env, so it goes through the private file");
        let config = mcp_config(&req.mcp_servers).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = PrivateConfigFile::create(dir.path(), &config).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(file.path()).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let contents = std::fs::read_to_string(file.path()).unwrap();
        assert!(contents.contains(&token), "the private file holds the token");
        assert!(contents.contains("ember-channels"));
        let args = ClaudeCodeAdapter::args_with(&req, Some(file.path()));
        assert!(!args.join("\n").contains(&token), "{args:?}");
        assert!(!format!("{:?}", req.mcp_servers).contains(&token), "Debug of an MCP server shows only the keys");
        let i = args.iter().position(|x| x == "--allowedTools").unwrap();
        assert_eq!(args[i + 1], CHANNEL_POST_TOOL);
        assert!(args[i + 2].starts_with("--mcp-config="), "a flag follows the tool: {args:?}");
    }

    #[test]
    fn fr_t12_claude_prompt_names_the_tool_keeps_the_rules_and_the_fallback() {
        let a = world(Some(CLI));
        let r = rec(&a, AgentKind::ClaudeCode);
        let text = a.instructions_for(&r);
        assert!(text.contains(&format!("call the tool `{CHANNEL_POST_TOOL}` with `channel` set to the channel id and `text` set to your answer")), "{text}");
        assert!(text.contains("if the tool is not available, run `ember-a2a channel post <channel-id> \"<text>\"`"), "{text}");
        assert!(text.contains("Reply only when you were addressed"));
        assert!(text.contains("for 30 minutes after a person mentioned you there"));
        assert!(text.contains(&format!("at most {CHANNEL_POSTS_PER_MINUTE} posts a minute")));
        assert!(text.contains("Your post is shown to the people in the channel"));
    }

    /// The channel paragraph as it was before the tool existed.
    const OLD_CHANNEL_PARAGRAPH: &str = "To answer there, run `ember-a2a channel post <channel-id> \"<text>\"` \
        (use `-` to read the text from stdin). Reply only when you were addressed, and keep \
        it short. You may post only in a channel you belong to, for 30 minutes after a \
        person mentioned you there, at most 6 posts a minute. \
        Your post is shown to the people in the channel; `@name` in it does not reach \
        another agent.";

    #[test]
    fn fr_t12_a_session_that_keeps_the_command_keeps_the_old_prompt_byte_for_byte() {
        let a = world(Some(CLI));
        let claude = a.instructions_for(&rec(&a, AgentKind::ClaudeCode));
        // The scripted agent, and every harness on another computer.
        let mut keep = vec![rec(&a, AgentKind::Scripted)];
        for agent in [AgentKind::Codex, AgentKind::Antigravity, acp()] {
            let r = rec(&a, agent);
            move_to(&a, &r, "pi-1");
            keep.push(r);
        }
        for r in &keep {
            let text = a.instructions_for(r);
            assert!(text.ends_with(OLD_CHANNEL_PARAGRAPH), "{:?}: {text}", r.agent);
            assert_eq!(text, a.instructions(), "{:?}", r.agent);
            // Everything before the channel answer is shared with Claude Code.
            let cut = "To answer there, ";
            assert_eq!(text[..text.find(cut).unwrap()], claude[..claude.find(cut).unwrap()]);
        }
        // Without the binary every harness keeps the old text as well.
        let b = world(None);
        for agent in [AgentKind::ClaudeCode, AgentKind::Codex, AgentKind::Antigravity, acp()] {
            assert!(b.instructions_for(&rec(&b, agent)).ends_with(OLD_CHANNEL_PARAGRAPH), "{agent:?}");
        }
    }

    fn mention(id: &str, channel: &str, guest_until: Option<i64>) -> A2aMessage {
        A2aMessage {
            id: id.into(),
            from_session: String::new(),
            to_session: "x".into(),
            reply_to: None,
            text: "hi there".into(),
            created_at: 0,
            delivered_at: None,
            origin_channel: Some(channel.into()),
            origin_label: Some("Mina".into()),
            origin_guest_until: guest_until,
            kind: None,
        }
    }

    #[test]
    fn fr_t12_person_mention_footer_per_recipient() {
        let a = world(Some(CLI));
        let m = mention("m1", "chan1", None);
        let claude = a.render(&rec(&a, AgentKind::ClaudeCode), std::slice::from_ref(&m)).unwrap();
        assert!(
            claude.contains(&format!(
                "To reply in the channel: call the tool {CHANNEL_POST_TOOL} with channel \"chan1\" and text \"...\"\n\nhi there"
            )),
            "{claude}"
        );
        assert!(!claude.contains("ember-a2a channel post"));
        let scripted = a.render(&rec(&a, AgentKind::Scripted), std::slice::from_ref(&m)).unwrap();
        for agent in [AgentKind::Codex, AgentKind::Antigravity, acp()] {
            let text = a.render(&rec(&a, agent), std::slice::from_ref(&m)).unwrap();
            assert!(
                text.ends_with(
                    "To reply in the channel: call the channel_post tool of the MCP server ember-channels with channel \"chan1\" and text \"...\"\n\nhi there"
                ),
                "{agent:?}: {text}"
            );
            assert!(!text.contains("ember-a2a channel post") && !text.contains(CHANNEL_POST_TOOL), "{agent:?}");
            let cut = |s: &str| s[..s.find("To reply in the channel:").unwrap()].to_string();
            assert_eq!(cut(&text), cut(&scripted), "{agent:?}: the header is byte-identical");
        }
        assert_eq!(
            scripted,
            "[ember a2a] Message m1 from Mina (channel chan1)\n\
             This message is from a person in a channel, not from another agent.\n\
             To reply in the channel: ember-a2a channel post chan1 \"...\"\n\nhi there"
        );
    }

    #[test]
    fn fr_t12_guest_footer_per_recipient_keeps_the_untrusted_block() {
        let a = world(Some(CLI));
        let m = mention("m2", "chan1", Some(0));
        let codex = a.render(&rec(&a, AgentKind::Scripted), std::slice::from_ref(&m)).unwrap();
        assert_eq!(
            codex,
            channel::guest_message_text("m2", "Mina", None, "chan1", 0, "hi there"),
            "unchanged for a session that keeps the command"
        );
        for agent in [AgentKind::Codex, AgentKind::Antigravity, acp()] {
            let text = a.render(&rec(&a, agent), std::slice::from_ref(&m)).unwrap();
            assert!(
                text.ends_with(
                    "To reply in the channel: call the channel_post tool of the MCP server ember-channels with channel \"chan1\" and text \"...\""
                ),
                "{agent:?}: {text}"
            );
            let cut = |s: &str| s[..s.rfind("To reply in the channel:").unwrap()].to_string();
            assert_eq!(cut(&text), cut(&codex), "{agent:?}: header and markers are byte-identical");
            assert!(text.contains("UNTRUSTED input"), "{agent:?}");
        }
        assert!(codex.ends_with("To reply in the channel: ember-a2a channel post chan1 \"...\""));
        let claude = a.render(&rec(&a, AgentKind::ClaudeCode), &[m]).unwrap();
        let tail = format!(
            "To reply in the channel: call the tool {CHANNEL_POST_TOOL} with channel \"chan1\" and text \"...\""
        );
        assert!(claude.ends_with(&tail), "{claude}");
        let cut = |s: &str| s[..s.rfind("To reply in the channel:").unwrap()].to_string();
        assert_eq!(cut(&claude), cut(&codex), "header and markers are byte-identical");
        assert!(claude.contains("UNTRUSTED input"));
    }

    // ---- name collision with the user's own Claude Code configuration (FR-T12) ----

    fn put(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn with_server(key: &str) -> String {
        format!(r#"{{"mcpServers":{{"{key}":{{"command":"x"}}}}}}"#)
    }

    fn taken(cwd: &Path, home: Option<&Path>, cfg: Option<&Path>, managed: &[PathBuf]) -> NameCheck {
        channel_server_name_taken(cwd, home, cfg, managed)
    }

    #[test]
    fn fr_t12_collision_in_the_project_mcp_json_of_cwd_and_of_a_parent() {
        let d = tempfile::tempdir().unwrap();
        let cwd = d.path().join("a/b");
        std::fs::create_dir_all(&cwd).unwrap();
        put(&cwd.join(".mcp.json"), &with_server("ember-channels"));
        assert_eq!(taken(&cwd, None, None, &[]), NameCheck::Taken { kind: KIND_PROJECT });
        std::fs::remove_file(cwd.join(".mcp.json")).unwrap();
        assert_eq!(taken(&cwd, None, None, &[]), NameCheck::Free);
        put(&d.path().join("a/.mcp.json"), &with_server("ember-channels"));
        assert_eq!(taken(&cwd, None, None, &[]), NameCheck::Taken { kind: KIND_PROJECT });
    }

    #[test]
    fn fr_t12_collision_in_the_user_config_at_both_locations() {
        let d = tempfile::tempdir().unwrap();
        let cwd = d.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let (home, cfg) = (d.path().join("home"), d.path().join("cfgdir"));
        put(&cfg.join(".claude.json"), &with_server("ember-channels"));
        assert_eq!(taken(&cwd, Some(home.as_path()), Some(&cfg), &[]), NameCheck::Taken { kind: KIND_USER });
        std::fs::remove_file(cfg.join(".claude.json")).unwrap();
        assert_eq!(taken(&cwd, Some(home.as_path()), Some(&cfg), &[]), NameCheck::Free);
        put(&home.join(".claude.json"), &with_server("ember-channels"));
        assert_eq!(taken(&cwd, Some(home.as_path()), Some(&cfg), &[]), NameCheck::Taken { kind: KIND_USER });
        assert_eq!(taken(&cwd, Some(home.as_path()), None, &[]), NameCheck::Taken { kind: KIND_USER });
    }

    #[test]
    fn fr_t12_collision_in_local_scope_projects_for_cwd_and_ancestors() {
        let d = tempfile::tempdir().unwrap();
        let cwd = d.path().join("proj/sub");
        std::fs::create_dir_all(&cwd).unwrap();
        let home = d.path().join("home");
        let key = cwd.to_str().unwrap();
        put(&home.join(".claude.json"), &format!(r#"{{"projects":{{"{key}":{}}}}}"#, with_server("Ember-Channels")));
        assert_eq!(taken(&cwd, Some(home.as_path()), None, &[]), NameCheck::Taken { kind: KIND_LOCAL });
        let parent = d.path().join("proj");
        let pkey = parent.to_str().unwrap();
        put(&home.join(".claude.json"), &format!(r#"{{"projects":{{"{pkey}":{}}}}}"#, with_server("ember-channels")));
        assert_eq!(taken(&cwd, Some(home.as_path()), None, &[]), NameCheck::Taken { kind: KIND_LOCAL });
    }

    #[test]
    fn fr_t12_collision_in_a_managed_file_and_with_different_letter_case() {
        let d = tempfile::tempdir().unwrap();
        let cwd = d.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let m = d.path().join("managed-mcp.json");
        put(&m, &with_server("EMBER-CHANNELS"));
        assert_eq!(taken(&cwd, None, None, std::slice::from_ref(&m)), NameCheck::Taken { kind: KIND_MANAGED });
        put(&cwd.join(".mcp.json"), &with_server("Ember-Channels"));
        assert_eq!(taken(&cwd, None, None, &[]), NameCheck::Taken { kind: KIND_PROJECT });
    }

    #[test]
    fn fr_t12_no_collision_when_files_are_absent_or_name_other_things() {
        let d = tempfile::tempdir().unwrap();
        let cwd = d.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let (home, cfg) = (d.path().join("home"), d.path().join("cfg"));
        let m = d.path().join("none.json");
        assert_eq!(taken(&cwd, Some(home.as_path()), Some(&cfg), std::slice::from_ref(&m)), NameCheck::Free, "absent files");
        put(&cwd.join(".mcp.json"), &with_server("github"));
        put(&home.join(".claude.json"), &with_server("ember-channels-2"));
        let other = d.path().join("elsewhere");
        let okey = other.to_str().unwrap();
        put(&cfg.join(".claude.json"), &format!(r#"{{"projects":{{"{okey}":{}}}}}"#, with_server("ember-channels")));
        assert_eq!(taken(&cwd, Some(home.as_path()), Some(&cfg), &[m]), NameCheck::Free);
    }

    fn unsure(kind: &'static str, why: &'static str) -> NameCheck {
        NameCheck::Unsure { kind, why }
    }

    #[test]
    fn fr_t12_collision_check_fails_closed() {
        let d = tempfile::tempdir().unwrap();
        let cwd = d.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        // Invalid JSON.
        put(&cwd.join(".mcp.json"), "{ not json");
        assert_eq!(taken(&cwd, None, None, &[]), unsure(KIND_PROJECT, "not valid JSON"));
        // Oversized.
        std::fs::remove_file(cwd.join(".mcp.json")).unwrap();
        let home = d.path().join("home");
        let big = format!(r#"{{"pad":"{}"}}"#, "x".repeat(MAX_CONFIG_BYTES as usize));
        put(&home.join(".claude.json"), &big);
        assert_eq!(taken(&cwd, Some(home.as_path()), None, &[]), unsure(KIND_USER, "larger than 4 MiB"));
        // Not a regular file (a directory).
        std::fs::remove_file(home.join(".claude.json")).unwrap();
        std::fs::create_dir_all(home.join(".claude.json")).unwrap();
        assert_eq!(taken(&cwd, Some(home.as_path()), None, &[]), unsure(KIND_USER, "not a regular file"));
        // Managed file with invalid JSON.
        let m = d.path().join("m.json");
        put(&m, "[");
        assert_eq!(taken(&cwd, None, None, &[m]), unsure(KIND_MANAGED, "not valid JSON"));
    }

    #[test]
    fn fr_t12_collision_each_unsure_reason_and_a_missing_file_is_free() {
        let d = tempfile::tempdir().unwrap();
        let cwd = d.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        // Missing: free.
        assert_eq!(taken(&cwd, None, None, &[d.path().join("absent.json")]), NameCheck::Free);
        // A `.mcp.json` that is a directory (in an ancestor too) is "not a regular file".
        let parent = d.path().to_path_buf();
        std::fs::create_dir_all(parent.join(".mcp.json")).unwrap();
        assert_eq!(taken(&cwd, None, None, &[]), unsure(KIND_PROJECT, WHY_NOT_REGULAR));
        std::fs::remove_dir(parent.join(".mcp.json")).unwrap();
        // A non-JSON file in an ancestor.
        put(&parent.join(".mcp.json"), "plain text");
        assert_eq!(taken(&cwd, None, None, &[]), unsure(KIND_PROJECT, WHY_NOT_JSON));
        std::fs::remove_file(parent.join(".mcp.json")).unwrap();
        // Exactly at the limit is read; one byte over is too large.
        let m = d.path().join("m.json");
        let pad = |n: usize| format!(r#"{{"pad":"{}"}}"#, "x".repeat(n - 10));
        put(&m, &pad(MAX_CONFIG_BYTES as usize));
        assert_eq!(std::fs::metadata(&m).unwrap().len(), MAX_CONFIG_BYTES);
        assert_eq!(taken(&cwd, None, None, std::slice::from_ref(&m)), NameCheck::Free);
        put(&m, &pad(MAX_CONFIG_BYTES as usize + 1));
        assert_eq!(taken(&cwd, None, None, &[m]), unsure(KIND_MANAGED, WHY_TOO_BIG));
    }

    #[cfg(unix)]
    #[test]
    fn fr_t12_collision_check_fails_closed_on_an_unreadable_file() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return; // root reads anything
        }
        let d = tempfile::tempdir().unwrap();
        let cwd = d.path().join("proj");
        put(&cwd.join(".mcp.json"), "{}");
        std::fs::set_permissions(cwd.join(".mcp.json"), std::fs::Permissions::from_mode(0o000)).unwrap();
        let r = taken(&cwd, None, None, &[]);
        std::fs::set_permissions(cwd.join(".mcp.json"), std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(r, unsure(KIND_PROJECT, WHY_UNREADABLE));
    }

    #[cfg(unix)]
    #[test]
    fn fr_t12_collision_a_fifo_is_not_a_regular_file_and_does_not_block() {
        use std::os::unix::ffi::OsStrExt;
        let d = tempfile::tempdir().unwrap();
        let cwd = d.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let fifo = cwd.join(".mcp.json");
        let c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let (tx, rx) = std::sync::mpsc::channel();
        let cwd2 = cwd.clone();
        std::thread::spawn(move || {
            let _ = tx.send(taken(&cwd2, None, None, &[]));
        });
        let r = rx.recv_timeout(Duration::from_secs(5)).expect("the check blocked on a FIFO");
        assert_eq!(r, unsure(KIND_PROJECT, WHY_NOT_REGULAR));
    }

    #[cfg(unix)]
    #[test]
    fn fr_t12_collision_a_symlink_to_a_regular_file_is_still_read() {
        let d = tempfile::tempdir().unwrap();
        let cwd = d.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let real = d.path().join("dotfiles/mcp.json");
        put(&real, &with_server("ember-channels"));
        std::os::unix::fs::symlink(&real, cwd.join(".mcp.json")).unwrap();
        assert_eq!(taken(&cwd, None, None, &[]), NameCheck::Taken { kind: KIND_PROJECT });
        put(&real, &with_server("other"));
        assert_eq!(taken(&cwd, None, None, &[]), NameCheck::Free);
        // A dangling link is a missing file.
        std::fs::remove_file(&real).unwrap();
        assert_eq!(taken(&cwd, None, None, &[]), NameCheck::Free);
    }

    #[test]
    fn fr_t12_collision_reasons_are_static_and_hold_no_file_contents() {
        let d = tempfile::tempdir().unwrap();
        let cwd = d.path().join("proj");
        put(&cwd.join(".mcp.json"), "SECRET-CONTENT-123 {");
        let NameCheck::Unsure { kind, why } = taken(&cwd, None, None, &[]) else { panic!("unsure expected") };
        for text in [kind, why] {
            assert!(!text.contains("SECRET"), "{text}");
        }
        assert!(!unsure_notice(kind, why).contains("SECRET"));
        assert_eq!([WHY_UNREADABLE, WHY_NOT_JSON, WHY_TOO_BIG, WHY_NOT_REGULAR], ["unreadable", "not valid JSON", "larger than 4 MiB", "not a regular file"]);
        assert_eq!([KIND_PROJECT, KIND_USER, KIND_LOCAL, KIND_MANAGED], ["project .mcp.json", "user config", "local config", "managed"]);
    }

    #[test]
    fn fr_t12_collision_unsure_notice_text() {
        assert_eq!(
            unsure_notice("user config", "larger than 4 MiB"),
            "The channel tool was not registered because the Claude Code configuration (user config) could not be checked (larger than 4 MiB); replies use the ember-a2a command."
        );
    }

    #[test]
    fn fr_t12_collision_hook_unsure_skips_with_its_own_notice_and_taken_keeps_the_old_text() {
        let d = tempfile::tempdir().unwrap();
        let a = world(Some(CLI));
        let notices = |r: &SessionRecord| -> Vec<String> {
            a.sessions.store().events_after(&r.id, 0).unwrap().into_iter()
                .filter_map(|e| match e.event { AgentEvent::Notice { message } => Some(message), _ => None })
                .collect()
        };
        let r = rec(&a, AgentKind::ClaudeCode);
        let cwd = d.path().join("proj");
        put(&cwd.join(".mcp.json"), "{ nope");
        let mut req = request(&a, &r);
        req.cwd = cwd.clone();
        a.channel_tool_config(&r, &mut req);
        assert!(req.mcp_servers.is_empty() && req.allowed_tools.is_empty(), "fail closed");
        assert!(a.channel_fallback.lock().unwrap().contains(&r.id));
        assert_eq!(notices(&r), [unsure_notice(KIND_PROJECT, "not valid JSON")]);
        // Taken: the old text, byte for byte.
        let r2 = rec(&a, AgentKind::ClaudeCode);
        put(&cwd.join(".mcp.json"), &with_server("ember-channels"));
        let mut req = request(&a, &r2);
        req.cwd = cwd;
        a.channel_tool_config(&r2, &mut req);
        assert_eq!(
            notices(&r2),
            ["The channel tool could not be registered because a MCP server named ember-channels is already configured; replies use the ember-a2a command."]
        );
    }

    #[test]
    fn fr_t12_the_end_hook_drops_the_fallback_entry_and_the_set_does_not_grow() {
        let a = world(Some(CLI));
        a.install_end_hook();
        let r = rec(&a, AgentKind::ClaudeCode);
        let keep = rec(&a, AgentKind::ClaudeCode);
        a.channel_fallback.lock().unwrap().insert(r.id.clone());
        a.channel_fallback.lock().unwrap().insert(keep.id.clone());
        a.sessions.session_ended(&r.id);
        let set = a.channel_fallback.lock().unwrap().clone();
        assert!(!set.contains(&r.id) && set.contains(&keep.id));
        // Many start/skip/end cycles leave nothing behind.
        for i in 0..1000 {
            let id = format!("s{i}");
            a.channel_fallback.lock().unwrap().insert(id.clone());
            a.sessions.session_ended(&id);
        }
        assert_eq!(a.channel_fallback.lock().unwrap().len(), 1);
    }

    fn set_sources(a: &A2a, home: &Path) {
        *a.claude_sources.lock().unwrap() = ClaudeConfigSources { home: Some(home.to_path_buf()), ..Default::default() };
    }

    #[test]
    fn fr_t12_collision_hook_skips_registers_nothing_and_sets_the_fallback() {
        let d = tempfile::tempdir().unwrap();
        let a = world(Some(CLI));
        let r = rec(&a, AgentKind::ClaudeCode);
        let cwd = d.path().join("proj");
        put(&cwd.join(".mcp.json"), &with_server("ember-channels"));
        let mut req = request(&a, &r);
        req.cwd = cwd.clone();
        req.instructions = Some(a.instructions_for(&r));
        assert!(req.instructions.as_ref().unwrap().contains(CHANNEL_POST_TOOL));
        a.channel_tool_config(&r, &mut req);
        assert!(req.mcp_servers.is_empty() && req.allowed_tools.is_empty());
        assert!(a.channel_fallback.lock().unwrap().contains(&r.id));
        assert!(!a.channel_reply_by_tool(&r));
        // The prompt for this very start names the command, not the tool.
        let text = req.instructions.unwrap();
        assert!(!text.contains(CHANNEL_POST_TOOL), "{text}");
        assert!(text.contains("run `ember-a2a channel post <channel-id>"));
        assert_eq!(text, a.instructions_for(&r));
        // A person mention footer is the CLI text too.
        let m = a.render(&r, &[mention("m1", "chan1", None)]).unwrap();
        assert!(m.contains("ember-a2a channel post chan1") && !m.contains(CHANNEL_POST_TOOL), "{m}");
        // ONE notice, even when the check runs again.
        let mut again = request(&a, &r);
        again.cwd = cwd;
        a.channel_tool_config(&r, &mut again);
        let n = a.sessions.store().events_after(&r.id, 0).unwrap().iter()
            .filter(|e| matches!(&e.event, AgentEvent::Notice { message } if message == CHANNEL_FALLBACK_NOTICE))
            .count();
        assert_eq!(n, 1);
    }

    #[test]
    fn fr_t12_collision_notice_text() {
        assert_eq!(
            CHANNEL_FALLBACK_NOTICE,
            "The channel tool could not be registered because a MCP server named ember-channels is already configured; replies use the ember-a2a command."
        );
    }

    #[test]
    fn fr_t12_collision_hook_skips_for_a_user_config_found_through_sources() {
        let d = tempfile::tempdir().unwrap();
        let a = world(Some(CLI));
        set_sources(&a, d.path());
        put(&d.path().join(".claude.json"), &with_server("ember-channels"));
        let r = rec(&a, AgentKind::ClaudeCode);
        let mut req = request(&a, &r);
        a.channel_tool_config(&r, &mut req);
        assert!(req.mcp_servers.is_empty() && req.allowed_tools.is_empty());
    }

    #[test]
    fn fr_t12_a_later_start_without_the_collision_registers_again_and_clears_the_set() {
        let d = tempfile::tempdir().unwrap();
        let a = world(Some(CLI));
        let r = rec(&a, AgentKind::ClaudeCode);
        let cwd = d.path().join("proj");
        put(&cwd.join(".mcp.json"), &with_server("ember-channels"));
        let mut req = request(&a, &r);
        req.cwd = cwd.clone();
        a.channel_tool_config(&r, &mut req);
        assert!(a.channel_fallback.lock().unwrap().contains(&r.id));
        std::fs::remove_file(cwd.join(".mcp.json")).unwrap();
        let mut req = request(&a, &r);
        req.cwd = cwd;
        req.instructions = Some(a.instructions_for(&r));
        a.channel_tool_config(&r, &mut req);
        assert_eq!(req.mcp_servers.len(), 1);
        assert_eq!(req.allowed_tools, vec![CHANNEL_POST_TOOL.to_string()]);
        assert!(a.channel_fallback.lock().unwrap().is_empty());
        assert!(a.channel_reply_by_tool(&r));
        assert!(req.instructions.unwrap().contains(CHANNEL_POST_TOOL), "prompt names the tool again");
    }

    // ---- Codex, Antigravity and ACP agents (FR-T12, #380 c) ----

    fn tool_harnesses() -> [AgentKind; 3] {
        [AgentKind::Codex, AgentKind::Antigravity, acp()]
    }

    /// The registered entry is `ember-a2a mcp` with the session's two variables and nothing else.
    fn assert_channel_entry(a: &A2a, r: &SessionRecord, req: &StartRequest) {
        assert_eq!(req.mcp_servers.len(), 1, "{:?}", r.agent);
        let s = &req.mcp_servers[0];
        assert_eq!(s.name, "ember-channels");
        assert_eq!(s.command, CLI);
        assert_eq!(s.args, vec!["mcp".to_string()]);
        assert_eq!(s.startup_timeout_secs, Some(10));
        let token = env_of(req, "EMBER_RUNTIME_TOKEN");
        assert_eq!(
            s.env,
            vec![
                ("EMBER_URL".to_string(), "http://127.0.0.1:8740".to_string()),
                ("EMBER_RUNTIME_TOKEN".to_string(), token.clone()),
            ],
            "the session's credentials and nothing else"
        );
        // The same token the command uses: no second one was issued.
        assert_eq!(a.store.session_for_token(&token).unwrap().as_deref(), Some(r.id.as_str()));
    }

    #[test]
    fn fr_t12_hook_registers_the_server_for_codex_antigravity_and_acp() {
        let a = world(Some(CLI));
        for agent in tool_harnesses() {
            let r = rec(&a, agent);
            let mut req = request(&a, &r);
            a.channel_tool_config(&r, &mut req);
            assert_channel_entry(&a, &r, &req);
            assert!(a.channel_reply_by_tool(&r), "{agent:?}");
            assert_eq!(a.channel_reply(&r), ChannelReply::McpTool, "{agent:?}");
        }
    }

    #[test]
    fn fr_t12_the_allow_rule_is_for_claude_code_and_codex_only() {
        let a = world(Some(CLI));
        for (agent, rule) in
            [(AgentKind::ClaudeCode, true), (AgentKind::Codex, true), (AgentKind::Antigravity, false), (acp(), false)]
        {
            let r = rec(&a, agent);
            let mut req = request(&a, &r);
            a.channel_tool_config(&r, &mut req);
            let want: Vec<String> = if rule { vec![CHANNEL_POST_TOOL.to_string()] } else { Vec::new() };
            assert_eq!(req.allowed_tools, want, "{agent:?}");
        }
    }

    #[test]
    fn fr_t12_other_harnesses_skip_under_the_same_conditions_as_claude_code() {
        for agent in tool_harnesses() {
            // Another computer.
            let a = world(Some(CLI));
            let r = rec(&a, agent);
            move_to(&a, &r, "pi-1");
            let mut req = request(&a, &r);
            a.channel_tool_config(&r, &mut req);
            assert!(req.mcp_servers.is_empty() && req.allowed_tools.is_empty(), "{agent:?}: another computer");
            assert!(!a.channel_reply_by_tool(&r), "{agent:?}");
            // A request that already runs elsewhere.
            let r = rec(&a, agent);
            let mut req = request(&a, &r);
            req.remote = Some(crate::agents::RemoteExec {
                environment_id: "ember-x".into(),
                exec_server_url: "ws://x".into(),
            });
            a.channel_tool_config(&r, &mut req);
            assert!(req.mcp_servers.is_empty() && req.allowed_tools.is_empty(), "{agent:?}: remote");
            // Missing credentials.
            for drop in ["EMBER_RUNTIME_TOKEN", "EMBER_URL"] {
                let mut req = request(&a, &r);
                req.env.retain(|(k, _)| k != drop);
                a.channel_tool_config(&r, &mut req);
                assert!(req.mcp_servers.is_empty() && req.allowed_tools.is_empty(), "{agent:?}: {drop}");
            }
            // A server of that name already in the request.
            let mut req = request(&a, &r);
            req.mcp_servers.push(McpServer {
                name: CHANNEL_MCP_SERVER.into(),
                command: "/usr/bin/other".into(),
                ..Default::default()
            });
            a.channel_tool_config(&r, &mut req);
            assert_eq!(req.mcp_servers.len(), 1, "{agent:?}");
            assert_eq!(req.mcp_servers[0].command, "/usr/bin/other", "{agent:?}");
            assert!(req.allowed_tools.is_empty(), "{agent:?}");
            // No binary.
            let b = world(None);
            let r = rec(&b, agent);
            let mut req = request(&b, &r);
            b.channel_tool_config(&r, &mut req);
            assert!(req.mcp_servers.is_empty() && req.allowed_tools.is_empty(), "{agent:?}: no binary");
        }
    }

    #[test]
    fn fr_t12_mcp_prompt_names_the_server_and_the_tool_and_keeps_the_rules_and_the_fallback() {
        let a = world(Some(CLI));
        for agent in tool_harnesses() {
            let text = a.instructions_for(&rec(&a, agent));
            assert!(
                text.contains(
                    "To answer there, call the `channel_post` tool of the MCP server `ember-channels` with `channel` set to the channel id and `text` set to your answer; if the tool is not available, run `ember-a2a channel post <channel-id> \"<text>\"`. Reply only when you were addressed"
                ),
                "{agent:?}: {text}"
            );
            // No combined tool name is claimed: only Claude Code's is measured.
            assert!(!text.contains(CHANNEL_POST_TOOL), "{agent:?}");
            assert!(text.contains("for 30 minutes after a person mentioned you there"));
            assert!(text.contains(&format!("at most {CHANNEL_POSTS_PER_MINUTE} posts a minute")));
            assert!(text.contains("Your post is shown to the people in the channel"));
            // Everything before the channel answer is the shared text.
            let cut = "To answer there, ";
            let old = a.instructions();
            assert_eq!(text[..text.find(cut).unwrap()], old[..old.find(cut).unwrap()], "{agent:?}");
        }
    }

    #[test]
    fn fr_t12_codex_launch_carries_the_server_without_the_token_and_the_one_approval() {
        let a = world(Some(CLI));
        let r = rec(&a, AgentKind::Codex);
        let mut req = request(&a, &r);
        let token = env_of(&req, "EMBER_RUNTIME_TOKEN");
        a.channel_tool_config(&r, &mut req);
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
                "mcp_servers.ember-channels.tools.channel_post.approval_mode=\"approve\"",
            ]
        );
        assert!(!args.join("\n").contains(&token), "the token is never an argument");
        // Codex forwards the two variables by name from its own environment.
        let (env, skipped) = crate::agents::codex::mcp_process_env(&req);
        assert!(skipped.is_empty());
        assert_eq!(
            env,
            [
                ("EMBER_URL".to_string(), "http://127.0.0.1:8740".to_string()),
                ("EMBER_RUNTIME_TOKEN".to_string(), token),
            ]
        );
    }

    #[test]
    fn fr_t12_antigravity_session_file_carries_the_server_and_its_credentials() {
        let a = world(Some(CLI));
        let r = rec(&a, AgentKind::Antigravity);
        let mut req = request(&a, &r);
        let token = env_of(&req, "EMBER_RUNTIME_TOKEN");
        a.channel_tool_config(&r, &mut req);
        let cfg = crate::agents::antigravity::mcp_config(&req.mcp_servers).unwrap();
        assert_eq!(
            cfg,
            serde_json::json!({ "mcpServers": { "ember-channels": {
                "command": CLI,
                "args": ["mcp"],
                "env": { "EMBER_URL": "http://127.0.0.1:8740", "EMBER_RUNTIME_TOKEN": token },
            } } })
        );
    }

    #[test]
    fn fr_t12_acp_session_new_carries_the_server_and_its_credentials() {
        let a = world(Some(CLI));
        let r = rec(&a, acp());
        let mut req = request(&a, &r);
        let token = env_of(&req, "EMBER_RUNTIME_TOKEN");
        a.channel_tool_config(&r, &mut req);
        assert_eq!(
            crate::agents::acp::mcp_servers_param(&req.mcp_servers),
            serde_json::json!([{
                "name": "ember-channels",
                "command": CLI,
                "args": ["mcp"],
                "env": [
                    { "name": "EMBER_URL", "value": "http://127.0.0.1:8740" },
                    { "name": "EMBER_RUNTIME_TOKEN", "value": token },
                ],
            }])
        );
    }

    // ---- name check in the user's own Codex and Antigravity configuration ----

    #[test]
    fn fr_t12_codex_name_check_reads_the_user_project_and_system_files() {
        let d = tempfile::tempdir().unwrap();
        let cwd = d.path().join("proj/sub");
        std::fs::create_dir_all(&cwd).unwrap();
        let home = d.path().join("codex-home");
        assert_eq!(codex_channel_name_taken(&cwd, Some(home.as_path()), &[]), NameCheck::Free, "no file at all");
        put(&home.join("config.toml"), "model = \"x\"\n[mcp_servers.other]\ncommand = \"y\"\n");
        assert_eq!(codex_channel_name_taken(&cwd, Some(home.as_path()), &[]), NameCheck::Free);
        put(&home.join("config.toml"), "[mcp_servers.Ember-Channels]\ncommand = \"y\"\n");
        assert_eq!(
            codex_channel_name_taken(&cwd, Some(home.as_path()), &[]),
            NameCheck::Taken { kind: "user config.toml" },
            "letter case is ignored"
        );
        assert_eq!(codex_channel_name_taken(&cwd, None, &[]), NameCheck::Free);
        // A project file of an ancestor.
        put(&d.path().join("proj/.codex/config.toml"), "[mcp_servers.\"ember-channels\"]\ncommand = \"y\"\n");
        assert_eq!(codex_channel_name_taken(&cwd, None, &[]), NameCheck::Taken { kind: "project .codex/config.toml" });
        std::fs::remove_file(d.path().join("proj/.codex/config.toml")).unwrap();
        // A system file.
        let system = d.path().join("etc/config.toml");
        put(&system, "# ember-channels is ours\n");
        assert_eq!(
            codex_channel_name_taken(&cwd, None, &[system]),
            NameCheck::Taken { kind: "system config.toml" },
            "a comment that mentions the name counts: fail closed"
        );
    }

    /// A quoted key may spell a character as an escape: `"ember\u002dchannels"` is the key
    /// `ember-channels` for TOML and for JSON, so the text search decodes ASCII escapes.
    #[test]
    fn fr_t12_name_check_sees_a_name_written_with_an_escape() {
        let d = tempfile::tempdir().unwrap();
        let cwd = d.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        for key in ["ember\\u002dchannels", "ember\\U0000002Dchannels", "\\x65mber-channels", "ember\\u002Dchannel\\u0073"] {
            put(&cwd.join(".codex/config.toml"), &format!("[mcp_servers.\"{key}\"]\ncommand = \"y\"\n"));
            assert_eq!(
                codex_channel_name_taken(&cwd, None, &[]),
                NameCheck::Taken { kind: "project .codex/config.toml" },
                "{key}"
            );
        }
        std::fs::remove_file(cwd.join(".codex/config.toml")).unwrap();
        let home = d.path().join("home");
        put(&home.join(".gemini/config/mcp_config.json"), &with_server("ember\\u002dchannels"));
        assert_eq!(agy_channel_name_taken(&cwd, Some(home.as_path())), NameCheck::Taken { kind: "user mcp_config.json" });
        // A Windows path in a literal string is no escape and no name.
        assert!(!mentions_channel_server(b"notify = 'C:\\Users\\x\\update.exe'\n"));
        assert_eq!(unescape_ascii(b"a\\u00e9\\u002d\\x4"), b"a\\u00e9-\\x4".to_vec());
    }

    #[cfg(unix)]
    #[test]
    fn fr_t12_codex_name_check_fails_closed_on_a_file_it_cannot_judge() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("codex-home");
        std::fs::create_dir_all(home.join("config.toml")).unwrap();
        assert_eq!(
            codex_channel_name_taken(d.path(), Some(home.as_path()), &[]),
            NameCheck::Unsure { kind: "user config.toml", why: "not a regular file" }
        );
    }

    #[test]
    fn fr_t12_agy_name_check_reads_the_user_legacy_and_project_files() {
        let d = tempfile::tempdir().unwrap();
        let cwd = d.path().join("proj/sub");
        std::fs::create_dir_all(&cwd).unwrap();
        let home = d.path().join("home");
        assert_eq!(agy_channel_name_taken(&cwd, Some(home.as_path())), NameCheck::Free);
        // agy accepts comments and trailing commas in this file, and an empty legacy file exists
        // on real machines: neither is an error for a text search.
        put(&home.join(".gemini/config/mcp_config.json"), "{ // mine\n \"mcpServers\": { \"other\": { \"command\": \"x\", }, } }");
        put(&home.join(".gemini/antigravity/mcp_config.json"), "");
        assert_eq!(agy_channel_name_taken(&cwd, Some(home.as_path())), NameCheck::Free);
        put(&home.join(".gemini/antigravity/mcp_config.json"), &with_server("EMBER-channels"));
        assert_eq!(agy_channel_name_taken(&cwd, Some(home.as_path())), NameCheck::Taken { kind: "user mcp_config.json" });
        put(&home.join(".gemini/antigravity/mcp_config.json"), "");
        put(&home.join(".gemini/config/mcp_config.json"), &with_server("ember-channels"));
        assert_eq!(agy_channel_name_taken(&cwd, Some(home.as_path())), NameCheck::Taken { kind: "user mcp_config.json" });
        put(&d.path().join("proj/.agents/mcp_config.json"), &with_server("ember-channels"));
        assert_eq!(
            agy_channel_name_taken(&cwd, None),
            NameCheck::Taken { kind: "project .agents/mcp_config.json" }
        );
    }

    #[test]
    fn fr_t12_hook_steps_aside_for_a_name_taken_in_codex_or_antigravity_configuration() {
        let d = tempfile::tempdir().unwrap();
        let a = world(Some(CLI));
        set_sources(&a, d.path());
        put(&d.path().join(".codex/config.toml"), "[mcp_servers.ember-channels]\ncommand = \"y\"\n");
        put(&d.path().join(".gemini/config/mcp_config.json"), &with_server("ember-channels"));
        for agent in [AgentKind::Codex, AgentKind::Antigravity] {
            let r = rec(&a, agent);
            let mut req = request(&a, &r);
            req.instructions = Some(a.instructions_for(&r));
            assert!(req.instructions.as_ref().unwrap().contains("tool of the MCP server"), "{agent:?}");
            a.channel_tool_config(&r, &mut req);
            assert!(req.mcp_servers.is_empty() && req.allowed_tools.is_empty(), "{agent:?}");
            assert!(a.channel_fallback.lock().unwrap().contains(&r.id), "{agent:?}");
            assert_eq!(a.channel_reply(&r), ChannelReply::Command, "{agent:?}");
            // The prompt of this very start is the old text again.
            assert_eq!(req.instructions.unwrap(), a.instructions(), "{agent:?}");
            let n = a.sessions.store().events_after(&r.id, 0).unwrap().iter()
                .filter(|e| matches!(&e.event, AgentEvent::Notice { message } if message == CHANNEL_FALLBACK_NOTICE))
                .count();
            assert_eq!(n, 1, "{agent:?}");
        }
        // An ACP agent's own configuration is not known to Ember: nothing is checked.
        let r = rec(&a, acp());
        let mut req = request(&a, &r);
        a.channel_tool_config(&r, &mut req);
        assert_eq!(req.mcp_servers.len(), 1);
    }

    #[test]
    fn fr_t12_codex_home_of_the_start_request_wins_over_the_default() {
        let d = tempfile::tempdir().unwrap();
        let a = world(Some(CLI));
        set_sources(&a, d.path());
        put(&d.path().join(".codex/config.toml"), "[mcp_servers.ember-channels]\ncommand = \"y\"\n");
        let account = d.path().join("account");
        std::fs::create_dir_all(&account).unwrap();
        let r = rec(&a, AgentKind::Codex);
        let mut req = request(&a, &r);
        req.env.push(("CODEX_HOME".into(), account.to_string_lossy().into_owned()));
        a.channel_tool_config(&r, &mut req);
        assert_eq!(req.mcp_servers.len(), 1, "the account's home has no such server");
    }

    #[cfg(unix)]
    #[test]
    fn fr_t12_unsure_notice_names_the_harness() {
        let d = tempfile::tempdir().unwrap();
        let a = world(Some(CLI));
        set_sources(&a, d.path());
        std::fs::create_dir_all(d.path().join(".codex/config.toml")).unwrap();
        let r = rec(&a, AgentKind::Codex);
        let mut req = request(&a, &r);
        a.channel_tool_config(&r, &mut req);
        assert!(req.mcp_servers.is_empty() && req.allowed_tools.is_empty(), "fail closed");
        let notices: Vec<String> = a.sessions.store().events_after(&r.id, 0).unwrap().into_iter()
            .filter_map(|e| match e.event { AgentEvent::Notice { message } => Some(message), _ => None })
            .collect();
        assert_eq!(
            notices,
            ["The channel tool was not registered because the Codex configuration (user config.toml) could not be checked (not a regular file); replies use the ember-a2a command."]
        );
        assert_eq!(
            unsure_notice("user config", "unreadable"),
            "The channel tool was not registered because the Claude Code configuration (user config) could not be checked (unreadable); replies use the ember-a2a command."
        );
    }

    // ---- Codex layers the name check does not read ----

    fn layer_paths(home: &Path, managed: &[PathBuf]) -> CodexLayerPaths {
        codex_layer_paths(Some(home), managed)
    }

    #[test]
    fn fr_t12_codex_layer_detection_finds_nothing_without_a_layer() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("codex-home");
        assert_eq!(codex_unread_layer(&layer_paths(&home, &[])), None, "no CODEX_HOME at all");
        put(&home.join("config.toml"), "model = \"x\"\n");
        put(&home.join("plugins/p/README.md"), "");
        put(&home.join("notes.toml"), "");
        assert_eq!(codex_unread_layer(&layer_paths(&home, &[d.path().join("absent.plist")])), None);
        assert_eq!(codex_unread_layer(&CodexLayerPaths::default()), None);
    }

    #[test]
    fn fr_t12_codex_layer_detection_sees_each_kind_by_presence() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("codex-home");
        put(&home.join("config.toml"), "");
        put(&home.join("work.config.toml"), "not even toml [[[");
        assert_eq!(
            codex_unread_layer(&layer_paths(&home, &[])),
            Some(UnreadLayer { kind: KIND_CODEX_PROFILE, unreadable: false })
        );
        std::fs::remove_file(home.join("work.config.toml")).unwrap();
        for file in ["plugins/one/.mcp.json", "plugins/one/.codex-plugin/plugin.json"] {
            put(&home.join(file), "garbage");
            assert_eq!(
                codex_unread_layer(&layer_paths(&home, &[])),
                Some(UnreadLayer { kind: KIND_CODEX_PLUGIN, unreadable: false }),
                "{file}"
            );
            std::fs::remove_file(home.join(file)).unwrap();
        }
        // Deeper than three levels is not looked at.
        put(&home.join("plugins/a/b/c/d/.mcp.json"), "");
        assert_eq!(codex_unread_layer(&layer_paths(&home, &[])), None);
        let plist = d.path().join("Managed Preferences/com.openai.codex.plist");
        put(&plist, "");
        assert_eq!(
            codex_unread_layer(&layer_paths(&home, std::slice::from_ref(&plist))),
            Some(UnreadLayer { kind: KIND_CODEX_MANAGED, unreadable: false })
        );
        // A managed directory counts as well.
        let dir = d.path().join("ProgramData/OpenAI/Codex");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            codex_unread_layer(&layer_paths(&home, &[dir])),
            Some(UnreadLayer { kind: KIND_CODEX_MANAGED, unreadable: false })
        );
    }

    #[cfg(unix)]
    #[test]
    fn fr_t12_codex_layer_detection_fails_closed_on_a_location_it_cannot_read() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        // CODEX_HOME is a file: it cannot be listed.
        let home = d.path().join("codex-home");
        put(&home, "");
        assert_eq!(
            codex_unread_layer(&layer_paths(&home, &[])),
            Some(UnreadLayer { kind: KIND_CODEX_PROFILE, unreadable: true })
        );
        // A plugin directory that cannot be listed (root can list it anyway, so skip then).
        let home = d.path().join("codex-home2");
        let locked = home.join("plugins/locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read_dir(&locked).is_err() {
            assert_eq!(
                codex_unread_layer(&layer_paths(&home, &[])),
                Some(UnreadLayer { kind: KIND_CODEX_PLUGIN, unreadable: true })
            );
        }
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        // A managed path whose parent is a file cannot be judged.
        let file = d.path().join("prefs");
        put(&file, "");
        assert_eq!(
            codex_unread_layer(&layer_paths(&home, &[file.join("com.openai.codex.plist")])),
            Some(UnreadLayer { kind: KIND_CODEX_MANAGED, unreadable: true })
        );
    }

    #[test]
    fn fr_t12_codex_layer_locations_follow_the_platform() {
        let home = Path::new("/h/.codex");
        let p = codex_layer_paths(Some(home), &[PathBuf::from("/m")]);
        assert_eq!(p.profile_dirs, [home.to_path_buf()]);
        assert_eq!(p.plugin_roots, [home.join("plugins")]);
        assert_eq!(p.managed, [PathBuf::from("/m")]);
        assert_eq!(codex_layer_paths(None, &[]), CodexLayerPaths::default());
        let real = codex_managed_locations();
        if cfg!(target_os = "macos") {
            assert!(real.contains(&PathBuf::from("/Library/Managed Preferences/com.openai.codex.plist")));
            assert!(real.contains(&PathBuf::from("/Library/Preferences/com.openai.codex.plist")));
        } else if !cfg!(windows) {
            assert!(real.is_empty());
        }
    }

    #[test]
    fn fr_t12_codex_layer_present_registers_the_server_without_the_auto_approval() {
        let d = tempfile::tempdir().unwrap();
        let a = world(Some(CLI));
        set_sources(&a, d.path());
        put(&d.path().join(".codex/work.config.toml"), "");
        let r = rec(&a, AgentKind::Codex);
        let mut req = request(&a, &r);
        a.channel_tool_config(&r, &mut req);
        assert_eq!(req.mcp_servers.len(), 1, "the server is still registered");
        assert!(req.allowed_tools.is_empty(), "but not auto-approved");
        let args = crate::agents::codex::app_server_args(&req);
        assert!(!args.iter().any(|x| x.contains("approval_mode")), "{args:?}");
        assert_eq!(a.channel_reply(&r), ChannelReply::McpTool);
        // The managed preferences alone do the same.
        std::fs::remove_file(d.path().join(".codex/work.config.toml")).unwrap();
        let plist = d.path().join("com.openai.codex.plist");
        put(&plist, "");
        a.claude_sources.lock().unwrap().codex_managed = vec![plist.clone()];
        let mut req = request(&a, &r);
        a.channel_tool_config(&r, &mut req);
        assert!(req.allowed_tools.is_empty());
        // Without any layer the one approval is back, exactly once.
        std::fs::remove_file(&plist).unwrap();
        let mut req = request(&a, &r);
        a.channel_tool_config(&r, &mut req);
        let approvals: Vec<String> = crate::agents::codex::app_server_args(&req)
            .into_iter()
            .filter(|x| x.contains("approval_mode"))
            .collect();
        assert_eq!(approvals, ["mcp_servers.ember-channels.tools.channel_post.approval_mode=\"approve\""]);
    }
}
