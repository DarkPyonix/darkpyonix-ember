//! FR-X5 (#1047): Codex on another computer needs a `codex` binary there. When it is missing,
//! `/exec-server` answers the upgrade with a clear error instead of closing the socket.
#![cfg(unix)]

use std::ffi::OsString;
use std::time::Duration;

mod common;

use ember_node::api::{self, Node};
use ember_node::client::{ClientError, NodeClient};
use ember_node::config::NodeConfig;
use ember_node::exec_server::{resolve_codex_bin_from, CODEX_BIN_ENV};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn fr_x5_codex_is_looked_up_on_path_and_its_absence_is_named() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let empty = base.join("empty");
    let bin = base.join("bin");
    std::fs::create_dir_all(&empty).unwrap();
    std::fs::create_dir_all(&bin).unwrap();

    let e = resolve_codex_bin_from(None, Some(empty.clone().into_os_string())).unwrap_err().to_string();
    assert!(e.contains("codex"), "{e}");
    assert!(e.contains(CODEX_BIN_ENV), "{e}");
    assert!(e.contains("not installed"), "{e}");
    assert!(resolve_codex_bin_from(None, None).is_err());

    // A plain file named codex that is not executable does not count.
    std::fs::write(bin.join("codex"), "not a program").unwrap();
    assert!(resolve_codex_bin_from(None, Some(bin.clone().into_os_string())).is_err());

    common::write_exec_ready(&bin.join("codex"), "#!/bin/sh\nexit 0\n");
    let path: OsString = std::env::join_paths([empty.clone(), bin.clone()]).unwrap();
    assert_eq!(resolve_codex_bin_from(None, Some(path.clone())).unwrap(), bin.join("codex"));

    // The configured binary wins, and a configured path that is missing is named in the error.
    let other = base.join("other-codex");
    let e = resolve_codex_bin_from(Some(other.clone().into_os_string()), Some(path.clone())).unwrap_err().to_string();
    assert!(e.contains(&other.display().to_string()), "{e}");
    assert!(e.contains(CODEX_BIN_ENV), "{e}");
    common::write_exec_ready(&other, "#!/bin/sh\nexit 0\n");
    assert_eq!(resolve_codex_bin_from(Some(other.clone().into_os_string()), Some(path.clone())).unwrap(), other);
    // A bare name is looked up on PATH.
    assert_eq!(resolve_codex_bin_from(Some(OsString::from("codex")), Some(path)).unwrap(), bin.join("codex"));
}

#[tokio::test]
async fn fr_x5_missing_codex_answers_the_upgrade_with_a_clear_error() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let missing = root.join("no-such-codex");
    // The only test in this binary that reads the variable, so setting it is safe.
    std::env::set_var(CODEX_BIN_ENV, &missing);

    let node = Node::new(NodeConfig::new("t", vec![root.clone()])).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(api::serve(listener, node));

    let client = NodeClient::new(&format!("http://{addr}"), "t").unwrap();
    let Err(e) = client.exec_server().await else { panic!("the upgrade must be refused") };
    assert!(matches!(e, ClientError::Api { status: 503, .. }), "{e:?}");

    // The body names the cause and the fix (read over raw TCP so the whole body is seen).
    let body = tokio::time::timeout(Duration::from_secs(10), async {
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        s.write_all(
            b"GET /exec-server HTTP/1.1\r\nHost: node\r\nAuthorization: Bearer t\r\nConnection: Upgrade, close\r\n\
              Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
        )
        .await
        .unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).await.unwrap();
        String::from_utf8_lossy(&out).into_owned()
    })
    .await
    .expect("timed out");
    assert!(body.starts_with("HTTP/1.1 503"), "{body}");
    assert!(body.contains("not installed"), "{body}");
    assert!(body.contains(CODEX_BIN_ENV), "{body}");
    assert!(body.contains(&missing.display().to_string()), "{body}");
}
