//! The git API over HTTP (SPEC FR-G1 to FR-G3): temp repositories under `.scratch/`, a local
//! bare repository as the remote for fetch, pull and push.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;

use ember_node::api::{self, Node};
use ember_node::config::NodeConfig;
use ember_node::git::*;
use serde::de::DeserializeOwned;
use serde::Serialize;

const TOKEN: &str = "test-token";

struct Fixture {
    base: String,
    root: PathBuf,
    http: reqwest::Client,
    _dir: tempfile::TempDir,
}

fn scratch() -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/git-api-tests");
    std::fs::create_dir_all(&p).unwrap();
    p.canonicalize().unwrap()
}

async fn start() -> Fixture {
    static ONCE: Once = Once::new();
    // The scratch directory sits inside this repository's work tree; stop git's upward search
    // there, so a plain directory below it is "not a repository".
    ONCE.call_once(|| {
        std::env::set_var("GIT_CEILING_DIRECTORIES", scratch());
        // Machine config must not leak in (user identity, signing), and the limits are small so
        // the timeout and size tests are quick.
        std::env::set_var("GIT_CONFIG_GLOBAL", "/dev/null");
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
        std::env::set_var("EMBER_GIT_NETWORK_TIMEOUT_SECS", "3");
        std::env::set_var("EMBER_GIT_MAX_OUTPUT_BYTES", "1048576");
    });
    let dir = tempfile::Builder::new().prefix("g").tempdir_in(scratch()).unwrap();
    let root = dir.path().canonicalize().unwrap();
    let node = Node::new(NodeConfig::new(TOKEN, vec![root.clone()])).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node));
    Fixture { base, root, http: reqwest::Client::new(), _dir: dir }
}

/// Run git in `dir` and return stdout; panics on failure.
fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("LC_ALL", "C")
        .env("GIT_CEILING_DIRECTORIES", scratch())
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.name", "Tester"]);
    git(dir, &["config", "user.email", "tester@example.com"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

fn write(dir: &Path, name: &str, text: &str) {
    let p = dir.join(name);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn commit_all(dir: &Path, msg: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", msg]);
}

fn s(p: &Path) -> String {
    p.to_str().unwrap().to_string()
}

impl Fixture {
    async fn raw(&self, op: &str, body: &impl Serialize) -> (u16, String) {
        let r = self
            .http
            .post(format!("{}/git/{op}", self.base))
            .bearer_auth(TOKEN)
            .json(body)
            .send()
            .await
            .unwrap();
        (r.status().as_u16(), r.text().await.unwrap())
    }

    async fn ok<T: DeserializeOwned>(&self, op: &str, body: &impl Serialize) -> T {
        let (status, text) = self.raw(op, body).await;
        assert_eq!(status, 200, "{op}: {text}");
        serde_json::from_str(&text).unwrap()
    }

    async fn no_content(&self, op: &str, body: &impl Serialize) {
        let (status, text) = self.raw(op, body).await;
        assert_eq!(status, 204, "{op}: {text}");
    }

    /// Expect an error response: its status and `code`, and the message.
    async fn err(&self, op: &str, body: &impl Serialize, status: u16, code: &str) -> String {
        let (st, text) = self.raw(op, body).await;
        assert_eq!(st, status, "{op}: {text}");
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["code"], code, "{text}");
        v["error"].as_str().unwrap().to_string()
    }

    async fn status(&self, repo: &Path) -> GitStatus {
        self.ok("status", &GitRepoRequest { repo: s(repo) }).await
    }

    async fn diff(&self, repo: &Path, path: Option<&str>, staged: bool) -> String {
        let d: GitDiff = self
            .ok("diff", &GitDiffRequest { repo: s(repo), path: path.map(str::to_string), staged, include_untracked: false })
            .await;
        d.patch
    }

    async fn stage(&self, repo: &Path, paths: &[&str]) {
        let body = GitPathsRequest { repo: s(repo), paths: paths.iter().map(|p| p.to_string()).collect() };
        self.no_content("stage", &body).await;
    }

    async fn commit(&self, repo: &Path, summary: &str) -> GitCommitResponse {
        self.ok(
            "commit",
            &GitCommitRequest { repo: s(repo), summary: summary.into(), description: None, amend: false },
        )
        .await
    }
}

fn merge_conflict(repo: &Path, diff3: bool) {
    init_repo(repo);
    if diff3 { git(repo, &["config", "merge.conflictStyle", "diff3"]); }
    write(repo, "a[1].txt", "before\nbase\nafter\n");
    commit_all(repo, "base");
    git(repo, &["switch", "-q", "-c", "side"]);
    write(repo, "a[1].txt", "before\nside\nafter\n");
    commit_all(repo, "side");
    git(repo, &["switch", "-q", "main"]);
    write(repo, "a[1].txt", "before\nmain\nafter\n");
    commit_all(repo, "main");
    let output = Command::new("git").args(["merge", "side"]).current_dir(repo).output().unwrap();
    assert!(!output.status.success());
}

#[tokio::test]
async fn fr_g7_conflict_blocks_and_resolution() {
    let f = start().await;
    let repo = f.root.join("r");
    merge_conflict(&repo, true);
    let request = serde_json::json!({"repo": s(&repo), "path": "a[1].txt"});
    let blocks: serde_json::Value = f.ok("conflict", &request).await;
    assert_eq!(blocks["blocks"], serde_json::json!([
        {"kind": "common", "text": "before\n"},
        {"kind": "conflict", "ours": "main\n", "theirs": "side\n", "ours_label": "HEAD", "theirs_label": "side"},
        {"kind": "common", "text": "after\n"}
    ]));
    let resolution = serde_json::json!({"repo": s(&repo), "path": "a[1].txt", "content": "before\nresolved\nafter\n"});
    f.no_content("resolve", &resolution).await;
    assert_eq!(std::fs::read_to_string(repo.join("a[1].txt")).unwrap(), "before\nresolved\nafter\n");
    f.commit(&repo, "resolve").await;
    let c: GitConflicts = f.ok("conflicts", &GitRepoRequest { repo: s(&repo) }).await;
    assert!(c.paths.is_empty());
    for op in ["conflict", "resolve"] {
        let body = if op == "conflict" { &request } else { &resolution };
        let msg = f.err(op, body, 409, "conflict").await;
        assert!(msg.chars().any(|c| ('\u{AC00}'..='\u{D7A3}').contains(&c)));
    }
}

#[tokio::test]
async fn fr_g7_standard_markers_and_literal_paths() {
    let f = start().await;
    let repo = f.root.join("r");
    merge_conflict(&repo, false);
    let body = serde_json::json!({"repo": s(&repo), "path": "a[1].txt"});
    let value: serde_json::Value = f.ok("conflict", &body).await;
    assert_eq!(value["blocks"][1]["ours"], "main\n");
    assert_eq!(value["blocks"][1]["theirs"], "side\n");
    let other = serde_json::json!({"repo": s(&repo), "path": "a1.txt"});
    f.err("conflict", &other, 409, "conflict").await;
}

#[tokio::test]
async fn fr_g7_rejects_paths_outside_repo() {
    let f = start().await;
    let repo = f.root.join("r");
    merge_conflict(&repo, false);
    let outside = f.root.join("outside");
    std::fs::write(&outside, "untouched").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, repo.join("escape")).unwrap();
    let mut paths = vec!["../outside".to_string(), s(&outside), String::new()];
    #[cfg(unix)]
    paths.push("escape".into());
    for path in paths {
        for op in ["conflict", "resolve"] {
            let body = serde_json::json!({"repo": s(&repo), "path": path, "content": "changed"});
            f.err(op, &body, 400, "bad_request").await;
        }
    }
    assert_eq!(std::fs::read_to_string(outside).unwrap(), "untouched");
}

#[tokio::test]
async fn fr_g7_subdirectory_missing_and_marker_formats() {
    let f = start().await;
    let repo = f.root.join("r");
    merge_conflict(&repo, false);
    let sub = repo.join("sub");
    std::fs::create_dir(&sub).unwrap();
    let body = serde_json::json!({"repo": s(&sub), "path": "a[1].txt"});
    write(&repo, "a[1].txt", "before\r\n<<<<<<< ours\r\none\r\n=======\r\ntwo\r\n>>>>>>> theirs\r\nafter\r\n");
    let value: serde_json::Value = f.ok("conflict", &body).await;
    assert_eq!(value["blocks"], serde_json::json!([
        {"kind":"common", "text":"before\r\n"},
        {"kind":"conflict", "ours":"one\r\n", "theirs":"two\r\n", "ours_label":"ours", "theirs_label":"theirs"},
        {"kind":"common", "text":"after\r\n"}
    ]));
    write(&repo, "a[1].txt", "before\n<<<<<<< ours\none\n=======\ntwo");
    let value: serde_json::Value = f.ok("conflict", &body).await;
    assert_eq!(value["blocks"], serde_json::json!([{"kind":"common", "text":"before\n<<<<<<< ours\none\n=======\ntwo"}]));
    std::fs::remove_file(repo.join("a[1].txt")).unwrap();
    let msg = f.err("conflict", &body, 409, "conflict").await;
    assert!(msg.chars().any(|c| ('\u{AC00}'..='\u{D7A3}').contains(&c)));
    let resolve = serde_json::json!({"repo": s(&sub), "path": "a[1].txt", "content": "resolved\n"});
    f.no_content("resolve", &resolve).await;
    assert_eq!(std::fs::read_to_string(repo.join("a[1].txt")).unwrap(), "resolved\n");
    std::fs::remove_file(repo.join("a[1].txt")).unwrap();
    f.err("resolve", &resolve, 409, "conflict").await;
    assert!(!repo.join("a[1].txt").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn fr_g7_rejects_symlink_at_unmerged_path() {
    let f = start().await;
    let repo = f.root.join("r");
    merge_conflict(&repo, false);
    let outside = f.root.join("outside");
    std::fs::write(&outside, "untouched").unwrap();
    std::fs::remove_file(repo.join("a[1].txt")).unwrap();
    std::os::unix::fs::symlink(&outside, repo.join("a[1].txt")).unwrap();
    let body = serde_json::json!({"repo": s(&repo), "path": "a[1].txt", "content": "changed"});
    for op in ["conflict", "resolve"] {
        f.err(op, &body, 400, "bad_request").await;
    }
    assert_eq!(std::fs::read_to_string(outside).unwrap(), "untouched");
}

#[tokio::test]
async fn fr_g7_delete_modify_missing_file() {
    let f = start().await;
    let repo = f.root.join("r");
    init_repo(&repo);
    write(&repo, "file", "base\n");
    commit_all(&repo, "base");
    git(&repo, &["switch", "-q", "-c", "side"]);
    write(&repo, "file", "modified\n");
    commit_all(&repo, "modify");
    git(&repo, &["switch", "-q", "main"]);
    git(&repo, &["rm", "-q", "file"]);
    commit_all(&repo, "delete");
    let output = Command::new("git").args(["merge", "side"]).current_dir(&repo).output().unwrap();
    assert!(!output.status.success());
    std::fs::remove_file(repo.join("file")).unwrap();
    let body = serde_json::json!({"repo": s(&repo), "path": "file", "content": "resolved\n"});
    let msg = f.err("conflict", &body, 409, "conflict").await;
    assert!(msg.chars().any(|c| ('\u{AC00}'..='\u{D7A3}').contains(&c)));
    f.no_content("resolve", &body).await;
}
