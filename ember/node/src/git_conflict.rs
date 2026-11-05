//! Conflict file parsing and resolution (FR-G7).
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use crate::git::{self, GitError, GitPathsRequest, GitRepoRequest};
use crate::policy::PathPolicy;

#[derive(Debug, Deserialize, Serialize)]
pub struct GitConflictRequest { pub repo: String, pub path: String }
#[derive(Debug, Deserialize, Serialize)]
pub struct GitResolveRequest { pub repo: String, pub path: String, pub content: String }
#[derive(Debug, Deserialize, Serialize)]
pub struct GitConflict { pub blocks: Vec<ConflictBlock> }
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ConflictBlock {
    Common { text: String },
    Conflict { ours: String, theirs: String, ours_label: String, theirs_label: String },
}

async fn target(policy: &PathPolicy, repo: &str, path: &str) -> Result<PathBuf, GitError> {
    let p = Path::new(path);
    if path.is_empty() || p.components().any(|c| !matches!(c, Component::Normal(_))) || path.contains('\0') {
        return Err(GitError::Bad("올바른 저장소 상대 경로가 아닙니다.".into()));
    }
    let root = git::worktree_root(policy, repo).await?;
    let mut file = root.clone();
    for component in p.components() {
        file.push(component.as_os_str());
        match tokio::fs::symlink_metadata(&file).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(GitError::Bad("심볼릭 링크는 사용할 수 없습니다.".into()));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(GitError::Internal(format!("경로를 확인하지 못했습니다: {e}"))),
        }
    }
    let paths = git::conflicts(policy, &GitRepoRequest { repo: root.to_string_lossy().into_owned() }).await?.paths;
    if !paths.iter().any(|v| v == path) {
        return Err(GitError::Conflict("병합 충돌이 없는 경로입니다.".into()));
    }
    Ok(root.join(p))
}

pub async fn conflict(policy: &PathPolicy, r: &GitConflictRequest) -> Result<GitConflict, GitError> {
    let path = target(policy, &r.repo, &r.path).await?;
    let file = tokio::fs::File::open(path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            GitError::Conflict("충돌 파일이 작업 트리에 없습니다.".into())
        } else {
            GitError::Internal(format!("파일을 읽지 못했습니다: {e}"))
        }
    })?;
    let mut bytes = Vec::new();
    file.take(git::max_output() as u64 + 1).read_to_end(&mut bytes).await
        .map_err(|e| GitError::Internal(format!("파일을 읽지 못했습니다: {e}")))?;
    if bytes.len() > git::max_output() { return Err(GitError::TooLarge("파일이 너무 큽니다.".into())); }
    let text = String::from_utf8(bytes).map_err(|e| GitError::Bad(format!("UTF-8 파일이 아닙니다: {e}")))?;
    Ok(GitConflict { blocks: parse(&text) })
}

fn label(line: &str, marker: &str) -> Option<String> {
    let rest = line.trim_end_matches(['\r', '\n']).strip_prefix(marker)?;
    if rest.is_empty() { Some(String::new()) } else { rest.strip_prefix(' ').map(str::to_string) }
}

fn parse(text: &str) -> Vec<ConflictBlock> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut blocks = Vec::new();
    let mut common = String::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some(ours_label) = label(lines[i], "<<<<<<<") {
            let start = i;
            i += 1;
            let mut ours = String::new();
            while i < lines.len() && label(lines[i], "|||||||").is_none() && lines[i].trim_end_matches(['\r', '\n']) != "=======" {
                ours.push_str(lines[i]); i += 1;
            }
            if i < lines.len() && label(lines[i], "|||||||").is_some() {
                i += 1;
                while i < lines.len() && lines[i].trim_end_matches(['\r', '\n']) != "=======" { i += 1; }
            }
            if i < lines.len() {
                i += 1;
                let mut theirs = String::new();
                while i < lines.len() && label(lines[i], ">>>>>>>").is_none() { theirs.push_str(lines[i]); i += 1; }
                if i < lines.len() {
                    let theirs_label = label(lines[i], ">>>>>>>").unwrap();
                    if !common.is_empty() { blocks.push(ConflictBlock::Common { text: std::mem::take(&mut common) }); }
                    blocks.push(ConflictBlock::Conflict { ours, theirs, ours_label, theirs_label });
                    i += 1;
                    continue;
                }
            }
            common.push_str(&lines[start..i].concat());
        } else { common.push_str(lines[i]); i += 1; }
    }
    if !common.is_empty() { blocks.push(ConflictBlock::Common { text: common }); }
    blocks
}

pub async fn resolve(policy: &PathPolicy, r: &GitResolveRequest) -> Result<(), GitError> {
    let path = target(policy, &r.repo, &r.path).await?;
    tokio::fs::write(&path, &r.content).await.map_err(|e| GitError::Internal(format!("파일을 쓰지 못했습니다: {e}")))?;
    let root = git::worktree_root(policy, &r.repo).await?;
    git::stage(policy, &GitPathsRequest { repo: root.to_string_lossy().into_owned(), paths: vec![r.path.clone()] }).await
}
