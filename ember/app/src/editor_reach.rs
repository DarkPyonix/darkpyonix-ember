//! The editor window's byte stream to the OSE through the server's relay route (SPEC `FR-N9.1`,
//! #879). [`RelayStream`] and the relay path rules are `ember_client::relay_stream` (moved there
//! for the gateway, #80); this module keeps the app's dial and its sentences.
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

use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Error as WsError;

use crate::terminal_stream::{ClientError, TermReach};

// The stream and its URL rules live in ember-client (`relay_stream`, #80) so the gateway can use
// them too; the app keeps the names it had.
pub use ember_client::relay_stream::{peer_path, relay_url, ws_io, RelaySocket, RelayStream};

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
    peer_path(relay)?;
    match ember_client::relay_stream::dial_peer(api, relay).await {
        Ok(stream) => Ok(stream),
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

    // The stream's own fr_n9 tests moved with it to ember/client/src/relay_stream.rs.

    #[tokio::test]
    async fn fr_n9_a_peer_base_is_never_dialed_as_a_url() {
        let e = dial(&TermReach::Http("peer:abc".into()), "/api/computers/local/ide/relay/abc").await.err().unwrap();
        assert_eq!(e.to_string(), crate::device_link::UNREACHABLE);
    }
}
