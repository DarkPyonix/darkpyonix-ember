//! Connecting to a Code-OSS server: HTTP upgrade, then auth → sign → connectionType.
//!
//! Source: `src/vs/platform/remote/common/remoteAgentConnection.ts`
//! (`connectToRemoteExtensionHostAgent` L228-310, `connectToRemoteExtensionHostAgentAndReadOneMessage`)
//! and the server side `src/vs/server/node/remoteExtensionHostAgentServer.ts`
//! (`handleUpgrade` L201-237, `_handleWebSocketConnection` L274-397, `_handleConnectionType` L400-520).
//!
//! The upgrade uses `skipWebSocketFrames=true`, the same raw mode `managedSocket.ts`
//! (`makeRawSocketHeaders`, L11-26) uses: after the `101` response the socket carries
//! `PersistentProtocol` frames directly, with no WebSocket framing or permessage-deflate. That is
//! what lets this crate run over any byte stream.
//!
//! **Version gate.** Before the first [`connect`], call [`verify_server`] on a fresh stream: it
//! reads `GET /version` (agent server L145-148, served without a connection token) and returns
//! [`Error::UnsupportedServerVersion`] unless the server is [`crate::PINNED_COMMIT`]. Put the
//! returned commit in [`ConnectOptions::commit`]; the server then repeats the check itself
//! (L351-357) and [`connect`] refuses any other value before touching the stream.

use std::future::Future;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::connection::{Connection, ConnectionHandle};
use crate::protocol::RECONNECT_BACKOFF_SECS;
use crate::{Error, Result};

/// `ConnectionType` (remoteAgentConnection.ts L26-30).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ConnectionType {
    Management = 1,
    ExtensionHost = 2,
    Tunnel = 3,
}

/// `IRemoteExtensionHostStartParams` (L348-354), sent as `args` of an ExtensionHost connection.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtensionHostStartParams {
    pub language: String,
    #[serde(rename = "debugId", skip_serializing_if = "Option::is_none")]
    pub debug_id: Option<String>,
    #[serde(rename = "break", skip_serializing_if = "Option::is_none")]
    pub break_on_start: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::BTreeMap<String, Option<String>>>,
}

/// The cookie the DarkPyonix manager authorises editor requests by (`FR-W4c` (1c), INTENT D55).
pub const TICKET_COOKIE: &str = "dpx-editor";

/// The manager's editor ticket (`ose.ticket` of the launch). It authorises every request under
/// the editor server's path, so it never reaches a log: `Debug` prints `[REDACTED]`.
#[derive(Clone, PartialEq, Eq)]
pub struct Ticket(String);

impl Ticket {
    /// `None` for an empty ticket (the demo's direct OSE, or a launch without one).
    pub fn new(ticket: impl Into<String>) -> Option<Ticket> {
        let ticket = ticket.into();
        (!ticket.is_empty()).then_some(Ticket(ticket))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A ticket is one cookie value (RFC 6265 `cookie-octet`): visible ASCII without `"`, `,`,
    /// `;` or `\`, so it cannot add a header line or a second cookie.
    pub fn is_cookie_value(ticket: &str) -> bool {
        !ticket.is_empty() && ticket.bytes().all(|b| (0x21..=0x7e).contains(&b) && !matches!(b, b'"' | b',' | b';' | b'\\'))
    }
}

impl std::fmt::Debug for Ticket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Ticket([REDACTED])")
    }
}

/// Everything needed to (re)open one logical connection.
#[derive(Debug, Clone)]
pub struct ConnectOptions {
    /// `Host` header value, e.g. `"127.0.0.1:8000"`.
    pub host: String,
    /// The editor server's base path, `/` for a server at the root. The DarkPyonix manager serves
    /// it under `ose.path` (`/ide/ose/`, `--server-base-path`), so every request of this crate
    /// (the upgrades and `GET /version`) goes below it (`FR-W4c` (1c), INTENT D55).
    pub path: String,
    /// The manager's ticket, sent as `Cookie: dpx-editor=<ticket>` on every request. The editor
    /// server itself has no connection token, so the `auth` message always carries
    /// [`NO_CONNECTION_TOKEN`].
    pub ticket: Option<Ticket>,
    /// Identifies this logical connection across reconnects. One per connection, never reused.
    pub reconnection_token: String,
    /// Our idea of the server commit. When both sides set it, the server refuses a mismatch
    /// ("Client refused: version mismatch"). Use the value returned by [`verify_server`]; any
    /// value other than [`crate::PINNED_COMMIT`] makes [`connect`] fail with
    /// [`Error::UnsupportedServerVersion`].
    pub commit: Option<String>,
    /// Timeout for the whole handshake.
    pub timeout: Duration,
}

impl ConnectOptions {
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            path: "/".into(),
            ticket: None,
            reconnection_token: uuid::Uuid::new_v4().to_string(),
            commit: None,
            timeout: Duration::from_secs(30),
        }
    }

    /// The query string (`connectToRemoteExtensionHostAgent` L235 + `skipWebSocketFrames`).
    pub fn query(&self, reconnection: bool) -> String {
        format!(
            "reconnectionToken={}&reconnection={}&skipWebSocketFrames=true",
            self.reconnection_token, reconnection
        )
    }

    /// The base path with exactly one trailing `/` (`/ide/ose/`, or `/`).
    fn base(&self) -> String {
        format!("{}/", self.path.trim_end_matches('/'))
    }

    /// The `Cookie` header line for the ticket, or nothing.
    fn cookie_line(&self) -> String {
        match &self.ticket {
            Some(t) => format!("Cookie: {TICKET_COOKIE}={}\r\n", t.as_str()),
            None => String::new(),
        }
    }

    /// Refuses a path or ticket that would break the request line or add a header line.
    fn check(&self) -> Result<()> {
        let path_ok = self.path.starts_with('/')
            && !self.path.contains(['?', '#'])
            && !self.path.chars().any(|c| c.is_whitespace() || c.is_control());
        if !path_ok {
            return Err(Error::Handshake(format!("not an editor server path: {:?}", self.path)));
        }
        if self.ticket.as_ref().is_some_and(|t| !Ticket::is_cookie_value(t.as_str())) {
            return Err(Error::Handshake("the editor ticket is not a cookie value".into()));
        }
        Ok(())
    }
}

/// Handshake control messages (`HandshakeMessage`, L44-74).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HandshakeMessage {
    Auth {
        auth: String,
        data: String,
    },
    Sign {
        data: String,
        #[serde(rename = "signedData")]
        signed_data: String,
    },
    ConnectionType {
        #[serde(skip_serializing_if = "Option::is_none")]
        commit: Option<String>,
        #[serde(rename = "signedData")]
        signed_data: String,
        #[serde(rename = "desiredConnectionType")]
        desired_connection_type: u8,
        #[serde(skip_serializing_if = "Option::is_none")]
        args: Option<Value>,
    },
    Error {
        reason: String,
    },
    Ok,
}

/// The `auth` value when there is no connection token (L260), as the upstream web client sends
/// it. The editor server behind the DarkPyonix manager has none (`FR-W4c` (1c), INTENT D55), so
/// this is the only value the `auth` message carries.
pub const NO_CONNECTION_TOKEN: &str = "00000000000000000000";

/// Build the raw upgrade request (cf. `makeRawSocketHeaders`): `GET <path>?<query>` below the
/// editor server's base path, with the ticket cookie when there is one. [`upgrade`] checks the
/// path and the ticket before it writes this.
pub fn upgrade_request(opts: &ConnectOptions, reconnection: bool) -> String {
    let key = base64_16(uuid::Uuid::new_v4().as_bytes());
    format!(
        "GET {}?{} HTTP/1.1\r\nHost: {}\r\n{}Connection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {}\r\n\r\n",
        opts.base(),
        opts.query(reconnection),
        opts.host,
        opts.cookie_line(),
        key
    )
}

/// Write the upgrade request and read the response headers. Returns any bytes read past the
/// header terminator (they belong to the first protocol frame).
pub async fn upgrade<S>(stream: &mut S, opts: &ConnectOptions, reconnection: bool) -> Result<Vec<u8>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    opts.check()?;
    stream.write_all(upgrade_request(opts, reconnection).as_bytes()).await?;
    stream.flush().await?;

    let mut buf = Vec::with_capacity(512);
    let mut chunk = [0u8; 512];
    loop {
        if let Some(end) = find_header_end(&buf) {
            check_upgrade_response(&buf[..end])?;
            return Ok(buf[end + 4..].to_vec());
        }
        if buf.len() > 16 * 1024 {
            return Err(Error::Handshake("upgrade response headers too large".into()));
        }
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(Error::Handshake("socket closed during upgrade".into()));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn check_upgrade_response(head: &[u8]) -> Result<()> {
    let text = String::from_utf8_lossy(head);
    let status = text.lines().next().unwrap_or_default();
    let code = status.split_whitespace().nth(1).unwrap_or_default();
    if code == "101" {
        Ok(())
    } else {
        Err(Error::Handshake(format!("upgrade refused: {status}")))
    }
}

/// The server's refusal reason when the `connectionType` commit differs from its own
/// (agent server L351-357).
pub const VERSION_MISMATCH_REASON: &str = "Client refused: version mismatch";

fn parse_control(raw: &[u8]) -> Result<Value> {
    let v: Value = serde_json::from_slice(raw)?;
    if v.get("type").and_then(Value::as_str) == Some("error") {
        let reason = v.get("reason").and_then(Value::as_str).unwrap_or("unknown").to_owned();
        if reason == VERSION_MISMATCH_REASON {
            return Err(Error::UnsupportedServerVersion { server: None });
        }
        return Err(Error::Refused(reason));
    }
    Ok(v)
}

/// The `GET /version` request, below the base path and with the ticket cookie. HTTP/1.0 so that
/// Node answers with a plain body and closes the socket (no chunked encoding).
pub fn version_request(opts: &ConnectOptions) -> String {
    format!(
        "GET {}version HTTP/1.0\r\nHost: {}\r\n{}Connection: close\r\n\r\n",
        opts.base(),
        opts.host,
        opts.cookie_line()
    )
}

/// Read the server commit with `GET /version` over a fresh `stream` (consumed; the server closes
/// it). An empty string means a dev server without a product commit.
pub async fn server_commit<S>(mut stream: S, opts: &ConnectOptions) -> Result<String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    opts.check()?;
    let fut = async {
        stream.write_all(version_request(opts).as_bytes()).await?;
        stream.flush().await?;
        let mut buf = Vec::with_capacity(512);
        let mut chunk = [0u8; 512];
        loop {
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.len() > 16 * 1024 {
                return Err(Error::Handshake("GET /version response too large".into()));
            }
        }
        parse_version_response(&buf)
    };
    tokio::time::timeout(opts.timeout, fut).await.map_err(|_| Error::Timeout("GET /version"))?
}

fn parse_version_response(raw: &[u8]) -> Result<String> {
    let end = find_header_end(raw).ok_or_else(|| Error::Handshake("GET /version: truncated response".into()))?;
    let head = String::from_utf8_lossy(&raw[..end]);
    let status = head.lines().next().unwrap_or_default();
    match status.split_whitespace().nth(1) {
        Some("200") => {}
        // No `/version` route (not a Code-OSS agent server, or one too old/new to tell): the
        // commit is unknown, which the version gate treats like a mismatch.
        Some("404") => return Err(Error::UnsupportedServerVersion { server: None }),
        // A wrong or missing connection token (the agent server checks it before `/version`),
        // or an auth gate in front of it. FR-E10: auth errors never switch targets.
        Some(code @ ("401" | "403")) => return Err(Error::Refused(format!("GET /version: HTTP {code}"))),
        // A relay or proxy that cannot reach the server (502, 503, 504), or a server error.
        // FR-E10: network errors never switch targets.
        code => return Err(Error::Handshake(format!("GET /version: HTTP {}", code.unwrap_or("status missing")))),
    }
    let body = &raw[end + 4..];
    let chunked = head.lines().skip(1).any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    let body = if chunked { dechunk(body)? } else { body.to_vec() };
    Ok(String::from_utf8_lossy(&body).trim().to_owned())
}

/// Minimal `Transfer-Encoding: chunked` decoder (no trailers, no extensions).
fn dechunk(mut body: &[u8]) -> Result<Vec<u8>> {
    let bad = || Error::Handshake("GET /version: malformed chunked body".into());
    let mut out = Vec::new();
    loop {
        let line_end = body.windows(2).position(|w| w == b"\r\n").ok_or_else(bad)?;
        let size_str = String::from_utf8_lossy(&body[..line_end]);
        let size = usize::from_str_radix(size_str.split(';').next().unwrap_or_default().trim(), 16).map_err(|_| bad())?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if body.len() < size {
            return Err(bad());
        }
        out.extend_from_slice(&body[..size]);
        body = body.get(size + 2..).ok_or_else(bad)?;
    }
}

/// The connect-time version gate: read `GET /version` over `stream` and accept only
/// [`crate::PINNED_COMMIT`]. Returns the commit to put in [`ConnectOptions::commit`]; otherwise
/// [`Error::UnsupportedServerVersion`].
pub async fn verify_server<S>(stream: S, opts: &ConnectOptions) -> Result<String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let commit = server_commit(stream, opts).await?;
    crate::check_server_commit(&commit)?;
    Ok(commit)
}

/// Run auth → sign → connectionType on an attached transport and return the server's first
/// reply (`{"type":"ok"}` for Management, `{}` or `{"debugPort":n}` for ExtensionHost).
///
/// Signing: Code-OSS without the proprietary `vsda` module leaves `signService.sign` as the
/// identity and skips validation (`abstractSignService.ts`), and a server without `vsda` accepts
/// any `signedData` (agent server L362-364), so we echo the challenge. The editor server runs
/// without a connection token (`--without-connection-token`, `FR-W4a`, INTENT D55), whose
/// `validate` accepts any `auth`; the request is authorised by the manager's ticket cookie
/// instead. We never validate the server's signature (that needs `vsda`); authenticity of the
/// server is the transport's job (TLS / ember-transport).
pub async fn handshake(
    handle: &ConnectionHandle,
    opts: &ConnectOptions,
    ty: ConnectionType,
    args: Option<Value>,
) -> Result<Value> {
    let fut = async {
        let auth = HandshakeMessage::Auth {
            auth: NO_CONNECTION_TOKEN.to_owned(),
            data: uuid::Uuid::new_v4().to_string(),
        };
        handle.send_control(serde_json::to_vec(&auth)?);

        let sign = tokio::time::timeout(Duration::from_secs(10), handle.recv_control())
            .await
            .map_err(|_| Error::Timeout("sign request"))??;
        let sign = parse_control(&sign)?;
        let challenge = match serde_json::from_value::<HandshakeMessage>(sign)? {
            HandshakeMessage::Sign { data, .. } => data,
            other => return Err(Error::Handshake(format!("expected sign, got {other:?}"))),
        };

        let conn_type = HandshakeMessage::ConnectionType {
            commit: opts.commit.clone(),
            signed_data: challenge,
            desired_connection_type: ty as u8,
            args,
        };
        handle.send_control(serde_json::to_vec(&conn_type)?);

        let first = handle.recv_control().await?;
        parse_control(&first)
    };
    tokio::time::timeout(opts.timeout, fut).await.map_err(|_| Error::Timeout("handshake"))?
}

/// Open a new logical connection over `stream`.
pub async fn connect<S>(
    mut stream: S,
    opts: &ConnectOptions,
    ty: ConnectionType,
    args: Option<Value>,
) -> Result<(Connection, Value)>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    if let Some(commit) = &opts.commit {
        crate::check_server_commit(commit)?;
    }
    let leftover = upgrade(&mut stream, opts, false).await?;
    let conn = Connection::new(stream, leftover);
    let first = handshake(&conn.handle, opts, ty, args).await?;
    Ok((conn, first))
}

/// Re-attach an existing logical connection over a fresh `stream` (same reconnection token).
/// On success unacknowledged messages are replayed; regular traffic resumes transparently.
pub async fn reconnect<S>(
    handle: &ConnectionHandle,
    mut stream: S,
    opts: &ConnectOptions,
    ty: ConnectionType,
    args: Option<Value>,
) -> Result<Value>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let leftover = upgrade(&mut stream, opts, true).await?;
    handle.drain_control().await;
    handle.replace_transport(stream, leftover);
    let first = handshake(handle, opts, ty, args).await?;
    handle.finish_reconnect();
    Ok(first)
}

/// Is this handshake error permanent (never retry)? Mirrors the reconnect loop's rules:
/// an `error` control message (unknown/duplicate reconnection token, auth mismatch, version
/// mismatch) is fatal; network failures and timeouts are retried.
pub fn is_permanent(err: &Error) -> bool {
    matches!(err, Error::Refused(_) | Error::UnsupportedServerVersion { .. })
}

/// The reconnect loop (`PersistentConnection._runReconnectingLoop`): back off
/// 0, 5, 5, 10, … 30 s, retry transient failures until `grace` elapses.
pub async fn reconnect_loop<F, Fut, S>(
    handle: &ConnectionHandle,
    mut dial: F,
    opts: &ConnectOptions,
    ty: ConnectionType,
    args: Option<Value>,
    grace: Duration,
) -> Result<Value>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = std::io::Result<S>>,
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let started = tokio::time::Instant::now();
    let mut attempt = 0usize;
    loop {
        let wait = RECONNECT_BACKOFF_SECS[attempt.min(RECONNECT_BACKOFF_SECS.len() - 1)];
        tokio::time::sleep(Duration::from_secs(wait)).await;
        attempt += 1;
        let result = match dial().await {
            Ok(stream) => reconnect(handle, stream, opts, ty, args.clone()).await,
            Err(e) => Err(Error::Io(e)),
        };
        match result {
            Ok(v) => return Ok(v),
            Err(e) if is_permanent(&e) => return Err(e),
            Err(e) => {
                tracing::info!("editor-conn: reconnect attempt {attempt} failed: {e}");
                if started.elapsed() >= grace {
                    return Err(e);
                }
            }
        }
    }
}

/// Minimal base64 for the 16-byte `Sec-WebSocket-Key` (avoids a dependency).
fn base64_16(bytes: &[u8; 16]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(24);
    for c in bytes.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{Frame, FrameDecoder, MessageType};

    #[test]
    fn handshake_messages_match_upstream_json() {
        let auth = HandshakeMessage::Auth { auth: NO_CONNECTION_TOKEN.into(), data: "d".into() };
        assert_eq!(
            serde_json::to_value(&auth).unwrap(),
            serde_json::json!({"type":"auth","auth":"00000000000000000000","data":"d"})
        );
        let ct = HandshakeMessage::ConnectionType {
            commit: None,
            signed_data: "s".into(),
            desired_connection_type: ConnectionType::ExtensionHost as u8,
            args: Some(serde_json::json!({"language":"en"})),
        };
        assert_eq!(
            serde_json::to_value(&ct).unwrap(),
            serde_json::json!({"type":"connectionType","signedData":"s","desiredConnectionType":2,"args":{"language":"en"}})
        );
        let sign: HandshakeMessage =
            serde_json::from_str(r#"{"type":"sign","data":"x","signedData":"y"}"#).unwrap();
        assert_eq!(sign, HandshakeMessage::Sign { data: "x".into(), signed_data: "y".into() });
    }

    #[test]
    fn base64_key_is_24_chars() {
        assert_eq!(base64_16(&[0u8; 16]), "AAAAAAAAAAAAAAAAAAAAAA==");
    }

    #[test]
    fn rejects_non_101() {
        assert!(check_upgrade_response(b"HTTP/1.1 403 Forbidden").is_err());
        assert!(check_upgrade_response(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket").is_ok());
    }

    /// A scripted fake server on a duplex pipe: answers the upgrade, then the handshake.
    #[tokio::test]
    async fn management_handshake_against_fake_server() {
        let (client, mut server) = tokio::io::duplex(64 * 1024);
        let server_task = tokio::spawn(async move {
            // 1. upgrade request
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            while find_header_end(&buf).is_none() {
                let n = server.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
            }
            let head = String::from_utf8_lossy(&buf).to_string();
            assert!(head.starts_with("GET /?reconnectionToken="));
            assert!(head.contains("&reconnection=false&skipWebSocketFrames=true HTTP/1.1"));
            assert!(!head.to_ascii_lowercase().contains("\r\ncookie:"), "no ticket, no cookie: {head}");
            let end = find_header_end(&buf).unwrap();
            let mut dec = FrameDecoder::new();
            dec.push(&buf[end + 4..]);
            server
                .write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n")
                .await
                .unwrap();

            // 2. auth
            let auth = loop {
                if let Some(f) = dec.next_frame().unwrap() {
                    break f;
                }
                let n = server.read(&mut chunk).await.unwrap();
                dec.push(&chunk[..n]);
            };
            assert_eq!(auth.ty, MessageType::Control);
            let auth: Value = serde_json::from_slice(&auth.data).unwrap();
            assert_eq!(auth["type"], "auth");
            assert_eq!(auth["auth"], NO_CONNECTION_TOKEN);

            let sign = br#"{"type":"sign","data":"challenge","signedData":"x"}"#.to_vec();
            server.write_all(&Frame::new(MessageType::Control, 0, 0, sign).encode()).await.unwrap();

            // 3. connectionType
            let ct = loop {
                if let Some(f) = dec.next_frame().unwrap() {
                    break f;
                }
                let n = server.read(&mut chunk).await.unwrap();
                dec.push(&chunk[..n]);
            };
            let ct: Value = serde_json::from_slice(&ct.data).unwrap();
            assert_eq!(ct["type"], "connectionType");
            assert_eq!(ct["desiredConnectionType"], 1);
            assert_eq!(ct["signedData"], "challenge");

            let ok = Frame::new(MessageType::Control, 0, 0, br#"{"type":"ok"}"#.to_vec()).encode();
            server.write_all(&ok).await.unwrap();
            server
        });

        let opts = ConnectOptions::new("localhost:8000");
        let (_conn, first) = connect(client, &opts, ConnectionType::Management, None).await.unwrap();
        assert_eq!(first, serde_json::json!({"type":"ok"}));
        server_task.await.unwrap();
    }

    /// Reads one request head from the fake server's end of the pipe.
    async fn read_head(server: &mut tokio::io::DuplexStream) -> String {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        while find_header_end(&buf).is_none() {
            let n = server.read(&mut chunk).await.unwrap();
            assert!(n > 0, "the client closed before its request head ended");
            buf.extend_from_slice(&chunk[..n]);
        }
        String::from_utf8_lossy(&buf[..find_header_end(&buf).unwrap() + 4]).into_owned()
    }

    fn manager_opts() -> ConnectOptions {
        let mut opts = ConnectOptions::new("localhost");
        opts.path = "/ide/ose/".into();
        opts.ticket = Ticket::new("tk-0123456789abcdef");
        opts
    }

    /// SPEC FR-W4c (1c), INTENT D55: the editor server is served by the DarkPyonix manager under
    /// `ose.path`, authorised by the ticket. The first bytes of an upgrade (and of `GET /version`)
    /// ask for the path below `/ide/ose/` and carry `Cookie: dpx-editor=<ticket>`.
    #[tokio::test]
    async fn fr_w4c_editor_connects_under_the_manager_path_with_the_ticket_cookie() {
        let (client, mut server) = tokio::io::duplex(64 * 1024);
        let opts = manager_opts();
        let task = tokio::spawn(async move {
            let head = read_head(&mut server).await;
            server.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n").await.unwrap();
            head
        });
        let err = connect(client, &opts, ConnectionType::Management, None).await.err().unwrap();
        assert!(matches!(err, Error::Handshake(ref m) if m.contains("403")), "{err}");
        let head = task.await.unwrap();
        let request_line = head.lines().next().unwrap();
        assert!(request_line.starts_with("GET /ide/ose/?reconnectionToken="), "{request_line}");
        assert!(request_line.ends_with("&reconnection=false&skipWebSocketFrames=true HTTP/1.1"), "{request_line}");
        assert!(head.contains("\r\nCookie: dpx-editor=tk-0123456789abcdef\r\n"), "{head}");
        assert_eq!(head.matches("Cookie:").count(), 1, "{head}");

        // A path without its trailing slash is the same path.
        let mut bare = manager_opts();
        bare.path = "/ide/ose".into();
        assert!(upgrade_request(&bare, true).starts_with("GET /ide/ose/?reconnectionToken="));

        // `GET /version` goes below the same path with the same cookie.
        assert_eq!(
            version_request(&opts),
            "GET /ide/ose/version HTTP/1.0\r\nHost: localhost\r\nCookie: dpx-editor=tk-0123456789abcdef\r\nConnection: close\r\n\r\n"
        );

        // The ticket never reaches a log.
        assert!(!format!("{opts:?}").contains("tk-0123456789abcdef"));
        assert_eq!(Ticket::new(""), None);
    }

    /// SPEC FR-W4c (1c), INTENT D55: a ticket or path that would add a header line or break the
    /// request line is refused before anything is written.
    #[tokio::test]
    async fn fr_w4c_a_bad_ticket_or_path_is_refused_before_writing() {
        for (path, ticket) in [("/ide/ose/", "a\r\nX-Evil: 1"), ("/ide/ose/", "a;b"), ("/ide/ose/", "a b"), ("/ide ose/", "t"), ("ide/ose/", "t"), ("/ide/ose/?x=1", "t")] {
            let (client, mut server) = tokio::io::duplex(1024);
            let mut opts = manager_opts();
            opts.path = path.into();
            opts.ticket = Ticket::new(ticket);
            let err = connect(client, &opts, ConnectionType::Management, None).await.err().unwrap();
            assert!(matches!(err, Error::Handshake(_)), "{path:?} {ticket:?}: {err}");
            let (c2, _s2) = tokio::io::duplex(1024);
            assert!(server_commit(c2, &opts).await.is_err(), "{path:?} {ticket:?}");
            let mut rest = Vec::new();
            let _ = server.read_to_end(&mut rest).await;
            assert!(rest.is_empty(), "nothing was written for {path:?} {ticket:?}");
        }
    }

    /// SPEC FR-W4c (1c), INTENT D55: the editor server has no connection token, so the `auth`
    /// message carries the upstream "no token" value and `signedData` echoes the challenge, with
    /// or without a ticket.
    #[tokio::test]
    async fn fr_w4c_editor_auth_has_no_connection_token() {
        let (client, mut server) = tokio::io::duplex(64 * 1024);
        let task = tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            while find_header_end(&buf).is_none() {
                let n = server.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
            }
            let end = find_header_end(&buf).unwrap();
            let mut dec = FrameDecoder::new();
            dec.push(&buf[end + 4..]);
            server.write_all(b"HTTP/1.1 101 Switching Protocols\r\n\r\n").await.unwrap();
            let auth = loop {
                if let Some(f) = dec.next_frame().unwrap() {
                    break f;
                }
                let n = server.read(&mut chunk).await.unwrap();
                dec.push(&chunk[..n]);
            };
            let auth: Value = serde_json::from_slice(&auth.data).unwrap();
            let sign = br#"{"type":"sign","data":"the-challenge","signedData":"x"}"#.to_vec();
            server.write_all(&Frame::new(MessageType::Control, 0, 0, sign).encode()).await.unwrap();
            let ct = loop {
                if let Some(f) = dec.next_frame().unwrap() {
                    break f;
                }
                let n = server.read(&mut chunk).await.unwrap();
                dec.push(&chunk[..n]);
            };
            let ct: Value = serde_json::from_slice(&ct.data).unwrap();
            server.write_all(&Frame::new(MessageType::Control, 0, 0, br#"{"type":"ok"}"#.to_vec()).encode()).await.unwrap();
            (auth, ct, server)
        });
        let (_conn, first) = connect(client, &manager_opts(), ConnectionType::Management, None).await.unwrap();
        assert_eq!(first, serde_json::json!({"type":"ok"}));
        let (auth, ct, _server) = task.await.unwrap();
        assert_eq!(auth["type"], "auth");
        assert_eq!(auth["auth"], NO_CONNECTION_TOKEN);
        assert_eq!(NO_CONNECTION_TOKEN, "00000000000000000000");
        assert!(!auth.to_string().contains("tk-0123456789abcdef"), "the ticket is a cookie, never the auth: {auth}");
        assert_eq!(ct["signedData"], "the-challenge");
    }

    #[test]
    fn version_request_and_response() {
        let opts = ConnectOptions::new("127.0.0.1:8000");
        assert_eq!(
            version_request(&opts),
            "GET /version HTTP/1.0\r\nHost: 127.0.0.1:8000\r\nConnection: close\r\n\r\n"
        );
        let plain = format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\n{}", crate::PINNED_COMMIT);
        assert_eq!(parse_version_response(plain.as_bytes()).unwrap(), crate::PINNED_COMMIT);
        let chunked = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n28\r\n{}\r\n0\r\n\r\n",
            crate::PINNED_COMMIT
        );
        assert_eq!(parse_version_response(chunked.as_bytes()).unwrap(), crate::PINNED_COMMIT);
        assert!(matches!(
            parse_version_response(b"HTTP/1.1 404 Not Found\r\n\r\n"),
            Err(Error::UnsupportedServerVersion { server: None })
        ));
        // empty body: dev build without a commit
        let empty = parse_version_response(b"HTTP/1.1 200 OK\r\n\r\n").unwrap();
        assert!(matches!(crate::check_server_commit(&empty), Err(Error::UnsupportedServerVersion { .. })));
    }

    #[test]
    fn fr_e10_network_and_auth_errors_on_version_do_not_fall_back() {
        for (raw, auth) in [
            (&b"HTTP/1.1 401 Unauthorized\r\n\r\n"[..], true),
            (b"HTTP/1.1 403 Forbidden\r\n\r\nForbidden.", true),
            (b"HTTP/1.1 500 Internal Server Error\r\n\r\n", false),
            (b"HTTP/1.1 502 Bad Gateway\r\n\r\n", false),
            (b"HTTP/1.1 503 Service Unavailable\r\n\r\n", false),
            (b"HTTP/1.1 504 Gateway Timeout\r\n\r\n", false),
            (b"garbage\r\n\r\n", false),
        ] {
            let err = parse_version_response(raw).unwrap_err();
            let line = String::from_utf8_lossy(raw).lines().next().unwrap_or_default().to_owned();
            assert!(!matches!(err, Error::UnsupportedServerVersion { .. }), "{line}: {err}");
            assert_eq!(matches!(err, Error::Refused(_)), auth, "{line}: {err}");
            assert!(!is_permanent(&err) || auth, "{line}: a network error must stay retryable: {err}");
        }
    }

    #[tokio::test]
    async fn verify_server_refuses_other_commit() {
        let (client, mut server) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            while find_header_end(&buf).is_none() {
                let n = server.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
            }
            assert!(buf.starts_with(b"GET /version HTTP/1.0\r\n"));
            server
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\n0036dcb6c18a603d2168c00fa578be600987ac7b")
                .await
                .unwrap();
            // dropping `server` closes the pipe
        });
        let err = verify_server(client, &ConnectOptions::new("h")).await.unwrap_err();
        assert!(matches!(err, Error::UnsupportedServerVersion { server: Some(ref c) } if c.starts_with("0036dcb6")));
        assert!(is_permanent(&err));
    }

    #[tokio::test]
    async fn connect_refuses_unpinned_commit_without_io() {
        let (client, _server) = tokio::io::duplex(64);
        let mut opts = ConnectOptions::new("h");
        opts.commit = Some("deadbeef".into());
        let err = connect(client, &opts, ConnectionType::Management, None).await.err().unwrap();
        assert!(matches!(err, Error::UnsupportedServerVersion { server: Some(ref c) } if c == "deadbeef"));
    }

    #[test]
    fn server_version_mismatch_is_typed() {
        let raw = br#"{"type":"error","reason":"Client refused: version mismatch"}"#;
        assert!(matches!(parse_control(raw), Err(Error::UnsupportedServerVersion { server: None })));
    }

    #[tokio::test]
    async fn error_message_is_refusal() {
        let (client, mut server) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            while find_header_end(&buf).is_none() {
                let n = server.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
            }
            let mut out = b"HTTP/1.1 101 Switching Protocols\r\n\r\n".to_vec();
            let err = br#"{"type":"error","reason":"Unauthorized client refused: auth mismatch"}"#.to_vec();
            Frame::new(MessageType::Control, 0, 0, err).encode_into(&mut out);
            server.write_all(&out).await.unwrap();
            // keep the pipe open
            let _ = server.read(&mut chunk).await;
        });
        let opts = ConnectOptions::new("h");
        let err = connect(client, &opts, ConnectionType::Management, None).await.err().unwrap();
        assert!(matches!(err, Error::Refused(ref r) if r.contains("auth mismatch")));
        assert!(is_permanent(&err));
    }
}
