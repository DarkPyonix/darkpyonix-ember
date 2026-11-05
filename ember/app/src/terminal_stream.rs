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
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("websocket: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("protocol: {0}")]
    Protocol(String),
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

#[derive(Serialize)]
pub struct TermCreateRequest {
    pub program: Option<()>,
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
    /// Attach or relay failure text.
    pub error: Option<String>,
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

    /// Text for an attach or relay failure.
    pub fn error_text(&self) -> Option<String> {
        self.error.as_ref().map(|e| format!("터미널 오류: {e}"))
    }

    /// The one line to show instead of (or under) the screen: a failure, else the exit status,
    /// else the connection state.
    pub fn notice(&self) -> Option<String> {
        self.error_text().or_else(|| self.exit_text()).or_else(|| self.message().map(str::to_owned))
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

    /// Apply one frame from the relay.
    pub fn apply_event(&mut self, event: TermEvent) {
        match event {
            TermEvent::Attached { client, term } => {
                self.state.status = TerminalStatus::Connected;
                self.state.error = None;
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
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let shared = Arc::new(Shared {
        session: Mutex::new(TerminalSession::new()),
        revision: AtomicU64::new(0),
        size: Mutex::new((80, 24)),
        tx,
        on_change,
    });
    shared.update(|s| s.connecting());
    let task_shared = Arc::downgrade(&shared);
    let (base, computer, folder, project) =
        (base_url.to_owned(), computer_id.to_owned(), folder.to_owned(), project.to_owned());
    crate::services::services().rt.spawn(async move {
        if let Err(e) = run(&task_shared, &base, &computer, &folder, &project, rx).await {
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

async fn run(
    shared: &std::sync::Weak<Shared>,
    base: &str,
    computer: &str,
    folder: &str,
    project: &str,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<String>,
) -> Result<(), ClientError> {
    let live = || shared.upgrade().ok_or_else(|| ClientError::Protocol("terminal closed".into()));
    let (cols, rows) = {
        let strong = live()?;
        let size = *strong.size.lock().unwrap_or_else(|e| e.into_inner());
        size
    };
    let size = PtySize { rows, cols };
    let request = TermCreateRequest {
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
    let created: TermCreateResponse = reqwest::Client::new()
        .post(terms_url(base, computer))
        .json(&request)
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
    let (mut ws, _) = tokio_tungstenite::connect_async(attach_url(base, computer, &created.term.id)).await?;
    let hello = serde_json::to_string(&hello).map_err(|e| ClientError::Protocol(e.to_string()))?;
    ws.send(Message::Text(hello.into())).await?;
    loop {
        tokio::select! {
            incoming = ws.next() => match incoming {
                None | Some(Ok(Message::Close(_))) => break,
                Some(Ok(Message::Text(text))) => {
                    let Ok(event) = serde_json::from_str::<TermEvent>(&text) else { continue };
                    let last = matches!(event, TermEvent::Exit { .. } | TermEvent::Error { .. });
                    live()?.update(|s| s.apply_event(event));
                    if last {
                        break;
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(tokio_tungstenite::tungstenite::Error::ConnectionClosed)) => break,
                Some(Err(e)) => return Err(e.into()),
            },
            outgoing = rx.recv() => match outgoing {
                Some(frame) => ws.send(Message::Text(frame.into())).await?,
                None => {
                    let _ = ws.close(None).await;
                    break;
                }
            },
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(session.state.notice().as_deref(), Some("터미널 오류: node unavailable"));
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

}
