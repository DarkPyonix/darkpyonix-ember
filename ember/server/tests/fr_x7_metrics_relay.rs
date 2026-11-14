//! FR-X7: the node's metrics sample relayed under `/api/computers/{id}/metrics`, against a
//! real `ember_node` and a real server listener.

use std::sync::Arc;
use std::time::Duration;

use ember_node::api::{self, Node};
use ember_node::config::NodeConfig;
use ember_node::proto::MetricsSample;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::AgentAdapter;
use ember_server::computers::{self, Computers, Registry};
use ember_server::session::Sessions;
use ember_server::store::Store;
use serde_json::{json, Value};

const TOKEN: &str = "node-token";

async fn within<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), f).await.expect("timed out")
}

#[tokio::test]
async fn fr_x7_metrics_relay_returns_the_node_sample_and_errors_map() {
    within(async {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let node = Node::new(NodeConfig::new(TOKEN, vec![root])).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let node_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(api::serve(listener, node));

        let store = Arc::new(Store::open_in_memory().unwrap());
        let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
        let sessions = Sessions::new(store.clone(), adapters);
        let computers = Computers::new(Registry::new(store), computers::default_connector());
        let app = computers::metrics::router(computers.clone()).merge(computers::api::router(computers, sessions));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });

        let c = reqwest::Client::new();
        let r = c
            .post(format!("{base}/api/computers"))
            .json(&json!({ "name": "box", "url": node_url, "token": TOKEN }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 201);
        let cid = r.json::<Value>().await.unwrap()["id"].as_str().unwrap().to_string();

        let r = c.get(format!("{base}/api/computers/{cid}/metrics?top=4")).send().await.unwrap();
        assert_eq!(r.status(), 200);
        let s: MetricsSample = r.json().await.unwrap();
        assert!(!s.cpu.cores.is_empty());
        assert!(s.processes.len() <= 4);
        assert!(s.gpu.devices.is_empty() && s.gpu.reason.is_some());

        let r = c.get(format!("{base}/api/computers/nope/metrics")).send().await.unwrap();
        assert_eq!(r.status(), 404);
    })
    .await;
}
