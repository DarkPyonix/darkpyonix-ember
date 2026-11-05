//! FR-P3 / FR-P6: the node's persistent terminal routes relayed under
//! `/api/computers/{id}/terms`, against a real `ember_node` and a real server listener.
//! A session outlives the server connection and a server restart (a new server instance on the
//! same store); the node, not the server, owns it.

use std::sync::Arc;
use std::time::Duration;

use ember_node::api::{self, Node};
use ember_node::client::NodeClient;
use ember_node::config::NodeConfig;
use ember_node::proto::*;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::AgentAdapter;
use ember_server::computers::{self, Computers, Registry};
use ember_server::session::Sessions;
use ember_server::store::Store;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

const TOKEN: &str = "node-token";

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct NodeFx {
    url: String,
    direct: NodeClient,
    root: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

async fn start_node() -> NodeFx {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let node = Node::new(NodeConfig::new(TOKEN, vec![root.clone()])).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node));
    NodeFx { direct: NodeClient::new(&url, TOKEN).unwrap(), url, root, _dir: dir }
}

/// One server instance over `store`, listening on loopback.
struct Server {
    base: String,
    task: tokio::task::JoinHandle<()>,
}

async fn start_server(store: Arc<Store>) -> Server {
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    let sessions = Sessions::new(store.clone(), adapters);
    let computers = Computers::new(Registry::new(store), computers::default_connector());
    let app = computers::terms::router(computers.clone()).merge(computers::api::router(computers, sessions));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = listener.local_addr().unwrap().to_string();
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Server { base, task }
}

fn http(s: &Server, path: &str) -> String {
    format!("http://{}{path}", s.base)
}

fn create_body(root: &std::path::Path, key: &str) -> Value {
    json!({
        "program": { "argv": ["/bin/sh"] },
        "cwd": root,
        "env": { "PS1": "$ " },
        "size": { "rows": 24, "cols": 80 },
        "origin": "ide-ember",
        "project": root.display().to_string(),
        "key": key,
    })
}

fn hello(device: &str) -> String {
    serde_json::to_string(&TermHello {
        device: device.into(),
        kind: Some("test".into()),
        pid: None,
        size: None,
        active: true,
        read_only: false,
        snapshot: true,
    })
    .unwrap()
}

/// Await `fut` for at most 20 s and panic with `label` otherwise, so a hang names its step.
async fn step<T>(label: &str, fut: impl std::future::Future<Output = T>) -> T {
    match tokio::time::timeout(Duration::from_secs(20), fut).await {
        Ok(v) => v,
        Err(_) => panic!("step timed out after 20 s: {label}"),
    }
}

async fn attach(s: &Server, computer: &str, tid: &str, device: &str) -> Ws {
    let url = format!("ws://{}/api/computers/{computer}/terms/{tid}/attach", s.base);
    let (mut ws, _) = step(&format!("connect attach websocket for {device}"), tokio_tungstenite::connect_async(url)).await.unwrap();
    step(&format!("send hello for {device}"), ws.send(Message::text(hello(device)))).await.unwrap();
    ws
}

async fn event(ws: &mut Ws) -> Option<TermEvent> {
    step("wait for the next terminal event", async {
        loop {
            match ws.next().await? {
                Ok(Message::Text(t)) => return Some(serde_json::from_str(t.as_str()).unwrap()),
                Ok(Message::Close(_)) | Err(_) => return None,
                Ok(_) => continue,
            }
        }
    })
    .await
}

/// Wait until the node reports no attached client for `tid` (a dropped server connection has
/// been noticed), so the next server instance starts from a known state.
async fn wait_no_clients(node: &NodeFx, tid: &str) {
    step(&format!("node reports 0 clients for {tid}"), async {
        loop {
            if node.direct.term(tid).await.unwrap().clients.is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
}

/// Read output and snapshot bytes until `needle` shows up.
async fn read_until(ws: &mut Ws, needle: &str) -> String {
    step(&format!("read output until {needle:?}"), read_until_inner(ws, needle)).await
}

async fn read_until_inner(ws: &mut Ws, needle: &str) -> String {
    let mut out = String::new();
    while !out.contains(needle) {
        match event(ws).await {
            Some(TermEvent::Output { data }) | Some(TermEvent::Snapshot { data, .. }) => {
                out.push_str(&String::from_utf8_lossy(&data))
            }
            Some(TermEvent::Exit { .. }) | Some(TermEvent::Error { .. }) | None => {
                panic!("ended waiting for {needle:?}; output {out:?}")
            }
            Some(_) => {}
        }
    }
    out
}

async fn type_line(ws: &mut Ws, line: &str) {
    let input = TermInput::Input { data: line.as_bytes().to_vec() };
    ws.send(Message::text(serde_json::to_string(&input).unwrap())).await.unwrap();
}

/// Type `line` and read until `needle` shows up. A shell that is still starting can drop what is
/// typed before it reads (a line-editor start flushes pending input), so the line is typed again
/// every 3 s until the output arrives (#345).
async fn type_until(ws: &mut Ws, line: &str, needle: &str) -> String {
    step(&format!("type {line:?} until {needle:?}"), async {
        let mut out = String::new();
        loop {
            type_line(ws, line).await;
            let round = tokio::time::Instant::now() + Duration::from_secs(3);
            while !out.contains(needle) {
                match tokio::time::timeout_at(round, event(ws)).await {
                    Err(_) => break,
                    Ok(Some(TermEvent::Output { data })) | Ok(Some(TermEvent::Snapshot { data, .. })) => {
                        out.push_str(&String::from_utf8_lossy(&data))
                    }
                    Ok(Some(TermEvent::Exit { .. })) | Ok(Some(TermEvent::Error { .. })) | Ok(None) => {
                        panic!("ended waiting for {needle:?}; output {out:?}")
                    }
                    Ok(Some(_)) => {}
                }
            }
            if out.contains(needle) {
                return out;
            }
        }
    })
    .await
}

async fn within<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), f).await.expect("timed out")
}

async fn register(s: &Server, node: &NodeFx) -> String {
    // Through the computers API, as a client would.
    let r = step(
        "register the computer",
        reqwest::Client::new().post(http(s, "/api/computers")).json(&json!({ "name": "box", "url": node.url, "token": TOKEN })).send(),
    )
    .await
    .unwrap();
    assert_eq!(r.status(), 201);
    r.json::<Value>().await.unwrap()["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn fr_p3_create_list_attach_type_and_read_through_the_server() {
    within(async {
        let node = start_node().await;
        let store = Arc::new(Store::open_in_memory().unwrap());
        let s = start_server(store).await;
        let cid = register(&s, &node).await;
        let c = reqwest::Client::new();

        let r = c.post(http(&s, &format!("/api/computers/{cid}/terms"))).json(&create_body(&node.root, "k1")).send().await.unwrap();
        assert_eq!(r.status(), 201);
        let created: TermCreateResponse = r.json().await.unwrap();
        assert!(created.created);
        let tid = created.term.id.clone();
        // The same key answers the running session, 200.
        let r = c.post(http(&s, &format!("/api/computers/{cid}/terms"))).json(&create_body(&node.root, "k1")).send().await.unwrap();
        assert_eq!(r.status(), 200);

        let list: Vec<TermInfo> = c
            .get(http(&s, &format!("/api/computers/{cid}/terms?origin=ide-ember&running=true")))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, tid);

        let mut ws = attach(&s, &cid, &tid, "laptop").await;
        match event(&mut ws).await {
            Some(TermEvent::Attached { term, .. }) => assert_eq!(term.id, tid),
            other => panic!("expected attached, got {other:?}"),
        }
        type_until(&mut ws, "echo relay-$((40+2))\n", "relay-42").await;

        // The plain snapshot route and the single-session route.
        let snap: TermSnapshot =
            c.get(http(&s, &format!("/api/computers/{cid}/terms/{tid}/snapshot"))).send().await.unwrap().json().await.unwrap();
        assert!(snap.text.contains("relay-42"), "{}", snap.text);
        let one: TermInfo = c.get(http(&s, &format!("/api/computers/{cid}/terms/{tid}"))).send().await.unwrap().json().await.unwrap();
        assert_eq!(one.clients.len(), 1);

        // Kill through the server.
        let r = c.post(http(&s, &format!("/api/computers/{cid}/terms/{tid}/kill"))).json(&json!({})).send().await.unwrap();
        assert_eq!(r.status(), 204);
    })
    .await;
}

#[tokio::test]
async fn fr_p6_the_session_keeps_running_when_the_server_connection_drops() {
    within(async {
        let node = start_node().await;
        let s = start_server(Arc::new(Store::open_in_memory().unwrap())).await;
        let cid = register(&s, &node).await;
        let c = reqwest::Client::new();
        let created: TermCreateResponse = c
            .post(http(&s, &format!("/api/computers/{cid}/terms")))
            .json(&create_body(&node.root, "k2"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let tid = created.term.id;

        let mut ws = attach(&s, &cid, &tid, "laptop").await;
        event(&mut ws).await;
        type_until(&mut ws, "echo before-drop\n", "before-drop").await;
        // Abrupt drop, no detach message.
        drop(ws);
        // The node sees the client leave, and the session is still running.
        for _ in 0..100 {
            if node.direct.term(&tid).await.unwrap().clients.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let t = node.direct.term(&tid).await.unwrap();
        assert_eq!(t.state, TermState::Running);
        assert!(t.clients.is_empty(), "the node attachment is released: {:?}", t.clients);

        // A new attach shows what was typed.
        let mut ws = attach(&s, &cid, &tid, "phone").await;
        event(&mut ws).await;
        read_until(&mut ws, "before-drop").await;
    })
    .await;
}

#[tokio::test]
async fn fr_p6_a_restarted_server_lists_and_attaches_again() {
    within(async {
        let node = start_node().await;
        let store = Arc::new(Store::open_in_memory().unwrap());
        let first = start_server(store.clone()).await;
        let cid = register(&first, &node).await;
        let c = reqwest::Client::new();
        let mut ids = Vec::new();
        for key in ["a", "b"] {
            let r = step(
                &format!("create session {key}"),
                c.post(http(&first, &format!("/api/computers/{cid}/terms"))).json(&create_body(&node.root, key)).send(),
            )
            .await
            .unwrap();
            let t: TermCreateResponse = step(&format!("read create response {key}"), r.json()).await.unwrap();
            ids.push(t.term.id);
        }
        // `ids` is sorted below, so keep the id of the session that gets the typed line.
        let typed_id = ids[0].clone();
        let mut ws = attach(&first, &cid, &typed_id, "laptop").await;
        event(&mut ws).await;
        type_until(&mut ws, "echo kept-$((6*7))\n", "kept-42").await;

        // The server goes away: its listener and every connection with it.
        first.task.abort();
        let _ = first.task.await;
        drop(ws);
        // Wait until the node has seen the first attachment leave before the second server starts.
        wait_no_clients(&node, &ids[0]).await;

        // A new instance on the same store (same computers registry).
        let second = start_server(store).await;
        let r = step("list sessions on the second server", c.get(http(&second, &format!("/api/computers/{cid}/terms"))).send())
            .await
            .unwrap();
        let list: Vec<TermInfo> = step("read the session list", r.json()).await.unwrap();
        let mut got: Vec<_> = list.iter().map(|t| t.id.clone()).collect();
        got.sort();
        ids.sort();
        assert_eq!(got, ids, "every session is still listed");
        assert!(list.iter().all(|t| t.state == TermState::Running));

        for id in &ids {
            let mut ws = attach(&second, &cid, id, "after-restart").await;
            assert!(matches!(event(&mut ws).await, Some(TermEvent::Attached { .. })));
        }
        let mut ws = attach(&second, &cid, &typed_id, "after-restart-2").await;
        event(&mut ws).await;
        read_until(&mut ws, "kept-42").await;
        step("type again line", type_line(&mut ws, "echo again-$((1+1))\n")).await;
        read_until(&mut ws, "again-2").await;
    })
    .await;
}

#[tokio::test]
async fn fr_p3_errors_keep_the_nodes_status_and_unreachable_nodes_are_502() {
    within(async {
        let node = start_node().await;
        let s = start_server(Arc::new(Store::open_in_memory().unwrap())).await;
        let cid = register(&s, &node).await;
        let c = reqwest::Client::new();

        // Unknown computer, and the local computer (no node client).
        let r = c.get(http(&s, "/api/computers/nope/terms")).send().await.unwrap();
        assert_eq!(r.status(), 404);
        let r = c.get(http(&s, "/api/computers/local/terms")).send().await.unwrap();
        assert_eq!(r.status(), 400);

        // The node's own 404 and its body.
        let r = c.get(http(&s, &format!("/api/computers/{cid}/terms/missing"))).send().await.unwrap();
        assert_eq!(r.status(), 404);
        assert_eq!(r.json::<Value>().await.unwrap()["code"], "not_found");

        // A cwd outside the node's roots is the node's refusal, not a server error.
        let mut body = create_body(&node.root, "x");
        body["cwd"] = json!("/");
        let r = c.post(http(&s, &format!("/api/computers/{cid}/terms"))).json(&body).send().await.unwrap();
        assert!(r.status().is_client_error(), "{}", r.status());

        // Attach to an unknown session: one error event, then the socket closes.
        let mut ws = attach(&s, &cid, "missing", "laptop").await;
        assert!(matches!(event(&mut ws).await, Some(TermEvent::Error { .. })));
        assert!(event(&mut ws).await.is_none());

        // A registered computer that is down.
        let r = c
            .post(http(&s, "/api/computers"))
            .json(&json!({ "name": "down", "url": "http://127.0.0.1:1", "token": "t" }))
            .send()
            .await
            .unwrap();
        let down = r.json::<Value>().await.unwrap()["id"].as_str().unwrap().to_string();
        let r = c.get(http(&s, &format!("/api/computers/{down}/terms"))).send().await.unwrap();
        assert_eq!(r.status(), 502);
    })
    .await;
}
