//! Typed client for the ember node API, used by ember server.
//!
//! ```no_run
//! # async fn demo() -> Result<(), ember_node::client::ClientError> {
//! use ember_node::client::NodeClient;
//! let node = NodeClient::new("http://127.0.0.1:8741", "secret")?;
//! let file = node.read_file("/home/me/project/README.md").await?;
//! println!("{} bytes, sha256 {:?}", file.size, file.sha256);
//! # Ok(()) }
//! ```

use std::path::{Path, PathBuf};

use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::proto::*;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The daemon answered with an error body.
    #[error("{status}: {} ({:?})", .body.error, .body.code)]
    Api { status: u16, body: ErrorBody },
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("websocket: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("bad url: {0}")]
    Url(String),
}

impl ClientError {
    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            ClientError::Api { body, .. } => Some(body.code),
            _ => None,
        }
    }

    /// For a failed write precondition: `Some(current hash or None if absent)`.
    pub fn precondition_actual(&self) -> Option<Option<&str>> {
        match self {
            ClientError::Api { body, .. } if body.code == ErrorCode::PreconditionFailed => {
                Some(body.actual_sha256.as_deref())
            }
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, ClientError>;

#[derive(Clone)]
pub struct NodeClient {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl NodeClient {
    /// `base` is `http://host:port` (no trailing path).
    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Result<Self> {
        let base = base.into().trim_end_matches('/').to_string();
        if !base.starts_with("http://") && !base.starts_with("https://") {
            return Err(ClientError::Url(base));
        }
        Ok(Self { base, token: token.into(), http: reqwest::Client::new() })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    async fn decode<T: DeserializeOwned>(resp: reqwest::Response) -> Result<T> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp.json().await?);
        }
        Err(Self::api_error(status, resp).await)
    }

    async fn api_error(status: StatusCode, resp: reqwest::Response) -> ClientError {
        let text = resp.text().await.unwrap_or_default();
        let body = serde_json::from_str(&text).unwrap_or(ErrorBody {
            code: if status == StatusCode::UNAUTHORIZED { ErrorCode::Unauthorized } else { ErrorCode::Internal },
            error: text,
            actual_sha256: None,
        });
        ClientError::Api { status: status.as_u16(), body }
    }

    async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        let resp = self.http.post(self.url(path)).bearer_auth(&self.token).json(body).send().await?;
        Self::decode(resp).await
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self.http.get(self.url(path)).bearer_auth(&self.token).send().await?;
        Self::decode(resp).await
    }

    async fn no_content(&self, req: reqwest::RequestBuilder) -> Result<()> {
        let resp = req.bearer_auth(&self.token).send().await?;
        let status = resp.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(Self::api_error(status, resp).await)
        }
    }

    async fn websocket(&self, path: &str) -> Result<Ws> {
        let url = format!("ws{}{path}", self.base.strip_prefix("http").unwrap_or(&self.base));
        let mut req = url.into_client_request()?;
        let auth = HeaderValue::from_str(&format!("Bearer {}", self.token))
            .map_err(|e| ClientError::Protocol(e.to_string()))?;
        req.headers_mut().insert("authorization", auth);
        match tokio_tungstenite::connect_async(req).await {
            Ok((ws, _)) => Ok(ws),
            Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
                let status = resp.status().as_u16();
                let body = resp
                    .body()
                    .as_deref()
                    .and_then(|b| serde_json::from_slice(b).ok())
                    .unwrap_or(ErrorBody { code: ErrorCode::Unauthorized, error: format!("upgrade refused: {status}"), actual_sha256: None });
                Err(ClientError::Api { status, body })
            }
            Err(e) => Err(e.into()),
        }
    }

    // -----------------------------------------------------------------------------------------

    /// Unauthenticated liveness and protocol version.
    pub async fn health(&self) -> Result<Health> {
        Ok(self.http.get(self.url("/v1/health")).send().await?.json().await?)
    }

    pub async fn env(&self) -> Result<EnvInfo> {
        self.get("/v1/env").await
    }

    pub async fn stat(&self, path: impl AsRef<Path>) -> Result<Stat> {
        self.post("/v1/fs/stat", &PathRequest { path: path.as_ref().into() }).await
    }

    /// Read a whole file (up to [`MAX_READ`]) with its hash.
    pub async fn read_file(&self, path: impl AsRef<Path>) -> Result<ReadResponse> {
        self.read(&ReadRequest { path: path.as_ref().into(), offset: 0, len: None, hash: true }).await
    }

    pub async fn read(&self, req: &ReadRequest) -> Result<ReadResponse> {
        self.post("/v1/fs/read", req).await
    }

    pub async fn write(&self, req: &WriteRequest) -> Result<WriteResponse> {
        self.post("/v1/fs/write", req).await
    }

    /// Write `data`, optionally only if the file is unchanged since it was read with `expect`.
    pub async fn write_file(
        &self,
        path: impl AsRef<Path>,
        data: impl Into<Vec<u8>>,
        expect: Option<Expect>,
    ) -> Result<WriteResponse> {
        self.write(&WriteRequest { path: path.as_ref().into(), data: data.into(), expect, create_parents: false })
            .await
    }

    pub async fn list(&self, path: impl AsRef<Path>) -> Result<ListResponse> {
        self.post("/v1/fs/list", &PathRequest { path: path.as_ref().into() }).await
    }

    pub async fn glob(&self, req: &GlobRequest) -> Result<GlobResponse> {
        self.post("/v1/fs/glob", req).await
    }

    pub async fn grep(&self, req: &GrepRequest) -> Result<GrepResponse> {
        self.post("/v1/fs/grep", req).await
    }

    /// Start a command and return its live session. Dropping the session kills the command.
    pub async fn exec(&self, req: &ExecRequest) -> Result<ExecSession> {
        let mut ws = self.websocket("/v1/exec").await?;
        ws.send(Message::Text(serde_json::to_string(req).expect("serialisable").into())).await?;
        let (sink, stream) = ws.split();
        let mut session = ExecSession { tx: ExecSender { sink }, rx: ExecReceiver { stream }, pid: 0 };
        match session.rx.recv().await? {
            Some(ExecEvent::Started { pid }) => {
                session.pid = pid;
                Ok(session)
            }
            Some(ExecEvent::Error { message }) => Err(ClientError::Api {
                status: 400,
                body: ErrorBody { code: ErrorCode::BadRequest, error: message, actual_sha256: None },
            }),
            other => Err(ClientError::Protocol(format!("expected started, got {other:?}"))),
        }
    }

    /// Run a command to completion, collecting its output.
    pub async fn run(&self, command: CommandSpec) -> Result<ExecOutput> {
        let mut s = self.exec(&ExecRequest { command, pty: None }).await?;
        s.tx.send(ExecInput::CloseStdin).await?;
        let mut out = ExecOutput::default();
        while let Some(ev) = s.rx.recv().await? {
            match ev {
                ExecEvent::Stdout { data } => out.stdout.extend(data),
                ExecEvent::Stderr { data } => out.stderr.extend(data),
                ExecEvent::Exit { code, signal } => {
                    out.code = code;
                    out.signal = signal;
                    return Ok(out);
                }
                ExecEvent::Error { message } => return Err(ClientError::Protocol(message)),
                ExecEvent::Started { .. } => {}
            }
        }
        Err(ClientError::Protocol("connection closed before exit".into()))
    }

    pub async fn start_job(&self, req: &JobRequest) -> Result<JobInfo> {
        self.post("/v1/jobs", req).await
    }

    pub async fn jobs(&self) -> Result<Vec<JobInfo>> {
        self.get("/v1/jobs").await
    }

    pub async fn job(&self, id: &str, tail_bytes: Option<usize>) -> Result<JobDetail> {
        match tail_bytes {
            Some(n) => self.get(&format!("/v1/jobs/{id}?tail={n}")).await,
            None => self.get(&format!("/v1/jobs/{id}")).await,
        }
    }

    pub async fn kill_job(&self, id: &str, signal: Option<i32>) -> Result<()> {
        self.no_content(self.http.post(self.url(&format!("/v1/jobs/{id}/kill"))).json(&KillRequest { signal }))
            .await
    }

    pub async fn remove_job(&self, id: &str) -> Result<()> {
        self.no_content(self.http.delete(self.url(&format!("/v1/jobs/{id}")))).await
    }

    /// Subscribe to node events with `seq > after` (0 for everything still in history).
    pub async fn events(&self, after: u64) -> Result<EventStream> {
        Ok(EventStream { ws: self.websocket(&format!("/v1/events?after={after}")).await? })
    }

    // -----------------------------------------------------------------------------------------
    // Persistent terminal sessions (`/v1/terms`)

    /// Start a persistent session, or (with `key`) return the running one with that key.
    pub async fn term_create(&self, req: &TermCreateRequest) -> Result<TermCreateResponse> {
        self.post("/v1/terms", req).await
    }

    /// Sessions on this computer, filtered by `q`, oldest first.
    pub async fn terms(&self, q: &TermListQuery) -> Result<Vec<TermInfo>> {
        let resp = self.http.get(self.url("/v1/terms")).query(q).bearer_auth(&self.token).send().await?;
        Self::decode(resp).await
    }

    pub async fn term(&self, id: &str) -> Result<TermInfo> {
        self.get(&format!("/v1/terms/{id}")).await
    }

    /// The session's current screen (escape sequences and plain text), without attaching.
    pub async fn term_snapshot(&self, id: &str) -> Result<TermSnapshot> {
        self.get(&format!("/v1/terms/{id}/snapshot")).await
    }

    /// Take or release control on behalf of an attached client (e.g. from a UI that is not
    /// the attached process itself).
    pub async fn term_control(&self, id: &str, client: u64, take: bool) -> Result<()> {
        self.no_content(
            self.http.post(self.url(&format!("/v1/terms/{id}/control"))).json(&TermControlRequest { client, take }),
        )
        .await
    }

    /// Signal a session (default SIGHUP, then SIGKILL after a grace period).
    pub async fn term_kill(&self, id: &str, signal: Option<i32>) -> Result<()> {
        self.no_content(self.http.post(self.url(&format!("/v1/terms/{id}/kill"))).json(&KillRequest { signal }))
            .await
    }

    /// Forget a finished session.
    pub async fn term_remove(&self, id: &str) -> Result<()> {
        self.no_content(self.http.delete(self.url(&format!("/v1/terms/{id}")))).await
    }

    /// Attach to a session. The first events after [`TermAttachment::client`] are the snapshot
    /// (if `hello.snapshot`) and then the live stream. Dropping the attachment detaches; it
    /// never ends the session.
    pub async fn term_attach(&self, id: &str, hello: &TermHello) -> Result<TermAttachment> {
        let mut ws = self.websocket(&format!("/v1/terms/{id}/attach")).await?;
        ws.send(Message::Text(serde_json::to_string(hello).expect("serialisable").into())).await?;
        let (sink, stream) = ws.split();
        let mut rx = TermReceiver { stream };
        match rx.recv().await? {
            Some(TermEvent::Attached { client, term }) => Ok(TermAttachment { tx: TermSender { sink }, rx, client, term }),
            Some(TermEvent::Error { message }) => Err(ClientError::Api {
                status: 404,
                body: ErrorBody { code: ErrorCode::NotFound, error: message, actual_sha256: None },
            }),
            other => Err(ClientError::Protocol(format!("expected attached, got {other:?}"))),
        }
    }
}

/// A live attachment to a persistent terminal session.
pub struct TermAttachment {
    pub tx: TermSender,
    pub rx: TermReceiver,
    /// This client's id within the session.
    pub client: u64,
    /// The session as of the attach.
    pub term: TermInfo,
}

impl TermAttachment {
    pub async fn send(&mut self, input: TermInput) -> Result<()> {
        self.tx.send(input).await
    }

    /// Next event; `None` once the daemon closed the stream (after `exit` or `error`).
    pub async fn recv(&mut self) -> Result<Option<TermEvent>> {
        self.rx.recv().await
    }

    pub fn into_split(self) -> (TermSender, TermReceiver) {
        (self.tx, self.rx)
    }
}

pub struct TermSender {
    sink: SplitSink<Ws, Message>,
}

impl TermSender {
    pub async fn send(&mut self, input: TermInput) -> Result<()> {
        self.sink.send(Message::Text(serde_json::to_string(&input).expect("serialisable").into())).await?;
        Ok(())
    }

    pub async fn input(&mut self, data: impl Into<Vec<u8>>) -> Result<()> {
        self.send(TermInput::Input { data: data.into() }).await
    }

    /// Detach cleanly (the session keeps running).
    pub async fn detach(mut self) -> Result<()> {
        let _ = self.send(TermInput::Detach).await;
        let _ = self.sink.close().await;
        Ok(())
    }
}

pub struct TermReceiver {
    stream: SplitStream<Ws>,
}

impl TermReceiver {
    pub async fn recv(&mut self) -> Result<Option<TermEvent>> {
        next_json(&mut self.stream).await
    }
}

/// Output of [`NodeClient::run`].
#[derive(Debug, Default, Clone)]
pub struct ExecOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

/// A live command. Use [`ExecSession::into_split`] to send and receive from separate tasks.
pub struct ExecSession {
    pub tx: ExecSender,
    pub rx: ExecReceiver,
    pub pid: u32,
}

impl ExecSession {
    pub async fn send(&mut self, input: ExecInput) -> Result<()> {
        self.tx.send(input).await
    }

    /// Next event; `None` once the daemon closed the stream (after `Exit`).
    pub async fn recv(&mut self) -> Result<Option<ExecEvent>> {
        self.rx.recv().await
    }

    pub fn into_split(self) -> (ExecSender, ExecReceiver) {
        (self.tx, self.rx)
    }
}

pub struct ExecSender {
    sink: SplitSink<Ws, Message>,
}

impl ExecSender {
    pub async fn send(&mut self, input: ExecInput) -> Result<()> {
        self.sink.send(Message::Text(serde_json::to_string(&input).expect("serialisable").into())).await?;
        Ok(())
    }

    pub async fn stdin(&mut self, data: impl Into<Vec<u8>>) -> Result<()> {
        self.send(ExecInput::Stdin { data: data.into() }).await
    }
}

pub struct ExecReceiver {
    stream: SplitStream<Ws>,
}

impl ExecReceiver {
    pub async fn recv(&mut self) -> Result<Option<ExecEvent>> {
        next_json(&mut self.stream).await
    }
}

async fn next_json<T: DeserializeOwned, S>(stream: &mut S) -> Result<Option<T>>
where
    S: futures::Stream<Item = std::result::Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        match stream.next().await {
            None | Some(Ok(Message::Close(_))) => return Ok(None),
            Some(Ok(Message::Text(t))) => {
                return serde_json::from_str(&t).map(Some).map_err(|e| ClientError::Protocol(e.to_string()))
            }
            Some(Ok(_)) => continue,
            Some(Err(tokio_tungstenite::tungstenite::Error::ConnectionClosed)) => return Ok(None),
            Some(Err(e)) => return Err(e.into()),
        }
    }
}

/// Node → server notifications. Reconnect with the last seen `seq` to resume.
pub struct EventStream {
    ws: Ws,
}

impl EventStream {
    pub async fn recv(&mut self) -> Result<Option<NodeEvent>> {
        next_json(&mut self.ws).await
    }
}

/// Helper for building a [`CommandSpec`] that runs `sh -c <command>` in `cwd`.
pub fn shell(command: impl Into<String>, cwd: impl Into<PathBuf>) -> CommandSpec {
    CommandSpec {
        program: Program::Shell(command.into()),
        cwd: cwd.into(),
        env: Default::default(),
        env_clear: false,
    }
}
