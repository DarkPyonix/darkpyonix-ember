//! FR-G17 missing git identity on a merge commit. Own test file (own process): it changes HOME and git env process-wide.
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
async fn fr_g17_missing_identity_is_a_korean_409_and_the_merge_stays() {
    let (dir, repo, base, client) = setup().await;
    let head = conflicting_merge(&repo);
    call(&client, &base, "resolve", json!({"repo": repo, "path": "a", "content": "1\nboth\n3\n"})).await;
    git(&repo, &["config", "--unset", "user.name"]);
    git(&repo, &["config", "--unset", "user.email"]);
    git(&repo, &["config", "user.useConfigOnly", "true"]);
    let home = dir.path().join("empty-home");
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var("GIT_CONFIG_GLOBAL", "/dev/null");
    std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
    for name in ["GIT_AUTHOR_NAME", "GIT_AUTHOR_EMAIL", "GIT_COMMITTER_NAME", "GIT_COMMITTER_EMAIL", "EMAIL"] {
        std::env::remove_var(name);
    }
    let (status, v) = call(&client, &base, "merge/commit", json!({"repo": repo, "confirm": true})).await;
    assert_eq!(status, 409, "{v}");
    assert_eq!(v["error"], "git 사용자 이름과 이메일이 설정되어 있지 않습니다.");
    no_english_git_text(&v);
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head);
    assert!(repo.join(".git/MERGE_HEAD").exists());
}
