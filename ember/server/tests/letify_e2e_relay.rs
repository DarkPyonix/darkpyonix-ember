//! SPEC FR-N2 and FR-N5: client and node traffic crosses the main server relay.

use std::net::{SocketAddr, TcpListener};
use std::time::Duration;

use ember_node::api::Node;
use ember_node::client::{NodeClient, NODE_SERVICE};
use ember_node::config::NodeConfig;
use ember_server::transport::{with_main_server_roles, ServerTransport};
use ember_transport::{
    BackendKind, LetifyConfig, MemorySignalHub, PathState, PeerGate, RelayConfig, SecretKey,
    Transport, TransportConfig,
};

#[tokio::test(flavor = "multi_thread")]
async fn fr_n2_client_node_traffic_relayed_through_main_server() {
    let signals = MemorySignalHub::new();
    let node_key = SecretKey::generate();
    let client_key = SecretKey::generate();
    let node_id = node_key.peer_id();
    let client_id = client_key.peer_id();

    let server_config = TransportConfig::new(SecretKey::generate())
        .backend(BackendKind::Letify)
        .bind_addr("127.0.0.1:0".parse().unwrap())
        .letify(LetifyConfig {
            relay_listen: Some("127.0.0.1:0".parse().unwrap()),
            qad_listen: Some("127.0.0.1:0".parse().unwrap()),
            ..LetifyConfig::default()
        });
    let server = Transport::bind(with_main_server_roles(server_config, 0)).await.unwrap();
    let relay_gate = PeerGate::allow_list([node_id, client_id]);
    relay_gate.attach(&server);
    let relay_url = server.local_addr().relays.into_iter().next().expect("main server relay URL");

    // Reserve both advertised ports without accepting connections, blocking direct sessions.
    let blocked_node = TcpListener::bind("127.0.0.1:0").unwrap();
    let blocked_client = TcpListener::bind("127.0.0.1:0").unwrap();
    let device = |key: SecretKey, blocked: SocketAddr| {
        let config = LetifyConfig {
            signal: Some(signals.channel(key.peer_id())),
            advertise: vec![blocked],
            connect_timeout: Duration::from_secs(20),
            ..LetifyConfig::default()
        };
        TransportConfig::new(key)
            .backend(BackendKind::Letify)
            .bind_addr("127.0.0.1:0".parse().unwrap())
            .relay(RelayConfig::Custom(vec![relay_url.clone()]))
            .letify(config)
    };
    let node_transport = Transport::bind(device(node_key, blocked_node.local_addr().unwrap())).await.unwrap();
    let client_transport = Transport::bind(device(client_key, blocked_client.local_addr().unwrap())).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let node = Node::new(NodeConfig::new("node-token", vec![root.clone()])).unwrap();
    let node_gate = PeerGate::allow_list([client_id]);
    let task = tokio::spawn(ember_node::transport::serve(&node_transport, node, node_gate).unwrap());

    let dialer = ServerTransport::new(client_transport.clone()).dialer;
    let addr = node_transport.local_addr();
    let client = NodeClient::over_transport(dialer.clone(), addr.clone(), "node-token");
    tokio::time::timeout(Duration::from_secs(30), async {
        assert!(client.health().await.unwrap().ok);
        let path = root.join("relay.txt");
        let data = vec![42u8; 2 << 20];
        client.write_file(&path, data.clone(), None).await.unwrap();
        assert_eq!(client.read_file(&path).await.unwrap().data, data);
        let conn = dialer.connection(&addr, NODE_SERVICE).await.unwrap();
        assert!(matches!(conn.path_state(), PathState::Relayed { .. }), "expected relayed path, got {}", conn.path_state());
    })
    .await
    .expect("relayed node operations timed out");

    client_transport.close().await;
    node_transport.close().await;
    server.close().await;
    task.abort();
}
