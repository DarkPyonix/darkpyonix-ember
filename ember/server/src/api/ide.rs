//! "Open IDE" launch targets for a session.
//!
//! `GET /api/sessions/{id}/ide[?computer=<name>]` returns, for the session's project and
//! current computer, how to open each IDE. The server only returns data; it never opens or
//! renders anything:
//!
//! | `kind`    | What it is                                    | What we return                    |
//! |-----------|-----------------------------------------------|-----------------------------------|
//! | `ember`   | Ember's own editor-core IDE (dioxus-compose, M8) | launch info only (no URL)       |
//! | `vscode`  | VS Code Web wrapped by `ember/proxy/` on that computer | the proxy URL with `?folder=`   |
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
use axum::routing::get;
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::computers::{Computer, Computers, LOCAL, LOCAL_NAME};
use crate::ose::{valid_computer_id, OseManager, RelayLookupError, RelayTarget};
use crate::session::Sessions;
use crate::store::SessionRecord;

/// Byte window at the start of the client-to-OSE stream in which the C2 placeholder is replaced
/// with the real OSE connection token (`FR-W4`, `#458`, `#461`).
pub const RELAY_SUBSTITUTION_WINDOW: usize = 8 * 1024;
/// Timeout for opening the loopback TCP connection to the local OSE process.
const RELAY_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Maximum chunk size read from the OSE TCP socket per WebSocket binary frame.
const RELAY_READ_CHUNK: usize = 32 * 1024;

/// One computer's IDE endpoints.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct ComputerIde {
    /// The computer the main server runs on.
    #[serde(default)]
    pub local: bool,
    /// The wrapped VS Code Web on that computer: the URL `python -m dpx.serve` announces.
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
///     "mini":   { "local": true, "ide_url": "http://127.0.0.1:8890/", "os": "macos" },
///     "studio": { "ide_url": "http://studio.local:8890/", "ssh_host": "studio.local",
///                 "ssh_user": "me", "os": "linux" } } }
/// ```
///
/// Unset: one local computer named `local`, whose `ide_url` is `EMBER_IDE_URL`.
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
            _ => {
                let mut c = IdeConfig::default();
                if let Some(local) = c.computers.get_mut("local") {
                    local.ide_url = std::env::var("EMBER_IDE_URL").ok().filter(|u| !u.is_empty());
                }
                c
            }
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

#[derive(Clone, Serialize, PartialEq)]
pub struct EmberOseConnection {
    pub endpoint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay: Option<String>,
    pub connection_token: String,
}

impl std::fmt::Debug for EmberOseConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmberOseConnection")
            .field("endpoint", &self.endpoint)
            .field("relay", &self.relay)
            .field("connection_token", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Target {
    /// `ember`, `vscode` or `gateway`.
    pub kind: &'static str,
    pub available: bool,
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
        Target { kind, available: false, url: None, command: None, launch: None, reason: Some(reason) }
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

/// The wrapped VS Code Web URL for `folder`: `<ide_url>/?folder=<folder>`.
pub fn vscode_web_url(ide_url: &str, folder: &str) -> String {
    format!("{}/?folder={}", ide_url.trim_end_matches('/'), percent_encode(folder, true))
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

/// The launch targets for one session on one resolved computer.
pub fn launch_targets_for(
    rec: &SessionRecord,
    target: &IdeComputer,
    server_url: Option<&str>,
    ose: &OseManager,
) -> IdeLaunch {
    let computer_name = target.id.as_str();
    let computer = &target.ide;
    let display = target.named.then(|| target.name.clone());
    let folder = rec.cwd.clone();

    let connection = if computer.local {
        ose.connection(std::path::Path::new(&folder))
    } else {
        Err("OSE server is only available on the local computer".into())
    };
    let ember = Target {
        kind: "ember",
        available: connection.is_ok(),
        url: None,
        command: None,
        launch: Some(EmberLaunch {
            session: rec.id.clone(),
            project: rec.project.clone(),
            computer: computer_name.to_string(),
            computer_name: display.clone(),
            folder: folder.clone(),
            server_url: server_url.map(String::from),
            ose: connection.as_ref().ok().cloned(),
        }),
        reason: connection.err(),
    };

    let vscode = match computer.ide_url.as_deref().filter(|u| !u.is_empty()) {
        Some(base) => Target {
            kind: "vscode",
            available: true,
            url: Some(vscode_web_url(base, &folder)),
            command: None,
            launch: None,
            reason: None,
        },
        None => Target::unavailable(
            "vscode",
            format!("no ide_url configured for computer {computer_name:?} (run `python -m dpx.serve` there)"),
        ),
    };

    let gateway = match gateway_url(computer, &folder) {
        Ok(url) => Target {
            kind: "gateway",
            available: true,
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
        .route("/api/computers/{computer}/ide/relay/{launch_id}", get(ide_relay))
        .with_state(IdeState {
            sessions,
            config,
            ose,
            computers,
            warned: Arc::new(Mutex::new(HashSet::new())),
        })
}

/// C2 placeholder-to-real-token substitution for the first `window` bytes of the
/// client-to-OSE byte stream (`FR-W4`, `#458`, `#461`).
///
/// Substitution works within one client WebSocket message: every placeholder that starts inside
/// the window and lies whole in the message is replaced. Each client message goes to the OSE as
/// exactly one write, never split, held or merged with another (#776). A placeholder split across
/// two messages is not replaced; Code-OSS and `ember-editor-conn` write each protocol message
/// (header and body, including the `auth` control message) as one WebSocket message, so the token
/// never straddles messages. Placeholder and real token have the same length, so byte offsets
/// do not shift.
struct TokenSubstituter {
    placeholder: Vec<u8>,
    real_token: Vec<u8>,
    window: usize,
    seen: usize,
}

impl TokenSubstituter {
    fn new(placeholder: &str, real_token: &str, window: usize) -> Self {
        Self {
            placeholder: placeholder.as_bytes().to_vec(),
            real_token: real_token.as_bytes().to_vec(),
            window,
            seen: 0,
        }
    }

    fn enabled(&self) -> bool {
        !self.placeholder.is_empty()
            && self.placeholder.len() == self.real_token.len()
            && self.placeholder != self.real_token
    }

    /// One client message in, the same message (same length) out.
    fn feed(&mut self, msg: &[u8]) -> Vec<u8> {
        let start = self.seen;
        self.seen = self.seen.saturating_add(msg.len());
        if !self.enabled() || start >= self.window {
            return msg.to_vec();
        }
        let in_win = (self.window - start).min(msg.len());
        let pat_len = self.placeholder.len();
        let mut out = msg.to_vec();
        let mut i = 0;
        while i < in_win && i + pat_len <= msg.len() {
            if msg[i..i + pat_len] == self.placeholder[..] {
                out[i..i + pat_len].copy_from_slice(&self.real_token);
                i += pat_len;
            } else {
                i += 1;
            }
        }
        out
    }
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
        if let Some(c) = &st.computers {
            match c.registry().get(&computer) {
                Ok(Some(_)) => {
                    return error(StatusCode::BAD_REQUEST, "OSE server is only available on the local computer".into())
                }
                Ok(None) => return error(StatusCode::NOT_FOUND, format!("unknown computer: {computer}")),
                Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
            }
        } else {
            return error(StatusCode::NOT_FOUND, format!("unknown computer: {computer}"));
        }
    }
    let target = match st.ose.open_relay(&computer, &launch_id) {
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

async fn pump_ose_relay(socket: WebSocket, target: RelayTarget) {
    let tcp = match tokio::time::timeout(RELAY_CONNECT_TIMEOUT, tokio::net::TcpStream::connect(target.address)).await {
        Ok(Ok(s)) => {
            let _ = s.set_nodelay(true);
            s
        }
        _ => {
            let mut socket = socket;
            let _ = socket
                .send(Message::Close(Some(CloseFrame {
                    code: 1011,
                    reason: "cannot connect to OSE server".into(),
                })))
                .await;
            return;
        }
    };
    let (mut tcp_read, mut tcp_write) = tcp.into_split();
    let (mut ws_out, mut ws_in) = socket.split();
    let mut sub = TokenSubstituter::new(&target.placeholder, &target.real_token, RELAY_SUBSTITUTION_WINDOW);
    let idle = target.idle_timeout;
    let last_activity_ns = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let started = std::time::Instant::now();
    let touch = {
        let last = last_activity_ns.clone();
        move || {
            last.store(
                started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                std::sync::atomic::Ordering::Relaxed,
            );
        }
    };

    // Bounded channel (capacity 4) for control frames (Pong / 1003 Close) from the reader side so
    // `ws_in` and `ws_out` never block each other across `tokio::select!`.
    let (ctrl_tx, mut ctrl_rx) = tokio::sync::mpsc::channel::<Message>(4);

    let touch_in = touch.clone();
    let c2s = async move {
        while let Some(msg) = ws_in.next().await {
            touch_in();
            match msg {
                Ok(Message::Binary(bytes)) => {
                    let out = sub.feed(&bytes);
                    if !out.is_empty() && tcp_write.write_all(&out).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Text(_)) => {
                    let _ = ctrl_tx
                        .send(Message::Close(Some(CloseFrame {
                            code: 1003,
                            reason: "text frames are not allowed on the OSE binary relay".into(),
                        })))
                        .await;
                    let _ = tcp_write.shutdown().await;
                    break;
                }
                Ok(Message::Ping(p)) => {
                    if ctrl_tx.send(Message::Pong(p)).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Pong(_)) => {}
                Ok(Message::Close(_)) | Err(_) => break,
            }
        }
        let _ = tcp_write.shutdown().await;
    };

    let s2c = async move {
        let mut read_buf = vec![0u8; RELAY_READ_CHUNK];
        loop {
            tokio::select! {
                biased;
                ctrl = ctrl_rx.recv(), if !ctrl_rx.is_closed() => {
                    if let Some(frame) = ctrl {
                        let is_close = matches!(frame, Message::Close(_));
                        if ws_out.send(frame).await.is_err() || is_close {
                            break;
                        }
                    }
                }
                n = tcp_read.read(&mut read_buf) => {
                    match n {
                        Ok(0) | Err(_) => {
                            if let Ok(frame) = ctrl_rx.try_recv() {
                                let _ = ws_out.send(frame).await;
                            } else {
                                let _ = ws_out.close().await;
                            }
                            break;
                        }
                        Ok(n) => {
                            touch();
                            if ws_out.send(Message::Binary(read_buf[..n].to_vec().into())).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        }
    };

    let idle_watchdog = async {
        let check_step = (idle / 4).clamp(Duration::from_millis(20), Duration::from_secs(5));
        loop {
            tokio::time::sleep(check_step).await;
            let last = Duration::from_nanos(last_activity_ns.load(std::sync::atomic::Ordering::Relaxed));
            if started.elapsed().saturating_sub(last) >= idle {
                break;
            }
        }
    };

    let both = async {
        tokio::join!(c2s, s2c);
    };
    tokio::select! {
        _ = both => {}
        _ = idle_watchdog => {}
    }
}

#[derive(Deserialize)]
struct IdeQuery {
    computer: Option<String>,
}

fn error(code: StatusCode, msg: String) -> Response {
    (code, Json(json!({ "error": msg }))).into_response()
}

async fn ide_targets(
    State(st): State<IdeState>,
    Path(id): Path<String>,
    Query(q): Query<IdeQuery>,
    headers: HeaderMap,
) -> Response {
    let rec = match st.sessions.store().session(&id) {
        Ok(Some(rec)) => rec,
        Ok(None) => return error(StatusCode::NOT_FOUND, format!("session not found: {id}")),
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    };
    let registered = match &st.computers {
        Some(c) => match c.registry().list() {
            Ok(l) => Some(l),
            Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
        },
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
    let computer = match q.computer {
        Some(name) => match find_computer(&all, &name) {
            Some(c) => c,
            None => return error(StatusCode::NOT_FOUND, format!("unknown computer: {name}")),
        },
        None => {
            let current = st.computers.as_ref().and_then(|c| c.current(&st.sessions, &id).ok());
            let from_session = current
                .filter(|cur| !cur.implicit)
                .and_then(|cur| all.iter().find(|c| c.id == cur.computer.id));
            match from_session
                .or_else(|| find_computer(&all, &st.config.default_computer))
                .or_else(|| all.iter().find(|c| c.id == LOCAL))
            {
                Some(c) => c,
                None => {
                    return error(
                        StatusCode::NOT_FOUND,
                        format!("unknown computer: {}", st.config.default_computer),
                    )
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
    Json(launch_targets_for(&rec, computer, server_url.as_deref(), &st.ose)).into_response()
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
        assert!(!ember.available);
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
        assert!(first.targets[0].available);
        let connection = first.targets[0].launch.as_ref().unwrap().ose.as_ref().unwrap();
        assert_eq!(Some(connection), second.targets[0].launch.as_ref().unwrap().ose.as_ref());
        assert!(!connection.connection_token.is_empty());
        let value = serde_json::to_value(&first).unwrap();
        assert_eq!(value["targets"][0]["launch"]["ose"]["connection_token"], connection.connection_token);
        assert!(!format!("{first:?}").contains(&connection.connection_token));
    }

    #[test]
    fn percent_encoding() {
        assert_eq!(percent_encode("/a b/ü", true), "/a%20b/%C3%BC");
        assert_eq!(percent_encode("/a b", false), "%2Fa%20b");
        assert_eq!(percent_encode("a&b=c#d", true), "a%26b%3Dc%23d");
    }

    #[test]
    fn vscode_url_joins_cleanly() {
        assert_eq!(
            vscode_web_url("http://127.0.0.1:8890/", "/Users/me/my app"),
            "http://127.0.0.1:8890/?folder=/Users/me/my%20app"
        );
        assert_eq!(vscode_web_url("http://h:1", "/x"), "http://h:1/?folder=/x");
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
