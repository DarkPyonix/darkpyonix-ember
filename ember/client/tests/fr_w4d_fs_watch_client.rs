//! FR-W4d (#460): `Api::watch_fs` against an in-process server relaying a real node, over plain
//! HTTP and over the in-memory transport. Event timing varies, so waits poll with a bound.

use std::sync::Arc;
use std::time::Duration;

use ember_client::api::Api;
use ember_client::fs_watch::{FsChangeKind, FsWatch, FsWatchMessage};
use ember_node::api::{self, Node};
use ember_node::config::NodeConfig;
use ember_server::computers::{self, Computers, Registry};
use ember_server::devices::Devices;
use ember_server::store::Store;
use ember_transport::mem::MemNetwork;
use ember_transport::Dialer;

const TOKEN: &str = "node-token";
const BOUND: Duration = Duration::from_secs(20);

struct Fx {
    app: axum::Router,
    store: Arc<Store>,
    cid: String,
    root: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

async fn fx() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let node = Node::new(NodeConfig::new(TOKEN, vec![root.clone()])).unwrap();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let node_url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(api::serve(l, node));
    let store = Arc::new(Store::open_in_memory().unwrap());
    let c = Computers::new(Registry::new(store.clone()), computers::default_connector());
    let cid = c.registry().insert("box", &node_url, TOKEN).unwrap().id;
    Fx { app: computers::fs::router(c), store, cid, root, _dir: dir }
}

async fn next_until(w: &mut FsWatch, what: &str, want: impl Fn(&FsWatchMessage) -> bool) -> FsWatchMessage {
    let deadline = tokio::time::Instant::now() + BOUND;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, w.next()).await {
            Ok(Ok(Some(m))) if want(&m) => return m,
            Ok(Ok(Some(_))) => continue,
            other => panic!("no {what} within {BOUND:?}: {other:?}"),
        }
    }
}

/// Watch the root, write a file, see it, unwatch.
async fn exercise(api: &Api, f: &Fx) {
    let mut w = api.watch_fs(&f.cid).await.expect("the watch socket opens");
    let id = w.watch(&f.root, true).await.unwrap();
    let ok = next_until(&mut w, "watching", |m| !matches!(m, FsWatchMessage::Change { .. })).await;
    assert_eq!(ok, FsWatchMessage::Watching { id, path: f.root.clone() });
    let a = f.root.join("a.txt");
    std::fs::write(&a, "x").unwrap();
    next_until(&mut w, "the change", |m| match m {
        FsWatchMessage::Change { id: got, events } => {
            *got == id && events.iter().any(|e| e.path == a && matches!(e.kind, FsChangeKind::Created | FsChangeKind::Changed))
        }
        _ => false,
    })
    .await;
    w.unwatch(id).await.unwrap();
    next_until(&mut w, "unwatched", |m| *m == FsWatchMessage::Unwatched { id }).await;
    w.close().await;
}

#[tokio::test]
async fn fr_w4d_client_watches_over_http() {
    let f = fx().await;
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let app = f.app.clone();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let api = Api::with_token(&base, None).unwrap();
    exercise(&api, &f).await;
    // An unknown computer keeps the server's 404.
    let err = api.watch_fs("nope").await.err().expect("refused");
    assert!(matches!(err, ember_client::api::ApiError::Status { status: 404, .. }), "{err:?}");
}

#[tokio::test]
async fn fr_w4d_client_watches_over_the_transport() {
    let f = fx().await;
    let net = MemNetwork::new();
    let (server_t, device_t) = (net.transport(), net.transport());
    let devices = Devices::open(f.store.clone()).unwrap();
    devices.add(device_t.peer_id(), "phone").unwrap();
    tokio::spawn(ember_server::transport::serve(&server_t, f.app.clone(), devices.gate().clone()).unwrap());
    let api = Api::over_transport(Dialer::new(device_t), server_t.peer_id());
    exercise(&api, &f).await;
}
