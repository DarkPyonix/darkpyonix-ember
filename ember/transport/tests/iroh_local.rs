//! iroh backend on localhost: two endpoints in one process, relay disabled, addresses found
//! through a shared in-memory directory. Needs no internet.
#![cfg(feature = "iroh")]

mod common;

use std::time::Duration;

use common::{echo_roundtrip, iroh_pair, spawn_echo};
use ember_transport::{CloseReason, TransportError};

#[tokio::test]
async fn round_trip_by_peer_id_via_directory() {
    let (a, b, _dir) = iroh_pair().await;
    spawn_echo(&b, "ember/echo/0");

    // Dial by PeerId only: the address comes from the directory.
    let conn = tokio::time::timeout(Duration::from_secs(10), a.connect(b.peer_id(), "ember/echo/0"))
        .await
        .expect("connect timed out")
        .unwrap();
    assert_eq!(conn.peer(), b.peer_id());
    assert_eq!(echo_roundtrip(&conn, b"hello iroh").await, b"hello iroh");
    let big = vec![3u8; 4 << 20];
    assert_eq!(echo_roundtrip(&conn, &big).await, big);

    let path = conn.path_state();
    assert!(path.is_direct(), "expected a direct localhost path, got {path}");

    conn.close(0, "done");
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn close_reason_reaches_peer_and_unknown_service_fails() {
    let (a, b, _dir) = iroh_pair().await;
    let mut listener = b.listen("svc").unwrap();

    assert!(matches!(
        a.connect(b.peer_id(), "not-listening").await,
        Err(TransportError::Connect { .. })
    ));

    let conn = a.connect(b.peer_id(), "svc").await.unwrap();
    // QUIC streams become visible to the peer on first write.
    let mut s = conn.open_bi().await.unwrap();
    tokio::io::AsyncWriteExt::write_all(&mut s, b"x").await.unwrap();
    let accepted = listener.accept().await.unwrap();
    assert_eq!(accepted.peer(), a.peer_id());
    let _ = accepted.accept_bi().await.unwrap();

    accepted.close(7, "revoked");
    assert_eq!(conn.closed().await, CloseReason::Remote { code: 7, reason: "revoked".into() });
    assert!(conn.open_bi().await.is_err());
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn explicit_address_hint_without_directory() {
    use ember_transport::{RelayConfig, SecretKey, Transport, TransportConfig};
    let cfg = || {
        TransportConfig::new(SecretKey::generate())
            .relay(RelayConfig::Disabled)
            .bind_addr("127.0.0.1:0".parse().unwrap())
    };
    let a = Transport::bind(cfg()).await.unwrap();
    let b = Transport::bind(cfg()).await.unwrap();
    spawn_echo(&b, "echo");
    // Wait for b to know its bound address.
    let mut addr = b.local_addr();
    for _ in 0..200 {
        if !addr.direct.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        addr = b.local_addr();
    }
    assert!(!addr.direct.is_empty());
    let conn = a.connect(addr, "echo").await.unwrap();
    assert_eq!(echo_roundtrip(&conn, b"hint").await, b"hint");
    a.close().await;
    b.close().await;
}
