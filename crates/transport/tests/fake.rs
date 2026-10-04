mod common;

use std::time::Duration;

use common::{echo_roundtrip, spawn_echo};
use ember_transport::mem::MemNetwork;
use ember_transport::{CloseReason, PathState, TransportError};

#[tokio::test]
async fn round_trip_and_identity() {
    let net = MemNetwork::new();
    let (a, b) = (net.transport(), net.transport());
    spawn_echo(&b, "echo");

    let conn = a.connect(b.peer_id(), "echo").await.unwrap();
    assert_eq!(conn.peer(), b.peer_id());
    assert_eq!(conn.service(), "echo");
    assert_eq!(echo_roundtrip(&conn, b"hello").await, b"hello");
    // Several streams on one connection.
    let big = vec![7u8; 1 << 20];
    let (x, y) = tokio::join!(echo_roundtrip(&conn, &big), echo_roundtrip(&conn, b"two"));
    assert_eq!(x, big);
    assert_eq!(y, b"two");
}

#[tokio::test]
async fn accepted_side_sees_caller_identity() {
    let net = MemNetwork::new();
    let (a, b) = (net.transport(), net.transport());
    let mut listener = b.listen("svc").unwrap();
    let _conn = a.connect(b.peer_id(), "svc").await.unwrap();
    let accepted = listener.accept().await.unwrap();
    assert_eq!(accepted.peer(), a.peer_id());
}

#[tokio::test]
async fn unknown_service_and_peer_fail() {
    let net = MemNetwork::new();
    let (a, b) = (net.transport(), net.transport());
    assert!(matches!(
        a.connect(b.peer_id(), "nope").await,
        Err(TransportError::Connect { .. })
    ));
    let stranger = MemNetwork::new().transport();
    assert!(a.connect(stranger.peer_id(), "x").await.is_err());
    // Double listen is refused; after dropping the listener it can be re-registered.
    let l = b.listen("svc").unwrap();
    assert!(matches!(b.listen("svc"), Err(TransportError::AlreadyListening(_))));
    drop(l);
    b.listen("svc").unwrap();
}

#[tokio::test]
async fn close_is_seen_by_peer() {
    let net = MemNetwork::new();
    let (a, b) = (net.transport(), net.transport());
    let mut listener = b.listen("svc").unwrap();
    let conn = a.connect(b.peer_id(), "svc").await.unwrap();
    let accepted = listener.accept().await.unwrap();

    conn.close(42, "bye");
    assert_eq!(
        accepted.closed().await,
        CloseReason::Remote { code: 42, reason: "bye".into() }
    );
    assert_eq!(conn.closed().await, CloseReason::Local);
    assert!(matches!(accepted.accept_bi().await, Err(TransportError::Closed(_))));
    assert!(conn.open_bi().await.is_err());
    assert_eq!(conn.path_state(), PathState::Unknown);
}

#[tokio::test]
async fn path_changes_are_notified() {
    let net = MemNetwork::new();
    let (a, b) = (net.transport(), net.transport());
    let _l = b.listen("svc").unwrap();
    let conn = a.connect(b.peer_id(), "svc").await.unwrap();
    assert!(conn.path_state().is_direct());

    let mut watch = conn.watch_path();
    let relayed = PathState::Relayed { relay: "https://relay.example".into(), rtt: Duration::from_millis(40) };
    net.set_path(a.peer_id(), b.peer_id(), relayed.clone());
    tokio::time::timeout(Duration::from_secs(1), watch.changed()).await.unwrap().unwrap();
    assert_eq!(*watch.borrow(), relayed);
    assert_eq!(conn.path_state(), relayed);
}

#[tokio::test]
async fn transport_close_ends_connections() {
    let net = MemNetwork::new();
    let (a, b) = (net.transport(), net.transport());
    let mut listener = b.listen("svc").unwrap();
    let conn = a.connect(b.peer_id(), "svc").await.unwrap();
    let _accepted = listener.accept().await.unwrap();
    b.close().await;
    tokio::time::timeout(Duration::from_secs(1), conn.closed()).await.unwrap();
    assert!(a.connect(b.peer_id(), "svc").await.is_err());
}
