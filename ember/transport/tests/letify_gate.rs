//! `FR-N3` admission and revocation, and `FR-N6` canonical identities, on the letify-link
//! backend. `PeerGate::attach` wires the gate to letify-link's `PeerPolicy::revoke` (handshakes
//! and live sessions) and to the relay.
#![cfg(feature = "letify")]

mod common;

use std::time::{Duration, Instant};

use common::{echo_roundtrip, spawn_echo};
use ember_transport::gate::{CLOSE_NOT_ALLOWED, CLOSE_REVOKED};
use ember_transport::{
    BackendKind, CloseReason, LetifyConfig, MemoryDirectory, PeerGate, PeerId, SecretKey, Transport,
    TransportConfig, TransportError,
};

/// A transport whose dials give up after two seconds, so a refused handshake fails quickly.
fn config(dir: &MemoryDirectory, key: SecretKey) -> TransportConfig {
    let letify = LetifyConfig { connect_timeout: Duration::from_secs(2), ..LetifyConfig::default() };
    TransportConfig::new(key)
        .backend(BackendKind::Letify)
        .bind_addr("127.0.0.1:0".parse().unwrap())
        .directory(std::sync::Arc::new(dir.clone()))
        .letify(letify)
}

async fn bind(dir: &MemoryDirectory) -> Transport {
    Transport::bind(config(dir, SecretKey::generate())).await.unwrap()
}

/// Serves `service` on `server` through `gate` and returns the admitted connections.
fn serve_gated(server: &Transport, service: &str, gate: PeerGate) -> tokio::sync::mpsc::UnboundedReceiver<ember_transport::Connection> {
    let mut listener = server.listen(service).unwrap();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(conn) = listener.accept().await {
            if let Some(conn) = gate.admit(conn) {
                let _ = tx.send(conn);
            }
        }
    });
    rx
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n3_letify_revoke_closes_the_session_at_once_and_refuses_new_ones() {
    let dir = MemoryDirectory::new();
    let (server, device) = (bind(&dir).await, bind(&dir).await);
    let gate = PeerGate::allow_list([device.peer_id()]);
    gate.attach(&server);
    let mut admitted = serve_gated(&server, "svc", gate.clone());

    let conn = device.connect(server.peer_id(), "svc").await.unwrap();
    let on_server = admitted.recv().await.unwrap();
    assert_eq!(on_server.peer(), device.peer_id());
    assert_eq!(gate.live_connections(&device.peer_id()), 1);

    let started = Instant::now();
    assert_eq!(gate.revoke(&device.peer_id()), 1);
    let reason = tokio::time::timeout(Duration::from_secs(1), conn.closed()).await.expect("closed within a second");
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(
        matches!(reason, CloseReason::Remote { code: CLOSE_REVOKED, .. }),
        "the device sees the revoked code, got {reason:?}"
    );

    // The device cannot start another session: the handshake is refused by the policy.
    let err = device.connect(server.peer_id(), "svc").await.unwrap_err();
    assert!(matches!(err, TransportError::Connect { .. }), "{err:?}");

    // Allowing the device again restores access.
    gate.allow(device.peer_id());
    let again = device.connect(server.peer_id(), "svc").await.unwrap();
    assert_eq!(admitted.recv().await.unwrap().peer(), device.peer_id());
    drop(again);
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n3_letify_a_peer_not_on_the_list_is_closed_at_accept() {
    let dir = MemoryDirectory::new();
    let (server, device, stranger) = (bind(&dir).await, bind(&dir).await, bind(&dir).await);
    let gate = PeerGate::allow_list([device.peer_id()]);
    gate.attach(&server);
    let mut admitted = serve_gated(&server, "svc", gate.clone());

    let conn = stranger.connect(server.peer_id(), "svc").await.unwrap();
    let reason = tokio::time::timeout(Duration::from_secs(2), conn.closed()).await.unwrap();
    assert!(matches!(reason, CloseReason::Remote { code: CLOSE_NOT_ALLOWED, .. }), "{reason:?}");
    assert_eq!(gate.live_connections(&stranger.peer_id()), 0);

    let _ok = device.connect(server.peer_id(), "svc").await.unwrap();
    assert_eq!(admitted.recv().await.unwrap().peer(), device.peer_id());
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n3_letify_set_allowed_drops_peers_no_longer_listed() {
    let dir = MemoryDirectory::new();
    let (server, a, b) = (bind(&dir).await, bind(&dir).await, bind(&dir).await);
    let gate = PeerGate::allow_list([a.peer_id(), b.peer_id()]);
    gate.attach(&server);
    let mut admitted = serve_gated(&server, "svc", gate.clone());
    let (ca, cb) = (
        a.connect(server.peer_id(), "svc").await.unwrap(),
        b.connect(server.peer_id(), "svc").await.unwrap(),
    );
    admitted.recv().await.unwrap();
    admitted.recv().await.unwrap();

    assert_eq!(gate.set_allowed([a.peer_id()]), 1);
    tokio::time::timeout(Duration::from_secs(1), cb.closed()).await.expect("b is cut");
    // a is untouched.
    assert!(ca.open_bi().await.is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n6_letify_revoking_a_key_by_its_sign_flipped_alias_revokes_the_key() {
    // The points A and -A convert to the same X25519 key, so letify-link treats them as one
    // identity (`NodeId::canonical`). Revoking either encoding cuts the device, which a gate that
    // compares exact bytes would not do.
    let dir = MemoryDirectory::new();
    let (server, device) = (bind(&dir).await, bind(&dir).await);
    let gate = PeerGate::open();
    gate.attach(&server);
    let mut admitted = serve_gated(&server, "svc", gate.clone());
    let conn = device.connect(server.peer_id(), "svc").await.unwrap();
    admitted.recv().await.unwrap();

    let mut flipped = *device.peer_id().as_bytes();
    flipped[31] ^= 0x80;
    let alias = PeerId::from_bytes(flipped).expect("the sign-flipped point is a valid, large-order key");
    assert_ne!(alias, device.peer_id());

    gate.revoke(&alias);
    let reason = tokio::time::timeout(Duration::from_secs(2), conn.closed()).await.expect("the device is cut");
    assert!(matches!(reason, CloseReason::Remote { code: CLOSE_REVOKED, .. }), "{reason:?}");
    assert!(device.connect(server.peer_id(), "svc").await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n6_letify_a_peer_id_with_a_torsion_component_cannot_be_dialed() {
    // Ember's PeerId accepts any canonical point of large order. letify-link also requires the
    // prime order subgroup, so such an id is refused at dial with a reason, not at the handshake.
    use ed25519_dalek::VerifyingKey;
    const ORDER_8: [u8; 32] = [
        0xc7, 0x17, 0x6a, 0x70, 0x3d, 0x4d, 0xd8, 0x4f, 0xba, 0x3c, 0x0b, 0x76, 0x0d, 0x10, 0x67, 0x0f, 0x2a, 0x20,
        0x53, 0xfa, 0x2c, 0x39, 0xcc, 0xc6, 0x4e, 0xc7, 0xfd, 0x77, 0x92, 0xac, 0x03, 0x7a,
    ];
    let honest = SecretKey::generate().peer_id();
    let key = VerifyingKey::from_bytes(honest.as_bytes()).unwrap();
    let small = VerifyingKey::from_bytes(&ORDER_8).unwrap();
    let mixed = (key.to_edwards() + small.to_edwards()).compress().to_bytes();
    let mixed = PeerId::from_bytes(mixed).expect("a mixed-order point is large order, so Ember accepts it");

    let dir = MemoryDirectory::new();
    let a = bind(&dir).await;
    match a.connect(mixed, "svc").await {
        Err(TransportError::Connect { reason, .. }) => assert!(reason.contains("prime order"), "{reason}"),
        other => panic!("expected a refusal naming the subgroup, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n3_letify_revocation_does_not_disturb_other_devices() {
    let dir = MemoryDirectory::new();
    let (server, a, b) = (bind(&dir).await, bind(&dir).await, bind(&dir).await);
    let gate = PeerGate::open();
    gate.attach(&server);
    spawn_echo(&server, "echo");
    let ca = a.connect(server.peer_id(), "echo").await.unwrap();
    let cb = b.connect(server.peer_id(), "echo").await.unwrap();
    gate.revoke(&a.peer_id());
    tokio::time::timeout(Duration::from_secs(1), ca.closed()).await.unwrap();
    assert_eq!(echo_roundtrip(&cb, b"b is fine").await, b"b is fine");
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n3_letify_revoking_a_peer_closes_the_sessions_we_started_to_it() {
    // The main server starts a session to a node (it is the initiator) and revokes the node:
    // that session closes and is not redialed, which `Session::close_when_revoked` and the
    // revoke hook both guarantee.
    let dir = MemoryDirectory::new();
    let (server, node) = (bind(&dir).await, bind(&dir).await);
    common::spawn_echo(&node, "echo");
    let conn = server.connect(node.peer_id(), "echo").await.unwrap();
    assert_eq!(echo_roundtrip(&conn, b"up").await, b"up");

    server.revoke_peer(node.peer_id());
    tokio::time::timeout(Duration::from_secs(2), conn.closed()).await.expect("the initiator session closes");
    // Nothing redials it: a new connection from the server needs a new session, and the node
    // still accepts (the server revoked only its own side), so this succeeds once allowed again.
    server.allow_peer(node.peer_id());
    let again = server.connect(node.peer_id(), "echo").await.unwrap();
    assert_eq!(echo_roundtrip(&again, b"back").await, b"back");
}
