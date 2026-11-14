//! FR-G5 history over HTTP, using repositories under `.scratch/`.
use std::path::{Path, PathBuf};
use std::process::Command;
use ember_node::{api::{self, Node}, config::NodeConfig};
use serde_json::{json, Value};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C").output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

async fn setup() -> (tempfile::TempDir, PathBuf, String, reqwest::Client) {
    let scratch = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/git-log-tests");
    std::fs::create_dir_all(&scratch).unwrap();
    let dir = tempfile::Builder::new().prefix("g").tempdir_in(scratch).unwrap();
    let repo = dir.path().canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Tester"]);
    git(&repo, &["config", "user.email", "tester@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    let node = Node::new(NodeConfig::new("token", vec![repo.clone()])).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/git/log", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node));
    (dir, repo, url, reqwest::Client::new())
}

fn commit(repo: &Path, file: &str, subject: &str) -> String {
    std::fs::write(repo.join(file), subject).unwrap();
    git(repo, &["add", file]);
    git(repo, &["commit", "-q", "-m", subject]);
    git(repo, &["rev-parse", "HEAD"])
}

async fn log(client: &reqwest::Client, url: &str, body: Value) -> Value {
    let response = client.post(url).bearer_auth("token").json(&body).send().await.unwrap();
    let status = response.status();
    let text = response.text().await.unwrap();
    assert_eq!(status, 200, "{text}");
    serde_json::from_str(&text).unwrap()
}

#[tokio::test]
async fn fr_g5_merge_refs_and_pagination() {
    let (_dir, repo, url, client) = setup().await;
    let root = commit(&repo, "root", "root");
    git(&repo, &["checkout", "-q", "-b", "feature"]);
    let feature = commit(&repo, "feature", "feature");
    git(&repo, &["tag", "v1"]);
    git(&repo, &["update-ref", "refs/remotes/origin/feature", &feature]);
    git(&repo, &["checkout", "-q", "main"]);
    let main = commit(&repo, "main", "main");
    git(&repo, &["merge", "-q", "--no-ff", "feature", "-m", "merge"]);
    let merge = git(&repo, &["rev-parse", "HEAD"]);
    let body = json!({"repo": repo});
    let result = log(&client, &url, body.clone()).await;
    let commits = result["commits"].as_array().unwrap();
    assert_eq!(commits.len(), 4);
    assert_eq!(commits[0]["sha"], merge);
    assert_eq!(commits[0]["parents"], json!([main, feature]));
    assert_eq!(commits[0]["author"], "Tester");
    assert_eq!(commits[0]["email"], "tester@example.com");
    assert!(commits[0]["time"].as_i64().unwrap() > 0);
    assert_eq!(commits[0]["subject"], "merge");
    let refs = commits[0]["refs"].as_array().unwrap();
    assert!(refs.contains(&json!({"name":"HEAD", "kind":"head"})));
    assert!(refs.contains(&json!({"name":"refs/heads/main", "kind":"branch"})));
    let tip = commits.iter().find(|c| c["sha"] == feature).unwrap();
    for (name, kind) in [("refs/heads/feature", "branch"), ("refs/tags/v1", "tag"), ("refs/remotes/origin/feature", "remote")] {
        assert!(tip["refs"].as_array().unwrap().contains(&json!({"name":name, "kind":kind})));
    }
    let page = log(&client, &url, json!({"repo":repo, "limit":1, "skip":1})).await;
    assert_eq!(page["commits"], json!([commits[1]]));
    let branch = log(&client, &url, json!({"repo":repo, "branch":"feature"})).await;
    assert_eq!(branch["commits"].as_array().unwrap().len(), 2);
    assert_eq!(branch["commits"][0]["sha"], feature);
    assert_eq!(branch["commits"][1]["sha"], root);
}

#[tokio::test]
async fn fr_g5_unborn_branch_is_empty() {
    let (_dir, repo, url, client) = setup().await;
    assert_eq!(log(&client, &url, json!({"repo":repo})).await, json!({"commits":[]}));
    assert_eq!(log(&client, &url, json!({"repo":repo, "branch":"main"})).await, json!({"commits":[]}));
}

#[tokio::test]
async fn fr_g5_nonexistent_branch_returns_korean_conflict() {
    let (_dir, repo, url, client) = setup().await;
    commit(&repo, "root", "root");
    let response = client.post(&url).bearer_auth("token")
        .json(&json!({"repo":repo, "branch":"missing"})).send().await.unwrap();
    assert_eq!(response.status(), 409);
    let body: Value = response.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("브랜치를 찾을 수 없습니다"));
}

#[tokio::test]
async fn fr_g5_detached_head_has_head_ref_without_branch() {
    let (_dir, repo, url, client) = setup().await;
    let root = commit(&repo, "root", "root");
    commit(&repo, "next", "next");
    git(&repo, &["checkout", "-q", "--detach", &root]);
    let result = log(&client, &url, json!({"repo":repo})).await;
    let commit = &result["commits"][0];
    assert_eq!(commit["sha"], root);
    let refs = commit["refs"].as_array().unwrap();
    assert!(refs.contains(&json!({"name":"HEAD", "kind":"head"})));
    assert!(!refs.iter().any(|r| r["kind"] == "branch"));
}

#[tokio::test]
async fn fr_g5_rejects_invalid_limit_and_branch() {
    let (_dir, repo, url, client) = setup().await;
    for body in [json!({"repo":repo, "limit":501}), json!({"repo":repo, "branch":"--all"})] {
        let response = client.post(&url).bearer_auth("token").json(&body).send().await.unwrap();
        assert_eq!(response.status(), 400);
    }
}
