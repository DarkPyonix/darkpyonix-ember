//! The agent CLI's own API traffic leaves through the session's current computer (SPEC FR-X6,
//! INTENT D19), so the model provider sees that computer's IP.
//!
//! ```text
//! agent CLI --HTTPS_PROXY=http://ember:<pw>@127.0.0.1:<port>--> session proxy (this server)
//!    CONNECT host:port ─▶ route(session)
//!       None (main server)  ─▶ TCP connect from this server
//!       Some(node)          ─▶ node `/egress` (SOCKS5 CONNECT, DNS on the node) ─▶ TCP target
//! ```
//!
//! One loopback HTTP CONNECT proxy runs per session, with its own random credential (any other
//! local process lacks it). The route is asked **on every CONNECT**, so after a computer switch
//! new connections follow the new computer; a tunnel already open stays on the computer it was
//! opened through until it closes. The node side is the existing `/egress` route
//! ([`ember_node::egress`]), so no new node API is needed. Plain `http://` proxying (absolute-URI
//! requests) is not supported: model providers speak HTTPS.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use base64::Engine;
use ember_node::client::NodeClient;
use ember_node::egress::{self, reply, Target};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::session::{EndHook, StartHook};

/// Where a session's traffic leaves now: `Ok(None)` is this server (direct), `Ok(Some(node))`
/// is the node of the session's current computer. Called once per CONNECT.
pub type Route = Arc<dyn Fn(&str) -> anyhow::Result<Option<NodeClient>> + Send + Sync>;

const MAX_HEAD: usize = 16 * 1024;
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);

/// The per-session proxies of this server.
pub struct SessionEgress {
    route: Route,
    proxies: Mutex<HashMap<String, Proxy>>,
}

struct Proxy {
    addr: SocketAddr,
    password: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl SessionEgress {
    pub fn new(route: Route) -> Arc<SessionEgress> {
        Arc::new(SessionEgress { route, proxies: Mutex::new(HashMap::new()) })
    }

    /// The proxy environment for an agent process of `session_id`, starting the session's proxy
    /// on first use. Needs a Tokio runtime. `NO_PROXY` keeps loopback (A2A, MCP, browser) direct.
    pub fn env_for(&self, session_id: &str) -> anyhow::Result<Vec<(String, String)>> {
        let url = self.proxy_url(session_id)?;
        let no = "localhost,127.0.0.1,::1".to_string();
        let mut env = Vec::new();
        for k in ["HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy"] {
            env.push((k.to_string(), url.clone()));
        }
        env.push(("NO_PROXY".into(), no.clone()));
        env.push(("no_proxy".into(), no));
        Ok(env)
    }

    /// `http://ember:<password>@127.0.0.1:<port>` of the session's proxy.
    pub fn proxy_url(&self, session_id: &str) -> anyhow::Result<String> {
        let mut proxies = self.proxies.lock().unwrap();
        if let Some(p) = proxies.get(session_id) {
            return Ok(format!("http://ember:{}@{}", p.password, p.addr));
        }
        let handle = tokio::runtime::Handle::try_current().context("the session proxy needs a Tokio runtime")?;
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        std_listener.set_nonblocking(true)?;
        let addr = std_listener.local_addr()?;
        let listener = {
            let _g = handle.enter();
            TcpListener::from_std(std_listener)?
        };
        let password = uuid::Uuid::new_v4().simple().to_string();
        let (route, sid, pw) = (self.route.clone(), session_id.to_string(), password.clone());
        let task = handle.spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                };
                let _ = stream.set_nodelay(true);
                let (route, sid, pw) = (route.clone(), sid.clone(), pw.clone());
                tokio::spawn(async move {
                    if let Err(e) = serve(stream, &sid, &pw, &route).await {
                        tracing::debug!(session = %sid, "egress proxy connection ended: {e:#}");
                    }
                });
            }
        });
        let url = format!("http://ember:{password}@{addr}");
        proxies.insert(session_id.to_string(), Proxy { addr, password, task });
        Ok(url)
    }

    /// Stop the session's proxy (open tunnels finish on their own).
    pub fn stop(&self, session_id: &str) {
        self.proxies.lock().unwrap().remove(session_id);
    }

    /// An end hook stopping the session's proxy when the session ends.
    pub fn end_hook(self: Arc<Self>) -> EndHook {
        Arc::new(move |id| self.stop(id))
    }

    /// A start hook adding the proxy environment to every agent process (FR-X6).
    pub fn start_hook(self: Arc<Self>) -> StartHook {
        Arc::new(move |rec| match self.env_for(&rec.id) {
            Ok(env) => env,
            Err(e) => {
                tracing::warn!(session = %rec.id, "agent egress proxy unavailable: {e:#}");
                Vec::new()
            }
        })
    }
}

async fn respond<S: AsyncWrite + Unpin>(s: &mut S, status: &str, extra: &str) -> std::io::Result<()> {
    s.write_all(format!("HTTP/1.1 {status}\r\n{extra}Content-Length: 0\r\nConnection: close\r\n\r\n").as_bytes())
        .await?;
    s.flush().await
}

/// Parse `host:port` / `[v6]:port` into a SOCKS target.
fn parse_authority(a: &str) -> Option<Target> {
    let (host, port) = a.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() || host.len() > 255 {
        return None;
    }
    Some(match host.parse::<std::net::IpAddr>() {
        Ok(ip) => Target::Ip(SocketAddr::new(ip, port)),
        Err(_) => Target::Domain(host.to_string(), port),
    })
}

async fn serve(mut client: TcpStream, session_id: &str, password: &str, route: &Route) -> anyhow::Result<()> {
    // Read the request head.
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        anyhow::ensure!(buf.len() < MAX_HEAD, "request head too large");
        let n = tokio::time::timeout(HEAD_TIMEOUT, client.read(&mut chunk)).await??;
        anyhow::ensure!(n > 0, "closed before the request head ended");
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..end]).into_owned();
    let leftover = buf[end..].to_vec();
    let mut lines = head.split("\r\n");
    let first = lines.next().unwrap_or("");
    let mut parts = first.split(' ');
    let (method, authority) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));

    let expected = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("ember:{password}"))
    );
    let authed = lines.any(|l| {
        l.split_once(':').is_some_and(|(k, v)| {
            k.eq_ignore_ascii_case("proxy-authorization") && v.trim() == expected
        })
    });
    if !authed {
        respond(&mut client, "407 Proxy Authentication Required", "Proxy-Authenticate: Basic realm=\"ember\"\r\n").await?;
        return Ok(());
    }
    if !method.eq_ignore_ascii_case("CONNECT") {
        respond(&mut client, "405 Method Not Allowed", "Allow: CONNECT\r\n").await?;
        return Ok(());
    }
    let Some(target) = parse_authority(authority) else {
        respond(&mut client, "400 Bad Request", "").await?;
        return Ok(());
    };

    let upstream = match open(session_id, &target, route).await {
        Ok(u) => u,
        Err((status, why)) => {
            tracing::warn!(session = %session_id, %target, "egress CONNECT failed: {why}");
            respond(&mut client, status, "").await?;
            return Ok(());
        }
    };
    client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;
    let mut upstream = upstream;
    if !leftover.is_empty() {
        upstream.write_all(&leftover).await?;
    }
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    Ok(())
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// Open the outbound connection for `target` from wherever the session is now.
async fn open(session_id: &str, target: &Target, route: &Route) -> Result<Box<dyn Io>, (&'static str, String)> {
    let node = route(session_id).map_err(|e| ("502 Bad Gateway", format!("no route: {e:#}")))?;
    match node {
        None => {
            let connect = async {
                match target {
                    Target::Ip(a) => TcpStream::connect(a).await,
                    Target::Domain(h, p) => TcpStream::connect((h.as_str(), *p)).await,
                }
            };
            match tokio::time::timeout(egress::CONNECT_TIMEOUT, connect).await {
                Ok(Ok(s)) => {
                    let _ = s.set_nodelay(true);
                    Ok(Box::new(s))
                }
                Ok(Err(e)) => Err(("502 Bad Gateway", e.to_string())),
                Err(_) => Err(("504 Gateway Timeout", "connect timed out".into())),
            }
        }
        Some(node) => {
            let ws = node.egress().await.map_err(|e| ("502 Bad Gateway", format!("node /egress: {e}")))?;
            // `bridge` copies bytes between a stream and the WebSocket; the SOCKS5 client runs
            // on the near end, then the same stream carries the tunnel.
            let (mut near, far) = tokio::io::duplex(64 * 1024);
            tokio::spawn(async move {
                let _ = egress::bridge(far, ws).await;
            });
            let code = tokio::time::timeout(egress::HANDSHAKE_TIMEOUT, egress::client_connect(&mut near, target, None))
                .await
                .map_err(|_| ("504 Gateway Timeout", "node handshake timed out".to_string()))?
                .map_err(|e| ("502 Bad Gateway", format!("node handshake: {e:#}")))?;
            match code {
                reply::SUCCEEDED => Ok(Box::new(near)),
                reply::NOT_ALLOWED => Err(("403 Forbidden", "the node's egress policy denies it".into())),
                c => Err(("502 Bad Gateway", format!("node refused with SOCKS reply {c:#04x}"))),
            }
        }
    }
}
