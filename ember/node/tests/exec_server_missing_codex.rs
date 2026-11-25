//! SPEC FR-X5 (#1047): with no codex binary, `/exec-server` answers the upgrade with 503
//! `codex_not_installed` naming `EMBER_NODE_CODEX_BIN`, instead of opening and closing the
//! WebSocket with no reason. The only test in this binary, because it sets process environment.

use ember_node::api::{self, Node};
use ember_node::client::{ClientError, NodeClient};
use ember_node::config::NodeConfig;
use ember_node::exec_server::CODEX_BIN_ENV;
use ember_node::proto::ErrorCode;

#[tokio::test]
async fn fr_x5_missing_codex_answers_a_named_error() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let node = Node::new(NodeConfig::new("t", vec![root.clone()])).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node));
    let client = NodeClient::new(&base, "t").unwrap();

    // A path that does not exist, and a bare name that is not on PATH.
    for value in [root.join("no-such-codex").into_os_string(), "ember-test-no-such-codex".into()] {
        std::env::set_var(CODEX_BIN_ENV, &value);
        let Err(e) = client.exec_server().await else { panic!("{value:?}: the upgrade must be refused") };
        let ClientError::Api { status, body } = e else { panic!("{value:?}: {e:?}") };
        assert_eq!(status, 503, "{body:?}");
        assert_eq!(body.code, ErrorCode::CodexNotInstalled, "{body:?}");
        assert!(body.error.contains(CODEX_BIN_ENV), "the message names the variable: {}", body.error);
    }
}
