//! Workbench-facing terminal state and persistent node attachment.

#[path = "terminal_screen.rs"]
mod screen;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("websocket: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("protocol: {0}")]
    Protocol(String),
}

/// HTTP endpoint and bearer token for a node.
#[derive(Clone, Debug)]
pub struct NodeClient {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl NodeClient {
    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Result<Self, ClientError> {
        let base = base.into().trim_end_matches('/').to_owned();
        if !base.starts_with("http://") {
            return Err(ClientError::Protocol("only plain HTTP node endpoints are supported".into()));
        }
        Ok(Self { base, token: token.into(), http: reqwest::Client::new() })
    }

    async fn term_create(&self, request: &TermCreateRequest) -> Result<TermCreateResponse, ClientError> {
        Ok(self.http.post(format!("{}/terms", self.base)).bearer_auth(&self.token)
            .json(request).send().await?.error_for_status()?.json().await?)
    }

    async fn term_attach(&self, id: &str, hello: &TermHello) -> Result<TermAttachment, ClientError> {
        let url = format!("ws://{}/terms/{id}/attach", self.base.trim_start_matches("http://"));
        let mut request = url.into_client_request()?;
        let auth = format!("Bearer {}", self.token).parse()
            .map_err(|e: tokio_tungstenite::tungstenite::http::header::InvalidHeaderValue| ClientError::Protocol(e.to_string()))?;
        request.headers_mut().insert("authorization", auth);
        let (mut ws, _) = tokio_tungstenite::connect_async(request).await?;
        ws.send(Message::Text(serde_json::to_string(hello).map_err(|e| ClientError::Protocol(e.to_string()))?.into())).await?;
        let mut attachment = TermAttachment { ws, term: TermInfo { id: String::new(), size: PtySize { rows: 0, cols: 0 } } };
        match attachment.recv().await? {
            Some(TermEvent::Attached { term }) => { attachment.term = term; Ok(attachment) }
            Some(TermEvent::Error { message }) => Err(ClientError::Protocol(message)),
            other => Err(ClientError::Protocol(format!("expected attached, got {other:?}"))),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct PtySize { rows: u16, cols: u16 }
#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
enum TermOrigin { IdeEmber }
#[derive(Serialize)]
struct TermCreateRequest {
    program: Option<()>, cwd: std::path::PathBuf, env: std::collections::BTreeMap<String, String>,
    env_clear: bool, size: Option<PtySize>, origin: TermOrigin, project: Option<String>,
    title: Option<String>, key: Option<String>, tags: std::collections::BTreeMap<String, String>,
}
#[derive(Deserialize)]
struct TermCreateResponse { term: TermInfo }
#[derive(Debug, Deserialize)]
struct TermInfo { id: String, size: PtySize }
#[derive(Serialize)]
struct TermHello { device: String, kind: Option<String>, pid: Option<u32>, size: Option<PtySize>, active: bool, read_only: bool, snapshot: bool }
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
enum TermEvent {
    Attached { term: TermInfo },
    Snapshot { size: PtySize, #[serde(deserialize_with = "decode_bytes")] data: Vec<u8> },
    Output { #[serde(deserialize_with = "decode_bytes")] data: Vec<u8> },
    Resized { size: PtySize },
    Exit, Error { message: String },
    #[serde(other)] Other,
}
fn decode_bytes<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
    use base64::Engine;
    let text = String::deserialize(deserializer)?;
    base64::engine::general_purpose::STANDARD.decode(text).map_err(serde::de::Error::custom)
}
struct TermAttachment {
    ws: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    term: TermInfo,
}
impl TermAttachment {
    async fn recv(&mut self) -> Result<Option<TermEvent>, ClientError> {
        loop {
            match self.ws.next().await {
                None | Some(Ok(Message::Close(_))) | Some(Err(tokio_tungstenite::tungstenite::Error::ConnectionClosed)) => return Ok(None),
                Some(Ok(Message::Text(text))) => return serde_json::from_str(&text).map(Some).map_err(|e| ClientError::Protocol(e.to_string())),
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(e.into()),
            }
        }
    }
}

/// Connection state shown by the terminal panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TerminalStatus {
    #[default]
    Disconnected,
    Connecting,
    Connected,
}

/// A complete decoded screen, suitable for storing in a UI signal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalState {
    pub status: TerminalStatus,
    pub lines: Vec<String>,
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

/// Select an existing terminal or create a shell in a folder on the selected computer.
#[derive(Clone, Debug)]
pub enum TerminalTarget {
    Session(String),
    Folder(std::path::PathBuf),
}

/// Attach on the supplied computer and drive the workbench signal until the stream closes.
/// The caller owns the task and can cancel it to detach without ending the terminal.
pub async fn start(
    computer: NodeClient,
    target: TerminalTarget,
    state: dioxus_compose::prelude::SyncSignal<TerminalState>,
) -> Result<(), ClientError> {
    let (tx, rx) = tokio::sync::mpsc::channel(32);
    let worker = async move {
        let _ = tx.send(TerminalUpdate::Connecting).await;
        let id = match target {
            TerminalTarget::Session(id) => id,
            TerminalTarget::Folder(folder) => {
                computer.term_create(&TermCreateRequest {
                    program: None,
                    cwd: folder.clone(),
                    env: Default::default(),
                    env_clear: false,
                    size: None,
                    origin: TermOrigin::IdeEmber,
                    project: Some(folder.to_string_lossy().into_owned()),
                    title: None,
                    key: None,
                    tags: Default::default(),
                }).await?.term.id
            }
        };
        let hello = TermHello {
            device: "ember-editor".into(), kind: Some("editor".into()),
            pid: Some(std::process::id()), size: None, active: true,
            read_only: false, snapshot: true,
        };
        let mut attachment = computer.term_attach(&id, &hello).await?;
        let size = attachment.term.size;
        let mut screen = screen::Screen::new(size.cols, size.rows);
        loop {
            match attachment.recv().await? {
                Some(TermEvent::Snapshot { size, data }) => {
                    screen = screen::Screen::new(size.cols, size.rows);
                    screen.feed(&data);
                }
                Some(TermEvent::Output { data }) => screen.feed(&data),
                Some(TermEvent::Resized { size }) => screen.resize(size.cols, size.rows),
                Some(TermEvent::Exit) | None => break,
                Some(TermEvent::Error { message }) => return Err(ClientError::Protocol(message)),
                Some(_) => continue,
            }
            if tx.send(TerminalUpdate::Lines(screen_lines(&screen))).await.is_err() {
                break;
            }
        }
        Ok(())
    };
    let (result, ()) = tokio::join!(worker, feed(rx, state));
    result
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
    fn terminal_wire_messages_match_node_protocol() {
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
        assert!(matches!(serde_json::from_str::<TermEvent>(r#"{"type":"exit","code":0,"signal":null}"#).unwrap(), TermEvent::Exit));
        assert!(matches!(serde_json::from_str::<TermEvent>(r#"{"type":"clients","clients":[]}"#).unwrap(), TermEvent::Other));
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

}

