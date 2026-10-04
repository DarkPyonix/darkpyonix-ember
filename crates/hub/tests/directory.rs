//! Address directory through the hub (FR-N2): each endpoint publishes its signed record to the
//! hub's pkarr endpoint, and a peer of the same account dials it by peer id alone. Localhost
//! only (relay disabled, direct addresses published); needs no internet.

use std::time::{Duration, Instant};

use ember_hub::fake::FakeHub;
use ember_hub::{HubConfig, Role};
use ember_transport::{Connection, PeerId, RelayConfig, SecretKey, Transport};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn bind(hub: &FakeHub, key: SecretKey, token: Option<String>) -> Transport {
    let cfg = HubConfig::new(hub.url())
        .transport_config(key, token)
        .relay(RelayConfig::Disabled)
        .bind_addr("127.0.0.1:0".parse().unwrap());
    Transport::bind(cfg).await.unwrap()
}

async fn wait_published(hub: &FakeHub, peer: &PeerId) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while hub.record(peer).is_none() {
        assert!(Instant::now() < deadline, "endpoint never published to the hub");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn spawn_echo(t: &Transport, service: &str) {
    let mut listener = t.listen(service).unwrap();
    tokio::spawn(async move {
        while let Some(conn) = listener.accept().await {
            tokio::spawn(async move {
                while let Ok(stream) = conn.accept_bi().await {
                    let (mut w, mut r) = stream.into_split();
                    tokio::io::copy(&mut r, &mut w).await.ok();
                    w.shutdown().await.ok();
                }
            });
        }
    });
}

async fn echo(conn: &Connection, msg: &[u8]) -> Vec<u8> {
    let (mut w, mut r) = conn.open_bi().await.unwrap().into_split();
    w.write_all(msg).await.unwrap();
    w.shutdown().await.unwrap();
    let mut out = Vec::new();
    r.read_to_end(&mut out).await.unwrap();
    out
}

#[tokio::test]
async fn publish_and_resolve_through_the_hub() {
    let hub = FakeHub::start().await;
    let (ka, kb) = (SecretKey::generate(), SecretKey::generate());
    let ta = hub.register(ka.peer_id(), "server", Role::MainServer);
    hub.register(kb.peer_id(), "node", Role::Computer);

    // The resolver's URL carries the read-only resolve token, never the device token (NFR-H2).
    let a = bind(&hub, ka.clone(), hub.resolve_token(&ka.peer_id())).await;
    let b = bind(&hub, kb.clone(), hub.resolve_token(&kb.peer_id())).await;
    spawn_echo(&b, "echo");
    wait_published(&hub, &b.peer_id()).await;

    // The JSON view of the same record.
    let rec = hub.client().with_token(&ta).addresses(&b.peer_id()).await.unwrap();
    assert_eq!(rec.endpoint_id, b.peer_id());
    assert!(!rec.to_peer_addr().direct.is_empty(), "direct addresses were published: {rec:?}");

    // Dial by peer id only: resolved through the hub.
    let conn = tokio::time::timeout(Duration::from_secs(15), a.connect(b.peer_id(), "echo"))
        .await
        .expect("connect timed out")
        .unwrap();
    assert_eq!(echo(&conn, b"via hub").await, b"via hub");
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn unregistered_endpoints_are_not_published_and_tokenless_peers_resolve_nothing() {
    let hub = FakeHub::start().await;
    let kb = SecretKey::generate();
    hub.register(kb.peer_id(), "node", Role::Computer);
    let b = bind(&hub, kb.clone(), hub.resolve_token(&kb.peer_id())).await;
    spawn_echo(&b, "echo");
    wait_published(&hub, &b.peer_id()).await;

    // Not registered: its publishes are refused (403), and without a token it cannot resolve.
    let stranger = bind(&hub, SecretKey::generate(), None).await;
    let r = tokio::time::timeout(Duration::from_secs(5), stranger.connect(b.peer_id(), "echo")).await;
    assert!(!matches!(r, Ok(Ok(_))), "a tokenless endpoint must not find the peer");
    assert!(hub.record(&stranger.peer_id()).is_none());

    // Registering later and setting the token at runtime enables resolving.
    // A device token in the resolver URL is refused by the hub; the resolve token works.
    let device_token = hub.register(stranger.peer_id(), "late", Role::Computer);
    stranger.set_directory_token(Some(device_token));
    let r = tokio::time::timeout(Duration::from_secs(5), stranger.connect(b.peer_id(), "echo")).await;
    assert!(!matches!(r, Ok(Ok(_))), "a device token must not resolve through /pkarr?token=");
    stranger.set_directory_token(hub.resolve_token(&stranger.peer_id()));
    let conn = tokio::time::timeout(Duration::from_secs(15), stranger.connect(b.peer_id(), "echo"))
        .await
        .expect("connect timed out")
        .unwrap();
    assert_eq!(echo(&conn, b"late").await, b"late");

    // Once the hub removes b, its record is gone.
    assert!(hub.remove(&b.peer_id()));
    assert!(hub.record(&b.peer_id()).is_none());
    stranger.close().await;
    b.close().await;
}
