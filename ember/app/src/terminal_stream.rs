//! Workbench-facing terminal state and the attachment to the ember server's terminal relay.
//!
//! The relay lives on the server: `POST /api/computers/{computer}/terms` creates (or re-adopts,
//! by key) a persistent session on a computer's node, and
//! `GET /api/computers/{computer}/terms/{id}/attach` is a WebSocket that carries the node's
//! terminal events down and the client's input frames up. The wire types below mirror the
//! node's JSON shapes locally, so this crate does not depend on the node crate.

#[path = "terminal_screen.rs"]
mod screen;

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("websocket: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("protocol: {0}")]
    Protocol(String),
    /// A fixed Korean sentence for the panel (NFR-L4); the detail is logged, not shown.
    #[error("{0}")]
    Refused(&'static str),
}

// ---------------------------------------------------------------------------------------------
// Relay URLs

fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for b in segment.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Whether the relay at `base` may carry a terminal (FR-L31.19). `https` always may, and is
/// verified against the webpki roots. Plain `http` may only to a loopback host or to a host listed
/// with an explicit `http://` entry in `EMBER_TOKEN_HOSTS` (`extra_hosts` here), the same rule as
/// the token (FR-S11), because keystrokes and shell output would otherwise cross a network in
/// clear. Any other scheme is refused.
pub fn relay_allowed_with_hosts(base: &str, extra_hosts: &str) -> Result<(), String> {
    let Ok(url) = reqwest::Url::parse(base) else {
        return Err(format!("the server address is not a URL: {base}"));
    };
    match url.scheme() {
        "https" => Ok(()),
        "http" if ember_client::token::token_allowed_for_hosts(base, extra_hosts) => Ok(()),
        "http" => Err(format!(
            "refusing plain http to {}: use https, or list http://host:port in EMBER_TOKEN_HOSTS",
            url.host_str().unwrap_or("")
        )),
        other => Err(format!("unsupported server scheme {other}: use https or http")),
    }
}

/// [`relay_allowed_with_hosts`] with `EMBER_TOKEN_HOSTS` read from the environment.
pub fn relay_allowed(base: &str) -> Result<(), String> {
    relay_allowed_with_hosts(base, &std::env::var("EMBER_TOKEN_HOSTS").unwrap_or_default())
}

/// The fixed Korean sentence the panel shows when [`relay_allowed`] refuses (NFR-L4); the English
/// reason goes to the log only.
pub const RELAY_REFUSED: &str =
    "이 서버 주소로는 터미널을 열 수 없습니다. https 주소를 쓰거나 같은 컴퓨터의 서버에 연결하세요.";

pub(crate) fn check_relay(base: &str) -> Result<(), ClientError> {
    relay_allowed(base).map_err(|reason| {
        tracing::warn!("terminal relay refused: {reason}");
        ClientError::Refused(RELAY_REFUSED)
    })
}

/// Installs the ring provider as rustls' process default, once. reqwest and tokio-tungstenite
/// both build their rustls client config from that default; installing it explicitly keeps the
/// TLS stack working even if another dependency also enables a second rustls provider.
pub(crate) fn ensure_tls_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// `{base}/api/computers/{computer}/terms`, the create and list endpoint.
pub fn terms_url(base: &str, computer_id: &str) -> String {
    format!("{}/api/computers/{}/terms", base.trim_end_matches('/'), encode_segment(computer_id))
}

/// `ws(s)://.../api/computers/{computer}/terms/{term}/attach`, the attach WebSocket.
pub fn attach_url(base: &str, computer_id: &str, term_id: &str) -> String {
    let http = terms_url(base, computer_id);
    let ws = if let Some(rest) = http.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = http.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        http
    };
    format!("{ws}/{}/attach", encode_segment(term_id))
}

/// `GET {base}/api/computers/{computer}/terms?project=&origin=&running=`: the terminals of a project.
pub fn terms_list_url(base: &str, computer_id: &str, project: &str, origin: Option<&str>, running: bool) -> String {
    let mut url = format!("{}?project={}", terms_url(base, computer_id), encode_segment(project));
    if let Some(origin) = origin {
        url.push_str(&format!("&origin={}", encode_segment(origin)));
    }
    if running {
        url.push_str("&running=true");
    }
    url
}

/// Idempotency key of the editor's terminal for a project folder.
pub fn session_key(project: &str, folder: &str) -> String {
    format!("{project}:{folder}")
}

// ---------------------------------------------------------------------------------------------
// Wire types (mirror of the node's JSON; unknown fields are ignored)

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PtySize {
    pub rows: u16,
    pub cols: u16,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TermOrigin {
    IdeEmber,
}

/// What a new terminal runs (the node's `Program`): `None` in the request is the login shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TermProgram {
    /// Run this argv directly (no shell).
    Argv(Vec<String>),
    /// Run through `/bin/sh -c` on the node.
    Shell(String),
}

/// A terminal to start for a task (FR-L31.16): always a new session (no key).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermLaunch {
    pub program: TermProgram,
    pub cwd: String,
    pub env: std::collections::BTreeMap<String, String>,
    pub title: String,
}

#[derive(Serialize)]
pub struct TermCreateRequest {
    pub program: Option<TermProgram>,
    pub cwd: std::path::PathBuf,
    pub env: std::collections::BTreeMap<String, String>,
    pub env_clear: bool,
    pub size: Option<PtySize>,
    pub origin: TermOrigin,
    pub project: Option<String>,
    pub title: Option<String>,
    pub key: Option<String>,
    pub tags: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct TermCreateResponse {
    #[serde(default)]
    pub created: bool,
    pub term: TermInfo,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TermInfo {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub size: PtySize,
    /// What started it (`ide-ember`, `agent`, ...), as the list endpoint reports it.
    #[serde(default)]
    pub origin: String,
    #[serde(default)]
    pub project: Option<String>,
    /// `running` while the process is alive.
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub last_activity_ms: u64,
    /// The idempotency key it was created with (`"<project>:<folder>"` and its `#n` siblings).
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub cwd: std::path::PathBuf,
    #[serde(default)]
    pub created_ms: u64,
}

/// One attached client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermClient {
    pub client: u64,
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TermRefusal {
    Controlled,
    ReadOnly,
    NotRunning,
}

/// First frame the client sends on the attach socket.
#[derive(Debug, Clone, Serialize)]
pub struct TermHello {
    pub device: String,
    pub kind: Option<String>,
    pub pid: Option<u32>,
    pub size: Option<PtySize>,
    pub active: bool,
    pub read_only: bool,
    pub snapshot: bool,
}

impl TermHello {
    /// The first frame of a viewer that only watches. The node enforces `read_only` (it refuses this
    /// client's input and never gives it control); `active: false` keeps its window from resizing the
    /// terminal the others share. The server does not force this: the owner's own panel declares it.
    pub fn viewer() -> Self {
        Self {
            device: device_name(),
            kind: Some("ember-app".into()),
            pid: Some(std::process::id()),
            size: None,
            active: false,
            read_only: true,
            snapshot: true,
        }
    }
}

/// Which of a project's terminals a member's pane shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermPick {
    One(String),
    /// The project has no running agent terminal.
    None,
    /// More than one runs and nothing in the list says which is the member's.
    Ambiguous,
}

/// The terminal of a member's agent: the one running terminal of the project that an agent started.
/// The node's list carries no link from a terminal to a member, so with two or more it does not guess.
pub fn pick_agent_terminal(list: &[TermInfo]) -> TermPick {
    let mut running = list.iter().filter(|t| t.origin == "agent" && t.state == "running");
    match (running.next(), running.next()) {
        (None, _) => TermPick::None,
        (Some(t), None) => TermPick::One(t.id.clone()),
        (Some(_), Some(_)) => TermPick::Ambiguous,
    }
}

/// Frames the node (through the relay) sends down the attach socket.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum TermEvent {
    /// The relay's and the node's `attached` frames both land here; each field may be absent.
    Attached {
        #[serde(default)]
        client: Option<u64>,
        #[serde(default)]
        term: Option<TermInfoLite>,
    },
    Snapshot { size: PtySize, #[serde(with = "b64")] data: Vec<u8> },
    Output { #[serde(with = "b64")] data: Vec<u8> },
    Resized { size: PtySize },
    Control { controller: Option<TermClient> },
    Refused { reason: TermRefusal, #[serde(default)] controller: Option<TermClient> },
    Clients { clients: Vec<TermClient> },
    Title { title: Option<String> },
    Exit { #[serde(default)] code: Option<i32>, #[serde(default)] signal: Option<i32> },
    Error { message: String },
    #[serde(other)]
    Other,
}

/// The part of the session description the screen needs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct TermInfoLite {
    #[serde(default)]
    pub size: PtySize,
}

mod b64 {
    use base64::Engine;
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD.decode(text).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------------------------
// Outgoing frames (TermInput)

/// Keystrokes or paste, base64 on the wire.
pub fn input_frame(data: &[u8]) -> String {
    serde_json::json!({ "type": "input", "data": base64::engine::general_purpose::STANDARD.encode(data) }).to_string()
}

/// This client's viewport size.
pub fn resize_frame(cols: u16, rows: u16) -> String {
    serde_json::json!({ "type": "resize", "size": { "rows": rows, "cols": cols } }).to_string()
}

pub fn take_control_frame() -> String {
    serde_json::json!({ "type": "take_control" }).to_string()
}

pub fn release_control_frame() -> String {
    serde_json::json!({ "type": "release_control" }).to_string()
}

/// Detach without ending the session.
pub fn detach_frame() -> String {
    serde_json::json!({ "type": "detach" }).to_string()
}

/// Modifier keys held with a key press.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Modifiers {
    pub const NONE: Modifiers = Modifiers { ctrl: false, alt: false, shift: false };
    pub const CTRL: Modifiers = Modifiers { ctrl: true, alt: false, shift: false };
    pub const ALT: Modifiers = Modifiers { ctrl: false, alt: true, shift: false };
    pub const SHIFT: Modifiers = Modifiers { ctrl: false, alt: false, shift: true };

    /// The xterm modifier parameter (1 means none).
    fn param(self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl)
    }
}

fn csi(final_byte: char, mods: Modifiers) -> Vec<u8> {
    match mods.param() {
        1 => format!("\x1b[{final_byte}").into_bytes(),
        m => format!("\x1b[1;{m}{final_byte}").into_bytes(),
    }
}

fn csi_tilde(code: u8, mods: Modifiers) -> Vec<u8> {
    match mods.param() {
        1 => format!("\x1b[{code}~").into_bytes(),
        m => format!("\x1b[{code};{m}~").into_bytes(),
    }
}

/// Bytes a terminal expects for a key press. `key` is a key name (`Enter`, `Backspace`, `Tab`,
/// `Escape`, `ArrowUp`, `Home`, `PageDown`, `F5`, ...) or a single character. Returns `None`
/// for keys with no terminal encoding (a bare modifier, an unmapped Ctrl combination).
/// Whole strings (paste, IME commits) go through [`text_bytes`].
pub fn key_bytes(key: &str, mods: Modifiers) -> Option<Vec<u8>> {
    let named = match key {
        "Enter" => Some(b"\r".to_vec()),
        "Backspace" => Some(if mods.ctrl { vec![0x08] } else { vec![0x7f] }),
        "Tab" => Some(if mods.shift { b"\x1b[Z".to_vec() } else { b"\t".to_vec() }),
        "Escape" | "Esc" => Some(vec![0x1b]),
        "ArrowUp" | "Up" => Some(csi('A', mods)),
        "ArrowDown" | "Down" => Some(csi('B', mods)),
        "ArrowRight" | "Right" => Some(csi('C', mods)),
        "ArrowLeft" | "Left" => Some(csi('D', mods)),
        "Home" => Some(csi('H', mods)),
        "End" => Some(csi('F', mods)),
        "Insert" => Some(csi_tilde(2, mods)),
        "Delete" => Some(csi_tilde(3, mods)),
        "PageUp" => Some(csi_tilde(5, mods)),
        "PageDown" => Some(csi_tilde(6, mods)),
        "F1" | "F2" | "F3" | "F4" => {
            let last = ['P', 'Q', 'R', 'S'][(key.as_bytes()[1] - b'1') as usize];
            Some(if mods.param() == 1 { format!("\x1bO{last}").into_bytes() } else { csi(last, mods) })
        }
        "F5" => Some(csi_tilde(15, mods)),
        "F6" => Some(csi_tilde(17, mods)),
        "F7" => Some(csi_tilde(18, mods)),
        "F8" => Some(csi_tilde(19, mods)),
        "F9" => Some(csi_tilde(20, mods)),
        "F10" => Some(csi_tilde(21, mods)),
        "F11" => Some(csi_tilde(23, mods)),
        "F12" => Some(csi_tilde(24, mods)),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    let mut chars = key.chars();
    let c = chars.next()?;
    if chars.next().is_some() || c.is_control() {
        return None;
    }
    let mut out = Vec::new();
    if mods.alt {
        out.push(0x1b);
    }
    if mods.ctrl {
        let byte = match c {
            'a'..='z' | 'A'..='Z' => c.to_ascii_lowercase() as u8 - 0x60,
            '@' | ' ' => 0,
            '[' => 27,
            '\\' => 28,
            ']' => 29,
            '^' => 30,
            '_' | '/' => 31,
            '?' => 127,
            _ => return None,
        };
        out.push(byte);
    } else {
        let mut buf = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
    Some(out)
}

/// Bytes for typed or pasted text. Newlines become the carriage return a terminal expects.
pub fn text_bytes(text: &str) -> Vec<u8> {
    text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
}

// ---------------------------------------------------------------------------------------------
// Session state

/// Connection state shown by the terminal panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TerminalStatus {
    #[default]
    Disconnected,
    Connecting,
    Connected,
}

/// What the panel shows: a decoded screen plus who controls the session and how it ended.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalState {
    pub status: TerminalStatus,
    pub lines: Vec<String>,
    /// Cursor as (row, column) on the visible screen.
    pub cursor: (usize, usize),
    /// PTY size as (cols, rows), once known.
    pub size: (u16, u16),
    /// True while the program enabled bracketed paste (`ESC[?2004h`), so a paste is wrapped
    /// in `ESC[200~` and `ESC[201~` (FR-L31.21).
    pub bracketed_paste: bool,
    /// This client's id on the session, once attached.
    pub client: Option<u64>,
    /// The client that holds control, if any.
    pub controller: Option<TermClient>,
    pub clients: Vec<TermClient>,
    pub title: Option<String>,
    /// Why the last input was not delivered.
    pub refusal: Option<TermRefusal>,
    /// Set once the process has exited: (code, signal).
    pub exit: Option<(Option<i32>, Option<i32>)>,
    /// Attach or relay failure, kept for the log. The panel shows a fixed sentence instead.
    pub error: Option<String>,
    /// Set when a relay frame could not be read; cleared on the next attach (FR-L31.22).
    pub frame_error: bool,
    /// The automatic reconnect in progress, if any (FR-L31.22).
    pub reconnect: Option<ReconnectLine>,
}

/// Where the automatic reconnect stands, for the panel's state line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconnectLine {
    /// Waiting `secs` before attempt `attempt`.
    Waiting { attempt: u32, secs: u64 },
    /// Reattaching now.
    Reconnecting,
    /// Out of attempts; the panel offers 다시 연결.
    GaveUp,
}

impl ReconnectLine {
    pub fn text(self) -> String {
        use crate::terminal_reconnect as rc;
        match self {
            ReconnectLine::Waiting { attempt, secs } => rc::waiting_text(attempt, std::time::Duration::from_secs(secs)),
            ReconnectLine::Reconnecting => rc::RECONNECTING_TEXT.to_owned(),
            ReconnectLine::GaveUp => rc::GAVE_UP_TEXT.to_owned(),
        }
    }
}

/// Updates from the terminal transport and its VT decoder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalUpdate {
    Connecting,
    /// Replace the displayed screen, including any scrollback the decoder exposes.
    Lines(Vec<String>),
    Disconnected,
}

impl TerminalState {
    pub fn apply(&mut self, update: TerminalUpdate) {
        match update {
            TerminalUpdate::Connecting => self.status = TerminalStatus::Connecting,
            TerminalUpdate::Lines(lines) => {
                self.lines = lines;
                self.status = TerminalStatus::Connected;
            }
            TerminalUpdate::Disconnected => self.status = TerminalStatus::Disconnected,
        }
    }

    /// Status text for the panel, if the connection is not ready.
    pub fn message(&self) -> Option<&'static str> {
        match self.status {
            TerminalStatus::Disconnected => Some("터미널이 연결되지 않았습니다."),
            TerminalStatus::Connecting => Some("터미널에 연결하는 중입니다."),
            TerminalStatus::Connected => None,
        }
    }

    /// Banner shown over a connected terminal: another device is typing, or the last input was
    /// refused. `None` when this client may type.
    pub fn banner(&self) -> Option<String> {
        if let Some(controller) = &self.controller {
            if Some(controller.client) != self.client {
                return Some(format!("다른 기기가 입력 중입니다: {}", controller.device));
            }
        }
        match self.refusal? {
            TermRefusal::Controlled => Some("다른 기기가 입력 중입니다.".to_owned()),
            TermRefusal::ReadOnly => Some("읽기 전용으로 연결되어 입력할 수 없습니다.".to_owned()),
            TermRefusal::NotRunning => Some("터미널이 실행 중이 아닙니다.".to_owned()),
        }
    }

    /// True when the panel should offer a reconnect: the connection is gone or failed and no
    /// automatic attempt is pending.
    pub fn needs_reconnect(&self) -> bool {
        let automatic = matches!(self.reconnect, Some(ReconnectLine::Waiting { .. } | ReconnectLine::Reconnecting));
        !automatic && (self.status == TerminalStatus::Disconnected || self.error.is_some())
    }

    /// True when another client holds control, so input from this one would be refused.
    pub fn controlled_by_other(&self) -> bool {
        self.controller.as_ref().is_some_and(|c| Some(c.client) != self.client)
    }

    /// Text for a finished process.
    pub fn exit_text(&self) -> Option<String> {
        let (code, signal) = self.exit?;
        Some(match (code, signal) {
            (Some(code), _) => format!("프로세스가 종료되었습니다. (종료 코드 {code})"),
            (None, Some(signal)) => format!("프로세스가 종료되었습니다. (시그널 {signal})"),
            (None, None) => "프로세스가 종료되었습니다.".to_owned(),
        })
    }

    /// Text for an attach or relay failure: one fixed sentence, never the transport's own text.
    /// The relay refusal (FR-L31.19) is the app's own sentence and says what to do, so it is shown.
    pub fn error_text(&self) -> Option<String> {
        self.error.as_ref().map(|e| {
            if e == RELAY_REFUSED { format!("터미널 오류: {RELAY_REFUSED}") } else { crate::terminal_reconnect::FAILED_TEXT.to_owned() }
        })
    }

    /// The one line to show instead of (or under) the screen: the reconnect state, else a
    /// failure, else the exit status, else the connection state, else an unreadable frame.
    pub fn notice(&self) -> Option<String> {
        self.reconnect
            .map(ReconnectLine::text)
            .or_else(|| self.error_text())
            .or_else(|| self.exit_text())
            .or_else(|| self.message().map(str::to_owned))
            .or_else(|| self.frame_error.then(|| crate::terminal_reconnect::FRAME_ERROR_TEXT.to_owned()))
    }
}

/// A terminal session: the VT screen plus the state derived from it. Pure, so the event
/// handling is testable without a socket.
pub struct TerminalSession {
    screen: screen::Screen,
    pub state: TerminalState,
}

impl Default for TerminalSession {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalSession {
    pub fn new() -> Self {
        Self { screen: screen::Screen::new(80, 24), state: TerminalState::default() }
    }

    fn refresh(&mut self) {
        self.state.lines = screen_lines(&self.screen);
        self.state.cursor = self.screen.cursor();
        let (cols, rows) = self.screen.size();
        self.state.size = (cols, rows);
        self.state.bracketed_paste = self.screen.bracketed_paste();
    }

    pub fn connecting(&mut self) {
        self.state.error = None;
        self.state.status = TerminalStatus::Connecting;
    }

    pub fn disconnected(&mut self) {
        self.state.status = TerminalStatus::Disconnected;
    }

    pub fn fail(&mut self, message: impl Into<String>) {
        self.state.error = Some(message.into());
        self.state.status = TerminalStatus::Disconnected;
    }

    /// A relay frame could not be read. The stream goes on; the panel says so once.
    pub fn frame_error(&mut self) {
        self.state.frame_error = true;
    }

    /// The stream dropped; the next attempt starts after `delay`. The last screen stays.
    pub fn waiting(&mut self, attempt: u32, delay: std::time::Duration) {
        self.state.status = TerminalStatus::Disconnected;
        self.state.reconnect = Some(ReconnectLine::Waiting { attempt, secs: delay.as_secs().max(1) });
    }

    /// The reattach is in flight.
    pub fn reconnecting(&mut self) {
        self.state.status = TerminalStatus::Connecting;
        self.state.reconnect = Some(ReconnectLine::Reconnecting);
    }

    /// No attempts left.
    pub fn gave_up(&mut self) {
        self.state.status = TerminalStatus::Disconnected;
        self.state.reconnect = Some(ReconnectLine::GaveUp);
    }

    /// Apply one frame from the relay.
    pub fn apply_event(&mut self, event: TermEvent) {
        match event {
            TermEvent::Attached { client, term } => {
                self.state.status = TerminalStatus::Connected;
                self.state.error = None;
                self.state.reconnect = None;
                self.state.frame_error = false;
                if client.is_some() {
                    self.state.client = client;
                }
                if let Some(size) = term.map(|t| t.size).filter(|s| s.cols > 0 && s.rows > 0) {
                    self.screen = screen::Screen::new(size.cols, size.rows);
                    self.refresh();
                }
            }
            TermEvent::Snapshot { size, data } => {
                self.screen = screen::Screen::new(size.cols, size.rows);
                self.screen.feed(&data);
                self.state.status = TerminalStatus::Connected;
                self.refresh();
            }
            TermEvent::Output { data } => {
                self.screen.feed(&data);
                self.state.refusal = None;
                self.refresh();
            }
            TermEvent::Resized { size } => {
                self.screen.resize(size.cols, size.rows);
                self.refresh();
            }
            TermEvent::Control { controller } => {
                self.state.controller = controller;
                self.state.refusal = None;
            }
            TermEvent::Refused { reason, controller } => {
                self.state.refusal = Some(reason);
                if controller.is_some() {
                    self.state.controller = controller;
                }
            }
            TermEvent::Clients { clients } => self.state.clients = clients,
            TermEvent::Title { title } => self.state.title = title,
            TermEvent::Exit { code, signal } => {
                self.state.exit = Some((code, signal));
                self.state.status = TerminalStatus::Disconnected;
            }
            TermEvent::Error { message } => self.fail(message),
            TermEvent::Other => {}
        }
    }
}

/// Feed decoded updates into a sync signal owned by the workbench. Closing the sender marks
/// the stream disconnected while retaining the last screen. Dropping this future stops the feed.
pub async fn feed(mut updates: tokio::sync::mpsc::Receiver<TerminalUpdate>, mut state: dioxus_compose::prelude::SyncSignal<TerminalState>) {
    use dioxus_compose::prelude::*;

    feed_updates(&mut updates, |update| state.write().apply(update)).await;
}

async fn feed_updates(updates: &mut tokio::sync::mpsc::Receiver<TerminalUpdate>, mut apply: impl FnMut(TerminalUpdate)) {
    while let Some(update) = updates.recv().await {
        apply(update);
    }
    apply(TerminalUpdate::Disconnected);
}

/// Decode a complete VT stream into visible screen lines without performing I/O.
pub fn decode_screen(bytes: &[u8], cols: u16, rows: u16) -> Vec<String> {
    let mut screen = screen::Screen::new(cols, rows);
    screen.feed(bytes);
    screen_lines(&screen)
}

fn screen_lines(screen: &screen::Screen) -> Vec<String> {
    let text = screen.text();
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

// ---------------------------------------------------------------------------------------------
// Live attachment

/// Best-effort name of this device, shown to other clients as "controlled by <device>".
pub fn device_name() -> String {
    ["HOSTNAME", "COMPUTERNAME", "HOST"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
        .unwrap_or_else(|| "ember-editor".to_owned())
}

struct Shared {
    session: Mutex<TerminalSession>,
    revision: AtomicU64,
    size: Mutex<(u16, u16)>,
    tx: tokio::sync::mpsc::UnboundedSender<String>,
    on_change: Option<ChangeCallback>,
    /// Why a viewer has nothing to show (set by [`start_view`]).
    pick: Mutex<Option<TermPick>>,
}

/// Called after every state change, from the transport task.
pub type ChangeCallback = Arc<dyn Fn() + Send + Sync>;

impl Shared {
    fn update(&self, f: impl FnOnce(&mut TerminalSession)) {
        f(&mut self.session.lock().unwrap_or_else(|e| e.into_inner()));
        self.revision.fetch_add(1, Ordering::Release);
        if let Some(on_change) = &self.on_change {
            on_change();
        }
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        let _ = self.tx.send(detach_frame());
    }
}

/// A running attachment. Cloning shares it; when the last clone drops, the client detaches
/// (the session itself keeps running on the computer).
#[derive(Clone)]
pub struct TerminalHandle {
    shared: Arc<Shared>,
}

impl TerminalHandle {
    /// Current state, copied out of the shared session.
    pub fn snapshot(&self) -> TerminalState {
        self.shared.session.lock().unwrap_or_else(|e| e.into_inner()).state.clone()
    }

    /// For a viewer: why there is no terminal ([`TermPick::None`] or [`TermPick::Ambiguous`]); `None`
    /// while one is attached or being looked for.
    pub fn pick(&self) -> Option<TermPick> {
        self.shared.pick.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Bumped on every change; the UI polls it to know when to re-read [`snapshot`](Self::snapshot).
    pub fn revision(&self) -> u64 {
        self.shared.revision.load(Ordering::Acquire)
    }

    pub fn send_input(&self, bytes: &[u8]) {
        if !bytes.is_empty() {
            let _ = self.shared.tx.send(input_frame(bytes));
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 {
            return;
        }
        {
            let mut size = self.shared.size.lock().unwrap_or_else(|e| e.into_inner());
            if *size == (cols, rows) {
                return;
            }
            *size = (cols, rows);
        }
        let _ = self.shared.tx.send(resize_frame(cols, rows));
    }

    pub fn take_control(&self) {
        let _ = self.shared.tx.send(take_control_frame());
    }

    pub fn release_control(&self) {
        let _ = self.shared.tx.send(release_control_frame());
    }

    /// Detach now; the process keeps running.
    pub fn detach(&self) {
        let _ = self.shared.tx.send(detach_frame());
    }
}

/// Attach the editor's terminal for `folder` on `computer_id`, creating the session when no
/// running one has the key `"<project>:<folder>"`. Work runs on the app runtime; the returned
/// handle is safe to use from the UI thread.
pub fn start(
    base_url: &str,
    computer_id: &str,
    folder: &str,
    project: &str,
    on_change: Option<ChangeCallback>,
) -> TerminalHandle {
    start_with(base_url, computer_id, folder, project, None, on_change)
}

/// Start a new terminal on `computer_id` that runs `launch` (a task of the 실행 window,
/// FR-L31.16) and attach to it. The session is the project's (listings filter on `project`).
pub fn start_task(
    base_url: &str,
    computer_id: &str,
    project: &str,
    launch: TermLaunch,
    on_change: Option<ChangeCallback>,
) -> TerminalHandle {
    let folder = launch.cwd.clone();
    start_with(base_url, computer_id, &folder, project, Some(launch), on_change)
}

fn start_with(
    base_url: &str,
    computer_id: &str,
    folder: &str,
    project: &str,
    launch: Option<TermLaunch>,
    on_change: Option<ChangeCallback>,
) -> TerminalHandle {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let shared = Arc::new(Shared {
        session: Mutex::new(TerminalSession::new()),
        revision: AtomicU64::new(0),
        // The size until the panel measures itself (FR-L31.20, `ui::term_fit`).
        size: Mutex::new((80, 24)),
        tx,
        on_change,
        pick: Mutex::new(None),
    });
    shared.update(|s| s.connecting());
    let task_shared = Arc::downgrade(&shared);
    let (base, computer, folder, project) =
        (base_url.to_owned(), computer_id.to_owned(), folder.to_owned(), project.to_owned());
    crate::services::services().rt.spawn(async move {
        use crate::terminal_reconnect::{Next, Reconnect, StreamEnd};
        let mut rx = rx;
        let mut policy = Reconnect::default();
        loop {
            let end = match run(&task_shared, &base, &computer, &folder, &project, launch.clone(), &mut rx, &mut policy).await {
                Ok(end) => end,
                Err(e) => {
                    // The raw text goes to the log; the panel shows a fixed sentence.
                    tracing::warn!("editor terminal stream failed: {e}");
                    let Some(shared) = task_shared.upgrade() else { return };
                    shared.update(|s| s.fail(e.to_string()));
                    StreamEnd::Dropped
                }
            };
            let Some(shared) = task_shared.upgrade() else { return };
            // A task terminal has no key (FR-L31.16): attaching again would start the task a second
            // time, so its stream is not retried (FR-L31.22 covers the keyed panel shell).
            if launch.is_some() {
                shared.update(|s| s.disconnected());
                return;
            }
            match policy.on_end(end) {
                Next::Stop => {
                    shared.update(|s| s.disconnected());
                    return;
                }
                Next::GiveUp => {
                    shared.update(|s| s.gave_up());
                    return;
                }
                Next::Retry { attempt, delay } => {
                    shared.update(|s| s.waiting(attempt, delay));
                    drop(shared);
                    tokio::time::sleep(delay).await;
                    // Keys typed while the stream was down are not replayed into the shell.
                    loop {
                        match rx.try_recv() {
                            Ok(_) => continue,
                            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return,
                        }
                    }
                    let Some(shared) = task_shared.upgrade() else { return };
                    shared.update(|s| s.reconnecting());
                }
            }
        }
    });
    TerminalHandle { shared }
}

/// Attach the editor to the existing terminal `term_id` (FR-P6: a terminal that survived the window,
/// listed by [`crate::terminal_list`]). Dropping the last clone detaches; the terminal keeps running.
pub fn attach_existing(base_url: &str, computer_id: &str, term_id: &str, on_change: Option<ChangeCallback>) -> TerminalHandle {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let shared = Arc::new(Shared {
        session: Mutex::new(TerminalSession::new()),
        revision: AtomicU64::new(0),
        size: Mutex::new((80, 24)),
        tx,
        on_change,
        pick: Mutex::new(None),
    });
    shared.update(|s| s.connecting());
    let task_shared = Arc::downgrade(&shared);
    let (base, computer, term) = (base_url.to_owned(), computer_id.to_owned(), term_id.to_owned());
    crate::services::services().rt.spawn(async move {
        let hello = TermHello {
            device: device_name(),
            kind: Some("ember-editor".into()),
            pid: Some(std::process::id()),
            size: Some(PtySize { rows: 24, cols: 80 }),
            active: true,
            read_only: false,
            snapshot: true,
        };
        let token = server_token(&base);
        let mut rx = rx;
        // No reconnect policy: a reattached tab ends when its stream ends, as in #768.
        if let Err(e) = stream(&task_shared, &base, &computer, &term, hello, token, &mut rx, None).await {
            if let Some(shared) = task_shared.upgrade() {
                shared.update(|s| s.fail(e.to_string()));
            }
        }
        if let Some(shared) = task_shared.upgrade() {
            shared.update(|s| s.disconnected());
        }
    });
    TerminalHandle { shared }
}

/// The server token, only for a server this machine may trust with it.
pub(crate) fn server_token(base: &str) -> Option<String> {
    ember_client::token::token_allowed_for(base)
        .then(ember_client::token::server_token_from_env)
        .flatten()
}

// ---------------------------------------------------------------------------------------------
// Reach: plain HTTP or the transport (SPEC FR-N9, #867)

/// How the app reaches the server's terminal relay. With a server reached as a peer the app's
/// `server_url` is `peer:<id>`, which is not a URL: the relay is then reached through the
/// client's [`Api`](ember_client::api::Api) over the transport, which sends no token (the device
/// key is the credential).
#[derive(Clone, Debug)]
pub enum TermReach {
    /// The server's HTTP base URL; the attach is a `ws(s)` socket as before.
    Http(String),
    /// The server over the transport.
    Peer(ember_client::api::Api),
}

impl TermReach {
    /// The reach for `api`, the app's client `Api`: over the transport when it has a peer, else
    /// `server_url` over HTTP.
    pub fn select(server_url: &str, api: &ember_client::api::Api) -> TermReach {
        if api.peer().is_some() {
            TermReach::Peer(api.clone())
        } else {
            TermReach::Http(server_url.to_owned())
        }
    }

    /// The app's own reach, from [`services`](crate::services::services).
    pub fn of_app() -> TermReach {
        let s = crate::services::services();
        Self::select(&s.config.server_url, s.client.api())
    }

    pub fn is_peer(&self) -> bool {
        matches!(self, TermReach::Peer(_))
    }
}

/// Where a terminal call goes: a URL over HTTP, or a path below `/api` for the `Api`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermTarget {
    Url(String),
    Api(String),
}

/// True for the `peer:<id>` base the app keeps while a peer is set.
fn is_peer_base(base: &str) -> bool {
    base.starts_with("peer:")
}

/// A `peer:<id>` base without a transport `Api` is never dialed as a URL.
pub(crate) fn refuse_peer_base(base: &str) -> Result<(), ClientError> {
    if is_peer_base(base) {
        tracing::warn!("terminal relay: {base} is a peer, and no transport is open to it");
        return Err(ClientError::Refused(crate::device_link::UNREACHABLE));
    }
    Ok(())
}

/// `{path}` without its `/api` prefix, as the `Api` takes it.
fn below_api(path: String) -> String {
    path.strip_prefix("/api").map(str::to_owned).unwrap_or(path)
}

/// The list of a project's terminals ([`terms_list_url`]) on `reach`.
pub fn list_target(
    reach: &TermReach,
    computer_id: &str,
    project: &str,
    origin: Option<&str>,
    running: bool,
) -> Result<TermTarget, ClientError> {
    match reach {
        TermReach::Http(base) => {
            refuse_peer_base(base)?;
            Ok(TermTarget::Url(terms_list_url(base, computer_id, project, origin, running)))
        }
        TermReach::Peer(_) => Ok(TermTarget::Api(below_api(terms_list_url("", computer_id, project, origin, running)))),
    }
}

/// The attach socket of `term_id` ([`attach_url`]) on `reach`. Over HTTP the relay rule applies
/// (FR-L31.19).
pub fn attach_target(reach: &TermReach, computer_id: &str, term_id: &str) -> Result<TermTarget, ClientError> {
    match reach {
        TermReach::Http(base) => {
            refuse_peer_base(base)?;
            check_relay(base)?;
            Ok(TermTarget::Url(attach_url(base, computer_id, term_id)))
        }
        TermReach::Peer(_) => Ok(TermTarget::Api(below_api(attach_url("", computer_id, term_id)))),
    }
}

/// The sentence for a refused answer: a 403 `scope_denied` is the device's scope (FR-N8).
pub fn status_refusal(status: u16, body: &[u8]) -> Option<&'static str> {
    let code = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("code").and_then(|c| c.as_str()).map(str::to_owned));
    (status == 403 && code.as_deref() == Some("scope_denied")).then_some(crate::ui::device_scope::SCOPE_DENIED)
}

/// The device sentence `text` is, if it is one: not allowed or revoked, unreachable, or the
/// scope refused (FR-N8, FR-N9). The session pane shows these as they are.
pub fn known_refusal(text: &str) -> Option<&'static str> {
    [crate::device_link::NOT_ALLOWED, crate::device_link::UNREACHABLE, crate::ui::device_scope::SCOPE_DENIED]
        .into_iter()
        .find(|t| *t == text)
}

/// A failed call over the transport as the panel's error: the device sentence when there is one.
fn api_failure(e: ember_client::api::ApiError) -> ClientError {
    match crate::device_link::refusal_text(&e) {
        Some(t) => {
            tracing::warn!("terminal over the transport refused: {e}");
            ClientError::Refused(t)
        }
        None => ClientError::Protocol(e.to_string()),
    }
}

/// Runs `fut` on the app runtime, or on the current one where the app's services are not
/// installed (the crate's integration tests).
fn spawn_task(fut: impl std::future::Future<Output = ()> + Send + 'static) {
    match crate::services::try_services() {
        Some(s) => {
            s.rt.spawn(fut);
        }
        None => {
            tokio::spawn(fut);
        }
    }
}

/// A new attachment in the connecting state, and the receiver of its outgoing frames.
fn new_shared(on_change: Option<ChangeCallback>) -> (Arc<Shared>, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let shared = Arc::new(Shared {
        session: Mutex::new(TerminalSession::new()),
        revision: AtomicU64::new(0),
        size: Mutex::new((80, 24)),
        tx,
        on_change,
        pick: Mutex::new(None),
    });
    shared.update(|s| s.connecting());
    (shared, rx)
}

/// A handle that shows `sentence` and never attaches.
fn refused_handle(sentence: &'static str, on_change: Option<ChangeCallback>) -> TerminalHandle {
    let (shared, _rx) = new_shared(on_change);
    shared.update(|s| s.fail(sentence));
    TerminalHandle { shared }
}

/// [`start_view`] on `reach` (FR-N9, #867): the session pane's entry. Over HTTP it is
/// [`start_view`] unchanged; over the transport the list goes through the `Api` and the attach
/// through [`ember_client::api::Api::open_peer_socket`]. A refusal (the device's scope, a revoked
/// device, an unreachable server) ends in its sentence; the pane does not retry.
pub fn start_view_with(
    reach: TermReach,
    computer_id: &str,
    project: &str,
    on_change: Option<ChangeCallback>,
) -> TerminalHandle {
    let api = match reach {
        TermReach::Http(base) if is_peer_base(&base) => {
            return refused_handle(crate::device_link::UNREACHABLE, on_change);
        }
        TermReach::Http(base) => return start_view(&base, computer_id, project, on_change),
        TermReach::Peer(api) => api,
    };
    let (shared, rx) = new_shared(on_change);
    let task_shared = Arc::downgrade(&shared);
    let (computer, project) = (computer_id.to_owned(), project.to_owned());
    spawn_task(async move {
        let r = view_peer(&task_shared, &api, &computer, &project, rx).await;
        finish(&task_shared, r);
    });
    TerminalHandle { shared }
}

/// [`attach_existing`] on `reach`: over HTTP unchanged, over the transport through the `Api`.
pub fn attach_existing_with(
    reach: TermReach,
    computer_id: &str,
    term_id: &str,
    on_change: Option<ChangeCallback>,
) -> TerminalHandle {
    let api = match reach {
        TermReach::Http(base) if is_peer_base(&base) => {
            return refused_handle(crate::device_link::UNREACHABLE, on_change);
        }
        TermReach::Http(base) => return attach_existing(&base, computer_id, term_id, on_change),
        TermReach::Peer(api) => api,
    };
    let (shared, rx) = new_shared(on_change);
    let task_shared = Arc::downgrade(&shared);
    let (computer, term) = (computer_id.to_owned(), term_id.to_owned());
    spawn_task(async move {
        let hello = TermHello {
            device: device_name(),
            kind: Some("ember-editor".into()),
            pid: Some(std::process::id()),
            size: Some(PtySize { rows: 24, cols: 80 }),
            active: true,
            read_only: false,
            snapshot: true,
        };
        let mut rx = rx;
        let r = stream_peer(&task_shared, &api, &computer, &term, hello, &mut rx).await;
        let r = after_peer_end(&api, r).await.map(|_| ());
        finish(&task_shared, r);
    });
    TerminalHandle { shared }
}

/// Records how a stream without reconnects ended: the failure, then disconnected.
fn finish(shared: &std::sync::Weak<Shared>, r: Result<(), ClientError>) {
    let Some(shared) = shared.upgrade() else { return };
    if let Err(e) = r {
        tracing::warn!("terminal stream over the transport ended: {e}");
        shared.update(|s| s.fail(e.to_string()));
    }
    shared.update(|s| s.disconnected());
}

async fn view_peer(
    shared: &std::sync::Weak<Shared>,
    api: &ember_client::api::Api,
    computer: &str,
    project: &str,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<String>,
) -> Result<(), ClientError> {
    let reach = TermReach::Peer(api.clone());
    let TermTarget::Api(path) = list_target(&reach, computer, project, Some("agent"), true)? else {
        return Err(ClientError::Protocol("the terminal list has no path over the transport".into()));
    };
    let (status, body) = api.raw_request(reqwest::Method::GET, &path, None).await.map_err(api_failure)?;
    if !(200..300).contains(&status) {
        return Err(match status_refusal(status, &body) {
            Some(t) => ClientError::Refused(t),
            None => ClientError::Protocol(format!("terminal list answered {status}: {}", String::from_utf8_lossy(&body))),
        });
    }
    let list: Vec<TermInfo> = serde_json::from_slice(&body).map_err(|e| ClientError::Protocol(e.to_string()))?;
    let pick = pick_agent_terminal(&list);
    let TermPick::One(term_id) = &pick else {
        if let Some(strong) = shared.upgrade() {
            *strong.pick.lock().unwrap_or_else(|e| e.into_inner()) = Some(pick);
        }
        return Ok(());
    };
    let r = stream_peer(shared, api, computer, term_id, TermHello::viewer(), &mut rx).await;
    after_peer_end(api, r).await.map(|_| ())
}

/// Attaches to `term_id` over the transport and runs until the stream ends. No reconnect
/// policy: the pane's viewer and a reattached tab end with their stream, as over HTTP.
async fn stream_peer(
    shared: &std::sync::Weak<Shared>,
    api: &ember_client::api::Api,
    computer: &str,
    term_id: &str,
    hello: TermHello,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
) -> Result<crate::terminal_reconnect::StreamEnd, ClientError> {
    let TermTarget::Api(path) = attach_target(&TermReach::Peer(api.clone()), computer, term_id)? else {
        return Err(ClientError::Protocol("the attach has no path over the transport".into()));
    };
    let mut ws = api.open_peer_socket(&path).await.map_err(api_failure)?;
    pump(shared, &mut ws, hello, rx, None).await
}

/// After a stream over the transport ended (dropped, failed, or the process exited), asks the
/// server once (`GET /api/health`): a revoked or refused device, or a server that is gone, ends
/// in the device sentence rather than a dropped stream. A stream this side released, and a
/// refusal already named, are left as they are.
async fn after_peer_end(
    api: &ember_client::api::Api,
    r: Result<crate::terminal_reconnect::StreamEnd, ClientError>,
) -> Result<crate::terminal_reconnect::StreamEnd, ClientError> {
    use crate::terminal_reconnect::StreamEnd;
    match &r {
        Ok(StreamEnd::Released) | Err(ClientError::Refused(_)) => return r,
        _ => {}
    }
    match crate::device_link::check(api).await {
        Err(sentence) => Err(ClientError::Refused(sentence)),
        Ok(()) => r,
    }
}

async fn run(
    shared: &std::sync::Weak<Shared>,
    base: &str,
    computer: &str,
    folder: &str,
    project: &str,
    launch: Option<TermLaunch>,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
    policy: &mut crate::terminal_reconnect::Reconnect,
) -> Result<crate::terminal_reconnect::StreamEnd, ClientError> {
    let live = || shared.upgrade().ok_or_else(|| ClientError::Protocol("terminal closed".into()));
    let (cols, rows) = {
        let strong = live()?;
        let size = *strong.size.lock().unwrap_or_else(|e| e.into_inner());
        size
    };
    let size = PtySize { rows, cols };
    let request = task_request(folder, project, size, launch);
    // FR-L31.19: https is verified (webpki roots); plain http only where FR-S11 allows it.
    check_relay(base)?;
    ensure_tls_provider();
    // The shared loader; the token goes only to a server this machine may trust with it.
    let token = ember_client::token::token_allowed_for(base)
        .then(ember_client::token::server_token_from_env)
        .flatten();
    let mut post = reqwest::Client::new().post(terms_url(base, computer)).json(&request);
    if let Some(token) = &token {
        post = post.bearer_auth(token);
    }
    let created: TermCreateResponse = post
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let hello = TermHello {
        device: device_name(),
        kind: Some("ember-editor".into()),
        pid: Some(std::process::id()),
        size: Some(size),
        active: true,
        read_only: false,
        snapshot: true,
    };
    // Same key, so a reattach after a drop re-adopts the PTY the node kept (FR-P6); the hello
    // asks for a snapshot, which redraws the screen.
    stream(shared, base, computer, &created.term.id, hello, token, rx, Some(policy)).await
}

/// The create request: the panel's shell under the `"<project>:<folder>"` key, or a task's
/// program in a new session with the task's title and no key (FR-L31.16).
pub fn task_request(folder: &str, project: &str, size: PtySize, launch: Option<TermLaunch>) -> TermCreateRequest {
    let base = TermCreateRequest {
        program: None,
        cwd: folder.into(),
        env: Default::default(),
        env_clear: false,
        size: Some(size),
        origin: TermOrigin::IdeEmber,
        project: Some(project.to_owned()),
        title: None,
        key: Some(session_key(project, folder)),
        tags: Default::default(),
    };
    match launch {
        None => base,
        Some(l) => {
            let mut tags = std::collections::BTreeMap::new();
            tags.insert("ember.task".to_string(), l.title.clone());
            TermCreateRequest { program: Some(l.program), cwd: l.cwd.into(), env: l.env, title: Some(l.title), key: None, tags, ..base }
        }
    }
}

/// Attaches to terminal `term_id` with `hello` as the first frame and runs until it ends.
async fn stream(
    shared: &std::sync::Weak<Shared>,
    base: &str,
    computer: &str,
    term_id: &str,
    hello: TermHello,
    token: Option<String>,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
    policy: Option<&mut crate::terminal_reconnect::Reconnect>,
) -> Result<crate::terminal_reconnect::StreamEnd, ClientError> {
    // FR-L31.19: wss for an https server, verified against the webpki roots.
    check_relay(base)?;
    ensure_tls_provider();
    let mut upgrade = attach_url(base, computer, term_id).into_client_request()?;
    if let Some(token) = &token {
        let value = HeaderValue::from_str(&ember_client::token::bearer_header(token))
            .map_err(|_| ClientError::Protocol("the server token is not a valid header value".into()))?;
        upgrade.headers_mut().insert("Authorization", value);
    }
    let (mut ws, _) = tokio_tungstenite::connect_async(upgrade).await?;
    pump(shared, &mut ws, hello, rx, policy).await
}

/// Sends `hello` on an attached socket, then carries frames both ways until the stream ends.
/// The same for a socket over HTTP and one over the transport (FR-N9).
async fn pump<S>(
    shared: &std::sync::Weak<Shared>,
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
    hello: TermHello,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
    mut policy: Option<&mut crate::terminal_reconnect::Reconnect>,
) -> Result<crate::terminal_reconnect::StreamEnd, ClientError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use crate::terminal_reconnect::StreamEnd;
    let live = || shared.upgrade().ok_or_else(|| ClientError::Protocol("terminal closed".into()));
    let hello = serde_json::to_string(&hello).map_err(|e| ClientError::Protocol(e.to_string()))?;
    ws.send(Message::Text(hello.into())).await?;
    loop {
        tokio::select! {
            incoming = ws.next() => match incoming {
                None | Some(Ok(Message::Close(_))) => return Ok(StreamEnd::Dropped),
                Some(Ok(Message::Text(text))) => {
                    // An unparsable frame is skipped, logged, and named on the panel (FR-L31.22).
                    let event = match serde_json::from_str::<TermEvent>(&text) {
                        Ok(event) => event,
                        Err(e) => {
                            tracing::warn!("skipping an unparsable terminal frame: {e}");
                            live()?.update(|s| s.frame_error());
                            continue;
                        }
                    };
                    if matches!(event, TermEvent::Attached { .. }) {
                        if let Some(policy) = policy.as_deref_mut() {
                            policy.attached();
                        }
                    }
                    let end = match &event {
                        TermEvent::Exit { .. } => Some(StreamEnd::Exited),
                        TermEvent::Error { .. } => Some(StreamEnd::Dropped),
                        _ => None,
                    };
                    live()?.update(|s| s.apply_event(event));
                    if let Some(end) = end {
                        return Ok(end);
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(tokio_tungstenite::tungstenite::Error::ConnectionClosed)) => return Ok(StreamEnd::Dropped),
                Some(Err(e)) => return Err(e.into()),
            },
            outgoing = rx.recv() => match outgoing {
                Some(frame) => ws.send(Message::Text(frame.into())).await?,
                None => {
                    let _ = ws.close(None).await;
                    return Ok(StreamEnd::Released);
                }
            },
        }
    }
}

/// Watches a project's agent terminal on `computer_id`, read-only: lists the project's running
/// terminals, takes the one agent terminal there is ([`pick_agent_terminal`]) and attaches with
/// [`TermHello::viewer`]. With none, or with several, nothing is attached and [`TerminalHandle::pick`]
/// says which. Dropping the last clone of the handle detaches; the terminal keeps running.
pub fn start_view(
    base_url: &str,
    computer_id: &str,
    project: &str,
    on_change: Option<ChangeCallback>,
) -> TerminalHandle {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let shared = Arc::new(Shared {
        session: Mutex::new(TerminalSession::new()),
        revision: AtomicU64::new(0),
        size: Mutex::new((80, 24)),
        tx,
        on_change,
        pick: Mutex::new(None),
    });
    shared.update(|s| s.connecting());
    let task_shared = Arc::downgrade(&shared);
    let (base, computer, project) = (base_url.to_owned(), computer_id.to_owned(), project.to_owned());
    crate::services::services().rt.spawn(async move {
        if let Err(e) = view(&task_shared, &base, &computer, &project, rx).await {
            if let Some(shared) = task_shared.upgrade() {
                shared.update(|s| s.fail(e.to_string()));
            }
        }
        if let Some(shared) = task_shared.upgrade() {
            shared.update(|s| s.disconnected());
        }
    });
    TerminalHandle { shared }
}

async fn view(
    shared: &std::sync::Weak<Shared>,
    base: &str,
    computer: &str,
    project: &str,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<String>,
) -> Result<(), ClientError> {
    let token = ember_client::token::token_allowed_for(base)
        .then(ember_client::token::server_token_from_env)
        .flatten();
    let mut get = reqwest::Client::new().get(terms_list_url(base, computer, project, Some("agent"), true));
    if let Some(token) = &token {
        get = get.bearer_auth(token);
    }
    let list: Vec<TermInfo> = get.send().await?.error_for_status()?.json().await?;
    let pick = pick_agent_terminal(&list);
    let TermPick::One(term_id) = &pick else {
        if let Some(strong) = shared.upgrade() {
            *strong.pick.lock().unwrap_or_else(|e| e.into_inner()) = Some(pick);
        }
        return Ok(());
    };
    stream(shared, base, computer, term_id, TermHello::viewer(), token, &mut rx, None).await.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_l18_a_viewer_attaches_read_only_without_resizing_the_shared_terminal() {
        let hello = serde_json::to_value(TermHello::viewer()).unwrap();
        assert_eq!(hello["read_only"], true);
        assert_eq!(hello["active"], false, "a viewer must not take the terminal's size");
        assert_eq!(hello["snapshot"], true);
        assert_eq!(hello["kind"], "ember-app");
        assert!(hello["size"].is_null());
    }

    #[test]
    fn fr_l31_16_a_task_terminal_is_a_new_session_running_the_program() {
        let size = PtySize { rows: 24, cols: 80 };
        let panel = serde_json::to_value(task_request("/w", "/w", size, None)).unwrap();
        assert_eq!(panel["program"], serde_json::Value::Null);
        assert_eq!(panel["key"], "/w:/w");
        let mut env = std::collections::BTreeMap::new();
        env.insert("A".to_string(), "1".to_string());
        let launch = TermLaunch { program: TermProgram::Shell("cargo build".into()), cwd: "/w/sub".into(), env, title: "build".into() };
        let json = serde_json::to_value(task_request("/w/sub", "/w", size, Some(launch))).unwrap();
        // The node's `Program` is snake_case and externally tagged.
        assert_eq!(json["program"], serde_json::json!({"shell": "cargo build"}));
        assert_eq!(json["cwd"], "/w/sub");
        assert_eq!(json["env"]["A"], "1");
        assert_eq!(json["title"], "build");
        assert_eq!(json["key"], serde_json::Value::Null, "every run is a new terminal");
        assert_eq!(json["project"], "/w");
        assert_eq!(json["origin"], "ide-ember");
        let argv = serde_json::to_value(TermProgram::Argv(vec!["node".into(), "a.js".into()])).unwrap();
        assert_eq!(argv, serde_json::json!({"argv": ["node", "a.js"]}));
    }

    #[test]
    fn fr_l18_the_project_terminal_list_url_carries_project_origin_and_running() {
        assert_eq!(
            terms_list_url("http://s:1/", "c 1", "my proj", Some("agent"), true),
            "http://s:1/api/computers/c%201/terms?project=my%20proj&origin=agent&running=true"
        );
        assert_eq!(terms_list_url("http://s:1", "c", "p", None, false), "http://s:1/api/computers/c/terms?project=p");
    }

    fn term(id: &str, origin: &str, state: &str) -> TermInfo {
        TermInfo { id: id.into(), origin: origin.into(), state: state.into(), ..Default::default() }
    }

    #[test]
    fn fr_l18_a_member_terminal_is_picked_only_when_it_is_the_one_running_agent_terminal() {
        assert_eq!(pick_agent_terminal(&[]), TermPick::None);
        assert_eq!(pick_agent_terminal(&[term("a", "agent", "running")]), TermPick::One("a".into()));
        let mixed = [term("e", "ide-ember", "running"), term("x", "agent", "exited"), term("a", "agent", "running")];
        assert_eq!(pick_agent_terminal(&mixed), TermPick::One("a".into()), "other origins and ended terminals do not count");
        let two = [term("a", "agent", "running"), term("b", "agent", "running")];
        assert_eq!(pick_agent_terminal(&two), TermPick::Ambiguous, "two candidates and no link to a member: no guess");
        assert_eq!(pick_agent_terminal(&[term("e", "ide-ember", "running")]), TermPick::None);
    }

    #[test]
    fn fr_l31_terminal_starts_disconnected_with_korean_message() {
        let state = TerminalState::default();
        assert_eq!(state.status, TerminalStatus::Disconnected);
        assert_eq!(state.message(), Some("터미널이 연결되지 않았습니다."));
        assert!(state.lines.is_empty());
    }

    #[test]
    fn fr_l31_terminal_connecting_and_updates_are_visible() {
        let mut state = TerminalState::default();
        state.apply(TerminalUpdate::Connecting);
        assert_eq!(state.status, TerminalStatus::Connecting);
        state.apply(TerminalUpdate::Lines(vec!["첫 줄".into(), "둘째 줄".into()]));
        assert_eq!(state.status, TerminalStatus::Connected);
        assert_eq!(state.lines, ["첫 줄", "둘째 줄"]);
        assert_eq!(state.message(), None);
    }

    #[test]
    fn fr_p2_terminal_updates_replace_screen_and_disconnect_preserves_it() {
        let mut state = TerminalState::default();
        state.apply(TerminalUpdate::Lines(vec!["이전 화면".into()]));
        state.apply(TerminalUpdate::Lines(vec!["새 화면".into()]));
        state.apply(TerminalUpdate::Disconnected);
        assert_eq!(state.lines, ["새 화면"]);
        assert_eq!(state.message(), Some("터미널이 연결되지 않았습니다."));
    }
    #[test]
    fn fr_p3_wire_messages_match_node_protocol() {
        let request = TermCreateRequest {
            program: None, cwd: "/workspace".into(), env: Default::default(), env_clear: false,
            size: None, origin: TermOrigin::IdeEmber, project: Some("/workspace".into()),
            title: None, key: None, tags: Default::default(),
        };
        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json["origin"], "ide-ember");
        assert_eq!(json["cwd"], "/workspace");
        let event: TermEvent = serde_json::from_str(r#"{"type":"snapshot","size":{"cols":80,"rows":24},"data":"aGk="}"#).unwrap();
        match event {
            TermEvent::Snapshot { size, data } => {
                assert_eq!((size.cols, size.rows), (80, 24));
                assert_eq!(data, b"hi");
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert_eq!(
            serde_json::from_str::<TermEvent>(r#"{"type":"exit","code":0,"signal":null}"#).unwrap(),
            TermEvent::Exit { code: Some(0), signal: None }
        );
        assert_eq!(serde_json::from_str::<TermEvent>(r#"{"type":"bell"}"#).unwrap(), TermEvent::Other);
    }

    #[test]
    fn decoder_handles_styles_cursor_and_erases() {
        assert_eq!(decode_screen(b"plain \x1b[31mred\x1b[0m", 40, 4), ["plain red"]);
        assert_eq!(decode_screen(b"hello\rXY\x1b[K", 40, 4), ["XY"]);
        assert_eq!(decode_screen(b"first\r\nsecond\x1b[1;1Htop\x1b[2J", 40, 4), Vec::<String>::new());
        assert_eq!(decode_screen(b"abc\x08Z\r\nline\x1b[1A\x1b[2G!", 40, 4), ["a!Z", "line"]);
        assert_eq!(decode_screen("한글".as_bytes(), 40, 4), ["한글"]);
    }

    #[test]
    fn decoder_keeps_partial_sequences_between_events() {
        let mut screen = screen::Screen::new(40, 4);
        screen.feed(b"a\x1b[");
        screen.feed(b"31mred\x1b[0m");
        let bytes = "한".as_bytes();
        screen.feed(&bytes[..1]);
        screen.feed(&bytes[1..]);
        assert_eq!(screen_lines(&screen), ["ared한"]);
    }

    #[tokio::test]
    async fn feed_disconnect_keeps_last_screen() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        tx.send(TerminalUpdate::Connecting).await.unwrap();
        tx.send(TerminalUpdate::Lines(vec!["last screen".into()])).await.unwrap();
        drop(tx);
        let mut state = TerminalState::default();
        feed_updates(&mut rx, |update| state.apply(update)).await;
        assert_eq!(state.status, TerminalStatus::Disconnected);
        assert_eq!(state.lines, ["last screen"]);
    }

    fn client(id: u64, device: &str) -> TermClient {
        TermClient { client: id, device: device.into(), kind: None, read_only: false }
    }

    fn frame(frame: String) -> serde_json::Value {
        serde_json::from_str(&frame).unwrap()
    }

    #[test]
    fn fr_p3_relay_urls_use_computer_and_term_ids() {
        assert_eq!(terms_url("http://srv:8080/", "c1"), "http://srv:8080/api/computers/c1/terms");
        assert_eq!(attach_url("http://srv:8080", "c1", "t9"), "ws://srv:8080/api/computers/c1/terms/t9/attach");
        assert_eq!(attach_url("https://srv", "a b", "t"), "wss://srv/api/computers/a%20b/terms/t/attach");
        assert_eq!(session_key("proj", "/work"), "proj:/work");
    }

    #[test]
    fn fr_l31_19_attach_scheme_follows_the_server_scheme() {
        assert_eq!(attach_url("https://srv:8443/", "c", "t"), "wss://srv:8443/api/computers/c/terms/t/attach");
        assert_eq!(attach_url("http://127.0.0.1:8740", "c", "t"), "ws://127.0.0.1:8740/api/computers/c/terms/t/attach");
        assert_eq!(terms_url("https://srv:8443", "c"), "https://srv:8443/api/computers/c/terms");
    }

    #[test]
    fn fr_l31_19_https_is_always_allowed() {
        assert!(relay_allowed_with_hosts("https://srv.example:8443", "").is_ok());
        assert!(relay_allowed_with_hosts("https://10.0.0.5", "").is_ok());
    }

    #[test]
    fn fr_l31_19_plain_http_is_allowed_to_loopback_only() {
        for base in ["http://127.0.0.1:8740", "http://localhost:8740", "http://[::1]:8740", "http://127.1.2.3"] {
            assert!(relay_allowed_with_hosts(base, "").is_ok(), "{base}");
        }
        let err = relay_allowed_with_hosts("http://srv.example:8740", "").unwrap_err();
        assert!(err.contains("refusing plain http"), "{err}");
        assert!(relay_allowed_with_hosts("http://192.168.0.2:8740", "").is_err());
    }

    #[test]
    fn fr_l31_19_plain_http_to_a_listed_host_needs_an_explicit_http_entry() {
        // A bare host entry admits that host over https only, as for the token (FR-S11).
        assert!(relay_allowed_with_hosts("http://srv.example:8740", "srv.example").is_err());
        assert!(relay_allowed_with_hosts("http://srv.example:8740", "http://srv.example:8740").is_ok());
        assert!(relay_allowed_with_hosts("http://srv.example:9000", "http://srv.example:8740").is_err());
    }

    #[test]
    fn fr_l31_19_other_schemes_and_non_urls_are_refused() {
        assert!(relay_allowed_with_hosts("ws://127.0.0.1:8740", "").is_err());
        assert!(relay_allowed_with_hosts("ftp://srv", "").is_err());
        assert!(relay_allowed_with_hosts("not a url", "").is_err());
    }

    #[test]
    fn fr_l31_19_the_refusal_the_panel_shows_is_korean() {
        let err = check_relay("http://srv.example:8740").unwrap_err();
        assert_eq!(err.to_string(), RELAY_REFUSED, "the panel text carries no English reason");
        let mut session = TerminalSession::new();
        session.fail(err.to_string());
        assert_eq!(session.state.error_text().as_deref(), Some(format!("터미널 오류: {RELAY_REFUSED}").as_str()));
        assert!(RELAY_REFUSED.ends_with("하세요."));
    }

    #[test]
    fn fr_p3_create_request_carries_key_cwd_and_origin() {
        let request = TermCreateRequest {
            program: None, cwd: "/work".into(), env: Default::default(), env_clear: false,
            size: Some(PtySize { rows: 24, cols: 80 }), origin: TermOrigin::IdeEmber,
            project: Some("proj".into()), title: None, key: Some(session_key("proj", "/work")),
            tags: Default::default(),
        };
        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json["key"], "proj:/work");
        assert_eq!(json["origin"], "ide-ember");
        assert_eq!(json["size"]["cols"], 80);
        let response: TermCreateResponse = serde_json::from_str(
            r#"{"created":false,"term":{"id":"t1","title":"zsh","size":{"rows":24,"cols":80},"extra":1}}"#,
        ).unwrap();
        assert!(!response.created);
        assert_eq!(response.term.id, "t1");
    }

    #[test]
    fn fr_p3_snapshot_then_output_builds_the_screen() {
        let mut session = TerminalSession::new();
        session.connecting();
        session.apply_event(TermEvent::Attached { client: Some(7), term: None });
        assert_eq!(session.state.client, Some(7));
        session.apply_event(TermEvent::Snapshot { size: PtySize { rows: 4, cols: 40 }, data: b"old\r\n".to_vec() });
        assert_eq!(session.state.lines, ["old"]);
        session.apply_event(TermEvent::Output { data: b"new".to_vec() });
        assert_eq!(session.state.lines, ["old", "new"]);
        // A second snapshot resets the screen first.
        session.apply_event(TermEvent::Snapshot { size: PtySize { rows: 4, cols: 40 }, data: b"fresh".to_vec() });
        assert_eq!(session.state.lines, ["fresh"]);
        session.apply_event(TermEvent::Resized { size: PtySize { rows: 10, cols: 50 } });
        assert_eq!(session.state.size, (50, 10));
        assert_eq!(session.state.status, TerminalStatus::Connected);
    }

    #[test]
    fn fr_p6_term_list_response_decodes() {
        let list: Vec<TermInfo> = serde_json::from_str(
            r#"[{"id":"t1","key":"p:/w","title":"zsh","origin":"ide-ember","project":"p","argv":["/bin/zsh"],"cwd":"/w","pid":42,"state":"running","exit_code":null,"signal":null,"size":{"rows":24,"cols":80},"created_ms":1,"finished_ms":null,"last_activity_ms":2,"tags":{},"clients":[],"controller":null,"survives_node_restart":false,"adopted":false,"has_screen":true}]"#,
        )
        .unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].id.as_str(), list[0].title.as_str()), ("t1", "zsh"));
        assert_eq!((list[0].size.cols, list[0].size.rows), (80, 24));
    }

    #[test]
    fn fr_p6_reconnect_is_offered_when_disconnected_or_failed() {
        let mut session = TerminalSession::new();
        assert!(session.state.needs_reconnect());
        session.connecting();
        assert!(!session.state.needs_reconnect());
        session.fail("boom");
        assert!(session.state.needs_reconnect());
        session.state.status = TerminalStatus::Connected;
        assert!(session.state.needs_reconnect());
        session.state.error = None;
        assert!(!session.state.needs_reconnect());
    }

    #[test]
    fn fr_l31_22_drop_shows_waiting_then_reconnecting_and_attach_clears_it() {
        let mut session = TerminalSession::new();
        session.apply_event(TermEvent::Snapshot { size: PtySize { rows: 24, cols: 80 }, data: b"$ ls".to_vec() });
        session.fail("connection reset by peer (os error 54)");
        session.waiting(1, std::time::Duration::from_secs(1));
        assert_eq!(session.state.notice().as_deref(), Some("터미널 연결이 끊어졌습니다. 1초 후 다시 연결합니다. (1/6)"));
        assert_eq!(session.state.lines, ["$ ls"], "the last screen stays while waiting");
        assert!(!session.state.needs_reconnect(), "no manual button while an attempt is pending");
        session.reconnecting();
        assert_eq!(session.state.notice().as_deref(), Some("터미널에 다시 연결하는 중입니다."));
        assert!(!session.state.needs_reconnect());
        session.apply_event(TermEvent::Attached { client: Some(3), term: None });
        assert_eq!(session.state.reconnect, None);
        assert_eq!(session.state.error, None);
        assert_eq!(session.state.notice(), None);
    }

    #[test]
    fn fr_l31_22_giving_up_offers_the_manual_reconnect() {
        let mut session = TerminalSession::new();
        session.fail("timeout");
        session.gave_up();
        assert!(session.state.needs_reconnect());
        assert_eq!(session.state.notice().as_deref(), Some("터미널에 다시 연결하지 못했습니다. 다시 연결을 눌러 주십시오."));
    }

    #[test]
    fn fr_l31_22_errors_show_a_fixed_sentence_never_the_transport_text() {
        let mut session = TerminalSession::new();
        session.fail("websocket: IO error: Connection refused (os error 61)");
        let notice = session.state.notice().unwrap();
        assert_eq!(notice, "터미널 연결에 문제가 생겼습니다.");
        assert!(!notice.contains("os error"));
    }

    #[test]
    fn fr_l31_22_an_unreadable_frame_is_named_on_a_connected_panel() {
        let mut session = TerminalSession::new();
        session.apply_event(TermEvent::Attached { client: Some(1), term: None });
        assert_eq!(session.state.notice(), None);
        session.frame_error();
        assert_eq!(session.state.notice().as_deref(), Some("서버가 보낸 터미널 데이터 일부를 읽지 못했습니다."));
        assert_eq!(session.state.status, TerminalStatus::Connected, "the stream keeps going");
        session.apply_event(TermEvent::Attached { client: Some(1), term: None });
        assert!(!session.state.frame_error);
    }

    #[test]
    fn fr_p4_banner_names_the_other_controller() {
        let mut session = TerminalSession::new();
        session.apply_event(TermEvent::Attached { client: Some(1), term: None });
        assert_eq!(session.state.banner(), None);
        session.apply_event(TermEvent::Control { controller: Some(client(2, "맥북")) });
        assert_eq!(session.state.banner().as_deref(), Some("다른 기기가 입력 중입니다: 맥북"));
        assert!(session.state.controlled_by_other());
        session.apply_event(TermEvent::Control { controller: Some(client(1, "me")) });
        assert_eq!(session.state.banner(), None);
        session.apply_event(TermEvent::Control { controller: None });
        session.apply_event(TermEvent::Refused { reason: TermRefusal::ReadOnly, controller: None });
        assert!(session.state.banner().unwrap().contains("읽기 전용"));
        session.apply_event(TermEvent::Output { data: b"x".to_vec() });
        assert_eq!(session.state.banner(), None);
    }

    #[test]
    fn fr_p4_exit_error_and_not_connected_texts() {
        let mut session = TerminalSession::new();
        assert_eq!(session.state.notice().as_deref(), Some("터미널이 연결되지 않았습니다."));
        session.connecting();
        assert_eq!(session.state.notice().as_deref(), Some("터미널에 연결하는 중입니다."));
        session.apply_event(TermEvent::Exit { code: Some(2), signal: None });
        assert_eq!(session.state.notice().as_deref(), Some("프로세스가 종료되었습니다. (종료 코드 2)"));
        session.apply_event(TermEvent::Exit { code: None, signal: Some(9) });
        assert_eq!(session.state.exit_text().as_deref(), Some("프로세스가 종료되었습니다. (시그널 9)"));
        session.apply_event(TermEvent::Error { message: "node unavailable".into() });
        assert_eq!(session.state.notice().as_deref(), Some("터미널 연결에 문제가 생겼습니다."));
        assert_eq!(session.state.status, TerminalStatus::Disconnected);
    }

    #[test]
    fn fr_p4_relay_events_deserialize() {
        let event: TermEvent = serde_json::from_str(
            r#"{"type":"refused","reason":"controlled","controller":{"client":3,"device":"d","attached_ms":1}}"#,
        ).unwrap();
        assert_eq!(event, TermEvent::Refused { reason: TermRefusal::Controlled, controller: Some(client(3, "d")) });
        let event: TermEvent = serde_json::from_str(r#"{"type":"error","message":"down"}"#).unwrap();
        assert_eq!(event, TermEvent::Error { message: "down".into() });
        let event: TermEvent = serde_json::from_str(r#"{"type":"attached"}"#).unwrap();
        assert_eq!(event, TermEvent::Attached { client: None, term: None });
    }

    #[test]
    fn fr_p4_input_frames_have_the_node_shapes() {
        assert_eq!(frame(input_frame(b"ls\r")), serde_json::json!({"type":"input","data":"bHMN"}));
        assert_eq!(
            frame(resize_frame(80, 24)),
            serde_json::json!({"type":"resize","size":{"rows":24,"cols":80}})
        );
        assert_eq!(frame(take_control_frame()), serde_json::json!({"type":"take_control"}));
        assert_eq!(frame(release_control_frame()), serde_json::json!({"type":"release_control"}));
        assert_eq!(frame(detach_frame()), serde_json::json!({"type":"detach"}));
        let hello = serde_json::to_value(TermHello {
            device: "mac".into(), kind: Some("ember-editor".into()), pid: None,
            size: Some(PtySize { rows: 24, cols: 80 }), active: true, read_only: false, snapshot: true,
        }).unwrap();
        assert_eq!(hello["kind"], "ember-editor");
        assert_eq!(hello["active"], true);
    }

    #[test]
    fn fr_p4_key_bytes_table() {
        let k = |key: &str, m: Modifiers| key_bytes(key, m);
        assert_eq!(k("Enter", Modifiers::NONE), Some(b"\r".to_vec()));
        assert_eq!(k("Backspace", Modifiers::NONE), Some(vec![0x7f]));
        assert_eq!(k("Tab", Modifiers::NONE), Some(b"\t".to_vec()));
        assert_eq!(k("Tab", Modifiers::SHIFT), Some(b"\x1b[Z".to_vec()));
        assert_eq!(k("Escape", Modifiers::NONE), Some(vec![0x1b]));
        assert_eq!(k("ArrowUp", Modifiers::NONE), Some(b"\x1b[A".to_vec()));
        assert_eq!(k("ArrowLeft", Modifiers::CTRL), Some(b"\x1b[1;5D".to_vec()));
        assert_eq!(k("Delete", Modifiers::NONE), Some(b"\x1b[3~".to_vec()));
        assert_eq!(k("F1", Modifiers::NONE), Some(b"\x1bOP".to_vec()));
        assert_eq!(k("F5", Modifiers::NONE), Some(b"\x1b[15~".to_vec()));
        assert_eq!(k("c", Modifiers::CTRL), Some(vec![3]));
        assert_eq!(k("D", Modifiers::CTRL), Some(vec![4]));
        assert_eq!(k("a", Modifiers::ALT), Some(vec![0x1b, b'a']));
        assert_eq!(k("a", Modifiers::NONE), Some(b"a".to_vec()));
        assert_eq!(k("한", Modifiers::NONE), Some("한".as_bytes().to_vec()));
        assert_eq!(k("Shift", Modifiers::NONE), None);
        assert_eq!(k("1", Modifiers::CTRL), None);
        assert_eq!(text_bytes("a\nb"), b"a\rb".to_vec());
    }

    fn peer_api() -> (ember_transport::mem::MemNetwork, ember_client::api::Api) {
        let net = ember_transport::mem::MemNetwork::new();
        let (device, server) = (net.transport(), net.transport());
        let api = ember_client::api::Api::over_transport(
            ember_transport::Dialer::new(device),
            ember_transport::PeerAddr::new(server.peer_id()),
        );
        (net, api)
    }

    #[tokio::test]
    async fn fr_n9_the_pane_reaches_a_peer_server_through_the_api_not_a_url() {
        let (_net, api) = peer_api();
        let reach = TermReach::select(api.base_url(), &api);
        assert!(matches!(reach, TermReach::Peer(_)), "{reach:?}");
        assert!(reach.is_peer());
        // Over the transport: paths below `/api` for the `Api`, never a URL built from `peer:<id>`.
        assert_eq!(
            list_target(&reach, "local", "/w", Some("agent"), true).unwrap(),
            TermTarget::Api("/computers/local/terms?project=%2Fw&origin=agent&running=true".into())
        );
        assert_eq!(
            attach_target(&reach, "c 1", "t/1").unwrap(),
            TermTarget::Api("/computers/c%201/terms/t%2F1/attach".into())
        );
    }

    #[test]
    fn fr_n9_the_pane_keeps_the_http_path_and_never_reads_peer_as_a_url() {
        let http = ember_client::api::Api::new("http://127.0.0.1:8740").unwrap();
        let reach = TermReach::select("http://127.0.0.1:8740", &http);
        assert!(matches!(&reach, TermReach::Http(u) if u == "http://127.0.0.1:8740"), "{reach:?}");
        assert!(!reach.is_peer());
        // Unchanged: the same URLs the HTTP path built before.
        assert_eq!(
            list_target(&reach, "local", "/w", Some("agent"), true).unwrap(),
            TermTarget::Url(terms_list_url("http://127.0.0.1:8740", "local", "/w", Some("agent"), true))
        );
        assert_eq!(
            attach_target(&reach, "local", "t1").unwrap(),
            TermTarget::Url(attach_url("http://127.0.0.1:8740", "local", "t1"))
        );
        // `peer:<id>` with no transport `Api` is refused with the device sentence, not dialed.
        let peerish = TermReach::Http("peer:abcd".into());
        for r in [list_target(&peerish, "local", "/w", None, false), attach_target(&peerish, "local", "t1")] {
            match r {
                Err(ClientError::Refused(t)) => assert_eq!(t, crate::device_link::UNREACHABLE),
                other => panic!("{other:?}"),
            }
        }
        // A remote plain http server is still refused as before (FR-L31.19).
        match attach_target(&TermReach::Http("http://192.0.2.1:8740".into()), "local", "t1") {
            Err(ClientError::Refused(t)) => assert_eq!(t, RELAY_REFUSED),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn fr_n9_scope_and_device_refusals_read_as_their_sentences() {
        use crate::device_link::{NOT_ALLOWED, UNREACHABLE};
        use crate::ui::device_scope::SCOPE_DENIED;
        let denied = br#"{"code":"scope_denied","error":"this device's scope (chat_only) does not allow this route"}"#;
        assert_eq!(status_refusal(403, denied), Some(SCOPE_DENIED));
        assert_eq!(status_refusal(403, br#"{"error":"x"}"#), None);
        assert_eq!(status_refusal(502, b"bad gateway"), None);
        for t in [NOT_ALLOWED, UNREACHABLE, SCOPE_DENIED] {
            assert_eq!(known_refusal(t), Some(t));
        }
        assert_eq!(known_refusal("websocket: connection reset"), None);
        // The pane shows the sentence, not the generic failure, and does not offer a retry loop.
        let mut s = TerminalSession::new();
        s.fail(ClientError::Refused(SCOPE_DENIED).to_string());
        assert_eq!(crate::ui::term_pane::view_from_state(&s.state), crate::ui::term_pane::TermView::Failed(SCOPE_DENIED));
        s.fail("websocket: connection reset");
        assert_eq!(
            crate::ui::term_pane::view_from_state(&s.state),
            crate::ui::term_pane::TermView::Failed(crate::ui::term_pane::FAILED)
        );
    }
}
