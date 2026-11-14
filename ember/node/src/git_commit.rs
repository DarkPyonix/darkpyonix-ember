//! Commit detail, revert and cherry-pick (SPEC FR-G14, FR-G15).
use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::git_log::{ref_of, GitLogRef};
use super::{message, open, run, GitError, Out};
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

pub(super) fn cut_chars(text: &str, max: usize) -> String {
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

pub(super) const GENERIC_FAILURE: &str = "git 작업에 실패했습니다.";
const IN_PROGRESS: &str = "진행 중인 git 작업이 있습니다. 먼저 끝내거나 중단하세요.";
const ABORT_FAILED: &str = "되돌리기를 취소하지 못했습니다. 저장소에 진행 중인 작업이 남아 있을 수 있으니 터미널에서 git status를 확인하세요.";
const TOO_MANY_FILES: &str = "변경된 파일이 너무 많아 목록을 만들 수 없습니다.";
/// The user's hooks never run through the API (SPEC FR-G15).
pub(super) const NO_HOOKS: [&str; 2] = ["-c", "core.hooksPath=/dev/null"];

/// Korean text for a failed git call. Git's own text is never returned to the client.
pub fn failure_message(git_text: &str) -> &'static str {
    let t = git_text.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| t.contains(n));
    if has(&["index.lock", "another git process"]) {
        "다른 git 작업이 진행 중입니다. 잠시 뒤 다시 시도하세요."
    } else if has(&["please tell me who you are", "unable to auto-detect", "empty ident name", "no email was given", "no name was given", "author identity unknown"]) {
        "git 사용자 이름과 이메일이 설정되어 있지 않습니다."
    } else if has(&["nothing to commit", "is now empty", "empty commit", "nothing added to commit"]) {
        "적용할 변경이 없어 건너뛰었습니다."
    } else if has(&["conflict"]) {
        "충돌이 발생해 취소했습니다."
    } else {
        GENERIC_FAILURE
    }
}

/// The answer after `<verb> --abort` ran with `abort_exit_code`: the cancel message only when
/// the abort worked, else the honest "could not cancel".
pub fn abort_outcome(abort_exit_code: i32, cancelled: &str) -> GitError {
    if abort_exit_code == 0 {
        GitError::Conflict(cancelled.to_owned())
    } else {
        GitError::Conflict(ABORT_FAILED.to_owned())
    }
}

/// Drop control characters (C0, DEL, C1, ESC). `\n` and `\t` stay when `keep_whitespace`.
pub fn strip_controls(text: &str, keep_whitespace: bool) -> String {
    text.chars()
        .filter(|c| if *c == '\n' || *c == '\t' { keep_whitespace } else { !c.is_control() })
        .collect()
}

/// Run git; a failure becomes a Korean message with no git text (the text is only logged).
pub(super) async fn run_k(dir: &Path, args: &[&str], too_big: &str) -> Result<Out, GitError> {
    let o = run(dir, args, None).await?;
    if o.truncated {
        return Err(GitError::TooLarge(too_big.to_owned()));
    }
    if o.code != 0 {
        tracing::debug!(git_text = %message(&o), "git call failed");
        return Err(GitError::Conflict(failure_message(&message(&o)).to_owned()));
    }
    Ok(o)
}

/// True when a cherry-pick, revert, merge or rebase is in progress in the repository.
async fn in_progress(dir: &Path) -> Result<bool, GitError> {
    let o = run_k(dir, &[
        "rev-parse",
        "--git-path", "CHERRY_PICK_HEAD", "--git-path", "REVERT_HEAD", "--git-path", "MERGE_HEAD",
        "--git-path", "sequencer", "--git-path", "rebase-merge", "--git-path", "rebase-apply",
    ], GENERIC_FAILURE).await?;
    Ok(o.stdout.lines().any(|p| !p.is_empty() && dir.join(p).exists()))
}

pub async fn show(policy: &PathPolicy, r: &GitCommitShowRequest) -> Result<GitCommitShowResponse, GitError> {
    let sha = check_sha(&r.sha)?;
    let dir = open(policy, &r.repo).await?;
    let full = resolve(&dir, sha).await?;
    let meta = run_k(&dir, &[
        "show", "--no-patch", "--no-color", "--decorate=full",
        "--format=%H%x00%P%x00%an%x00%aI%x00%s%x00%(decorate:prefix=,suffix=,separator=%x1f,pointer=%x1f,tag=)%x00%b",
        "--end-of-options", &full,
    ], GENERIC_FAILURE).await?;
    let mut fields = meta.stdout.splitn(7, '\0');
    let mut next = || fields.next().ok_or_else(bad_output);
    let sha_full = next()?.to_owned();
    let parents: Vec<String> = next()?.split_whitespace().map(str::to_owned).collect();
    let author = strip_controls(next()?, false);
    let time = next()?.to_owned();
    let subject = strip_controls(next()?, false);
    let refs = next()?.split('\x1f').filter_map(ref_of).collect();
    let body = cut_chars(&strip_controls(next()?.trim(), true), MAX_BODY_CHARS);

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
    let names = run_k(&dir, &name_args, TOO_MANY_FILES).await?;
    let stats = run_k(&dir, &stat_args, TOO_MANY_FILES).await?;
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
    // An operation the user (or another tool) left paused is theirs: refuse, never touch it.
    if in_progress(&dir).await? {
        return Err(GitError::Conflict(IN_PROGRESS.into()));
    }
    let full = resolve(&dir, sha).await?;
    let dirty = run_k(&dir, &["status", "--porcelain", "--untracked-files=no"], GENERIC_FAILURE).await?;
    if !dirty.stdout.trim().is_empty() {
        return Err(GitError::Conflict("작업 중인 변경이 있어 되돌릴 수 없습니다. 먼저 커밋하거나 스태시하세요.".into()));
    }
    let listed = run_k(&dir, &["rev-list", "--parents", "-n", "1", &full], GENERIC_FAILURE).await?;
    let is_merge = listed.stdout.split_whitespace().count() > 2;
    let (verb, op_args): (&str, Vec<&str>) = match action {
        Action::Revert if is_merge => ("revert", vec!["revert", "--no-edit", "-m", "1", &full]),
        Action::Revert => ("revert", vec!["revert", "--no-edit", &full]),
        Action::CherryPick if is_merge => {
            return Err(GitError::Bad("병합 커밋은 지원하지 않습니다.".into()));
        }
        Action::CherryPick => ("cherry-pick", vec!["cherry-pick", &full]),
    };
    let before = run_k(&dir, &["rev-parse", "HEAD"], GENERIC_FAILURE).await?.stdout.trim().to_owned();
    let args: Vec<&str> = NO_HOOKS.iter().copied().chain(op_args).collect();
    let out = run(&dir, &args, None).await?;
    if out.code != 0 {
        let combined = format!("{}\n{}", out.stderr.trim(), out.stdout.trim());
        tracing::debug!(git_text = %combined.trim(), verb, "git operation failed");
        let msg = failure_message(&combined);
        // Abort only an operation this call started: the pre-check made sure none was there
        // before, so a leftover state now is ours. A failure before it started (a lock, say)
        // leaves nothing to abort.
        if in_progress(&dir).await.unwrap_or(true) {
            let abort = run(&dir, &[NO_HOOKS[0], NO_HOOKS[1], verb, "--abort"], None).await?;
            if abort.code != 0 {
                tracing::debug!(git_text = %message(&abort), verb, "git abort failed");
            }
            return Err(abort_outcome(abort.code, msg));
        }
        // Nothing is paused, yet git may have left the change staged (a missing identity stops
        // the commit after the apply). The tracked tree was clean before, so put it back.
        let now = run(&dir, &["rev-parse", "HEAD"], None).await?;
        if now.code == 0 && now.stdout.trim() == before {
            let reset = run(&dir, &["reset", "--hard", "-q", &before], None).await?;
            if reset.code != 0 {
                tracing::debug!(git_text = %message(&reset), verb, "git reset after failure failed");
            }
        }
        return Err(GitError::Conflict(msg.to_owned()));
    }
    let head = run_k(&dir, &["rev-parse", "HEAD"], GENERIC_FAILURE).await?;
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

    #[test]
    fn fr_g15_failure_text_maps_to_korean_only() {
        let cases = [
            ("fatal: Unable to create '/r/.git/index.lock': File exists.", "다른 git 작업이 진행 중입니다. 잠시 뒤 다시 시도하세요."),
            ("*** Please tell me who you are.", "git 사용자 이름과 이메일이 설정되어 있지 않습니다."),
            ("fatal: unable to auto-detect email address", "git 사용자 이름과 이메일이 설정되어 있지 않습니다."),
            ("The previous cherry-pick is now empty, possibly due to conflict resolution.", "적용할 변경이 없어 건너뛰었습니다."),
            ("nothing to commit, working tree clean", "적용할 변경이 없어 건너뛰었습니다."),
            ("CONFLICT (content): Merge conflict in a", "충돌이 발생해 취소했습니다."),
            ("fatal: something strange", "git 작업에 실패했습니다."),
        ];
        for (text, want) in cases {
            assert_eq!(failure_message(text), want, "{text}");
        }
    }

    #[test]
    fn fr_g15_abort_outcome_claims_cancel_only_when_the_abort_worked() {
        assert_eq!(abort_outcome(0, "충돌이 발생해 취소했습니다.").to_string(), "충돌이 발생해 취소했습니다.");
        for code in [1, 128, -1] {
            let e = abort_outcome(code, "충돌이 발생해 취소했습니다.").to_string();
            assert!(e.starts_with("되돌리기를 취소하지 못했습니다."), "{e}");
            assert!(!e.contains("충돌이 발생해"));
        }
    }

    #[test]
    fn fr_g14_control_characters_are_stripped() {
        assert_eq!(strip_controls("a\x1b[31mb\x07c\r\nd\te\u{7f}", true), "a[31mbc\nd\te");
        assert_eq!(strip_controls("x\ny\tz\x00", false), "xyz");
    }
}
