//! Stash operations (SPEC FR-G6).
use serde::{Deserialize, Serialize};
use super::{finish, open, run, run_ok, GitError, GitRepoRequest, PathPolicy};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitStashPushRequest {
    pub repo: String,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub include_untracked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitStashIndexRequest {
    pub repo: String,
    #[serde(default)]
    pub index: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy)]
pub enum GitStashOperation {
    Apply,
    Pop,
    Drop,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitStashEntry {
    pub index: u32,
    pub message: String,
    pub branch: String,
    pub time: String,
}

pub async fn list(policy: &PathPolicy, request: &GitRepoRequest) -> Result<Vec<GitStashEntry>, GitError> {
    let dir = open(policy, &request.repo).await?;
    let output = run_ok(&dir, &["stash", "list", "--format=%gd%x00%gs%x00%cI"], None).await?;
    output.stdout.lines().map(|line| {
        let mut fields = line.splitn(3, '\0');
        let reference = fields.next().unwrap_or_default();
        let subject = fields.next().unwrap_or_default();
        let time = fields.next().ok_or_else(|| GitError::Internal("잘못된 stash 목록입니다.".into()))?;
        let index = reference.strip_prefix("stash@{").and_then(|s| s.strip_suffix('}'))
            .and_then(|s| s.parse().ok()).ok_or_else(|| GitError::Internal("잘못된 stash 인덱스입니다.".into()))?;
        let (origin, message) = subject.split_once(": ").unwrap_or(("", subject));
        let branch = origin.strip_prefix("On ").or_else(|| origin.strip_prefix("WIP on ")).unwrap_or(origin);
        Ok(GitStashEntry { index, message: message.into(), branch: branch.into(), time: time.into() })
    }).collect()
}

pub async fn push(policy: &PathPolicy, request: &GitStashPushRequest) -> Result<(), GitError> {
    let dir = open(policy, &request.repo).await?;
    let mut args = vec!["stash", "push"];
    if request.include_untracked { args.push("--include-untracked"); }
    if let Some(message) = &request.message {
        if message.contains('\0') { return Err(GitError::Bad("잘못된 stash 메시지입니다.".into())); }
        args.extend(["-m", message.as_str()]);
    }
    run_ok(&dir, &args, None).await?;
    Ok(())
}

pub async fn change(policy: &PathPolicy, request: &GitStashIndexRequest, operation: GitStashOperation) -> Result<(), GitError> {
    let dir = open(policy, &request.repo).await?;
    let index = request.index.as_ref().and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| GitError::Conflict("그런 stash가 없습니다.".into()))?;
    let entries = list(policy, &GitRepoRequest { repo: request.repo.clone() }).await?;
    if !entries.iter().any(|entry| entry.index == index) {
        return Err(GitError::Conflict("그런 stash가 없습니다.".into()));
    }
    let reference = format!("stash@{{{index}}}");
    let operation = match operation {
        GitStashOperation::Apply => "apply",
        GitStashOperation::Pop => "pop",
        GitStashOperation::Drop => "drop",
    };
    let mut output = run(&dir, &["stash", operation, &reference], None).await?;
    if output.code != 0 {
        output.stderr = format!("{}\n{}", output.stderr, output.stdout);
    }
    finish(output)?;
    Ok(())
}
