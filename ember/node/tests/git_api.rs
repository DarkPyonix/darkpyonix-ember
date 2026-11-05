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

fn file<'a>(st: &'a GitStatus, path: &str) -> &'a GitFile {
    st.files.iter().find(|f| f.path == path).unwrap_or_else(|| panic!("{path} not in {:?}", st.files))
}

fn numbered(changes: &[(usize, &str)]) -> String {
    (1..=40)
        .map(|i| match changes.iter().find(|(n, _)| *n == i) {
            Some((_, t)) => format!("{t}\n"),
            None => format!("line {i}\n"),
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// FR-G1

#[tokio::test]
async fn fr_g1_status_of_a_clean_repo() {
    let f = start().await;
    let repo = f.root.join("r");
    init_repo(&repo);
    write(&repo, "a.txt", "one\n");
    commit_all(&repo, "first");

    let st = f.status(&repo).await;
    assert_eq!(st.branch, "main");
    assert_eq!(st.upstream, None);
    assert_eq!((st.ahead, st.behind), (0, 0));
    assert_eq!(st.branches, vec!["main".to_string()]);
    assert!(st.files.is_empty(), "{:?}", st.files);
    assert_eq!(st.fetched_age_secs, None);
}

#[tokio::test]
async fn fr_g1_status_lists_modified_added_untracked_and_renamed_files() {
    let f = start().await;
    let repo = f.root.join("r");
    init_repo(&repo);
    write(&repo, "mod.txt", "one\ntwo\n");
    write(&repo, "old.txt", "this file will be renamed\nwith some content\n");
    commit_all(&repo, "first");

    write(&repo, "mod.txt", "one\ntwo\nthree\n");
    write(&repo, "added.txt", "new\n");
    git(&repo, &["add", "added.txt"]);
    write(&repo, "new/untracked.txt", "u\n");
    git(&repo, &["mv", "old.txt", "renamed.txt"]);

    let st = f.status(&repo).await;
    let m = file(&st, "mod.txt");
    assert_eq!((m.index, m.worktree), ('.', 'M'));
    let a = file(&st, "added.txt");
    assert_eq!((a.index, a.worktree), ('A', '.'));
    let u = file(&st, "new/untracked.txt");
    assert_eq!((u.index, u.worktree), ('?', '?'));
    let r = file(&st, "renamed.txt");
    assert_eq!(r.index, 'R');
    assert_eq!(r.old_path.as_deref(), Some("old.txt"));
    assert_eq!(st.files.len(), 4, "{:?}", st.files);
}

#[tokio::test]
async fn fr_g1_a_plain_directory_is_not_a_repo() {
    let f = start().await;
    let dir = f.root.join("plain");
    std::fs::create_dir_all(&dir).unwrap();
    for op in ["status", "conflicts"] {
        let msg = f.err(op, &GitRepoRequest { repo: s(&dir) }, 404, "not_a_repo").await;
        assert!(!msg.is_empty());
    }
    f.err("diff", &GitDiffRequest { repo: s(&dir), path: None, staged: false, include_untracked: false }, 404, "not_a_repo").await;
}

#[tokio::test]
async fn fr_g1_repo_paths_obey_auth_and_the_root_policy() {
    let f = start().await;
    let repo = f.root.join("r");
    init_repo(&repo);

    let r = f
        .http
        .post(format!("{}/git/status", f.base))
        .json(&GitRepoRequest { repo: s(&repo) })
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);

    let outside = scratch();
    f.err("status", &GitRepoRequest { repo: s(&outside) }, 403, "forbidden_path").await;
    f.err("status", &GitRepoRequest { repo: "relative/path".into() }, 400, "bad_request").await;
}

#[tokio::test]
async fn fr_g1_diff_of_unstaged_staged_and_untracked_changes() {
    let f = start().await;
    let repo = f.root.join("r");
    init_repo(&repo);
    write(&repo, "a.txt", "one\ntwo\n");
    write(&repo, "b.txt", "bee\n");
    commit_all(&repo, "first");

    write(&repo, "a.txt", "one\nTWO\n");
    write(&repo, "b.txt", "bee\nbuzz\n");
    write(&repo, "u.txt", "fresh\n");

    let all = f.diff(&repo, None, false).await;
    assert!(all.contains("diff --git a/a.txt b/a.txt") && all.contains("+TWO"), "{all}");
    assert!(all.contains("diff --git a/b.txt b/b.txt"), "{all}");
    assert!(!all.contains("u.txt"), "untracked files are not in the whole-tree diff: {all}");

    let one = f.diff(&repo, Some("a.txt"), false).await;
    assert!(one.contains("a.txt") && !one.contains("b.txt"), "{one}");

    let untracked = f.diff(&repo, Some("u.txt"), false).await;
    assert!(untracked.contains("+fresh") && untracked.contains("new file mode"), "{untracked}");

    assert_eq!(f.diff(&repo, None, true).await, "");
    f.stage(&repo, &["a.txt"]).await;
    let staged = f.diff(&repo, None, true).await;
    assert!(staged.contains("+TWO") && !staged.contains("buzz"), "{staged}");
    assert!(!f.diff(&repo, Some("a.txt"), false).await.contains("TWO"));
}

// ---------------------------------------------------------------------------------------------
// FR-G2

#[tokio::test]
async fn fr_g2_stage_and_unstage_whole_files() {
    let f = start().await;
    let repo = f.root.join("r");
    init_repo(&repo);
    write(&repo, "a.txt", "one\n");
    commit_all(&repo, "first");
    write(&repo, "a.txt", "two\n");
    write(&repo, "b.txt", "bee\n");

    f.stage(&repo, &["a.txt", "b.txt"]).await;
    let st = f.status(&repo).await;
    assert_eq!(file(&st, "a.txt").index, 'M');
    assert_eq!(file(&st, "b.txt").index, 'A');

    let body = GitPathsRequest { repo: s(&repo), paths: vec!["a.txt".into(), "b.txt".into()] };
    f.no_content("unstage", &body).await;
    let st = f.status(&repo).await;
    assert_eq!((file(&st, "a.txt").index, file(&st, "a.txt").worktree), ('.', 'M'));
    assert_eq!(file(&st, "b.txt").index, '?');

    let empty = GitPathsRequest { repo: s(&repo), paths: vec![] };
    f.err("stage", &empty, 400, "bad_request").await;
    f.err("unstage", &empty, 400, "bad_request").await;
}

#[tokio::test]
async fn fr_g2_unstage_works_before_the_first_commit() {
    let f = start().await;
    let repo = f.root.join("r");
    init_repo(&repo);
    write(&repo, "a.txt", "one\n");
    f.stage(&repo, &["a.txt"]).await;
    assert_eq!(file(&f.status(&repo).await, "a.txt").index, 'A');
    f.no_content("unstage", &GitPathsRequest { repo: s(&repo), paths: vec!["a.txt".into()] }).await;
    assert_eq!(file(&f.status(&repo).await, "a.txt").index, '?');
}

/// The first hunk of a two-hunk diff, with the file header.
fn first_hunk(patch: &str) -> String {
    let start = patch.find("\n@@").expect("a hunk") + 1;
    let header = &patch[..start];
    let rest = &patch[start..];
    let end = rest[2..].find("\n@@").map(|i| i + 3).unwrap_or(rest.len());
    format!("{header}{}", &rest[..end])
}

#[tokio::test]
async fn fr_g2_a_hunk_applied_to_the_index_leaves_the_other_hunk_unstaged() {
    let f = start().await;
    let repo = f.root.join("r");
    init_repo(&repo);
    write(&repo, "a.txt", &numbered(&[]));
    commit_all(&repo, "first");
    write(&repo, "a.txt", &numbered(&[(2, "line two CHANGED"), (38, "line 38 CHANGED")]));

    let patch = f.diff(&repo, Some("a.txt"), false).await;
    assert_eq!(patch.matches("\n@@").count(), 2, "{patch}");
    let hunk = first_hunk(&patch);
    assert!(hunk.contains("line two CHANGED") && !hunk.contains("line 38 CHANGED"), "{hunk}");

    f.no_content("apply", &GitApplyRequest { repo: s(&repo), patch: hunk.clone(), reverse: false }).await;

    let staged = f.diff(&repo, None, true).await;
    assert!(staged.contains("line two CHANGED") && !staged.contains("line 38 CHANGED"), "{staged}");
    let unstaged = f.diff(&repo, Some("a.txt"), false).await;
    assert!(unstaged.contains("line 38 CHANGED") && !unstaged.contains("line two CHANGED"), "{unstaged}");
    let st = f.status(&repo).await;
    assert_eq!((file(&st, "a.txt").index, file(&st, "a.txt").worktree), ('M', 'M'));

    // The commit holds only the staged hunk.
    let c = f.commit(&repo, "stage one hunk").await;
    assert_eq!(c.sha, git(&repo, &["rev-parse", "HEAD"]).trim());
    let shown = git(&repo, &["show", "--format=", "HEAD"]);
    assert!(shown.contains("line two CHANGED") && !shown.contains("line 38 CHANGED"), "{shown}");
    assert!(f.diff(&repo, Some("a.txt"), false).await.contains("line 38 CHANGED"));
}

#[tokio::test]
async fn fr_g2_a_reverse_patch_unstages_a_hunk() {
    let f = start().await;
    let repo = f.root.join("r");
    init_repo(&repo);
    write(&repo, "a.txt", &numbered(&[]));
    commit_all(&repo, "first");
    write(&repo, "a.txt", &numbered(&[(2, "line two CHANGED"), (38, "line 38 CHANGED")]));
    f.stage(&repo, &["a.txt"]).await;

    let staged = f.diff(&repo, Some("a.txt"), true).await;
    let hunk = first_hunk(&staged);
    f.no_content("apply", &GitApplyRequest { repo: s(&repo), patch: hunk, reverse: true }).await;

    let staged = f.diff(&repo, None, true).await;
    assert!(staged.contains("line 38 CHANGED") && !staged.contains("line two CHANGED"), "{staged}");
}

#[tokio::test]
async fn fr_g2_a_patch_that_does_not_apply_is_refused() {
    let f = start().await;
    let repo = f.root.join("r");
    init_repo(&repo);
    write(&repo, "a.txt", "one\n");
    commit_all(&repo, "first");

    let bogus = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-nothing like this\n+x\n";
    f.err("apply", &GitApplyRequest { repo: s(&repo), patch: bogus.into(), reverse: false }, 409, "conflict").await;
    f.err("apply", &GitApplyRequest { repo: s(&repo), patch: "  ".into(), reverse: false }, 400, "bad_request").await;
}

#[tokio::test]
async fn fr_g2_commit_needs_a_staged_change_and_supports_description_and_amend() {
    let f = start().await;
    let repo = f.root.join("r");
    init_repo(&repo);
    write(&repo, "a.txt", "one\n");

    // Empty index.
    let msg = f
        .err(
            "commit",
            &GitCommitRequest { repo: s(&repo), summary: "x".into(), description: None, amend: false },
            409,
            "conflict",
        )
        .await;
    assert!(!msg.is_empty());
    f.err(
        "commit",
        &GitCommitRequest { repo: s(&repo), summary: "  ".into(), description: None, amend: false },
        400,
        "bad_request",
    )
    .await;

    f.stage(&repo, &["a.txt"]).await;
    let first: GitCommitResponse = f
        .ok(
            "commit",
            &GitCommitRequest {
                repo: s(&repo),
                summary: "Add a".into(),
                description: Some("Longer text.".into()),
                amend: false,
            },
        )
        .await;
    assert_eq!(git(&repo, &["log", "-1", "--format=%s"]).trim(), "Add a");
    assert_eq!(git(&repo, &["log", "-1", "--format=%b"]).trim(), "Longer text.");

    let amended: GitCommitResponse = f
        .ok(
            "commit",
            &GitCommitRequest { repo: s(&repo), summary: "Add a file".into(), description: None, amend: true },
        )
        .await;
    assert_ne!(first.sha, amended.sha);
    assert_eq!(git(&repo, &["rev-list", "--count", "HEAD"]).trim(), "1");
    assert_eq!(git(&repo, &["log", "-1", "--format=%s"]).trim(), "Add a file");
}

// ---------------------------------------------------------------------------------------------
// FR-G3

#[tokio::test]
async fn fr_g3_branches_are_created_and_switched() {
    let f = start().await;
    let repo = f.root.join("r");
    init_repo(&repo);
    write(&repo, "a.txt", "one\n");
    commit_all(&repo, "first");

    f.no_content("branch", &GitBranchRequest { repo: s(&repo), name: "feat/x".into(), from: None }).await;
    let st = f.status(&repo).await;
    assert_eq!(st.branch, "feat/x");
    assert_eq!(st.branches, vec!["feat/x".to_string(), "main".to_string()]);

    f.no_content("checkout", &GitCheckoutRequest { repo: s(&repo), branch: "main".into() }).await;
    assert_eq!(f.status(&repo).await.branch, "main");

    f.no_content("branch", &GitBranchRequest { repo: s(&repo), name: "feat/y".into(), from: Some("feat/x".into()) })
        .await;
    assert_eq!(f.status(&repo).await.branch, "feat/y");

    // Existing name, unknown branch, option-like input.
    f.err("branch", &GitBranchRequest { repo: s(&repo), name: "main".into(), from: None }, 409, "conflict").await;
    f.err("checkout", &GitCheckoutRequest { repo: s(&repo), branch: "nope".into() }, 409, "conflict").await;
    f.err("checkout", &GitCheckoutRequest { repo: s(&repo), branch: "--detach".into() }, 400, "bad_request").await;
    f.err("branch", &GitBranchRequest { repo: s(&repo), name: "".into(), from: None }, 400, "bad_request").await;
}

struct Remote {
    a: PathBuf,
    b: PathBuf,
}

/// A bare remote with one commit pushed, and two clones of it (`a` and `b`).
fn remote_fixture(root: &Path) -> Remote {
    let bare = root.join("remote.git");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "-q", "--bare", "-b", "main"]);
    let seed = root.join("seed");
    init_repo(&seed);
    write(&seed, "a.txt", "one\n");
    commit_all(&seed, "first");
    git(&seed, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git(&seed, &["push", "-q", "-u", "origin", "main"]);
    let mut clones = Vec::new();
    for name in ["a", "b"] {
        let dir = root.join(name);
        git(root, &["clone", "-q", bare.to_str().unwrap(), name]);
        git(&dir, &["config", "user.name", "Tester"]);
        git(&dir, &["config", "user.email", "tester@example.com"]);
        git(&dir, &["config", "commit.gpgsign", "false"]);
        clones.push(dir);
    }
    let b = clones.pop().unwrap();
    let a = clones.pop().unwrap();
    Remote { a, b }
}

fn remote_req(repo: &Path) -> GitRemoteRequest {
    GitRemoteRequest { repo: s(repo), remote: None }
}

#[tokio::test]
async fn fr_g3_push_fetch_and_pull_change_ahead_and_behind() {
    let f = start().await;
    let r = remote_fixture(&f.root);

    let st = f.status(&r.a).await;
    assert_eq!(st.upstream.as_deref(), Some("origin/main"));
    assert_eq!((st.ahead, st.behind), (0, 0));

    // A commits locally: ahead 1, then push brings it to the remote.
    write(&r.a, "a.txt", "one\ntwo\n");
    f.stage(&r.a, &["a.txt"]).await;
    f.commit(&r.a, "second").await;
    let st = f.status(&r.a).await;
    assert_eq!((st.ahead, st.behind), (1, 0));
    let out: GitOutput = f.ok("push", &remote_req(&r.a)).await;
    let _ = out.output;
    let st = f.status(&r.a).await;
    assert_eq!((st.ahead, st.behind), (0, 0));

    // B does not know yet; fetch makes it behind 1 and records the fetch time.
    assert_eq!(f.status(&r.b).await.behind, 0);
    let _: GitOutput = f.ok("fetch", &GitRemoteRequest { repo: s(&r.b), remote: Some("origin".into()) }).await;
    let st = f.status(&r.b).await;
    assert_eq!((st.ahead, st.behind), (0, 1));
    assert!(st.fetched_age_secs.is_some_and(|a| a < 60), "{:?}", st.fetched_age_secs);

    // Fast-forward pull.
    let _: GitOutput = f.ok("pull", &remote_req(&r.b)).await;
    let st = f.status(&r.b).await;
    assert_eq!((st.ahead, st.behind), (0, 0));
    assert_eq!(std::fs::read_to_string(r.b.join("a.txt")).unwrap(), "one\ntwo\n");
}

#[tokio::test]
async fn fr_g3_push_publishes_a_branch_without_upstream() {
    let f = start().await;
    let r = remote_fixture(&f.root);
    f.no_content("branch", &GitBranchRequest { repo: s(&r.a), name: "feat/new".into(), from: None }).await;
    write(&r.a, "n.txt", "n\n");
    f.stage(&r.a, &["n.txt"]).await;
    f.commit(&r.a, "new").await;
    let _: GitOutput = f.ok("push", &remote_req(&r.a)).await;
    let st = f.status(&r.a).await;
    assert_eq!(st.upstream.as_deref(), Some("origin/feat/new"));
    assert_eq!((st.ahead, st.behind), (0, 0));
}

#[tokio::test]
async fn fr_g3_a_diverged_pull_answers_409_with_a_korean_message_and_gits_text() {
    let f = start().await;
    let r = remote_fixture(&f.root);

    write(&r.a, "a.txt", "from a\n");
    f.stage(&r.a, &["a.txt"]).await;
    f.commit(&r.a, "a change").await;
    let _: GitOutput = f.ok("push", &remote_req(&r.a)).await;

    write(&r.b, "b.txt", "from b\n");
    f.stage(&r.b, &["b.txt"]).await;
    f.commit(&r.b, "b change").await;
    let _: GitOutput = f.ok("fetch", &remote_req(&r.b)).await;
    let st = f.status(&r.b).await;
    assert_eq!((st.ahead, st.behind), (1, 1));

    let msg = f.err("pull", &remote_req(&r.b), 409, "conflict").await;
    assert!(msg.to_lowercase().contains("fast-forward"), "{msg}");

    // A rejected push carries git's message too.
    let msg = f.err("push", &remote_req(&r.b), 409, "conflict").await;
    assert!(msg.contains("rejected") || msg.contains("failed to push"), "{msg}");
    // Nothing was changed by the failed pull.
    assert_eq!(f.status(&r.b).await.ahead, 1);
}

#[tokio::test]
async fn fr_g3_conflicts_lists_unmerged_paths() {
    let f = start().await;
    let repo = f.root.join("r");
    init_repo(&repo);
    write(&repo, "a.txt", "base\n");
    write(&repo, "ok.txt", "fine\n");
    commit_all(&repo, "base");
    git(&repo, &["switch", "-q", "-c", "side"]);
    write(&repo, "a.txt", "side\n");
    commit_all(&repo, "side");
    git(&repo, &["switch", "-q", "main"]);
    write(&repo, "a.txt", "main\n");
    commit_all(&repo, "main");

    assert!(f.ok::<GitConflicts>("conflicts", &GitRepoRequest { repo: s(&repo) }).await.paths.is_empty());

    let merge = Command::new("git")
        .args(["merge", "side"])
        .current_dir(&repo)
        .env("GIT_CEILING_DIRECTORIES", scratch())
        .output()
        .unwrap();
    assert!(!merge.status.success(), "the merge must conflict");

    let c: GitConflicts = f.ok("conflicts", &GitRepoRequest { repo: s(&repo) }).await;
    assert_eq!(c.paths, vec!["a.txt".to_string()]);
    let st = f.status(&repo).await;
    assert_eq!((file(&st, "a.txt").index, file(&st, "a.txt").worktree), ('U', 'U'));
}

// ---------------------------------------------------------------------------------------------
// Added coverage (review of #257)

fn has_korean(s: &str) -> bool {
    s.chars().any(|c| ('\u{AC00}'..='\u{D7A3}').contains(&c))
}

fn one_commit(repo: &Path) {
    init_repo(repo);
    write(repo, "a.txt", "one\n");
    commit_all(repo, "first");
}

#[tokio::test]
async fn fr_g1_unborn_branch_status() {
    let f = start().await;
    let repo = f.root.join("r");
    init_repo(&repo);
    write(&repo, "a.txt", "one\n");
    let st = f.status(&repo).await;
    assert_eq!(st.branch, "main");
    assert!(st.branches.is_empty(), "{:?}", st.branches);
    assert_eq!((file(&st, "a.txt").index, file(&st, "a.txt").worktree), ('?', '?'));
}

#[tokio::test]
async fn fr_g1_detached_head_status() {
    let f = start().await;
    let repo = f.root.join("r");
    one_commit(&repo);
    git(&repo, &["checkout", "-q", "--detach"]);
    let st = f.status(&repo).await;
    assert_eq!(st.branch, "(detached)");
    assert_eq!(st.upstream, None);
}

#[tokio::test]
async fn fr_g1_a_bare_repository_is_not_answered_as_not_a_repo() {
    let f = start().await;
    let bare = f.root.join("bare.git");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "-q", "--bare"]);
    let msg = f.err("status", &GitRepoRequest { repo: s(&bare) }, 409, "conflict").await;
    assert!(has_korean(&msg) && msg.contains("bare"), "{msg}");
}

#[tokio::test]
async fn fr_g1_a_plain_directory_gets_a_korean_not_a_repo_message() {
    let f = start().await;
    let dir = f.root.join("plain");
    std::fs::create_dir_all(&dir).unwrap();
    let msg = f.err("status", &GitRepoRequest { repo: s(&dir) }, 404, "not_a_repo").await;
    assert!(has_korean(&msg), "{msg}");
}

#[test]
fn fr_g1_dubious_ownership_and_known_failures_map_to_korean() {
    let dubious = "fatal: detected dubious ownership in repository at '/x'";
    assert!(korean_message(dubious).is_some_and(|m| m.contains("소유자")));
    assert!(korean_message("fatal: You are not currently on a branch.").is_some_and(|m| m.contains("detached")));
    assert!(korean_message("fatal: Not possible to fast-forward, aborting.").is_some_and(|m| m.contains("갈라졌")));
    assert!(korean_message("Author identity unknown\n*** Please tell me who you are.").is_some());
    assert!(korean_message("fatal: unable to connect to 127.0.0.1:1").is_some_and(|m| m.contains("원격")));
    assert_eq!(korean_message("something unrelated"), None);
}

#[tokio::test]
async fn fr_g1_korean_and_space_file_names_go_through_status_diff_and_stage() {
    let f = start().await;
    let repo = f.root.join("r");
    one_commit(&repo);
    let names = ["한글 파일.txt", "with space.txt", "dir/중첩 폴더/문서.md"];
    for n in names {
        write(&repo, n, "내용\n");
    }
    let st = f.status(&repo).await;
    for n in names {
        assert_eq!((file(&st, n).index, file(&st, n).worktree), ('?', '?'), "{n}");
    }
    // The patch header names the raw path (no octal quoting).
    let d = f.diff(&repo, Some("한글 파일.txt"), false).await;
    assert!(d.contains("한글 파일.txt") && !d.contains("\\355"), "{d}");
    f.stage(&repo, &names).await;
    let st = f.status(&repo).await;
    for n in names {
        assert_eq!(file(&st, n).index, 'A', "{n}");
    }
    let staged = f.diff(&repo, Some("한글 파일.txt"), true).await;
    assert!(staged.contains("한글 파일.txt") && staged.contains("+내용"), "{staged}");
    // A tracked non-ASCII file modified later shows quoted-free headers in the unstaged diff.
    git(&repo, &["commit", "-q", "-m", "names"]);
    write(&repo, "한글 파일.txt", "바뀜\n");
    let d = f.diff(&repo, None, false).await;
    assert!(d.contains("a/한글 파일.txt"), "{d}");
}

#[tokio::test]
async fn fr_g2_a_glob_named_file_stages_only_itself() {
    let f = start().await;
    let repo = f.root.join("r");
    one_commit(&repo);
    write(&repo, "a[1].txt", "x\n");
    write(&repo, "a1.txt", "y\n");
    write(&repo, "b.rs", "z\n");
    f.stage(&repo, &["a[1].txt"]).await;
    let st = f.status(&repo).await;
    assert_eq!(file(&st, "a[1].txt").index, 'A');
    assert_eq!(file(&st, "a1.txt").index, '?');
    // A glob matches nothing literal: git refuses and nothing is staged.
    let body = GitPathsRequest { repo: s(&repo), paths: vec!["*.rs".into()] };
    f.err("stage", &body, 409, "conflict").await;
    assert_eq!(file(&f.status(&repo).await, "b.rs").index, '?', "a glob must not match");
}

#[tokio::test]
async fn fr_g2_a_path_starting_with_a_dash_is_a_path() {
    let f = start().await;
    let repo = f.root.join("r");
    one_commit(&repo);
    write(&repo, "-rf", "x\n");
    write(&repo, "other.txt", "y\n");
    let d = f.diff(&repo, Some("-rf"), false).await;
    assert!(d.contains("-rf") && !d.contains("other.txt"), "{d}");
    f.stage(&repo, &["-rf"]).await;
    let st = f.status(&repo).await;
    assert_eq!(file(&st, "-rf").index, 'A');
    assert_eq!(file(&st, "other.txt").index, '?');
}

#[tokio::test]
async fn fr_g2_include_untracked_returns_every_untracked_file_in_one_call() {
    let f = start().await;
    let repo = f.root.join("r");
    one_commit(&repo);
    write(&repo, "a.txt", "one\nchanged\n");
    write(&repo, "u1.txt", "u1\n");
    write(&repo, "sub/u2.txt", "u2\n");
    let d: GitDiff = f
        .ok("diff", &GitDiffRequest { repo: s(&repo), path: None, staged: false, include_untracked: true })
        .await;
    for p in ["a/a.txt", "b/u1.txt", "b/sub/u2.txt"] {
        assert!(d.patch.contains(p), "{p} missing in {}", d.patch);
    }
    // The default stays as before: tracked changes only. A body without the field still parses.
    let plain = f.diff(&repo, None, false).await;
    assert!(!plain.contains("u1.txt"), "{plain}");
    let (st, _) = f.raw("diff", &serde_json::json!({ "repo": s(&repo) })).await;
    assert_eq!(st, 200);
}

#[tokio::test]
async fn fr_g1_a_diff_larger_than_the_cap_answers_413() {
    let f = start().await;
    let repo = f.root.join("r");
    init_repo(&repo);
    write(&repo, "big.txt", "x\n");
    commit_all(&repo, "first");
    write(&repo, "big.txt", &"0123456789abcdef\n".repeat(200_000));
    let msg = f.err("diff", &GitDiffRequest { repo: s(&repo), path: None, staged: false, include_untracked: false }, 413, "too_large").await;
    assert!(has_korean(&msg), "{msg}");
}

#[tokio::test]
async fn fr_g3_push_and_pull_from_a_detached_head_answer_409_in_korean() {
    let f = start().await;
    let r = remote_fixture(&f.root);
    git(&r.a, &["checkout", "-q", "--detach"]);
    for op in ["push", "pull"] {
        let msg = f.err(op, &remote_req(&r.a), 409, "conflict").await;
        assert!(has_korean(&msg) && msg.contains("detached"), "{op}: {msg}");
    }
}

#[tokio::test]
async fn fr_g3_pull_without_an_upstream_answers_409_in_korean() {
    let f = start().await;
    let r = remote_fixture(&f.root);
    f.no_content("branch", &GitBranchRequest { repo: s(&r.a), name: "lonely".into(), from: None }).await;
    let msg = f.err("pull", &remote_req(&r.a), 409, "conflict").await;
    assert!(has_korean(&msg) && msg.contains("upstream"), "{msg}");
}

#[tokio::test]
async fn fr_g3_a_remote_that_is_not_configured_is_a_400() {
    let f = start().await;
    let r = remote_fixture(&f.root);
    for remote in ["nope", "file:///etc", "ext::sh -c id", "--upload-pack=x"] {
        for op in ["fetch", "pull", "push"] {
            let body = GitRemoteRequest { repo: s(&r.a), remote: Some(remote.into()) };
            let msg = f.err(op, &body, 400, "bad_request").await;
            assert!(has_korean(&msg), "{op} {remote}: {msg}");
        }
    }
}

#[tokio::test]
async fn fr_g3_an_unreachable_remote_ends_in_a_korean_error_quickly() {
    let f = start().await;
    let repo = f.root.join("r");
    one_commit(&repo);
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let missing = s(&f.root.join("does-not-exist.git"));
    for url in [missing, format!("git://127.0.0.1:{closed}/x.git")] {
        let _ = Command::new("git").args(["remote", "remove", "origin"]).current_dir(&repo).output();
        git(&repo, &["remote", "add", "origin", &url]);
        let t = std::time::Instant::now();
        for op in ["fetch", "push"] {
            let body = GitRemoteRequest { repo: s(&repo), remote: Some("origin".into()) };
            let msg = f.err(op, &body, 409, "conflict").await;
            assert!(has_korean(&msg) && msg.contains("원격"), "{op} {url}: {msg}");
        }
        assert!(t.elapsed() < std::time::Duration::from_secs(30), "{url}");
    }
}

#[tokio::test]
async fn fr_g3_a_remote_that_never_answers_times_out_with_504() {
    let f = start().await;
    let repo = f.root.join("r");
    one_commit(&repo);
    // Accepts connections and says nothing: git waits for the server greeting.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((sock, _)) = listener.accept().await {
            held.push(sock);
        }
    });
    git(&repo, &["remote", "add", "origin", &format!("git://127.0.0.1:{port}/x.git")]);
    let t = std::time::Instant::now();
    let msg = f
        .err("fetch", &GitRemoteRequest { repo: s(&repo), remote: Some("origin".into()) }, 504, "timeout")
        .await;
    assert!(has_korean(&msg), "{msg}");
    assert!(t.elapsed() < std::time::Duration::from_secs(30));
    // The node is still responsive afterwards (nothing holds a lock).
    assert_eq!(f.status(&repo).await.branch, "main");
}

#[tokio::test]
async fn fr_g3_merge_conflict_shows_u_records_and_conflicts() {
    let f = start().await;
    let repo = f.root.join("r");
    init_repo(&repo);
    write(&repo, "a.txt", "base\n");
    commit_all(&repo, "base");
    git(&repo, &["switch", "-q", "-c", "side"]);
    write(&repo, "a.txt", "side\n");
    commit_all(&repo, "side");
    git(&repo, &["switch", "-q", "main"]);
    write(&repo, "a.txt", "main\n");
    commit_all(&repo, "main");
    let m = Command::new("git").args(["merge", "side"]).current_dir(&repo).env("GIT_CEILING_DIRECTORIES", scratch()).output().unwrap();
    assert!(!m.status.success());

    let st = f.status(&repo).await;
    let a = file(&st, "a.txt");
    assert_eq!((a.index, a.worktree), ('U', 'U'));
    let c: GitConflicts = f.ok("conflicts", &GitRepoRequest { repo: s(&repo) }).await;
    assert_eq!(c.paths, vec!["a.txt".to_string()]);
    // A commit while unmerged is refused with a Korean message.
    let msg = f
        .err("commit", &GitCommitRequest { repo: s(&repo), summary: "x".into(), description: None, amend: false }, 409, "conflict")
        .await;
    assert!(has_korean(&msg), "{msg}");
}

#[tokio::test]
async fn fr_g2_a_commit_without_user_name_answers_409_in_korean() {
    let f = start().await;
    let repo = f.root.join("r");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.useConfigOnly", "true"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    write(&repo, "a.txt", "x\n");
    f.stage(&repo, &["a.txt"]).await;
    let msg = f
        .err("commit", &GitCommitRequest { repo: s(&repo), summary: "x".into(), description: None, amend: false }, 409, "conflict")
        .await;
    assert!(msg.contains("user.name"), "{msg}");
}

#[tokio::test]
async fn fr_g3_a_switch_blocked_by_local_changes_answers_409_in_korean() {
    let f = start().await;
    let repo = f.root.join("r");
    one_commit(&repo);
    git(&repo, &["switch", "-q", "-c", "other"]);
    write(&repo, "a.txt", "other\n");
    commit_all(&repo, "other");
    git(&repo, &["switch", "-q", "main"]);
    write(&repo, "a.txt", "dirty\n");
    let msg = f.err("checkout", &GitCheckoutRequest { repo: s(&repo), branch: "other".into() }, 409, "conflict").await;
    assert!(has_korean(&msg) && msg.contains("a.txt"), "{msg}");
    assert_eq!(f.status(&repo).await.branch, "main");
}

#[cfg(unix)]
#[tokio::test]
async fn fr_g3_a_configured_ssh_command_is_used_with_batch_mode() {
    let f = start().await;
    let repo = f.root.join("r");
    one_commit(&repo);
    // A stand-in for the user's ssh: records its arguments and fails like an unreachable host.
    let script = f.root.join("my-ssh.sh");
    let record = f.root.join("ssh-args.txt");
    std::fs::write(&script, format!("#!/bin/sh\necho \"$@\" > '{}'\nexit 255\n", s(&record))).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(&repo, &["config", "core.sshCommand", &s(&script)]);
    git(&repo, &["remote", "add", "origin", "ssh://example.invalid/x.git"]);
    let body = GitRemoteRequest { repo: s(&repo), remote: Some("origin".into()) };
    let msg = f.err("fetch", &body, 409, "conflict").await;
    assert!(has_korean(&msg), "{msg}");
    let args = std::fs::read_to_string(&record).expect("the configured ssh command ran");
    assert!(args.contains("BatchMode=yes"), "{args}");
    assert!(args.contains("example.invalid"), "{args}");
}
