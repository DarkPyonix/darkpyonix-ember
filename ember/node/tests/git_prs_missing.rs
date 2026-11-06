//! FR-G8 when `gh` is not installed: a separate test binary, because `EMBER_GH_BIN` is read
//! from the process environment.
use std::path::{Path, PathBuf};
use ember_node::{api::{self, Node}, config::NodeConfig};
use serde_json::{json, Value};

#[tokio::test]
async fn fr_g8_gh_missing_answers_409_in_korean() {
    let scratch = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/git-prs-tests");
    std::fs::create_dir_all(&scratch).unwrap();
    let dir = tempfile::Builder::new().prefix("m").tempdir_in(&scratch).unwrap();
    let repo: PathBuf = dir.path().canonicalize().unwrap();
    let init = std::process::Command::new("git").args(["init", "-q"]).current_dir(&repo).status().unwrap();
    assert!(init.success());
    std::env::set_var("EMBER_GH_BIN", scratch.join("no-such-gh"));
    let node = Node::new(NodeConfig::new("token", vec![repo.clone()])).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/git/prs", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node));
    let response = reqwest::Client::new().post(&url).bearer_auth("token").json(&json!({"repo": repo})).send().await.unwrap();
    assert_eq!(response.status(), 409);
    let body: Value = response.json().await.unwrap();
    let message = body["error"].as_str().unwrap();
    assert!(message.contains("gh") && message.contains("설치"), "{message}");
}
