//! FR-W4d (#460): `GET /api/computers/{id}/fs/watch` relays the node's file watch socket, with
//! the gates of the other fs relay routes. A real `ember_node` over TCP, a real server listener,
//! and the in-memory transport for the device paths.
//!
//! File system event timing varies, so every wait polls with a generous bound.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ember_node::api::{self, Node};
use ember_node::client::NodeClient;
use ember_node::config::NodeConfig;
use ember_node::fs_watch::WatchLimits;
use ember_server::computers::{self, Computers, Registry};
use ember_server::devices::{Devices, Scope};
use ember_server::store::Store;
use ember_server::transport::{self as server_transport, SERVER_SERVICE};
use ember_transport::mem::MemNetwork;
use ember_transport::{Dialer, PeerAddr, Transport};
use futures::{SinkExt, Stream, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

const TOKEN: &str = "node-token";
const BOUND: Duration = Duration::from_secs(20);
/// One heartbeat, the bound FR-N3 gives for a revoked device's streams.
const HEARTBEAT: Duration = Duration::from_secs(5);

struct Fx {
    base: String,
    cid: String,
    root: std::path::PathBuf,
    outside: std::path::PathBuf,
    node: NodeClient,
    app: axum::Router,
    store: Arc<Store>,
    _dir: tempfile::TempDir,
}

/// A node with one root and the given watch limits, registered on a server serving the fs relay.
async fn fx(limits: WatchLimits) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().canonicalize().unwrap();
    let root = tmp.join("root");
    let outside = tmp.join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let node = Node::new(NodeConfig::new(TOKEN, vec![root.clone()])).unwrap().with_watch_limits(limits);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let node_url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(api::serve(l, node));

    let store = Arc::new(Store::open_in_memory().unwrap());
    let computers = Computers::new(Registry::new(store.clone()), computers::default_connector());
    let cid = computers.registry().insert("box", &node_url, TOKEN).unwrap().id;
    let app = computers::fs::router(computers);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("127.0.0.1:{}", l.local_addr().unwrap().port());
    let served = app.clone();
    tokio::spawn(async move { axum::serve(l, served).await.unwrap() });
    Fx { base, cid, root, outside, node: NodeClient::new(&node_url, TOKEN).unwrap(), app, store, _dir: dir }
}

async fn send<S>(ws: &mut S, v: Value)
where
    S: futures::Sink<Message> + Unpin,
    S::Error: std::fmt::Debug,
{
    ws.send(Message::Text(v.to_string().into())).await.unwrap();
}

async fn next<S>(ws: &mut S, within: Duration) -> Option<Value>
where
    S: Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let fut = async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => return Some(serde_json::from_str::<Value>(t.as_str()).unwrap()),
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return None,
                Some(Ok(_)) => continue,
            }
        }
    };
    tokio::time::timeout(within, fut).await.ok().flatten()
}

async fn until<S>(ws: &mut S, what: &str, want: impl Fn(&Value) -> bool) -> Value
where
    S: Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let deadline = tokio::time::Instant::now() + BOUND;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match next(ws, left).await {
            Some(v) if want(&v) => return v,
            Some(_) => continue,
            None => panic!("no frame with {what} within {BOUND:?}"),
        }
    }
}

#[tokio::test]
async fn fr_w4d_relay_round_trip_to_a_node() {
    let f = fx(WatchLimits::default()).await;
    let url = format!("ws://{}/api/computers/{}/fs/watch", f.base, f.cid);
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    send(&mut ws, json!({ "op": "watch", "id": 1, "path": f.root, "recursive": true })).await;
    let ok = until(&mut ws, "watching", |v| v["id"] == 1).await;
    assert_eq!(ok["op"], "watching", "{ok}");
    let a = f.root.join("a.txt");
    std::fs::write(&a, "x").unwrap();
    let v = until(&mut ws, "a change", |v| v["op"] == "change").await;
    assert!(v["events"].as_array().unwrap().iter().any(|e| e["path"] == json!(a)), "{v}");

    send(&mut ws, json!({ "op": "watch", "id": 2, "path": f.outside, "recursive": true })).await;
    let refused = until(&mut ws, "the refusal", |v| v["id"] == 2).await;
    assert_eq!(refused["code"], "forbidden_path", "the node's answer passes unchanged: {refused}");

    // A binary frame ends the relay.
    ws.send(Message::Binary(vec![1, 2, 3].into())).await.unwrap();
    assert!(next(&mut ws, BOUND).await.is_none(), "the socket stayed open after a binary frame");
}

#[tokio::test]
async fn fr_w4d_relay_unknown_computer_is_404_and_a_bad_origin_403() {
    let f = fx(WatchLimits::default()).await;
    let url = format!("ws://{}/api/computers/nope/fs/watch", f.base);
    match tokio_tungstenite::connect_async(url).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => assert_eq!(r.status(), 404),
        other => panic!("expected 404, got {other:?}"),
    }
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://{}/api/computers/{}/fs/watch", f.base, f.cid).into_client_request().unwrap();
    req.headers_mut().insert("origin", "https://evil.example".parse().unwrap());
    match tokio_tungstenite::connect_async(req).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => assert_eq!(r.status(), 403),
        other => panic!("expected 403, got {other:?}"),
    }
}

#[tokio::test]
async fn fr_w4d_relay_local_computer_reaches_the_in_process_node() {
    let c = Computers::new(Registry::new(Arc::new(Store::open_in_memory().unwrap())), computers::default_connector());
    let app = computers::fs::router(c);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = l.local_addr().unwrap().to_string();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{base}/api/computers/local/fs/watch")).await.unwrap();
    let home = std::env::var("HOME").unwrap();
    send(&mut ws, json!({ "op": "watch", "id": 1, "path": home, "recursive": false })).await;
    let ok = until(&mut ws, "watching", |v| v["id"] == 1).await;
    assert_eq!(ok["op"], "watching", "{ok}");
}

/// The transport door as `main.rs` serves it, with device scopes.
fn scoped(net: &MemNetwork, f: &Fx) -> (Transport, Arc<Devices>) {
    let devices = Devices::open(f.store.clone()).unwrap();
    let server_t = net.transport();
    tokio::spawn(server_transport::serve_scoped(&server_t, f.app.clone(), devices.clone()).unwrap());
    (server_t, devices)
}

async fn device_ws(
    device: &Dialer,
    server: &Transport,
    path: &str,
) -> Result<tokio_tungstenite::WebSocketStream<ember_transport::BiStream>, tokio_tungstenite::tungstenite::Error> {
    let stream = device.open_bi(&PeerAddr::new(server.peer_id()), SERVER_SERVICE).await.expect("stream");
    tokio_tungstenite::client_async(format!("ws://ember-server{path}"), stream).await.map(|(ws, _)| ws)
}

#[tokio::test]
async fn fr_w4d_relay_refuses_read_only_and_chat_only_devices() {
    let f = fx(WatchLimits::default()).await;
    let net = MemNetwork::new();
    let (server_t, devices) = scoped(&net, &f);
    let path = format!("/api/computers/{}/fs/watch", f.cid);
    for scope in [Scope::ReadOnly, Scope::ChatOnly] {
        let t = net.transport();
        devices.add(t.peer_id(), "phone").unwrap();
        devices.set_scope(&t.peer_id(), scope).unwrap();
        match device_ws(&Dialer::new(t), &server_t, &path).await {
            Err(tokio_tungstenite::tungstenite::Error::Http(r)) => {
                assert_eq!(r.status(), 403, "{scope:?}");
                let body: Value = serde_json::from_slice(r.body().as_deref().unwrap_or_default()).unwrap();
                assert_eq!(body["code"], "scope_denied", "{scope:?}");
            }
            other => panic!("{scope:?} was not refused: {other:?}"),
        }
    }
}

#[tokio::test]
async fn fr_w4d_relay_full_device_works_and_a_revoke_ends_both_sockets() {
    // One watch per node: a second socket can watch only once the relayed one is gone.
    let f = fx(WatchLimits { total: 1, ..WatchLimits::default() }).await;
    let net = MemNetwork::new();
    let (server_t, devices) = scoped(&net, &f);
    let t = net.transport();
    devices.add(t.peer_id(), "laptop").unwrap();
    let mut ws = device_ws(&Dialer::new(t.clone()), &server_t, &format!("/api/computers/{}/fs/watch", f.cid))
        .await
        .expect("a full device opens the watch socket");
    send(&mut ws, json!({ "op": "watch", "id": 1, "path": f.root, "recursive": true })).await;
    assert_eq!(until(&mut ws, "watching", |v| v["id"] == 1).await["op"], "watching");

    let started = Instant::now();
    devices.remove(&t.peer_id()).unwrap();
    loop {
        match next(&mut ws, HEARTBEAT).await {
            None => break,
            Some(v) => assert!(started.elapsed() < HEARTBEAT, "still open after the revoke: {v}"),
        }
    }
    assert!(started.elapsed() <= HEARTBEAT + Duration::from_secs(1), "ended after {:?}", started.elapsed());

    // The node socket closed too: its watch was given back.
    let mut direct = f.node.fs_watch_socket().await.unwrap();
    let deadline = Instant::now() + BOUND;
    let mut id = 1;
    loop {
        send(&mut direct, json!({ "op": "watch", "id": id, "path": f.root, "recursive": true })).await;
        let v = until(&mut direct, "an answer", |v| v["id"] == id).await;
        if v["op"] == "watching" {
            break;
        }
        assert!(Instant::now() < deadline, "the relayed node socket kept its watch: {v}");
        id += 1;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
