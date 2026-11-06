//! FR-W4 / FR-L7 (#456): the node's read-only file operations relayed under
//! `/api/computers/{id}/fs/{op}`, against a real `ember_node` and a real server listener.
//!
//! The node, not the server, enforces the allowed roots. These tests pin what the node does
//! today for a path outside them, `..` traversal and a symlink pointing out, and that the write
//! operations are not reachable through the server.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_node::api::{self, Node};
use ember_node::config::NodeConfig;
use ember_node::proto::MAX_READ;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::AgentAdapter;
use ember_server::auth::{self, ServerAuth};
use ember_server::computers::{self, Computers, Registry};
use ember_server::session::Sessions;
use ember_server::store::Store;
use serde_json::{json, Value};
use tower::ServiceExt;

const TOKEN: &str = "node-token";

async fn within<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), f).await.expect("timed out")
}

struct Fx {
    base: String,
    cid: String,
    root: std::path::PathBuf,
    outside: std::path::PathBuf,
    c: reqwest::Client,
    _dir: tempfile::TempDir,
}

impl Fx {
    async fn post(&self, op: &str, body: Value) -> (StatusCode, Value) {
        let r = self.c.post(format!("{}/api/computers/{}/fs/{op}", self.base, self.cid)).json(&body).send().await.unwrap();
        let st = StatusCode::from_u16(r.status().as_u16()).unwrap();
        (st, r.json::<Value>().await.unwrap_or(Value::Null))
    }
}

/// A node whose only root is `<tmp>/root`, with `<tmp>/outside/secret.txt` beside it, and a
/// server with that node registered.
async fn fx() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().canonicalize().unwrap();
    let root = tmp.join("root");
    let outside = tmp.join("outside");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(root.join("hello.txt"), "hello needle world\n").unwrap();
    std::fs::write(root.join("sub/deep.txt"), "another needle\n").unwrap();
    std::fs::write(outside.join("secret.txt"), "TOP-SECRET needle\n").unwrap();
    std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("link-out.txt")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("dir-out")).unwrap();

    let node = Node::new(NodeConfig::new(TOKEN, vec![root.clone()])).unwrap();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let node_url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(api::serve(l, node));

    let store = Arc::new(Store::open_in_memory().unwrap());
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    let sessions = Sessions::new(store.clone(), adapters);
    let computers = Computers::new(Registry::new(store), computers::default_connector());
    let app = computers::fs::router(computers.clone()).merge(computers::api::router(computers, sessions));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });

    let c = reqwest::Client::new();
    let r = c.post(format!("{base}/api/computers")).json(&json!({ "name": "box", "url": node_url, "token": TOKEN })).send().await.unwrap();
    assert_eq!(r.status(), 201);
    let cid = r.json::<Value>().await.unwrap()["id"].as_str().unwrap().to_string();
    Fx { base, cid, root, outside, c, _dir: dir }
}

#[tokio::test]
async fn fr_w4_fs_relay_reads_inside_the_nodes_root() {
    within(async {
        let f = fx().await;
        let hello = f.root.join("hello.txt");

        let (st, v) = f.post("stat", json!({ "path": hello })).await;
        assert_eq!(st, 200, "{v}");
        assert_eq!(v["kind"], "file");
        assert_eq!(v["size"], 19);

        let (st, v) = f.post("lstat", json!({ "path": hello })).await;
        assert_eq!(st, 200, "{v}");

        let (st, v) = f.post("read", json!({ "path": hello })).await;
        assert_eq!(st, 200, "{v}");
        assert_eq!(v["eof"], true);
        assert!(v["sha256"].is_string());
        assert!(v["data"].is_string(), "data stays the node's base64 string: {v}");

        let (st, v) = f.post("list", json!({ "path": f.root })).await;
        assert_eq!(st, 200, "{v}");
        let names: Vec<&str> = v["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"hello.txt") && names.contains(&"sub"), "{names:?}");

        let (st, v) = f.post("readlink", json!({ "path": f.root.join("link-out.txt") })).await;
        assert_eq!(st, 200, "{v}");

        let (st, v) = f.post("glob", json!({ "root": f.root, "pattern": "**/*.txt" })).await;
        assert_eq!(st, 200, "{v}");
        let paths = v["paths"].as_array().unwrap();
        assert_eq!(paths.len(), 2, "{v}");

        let (st, v) = f.post("grep", json!({ "root": f.root, "pattern": "needle" })).await;
        assert_eq!(st, 200, "{v}");
        assert_eq!(v["matches"].as_array().unwrap().len(), 2, "{v}");
    })
    .await;
}

#[tokio::test]
async fn fr_w4_fs_relay_outside_the_roots_is_refused_by_the_node_and_nothing_is_read() {
    within(async {
        let f = fx().await;
        let secret = f.outside.join("secret.txt");
        for (op, body) in [
            ("stat", json!({ "path": secret })),
            ("lstat", json!({ "path": secret })),
            ("read", json!({ "path": secret })),
            ("list", json!({ "path": f.outside })),
            ("readlink", json!({ "path": secret })),
            ("glob", json!({ "root": f.outside, "pattern": "*" })),
            ("grep", json!({ "root": f.outside, "pattern": "needle" })),
            ("stat", json!({ "path": "/etc/hosts" })),
        ] {
            let (st, v) = f.post(op, body).await;
            assert_eq!(st, 403, "{op}: {v}");
            assert_eq!(v["code"], "forbidden_path", "{op}: the node's own body: {v}");
            assert!(!v.to_string().contains("TOP-SECRET"), "{op}: {v}");
        }
    })
    .await;
}

#[tokio::test]
async fn fr_w4_fs_relay_dotdot_and_symlinks_out_of_the_root_are_refused_by_the_node() {
    within(async {
        let f = fx().await;
        // `..` traversal: the node canonicalises, so the path lands outside and is refused.
        let dotdot = f.root.join("../outside/secret.txt");
        for op in ["stat", "read", "list"] {
            let (st, v) = f.post(op, json!({ "path": dotdot })).await;
            assert_eq!(st, 403, "{op}: {v}");
            assert!(!v.to_string().contains("TOP-SECRET"));
        }
        // A symlink inside the root that points out: stat and read follow it, so the node
        // refuses them (403). This is what the node does today.
        let link = f.root.join("link-out.txt");
        for op in ["stat", "read"] {
            let (st, v) = f.post(op, json!({ "path": link })).await;
            assert_eq!(st, 403, "{op}: {v}");
            assert!(!v.to_string().contains("TOP-SECRET"));
        }
        let (st, v) = f.post("list", json!({ "path": f.root.join("dir-out") })).await;
        assert_eq!(st, 403, "{v}");
        let (st, v) = f.post("read", json!({ "path": f.root.join("dir-out/secret.txt") })).await;
        assert_eq!(st, 403, "{v}");
        // lstat and readlink describe the link itself and do not follow it: the node answers
        // 200, and readlink shows the stored target text (a path string, never file content).
        let (st, v) = f.post("lstat", json!({ "path": link })).await;
        assert_eq!(st, 200, "{v}");
        assert_eq!(v["kind"], "symlink");
        let (st, v) = f.post("readlink", json!({ "path": link })).await;
        assert_eq!(st, 200, "{v}");
        // glob and grep walk without following links, so the file behind a link is not found.
        let (st, v) = f.post("grep", json!({ "root": f.root, "pattern": "TOP-SECRET" })).await;
        assert_eq!(st, 200, "{v}");
        assert!(v["matches"].as_array().unwrap().is_empty(), "{v}");
        let (st, v) = f.post("glob", json!({ "root": f.root, "pattern": "**/secret.txt" })).await;
        assert_eq!(st, 200, "{v}");
        assert!(v["paths"].as_array().unwrap().is_empty(), "{v}");
    })
    .await;
}

#[tokio::test]
async fn fr_w4_fs_relay_write_operations_are_not_routed() {
    within(async {
        let f = fx().await;
        let target = f.root.join("written.txt");
        let victim = f.root.join("hello.txt");
        for (op, body) in [
            ("write", json!({ "path": target, "data": "" })),
            ("remove", json!({ "path": victim, "recursive": false })),
            ("rename", json!({ "from": victim, "to": target })),
            ("mkdir", json!({ "path": f.root.join("newdir") })),
            ("symlink", json!({ "path": target, "target": "/etc/hosts" })),
            ("setattr", json!({ "path": victim, "mode": 0 })),
            ("pwrite", json!({ "path": victim, "offset": 0, "data": "" })),
            ("nonsense", json!({})),
        ] {
            let (st, _) = f.post(op, body).await;
            assert_eq!(st, 404, "{op} must not reach the node");
        }
        assert!(!target.exists());
        assert!(!f.root.join("newdir").exists());
        assert!(victim.exists());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "hello needle world\n");
        // Only POST is routed.
        let r = f.c.get(format!("{}/api/computers/{}/fs/read", f.base, f.cid)).send().await.unwrap();
        assert_eq!(r.status(), 405);
    })
    .await;
}

#[tokio::test]
async fn fr_w4_fs_relay_errors_map_unknown_404_local_400_unreachable_502() {
    within(async {
        let f = fx().await;
        let body = json!({ "path": f.root.join("hello.txt") });
        let r = f.c.post(format!("{}/api/computers/nope/fs/stat", f.base)).json(&body).send().await.unwrap();
        assert_eq!(r.status(), 404);
        let r = f.c.post(format!("{}/api/computers/local/fs/stat", f.base)).json(&body).send().await.unwrap();
        assert_eq!(r.status(), 400);
        // The node's own errors keep status and body.
        let (st, v) = f.post("stat", json!({ "path": f.root.join("missing.txt") })).await;
        assert_eq!(st, 404, "{v}");
        assert_eq!(v["code"], "not_found", "{v}");
        let (st, v) = f.post("read", json!({ "path": f.root })).await;
        assert_eq!(st, 400, "reading a directory: {v}");
        // A body of the wrong shape is a 400 from the server, before the node.
        let (st, _) = f.post("grep", json!({ "root": f.root })).await;
        assert!(st == 400 || st == 422, "{st}");
        // A registered computer that is down.
        let r = f
            .c
            .post(format!("{}/api/computers", f.base))
            .json(&json!({ "name": "down", "url": "http://127.0.0.1:1", "token": "t" }))
            .send()
            .await
            .unwrap();
        let down = r.json::<Value>().await.unwrap()["id"].as_str().unwrap().to_string();
        let r = f.c.post(format!("{}/api/computers/{down}/fs/stat", f.base)).json(&body).send().await.unwrap();
        assert_eq!(r.status(), 502);
    })
    .await;
}

#[tokio::test]
async fn fr_w4_fs_relay_request_body_is_capped_and_read_is_bounded_by_the_node() {
    within(async {
        let f = fx().await;
        // A request body over 1 MiB is refused by the server (413) and never reaches the node.
        let big = "a".repeat(2 * 1024 * 1024);
        let (st, _) = f.post("grep", json!({ "root": f.root, "pattern": big })).await;
        assert_eq!(st, 413);
        // A body just under the cap still passes to the node (here it is a bad regex size or a
        // plain no-match; either way it is not 413).
        let ok = "a".repeat(1000);
        let (st, _) = f.post("grep", json!({ "root": f.root, "pattern": ok })).await;
        assert_ne!(st, 413);

        // The server adds no response cap: a read of a file larger than the node's MAX_READ
        // returns the node's bound (16 MiB) with eof false, proxied whole.
        let huge = f.root.join("huge.bin");
        std::fs::File::create(&huge).unwrap().set_len(MAX_READ + 1024 * 1024).unwrap();
        let (st, v) = f.post("read", json!({ "path": huge, "hash": false })).await;
        assert_eq!(st, 200);
        assert_eq!(v["eof"], false);
        assert_eq!(v["size"], MAX_READ + 1024 * 1024);
        let b64 = v["data"].as_str().unwrap().len() as u64;
        assert!(b64 >= MAX_READ * 4 / 3 - 4 && b64 <= MAX_READ * 4 / 3 + 4, "{b64}");
    })
    .await;
}

// ---- auth: the main router is wrapped by auth::protect and nothing here is exempt ----

const LISTEN: &str = "127.0.0.1:8740";

fn protected_app(dir: &std::path::Path) -> (Router, String) {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let computers = Computers::new(Registry::new(store), computers::default_connector());
    let token = auth::load_or_create_token(dir).unwrap();
    let a = ServerAuth::new(token.clone(), LISTEN.parse().unwrap(), []);
    (auth::protect(computers::fs::router(computers), a), token)
}

async fn status(app: &Router, method: &str, path: &str, authz: Option<&str>) -> StatusCode {
    let mut b = Request::builder().method(method).uri(path).header("host", LISTEN).header("content-type", "application/json");
    if let Some(a) = authz {
        b = b.header("authorization", a);
    }
    app.clone().oneshot(b.body(Body::from(r#"{"path":"/x"}"#)).unwrap()).await.unwrap().status()
}

#[tokio::test]
async fn fr_w4_fs_relay_needs_the_server_token() {
    let dir = tempfile::tempdir().unwrap();
    let (app, token) = protected_app(dir.path());
    for op in ["stat", "lstat", "read", "list", "readlink", "glob", "grep"] {
        let path = format!("/api/computers/local/fs/{op}");
        assert_eq!(status(&app, "POST", &path, None).await, 401, "{op}: no token");
        assert_eq!(status(&app, "POST", &path, Some("Bearer dpg_guestcredential")).await, 401, "{op}: guest credential");
        assert_eq!(status(&app, "POST", &path, Some("Bearer wrong")).await, 401, "{op}: wrong token");
        // The right token reaches the handler: the local computer has no node, so it answers
        // 400 or 422 from the relay itself, not 401.
        let st = status(&app, "POST", &path, Some(&format!("Bearer {token}"))).await;
        assert!(st == 400 || st == 422, "{op}: {st}");
    }
    // The write operations stay unrouted behind the token too.
    let st = status(&app, "POST", "/api/computers/local/fs/write", Some(&format!("Bearer {token}"))).await;
    assert_eq!(st, 404);
}
