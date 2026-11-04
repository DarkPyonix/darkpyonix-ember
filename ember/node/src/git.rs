//! The git API (SPEC FR-G1 to FR-G3): repository state, staging, commits, branches and remotes,
//! implemented by running the `git` CLI in the repository (`LC_ALL=C`, no terminal prompts).
//!
//! Every request names the repository by absolute path; the path must resolve inside an allowed
//! root. A directory that is not a repository answers [`GitError::NotARepo`], a git refusal
//! (empty index, diverged pull, patch that does not apply) answers [`GitError::Conflict`] with
//! git's own message, and malformed input answers [`GitError::Bad`].

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::policy::{PathPolicy, PolicyError};

// ---------------------------------------------------------------------------------------------
// Wire types

/// A request that only names the repository (`/git/status`, `/git/conflicts`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitRepoRequest {
    pub repo: String,
}

pub type GitStatusRequest = GitRepoRequest;

/// One changed path. `index` and `worktree` are porcelain v2 status characters (`M`, `A`, `D`,
/// `R`, `U`, `.` for unchanged); an untracked file is `?`, `?`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitFile {
    pub path: String,
    #[serde(default)]
    pub old_path: Option<String>,
    pub index: char,
    pub worktree: char,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitStatus {
    /// Current branch, or `(detached)`.
    pub branch: String,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    /// Seconds since the last fetch (mtime of `FETCH_HEAD`), `None` if never fetched.
    pub fetched_age_secs: Option<u64>,
    /// Local branch names.
    pub branches: Vec<String>,
    pub files: Vec<GitFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitDiffRequest {
    pub repo: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub staged: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitDiff {
    pub patch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitPathsRequest {
    pub repo: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitApplyRequest {
    pub repo: String,
    pub patch: String,
    #[serde(default)]
    pub reverse: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitCommitRequest {
    pub repo: String,
    pub summary: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub amend: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitCommitResponse {
    pub sha: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitCheckoutRequest {
    pub repo: String,
    pub branch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitBranchRequest {
    pub repo: String,
    pub name: String,
    #[serde(default)]
    pub from: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitRemoteRequest {
    pub repo: String,
    #[serde(default)]
    pub remote: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitOutput {
    pub output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitConflicts {
    pub paths: Vec<String>,
}

// ---------------------------------------------------------------------------------------------
// Errors and process runner

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error(transparent)]
    Policy(#[from] PolicyError),
    #[error("not a git repository: {0}")]
    NotARepo(String),
    #[error("{0}")]
    Bad(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Internal(String),
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

async fn run(repo: &Path, args: &[&str], stdin: Option<&str>) -> Result<Out, GitError> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(args)
        .current_dir(repo)
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| GitError::Internal(format!("cannot run git: {e}")))?;
    if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // A write error (git exited early) surfaces through its exit status below.
        let _ = pipe.write_all(data.as_bytes()).await;
        drop(pipe);
    }
    let o = child.wait_with_output().await.map_err(|e| GitError::Internal(e.to_string()))?;
    Ok(Out {
        code: o.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    })
}

/// Run git and require exit status 0; a failure is a [`GitError::Conflict`] with git's message.
async fn run_ok(repo: &Path, args: &[&str], stdin: Option<&str>) -> Result<Out, GitError> {
    let o = run(repo, args, stdin).await?;
    if o.code == 0 {
        Ok(o)
    } else {
        Err(GitError::Conflict(message(&o)))
    }
}

fn message(o: &Out) -> String {
    let m = if o.stderr.trim().is_empty() { &o.stdout } else { &o.stderr };
    m.trim().to_string()
}

/// Resolve the repository path through the policy and check it is inside a work tree.
async fn open(policy: &PathPolicy, repo: &str) -> Result<PathBuf, GitError> {
    let dir = policy.resolve_existing(Path::new(repo))?;
    let o = run(&dir, &["rev-parse", "--is-inside-work-tree"], None).await?;
    if o.code != 0 || o.stdout.trim() != "true" {
        return Err(GitError::NotARepo(repo.to_string()));
    }
    Ok(dir)
}

/// A ref, remote or branch name must not look like an option.
fn name_arg<'a>(what: &str, v: &'a str) -> Result<&'a str, GitError> {
    if v.is_empty() || v.starts_with('-') || v.contains('\0') {
        return Err(GitError::Bad(format!("invalid {what}: {v:?}")));
    }
    Ok(v)
}

async fn has_head(dir: &Path) -> Result<bool, GitError> {
    Ok(run(dir, &["rev-parse", "--verify", "-q", "HEAD"], None).await?.code == 0)
}

// ---------------------------------------------------------------------------------------------
// Operations

pub async fn status(policy: &PathPolicy, r: &GitRepoRequest) -> Result<GitStatus, GitError> {
    let dir = open(policy, &r.repo).await?;
    let o = run_ok(&dir, &["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all"], None).await?;
    let mut st = GitStatus {
        branch: String::new(),
        upstream: None,
        ahead: 0,
        behind: 0,
        fetched_age_secs: None,
        branches: Vec::new(),
        files: Vec::new(),
    };
    let mut parts = o.stdout.split('\0');
    while let Some(rec) = parts.next() {
        if rec.is_empty() {
            continue;
        }
        if let Some(h) = rec.strip_prefix("# ") {
            if let Some(b) = h.strip_prefix("branch.head ") {
                st.branch = b.to_string();
            } else if let Some(u) = h.strip_prefix("branch.upstream ") {
                st.upstream = Some(u.to_string());
            } else if let Some(ab) = h.strip_prefix("branch.ab ") {
                for t in ab.split_whitespace() {
                    if let Some(n) = t.strip_prefix('+') {
                        st.ahead = n.parse().unwrap_or(0);
                    } else if let Some(n) = t.strip_prefix('-') {
                        st.behind = n.parse().unwrap_or(0);
                    }
                }
            }
            continue;
        }
        let xy = |s: &str| {
            let mut c = s.chars();
            (c.next().unwrap_or('.'), c.next().unwrap_or('.'))
        };
        match rec.chars().next() {
            Some('1') => {
                let f: Vec<&str> = rec.splitn(9, ' ').collect();
                if f.len() == 9 {
                    let (index, worktree) = xy(f[1]);
                    st.files.push(GitFile { path: f[8].to_string(), old_path: None, index, worktree });
                }
            }
            Some('2') => {
                let f: Vec<&str> = rec.splitn(10, ' ').collect();
                let orig = parts.next().unwrap_or("");
                if f.len() == 10 {
                    let (index, worktree) = xy(f[1]);
                    st.files.push(GitFile {
                        path: f[9].to_string(),
                        old_path: Some(orig.to_string()),
                        index,
                        worktree,
                    });
                }
            }
            Some('u') => {
                let f: Vec<&str> = rec.splitn(11, ' ').collect();
                if f.len() == 11 {
                    let (index, worktree) = xy(f[1]);
                    st.files.push(GitFile { path: f[10].to_string(), old_path: None, index, worktree });
                }
            }
            Some('?') => {
                if let Some(p) = rec.strip_prefix("? ") {
                    st.files.push(GitFile { path: p.to_string(), old_path: None, index: '?', worktree: '?' });
                }
            }
            _ => {}
        }
    }

    let b = run_ok(&dir, &["for-each-ref", "--format=%(refname:short)", "refs/heads"], None).await?;
    st.branches = b.stdout.lines().map(str::to_string).collect();

    let fh = run(&dir, &["rev-parse", "--git-path", "FETCH_HEAD"], None).await?;
    if fh.code == 0 {
        let p = dir.join(fh.stdout.trim());
        st.fetched_age_secs = std::fs::metadata(&p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|d| d.as_secs());
    }
    Ok(st)
}

pub async fn diff(policy: &PathPolicy, r: &GitDiffRequest) -> Result<GitDiff, GitError> {
    let dir = open(policy, &r.repo).await?;
    if let Some(p) = r.path.as_deref().filter(|_| !r.staged) {
        let u = run_ok(&dir, &["ls-files", "--others", "--exclude-standard", "-z", "--", p], None).await?;
        if !u.stdout.is_empty() {
            // Untracked: diff against /dev/null; exit status 1 means "differences found".
            let o = run(&dir, &["diff", "--no-color", "--no-index", "--", "/dev/null", p], None).await?;
            if o.code > 1 {
                return Err(GitError::Conflict(message(&o)));
            }
            return Ok(GitDiff { patch: o.stdout });
        }
    }
    let mut args = vec!["diff", "--no-color"];
    if r.staged {
        args.push("--cached");
    }
    if let Some(p) = r.path.as_deref() {
        args.push("--");
        args.push(p);
    }
    Ok(GitDiff { patch: run_ok(&dir, &args, None).await?.stdout })
}

fn paths_of(r: &GitPathsRequest) -> Result<(), GitError> {
    if r.paths.is_empty() {
        return Err(GitError::Bad("paths must not be empty".into()));
    }
    Ok(())
}

pub async fn stage(policy: &PathPolicy, r: &GitPathsRequest) -> Result<(), GitError> {
    paths_of(r)?;
    let dir = open(policy, &r.repo).await?;
    let mut args = vec!["add", "-A", "--"];
    args.extend(r.paths.iter().map(String::as_str));
    run_ok(&dir, &args, None).await.map(|_| ())
}

pub async fn unstage(policy: &PathPolicy, r: &GitPathsRequest) -> Result<(), GitError> {
    paths_of(r)?;
    let dir = open(policy, &r.repo).await?;
    let mut args = if has_head(&dir).await? {
        vec!["reset", "-q", "--"]
    } else {
        // No commit yet: there is no HEAD to reset the index to.
        vec!["rm", "-r", "-q", "--cached", "--ignore-unmatch", "--"]
    };
    args.extend(r.paths.iter().map(String::as_str));
    run_ok(&dir, &args, None).await.map(|_| ())
}

pub async fn apply(policy: &PathPolicy, r: &GitApplyRequest) -> Result<(), GitError> {
    if r.patch.trim().is_empty() {
        return Err(GitError::Bad("patch must not be empty".into()));
    }
    let dir = open(policy, &r.repo).await?;
    let mut args = vec!["apply", "--cached", "--recount"];
    if r.reverse {
        args.push("-R");
    }
    args.push("-");
    run_ok(&dir, &args, Some(&r.patch)).await.map(|_| ())
}

pub async fn commit(policy: &PathPolicy, r: &GitCommitRequest) -> Result<GitCommitResponse, GitError> {
    if r.summary.trim().is_empty() {
        return Err(GitError::Bad("summary must not be empty".into()));
    }
    let dir = open(policy, &r.repo).await?;
    if !r.amend {
        // `--quiet` exits 1 when the index differs from HEAD (or from the empty tree).
        let d = run(&dir, &["diff", "--cached", "--quiet"], None).await?;
        if d.code == 0 {
            return Err(GitError::Conflict("nothing staged to commit".into()));
        }
    }
    let mut msg = r.summary.trim().to_string();
    if let Some(d) = r.description.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        msg.push_str("\n\n");
        msg.push_str(d);
    }
    msg.push('\n');
    let mut args = vec!["commit", "-q", "--allow-empty-message", "-F", "-"];
    if r.amend {
        args.push("--amend");
    }
    run_ok(&dir, &args, Some(&msg)).await?;
    let sha = run_ok(&dir, &["rev-parse", "HEAD"], None).await?;
    Ok(GitCommitResponse { sha: sha.stdout.trim().to_string() })
}

pub async fn checkout(policy: &PathPolicy, r: &GitCheckoutRequest) -> Result<(), GitError> {
    let branch = name_arg("branch", &r.branch)?;
    let dir = open(policy, &r.repo).await?;
    run_ok(&dir, &["switch", branch], None).await.map(|_| ())
}

pub async fn branch(policy: &PathPolicy, r: &GitBranchRequest) -> Result<(), GitError> {
    let name = name_arg("branch name", &r.name)?;
    let from = r.from.as_deref().map(|f| name_arg("start point", f)).transpose()?;
    let dir = open(policy, &r.repo).await?;
    let mut args = vec!["switch", "-c", name];
    if let Some(f) = from {
        args.push(f);
    }
    run_ok(&dir, &args, None).await.map(|_| ())
}

fn combined(o: &Out) -> GitOutput {
    let mut s = o.stdout.clone();
    if !o.stderr.is_empty() {
        if !s.is_empty() && !s.ends_with('\n') {
            s.push('\n');
        }
        s.push_str(&o.stderr);
    }
    GitOutput { output: s.trim_end().to_string() }
}

fn remote_arg(r: &GitRemoteRequest) -> Result<Option<&str>, GitError> {
    r.remote.as_deref().map(|v| name_arg("remote", v)).transpose()
}

pub async fn fetch(policy: &PathPolicy, r: &GitRemoteRequest) -> Result<GitOutput, GitError> {
    let remote = remote_arg(r)?;
    let dir = open(policy, &r.repo).await?;
    let mut args = vec!["fetch"];
    args.extend(remote);
    run_ok(&dir, &args, None).await.map(|o| combined(&o))
}

/// Fast-forward only: a diverged branch is refused with git's message.
pub async fn pull(policy: &PathPolicy, r: &GitRemoteRequest) -> Result<GitOutput, GitError> {
    let remote = remote_arg(r)?;
    let dir = open(policy, &r.repo).await?;
    let mut args = vec!["pull", "--ff-only"];
    args.extend(remote);
    run_ok(&dir, &args, None).await.map(|o| combined(&o))
}

/// Push the current branch. When it has no upstream yet, it is published and tracked
/// (`push -u <remote|origin> HEAD`).
pub async fn push(policy: &PathPolicy, r: &GitRemoteRequest) -> Result<GitOutput, GitError> {
    let remote = remote_arg(r)?;
    let dir = open(policy, &r.repo).await?;
    let has_upstream = run(&dir, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"], None).await?.code == 0;
    let mut args = vec!["push"];
    if has_upstream {
        args.extend(remote);
    } else {
        args.extend(["-u", remote.unwrap_or("origin"), "HEAD"]);
    }
    run_ok(&dir, &args, None).await.map(|o| combined(&o))
}

pub async fn conflicts(policy: &PathPolicy, r: &GitRepoRequest) -> Result<GitConflicts, GitError> {
    let dir = open(policy, &r.repo).await?;
    let o = run_ok(&dir, &["diff", "--name-only", "--diff-filter=U", "-z"], None).await?;
    Ok(GitConflicts { paths: o.stdout.split('\0').filter(|p| !p.is_empty()).map(str::to_string).collect() })
}
