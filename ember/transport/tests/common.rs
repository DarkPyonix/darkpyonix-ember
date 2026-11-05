//! Shared test helpers (included with `mod common;`).
#![allow(dead_code)]

use ember_transport::{Connection, Transport};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Echoes every stream on every connection accepted for `service`.
pub fn spawn_echo(transport: &Transport, service: &str) {
    let mut listener = transport.listen(service).unwrap();
    tokio::spawn(async move {
        while let Some(conn) = listener.accept().await {
            tokio::spawn(async move {
                while let Ok(stream) = conn.accept_bi().await {
                    tokio::spawn(async move {
                        let (mut w, mut r) = stream.into_split();
                        tokio::io::copy(&mut r, &mut w).await.ok();
                        w.shutdown().await.ok();
                    });
                }
            });
        }
    });
}

/// Sends `msg` on a new stream, finishes it and reads the full echo.
/// Writes and reads concurrently so large messages cannot deadlock on flow control.
pub async fn echo_roundtrip(conn: &Connection, msg: &[u8]) -> Vec<u8> {
    let stream = conn.open_bi().await.unwrap();
    let (mut w, mut r) = stream.into_split();
    let write = async {
        w.write_all(msg).await.unwrap();
        w.shutdown().await.unwrap();
    };
    let read = async {
        let mut out = Vec::new();
        r.read_to_end(&mut out).await.unwrap();
        out
    };
    let ((), out) = tokio::join!(write, read);
    out
}

/// Two iroh transports on localhost, no relay, finding each other through a shared in-memory
/// address directory (the pluggable lookup the darkpyonix.dev directory will implement).
#[cfg(feature = "iroh")]
pub async fn iroh_pair() -> (Transport, Transport, ember_transport::MemoryDirectory) {
    use ember_transport::{MemoryDirectory, RelayConfig, SecretKey, TransportConfig};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    let dir = MemoryDirectory::new();
    let make = || {
        TransportConfig::new(SecretKey::generate())
            .relay(RelayConfig::Disabled)
            .directory(Arc::new(dir.clone()))
            .bind_addr("127.0.0.1:0".parse().unwrap())
    };
    let a = Transport::bind(make()).await.unwrap();
    let b = Transport::bind(make()).await.unwrap();
    for t in [&a, &b] {
        let deadline = Instant::now() + Duration::from_secs(10);
        while dir.get(&t.peer_id()).is_none_or(|e| e.direct.is_empty()) {
            assert!(Instant::now() < deadline, "endpoint never published its address");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    (a, b, dir)
}

/// A letify-link transport on localhost: TCP on 127.0.0.1, found through a shared in-memory
/// address directory, no relay.
#[cfg(feature = "letify")]
pub fn letify_config(dir: &ember_transport::MemoryDirectory) -> ember_transport::TransportConfig {
    use ember_transport::{BackendKind, SecretKey, TransportConfig};
    use std::sync::Arc;

    TransportConfig::new(SecretKey::generate())
        .backend(BackendKind::Letify)
        .bind_addr("127.0.0.1:0".parse().unwrap())
        .directory(Arc::new(dir.clone()))
}

/// Two letify-link transports on localhost that find each other through the directory.
#[cfg(feature = "letify")]
pub async fn letify_pair() -> (Transport, Transport, ember_transport::MemoryDirectory) {
    let dir = ember_transport::MemoryDirectory::new();
    let a = Transport::bind(letify_config(&dir)).await.unwrap();
    let b = Transport::bind(letify_config(&dir)).await.unwrap();
    (a, b, dir)
}
