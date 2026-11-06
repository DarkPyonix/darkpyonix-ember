//! Commit detail, revert and cherry-pick (SPEC FR-G14, FR-G15).
use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::git_log::{ref_of, GitLogRef};
use super::{describe, message, open, run, run_ok, GitError};
use crate::policy::PathPolicy;

/// Files listed in one answer; the rest only count in `files_total`.
pub const MAX_FILES: usize = 500;
/// Characters of the commit message body kept.
pub const MAX_BODY_CHARS: usize = 20000;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GitCommitShowRequest {
    pub repo: String,
    pub sha: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GitCommitActionRequest {
    pub repo: String,
    pub sha: String,
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct GitCommitActionResponse { pub new_sha: String }

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitCommitFileStatus { Added, Modified, Deleted, Renamed, Copied, TypeChanged }

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct GitCommitFile {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub status: GitCommitFileStatus,
    pub additions: u64,
    pub deletions: u64,
    pub binary: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GitCommitShowResponse {
    pub sha: String,
    pub short: String,
    pub subject: String,
    pub body: String,
    pub author: String,
    /// Author time, ISO 8601 strict (`%aI`).
    pub time: String,
    pub parents: Vec<String>,
    pub refs: Vec<GitLogRef>,
    pub files: Vec<GitCommitFile>,
    pub files_total: usize,
    pub truncated: bool,
}

// ---------------------------------------------------------------------------------------------
// Pure parsers

#[derive(Debug, Clone, PartialEq)]
pub struct NameStatus {
    pub status: GitCommitFileStatus,
    pub path: String,
    pub old_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NumStat {
    pub additions: u64,
    pub deletions: u64,
    pub binary: bool,
    pub path: String,
    pub old_path: Option<String>,
}

fn bad_output() -> GitError { GitError::Internal("git 출력 형식이 잘못되었습니다.".into()) }

/// `git diff-tree --name-status -r -M -z`: `S\0path\0`, and `R<score>\0old\0new\0` for a rename
/// or a copy.
pub fn parse_name_status(out: &str) -> Result<Vec<NameStatus>, GitError> {
    let mut tokens = out.split('\0');
    let mut files = Vec::new();
    while let Some(code) = tokens.next() {
        if code.is_empty() { continue; }
        let status = match code.chars().next() {
            Some('A') => GitCommitFileStatus::Added,
            Some('M') | Some('U') => GitCommitFileStatus::Modified,
            Some('D') => GitCommitFileStatus::Deleted,
            Some('R') => GitCommitFileStatus::Renamed,
            Some('C') => GitCommitFileStatus::Copied,
            Some('T') => GitCommitFileStatus::TypeChanged,
            _ => return Err(bad_output()),
        };
        let first = tokens.next().filter(|t| !t.is_empty()).ok_or_else(bad_output)?.to_owned();
        if matches!(status, GitCommitFileStatus::Renamed | GitCommitFileStatus::Copied) {
            let second = tokens.next().filter(|t| !t.is_empty()).ok_or_else(bad_output)?.to_owned();
            files.push(NameStatus { status, path: second, old_path: Some(first) });
        } else {
            files.push(NameStatus { status, path: first, old_path: None });
        }
    }
    Ok(files)
}

/// `git diff-tree --numstat -r -M -z`: `add\tdel\tpath\0`, `add\tdel\t\0old\0new\0` for a rename,
/// and `-\t-` counts for a binary file.
pub fn parse_numstat(out: &str) -> Result<Vec<NumStat>, GitError> {
    let mut tokens = out.split('\0');
    let mut files = Vec::new();
    while let Some(head) = tokens.next() {
        if head.is_empty() { continue; }
        let mut parts = head.splitn(3, '\t');
        let adds = parts.next().ok_or_else(bad_output)?;
        let dels = parts.next().ok_or_else(bad_output)?;
        let path = parts.next().ok_or_else(bad_output)?;
        let binary = adds == "-" && dels == "-";
        let count = |s: &str| if binary { Ok(0) } else { s.parse::<u64>().map_err(|_| bad_output()) };
        let (additions, deletions) = (count(adds)?, count(dels)?);
        let (path, old_path) = if path.is_empty() {
            let old = tokens.next().ok_or_else(bad_output)?.to_owned();
            let new = tokens.next().ok_or_else(bad_output)?.to_owned();
            (new, Some(old))
        } else {
            (path.to_owned(), None)
        };
        files.push(NumStat { additions, deletions, binary, path, old_path });
    }
    Ok(files)
}

/// Join the two listings by path (the new path of a rename). A file the numstat does not
/// mention counts 0/0.
pub fn merge_files(names: Vec<NameStatus>, stats: Vec<NumStat>) -> Vec<GitCommitFile> {
    let by_path: HashMap<String, NumStat> = stats.into_iter().map(|s| (s.path.clone(), s)).collect();
    names.into_iter().map(|n| {
        let stat = by_path.get(&n.path);
        GitCommitFile {
            additions: stat.map_or(0, |s| s.additions),
            deletions: stat.map_or(0, |s| s.deletions),
            binary: stat.map_or(false, |s| s.binary),
            path: n.path,
            old_path: n.old_path,
            status: n.status,
        }
    }).collect()
}

/// 7 to 40 hex characters, else 400.
pub fn check_sha(sha: &str) -> Result<&str, GitError> {
    if (7..=40).contains(&sha.len()) && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(sha)
    } else {
        Err(GitError::Bad("커밋 SHA는 7자에서 40자의 16진수여야 합니다.".into()))
    }
}

fn cut_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => text[..at].to_owned(),
        None => text.to_owned(),
    }
}

// ---------------------------------------------------------------------------------------------
// Operations

/// The full sha of the commit `sha` names, or 404.
async fn resolve(dir: &Path, sha: &str) -> Result<String, GitError> {
    let spec = format!("{sha}^{{commit}}");
    let o = run(dir, &["rev-parse", "--verify", "--quiet", &spec], None).await?;
    let full = o.stdout.trim().to_owned();
    if o.code != 0 || full.len() < 40 {
        return Err(GitError::NotFound("그런 커밋을 찾을 수 없습니다.".into()));
    }
    Ok(full)
}

pub async fn show(policy: &PathPolicy, r: &GitCommitShowRequest) -> Result<GitCommitShowResponse, GitError> {
    let sha = check_sha(&r.sha)?;
    let dir = open(policy, &r.repo).await?;
    let full = resolve(&dir, sha).await?;
    let meta = run_ok(&dir, &[
        "show", "--no-patch", "--no-color", "--decorate=full",
        "--format=%H%x00%P%x00%an%x00%aI%x00%s%x00%(decorate:prefix=,suffix=,separator=%x1f,pointer=%x1f,tag=)%x00%b",
        "--end-of-options", &full,
    ], None).await?;
    let mut fields = meta.stdout.splitn(7, '\0');
    let mut next = || fields.next().ok_or_else(bad_output);
    let sha_full = next()?.to_owned();
    let parents: Vec<String> = next()?.split_whitespace().map(str::to_owned).collect();
    let author = next()?.to_owned();
    let time = next()?.to_owned();
    let subject = next()?.to_owned();
    let refs = next()?.split('\x1f').filter_map(ref_of).collect();
    let body = cut_chars(next()?.trim(), MAX_BODY_CHARS);

    // A merge commit is compared with its first parent, the root commit with the empty tree.
    let mut name_args = vec!["diff-tree", "--no-commit-id", "--name-status", "-r", "-M", "-z"];
    let mut stat_args = vec!["diff-tree", "--no-commit-id", "--numstat", "-r", "-M", "-z"];
    let mut tail: Vec<&str> = Vec::new();
    match parents.first() {
        Some(first) => { tail.push(first); tail.push(&full); }
        None => { name_args.push("--root"); stat_args.push("--root"); tail.push(&full); }
    }
    name_args.extend(&tail);
    stat_args.extend(&tail);
    let names = run_ok(&dir, &name_args, None).await?;
    let stats = run_ok(&dir, &stat_args, None).await?;
    let mut files = merge_files(parse_name_status(&names.stdout)?, parse_numstat(&stats.stdout)?);
    let files_total = files.len();
    let truncated = files_total > MAX_FILES;
    files.truncate(MAX_FILES);
    Ok(GitCommitShowResponse {
        short: sha_full.chars().take(7).collect(),
        sha: sha_full, subject, body, author, time, parents, refs, files, files_total, truncated,
    })
}

#[derive(Clone, Copy, PartialEq)]
enum Action { Revert, CherryPick }

pub async fn revert(policy: &PathPolicy, r: &GitCommitActionRequest) -> Result<GitCommitActionResponse, GitError> {
    act(policy, r, Action::Revert).await
}

pub async fn cherry_pick(policy: &PathPolicy, r: &GitCommitActionRequest) -> Result<GitCommitActionResponse, GitError> {
    act(policy, r, Action::CherryPick).await
}

async fn act(policy: &PathPolicy, r: &GitCommitActionRequest, action: Action) -> Result<GitCommitActionResponse, GitError> {
    let sha = check_sha(&r.sha)?;
    if !r.confirm {
        return Err(GitError::Bad("확인이 필요합니다.".into()));
    }
    let dir = open(policy, &r.repo).await?;
    let full = resolve(&dir, sha).await?;
    let dirty = run_ok(&dir, &["status", "--porcelain", "--untracked-files=no"], None).await?;
    if !dirty.stdout.trim().is_empty() {
        return Err(GitError::Conflict("작업 중인 변경이 있어 되돌릴 수 없습니다. 먼저 커밋하거나 스태시하세요.".into()));
    }
    let listed = run_ok(&dir, &["rev-list", "--parents", "-n", "1", &full], None).await?;
    let is_merge = listed.stdout.split_whitespace().count() > 2;
    let (verb, args): (&str, Vec<&str>) = match action {
        Action::Revert if is_merge => ("revert", vec!["revert", "--no-edit", "-m", "1", &full]),
        Action::Revert => ("revert", vec!["revert", "--no-edit", &full]),
        Action::CherryPick if is_merge => {
            return Err(GitError::Bad("병합 커밋은 지원하지 않습니다.".into()));
        }
        Action::CherryPick => ("cherry-pick", vec!["cherry-pick", &full]),
    };
    let out = run(&dir, &args, None).await?;
    if out.code != 0 {
        let text = message(&out);
        let combined = format!("{}\n{}", out.stderr.trim(), out.stdout.trim());
        // Leave nothing half-done: the abort restores HEAD, the index and the work tree.
        let _ = run(&dir, &[verb, "--abort"], None).await?;
        if combined.to_lowercase().contains("conflict") {
            return Err(GitError::Conflict(format!("충돌이 발생해 취소했습니다.\n{}", combined.trim())));
        }
        return Err(GitError::Conflict(describe(&text)));
    }
    let head = run_ok(&dir, &["rev-parse", "HEAD"], None).await?;
    Ok(GitCommitActionResponse { new_sha: head.stdout.trim().to_owned() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_g14_name_status_parses_all_kinds_and_renames() {
        let out = "A\0new.txt\0M\0src/a.rs\0D\0gone\0R100\0old name\0new name\0C75\0src\0copy\0T\0link\0";
        let files = parse_name_status(out).unwrap();
        assert_eq!(files.len(), 6);
        assert_eq!(files[0], NameStatus { status: GitCommitFileStatus::Added, path: "new.txt".into(), old_path: None });
        assert_eq!(files[2].status, GitCommitFileStatus::Deleted);
        assert_eq!(files[3], NameStatus { status: GitCommitFileStatus::Renamed, path: "new name".into(), old_path: Some("old name".into()) });
        assert_eq!(files[4].status, GitCommitFileStatus::Copied);
        assert_eq!(files[4].old_path.as_deref(), Some("src"));
        assert_eq!(files[5].status, GitCommitFileStatus::TypeChanged);
    }

    #[test]
    fn fr_g14_name_status_rejects_garbage_and_accepts_empty() {
        assert!(parse_name_status("").unwrap().is_empty());
        assert!(parse_name_status("Z\0x\0").is_err());
        assert!(parse_name_status("R90\0only-one\0").is_err());
    }

    #[test]
    fn fr_g14_numstat_parses_counts_binary_and_renames() {
        let out = "3\t1\tsrc/a.rs\0-\t-\timg.png\x000\t0\t\0old\0new\0";
        let files = parse_numstat(out).unwrap();
        assert_eq!(files.len(), 3);
        assert_eq!((files[0].additions, files[0].deletions, files[0].binary), (3, 1, false));
        assert_eq!((files[1].additions, files[1].deletions, files[1].binary), (0, 0, true));
        assert_eq!(files[1].path, "img.png");
        assert_eq!(files[2].path, "new");
        assert_eq!(files[2].old_path.as_deref(), Some("old"));
    }

    #[test]
    fn fr_g14_merge_files_joins_by_new_path() {
        let names = parse_name_status("M\0a\0R90\0old\0new\0A\0b\0").unwrap();
        let stats = parse_numstat("2\t1\ta\x005\t5\t\0old\0new\0").unwrap();
        let files = merge_files(names, stats);
        assert_eq!((files[0].additions, files[0].deletions), (2, 1));
        assert_eq!(files[1].old_path.as_deref(), Some("old"));
        assert_eq!(files[1].additions, 5);
        assert_eq!((files[2].additions, files[2].deletions, files[2].binary), (0, 0, false));
    }

    #[test]
    fn fr_g14_sha_must_be_seven_to_forty_hex() {
        assert!(check_sha("abc1234").is_ok());
        assert!(check_sha(&"a".repeat(40)).is_ok());
        for bad in ["abc123", "", "--all", "abc123g", &"a".repeat(41), "abc 1234"] {
            assert!(matches!(check_sha(bad), Err(GitError::Bad(_))), "{bad:?}");
        }
    }

    #[test]
    fn fr_g14_body_is_cut_at_a_character_boundary() {
        let text = "가".repeat(30000);
        assert_eq!(cut_chars(&text, MAX_BODY_CHARS).chars().count(), MAX_BODY_CHARS);
        assert_eq!(cut_chars("short", MAX_BODY_CHARS), "short");
    }
}
