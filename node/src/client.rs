//! Typed client for the ember node API, used by ember server.
//!
//! A [`NodeClient`] reaches the node one of two ways, behind the same API:
//!
//! - **HTTP** ([`NodeClient::new`]): `http://host:port` over TCP.
//! - **Transport** ([`NodeClient::over_transport`]): the peer-to-peer transport (SPEC `FR-N1`,
//!   `FR-N5`), dialing the node by its [`PeerAddr`] for service [`NODE_SERVICE`]. Each HTTP
//!   request runs on a fresh stream of one cached connection ([`Dialer`]); each WebSocket
//!   (exec, events, exec-server, terminal attach) gets its own stream.
//!
//! ```no_run
//! # async fn demo() -> Result<(), ember_node::client::ClientError> {
//! use ember_node::client::NodeClient;
//! let node = NodeClient::new("http://127.0.0.1:8741", "secret")?;
//! let file = node.read_file("/home/me/project/README.md").await?;
//! println!("{} bytes, sha256 {:?}", file.size, file.sha256);
//! # Ok(()) }
//! ```

use std::fmt;
use std::path::{Path, PathBuf};

use ember_transport::{Dialer, PeerAddr};
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use http::{header, Method, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use crate::proto::*;

/// Transport service name of the node API (`FR-N5`). Versioned with the API's major version.
pub const NODE_SERVICE: &str = "ember-node/1";

/// Host name used in requests over the transport (the stream already names the peer).
const PEER_HOST: &str = "ember-node";

/// Any byte stream a node WebSocket can run on (TCP or a transport stream).
pub trait NodeStream: AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static {}
impl<T: AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static> NodeStream for T {}

/// The stream under every node WebSocket, whichever way the node is reached.
pub type NodeIo = Box<dyn NodeStream>;

type Ws = WebSocketStream<NodeIo>;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The daemon answered with an error body.
    #[error("{status}: {} ({:?})", .body.error, .body.code)]
    Api { status: u16, body: ErrorBody },
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    /// The peer-to-peer transport failed (dial, stream, or HTTP over a stream).
    #[error("transport: {0}")]
    Transport(String),
    #[error("websocket: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("bad url: {0}")]
    Url(String),
}

impl ClientError {
    /// The portable errno name of a failed file operation (`ENOENT`, `ENOTEMPTY`, …).
    pub fn errno(&self) -> Option<&str> {
        match self {
            ClientError::Api { body, .. } => body.errno.as_deref(),
            _ => None,
        }
    }

    /// True when the request never got an answer from the daemon (connection refused, reset,
    /// timed out): the node is unreachable, as opposed to having refused the operation.
    pub fn is_transport(&self) -> bool {
        matches!(self, ClientError::Http(_) | ClientError::WebSocket(_))
    }

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

fn transport_err(e: impl fmt::Display) -> ClientError {
    ClientError::Transport(e.to_string())
}

pub type Result<T> = std::result::Result<T, ClientError>;

/// How the node is reached.
#[derive(Clone)]
enum Reach {
    /// `http://host:port`, no trailing path.
    Http { base: String, http: reqwest::Client },
    /// A peer on the transport.
    Peer { dialer: Dialer, addr: PeerAddr },
}

/// A status and a fully read body.
struct Raw {
    status: StatusCode,
    body: Bytes,
}

#[derive(Clone)]
pub struct NodeClient {
    reach: Reach,
    token: String,
}

impl fmt::Debug for NodeClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeClient({self})")
    }
}

/// Where the client points: the base URL, or `peer:<id>`.
impl fmt::Display for NodeClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.reach {
            Reach::Http { base, .. } => f.write_str(base),
            Reach::Peer { addr, .. } => write!(f, "peer:{}", addr.peer),
        }
    }
}

impl NodeClient {
    /// `base` is `http://host:port` (no trailing path).
    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Result<Self> {
        let base = base.into().trim_end_matches('/').to_string();
        if !base.starts_with("http://") && !base.starts_with("https://") {
            return Err(ClientError::Url(base));
        }
        Ok(Self { reach: Reach::Http { base, http: reqwest::Client::new() }, token: token.into() })
    }

    /// Like [`NodeClient::new`], but every HTTP request fails after `timeout` (and connecting
    /// after `min(timeout, 5s)`) instead of waiting forever: for callers that must not hang
    /// when the node goes away, such as the project mount. WebSockets are not affected.
    pub fn with_timeout(base: impl Into<String>, token: impl Into<String>, timeout: std::time::Duration) -> Result<Self> {
        let mut c = Self::new(base, token)?;
        if let Reach::Http { http, .. } = &mut c.reach {
            *http = reqwest::Client::builder()
                .timeout(timeout)
                .connect_timeout(timeout.min(std::time::Duration::from_secs(5)))
                .build()?;
        }
        Ok(c)
    }

    /// A client that reaches the node over the transport: `addr` is the node's identity (plus
    /// optional address hints), dialed through `dialer` for [`NODE_SERVICE`]. Clients sharing a
    /// dialer share its connection to the node. The bearer token is still sent.
    pub fn over_transport(dialer: Dialer, addr: impl Into<PeerAddr>, token: impl Into<String>) -> Self {
        Self { reach: Reach::Peer { dialer, addr: addr.into() }, token: token.into() }
    }

    /// The node's transport address, when reached over the transport.
    pub fn peer(&self) -> Option<&PeerAddr> {
        match &self.reach {
            Reach::Peer { addr, .. } => Some(addr),
            Reach::Http { .. } => None,
        }
    }

    /// The node's base URL, when reached over HTTP.
    pub fn base_url(&self) -> Option<&str> {
        match &self.reach {
            Reach::Http { base, .. } => Some(base),
            Reach::Peer { .. } => None,
        }
    }

    /// One request/response. `json` is sent as an `application/json` body.
    async fn send(&self, method: Method, path: &str, json: Option<Vec<u8>>, auth: bool) -> Result<Raw> {
        match &self.reach {
            Reach::Http { base, http } => {
                let mut req = http.request(method, format!("{base}{path}"));
                if auth {
                    req = req.bearer_auth(&self.token);
                }
                if let Some(body) = json {
                    req = req.header(header::CONTENT_TYPE, "application/json").body(body);
                }
                let resp = req.send().await?;
                let status = resp.status();
                Ok(Raw { status, body: resp.bytes().await? })
            }
            Reach::Peer { dialer, addr } => {
                let stream = dialer.open_bi(addr, NODE_SERVICE).await.map_err(transport_err)?;
                let mut sender = ember_transport::http::http1_handshake::<Full<Bytes>>(stream)
                    .await
                    .map_err(transport_err)?;
                let mut req = hyper::Request::builder()
                    .method(method)
                    .uri(format!("http://{PEER_HOST}{path}"))
                    .header(header::HOST, PEER_HOST);
                if auth {
                    req = req.header(header::AUTHORIZATION, format!("Bearer {}", self.token));
                }
                let body = match json {
                    Some(b) => {
                        req = req.header(header::CONTENT_TYPE, "application/json");
                        Full::new(Bytes::from(b))
                    }
                    None => Full::default(),
                };
                let req = req.body(body).map_err(|e| ClientError::Protocol(e.to_string()))?;
                let resp = sender.send_request(req).await.map_err(transport_err)?;
                let status = resp.status();
                let body = resp.into_body().collect().await.map_err(transport_err)?.to_bytes();
                Ok(Raw { status, body })
            }
        }
    }

    fn decode<T: DeserializeOwned>(raw: Raw) -> Result<T> {
        if raw.status.is_success() {
            return serde_json::from_slice(&raw.body).map_err(|e| ClientError::Protocol(e.to_string()));
        }
        Err(Self::api_error(raw))
    }

    fn api_error(raw: Raw) -> ClientError {
        let text = String::from_utf8_lossy(&raw.body).into_owned();
        let body = serde_json::from_str(&text).unwrap_or(ErrorBody {
            code: if raw.status == StatusCode::UNAUTHORIZED { ErrorCode::Unauthorized } else { ErrorCode::Internal },
            error: text,
            actual_sha256: None,
            errno: None,
        });
        ClientError::Api { status: raw.status.as_u16(), body }
    }

    fn json<B: Serialize>(body: &B) -> Vec<u8> {
        serde_json::to_vec(body).expect("serialisable")
    }

    async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        Self::decode(self.send(Method::POST, path, Some(Self::json(body)), true).await?)
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        Self::decode(self.send(Method::GET, path, None, true).await?)
    }

    async fn no_content(&self, method: Method, path: &str, json: Option<Vec<u8>>) -> Result<()> {
        let raw = self.send(method, path, json, true).await?;
        if raw.status.is_success() {
            Ok(())
        } else {
            Err(Self::api_error(raw))
        }
    }

    pub(crate) async fn websocket(&self, path: &str) -> Result<Ws> {
        let url = match &self.reach {
            Reach::Http { base, .. } => format!("ws{}{path}", base.strip_prefix("http").unwrap_or(base)),
            Reach::Peer { .. } => format!("ws://{PEER_HOST}{path}"),
        };
        let mut req = url.into_client_request()?;
        let auth = HeaderValue::from_str(&format!("Bearer {}", self.token))
            .map_err(|e| ClientError::Protocol(e.to_string()))?;
        req.headers_mut().insert("authorization", auth);
        let io: NodeIo = match &self.reach {
            Reach::Http { .. } => {
                let uri = req.uri();
                if uri.scheme_str() != Some("ws") {
                    return Err(ClientError::Url(format!("{uri}: only plain ws:// is supported over TCP")));
                }
                let host = uri.host().unwrap_or_default().trim_start_matches('[').trim_end_matches(']').to_string();
                let port = uri.port_u16().unwrap_or(80);
                let tcp = tokio::net::TcpStream::connect((host.as_str(), port)).await?;
                let _ = tcp.set_nodelay(true);
                Box::new(tcp)
            }
            Reach::Peer { dialer, addr } => {
                Box::new(dialer.open_bi(addr, NODE_SERVICE).await.map_err(transport_err)?)
            }
        };
        match tokio_tungstenite::client_async(req, io).await {
            Ok((ws, _)) => Ok(ws),
            Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
                let status = resp.status().as_u16();
                let body = resp
                    .body()
                    .as_deref()
                    .and_then(|b| serde_json::from_slice(b).ok())
                    .unwrap_or(ErrorBody { code: ErrorCode::Unauthorized, error: format!("upgrade refused: {status}"), actual_sha256: None, errno: None });
                Err(ClientError::Api { status, body })
            }
            Err(e) => Err(e.into()),
        }
    }

    // -----------------------------------------------------------------------------------------

    /// Unauthenticated liveness and protocol version.
    pub async fn health(&self) -> Result<Health> {
        let raw = self.send(Method::GET, "/v1/health", None, false).await?;
        serde_json::from_slice(&raw.body).map_err(|e| ClientError::Protocol(e.to_string()))
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

    /// Like [`NodeClient::stat`] but a final symbolic link is described, not followed.
    pub async fn lstat(&self, path: impl AsRef<Path>) -> Result<Stat> {
        self.post("/v1/fs/lstat", &PathRequest { path: path.as_ref().into() }).await
    }

    pub async fn readlink(&self, path: impl AsRef<Path>) -> Result<ReadlinkResponse> {
        self.post("/v1/fs/readlink", &PathRequest { path: path.as_ref().into() }).await
    }

    pub async fn symlink(&self, path: impl AsRef<Path>, target: impl AsRef<Path>) -> Result<Stat> {
        self.post("/v1/fs/symlink", &SymlinkRequest { path: path.as_ref().into(), target: target.as_ref().into() })
            .await
    }

    pub async fn mkdir(&self, req: &MkdirRequest) -> Result<Stat> {
        self.post("/v1/fs/mkdir", req).await
    }

    pub async fn remove(&self, path: impl AsRef<Path>, recursive: bool) -> Result<()> {
        let body = RemoveRequest { path: path.as_ref().into(), recursive };
        self.no_content(Method::POST, "/v1/fs/remove", Some(Self::json(&body))).await
    }

    pub async fn rename(&self, req: &RenameRequest) -> Result<()> {
        self.no_content(Method::POST, "/v1/fs/rename", Some(Self::json(req))).await
    }

    pub async fn setattr(&self, req: &SetAttrRequest) -> Result<Stat> {
        self.post("/v1/fs/setattr", req).await
    }

    pub async fn pwrite(&self, path: impl AsRef<Path>, offset: u64, data: impl Into<Vec<u8>>) -> Result<Stat> {
        self.post("/v1/fs/pwrite", &PwriteRequest { path: path.as_ref().into(), offset, data: data.into() }).await
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
                body: ErrorBody { code: ErrorCode::BadRequest, error: message, actual_sha256: None, errno: None },
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
        self.no_content(Method::POST, &format!("/v1/jobs/{id}/kill"), Some(Self::json(&KillRequest { signal })))
            .await
    }

    pub async fn remove_job(&self, id: &str) -> Result<()> {
        self.no_content(Method::DELETE, &format!("/v1/jobs/{id}"), None).await
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
        let query = serde_urlencoded::to_string(q).map_err(|e| ClientError::Protocol(e.to_string()))?;
        if query.is_empty() {
            self.get("/v1/terms").await
        } else {
            self.get(&format!("/v1/terms?{query}")).await
        }
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
            Method::POST,
            &format!("/v1/terms/{id}/control"),
            Some(Self::json(&TermControlRequest { client, take })),
        )
        .await
    }

    /// Signal a session (default SIGHUP, then SIGKILL after a grace period).
    pub async fn term_kill(&self, id: &str, signal: Option<i32>) -> Result<()> {
        self.no_content(Method::POST, &format!("/v1/terms/{id}/kill"), Some(Self::json(&KillRequest { signal })))
            .await
    }

    /// Forget a finished session.
    pub async fn term_remove(&self, id: &str) -> Result<()> {
        self.no_content(Method::DELETE, &format!("/v1/terms/{id}"), None).await
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
                body: ErrorBody { code: ErrorCode::NotFound, error: message, actual_sha256: None, errno: None },
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
