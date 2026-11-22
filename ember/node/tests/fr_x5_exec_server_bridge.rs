//! FR-X5 / FR-X1 (#1047): `/exec-server` end to end. A request for a path outside the node's
//! allowed folders is answered by the node with a JSON-RPC error and never reaches codex; a
//! request inside reaches it. The fake codex is `cat`, so whatever reaches it comes back.
#![cfg(unix)]

use std::time::Duration;

mod common;

use ember_node::api::{self, Node};
use ember_node::client::NodeClient;
use ember_node::config::NodeConfig;
use ember_node::exec_server::{ExecServerStream, CODEX_BIN_ENV};
use ember_node::exec_server_guard::FORBIDDEN_CODE;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

/// Read whole lines from the stream until one arrives (codex output may be chunked arbitrarily).
async fn next_line(ws: &mut ExecServerStream, buf: &mut Vec<u8>) -> Value {
    loop {
        if let Some(i) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=i).collect();
            return serde_json::from_slice(&line[..line.len() - 1]).unwrap();
        }
        match ws.next().await.expect("stream open").unwrap() {
            Message::Binary(b) => buf.extend_from_slice(&b),
            Message::Text(t) => buf.extend_from_slice(t.as_str().as_bytes()),
            other => panic!("unexpected frame {other:?}"),
        }
    }
}

fn line(v: Value) -> Message {
    let mut b = serde_json::to_vec(&v).unwrap();
    b.push(b'\n');
    Message::binary(b)
}

#[tokio::test]
async fn fr_x5_exec_server_refuses_outside_paths_and_relays_inside_ones() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let root = base.join("root");
    let outside = base.join("outside");
    std::fs::create_dir_all(root.join("work")).unwrap();
    std::fs::create_dir_all(root.join(".ember")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(root.join("work/ok"), "inside").unwrap();
    std::fs::write(outside.join("secret"), "s3cret").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("out")).unwrap();
    let fake = base.join("fake-codex");
    common::write_exec_ready(
        &fake,
        "#!/bin/sh\n[ \"$*\" = \"exec-server --listen stdio\" ] || { echo \"bad args: $*\" >&2; exit 2; }\nexec cat\n",
    );
    // The only test in this binary, so setting process environment is safe.
    std::env::set_var(CODEX_BIN_ENV, &fake);

    let node = Node::new(NodeConfig::new("t", vec![root.clone()]).with_denied(vec![root.join(".ember")])).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node));
    let client = NodeClient::new(&url, "t").unwrap();

    tokio::time::timeout(Duration::from_secs(20), async {
        let mut ws = client.exec_server().await.unwrap();
        let mut buf = Vec::new();
        let file = |p: &std::path::Path| format!("file://{}", p.display());

        // Outside, through `..`: the node answers, codex never sees it.
        ws.send(line(json!({"id": 1, "method": "fs/readFile", "params": {"path": format!("{}/work/../../outside/secret", file(&root))}}))).await.unwrap();
        let r = next_line(&mut ws, &mut buf).await;
        assert_eq!(r["id"], json!(1), "{r}");
        assert_eq!(r["error"]["code"], json!(FORBIDDEN_CODE), "{r}");

        // Inside: reaches codex (echoed back by the fake).
        let inside = json!({"id": 2, "method": "fs/readFile", "params": {"path": file(&root.join("work/ok"))}});
        ws.send(line(inside.clone())).await.unwrap();
        assert_eq!(next_line(&mut ws, &mut buf).await, inside);

        // A command whose cwd leaves through a symlink.
        ws.send(line(json!({"id": 3, "method": "process/start", "params": {"processId": "p", "argv": ["cat", "secret"], "cwd": file(&root.join("out")), "env": {}, "tty": false}}))).await.unwrap();
        let r = next_line(&mut ws, &mut buf).await;
        assert_eq!(r["id"], json!(3), "{r}");
        assert_eq!(r["error"]["code"], json!(FORBIDDEN_CODE), "{r}");

        // The denied entry inside the root.
        ws.send(line(json!({"id": 4, "method": "fs/readDirectory", "params": {"path": file(&root.join(".ember"))}}))).await.unwrap();
        let r = next_line(&mut ws, &mut buf).await;
        assert_eq!(r["id"], json!(4), "{r}");
        assert!(r.get("error").is_some(), "{r}");

        // One frame may carry several lines, and a line may span frames.
        let a = serde_json::to_vec(&json!({"id": 5, "method": "fs/readFile", "params": {"path": file(&outside.join("secret"))}})).unwrap();
        let b = json!({"id": 6, "method": "fs/getMetadata", "params": {"path": file(&root.join("work/ok"))}});
        let mut bytes = a;
        bytes.push(b'\n');
        let b_bytes = serde_json::to_vec(&b).unwrap();
        let (b1, b2) = b_bytes.split_at(b_bytes.len() / 2);
        bytes.extend_from_slice(b1);
        ws.send(Message::binary(bytes)).await.unwrap();
        let mut rest = b2.to_vec();
        rest.push(b'\n');
        ws.send(Message::binary(rest)).await.unwrap();
        let r = next_line(&mut ws, &mut buf).await;
        assert_eq!(r["id"], json!(5), "{r}");
        assert_eq!(r["error"]["code"], json!(FORBIDDEN_CODE), "{r}");
        assert_eq!(next_line(&mut ws, &mut buf).await, b);

        ws.close(None).await.unwrap();
        // Nothing refused was echoed: every line still to come is checked.
        while let Some(msg) = ws.next().await {
            match msg {
                Ok(Message::Binary(b)) => buf.extend_from_slice(&b),
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
        let rest = String::from_utf8_lossy(&buf);
        assert!(!rest.contains("secret"), "a refused request reached codex: {rest}");
    })
    .await
    .expect("timed out");
}
