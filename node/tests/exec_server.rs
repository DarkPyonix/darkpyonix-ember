//! `/v1/exec-server`: the raw byte relay to `codex exec-server --listen stdio`, exercised with a
//! fake codex (a shell script that checks its arguments and echoes stdin back).

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use ember_node::api::{self, Node};
use ember_node::client::{ClientError, NodeClient};
use ember_node::config::NodeConfig;
use ember_node::exec_server::CODEX_BIN_ENV;
use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn relays_bytes_both_ways_and_requires_the_token() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let fake = root.join("fake-codex");
    // Only the exact arguments the bridge promises are accepted.
    std::fs::write(
        &fake,
        "#!/bin/sh\n[ \"$*\" = \"exec-server --listen stdio\" ] || { echo \"bad args: $*\" >&2; exit 2; }\nexec cat\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    // The only test in this binary, so setting process environment is safe.
    std::env::set_var(CODEX_BIN_ENV, &fake);

    let node = Node::new(NodeConfig { token: "t".into(), roots: vec![root.clone()] }).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node));

    let bad = NodeClient::new(&base, "wrong").unwrap();
    let Err(e) = bad.exec_server().await else { panic!("refused") };
    assert!(matches!(e, ClientError::Api { status: 401, .. }), "{e:?}");

    let client = NodeClient::new(&base, "t").unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut ws = client.exec_server().await.unwrap();
        let line = b"{\"id\":1,\"method\":\"initialize\",\"params\":{}}\n".to_vec();
        ws.send(Message::binary(line.clone())).await.unwrap();
        // Text frames are relayed as their bytes too.
        ws.send(Message::text("{\"method\":\"initialized\"}\n")).await.unwrap();
        let mut want = line.clone();
        want.extend_from_slice(b"{\"method\":\"initialized\"}\n");
        let mut got = Vec::new();
        while got.len() < want.len() {
            match ws.next().await.expect("stream open").unwrap() {
                Message::Binary(b) => got.extend_from_slice(&b),
                other => panic!("unexpected frame {other:?}"),
            }
        }
        assert_eq!(got, want);
        // Closing our side ends the child (stdin closes, `cat` exits) and the server closes too.
        ws.close(None).await.unwrap();
        while let Some(msg) = ws.next().await {
            if msg.is_err() || matches!(msg, Ok(Message::Close(_))) {
                break;
            }
        }
    })
    .await
    .expect("timed out");
}
