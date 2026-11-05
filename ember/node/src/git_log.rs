//! Repository history (SPEC FR-G5).
use serde::{Deserialize, Serialize};
use super::{finish, has_head, name_arg, open, run, GitError};
use crate::policy::PathPolicy;

fn default_limit() -> u32 { 50 }

#[derive(Debug, Deserialize, Serialize)]
pub struct GitLogRequest {
    pub repo: String,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub skip: u64,
    #[serde(default)]
    pub branch: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GitLogResponse { pub commits: Vec<GitLogCommit> }

#[derive(Debug, Deserialize, Serialize)]
pub struct GitLogCommit {
    pub sha: String,
    pub parents: Vec<String>,
    pub author: String,
    pub email: String,
    pub time: i64,
    pub subject: String,
    pub refs: Vec<GitLogRef>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GitLogRef { pub name: String, pub kind: GitLogRefKind }

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GitLogRefKind { Branch, Remote, Tag, Head }

pub async fn log(policy: &PathPolicy, r: &GitLogRequest) -> Result<GitLogResponse, GitError> {
    if r.limit > 500 {
        return Err(GitError::Bad("limit은 500을 넘을 수 없습니다.".into()));
    }
    if let Some(branch) = &r.branch { name_arg("branch", branch)?; }
    let dir = open(policy, &r.repo).await?;
    if !has_head(&dir).await? {
        let head = run(&dir, &["symbolic-ref", "-q", "HEAD"], None).await?;
        let current = head.stdout.trim();
        if r.branch.as_deref().map_or(true, |branch| {
            head.code == 0 && (branch == current || current.strip_prefix("refs/heads/") == Some(branch))
        }) {
            return Ok(GitLogResponse { commits: Vec::new() });
        }
    }
    let limit = format!("--max-count={}", r.limit);
    let skip = format!("--skip={}", r.skip);
    let revision = r.branch.as_deref().unwrap_or("HEAD");
    let out = finish(run(&dir, &[
        "log", "--topo-order", "--decorate=full", "--no-color", &limit, &skip,
        "--format=%x1e%H%x00%P%x00%an%x00%ae%x00%at%x00%s%x00%(decorate:prefix=,suffix=,separator=%x1f,pointer=%x1f,tag=)",
        "--end-of-options", revision, "--",
    ], None).await?)?;
    let mut commits = Vec::new();
    let records = out.stdout.strip_prefix('\x1e').unwrap_or(&out.stdout);
    for record in records.split("\n\x1e").filter(|s| !s.is_empty()) {
        let mut fields = record.trim_end_matches('\n').split('\0');
        let mut next = || fields.next().ok_or_else(|| GitError::Internal("git 로그 출력 형식이 잘못되었습니다.".into()));
        let sha = next()?.to_owned();
        let parents = next()?.split_whitespace().map(str::to_owned).collect();
        let author = next()?.to_owned();
        let email = next()?.to_owned();
        let time = next()?.parse().map_err(|_| GitError::Internal("git 로그 시간 형식이 잘못되었습니다.".into()))?;
        let subject = next()?.to_owned();
        let refs = fields.flat_map(|field| field.split('\x1f')).filter_map(|name| {
            let kind = if name == "HEAD" { GitLogRefKind::Head }
                else if name.starts_with("refs/heads/") { GitLogRefKind::Branch }
                else if name.starts_with("refs/remotes/") { GitLogRefKind::Remote }
                else if name.starts_with("refs/tags/") { GitLogRefKind::Tag }
                else { return None };
            Some(GitLogRef { name: name.to_owned(), kind })
        }).collect();
        commits.push(GitLogCommit { sha, parents, author, email, time, subject, refs });
    }
    Ok(GitLogResponse { commits })
}
