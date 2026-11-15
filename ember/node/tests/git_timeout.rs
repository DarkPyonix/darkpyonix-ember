//! Local git timeout, hook deadline, SIGTERM-to-SIGKILL escalation, and leftover `.git/index.lock`
//! reporting (SPEC FR-G2, #641).
//!
//! Runs in its own test binary so the shortened local, hook and grace timeouts do not affect the
//! shared `git_api` suite.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;

mod common;

use ember_node::api::{self, Node};
use ember_node::config::NodeConfig;
use ember_node::git::*;
use serde::de::DeserializeOwned;
use serde::Serialize;

const TOKEN: &str = "test-token";

/// Serialize tests in this binary because they temporarily override `EMBER_GIT_HOOK_TIMEOUT_SECS`.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Fixture {
    base: String,
    root: PathBuf,
    http: reqwest::Client,
    _dir: tempfile::TempDir,
}

fn scratch() -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/git-timeout-tests");
    std::fs::create_dir_all(&p).unwrap();
    p.canonicalize().unwrap()
}

async fn start() -> Fixture {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        std::env::set_var("GIT_CEILING_DIRECTORIES", scratch());
        std::env::set_var("GIT_CONFIG_GLOBAL", "/dev/null");
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
        std::env::set_var("EMBER_GIT_LOCAL_TIMEOUT_SECS", "1");
        std::env::set_var("EMBER_GIT_HOOK_TIMEOUT_SECS", "2");
        std::env::set_var("EMBER_GIT_KILL_GRACE_MS", "300");
    });
    let dir = tempfile::Builder::new().prefix("gt").tempdir_in(scratch()).unwrap();
    let root = dir.path().canonicalize().unwrap();
    let node = Node::new(NodeConfig::new(TOKEN, vec![root.clone()])).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node));
    Fixture { base, root, http: reqwest::Client::new(), _dir: dir }
}

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

fn one_commit(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.name", "Tester"]);
    git(dir, &["config", "user.email", "tester@example.com"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    write(dir, "a.txt", "one\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "first"]);
}

fn write(dir: &Path, name: &str, text: &str) {
    let p = dir.join(name);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn s(p: &Path) -> String {
    p.to_str().unwrap().to_string()
}

fn pid_alive(pid: &str) -> bool {
    Command::new("kill").args(["-0", pid]).output().map(|o| o.status.success()).unwrap_or(false)
}

async fn wait_gone(pid: &str) -> bool {
    for _ in 0..50 {
        if !pid_alive(pid) {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    false
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

    async fn err(&self, op: &str, body: &impl Serialize, status: u16, code: &str) -> String {
        let (st, text) = self.raw(op, body).await;
        assert_eq!(st, status, "{op}: {text}");
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["code"], code, "{text}");
        v["error"].as_str().unwrap().to_string()
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

#[tokio::test]
async fn fr_g2_hook_slower_than_short_local_deadline_still_commits() {
    let f = start().await;
    let repo = f.root.join("r");
    one_commit(&repo);
    let hook = repo.join(".git/hooks/pre-commit");
    let marker = f.root.join("hook-ran.txt");
    common::write_exec_ready(&hook, &format!("#!/bin/sh\nsleep 2\necho ran > '{}'\nexit 0\n", s(&marker)));

    write(&repo, "a.txt", "one\ntwo\n");
    f.stage(&repo, &["a.txt"]).await;

    let committed = {
        let _guard = ENV_LOCK.lock().await;
        std::env::set_var("EMBER_GIT_LOCAL_TIMEOUT_SECS", "1");
        std::env::set_var("EMBER_GIT_HOOK_TIMEOUT_SECS", "5");
        let res = f.commit(&repo, "with-slow-hook").await;
        std::env::set_var("EMBER_GIT_HOOK_TIMEOUT_SECS", "2");
        res
    };
    assert_eq!(committed.sha, git(&repo, &["rev-parse", "HEAD"]).trim());
    assert!(marker.exists(), "the pre-commit hook ran to completion");
}

#[tokio::test]
async fn fr_g2_hung_gpg_signer_ends_by_sigterm_with_no_index_lock_left() {
    let f = start().await;
    let repo = f.root.join("r");
    one_commit(&repo);
    let script = f.root.join("slow-gpg.sh");
    let record = f.root.join("gpg-pid.txt");
    let term_marker = f.root.join("gpg-term.txt");
    common::write_exec_ready(
        &script,
        &format!(
            "#!/bin/sh\ntrap 'echo term > \"{}\"; exit 0' TERM\necho $$ > '{}'\nwhile :; do sleep 1; done\n",
            s(&term_marker),
            s(&record)
        ),
    );
    git(&repo, &["config", "gpg.program", &s(&script)]);
    git(&repo, &["config", "commit.gpgsign", "true"]);
    git(&repo, &["config", "user.signingkey", "deadbeef"]);

    write(&repo, "a.txt", "one\ntwo\n");
    f.stage(&repo, &["a.txt"]).await;

    let (msg, elapsed) = {
        let _guard = ENV_LOCK.lock().await;
        std::env::set_var("EMBER_GIT_HOOK_TIMEOUT_SECS", "1");
        std::env::set_var("EMBER_GIT_KILL_GRACE_MS", "2000");
        let t = std::time::Instant::now();
        let msg = f
            .err(
                "commit",
                &GitCommitRequest { repo: s(&repo), summary: "signed".into(), description: None, amend: false },
                504,
                "timeout",
            )
            .await;
        let elapsed = t.elapsed();
        std::env::set_var("EMBER_GIT_HOOK_TIMEOUT_SECS", "2");
        std::env::set_var("EMBER_GIT_KILL_GRACE_MS", "300");
        (msg, elapsed)
    };
    assert!(msg.contains("GPG 서명"), "{msg}");
    assert!(!msg.contains("index.lock"), "{msg}");
    // Ended by SIGTERM well before the 2 s grace period elapsed.
    assert!(elapsed < std::time::Duration::from_millis(2500), "elapsed: {elapsed:?}");

    let pid = std::fs::read_to_string(&record).expect("the gpg stand-in ran").trim().to_string();
    assert!(wait_gone(&pid).await, "gpg child {pid} still runs after SIGTERM");
    assert!(term_marker.exists(), "gpg stand-in did not receive SIGTERM");
    assert!(!repo.join(".git/index.lock").exists(), ".git/index.lock should not remain after SIGTERM");

    // The write lock was released on timeout, so turning signing off allows the commit to succeed.
    git(&repo, &["config", "commit.gpgsign", "false"]);
    let committed = {
        let _guard = ENV_LOCK.lock().await;
        f.commit(&repo, "unsigned").await
    };
    assert_eq!(committed.sha, git(&repo, &["rev-parse", "HEAD"]).trim());
}

#[tokio::test]
async fn fr_g2_process_ignoring_sigterm_is_killed_after_grace() {
    let f = start().await;
    let repo = f.root.join("r");
    one_commit(&repo);
    let script = f.root.join("stubborn-gpg.sh");
    let record = f.root.join("stubborn-pid.txt");
    common::write_exec_ready(
        &script,
        &format!("#!/bin/sh\ntrap '' TERM\necho $$ > '{}'\nwhile :; do sleep 1; done\n", s(&record)),
    );
    git(&repo, &["config", "gpg.program", &s(&script)]);
    git(&repo, &["config", "commit.gpgsign", "true"]);
    git(&repo, &["config", "user.signingkey", "deadbeef"]);

    write(&repo, "a.txt", "one\ntwo\n");
    f.stage(&repo, &["a.txt"]).await;

    let (msg, elapsed) = {
        let _guard = ENV_LOCK.lock().await;
        std::env::set_var("EMBER_GIT_HOOK_TIMEOUT_SECS", "1");
        std::env::set_var("EMBER_GIT_KILL_GRACE_MS", "300");
        let t = std::time::Instant::now();
        let msg = f
            .err(
                "commit",
                &GitCommitRequest { repo: s(&repo), summary: "signed".into(), description: None, amend: false },
                504,
                "timeout",
            )
            .await;
        let elapsed = t.elapsed();
        std::env::set_var("EMBER_GIT_HOOK_TIMEOUT_SECS", "2");
        (msg, elapsed)
    };
    assert!(msg.contains("중단했습니다"), "{msg}");
    // Must have waited at least the 1 s deadline + 300 ms grace period before SIGKILL.
    assert!(elapsed >= std::time::Duration::from_millis(1200), "elapsed: {elapsed:?}");
    assert!(elapsed < std::time::Duration::from_secs(10), "elapsed: {elapsed:?}");

    let pid = std::fs::read_to_string(&record).expect("the stubborn gpg stand-in ran").trim().to_string();
    assert!(wait_gone(&pid).await, "stubborn gpg child {pid} still runs after SIGKILL");
}

#[tokio::test]
async fn fr_g2_leftover_index_lock_after_timeout_is_named_in_error_and_not_deleted() {
    let f = start().await;
    let repo = f.root.join("r");
    one_commit(&repo);
    let lock_path = repo.join(".git/index.lock");
    let hook = repo.join(".git/hooks/pre-commit");
    common::write_exec_ready(
        &hook,
        &format!("#!/bin/sh\necho locked > '{}'\nsleep 300\n", s(&lock_path)),
    );

    write(&repo, "a.txt", "one\ntwo\n");
    f.stage(&repo, &["a.txt"]).await;

    let msg = {
        let _guard = ENV_LOCK.lock().await;
        std::env::set_var("EMBER_GIT_HOOK_TIMEOUT_SECS", "1");
        std::env::set_var("EMBER_GIT_KILL_GRACE_MS", "200");
        let msg = f
            .err(
                "commit",
                &GitCommitRequest { repo: s(&repo), summary: "lock-left".into(), description: None, amend: false },
                504,
                "timeout",
            )
            .await;
        std::env::set_var("EMBER_GIT_HOOK_TIMEOUT_SECS", "2");
        std::env::set_var("EMBER_GIT_KILL_GRACE_MS", "300");
        msg
    };
    assert!(msg.contains("index.lock"), "expected index.lock in timeout error: {msg}");
    assert!(lock_path.exists(), ".git/index.lock must be reported and not deleted");
}
