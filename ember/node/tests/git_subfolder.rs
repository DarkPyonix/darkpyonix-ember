//! The git API for a directory below the work tree root (SPEC FR-L31.9): `/git/toplevel` names
//! the root and the directory's path below it, and `diff`, `stage`, `unstage` and `apply` take
//! root-relative paths (as `status` gives them) for files inside and outside that directory.

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
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/git-subfolder-tests");
    std::fs::create_dir_all(&p).unwrap();
    p.canonicalize().unwrap()
}

async fn start() -> Fixture {
    static ONCE: Once = Once::new();
    // Stop git's upward search at the scratch directory, and keep machine config out.
    ONCE.call_once(|| {
        std::env::set_var("GIT_CEILING_DIRECTORIES", scratch());
        std::env::set_var("GIT_CONFIG_GLOBAL", "/dev/null");
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
    });
    let dir = tempfile::Builder::new().prefix("s").tempdir_in(scratch()).unwrap();
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

fn write(dir: &Path, name: &str, text: &str) {
    let p = dir.join(name);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn s(p: &Path) -> String {
    p.to_str().unwrap().to_string()
}

/// A repository `r` with `sub/f.txt` and `other/g.txt` committed, then both changed and
/// `other/n.txt` added untracked. Returns the repository and its `sub` directory.
fn repo_with_changes(root: &Path) -> (PathBuf, PathBuf) {
    let repo = root.join("r");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Tester"]);
    git(&repo, &["config", "user.email", "tester@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    write(&repo, "sub/f.txt", "a\nb\n");
    write(&repo, "other/g.txt", "x\ny\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    write(&repo, "sub/f.txt", "a\nB\n");
    write(&repo, "other/g.txt", "x\nY\n");
    write(&repo, "other/n.txt", "new\n");
    let sub = repo.join("sub");
    (repo, sub)
}

impl Fixture {
    async fn raw(&self, op: &str, body: &impl Serialize) -> (u16, String) {
        let r = self.http.post(format!("{}/git/{op}", self.base)).bearer_auth(TOKEN).json(body).send().await.unwrap();
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

    async fn diff(&self, repo: &Path, path: Option<&str>, staged: bool, include_untracked: bool) -> String {
        let d: GitDiff = self
            .ok("diff", &GitDiffRequest { repo: s(repo), path: path.map(str::to_string), staged, include_untracked })
            .await;
        d.patch
    }

    async fn paths(&self, op: &str, repo: &Path, paths: &[&str]) {
        let body = GitPathsRequest { repo: s(repo), paths: paths.iter().map(|p| p.to_string()).collect() };
        self.no_content(op, &body).await;
    }
}

fn staged_names(repo: &Path) -> Vec<String> {
    git(repo, &["diff", "--cached", "--name-only"]).lines().map(str::to_string).collect()
}

#[tokio::test]
async fn fr_l31_9_toplevel_names_the_root_and_the_prefix() {
    let f = start().await;
    let (repo, sub) = repo_with_changes(&f.root);
    let t: GitToplevel = f.ok("toplevel", &GitRepoRequest { repo: s(&sub) }).await;
    assert_eq!(Path::new(&t.toplevel).canonicalize().unwrap(), repo.canonicalize().unwrap());
    assert_eq!(t.prefix, "sub");

    std::fs::create_dir_all(sub.join("deep/er")).unwrap();
    let t: GitToplevel = f.ok("toplevel", &GitRepoRequest { repo: s(&sub.join("deep/er")) }).await;
    assert_eq!(t.prefix, "sub/deep/er");

    let t: GitToplevel = f.ok("toplevel", &GitRepoRequest { repo: s(&repo) }).await;
    assert_eq!(t.prefix, "");

    let plain = f.root.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let (status, text) = f.raw("toplevel", &GitRepoRequest { repo: s(&plain) }).await;
    assert_eq!(status, 404, "{text}");
}

#[tokio::test]
async fn fr_l31_9_status_from_a_subfolder_lists_every_change_by_root_path() {
    let f = start().await;
    let (_repo, sub) = repo_with_changes(&f.root);
    let st: GitStatus = f.ok("status", &GitRepoRequest { repo: s(&sub) }).await;
    let mut paths: Vec<&str> = st.files.iter().map(|f| f.path.as_str()).collect();
    paths.sort();
    assert_eq!(paths, vec!["other/g.txt", "other/n.txt", "sub/f.txt"]);
}

#[tokio::test]
async fn fr_l31_9_diff_from_a_subfolder_takes_root_paths_inside_and_outside() {
    let f = start().await;
    let (_repo, sub) = repo_with_changes(&f.root);
    let inside = f.diff(&sub, Some("sub/f.txt"), false, false).await;
    assert!(inside.contains("+++ b/sub/f.txt") && inside.contains("+B"), "{inside}");
    let outside = f.diff(&sub, Some("other/g.txt"), false, false).await;
    assert!(outside.contains("+++ b/other/g.txt") && outside.contains("+Y"), "{outside}");
    let untracked = f.diff(&sub, Some("other/n.txt"), false, false).await;
    assert!(untracked.contains("b/other/n.txt") && untracked.contains("+new"), "{untracked}");
    // The whole tree with the untracked files names each by its root path too.
    let all = f.diff(&sub, None, false, true).await;
    for p in ["b/sub/f.txt", "b/other/g.txt", "b/other/n.txt"] {
        assert!(all.contains(p), "{p} missing in {all}");
    }
}

#[tokio::test]
async fn fr_l31_9_stage_and_unstage_from_a_subfolder_take_root_paths() {
    let f = start().await;
    let (repo, sub) = repo_with_changes(&f.root);
    f.paths("stage", &sub, &["sub/f.txt", "other/g.txt", "other/n.txt"]).await;
    assert_eq!(staged_names(&repo), vec!["other/g.txt", "other/n.txt", "sub/f.txt"]);
    f.paths("unstage", &sub, &["other/g.txt"]).await;
    assert_eq!(staged_names(&repo), vec!["other/n.txt", "sub/f.txt"]);
}

#[tokio::test]
async fn fr_l31_9_apply_from_a_subfolder_stages_a_file_outside_it() {
    let f = start().await;
    let (repo, sub) = repo_with_changes(&f.root);
    // `git apply --cached` run in `sub` would skip this patch and still exit 0.
    let patch = f.diff(&sub, Some("other/g.txt"), false, false).await;
    f.no_content("apply", &GitApplyRequest { repo: s(&sub), patch: patch.clone(), reverse: false }).await;
    assert_eq!(staged_names(&repo), vec!["other/g.txt"]);
    let staged = f.diff(&sub, Some("other/g.txt"), true, false).await;
    f.no_content("apply", &GitApplyRequest { repo: s(&sub), patch: staged, reverse: true }).await;
    assert!(staged_names(&repo).is_empty());

    let inside = f.diff(&sub, Some("sub/f.txt"), false, false).await;
    f.no_content("apply", &GitApplyRequest { repo: s(&sub), patch: inside, reverse: false }).await;
    assert_eq!(staged_names(&repo), vec!["sub/f.txt"]);
}
