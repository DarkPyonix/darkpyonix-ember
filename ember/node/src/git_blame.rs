//! Blame of one line (SPEC FR-L31.8): who last changed a line of a file, for the editor's
//! status bar, as Code-OSS's git extension shows it.
//!
//! `git blame --porcelain -L <line>,<line> -- <path>` in the work tree root. With `contents`
//! (the editor's unsaved buffer) git blames that text instead of the file on disk
//! (`--contents -`), so a line edited and not saved yet comes back as not committed, and the
//! line numbers are the buffer's. A line that no commit holds (an edit, a new or untracked
//! file, a repository without a commit) answers `commit: null`.
//!
//! The editor's buffer arrives with `\n` line breaks (the widget's text form), while the file may
//! use `\r\n`. Git compares the buffer with the committed blob byte for byte, so a CRLF file sent
//! as LF would come back with every line not committed. When `contents` has no `\r` and the
//! saved file in the work tree has CRLF line endings (more CRLF than bare LF, the rule Monaco
//! uses to pick a document's EOL), the buffer is converted to CRLF first ([`match_eol`]).
//!
//! Limits: `path` is relative to the work tree root with plain components only (no `..`, no
//! absolute path, no NUL), `contents` is at most [`super::max_output`] bytes, and git is stopped
//! after [`blame_timeout`] (504), because a blame walks history and a long one must not hold a
//! request open.
use std::path::{Component, Path};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{has_head, message, run_with, worktree_root, GitError};
use crate::policy::PathPolicy;

/// Default deadline of one blame.
pub const BLAME_TIMEOUT_SECS: u64 = 10;

/// The blame deadline: `EMBER_GIT_BLAME_TIMEOUT_SECS` if set to a positive number (tests), else
/// [`BLAME_TIMEOUT_SECS`].
pub fn blame_timeout() -> Duration {
    let secs = std::env::var("EMBER_GIT_BLAME_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(BLAME_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct GitBlameRequest {
    pub repo: String,
    /// Relative to the work tree root, as `git/status` names files.
    pub path: String,
    /// Counted from 1, as git counts.
    pub line: u32,
    /// The editor's unsaved text, blamed instead of the file on disk.
    #[serde(default)]
    pub contents: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct GitBlameResponse {
    pub line: u32,
    /// `None` when no commit holds the line yet.
    pub commit: Option<GitBlameCommit>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct GitBlameCommit {
    pub sha: String,
    pub author: String,
    pub email: String,
    /// Author time, seconds since the epoch.
    pub time: i64,
    pub summary: String,
}

/// The path must name a file inside the work tree: plain components only.
pub fn check_path(path: &str) -> Result<(), GitError> {
    let p = Path::new(path);
    if path.is_empty() || path.contains('\0') || p.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(GitError::Bad("올바른 저장소 상대 경로가 아닙니다.".into()));
    }
    Ok(())
}

/// One `git blame --porcelain` entry: the header line `<sha> <orig> <final> [<count>]`, the
/// commit's `key value` lines, and the line itself after a tab. A sha of zeros is git's mark
/// for a line no commit holds.
pub fn parse_porcelain(text: &str) -> Result<Option<GitBlameCommit>, GitError> {
    let bad = || GitError::Internal("git blame 출력 형식이 잘못되었습니다.".into());
    let mut lines = text.lines();
    let header = lines.next().ok_or_else(bad)?;
    let sha = header.split(' ').next().filter(|s| s.len() >= 40 && s.bytes().all(|b| b.is_ascii_hexdigit())).ok_or_else(bad)?;
    let (mut author, mut email, mut time, mut summary) = (String::new(), String::new(), None, String::new());
    for line in lines {
        if line.starts_with('\t') {
            break;
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "author" => author = value.to_string(),
            "author-mail" => email = value.trim_start_matches('<').trim_end_matches('>').to_string(),
            "author-time" => time = Some(value.parse::<i64>().map_err(|_| bad())?),
            "summary" => summary = value.to_string(),
            _ => {}
        }
    }
    if sha.bytes().all(|b| b == b'0') {
        return Ok(None);
    }
    Ok(Some(GitBlameCommit { sha: sha.to_string(), author, email, time: time.ok_or_else(bad)?, summary }))
}

/// Whether `saved` (a file's bytes) uses `\r\n` line endings: more CRLF than bare LF.
pub fn is_crlf(saved: &[u8]) -> bool {
    let lf = saved.iter().filter(|b| **b == b'\n').count();
    let crlf = saved.windows(2).filter(|w| w == b"\r\n").count();
    crlf > 0 && crlf * 2 > lf
}

/// `contents` with the saved file's line endings: a buffer with `\n` breaks only becomes
/// `\r\n` when the saved file is CRLF; a buffer that already has `\r` is sent as it is.
pub fn match_eol<'a>(contents: &'a str, saved: Option<&[u8]>) -> std::borrow::Cow<'a, str> {
    match saved {
        Some(saved) if !contents.contains('\r') && contents.contains('\n') && is_crlf(saved) => {
            std::borrow::Cow::Owned(contents.replace('\n', "\r\n"))
        }
        _ => std::borrow::Cow::Borrowed(contents),
    }
}

/// The saved file's bytes, through the path policy (so a symlink cannot lead outside the
/// allowed roots), at most [`super::max_output`] of them; `None` when it cannot be read.
async fn saved_bytes(policy: &PathPolicy, root: &Path, path: &str) -> Option<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let file = policy.resolve_existing(&root.join(path)).ok()?;
    let f = tokio::fs::File::open(file).await.ok()?;
    let mut bytes = Vec::new();
    f.take(super::max_output() as u64).read_to_end(&mut bytes).await.ok()?;
    Some(bytes)
}

pub async fn blame(policy: &PathPolicy, r: &GitBlameRequest) -> Result<GitBlameResponse, GitError> {
    check_path(&r.path)?;
    if r.line == 0 {
        return Err(GitError::Bad("line은 1 이상이어야 합니다.".into()));
    }
    if r.contents.as_ref().is_some_and(|c| c.len() > super::max_output()) {
        return Err(GitError::TooLarge("파일이 너무 커서 blame을 확인할 수 없습니다.".into()));
    }
    let root = worktree_root(policy, &r.repo).await?;
    let uncommitted = GitBlameResponse { line: r.line, commit: None };
    if !has_head(&root).await? {
        return Ok(uncommitted);
    }
    let contents = match r.contents.as_deref() {
        Some(c) => Some(match_eol(c, saved_bytes(policy, &root, &r.path).await.as_deref()).into_owned()),
        None => None,
    };
    if contents.as_ref().is_some_and(|c| c.len() > super::max_output()) {
        return Err(GitError::TooLarge("파일이 너무 커서 blame을 확인할 수 없습니다.".into()));
    }
    let range = format!("{0},{0}", r.line);
    let mut args: Vec<&str> = vec!["blame", "--porcelain", "-L", range.as_str()];
    if r.contents.is_some() {
        args.extend(["--contents", "-"]);
    }
    args.extend(["--", r.path.as_str()]);
    let out = match run_with(&root, &args, contents.as_deref(), Some(blame_timeout())).await {
        Err(GitError::Timeout(_)) => {
            return Err(GitError::Timeout(format!(
                "git blame이 {}초 안에 끝나지 않아 중단했습니다.",
                blame_timeout().as_secs()
            )))
        }
        other => other?,
    };
    if out.truncated {
        return Err(super::too_large());
    }
    if out.code != 0 {
        let text = message(&out);
        let lower = text.to_lowercase();
        // A file that HEAD does not have yet (new or untracked): no line of it is committed.
        if lower.contains("no such path") {
            return Ok(uncommitted);
        }
        if lower.contains("has only") {
            return Err(GitError::Bad(format!("파일에 {}번째 줄이 없습니다.", r.line)));
        }
        return Err(GitError::Conflict(super::describe(&text)));
    }
    Ok(GitBlameResponse { line: r.line, commit: parse_porcelain(&out.stdout)? })
}
