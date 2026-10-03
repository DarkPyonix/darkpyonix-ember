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

/// Everything needed to (re)open one logical connection.
#[derive(Debug, Clone)]
pub struct ConnectOptions {
    /// `Host` header value, e.g. `"127.0.0.1:8000"`.
    pub host: String,
    /// Request path; the server accepts any (`getServerRootPath()` is `/<quality>-<commit>` in the
    /// workbench, but `handleUpgrade` ignores the path).
    pub path: String,
    /// `--connection-token` of the server, if it has one. Sent in the `auth` message.
    pub connection_token: Option<String>,
    /// Identifies this logical connection across reconnects. One per connection, never reused.
    pub reconnection_token: String,
    /// Our idea of the server commit. When both sides set it, the server refuses a mismatch
    /// ("Client refused: version mismatch"). Use the value of `GET /version`.
    pub commit: Option<String>,
    /// Timeout for the whole handshake.
    pub timeout: Duration,
}

impl ConnectOptions {
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            path: "/".into(),
            connection_token: None,
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

/// The default `auth` value when there is no connection token (L260).
pub const NO_CONNECTION_TOKEN: &str = "00000000000000000000";

/// Build the raw upgrade request (cf. `makeRawSocketHeaders`).
pub fn upgrade_request(opts: &ConnectOptions, reconnection: bool) -> String {
    let key = base64_16(uuid::Uuid::new_v4().as_bytes());
    format!(
        "GET {}?{} HTTP/1.1\r\nHost: {}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {}\r\n\r\n",
        opts.path,
        opts.query(reconnection),
        opts.host,
        key
    )
}

/// Write the upgrade request and read the response headers. Returns any bytes read past the
/// header terminator (they belong to the first protocol frame).
pub async fn upgrade<S>(stream: &mut S, opts: &ConnectOptions, reconnection: bool) -> Result<Vec<u8>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
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

fn parse_control(raw: &[u8]) -> Result<Value> {
    let v: Value = serde_json::from_slice(raw)?;
    if v.get("type").and_then(Value::as_str) == Some("error") {
        let reason = v.get("reason").and_then(Value::as_str).unwrap_or("unknown").to_owned();
        return Err(Error::Refused(reason));
    }
    Ok(v)
}

/// Run auth → sign → connectionType on an attached transport and return the server's first
/// reply (`{"type":"ok"}` for Management, `{}` or `{"debugPort":n}` for ExtensionHost).
///
/// Signing: Code-OSS without the proprietary `vsda` module leaves `signService.sign` as the
/// identity and skips validation (`abstractSignService.ts`), and a server without `vsda` accepts
/// any `signedData` (agent server L362-364). A Microsoft build with `vsda` also accepts
/// `signedData == connectionToken` ("web client", L363), so we send the connection token when we
/// have one and echo the challenge otherwise. We never validate the server's signature (that
/// needs `vsda`); authenticity of the server is the transport's job (TLS / iroh).
pub async fn handshake(
    handle: &ConnectionHandle,
    opts: &ConnectOptions,
    ty: ConnectionType,
    args: Option<Value>,
) -> Result<Value> {
    let fut = async {
        let auth = HandshakeMessage::Auth {
            auth: opts.connection_token.clone().unwrap_or_else(|| NO_CONNECTION_TOKEN.to_owned()),
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
            signed_data: opts.connection_token.clone().unwrap_or(challenge),
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
    matches!(err, Error::Refused(_))
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
            assert_eq!(auth["auth"], "tok");

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
            assert_eq!(ct["signedData"], "tok");

            let ok = Frame::new(MessageType::Control, 0, 0, br#"{"type":"ok"}"#.to_vec()).encode();
            server.write_all(&ok).await.unwrap();
            server
        });

        let mut opts = ConnectOptions::new("localhost:8000");
        opts.connection_token = Some("tok".into());
        let (_conn, first) = connect(client, &opts, ConnectionType::Management, None).await.unwrap();
        assert_eq!(first, serde_json::json!({"type":"ok"}));
        server_task.await.unwrap();
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
