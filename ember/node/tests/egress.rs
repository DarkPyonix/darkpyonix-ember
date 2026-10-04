//! `/v1/egress` (FR-R1): a SOCKS5 handshake and CONNECT carried over the node's WebSocket
//! stream to a local TCP echo server, the half-close convention, the token and the policy.

use std::net::SocketAddr;
use std::time::Duration;

use ember_node::api::{self, Node};
use ember_node::client::{ClientError, NodeClient};
use ember_node::config::NodeConfig;
use ember_node::egress::{self, reply, EgressPolicy, Target};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TOKEN: &str = "egress-token";

async fn node(policy: EgressPolicy) -> String {
    let dir = std::env::temp_dir();
    let mut cfg = NodeConfig::new(TOKEN, vec![dir]);
    cfg.egress = policy;
    let node = Node::new(cfg).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node));
    base
}

/// Echoes everything, then closes its write side once the client half-closed.
async fn echo() -> SocketAddr {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
                let _ = w.shutdown().await;
            });
        }
    });
    addr
}

/// A local byte stream whose other end is bridged to a fresh `/v1/egress` stream, exactly what
/// ember server's loopback listener does for each Chrome connection.
async fn egress_stream(client: &NodeClient) -> tokio::io::DuplexStream {
    let ws = client.egress().await.unwrap();
    let (ours, theirs) = tokio::io::duplex(64 * 1024);
    tokio::spawn(egress::bridge(theirs, ws));
    ours
}

async fn within<T>(fut: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(20), fut).await.expect("timed out")
}

#[tokio::test]
async fn socks5_connect_through_the_node_stream() {
    let base = node(EgressPolicy::default()).await;
    let target = echo().await;
    let client = NodeClient::new(&base, TOKEN).unwrap();
    within(async {
        // IPv4 CONNECT, data both ways.
        let mut s = egress_stream(&client).await;
        assert_eq!(egress::client_connect(&mut s, &Target::Ip(target), None).await.unwrap(), reply::SUCCEEDED);
        s.write_all(b"hello through the node").await.unwrap();
        let mut got = vec![0u8; 22];
        s.read_exact(&mut got).await.unwrap();
        assert_eq!(got, b"hello through the node");

        // Half-close: our EOF reaches the target, the target's EOF comes back, the rest arrives.
        s.write_all(b"tail").await.unwrap();
        s.shutdown().await.unwrap();
        let mut rest = Vec::new();
        s.read_to_end(&mut rest).await.unwrap();
        assert_eq!(rest, b"tail");

        // Domain CONNECT, resolved on the node.
        let mut s = egress_stream(&client).await;
        let t = Target::Domain("localhost".into(), target.port());
        assert_eq!(egress::client_connect(&mut s, &t, None).await.unwrap(), reply::SUCCEEDED);
        s.write_all(b"x").await.unwrap();
        let mut one = [0u8; 1];
        s.read_exact(&mut one).await.unwrap();
        assert_eq!(&one, b"x");
    })
    .await;
}

#[tokio::test]
async fn token_and_policy_are_enforced() {
    let base = node(EgressPolicy::with_deny_list("loopback").unwrap()).await;
    let target = echo().await;

    let Err(e) = NodeClient::new(&base, "wrong").unwrap().egress().await else { panic!("refused") };
    assert!(matches!(e, ClientError::Api { status: 401, .. }), "{e:?}");

    let client = NodeClient::new(&base, TOKEN).unwrap();
    within(async {
        let mut s = egress_stream(&client).await;
        assert_eq!(egress::client_connect(&mut s, &Target::Ip(target), None).await.unwrap(), reply::NOT_ALLOWED);
    })
    .await;

    let off = node(EgressPolicy { enabled: false, deny: Vec::new() }).await;
    let Err(e) = NodeClient::new(&off, TOKEN).unwrap().egress().await else { panic!("refused") };
    assert!(matches!(e, ClientError::Api { status: 403, .. }), "{e:?}");
}
