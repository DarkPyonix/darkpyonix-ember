//! SPEC FR-N1 and FR-N5: the server reaches a real node over letify on loopback.

use std::time::Duration;

use ember_node::api::Node;
use ember_node::client::{shell, NodeClient, NODE_SERVICE};
use ember_node::config::NodeConfig;
use ember_server::transport::ServerTransport;
use ember_transport::{BackendKind, PeerGate, SecretKey, Transport, TransportConfig};

async fn endpoint() -> Transport {
    let config = TransportConfig::new(SecretKey::generate())
        .backend(BackendKind::Letify)
        .bind_addr("127.0.0.1:0".parse().unwrap());
    Transport::bind(config).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n1_server_node_over_real_letify_transport() {
    let server = endpoint().await;
    let node_transport = endpoint().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let node = Node::new(NodeConfig::new("node-token", vec![root.clone()])).unwrap();
    let gate = PeerGate::allow_list([server.peer_id()]);
    let task = tokio::spawn(ember_node::transport::serve(&node_transport, node, gate).unwrap());

    let server_transport = ServerTransport::new(server.clone());
    let addr = node_transport.local_addr();
    let client = NodeClient::over_transport(server_transport.dialer.clone(), addr.clone(), "node-token");
    tokio::time::timeout(Duration::from_secs(20), async {
        assert!(client.health().await.unwrap().ok);
        let path = root.join("hello.txt");
        client.write_file(&path, b"over letify".to_vec(), None).await.unwrap();
        assert_eq!(client.read_file(&path).await.unwrap().data, b"over letify");
        let output = client.run(shell("printf 'node-42'", &root)).await.unwrap();
        assert_eq!(output.stdout, b"node-42");
        assert_eq!(output.code, Some(0));
        let conn = server_transport.dialer.connection(&addr, NODE_SERVICE).await.unwrap();
        assert!(conn.path_state().is_direct(), "expected direct TCP, got {}", conn.path_state());
    })
    .await
    .expect("node operations timed out");

    server.close().await;
    node_transport.close().await;
    task.abort();
}
