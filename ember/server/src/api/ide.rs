//! "Open IDE" launch targets for a session.
//!
//! `GET /api/sessions/{id}/ide[?computer=<name>]` returns, for the session's project and
//! current computer, how to open each IDE. The server only returns data; it never opens or
//! renders anything:
//!
//! | `kind`    | What it is                                    | What we return                    |
//! |-----------|-----------------------------------------------|-----------------------------------|
//! | `ember`   | Ember's own editor-core IDE (dioxus-compose, M8) | launch info only (no URL)       |
//! | `vscode`  | VS Code Web of that computer's OSE, in the IDE window | `{kind}` alone; started on pick by `POST .../ide/vscode` |
//! | `gateway` | JetBrains Gateway                             | a `jetbrains-gateway://` link and the command that opens it on that computer |
//!
//! **Computers come from the registry** (`FR-L7`, `FR-W4`) when the router is built with
//! [`router_with_computers`]: the IDE computers are the implicit local computer plus every
//! registered node, and the launch's `computer` is the REGISTRY id (what
//! `/api/computers/{id}/terms` expects), with the display name beside it as `computer_name`.
//! The computer is `?computer=` (a registry id, a registry name or an `EMBER_IDE_COMPUTERS`
//! name), else the session's current computer, else [`IdeConfig::default_computer`], else the
//! local computer. `EMBER_IDE_COMPUTERS` stays as an override: an entry whose name is a registry
//! id or name replaces that computer's IDE details (the id stays the registry id); an entry that
//! matches none is still a computer, with id = name. Without a registry ([`router`]) only the
//! `EMBER_IDE_COMPUTERS` list exists, exactly as before.
//! The folder is the session's `cwd`, taken as a path on that computer.
//!
//! **The VS Code target starts only when it is picked** (SPEC FR-W4c (1b), (1c), D55, #1064).
//! The listing carries it as `{kind: "vscode"}` alone (no `available`, no `reason`, no `launch`)
//! on every computer and starts nothing, so listing the targets never costs a second OSE process:
//! VS Code is always offered, because the DarkPyonix manager installed at the first connection
//! brings the OSE. `POST /api/sessions/{id}/ide/vscode[?computer=<name>]` (the same computer
//! resolution) starts or reuses the folder's token-free OSE on a 0700 Unix socket: a registered
//! computer's node does it (`POST /ide/ose {folder, target: "vscode"}`), the local computer's comes
//! from this server's own manager. It answers the launch, whose `ose` is
//! `{relay: "/api/computers/{id}/ide/relay/{launch_id}"}` (plus `ose_commit` when known), or
//! `{code, error}`, the only place a VS Code failure appears. A computer known only from
//! `EMBER_IDE_COMPUTERS` and not local has no OSE to relay to: its Ember target is unavailable and
//! picking VS Code there is an error. `ide_url` (and `EMBER_IDE_URL`) no longer makes a VS Code
//! URL; `ide_url` is still accepted so an existing configuration parses.
//!
//! Link formats:
//! - Gateway: `jetbrains-gateway://connect#type=ssh&host=…&port=…&user=…&projectPath=…`
//!   `[&idePath=…&deploy=false]`, as in JetBrains' "Connect and work with JetBrains Gateway"
//!   (<https://www.jetbrains.com/help/idea/remote-development-a.html>). JetBrains is moving
//!   Gateway into the Toolbox App with a `jetbrains://` scheme and only "limited backward
//!   compatibility" for these links (<https://www.jetbrains.com/help/toolbox-app/jetbrains-gateway-migrations-guide.html>).

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use ember_node::client::ClientError;
use ember_node::ose_api::{pump_ose_relay, OseRelayStream};
use ember_node::proto::ErrorCode;
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::computers::{Computer, Computers, LOCAL, LOCAL_NAME};
use crate::ose::{
    valid_computer_id, OseManager, OseTarget, RelayLookupError, LOCAL_COMPUTER, NOT_INSTALLED, SOCKET_UNSUPPORTED,
};
use crate::session::Sessions;
use crate::store::SessionRecord;

/// The relay of the local OSE and its placeholder substitution are `ember_node::ose_api`: the
/// node runs the same relay for the OSE of its own projects (SPEC FR-W4b, #896).
pub use ember_node::ose_api::RELAY_SUBSTITUTION_WINDOW;
/// The connection of one OSE launch, defined beside the manager that builds it.
pub use crate::ose::EmberOseConnection;

/// Why a computer that is neither local nor registered has no Ember target.
const LOCAL_ONLY: &str = "OSE server is only available on the local computer";
/// Longest wait for a node's `POST /ide/ose` (SPEC FR-W4c): a new OSE takes up to 10 s to listen
/// on the node, and a node that never answers must not hold the launch request open.
pub const NODE_OSE_START_TIMEOUT: Duration = Duration::from_secs(20);
/// Longest wait for the node's relay socket to open, before the client's upgrade is answered.
const NODE_RELAY_OPEN_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest wait for a close handshake when a bridged relay ends.
const NODE_RELAY_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
/// The `code` of the 502 that says the NODE refused this server's credential (SPEC FR-W4c).
pub const NODE_AUTH_FAILED: &str = "node_auth_failed";

/// True for a node status that means the node refused this server (its bearer token, or the
/// peer allow-list), not the request.
pub(crate) fn node_refused_server(status: u16) -> bool {
    status == StatusCode::UNAUTHORIZED.as_u16() || status == StatusCode::FORBIDDEN.as_u16()
}

/// The answer when the node of `computer` refused this server's credential (a stale node token in
/// the registry): 502, never the node's 401 or 403. Passed on unchanged, that status would read as
/// the CALLER's credential being refused, and the app would drop its server token or open the
/// owner sheet. The body names the computer and no token.
pub(crate) fn node_auth_failed(computer: &str) -> Response {
    let body = json!({ "code": NODE_AUTH_FAILED, "error": node_auth_failed_message(computer) });
    (StatusCode::BAD_GATEWAY, Json(body)).into_response()
}

/// The English message of [`node_auth_failed`]: names the computer, never a token.
fn node_auth_failed_message(computer: &str) -> String {
    format!(
        "the server could not authenticate to computer {computer}: the computer refused the node token the server holds; enrol the computer again"
    )
}

/// One computer's IDE endpoints.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct ComputerIde {
    /// The computer the main server runs on.
    #[serde(default)]
    pub local: bool,
    /// Ignored since D55 (#1064): the VS Code target goes through the relay and no longer uses a
    /// pre-set address. Still parsed, so an existing `EMBER_IDE_COMPUTERS` stays valid.
    pub ide_url: Option<String>,
    /// SSH destination host for Gateway.
    pub ssh_host: Option<String>,
    pub ssh_user: Option<String>,
    pub ssh_port: Option<u16>,
    /// An IDE backend already installed on the computer (Gateway's `idePath`). Without it
    /// Gateway asks which IDE to use.
    pub jetbrains_ide_path: Option<String>,
    /// `macos`, `linux` or `windows`: decides the command that opens a link there.
    pub os: Option<String>,
}

/// `EMBER_IDE_COMPUTERS`: inline JSON (starting with `{`) or a path to a JSON file:
///
/// ```json
/// { "default_computer": "mini",
///   "server_url": "http://mini.local:8740",
///   "computers": {
///     "mini":   { "local": true, "os": "macos" },
///     "studio": { "ssh_host": "studio.local", "ssh_user": "me", "os": "linux" } } }
/// ```
///
/// Unset: one local computer named `local`. `EMBER_IDE_URL` is no longer read (D55).
/// `server_url` (what the Ember editor connects to) defaults to `EMBER_PUBLIC_URL`, else the
/// request's `Host`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct IdeConfig {
    pub default_computer: String,
    #[serde(default)]
    pub server_url: Option<String>,
    pub computers: BTreeMap<String, ComputerIde>,
}

impl Default for IdeConfig {
    fn default() -> Self {
        let mut computers = BTreeMap::new();
        computers.insert("local".to_string(), ComputerIde { local: true, ..Default::default() });
        IdeConfig { default_computer: "local".into(), server_url: None, computers }
    }
}

impl IdeConfig {
    pub fn from_env() -> anyhow::Result<IdeConfig> {
        let mut cfg = match std::env::var("EMBER_IDE_COMPUTERS") {
            Ok(v) if !v.trim().is_empty() => Self::parse(&v)?,
            _ => IdeConfig::default(),
        };
        if cfg.server_url.is_none() {
            cfg.server_url = std::env::var("EMBER_PUBLIC_URL").ok().filter(|u| !u.is_empty());
        }
        Ok(cfg)
    }

    /// Inline JSON or a file path.
    pub fn parse(value: &str) -> anyhow::Result<IdeConfig> {
        let text = if value.trim_start().starts_with('{') {
            value.to_string()
        } else {
            std::fs::read_to_string(value.trim())
                .map_err(|e| anyhow::anyhow!("EMBER_IDE_COMPUTERS: cannot read {value}: {e}"))?
        };
        let cfg: IdeConfig = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("EMBER_IDE_COMPUTERS: invalid JSON: {e}"))?;
        if !cfg.computers.contains_key(&cfg.default_computer) {
            anyhow::bail!(
                "EMBER_IDE_COMPUTERS: default_computer {:?} is not in computers",
                cfg.default_computer
            );
        }
        Ok(cfg)
    }
}

/// What the Ember editor needs to open a project (it is a native app, not a URL).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EmberLaunch {
    pub session: String,
    pub project: String,
    /// The computer's registry id (or, without a registry, its `EMBER_IDE_COMPUTERS` name).
    pub computer: String,
    /// The computer's display name; present when the computers registry is in use.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub computer_name: Option<String>,
    pub folder: String,
    /// The main server the editor connects to.
    pub server_url: Option<String>,
    /// Connection details when an OSE endpoint is reachable by the editor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ose: Option<EmberOseConnection>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Target {
    /// `ember`, `vscode` or `gateway`.
    pub kind: &'static str,
    /// Whether the target can be opened now. `None`, and left out of the JSON, for the VS Code
    /// target, which is always offered and starts only on pick (SPEC FR-W4c (1b), D55): its
    /// failure is the error of `POST /api/sessions/{id}/ide/vscode`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// argv that opens `url` on the target computer (Gateway), for ember node to run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch: Option<EmberLaunch>,
    /// Why the target is unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Target {
    fn unavailable(kind: &'static str, reason: String) -> Target {
        Target { kind, available: Some(false), url: None, command: None, launch: None, reason: Some(reason) }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ComputerRef {
    /// The computer's id (the same value as `launch.computer`).
    pub name: String,
    pub local: bool,
    /// The computer's display name; present when the computers registry is in use.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub computer_name: Option<String>,
}

/// One computer the IDE can target, after merging the registry with `EMBER_IDE_COMPUTERS`.
#[derive(Debug, Clone, PartialEq)]
pub struct IdeComputer {
    /// Registry id, or the `EMBER_IDE_COMPUTERS` name when no registry computer matches.
    pub id: String,
    pub name: String,
    pub ide: ComputerIde,
    /// Other names `?computer=` accepts for it (the matching `EMBER_IDE_COMPUTERS` name).
    pub aliases: Vec<String>,
    /// Whether `computer_name` is reported (registry in use).
    pub named: bool,
}

impl IdeComputer {
    fn matches(&self, q: &str) -> bool {
        self.id == q || self.name == q || self.aliases.iter().any(|a| a == q)
    }
}

/// The IDE computers: the registry (the local computer first, then the nodes) with the
/// `EMBER_IDE_COMPUTERS` entries merged on by id or name, and the names of entries that matched
/// no registry computer. `registry: None` gives the env list alone (id = name), as before.
pub fn resolve_computers(
    config: &IdeConfig,
    registry: Option<&[Computer]>,
) -> (Vec<IdeComputer>, Vec<String>) {
    let Some(registered) = registry else {
        let all = config
            .computers
            .iter()
            .map(|(n, ide)| IdeComputer {
                id: n.clone(), name: n.clone(), ide: ide.clone(), aliases: vec![], named: false,
            })
            .collect();
        return (all, vec![]);
    };
    let mut all = vec![IdeComputer {
        id: LOCAL.into(),
        name: LOCAL_NAME.into(),
        ide: ComputerIde { local: true, ..Default::default() },
        aliases: vec![],
        named: true,
    }];
    all.extend(registered.iter().map(|c| IdeComputer {
        id: c.id.clone(), name: c.name.clone(), ide: ComputerIde::default(), aliases: vec![], named: true,
    }));
    let mut unmatched = Vec::new();
    for (n, ide) in &config.computers {
        match all.iter_mut().find(|c| c.id == *n || c.name == *n) {
            Some(c) => {
                c.ide = ide.clone();
                if c.id != *n && c.name != *n {
                    c.aliases.push(n.clone());
                }
            }
            None => {
                unmatched.push(n.clone());
                all.push(IdeComputer {
                    id: n.clone(), name: n.clone(), ide: ide.clone(), aliases: vec![], named: true,
                });
            }
        }
    }
    (all, unmatched)
}

/// The computer `q` names: an id first, then a name or alias.
pub fn find_computer<'a>(all: &'a [IdeComputer], q: &str) -> Option<&'a IdeComputer> {
    all.iter().find(|c| c.id == q).or_else(|| all.iter().find(|c| c.matches(q)))
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct IdeLaunch {
    pub session: String,
    pub project: String,
    pub folder: String,
    pub computer: ComputerRef,
    /// Always in the order ember, vscode, gateway.
    pub targets: Vec<Target>,
}

/// Percent-encodes everything but RFC 3986 unreserved characters (and `/` when asked).
pub fn percent_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let keep = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'.' | b'_' | b'~')
            || (keep_slash && b == b'/');
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `jetbrains-gateway://connect#…` for an SSH connection, or why one cannot be built.
pub fn gateway_url(c: &ComputerIde, folder: &str) -> Result<String, String> {
    let host = c.ssh_host.as_deref().filter(|h| !h.is_empty()).ok_or_else(|| {
        "no ssh_host configured for this computer; Gateway connects over SSH".to_string()
    })?;
    let mut params = vec![
        ("type", "ssh".to_string()),
        ("host", host.to_string()),
        ("port", c.ssh_port.unwrap_or(22).to_string()),
    ];
    if let Some(user) = c.ssh_user.as_deref().filter(|u| !u.is_empty()) {
        params.push(("user", user.to_string()));
    }
    params.push(("projectPath", folder.to_string()));
    if let Some(ide) = c.jetbrains_ide_path.as_deref().filter(|p| !p.is_empty()) {
        params.push(("idePath", ide.to_string()));
        params.push(("deploy", "false".to_string()));
    }
    let fragment: Vec<String> =
        params.iter().map(|(k, v)| format!("{k}={}", percent_encode(v, false))).collect();
    Ok(format!("jetbrains-gateway://connect#{}", fragment.join("&")))
}

/// The argv that hands a URL to its registered handler on a computer of this OS.
pub fn open_command(os: Option<&str>, url: &str) -> Option<Vec<String>> {
    let argv: Vec<&str> = match os? {
        "macos" => vec!["open", url],
        "linux" => vec!["xdg-open", url],
        "windows" => vec!["cmd", "/c", "start", "", url],
        _ => return None,
    };
    Some(argv.into_iter().map(String::from).collect())
}

/// The launch targets for one session on one computer known only by its `EMBER_IDE_COMPUTERS` name.
pub fn launch_targets(
    rec: &SessionRecord,
    computer_name: &str,
    computer: &ComputerIde,
    server_url: Option<&str>,
    ose: &OseManager,
) -> IdeLaunch {
    let c = IdeComputer {
        id: computer_name.into(), name: computer_name.into(), ide: computer.clone(),
        aliases: vec![], named: false,
    };
    launch_targets_for(rec, &c, server_url, ose)
}

/// The launch targets for one session on one resolved computer, with the OSE of this server for
/// a local computer. A registered computer's OSE runs on its node: the route asks the node and
/// calls [`launch_targets_with`] (SPEC FR-W4c). Only the Ember target's OSE is started; the VS
/// Code target is listed and starts on pick (FR-W4c (1b), D55).
pub fn launch_targets_for(
    rec: &SessionRecord,
    target: &IdeComputer,
    server_url: Option<&str>,
    ose: &OseManager,
) -> IdeLaunch {
    let folder = std::path::Path::new(&rec.cwd);
    let connection = if target.ide.local { ose.connection(folder) } else { Err(LOCAL_ONLY.into()) };
    launch_targets_with(rec, target, server_url, connection)
}

/// The launch of `rec`'s folder on `target`, with `ose` as its connection.
fn launch_of(
    rec: &SessionRecord,
    target: &IdeComputer,
    server_url: Option<&str>,
    ose: Option<EmberOseConnection>,
) -> EmberLaunch {
    EmberLaunch {
        session: rec.id.clone(),
        project: rec.project.clone(),
        computer: target.id.clone(),
        computer_name: target.named.then(|| target.name.clone()),
        folder: rec.cwd.clone(),
        server_url: server_url.map(String::from),
        ose,
    }
}

/// The launch targets for one session on one resolved computer, given the OSE connection of the
/// Ember target (or why there is none). The VS Code target is `{kind: "vscode"}` alone, on every
/// computer: it is started on pick by `POST /api/sessions/{id}/ide/vscode`, whose error is the
/// only place a VS Code failure appears (SPEC FR-W4c (1b), (1c), D55).
pub fn launch_targets_with(
    rec: &SessionRecord,
    target: &IdeComputer,
    server_url: Option<&str>,
    connection: Result<EmberOseConnection, String>,
) -> IdeLaunch {
    let computer_name = target.id.as_str();
    let computer = &target.ide;
    let display = target.named.then(|| target.name.clone());
    let folder = rec.cwd.clone();
    let ember = Target {
        kind: "ember",
        available: Some(connection.is_ok()),
        url: None,
        command: None,
        launch: Some(launch_of(rec, target, server_url, connection.as_ref().ok().cloned())),
        reason: connection.err(),
    };
    let vscode = Target { kind: "vscode", available: None, url: None, command: None, launch: None, reason: None };

    let gateway = match gateway_url(computer, &folder) {
        Ok(url) => Target {
            kind: "gateway",
            available: Some(true),
            command: open_command(computer.os.as_deref(), &url),
            url: Some(url),
            launch: None,
            reason: None,
        },
        Err(reason) => Target::unavailable("gateway", reason),
    };

    IdeLaunch {
        session: rec.id.clone(),
        project: rec.project.clone(),
        folder,
        computer: ComputerRef { name: computer_name.to_string(), local: computer.local, computer_name: display },
        targets: vec![ember, vscode, gateway],
    }
}

/// Why `POST /api/sessions/{id}/ide/vscode` gave no launch: the status and the body
/// `{code, error}` (SPEC FR-W4c (1c)).
#[derive(Debug, Clone, PartialEq)]
pub struct VscodeLaunchError {
    pub status: StatusCode,
    pub code: &'static str,
    pub error: String,
}

impl VscodeLaunchError {
    fn new(status: StatusCode, code: &'static str, error: impl Into<String>) -> Self {
        VscodeLaunchError { status, code, error: error.into() }
    }
}

impl IntoResponse for VscodeLaunchError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "code": self.code, "error": self.error }))).into_response()
    }
}

/// `code` of the 404 for a session this server does not know.
pub const SESSION_NOT_FOUND: &str = "session_not_found";
/// `code` of the 404 for a `?computer=` that names no IDE computer.
pub const UNKNOWN_COMPUTER: &str = "unknown_computer";
/// `code` of the 409 for a computer that cannot run the VS Code target (known only from
/// `EMBER_IDE_COMPUTERS` and not local, so no OSE to relay to, or a platform without Unix
/// sockets).
pub const VSCODE_UNAVAILABLE: &str = "vscode_unavailable";
/// `code` of the 503 when the OSE binary is missing (on this server or on the node).
pub const OSE_NOT_INSTALLED: &str = "ose_not_installed";
/// `code` of the 502 when the node does not answer.
pub const NODE_UNREACHABLE: &str = "node_unreachable";
/// `code` of the 502 when the node answered a launch with a token or an endpoint: it is older than
/// D55 and ignored `target` (its message is [`NODE_TOO_OLD_FOR_VSCODE`]).
pub const NODE_TOO_OLD_FOR_VSCODE_CODE: &str = "node_too_old_for_vscode";
/// `code` of the 502 when the node refused the launch for another reason (its text is the error).
pub const NODE_ERROR: &str = "node_error";
/// `code` of the 500 when this server's own OSE did not start (its text is the error).
pub const OSE_START_FAILED: &str = "ose_start_failed";
/// `code` of a 500 for an internal failure (the store, a task).
pub const INTERNAL: &str = "internal";

/// Starts or reuses the local computer's VS Code OSE of `rec`'s folder on this server's own
/// manager and answers its launch (SPEC FR-W4c (1c), FR-W4a). Blocking for up to 10 s while a
/// new OSE starts: call from `spawn_blocking`.
pub fn local_vscode_launch(
    rec: &SessionRecord,
    target: &IdeComputer,
    server_url: Option<&str>,
    ose: &OseManager,
) -> Result<EmberLaunch, VscodeLaunchError> {
    match ose.launch_for_target(LOCAL_COMPUTER, std::path::Path::new(&rec.cwd), OseTarget::Vscode) {
        Ok(l) => Ok(launch_of(rec, target, server_url, Some(l.connection))),
        Err(m) if m == NOT_INSTALLED => Err(VscodeLaunchError::new(StatusCode::SERVICE_UNAVAILABLE, OSE_NOT_INSTALLED, m)),
        Err(m) if m == SOCKET_UNSUPPORTED => Err(VscodeLaunchError::new(StatusCode::CONFLICT, VSCODE_UNAVAILABLE, m)),
        Err(m) => Err(VscodeLaunchError::new(StatusCode::INTERNAL_SERVER_ERROR, OSE_START_FAILED, m)),
    }
}

/// The answer to a node's `POST /ide/ose {folder, target: "vscode"}` for computer `id`, as the
/// launch of `rec`'s folder or the error of route (1c). Pure, for tests.
pub fn node_vscode_launch(
    rec: &SessionRecord,
    target: &IdeComputer,
    server_url: Option<&str>,
    answer: Result<ember_node::ose_api::OseLaunchResponse, ClientError>,
) -> Result<EmberLaunch, VscodeLaunchError> {
    let id = target.id.as_str();
    match answer {
        Ok(launch) => match connection_from_node(id, OseTarget::Vscode, launch) {
            Ok(conn) => Ok(launch_of(rec, target, server_url, Some(conn))),
            Err(m) if m == NODE_TOO_OLD_FOR_VSCODE => {
                Err(VscodeLaunchError::new(StatusCode::BAD_GATEWAY, NODE_TOO_OLD_FOR_VSCODE_CODE, m))
            }
            Err(m) => Err(VscodeLaunchError::new(StatusCode::BAD_GATEWAY, NODE_ERROR, m)),
        },
        // A 403 for a path outside the node's roots is the node's answer, not a refused server.
        Err(ClientError::Api { status, body })
            if node_refused_server(status) && body.code != ErrorCode::ForbiddenPath =>
        {
            Err(VscodeLaunchError::new(StatusCode::BAD_GATEWAY, NODE_AUTH_FAILED, node_auth_failed_message(id)))
        }
        Err(ClientError::Api { status, body }) if status == StatusCode::SERVICE_UNAVAILABLE.as_u16() => {
            Err(VscodeLaunchError::new(StatusCode::SERVICE_UNAVAILABLE, OSE_NOT_INSTALLED, body.error))
        }
        Err(ClientError::Api { body, .. }) => Err(VscodeLaunchError::new(StatusCode::BAD_GATEWAY, NODE_ERROR, body.error)),
        Err(e) => Err(VscodeLaunchError::new(StatusCode::BAD_GATEWAY, NODE_UNREACHABLE, format!("node unreachable: {e}"))),
    }
}

#[derive(Clone)]
struct IdeState {
    sessions: Arc<Sessions>,
    config: Arc<IdeConfig>,
    ose: Arc<OseManager>,
    computers: Option<Arc<Computers>>,
    /// Env names already warned about (one warning per name).
    warned: Arc<Mutex<HashSet<String>>>,
}

/// The routes without a registry: the computers are the `EMBER_IDE_COMPUTERS` list alone.
pub fn router(sessions: Arc<Sessions>, config: Arc<IdeConfig>) -> Router {
    build(sessions, config, None, Arc::new(OseManager::default()))
}

/// Like [`router`] with the manager shared with the extension route (`ide_extensions`).
pub fn router_with_ose(sessions: Arc<Sessions>, config: Arc<IdeConfig>, ose: Arc<OseManager>) -> Router {
    build(sessions, config, None, ose)
}

/// The routes with the computers registry (`FR-L7`, `FR-W4`).
pub fn router_with_computers(
    sessions: Arc<Sessions>,
    config: Arc<IdeConfig>,
    computers: Arc<Computers>,
) -> Router {
    build(sessions, config, Some(computers), Arc::new(OseManager::default()))
}

/// The registry routes with the manager shared with the extension route (`ide_extensions`).
pub fn router_with_computers_and_ose(
    sessions: Arc<Sessions>,
    config: Arc<IdeConfig>,
    computers: Arc<Computers>,
    ose: Arc<OseManager>,
) -> Router {
    build(sessions, config, Some(computers), ose)
}

fn build(
    sessions: Arc<Sessions>,
    config: Arc<IdeConfig>,
    computers: Option<Arc<Computers>>,
    ose: Arc<OseManager>,
) -> Router {
    Router::new()
        .route("/api/sessions/{id}/ide", get(ide_targets))
        .route("/api/sessions/{id}/ide/vscode", post(ide_vscode))
        .route("/api/computers/{computer}/ide/relay/{launch_id}", get(ide_relay))
        .with_state(IdeState {
            sessions,
            config,
            ose,
            computers,
            warned: Arc::new(Mutex::new(HashSet::new())),
        })
}

async fn ide_relay(
    State(st): State<IdeState>,
    Path((computer, launch_id)): Path<(String, String)>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if let Err(code) = crate::api::origin::check_origin(&headers) {
        return error(code, "origin not allowed".into());
    }
    if !valid_computer_id(&computer) {
        return error(StatusCode::BAD_REQUEST, "invalid computer id".into());
    }
    if computer != LOCAL {
        let Some(c) = &st.computers else {
            return error(StatusCode::NOT_FOUND, format!("unknown computer: {computer}"));
        };
        // The registry answer is not kept across the await below.
        let registered = match c.registry().get(&computer) {
            Ok(found) => found.is_some(),
            Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
        };
        if !registered {
            return error(StatusCode::NOT_FOUND, format!("unknown computer: {computer}"));
        }
        // FR-W4c: the OSE of a registered computer runs on its node.
        return node_relay(c, &computer, &launch_id, ws).await;
    }
    // The lookup takes the manager's lock, which a starting OSE holds for up to 10 s: wait for it
    // off the async workers.
    let (ose, id) = (st.ose.clone(), launch_id.clone());
    let found = match tokio::task::spawn_blocking(move || ose.open_relay(LOCAL, &id)).await {
        Ok(found) => found,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("relay lookup task failed: {e}")),
    };
    let target = match found {
        Ok(t) => t,
        Err(RelayLookupError::NotFound) => {
            return error(StatusCode::NOT_FOUND, format!("unknown OSE launch: {launch_id}"))
        }
        Err(RelayLookupError::TooManyRelays) => {
            return error(StatusCode::TOO_MANY_REQUESTS, "too many active IDE relays".into())
        }
    };
    ws.on_upgrade(move |socket| pump_ose_relay(socket, target)).into_response()
}

/// True for an id this server may put into a node path: the manager's 32 hex characters, and in
/// any case nothing but ASCII letters and digits.
fn safe_launch_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// The message of a VS Code target on a node that answered an Ember launch: a node older than
/// D55 ignores `target` (#1064).
pub const NODE_TOO_OLD_FOR_VSCODE: &str =
    "the node of this computer is too old for the VS Code target: update ember-node on that computer";

/// The connection of a node's launch for `target`, with this server's relay path for computer
/// `id`. A VS Code launch must carry no token and no endpoint (an old node that ignored the target
/// answers both); an Ember launch must carry its placeholder. Pure, for tests.
pub fn connection_from_node(
    id: &str,
    target: OseTarget,
    launch: ember_node::ose_api::OseLaunchResponse,
) -> Result<EmberOseConnection, String> {
    if !safe_launch_id(&launch.launch_id) {
        return Err("the node answered an invalid OSE launch id".into());
    }
    match target {
        OseTarget::Vscode if !launch.connection_token.is_empty() || !launch.endpoint.is_empty() => {
            return Err(NODE_TOO_OLD_FOR_VSCODE.into())
        }
        OseTarget::Ember if launch.connection_token.is_empty() => {
            return Err("the node answered an OSE launch without a connection token".into())
        }
        _ => {}
    }
    Ok(EmberOseConnection {
        endpoint: launch.endpoint,
        relay: Some(format!("/api/computers/{id}/ide/relay/{}", launch.launch_id)),
        connection_token: launch.connection_token,
        ose_commit: launch.ose_commit,
    })
}

/// The OSE of `folder` for `target` on the node of the registered computer `id` (SPEC FR-W4c):
/// the node starts or reuses it, and the answer is turned into this server's relay path. The
/// Ember token in it is the node's placeholder (the real one stays on the node); the VS Code
/// launch has none (D55). An error is the reason the target is unavailable.
async fn node_ose_connection(
    computers: &Computers,
    id: &str,
    folder: &str,
    target: OseTarget,
) -> Result<EmberOseConnection, String> {
    let node = computers.client(id).map_err(|e| format!("{e:#}"))?.with_deadline(NODE_OSE_START_TIMEOUT);
    match node.ide_ose_for(folder, target).await {
        Ok(launch) => connection_from_node(id, target, launch),
        Err(ClientError::Api { body, .. }) => Err(body.error),
        Err(e) => Err(format!("node unreachable: {e}")),
    }
}

/// The relay of a registered computer (SPEC FR-W4c). The node's relay socket is opened first, so
/// its refusal (unknown launch 404, relay limit 429) and an unreachable node (502) are plain
/// answers; only then is the client upgraded and the two sockets bridged. A node that refuses
/// this server itself (401, 403) is [`node_auth_failed`], not its own status.
async fn node_relay(computers: &Computers, computer: &str, launch_id: &str, ws: WebSocketUpgrade) -> Response {
    if !safe_launch_id(launch_id) {
        return error(StatusCode::NOT_FOUND, format!("unknown OSE launch: {launch_id}"));
    }
    let node = match computers.client(computer) {
        Ok(n) => n,
        Err(e) => return error(StatusCode::BAD_GATEWAY, format!("node unreachable: {e:#}")),
    };
    let upstream = match tokio::time::timeout(NODE_RELAY_OPEN_TIMEOUT, node.ide_ose_relay(launch_id)).await {
        Ok(Ok(u)) => u,
        Ok(Err(ClientError::Api { status, .. })) if node_refused_server(status) => return node_auth_failed(computer),
        Ok(Err(ClientError::Api { status, body })) => {
            return error(StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY), body.error)
        }
        Ok(Err(e)) => return error(StatusCode::BAD_GATEWAY, format!("node unreachable: {e}")),
        Err(_) => return error(StatusCode::BAD_GATEWAY, "node unreachable: the relay did not open in time".into()),
    };
    ws.on_upgrade(move |socket| bridge_node_relay(socket, upstream)).into_response()
}

/// How a bridged relay ended.
enum BridgeEnd {
    /// The client sent a text frame: it is told 1003, as on the local relay.
    ClientText,
    /// The node closed, with its close code and reason when it sent one (1011: no OSE to dial).
    NodeClosed(Option<(u16, String)>),
    /// The client closed or was cut off (a revoked device, `FR-N3`), or a send failed.
    Other,
}

/// Bridges one client relay socket to the node's relay socket, binary frames both ways. The two
/// directions run under one `tokio::select!`: when either side ends, the other socket is closed
/// too, so a revoked device's stream (the transport closes it within one heartbeat, `FR-N3`) takes
/// the node's socket, and with it the node's TCP connection to the OSE, down with it. Nothing is
/// rewritten here: the node replaces the placeholder token.
///
/// Pings are carried across, because the node's idle clock counts every frame of its client: the
/// client's Ping and Pong go to the node, the node's Ping goes to the client. A client that keeps
/// alive with WebSocket pings alone therefore stays inside the idle timeout, as on the local
/// relay. The Pong that answers a Ping is still sent by each socket itself, so it is not
/// forwarded a second time from the node.
async fn bridge_node_relay(socket: WebSocket, upstream: OseRelayStream) {
    use tokio_tungstenite::tungstenite::Message as NodeMessage;

    let (mut client_tx, mut client_rx) = socket.split();
    let (mut node_tx, mut node_rx) = upstream.split();
    let end = {
        let up = async {
            loop {
                match client_rx.next().await {
                    Some(Ok(Message::Binary(bytes))) => {
                        if node_tx.send(NodeMessage::Binary(bytes)).await.is_err() {
                            return BridgeEnd::Other;
                        }
                    }
                    Some(Ok(Message::Text(_))) => return BridgeEnd::ClientText,
                    Some(Ok(Message::Ping(payload))) => {
                        if node_tx.send(NodeMessage::Ping(payload)).await.is_err() {
                            return BridgeEnd::Other;
                        }
                    }
                    Some(Ok(Message::Pong(payload))) => {
                        if node_tx.send(NodeMessage::Pong(payload)).await.is_err() {
                            return BridgeEnd::Other;
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return BridgeEnd::Other,
                }
            }
        };
        let down = async {
            loop {
                match node_rx.next().await {
                    Some(Ok(NodeMessage::Binary(bytes))) => {
                        if client_tx.send(Message::Binary(bytes)).await.is_err() {
                            return BridgeEnd::Other;
                        }
                    }
                    Some(Ok(NodeMessage::Close(frame))) => {
                        return BridgeEnd::NodeClosed(
                            frame.map(|f| (u16::from(f.code), f.reason.as_str().to_string())),
                        )
                    }
                    Some(Ok(NodeMessage::Ping(payload))) => {
                        if client_tx.send(Message::Ping(payload)).await.is_err() {
                            return BridgeEnd::Other;
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => return BridgeEnd::NodeClosed(None),
                }
            }
        };
        tokio::select! {
            end = up => end,
            end = down => end,
        }
    };
    let close = async {
        let frame = match end {
            BridgeEnd::ClientText => Some(CloseFrame {
                code: 1003,
                reason: "text frames are not allowed on the OSE binary relay".into(),
            }),
            BridgeEnd::NodeClosed(Some((code, reason))) => Some(CloseFrame { code, reason: reason.as_str().into() }),
            BridgeEnd::NodeClosed(None) | BridgeEnd::Other => None,
        };
        // Both at once: a node that hangs must not use up the budget before the client is told.
        let _ = tokio::join!(node_tx.close(), client_tx.send(Message::Close(frame)));
    };
    // A peer that is gone never answers a close: do not wait for it.
    let _ = tokio::time::timeout(NODE_RELAY_CLOSE_TIMEOUT, close).await;
}

#[derive(Deserialize)]
struct IdeQuery {
    computer: Option<String>,
}

fn error(code: StatusCode, msg: String) -> Response {
    (code, Json(json!({ "error": msg }))).into_response()
}

/// A 500 for an internal failure (the store, the registry), in the shape [`resolve`] answers.
fn internal(e: impl std::fmt::Display) -> (StatusCode, &'static str, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, INTERNAL, format!("{e:#}"))
}

/// The session, computer and server URL a request on `/api/sessions/{id}/ide[...]` names.
struct Resolved {
    rec: SessionRecord,
    computer: IdeComputer,
    /// The computer is a registered node (not local): its OSE runs on the node (SPEC FR-W4c).
    on_node: bool,
    server_url: Option<String>,
}

/// Resolves the session `id` and the computer of `?computer=` (else the session's current
/// computer, else `default_computer`, else the local computer), the same way for the listing and
/// for the VS Code pick. An error is the status, the `code` (the pick's body) and the message.
fn resolve(
    st: &IdeState,
    id: &str,
    q: Option<String>,
    headers: &HeaderMap,
) -> Result<Resolved, (StatusCode, &'static str, String)> {
    let rec = match st.sessions.store().session(id) {
        Ok(Some(rec)) => rec,
        Ok(None) => return Err((StatusCode::NOT_FOUND, SESSION_NOT_FOUND, format!("session not found: {id}"))),
        Err(e) => return Err(internal(e)),
    };
    let registered = match &st.computers {
        Some(c) => Some(c.registry().list().map_err(internal)?),
        None => None,
    };
    let (all, unmatched) = resolve_computers(&st.config, registered.as_deref());
    for n in unmatched {
        if st.warned.lock().unwrap().insert(n.clone()) {
            tracing::warn!(
                name = %n,
                "EMBER_IDE_COMPUTERS entry matches no registered computer; using its name as the id"
            );
        }
    }
    let computer = match q {
        Some(name) => match find_computer(&all, &name) {
            Some(c) => c,
            None => return Err((StatusCode::NOT_FOUND, UNKNOWN_COMPUTER, format!("unknown computer: {name}"))),
        },
        None => {
            let current = st.computers.as_ref().and_then(|c| c.current(&st.sessions, id).ok());
            let from_session = current
                .filter(|cur| !cur.implicit)
                .and_then(|cur| all.iter().find(|c| c.id == cur.computer.id));
            match from_session
                .or_else(|| find_computer(&all, &st.config.default_computer))
                .or_else(|| all.iter().find(|c| c.id == LOCAL))
            {
                Some(c) => c,
                None => {
                    return Err((
                        StatusCode::NOT_FOUND,
                        UNKNOWN_COMPUTER,
                        format!("unknown computer: {}", st.config.default_computer),
                    ))
                }
            }
        }
    };
    let server_url = st.config.server_url.clone().or_else(|| {
        headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .map(|h| format!("http://{h}"))
    });
    // FR-W4c: a registered computer's OSE runs on its node. A computer marked local in
    // `EMBER_IDE_COMPUTERS`, or known only from it, keeps the answer of this server's own manager.
    let on_node = !computer.ide.local
        && computer.id != LOCAL
        && registered.as_deref().is_some_and(|l| l.iter().any(|c| c.id == computer.id));
    Ok(Resolved { rec, computer: computer.clone(), on_node, server_url })
}

async fn ide_targets(
    State(st): State<IdeState>,
    Path(id): Path<String>,
    Query(q): Query<IdeQuery>,
    headers: HeaderMap,
) -> Response {
    let Resolved { rec, computer, on_node, server_url } = match resolve(&st, &id, q.computer, &headers) {
        Ok(r) => r,
        Err((status, _, msg)) => return error(status, msg),
    };
    // Only the Ember target's OSE is started here; the VS Code target starts on pick (FR-W4c (1b)).
    let launch = match &st.computers {
        Some(computers) if on_node => {
            let connection = node_ose_connection(computers, &computer.id, &rec.cwd, OseTarget::Ember).await;
            launch_targets_with(&rec, &computer, server_url.as_deref(), connection)
        }
        _ => {
            // A new OSE takes up to 10 s to listen: keep the wait off the async workers.
            let ose = st.ose.clone();
            let started = tokio::task::spawn_blocking(move || {
                launch_targets_for(&rec, &computer, server_url.as_deref(), &ose)
            })
            .await;
            match started {
                Ok(launch) => launch,
                Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("OSE launch task failed: {e}")),
            }
        }
    };
    Json(launch).into_response()
}

/// `POST /api/sessions/{id}/ide/vscode[?computer=<name>]` (SPEC FR-W4c (1c), D55): starts or
/// reuses the folder's VS Code OSE when the user picks VS Code, and answers its launch
/// `{session, project, computer, computer_name?, folder, server_url, ose: {relay, ose_commit?}}`,
/// or `{code, error}`.
async fn ide_vscode(
    State(st): State<IdeState>,
    Path(id): Path<String>,
    Query(q): Query<IdeQuery>,
    headers: HeaderMap,
) -> Response {
    let Resolved { rec, computer, on_node, server_url } = match resolve(&st, &id, q.computer, &headers) {
        Ok(r) => r,
        Err((status, code, msg)) => return VscodeLaunchError::new(status, code, msg).into_response(),
    };
    let launch = match &st.computers {
        Some(computers) if on_node => {
            let answer = match computers.client(&computer.id) {
                Ok(node) => node.with_deadline(NODE_OSE_START_TIMEOUT).ide_ose_for(&rec.cwd, OseTarget::Vscode).await,
                Err(e) => Err(ClientError::Transport(format!("{e:#}"))),
            };
            node_vscode_launch(&rec, &computer, server_url.as_deref(), answer)
        }
        _ if computer.ide.local => {
            // A new OSE takes up to 10 s to listen: keep the wait off the async workers.
            let ose = st.ose.clone();
            let started = tokio::task::spawn_blocking(move || {
                local_vscode_launch(&rec, &computer, server_url.as_deref(), &ose)
            })
            .await;
            match started {
                Ok(launch) => launch,
                Err(e) => Err(VscodeLaunchError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    INTERNAL,
                    format!("OSE launch task failed: {e}"),
                )),
            }
        }
        // Known only from `EMBER_IDE_COMPUTERS` and not local: no OSE to relay to.
        _ => Err(VscodeLaunchError::new(StatusCode::CONFLICT, VSCODE_UNAVAILABLE, LOCAL_ONLY)),
    };
    match launch {
        Ok(launch) => Json(launch).into_response(),
        Err(e) => e.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_l7_fr_l31_fr_w4_serializes_editor_connection() {
        let launch = EmberLaunch {
            session: "s".into(), project: "p".into(), computer: "local".into(), computer_name: None,
            folder: "/w".into(), server_url: None,
            ose: Some(EmberOseConnection {
                endpoint: "http://localhost:9000".into(),
                relay: Some("/api/computers/local/ide/relay/abc".into()),
                connection_token: "secret".into(),
                ose_commit: None,
            }),
        };
        let value = serde_json::to_value(launch).unwrap();
        assert_eq!(value["ose"]["endpoint"], "http://localhost:9000");
        assert_eq!(value["ose"]["relay"], "/api/computers/local/ide/relay/abc");
        assert_eq!(value["ose"]["connection_token"], "secret");
    }

    fn record(folder: &std::path::Path) -> SessionRecord {
        SessionRecord {
            id: "s".into(), project: "p".into(), agent: crate::agents::AgentKind::Scripted,
            cwd: folder.to_string_lossy().into_owned(), model: None, native_id: None,
            status: crate::events::SessionStatus::Idle, title: String::new(), created_at: 0,
            updated_at: 0, last_seq: 0, account_id: None, account_reason: None,
            pinned: false, archived: false,
        }
    }

    #[test]
    fn fr_l7_fr_w4_missing_server_omits_connection() {
        let dir = tempfile::tempdir().unwrap();
        let manager = OseManager::new(None);
        let targets = launch_targets(&record(dir.path()), "local", &ComputerIde { local: true, ..Default::default() }, None, &manager);
        let ember = &targets.targets[0];
        assert_eq!(ember.available, Some(false));
        assert!(ember.reason.as_ref().unwrap().contains("not installed"));
        assert!(serde_json::to_value(ember).unwrap()["launch"].get("ose").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn fr_l7_fr_w4_launch_starts_and_reuses_server() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("server");
        crate::test_exec::write_exec_ready(&script, r##"#!/bin/sh
exec python3 -c 'import socket,sys; p=int(sys.argv[sys.argv.index("--port")+1]); s=socket.socket(); s.bind(("127.0.0.1",p)); s.listen();
while True: s.accept()[0].close()' "$@"
"##);
        let manager = OseManager::with_data_root(Some(script), dir.path().join("ose-data"));
        let computer = ComputerIde { local: true, ..Default::default() };
        let first = launch_targets(&record(dir.path()), "local", &computer, None, &manager);
        let second = launch_targets(&record(dir.path()), "local", &computer, None, &manager);
        assert_eq!(first.targets[0].available, Some(true));
        let connection = first.targets[0].launch.as_ref().unwrap().ose.as_ref().unwrap();
        assert_eq!(Some(connection), second.targets[0].launch.as_ref().unwrap().ose.as_ref());
        assert!(!connection.connection_token.is_empty());
        let value = serde_json::to_value(&first).unwrap();
        assert_eq!(value["targets"][0]["launch"]["ose"]["connection_token"], connection.connection_token);
        assert!(!format!("{first:?}").contains(&connection.connection_token));
    }

    /// FR-W4c (#896): a node that refuses this server is a 502 with its own code, and no 401.
    #[tokio::test]
    async fn fr_w4c_node_auth_failed_is_a_502_with_a_code_and_no_token() {
        use http_body_util::BodyExt;

        assert!(node_refused_server(401) && node_refused_server(403));
        for passed in [200, 400, 404, 409, 429, 500, 502, 503] {
            assert!(!node_refused_server(passed), "{passed}");
        }
        let res = node_auth_failed("0d0c7a52-box");
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["code"], NODE_AUTH_FAILED);
        let message = v["error"].as_str().unwrap();
        assert!(message.contains("could not authenticate to computer 0d0c7a52-box"), "{message}");
        assert!(message.is_ascii(), "{message}");
    }

    #[test]
    fn percent_encoding() {
        assert_eq!(percent_encode("/a b/ü", true), "/a%20b/%C3%BC");
        assert_eq!(percent_encode("/a b", false), "%2Fa%20b");
        assert_eq!(percent_encode("a&b=c#d", true), "a%26b%3Dc%23d");
    }

    /// FR-W4c (1b) (D55, #1064): the listing's VS Code target is `{kind: "vscode"}` alone, on
    /// every computer, whatever the Ember target's state.
    #[test]
    fn fr_w4c_listed_vscode_target_is_the_kind_alone() {
        let dir = tempfile::tempdir().unwrap();
        let computer = IdeComputer {
            id: "c1".into(), name: "box".into(), ide: ComputerIde::default(), aliases: vec![], named: true,
        };
        let ember = EmberOseConnection {
            endpoint: "http://127.0.0.1:9000".into(),
            relay: Some("/api/computers/c1/ide/relay/e".into()),
            connection_token: "p".repeat(64),
            ose_commit: None,
        };
        for connection in [Ok(ember), Err("node unreachable: y".to_string())] {
            let launch = launch_targets_with(&record(dir.path()), &computer, Some("http://s"), connection);
            let v = serde_json::to_value(&launch).unwrap();
            assert_eq!(v["targets"][1], serde_json::json!({ "kind": "vscode" }));
            assert!(v["targets"][0]["available"].is_boolean(), "the Ember target keeps `available`");
            assert!(v["targets"][2]["available"].is_boolean(), "the Gateway target keeps `available`");
        }
        // A computer known only from `EMBER_IDE_COMPUTERS` and not local lists it too.
        let env_only = launch_targets(&record(dir.path()), "lab", &ComputerIde::default(), None, &OseManager::new(None));
        assert_eq!(serde_json::to_value(&env_only.targets[1]).unwrap(), serde_json::json!({ "kind": "vscode" }));
    }

    /// FR-W4c (1c) (D55, #1064): a node's answer to the pick becomes the launch with the relay
    /// alone, or `{code, error}` with the status the SPEC names.
    #[test]
    fn fr_w4c_node_vscode_pick_answers_the_launch_or_a_coded_error() {
        use ember_node::ose_api::OseLaunchResponse;
        use ember_node::proto::ErrorBody;
        let dir = tempfile::tempdir().unwrap();
        let rec = record(dir.path());
        let computer = IdeComputer {
            id: "c1".into(), name: "box".into(), ide: ComputerIde::default(), aliases: vec![], named: true,
        };
        let id = "0123456789abcdef0123456789abcdef".to_string();
        let socket = OseLaunchResponse {
            launch_id: id.clone(), endpoint: String::new(), connection_token: String::new(), ose_commit: Some("abc".into()),
        };
        let launch = node_vscode_launch(&rec, &computer, Some("http://s"), Ok(socket)).unwrap();
        let folder = dir.path().to_string_lossy().into_owned();
        assert_eq!(
            serde_json::to_value(&launch).unwrap(),
            serde_json::json!({
                "session": "s", "project": "p", "computer": "c1", "computer_name": "box",
                "folder": folder, "server_url": "http://s",
                "ose": { "relay": format!("/api/computers/c1/ide/relay/{id}"), "ose_commit": "abc" }
            })
        );

        let api = |status: u16, code: ErrorCode, error: &str| ClientError::Api {
            status,
            body: ErrorBody { code, error: error.into(), actual_sha256: None, errno: None },
        };
        let tcp = OseLaunchResponse {
            launch_id: id, endpoint: "http://127.0.0.1:1".into(), connection_token: "p".repeat(64), ose_commit: None,
        };
        let cases = [
            (Ok(tcp), StatusCode::BAD_GATEWAY, NODE_TOO_OLD_FOR_VSCODE_CODE),
            (Err(api(503, ErrorCode::Internal, NOT_INSTALLED)), StatusCode::SERVICE_UNAVAILABLE, OSE_NOT_INSTALLED),
            (Err(ClientError::Transport("refused".into())), StatusCode::BAD_GATEWAY, NODE_UNREACHABLE),
            (Err(ClientError::Timeout(NODE_OSE_START_TIMEOUT)), StatusCode::BAD_GATEWAY, NODE_UNREACHABLE),
            (Err(api(401, ErrorCode::Unauthorized, "bad token")), StatusCode::BAD_GATEWAY, NODE_AUTH_FAILED),
            (Err(api(403, ErrorCode::ForbiddenPath, "outside the allowed roots")), StatusCode::BAD_GATEWAY, NODE_ERROR),
        ];
        for (answer, status, code) in cases {
            let e = node_vscode_launch(&rec, &computer, None, answer).unwrap_err();
            assert_eq!((e.status, e.code), (status, code), "{e:?}");
        }
        let old = node_vscode_launch(&rec, &computer, None, Ok(OseLaunchResponse {
            launch_id: "0123456789abcdef0123456789abcdef".into(), endpoint: String::new(),
            connection_token: "p".repeat(64), ose_commit: None,
        }))
        .unwrap_err();
        assert_eq!(old.error, NODE_TOO_OLD_FOR_VSCODE);
        let forbidden = node_vscode_launch(&rec, &computer, None, Err(api(403, ErrorCode::ForbiddenPath, "outside the allowed roots"))).unwrap_err();
        assert_eq!(forbidden.error, "outside the allowed roots");
    }

    /// FR-W4c (1b) (D55, #1064): a node's VS Code launch with a token or an endpoint is a node that
    /// ignored the target (older than D55), and an Ember launch needs its placeholder.
    #[test]
    fn fr_w4c_node_launch_is_checked_per_target() {
        use ember_node::ose_api::OseLaunchResponse;
        let id = "0123456789abcdef0123456789abcdef".to_string();
        let socket = OseLaunchResponse { launch_id: id.clone(), endpoint: String::new(), connection_token: String::new(), ose_commit: None };
        let tcp = OseLaunchResponse {
            launch_id: id.clone(), endpoint: "http://127.0.0.1:1".into(), connection_token: "p".repeat(64), ose_commit: None,
        };
        let vs = connection_from_node("c1", OseTarget::Vscode, socket.clone()).unwrap();
        assert_eq!(serde_json::to_value(&vs).unwrap(), serde_json::json!({ "relay": format!("/api/computers/c1/ide/relay/{id}") }));
        assert_eq!(connection_from_node("c1", OseTarget::Vscode, tcp.clone()), Err(NODE_TOO_OLD_FOR_VSCODE.to_string()));
        assert!(connection_from_node("c1", OseTarget::Ember, socket).is_err());
        assert_eq!(connection_from_node("c1", OseTarget::Ember, tcp).unwrap().connection_token, "p".repeat(64));
        let bad = OseLaunchResponse { launch_id: "../x".into(), endpoint: String::new(), connection_token: String::new(), ose_commit: None };
        assert!(connection_from_node("c1", OseTarget::Vscode, bad).is_err());
    }

    /// FR-W4c (1b), (1c) (D55, #1064): listing the local computer's targets starts no VS Code
    /// OSE; the pick starts this server's own socket OSE of the folder, behind the local relay
    /// path, reuses it on a second pick, and the relay reaches its socket with no token to
    /// replace. A short `/tmp` dir keeps the socket path inside a socket address.
    #[cfg(unix)]
    #[test]
    fn fr_w4c_local_vscode_target_is_the_servers_socket_ose() {
        let dir = tempfile::Builder::new().prefix("ew").tempdir_in("/tmp").unwrap();
        let base = dir.path().canonicalize().unwrap();
        let script = base.join("server");
        let starts = base.join("starts");
        crate::test_exec::write_exec_ready(&script, &format!(r##"#!/bin/sh
echo "$@" >> '{}'
exec python3 -c '
import socket, sys
a = sys.argv
if "--socket-path" in a:
    s = socket.socket(socket.AF_UNIX)
    s.bind(a[a.index("--socket-path") + 1])
else:
    s = socket.socket()
    s.bind(("127.0.0.1", int(a[a.index("--port") + 1])))
s.listen()
while True:
    s.accept()[0].close()
' "$@"
"##, starts.display()));
        let socket_starts = || {
            std::fs::read_to_string(&starts).unwrap_or_default().lines().filter(|l| l.contains("--socket-path")).count()
        };
        let manager = OseManager::with_data_root(Some(script), base.join("d"));
        let computer = ComputerIde { local: true, ..Default::default() };
        let listed = launch_targets(&record(&base), "local", &computer, None, &manager);
        assert_eq!(serde_json::to_value(&listed.targets[1]).unwrap(), serde_json::json!({ "kind": "vscode" }));
        assert_eq!(listed.targets[0].available, Some(true), "{:?}", listed.targets[0].reason);
        assert_eq!(socket_starts(), 0, "listing started a VS Code OSE");

        let target = IdeComputer { id: "local".into(), name: "local".into(), ide: computer, aliases: vec![], named: false };
        let first = local_vscode_launch(&record(&base), &target, Some("http://s"), &manager).unwrap();
        assert_eq!(socket_starts(), 1);
        let conn = first.ose.as_ref().unwrap();
        assert!(conn.endpoint.is_empty() && conn.connection_token.is_empty());
        let relay = conn.relay.as_deref().unwrap();
        let launch_id = relay.strip_prefix("/api/computers/local/ide/relay/").expect(relay);
        assert_ne!(listed.targets[0].launch.as_ref().unwrap().ose.as_ref().unwrap().relay.as_deref(), Some(relay));
        let again = local_vscode_launch(&record(&base), &target, Some("http://s"), &manager).unwrap();
        assert_eq!(again.ose.as_ref(), Some(conn), "reused");
        assert_eq!(socket_starts(), 1, "a second pick started another OSE");
        let found = manager.open_relay(LOCAL, launch_id).ok().expect("the relay finds the launch");
        assert!(matches!(&found.address, crate::ose::OseAddress::Unix(p) if p.starts_with(base.join("d"))));
        assert!(found.real_token.is_empty() && found.placeholder.is_empty());

        // Without a binary the pick is a 503 with its code.
        let none = OseManager::with_data_root(None, base.join("d2"));
        let e = local_vscode_launch(&record(&base), &target, None, &none).unwrap_err();
        assert_eq!((e.status, e.code, e.error.as_str()), (StatusCode::SERVICE_UNAVAILABLE, OSE_NOT_INSTALLED, NOT_INSTALLED));
    }

    #[test]
    fn gateway_url_matches_documented_form() {
        let c = ComputerIde {
            ssh_host: Some("ec2.example.com".into()),
            ssh_user: Some("ubuntu".into()),
            jetbrains_ide_path: Some("/home/ubuntu/.cache/JetBrains/RemoteDev/dist/idea".into()),
            ..Default::default()
        };
        assert_eq!(
            gateway_url(&c, "/home/ubuntu/development/spring-petclinic").unwrap(),
            "jetbrains-gateway://connect#type=ssh&host=ec2.example.com&port=22&user=ubuntu\
             &projectPath=%2Fhome%2Fubuntu%2Fdevelopment%2Fspring-petclinic\
             &idePath=%2Fhome%2Fubuntu%2F.cache%2FJetBrains%2FRemoteDev%2Fdist%2Fidea&deploy=false"
        );
    }

    #[test]
    fn gateway_without_ide_path_or_user() {
        let c = ComputerIde { ssh_host: Some("h".into()), ssh_port: Some(2222), ..Default::default() };
        assert_eq!(
            gateway_url(&c, "/p").unwrap(),
            "jetbrains-gateway://connect#type=ssh&host=h&port=2222&projectPath=%2Fp"
        );
        assert!(gateway_url(&ComputerIde::default(), "/p").is_err());
    }

    #[test]
    fn open_commands_per_os() {
        assert_eq!(open_command(Some("macos"), "u"), Some(vec!["open".into(), "u".into()]));
        assert_eq!(open_command(Some("linux"), "u"), Some(vec!["xdg-open".into(), "u".into()]));
        assert_eq!(open_command(Some("windows"), "u").unwrap().len(), 5);
        assert_eq!(open_command(None, "u"), None);
        assert_eq!(open_command(Some("plan9"), "u"), None);
    }

    #[test]
    fn config_parse_inline_and_default_check() {
        let cfg = IdeConfig::parse(
            r#"{"default_computer":"mini","computers":{"mini":{"local":true,"ide_url":"http://x/"}}}"#,
        )
        .unwrap();
        assert!(cfg.computers["mini"].local);
        assert_eq!(cfg.computers["mini"].ide_url.as_deref(), Some("http://x/"));
        assert!(IdeConfig::parse(r#"{"default_computer":"nope","computers":{}}"#).is_err());
        assert!(IdeConfig::parse("{not json").is_err());
    }
}
