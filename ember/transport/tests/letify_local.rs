//! The transport conformance suite against the letify-link backend on localhost (SPEC `FR-N5`:
//! "the same suite passes against both backends"). The cases mirror `fake.rs` and
//! `iroh_local.rs`; the addresses come from a shared in-memory directory, no internet is needed.
#![cfg(feature = "letify")]

mod common;

use std::time::Duration;

use common::{echo_roundtrip, letify_config, letify_pair, spawn_echo};
use ember_transport::{
    CloseReason, MemoryDirectory, MemoryPkarrStore, PathState, PeerAddr, PowerState, Transport, TransportConfig,
    TransportError,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test(flavor = "multi_thread")]
async fn fr_n5_letify_round_trip_by_peer_id_via_directory() {
    let (a, b, _dir) = letify_pair().await;
    spawn_echo(&b, "ember/echo/0");

    // Dial by PeerId only: the address comes from the directory.
    let conn = tokio::time::timeout(Duration::from_secs(10), a.connect(b.peer_id(), "ember/echo/0"))
        .await
        .expect("connect timed out")
        .unwrap();
    assert_eq!(conn.peer(), b.peer_id());
    assert_eq!(conn.service(), "ember/echo/0");
    assert_eq!(echo_roundtrip(&conn, b"hello letify").await, b"hello letify");

    // Several streams on one connection at once, one of them large.
    let big = vec![3u8; 4 << 20];
    let (x, y) = tokio::join!(echo_roundtrip(&conn, &big), echo_roundtrip(&conn, b"two"));
    assert_eq!(x, big);
    assert_eq!(y, b"two");

    let path = conn.path_state();
    assert!(path.is_direct(), "expected a direct localhost path, got {path}");

    conn.close(0, "done");
    a.close().await;
    b.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n5_letify_accepted_side_sees_caller_identity() {
    let (a, b, _dir) = letify_pair().await;
    let mut listener = b.listen("svc").unwrap();
    let _conn = a.connect(b.peer_id(), "svc").await.unwrap();
    let accepted = tokio::time::timeout(Duration::from_secs(5), listener.accept()).await.unwrap().unwrap();
    assert_eq!(accepted.peer(), a.peer_id());
    assert_eq!(accepted.service(), "svc");
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n5_letify_unknown_service_and_peer_fail() {
    let (a, b, _dir) = letify_pair().await;
    // The peer is reachable but does not listen for the service.
    match a.connect(b.peer_id(), "nope").await {
        Err(TransportError::Connect { reason, .. }) => assert!(reason.contains("service"), "{reason}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    // A peer nobody knows an address of.
    let stranger = ember_transport::SecretKey::generate().peer_id();
    assert!(matches!(a.connect(stranger, "x").await, Err(TransportError::Connect { .. })));
    // Double listen is refused; after dropping the listener it can be registered again.
    let l = b.listen("svc").unwrap();
    assert!(matches!(b.listen("svc"), Err(TransportError::AlreadyListening(_))));
    drop(l);
    b.listen("svc").unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n5_letify_close_reason_reaches_peer() {
    let (a, b, _dir) = letify_pair().await;
    let mut listener = b.listen("svc").unwrap();
    let conn = a.connect(b.peer_id(), "svc").await.unwrap();
    let accepted = listener.accept().await.unwrap();

    conn.close(42, "bye");
    assert_eq!(accepted.closed().await, CloseReason::Remote { code: 42, reason: "bye".into() });
    assert_eq!(conn.closed().await, CloseReason::Local);
    assert!(matches!(accepted.accept_bi().await, Err(TransportError::Closed(_))));
    assert!(conn.open_bi().await.is_err());
    assert_eq!(conn.path_state(), PathState::Unknown);
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n5_letify_dropping_a_connection_ends_it_for_the_peer() {
    let (a, b, _dir) = letify_pair().await;
    let mut listener = b.listen("svc").unwrap();
    let conn = a.connect(b.peer_id(), "svc").await.unwrap();
    let accepted = listener.accept().await.unwrap();
    drop(conn);
    let reason = tokio::time::timeout(Duration::from_secs(5), accepted.closed()).await.unwrap();
    assert!(matches!(reason, CloseReason::Remote { code: 0, .. }), "{reason:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n5_letify_open_streams_end_when_the_connection_closes() {
    let (a, b, _dir) = letify_pair().await;
    spawn_echo(&b, "echo");
    let conn = a.connect(b.peer_id(), "echo").await.unwrap();
    let mut stream = conn.open_bi().await.unwrap();
    stream.write_all(b"x").await.unwrap();
    let mut one = [0u8; 1];
    stream.read_exact(&mut one).await.unwrap();

    conn.close(0, "done");
    // The stream was open and idle: it reads end of stream and writes fail.
    let mut rest = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut rest)).await.unwrap().unwrap();
    assert!(stream.write_all(b"y").await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n5_letify_transport_close_ends_connections() {
    let (a, b, _dir) = letify_pair().await;
    let mut listener = b.listen("svc").unwrap();
    let conn = a.connect(b.peer_id(), "svc").await.unwrap();
    let _accepted = listener.accept().await.unwrap();
    b.close().await;
    tokio::time::timeout(Duration::from_secs(5), conn.closed()).await.unwrap();
    // Nothing listens at the peer's address any more: the dial fails at once, not after a timeout.
    let started = std::time::Instant::now();
    assert!(a.connect(b.peer_id(), "svc").await.is_err());
    assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n5_letify_explicit_address_hint_without_directory() {
    let cfg = || {
        TransportConfig::new(ember_transport::SecretKey::generate())
            .backend(ember_transport::BackendKind::Letify)
            .bind_addr("127.0.0.1:0".parse().unwrap())
    };
    let a = Transport::bind(cfg()).await.unwrap();
    let b = Transport::bind(cfg()).await.unwrap();
    spawn_echo(&b, "echo");
    let addr = b.local_addr();
    assert!(!addr.direct.is_empty());
    let conn = a.connect(addr, "echo").await.unwrap();
    assert_eq!(echo_roundtrip(&conn, b"hint").await, b"hint");
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n5_letify_both_sides_open_connections_on_one_session() {
    let (a, b, _dir) = letify_pair().await;
    spawn_echo(&a, "from-b");
    spawn_echo(&b, "from-a");
    let to_b = a.connect(b.peer_id(), "from-a").await.unwrap();
    // b dials a back: the session a started is reused, with ids of the other parity.
    let to_a = b.connect(a.peer_id(), "from-b").await.unwrap();
    assert_eq!(echo_roundtrip(&to_b, b"one").await, b"one");
    assert_eq!(echo_roundtrip(&to_a, b"two").await, b"two");
    // Both connections to the same service are independent.
    let again = a.connect(b.peer_id(), "from-a").await.unwrap();
    assert_eq!(echo_roundtrip(&again, b"three").await, b"three");
    again.close(0, "done");
    assert_eq!(echo_roundtrip(&to_b, b"four").await, b"four");
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n2_letify_peers_are_found_through_the_pkarr_directory() {
    // No address is configured anywhere: each endpoint publishes its signed record to the
    // directory (here the in-memory hub) and the dialer resolves the peer's record from it.
    let store = MemoryPkarrStore::new();
    let make = || {
        let mut letify = ember_transport::LetifyConfig::default();
        letify.pkarr = Some(std::sync::Arc::new(store.clone()));
        TransportConfig::new(ember_transport::SecretKey::generate())
            .backend(ember_transport::BackendKind::Letify)
            .bind_addr("127.0.0.1:0".parse().unwrap())
            .letify(letify)
    };
    let a = Transport::bind(make()).await.unwrap();
    let b = Transport::bind(make()).await.unwrap();
    spawn_echo(&b, "echo");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while store.len() < 2 {
        assert!(std::time::Instant::now() < deadline, "records were never published");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let conn = tokio::time::timeout(Duration::from_secs(10), a.connect(b.peer_id(), "echo"))
        .await
        .expect("connect timed out")
        .unwrap();
    assert_eq!(echo_roundtrip(&conn, b"by record").await, b"by record");
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n5_letify_resume_now_and_power_states_keep_connections_usable() {
    let (a, b, _dir) = letify_pair().await;
    spawn_echo(&b, "echo");
    let conn = a.connect(b.peer_id(), "echo").await.unwrap();
    assert_eq!(echo_roundtrip(&conn, b"before").await, b"before");

    for t in [&a, &b] {
        t.set_power_state(PowerState::Background);
        t.set_power_state(PowerState::PowerSave);
    }
    // The app returns to the foreground: paths are re-established, the connection survives.
    for t in [&a, &b] {
        t.set_power_state(PowerState::Foreground);
        t.resume_now();
    }
    let big = vec![9u8; 1 << 20];
    let out = tokio::time::timeout(Duration::from_secs(20), echo_roundtrip(&conn, &big)).await.unwrap();
    assert_eq!(out, big);
    assert_eq!(echo_roundtrip(&conn, b"after").await, b"after");
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n5_letify_closing_one_peer_leaves_other_connections_serving() {
    let dir = MemoryDirectory::new();
    let a = Transport::bind(letify_config(&dir)).await.unwrap();
    let b = Transport::bind(letify_config(&dir)).await.unwrap();
    let c = Transport::bind(letify_config(&dir)).await.unwrap();
    spawn_echo(&b, "echo");
    spawn_echo(&c, "echo");
    let to_b = a.connect(b.peer_id(), "echo").await.unwrap();
    let to_c = a.connect(c.peer_id(), "echo").await.unwrap();
    b.close().await;
    assert_eq!(echo_roundtrip(&to_c, b"still here").await, b"still here");
    tokio::time::timeout(Duration::from_secs(5), to_b.closed()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n5_letify_local_addr_names_where_the_peer_can_be_dialed() {
    let (_a, b, _dir) = letify_pair().await;
    let addr: PeerAddr = b.local_addr();
    assert_eq!(addr.peer, b.peer_id());
    assert!(addr.direct.iter().all(|d| d.ip().is_loopback()), "{addr:?}");
    assert!(addr.relays.is_empty());
}
