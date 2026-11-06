//! FR-G16 merge state and FR-G17 merge abort and commit over HTTP, on repositories under `.scratch/`.
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

/// Like `git`, but the exit code is the caller's business (a conflicting merge exits 1).
fn git_try(dir: &Path, args: &[&str]) -> bool {
    Command::new("git").args(args).current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C").output().unwrap().status.success()
}

async fn setup() -> (tempfile::TempDir, PathBuf, String, reqwest::Client) {
    let scratch = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/git-merge-tests");
    std::fs::create_dir_all(&scratch).unwrap();
    std::env::set_var("GIT_CEILING_DIRECTORIES", scratch.canonicalize().unwrap());
    let dir = tempfile::Builder::new().prefix("m").tempdir_in(scratch).unwrap();
    std::fs::create_dir_all(dir.path().join("repo")).unwrap();
    let repo = dir.path().join("repo").canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Tester"]);
    git(&repo, &["config", "user.email", "tester@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    let node = Node::new(NodeConfig::new("token", vec![dir.path().canonicalize().unwrap()])).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node));
    (dir, repo, base, reqwest::Client::new())
}

fn commit(repo: &Path, file: &str, content: &str, subject: &str) -> String {
    std::fs::write(repo.join(file), content).unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", subject]);
    git(repo, &["rev-parse", "HEAD"])
}

/// `main` and `feat/x` both edit line 2 of `a`; `git merge feat/x` stops with a conflict.
/// Returns the HEAD sha of main before the merge.
fn conflicting_merge(repo: &Path) -> String {
    commit(repo, "a", "1\nbase\n3\n", "base");
    git(repo, &["checkout", "-q", "-b", "feat/x"]);
    commit(repo, "a", "1\nfeature\n3\n", "feature edit");
    git(repo, &["checkout", "-q", "main"]);
    let head = commit(repo, "a", "1\nmain side\n3\n", "main edit");
    assert!(!git_try(repo, &["merge", "feat/x"]), "the merge should conflict");
    assert!(repo.join(".git/MERGE_HEAD").exists());
    head
}

fn no_english_git_text(v: &Value) {
    let text = v["error"].as_str().unwrap_or_default();
    for word in ["CONFLICT", "error:", "fatal:", "hint:", "Unable", "index.lock", "could not", "Merge conflict", "Automatic merge", "MERGE_HEAD"] {
        assert!(!text.contains(word), "{word} in {text:?}");
    }
}

async fn call(client: &reqwest::Client, base: &str, op: &str, body: Value) -> (u16, Value) {
    let response = client.post(format!("{base}/git/{op}")).bearer_auth("token").json(&body).send().await.unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

#[tokio::test]
async fn fr_g16_state_without_a_merge() {
    let (_dir, repo, base, client) = setup().await;
    commit(&repo, "a", "1\n", "root");
    let (status, v) = call(&client, &base, "merge", json!({"repo": repo})).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v, json!({"in_progress": false, "from": null, "into": "main", "conflict_files": 0, "can_commit": false}));
}

#[tokio::test]
async fn fr_g16_state_during_a_conflicting_merge_then_after_resolving() {
    let (_dir, repo, base, client) = setup().await;
    conflicting_merge(&repo);
    let (status, v) = call(&client, &base, "merge", json!({"repo": repo})).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v, json!({"in_progress": true, "from": "feat/x", "into": "main", "conflict_files": 1, "can_commit": false}));
    let (status, r) = call(&client, &base, "resolve", json!({"repo": repo, "path": "a", "content": "1\nboth\n3\n"})).await;
    assert_eq!(status, 200, "{r}");
    let (_, v) = call(&client, &base, "merge", json!({"repo": repo})).await;
    assert_eq!(v, json!({"in_progress": true, "from": "feat/x", "into": "main", "conflict_files": 0, "can_commit": true}));
}

#[tokio::test]
async fn fr_g16_from_falls_back_to_the_merge_head_name() {
    let (_dir, repo, base, client) = setup().await;
    conflicting_merge(&repo);
    // An unreadable message still names the branch through MERGE_HEAD.
    std::fs::write(repo.join(".git/MERGE_MSG"), "Merge made by hand\n").unwrap();
    let (_, v) = call(&client, &base, "merge", json!({"repo": repo})).await;
    assert_eq!(v["in_progress"], true);
    assert_eq!(v["from"], "feat/x", "{v}");
}

#[tokio::test]
async fn fr_g16_non_repository_is_404() {
    let (dir, _repo, base, client) = setup().await;
    let plain = dir.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let (status, _) = call(&client, &base, "merge", json!({"repo": plain.canonicalize().unwrap()})).await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn fr_g17_abort_restores_the_tree_and_clears_merge_head() {
    let (_dir, repo, base, client) = setup().await;
    let head = conflicting_merge(&repo);
    let (status, v) = call(&client, &base, "merge/abort", json!({"repo": repo, "confirm": true})).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v, json!({"ok": true}));
    assert!(!repo.join(".git/MERGE_HEAD").exists());
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
    assert_eq!(std::fs::read_to_string(repo.join("a")).unwrap(), "1\nmain side\n3\n");
    let (_, s) = call(&client, &base, "merge", json!({"repo": repo})).await;
    assert_eq!(s["in_progress"], false);
    assert_eq!(s["conflict_files"], 0);
}

#[tokio::test]
async fn fr_g17_commit_after_resolving_makes_a_two_parent_merge_commit() {
    let (_dir, repo, base, client) = setup().await;
    let head = conflicting_merge(&repo);
    let feat = git(&repo, &["rev-parse", "feat/x"]);
    call(&client, &base, "resolve", json!({"repo": repo, "path": "a", "content": "1\nboth\n3\n"})).await;
    let (status, v) = call(&client, &base, "merge/commit", json!({"repo": repo, "confirm": true})).await;
    assert_eq!(status, 200, "{v}");
    let new_sha = v["new_sha"].as_str().unwrap().to_owned();
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), new_sha);
    assert_eq!(git(&repo, &["rev-list", "--parents", "-n", "1", "HEAD"]), format!("{new_sha} {head} {feat}"));
    assert!(!repo.join(".git/MERGE_HEAD").exists());
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
    let (_, s) = call(&client, &base, "merge", json!({"repo": repo})).await;
    assert_eq!(s["in_progress"], false);
}

#[tokio::test]
async fn fr_g17_commit_with_conflicts_left_is_409_and_changes_nothing() {
    let (_dir, repo, base, client) = setup().await;
    let head = conflicting_merge(&repo);
    let (status, v) = call(&client, &base, "merge/commit", json!({"repo": repo, "confirm": true})).await;
    assert_eq!(status, 409, "{v}");
    assert_eq!(v["error"], "충돌이 남아 있어 병합 커밋을 만들 수 없습니다.");
    no_english_git_text(&v);
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head);
    assert!(repo.join(".git/MERGE_HEAD").exists());
    assert_eq!(git(&repo, &["diff", "--name-only", "--diff-filter=U"]), "a");
}

#[tokio::test]
async fn fr_g17_abort_and_commit_without_a_merge_are_409() {
    let (_dir, repo, base, client) = setup().await;
    let head = commit(&repo, "a", "1\n", "root");
    for op in ["merge/abort", "merge/commit"] {
        let (status, v) = call(&client, &base, op, json!({"repo": repo, "confirm": true})).await;
        assert_eq!(status, 409, "{op} {v}");
        assert_eq!(v["error"], "진행 중인 병합이 없습니다.");
        no_english_git_text(&v);
    }
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head);
}

#[tokio::test]
async fn fr_g17_missing_confirm_is_400_before_any_git_call() {
    let (dir, repo, base, client) = setup().await;
    let head = conflicting_merge(&repo);
    for op in ["merge/abort", "merge/commit"] {
        for body in [json!({"repo": repo}), json!({"repo": repo, "confirm": false})] {
            let (status, v) = call(&client, &base, op, body).await;
            assert_eq!(status, 400, "{op} {v}");
            assert_eq!(v["error"], "확인이 필요합니다.");
        }
        // No repository lookup either: a path outside the node's roots is still the confirm error.
        let (status, v) = call(&client, &base, op, json!({"repo": dir.path().join("elsewhere")})).await;
        assert_eq!(status, 400, "{op} {v}");
    }
    assert!(repo.join(".git/MERGE_HEAD").exists());
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head);
}

#[tokio::test]
async fn fr_g17_user_hooks_do_not_run() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, repo, base, client) = setup().await;
    conflicting_merge(&repo);
    call(&client, &base, "resolve", json!({"repo": repo, "path": "a", "content": "1\nboth\n3\n"})).await;
    let marker = dir.path().join("hook-ran");
    for name in ["pre-merge-commit", "commit-msg", "post-commit", "post-merge"] {
        let hook = repo.join(".git/hooks").join(name);
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let (status, v) = call(&client, &base, "merge/commit", json!({"repo": repo, "confirm": true})).await;
    assert_eq!(status, 200, "{v}");
    assert!(!marker.exists(), "a hook ran through the API");
}

#[tokio::test]
async fn fr_g17_abort_hooks_do_not_run() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, repo, base, client) = setup().await;
    conflicting_merge(&repo);
    let marker = dir.path().join("hook-ran");
    let hook = repo.join(".git/hooks/post-checkout");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (status, v) = call(&client, &base, "merge/abort", json!({"repo": repo, "confirm": true})).await;
    assert_eq!(status, 200, "{v}");
    assert!(!marker.exists(), "a hook ran through the API");
}
