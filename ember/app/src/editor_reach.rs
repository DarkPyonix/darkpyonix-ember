//! The editor window's byte stream to the OSE through the server's relay route (SPEC `FR-N9.1`,
//! #879).
//!
//! The server hands out `ose.relay` (`/api/computers/{computer}/ide/relay/{launch_id}`, `FR-W4`)
//! and a placeholder connection token. The relay is a WebSocket that carries the OSE's TCP bytes
//! as binary messages, and it puts the real token in place of the placeholder only inside one
//! client message. [`RelayStream`] is the editor's `AsyncRead + AsyncWrite` over that socket:
//! every `poll_write` buffer goes out as exactly one binary message, never split or merged, and
//! `ember-editor-conn` writes the upgrade request, the `auth` message and each batch of frames
//! with one `write_all` of one buffer, so the placeholder never straddles two messages.
//!
//! The route is reached the way the terminal relay is ([`TermReach`]): over the transport
//! through [`Api::open_peer_socket`](ember_client::api::Api::open_peer_socket), with the device
//! key as the credential, or over HTTP as `ws(s)://<server><relay>` with the server token under
//! the terminal relay's rules (`FR-S11`, `FR-L31.19`).

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use futures_util::{Sink, Stream};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use crate::terminal_stream::{ClientError, TermReach};

/// A WebSocket the relay stream runs on: the plain HTTP socket or the one over the transport.
pub trait RelaySocket:
    Sink<Message, Error = WsError> + Stream<Item = Result<Message, WsError>> + Send + Unpin
{
}

impl<T> RelaySocket for T where
    T: Sink<Message, Error = WsError> + Stream<Item = Result<Message, WsError>> + Send + Unpin
{
}

/// The OSE's byte stream over the relay WebSocket.
pub struct RelayStream {
    ws: Box<dyn RelaySocket>,
    /// The binary message being read, and how much of it was handed out.
    pending: Vec<u8>,
    pos: usize,
    /// A Close came, or the socket ended.
    eof: bool,
}

impl RelayStream {
    pub fn new(ws: impl RelaySocket + 'static) -> RelayStream {
        RelayStream { ws: Box::new(ws), pending: Vec::new(), pos: 0, eof: false }
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
                // Ping and Pong are answered by the socket; a raw frame is never read.
                Some(Ok(_)) => {}
                Some(Err(WsError::ConnectionClosed | WsError::AlreadyClosed)) => this.eof = true,
                Some(Err(e)) => return Poll::Ready(Err(ws_io(e))),
            }
        }
    }
}

impl AsyncWrite for RelayStream {
    /// Takes the whole buffer as one binary message, so `write_all` makes one call per buffer.
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

    /// Closes the socket; the relay then closes its side of the OSE connection.
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match ready!(Pin::new(&mut *self.get_mut().ws).poll_close(cx)) {
            Ok(()) | Err(WsError::ConnectionClosed | WsError::AlreadyClosed) => Poll::Ready(Ok(())),
            Err(e) => Poll::Ready(Err(ws_io(e))),
        }
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

/// The relay path below `/api`, as [`Api::open_peer_socket`](ember_client::api::Api::open_peer_socket) takes it.
pub fn peer_path(relay: &str) -> io::Result<String> {
    let relay = checked_relay(relay)?;
    Ok(relay.strip_prefix("/api").unwrap_or(relay).to_owned())
}

/// A terminal-relay refusal as an `io::Error` whose text is the panel's sentence.
fn client_io(e: ClientError) -> io::Error {
    match e {
        ClientError::Refused(sentence) => io::Error::new(io::ErrorKind::PermissionDenied, sentence),
        other => io::Error::other(other.to_string()),
    }
}

/// Opens the relay over plain HTTP at `base` (`http(s)://host:port`), with `token` as
/// `Authorization: Bearer` when given. A `peer:<id>` base is never dialed as a URL, and plain
/// `http` goes only where the terminal relay may go (`FR-L31.19`).
pub async fn dial_http(base: &str, relay: &str, token: Option<&str>) -> io::Result<RelayStream> {
    crate::terminal_stream::refuse_peer_base(base).map_err(client_io)?;
    crate::terminal_stream::check_relay(base).map_err(client_io)?;
    crate::terminal_stream::ensure_tls_provider();
    let mut upgrade = relay_url(base, relay)?.into_client_request().map_err(ws_io)?;
    if let Some(token) = token {
        let value = HeaderValue::from_str(&ember_client::token::bearer_header(token))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "the server token is not a valid header value"))?;
        upgrade.headers_mut().insert("Authorization", value);
    }
    match tokio_tungstenite::connect_async(upgrade).await {
        Ok((ws, _)) => Ok(RelayStream::new(ws)),
        Err(WsError::Http(resp)) => {
            let status = resp.status();
            tracing::warn!("the OSE relay upgrade was answered with {status}");
            let kind = if status.as_u16() == 401 || status.as_u16() == 403 {
                io::ErrorKind::PermissionDenied
            } else {
                io::ErrorKind::ConnectionRefused
            };
            Err(io::Error::new(kind, format!("the OSE relay upgrade was answered with {status}")))
        }
        Err(e) => Err(ws_io(e)),
    }
}

/// Opens the relay over the transport through `api`, on its own stream. A refused device reads
/// as the device sentence (`FR-N9`), a 403 `scope_denied` as the `FR-N8` sentence.
pub async fn dial_peer(api: &ember_client::api::Api, relay: &str) -> io::Result<RelayStream> {
    let path = peer_path(relay)?;
    match api.open_peer_socket(&path).await {
        Ok(ws) => Ok(RelayStream::new(ws)),
        Err(e) => match crate::device_link::refusal_text(&e) {
            Some(sentence) => {
                tracing::warn!("the OSE relay over the transport was refused: {e}");
                Err(io::Error::new(io::ErrorKind::PermissionDenied, sentence))
            }
            None => Err(io::Error::other(e.to_string())),
        },
    }
}

/// Opens the relay on `reach`: the `dial` of `EditorSession::connect`, which calls it once per
/// socket (`GET /version`, management, extension host). Over HTTP the server token goes only to
/// a server this machine may trust with it (`FR-S11`).
pub async fn dial(reach: &TermReach, relay: &str) -> io::Result<RelayStream> {
    match reach {
        TermReach::Http(base) => {
            let token = crate::terminal_stream::server_token(base);
            dial_http(base, relay, token.as_deref()).await
        }
        TermReach::Peer(api) => dial_peer(api, relay).await,
    }
}

/// The app's reach for the editor window: over the transport when the app's `Api` has a peer,
/// else `server_url` (the target's, or the app's own when the target has none) over HTTP.
pub fn reach_for(server_url: &str) -> TermReach {
    let s = crate::services::services();
    let url = if server_url.trim().is_empty() { s.config.server_url.as_str() } else { server_url };
    TermReach::select(url, s.client.api())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A socket that records what is sent and hands out `incoming` in order.
    #[derive(Default)]
    struct Fake {
        sent: Arc<Mutex<Vec<Message>>>,
        incoming: VecDeque<Result<Message, WsError>>,
        closed: Arc<AtomicBool>,
    }

    impl Sink<Message> for Fake {
        type Error = WsError;
        fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), WsError>> {
            Poll::Ready(Ok(()))
        }
        fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), WsError> {
            self.sent.lock().unwrap().push(item);
            Ok(())
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), WsError>> {
            Poll::Ready(Ok(()))
        }
        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), WsError>> {
            self.closed.store(true, Ordering::SeqCst);
            Poll::Ready(Ok(()))
        }
    }

    impl Stream for Fake {
        type Item = Result<Message, WsError>;
        fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Poll::Ready(self.get_mut().incoming.pop_front())
        }
    }

    fn binary(b: &[u8]) -> Message {
        Message::Binary(b.to_vec().into())
    }

    #[tokio::test]
    async fn fr_n9_each_write_is_one_binary_message() {
        let fake = Fake::default();
        let (sent, closed) = (fake.sent.clone(), fake.closed.clone());
        let mut s = RelayStream::new(fake);
        let big = vec![7u8; 100_000];
        s.write_all(&big).await.unwrap();
        s.write_all(b"auth").await.unwrap();
        s.write_all(b"").await.unwrap();
        s.flush().await.unwrap();
        assert_eq!(*sent.lock().unwrap(), vec![binary(&big), binary(b"auth")], "never split, merged or empty");
        s.shutdown().await.unwrap();
        assert!(closed.load(Ordering::SeqCst), "shutdown closes the socket");
    }

    #[tokio::test]
    async fn fr_n9_reads_join_binary_messages_and_end_at_close() {
        let mut fake = Fake::default();
        fake.incoming = VecDeque::from([
            Ok(binary(b"hel")),
            Ok(Message::Ping(b"p".to_vec().into())),
            Ok(binary(b"")),
            Ok(binary(b"lo")),
            Ok(Message::Close(None)),
            Ok(binary(b"after close")),
        ]);
        let mut s = RelayStream::new(fake);
        let mut one = [0u8; 2];
        s.read_exact(&mut one).await.unwrap();
        assert_eq!(&one, b"he", "a message is handed out in pieces as the reader asks");
        let mut rest = Vec::new();
        s.read_to_end(&mut rest).await.unwrap();
        assert_eq!(rest, b"llo", "an empty message is not the end; Close is");
    }

    #[tokio::test]
    async fn fr_n9_a_closed_socket_is_eof_and_text_is_an_error() {
        let mut fake = Fake::default();
        fake.incoming = VecDeque::from([Ok(binary(b"x")), Err(WsError::ConnectionClosed)]);
        let mut s = RelayStream::new(fake);
        let mut all = Vec::new();
        s.read_to_end(&mut all).await.unwrap();
        assert_eq!(all, b"x");

        let mut fake = Fake::default();
        fake.incoming = VecDeque::from([Ok(Message::text(String::from("no")))]);
        let mut s = RelayStream::new(fake);
        let e = s.read_to_end(&mut Vec::new()).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn fr_n9_relay_url_and_peer_path() {
        let relay = "/api/computers/local/ide/relay/abc";
        assert_eq!(relay_url("http://127.0.0.1:8740", relay).unwrap(), "ws://127.0.0.1:8740/api/computers/local/ide/relay/abc");
        assert_eq!(relay_url("https://srv.example/", relay).unwrap(), "wss://srv.example/api/computers/local/ide/relay/abc");
        assert!(relay_url("peer:abc", relay).is_err());
        assert_eq!(peer_path(relay).unwrap(), "/computers/local/ide/relay/abc");
        for bad in ["", "computers/local/ide/relay/abc", "/other/x", "/api/x y", "/api/http://evil"] {
            assert!(relay_url("http://127.0.0.1:8740", bad).is_err(), "{bad}");
            assert!(peer_path(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn fr_n9_a_peer_base_is_never_dialed_as_a_url() {
        let e = dial(&TermReach::Http("peer:abc".into()), "/api/computers/local/ide/relay/abc").await.err().unwrap();
        assert_eq!(e.to_string(), crate::device_link::UNREACHABLE);
    }
}
