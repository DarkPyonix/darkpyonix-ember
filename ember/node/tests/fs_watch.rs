//! FR-W4d (#460): `GET /fs/watch` on a real node over TCP.
//!
//! File system event timing varies by platform (FSEvents batches, inotify does not), so every
//! assertion waits for a condition with a generous bound and never sleeps and then asserts.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ember_node::api::{self, Node};
use ember_node::client::{FsWatchSocket, NodeClient};
use ember_node::config::NodeConfig;
use ember_node::fs_watch::WatchLimits;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

const TOKEN: &str = "node-token";
/// How long a condition may take to show up.
const BOUND: Duration = Duration::from_secs(20);

struct Fx {
    root: PathBuf,
    outside: PathBuf,
    denied: PathBuf,
    client: NodeClient,
    url: String,
    _dir: tempfile::TempDir,
}

async fn fx_with(limits: WatchLimits) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().canonicalize().unwrap();
    let root = tmp.join("root");
    let outside = tmp.join("outside");
    let denied = root.join(".ember");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir_all(&denied).unwrap();
    let node = Node::new(NodeConfig::new(TOKEN, vec![root.clone()]).with_denied(vec![denied.clone()]))
        .unwrap()
        .with_watch_limits(limits);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(api::serve(l, node));
    Fx { root, outside, denied, client: NodeClient::new(&url, TOKEN).unwrap(), url, _dir: dir }
}

async fn fx() -> Fx {
    fx_with(WatchLimits::default()).await
}

async fn send(ws: &mut FsWatchSocket, v: Value) {
    ws.send(Message::Text(v.to_string().into())).await.unwrap();
}

/// The next JSON frame, or `None` when the socket ended or nothing came within `within`.
async fn next(ws: &mut FsWatchSocket, within: Duration) -> Option<Value> {
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

/// Read frames until `want` holds for one of them (and return it), or panic after [`BOUND`].
async fn until(ws: &mut FsWatchSocket, what: &str, want: impl Fn(&Value) -> bool) -> Value {
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

async fn watch(ws: &mut FsWatchSocket, id: u64, path: &Path, recursive: bool) -> Value {
    send(ws, json!({ "op": "watch", "id": id, "path": path, "recursive": recursive })).await;
    until(ws, "the watch answer", |v| v["id"] == id && (v["op"] == "watching" || v["op"] == "error")).await
}

/// Whether a `change` frame of watch `id` has an event of `kind` for `path`.
fn has(v: &Value, id: u64, kind: &str, path: &Path) -> bool {
    v["op"] == "change"
        && v["id"] == id
        && v["events"].as_array().unwrap().iter().any(|e| e["kind"] == kind && e["path"] == json!(path))
}

/// Every event path of every `change` frame read within `within`.
async fn paths_seen(ws: &mut FsWatchSocket, within: Duration) -> Vec<String> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return out;
        }
        match next(ws, left).await {
            Some(v) if v["op"] == "change" => {
                for e in v["events"].as_array().unwrap() {
                    out.push(e["path"].as_str().unwrap_or_default().to_string());
                    if let Some(to) = e["to"].as_str() {
                        out.push(to.to_string());
                    }
                }
            }
            Some(_) => continue,
            None => return out,
        }
    }
}

#[tokio::test]
async fn fr_w4d_watch_reports_create_modify_rename_and_remove() {
    let f = fx().await;
    let mut ws = f.client.fs_watch_socket().await.unwrap();
    let ok = watch(&mut ws, 1, &f.root, true).await;
    assert_eq!(ok["op"], "watching", "{ok}");
    assert_eq!(ok["path"], json!(f.root));

    let a = f.root.join("a.txt");
    std::fs::write(&a, "one").unwrap();
    until(&mut ws, "created a.txt", |v| has(v, 1, "created", &a) || has(v, 1, "changed", &a)).await;

    std::fs::write(&a, "two, longer").unwrap();
    until(&mut ws, "changed a.txt", |v| has(v, 1, "changed", &a) || has(v, 1, "created", &a)).await;

    let b = f.root.join("b.txt");
    std::fs::rename(&a, &b).unwrap();
    // A paired rename is one `renamed`; FSEvents reports it per path.
    until(&mut ws, "the rename", |v| {
        let paired = v["op"] == "change"
            && v["events"].as_array().unwrap().iter().any(|e| {
                e["kind"] == "renamed" && e["path"] == json!(a) && e["to"] == json!(b)
            });
        paired || has(v, 1, "created", &b)
    })
    .await;

    std::fs::remove_file(&b).unwrap();
    until(&mut ws, "removed b.txt", |v| has(v, 1, "removed", &b)).await;
}

#[tokio::test]
async fn fr_w4d_repeated_writes_are_coalesced_into_one_entry_per_path() {
    let f = fx().await;
    let mut ws = f.client.fs_watch_socket().await.unwrap();
    assert_eq!(watch(&mut ws, 1, &f.root, true).await["op"], "watching");
    let a = f.root.join("many.txt");
    for i in 0..50 {
        std::fs::write(&a, format!("{i}")).unwrap();
    }
    let v = until(&mut ws, "a change of many.txt", |v| {
        v["op"] == "change" && v["events"].as_array().unwrap().iter().any(|e| e["path"] == json!(a))
    })
    .await;
    let n = v["events"].as_array().unwrap().iter().filter(|e| e["path"] == json!(a)).count();
    assert_eq!(n, 1, "one batch lists a path once: {v}");
}

#[tokio::test]
async fn fr_w4d_paths_outside_the_roots_or_denied_are_refused() {
    let f = fx().await;
    let mut ws = f.client.fs_watch_socket().await.unwrap();
    let out = watch(&mut ws, 1, &f.outside, true).await;
    assert_eq!(out["op"], "error", "{out}");
    assert_eq!(out["code"], "forbidden_path");
    let den = watch(&mut ws, 2, &f.denied, true).await;
    assert_eq!(den["op"], "error", "{den}");
    assert_eq!(den["code"], "forbidden_path");
    let missing = watch(&mut ws, 3, &f.root.join("nope"), true).await;
    assert_eq!(missing["op"], "error", "{missing}");
    assert_eq!(missing["code"], "not_found");
    send(&mut ws, json!({ "op": "watch", "id": 4, "path": "relative/path", "recursive": true })).await;
    let rel = until(&mut ws, "the relative path answer", |v| v["id"] == 4).await;
    assert_eq!(rel["code"], "bad_request", "{rel}");
}

#[tokio::test]
async fn fr_w4d_a_denied_file_inside_a_watched_tree_gives_no_event() {
    let f = fx().await;
    let mut ws = f.client.fs_watch_socket().await.unwrap();
    assert_eq!(watch(&mut ws, 1, &f.root, true).await["op"], "watching");
    std::fs::write(f.denied.join("server-token"), "secret").unwrap();
    // A file outside the denied entry, written after, proves events flow.
    let marker = f.root.join("marker.txt");
    std::fs::write(&marker, "x").unwrap();
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + BOUND;
    while !seen.iter().any(|p: &String| p == marker.to_str().unwrap()) {
        assert!(tokio::time::Instant::now() < deadline, "no event for the marker: {seen:?}");
        seen.extend(paths_seen(&mut ws, Duration::from_millis(500)).await);
    }
    // Let anything still in flight arrive.
    seen.extend(paths_seen(&mut ws, Duration::from_secs(1)).await);
    let den = f.denied.to_str().unwrap();
    assert!(!seen.iter().any(|p| p.starts_with(den)), "a denied path leaked: {seen:?}");
}

#[tokio::test]
async fn fr_w4d_unwatch_stops_events() {
    let f = fx().await;
    let mut ws = f.client.fs_watch_socket().await.unwrap();
    assert_eq!(watch(&mut ws, 7, &f.root, true).await["op"], "watching");
    let a = f.root.join("before.txt");
    std::fs::write(&a, "x").unwrap();
    until(&mut ws, "an event before unwatch", |v| v["op"] == "change" && v["id"] == 7).await;

    send(&mut ws, json!({ "op": "unwatch", "id": 7 })).await;
    until(&mut ws, "the unwatch answer", |v| v["op"] == "unwatched" && v["id"] == 7).await;
    std::fs::write(f.root.join("after.txt"), "x").unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while let Some(v) = next(&mut ws, deadline.saturating_duration_since(tokio::time::Instant::now())).await {
        assert_ne!(v["op"], "change", "an event after unwatch: {v}");
    }

    send(&mut ws, json!({ "op": "unwatch", "id": 7 })).await;
    let again = until(&mut ws, "the second unwatch answer", |v| v["id"] == 7).await;
    assert_eq!(again["code"], "unknown_watch", "{again}");
}

#[tokio::test]
async fn fr_w4d_the_watch_caps_answer_too_many_watches() {
    let f = fx_with(WatchLimits { per_socket: 2, total: 3, ..WatchLimits::default() }).await;
    let mut one = f.client.fs_watch_socket().await.unwrap();
    assert_eq!(watch(&mut one, 1, &f.root, true).await["op"], "watching");
    let dup = watch(&mut one, 1, &f.root, true).await;
    assert_eq!(dup["code"], "duplicate_id", "{dup}");
    assert_eq!(watch(&mut one, 2, &f.root, false).await["op"], "watching");
    let third = watch(&mut one, 3, &f.root, true).await;
    assert_eq!(third["code"], "too_many_watches", "per socket: {third}");

    let mut two = f.client.fs_watch_socket().await.unwrap();
    assert_eq!(watch(&mut two, 1, &f.root, true).await["op"], "watching");
    let over = watch(&mut two, 2, &f.root, true).await;
    assert_eq!(over["code"], "too_many_watches", "per node: {over}");

    // Closing a socket gives its watches back.
    drop(one);
    let deadline = tokio::time::Instant::now() + BOUND;
    let mut id = 10;
    loop {
        let v = watch(&mut two, id, &f.root, true).await;
        if v["op"] == "watching" {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "the closed socket kept its watches: {v}");
        id += 1;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn fr_w4d_a_batch_over_the_cap_is_signalled_as_overflow() {
    let f = fx_with(WatchLimits { batch_events: 5, ..WatchLimits::default() }).await;
    let mut ws = f.client.fs_watch_socket().await.unwrap();
    assert_eq!(watch(&mut ws, 3, &f.root, true).await["op"], "watching");
    for i in 0..40 {
        std::fs::write(f.root.join(format!("f{i}.txt")), "x").unwrap();
    }
    let v = until(&mut ws, "an overflow", |v| v["op"] == "overflow").await;
    assert_eq!(v["id"], 3);
}

#[tokio::test]
async fn fr_w4d_git_internals_are_dropped_except_what_scm_needs() {
    let f = fx().await;
    let git = f.root.join(".git");
    std::fs::create_dir_all(git.join("objects/ab")).unwrap();
    std::fs::create_dir_all(git.join("refs/heads")).unwrap();
    let mut ws = f.client.fs_watch_socket().await.unwrap();
    assert_eq!(watch(&mut ws, 1, &f.root, true).await["op"], "watching");
    std::fs::write(git.join("objects/ab/cdef"), "blob").unwrap();
    std::fs::write(git.join("index.lock"), "lock").unwrap();
    std::fs::write(git.join("refs/heads/main"), "0000").unwrap();
    std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    let mut seen = Vec::new();
    let head = git.join("HEAD");
    let main = git.join("refs/heads/main");
    let deadline = tokio::time::Instant::now() + BOUND;
    while !(seen.contains(&head.to_str().unwrap().to_string()) && seen.contains(&main.to_str().unwrap().to_string())) {
        assert!(tokio::time::Instant::now() < deadline, "HEAD and the ref were not reported: {seen:?}");
        seen.extend(paths_seen(&mut ws, Duration::from_millis(500)).await);
    }
    seen.extend(paths_seen(&mut ws, Duration::from_secs(1)).await);
    assert!(!seen.iter().any(|p| p.contains("/objects")), "objects leaked: {seen:?}");
    assert!(!seen.iter().any(|p| p.ends_with("index.lock")), "the lock file leaked: {seen:?}");
}

#[tokio::test]
async fn fr_w4d_watch_needs_the_token() {
    let f = fx().await;
    let bad = NodeClient::new(&f.url, "wrong").unwrap();
    assert!(bad.fs_watch_socket().await.is_err());
}
