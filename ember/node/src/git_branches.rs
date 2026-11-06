//! Local branches with upstream tracking, last commit and worktree (SPEC FR-G9).
use serde::{Deserialize, Serialize};
use super::{finish, open, run, GitError, GitRepoRequest};
use crate::policy::PathPolicy;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct GitBranchesResponse { pub branches: Vec<GitBranchInfo> }

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct GitBranchInfo {
    pub name: String,
    pub is_current: bool,
    pub is_default: bool,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub last_sha: String,
    pub last_subject: String,
    pub last_time: Option<String>,
    pub worktree: Option<String>,
}

/// One parsed `for-each-ref` line, plus the commit time in unix seconds for ordering.
#[derive(Debug, Clone, PartialEq)]
pub struct RawBranch {
    pub info: GitBranchInfo,
    pub unix: i64,
}

/// `%(upstream:track)` text to (ahead, behind): `[ahead 2, behind 1]`, `[ahead 2]`, `[behind 1]`,
/// `[gone]` and the empty string (in sync, or no upstream) are the forms git prints.
pub fn parse_track(track: &str) -> (u32, u32) {
    let inner = track.trim().trim_start_matches('[').trim_end_matches(']');
    let (mut ahead, mut behind) = (0, 0);
    for part in inner.split(',') {
        let part = part.trim();
        if let Some(n) = part.strip_prefix("ahead ") { ahead = n.trim().parse().unwrap_or(0); }
        else if let Some(n) = part.strip_prefix("behind ") { behind = n.trim().parse().unwrap_or(0); }
    }
    (ahead, behind)
}

const FORMAT: &str = "--format=%(refname:short)%1f%(HEAD)%1f%(upstream:short)%1f%(upstream:track)%1f%(objectname:short)%1f%(committerdate:unix)%1f%(committerdate:iso-strict)%1f%(worktreepath)%1f%(contents:subject)";

/// Split the output of [`FORMAT`]: one line per branch, nine fields separated by U+001F. The
/// subject is last, so it may hold any text (spaces, `|`); a subject cannot hold a newline.
pub fn parse_lines(out: &str) -> Result<Vec<RawBranch>, GitError> {
    let bad = || GitError::Internal("git 브랜치 출력 형식이 잘못되었습니다.".into());
    let mut branches = Vec::new();
    for line in out.lines().filter(|l| !l.is_empty()) {
        let f: Vec<&str> = line.splitn(9, '\x1f').collect();
        if f.len() != 9 { return Err(bad()); }
        let non_empty = |s: &str| (!s.is_empty()).then(|| s.to_owned());
        let (ahead, behind) = parse_track(f[3]);
        branches.push(RawBranch {
            info: GitBranchInfo {
                name: f[0].to_owned(),
                is_current: f[1] == "*",
                is_default: false,
                upstream: non_empty(f[2]),
                ahead,
                behind,
                last_sha: f[4].to_owned(),
                last_subject: f[8].to_owned(),
                last_time: non_empty(f[6]),
                worktree: non_empty(f[7]),
            },
            unix: f[5].parse().unwrap_or(0),
        });
    }
    Ok(branches)
}

/// `main` or `master` when present, used when no remote HEAD names the default.
pub fn fallback_default(names: &[&str]) -> Option<String> {
    ["main", "master"].into_iter().find(|n| names.contains(n)).map(str::to_owned)
}

/// Current first, then the default, then the newest last commit first (ties by name).
pub fn sort_branches(branches: &mut [RawBranch]) {
    branches.sort_by(|a, b| {
        b.info.is_current.cmp(&a.info.is_current)
            .then(b.info.is_default.cmp(&a.info.is_default))
            .then(b.unix.cmp(&a.unix))
            .then(a.info.name.cmp(&b.info.name))
    });
}

/// The branch `refs/remotes/<remote>/HEAD` points to, trying `origin` first.
async fn remote_default(dir: &std::path::Path) -> Option<String> {
    let remotes = run(dir, &["remote"], None).await.ok()?;
    let mut names: Vec<&str> = remotes.stdout.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    names.sort_by_key(|n| *n != "origin");
    for remote in names {
        let reference = format!("refs/remotes/{remote}/HEAD");
        let out = run(dir, &["symbolic-ref", "-q", "--short", &reference], None).await.ok()?;
        if out.code == 0 {
            if let Some(branch) = out.stdout.trim().strip_prefix(&format!("{remote}/")) {
                return Some(branch.to_owned());
            }
        }
    }
    None
}

pub async fn list(policy: &PathPolicy, r: &GitRepoRequest) -> Result<GitBranchesResponse, GitError> {
    let dir = open(policy, &r.repo).await?;
    let out = finish(run(&dir, &["for-each-ref", FORMAT, "refs/heads"], None).await?)?;
    let mut branches = parse_lines(&out.stdout)?;
    let names: Vec<&str> = branches.iter().map(|b| b.info.name.as_str()).collect();
    let default = match remote_default(&dir).await {
        Some(name) if names.contains(&name.as_str()) => Some(name),
        _ => fallback_default(&names),
    };
    for b in &mut branches {
        b.info.is_default = default.as_deref() == Some(b.info.name.as_str());
    }
    sort_branches(&mut branches);
    Ok(GitBranchesResponse { branches: branches.into_iter().map(|b| b.info).collect() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(name: &str, current: bool, default: bool, unix: i64) -> RawBranch {
        RawBranch {
            info: GitBranchInfo {
                name: name.into(), is_current: current, is_default: default, upstream: None, ahead: 0, behind: 0,
                last_sha: "abc".into(), last_subject: String::new(), last_time: None, worktree: None,
            },
            unix,
        }
    }

    #[test]
    fn fr_g9_track_parser_reads_every_form() {
        assert_eq!(parse_track("[ahead 2, behind 1]"), (2, 1));
        assert_eq!(parse_track("[ahead 2]"), (2, 0));
        assert_eq!(parse_track("[behind 7]"), (0, 7));
        assert_eq!(parse_track("[gone]"), (0, 0));
        assert_eq!(parse_track(""), (0, 0));
    }

    #[test]
    fn fr_g9_line_splitter_keeps_subjects_with_spaces_and_pipes() {
        let line = "feat/x\x1f*\x1forigin/feat/x\x1f[ahead 2, behind 1]\x1fabc1234\x1f1700000000\x1f2023-11-14T22:13:20+00:00\x1f/w/x\x1fFix: a | b  c\n\
                    old\x1f \x1f\x1f\x1fdef5678\x1f1600000000\x1f2020-09-13T12:26:40+00:00\x1f\x1fsubject\n";
        let parsed = parse_lines(line).unwrap();
        assert_eq!(parsed.len(), 2);
        let first = &parsed[0].info;
        assert_eq!((first.name.as_str(), first.is_current), ("feat/x", true));
        assert_eq!(first.upstream.as_deref(), Some("origin/feat/x"));
        assert_eq!((first.ahead, first.behind), (2, 1));
        assert_eq!(first.last_subject, "Fix: a | b  c");
        assert_eq!(first.worktree.as_deref(), Some("/w/x"));
        assert_eq!(parsed[0].unix, 1_700_000_000);
        let second = &parsed[1].info;
        assert!(!second.is_current);
        assert_eq!((second.upstream.clone(), second.worktree.clone()), (None, None));
        assert!(parse_lines("only\x1ftwo\n").is_err());
    }

    #[test]
    fn fr_g9_default_falls_back_to_main_then_master() {
        assert_eq!(fallback_default(&["dev", "master", "main"]).as_deref(), Some("main"));
        assert_eq!(fallback_default(&["dev", "master"]).as_deref(), Some("master"));
        assert_eq!(fallback_default(&["dev"]), None);
    }

    #[test]
    fn fr_g9_order_is_current_then_default_then_newest() {
        let mut v = vec![raw("old", false, false, 1), raw("new", false, false, 9), raw("main", false, true, 5), raw("cur", true, false, 0)];
        sort_branches(&mut v);
        let names: Vec<&str> = v.iter().map(|b| b.info.name.as_str()).collect();
        assert_eq!(names, ["cur", "main", "new", "old"]);
        let mut v = vec![raw("main", true, true, 5), raw("a", false, false, 7)];
        sort_branches(&mut v);
        assert_eq!(v[0].info.name, "main");
    }
}
