//! HTTP (and WebSocket) over transport streams. Ember server and node APIs are HTTP; this lets
//! them run unchanged on the peer-to-peer transport.
//!
//! Mapping: one transport stream carries one HTTP/1.1 connection (keep-alive, upgrades and
//! therefore WebSockets work). A client opens more streams on the same transport connection
//! for concurrency; streams are cheap.
//!
//! Serving: `axum::serve(HttpListener::new(listener), router)`. Handlers can take
//! `ConnectInfo<PeerId>` (with `into_make_service_with_connect_info::<PeerId>()`) to learn the
//! authenticated caller.
//!
//! Dialing: [`http1_client`] returns a hyper `SendRequest` over a fresh stream. For a
//! WebSocket, open a stream with [`crate::Connection::open_bi`] and hand it to any client
//! handshake (e.g. `tokio_tungstenite::client_async`).

use std::io;

use axum::extract::connect_info::Connected;
use axum::serve::IncomingStream;
use hyper::body::Body;
use hyper::client::conn::http1::SendRequest;
use hyper_util::rt::TokioIo;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::{BiStream, Connection, Listener, PeerId, Result, TransportError};

/// Turns a transport [`Listener`] into an `axum::serve` listener: every bidirectional stream
/// any peer opens on any accepted connection becomes one HTTP connection.
pub struct HttpListener {
    streams: mpsc::Receiver<(BiStream, PeerId)>,
    local: PeerId,
    task: JoinHandle<()>,
}

impl HttpListener {
    pub fn new(mut listener: Listener) -> Self {
        let local = listener.local_peer();
        let (tx, streams) = mpsc::channel(64);
        let task = tokio::spawn(async move {
            while let Some(conn) = listener.accept().await {
                let tx = tx.clone();
                tokio::spawn(async move {
                    let peer = conn.peer();
                    while let Ok(stream) = conn.accept_bi().await {
                        if tx.send((stream, peer)).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        Self { streams, local, task }
    }
}

impl Drop for HttpListener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl axum::serve::Listener for HttpListener {
    type Io = BiStream;
    type Addr = PeerId;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.streams.recv().await {
            Some(item) => item,
            // Transport closed: never yield again. Stop the server with graceful shutdown.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(self.local)
    }
}

impl Connected<IncomingStream<'_, HttpListener>> for PeerId {
    fn connect_info(stream: IncomingStream<'_, HttpListener>) -> Self {
        *stream.remote_addr()
    }
}

/// Opens a stream on `conn` and performs an HTTP/1.1 client handshake over it. The returned
/// sender reuses the stream for sequential requests (and supports upgrades).
pub async fn http1_client<B>(conn: &Connection) -> Result<SendRequest<B>>
where
    B: Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let stream = conn.open_bi().await?;
    let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|e| TransportError::Stream(format!("http handshake: {e}")))?;
    tokio::spawn(async move {
        if let Err(e) = connection.with_upgrades().await {
            tracing::debug!(error = %e, "http client connection ended");
        }
    });
    Ok(sender)
}
