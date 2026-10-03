//! Network egress for the remote browser (SPEC FR-R1): a SOCKS5 exit on this computer.
//!
//! The main server's browser uses a proxy; for "egress from computer X" that proxy is a loopback
//! listener in ember server that carries each proxied TCP connection to this node and lets the
//! node open the outbound connection. Pages then see this computer's public IP, its LAN and its
//! `localhost`.
//!
//! # `/v1/egress` (WebSocket, bearer token like every other route)
//!
//! One WebSocket = one proxied TCP connection. The WebSocket carries a **raw SOCKS5 byte
//! stream** (RFC 1928) in Binary frames (Text frames are treated as bytes too), chunked
//! arbitrarily. The node runs the SOCKS5 server side; the client — ember server's loopback
//! listener ([`bridge`]) — just copies bytes, so Chrome's own SOCKS5 client does the handshake
//! end to end. Over this route no SOCKS authentication is offered (method `0x00`): the WebSocket
//! upgrade already carried the bearer token.
//!
//! - Commands: `CONNECT` only (`BIND` and `UDP ASSOCIATE` get reply `0x07`).
//! - Address types: IPv4, IPv6, domain name (resolved **on the node**, so DNS is the egress
//!   computer's too).
//! - **Half-close:** an *empty* Binary frame means "no more bytes in this direction" (TCP FIN).
//!   Each side sends one when its TCP read side ends; a side that has both sent and received one
//!   closes the WebSocket. A Close frame (or a dropped connection) ends both directions at once.
//!
//! # Plain SOCKS5 listener (optional)
//!
//! `EMBER_NODE_SOCKS_LISTEN=<addr>` also serves SOCKS5 on a TCP port, for direct use on a LAN.
//! Clients connecting from loopback need no authentication; anyone else must use
//! username/password authentication (RFC 1929) with the node token as the password (the username
//! is ignored).
//!
//! # Policy
//!
//! Default: allow every destination (the egress computer's `localhost` and LAN must be reachable,
//! FR-R1). `EMBER_NODE_EGRESS_DENY` lists denied destinations, comma-separated: CIDRs
//! (`10.0.0.0/8`, `fd00::/8`, a bare address = /32 or /128) and the keywords `private`
//! (RFC 1918, CGNAT 100.64/10, IPv6 ULA fc00::/7), `link-local` (169.254/16 — includes cloud
//! metadata endpoints — and fe80::/10), `loopback` (127/8, ::1). A domain is resolved first and
//! every resolved address is checked; denied addresses are skipped, and if none is left the
//! request is refused with reply `0x02`. `EMBER_NODE_EGRESS=off` disables the egress entirely.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, ensure, Context};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::api::Node;
use crate::client::{ClientError, NodeClient};
use crate::proto::{ErrorBody, ErrorCode};

/// How long one outbound connection attempt may take.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a client may take for the SOCKS5 handshake.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// Buffer between the WebSocket and the SOCKS5 server.
const PIPE_BUF: usize = 64 * 1024;

// ---------------------------------------------------------------------------------------------
// Policy

/// A denied address range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    pub addr: IpAddr,
    pub prefix: u8,
}

impl Cidr {
    pub fn parse(s: &str) -> anyhow::Result<Cidr> {
        let s = s.trim();
        let (a, p) = match s.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (s, None),
        };
        let addr: IpAddr = a.parse().with_context(|| format!("bad address in {s:?}"))?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match p {
            Some(p) => p.parse::<u8>().with_context(|| format!("bad prefix in {s:?}"))?,
            None => max,
        };
        ensure!(prefix <= max, "prefix /{prefix} too long in {s:?}");
        Ok(Cidr { addr, prefix })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, canonical(ip)) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = if self.prefix == 0 { 0 } else { u32::MAX << (32 - self.prefix as u32) };
                u32::from(net) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = if self.prefix == 0 { 0 } else { u128::MAX << (128 - self.prefix as u32) };
                u128::from(net) & mask == u128::from(ip) & mask
            }
            _ => false,
        }
    }
}

/// An IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) is checked as the IPv4 address it is.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    }
}

fn keyword(k: &str) -> Option<Vec<Cidr>> {
    let list: &[&str] = match k {
        "private" => &["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "100.64.0.0/10", "fc00::/7"],
        "link-local" => &["169.254.0.0/16", "fe80::/10"],
        "loopback" => &["127.0.0.0/8", "::1/128"],
        _ => return None,
    };
    Some(list.iter().map(|c| Cidr::parse(c).expect("valid built-in CIDR")).collect())
}

/// Which destinations the egress may reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressPolicy {
    /// `false`: `/v1/egress` refuses every connection (403).
    pub enabled: bool,
    pub deny: Vec<Cidr>,
}

impl Default for EgressPolicy {
    /// Enabled, everything allowed.
    fn default() -> Self {
        EgressPolicy { enabled: true, deny: Vec::new() }
    }
}

impl EgressPolicy {
    /// Parse a deny list (`EMBER_NODE_EGRESS_DENY` syntax).
    pub fn with_deny_list(list: &str) -> anyhow::Result<EgressPolicy> {
        let mut deny = Vec::new();
        for item in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            match keyword(item) {
                Some(cidrs) => deny.extend(cidrs),
                None => deny.push(Cidr::parse(item)?),
            }
        }
        Ok(EgressPolicy { enabled: true, deny })
    }

    /// `EMBER_NODE_EGRESS` (`off`/`0`/`false` disables) and `EMBER_NODE_EGRESS_DENY`.
    pub fn from_env() -> anyhow::Result<EgressPolicy> {
        let mut p = match std::env::var("EMBER_NODE_EGRESS_DENY") {
            Ok(list) => EgressPolicy::with_deny_list(&list).context("EMBER_NODE_EGRESS_DENY")?,
            Err(_) => EgressPolicy::default(),
        };
        if matches!(std::env::var("EMBER_NODE_EGRESS").as_deref(), Ok("off" | "0" | "false" | "no")) {
            p.enabled = false;
        }
        Ok(p)
    }

    pub fn allows(&self, ip: IpAddr) -> bool {
        self.enabled && !self.deny.iter().any(|c| c.contains(ip))
    }
}

// ---------------------------------------------------------------------------------------------
// SOCKS5 server

pub const SOCKS_VERSION: u8 = 5;
pub const METHOD_NO_AUTH: u8 = 0x00;
pub const METHOD_USER_PASS: u8 = 0x02;
pub const METHOD_NONE_ACCEPTABLE: u8 = 0xFF;
pub const CMD_CONNECT: u8 = 0x01;
pub const ATYP_IPV4: u8 = 0x01;
pub const ATYP_DOMAIN: u8 = 0x03;
pub const ATYP_IPV6: u8 = 0x04;

/// SOCKS5 reply codes (RFC 1928 §6).
pub mod reply {
    pub const SUCCEEDED: u8 = 0x00;
    pub const GENERAL_FAILURE: u8 = 0x01;
    pub const NOT_ALLOWED: u8 = 0x02;
    pub const NETWORK_UNREACHABLE: u8 = 0x03;
    pub const HOST_UNREACHABLE: u8 = 0x04;
    pub const CONNECTION_REFUSED: u8 = 0x05;
    pub const TTL_EXPIRED: u8 = 0x06;
    pub const COMMAND_NOT_SUPPORTED: u8 = 0x07;
    pub const ADDRESS_TYPE_NOT_SUPPORTED: u8 = 0x08;
}

/// What a SOCKS5 client must present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocksAuth {
    /// Method `0x00` (loopback clients, and `/v1/egress` whose upgrade carried the token).
    None,
    /// Method `0x02` (RFC 1929): any username, this password.
    Password(String),
}

/// A CONNECT destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Ip(SocketAddr),
    Domain(String, u16),
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Target::Ip(a) => write!(f, "{a}"),
            Target::Domain(h, p) => write!(f, "{h}:{p}"),
        }
    }
}

/// Constant-time comparison.
fn secret_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn write_reply<S: AsyncWrite + Unpin>(s: &mut S, code: u8, bound: Option<SocketAddr>) -> std::io::Result<()> {
    let mut out = vec![SOCKS_VERSION, code, 0x00];
    match bound.unwrap_or_else(|| SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))) {
        SocketAddr::V4(a) => {
            out.push(ATYP_IPV4);
            out.extend_from_slice(&a.ip().octets());
            out.extend_from_slice(&a.port().to_be_bytes());
        }
        SocketAddr::V6(a) => {
            out.push(ATYP_IPV6);
            out.extend_from_slice(&a.ip().octets());
            out.extend_from_slice(&a.port().to_be_bytes());
        }
    }
    s.write_all(&out).await?;
    s.flush().await
}

/// Read the greeting, authenticate, and read the request. Returns the command and target.
async fn handshake<S>(s: &mut S, auth: &SocksAuth) -> anyhow::Result<(u8, Target)>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let ver = s.read_u8().await?;
    ensure!(ver == SOCKS_VERSION, "not SOCKS5 (version byte {ver})");
    let n = s.read_u8().await? as usize;
    let mut methods = vec![0u8; n];
    s.read_exact(&mut methods).await?;
    let want = match auth {
        SocksAuth::None => METHOD_NO_AUTH,
        SocksAuth::Password(_) => METHOD_USER_PASS,
    };
    if !methods.contains(&want) {
        s.write_all(&[SOCKS_VERSION, METHOD_NONE_ACCEPTABLE]).await?;
        bail!("client offered no acceptable method ({methods:?}, need {want:#04x})");
    }
    s.write_all(&[SOCKS_VERSION, want]).await?;
    s.flush().await?;
    if let SocksAuth::Password(secret) = auth {
        // RFC 1929: VER(1) ULEN UNAME PLEN PASSWD
        let v = s.read_u8().await?;
        ensure!(v == 0x01, "bad username/password sub-negotiation version {v}");
        let ulen = s.read_u8().await? as usize;
        let mut user = vec![0u8; ulen];
        s.read_exact(&mut user).await?;
        let plen = s.read_u8().await? as usize;
        let mut pass = vec![0u8; plen];
        s.read_exact(&mut pass).await?;
        let ok = secret_eq(&pass, secret.as_bytes());
        s.write_all(&[0x01, if ok { 0x00 } else { 0x01 }]).await?;
        s.flush().await?;
        ensure!(ok, "SOCKS5 authentication failed");
    }
    let mut hdr = [0u8; 4];
    s.read_exact(&mut hdr).await?;
    ensure!(hdr[0] == SOCKS_VERSION, "bad request version {}", hdr[0]);
    let cmd = hdr[1];
    let target = match hdr[3] {
        ATYP_IPV4 => {
            let mut a = [0u8; 4];
            s.read_exact(&mut a).await?;
            let port = s.read_u16().await?;
            Target::Ip(SocketAddr::from((Ipv4Addr::from(a), port)))
        }
        ATYP_IPV6 => {
            let mut a = [0u8; 16];
            s.read_exact(&mut a).await?;
            let port = s.read_u16().await?;
            Target::Ip(SocketAddr::from((Ipv6Addr::from(a), port)))
        }
        ATYP_DOMAIN => {
            let len = s.read_u8().await? as usize;
            let mut name = vec![0u8; len];
            s.read_exact(&mut name).await?;
            let port = s.read_u16().await?;
            let name = String::from_utf8(name).context("domain name is not UTF-8")?;
            // A literal address sent as a "domain" is checked as the address it is.
            match name.trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>() {
                Ok(ip) => Target::Ip(SocketAddr::new(ip, port)),
                Err(_) => Target::Domain(name, port),
            }
        }
        other => {
            write_reply(s, reply::ADDRESS_TYPE_NOT_SUPPORTED, None).await?;
            bail!("address type {other:#04x} not supported");
        }
    };
    Ok((cmd, target))
}

/// Map a connect error to a SOCKS5 reply code.
fn reply_for(e: &std::io::Error) -> u8 {
    use std::io::ErrorKind::*;
    match e.kind() {
        ConnectionRefused => reply::CONNECTION_REFUSED,
        NetworkUnreachable => reply::NETWORK_UNREACHABLE,
        HostUnreachable => reply::HOST_UNREACHABLE,
        TimedOut => reply::TTL_EXPIRED,
        _ => reply::GENERAL_FAILURE,
    }
}

/// Resolve `target` and connect to the first allowed address. `Err` carries the reply code.
pub async fn connect(target: &Target, policy: &EgressPolicy) -> Result<TcpStream, (u8, String)> {
    if !policy.enabled {
        return Err((reply::NOT_ALLOWED, "egress is disabled on this node".into()));
    }
    let addrs: Vec<SocketAddr> = match target {
        Target::Ip(a) => vec![*a],
        Target::Domain(host, port) => match tokio::net::lookup_host((host.as_str(), *port)).await {
            Ok(it) => it.collect(),
            Err(e) => return Err((reply::HOST_UNREACHABLE, format!("resolving {host}: {e}"))),
        },
    };
    if addrs.is_empty() {
        return Err((reply::HOST_UNREACHABLE, format!("{target} resolved to no address")));
    }
    let allowed: Vec<SocketAddr> = addrs.iter().copied().filter(|a| policy.allows(a.ip())).collect();
    if allowed.is_empty() {
        return Err((reply::NOT_ALLOWED, format!("{target} is denied by the node's egress policy")));
    }
    let mut last = (reply::HOST_UNREACHABLE, String::new());
    for addr in allowed {
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
            Ok(Ok(s)) => {
                let _ = s.set_nodelay(true);
                return Ok(s);
            }
            Ok(Err(e)) => last = (reply_for(&e), format!("{addr}: {e}")),
            Err(_) => last = (reply::TTL_EXPIRED, format!("{addr}: connect timed out")),
        }
    }
    Err(last)
}

/// Serve one SOCKS5 client on `s` until either side closes. Errors describe why the client was
/// refused or the stream broke; they are not reported anywhere else.
pub async fn serve_socks5<S>(mut s: S, auth: SocksAuth, policy: Arc<EgressPolicy>) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (cmd, target) = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake(&mut s, &auth))
        .await
        .context("SOCKS5 handshake timed out")??;
    if cmd != CMD_CONNECT {
        write_reply(&mut s, reply::COMMAND_NOT_SUPPORTED, None).await?;
        bail!("command {cmd:#04x} not supported (CONNECT only)");
    }
    let mut upstream = match connect(&target, &policy).await {
        Ok(u) => u,
        Err((code, msg)) => {
            write_reply(&mut s, code, None).await?;
            bail!("CONNECT {target}: {msg}");
        }
    };
    tracing::debug!(%target, "egress: connected");
    write_reply(&mut s, reply::SUCCEEDED, upstream.local_addr().ok()).await?;
    tokio::io::copy_bidirectional(&mut s, &mut upstream).await?;
    Ok(())
}

/// Serve plain SOCKS5 on `listener` (`EMBER_NODE_SOCKS_LISTEN`): loopback peers need no
/// authentication, everyone else the node token as RFC 1929 password.
pub async fn serve_listener(listener: TcpListener, token: String, policy: Arc<EgressPolicy>) {
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!("SOCKS5 accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let auth = auth_for_peer(peer, &token);
        let policy = policy.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_socks5(stream, auth, policy).await {
                tracing::debug!(%peer, "SOCKS5: {e:#}");
            }
        });
    }
}

/// No authentication for loopback clients, the token otherwise.
pub fn auth_for_peer(peer: SocketAddr, token: &str) -> SocksAuth {
    if canonical(peer.ip()).is_loopback() {
        SocksAuth::None
    } else {
        SocksAuth::Password(token.to_string())
    }
}

// ---------------------------------------------------------------------------------------------
// `/v1/egress` (node side)

/// Route handler for `GET /v1/egress`.
pub async fn ws(State(node): State<Node>, ws: WebSocketUpgrade) -> Response {
    let policy = node.egress_policy().clone();
    if !policy.enabled {
        let body = ErrorBody {
            code: ErrorCode::BadRequest,
            error: "egress is disabled on this node (EMBER_NODE_EGRESS)".into(),
            actual_sha256: None,
        };
        return (StatusCode::FORBIDDEN, Json(body)).into_response();
    }
    ws.on_upgrade(move |socket| async move {
        let (ours, socks) = tokio::io::duplex(PIPE_BUF);
        let server = tokio::spawn(async move {
            if let Err(e) = serve_socks5(socks, SocksAuth::None, policy).await {
                tracing::debug!("egress stream: {e:#}");
            }
        });
        pump_axum(socket, ours).await;
        // Dropping our end of the pipe ends the SOCKS5 server (and its outbound connection).
        server.abort();
    })
}

/// Copy bytes between an axum WebSocket and `io`, with the empty-frame half-close convention.
async fn pump_axum<IO: AsyncRead + AsyncWrite>(ws: WebSocket, io: IO) {
    let (mut sink, mut stream) = ws.split();
    let (mut rd, mut wr) = tokio::io::split(io);
    let mut buf = vec![0u8; PIPE_BUF];
    let (mut in_eof, mut out_eof) = (false, false);
    loop {
        if in_eof && out_eof {
            let _ = sink.send(Message::Close(None)).await;
            break;
        }
        tokio::select! {
            m = stream.next() => {
                let bytes: Vec<u8> = match m {
                    Some(Ok(Message::Binary(b))) => b.to_vec(),
                    Some(Ok(Message::Text(t))) => t.as_str().as_bytes().to_vec(),
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                    _ => break,
                };
                if bytes.is_empty() {
                    in_eof = true;
                    let _ = wr.shutdown().await;
                } else if !in_eof && wr.write_all(&bytes).await.is_err() {
                    break;
                }
            }
            r = rd.read(&mut buf), if !out_eof => match r {
                Ok(0) | Err(_) => {
                    out_eof = true;
                    if sink.send(Message::Binary(Vec::<u8>::new().into())).await.is_err() { break; }
                }
                Ok(n) => {
                    if sink.send(Message::Binary(buf[..n].to_vec().into())).await.is_err() { break; }
                }
            },
        }
    }
    let _ = sink.close().await;
}

// ---------------------------------------------------------------------------------------------
// Client side

/// Client side of `/v1/egress`: one raw SOCKS5 byte stream to the node.
pub type EgressStream = tokio_tungstenite::WebSocketStream<crate::client::NodeIo>;

impl NodeClient {
    /// Open `/v1/egress`. Bytes go both ways as Binary frames; see the module docs.
    pub async fn egress(&self) -> Result<EgressStream, ClientError> {
        self.websocket("/v1/egress").await
    }
}

/// Copy bytes between `io` (e.g. a TCP connection from Chrome) and an `/v1/egress` stream until
/// both directions are done or either side closes. ember server's loopback SOCKS listener calls
/// this once per accepted connection.
pub async fn bridge<IO: AsyncRead + AsyncWrite>(io: IO, ws: EgressStream) -> anyhow::Result<()> {
    use tokio_tungstenite::tungstenite::Message as TMessage;
    let (mut sink, mut stream) = ws.split();
    let (mut rd, mut wr) = tokio::io::split(io);
    let mut buf = vec![0u8; PIPE_BUF];
    let (mut in_eof, mut out_eof) = (false, false);
    loop {
        if in_eof && out_eof {
            let _ = sink.send(TMessage::Close(None)).await;
            break;
        }
        tokio::select! {
            m = stream.next() => {
                let bytes: Vec<u8> = match m {
                    Some(Ok(TMessage::Binary(b))) => b.to_vec(),
                    Some(Ok(TMessage::Text(t))) => t.as_str().as_bytes().to_vec(),
                    Some(Ok(TMessage::Ping(_) | TMessage::Pong(_) | TMessage::Frame(_))) => continue,
                    Some(Ok(TMessage::Close(_))) | None => break,
                    Some(Err(e)) => return Err(e.into()),
                };
                if bytes.is_empty() {
                    in_eof = true;
                    let _ = wr.shutdown().await;
                } else if !in_eof {
                    wr.write_all(&bytes).await?;
                }
            }
            r = rd.read(&mut buf), if !out_eof => match r {
                Ok(0) | Err(_) => {
                    out_eof = true;
                    sink.send(TMessage::binary(Vec::<u8>::new())).await?;
                }
                Ok(n) => sink.send(TMessage::binary(buf[..n].to_vec())).await?,
            },
        }
    }
    let _ = sink.close().await;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// A minimal SOCKS5 client (tests, diagnostics)

/// Perform a no-auth (or, with `password`, RFC 1929) SOCKS5 CONNECT on `s`. Returns the reply
/// code (`0` = succeeded); on success `s` is then connected to the target.
pub async fn client_connect<S>(s: &mut S, target: &Target, password: Option<&str>) -> anyhow::Result<u8>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let method = if password.is_some() { METHOD_USER_PASS } else { METHOD_NO_AUTH };
    s.write_all(&[SOCKS_VERSION, 1, method]).await?;
    let mut resp = [0u8; 2];
    s.read_exact(&mut resp).await?;
    ensure!(resp[0] == SOCKS_VERSION, "bad version {}", resp[0]);
    ensure!(resp[1] == method, "server chose method {:#04x}", resp[1]);
    if let Some(p) = password {
        let mut m = vec![0x01, 1, b'u', p.len() as u8];
        m.extend_from_slice(p.as_bytes());
        s.write_all(&m).await?;
        let mut r = [0u8; 2];
        s.read_exact(&mut r).await?;
        ensure!(r[1] == 0, "authentication refused");
    }
    let mut req = vec![SOCKS_VERSION, CMD_CONNECT, 0];
    match target {
        Target::Ip(SocketAddr::V4(a)) => {
            req.push(ATYP_IPV4);
            req.extend_from_slice(&a.ip().octets());
            req.extend_from_slice(&a.port().to_be_bytes());
        }
        Target::Ip(SocketAddr::V6(a)) => {
            req.push(ATYP_IPV6);
            req.extend_from_slice(&a.ip().octets());
            req.extend_from_slice(&a.port().to_be_bytes());
        }
        Target::Domain(h, p) => {
            req.push(ATYP_DOMAIN);
            req.push(h.len() as u8);
            req.extend_from_slice(h.as_bytes());
            req.extend_from_slice(&p.to_be_bytes());
        }
    }
    s.write_all(&req).await?;
    let mut head = [0u8; 4];
    s.read_exact(&mut head).await?;
    let rest = match head[3] {
        ATYP_IPV4 => 4 + 2,
        ATYP_IPV6 => 16 + 2,
        ATYP_DOMAIN => s.read_u8().await? as usize + 2,
        other => bail!("bad reply address type {other}"),
    };
    let mut skip = vec![0u8; rest];
    s.read_exact(&mut skip).await?;
    Ok(head[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A TCP echo server; returns its address.
    async fn echo(bind: &str) -> Option<SocketAddr> {
        let l = TcpListener::bind(bind).await.ok()?;
        let addr = l.local_addr().ok()?;
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let (mut r, mut w) = s.split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                });
            }
        });
        Some(addr)
    }

    /// Start `serve_socks5` on one end of a pipe; return the other end.
    fn socks(auth: SocksAuth, policy: EgressPolicy) -> tokio::io::DuplexStream {
        let (a, b) = tokio::io::duplex(PIPE_BUF);
        tokio::spawn(serve_socks5(b, auth, Arc::new(policy)));
        a
    }

    async fn round_trip<S: AsyncRead + AsyncWrite + Unpin>(s: &mut S) {
        s.write_all(b"ping").await.unwrap();
        let mut got = [0u8; 4];
        s.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"ping");
    }

    #[tokio::test]
    async fn connect_ipv4_domain_and_ipv6() {
        let v4 = echo("127.0.0.1:0").await.unwrap();
        let mut s = socks(SocksAuth::None, EgressPolicy::default());
        assert_eq!(client_connect(&mut s, &Target::Ip(v4), None).await.unwrap(), reply::SUCCEEDED);
        round_trip(&mut s).await;

        let mut s = socks(SocksAuth::None, EgressPolicy::default());
        let t = Target::Domain("localhost".into(), v4.port());
        assert_eq!(client_connect(&mut s, &t, None).await.unwrap(), reply::SUCCEEDED);
        round_trip(&mut s).await;

        // IPv6 only where the host has it.
        if let Some(v6) = echo("[::1]:0").await {
            let mut s = socks(SocksAuth::None, EgressPolicy::default());
            assert_eq!(client_connect(&mut s, &Target::Ip(v6), None).await.unwrap(), reply::SUCCEEDED);
            round_trip(&mut s).await;
        }
    }

    #[tokio::test]
    async fn refusals() {
        let v4 = echo("127.0.0.1:0").await.unwrap();
        // Denied by policy.
        let deny = EgressPolicy::with_deny_list("loopback").unwrap();
        let mut s = socks(SocksAuth::None, deny.clone());
        assert_eq!(client_connect(&mut s, &Target::Ip(v4), None).await.unwrap(), reply::NOT_ALLOWED);
        let mut s = socks(SocksAuth::None, deny);
        let t = Target::Domain("localhost".into(), v4.port());
        assert_eq!(client_connect(&mut s, &t, None).await.unwrap(), reply::NOT_ALLOWED);

        // Nothing listening.
        let closed = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap()
        };
        let mut s = socks(SocksAuth::None, EgressPolicy::default());
        assert_eq!(client_connect(&mut s, &Target::Ip(closed), None).await.unwrap(), reply::CONNECTION_REFUSED);

        // BIND is not supported.
        let mut s = socks(SocksAuth::None, EgressPolicy::default());
        s.write_all(&[5, 1, 0]).await.unwrap();
        let mut r = [0u8; 2];
        s.read_exact(&mut r).await.unwrap();
        s.write_all(&[5, 0x02, 0, ATYP_IPV4, 127, 0, 0, 1, 0, 80]).await.unwrap();
        let mut head = [0u8; 10];
        s.read_exact(&mut head).await.unwrap();
        assert_eq!(head[1], reply::COMMAND_NOT_SUPPORTED);
    }

    #[tokio::test]
    async fn password_auth() {
        let v4 = echo("127.0.0.1:0").await.unwrap();
        let mut s = socks(SocksAuth::Password("tok".into()), EgressPolicy::default());
        assert_eq!(client_connect(&mut s, &Target::Ip(v4), Some("tok")).await.unwrap(), 0);
        round_trip(&mut s).await;

        let mut s = socks(SocksAuth::Password("tok".into()), EgressPolicy::default());
        assert!(client_connect(&mut s, &Target::Ip(v4), Some("nope")).await.is_err());
        // A client offering only "no auth" is turned away.
        let mut s = socks(SocksAuth::Password("tok".into()), EgressPolicy::default());
        assert!(client_connect(&mut s, &Target::Ip(v4), None).await.is_err());
    }

    #[test]
    fn policy_and_peers() {
        let p = EgressPolicy::with_deny_list("private, link-local, 203.0.113.0/24").unwrap();
        for ip in ["10.1.2.3", "192.168.0.1", "172.31.255.255", "169.254.169.254", "fd00::1", "::ffff:10.0.0.1", "203.0.113.9"] {
            assert!(!p.allows(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["127.0.0.1", "8.8.8.8", "172.32.0.1", "2001:db8::1", "::1"] {
            assert!(p.allows(ip.parse().unwrap()), "{ip}");
        }
        assert!(EgressPolicy::default().allows("10.0.0.1".parse().unwrap()));
        assert!(EgressPolicy::with_deny_list("10.0.0.0/33").is_err());
        assert!(EgressPolicy::with_deny_list("nonsense").is_err());

        assert_eq!(auth_for_peer("127.0.0.1:5".parse().unwrap(), "t"), SocksAuth::None);
        assert_eq!(auth_for_peer("[::1]:5".parse().unwrap(), "t"), SocksAuth::None);
        assert_eq!(auth_for_peer("192.168.1.2:5".parse().unwrap(), "t"), SocksAuth::Password("t".into()));
    }
}
