//! FR-G6 stash routes against repositories under `.scratch/`.
use std::{path::{Path, PathBuf}, process::Command};
use ember_node::{api::{self, Node}, config::NodeConfig};
use serde_json::{json, Value};

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(repo).env("LC_ALL", "C").output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

async fn fixture() -> (tempfile::TempDir, PathBuf, String, reqwest::Client) {
    let scratch = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/git-stash-tests");
    std::fs::create_dir_all(&scratch).unwrap();
    let dir = tempfile::Builder::new().prefix("stash").tempdir_in(scratch).unwrap();
    let repo = dir.path().canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Tester"]);
    git(&repo, &["config", "user.email", "tester@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("a"), "base\n").unwrap();
    git(&repo, &["add", "a"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let node = Node::new(NodeConfig::new("token", vec![repo.clone()])).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node));
    (dir, repo, base, reqwest::Client::new())
}

async fn call(client: &reqwest::Client, base: &str, op: &str, body: Value) -> (u16, String) {
    let response = client.post(format!("{base}/git/stash/{op}")).bearer_auth("token").json(&body).send().await.unwrap();
    (response.status().as_u16(), response.text().await.unwrap())
}

#[tokio::test]
async fn fr_g6_push_list_and_pop_restore_changes() {
    let (_dir, repo, base, client) = fixture().await;
    std::fs::write(repo.join("a"), "changed\n").unwrap();
    std::fs::write(repo.join("new"), "untracked\n").unwrap();
    let body = json!({"repo": repo, "message": "save: work", "include_untracked": true});
    assert_eq!(call(&client, &base, "push", body).await.0, 204);
    assert!(git(&repo, &["status", "--porcelain"]).is_empty());
    let (status, text) = call(&client, &base, "list", json!({"repo": repo})).await;
    assert_eq!(status, 200, "{text}");
    let entries: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(entries.as_array().unwrap().len(), 1);
    assert_eq!(entries[0]["index"], 0);
    assert_eq!(entries[0]["message"], "save: work");
    assert_eq!(entries[0]["branch"], "main");
    assert!(!entries[0]["time"].as_str().unwrap().is_empty());
    assert_eq!(call(&client, &base, "pop", json!({"repo": repo, "index": 0})).await.0, 204);
    assert_eq!(std::fs::read_to_string(repo.join("a")).unwrap(), "changed\n");
    assert!(repo.join("new").exists());
    assert!(git(&repo, &["stash", "list"]).is_empty());
}

#[tokio::test]
async fn fr_g6_conflicting_apply_and_pop_keep_entry() {
    for op in ["apply", "pop"] {
        let (_dir, repo, base, client) = fixture().await;
        std::fs::write(repo.join("a"), "stashed\n").unwrap();
        assert_eq!(call(&client, &base, "push", json!({"repo": repo})).await.0, 204);
        std::fs::write(repo.join("a"), "committed\n").unwrap();
        git(&repo, &["commit", "-am", "other"]);
        let (status, text) = call(&client, &base, op, json!({"repo": repo, "index": 0})).await;
        assert_eq!(status, 409, "{text}");
        let error: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(error["code"], "conflict");
        assert!(error["error"].as_str().unwrap().contains("병합 충돌이 있습니다"));
        assert_eq!(git(&repo, &["stash", "list"]).lines().count(), 1);
    }
}

#[tokio::test]
async fn fr_g6_apply_and_drop_select_index() {
    let (_dir, repo, base, client) = fixture().await;
    for value in ["first\n", "second\n"] {
        std::fs::write(repo.join("a"), value).unwrap();
        assert_eq!(call(&client, &base, "push", json!({"repo": repo})).await.0, 204);
    }
    assert_eq!(call(&client, &base, "apply", json!({"repo": repo, "index": 1})).await.0, 204);
    assert_eq!(std::fs::read_to_string(repo.join("a")).unwrap(), "first\n");
    assert_eq!(call(&client, &base, "drop", json!({"repo": repo, "index": 1})).await.0, 204);
    assert_eq!(git(&repo, &["stash", "list"]).lines().count(), 1);
    assert_eq!(call(&client, &base, "drop", json!({"repo": repo, "index": 99})).await.0, 409);
}

#[tokio::test]
async fn fr_g6_invalid_or_missing_stash_index_is_conflict() {
    let (_dir, repo, base, client) = fixture().await;
    std::fs::write(repo.join("a"), "saved\n").unwrap();
    assert_eq!(call(&client, &base, "push", json!({"repo": repo})).await.0, 204);
    for op in ["apply", "pop", "drop"] {
        for body in [json!({"repo": repo, "index": 99}), json!({"repo": repo}), json!({"repo": repo, "index": "bad"})] {
            let (status, text) = call(&client, &base, op, body).await;
            assert_eq!(status, 409, "{op}: {text}");
            let error: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(error["code"], "conflict");
            assert!(error["error"].as_str().unwrap().contains("그런 stash가 없습니다"));
        }
    }
    assert_eq!(git(&repo, &["stash", "list"]).lines().count(), 1);
}
