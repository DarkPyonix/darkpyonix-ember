//! The loopback gateway of SPEC `FR-W5b` (INTENT D53, D55, #1064).
//!
//! A page in a webview cannot open the server's relay route itself: the route wants the server
//! token or the device key, and the workbench speaks plain HTTP and WebSocket to its own origin.
//! The gateway listens on `127.0.0.1:<port>` and joins each accepted TCP connection to one relay
//! stream (`/api/computers/{id}/ide/relay/{launch_id}`, `FR-W4c` (2)), so the OSE behind the relay
//! serves the page as if the webview were talking to it directly.
//!
//! It does no HTTP work beyond the first request head of a connection:
//!
//! - The request must carry the per-launch token, as the cookie `ember-gw=<token>` or, on the
//!   first page load, as the query `egw=<token>`. The query form is answered with a 302 to the
//!   same URL without `egw` and a `Set-Cookie`, so the token leaves the address bar and every
//!   later request (resources, WebSockets) carries the cookie. Anything else is a 403 and the
//!   connection is closed. Why: any local process can reach a loopback port; the token keeps them
//!   off the relay, which carries this user's credential.
//! - An accepted connection opens one relay stream, the buffered head (and whatever body bytes
//!   came with it) is written to it, and bytes are then copied both ways until either side ends.
//!   Each read from the TCP side goes out as one binary WebSocket message.
//!
//! The OSE behind the VS Code target has no connection token (`FR-W4a`, D55), so nothing in the
//! byte stream is rewritten. The gateway names no transport backend type (`FR-N5`): a relay
//! stream is whatever [`Dial`] opens, and [`RelayTarget`] dials `ws(s)://<server><relay>` with the
//! server token, as the Ember editor does over HTTP (`ember/app/src/editor_reach.rs`).

use std::fmt;
use std::future::Future;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{ready, Context, Poll};

use futures_util::{Sink, Stream};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

/// The cookie that carries the per-launch token after the first page load.
pub const COOKIE_NAME: &str = "ember-gw";
/// The query parameter that carries the per-launch token on the first page load.
pub const QUERY_NAME: &str = "egw";
/// The longest request head the gateway reads before it refuses the connection (431).
pub const MAX_HEAD: usize = 16 * 1024;

// ---------------------------------------------------------------------------------------------
// The relay stream

/// A WebSocket a relay stream runs on.
pub trait RelaySocket:
    Sink<Message, Error = WsError> + Stream<Item = Result<Message, WsError>> + Send + Unpin
{
}

impl<T> RelaySocket for T where
    T: Sink<Message, Error = WsError> + Stream<Item = Result<Message, WsError>> + Send + Unpin
{
}

/// What [`Dial::dial`] returns.
pub type DialFuture = Pin<Box<dyn Future<Output = io::Result<Box<dyn RelaySocket>>> + Send>>;

/// Opens one relay stream. The gateway calls it once per accepted connection.
pub trait Dial: Send + Sync + 'static {
    fn dial(&self) -> DialFuture;
}

/// The server's relay route over plain `ws(s)`, with the server token as `Authorization: Bearer`.
#[derive(Clone)]
pub struct RelayTarget {
    /// The server's base URL, `http(s)://host:port`.
    pub server_url: String,
    /// The relay path as the server gives it, `/api/computers/{id}/ide/relay/{launch_id}`.
    pub relay_path: String,
    /// The server token, when the caller may send it to this server (`FR-S11`).
    pub token: Option<String>,
}

impl fmt::Debug for RelayTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayTarget")
            .field("server_url", &self.server_url)
            .field("relay_path", &self.relay_path)
            .field("token", &self.token.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl Dial for RelayTarget {
    fn dial(&self) -> DialFuture {
        let target = self.clone();
        Box::pin(async move {
            let ws = dial_http(&target).await?;
            Ok(Box::new(ws) as Box<dyn RelaySocket>)
        })
    }
}

/// The relay path as the server gives it: below the server's base URL and under `/api/`.
fn checked_relay(relay: &str) -> io::Result<&str> {
    let ok = relay.starts_with("/api/") && !relay.contains("://") && !relay.chars().any(char::is_whitespace);
    if ok {
        Ok(relay)
    } else {
        Err(io::Error::new(io::ErrorKind::InvalidInput, format!("not an OSE relay path: {relay}")))
    }
}

/// `ws(s)://<server><relay>`: `wss` for an `https` server, `ws` for `http`.
pub fn relay_url(base: &str, relay: &str) -> io::Result<String> {
    let relay = checked_relay(relay)?;
    let base = base.trim_end_matches('/');
    let ws = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("the server address is not http(s): {base}")));
    };
    Ok(format!("{ws}{relay}"))
}

fn ensure_tls_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

async fn dial_http(
    target: &RelayTarget,
) -> io::Result<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>> {
    ensure_tls_provider();
    let mut upgrade = relay_url(&target.server_url, &target.relay_path)?.into_client_request().map_err(ws_io)?;
    if let Some(token) = target.token.as_deref().filter(|t| !t.is_empty()) {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "the server token is not a valid header value"))?;
        upgrade.headers_mut().insert("Authorization", value);
    }
    match tokio_tungstenite::connect_async(upgrade).await {
        Ok((ws, _)) => Ok(ws),
        Err(WsError::Http(resp)) => {
            let status = resp.status();
            let kind = if matches!(status.as_u16(), 401 | 403) {
                io::ErrorKind::PermissionDenied
            } else {
                io::ErrorKind::ConnectionRefused
            };
            Err(io::Error::new(kind, format!("the OSE relay upgrade was answered with {status}")))
        }
        Err(e) => Err(ws_io(e)),
    }
}

fn ws_io(e: WsError) -> io::Error {
    match e {
        WsError::Io(e) => e,
        WsError::ConnectionClosed | WsError::AlreadyClosed => {
            io::Error::new(io::ErrorKind::NotConnected, "the OSE relay is closed")
        }
        other => io::Error::other(other.to_string()),
    }
}

/// The OSE's byte stream over a relay WebSocket: each write is one binary message, reads join
/// binary messages, and a Close (or the socket ending) is the end of the stream.
pub struct RelayStream {
    ws: Box<dyn RelaySocket>,
    pending: Vec<u8>,
    pos: usize,
    eof: bool,
}

impl RelayStream {
    pub fn new(ws: Box<dyn RelaySocket>) -> RelayStream {
        RelayStream { ws, pending: Vec::new(), pos: 0, eof: false }
    }
}

impl AsyncRead for RelayStream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            if this.pos < this.pending.len() {
                let n = (this.pending.len() - this.pos).min(buf.remaining());
                buf.put_slice(&this.pending[this.pos..this.pos + n]);
                this.pos += n;
                return Poll::Ready(Ok(()));
            }
            if this.eof {
                return Poll::Ready(Ok(()));
            }
            match ready!(Pin::new(&mut *this.ws).poll_next(cx)) {
                Some(Ok(Message::Binary(data))) => {
                    this.pending = data.to_vec();
                    this.pos = 0;
                }
                Some(Ok(Message::Close(_))) | None => this.eof = true,
                Some(Ok(Message::Text(_))) => {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "the OSE relay sent a text message",
                    )));
                }
                // Ping and Pong are answered by the socket.
                Some(Ok(_)) => {}
                Some(Err(WsError::ConnectionClosed | WsError::AlreadyClosed)) => this.eof = true,
                Some(Err(e)) => return Poll::Ready(Err(ws_io(e))),
            }
        }
    }
}

impl AsyncWrite for RelayStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let this = self.get_mut();
        ready!(Pin::new(&mut *this.ws).poll_ready(cx)).map_err(ws_io)?;
        Pin::new(&mut *this.ws).start_send(Message::Binary(buf.to_vec().into())).map_err(ws_io)?;
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.get_mut().ws).poll_flush(cx).map_err(ws_io)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match ready!(Pin::new(&mut *self.get_mut().ws).poll_close(cx)) {
            Ok(()) | Err(WsError::ConnectionClosed | WsError::AlreadyClosed) => Poll::Ready(Ok(())),
            Err(e) => Poll::Ready(Err(ws_io(e))),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The request head

/// What the gateway does with a connection, from its first request head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    /// The cookie carries the token: join the connection to a relay stream.
    Join,
    /// The query carries the token: answer 302 to this location and set the cookie.
    Redirect(String),
    /// No token, or a wrong one: answer 403.
    Refused,
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Decides [`Access`] for a request head (the bytes up to and including the blank line).
pub fn access(head: &[u8], token: &str) -> Access {
    let Ok(text) = std::str::from_utf8(head) else {
        return Access::Refused;
    };
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split(' ');
    let (Some(_method), Some(target), Some(_version)) = (parts.next(), parts.next(), parts.next()) else {
        return Access::Refused;
    };
    if !target.starts_with('/') {
        return Access::Refused;
    }

    if let Some((path, query)) = target.split_once('?') {
        let (query, _fragment) = query.split_once('#').unwrap_or((query, ""));
        let mut found = false;
        let mut rest = Vec::new();
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            match pair.split_once('=') {
                Some((QUERY_NAME, value)) => found |= same(value, token),
                _ if pair == QUERY_NAME => {}
                _ => rest.push(pair),
            }
        }
        if found {
            let location = if rest.is_empty() { path.to_owned() } else { format!("{path}?{}", rest.join("&")) };
            return Access::Redirect(location);
        }
    }

    for line in lines {
        let Some((name, value)) = line.split_once(':') else { continue };
        if !name.trim().eq_ignore_ascii_case("cookie") {
            continue;
        }
        for cookie in value.split(';') {
            if let Some((COOKIE_NAME, v)) = cookie.trim().split_once('=') {
                if same(v.trim(), token) {
                    return Access::Join;
                }
            }
        }
    }
    Access::Refused
}

/// Reads until the blank line that ends the request head. Returns everything read (head and any
/// body bytes that came with it) and the length of the head, or `None` when the peer closed first.
async fn read_head(tcp: &mut TcpStream) -> io::Result<Option<(Vec<u8>, usize)>> {
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        let n = tcp.read(&mut chunk).await?;
        if n == 0 {
            return Ok(None);
        }
        let from = buf.len().saturating_sub(3);
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf[from..].windows(4).position(|w| w == b"\r\n\r\n") {
            let end = from + i + 4;
            if end > MAX_HEAD {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "request head too large"));
            }
            return Ok(Some((buf, end)));
        }
        if buf.len() > MAX_HEAD {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "request head too large"));
        }
    }
}

async fn respond(tcp: &mut TcpStream, status: &str, extra: &str, body: &str) {
    let text = format!(
        "HTTP/1.1 {status}\r\n{extra}Content-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = tcp.write_all(text.as_bytes()).await;
    let _ = tcp.shutdown().await;
}

/// The `Set-Cookie` value the 302 carries.
pub fn set_cookie(token: &str) -> String {
    format!("{COOKIE_NAME}={token}; HttpOnly; SameSite=Strict; Path=/")
}

// ---------------------------------------------------------------------------------------------
// The gateway

struct Shared {
    token: String,
    dial: Box<dyn Dial>,
}

async fn serve(mut tcp: TcpStream, shared: Arc<Shared>) {
    let (buf, head_len) = match read_head(&mut tcp).await {
        Ok(Some(read)) => read,
        Ok(None) => return,
        Err(e) if e.kind() == io::ErrorKind::InvalidData => {
            respond(&mut tcp, "431 Request Header Fields Too Large", "", "request head too large\n").await;
            return;
        }
        Err(_) => return,
    };
    match access(&buf[..head_len], &shared.token) {
        Access::Refused => respond(&mut tcp, "403 Forbidden", "", "forbidden\n").await,
        Access::Redirect(location) => {
            let extra = format!("Location: {location}\r\nSet-Cookie: {}\r\n", set_cookie(&shared.token));
            respond(&mut tcp, "302 Found", &extra, "").await;
        }
        Access::Join => {
            let ws = match shared.dial.dial().await {
                Ok(ws) => ws,
                Err(e) => {
                    tracing::warn!("the gateway could not open the OSE relay: {e}");
                    respond(&mut tcp, "502 Bad Gateway", "", "the IDE relay could not be opened\n").await;
                    return;
                }
            };
            let mut relay = RelayStream::new(ws);
            if relay.write_all(&buf).await.is_err() || relay.flush().await.is_err() {
                return;
            }
            if let Err(e) = tokio::io::copy_bidirectional(&mut tcp, &mut relay).await {
                tracing::debug!("a gateway connection ended: {e}");
            }
            let _ = relay.shutdown().await;
        }
    }
}

async fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    loop {
        match listener.accept().await {
            Ok((tcp, _)) => {
                let _ = tcp.set_nodelay(true);
                tokio::spawn(serve(tcp, shared.clone()));
            }
            Err(e) => {
                tracing::warn!("the gateway could not accept a connection: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }
}

fn new_token() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Percent-encodes a query value: everything but the unreserved characters.
pub fn encode_query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `http://127.0.0.1:<port>/?folder=<folder>&egw=<token>`, the first page the window loads.
pub fn page_url(addr: SocketAddr, token: &str, folder: &str) -> String {
    format!("http://{addr}/?folder={}&{QUERY_NAME}={token}", encode_query_value(folder))
}

/// A running gateway on the caller's tokio runtime. Dropping it stops accepting connections.
pub struct Gateway {
    addr: SocketAddr,
    token: String,
    task: tokio::task::JoinHandle<()>,
}

impl Gateway {
    /// Starts a gateway on `127.0.0.1:0` whose connections go to the server's relay route.
    pub async fn start(relay: RelayTarget) -> io::Result<Gateway> {
        Gateway::start_with(relay).await
    }

    /// Starts a gateway whose connections each take one stream from `dial`.
    pub async fn start_with(dial: impl Dial) -> io::Result<Gateway> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let addr = listener.local_addr()?;
        let token = new_token()?;
        let shared = Arc::new(Shared { token: token.clone(), dial: Box::new(dial) });
        let task = tokio::spawn(accept_loop(listener, shared));
        Ok(Gateway { addr, token, task })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The per-launch token.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// `http://127.0.0.1:<port>`, the page's origin.
    pub fn origin(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn page_url(&self, folder: &str) -> String {
        page_url(self.addr, &self.token, folder)
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A gateway on its own thread and tokio runtime, for a caller that has no runtime (the IDE
/// window's event loop owns the main thread). Dropping it stops the runtime.
pub struct ThreadGateway {
    addr: SocketAddr,
    token: String,
    _stop: tokio::sync::oneshot::Sender<()>,
}

impl ThreadGateway {
    pub fn start(dial: impl Dial) -> io::Result<ThreadGateway> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<io::Result<(SocketAddr, String)>>();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        std::thread::Builder::new().name("ember-gateway".into()).spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            rt.block_on(async move {
                match Gateway::start_with(dial).await {
                    Ok(gw) => {
                        let _ = ready_tx.send(Ok((gw.addr(), gw.token().to_owned())));
                        let _ = stop_rx.await;
                        drop(gw);
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            });
        })?;
        let (addr, token) = ready_rx
            .recv()
            .map_err(|_| io::Error::other("the gateway thread ended before it started"))??;
        Ok(ThreadGateway { addr, token, _stop: stop_tx })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn origin(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn page_url(&self, folder: &str) -> String {
        page_url(self.addr, &self.token, folder)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "abc123";

    #[test]
    fn fr_w5b_access_reads_the_query_then_the_cookie() {
        assert_eq!(access(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n", T), Access::Refused);
        assert_eq!(
            access(b"GET /?folder=%2Fw&egw=abc123 HTTP/1.1\r\nHost: x\r\n\r\n", T),
            Access::Redirect("/?folder=%2Fw".into())
        );
        assert_eq!(access(b"GET /a/b?egw=abc123 HTTP/1.1\r\n\r\n", T), Access::Redirect("/a/b".into()));
        assert_eq!(access(b"GET /?egw=nope HTTP/1.1\r\n\r\n", T), Access::Refused);
        assert_eq!(access(b"GET /x HTTP/1.1\r\nCookie: a=1; ember-gw=abc123\r\n\r\n", T), Access::Join);
        assert_eq!(access(b"GET /x HTTP/1.1\r\ncookie: ember-gw=abc12\r\n\r\n", T), Access::Refused);
        assert_eq!(access(b"GET http://evil/ HTTP/1.1\r\nCookie: ember-gw=abc123\r\n\r\n", T), Access::Refused);
        assert_eq!(access(b"garbage\r\n\r\n", T), Access::Refused);
    }

    #[test]
    fn fr_w5b_page_url_and_cookie_shape() {
        let addr: SocketAddr = "127.0.0.1:4100".parse().unwrap();
        assert_eq!(page_url(addr, T, "/work/my repo"), "http://127.0.0.1:4100/?folder=%2Fwork%2Fmy%20repo&egw=abc123");
        assert_eq!(set_cookie(T), "ember-gw=abc123; HttpOnly; SameSite=Strict; Path=/");
        assert_eq!(relay_url("https://s.example/", "/api/computers/c/ide/relay/l").unwrap(), "wss://s.example/api/computers/c/ide/relay/l");
        assert!(relay_url("http://s", "/other").is_err());
        assert!(!format!("{:?}", RelayTarget { server_url: "u".into(), relay_path: "p".into(), token: Some("secret".into()) }).contains("secret"));
    }

    #[test]
    fn fr_w5b_tokens_are_fresh_per_launch() {
        let a = new_token().unwrap();
        let b = new_token().unwrap();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
    }
}
