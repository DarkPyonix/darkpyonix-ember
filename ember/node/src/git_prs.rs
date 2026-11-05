//! Pull requests of the repository's GitHub remote (SPEC FR-G8), read through the machine's own
//! `gh` CLI. The credentials are gh's own on that computer; Ember stores no secret.
//!
//! `gh` runs in the repository with no prompt, no colour and no update notice, under the same
//! network deadline and stdout cap as git. The binary is `gh` on `PATH`, or the program named by
//! `EMBER_GH_BIN` (tests point it at a fake script).
use std::ffi::OsString;
use std::process::Stdio;

use serde::{Deserialize, Serialize};

use super::{exec_cmd, network_timeout, open, GitError, Out, PathPolicy};

const DEFAULT_LIMIT: u32 = 30;
const MAX_LIMIT: u32 = 200;
const JSON_FIELDS: &str = "number,title,state,isDraft,author,headRefName,baseRefName,updatedAt,reviewDecision,statusCheckRollup,mergeable,url";

fn default_limit() -> u32 { DEFAULT_LIMIT }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GitPrsState {
    #[default]
    Open,
    Closed,
    Merged,
    All,
}

impl GitPrsState {
    fn as_arg(self) -> &'static str {
        match self {
            GitPrsState::Open => "open",
            GitPrsState::Closed => "closed",
            GitPrsState::Merged => "merged",
            GitPrsState::All => "all",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GitPrsRequest {
    pub repo: String,
    #[serde(default)]
    pub state: GitPrsState,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct GitPrsResponse { pub prs: Vec<GitPr> }

/// The state of one pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GitPrState { Open, Closed, Merged }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitPrReview { Approved, ChangesRequested, ReviewRequired, None }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct GitPrChecks { pub passed: u32, pub failed: u32, pub pending: u32 }

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GitPrMerge {
    pub can_merge: bool,
    /// Korean text: why the pull request cannot be merged, or that it can.
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct GitPr {
    pub number: u64,
    pub title: String,
    pub state: GitPrState,
    pub draft: bool,
    pub author: String,
    pub head: String,
    pub base: String,
    /// ISO 8601 as gh prints it (`2026-11-05T10:00:00Z`).
    pub updated_at: String,
    pub review: GitPrReview,
    pub checks: GitPrChecks,
    pub merge: GitPrMerge,
    pub url: String,
}

// What `gh pr list --json` prints; every field may be missing or null.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawAuthor { login: Option<String> }

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawCheck {
    conclusion: Option<String>,
    state: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawPr {
    number: u64,
    title: Option<String>,
    state: Option<String>,
    #[serde(rename = "isDraft")]
    is_draft: bool,
    author: Option<RawAuthor>,
    #[serde(rename = "headRefName")]
    head_ref_name: Option<String>,
    #[serde(rename = "baseRefName")]
    base_ref_name: Option<String>,
    #[serde(rename = "updatedAt")]
    updated_at: Option<String>,
    #[serde(rename = "reviewDecision")]
    review_decision: Option<String>,
    #[serde(rename = "statusCheckRollup")]
    status_check_rollup: Option<Vec<RawCheck>>,
    mergeable: Option<String>,
    url: Option<String>,
}

/// The outcome class of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckClass { Passed, Failed, Pending }

fn upper(v: &Option<String>) -> String {
    v.as_deref().unwrap_or("").trim().to_ascii_uppercase()
}

fn classify(c: &RawCheck) -> CheckClass {
    let result = |v: &str| match v {
        "SUCCESS" | "NEUTRAL" | "SKIPPED" => Some(CheckClass::Passed),
        "FAILURE" | "ERROR" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE" => Some(CheckClass::Failed),
        _ => None,
    };
    // A check run reports `conclusion` once it completed (none while queued or running); a
    // status context reports `state`.
    let conclusion = upper(&c.conclusion);
    if !conclusion.is_empty() {
        return result(&conclusion).unwrap_or(CheckClass::Pending);
    }
    let state = upper(&c.state);
    if !state.is_empty() {
        return result(&state).unwrap_or(CheckClass::Pending);
    }
    CheckClass::Pending
}

/// Classify one `statusCheckRollup` item given as JSON (`conclusion`, `status` or `state`).
pub fn check_class(item: &serde_json::Value) -> CheckClass {
    let field = |k: &str| item.get(k).and_then(|v| v.as_str()).map(str::to_owned);
    classify(&RawCheck { conclusion: field("conclusion"), state: field("state") })
}

fn review_of(decision: &Option<String>) -> GitPrReview {
    match upper(decision).as_str() {
        "APPROVED" => GitPrReview::Approved,
        "CHANGES_REQUESTED" => GitPrReview::ChangesRequested,
        "REVIEW_REQUIRED" => GitPrReview::ReviewRequired,
        _ => GitPrReview::None,
    }
}

/// The merge gate of FR-L19: the pull request must be open, not a draft, free of conflicts,
/// with every check passed and an approval. A repository with no checks passes the CI part.
pub fn merge_gate(state: GitPrState, draft: bool, mergeable: &str, review: GitPrReview, checks: GitPrChecks) -> GitPrMerge {
    let no = |reason: String| GitPrMerge { can_merge: false, reason };
    match state {
        GitPrState::Merged => return no("이미 병합되었습니다.".into()),
        GitPrState::Closed => return no("닫힌 풀 리퀘스트입니다.".into()),
        GitPrState::Open => {}
    }
    if draft {
        return no("초안 상태라 병합할 수 없습니다. 검토 준비 후 초안을 해제하세요.".into());
    }
    if mergeable.eq_ignore_ascii_case("CONFLICTING") {
        return no("기준 브랜치와 충돌합니다. 충돌을 먼저 해결해야 합니다.".into());
    }
    if checks.failed > 0 {
        return no(format!("CI 체크가 실패했습니다 (실패 {}개). 통과해야 병합할 수 있습니다.", checks.failed));
    }
    if checks.pending > 0 {
        return no(format!("CI 체크가 아직 진행 중입니다 (대기 {}개). 모두 통과해야 합니다.", checks.pending));
    }
    match review {
        GitPrReview::Approved => GitPrMerge { can_merge: true, reason: "병합할 수 있습니다.".into() },
        GitPrReview::ChangesRequested => no("변경 요청이 있습니다. 수정 후 다시 승인을 받아야 합니다.".into()),
        GitPrReview::ReviewRequired | GitPrReview::None => no("승인이 필요합니다. 리뷰어의 승인을 받아야 합니다.".into()),
    }
}

fn map_pr(raw: RawPr) -> GitPr {
    let state = match upper(&raw.state).as_str() {
        "MERGED" => GitPrState::Merged,
        "CLOSED" => GitPrState::Closed,
        _ => GitPrState::Open,
    };
    let mut checks = GitPrChecks::default();
    for c in raw.status_check_rollup.iter().flatten() {
        match classify(c) {
            CheckClass::Passed => checks.passed += 1,
            CheckClass::Failed => checks.failed += 1,
            CheckClass::Pending => checks.pending += 1,
        }
    }
    let review = review_of(&raw.review_decision);
    let merge = merge_gate(state, raw.is_draft, raw.mergeable.as_deref().unwrap_or(""), review, checks);
    GitPr {
        number: raw.number,
        title: raw.title.unwrap_or_default(),
        state,
        draft: raw.is_draft,
        author: raw.author.and_then(|a| a.login).unwrap_or_default(),
        head: raw.head_ref_name.unwrap_or_default(),
        base: raw.base_ref_name.unwrap_or_default(),
        updated_at: raw.updated_at.unwrap_or_default(),
        review,
        checks,
        merge,
        url: raw.url.unwrap_or_default(),
    }
}

/// Parse the JSON array of `gh pr list --json ...` into [`GitPr`]s.
pub fn parse_prs(json: &str) -> Result<Vec<GitPr>, GitError> {
    let raws: Vec<RawPr> = serde_json::from_str(json.trim())
        .map_err(|e| GitError::Internal(format!("gh 출력 형식이 잘못되었습니다: {e}")))?;
    Ok(raws.into_iter().map(map_pr).collect())
}

/// The Korean text for a known `gh` failure, or `None`.
fn gh_korean(text: &str) -> Option<&'static str> {
    let t = text.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| t.contains(n));
    if has(&["gh auth login", "not logged in", "authentication required", "bad credentials", "http 401"]) {
        Some("GitHub에 로그인되어 있지 않습니다. 이 컴퓨터에서 gh auth login을 실행하세요.")
    } else if has(&["none of the git remotes", "no github", "not a github", "could not determine", "no git remotes", "unable to determine base repo", "no default remote"]) {
        Some("이 저장소에는 GitHub 원격 저장소가 없습니다.")
    } else if has(&["could not resolve host", "error connecting", "connection refused", "network is unreachable", "i/o timeout", "tls handshake"]) {
        Some("GitHub에 연결하지 못했습니다 (네트워크를 확인하세요).")
    } else if has(&["http 403", "rate limit", "resource not accessible"]) {
        Some("GitHub가 요청을 거부했습니다 (권한이나 요청 한도를 확인하세요).")
    } else if has(&["http 404", "could not resolve to a repository"]) {
        Some("GitHub에서 저장소를 찾을 수 없습니다 (원격 주소와 접근 권한을 확인하세요).")
    } else {
        None
    }
}

/// gh's own text is shown as a detail line: at most this many characters, so a long error page
/// never becomes the message.
const MAX_DETAIL_CHARS: usize = 500;

fn clip_detail(text: &str) -> String {
    if text.chars().count() <= MAX_DETAIL_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_DETAIL_CHARS).collect();
    format!("{head}...")
}

fn gh_failure(o: &Out) -> GitError {
    let text = if o.stderr.trim().is_empty() { o.stdout.trim() } else { o.stderr.trim() };
    let text = clip_detail(text);
    let text = text.as_str();
    GitError::Conflict(match (gh_korean(text), text.is_empty()) {
        (Some(k), true) => k.to_string(),
        (Some(k), false) => format!("{k}\n{text}"),
        (None, true) => "gh 작업이 실패했습니다.".to_string(),
        (None, false) => format!("gh 작업이 실패했습니다.\n{text}"),
    })
}

pub async fn list(policy: &PathPolicy, r: &GitPrsRequest) -> Result<GitPrsResponse, GitError> {
    if r.limit == 0 || r.limit > MAX_LIMIT {
        return Err(GitError::Bad(format!("limit은 1에서 {MAX_LIMIT} 사이여야 합니다.")));
    }
    let dir = open(policy, &r.repo).await?;
    let program = std::env::var_os("EMBER_GH_BIN").unwrap_or_else(|| OsString::from("gh"));
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(["pr", "list", "--state", r.state.as_arg(), "--json", JSON_FIELDS, "--limit"])
        .arg(r.limit.to_string())
        .current_dir(&dir)
        .env("GH_PROMPT_DISABLED", "1")
        .env("NO_COLOR", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GH_SPINNER_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env_remove("GH_FORCE_TTY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = exec_cmd(cmd, "gh", None, Some(network_timeout())).await?;
    if out.truncated {
        return Err(super::too_large());
    }
    if out.code != 0 {
        return Err(gh_failure(&out));
    }
    Ok(GitPrsResponse { prs: parse_prs(&out.stdout)? })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_g8_gh_detail_is_clipped() {
        let long = "x".repeat(MAX_DETAIL_CHARS + 50);
        let clipped = clip_detail(&long);
        assert_eq!(clipped.chars().count(), MAX_DETAIL_CHARS + 3);
        assert!(clipped.ends_with("..."));
        assert_eq!(clip_detail("short"), "short");
    }
}
