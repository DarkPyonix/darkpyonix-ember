//! FR-G14 commit detail and FR-G15 revert and cherry-pick over HTTP, on repositories under `.scratch/`.
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
    let scratch = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/git-commit-tests");
    std::fs::create_dir_all(&scratch).unwrap();
    // The scratch directory sits inside this repository's work tree; stop git's upward search
    // there, so a plain directory below it is "not a repository".
    std::env::set_var("GIT_CEILING_DIRECTORIES", scratch.canonicalize().unwrap());
    let dir = tempfile::Builder::new().prefix("c").tempdir_in(scratch).unwrap();
    // The repository is a subdirectory, so `dir` also holds a sibling that is not a repository.
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

async fn call(client: &reqwest::Client, base: &str, op: &str, body: Value) -> (u16, Value) {
    let response = client.post(format!("{base}/git/{op}")).bearer_auth("token").json(&body).send().await.unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

#[tokio::test]
async fn fr_g14_ordinary_commit_lists_add_modify_delete_rename() {
    let (_dir, repo, base, client) = setup().await;
    commit(&repo, "keep", "1\n2\n3\n", "root");
    std::fs::write(repo.join("gone"), "bye\n").unwrap();
    std::fs::write(repo.join("old name.txt"), "line a\nline b\nline c\nline d\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "second"]);
    git(&repo, &["tag", "v1"]);
    std::fs::write(repo.join("keep"), "1\n2\n3\n4\n5\n").unwrap();
    std::fs::write(repo.join("added"), "x\ny\n").unwrap();
    std::fs::remove_file(repo.join("gone")).unwrap();
    git(&repo, &["mv", "old name.txt", "new name.txt"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "change things", "-m", "A longer body."]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let parent = git(&repo, &["rev-parse", "HEAD^"]);
    let (status, v) = call(&client, &base, "commit/show", json!({"repo": repo, "sha": &head[..10]})).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["sha"], head);
    assert_eq!(v["short"], &head[..7]);
    assert_eq!(v["subject"], "change things");
    assert_eq!(v["body"], "A longer body.");
    assert_eq!(v["author"], "Tester");
    assert!(v.get("email").is_none());
    assert_eq!(v["parents"], json!([parent]));
    assert!(v["time"].as_str().unwrap().contains('T'));
    assert!(v["refs"].as_array().unwrap().contains(&json!({"name": "refs/heads/main", "kind": "branch"})));
    assert_eq!(v["files_total"], 4);
    assert_eq!(v["truncated"], false);
    let files = v["files"].as_array().unwrap();
    let find = |p: &str| files.iter().find(|f| f["path"] == p).unwrap_or_else(|| panic!("{p} in {files:?}"));
    assert_eq!(find("added")["status"], "added");
    assert_eq!(find("added")["additions"], 2);
    assert_eq!(find("keep")["status"], "modified");
    assert_eq!((find("keep")["additions"].as_u64(), find("keep")["deletions"].as_u64()), (Some(2), Some(0)));
    assert_eq!(find("gone")["status"], "deleted");
    assert_eq!(find("gone")["deletions"], 1);
    assert_eq!(find("new name.txt")["status"], "renamed");
    assert_eq!(find("new name.txt")["old_path"], "old name.txt");
    assert_eq!(find("new name.txt")["binary"], false);
}

#[tokio::test]
async fn fr_g14_binary_file_has_zero_counts_and_flag() {
    let (_dir, repo, base, client) = setup().await;
    std::fs::write(repo.join("img.bin"), [0u8, 1, 2, 255, 0, 7]).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "bin"]);
    let (status, v) = call(&client, &base, "commit/show", json!({"repo": repo, "sha": git(&repo, &["rev-parse", "HEAD"])})).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["files"][0]["binary"], true);
    assert_eq!(v["files"][0]["additions"], 0);
}

#[tokio::test]
async fn fr_g14_merge_commit_diffs_against_first_parent() {
    let (_dir, repo, base, client) = setup().await;
    commit(&repo, "root", "r\n", "root");
    git(&repo, &["checkout", "-q", "-b", "feature"]);
    commit(&repo, "feature", "f\n", "feature");
    git(&repo, &["checkout", "-q", "main"]);
    let main = commit(&repo, "main", "m\n", "main");
    git(&repo, &["merge", "-q", "--no-ff", "feature", "-m", "merge feature"]);
    let merge = git(&repo, &["rev-parse", "HEAD"]);
    let (status, v) = call(&client, &base, "commit/show", json!({"repo": repo, "sha": merge})).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["parents"].as_array().unwrap().len(), 2);
    assert_eq!(v["parents"][0], main);
    assert_eq!(v["files_total"], 1);
    assert_eq!(v["files"][0]["path"], "feature");
    assert_eq!(v["files"][0]["status"], "added");
}

#[tokio::test]
async fn fr_g14_root_commit_lists_every_file_as_added() {
    let (_dir, repo, base, client) = setup().await;
    std::fs::write(repo.join("a"), "1\n").unwrap();
    std::fs::write(repo.join("b"), "2\n3\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    let root = git(&repo, &["rev-parse", "HEAD"]);
    let (status, v) = call(&client, &base, "commit/show", json!({"repo": repo, "sha": root})).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["parents"], json!([]));
    assert_eq!(v["files_total"], 2);
    assert!(v["files"].as_array().unwrap().iter().all(|f| f["status"] == "added"));
}

#[tokio::test]
async fn fr_g14_unknown_bad_and_non_repo() {
    let (dir, repo, base, client) = setup().await;
    commit(&repo, "a", "1\n", "first");
    let (status, v) = call(&client, &base, "commit/show", json!({"repo": repo, "sha": "deadbeef"})).await;
    assert_eq!(status, 404, "{v}");
    assert_eq!(v["code"], "not_found");
    assert!(v["error"].as_str().unwrap().contains("커밋"));
    for sha in ["abc12", "--all", "zzzzzzz", ""] {
        let (status, _) = call(&client, &base, "commit/show", json!({"repo": repo, "sha": sha})).await;
        assert_eq!(status, 400, "{sha:?}");
    }
    let plain = dir.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let (status, v) = call(&client, &base, "commit/show", json!({"repo": plain.canonicalize().unwrap(), "sha": "deadbeef"})).await;
    assert_eq!(status, 404, "{v}");
    assert_eq!(v["code"], "not_a_repo");
}

#[tokio::test]
async fn fr_g14_branch_can_start_from_a_sha() {
    let (_dir, repo, base, client) = setup().await;
    let first = commit(&repo, "a", "1\n", "first");
    commit(&repo, "a", "2\n", "second");
    let (status, v) = call(&client, &base, "branch", json!({"repo": repo, "name": "from-sha", "from": &first[..8]})).await;
    assert!(status == 200 || status == 204, "{status} {v}");
    assert_eq!(git(&repo, &["rev-parse", "from-sha"]), first);
}

#[tokio::test]
async fn fr_g15_revert_creates_a_revert_commit() {
    let (_dir, repo, base, client) = setup().await;
    commit(&repo, "a", "base\n", "base");
    let last = commit(&repo, "a", "changed\n", "change");
    let (status, v) = call(&client, &base, "revert", json!({"repo": repo, "sha": last, "confirm": true})).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["new_sha"], git(&repo, &["rev-parse", "HEAD"]));
    assert_eq!(std::fs::read_to_string(repo.join("a")).unwrap(), "base\n");
    assert!(git(&repo, &["log", "-1", "--format=%s"]).starts_with("Revert"));
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
}

#[tokio::test]
async fn fr_g15_revert_of_a_merge_uses_the_first_parent() {
    let (_dir, repo, base, client) = setup().await;
    commit(&repo, "root", "r\n", "root");
    git(&repo, &["checkout", "-q", "-b", "feature"]);
    commit(&repo, "feature", "f\n", "feature");
    git(&repo, &["checkout", "-q", "main"]);
    commit(&repo, "main", "m\n", "main");
    git(&repo, &["merge", "-q", "--no-ff", "feature", "-m", "merge"]);
    let merge = git(&repo, &["rev-parse", "HEAD"]);
    let (status, v) = call(&client, &base, "revert", json!({"repo": repo, "sha": merge, "confirm": true})).await;
    assert_eq!(status, 200, "{v}");
    assert!(!repo.join("feature").exists());
}

#[tokio::test]
async fn fr_g15_cherry_pick_applies_a_commit_from_another_branch() {
    let (_dir, repo, base, client) = setup().await;
    commit(&repo, "a", "base\n", "base");
    git(&repo, &["checkout", "-q", "-b", "other"]);
    let picked = commit(&repo, "b", "from other\n", "add b");
    git(&repo, &["checkout", "-q", "main"]);
    // main moves on, so the pick has another parent and cannot reuse the original commit.
    commit(&repo, "c", "main only\n", "add c");
    let (status, v) = call(&client, &base, "cherry-pick", json!({"repo": repo, "sha": picked, "confirm": true})).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["new_sha"], git(&repo, &["rev-parse", "HEAD"]));
    assert_ne!(v["new_sha"], picked);
    assert_eq!(std::fs::read_to_string(repo.join("b")).unwrap(), "from other\n");
    assert_eq!(git(&repo, &["log", "-1", "--format=%s"]), "add b");
}

#[tokio::test]
async fn fr_g15_dirty_tree_is_409_and_changes_nothing() {
    let (_dir, repo, base, client) = setup().await;
    commit(&repo, "a", "base\n", "base");
    let last = commit(&repo, "a", "changed\n", "change");
    std::fs::write(repo.join("a"), "my work\n").unwrap();
    for op in ["revert", "cherry-pick"] {
        let (status, v) = call(&client, &base, op, json!({"repo": repo, "sha": last, "confirm": true})).await;
        assert_eq!(status, 409, "{op}: {v}");
        assert_eq!(v["error"], "작업 중인 변경이 있어 되돌릴 수 없습니다. 먼저 커밋하거나 스태시하세요.");
    }
    assert_eq!(std::fs::read_to_string(repo.join("a")).unwrap(), "my work\n");
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), last);
}

#[tokio::test]
async fn fr_g15_conflicting_cherry_pick_is_aborted() {
    let (_dir, repo, base, client) = setup().await;
    commit(&repo, "a", "base\n", "base");
    git(&repo, &["checkout", "-q", "-b", "other"]);
    let picked = commit(&repo, "a", "other side\n", "other edit");
    git(&repo, &["checkout", "-q", "main"]);
    let head = commit(&repo, "a", "main side\n", "main edit");
    let (status, v) = call(&client, &base, "cherry-pick", json!({"repo": repo, "sha": picked, "confirm": true})).await;
    assert_eq!(status, 409, "{v}");
    assert!(v["error"].as_str().unwrap().starts_with("충돌이 발생해 취소했습니다."));
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head);
    assert_eq!(std::fs::read_to_string(repo.join("a")).unwrap(), "main side\n");
}

#[tokio::test]
async fn fr_g15_conflicting_revert_is_aborted() {
    let (_dir, repo, base, client) = setup().await;
    let first = commit(&repo, "a", "one\n", "one");
    commit(&repo, "a", "two\n", "two");
    let head = commit(&repo, "a", "three\n", "three");
    let (status, v) = call(&client, &base, "revert", json!({"repo": repo, "sha": &first[..9], "confirm": true})).await;
    assert_eq!(status, 409, "{v}");
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head);
}

#[tokio::test]
async fn fr_g15_missing_confirm_is_400_before_any_git_call() {
    let (_dir, repo, base, client) = setup().await;
    let last = commit(&repo, "a", "base\n", "base");
    for op in ["revert", "cherry-pick"] {
        for body in [json!({"repo": repo, "sha": last}), json!({"repo": repo, "sha": last, "confirm": false})] {
            let (status, v) = call(&client, &base, op, body).await;
            assert_eq!(status, 400, "{op}: {v}");
        }
        // Not even the repository is looked at: a path outside every root would be 403 otherwise.
        let (status, _) = call(&client, &base, op, json!({"repo": "/nonexistent-elsewhere", "sha": last})).await;
        assert_eq!(status, 400);
    }
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), last);
}

#[tokio::test]
async fn fr_g15_cherry_pick_of_a_merge_is_400() {
    let (_dir, repo, base, client) = setup().await;
    commit(&repo, "root", "r\n", "root");
    git(&repo, &["checkout", "-q", "-b", "feature"]);
    commit(&repo, "feature", "f\n", "feature");
    git(&repo, &["checkout", "-q", "main"]);
    commit(&repo, "main", "m\n", "main");
    git(&repo, &["merge", "-q", "--no-ff", "feature", "-m", "merge"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["checkout", "-q", "-b", "elsewhere", "HEAD~1"]);
    let (status, v) = call(&client, &base, "cherry-pick", json!({"repo": repo, "sha": head, "confirm": true})).await;
    assert_eq!(status, 400, "{v}");
    assert!(v["error"].as_str().unwrap().contains("병합 커밋은 지원하지 않습니다"));
}

#[tokio::test]
async fn fr_g15_unknown_sha_is_404_and_bad_sha_is_400() {
    let (_dir, repo, base, client) = setup().await;
    commit(&repo, "a", "base\n", "base");
    let (status, _) = call(&client, &base, "revert", json!({"repo": repo, "sha": "deadbeef", "confirm": true})).await;
    assert_eq!(status, 404);
    let (status, _) = call(&client, &base, "cherry-pick", json!({"repo": repo, "sha": "-x", "confirm": true})).await;
    assert_eq!(status, 400);
}

fn no_english_git_text(v: &Value) {
    let text = v["error"].as_str().unwrap_or_default();
    for word in ["CONFLICT", "error:", "fatal:", "hint:", "Unable", "index.lock", "could not", "Merge conflict"] {
        assert!(!text.contains(word), "{word} in {text:?}");
    }
}

#[tokio::test]
async fn fr_g15_conflict_answers_carry_no_git_text() {
    let (_dir, repo, base, client) = setup().await;
    commit(&repo, "a", "base\n", "base");
    git(&repo, &["checkout", "-q", "-b", "other"]);
    let picked = commit(&repo, "a", "other side\n", "other edit");
    git(&repo, &["checkout", "-q", "main"]);
    commit(&repo, "a", "main side\n", "main edit");
    let (status, v) = call(&client, &base, "cherry-pick", json!({"repo": repo, "sha": picked, "confirm": true})).await;
    assert_eq!(status, 409, "{v}");
    assert_eq!(v["error"], "충돌이 발생해 취소했습니다.");
    no_english_git_text(&v);
}

#[tokio::test]
async fn fr_g15_index_lock_is_a_korean_409_and_not_claimed_cancelled() {
    let (_dir, repo, base, client) = setup().await;
    commit(&repo, "a", "base\n", "base");
    let last = commit(&repo, "a", "changed\n", "change");
    std::fs::write(repo.join(".git/index.lock"), "").unwrap();
    for op in ["revert", "cherry-pick"] {
        let (status, v) = call(&client, &base, op, json!({"repo": repo, "sha": last, "confirm": true})).await;
        assert_eq!(status, 409, "{op}: {v}");
        let text = v["error"].as_str().unwrap();
        assert!(text.contains("다른 git 작업이 진행 중입니다") || text.contains("취소하지 못했습니다"), "{text}");
        assert!(!text.contains("취소했습니다"), "{text}");
        no_english_git_text(&v);
    }
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), last);
}

#[tokio::test]
async fn fr_g15_paused_cherry_pick_is_refused_and_left_alone() {
    let (_dir, repo, base, client) = setup().await;
    commit(&repo, "a", "base\n", "base");
    git(&repo, &["checkout", "-q", "-b", "other"]);
    let picked = commit(&repo, "a", "other side\n", "other edit");
    git(&repo, &["checkout", "-q", "main"]);
    let head = commit(&repo, "a", "main side\n", "main edit");
    // Leave the cherry-pick paused on its conflict, as a person at a terminal might.
    let paused = Command::new("git").args(["cherry-pick", &picked]).current_dir(&repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
    assert!(!paused.status.success());
    assert!(repo.join(".git/CHERRY_PICK_HEAD").exists());
    for op in ["revert", "cherry-pick"] {
        let (status, v) = call(&client, &base, op, json!({"repo": repo, "sha": head, "confirm": true})).await;
        assert_eq!(status, 409, "{op}: {v}");
        assert_eq!(v["error"], "진행 중인 git 작업이 있습니다. 먼저 끝내거나 중단하세요.");
        assert!(repo.join(".git/CHERRY_PICK_HEAD").exists(), "{op} touched the user's cherry-pick");
    }
}

#[tokio::test]
async fn fr_g15_other_operation_markers_are_refused_and_left_alone() {
    let (_dir, repo, base, client) = setup().await;
    commit(&repo, "a", "base\n", "base");
    let last = commit(&repo, "a", "changed\n", "change");
    for marker in ["REVERT_HEAD", "MERGE_HEAD", "CHERRY_PICK_HEAD"] {
        let path = repo.join(".git").join(marker);
        std::fs::write(&path, &last).unwrap();
        for op in ["revert", "cherry-pick"] {
            let (status, v) = call(&client, &base, op, json!({"repo": repo, "sha": last, "confirm": true})).await;
            assert_eq!(status, 409, "{marker} {op}: {v}");
            assert_eq!(v["error"], "진행 중인 git 작업이 있습니다. 먼저 끝내거나 중단하세요.");
            assert!(path.exists(), "{marker} removed by {op}");
        }
        std::fs::remove_file(&path).unwrap();
    }
    for dir in ["sequencer", "rebase-merge", "rebase-apply"] {
        let path = repo.join(".git").join(dir);
        std::fs::create_dir_all(&path).unwrap();
        let (status, v) = call(&client, &base, "revert", json!({"repo": repo, "sha": last, "confirm": true})).await;
        assert_eq!(status, 409, "{dir}: {v}");
        assert!(path.exists());
        std::fs::remove_dir_all(&path).unwrap();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn fr_g15_user_hooks_do_not_run() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, repo, base, client) = setup().await;
    commit(&repo, "a", "base\n", "base");
    let last = commit(&repo, "a", "changed\n", "change");
    let marker = dir.path().join("hook-ran");
    let hook = repo.join(".git/hooks/post-commit");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (status, v) = call(&client, &base, "revert", json!({"repo": repo, "sha": last, "confirm": true})).await;
    assert_eq!(status, 200, "{v}");
    assert!(!marker.exists(), "post-commit hook ran through the API");
}

#[tokio::test]
async fn fr_g14_many_files_are_truncated_to_500() {
    let (_dir, repo, base, client) = setup().await;
    commit(&repo, "root", "r\n", "root");
    for i in 0..600 {
        std::fs::write(repo.join(format!("f{i:03}.txt")), "x\n").unwrap();
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "many"]);
    let (status, v) = call(&client, &base, "commit/show", json!({"repo": repo, "sha": git(&repo, &["rev-parse", "HEAD"])})).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["truncated"], true);
    assert_eq!(v["files"].as_array().unwrap().len(), 500);
    assert_eq!(v["files_total"], 600);
}

#[tokio::test]
async fn fr_g14_control_characters_are_stripped_from_the_message() {
    let (_dir, repo, base, client) = setup().await;
    std::fs::write(repo.join("a"), "1\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "title\u{1b}[31m red", "-m", "body\u{7}line"]);
    let (status, v) = call(&client, &base, "commit/show", json!({"repo": repo, "sha": git(&repo, &["rev-parse", "HEAD"])})).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["subject"], "title[31m red");
    assert_eq!(v["body"], "bodyline");
}
