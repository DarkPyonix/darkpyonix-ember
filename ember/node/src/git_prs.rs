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
    // Detail only (FR-G10): a check run has `name` and `detailsUrl`, a status context `context`
    // and `targetUrl`.
    name: Option<String>,
    context: Option<String>,
    #[serde(rename = "detailsUrl")]
    details_url: Option<String>,
    #[serde(rename = "targetUrl")]
    target_url: Option<String>,
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
    classify(&RawCheck { conclusion: field("conclusion"), state: field("state"), ..Default::default() })
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

/// Run `gh` in the repository with no prompt, no colour and no update notice, under the network
/// deadline and the stdout cap. Arguments are an argv list; there is no shell.
async fn run_gh(policy: &PathPolicy, repo: &str, args: Vec<OsString>) -> Result<Out, GitError> {
    let dir = open(policy, repo).await?;
    let program = std::env::var_os("EMBER_GH_BIN").unwrap_or_else(|| OsString::from("gh"));
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
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
    Ok(out)
}

fn argv<const N: usize>(items: [&str; N]) -> Vec<OsString> {
    items.iter().map(|s| OsString::from(*s)).collect()
}

pub async fn list(policy: &PathPolicy, r: &GitPrsRequest) -> Result<GitPrsResponse, GitError> {
    if r.limit == 0 || r.limit > MAX_LIMIT {
        return Err(GitError::Bad(format!("limit은 1에서 {MAX_LIMIT} 사이여야 합니다.")));
    }
    let mut args = argv(["pr", "list", "--state", r.state.as_arg(), "--json", JSON_FIELDS, "--limit"]);
    args.push(r.limit.to_string().into());
    let out = run_gh(policy, &r.repo, args).await?;
    Ok(GitPrsResponse { prs: parse_prs(&out.stdout)? })
}

// ---- FR-G10 to FR-G12: detail, review, merge, create ----

const DETAIL_FIELDS: &str = "number,title,body,state,isDraft,author,headRefName,headRefOid,baseRefName,url,reviewDecision,mergeable,reviews,comments,statusCheckRollup,reviewRequests,additions,deletions,changedFiles,updatedAt";
/// A review or comment body is cut at this many characters.
const MAX_ITEM_CHARS: usize = 2000;
/// The pull request's own body is cut at this many characters.
const MAX_PR_BODY_CHARS: usize = 20_000;
/// At most this many comments (the newest) are returned.
const MAX_COMMENTS: usize = 50;
const MAX_TITLE_CHARS: usize = 256;
/// A review or pull request body longer than this is refused (it is an argv entry).
const MAX_INPUT_BODY_CHARS: usize = 60_000;
const MAX_PR_NUMBER: u64 = 999_999;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GitPrRef {
    pub repo: String,
    pub number: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GitPrCheckState { Passed, Failed, Pending }

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GitPrCheck {
    pub name: String,
    pub state: GitPrCheckState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitPrReviewState { Approved, ChangesRequested, Commented, Dismissed, Pending }

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GitPrReviewItem {
    pub author: String,
    pub state: GitPrReviewState,
    pub body: String,
    pub submitted_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GitPrComment {
    pub author: String,
    pub body: String,
    pub created_at: String,
}

/// FR-G10: one pull request in full. The list fields of FR-G8 are flattened in (`checks` is the
/// summary count); `check_runs` lists every check.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct GitPrDetail {
    #[serde(flatten)]
    pub pr: GitPr,
    pub body: String,
    pub check_runs: Vec<GitPrCheck>,
    pub reviews: Vec<GitPrReviewItem>,
    /// Oldest first, at most the newest [`MAX_COMMENTS`].
    pub comments: Vec<GitPrComment>,
    pub requested_reviewers: Vec<String>,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    /// The head commit (40 hex characters) the other fields describe; merge pins it.
    pub head_sha: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawReview {
    author: Option<RawAuthor>,
    state: Option<String>,
    body: Option<String>,
    #[serde(rename = "submittedAt")]
    submitted_at: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawComment {
    author: Option<RawAuthor>,
    body: Option<String>,
    #[serde(rename = "createdAt")]
    created_at: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawRequest {
    login: Option<String>,
    slug: Option<String>,
    name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawDetail {
    #[serde(flatten)]
    base: RawPr,
    body: Option<String>,
    reviews: Option<Vec<RawReview>>,
    comments: Option<Vec<RawComment>>,
    #[serde(rename = "reviewRequests")]
    review_requests: Option<Vec<RawRequest>>,
    additions: u64,
    deletions: u64,
    #[serde(rename = "changedFiles")]
    changed_files: u64,
    #[serde(rename = "headRefOid")]
    head_ref_oid: Option<String>,
}

fn cut(text: Option<String>, max: usize) -> String {
    let text = text.unwrap_or_default();
    if text.chars().count() <= max {
        return text;
    }
    let head: String = text.chars().take(max).collect();
    format!("{head}...")
}

fn login(a: Option<RawAuthor>) -> String {
    a.and_then(|a| a.login).unwrap_or_default()
}

fn map_detail(raw: RawDetail) -> GitPrDetail {
    let check_runs = raw.base.status_check_rollup.iter().flatten().map(|c| GitPrCheck {
        name: c.name.clone().filter(|n| !n.is_empty()).or_else(|| c.context.clone()).unwrap_or_default(),
        state: match classify(c) {
            CheckClass::Passed => GitPrCheckState::Passed,
            CheckClass::Failed => GitPrCheckState::Failed,
            CheckClass::Pending => GitPrCheckState::Pending,
        },
        detail_url: c.details_url.clone().filter(|u| !u.is_empty()).or_else(|| c.target_url.clone().filter(|u| !u.is_empty())),
    }).collect();
    let reviews = raw.reviews.unwrap_or_default().into_iter().map(|r| GitPrReviewItem {
        author: login(r.author),
        state: match upper(&r.state).as_str() {
            "APPROVED" => GitPrReviewState::Approved,
            "CHANGES_REQUESTED" => GitPrReviewState::ChangesRequested,
            "COMMENTED" => GitPrReviewState::Commented,
            "DISMISSED" => GitPrReviewState::Dismissed,
            _ => GitPrReviewState::Pending,
        },
        body: cut(r.body, MAX_ITEM_CHARS),
        submitted_at: r.submitted_at.unwrap_or_default(),
    }).collect();
    let mut comments: Vec<GitPrComment> = raw.comments.unwrap_or_default().into_iter().map(|c| GitPrComment {
        author: login(c.author),
        body: cut(c.body, MAX_ITEM_CHARS),
        created_at: c.created_at.unwrap_or_default(),
    }).collect();
    if comments.len() > MAX_COMMENTS {
        comments.drain(..comments.len() - MAX_COMMENTS);
    }
    let requested_reviewers = raw.review_requests.unwrap_or_default().into_iter()
        .filter_map(|r| r.login.or(r.slug).or(r.name).filter(|n| !n.is_empty()))
        .collect();
    GitPrDetail {
        body: cut(raw.body, MAX_PR_BODY_CHARS),
        pr: map_pr(raw.base),
        check_runs,
        reviews,
        comments,
        requested_reviewers,
        additions: raw.additions,
        deletions: raw.deletions,
        changed_files: raw.changed_files,
        head_sha: raw.head_ref_oid.unwrap_or_default(),
    }
}

/// Parse the JSON object of `gh pr view --json ...` into a [`GitPrDetail`].
pub fn parse_detail(json: &str) -> Result<GitPrDetail, GitError> {
    let raw: RawDetail = serde_json::from_str(json.trim())
        .map_err(|e| GitError::Internal(format!("gh 출력 형식이 잘못되었습니다: {e}")))?;
    Ok(map_detail(raw))
}

fn check_number(n: u64) -> Result<String, GitError> {
    if n == 0 || n > MAX_PR_NUMBER {
        return Err(GitError::Bad(format!("풀 리퀘스트 번호는 1에서 {MAX_PR_NUMBER} 사이여야 합니다.")));
    }
    Ok(n.to_string())
}

fn need_confirm(confirm: bool) -> Result<(), GitError> {
    if confirm { Ok(()) } else { Err(GitError::Bad("확인이 필요합니다.".into())) }
}

/// `--flag=value` as one argument, so a value that starts with `-` is never read as a flag.
fn flag_value(flag: &str, value: &str) -> OsString {
    OsString::from(format!("{flag}={value}"))
}

async fn fetch_detail(policy: &PathPolicy, repo: &str, number: u64) -> Result<GitPrDetail, GitError> {
    let mut args = argv(["pr", "view"]);
    args.push(check_number(number)?.into());
    args.extend(argv(["--json", DETAIL_FIELDS]));
    let out = run_gh(policy, repo, args).await?;
    parse_detail(&out.stdout)
}

/// FR-G10.
pub async fn detail(policy: &PathPolicy, r: &GitPrRef) -> Result<GitPrDetail, GitError> {
    check_number(r.number)?;
    fetch_detail(policy, &r.repo, r.number).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitPrReviewAction { Approve, RequestChanges, Comment }

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GitPrReviewRequest {
    pub repo: String,
    pub number: u64,
    pub action: GitPrReviewAction,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GitPrReviewResponse { pub number: u64, pub action: GitPrReviewAction }

fn check_input_body(body: &str) -> Result<(), GitError> {
    if body.chars().count() > MAX_INPUT_BODY_CHARS {
        return Err(GitError::Bad(format!("본문은 {MAX_INPUT_BODY_CHARS}자를 넘을 수 없습니다.")));
    }
    if body.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) {
        return Err(GitError::Bad("본문에 사용할 수 없는 제어 문자가 있습니다.".into()));
    }
    Ok(())
}

/// FR-G11: approve, request changes or comment.
pub async fn review(policy: &PathPolicy, r: &GitPrReviewRequest) -> Result<GitPrReviewResponse, GitError> {
    need_confirm(r.confirm)?;
    let number = check_number(r.number)?;
    let body = r.body.as_deref().unwrap_or("");
    check_input_body(body)?;
    let mut args = argv(["pr", "review"]);
    args.push(number.into());
    match r.action {
        GitPrReviewAction::Approve => {
            args.push("--approve".into());
            if !body.trim().is_empty() {
                args.push(flag_value("--body", body));
            }
        }
        GitPrReviewAction::RequestChanges | GitPrReviewAction::Comment => {
            if body.trim().is_empty() {
                return Err(GitError::Bad("내용을 입력하세요. 변경 요청과 댓글에는 본문이 필요합니다.".into()));
            }
            args.push(if r.action == GitPrReviewAction::RequestChanges { "--request-changes" } else { "--comment" }.into());
            args.push(flag_value("--body", body));
        }
    }
    run_gh(policy, &r.repo, args).await?;
    Ok(GitPrReviewResponse { number: r.number, action: r.action })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GitPrMergeMethod { Merge, Squash, Rebase }

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GitPrMergeRequest {
    pub repo: String,
    pub number: u64,
    pub method: GitPrMergeMethod,
    #[serde(default)]
    pub delete_branch: bool,
    /// The head commit the user saw (40 hex characters). Required: a missing or empty value is a
    /// 400 from `merge`; when it differs from the current head the merge is refused with 409.
    /// `default` keeps a missing field a 400 of ours instead of a deserializer 422.
    #[serde(default)]
    pub head_sha: String,
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GitPrMergeResponse { pub number: u64, pub merged: bool, pub method: GitPrMergeMethod }

/// FR-G11: merge. The pull request is read again first and the merge gate is never bypassed:
/// a closed gate answers 409 with its reason and `gh pr merge` is not run. No `--admin`, no
/// `--auto`. The merge always carries `--match-head-commit=<sha>` (the sha the caller saw), so a push after the read cannot be merged unreviewed.
///
/// `--delete-branch` is gh's: it deletes the remote branch, and when the merged branch is the one
/// checked out in the project's working directory gh may also delete the local branch and switch
/// the checkout to the base branch. The UI offers it only when the user chooses it.
pub async fn merge(policy: &PathPolicy, r: &GitPrMergeRequest) -> Result<GitPrMergeResponse, GitError> {
    need_confirm(r.confirm)?;
    let number = check_number(r.number)?;
    let given = r.head_sha.trim();
    if given.is_empty() {
        return Err(GitError::Bad("확인한 헤드가 필요합니다.".into()));
    }
    if !is_sha(given) {
        return Err(GitError::Bad("head_sha는 40자리 16진수여야 합니다.".into()));
    }
    let current = fetch_detail(policy, &r.repo, r.number).await?;
    if !current.pr.merge.can_merge {
        return Err(GitError::Conflict(current.pr.merge.reason));
    }
    if !current.head_sha.eq_ignore_ascii_case(given) {
        return Err(GitError::Conflict("풀 리퀘스트가 바뀌었습니다. 다시 불러온 뒤 병합하세요.".into()));
    }
    let sha = given.to_ascii_lowercase();
    let mut args = argv(["pr", "merge"]);
    args.push(number.into());
    args.push(match r.method {
        GitPrMergeMethod::Merge => "--merge",
        GitPrMergeMethod::Squash => "--squash",
        GitPrMergeMethod::Rebase => "--rebase",
    }.into());
    args.push(flag_value("--match-head-commit", &sha));
    if r.delete_branch {
        args.push("--delete-branch".into());
    }
    run_gh(policy, &r.repo, args).await?;
    Ok(GitPrMergeResponse { number: r.number, merged: true, method: r.method })
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GitPrCreateRequest {
    pub repo: String,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub head: Option<String>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GitPrCreateResponse { pub number: u64, pub url: String }

fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// git check-ref-format rules for a branch name.
fn valid_ref(v: &str) -> bool {
    !(v.is_empty()
        || v.chars().count() > 256
        || v.starts_with('-')
        || v.starts_with('/')
        || v.ends_with('/')
        || v.ends_with('.')
        || v.ends_with(".lock")
        || v.contains("..")
        || v.contains("@{")
        || v.contains("//")
        || v == "@"
        || v.split('/').any(|part| part.starts_with('.') || part.ends_with(".lock"))
        || v.chars().any(|c| c.is_control() || c.is_whitespace() || "~^:?*[\\".contains(c)))
}

/// `base` is a plain branch; `head` may also be `owner:branch` (a fork).
fn branch_arg(label: &str, v: &Option<String>, allow_owner: bool) -> Result<Option<String>, GitError> {
    let Some(v) = v.as_deref().map(str::trim).filter(|v| !v.is_empty()) else { return Ok(None) };
    let ok = match v.split_once(':') {
        Some((owner, branch)) if allow_owner => {
            !owner.is_empty()
                && owner.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                && !owner.starts_with('-')
                && valid_ref(branch)
        }
        _ => valid_ref(v),
    };
    if !ok {
        return Err(GitError::Bad(format!("{label} 브랜치 이름이 올바르지 않습니다.")));
    }
    Ok(Some(v.to_string()))
}

/// The `(number, url)` of the pull request url gh prints (the last line that is one).
fn created_url(stdout: &str) -> Option<(u64, String)> {
    stdout.lines().rev().map(str::trim).find_map(|line| {
        let rest = line.strip_prefix("https://")?;
        let (_, number) = rest.rsplit_once("/pull/")?;
        let number: u64 = number.trim_end_matches('/').parse().ok()?;
        Some((number, line.to_string()))
    })
}

/// FR-G12: create. The node pushes nothing; the head branch must exist on the remote.
pub async fn create(policy: &PathPolicy, r: &GitPrCreateRequest) -> Result<GitPrCreateResponse, GitError> {
    need_confirm(r.confirm)?;
    let title = r.title.trim();
    if title.is_empty() {
        return Err(GitError::Bad("제목을 입력하세요.".into()));
    }
    if title.chars().count() > MAX_TITLE_CHARS || title.chars().any(char::is_control) {
        return Err(GitError::Bad(format!("제목은 한 줄, {MAX_TITLE_CHARS}자 이내여야 합니다.")));
    }
    let body = r.body.as_deref().unwrap_or("");
    check_input_body(body)?;
    let base = branch_arg("기준", &r.base, false)?;
    let head = branch_arg("작업", &r.head, true)?;
    if let Some(h) = &head {
        // Cheap duplicate check, only for an explicit head (gh itself refuses a duplicate otherwise).
        let mut list = argv(["pr", "list"]);
        list.push(flag_value("--head", h));
        list.extend(argv(["--state", "open", "--json", "number"]));
        let out = run_gh(policy, &r.repo, list).await?;
        let open: Vec<RawPr> = serde_json::from_str(out.stdout.trim()).unwrap_or_default();
        if let Some(first) = open.first() {
            return Err(GitError::Conflict(format!("이미 열린 풀 리퀘스트가 있습니다: #{}", first.number)));
        }
    }
    let mut args = argv(["pr", "create"]);
    args.push(flag_value("--title", title));
    // Always given, so gh never opens an editor or asks.
    args.push(flag_value("--body", body));
    if let Some(b) = base {
        args.push(flag_value("--base", &b));
    }
    if let Some(h) = head {
        args.push(flag_value("--head", &h));
    }
    if r.draft {
        args.push("--draft".into());
    }
    let out = run_gh(policy, &r.repo, args).await?;
    let (number, url) = created_url(&out.stdout)
        .or_else(|| created_url(&out.stderr))
        .ok_or_else(|| GitError::Internal(format!("gh가 풀 리퀘스트 주소를 출력하지 않았습니다.\n{}", clip_detail(out.stdout.trim()))))?;
    Ok(GitPrCreateResponse { number, url })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_g12_created_url_is_parsed() {
        assert_eq!(created_url("Creating...\nhttps://github.com/o/r/pull/42\n"), Some((42, "https://github.com/o/r/pull/42".into())));
        assert_eq!(created_url("no url here"), None);
    }

    #[test]
    fn fr_g11_number_range() {
        assert!(check_number(0).is_err() && check_number(1_000_000).is_err());
        assert!(check_number(1).is_ok() && check_number(999_999).is_ok());
    }

    #[test]
    fn fr_g8_gh_detail_is_clipped() {
        let long = "x".repeat(MAX_DETAIL_CHARS + 50);
        let clipped = clip_detail(&long);
        assert_eq!(clipped.chars().count(), MAX_DETAIL_CHARS + 3);
        assert!(clipped.ends_with("..."));
        assert_eq!(clip_detail("short"), "short");
    }
}
