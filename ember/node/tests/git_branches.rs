//! FR-G9 branch list over HTTP, on repositories under `.scratch/` with a local bare remote.
use std::{path::{Path, PathBuf}, process::Command};
use ember_node::{api::{self, Node}, config::NodeConfig};
use serde_json::{json, Value};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C").output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn identify(dir: &Path) {
    git(dir, &["config", "user.name", "Tester"]);
    git(dir, &["config", "user.email", "tester@example.com"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

fn commit(dir: &Path, file: &str, subject: &str) {
    std::fs::write(dir.join(file), subject).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", subject]);
}

struct Fx { _dir: tempfile::TempDir, root: PathBuf, base: String, client: reqwest::Client }

async fn serve(root: &Path) -> (String, reqwest::Client) {
    let node = Node::new(NodeConfig::new("token", vec![root.to_path_buf()])).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node));
    (base, reqwest::Client::new())
}

async fn fixture() -> Fx {
    let scratch = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/git-branches-tests");
    std::fs::create_dir_all(&scratch).unwrap();
    let dir = tempfile::Builder::new().prefix("b").tempdir_in(scratch).unwrap();
    let root = dir.path().canonicalize().unwrap();
    let bare = root.join("remote.git");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "-q", "--bare", "-b", "main"]);
    let seed = root.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-q", "-b", "main"]);
    identify(&seed);
    commit(&seed, "a", "first");
    git(&seed, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git(&seed, &["push", "-q", "-u", "origin", "main"]);
    for name in ["work", "other"] {
        git(&root, &["clone", "-q", bare.to_str().unwrap(), name]);
        identify(&root.join(name));
    }
    let (base, client) = serve(&root).await;
    Fx { _dir: dir, root, base, client }
}

async fn branches(fx: &Fx, repo: &Path) -> (u16, Value) {
    let r = fx.client.post(format!("{}/git/branches", fx.base)).bearer_auth("token").json(&json!({"repo": repo})).send().await.unwrap();
    (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
}

fn by_name<'a>(list: &'a Value, name: &str) -> &'a Value {
    list["branches"].as_array().unwrap().iter().find(|b| b["name"] == name).unwrap_or_else(|| panic!("no {name} in {list}"))
}

#[tokio::test]
async fn fr_g9_ahead_behind_upstream_default_and_worktree() {
    let fx = fixture().await;
    let work = fx.root.join("work");
    let other = fx.root.join("other");
    // feat-a: pushed once, then 2 local commits ahead and 1 remote commit behind.
    git(&work, &["checkout", "-q", "-b", "feat-a"]);
    commit(&work, "fa", "feat a one");
    git(&work, &["push", "-q", "-u", "origin", "feat-a"]);
    git(&other, &["fetch", "-q"]);
    git(&other, &["checkout", "-q", "-b", "feat-a", "origin/feat-a"]);
    commit(&other, "fo", "remote only");
    git(&other, &["push", "-q", "origin", "feat-a"]);
    commit(&work, "l1", "local one");
    commit(&work, "l2", "local two | with pipe");
    git(&work, &["fetch", "-q"]);
    // feat-b: only behind (nothing local), tracked.
    git(&work, &["branch", "--track", "feat-b", "origin/main"]);
    // solo: no upstream, checked out in a linked worktree.
    let wt = fx.root.join("wt-solo");
    git(&work, &["worktree", "add", "-q", "-b", "solo", wt.to_str().unwrap(), "main"]);
    let (status, list) = branches(&fx, &work).await;
    assert_eq!(status, 200, "{list}");
    let a = by_name(&list, "feat-a");
    assert_eq!(a["is_current"], true);
    assert_eq!(a["upstream"], "origin/feat-a");
    assert_eq!((a["ahead"].as_u64(), a["behind"].as_u64()), (Some(2), Some(1)));
    assert_eq!(a["last_subject"], "local two | with pipe");
    assert!(!a["last_sha"].as_str().unwrap().is_empty());
    assert!(a["last_time"].as_str().unwrap().contains('T'));
    assert_eq!(a["worktree"], work.to_str().unwrap());
    let b = by_name(&list, "feat-b");
    assert_eq!((b["ahead"].as_u64(), b["behind"].as_u64()), (Some(0), Some(0)));
    assert_eq!(b["upstream"], "origin/main");
    assert!(b["worktree"].is_null());
    let m = by_name(&list, "main");
    assert_eq!((m["is_default"].clone(), m["is_current"].clone()), (json!(true), json!(false)));
    let solo = by_name(&list, "solo");
    assert!(solo["upstream"].is_null());
    assert_eq!((solo["ahead"].as_u64(), solo["behind"].as_u64()), (Some(0), Some(0)));
    assert_eq!(solo["worktree"], wt.canonicalize().unwrap().to_str().unwrap());
    // Current first, then the default, then the rest.
    let names: Vec<&str> = list["branches"].as_array().unwrap().iter().map(|b| b["name"].as_str().unwrap()).collect();
    assert_eq!(&names[..2], ["feat-a", "main"]);
    assert_eq!(names.len(), 4);
}

#[tokio::test]
async fn fr_g9_a_gone_upstream_reads_as_no_divergence() {
    let fx = fixture().await;
    let work = fx.root.join("work");
    git(&work, &["checkout", "-q", "-b", "temp"]);
    commit(&work, "t", "temp");
    git(&work, &["push", "-q", "-u", "origin", "temp"]);
    git(&work, &["push", "-q", "origin", "--delete", "temp"]);
    git(&work, &["fetch", "-q", "--prune"]);
    let (status, list) = branches(&fx, &work).await;
    assert_eq!(status, 200, "{list}");
    let t = by_name(&list, "temp");
    assert_eq!((t["ahead"].as_u64(), t["behind"].as_u64()), (Some(0), Some(0)));
}

#[tokio::test]
async fn fr_g9_without_a_remote_the_default_is_main_or_master() {
    let fx = fixture().await;
    let solo = fx.root.join("solo-repo");
    std::fs::create_dir_all(&solo).unwrap();
    git(&solo, &["init", "-q", "-b", "master"]);
    identify(&solo);
    commit(&solo, "a", "one");
    git(&solo, &["checkout", "-q", "-b", "topic"]);
    let (status, list) = branches(&fx, &solo).await;
    assert_eq!(status, 200, "{list}");
    assert_eq!(by_name(&list, "master")["is_default"], true);
    assert_eq!(by_name(&list, "topic")["is_default"], false);
    assert!(by_name(&list, "topic")["upstream"].is_null());
    assert_eq!(list["branches"][0]["name"], "topic");
}

#[tokio::test]
async fn fr_g9_a_plain_directory_is_404_not_a_repo() {
    let fx = fixture().await;
    let plain = fx.root.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let (status, body) = branches(&fx, &plain).await;
    assert_eq!(status, 404);
    assert_eq!(body["error"], "not_a_repo");
}
