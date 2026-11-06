//! Merge state, abort and commit (SPEC FR-G16, FR-G17).
use std::path::Path;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

use super::git_commit::{cut_chars, failure_message, run_k, strip_controls, GitCommitActionResponse, GENERIC_FAILURE, NO_HOOKS};
use super::{conflicts, message, open, run, GitError, GitRepoRequest};
use crate::policy::PathPolicy;

/// Characters of the merged branch name kept.
pub const MAX_NAME_CHARS: usize = 100;
/// Bytes of `MERGE_MSG` read; only its first line matters.
const MSG_READ_BYTES: u64 = 8192;

const NO_MERGE: &str = "진행 중인 병합이 없습니다.";
const CONFLICTS_LEFT: &str = "충돌이 남아 있어 병합 커밋을 만들 수 없습니다.";
const ABORT_FAILED: &str = "병합을 취소하지 못했습니다. 저장소에 진행 중인 작업이 남아 있을 수 있으니 터미널에서 git status를 확인하세요.";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GitMergeActionRequest {
    pub repo: String,
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct GitMergeState {
    pub in_progress: bool,
    /// The branch or ref being merged in; best effort, null when unknown.
    pub from: Option<String>,
    /// The current branch; null on a detached HEAD.
    pub into: Option<String>,
    pub conflict_files: usize,
    /// `in_progress` with no unmerged path left.
    pub can_commit: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct GitMergeAborted { pub ok: bool }

/// The name git quotes in the first line of `MERGE_MSG` (`Merge branch 'x' into y`,
/// `Merge remote-tracking branch 'origin/main'`, `Merge commit 'abc'`, `Merge tag 'v1'`).
pub fn merge_from(first_line: &str) -> Option<String> {
    let start = first_line.find('\'')? + 1;
    let len = first_line[start..].find('\'')?;
    clean_name(&first_line[start..start + len])
}

/// Control characters dropped, cut at [`MAX_NAME_CHARS`]; `None` when nothing is left.
pub fn clean_name(raw: &str) -> Option<String> {
    let name = cut_chars(strip_controls(raw, false).trim(), MAX_NAME_CHARS);
    if name.is_empty() { None } else { Some(name) }
}

/// A `git rev-parse --git-path` answer is relative to the directory git ran in.
async fn git_path(dir: &Path, name: &str) -> Result<std::path::PathBuf, GitError> {
    let o = run_k(dir, &["rev-parse", "--git-path", name], GENERIC_FAILURE).await?;
    Ok(dir.join(o.stdout.trim()))
}

async fn merging(dir: &Path) -> Result<bool, GitError> {
    Ok(tokio::fs::try_exists(git_path(dir, "MERGE_HEAD").await?).await.unwrap_or(false))
}

async fn merge_source(dir: &Path) -> Option<String> {
    if let Ok(path) = git_path(dir, "MERGE_MSG").await {
        if let Ok(file) = tokio::fs::File::open(path).await {
            let mut bytes = Vec::new();
            if file.take(MSG_READ_BYTES).read_to_end(&mut bytes).await.is_ok() {
                let text = String::from_utf8_lossy(&bytes);
                if let Some(name) = text.lines().next().and_then(merge_from) {
                    return Some(name);
                }
            }
        }
    }
    let o = run(dir, &["name-rev", "--name-only", "--no-undefined", "--refs=refs/heads/*", "--refs=refs/remotes/*", "MERGE_HEAD"], None).await.ok()?;
    if o.code != 0 { return None; }
    clean_name(o.stdout.trim().trim_start_matches("remotes/"))
}

async fn current_branch(dir: &Path) -> Option<String> {
    let o = run(dir, &["symbolic-ref", "--short", "-q", "HEAD"], None).await.ok()?;
    if o.code != 0 { return None; }
    clean_name(o.stdout.trim())
}

async fn unmerged(policy: &PathPolicy, repo: &str) -> Result<usize, GitError> {
    Ok(conflicts(policy, &GitRepoRequest { repo: repo.to_owned() }).await?.paths.len())
}

/// FR-G16.
pub async fn state(policy: &PathPolicy, r: &GitRepoRequest) -> Result<GitMergeState, GitError> {
    let dir = open(policy, &r.repo).await?;
    let in_progress = merging(&dir).await?;
    let conflict_files = unmerged(policy, &r.repo).await?;
    let (from, into) = if in_progress { (merge_source(&dir).await, current_branch(&dir).await) } else { (None, current_branch(&dir).await) };
    Ok(GitMergeState { in_progress, from, into, conflict_files, can_commit: in_progress && conflict_files == 0 })
}

fn need_confirm(r: &GitMergeActionRequest) -> Result<(), GitError> {
    if r.confirm { Ok(()) } else { Err(GitError::Bad("확인이 필요합니다.".into())) }
}

/// FR-G17: `git merge --abort`.
pub async fn abort(policy: &PathPolicy, r: &GitMergeActionRequest) -> Result<GitMergeAborted, GitError> {
    need_confirm(r)?;
    let dir = open(policy, &r.repo).await?;
    if !merging(&dir).await? {
        return Err(GitError::Conflict(NO_MERGE.into()));
    }
    let out = run(&dir, &[NO_HOOKS[0], NO_HOOKS[1], "merge", "--abort"], None).await?;
    if out.code != 0 {
        tracing::debug!(git_text = %message(&out), "git merge --abort failed");
        return Err(GitError::Conflict(ABORT_FAILED.into()));
    }
    Ok(GitMergeAborted { ok: true })
}

/// FR-G17: `git commit --no-edit` to finish the merge.
pub async fn commit(policy: &PathPolicy, r: &GitMergeActionRequest) -> Result<GitCommitActionResponse, GitError> {
    need_confirm(r)?;
    let dir = open(policy, &r.repo).await?;
    if !merging(&dir).await? {
        return Err(GitError::Conflict(NO_MERGE.into()));
    }
    if unmerged(policy, &r.repo).await? > 0 {
        return Err(GitError::Conflict(CONFLICTS_LEFT.into()));
    }
    let out = run(&dir, &[NO_HOOKS[0], NO_HOOKS[1], "commit", "--no-edit"], None).await?;
    if out.code != 0 {
        let combined = format!("{}\n{}", out.stderr.trim(), out.stdout.trim());
        tracing::debug!(git_text = %combined.trim(), "git merge commit failed");
        return Err(GitError::Conflict(failure_message(&combined).to_owned()));
    }
    let head = run_k(&dir, &["rev-parse", "HEAD"], GENERIC_FAILURE).await?;
    Ok(GitCommitActionResponse { new_sha: head.stdout.trim().to_owned() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_g16_merge_from_reads_the_quoted_name() {
        assert_eq!(merge_from("Merge branch 'feat/x' into main").as_deref(), Some("feat/x"));
        assert_eq!(merge_from("Merge remote-tracking branch 'origin/main'").as_deref(), Some("origin/main"));
        assert_eq!(merge_from("Merge tag 'v1.0'").as_deref(), Some("v1.0"));
        assert_eq!(merge_from("Merge made by something else"), None);
        assert_eq!(merge_from("Merge branch ''"), None);
    }

    #[test]
    fn fr_g16_names_are_cleaned_and_cut() {
        assert_eq!(clean_name("a\x1b[31m\x07b\n").as_deref(), Some("a[31mb"));
        assert_eq!(clean_name("\x1b\x07"), None);
        assert_eq!(clean_name(&"가".repeat(300)).unwrap().chars().count(), MAX_NAME_CHARS);
    }
}
