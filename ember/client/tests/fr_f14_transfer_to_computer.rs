//! The transfer client sending to a computer other than the server (SPEC `FR-F14`, `FR-F13`,
//! #773): the real server relay router in-process, a real ember node behind the in-memory fake
//! transport.

use std::time::Duration;

use ember_client::api::Api;
use ember_client::transfers::{offer_to, prepare, sha256_hex, upload_to, TransferError, TransferTarget, UploadEnd, UploadOptions};
use ember_node::api::Node;
use ember_node::config::NodeConfig;
use ember_server::computers::{self, Computers, Registry};
use ember_transport::mem::MemNetwork;
use ember_transport::{Dialer, PeerGate};

const TOKEN: &str = "node-token";

#[tokio::test]
async fn fr_f14_upload_to_a_computer_stores_the_file_on_its_node() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let root = base.join("root");
    let state = base.join("node-state");
    std::fs::create_dir_all(&root).unwrap();

    let net = MemNetwork::new();
    let (server_t, node_t) = (net.transport(), net.transport());
    let node = Node::new(NodeConfig { state_dir: Some(state.clone()), ..NodeConfig::new(TOKEN, vec![root]) }).unwrap();
    tokio::spawn(ember_node::transport::serve(&node_t, node, PeerGate::allow_list([server_t.peer_id()])).unwrap());
    let dialer = Dialer::new(server_t);
    let comps = Computers::with_transport(
        Registry::open_in_memory().unwrap(),
        computers::connector(Some(dialer.clone())),
        None,
        Some(dialer),
    );
    let pi = comps.register_peer("pi", &node_t.local_addr(), TOKEN).unwrap().id;
    let lan = comps.register("lan", "http://127.0.0.1:9", TOKEN).unwrap().id;

    let app = computers::transfers::router(comps);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let api = Api::with_token(&format!("http://{addr}"), None).unwrap();

    let data: Vec<u8> = (0..700_000u32).map(|i| (i % 253) as u8).collect();
    let src = base.join("photo.jpg");
    tokio::fs::write(&src, &data).await.unwrap();
    let files = prepare(&[src]).await.unwrap();

    let target = TransferTarget::Computer(pi);
    let created = offer_to(&api, &target, "MacBook", None, &files).await.unwrap();
    assert_eq!(created.status, "accepted");
    let opts = UploadOptions {
        chunk_bytes: 256 * 1024,
        poll: Duration::from_millis(20),
        answer_timeout: Duration::from_secs(10),
        max_retries: 10,
        retry_delay: Duration::from_millis(40),
    };
    let end = tokio::time::timeout(Duration::from_secs(30), upload_to(&api, &target, &created.id, &files, &opts, &mut |_| {}))
        .await
        .unwrap()
        .unwrap();
    match end {
        UploadEnd::Done(stored) => assert_eq!(stored[0].sha256, sha256_hex(&data)),
        other => panic!("unexpected end {other:?}"),
    }
    assert_eq!(std::fs::read(state.join("received/photo.jpg")).unwrap(), data);

    // A computer registered by plain HTTP is refused with its own reason.
    let refused = offer_to(&api, &TransferTarget::Computer(lan), "MacBook", None, &files).await.unwrap_err();
    assert_eq!(refused, TransferError::InsecureTarget);
}
