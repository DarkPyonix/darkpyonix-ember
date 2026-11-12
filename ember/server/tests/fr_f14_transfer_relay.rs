//! File transfer relayed to a computer's node (SPEC `FR-F14`, `FR-F6`, `FR-F15`, #773), through
//! the in-memory fake transport against a real ember node.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use ember_node::api::Node;
use ember_node::config::NodeConfig;
use ember_server::computers::{self, Computers, Registry};
use ember_transport::mem::MemNetwork;
use ember_transport::{Dialer, PeerGate, Transport};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

const TOKEN: &str = "node-token";

struct Fx {
    app: axum::Router,
    computers: Arc<Computers>,
    pi: String,
    node: Node,
    node_state: PathBuf,
    server_data: PathBuf,
    _node_t: Transport,
    _net: MemNetwork,
    _dir: tempfile::TempDir,
}

async fn call(app: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(match body {
            Some(v) => Body::from(serde_json::to_vec(&v).unwrap()),
            None => Body::empty(),
        })
        .unwrap();
    let resp = tokio::time::timeout(Duration::from_secs(20), app.clone().oneshot(req)).await.unwrap().unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn hex256(data: &[u8]) -> String {
    let d = Sha256::digest(data);
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(files_under(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

async fn start() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let root = base.join("root");
    let node_state = base.join("node-state");
    let server_data = base.join("server-data");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&server_data).unwrap();

    let net = MemNetwork::new();
    let (server_t, node_t) = (net.transport(), net.transport());
    let node = Node::new(NodeConfig { state_dir: Some(node_state.clone()), ..NodeConfig::new(TOKEN, vec![root]) }).unwrap();
    let gate = PeerGate::allow_list([server_t.peer_id()]);
    tokio::spawn(ember_node::transport::serve(&node_t, node.clone(), gate).unwrap());

    let dialer = Dialer::new(server_t);
    let computers = Computers::with_transport(
        Registry::open_in_memory().unwrap(),
        computers::connector(Some(dialer.clone())),
        None,
        Some(dialer),
    );
    let pi = computers.register_peer("pi", &node_t.local_addr(), TOKEN).unwrap().id;
    let client = computers.client(&pi).unwrap().with_deadline(Duration::from_secs(2));
    tokio::time::timeout(Duration::from_secs(20), async {
        while client.health().await.is_err() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let app = computers::transfers::router(computers.clone());
    Fx { app, computers, pi, node, node_state, server_data, _node_t: node_t, _net: net, _dir: dir }
}

fn chunk_body(data: &[u8], offset: u64) -> Value {
    json!({
        "file": 0,
        "offset": offset,
        "data_base64": base64::engine::general_purpose::STANDARD.encode(data),
        "sha256": hex256(data),
    })
}

#[tokio::test]
async fn fr_f14_a_relayed_transfer_lands_on_the_node_and_nothing_on_the_server() {
    let f = start().await;
    let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let pre = format!("/api/computers/{}/transfers", f.pi);
    let (st, v) = call(
        &f.app,
        "POST",
        &format!("{pre}/offers"),
        Some(json!({ "from": "MacBook", "files": [{ "name": "film.mov", "size": data.len(), "sha256": hex256(&data) }] })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    assert_eq!(v["status"], "accepted", "an owner offer to an owned computer is accepted at once: {v}");
    let id = v["id"].as_str().unwrap().to_string();

    // Send half, then ask where to continue, as a sender after a dropped connection does.
    let half = 150_000;
    for (i, part) in data[..half].chunks(50_000).enumerate() {
        let (st, v) = call(&f.app, "POST", &format!("{pre}/{id}/chunks"), Some(chunk_body(part, (i * 50_000) as u64))).await;
        assert_eq!(st, StatusCode::OK, "{v}");
    }
    let (st, v) = call(&f.app, "GET", &format!("{pre}/{id}/resume-offset/0"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["offset"], half);
    let (st, v) = call(&f.app, "POST", &format!("{pre}/{id}/chunks"), Some(chunk_body(&data[half..], half as u64))).await;
    assert_eq!(st, StatusCode::OK, "{v}");

    // A chunk with a wrong hash is refused by the node and its answer comes back unchanged.
    let mut bad = chunk_body(b"x", data.len() as u64);
    bad["sha256"] = json!(hex256(b"y"));
    let (st, _) = call(&f.app, "POST", &format!("{pre}/{id}/chunks"), Some(bad)).await;
    assert!(st.is_client_error());

    let (st, v) = call(&f.app, "POST", &format!("{pre}/{id}/complete"), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["stored"][0]["sha256"], hex256(&data));
    assert_eq!(std::fs::read(f.node_state.join("received/film.mov")).unwrap(), data);
    assert!(files_under(&f.server_data).is_empty(), "the server stores nothing (FR-F6)");

    let (st, v) = call(&f.app, "GET", &pre, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["history"][0]["result"], "done");
}

#[tokio::test]
async fn fr_f14_the_server_marks_relayed_offers_as_the_owners_even_if_the_body_says_otherwise() {
    let f = start().await;
    let pre = format!("/api/computers/{}/transfers", f.pi);
    let (st, v) = call(
        &f.app,
        "POST",
        &format!("{pre}/offers"),
        Some(json!({ "from": "MacBook", "owner": false, "files": [{ "name": "a", "size": 1 }] })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    assert_eq!(v["status"], "accepted");
}

#[tokio::test]
async fn fr_f15_a_non_owner_offer_waits_and_cannot_be_answered_through_the_server() {
    let f = start().await;
    // Not through the server relay: an offer without the owner mark, as a non-owner path would send.
    let v = f
        .node
        .transfers()
        .offer(serde_json::from_value(json!({ "from": "Guest", "files": [{ "name": "a", "size": 1 }] })).unwrap())
        .unwrap();
    assert_eq!(v["status"], "offered");
    let oid = v["id"].as_str().unwrap().to_string();
    let pre = format!("/api/computers/{}/transfers", f.pi);
    let (st, snap) = call(&f.app, "GET", &pre, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(snap["pending_offers"][0]["id"], oid.as_str());
    let (st, _) = call(&f.app, "POST", &format!("{pre}/offers/{oid}/accept"), None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, snap) = call(&f.app, "GET", &pre, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(snap["pending_offers"][0]["id"], oid.as_str(), "still waiting");
}

#[tokio::test]
async fn fr_f14_a_plain_http_computer_is_refused_before_anything_reaches_it() {
    let f = start().await;
    // A listener that records whether anything connected.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hit = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let h = hit.clone();
    tokio::spawn(async move {
        if listener.accept().await.is_ok() {
            h.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    });
    let lan = f.computers.register("lan", &format!("http://{addr}"), TOKEN).unwrap().id;
    let (st, v) = call(
        &f.app,
        "POST",
        &format!("/api/computers/{lan}/transfers/offers"),
        Some(json!({ "from": "MacBook", "files": [{ "name": "a", "size": 1 }] })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert!(v["error"].as_str().unwrap().contains("InsecureTarget"), "{v}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!hit.load(std::sync::atomic::Ordering::SeqCst), "nothing was sent to the plain HTTP node");
}

#[tokio::test]
async fn fr_f14_unknown_local_and_bad_ids_are_refused() {
    let f = start().await;
    let (st, _) = call(&f.app, "GET", "/api/computers/nope/transfers", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call(&f.app, "GET", "/api/computers/local/transfers", None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call(&f.app, "POST", &format!("/api/computers/{}/transfers/a.b/complete", f.pi), None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call(&f.app, "POST", &format!("/api/computers/{}/transfers/tr-1/explode", f.pi), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}
