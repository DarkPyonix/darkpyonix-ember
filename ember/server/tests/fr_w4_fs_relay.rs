//! FR-W4 / FR-L7 (#456): the node's read-only file operations relayed under
//! `/api/computers/{id}/fs/{op}`, against a real `ember_node` and a real server listener.
//!
//! The node, not the server, enforces the allowed roots. These tests pin what the node does
//! today for a path outside them, `..` traversal and a symlink pointing out. The write relay
//! carries `write`, `pwrite` and `setattr` and, since the node checks their parent directory by
//! handle (#456), the namespace operations `mkdir`, `remove`, `rename` and `symlink` (INTENT
//! D48).

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
    node_url: String,
    c: reqwest::Client,
    _dir: tempfile::TempDir,
}

impl Fx {
    async fn post(&self, op: &str, body: Value) -> (StatusCode, Value) {
        let r = self.c.post(format!("{}/api/computers/{}/fs/{op}", self.base, self.cid)).json(&body).send().await.unwrap();
        let st = StatusCode::from_u16(r.status().as_u16()).unwrap();
        (st, r.json::<Value>().await.unwrap_or(Value::Null))
    }

    /// The same operation sent straight to the node, bypassing the server.
    async fn direct(&self, op: &str, body: Value) -> (StatusCode, Value) {
        let r = self.c.post(format!("{}/fs/{op}", self.node_url)).bearer_auth(TOKEN).json(&body).send().await.unwrap();
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

    // `<root>/.ember` is denied, like the local node's data dir (#521). It is not created, so
    // the read tests see the same tree as before.
    let node = Node::new(NodeConfig::new(TOKEN, vec![root.clone()]).with_denied(vec![root.join(".ember")])).unwrap();
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
    Fx { base, cid, root, outside, node_url, c, _dir: dir }
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
async fn fr_w4_fs_relay_namespace_operations_reach_the_node() {
    within(async {
        let f = fx().await;
        let hello = f.root.join("hello.txt");

        let (st, v) = f.post("mkdir", json!({ "path": f.root.join("newdir/a/b"), "parents": true })).await;
        assert_eq!(st, 200, "{v}");
        assert_eq!(v["kind"], "dir");
        assert!(f.root.join("newdir/a/b").is_dir());
        let (st, v) = f.post("mkdir", json!({ "path": f.root.join("newdir") })).await;
        assert_eq!(st, 409, "an existing directory is the node's EEXIST: {v}");
        assert_eq!(v["errno"], "EEXIST", "{v}");

        let (st, v) = f.post("symlink", json!({ "path": f.root.join("newdir/l"), "target": "../hello.txt" })).await;
        assert_eq!(st, 200, "{v}");
        assert_eq!(v["kind"], "symlink");
        assert_eq!(std::fs::read_link(f.root.join("newdir/l")).unwrap(), std::path::PathBuf::from("../hello.txt"));

        let moved = f.root.join("newdir/moved.txt");
        let (st, v) = f.post("rename", json!({ "from": hello, "to": moved })).await;
        assert_eq!(st, 204, "rename answers the node's 204: {v}");
        assert_eq!(v, Value::Null, "no body");
        assert!(!hello.exists());
        assert_eq!(std::fs::read_to_string(&moved).unwrap(), "hello needle world\n");
        // overwrite: false keeps an existing target (the node's EEXIST).
        std::fs::write(f.root.join("other.txt"), "other").unwrap();
        let (st, v) = f.post("rename", json!({ "from": moved, "to": f.root.join("other.txt"), "overwrite": false })).await;
        assert_eq!(st, 409, "{v}");
        assert_eq!(std::fs::read_to_string(f.root.join("other.txt")).unwrap(), "other");

        let (st, v) = f.post("remove", json!({ "path": f.root.join("newdir"), "recursive": false })).await;
        assert_eq!(st, 409, "a non-empty directory without recursive: {v}");
        assert_eq!(v["errno"], "ENOTEMPTY", "{v}");
        let (st, v) = f.post("remove", json!({ "path": f.root.join("newdir"), "recursive": true })).await;
        assert_eq!(st, 204, "{v}");
        assert!(!f.root.join("newdir").exists());

        // The node's answer is relayed unchanged: status and body equal a direct call.
        for (op, body) in [
            ("remove", json!({ "path": f.root.join("missing"), "recursive": false })),
            ("remove", json!({ "path": f.root, "recursive": true })),
            ("mkdir", json!({ "path": f.root.join("no/parent") })),
            ("rename", json!({ "from": f.root.join("missing"), "to": f.root.join("x") })),
        ] {
            let relayed = f.post(op, body.clone()).await;
            let direct = f.direct(op, body).await;
            assert_eq!(relayed, direct, "{op}");
        }
        assert!(f.root.exists(), "a root is never removed");

        // Anything else is not routed; only POST is.
        let (st, _) = f.post("nonsense", json!({})).await;
        assert_eq!(st, 404);
        for op in ["read", "write", "mkdir", "remove", "rename", "symlink"] {
            let r = f.c.get(format!("{}/api/computers/{}/fs/{op}", f.base, f.cid)).send().await.unwrap();
            assert_eq!(r.status(), 405, "{op}");
        }
    })
    .await;
}

#[tokio::test]
async fn fr_w4_fs_relay_namespace_operations_outside_the_roots_or_denied_are_refused_by_the_node() {
    within(async {
        let f = fx().await;
        let secret = f.outside.join("secret.txt");
        let hello = f.root.join("hello.txt");
        for (op, body) in [
            ("mkdir", json!({ "path": f.outside.join("new") })),
            ("mkdir", json!({ "path": f.root.join("dir-out/new") })),
            ("mkdir", json!({ "path": f.root.join("dir-out/new/deep"), "parents": true })),
            ("mkdir", json!({ "path": f.root.join(".ember") })),
            ("mkdir", json!({ "path": f.root.join("../outside/new") })),
            ("remove", json!({ "path": secret, "recursive": false })),
            ("remove", json!({ "path": f.root.join("dir-out/secret.txt"), "recursive": false })),
            ("remove", json!({ "path": f.outside, "recursive": true })),
            ("rename", json!({ "from": hello, "to": f.outside.join("stolen.txt") })),
            ("rename", json!({ "from": secret, "to": f.root.join("got.txt") })),
            ("rename", json!({ "from": hello, "to": f.root.join("dir-out/stolen.txt") })),
            ("rename", json!({ "from": hello, "to": f.root.join(".ember") })),
            ("symlink", json!({ "path": f.outside.join("planted"), "target": "secret.txt" })),
            ("symlink", json!({ "path": f.root.join("l1"), "target": "/etc/hosts" })),
            ("symlink", json!({ "path": f.root.join("l2"), "target": secret })),
            ("symlink", json!({ "path": f.root.join("l3"), "target": "../outside/secret.txt" })),
            ("symlink", json!({ "path": f.root.join("l4"), "target": "dir-out/secret.txt" })),
            ("symlink", json!({ "path": f.root.join("l5"), "target": ".ember/server-token" })),
        ] {
            let (st, v) = f.post(op, body.clone()).await;
            assert_eq!(st, 403, "{op} {body}: {v}");
            assert_eq!(v["code"], "forbidden_path", "{op}: the node's own body: {v}");
            assert_eq!(v["errno"], "EACCES", "{op}: {v}");
        }
        // Nothing changed on either side.
        assert_eq!(std::fs::read_to_string(&secret).unwrap(), "TOP-SECRET needle\n");
        assert_eq!(std::fs::read_to_string(&hello).unwrap(), "hello needle world\n");
        let mut outside: Vec<String> = std::fs::read_dir(&f.outside)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        outside.sort();
        assert_eq!(outside, vec!["secret.txt".to_string()]);
        for name in ["got.txt", ".ember", "l1", "l2", "l3", "l4", "l5"] {
            assert!(std::fs::symlink_metadata(f.root.join(name)).is_err(), "{name} was created");
        }
        // A link that points out but already exists is removed as a link, never its target.
        let (st, v) = f.post("remove", json!({ "path": f.root.join("link-out.txt"), "recursive": false })).await;
        assert_eq!(st, 204, "{v}");
        assert!(secret.exists());
    })
    .await;
}

#[tokio::test]
async fn fr_w4_fs_relay_namespace_body_is_capped_at_1_mib() {
    within(async {
        let f = fx().await;
        let big = "a".repeat(2 * 1024 * 1024);
        for op in ["mkdir", "remove", "rename", "symlink"] {
            let (st, _) = f.post(op, json!({ "path": big, "from": "/x", "to": "/y", "target": "t" })).await;
            assert_eq!(st, 413, "{op}");
        }
    })
    .await;
}

fn b64(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

#[tokio::test]
async fn fr_w4_fs_relay_write_pwrite_setattr_reach_the_node() {
    within(async {
        let f = fx().await;
        let new = f.root.join("new/written.txt");
        let (st, v) = f.post("write", json!({ "path": new, "data": b64(b"first"), "create_parents": true })).await;
        assert_eq!(st, 200, "{v}");
        assert_eq!(v["size"], 5);
        assert_eq!(v["sha256"], sha256_hex(b"first"));
        assert_eq!(std::fs::read(&new).unwrap(), b"first");

        let (st, v) = f.post("pwrite", json!({ "path": new, "offset": 1, "data": b64(b"IR") })).await;
        assert_eq!(st, 200, "{v}");
        assert_eq!(std::fs::read(&new).unwrap(), b"fIRst");

        let (st, v) = f.post("setattr", json!({ "path": new, "size": 2, "mode": 0o600 })).await;
        assert_eq!(st, 200, "{v}");
        assert_eq!(std::fs::read(&new).unwrap(), b"fI");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&new).unwrap().permissions().mode() & 0o777, 0o600);

        // The answer is the node's, unchanged.
        // The reads above bump atime, so the comparison leaves atime out.
        let (st, mut d) = f.direct("stat", json!({ "path": new })).await;
        assert_eq!(st, 200);
        let mut v = v;
        v.as_object_mut().unwrap().remove("atime_ms");
        d.as_object_mut().unwrap().remove("atime_ms");
        assert_eq!(v, d, "setattr answers the node's Stat");
    })
    .await;
}

#[tokio::test]
async fn fr_w4_fs_relay_writes_outside_the_roots_or_denied_are_refused_by_the_node() {
    within(async {
        let f = fx().await;
        let secret = f.outside.join("secret.txt");
        let denied = f.root.join(".ember/server-token");
        for (op, body) in [
            ("write", json!({ "path": f.outside.join("new.txt"), "data": b64(b"x") })),
            ("write", json!({ "path": secret, "data": b64(b"x") })),
            ("write", json!({ "path": f.root.join("link-out.txt"), "data": b64(b"x") })),
            ("write", json!({ "path": f.root.join("../outside/secret.txt"), "data": b64(b"x") })),
            ("write", json!({ "path": denied, "data": b64(b"x"), "create_parents": true })),
            ("pwrite", json!({ "path": secret, "offset": 0, "data": b64(b"x") })),
            ("pwrite", json!({ "path": f.root.join("link-out.txt"), "offset": 0, "data": b64(b"x") })),
            ("setattr", json!({ "path": secret, "size": 0 })),
            ("setattr", json!({ "path": f.root.join("link-out.txt"), "mode": 0o777 })),
        ] {
            let (st, v) = f.post(op, body.clone()).await;
            let (dst, dv) = f.direct(op, body).await;
            assert!(st.is_client_error(), "{op}: {st} {v}");
            assert_eq!((st, &v), (dst, &dv), "{op}: the server passes the node's answer unchanged");
        }
        assert_eq!(std::fs::read_to_string(&secret).unwrap(), "TOP-SECRET needle\n");
        assert!(!f.outside.join("new.txt").exists());
        assert!(!f.root.join(".ember").exists());
    })
    .await;
}

#[tokio::test]
async fn fr_w4_fs_relay_write_precondition_conflict_is_the_nodes_412() {
    within(async {
        let f = fx().await;
        let hello = f.root.join("hello.txt");
        let current = sha256_hex(b"hello needle world\n");

        let (st, v) = f
            .post("write", json!({ "path": hello, "data": b64(b"x"), "expect": { "kind": "sha256", "sha256": "00".repeat(32) } }))
            .await;
        assert_eq!(st, 412, "{v}");
        assert_eq!(v["code"], "precondition_failed", "{v}");
        assert_eq!(v["actual_sha256"], current, "{v}");

        let (st, v) = f.post("write", json!({ "path": hello, "data": b64(b"x"), "expect": { "kind": "absent" } })).await;
        assert_eq!(st, 412, "{v}");
        assert_eq!(v["actual_sha256"], current, "{v}");
        assert_eq!(std::fs::read_to_string(&hello).unwrap(), "hello needle world\n");

        // The matching hash writes.
        let (st, v) = f.post("write", json!({ "path": hello, "data": b64(b"x"), "expect": { "kind": "sha256", "sha256": current } })).await;
        assert_eq!(st, 200, "{v}");
        assert_eq!(std::fs::read(&hello).unwrap(), b"x");
    })
    .await;
}

#[tokio::test]
async fn fr_w4_fs_relay_write_body_is_capped_at_24_mib() {
    within(async {
        let f = fx().await;
        assert_eq!(computers::fs::MAX_WRITE_BODY, 24 * 1024 * 1024);
        let target = f.root.join("big.bin");
        // 2 MiB of data passes (over the read cap of 1 MiB, under the write cap).
        let (st, v) = f.post("write", json!({ "path": target, "data": b64(&vec![7u8; 2 * 1024 * 1024]) })).await;
        assert_eq!(st, 200, "{v}");
        // 20 MiB of data is about 26.7 MiB of base64: 413, the node never sees it.
        let (st, _) = f.post("write", json!({ "path": f.root.join("huge.bin"), "data": b64(&vec![7u8; 20 * 1024 * 1024]) })).await;
        assert_eq!(st, 413);
        assert!(!f.root.join("huge.bin").exists());
        let (st, _) = f.post("pwrite", json!({ "path": target, "offset": 0, "data": b64(&vec![1u8; 20 * 1024 * 1024]) })).await;
        assert_eq!(st, 413);
        assert_eq!(std::fs::read(&target).unwrap()[0], 7);
        // The read routes keep their 1 MiB cap.
        let (st, _) = f.post("stat", json!({ "path": "a".repeat(2 * 1024 * 1024) })).await;
        assert_eq!(st, 413);
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
    for op in [
        "stat", "lstat", "read", "list", "readlink", "glob", "grep", "write", "pwrite", "setattr", "mkdir", "remove",
        "rename", "symlink",
    ] {
        let path = format!("/api/computers/local/fs/{op}");
        assert_eq!(status(&app, "POST", &path, None).await, 401, "{op}: no token");
        assert_eq!(status(&app, "POST", &path, Some("Bearer dpg_guestcredential")).await, 401, "{op}: guest credential");
        assert_eq!(status(&app, "POST", &path, Some("Bearer wrong")).await, 401, "{op}: wrong token");
        // The right token reaches the handler: the local computer has no node, so it answers
        // 400 or 422 from the relay itself, not 401.
        let st = status(&app, "POST", &path, Some(&format!("Bearer {token}"))).await;
        assert!(st == 400 || st == 422, "{op}: {st}");
    }
}

// ---- #467: limits, deadline, path edge cases ----

#[tokio::test]
async fn fr_w4_fs_relay_glob_and_grep_limit_is_clamped_by_the_server() {
    within(async {
        let f = fx().await;
        let many = f.root.join("many");
        std::fs::create_dir_all(&many).unwrap();
        let n = computers::fs::MAX_SEARCH_LIMIT + 50;
        for i in 0..n {
            std::fs::write(many.join(format!("f{i}.txt")), "needle\n").unwrap();
        }
        let (st, v) = f.post("glob", json!({ "root": many, "pattern": "*.txt", "limit": u64::MAX })).await;
        assert_eq!(st, 200, "{v}");
        assert_eq!(v["paths"].as_array().unwrap().len(), computers::fs::MAX_SEARCH_LIMIT);
        assert_eq!(v["truncated"], true);
        let (st, v) = f.post("grep", json!({ "root": many, "pattern": "needle", "limit": 1_000_000_000u64 })).await;
        assert_eq!(st, 200, "{v}");
        assert_eq!(v["matches"].as_array().unwrap().len(), computers::fs::MAX_SEARCH_LIMIT);
        // A smaller limit passes unchanged.
        let (st, v) = f.post("glob", json!({ "root": many, "pattern": "*.txt", "limit": 3 })).await;
        assert_eq!(st, 200, "{v}");
        assert_eq!(v["paths"].as_array().unwrap().len(), 3);
    })
    .await;
}

/// The node's answer to an odd path is relayed unchanged: same status, same body.
#[tokio::test]
async fn fr_w4_fs_relay_path_edge_cases_pass_the_nodes_answer_unchanged() {
    within(async {
        let f = fx().await;
        // A backslash is an ordinary file name byte on Unix.
        std::fs::write(f.root.join("back\\slash.txt"), "x").unwrap();
        // NFC name on disk; the NFD spelling may or may not resolve (APFS yes, ext4 no).
        std::fs::write(f.root.join("caf\u{e9}.txt"), "x").unwrap();
        let root = f.root.to_string_lossy().to_string();
        let long = format!("{root}/{}", "a".repeat(5000));
        let cases: Vec<(&str, Value)> = vec![
            ("stat", json!({ "path": format!("{root}/hello\u{0}.txt") })),
            ("read", json!({ "path": format!("{root}/hello.txt\u{0}/../../outside/secret.txt") })),
            ("stat", json!({ "path": format!("{root}/back\\slash.txt") })),
            ("stat", json!({ "path": format!("{root}\\..\\outside\\secret.txt") })),
            ("stat", json!({ "path": format!("{root}/caf\u{e9}.txt") })),
            ("stat", json!({ "path": format!("{root}/cafe\u{301}.txt") })),
            ("stat", json!({ "path": long })),
            ("list", json!({ "path": format!("{root}/sub/") })),
            ("stat", json!({ "path": format!("{root}/hello.txt/") })),
            ("glob", json!({ "root": format!("{root}/"), "pattern": "*.txt" })),
        ];
        for (op, body) in cases {
            let relayed = f.post(op, body.clone()).await;
            let direct = f.direct(op, body.clone()).await;
            assert_eq!(relayed, direct, "{op} {body}");
            assert!(!relayed.1.to_string().contains("TOP-SECRET"), "{op} {body}");
        }
        // The node refuses a NUL byte and a file with a trailing slash; nothing is read.
        let (st, _) = f.post("stat", json!({ "path": format!("{root}/hello\u{0}.txt") })).await;
        assert!(st.is_client_error(), "NUL: {st}");
        let (st, _) = f.post("read", json!({ "path": format!("{root}/hello.txt/") })).await;
        assert!(st.is_client_error(), "trailing slash on a file: {st}");
        let (st, _) = f.post("stat", json!({ "path": format!("{root}/back\\slash.txt") })).await;
        assert_eq!(st, 200);
    })
    .await;
}

/// A fake node that answers `/fs/stat` only after `delay`.
async fn slow_node(delay: Duration, started: Arc<std::sync::atomic::AtomicUsize>) -> String {
    use axum::routing::post;
    let app = Router::new().route(
        "/fs/stat",
        post(move || {
            let started = started.clone();
            async move {
                started.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::time::sleep(delay).await;
                axum::Json(json!({ "kind": "file", "size": 0 }))
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    url
}

async fn limited_server(limits: computers::fs::FsLimits, node_url: &str) -> (String, String) {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    let sessions = Sessions::new(store.clone(), adapters);
    let computers = Computers::new(Registry::new(store), computers::default_connector());
    let app = computers::fs::router_with(computers.clone(), limits).merge(computers::api::router(computers, sessions));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let r = reqwest::Client::new()
        .post(format!("{base}/api/computers"))
        .json(&json!({ "name": "slow", "url": node_url, "token": TOKEN }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    let cid = r.json::<Value>().await.unwrap()["id"].as_str().unwrap().to_string();
    (base, cid)
}

#[tokio::test]
async fn fr_w4_fs_relay_times_out_with_504() {
    within(async {
        let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let node = slow_node(Duration::from_secs(30), started).await;
        let limits = computers::fs::FsLimits { concurrent: 4, timeout: Duration::from_millis(300) };
        let (base, cid) = limited_server(limits, &node).await;
        let t = std::time::Instant::now();
        let r = reqwest::Client::new().post(format!("{base}/api/computers/{cid}/fs/stat")).json(&json!({ "path": "/x" })).send().await.unwrap();
        assert_eq!(r.status(), 504);
        assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
    })
    .await;
}

#[tokio::test]
async fn fr_w4_fs_relay_concurrency_is_bounded_by_the_semaphore() {
    within(async {
        let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let node = slow_node(Duration::from_millis(1500), started.clone()).await;
        let limits = computers::fs::FsLimits { concurrent: 2, timeout: Duration::from_millis(800) };
        let (base, cid) = limited_server(limits, &node).await;
        let c = reqwest::Client::new();
        let mut tasks = Vec::new();
        for _ in 0..5 {
            let (c, url) = (c.clone(), format!("{base}/api/computers/{cid}/fs/stat"));
            tasks.push(tokio::spawn(async move { c.post(url).json(&json!({ "path": "/x" })).send().await.unwrap().status().as_u16() }));
        }
        let mut codes = Vec::new();
        for t in tasks {
            codes.push(t.await.unwrap());
        }
        codes.sort();
        // Two hold the permits and hit the deadline (504); three never get a permit (503).
        assert_eq!(codes, vec![503, 503, 503, 504, 504], "{codes:?}");
        assert_eq!(started.load(std::sync::atomic::Ordering::SeqCst), 2, "only two reached the node");
    })
    .await;
}

#[tokio::test]
async fn fr_w4_fs_relay_client_drop_releases_the_permit() {
    within(async {
        let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let node = slow_node(Duration::from_secs(20), started.clone()).await;
        let limits = computers::fs::FsLimits { concurrent: 1, timeout: Duration::from_secs(30) };
        let (base, cid) = limited_server(limits, &node).await;
        let url = format!("{base}/api/computers/{cid}/fs/stat");
        // The first client gives up after 300 ms and closes its connection.
        let c1 = reqwest::Client::builder().timeout(Duration::from_millis(300)).build().unwrap();
        assert!(c1.post(&url).json(&json!({ "path": "/x" })).send().await.is_err());
        drop(c1);
        // The dropped request released its permit: a second one reaches the node.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while started.load(std::sync::atomic::Ordering::SeqCst) < 2 && std::time::Instant::now() < deadline {
            let c2 = reqwest::Client::builder().timeout(Duration::from_millis(300)).build().unwrap();
            let _ = c2.post(&url).json(&json!({ "path": "/x" })).send().await;
        }
        assert!(started.load(std::sync::atomic::Ordering::SeqCst) >= 2, "the permit was not released on client drop");
    })
    .await;
}
