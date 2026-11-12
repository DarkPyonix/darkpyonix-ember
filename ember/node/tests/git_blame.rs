//! FR-L31.8 blame of one line over HTTP, using repositories under `.scratch/`.
use std::path::{Path, PathBuf};
use std::process::Command;
use ember_node::git::git_blame::{check_path, parse_porcelain};
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
    let scratch = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/git-blame-tests");
    std::fs::create_dir_all(&scratch).unwrap();
    let dir = tempfile::Builder::new().prefix("g").tempdir_in(scratch).unwrap();
    let repo = dir.path().canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Tester"]);
    git(&repo, &["config", "user.email", "tester@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    let node = Node::new(NodeConfig::new("token", vec![repo.clone()])).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/git/blame", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node));
    (dir, repo, url, reqwest::Client::new())
}

fn commit(repo: &Path, file: &str, text: &str, subject: &str) -> String {
    if let Some(parent) = repo.join(file).parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(repo.join(file), text).unwrap();
    git(repo, &["add", file]);
    git(repo, &["commit", "-q", "-m", subject]);
    git(repo, &["rev-parse", "HEAD"])
}

async fn post(client: &reqwest::Client, url: &str, body: Value) -> (u16, Value) {
    let response = client.post(url).bearer_auth("token").json(&body).send().await.unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

#[test]
fn fr_l31_8_porcelain_is_parsed() {
    let sha = "1234567890abcdef1234567890abcdef12345678";
    let text = format!(
        "{sha} 2 2 1\nauthor Kim Minji\nauthor-mail <minji@example.com>\nauthor-time 1700000000\nauthor-tz +0900\ncommitter Kim Minji\ncommitter-mail <minji@example.com>\ncommitter-time 1700000100\ncommitter-tz +0900\nsummary Fix: the parser keeps spaces\nprevious abcdef1234567890abcdef1234567890abcdef12 src/lib.rs\nfilename src/lib.rs\n\tlet x = 1;\n"
    );
    let c = parse_porcelain(&text).unwrap().unwrap();
    assert_eq!(c.sha, sha);
    assert_eq!(c.author, "Kim Minji");
    assert_eq!(c.email, "minji@example.com");
    assert_eq!(c.time, 1_700_000_000);
    assert_eq!(c.summary, "Fix: the parser keeps spaces");
    // The line itself may look like a header: it comes after the tab and is not read.
    let tricky = format!("{sha} 1 1 1\nauthor A\nauthor-mail <a@b>\nauthor-time 5\nsummary s\nfilename f\n\tauthor Mallory\n");
    assert_eq!(parse_porcelain(&tricky).unwrap().unwrap().author, "A");
}

#[test]
fn fr_l31_8_zero_sha_is_not_committed() {
    let text = "0000000000000000000000000000000000000000 3 3 1\nauthor Not Committed Yet\nauthor-mail <not.committed.yet>\nauthor-time 1700000000\nauthor-tz +0000\nsummary Version of a.txt from a.txt\nfilename a.txt\n\tnew\n";
    assert_eq!(parse_porcelain(text).unwrap(), None);
}

#[test]
fn fr_l31_8_malformed_porcelain_is_an_error() {
    assert!(parse_porcelain("").is_err());
    assert!(parse_porcelain("not-a-sha 1 1 1\n").is_err());
    let no_time = "1234567890abcdef1234567890abcdef12345678 1 1 1\nauthor A\nsummary s\n\tx\n";
    assert!(parse_porcelain(no_time).is_err());
}

#[test]
fn fr_l31_8_path_must_stay_inside_the_work_tree() {
    assert!(check_path("src/lib.rs").is_ok());
    assert!(check_path("a.txt").is_ok());
    for bad in ["", "../outside.txt", "src/../../x", "/etc/passwd", "./a.txt", "a\0b"] {
        assert!(check_path(bad).is_err(), "{bad:?}");
    }
}

#[tokio::test]
async fn fr_l31_8_committed_line_names_its_commit() {
    let (_dir, repo, url, client) = setup().await;
    let first = commit(&repo, "src/a.txt", "one\ntwo\n", "first");
    let second = commit(&repo, "src/a.txt", "one\nTWO\n", "second: change line two");
    let (status, body) = post(&client, &url, json!({"repo": repo, "path": "src/a.txt", "line": 1})).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["line"], 1);
    assert_eq!(body["commit"]["sha"], first);
    assert_eq!(body["commit"]["author"], "Tester");
    assert_eq!(body["commit"]["email"], "tester@example.com");
    assert_eq!(body["commit"]["summary"], "first");
    assert!(body["commit"]["time"].as_i64().unwrap() > 0);
    let (_, body) = post(&client, &url, json!({"repo": repo, "path": "src/a.txt", "line": 2})).await;
    assert_eq!(body["commit"]["sha"], second);
    assert_eq!(body["commit"]["summary"], "second: change line two");
}

#[tokio::test]
async fn fr_l31_8_saved_and_unsaved_edits_are_not_committed() {
    let (_dir, repo, url, client) = setup().await;
    let first = commit(&repo, "a.txt", "one\ntwo\n", "first");
    // An edit saved to disk and not committed.
    std::fs::write(repo.join("a.txt"), "one\nedited\n").unwrap();
    let (status, body) = post(&client, &url, json!({"repo": repo, "path": "a.txt", "line": 2})).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, json!({"line": 2, "commit": null}));
    // The editor's unsaved buffer: a line inserted above shifts the committed line down.
    let contents = "inserted\none\nedited\n";
    let (_, body) = post(&client, &url, json!({"repo": repo, "path": "a.txt", "line": 1, "contents": contents})).await;
    assert_eq!(body["commit"], Value::Null);
    let (_, body) = post(&client, &url, json!({"repo": repo, "path": "a.txt", "line": 2, "contents": contents})).await;
    assert_eq!(body["commit"]["sha"], first);
}

#[tokio::test]
async fn fr_l31_8_untracked_file_and_empty_repository_are_not_committed() {
    let (_dir, repo, url, client) = setup().await;
    std::fs::write(repo.join("new.txt"), "x\n").unwrap();
    let (status, body) = post(&client, &url, json!({"repo": repo, "path": "new.txt", "line": 1})).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["commit"], Value::Null);
    commit(&repo, "a.txt", "a\n", "first");
    let (status, body) = post(&client, &url, json!({"repo": repo, "path": "new.txt", "line": 1})).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["commit"], Value::Null);
}

#[tokio::test]
async fn fr_l31_8_rejects_bad_paths_and_lines() {
    let (_dir, repo, url, client) = setup().await;
    commit(&repo, "a.txt", "a\nb\n", "first");
    for body in [
        json!({"repo": repo, "path": "../a.txt", "line": 1}),
        json!({"repo": repo, "path": "/etc/passwd", "line": 1}),
        json!({"repo": repo, "path": "a.txt", "line": 0}),
        json!({"repo": repo, "path": "a.txt", "line": 99}),
    ] {
        let (status, out) = post(&client, &url, body.clone()).await;
        assert_eq!(status, 400, "{body} -> {out}");
    }
}
