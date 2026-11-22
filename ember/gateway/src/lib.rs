//! Ember's in-app IDE gateway (SPEC `FR-W5b`, #80): a loopback HTTP listener that a phone's IDE
//! screen opens, and that pipes each request to the computer's OSE over the server's relay stream
//! (`FR-N9.1`, `ember_client::relay_stream`).
//!
//! **Who may connect.** The listener binds a loopback address only ([`GatewayConfig::bind`],
//! `127.0.0.1:0` by default), so other devices on the network never reach the workbench. Other
//! apps on the same phone can reach a loopback port, so every request must also carry the
//! per-launch random token ([`Gateway::token`]): as the `tkn` query of the first page load
//! ([`Gateway::url`]), then as the cookie the gateway sets on that load ([`TOKEN_COOKIE`],
//! `HttpOnly; SameSite=Strict; Path=/`), or as the [`TOKEN_HEADER`] header. Comparison is
//! constant-time. Without the token the answer is 403 and no stream is opened.
//!
//! **One relay stream per request.** The node replaces the OSE launch's placeholder token with the
//! real one only inside the first [`NODE_SUBSTITUTION_WINDOW`] bytes a client sends on one relay
//! stream, and only inside one WebSocket message (`ember/node/src/ose_api.rs`,
//! `RELAY_SUBSTITUTION_WINDOW` and `TokenSubstituter`). A browser keeps one connection alive for
//! many requests, so the gateway serves exactly one request per accepted connection on its own
//! stream: it asks the OSE for `Connection: close`, writes the rewritten request head as the
//! stream's first write (one message on the relay), puts the placeholder as the OSE's
//! [`OSE_TOKEN_COOKIE`] cookie on the line right after the request line, and forwards only the
//! request's own body. A WebSocket upgrade (the workbench's management and extension host
//! connections) keeps its stream for the life of the socket.
//!
//! **What reaches the OSE.** The gateway's own token never does: the `tkn` query is answered by a
//! redirect that drops it, and the gateway's cookie and header are taken out of every forwarded
//! request. The rest of the request goes on unchanged.
//!
//! The gateway reaches the OSE only through a [`Dialer`]; [`RelayDialer`] is the real one, over
//! ember-client's API (`FR-N5`: no transport types here). Tests use an in-memory stream.

use std::future::Future;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use ember_client::api::Api;
use ember_client::relay_stream::RelayStream;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};

/// The query parameter of the first page load that carries the gateway token.
pub const TOKEN_QUERY: &str = "tkn";
/// The cookie the gateway sets on the first page load.
pub const TOKEN_COOKIE: &str = "ember-gateway-tkn";
/// The header that may carry the gateway token instead.
pub const TOKEN_HEADER: &str = "X-Ember-Gateway-Token";
/// The OSE's own connection-token cookie (Code-OSS `vscode-tkn`); the gateway puts the launch's
/// placeholder there.
pub const OSE_TOKEN_COOKIE: &str = "vscode-tkn";
/// The node's substitution window, `RELAY_SUBSTITUTION_WINDOW` in `ember/node/src/ose_api.rs`.
/// The gateway does not depend on the node crate, so the value is repeated here.
pub const NODE_SUBSTITUTION_WINDOW: usize = 8 * 1024;

/// The largest request head the gateway reads.
const MAX_HEAD: usize = 16 * 1024;
/// How long a connection may take to send its request head.
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);

/// Opens one byte stream to the OSE: one call per forwarded request or upgraded socket.
pub trait Dialer: Send + Sync + 'static {
    type Stream: AsyncRead + AsyncWrite + Send + Unpin + 'static;
    fn dial(&self) -> impl Future<Output = io::Result<Self::Stream>> + Send;
}

/// The real dialer: the OSE relay route `relay` (`ose.relay`, `/api/computers/.../ide/relay/...`)
/// over the transport through `api` (`FR-N9.1`).
pub struct RelayDialer {
    api: Api,
    relay: String,
}

impl RelayDialer {
    pub fn new(api: Api, relay: impl Into<String>) -> RelayDialer {
        RelayDialer { api, relay: relay.into() }
    }
}

impl Dialer for RelayDialer {
    type Stream = RelayStream;
    fn dial(&self) -> impl Future<Output = io::Result<RelayStream>> + Send {
        async move {
            ember_client::relay_stream::dial_peer(&self.api, &self.relay)
                .await
                .map_err(|e| io::Error::other(e.to_string()))
        }
    }
}

/// How the gateway starts.
#[derive(Clone, Debug)]
pub struct GatewayConfig {
    /// Where to listen; a loopback address only. Default `127.0.0.1:0`.
    pub bind: SocketAddr,
    /// The OSE launch's placeholder token (`connection_token` of the launch, `FR-W4c`). When set,
    /// every forwarded request carries it as the [`OSE_TOKEN_COOKIE`] cookie.
    pub ose_token: Option<String>,
}

impl Default for GatewayConfig {
    fn default() -> GatewayConfig {
        GatewayConfig { bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), ose_token: None }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    /// Another device could reach the workbench through a non-loopback address.
    #[error("the gateway listens on loopback only, not {0}")]
    NotLoopback(SocketAddr),
    #[error("the OSE token is not a valid cookie value")]
    BadOseToken,
    #[error("the gateway could not start: {0}")]
    Io(#[from] io::Error),
}

/// A running gateway. Dropping it stops the listener and every connection.
pub struct Gateway {
    addr: SocketAddr,
    token: String,
    accept: JoinHandle<()>,
}

struct Shared<D> {
    dialer: D,
    token: String,
    ose_token: Option<String>,
}

impl Gateway {
    /// Binds `config.bind` (refused unless loopback), makes the launch's token and starts
    /// accepting. Each accepted request goes to the OSE on its own stream from `dialer`.
    pub async fn start<D: Dialer>(dialer: D, config: GatewayConfig) -> Result<Gateway, GatewayError> {
        if !config.bind.ip().is_loopback() {
            return Err(GatewayError::NotLoopback(config.bind));
        }
        if config.ose_token.as_deref().is_some_and(|t| !is_cookie_value(t)) {
            return Err(GatewayError::BadOseToken);
        }
        let token = new_token()?;
        let listener = TcpListener::bind(config.bind).await?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared { dialer, token: token.clone(), ose_token: config.ose_token });
        let accept = tokio::spawn(accept_loop(listener, shared));
        Ok(Gateway { addr, token, accept })
    }

    /// The bound loopback address.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// This launch's token: 64 hex characters.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The address the IDE screen opens first: `http://<addr><path_and_query>` with the token
    /// added as `tkn`. `path_and_query` is already encoded, such as `/?folder=%2Fhome%2Fp`.
    pub fn url(&self, path_and_query: &str) -> String {
        let path = if path_and_query.starts_with('/') { path_and_query.to_owned() } else { format!("/{path_and_query}") };
        let sep = if path.contains('?') { '&' } else { '?' };
        format!("http://{}{path}{sep}{TOKEN_QUERY}={}", self.addr, self.token)
    }

    /// Stops the listener and every open connection.
    pub fn stop(self) {}
}

impl Drop for Gateway {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

fn new_token() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(hex::encode(bytes))
}

/// Equal in time that depends only on the lengths (the token's length is public).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let diff = a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y));
    std::hint::black_box(diff) == 0
}

/// A cookie value that needs no quoting (RFC 6265 `cookie-octet`).
fn is_cookie_value(v: &str) -> bool {
    !v.is_empty() && v.bytes().all(|b| matches!(b, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e))
}

async fn accept_loop<D: Dialer>(listener: TcpListener, shared: Arc<Shared<D>>) {
    // Owned here, so aborting this task drops the set and ends every connection with it.
    let mut conns = JoinSet::new();
    loop {
        while conns.try_join_next().is_some() {}
        match listener.accept().await {
            Ok((tcp, peer)) if peer.ip().is_loopback() => {
                let _ = tcp.set_nodelay(true);
                conns.spawn(serve(tcp, shared.clone()));
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("the IDE gateway could not accept a connection: {e}");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

enum HeadError {
    Closed,
    TooLarge,
    NotText,
}

/// Reads up to the end of the request head; the bytes after it come back as the second value.
async fn read_head(tcp: &mut TcpStream) -> Result<(String, Vec<u8>), HeadError> {
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        let n = tcp.read(&mut chunk).await.map_err(|_| HeadError::Closed)?;
        if n == 0 {
            return Err(HeadError::Closed);
        }
        let from = buf.len().saturating_sub(3);
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf[from..].windows(4).position(|w| w == b"\r\n\r\n") {
            let end = from + i + 4;
            let rest = buf.split_off(end);
            let head = String::from_utf8(buf).map_err(|_| HeadError::NotText)?;
            return Ok((head, rest));
        }
        if buf.len() > MAX_HEAD {
            return Err(HeadError::TooLarge);
        }
    }
}

/// A parsed request head; everything borrows the head text.
struct Request<'a> {
    method: &'a str,
    path: &'a str,
    query: Option<&'a str>,
    version: &'a str,
    headers: Vec<(&'a str, &'a str)>,
}

/// What the gateway forwards after the head.
enum Body {
    /// `Content-Length` bytes (0 without one), then nothing more from this connection.
    Exact(u64),
    /// A chunked body: the client's bytes go on until the response ends.
    Chunked,
    /// An upgraded socket: both ways until either side ends.
    Upgrade,
}

impl<'a> Request<'a> {
    fn parse(head: &'a str) -> Option<Request<'a>> {
        let mut lines = head.trim_end_matches("\r\n").split("\r\n");
        let mut first = lines.next()?.split(' ');
        let (method, target, version) = (first.next()?, first.next()?, first.next()?);
        if first.next().is_some() || method.is_empty() || !target.starts_with('/') || !version.starts_with("HTTP/1.") {
            return None;
        }
        let (path, query) = match target.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (target, None),
        };
        let mut headers = Vec::new();
        for line in lines {
            let (name, value) = line.split_once(':')?;
            if name.is_empty() || name.bytes().any(|b| b.is_ascii_whitespace()) {
                return None;
            }
            headers.push((name, value.trim()));
        }
        Some(Request { method, path, query, version, headers })
    }

    fn header(&self, name: &str) -> impl Iterator<Item = &'a str> + '_ {
        let name = name.to_owned();
        self.headers.iter().filter(move |(n, _)| n.eq_ignore_ascii_case(&name)).map(|(_, v)| *v)
    }

    fn params(&self) -> impl Iterator<Item = (&'a str, &'a str)> {
        self.query.unwrap_or("").split('&').filter(|p| !p.is_empty()).map(|p| p.split_once('=').unwrap_or((p, "")))
    }

    fn cookies(&self) -> impl Iterator<Item = (&'a str, &'a str)> + '_ {
        self.header("cookie").flat_map(|v| v.split(';')).map(str::trim).filter(|c| !c.is_empty()).map(|c| c.split_once('=').unwrap_or(("", c)))
    }

    fn query_token(&self) -> Option<&'a str> {
        self.params().find(|(n, _)| *n == TOKEN_QUERY).map(|(_, v)| v)
    }

    /// Whether any token the request carries is this launch's.
    fn authorized(&self, token: &str) -> bool {
        let t = token.as_bytes();
        let query = self.params().filter(|(n, _)| *n == TOKEN_QUERY).map(|(_, v)| v);
        let cookie = self.cookies().filter(|(n, _)| *n == TOKEN_COOKIE).map(|(_, v)| v);
        let header = self.header(TOKEN_HEADER);
        query.chain(cookie).chain(header).fold(false, |ok, v| ok | ct_eq(v.as_bytes(), t))
    }

    fn is_upgrade(&self) -> bool {
        self.header("upgrade").any(|v| v.eq_ignore_ascii_case("websocket"))
            && self.header("connection").any(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case("upgrade")))
    }

    /// The target without the gateway's `tkn`.
    fn target_without_token(&self) -> String {
        let rest: Vec<String> =
            self.params().filter(|(n, _)| *n != TOKEN_QUERY).map(|(n, v)| if v.is_empty() && !self.has_eq(n) { n.to_owned() } else { format!("{n}={v}") }).collect();
        if rest.is_empty() { self.path.to_owned() } else { format!("{}?{}", self.path, rest.join("&")) }
    }

    /// Whether parameter `name` was written with `=` (so `a=` stays `a=` and `a` stays `a`).
    fn has_eq(&self, name: &str) -> bool {
        self.query.unwrap_or("").split('&').any(|p| p.strip_prefix(name).is_some_and(|r| r.starts_with('=')))
    }

    fn body(&self) -> Option<Body> {
        if self.is_upgrade() {
            return Some(Body::Upgrade);
        }
        if self.header("transfer-encoding").next().is_some() {
            return Some(Body::Chunked);
        }
        let mut lengths = self.header("content-length");
        match (lengths.next(), lengths.next()) {
            (None, _) => Some(Body::Exact(0)),
            (Some(n), None) => n.parse().ok().map(Body::Exact),
            _ => None,
        }
    }

    /// The head as it goes to the OSE: the target without `tkn`; the placeholder cookie (and the
    /// page's other cookies) on the line after the request line, so it lies inside the node's
    /// window; the gateway's cookie and header out; `Connection: close` unless upgraded. `None`
    /// when the placeholder would end past the node's window.
    fn forward(&self, ose_token: Option<&str>) -> Option<String> {
        let upgrade = self.is_upgrade();
        let mut out = format!("{} {} {}\r\n", self.method, self.target_without_token(), self.version);
        let mut cookies: Vec<String> = Vec::new();
        if let Some(t) = ose_token {
            cookies.push(format!("{OSE_TOKEN_COOKIE}={t}"));
            if out.len() + "Cookie: ".len() + cookies[0].len() > NODE_SUBSTITUTION_WINDOW {
                return None;
            }
        }
        for (n, v) in self.cookies() {
            let drop = n == TOKEN_COOKIE || (ose_token.is_some() && n == OSE_TOKEN_COOKIE);
            if !drop {
                cookies.push(if n.is_empty() { v.to_owned() } else { format!("{n}={v}") });
            }
        }
        if !cookies.is_empty() {
            out.push_str(&format!("Cookie: {}\r\n", cookies.join("; ")));
        }
        for (n, v) in &self.headers {
            let hop = ["connection", "keep-alive", "proxy-connection"].iter().any(|h| n.eq_ignore_ascii_case(h));
            if n.eq_ignore_ascii_case("cookie") || n.eq_ignore_ascii_case(TOKEN_HEADER) || (hop && !upgrade) {
                continue;
            }
            out.push_str(&format!("{n}: {v}\r\n"));
        }
        if !upgrade {
            out.push_str("Connection: close\r\n");
        }
        out.push_str("\r\n");
        Some(out)
    }
}

/// Answers the connection itself, with no body, and closes it.
async fn answer(tcp: &mut TcpStream, status: &str, extra: &str) {
    let text = format!("HTTP/1.1 {status}\r\n{extra}Content-Length: 0\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n");
    let _ = tcp.write_all(text.as_bytes()).await;
    let _ = tcp.shutdown().await;
}

/// One accepted connection: one request, or one upgraded socket, on one stream.
async fn serve<D: Dialer>(mut tcp: TcpStream, shared: Arc<Shared<D>>) {
    let (head, rest) = match tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut tcp)).await {
        Ok(Ok(read)) => read,
        Ok(Err(HeadError::TooLarge)) => return answer(&mut tcp, "431 Request Header Fields Too Large", "").await,
        Ok(Err(HeadError::NotText)) => return answer(&mut tcp, "400 Bad Request", "").await,
        Ok(Err(HeadError::Closed)) | Err(_) => return,
    };
    let Some(req) = Request::parse(&head) else {
        return answer(&mut tcp, "400 Bad Request", "").await;
    };
    if !req.authorized(&shared.token) {
        tracing::warn!("the IDE gateway refused a request without its token");
        return answer(&mut tcp, "403 Forbidden", "").await;
    }
    let Some(body) = req.body() else {
        return answer(&mut tcp, "400 Bad Request", "").await;
    };
    if req.query_token().is_some() && req.method == "GET" && !matches!(body, Body::Upgrade) {
        // The first page load: set the cookie and drop the token from the address. No stream.
        let extra = format!(
            "Location: {}\r\nSet-Cookie: {TOKEN_COOKIE}={}; HttpOnly; SameSite=Strict; Path=/\r\n",
            req.target_without_token(),
            shared.token
        );
        return answer(&mut tcp, "302 Found", &extra).await;
    }
    let Some(forwarded) = req.forward(shared.ose_token.as_deref()) else {
        return answer(&mut tcp, "414 URI Too Long", "").await;
    };
    let mut stream = match shared.dialer.dial().await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("the IDE gateway could not open the OSE relay: {e}");
            return answer(&mut tcp, "502 Bad Gateway", "").await;
        }
    };
    // One write: the relay carries it as one message, which the node's substitution needs.
    if stream.write_all(forwarded.as_bytes()).await.is_err() || stream.flush().await.is_err() {
        return answer(&mut tcp, "502 Bad Gateway", "").await;
    }
    pump(tcp, stream, rest, body).await;
}

/// Copies the response (or the socket) back, and the request's body (or the socket) on.
async fn pump<S: AsyncRead + AsyncWrite + Unpin>(tcp: TcpStream, stream: S, rest: Vec<u8>, body: Body) {
    let (mut cr, mut cw) = tcp.into_split();
    let (mut sr, mut sw) = tokio::io::split(stream);
    let down = async {
        let _ = tokio::io::copy(&mut sr, &mut cw).await;
        let _ = cw.shutdown().await;
    };
    let up = async {
        match body {
            Body::Exact(n) => {
                let first = rest.len().min(usize::try_from(n).unwrap_or(usize::MAX));
                if first > 0 && sw.write_all(&rest[..first]).await.is_err() {
                    return;
                }
                let left = n - first as u64;
                if left > 0 && tokio::io::copy(&mut (&mut cr).take(left), &mut sw).await.is_err() {
                    return;
                }
                let _ = sw.flush().await;
            }
            Body::Chunked => {
                if !rest.is_empty() && sw.write_all(&rest).await.is_err() {
                    return;
                }
                let _ = tokio::io::copy(&mut cr, &mut sw).await;
            }
            Body::Upgrade => {
                if !rest.is_empty() && sw.write_all(&rest).await.is_err() {
                    return;
                }
                let _ = tokio::io::copy(&mut cr, &mut sw).await;
                let _ = sw.shutdown().await;
                return;
            }
        }
        // A request is answered when the OSE closes the stream (`Connection: close`).
        std::future::pending::<()>().await
    };
    tokio::select! {
        _ = down => {}
        _ = up => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_w5b_token_compare_and_cookie_values() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
        assert!(!ct_eq(b"", b"a"));
        let t = new_token().unwrap();
        assert_eq!(t.len(), 64);
        assert!(is_cookie_value(&t));
        for bad in ["", "a b", "a;b", "a,b", "a\"b", "a\\b"] {
            assert!(!is_cookie_value(bad), "{bad:?}");
        }
    }

    #[test]
    fn fr_w5b_the_forwarded_head_drops_the_gateway_token() {
        let head = "GET /x?a=1&tkn=T&b=&c HTTP/1.1\r\nHost: h\r\nConnection: keep-alive\r\nX-Ember-Gateway-Token: T\r\nCookie: ember-gateway-tkn=T; z=9\r\n\r\n";
        let req = Request::parse(head).unwrap();
        assert!(req.authorized("T"));
        assert!(!req.authorized("U"));
        assert_eq!(req.target_without_token(), "/x?a=1&b=&c");
        let out = req.forward(Some("P")).unwrap();
        assert_eq!(out, "GET /x?a=1&b=&c HTTP/1.1\r\nCookie: vscode-tkn=P; z=9\r\nHost: h\r\nConnection: close\r\n\r\n");
        let long = format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(NODE_SUBSTITUTION_WINDOW));
        assert!(Request::parse(&long).unwrap().forward(Some("P")).is_none(), "the placeholder would pass the window");
        assert!(Request::parse("GET x HTTP/1.1\r\n\r\n").is_none());
        assert!(Request::parse("GET / HTTP/1.1\r\nbad line\r\n\r\n").is_none());
    }
}
