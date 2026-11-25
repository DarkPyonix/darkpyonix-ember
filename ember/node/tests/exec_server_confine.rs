//! SPEC FR-X5, FR-X1 (#1047): `/exec-server` screens every client request before it reaches
//! codex. A path outside the allowed folders (plainly, through `..`, through a symlink, or in a
//! denied entry) and a method that `codex exec-server` rust-v0.167.0 does not have are answered
//! with a JSON-RPC error by the node and never written to codex.
//!
//! The fake codex checks its arguments and echoes stdin (`cat`), so a request that reaches it
//! comes back as itself. Each exchange ends with a sentinel request; codex answers lines in order,
//! so once the sentinel's echo is back, a refused request that had been forwarded would have been
//! echoed before it.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

mod common;

use ember_node::api::{self, Node};
use ember_node::client::NodeClient;
use ember_node::config::NodeConfig;
use ember_node::exec_server::{CODEX_BIN_ENV, INVALID_PARAMS, METHOD_NOT_FOUND};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

const TOKEN: &str = "t";
const SENTINEL: &str = r#"{"id":"end","method":"environment/info"}"#;

/// One fake codex for the whole binary: the variable is process-wide, so it is set once, before
/// any test spawns a child, and never changed.
fn fake_codex() {
    static FAKE: OnceLock<PathBuf> = OnceLock::new();
    FAKE.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap().keep();
        let fake = dir.join("fake-codex");
        common::write_exec_ready(
            &fake,
            "#!/bin/sh\n[ \"$*\" = \"exec-server --listen stdio\" ] || { echo \"bad args: $*\" >&2; exit 2; }\nexec cat\n",
        );
        std::env::set_var(CODEX_BIN_ENV, &fake);
        fake
    });
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
    base: String,
}

/// `<tmp>/root` (allowed, with `inside.txt`, `denied/`, and `link` -> `<tmp>/outside/secret`) and
/// `<tmp>/outside/secret`. The node allows `root` and denies `root/denied`.
async fn start() -> Fixture {
    fake_codex();
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().canonicalize().unwrap();
    let root = tmp.join("root");
    let outside = tmp.join("outside");
    std::fs::create_dir_all(root.join("denied")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(root.join("inside.txt"), "in").unwrap();
    std::fs::write(root.join("denied/x"), "no").unwrap();
    std::fs::write(outside.join("secret"), "no").unwrap();
    std::os::unix::fs::symlink(outside.join("secret"), root.join("link")).unwrap();

    let node = Node::new(NodeConfig::new(TOKEN, vec![root.clone()]).with_denied(vec![root.join("denied")])).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node));
    Fixture { _dir: dir, root, outside, base }
}

fn uri(p: &Path) -> String {
    format!("file://{}", p.display())
}

/// Send `requests` and the sentinel; return every line that came back (parsed) once the sentinel
/// echo and `refusals` other replies have arrived.
async fn exchange(f: &Fixture, requests: &[String], refusals: usize) -> Vec<Value> {
    let client = NodeClient::new(&f.base, TOKEN).unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut ws = client.exec_server().await.unwrap();
        for r in requests {
            ws.send(Message::binary(format!("{r}\n").into_bytes())).await.unwrap();
        }
        ws.send(Message::text(format!("{SENTINEL}\n"))).await.unwrap();
        let mut bytes = Vec::new();
        loop {
            match ws.next().await.expect("stream open").unwrap() {
                Message::Binary(b) => bytes.extend_from_slice(&b),
                other => panic!("unexpected frame {other:?}"),
            }
            // Only complete lines count.
            if !bytes.ends_with(b"\n") {
                continue;
            }
            let lines: Vec<Value> = bytes
                .split(|b| *b == b'\n')
                .filter(|l| !l.is_empty())
                .map(|l| serde_json::from_slice(l).expect("each line is JSON"))
                .collect();
            let done = lines.iter().any(|v| v["id"] == "end" && v["method"] == "environment/info");
            let errors = lines.iter().filter(|v| v.get("error").is_some()).count();
            if done && errors >= refusals {
                let _ = ws.close(None).await;
                return lines;
            }
        }
    })
    .await
    .expect("timed out")
}

/// The reply to request `id` is a node refusal with `code` (and, for paths, `data.code`), and the
/// request itself never came back from codex.
fn assert_refused(lines: &[Value], id: i64, code: i64, data_code: Option<&str>) {
    let mine: Vec<&Value> = lines.iter().filter(|v| v["id"] == id).collect();
    assert_eq!(mine.len(), 1, "exactly one reply for {id}: {lines:?}");
    let e = &mine[0]["error"];
    assert_eq!(e["code"], code, "{lines:?}");
    if let Some(d) = data_code {
        assert_eq!(e["data"]["code"], d, "{lines:?}");
    }
    assert!(mine[0].get("method").is_none(), "request {id} reached codex: {lines:?}");
}

fn read_file(id: i64, path: &str) -> String {
    json!({ "id": id, "method": "fs/readFile", "params": { "path": path, "sandbox": null } }).to_string()
}

fn start_process(id: i64, cwd: &str) -> String {
    json!({
        "id": id, "method": "process/start",
        "params": { "processId": "p", "argv": ["true"], "cwd": cwd, "env": {}, "tty": false, "arg0": null }
    })
    .to_string()
}

#[tokio::test]
async fn fr_x5_a_path_outside_the_roots_is_refused_and_not_forwarded() {
    let f = start().await;
    let lines = exchange(
        &f,
        &[read_file(1, &uri(&f.outside.join("secret"))), start_process(2, &uri(&f.outside))],
        2,
    )
    .await;
    assert_refused(&lines, 1, INVALID_PARAMS, Some("forbidden_path"));
    assert_refused(&lines, 2, INVALID_PARAMS, Some("forbidden_path"));
}

#[tokio::test]
async fn fr_x5_a_dotdot_escape_is_refused() {
    let f = start().await;
    let plain = format!("{}/../outside/secret", uri(&f.root));
    let encoded = format!("{}/%2e%2E/outside/secret", uri(&f.root));
    let slash = format!("{}/x%2F..%2F..%2Foutside%2Fsecret", uri(&f.root));
    let lines = exchange(&f, &[read_file(1, &plain), read_file(2, &encoded), start_process(3, &slash)], 3).await;
    for id in 1..=3 {
        assert_refused(&lines, id, INVALID_PARAMS, Some("forbidden_path"));
    }
}

#[tokio::test]
async fn fr_x5_a_symlink_pointing_outside_is_refused() {
    let f = start().await;
    let link = uri(&f.root.join("link"));
    let write = json!({
        "id": 2, "method": "fs/writeFile",
        "params": { "path": link, "dataBase64": "eA==", "sandbox": null }
    })
    .to_string();
    let lines = exchange(&f, &[read_file(1, &link), write], 2).await;
    assert_refused(&lines, 1, INVALID_PARAMS, Some("forbidden_path"));
    assert_refused(&lines, 2, INVALID_PARAMS, Some("forbidden_path"));
}

#[tokio::test]
async fn fr_x5_a_denied_entry_is_refused() {
    let f = start().await;
    let meta = json!({
        "id": 1, "method": "fs/getMetadata",
        "params": { "path": uri(&f.root.join("denied/x")), "sandbox": null }
    })
    .to_string();
    let copy = json!({
        "id": 2, "method": "fs/copy",
        "params": {
            "sourcePath": uri(&f.root.join("inside.txt")),
            "destinationPath": uri(&f.root.join("denied/y")),
            "recursive": false, "sandbox": null
        }
    })
    .to_string();
    let lines = exchange(&f, &[meta, copy, start_process(3, &uri(&f.root.join("denied")))], 3).await;
    for id in 1..=3 {
        assert_refused(&lines, id, INVALID_PARAMS, Some("forbidden_path"));
    }
}

#[tokio::test]
async fn fr_x5_a_path_inside_is_forwarded() {
    let f = start().await;
    let read = read_file(1, &uri(&f.root.join("inside.txt")));
    let new_file = json!({
        "id": 2, "method": "fs/writeFile",
        "params": { "path": uri(&f.root.join("new.txt")), "dataBase64": "eA==", "sandbox": null }
    })
    .to_string();
    let start = start_process(3, &uri(&f.root));
    let lines = exchange(&f, &[read.clone(), new_file.clone(), start.clone()], 0).await;
    // Every request came back from codex as the same JSON value, and nothing was refused.
    for sent in [read, new_file, start] {
        let sent: Value = serde_json::from_str(&sent).unwrap();
        assert!(lines.contains(&sent), "{sent} not forwarded: {lines:?}");
    }
    assert!(lines.iter().all(|v| v.get("error").is_none()), "{lines:?}");
}

#[tokio::test]
async fn fr_x5_an_unknown_method_is_refused() {
    let f = start().await;
    let rename = json!({
        "id": 1, "method": "fs/rename",
        "params": { "from": uri(&f.root.join("inside.txt")), "to": uri(&f.root.join("b")) }
    })
    .to_string();
    let lines = exchange(&f, &[rename], 1).await;
    assert_refused(&lines, 1, METHOD_NOT_FOUND, None);
}
