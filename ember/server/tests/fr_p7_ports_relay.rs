//! FR-P7: the node's listening ports relayed under `/api/computers/{id}/ports`, against a real
//! `ember_node` (with a job that listens) and a real server listener.

use std::sync::Arc;
use std::time::Duration;

use ember_node::api::{self, Node};
use ember_node::client::{shell, NodeClient};
use ember_node::config::NodeConfig;
use ember_node::proto::{JobRequest, PortOwnerKind, PortScope, PortsResponse};
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
async fn fr_p7_ports_relay_returns_the_node_list_and_errors_map() {
    within(async {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let node = Node::new(NodeConfig::new(TOKEN, vec![root.clone()])).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let node_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(api::serve(listener, node));

        let store = Arc::new(Store::open_in_memory().unwrap());
        let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
        let sessions = Sessions::new(store.clone(), adapters);
        let computers = Computers::new(Registry::new(store), computers::default_connector());
        let app = computers::ports::router(computers.clone()).merge(computers::api::router(computers, sessions));
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

        // Nothing is running: an empty list, not an error.
        let r = c.get(format!("{base}/api/computers/{cid}/ports")).send().await.unwrap();
        assert_eq!(r.status(), 200);
        let empty: PortsResponse = r.json().await.unwrap();
        assert!(empty.ports.is_empty());
        assert_eq!(empty.supported, cfg!(any(target_os = "linux", target_os = "macos")));

        // A node job that listens shows up through the relay (Linux and macOS, with python3).
        let have_python = std::process::Command::new("python3").arg("--version").output().is_ok();
        if have_python && cfg!(any(target_os = "linux", target_os = "macos")) {
            let nc = NodeClient::new(&node_url, TOKEN).unwrap();
            let script = "python3 -c 'import socket,time;s=socket.socket();s.bind((\"127.0.0.1\",0));s.listen();print(s.getsockname()[1],flush=True);time.sleep(60)'";
            let job = nc.start_job(&JobRequest { command: shell(script, &root), label: None }).await.unwrap();
            let port: u16 = loop {
                let d = nc.job(&job.id, Some(4096)).await.unwrap();
                if let Some(p) = d.tail.lines().next().and_then(|l| l.trim().parse().ok()) {
                    break p;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            };
            let got: PortsResponse =
                c.get(format!("{base}/api/computers/{cid}/ports")).send().await.unwrap().json().await.unwrap();
            let p = got.ports.iter().find(|p| p.port == port).expect("job port relayed");
            assert_eq!(p.scope, PortScope::Loopback);
            assert_eq!(p.owner.kind, PortOwnerKind::Job);
            assert_eq!(p.owner.id, job.id);
            assert!(got.supported);
            nc.kill_job(&job.id, None).await.unwrap();
        }

        let r = c.get(format!("{base}/api/computers/nope/ports")).send().await.unwrap();
        assert_eq!(r.status(), 404);
    })
    .await;
}
