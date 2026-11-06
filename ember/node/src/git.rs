//! The git API (SPEC FR-G1 to FR-G3): repository state, staging, commits, branches and remotes,
//! implemented by running the `git` CLI in the repository (`LC_ALL=C`, no terminal prompts, no
//! credential or ssh prompts, literal pathspecs, unquoted non-ASCII paths).
//!
//! Every request names the repository by absolute path; the path must resolve inside an allowed
//! root. A directory that is not a repository answers [`GitError::NotARepo`], a git refusal
//! (empty index, diverged pull, patch that does not apply) answers [`GitError::Conflict`] with
//! git's own message, and malformed input answers [`GitError::Bad`]. Messages are Korean with
//! git's text after them as detail. `fetch`, `pull` and `push` stop after
//! [`network_timeout`] ([`GitError::Timeout`]); output beyond [`max_output`] is
//! [`GitError::TooLarge`].


#[path = "git_branches.rs"]
pub mod git_branches;
#[path = "git_commit.rs"]
pub mod git_commit;
#[path = "git_github.rs"]
pub mod git_github;
#[path = "git_log.rs"]
pub mod git_log;
#[path = "git_merge.rs"]
pub mod git_merge;
#[path = "git_prs.rs"]
pub mod git_prs;
#[path = "git_stash.rs"]
pub mod git_stash;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

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
    /// With no `path` and `staged` false: also include every untracked file, as new-file
    /// patches, in the same answer (one call instead of one per file).
    #[serde(default)]
    pub include_untracked: bool,
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
    #[error("{0}")]
    NotARepo(String),
    #[error("{0}")]
    Bad(String),
    #[error("{0}")]
    Conflict(String),
    /// The named object (a commit) does not exist (404 `not_found`).
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Internal(String),
    /// A network operation did not finish in time (504).
    #[error("{0}")]
    Timeout(String),
    /// The output exceeded [`max_output`] (413).
    #[error("{0}")]
    TooLarge(String),
}

/// Default deadline for `fetch`, `pull` and `push`.
pub const NETWORK_TIMEOUT_SECS: u64 = 60;
/// Default cap on the stdout of one git command (8 MiB).
pub const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_STDERR_BYTES: usize = 1024 * 1024;

/// The network deadline: `EMBER_GIT_NETWORK_TIMEOUT_SECS` if set to a positive number (tests),
/// else [`NETWORK_TIMEOUT_SECS`].
pub fn network_timeout() -> Duration {
    let secs = std::env::var("EMBER_GIT_NETWORK_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(NETWORK_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// The stdout cap: `EMBER_GIT_MAX_OUTPUT_BYTES` if set to a positive number (tests), else
/// [`MAX_OUTPUT_BYTES`].
pub fn max_output() -> usize {
    std::env::var("EMBER_GIT_MAX_OUTPUT_BYTES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(MAX_OUTPUT_BYTES)
}

fn too_large() -> GitError {
    GitError::TooLarge(format!(
        "git 출력이 너무 커서 중단했습니다 (최대 {} KiB). 파일 하나씩 확인하세요.",
        max_output() / 1024
    ))
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
    /// stdout reached [`max_output`] and git was stopped.
    truncated: bool,
}

async fn read_capped<R: AsyncRead + Unpin>(mut r: R, cap: usize) -> (Vec<u8>, bool) {
    let mut buf = Vec::new();
    let _ = (&mut r).take(cap as u64 + 1).read_to_end(&mut buf).await;
    let over = buf.len() > cap;
    buf.truncate(cap);
    (buf, over)
}

async fn run(repo: &Path, args: &[&str], stdin: Option<&str>) -> Result<Out, GitError> {
    run_with(repo, args, stdin, None).await
}

/// Run git with a deadline (network operations).
async fn run_net(repo: &Path, args: &[&str]) -> Result<Out, GitError> {
    run_with(repo, args, None, Some(network_timeout())).await
}

/// The `core.sshCommand` git would use here (repository, then global config), if any.
async fn configured_ssh_command(repo: &Path) -> Option<String> {
    let out = tokio::process::Command::new("git")
        .args(["config", "--get", "core.sshCommand"])
        .current_dir(repo)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !text.is_empty()).then_some(text)
}

/// The ssh command for a network operation: the configured one (or plain `ssh`) in batch mode.
fn batch_ssh_command(configured: Option<&str>) -> String {
    format!("{} -o BatchMode=yes", configured.unwrap_or("ssh"))
}

async fn run_with(repo: &Path, args: &[&str], stdin: Option<&str>, deadline: Option<Duration>) -> Result<Out, GitError> {
    let mut cmd = tokio::process::Command::new("git");
    // Non-ASCII paths stay raw (not octal-quoted), pathspecs are literal (`a[1].txt`, `*.rs`),
    // and nothing may wait for a person: no terminal, credential or ssh prompt. The user's own
    // askpass (`GIT_ASKPASS`, `core.askPass`, `SSH_ASKPASS`) and credential helper are kept; with
    // none configured, `GIT_TERMINAL_PROMPT=0` makes an unanswered prompt fail.
    cmd.args(["-c", "core.quotepath=false"])
        .args(args)
        .current_dir(repo)
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        // A revert or cherry-pick never opens an editor for its message.
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("SSH_ASKPASS_REQUIRE", "never")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // `git stash push --include-untracked` silently skips untracked files under literal
    // pathspecs (git 2.43), and the stash push here never takes a pathspec, so it opts out.
    if !args.starts_with(&["stash", "push"]) {
        cmd.env("GIT_LITERAL_PATHSPECS", "1");
    }
    // Network operations never ask for an ssh password. GIT_SSH_COMMAND outranks the user's
    // `core.sshCommand` (keys, proxy jump), so when the variable is unset the configured command
    // is read and kept, with BatchMode appended.
    if deadline.is_some() && std::env::var_os("GIT_SSH_COMMAND").is_none() {
        cmd.env("GIT_SSH_COMMAND", batch_ssh_command(configured_ssh_command(repo).await.as_deref()));
    }
    exec_cmd(cmd, "git", stdin, deadline).await
}

/// SIGKILL every process of the group led by `pid` (see `process_group(0)` in [`exec_cmd`]).
#[cfg(unix)]
fn kill_group(pid: Option<u32>) {
    if let Some(pid) = pid {
        // SAFETY: plain signal delivery to a process group id this function's caller created.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill_group(_pid: Option<u32>) {}

/// Spawn a prepared command with piped output, the stdout cap and an optional deadline. Shared
/// by git and the `gh` runner of FR-G8.
async fn exec_cmd(mut cmd: tokio::process::Command, what: &str, stdin: Option<&str>, deadline: Option<Duration>) -> Result<Out, GitError> {
    // A deadline marks a network command: it runs in its own process group, so a timeout or the
    // output cap can end the whole tree (git, ssh, credential helpers), not only git.
    #[cfg(unix)]
    if deadline.is_some() {
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound && what != "git" {
            // A companion program (`gh`) that is not installed is a state the person can fix.
            GitError::Conflict(format!("{what} 명령을 찾을 수 없습니다. 설치되어 있는지 확인하세요.\n{e}"))
        } else {
            GitError::Internal(format!("cannot run {what}: {e}"))
        }
    })?;
    let group = if deadline.is_some() { child.id() } else { None };
    let pipe = child.stdin.take();
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let cap = max_output();
    let work = async {
        let write = async {
            if let (Some(data), Some(mut p)) = (stdin, pipe) {
                // A write error (git exited early) surfaces through its exit status below.
                let _ = p.write_all(data.as_bytes()).await;
            }
        };
        let ((out, over), (err, _), ()) =
            tokio::join!(read_capped(stdout, cap), read_capped(stderr, MAX_STDERR_BYTES), write);
        if over {
            kill_group(group);
            let _ = child.kill().await;
        }
        let status = child.wait().await.map_err(|e| GitError::Internal(e.to_string()))?;
        Ok::<_, GitError>(Out {
            code: status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out).into_owned(),
            stderr: String::from_utf8_lossy(&err).into_owned(),
            truncated: over,
        })
    };
    match deadline {
        None => work.await,
        Some(d) => match tokio::time::timeout(d, work).await {
            Ok(r) => r,
            Err(_) => {
                kill_group(group);
                let _ = child.kill().await;
                Err(GitError::Timeout(format!(
                    "원격 저장소가 {}초 안에 응답하지 않아 중단했습니다 (네트워크 연결이나 인증 대기를 확인하세요).",
                    d.as_secs()
                )))
            }
        },
    }
}

/// Run git and require exit status 0; a failure is a [`GitError::Conflict`] with a Korean
/// message and git's own text as detail.
async fn run_ok(repo: &Path, args: &[&str], stdin: Option<&str>) -> Result<Out, GitError> {
    finish(run(repo, args, stdin).await?)
}

async fn run_net_ok(repo: &Path, args: &[&str]) -> Result<Out, GitError> {
    finish(run_net(repo, args).await?)
}

fn finish(o: Out) -> Result<Out, GitError> {
    if o.truncated {
        return Err(too_large());
    }
    if o.code == 0 {
        Ok(o)
    } else {
        Err(GitError::Conflict(describe(&message(&o))))
    }
}

fn message(o: &Out) -> String {
    let m = if o.stderr.trim().is_empty() { &o.stdout } else { &o.stderr };
    m.trim().to_string()
}

/// The Korean text for a known git failure, or `None`.
pub fn korean_message(git_text: &str) -> Option<&'static str> {
    let t = git_text.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| t.contains(n));
    if has(&["dubious ownership"]) {
        Some("저장소 소유자가 현재 사용자와 달라 git이 접근을 거부했습니다 (safe.directory 설정이 필요합니다).")
    } else if has(&["bad revision", "unknown revision or path not in the working tree"]) {
        Some("브랜치를 찾을 수 없습니다. 브랜치 이름을 확인하세요.")
    } else if has(&["not currently on a branch"]) {
        Some("HEAD가 브랜치에 있지 않습니다 (detached HEAD). 브랜치로 전환한 뒤 다시 시도하세요.")
    } else if has(&["no tracking information", "no upstream", "has no upstream"]) {
        Some("현재 브랜치에 upstream이 없습니다. 먼저 push로 원격 브랜치를 만드세요.")
    } else if has(&["not possible to fast-forward", "diverging branches", "cannot fast-forward"]) {
        Some("로컬과 원격 브랜치가 갈라졌습니다 (fast-forward 불가). 병합이나 rebase가 필요합니다.")
    } else if has(&["unmerged files", "conflict (", "merge conflict", "fix conflicts"]) {
        Some("병합 충돌이 있습니다. 충돌을 해결한 뒤 다시 시도하세요.")
    } else if has(&[
        "author identity unknown",
        "please tell me who you are",
        "unable to auto-detect",
        "empty ident name",
        "no email was given",
        "no name was given",
    ]) {
        Some("커밋 작성자(user.name, user.email)가 설정되지 않았습니다.")
    } else if has(&["would be overwritten", "please commit your changes or stash", "local changes"]) {
        Some("커밋하지 않은 로컬 변경 사항 때문에 전환할 수 없습니다. 커밋하거나 stash한 뒤 다시 시도하세요.")
    } else if has(&["[rejected]", "failed to push some refs", "non-fast-forward"]) {
        Some("원격에 더 새로운 커밋이 있어 push가 거부되었습니다. 먼저 pull 하세요.")
    } else if has(&[
        "could not resolve host",
        "unable to access",
        "unable to connect",
        "connection refused",
        "connection timed out",
        "network is unreachable",
        "could not read from remote repository",
        "does not appear to be a git repository",
        "permission denied (publickey",
        "host key verification failed",
        "terminal prompts disabled",
        "authentication failed",
    ]) {
        Some("원격 저장소에 연결하지 못했습니다 (주소, 네트워크, 인증을 확인하세요).")
    } else if has(&["nothing staged"]) {
        Some("커밋할 스테이지된 변경이 없습니다.")
    } else {
        None
    }
}

/// `<Korean text>\n<git's text>`, or just git's text when it is not a known failure.
fn describe(git_text: &str) -> String {
    match korean_message(git_text) {
        Some(k) if git_text.is_empty() => k.to_string(),
        Some(k) => format!("{k}\n{git_text}"),
        None if git_text.is_empty() => "git 작업이 실패했습니다.".to_string(),
        None => format!("git 작업이 실패했습니다.\n{git_text}"),
    }
}

/// Resolve the repository path through the policy and check it is inside a work tree.
async fn open(policy: &PathPolicy, repo: &str) -> Result<PathBuf, GitError> {
    let dir = policy.resolve_existing(Path::new(repo))?;
    let o = run(&dir, &["rev-parse", "--is-inside-work-tree"], None).await?;
    if o.code == 0 && o.stdout.trim() == "true" {
        return Ok(dir);
    }
    let text = message(&o);
    if o.code == 0 {
        // `false`: a bare repository or the inside of a `.git` directory.
        return Err(GitError::Conflict(format!(
            "작업 트리가 없는 저장소(bare)이거나 .git 안쪽입니다: {repo}"
        )));
    }
    if text.to_lowercase().contains("not a git repository") {
        return Err(GitError::NotARepo(format!("git 저장소가 아닙니다: {repo}")));
    }
    // Dubious ownership and any other refusal are not "not a repository".
    Err(GitError::Conflict(describe(&text)))
}

/// Resolve the worktree root so root-relative git paths work from any subdirectory.
pub(crate) async fn worktree_root(policy: &PathPolicy, repo: &str) -> Result<PathBuf, GitError> {
    let dir = open(policy, repo).await?;
    let out = run_ok(&dir, &["rev-parse", "--show-toplevel"], None).await?;
    let root = out.stdout.strip_suffix('\n').unwrap_or(&out.stdout);
    Ok(policy.resolve_existing(Path::new(root))?)
}

/// A ref, remote or branch name must not look like an option.
fn name_arg<'a>(what: &str, v: &'a str) -> Result<&'a str, GitError> {
    if v.is_empty() || v.starts_with('-') || v.contains('\0') {
        return Err(GitError::Bad(format!("잘못된 {what} 값입니다: {v:?}")));
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

/// Patch of one untracked path against `/dev/null`; exit status 1 means "differences found".
async fn untracked_patch(dir: &Path, p: &str) -> Result<String, GitError> {
    let o = run(
        dir,
        &["diff", "--no-color", "--no-ext-diff", "--no-textconv", "--no-index", "--", "/dev/null", p],
        None,
    )
    .await?;
    if o.truncated {
        return Err(too_large());
    }
    if o.code > 1 {
        return Err(GitError::Conflict(describe(&message(&o))));
    }
    Ok(o.stdout)
}

/// Diff of one file or all. With `include_untracked` (and no `path`, not staged) the untracked
/// files follow the tracked changes in the same patch; the total is capped by [`max_output`].
pub async fn diff(policy: &PathPolicy, r: &GitDiffRequest) -> Result<GitDiff, GitError> {
    let dir = open(policy, &r.repo).await?;
    if let Some(p) = r.path.as_deref().filter(|_| !r.staged) {
        let u = run_ok(&dir, &["ls-files", "--others", "--exclude-standard", "-z", "--", p], None).await?;
        if !u.stdout.is_empty() {
            return Ok(GitDiff { patch: untracked_patch(&dir, p).await? });
        }
    }
    let mut args = vec!["diff", "--no-color", "--no-ext-diff", "--no-textconv"];
    if r.staged {
        args.push("--cached");
    }
    if let Some(p) = r.path.as_deref() {
        args.push("--");
        args.push(p);
    }
    let mut patch = run_ok(&dir, &args, None).await?.stdout;
    if r.include_untracked && r.path.is_none() && !r.staged {
        let u = run_ok(&dir, &["ls-files", "--others", "--exclude-standard", "-z"], None).await?;
        for p in u.stdout.split('\0').filter(|p| !p.is_empty()) {
            patch.push_str(&untracked_patch(&dir, p).await?);
            if patch.len() > max_output() {
                return Err(too_large());
            }
        }
    }
    Ok(GitDiff { patch })
}

fn paths_of(r: &GitPathsRequest) -> Result<(), GitError> {
    if r.paths.is_empty() {
        return Err(GitError::Bad("paths는 비어 있을 수 없습니다.".into()));
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
        return Err(GitError::Bad("patch가 비어 있습니다.".into()));
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
        return Err(GitError::Bad("커밋 요약이 비어 있습니다.".into()));
    }
    let dir = open(policy, &r.repo).await?;
    if !r.amend {
        // `--quiet` exits 1 when the index differs from HEAD (or from the empty tree).
        let d = run(&dir, &["diff", "--cached", "--quiet"], None).await?;
        if d.code == 0 {
            return Err(GitError::Conflict("커밋할 스테이지된 변경이 없습니다.".into()));
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

/// The remote argument must be one of the configured `git remote` names (never a URL or a
/// transport), so a client cannot make the node talk to an arbitrary address.
async fn remote_arg(dir: &Path, r: &GitRemoteRequest) -> Result<Option<String>, GitError> {
    let Some(v) = r.remote.as_deref() else { return Ok(None) };
    let v = name_arg("원격 저장소 이름", v)?;
    check_remote(dir, v).await?;
    Ok(Some(v.to_string()))
}

async fn check_remote(dir: &Path, name: &str) -> Result<(), GitError> {
    let o = run_ok(dir, &["remote"], None).await?;
    let names: Vec<&str> = o.stdout.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if names.contains(&name) {
        Ok(())
    } else {
        Err(GitError::Bad(format!(
            "알 수 없는 원격 저장소입니다: {name} (설정된 원격: {})",
            if names.is_empty() { "없음".to_string() } else { names.join(", ") }
        )))
    }
}

fn detached() -> GitError {
    GitError::Conflict(korean_message("not currently on a branch").unwrap_or("detached HEAD").to_string())
}

/// `push` and `pull` need a branch: refuse a detached HEAD before touching the network.
async fn require_branch(dir: &Path) -> Result<(), GitError> {
    if run(dir, &["symbolic-ref", "-q", "HEAD"], None).await?.code == 0 {
        Ok(())
    } else {
        Err(detached())
    }
}

async fn has_upstream(dir: &Path) -> Result<bool, GitError> {
    Ok(run(dir, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"], None).await?.code == 0)
}

/// No lock is needed: only remote-tracking refs change, and the call may take a long time.
pub async fn fetch(policy: &PathPolicy, r: &GitRemoteRequest) -> Result<GitOutput, GitError> {
    let dir = open(policy, &r.repo).await?;
    let remote = remote_arg(&dir, r).await?;
    let mut args = vec!["fetch"];
    args.extend(remote.as_deref());
    run_net_ok(&dir, &args).await.map(|o| combined(&o))
}

/// First half of a pull: the checks and the network fetch. Needs no lock.
pub async fn pull_fetch(policy: &PathPolicy, r: &GitRemoteRequest) -> Result<GitOutput, GitError> {
    let dir = open(policy, &r.repo).await?;
    let remote = remote_arg(&dir, r).await?;
    require_branch(&dir).await?;
    if !has_upstream(&dir).await? {
        return Err(GitError::Conflict(korean_message("no tracking information").unwrap_or("no upstream").to_string()));
    }
    // The merge takes the upstream, so pulling from another remote would merge what was not
    // fetched. Refuse before the network is touched.
    if let Some(name) = remote.as_deref() {
        let branch = run_ok(&dir, &["symbolic-ref", "--short", "HEAD"], None).await?.stdout.trim().to_string();
        let key = format!("branch.{branch}.remote");
        let up = run(&dir, &["config", "--get", &key], None).await?.stdout.trim().to_string();
        if !up.is_empty() && up != name {
            return Err(GitError::Conflict(format!(
                "현재 브랜치의 업스트림은 {up}입니다. 그 원격으로 풀 하세요."
            )));
        }
    }
    let mut args = vec!["fetch"];
    args.extend(remote.as_deref());
    run_net_ok(&dir, &args).await.map(|o| combined(&o))
}

/// Second half of a pull: fast-forward to the upstream. Local only (index and work tree), so
/// the caller holds the write lock for this part.
pub async fn pull_merge(policy: &PathPolicy, r: &GitRepoRequest) -> Result<GitOutput, GitError> {
    let dir = open(policy, &r.repo).await?;
    require_branch(&dir).await?;
    run_ok(&dir, &["merge", "--ff-only", "@{u}"], None).await.map(|o| combined(&o))
}

/// Fast-forward only: a diverged branch is refused with a Korean message and git's text. This
/// is `pull_fetch` then `pull_merge`; the HTTP handler calls them separately to hold the write
/// lock only for the merge.
pub async fn pull(policy: &PathPolicy, r: &GitRemoteRequest) -> Result<GitOutput, GitError> {
    let f = pull_fetch(policy, r).await?;
    let m = pull_merge(policy, &GitRepoRequest { repo: r.repo.clone() }).await?;
    Ok(GitOutput { output: [f.output, m.output].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join("\n") })
}

/// Push the current branch. When it has no upstream yet, it is published and tracked
/// (`push -u <remote|origin> HEAD`). Needs no lock: nothing local changes but tracking refs.
pub async fn push(policy: &PathPolicy, r: &GitRemoteRequest) -> Result<GitOutput, GitError> {
    let dir = open(policy, &r.repo).await?;
    let remote = remote_arg(&dir, r).await?;
    require_branch(&dir).await?;
    let mut args = vec!["push"];
    if has_upstream(&dir).await? {
        args.extend(remote.as_deref());
    } else {
        let name = remote.as_deref().unwrap_or("origin");
        check_remote(&dir, name).await?;
        args.extend(["-u", name, "HEAD"]);
    }
    run_net_ok(&dir, &args).await.map(|o| combined(&o))
}

pub async fn conflicts(policy: &PathPolicy, r: &GitRepoRequest) -> Result<GitConflicts, GitError> {
    let dir = open(policy, &r.repo).await?;
    let o = run_ok(&dir, &["diff", "--name-only", "--diff-filter=U", "-z"], None).await?;
    Ok(GitConflicts { paths: o.stdout.split('\0').filter(|p| !p.is_empty()).map(str::to_string).collect() })
}
