//! FR-P3: the terminal attach WebSocket refuses a cross-site browser page (cross-site WebSocket
//! hijacking). Browsers send `Origin` on a WebSocket and apply no CORS, so the server checks it.
//! No `Origin` (native clients) and a same-host or loopback `Origin` still attach.

use std::sync::Arc;
use std::time::Duration;

use ember_node::api::{self, Node};
use ember_node::config::NodeConfig;
use ember_node::proto::*;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::AgentAdapter;
use ember_server::computers::{self, Computers, Registry};
use ember_server::session::Sessions;
use ember_server::store::Store;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

const TOKEN: &str = "node-token";

struct Fx {
    base: String,
    cid: String,
    tid: String,
    _dir: tempfile::TempDir,
}

async fn setup() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let node = Node::new(NodeConfig::new(TOKEN, vec![root.clone()])).unwrap();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let node_url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(api::serve(l, node));

    let store = Arc::new(Store::open_in_memory().unwrap());
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    let sessions = Sessions::new(store.clone(), adapters);
    let computers = Computers::new(Registry::new(store), computers::default_connector());
    let app = computers::terms::router(computers.clone()).merge(computers::api::router(computers, sessions));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = l.local_addr().unwrap().to_string();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });

    let c = reqwest::Client::new();
    let r = c
        .post(format!("http://{base}/api/computers"))
        .json(&json!({ "name": "box", "url": node_url, "token": TOKEN }))
        .send()
        .await
        .unwrap();
    let cid = r.json::<Value>().await.unwrap()["id"].as_str().unwrap().to_string();
    let r = c
        .post(format!("http://{base}/api/computers/{cid}/terms"))
        .json(&json!({
            "program": { "argv": ["/bin/sh"] }, "cwd": root, "env": { "PS1": "$ " },
            "size": { "rows": 24, "cols": 80 }, "origin": "ide-ember",
            "project": root.display().to_string(), "key": "k",
        }))
        .send()
        .await
        .unwrap();
    let created: TermCreateResponse = r.json().await.unwrap();
    Fx { base, cid, tid: created.term.id, _dir: dir }
}

fn hello() -> String {
    serde_json::to_string(&TermHello {
        device: "t".into(),
        kind: Some("test".into()),
        pid: None,
        size: None,
        active: true,
        read_only: false,
        snapshot: true,
    })
    .unwrap()
}

/// Try the upgrade with an optional `Origin`; Ok(attached) or Err(HTTP status).
async fn try_attach(fx: &Fx, origin: Option<&str>) -> Result<(), u16> {
    let url = format!("ws://{}/api/computers/{}/terms/{}/attach", fx.base, fx.cid, fx.tid);
    let mut req = url.into_client_request().unwrap();
    if let Some(o) = origin {
        req.headers_mut().insert("Origin", o.parse().unwrap());
    }
    match tokio_tungstenite::connect_async(req).await {
        Ok((mut ws, _)) => {
            ws.send(Message::text(hello())).await.unwrap();
            while let Some(Ok(m)) = ws.next().await {
                if let Message::Text(t) = m {
                    if let Ok(TermEvent::Attached { .. }) = serde_json::from_str(t.as_str()) {
                        return Ok(());
                    }
                }
            }
            panic!("closed before attached");
        }
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => Err(r.status().as_u16()),
        Err(e) => panic!("{e}"),
    }
}

async fn within<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), f).await.expect("timed out")
}

#[tokio::test]
async fn fr_p3_foreign_origin_is_refused_and_never_reaches_the_node() {
    within(async {
        let fx = setup().await;
        assert_eq!(try_attach(&fx, Some("https://evil.example")).await, Err(403));
        assert_eq!(try_attach(&fx, Some("null")).await, Err(403));
        // The node never saw an attachment.
        let one: TermInfo = reqwest::get(format!("http://{}/api/computers/{}/terms/{}", fx.base, fx.cid, fx.tid))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(one.clients.len(), 0);
    })
    .await;
}

#[tokio::test]
async fn fr_p3_no_origin_header_still_attaches() {
    within(async {
        let fx = setup().await;
        assert_eq!(try_attach(&fx, None).await, Ok(()));
    })
    .await;
}

#[tokio::test]
async fn fr_p3_same_host_and_loopback_origin_attach() {
    within(async {
        let fx = setup().await;
        assert_eq!(try_attach(&fx, Some(&format!("http://{}", fx.base))).await, Ok(()));
        assert_eq!(try_attach(&fx, Some("http://localhost:5173")).await, Ok(()));
    })
    .await;
}
