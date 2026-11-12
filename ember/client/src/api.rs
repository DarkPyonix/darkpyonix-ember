//! HTTP client for the main server's `/api` (see `ember/server/src/api/mod.rs`).
//!
//! An [`Api`] reaches the server one of two ways, behind the same methods:
//!
//! - **HTTP** ([`Api::new`]): a base URL over TCP.
//! - **Transport** ([`Api::over_transport`]): the peer-to-peer transport (SPEC `FR-N1`), dialing
//!   the server by its [`PeerAddr`] for service [`SERVER_SERVICE`]. The server admits only
//!   devices on its allow-list (`FR-N3`). Each request runs on a fresh stream of one cached
//!   connection; the push socket ([`Api::open_push`]) gets its own stream.

use std::fmt;
use std::sync::Arc;

use ember_transport::{Dialer, PeerAddr, PowerState};
use http::{header, Method, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::WebSocketStream;

use crate::wire::{
    ApprovalDecision, DailyUsage, DetectedAgent, MentionCandidate, NewSession, Project, SearchHit, SessionDetail, SessionPatch,
    QuestionAnswer, SessionRecord, StoredEvent, TeamMail, TeamMember, TeamView,
};
use crate::channels::{Allowance, AllowanceBody, Channel, ChannelMessage, CreatedInvite, Guest, Invite, FeedAnswer, FeedDecision, FeedItem, MentionPreview, MentionPreviewRequest, MessagesQuery, NewChannel, NewChannelMessage, ReadBody};
use crate::wire::{NewSchedule, Schedule, SchedulePatch, ScheduleRun};

/// Transport service name of the server API (matches `ember_server::transport::SERVER_SERVICE`).
pub const SERVER_SERVICE: &str = "ember-server";

/// Host name used in requests over the transport (the stream already names the peer).
const PEER_HOST: &str = "ember-server";

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("request failed: {0}")]
    Transport(#[from] reqwest::Error),
    /// The peer-to-peer transport failed (dial, stream, or HTTP over a stream).
    #[error("peer connection failed: {0}")]
    Peer(String),
    #[error("server answered {status}: {message}")]
    Status { status: u16, message: String, code: Option<String> },
    /// The server answered 401: the token is missing or wrong (FR-S11). The client never retries
    /// on this, because the same token would be refused again.
    #[error("authentication failed: the server refused the token ({0})")]
    Unauthorized(String),
    /// The server answered 400 `not_a_member` (FR-T13): the message mentions names that are not
    /// members of the channel. Nothing was stored. `unknown` lists those names.
    #[error("server answered 400: {message}")]
    NotAMember { message: String, unknown: Vec<String> },
    #[error("invalid response: {0}")]
    Decode(String),
    #[error("push connection failed: {0}")]
    Push(String),
    #[error("invalid base url {0:?}")]
    BadUrl(String),
}

/// What the person sends for a question: the answers, or a skip (`{"decline":true}`).
/// `Debug` shows only the count: the values may be a password.
#[derive(Clone, PartialEq)]
pub enum QuestionReply {
    Answers(Vec<QuestionAnswer>),
    Skip,
}

impl std::fmt::Debug for QuestionReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QuestionReply::Answers(a) => write!(f, "QuestionReply::Answers({} answers, values hidden)", a.len()),
            QuestionReply::Skip => write!(f, "QuestionReply::Skip"),
        }
    }
}

/// Path of `POST /sessions/{id}/questions/{question_id}/answer`, segments encoded.
pub(crate) fn question_answer_path(id: &str, question_id: &str) -> String {
    format!("/sessions/{}/questions/{}/answer", seg(id), seg(question_id))
}

/// The request body: exactly one of `answers` or `decline` (the server refuses both or neither).
pub(crate) fn question_reply_body(reply: &QuestionReply) -> serde_json::Value {
    match reply {
        QuestionReply::Answers(a) => json!({ "answers": a }),
        QuestionReply::Skip => json!({ "decline": true }),
    }
}

/// Why sending a question's answer failed, as the card tells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionCallError {
    /// 404: unknown session, or the question is not pending any more. Terminal for the card.
    Gone,
    /// 400: the answers do not fit the question.
    Invalid,
    /// 409: the session has no running agent. Terminal for the card.
    NoAgent,
    /// 401 or 403: only the owner answers.
    NotOwner,
    /// 422: the request shape is not understood (app older than the server).
    Unsupported,
    /// Anything else, including transport.
    Failed,
}

impl QuestionCallError {
    pub fn from_error(e: &ApiError) -> QuestionCallError {
        match e {
            ApiError::Unauthorized(_) => QuestionCallError::NotOwner,
            ApiError::Status { status: 404, .. } => QuestionCallError::Gone,
            ApiError::Status { status: 400, .. } | ApiError::NotAMember { .. } => QuestionCallError::Invalid,
            ApiError::Status { status: 409, .. } => QuestionCallError::NoAgent,
            ApiError::Status { status: 401 | 403, .. } => QuestionCallError::NotOwner,
            ApiError::Status { status: 422, .. } => QuestionCallError::Unsupported,
            _ => QuestionCallError::Failed,
        }
    }

    /// A card that cannot be answered any more stays disabled.
    pub fn is_terminal(self) -> bool {
        matches!(self, QuestionCallError::Gone | QuestionCallError::NoAgent)
    }
}

impl QuestionCallError {
    /// The fixed sentence for this failure. Never the server's text, which is English and could
    /// echo input.
    pub fn text_ko(self) -> &'static str {
        match self {
            QuestionCallError::Gone => "이 질문은 이미 답했거나 끝난 질문입니다.",
            QuestionCallError::Invalid => "답이 질문과 맞지 않습니다. 선택을 확인해 주세요.",
            QuestionCallError::NoAgent => "에이전트가 실행 중이 아니어서 답을 보낼 수 없습니다.",
            QuestionCallError::NotOwner => "이 질문에는 소유자만 답할 수 있습니다.",
            QuestionCallError::Unsupported => "답을 보낼 수 없습니다. 앱을 업데이트해 주세요.",
            QuestionCallError::Failed => "답을 보내지 못했습니다. 연결을 확인하고 다시 시도해 주세요.",
        }
    }
}

/// The fixed sentence for a failed answer (see [`QuestionCallError::text_ko`]).
pub fn question_error_ko(e: &ApiError) -> &'static str {
    QuestionCallError::from_error(e).text_ko()
}

/// `POST /sessions/{id}/fork` body: `{turn_seq?}` (server `ForkBody`).
pub(crate) fn fork_body(turn_seq: Option<i64>) -> serde_json::Value {
    match turn_seq {
        Some(seq) => json!({ "turn_seq": seq }),
        None => json!({}),
    }
}

impl ApiError {
    /// The server's JSON `code` (`invalid_hours`, `not_found`, ...), when the error body had one.
    pub fn code(&self) -> Option<&str> {
        match self {
            ApiError::Status { code, .. } => code.as_deref(),
            ApiError::NotAMember { .. } => Some("not_a_member"),
            _ => None,
        }
    }

    pub fn status(&self) -> Option<u16> {
        match self {
            ApiError::Status { status, .. } => Some(*status),
            ApiError::Unauthorized(_) => Some(401),
            ApiError::NotAMember { .. } => Some(400),
            _ => None,
        }
    }
}

fn peer_err(e: impl fmt::Display) -> ApiError {
    ApiError::Peer(e.to_string())
}

pub type ApiResult<T> = Result<T, ApiError>;

// ---- git (FR-G4) ---------------------------------------------------------------------------

/// One changed file of `POST /projects/{id}/git/status`. `index` and `worktree` are the
/// porcelain v2 status characters (`M`, `A`, `D`, `R`, `.`; an untracked file is `?`, `?`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitFile {
    pub path: String,
    #[serde(default)]
    pub old_path: Option<String>,
    pub index: char,
    pub worktree: char,
}

/// `POST /projects/{id}/git/status` answer.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GitStatus {
    pub branch: String,
    #[serde(default)]
    pub upstream: Option<String>,
    #[serde(default)]
    pub ahead: u32,
    #[serde(default)]
    pub behind: u32,
    #[serde(default)]
    pub fetched_age_secs: Option<u64>,
    #[serde(default)]
    pub branches: Vec<String>,
    #[serde(default)]
    pub files: Vec<GitFile>,
}

/// `POST /projects/{id}/git/diff` answer: a unified diff.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GitDiff {
    pub patch: String,
}

/// `POST /projects/{id}/git/commit` answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitCommitted {
    pub sha: String,
}

/// `POST /projects/{id}/git/fetch|pull|push` answer: what git printed.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GitOutput {
    #[serde(default)]
    pub output: String,
}

/// `POST /projects/{id}/git/conflicts` answer: the unmerged paths.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GitConflicts {
    #[serde(default)]
    pub paths: Vec<String>,
}

/// Repository history returned by the node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitLog {
    pub commits: Vec<GitLogCommit>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitLogCommit {
    pub sha: String,
    pub parents: Vec<String>,
    pub author: String,
    pub email: String,
    pub time: i64,
    pub subject: String,
    pub refs: Vec<GitLogRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitLogRef {
    pub name: String,
    pub kind: GitLogRefKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GitLogRefKind {
    Branch,
    Remote,
    Tag,
    Head,
}

/// One commit with its changed files (FR-G14).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitCommitDetail {
    pub sha: String,
    pub short: String,
    pub subject: String,
    #[serde(default)]
    pub body: String,
    pub author: String,
    /// Author time, ISO 8601 strict.
    pub time: String,
    pub parents: Vec<String>,
    #[serde(default)]
    pub refs: Vec<GitLogRef>,
    #[serde(default)]
    pub files: Vec<GitCommitFile>,
    #[serde(default)]
    pub files_total: usize,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitCommitFile {
    pub path: String,
    #[serde(default)]
    pub old_path: Option<String>,
    pub status: GitCommitFileStatus,
    #[serde(default)]
    pub additions: u64,
    #[serde(default)]
    pub deletions: u64,
    #[serde(default)]
    pub binary: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitCommitFileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
}

/// The new HEAD after a revert or cherry-pick (FR-G15).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitCommitApplied {
    pub new_sha: String,
}

/// The merge in progress, for the conflict screen banner (`git/merge`, FR-G16).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GitMergeState {
    #[serde(default)]
    pub in_progress: bool,
    /// The branch or ref being merged in; null when unknown.
    #[serde(default)]
    pub from: Option<String>,
    /// The current branch; null on a detached HEAD.
    #[serde(default)]
    pub into: Option<String>,
    #[serde(default)]
    pub conflict_files: u32,
    /// `in_progress` with no unmerged path left.
    #[serde(default)]
    pub can_commit: bool,
}

/// The answer of `git/merge/abort` (FR-G17).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GitMergeAborted {
    #[serde(default)]
    pub ok: bool,
}

/// Local branches (`POST /projects/{id}/git/branches`, FR-G9).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GitBranches {
    #[serde(default)]
    pub branches: Vec<GitBranchInfo>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GitBranchInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub is_current: bool,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub upstream: Option<String>,
    #[serde(default)]
    pub ahead: u32,
    #[serde(default)]
    pub behind: u32,
    #[serde(default)]
    pub last_sha: String,
    #[serde(default)]
    pub last_subject: String,
    #[serde(default)]
    pub last_time: Option<String>,
    #[serde(default)]
    pub worktree: Option<String>,
}

/// The Home GitHub card (`GET /api/integrations/github`, FR-G13). A number is `None` when its
/// call failed; the card shows that as 알 수 없음.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GithubSummary {
    #[serde(default)]
    pub state: GithubState,
    /// Korean reason when `state` is not `On`.
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub repos: Vec<GithubRepo>,
    #[serde(default)]
    pub totals: GithubTotals,
    /// Epoch seconds of the check.
    #[serde(default)]
    pub checked_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GithubState {
    On,
    Warn,
    #[default]
    Off,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GithubRepo {
    #[serde(default)]
    pub name_with_owner: Option<String>,
    #[serde(default)]
    pub default_branch: Option<String>,
    #[serde(default)]
    pub open_prs: Option<u64>,
    #[serde(default)]
    pub review_requested: Option<u64>,
    /// `open_prs` stopped at the 100 limit.
    #[serde(default)]
    pub capped: bool,
    /// Last workflow runs, newest first: `success`, `failure`, `cancelled`, `pending`.
    #[serde(default)]
    pub ci: Option<Vec<String>>,
    /// Korean text of the repository's first failed call.
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub projects: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GithubTotals {
    /// Repositories listed; null when the state is `off` or none is listed.
    #[serde(default)]
    pub repos: Option<u64>,
    /// Sum of the known values; null when no repository gave one (never an invented 0).
    #[serde(default)]
    pub open_prs: Option<u64>,
    /// Sum of the known values; null when no repository gave one.
    #[serde(default)]
    pub review_requested: Option<u64>,
    /// Null when no run is known.
    #[serde(default)]
    pub ci_pass_percent: Option<u64>,
}

/// Pull requests of the repository's GitHub remote (`POST /projects/{id}/git/prs`, FR-G8).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GitPrs {
    #[serde(default)]
    pub prs: Vec<GitPr>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitPr {
    pub number: u64,
    pub title: String,
    pub state: GitPrState,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub head: String,
    #[serde(default)]
    pub base: String,
    /// ISO 8601 (`2026-11-05T10:00:00Z`).
    #[serde(default)]
    pub updated_at: String,
    pub review: GitPrReview,
    pub checks: GitPrChecks,
    pub merge: GitPrMerge,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GitPrState {
    Open,
    Closed,
    Merged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitPrReview {
    Approved,
    ChangesRequested,
    ReviewRequired,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GitPrChecks {
    #[serde(default)]
    pub passed: u32,
    #[serde(default)]
    pub failed: u32,
    #[serde(default)]
    pub pending: u32,
}

/// The merge gate: `reason` is the node's Korean text, also when `can_merge` is true.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitPrMerge {
    pub can_merge: bool,
    #[serde(default)]
    pub reason: String,
}

/// One pull request in full (`POST /projects/{id}/git/pr`, FR-G10). The list fields of
/// [`GitPr`] are flattened in (`checks` is the summary count); `check_runs` lists every check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitPrDetail {
    #[serde(flatten)]
    pub pr: GitPr,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub check_runs: Vec<GitPrCheck>,
    #[serde(default)]
    pub reviews: Vec<GitPrReviewItem>,
    /// Oldest first, at most the newest 50.
    #[serde(default)]
    pub comments: Vec<GitPrComment>,
    #[serde(default)]
    pub requested_reviewers: Vec<String>,
    #[serde(default)]
    pub additions: u64,
    #[serde(default)]
    pub deletions: u64,
    #[serde(default)]
    pub changed_files: u64,
    /// The head commit (40 hex characters) the fields describe; pass it to `git_pr_merge`.
    #[serde(default)]
    pub head_sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitPrCheck {
    pub name: String,
    pub state: GitPrCheckState,
    #[serde(default)]
    pub detail_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GitPrCheckState {
    Passed,
    Failed,
    Pending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitPrReviewState {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
    Pending,
}

/// A review, its body cut at 2000 characters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitPrReviewItem {
    #[serde(default)]
    pub author: String,
    pub state: GitPrReviewState,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub submitted_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitPrComment {
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitPrReviewAction {
    Approve,
    RequestChanges,
    Comment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GitPrMergeMethod {
    Merge,
    Squash,
    Rebase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitPrReviewed {
    pub number: u64,
    pub action: GitPrReviewAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitPrMerged {
    pub number: u64,
    pub merged: bool,
    pub method: GitPrMergeMethod,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitPrCreated {
    pub number: u64,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitStashEntry {
    pub index: u32,
    pub message: String,
    pub branch: String,
    pub time: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitConflict {
    pub blocks: Vec<GitConflictBlock>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum GitConflictBlock {
    Common { text: String },
    Conflict { ours: String, theirs: String, ours_label: String, theirs_label: String },
}

/// The `POST /projects/{id}/git/<op>` body for `diff`.
pub(crate) fn git_diff_body(path: Option<&str>, staged: bool) -> serde_json::Value {
    json!({ "path": path, "staged": staged })
}

/// The `diff` body for the whole work tree with the untracked files in the same patch.
pub(crate) fn git_diff_untracked_body() -> serde_json::Value {
    json!({ "path": null, "staged": false, "include_untracked": true })
}

/// The `POST /projects/{id}/git/commit` body.
pub(crate) fn git_commit_body(summary: &str, description: Option<&str>, amend: bool) -> serde_json::Value {
    json!({ "summary": summary, "description": description, "amend": amend })
}

/// `GET /health`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Health {
    pub ok: bool,
}

/// `POST /sessions/{id}/lease` answer.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Lease {
    pub ttl_secs: u64,
}

/// Any byte stream the push WebSocket can run on (TCP or a transport stream).
pub trait PushIo: AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static {}
impl<T: AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static> PushIo for T {}

/// The push WebSocket, whichever way the server is reached.
pub type PushSocket = WebSocketStream<Box<dyn PushIo>>;

#[derive(Debug, Clone)]
enum Reach {
    Http(reqwest::Client),
    Peer { dialer: Dialer, addr: PeerAddr },
}

/// A main server's HTTP API. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Api {
    reach: Reach,
    /// e.g. `http://127.0.0.1:8740`, no trailing slash; `peer:<id>` over the transport.
    base: String,
    /// The `Authorization` header, sent on the plain HTTP reach only (the transport authenticates
    /// by device key, FR-N3). The value is marked sensitive, so `{:?}` prints `Sensitive`, never
    /// the token; the plaintext is not kept. Shared by clones, so a token re-read after a
    /// rotation (`FR-S9`, #369) reaches every clone.
    auth: Arc<std::sync::RwLock<Option<header::HeaderMap>>>,
    /// Where the token came from. Unless it was given ([`Api::with_token`]), a 401 re-reads it
    /// once and retries, so a rotated `server-token` is picked up (#369).
    source: TokenSource,
}

/// Where an [`Api`] re-reads its server token after a 401 (#369).
#[derive(Debug, Clone)]
enum TokenSource {
    /// Given by the caller, or no HTTP reach: never re-read.
    Fixed,
    /// The environment or the token file, as [`crate::token::server_token_from_env`] finds it.
    Env,
    /// This token file (its first line).
    File(std::path::PathBuf),
}

/// A status and a fully read body.
#[derive(Debug)]
struct Raw {
    status: StatusCode,
    body: Bytes,
}

impl Api {
    pub fn new(base_url: &str) -> ApiResult<Api> {
        // A server that is not local gets no token and none is read (FR-S11).
        let token = crate::token::token_allowed_for(base_url).then(crate::token::server_token_from_env).flatten();
        let mut api = Api::with_token(base_url, token)?;
        if crate::token::token_allowed_for(base_url) {
            api.source = TokenSource::Env;
        }
        Ok(api)
    }

    /// Like [`Api::new`], with the token read from `token_file` (its first line, trimmed) now and
    /// again after a 401, so a rotated token is picked up without a new `Api` (#369).
    pub fn with_token_file(base_url: &str, token_file: &std::path::Path) -> ApiResult<Api> {
        let token = Self::read_first_line(token_file);
        let mut api = Api::with_token(base_url, token)?;
        api.source = TokenSource::File(token_file.to_path_buf());
        Ok(api)
    }

    fn read_first_line(path: &std::path::Path) -> Option<String> {
        let s = std::fs::read_to_string(path).ok()?;
        let s = s.lines().next().unwrap_or_default().trim();
        (!s.is_empty()).then(|| s.to_owned())
    }

    /// Like [`Api::new`] with the server token given (`None` sends no `Authorization` header, as
    /// an older server expects). The token is read once here, never per request (FR-S11); a
    /// token given here is never re-read.
    pub fn with_token(base_url: &str, token: Option<String>) -> ApiResult<Api> {
        let base = base_url.trim_end_matches('/').to_string();
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            return Err(ApiError::BadUrl(base_url.to_string()));
        }
        // The token goes only to a loopback or listed host (FR-S11).
        let auth = token
            .filter(|_| crate::token::token_allowed_for(&base))
            .as_deref()
            .and_then(crate::token::auth_headers);
        // The header is added per request from `auth`, so a re-read token replaces it (#369).
        let builder = reqwest::Client::builder();
        Ok(Api {
            reach: Reach::Http(builder.build()?),
            base,
            auth: Arc::new(std::sync::RwLock::new(auth)),
            source: TokenSource::Fixed,
        })
    }

    /// The server reached over the transport at `addr` (dialed through `dialer`, which holds
    /// this device's identity).
    pub fn over_transport(dialer: Dialer, addr: impl Into<PeerAddr>) -> Api {
        let addr = addr.into();
        Api { base: format!("peer:{}", addr.peer), reach: Reach::Peer { dialer, addr }, auth: Arc::default(), source: TokenSource::Fixed }
    }

    /// The base URL, or `peer:<id>` over the transport.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// The server's transport address, when reached over the transport.
    pub fn peer(&self) -> Option<&PeerAddr> {
        match &self.reach {
            Reach::Peer { addr, .. } => Some(addr),
            Reach::Http(_) => None,
        }
    }

    /// Tells the transport the app's power state. Over the peer-to-peer transport a backend that
    /// keeps a session alive slows its keepalives in the background; over plain
    /// HTTP this does nothing.
    pub fn set_power_state(&self, state: PowerState) {
        if let Reach::Peer { dialer, .. } = &self.reach {
            dialer.transport().set_power_state(state);
        }
    }

    /// Re-establishes the transport's paths now (the app returned to the foreground, or the
    /// network changed). Open connections survive. Does nothing over plain HTTP.
    pub fn resume_transport(&self) {
        if let Reach::Peer { dialer, .. } = &self.reach {
            dialer.transport().resume_now();
        }
    }

    /// The push WebSocket URL (`ws(s)://…/api/push`).
    pub fn push_url(&self) -> String {
        match &self.reach {
            Reach::Http(_) => {
                let rest = self.base.strip_prefix("http").unwrap_or(&self.base);
                format!("ws{rest}/api/push")
            }
            Reach::Peer { .. } => format!("ws://{PEER_HOST}/api/push"),
        }
    }

    /// `path` is below `/api` and may carry a query string.
    async fn send(&self, method: Method, path: &str, json: Option<serde_json::Value>) -> ApiResult<Raw> {
        match &self.reach {
            Reach::Http(http) => {
                let url = format!("{}/api{path}", self.base);
                let raw = Self::send_http(http, method.clone(), &url, json.as_ref(), self.auth_headers()).await?;
                // A rotated server token (#369): re-read it once and retry when it changed.
                if raw.status == StatusCode::UNAUTHORIZED && self.reload_token() {
                    return Self::send_http(http, method, &url, json.as_ref(), self.auth_headers()).await;
                }
                Ok(raw)
            }
            Reach::Peer { dialer, addr } => {
                let stream = dialer.open_bi(addr, SERVER_SERVICE).await.map_err(peer_err)?;
                let mut sender =
                    ember_transport::http::http1_handshake::<Full<Bytes>>(stream).await.map_err(peer_err)?;
                let mut req = hyper::Request::builder()
                    .method(method)
                    .uri(format!("http://{PEER_HOST}/api{path}"))
                    .header(header::HOST, PEER_HOST);
                let body = match json {
                    Some(v) => {
                        req = req.header(header::CONTENT_TYPE, "application/json");
                        Full::new(Bytes::from(serde_json::to_vec(&v).expect("json value serialises")))
                    }
                    None => Full::default(),
                };
                let req = req.body(body).map_err(peer_err)?;
                let resp = sender.send_request(req).await.map_err(peer_err)?;
                let status = resp.status();
                let body = resp.into_body().collect().await.map_err(peer_err)?.to_bytes();
                Ok(Raw { status, body })
            }
        }
    }

    async fn send_http(
        http: &reqwest::Client,
        method: Method,
        url: &str,
        json: Option<&serde_json::Value>,
        auth: Option<header::HeaderMap>,
    ) -> ApiResult<Raw> {
        let mut req = http.request(method, url);
        if let Some(headers) = auth {
            req = req.headers(headers);
        }
        if let Some(body) = json {
            req = req.json(body);
        }
        let res = req.send().await?;
        let status = res.status();
        Ok(Raw { status, body: res.bytes().await? })
    }

    fn auth_headers(&self) -> Option<header::HeaderMap> {
        self.auth.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Re-reads the server token from the environment or the token file (FR-S11) when this `Api`
    /// was made by [`Api::new`] or [`Api::with_token_file`]. True when a token was found and
    /// differs from the one in use, so the caller retries; the server rotated it
    /// (`POST /api/server-token/rotate`, #369). The token goes only to a loopback or listed host.
    pub fn reload_token(&self) -> bool {
        if !crate::token::token_allowed_for(&self.base) {
            return false;
        }
        let token = match &self.source {
            TokenSource::Fixed => return false,
            TokenSource::Env => crate::token::server_token_from_env(),
            TokenSource::File(path) => Self::read_first_line(path),
        };
        let Some(fresh) = token.as_deref().and_then(crate::token::auth_headers) else {
            return false;
        };
        let mut cur = self.auth.write().unwrap_or_else(|e| e.into_inner());
        let same = cur.as_ref().and_then(|h| h.get(header::AUTHORIZATION)) == fresh.get(header::AUTHORIZATION);
        if same {
            return false;
        }
        *cur = Some(fresh);
        true
    }

    fn check(raw: Raw) -> ApiResult<Raw> {
        if raw.status.is_success() {
            return Ok(raw);
        }
        let body = String::from_utf8_lossy(&raw.body).into_owned();
        let message = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or_else(|| body.clone());
        if raw.status == StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized(message));
        }
        if raw.status == StatusCode::BAD_REQUEST {
            if let Some(unknown) = Self::not_a_member(&body) {
                return Err(ApiError::NotAMember { message, unknown });
            }
        }
        let code = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("code").and_then(|c| c.as_str()).map(str::to_string));
        Err(ApiError::Status { status: raw.status.as_u16(), message, code })
    }

    /// The `unknown` names of a `not_a_member` error body, or `None` for any other body.
    fn not_a_member(body: &str) -> Option<Vec<String>> {
        let v: serde_json::Value = serde_json::from_str(body).ok()?;
        if v.get("code")?.as_str()? != "not_a_member" {
            return None;
        }
        Some(
            v.get("unknown")
                .and_then(|u| u.as_array())
                .map(|a| a.iter().filter_map(|n| n.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
        )
    }

    fn decode<T: DeserializeOwned>(raw: Raw) -> ApiResult<T> {
        serde_json::from_slice(&Self::check(raw)?.body).map_err(|e| ApiError::Decode(e.to_string()))
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> ApiResult<T> {
        Self::decode(self.send(Method::GET, path, None).await?)
    }

    async fn post(&self, path: &str, body: serde_json::Value) -> ApiResult<Raw> {
        Self::check(self.send(Method::POST, path, Some(body)).await?)
    }

    /// Opens the push WebSocket (`/api/push`).
    pub async fn open_push(&self) -> ApiResult<PushSocket> {
        let url = self.push_url();
        let io: Box<dyn PushIo> = match &self.reach {
            Reach::Http(_) => {
                let rest = url
                    .strip_prefix("ws://")
                    .ok_or_else(|| ApiError::Push(format!("{url}: only plain ws:// is supported")))?;
                let authority = rest.split('/').next().unwrap_or_default();
                let (host, port) = authority_addr(authority);
                let tcp = tokio::net::TcpStream::connect((host.as_str(), port))
                    .await
                    .map_err(|e| ApiError::Push(e.to_string()))?;
                let _ = tcp.set_nodelay(true);
                Box::new(tcp)
            }
            Reach::Peer { dialer, addr } => Box::new(dialer.open_bi(addr, SERVER_SERVICE).await.map_err(peer_err)?),
        };
        let request = self.push_request(&url)?;
        let (ws, _) = tokio_tungstenite::client_async(request, io).await.map_err(|e| ApiError::Push(e.to_string()))?;
        Ok(ws)
    }

    /// The WebSocket upgrade request for `url`, carrying `Authorization: Bearer` on the plain
    /// HTTP reach when a token exists (FR-S11).
    fn push_request(&self, url: &str) -> ApiResult<http::Request<()>> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut request = url.into_client_request().map_err(|e| ApiError::Push(e.to_string()))?;
        if let (Reach::Http(_), Some(headers)) = (&self.reach, self.auth_headers()) {
            request.headers_mut().extend(headers);
        }
        Ok(request)
    }

    pub async fn health(&self) -> ApiResult<Health> {
        self.get("/health").await
    }

    pub async fn agents(&self) -> ApiResult<Vec<DetectedAgent>> {
        self.get("/agents").await
    }

    /// All sessions, or one project's, most recently updated first.
    pub async fn sessions(&self, project: Option<&str>) -> ApiResult<Vec<SessionRecord>> {
        match project {
            Some(p) => {
                let q = serde_urlencoded::to_string([("project", p)]).map_err(|e| ApiError::Decode(e.to_string()))?;
                self.get(&format!("/sessions?{q}")).await
            }
            None => self.get("/sessions").await,
        }
    }

    pub async fn session(&self, id: &str) -> ApiResult<SessionDetail> {
        self.get(&format!("/sessions/{}", seg(id))).await
    }

    pub async fn create_session(&self, new: &NewSession) -> ApiResult<SessionRecord> {
        let body = serde_json::to_value(new).expect("NewSession serialises");
        Self::decode(self.post("/sessions", body).await?)
    }

    /// Stored events with `seq > after`, oldest first.
    pub async fn events(&self, id: &str, after: i64) -> ApiResult<Vec<StoredEvent>> {
        self.get(&format!("/sessions/{}/events?after={after}", seg(id))).await
    }

    pub async fn send_message(&self, id: &str, text: &str) -> ApiResult<()> {
        self.post(&format!("/sessions/{}/messages", seg(id)), json!({ "text": text })).await?;
        Ok(())
    }

    pub async fn answer(&self, id: &str, approval_id: &str, decision: ApprovalDecision) -> ApiResult<()> {
        let path = format!("/sessions/{}/approvals/{}", seg(id), seg(approval_id));
        self.post(&path, json!({ "decision": decision })).await?;
        Ok(())
    }

    /// Answer, or skip, an agent's question (FR-T11, FR-A5). Owner token only. The error
    /// carries the server's text, which the UI never shows: it maps the error with
    /// [`question_error_ko`].
    pub async fn answer_question(&self, id: &str, question_id: &str, reply: &QuestionReply) -> ApiResult<()> {
        self.post(&question_answer_path(id, question_id), question_reply_body(reply)).await?;
        Ok(())
    }

    /// Switch a session's approve-everything mode (FR-A5). The server answers the approvals
    /// already pending, and pushes the change to every client.
    pub async fn set_approve_all(&self, id: &str, enabled: bool) -> ApiResult<()> {
        let path = format!("/sessions/{}/approve-all", seg(id));
        Self::check(self.send(Method::PUT, &path, Some(json!({ "enabled": enabled }))).await?)?;
        Ok(())
    }

    pub async fn interrupt(&self, id: &str) -> ApiResult<()> {
        self.post(&format!("/sessions/{}/interrupt", seg(id)), json!({})).await?;
        Ok(())
    }

    /// Keep `id`'s agent alive while it is being viewed (FR-S6).
    pub async fn lease(&self, id: &str) -> ApiResult<Lease> {
        Self::decode(self.post(&format!("/sessions/{}/lease", seg(id)), json!({})).await?)
    }

    // ---- projects and computer assignment (FR-L4) ------------------------------------------

    /// The GitHub card: connection state, account, repositories and totals (FR-G13). The server
    /// caches its answer for 90 seconds.
    pub async fn github_summary(&self) -> ApiResult<GithubSummary> {
        self.get("/integrations/github").await
    }

    /// Every project with its assigned computers.
    pub async fn projects(&self) -> ApiResult<Vec<Project>> {
        self.get("/projects").await
    }

    /// Create a project (no error when it exists).
    pub async fn create_project(&self, name: &str) -> ApiResult<Project> {
        Self::decode(self.post("/projects", json!({ "name": name })).await?)
    }

    /// Assign `computer_id` (`local` = the main server) to `project`. Idempotent.
    pub async fn assign_computer(&self, project: &str, computer_id: &str) -> ApiResult<Project> {
        let path = format!("/projects/{}/computers/{}", seg(project), seg(computer_id));
        Self::decode(self.send(Method::PUT, &path, None).await?)
    }

    /// Unassign `computer_id` from `project`. Idempotent.
    pub async fn unassign_computer(&self, project: &str, computer_id: &str) -> ApiResult<Project> {
        let path = format!("/projects/{}/computers/{}", seg(project), seg(computer_id));
        Self::decode(self.send(Method::DELETE, &path, None).await?)
    }

    // ---- git (FR-G4) -------------------------------------------------------------------------

    /// `POST /projects/{id}/git/<op>`; the server adds the repository path of the project.
    async fn git_post(&self, project: &str, op: &str, body: serde_json::Value) -> ApiResult<Raw> {
        self.post(&format!("/projects/{}/git/{op}", seg(project)), body).await
    }

    /// Any git operation as raw JSON: the body as the contract has it (without `repo`), the answer
    /// as a value (`Null` for a 204). The typed calls below wrap it; the app also uses it so a
    /// demo source can answer the same shapes.
    pub async fn git_raw(&self, project: &str, op: &str, body: serde_json::Value) -> ApiResult<serde_json::Value> {
        let raw = self.git_post(project, op, body).await?;
        if raw.body.is_empty() {
            return Ok(serde_json::Value::Null);
        }
        serde_json::from_slice(&raw.body).map_err(|e| ApiError::Decode(e.to_string()))
    }

    /// Branch, remote state and changed files of the project's repository. A project whose
    /// path is not a repository answers 404.
    pub async fn git_status(&self, project: &str) -> ApiResult<GitStatus> {
        Self::decode(self.git_post(project, "status", json!({})).await?)
    }

    /// The unified diff of one file (`path`) or all, staged or not.
    pub async fn git_diff(&self, project: &str, path: Option<&str>, staged: bool) -> ApiResult<GitDiff> {
        Self::decode(self.git_post(project, "diff", git_diff_body(path, staged)).await?)
    }

    /// The work tree diff of the whole repository with every untracked file in the same patch (one
    /// call instead of one per file).
    pub async fn git_diff_untracked(&self, project: &str) -> ApiResult<GitDiff> {
        Self::decode(self.git_post(project, "diff", git_diff_untracked_body()).await?)
    }

    /// Stage whole files.
    pub async fn git_stage(&self, project: &str, paths: &[String]) -> ApiResult<()> {
        self.git_post(project, "stage", json!({ "paths": paths })).await?;
        Ok(())
    }

    /// Unstage whole files.
    pub async fn git_unstage(&self, project: &str, paths: &[String]) -> ApiResult<()> {
        self.git_post(project, "unstage", json!({ "paths": paths })).await?;
        Ok(())
    }

    /// Apply a unified patch to the index (`reverse` unstages it): a hunk or a line range.
    pub async fn git_apply(&self, project: &str, patch: &str, reverse: bool) -> ApiResult<()> {
        self.git_post(project, "apply", json!({ "patch": patch, "reverse": reverse })).await?;
        Ok(())
    }

    /// Commit the index. An empty index answers 409.
    pub async fn git_commit(
        &self,
        project: &str,
        summary: &str,
        description: Option<&str>,
        amend: bool,
    ) -> ApiResult<GitCommitted> {
        Self::decode(self.git_post(project, "commit", git_commit_body(summary, description, amend)).await?)
    }

    /// Switch to an existing local branch.
    pub async fn git_checkout(&self, project: &str, branch: &str) -> ApiResult<()> {
        self.git_post(project, "checkout", json!({ "branch": branch })).await?;
        Ok(())
    }

    /// Create a branch (from `from`, default the current one) and switch to it.
    pub async fn git_branch(&self, project: &str, name: &str, from: Option<&str>) -> ApiResult<()> {
        self.git_post(project, "branch", json!({ "name": name, "from": from })).await?;
        Ok(())
    }

    pub async fn git_fetch(&self, project: &str, remote: Option<&str>) -> ApiResult<GitOutput> {
        Self::decode(self.git_post(project, "fetch", json!({ "remote": remote })).await?)
    }

    /// Fast-forward only; a diverged branch answers 409 with git's message.
    pub async fn git_pull(&self, project: &str, remote: Option<&str>) -> ApiResult<GitOutput> {
        Self::decode(self.git_post(project, "pull", json!({ "remote": remote })).await?)
    }

    pub async fn git_push(&self, project: &str, remote: Option<&str>) -> ApiResult<GitOutput> {
        Self::decode(self.git_post(project, "push", json!({ "remote": remote })).await?)
    }

    /// The unmerged paths.
    pub async fn git_conflicts(&self, project: &str) -> ApiResult<GitConflicts> {
        Self::decode(self.git_post(project, "conflicts", json!({})).await?)
    }

    /// Read a page of repository history.
    pub async fn git_log(&self, project: &str, limit: u32, skip: u64, branch: Option<&str>) -> ApiResult<GitLog> {
        Self::decode(self.git_post(project, "log", json!({ "limit": limit, "skip": skip, "branch": branch })).await?)
    }

    /// One commit with its message, refs and changed files (FR-G14).
    pub async fn git_commit_show(&self, project: &str, sha: &str) -> ApiResult<GitCommitDetail> {
        Self::decode(self.git_post(project, "commit/show", json!({ "sha": sha })).await?)
    }

    /// Revert a commit on the current branch (FR-G15; 409 for a dirty tree or a conflict).
    pub async fn git_revert(&self, project: &str, sha: &str) -> ApiResult<GitCommitApplied> {
        Self::decode(self.git_post(project, "revert", json!({ "sha": sha, "confirm": true })).await?)
    }

    /// Cherry-pick a commit onto the current branch (FR-G15; 409 for a dirty tree or a conflict).
    pub async fn git_cherry_pick(&self, project: &str, sha: &str) -> ApiResult<GitCommitApplied> {
        Self::decode(self.git_post(project, "cherry-pick", json!({ "sha": sha, "confirm": true })).await?)
    }

    /// Whether a merge is in progress, what is merged into what, and how many files conflict (FR-G16).
    pub async fn git_merge_state(&self, project: &str) -> ApiResult<GitMergeState> {
        Self::decode(self.git_post(project, "merge", json!({})).await?)
    }

    /// Cancel the merge in progress (FR-G17; 409 when none is).
    pub async fn git_merge_abort(&self, project: &str) -> ApiResult<GitMergeAborted> {
        Self::decode(self.git_post(project, "merge/abort", json!({ "confirm": true })).await?)
    }

    /// Finish the merge in progress with a merge commit (FR-G17; 409 when none is or conflicts remain).
    pub async fn git_merge_commit(&self, project: &str) -> ApiResult<GitCommitApplied> {
        Self::decode(self.git_post(project, "merge/commit", json!({ "confirm": true })).await?)
    }

    /// List local branches with upstream ahead/behind, last commit, default and worktree (FR-G9).
    pub async fn git_branches(&self, project: &str) -> ApiResult<GitBranches> {
        Self::decode(self.git_post(project, "branches", json!({})).await?)
    }

    /// List pull requests (`state` is `open`, `closed`, `merged` or `all`). The node runs the
    /// computer's own `gh`; a missing or logged-out `gh` answers 409 with a Korean message.
    pub async fn git_prs(&self, project: &str, state: &str, limit: u32) -> ApiResult<GitPrs> {
        Self::decode(self.git_post(project, "prs", json!({ "state": state, "limit": limit })).await?)
    }

    /// One pull request in full (FR-G10).
    pub async fn git_pr(&self, project: &str, number: u64) -> ApiResult<GitPrDetail> {
        Self::decode(self.git_post(project, "pr", json!({ "number": number })).await?)
    }

    /// Approve, request changes or comment (FR-G11). `request_changes` and `comment` need a
    /// body. The call sends `confirm: true`, so call it only after the person confirmed.
    pub async fn git_pr_review(&self, project: &str, number: u64, action: GitPrReviewAction, body: Option<&str>) -> ApiResult<GitPrReviewed> {
        Self::decode(self.git_post(project, "pr/review", json!({ "number": number, "action": action, "body": body, "confirm": true })).await?)
    }

    /// Merge (FR-G11). A closed merge gate answers 409 with the Korean reason. The call sends
    /// `confirm: true`, so call it only after the person confirmed. `head_sha` is required: the
    /// `GitPrDetail::head_sha` the person saw (empty answers 400). When the head moved since, the node answers 409
    /// ("풀 리퀘스트가 바뀌었습니다...") and merges nothing.
    pub async fn git_pr_merge(&self, project: &str, number: u64, method: GitPrMergeMethod, delete_branch: bool, head_sha: &str) -> ApiResult<GitPrMerged> {
        let body = json!({ "number": number, "method": method, "delete_branch": delete_branch, "head_sha": head_sha, "confirm": true });
        Self::decode(self.git_post(project, "pr/merge", body).await?)
    }

    /// Create a pull request from a branch that already exists on the remote (FR-G12).
    pub async fn git_pr_create(&self, project: &str, title: &str, body: Option<&str>, base: Option<&str>, head: Option<&str>, draft: bool) -> ApiResult<GitPrCreated> {
        Self::decode(self.git_post(project, "pr/create", json!({ "title": title, "body": body, "base": base, "head": head, "draft": draft, "confirm": true })).await?)
    }

    pub async fn git_stash_list(&self, project: &str) -> ApiResult<Vec<GitStashEntry>> {
        Self::decode(self.git_post(project, "stash/list", json!({})).await?)
    }

    pub async fn git_stash_push(&self, project: &str, message: Option<&str>, include_untracked: bool) -> ApiResult<()> {
        self.git_post(project, "stash/push", json!({ "message": message, "include_untracked": include_untracked })).await?;
        Ok(())
    }

    pub async fn git_stash_apply(&self, project: &str, index: u32) -> ApiResult<()> {
        self.git_post(project, "stash/apply", json!({ "index": index })).await?;
        Ok(())
    }

    pub async fn git_stash_pop(&self, project: &str, index: u32) -> ApiResult<()> {
        self.git_post(project, "stash/pop", json!({ "index": index })).await?;
        Ok(())
    }

    pub async fn git_stash_drop(&self, project: &str, index: u32) -> ApiResult<()> {
        self.git_post(project, "stash/drop", json!({ "index": index })).await?;
        Ok(())
    }

    pub async fn git_conflict(&self, project: &str, path: &str) -> ApiResult<GitConflict> {
        Self::decode(self.git_post(project, "conflict", json!({ "path": path })).await?)
    }

    pub async fn git_resolve(&self, project: &str, path: &str, content: &str) -> ApiResult<()> {
        self.git_post(project, "resolve", json!({ "path": path, "content": content })).await?;
        Ok(())
    }

    // ---- channels (FR-T10, FR-T16) ---------------------------------------------------------

    /// Every channel, or one project's. Entries carry `last_message` and `unread`, no `messages`.
    pub async fn channels(&self, project: Option<&str>) -> ApiResult<Vec<Channel>> {
        let path = crate::channels::list_path(project).map_err(|e| ApiError::Decode(e.to_string()))?;
        self.get(&path).await
    }

    pub async fn create_channel(&self, new: &NewChannel) -> ApiResult<Channel> {
        let body = serde_json::to_value(new).expect("NewChannel serialises");
        Self::decode(self.post("/channels", body).await?)
    }

    /// One channel with its latest messages.
    pub async fn channel(&self, id: &str) -> ApiResult<Channel> {
        self.get(&format!("/channels/{}", seg(id))).await
    }

    /// Messages with `seq > after`, oldest first.
    pub async fn channel_messages(&self, id: &str, q: &MessagesQuery) -> ApiResult<Vec<ChannelMessage>> {
        self.get(&crate::channels::messages_path(id, q)).await
    }

    pub async fn post_channel_message(&self, id: &str, new: &NewChannelMessage) -> ApiResult<ChannelMessage> {
        let body = serde_json::to_value(new).expect("NewChannelMessage serialises");
        Self::decode(self.post(&format!("/channels/{}/messages", seg(id)), body).await?)
    }

    /// Which member agents `text` would reach and which `@names` are not members (FR-T12, FR-T13).
    pub async fn preview_channel_mentions(&self, id: &str, text: &str) -> ApiResult<MentionPreview> {
        let body = serde_json::to_value(MentionPreviewRequest { text: text.to_string() }).expect("MentionPreviewRequest serialises");
        Self::decode(self.post(&crate::channels::mentions_preview_path(id), body).await?)
    }

    /// Mark everything up to `upto` (default the latest) as read; the cursor only moves forward.
    pub async fn mark_channel_read(&self, id: &str, upto: Option<i64>) -> ApiResult<Channel> {
        let body = serde_json::to_value(ReadBody { seq: upto }).expect("ReadBody serialises");
        Self::decode(self.post(&format!("/channels/{}/read", seg(id)), body).await?)
    }

    /// The state of the guest door (FR-T14): on or off, its listen address and public URL, and
    /// whether the link needs a reachability check. Known before the first invite is made.
    pub async fn guest_door(&self) -> ApiResult<crate::channels::GuestDoorState> {
        self.get(crate::channels::GUEST_DOOR_PATH).await
    }

    /// The feed of a channel (FR-T11): pending approvals and last-turn reports of its agents,
    /// newest first.
    pub async fn channel_feed(&self, id: &str) -> ApiResult<Vec<FeedItem>> {
        self.get(&crate::channels::feed_path(id)).await
    }

    /// Answer a pending approval from the feed; the server answers 204.
    pub async fn answer_channel_feed(&self, id: &str, approval_id: &str, decision: FeedDecision) -> ApiResult<()> {
        let body = serde_json::to_value(FeedAnswer { decision }).expect("FeedAnswer serialises");
        self.post(&crate::channels::feed_answer_path(id, approval_id), body).await?;
        Ok(())
    }

    // ---- invites, guests, allowances (FR-T14, FR-T17) --------------------------------------

    /// Create a single-use invite. The token and link are in this answer only. `None` lets the
    /// server pick its default TTL; the body is `{}` then, never empty.
    pub async fn create_invite(&self, channel: &str, ttl_hours: Option<u32>) -> ApiResult<CreatedInvite> {
        let body = crate::channels::new_invite_body(ttl_hours);
        Self::decode(self.post(&crate::channels::invites_path(channel), body).await?)
    }

    /// Invites of a channel, newest first (no token, no link).
    pub async fn invites(&self, channel: &str) -> ApiResult<Vec<Invite>> {
        self.get(&crate::channels::invites_path(channel)).await
    }

    /// Revoke an invite (204); also ends the guest made from it.
    pub async fn revoke_invite(&self, channel: &str, invite_id: &str) -> ApiResult<()> {
        Self::check(self.send(Method::DELETE, &crate::channels::invite_path(channel, invite_id), None).await?)?;
        Ok(())
    }

    /// Guests of a channel, newest first, revoked and expired ones included.
    pub async fn guests(&self, channel: &str) -> ApiResult<Vec<Guest>> {
        self.get(&crate::channels::guests_path(channel)).await
    }

    /// Remove a guest (204): the credential and allowance end at once.
    pub async fn remove_guest(&self, channel: &str, guest_id: &str) -> ApiResult<()> {
        Self::check(self.send(Method::DELETE, &crate::channels::guest_path(channel, guest_id), None).await?)?;
        Ok(())
    }

    /// Allow the guest's mentions to reach agents for `hours` (1..=168); replaces any end time.
    pub async fn set_allowance(&self, channel: &str, guest_id: &str, hours: u32) -> ApiResult<Allowance> {
        let body = serde_json::to_value(AllowanceBody { hours }).expect("AllowanceBody serialises");
        let path = crate::channels::guest_allow_path(channel, guest_id);
        Self::decode(self.send(Method::PUT, &path, Some(body)).await?)
    }

    /// Stop delivering the guest's mentions (204).
    pub async fn clear_allowance(&self, channel: &str, guest_id: &str) -> ApiResult<()> {
        Self::check(self.send(Method::DELETE, &crate::channels::guest_allow_path(channel, guest_id), None).await?)?;
        Ok(())
    }

    // ---- schedules (FR-A8) -----------------------------------------------------------------

    /// Every schedule, or one project's.
    pub async fn schedules(&self, project: Option<&str>) -> ApiResult<Vec<Schedule>> {
        match project {
            Some(p) => self.get(&format!("/schedules?project={}", seg(p))).await,
            None => self.get("/schedules").await,
        }
    }

    pub async fn create_schedule(&self, new: &NewSchedule) -> ApiResult<Schedule> {
        let body = serde_json::to_value(new).expect("NewSchedule serialises");
        Self::decode(self.post("/schedules", body).await?)
    }

    pub async fn update_schedule(&self, id: &str, patch: &SchedulePatch) -> ApiResult<Schedule> {
        let body = serde_json::to_value(patch).expect("SchedulePatch serialises");
        Self::decode(self.send(Method::PATCH, &format!("/schedules/{}", seg(id)), Some(body)).await?)
    }

    pub async fn delete_schedule(&self, id: &str) -> ApiResult<()> {
        Self::check(self.send(Method::DELETE, &format!("/schedules/{}", seg(id)), None).await?)?;
        Ok(())
    }

    pub async fn pause_schedule(&self, id: &str) -> ApiResult<Schedule> {
        Self::decode(self.post(&format!("/schedules/{}/pause", seg(id)), json!({})).await?)
    }

    pub async fn resume_schedule(&self, id: &str) -> ApiResult<Schedule> {
        Self::decode(self.post(&format!("/schedules/{}/resume", seg(id)), json!({})).await?)
    }

    /// Run now; answers the run (the server answers 202).
    pub async fn run_schedule(&self, id: &str) -> ApiResult<ScheduleRun> {
        Self::decode(self.post(&format!("/schedules/{}/run", seg(id)), json!({})).await?)
    }

    /// The most recent runs, newest first (server default 50, at most 500).
    pub async fn schedule_runs(&self, id: &str, limit: Option<usize>) -> ApiResult<Vec<ScheduleRun>> {
        let q = limit.map(|n| format!("?limit={n}")).unwrap_or_default();
        self.get(&format!("/schedules/{}/runs{q}", seg(id))).await
    }

    // ---- session metadata, search, export, fork (FR-L9, FR-S4, FR-S5) -----------------------

    /// Rename, pin or archive a session; answers the updated record.
    pub async fn patch_session(&self, id: &str, patch: &SessionPatch) -> ApiResult<SessionRecord> {
        let body = serde_json::to_value(patch).expect("SessionPatch serialises");
        Self::decode(self.send(Method::PATCH, &format!("/sessions/{}", seg(id)), Some(body)).await?)
    }

    // ---- device scope (FR-N8, #366) ----------------------------------------------------------

    /// Sets an admitted device's scope (`PATCH /api/devices/{peer_id}`). Owner only: the route
    /// is on the local listener with the server token, never on the transport. Answers the
    /// updated device row.
    pub async fn set_device_scope(&self, peer_id: &str, scope: DeviceScope) -> ApiResult<DeviceRecord> {
        let body = serde_json::json!({ "scope": scope });
        Self::decode(self.send(Method::PATCH, &format!("/devices/{}", seg(peer_id)), Some(body)).await?)
    }

    /// Full-text search across every session's messages, best match first.
    pub async fn search(&self, query: &str, limit: Option<usize>) -> ApiResult<Vec<SearchHit>> {
        let mut q = vec![("q", query.to_string())];
        if let Some(n) = limit {
            q.push(("limit", n.to_string()));
        }
        let q = serde_urlencoded::to_string(q).map_err(|e| ApiError::Decode(e.to_string()))?;
        self.get(&format!("/search?{q}")).await
    }

    /// Tokens per account per UTC day for the last `days` days (FR-U7), oldest first. The server
    /// clamps `days` to 1..=366 and defaults to 7.
    pub async fn usage(&self, days: Option<u32>) -> ApiResult<Vec<DailyUsage>> {
        self.get(&usage_path(days)).await
    }

    /// The session as the server's self-contained export file (`format: "ember-transcript"`).
    pub async fn export_session(&self, id: &str) -> ApiResult<serde_json::Value> {
        self.get(&format!("/sessions/{}/export", seg(id))).await
    }

    /// Fork a session after its completed turn `turn_seq` (the last one when `None`, FR-S5).
    /// The server answers 409 without a completed turn; check [`SessionDetail::can_fork`] first.
    pub async fn fork_session(&self, id: &str, turn_seq: Option<i64>) -> ApiResult<SessionRecord> {
        let body = fork_body(turn_seq);
        Self::decode(self.post(&format!("/sessions/{}/fork", seg(id)), body).await?)
    }

    // ---- teams and mentions (FR-T6, FR-T7) ---------------------------------------------------

    /// The team `id` leads or belongs to (`None` when it is in none).
    pub async fn session_team(&self, id: &str) -> ApiResult<Option<TeamView>> {
        self.get(&format!("/sessions/{}/team", seg(id))).await
    }

    pub async fn team(&self, team_id: &str) -> ApiResult<TeamView> {
        self.get(&format!("/teams/{}", seg(team_id))).await
    }

    /// A team's mail, oldest first: the last `limit` (server default 20), or the first after
    /// mail number `after`.
    pub async fn team_mail(&self, team_id: &str, after: i64, limit: Option<usize>) -> ApiResult<Vec<TeamMail>> {
        let mut q = vec![("after", after.to_string())];
        if let Some(n) = limit {
            q.push(("limit", n.to_string()));
        }
        let q = serde_urlencoded::to_string(q).map_err(|e| ApiError::Decode(e.to_string()))?;
        self.get(&format!("/teams/{}/mail?{q}", seg(team_id))).await
    }

    /// End a teammate as the user: its agent stops and it leaves the team.
    pub async fn end_teammate(&self, team_id: &str, session_id: &str) -> ApiResult<TeamMember> {
        let path = format!("/teams/{}/members/{}/end", seg(team_id), seg(session_id));
        Self::decode(self.post(&path, json!({})).await?)
    }

    /// Sessions a message typed in `id` may mention as `@@...`, filtered by `query` (title
    /// substring or id prefix).
    pub async fn mention_candidates(&self, id: &str, query: &str, limit: Option<usize>) -> ApiResult<Vec<MentionCandidate>> {
        let mut q = vec![("q", query.to_string())];
        if let Some(n) = limit {
            q.push(("limit", n.to_string()));
        }
        let q = serde_urlencoded::to_string(q).map_err(|e| ApiError::Decode(e.to_string()))?;
        self.get(&format!("/sessions/{}/mentions?{q}", seg(id))).await
    }
}

/// `host:port` (default port 80) for `TcpStream::connect`, with IPv6 brackets removed.
fn authority_addr(authority: &str) -> (String, u16) {
    if let Some(rest) = authority.strip_prefix('[') {
        if let Some((host, tail)) = rest.split_once(']') {
            let port = tail.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(80);
            return (host.to_string(), port);
        }
    }
    match authority.rsplit_once(':') {
        Some((h, p)) => match p.parse() {
            Ok(port) => (h.to_string(), port),
            Err(_) => (authority.to_string(), 80),
        },
        None => (authority.to_string(), 80),
    }
}

// ---- file transfer (FR-F3, FR-F4, FR-F5, FR-F8, #487) ---------------------------------------

impl Api {
    /// Everything the receiving computer holds: devices, transfers, offers, received files,
    /// history and settings.
    pub async fn transfers(&self) -> ApiResult<crate::transfers::TransfersSnapshot> {
        self.get("/transfers").await
    }

    /// Offers files as device `from`. With `peer_id` the receiver requires a registered device.
    pub async fn offer_transfer(
        &self,
        from: &str,
        peer_id: Option<&str>,
        files: &[crate::transfers::DeclaredFile],
    ) -> ApiResult<crate::transfers::OfferCreated> {
        let body = json!({ "from": from, "peer_id": peer_id, "files": files });
        Self::decode(self.post("/transfers/offers", body).await?)
    }

    pub async fn accept_transfer_offer(&self, id: &str) -> ApiResult<crate::transfers::TransferItem> {
        Self::decode(self.post(&format!("/transfers/offers/{}/accept", seg(id)), json!({})).await?)
    }

    pub async fn decline_transfer_offer(&self, id: &str) -> ApiResult<crate::transfers::HistoryRecord> {
        Self::decode(self.post(&format!("/transfers/offers/{}/decline", seg(id)), json!({})).await?)
    }

    /// Appends `data` to file `file` of transfer `id` at `offset`. `sha256` is the chunk's hash
    /// (lowercase hex); the receiver refuses a chunk that does not match it (`FR-F5`).
    pub async fn transfer_chunk(
        &self,
        id: &str,
        file: usize,
        offset: u64,
        data: &[u8],
        sha256: Option<&str>,
    ) -> ApiResult<crate::transfers::TransferItem> {
        use base64::Engine;
        let body = json!({
            "file": file,
            "offset": offset,
            "data_base64": base64::engine::general_purpose::STANDARD.encode(data),
            "sha256": sha256,
        });
        Self::decode(self.post(&format!("/transfers/{}/chunks", seg(id)), body).await?)
    }

    /// The receiver's verified byte offset of file `file` (`FR-F5`): where to continue.
    pub async fn transfer_resume_offset(&self, id: &str, file: usize) -> ApiResult<u64> {
        let r: crate::transfers::ResumeOffset =
            self.get(&format!("/transfers/{}/resume-offset/{file}", seg(id))).await?;
        Ok(r.offset)
    }

    /// Asks the receiver to verify and store the files of transfer `id`.
    pub async fn complete_transfer(&self, id: &str) -> ApiResult<Vec<crate::transfers::StoredFile>> {
        let r: crate::transfers::Completed =
            Self::decode(self.post(&format!("/transfers/{}/complete", seg(id)), json!({})).await?)?;
        Ok(r.stored)
    }

    pub async fn pause_transfer(&self, id: &str) -> ApiResult<crate::transfers::TransferItem> {
        Self::decode(self.post(&format!("/transfers/{}/pause", seg(id)), json!({})).await?)
    }

    pub async fn resume_transfer(&self, id: &str) -> ApiResult<crate::transfers::TransferItem> {
        Self::decode(self.post(&format!("/transfers/{}/resume", seg(id)), json!({})).await?)
    }

    /// Cancels an offer or a transfer; the receiver removes the partial files (`FR-F4`).
    pub async fn cancel_transfer(&self, id: &str) -> ApiResult<crate::transfers::HistoryRecord> {
        Self::decode(self.post(&format!("/transfers/{}/cancel", seg(id)), json!({})).await?)
    }

    pub async fn transfer_history(&self) -> ApiResult<Vec<crate::transfers::HistoryRecord>> {
        self.get("/transfers/history").await
    }

    /// Clears the history; received files stay on disk (`FR-F8`).
    pub async fn clear_transfer_history(&self) -> ApiResult<()> {
        Self::check(self.send(Method::DELETE, "/transfers/history", None).await?)?;
        Ok(())
    }
}

/// Percent-encode one path segment (ids are UUIDs today, but approval ids come from agents).
pub(crate) fn seg(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Path of `GET /usage`, with `?days=` when given.
fn usage_path(days: Option<u32>) -> String {
    match days {
        Some(n) => format!("/usage?days={n}"),
        None => "/usage".to_string(),
    }
}

// ---------------------------------------------------------------------------------------------
// Listening ports of a computer's sessions (FR-P7)
// ---------------------------------------------------------------------------------------------

/// How far a listening port is reachable. An unknown value from a newer node reads as `Lan`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortScope {
    Loopback,
    Any,
    #[default]
    #[serde(other)]
    Lan,
}

/// The terminal or job a listening process belongs to (`kind` is `term` or `job`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct PortOwner {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub id: String,
}

/// One TCP port in LISTEN state under a node-owned terminal or job.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct ListeningPort {
    pub port: u16,
    #[serde(default)]
    pub scope: PortScope,
    /// Executable name only.
    #[serde(default)]
    pub process: String,
    #[serde(default)]
    pub owner: PortOwner,
}

/// `GET /api/computers/{id}/ports` answer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Ports {
    #[serde(default)]
    pub ports: Vec<ListeningPort>,
    /// False where the node cannot look (not Linux or macOS). Older nodes omit it: true.
    #[serde(default = "ports_supported_default")]
    pub supported: bool,
}

impl Default for Ports {
    fn default() -> Self {
        Ports { ports: Vec::new(), supported: true }
    }
}

fn ports_supported_default() -> bool {
    true
}

impl Api {
    /// Listening TCP ports of the processes under a computer's terminals and jobs (FR-P7).
    pub async fn computer_ports(&self, computer_id: &str) -> ApiResult<Ports> {
        self.get(&ports_path(computer_id)).await
    }
}

fn ports_path(computer_id: &str) -> String {
    format!("/computers/{}/ports", seg(computer_id))
}

#[cfg(test)]
mod ports_tests {
    use super::*;

    #[test]
    fn fr_p7_the_ports_path_escapes_the_computer_id() {
        assert_eq!(ports_path("a b"), "/computers/a%20b/ports");
    }

    #[test]
    fn fr_p7_a_ports_answer_decodes() {
        let p: Ports = serde_json::from_value(json!({
            "ports": [
                {"port": 3000, "scope": "loopback", "process": "node", "owner": {"kind": "job", "id": "j1"}},
                {"port": 8080, "scope": "any", "process": "python3", "owner": {"kind": "term", "id": "t1"}},
                {"port": 22, "scope": "future", "process": "x", "owner": {"kind": "term", "id": "t2"}}
            ]
        }))
        .unwrap();
        assert_eq!(p.ports.len(), 3);
        assert_eq!(p.ports[0].scope, PortScope::Loopback);
        assert_eq!(p.ports[0].owner, PortOwner { kind: "job".into(), id: "j1".into() });
        assert_eq!(p.ports[1].scope, PortScope::Any);
        assert_eq!(p.ports[2].scope, PortScope::Lan);
        assert!(p.supported, "an older node omits supported: true");
        assert_eq!(serde_json::from_value::<Ports>(json!({})).unwrap(), Ports::default());
        let off: Ports = serde_json::from_value(json!({"ports": [], "supported": false})).unwrap();
        assert!(!off.supported);
    }
}

// ---------------------------------------------------------------------------------------------
// Computer metrics (FR-X7, FR-L21)
// ---------------------------------------------------------------------------------------------

/// `GET /api/computers/{id}/metrics` answer: one sample of a computer's status. Decoded
/// tolerantly: a field an older node leaves out becomes its empty value.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct MetricsSample {
    /// Wall clock of the sample, milliseconds since the Unix epoch.
    #[serde(default)]
    pub at_ms: u64,
    #[serde(default)]
    pub cpu: CpuMetrics,
    #[serde(default)]
    pub memory: Option<MemoryMetrics>,
    #[serde(default)]
    pub disks: Vec<DiskMetrics>,
    /// Cumulative byte counters per interface; rates come from two samples.
    #[serde(default)]
    pub network: Vec<NetMetrics>,
    #[serde(default)]
    pub temperatures: Vec<TempMetrics>,
    #[serde(default)]
    pub gpu: GpuReport,
    #[serde(default)]
    pub processes: Vec<ProcessMetrics>,
    #[serde(default)]
    pub process_count: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct CpuMetrics {
    #[serde(default)]
    pub total_percent: Option<f32>,
    #[serde(default)]
    pub cores: Vec<f32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct MemoryMetrics {
    #[serde(default)]
    pub total_bytes: u64,
    #[serde(default)]
    pub used_bytes: u64,
    #[serde(default)]
    pub swap_total_bytes: u64,
    #[serde(default)]
    pub swap_used_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct DiskMetrics {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub mount: String,
    #[serde(default)]
    pub total_bytes: u64,
    #[serde(default)]
    pub available_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct NetMetrics {
    #[serde(default)]
    pub interface: String,
    #[serde(default)]
    pub rx_bytes: u64,
    #[serde(default)]
    pub tx_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct TempMetrics {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub celsius: f32,
}

/// GPU status; `devices` is empty with a `reason` when the node cannot read it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct GpuReport {
    #[serde(default)]
    pub devices: Vec<GpuMetrics>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct GpuMetrics {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub utilization_percent: Option<f32>,
    #[serde(default)]
    pub memory_total_bytes: Option<u64>,
    #[serde(default)]
    pub memory_used_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ProcessMetrics {
    #[serde(default)]
    pub pid: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub cpu_percent: f32,
    #[serde(default)]
    pub memory_bytes: u64,
}

impl Api {
    /// One metrics sample of a computer, with at most `top` processes (FR-X7).
    pub async fn metrics(&self, computer_id: &str, top: u32) -> ApiResult<MetricsSample> {
        self.get(&metrics_path(computer_id, top)).await
    }
}

fn metrics_path(computer_id: &str, top: u32) -> String {
    format!("/computers/{}/metrics?top={top}", seg(computer_id))
}

#[cfg(test)]
mod metrics_tests {
    use super::*;

    #[test]
    fn fr_x7_the_metrics_path_escapes_the_computer_id() {
        assert_eq!(metrics_path("a b", 20), "/computers/a%20b/metrics?top=20");
    }

    #[test]
    fn fr_x7_a_full_sample_decodes() {
        let s: MetricsSample = serde_json::from_value(json!({
            "at_ms": 5,
            "cpu": {"total_percent": 12.5, "cores": [10.0, 15.0]},
            "memory": {"total_bytes": 16, "used_bytes": 8, "swap_total_bytes": 4, "swap_used_bytes": 1},
            "disks": [{"name": "d", "mount": "/", "total_bytes": 100, "available_bytes": 40}],
            "network": [{"interface": "en0", "rx_bytes": 7, "tx_bytes": 9}],
            "temperatures": [{"label": "cpu", "celsius": 51.5}],
            "gpu": {"devices": [], "reason": "not readable"},
            "processes": [{"pid": 1, "name": "init", "cpu_percent": 0.5, "memory_bytes": 1024}],
            "process_count": 300
        }))
        .unwrap();
        assert_eq!(s.cpu.cores.len(), 2);
        assert_eq!(s.memory.as_ref().unwrap().used_bytes, 8);
        assert_eq!(s.gpu.reason.as_deref(), Some("not readable"));
        assert_eq!(s.processes[0].pid, 1);
        assert_eq!(s.process_count, 300);
    }

    #[test]
    fn fr_x7_a_sample_with_missing_fields_decodes_to_empty_values() {
        let s: MetricsSample = serde_json::from_value(json!({"at_ms": 1, "cpu": {"total_percent": null}})).unwrap();
        assert_eq!(s.cpu.total_percent, None);
        assert!(s.memory.is_none() && s.disks.is_empty() && s.processes.is_empty());
        let s: MetricsSample = serde_json::from_value(json!({})).unwrap();
        assert_eq!(s, MetricsSample::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_t11_answer_question_posts_exact_body() {
        assert_eq!(question_answer_path("s 1", "r/1"), "/sessions/s%201/questions/r%2F1/answer");
        let a = QuestionAnswer { question_id: "q0".into(), values: vec!["blue".into()] };
        assert_eq!(
            question_reply_body(&QuestionReply::Answers(vec![a])),
            json!({"answers": [{"question_id": "q0", "values": ["blue"]}]})
        );
        assert_eq!(question_reply_body(&QuestionReply::Skip), json!({"decline": true}));
        let secret = QuestionReply::Answers(vec![QuestionAnswer { question_id: "q".into(), values: vec!["hunter2".into()] }]);
        assert!(!format!("{secret:?}").contains("hunter2"));
    }

    #[test]
    fn fr_t11_question_error_texts_are_fixed_korean() {
        let st = |status: u16| ApiError::Status { status, message: "secret server text".into(), code: None };
        let all = [
            (st(404), QuestionCallError::Gone),
            (st(400), QuestionCallError::Invalid),
            (st(409), QuestionCallError::NoAgent),
            (st(403), QuestionCallError::NotOwner),
            (ApiError::Unauthorized("x".into()), QuestionCallError::NotOwner),
            (st(422), QuestionCallError::Unsupported),
            (st(500), QuestionCallError::Failed),
            (ApiError::Peer("p".into()), QuestionCallError::Failed),
        ];
        for (e, want) in all {
            assert_eq!(QuestionCallError::from_error(&e), want);
            let t = question_error_ko(&e);
            assert!(!t.contains("secret server text"));
            assert!(t.ends_with('.') && t.chars().any(|c| ('가'..='힣').contains(&c)));
        }
        assert!(QuestionCallError::Gone.is_terminal() && QuestionCallError::NoAgent.is_terminal());
        assert!(!QuestionCallError::Invalid.is_terminal() && !QuestionCallError::Failed.is_terminal());
    }

    #[test]
    fn fr_s11_http_client_sends_bearer_only_with_a_token() {
        let with = Api::with_token("http://127.0.0.1:1", Some("tok".into())).unwrap();
        let req = with.push_request(&with.push_url()).unwrap();
        let value = req.headers().get(header::AUTHORIZATION).expect("header on the upgrade");
        assert_eq!(value, "Bearer tok");
        assert!(value.is_sensitive());
        let without = Api::with_token("http://127.0.0.1:1", None).unwrap();
        assert!(without.push_request(&without.push_url()).unwrap().headers().get(header::AUTHORIZATION).is_none());
    }

    #[test]
    fn fr_s11_debug_output_never_holds_the_token() {
        let api = Api::with_token("http://127.0.0.1:1", Some("secret".into())).unwrap();
        assert!(!format!("{api:?}").contains("secret"));
    }

    #[test]
    fn fr_s11_no_token_is_sent_to_a_remote_host() {
        let api = Api::with_token("https://example.com", Some("secret".into())).unwrap();
        assert!(api.push_request(&api.push_url()).unwrap().headers().get(header::AUTHORIZATION).is_none());
    }

    #[test]
    fn fr_s11_401_maps_to_the_auth_error() {
        let raw = Raw { status: StatusCode::UNAUTHORIZED, body: Bytes::from_static(br#"{"error":"unauthorized"}"#) };
        let err = Api::check(raw).err().expect("401 is an error");
        assert!(matches!(&err, ApiError::Unauthorized(m) if m == "unauthorized"));
        assert_eq!(err.status(), Some(401));
        let other = Raw { status: StatusCode::FORBIDDEN, body: Bytes::from_static(b"no") };
        assert!(matches!(Api::check(other), Err(ApiError::Status { status: 403, .. })));
    }

    #[test]
    fn fr_t17_client_error_code_is_exposed() {
        let raw = Raw {
            status: StatusCode::BAD_REQUEST,
            body: Bytes::from_static(br#"{"error":"hours must be a whole number from 1 to 168","code":"invalid_hours"}"#),
        };
        let e = Api::check(raw).err().expect("an error");
        assert_eq!(e.code(), Some("invalid_hours"));
        assert_eq!(e.status(), Some(400));
        assert_eq!(e.to_string(), "server answered 400: hours must be a whole number from 1 to 168");
        let raw = Raw { status: StatusCode::NOT_FOUND, body: Bytes::from_static(br#"{"error":"invite i1 not found","code":"not_found"}"#) };
        assert_eq!(Api::check(raw).err().expect("an error").code(), Some("not_found"));
        let raw = Raw { status: StatusCode::INTERNAL_SERVER_ERROR, body: Bytes::from_static(b"plain") };
        assert_eq!(Api::check(raw).err().expect("an error").code(), None);
    }

    #[test]
    fn fr_t14_client_not_a_member_code_and_invite_body() {
        let e = ApiError::NotAMember { message: "m".into(), unknown: vec![] };
        assert_eq!(e.code(), Some("not_a_member"));
        assert_eq!(ApiError::Unauthorized("x".into()).code(), None);
        assert_eq!(crate::channels::new_invite_body(None), json!({}));
    }

    #[test]
    fn fr_t10_client_not_a_member_maps_to_its_error() {
        let body = br#"{"error":"not a member: ghost","code":"not_a_member","reason":"not_a_member","unknown":["ghost","zed"]}"#;
        let raw = Raw { status: StatusCode::BAD_REQUEST, body: Bytes::from_static(body) };
        let err = Api::check(raw).err().expect("400 is an error");
        assert_eq!(err.status(), Some(400));
        assert!(err.to_string().contains("not a member: ghost"));
        match err {
            ApiError::NotAMember { message, unknown } => {
                assert_eq!(message, "not a member: ghost");
                assert_eq!(unknown, vec!["ghost".to_string(), "zed".to_string()]);
            }
            other => panic!("expected NotAMember, got {other:?}"),
        }
        // Another 400, with or without a code, stays a plain status error.
        for b in [&br#"{"error":"bad","code":"invalid"}"#[..], b"nope"] {
            let raw = Raw { status: StatusCode::BAD_REQUEST, body: Bytes::copy_from_slice(b) };
            assert!(matches!(Api::check(raw), Err(ApiError::Status { status: 400, .. })));
        }
    }

    #[test]
    fn fr_u7_usage_path_carries_days() {
        assert_eq!(usage_path(None), "/usage");
        assert_eq!(usage_path(Some(14)), "/usage?days=14");
    }

    #[test]
    fn fr_u7_daily_usage_decodes_server_rows() {
        let rows: Vec<DailyUsage> = serde_json::from_value(json!([
            {"account_id": "gpt-1", "day": "2026-10-25", "input_tokens": 1200, "output_tokens": 34, "reports": 3},
            {"account_id": null, "day": "2026-10-26", "input_tokens": 5, "output_tokens": 6, "reports": 1}
        ]))
        .unwrap();
        assert_eq!(rows[0].account_id.as_deref(), Some("gpt-1"));
        assert_eq!(rows[0].total_tokens(), 1234);
        assert_eq!((rows[1].account_id.clone(), rows[1].day.as_str()), (None, "2026-10-26"));
    }

    #[test]
    fn fr_s5_fork_body_names_turn_seq() {
        assert_eq!(fork_body(None), json!({}));
        assert_eq!(fork_body(Some(7)), json!({ "turn_seq": 7 }));
    }

    #[test]
    fn fr_s5_session_detail_decodes_fork_fields() {
        let d: SessionDetail = serde_json::from_value(json!({
            "session": {"id":"s","project":"p","agent":"codex","cwd":"/","status":"idle","title":"t",
                        "created_at":1,"updated_at":1,"last_seq":3},
            "live": false, "can_fork": true, "fork_turns": [2, 5],
            "fork_of": {"parent_id": "q", "turn_seq": 2, "forked_at": 9}
        }))
        .unwrap();
        let info = crate::wire::ForkInfo::from(&d);
        assert!(info.can_fork);
        assert_eq!(info.turns, vec![2, 5]);
        assert_eq!(info.origin.unwrap().parent_id, "q");
        // Older servers omit the fields.
        let d: SessionDetail = serde_json::from_value(json!({
            "session": {"id":"s","project":"p","agent":"codex","cwd":"/","status":"idle","title":"t",
                        "created_at":1,"updated_at":1,"last_seq":3},
            "live": false
        }))
        .unwrap();
        assert!(!crate::wire::ForkInfo::from(&d).can_fork);
    }

    #[test]
    fn urls() {
        let api = Api::new("http://h:1/").unwrap();
        assert_eq!(api.push_url(), "ws://h:1/api/push");
        assert_eq!(Api::new("https://h").unwrap().push_url(), "wss://h/api/push");
        assert!(Api::new("h:1").is_err());
        assert_eq!(seg("a b/c"), "a%20b%2Fc");
        assert_eq!(authority_addr("h:1"), ("h".to_string(), 1));
        assert_eq!(authority_addr("h"), ("h".to_string(), 80));
        assert_eq!(authority_addr("[::1]:8740"), ("::1".to_string(), 8740));
    }
}

// ---- token usage summary (FR-U7, #332) ---------------------------------------------------------

/// The period a usage summary covers. A day is a UTC calendar day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsagePeriod {
    /// The last N UTC days including today (the server clamps to 1..=366).
    Days(u32),
    /// One UTC day, `YYYY-MM-DD`.
    Day(String),
}

/// Tokens one harness reported. Missing fields decode as zero or `None`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct AgentUsageRow {
    pub agent: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub turns: u64,
    pub cost_usd: Option<f64>,
}

/// One UTC day of a summary.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct UsageDayRow {
    pub day: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub turns: u64,
    pub cost_usd: Option<f64>,
    pub sessions: u64,
    pub by_agent: Vec<AgentUsageRow>,
}

/// `GET /api/usage/summary`. `cost_usd` is `null` today: the SPEC has no price table.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct UsageSummary {
    pub from: String,
    pub to: String,
    pub days: Vec<UsageDayRow>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub turns: u64,
    pub cost_usd: Option<f64>,
    pub by_agent: Vec<AgentUsageRow>,
    pub cost_note: Option<String>,
}

/// `GET /api/sessions/{id}/usage`. `reported: false` means the harness sent no token counts:
/// the totals are then unknown, not zero.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct SessionUsage {
    pub session_id: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub turns: u64,
    pub cost_usd: Option<f64>,
    pub reported: bool,
    pub cost_note: Option<String>,
}

fn usage_summary_path(period: &UsagePeriod, project: Option<&str>) -> String {
    let mut q: Vec<(&str, String)> = match period {
        UsagePeriod::Days(n) => vec![("days", n.to_string())],
        UsagePeriod::Day(d) => vec![("day", d.clone())],
    };
    if let Some(p) = project {
        q.push(("project", p.to_string()));
    }
    let q = serde_urlencoded::to_string(q).unwrap_or_default();
    format!("/usage/summary?{q}")
}

impl Api {
    /// Tokens per UTC day and per harness (FR-U7), for the last N days or one day, optionally
    /// of one project. Cost is `None` until a price table is specified.
    pub async fn usage_summary(&self, period: &UsagePeriod, project: Option<&str>) -> ApiResult<UsageSummary> {
        self.get(&usage_summary_path(period, project)).await
    }

    /// Tokens one session's harness reported (FR-U7).
    pub async fn session_usage(&self, id: &str) -> ApiResult<SessionUsage> {
        self.get(&format!("/sessions/{}/usage", seg(id))).await
    }
}

// ---- project settings (FR-L26) ----------------------------------------------------------------

/// Why `PATCH /projects/{name}` failed. The server answers `{error, code}`; the status and the
/// `code` are kept so a UI can pick its own sentence and never shows the raw text.
#[derive(Debug, thiserror::Error)]
pub enum ProjectPatchError {
    /// The server refused the patch: `code` is e.g. `name_taken`, `protected_project`,
    /// `unknown_field`, `not_yet` (absent for an answer without a code).
    #[error("server answered {status} ({code:?}): {message}")]
    Rejected { status: u16, code: Option<String>, message: String },
    /// Anything else: transport, authentication, undecodable answer.
    #[error(transparent)]
    Api(#[from] ApiError),
}

impl ProjectPatchError {
    /// The error `code` of a refusal, if the server sent one.
    pub fn code(&self) -> Option<&str> {
        match self {
            ProjectPatchError::Rejected { code, .. } => code.as_deref(),
            ProjectPatchError::Api(_) => None,
        }
    }

    /// The HTTP status of a refusal.
    pub fn status(&self) -> Option<u16> {
        match self {
            ProjectPatchError::Rejected { status, .. } => Some(*status),
            ProjectPatchError::Api(ApiError::Status { status, .. }) => Some(*status),
            ProjectPatchError::Api(_) => None,
        }
    }

    /// Reads a non-success answer: status, `code` and `error` of a JSON body.
    pub(crate) fn from_answer(status: u16, body: &[u8]) -> ProjectPatchError {
        let text = String::from_utf8_lossy(body).into_owned();
        let v: Option<serde_json::Value> = serde_json::from_str(&text).ok();
        let code = v.as_ref().and_then(|v| v.get("code")).and_then(|c| c.as_str()).map(str::to_string);
        let message = v.as_ref().and_then(|v| v.get("error")).and_then(|e| e.as_str()).map(str::to_string).unwrap_or(text);
        if status == 401 {
            return ProjectPatchError::Api(ApiError::Unauthorized(message));
        }
        ProjectPatchError::Rejected { status, code, message }
    }
}

impl Api {
    /// `PATCH /projects/{name}` (FR-L26): only the fields set in `patch` are sent. Answers the
    /// updated project; a rename answers under the new name.
    pub async fn patch_project(&self, name: &str, patch: &crate::wire::ProjectPatch) -> Result<Project, ProjectPatchError> {
        let body = serde_json::to_value(patch).expect("ProjectPatch serialises");
        let raw = self.send(Method::PATCH, &format!("/projects/{}", seg(name)), Some(body)).await?;
        if !raw.status.is_success() {
            return Err(ProjectPatchError::from_answer(raw.status.as_u16(), &raw.body));
        }
        serde_json::from_slice(&raw.body).map_err(|e| ProjectPatchError::Api(ApiError::Decode(e.to_string())))
    }

    /// `DELETE /projects/{name}` (FR-L26, #384): deletes an empty project. A refusal keeps
    /// status and `code` (`project_not_empty` 409, `protected_project` 403, `not_found` 404).
    pub async fn delete_project(&self, name: &str) -> Result<(), ProjectPatchError> {
        let raw = self.send(Method::DELETE, &format!("/projects/{}", seg(name)), None).await?;
        if !raw.status.is_success() {
            return Err(ProjectPatchError::from_answer(raw.status.as_u16(), &raw.body));
        }
        Ok(())
    }
}

#[cfg(test)]
mod project_patch_tests {
    use super::*;
    use crate::wire::{MemberDefaults, ProjectPatch};

    #[test]
    fn fr_l26_a_patch_sends_only_its_set_fields() {
        let p = ProjectPatch { repository: Some("github.com/a/b".into()), ..Default::default() };
        assert_eq!(serde_json::to_value(&p).unwrap(), json!({"repository": "github.com/a/b"}));
        assert!(ProjectPatch::default().is_empty());
        assert_eq!(serde_json::to_value(ProjectPatch::default()).unwrap(), json!({}));
    }

    #[test]
    fn fr_l26_clearing_sends_an_empty_string_or_null() {
        let p = ProjectPatch { default_branch: Some(String::new()), member_defaults: Some(None), ..Default::default() };
        assert_eq!(serde_json::to_value(&p).unwrap(), json!({"default_branch": "", "member_defaults": null}));
        let p = ProjectPatch {
            member_defaults: Some(Some(MemberDefaults { agent: Some("codex".into()), ..Default::default() })),
            ..Default::default()
        };
        assert_eq!(serde_json::to_value(&p).unwrap(), json!({"member_defaults": {"agent": "codex"}}));
    }

    #[test]
    fn fr_l26_profile_overrides_decode_with_tolerant_defaults() {
        let p: Project = serde_json::from_value(json!({"name": "p", "profile_overrides": {"codex": "acc-1", "claude-code": null}})).unwrap();
        assert_eq!(p.profile_overrides.get("codex"), Some(&Some("acc-1".to_string())));
        assert_eq!(p.profile_overrides.get("claude-code"), Some(&None), "a deleted account reads as null");
        let missing: Project = serde_json::from_value(json!({"name": "p"})).unwrap();
        assert!(missing.profile_overrides.is_empty());
        let null: Project = serde_json::from_value(json!({"name": "p", "profile_overrides": null})).unwrap();
        assert!(null.profile_overrides.is_empty());
    }

    #[test]
    fn fr_l26_profile_overrides_patch_sends_the_whole_object_or_nothing() {
        assert_eq!(serde_json::to_value(ProjectPatch::default()).unwrap(), json!({}));
        let mut m = std::collections::BTreeMap::new();
        m.insert("codex".to_string(), Some("acc-1".to_string()));
        let p = ProjectPatch { profile_overrides: Some(m), ..Default::default() };
        assert!(!p.is_empty());
        assert_eq!(serde_json::to_value(&p).unwrap(), json!({"profile_overrides": {"codex": "acc-1"}}));
        let p = ProjectPatch { profile_overrides: Some(Default::default()), ..Default::default() };
        assert_eq!(serde_json::to_value(&p).unwrap(), json!({"profile_overrides": {}}), "an empty map clears all");
    }

    #[test]
    fn fr_l26_archive_is_sent_as_a_bool() {
        let p = ProjectPatch { archived: Some(true), ..Default::default() };
        assert!(!p.is_empty());
        assert_eq!(serde_json::to_value(&p).unwrap(), json!({"archived": true}));
        let p = ProjectPatch { archived: Some(false), ..Default::default() };
        assert_eq!(serde_json::to_value(&p).unwrap(), json!({"archived": false}));
    }

    #[test]
    fn fr_l26_a_delete_refusal_keeps_status_and_code() {
        let e = ProjectPatchError::from_answer(409, br#"{"error":"busy","code":"project_not_empty","sessions":2}"#);
        assert_eq!((e.status(), e.code()), (Some(409), Some("project_not_empty")));
    }

    #[test]
    fn fr_l26_a_refusal_keeps_status_and_code() {
        let e = ProjectPatchError::from_answer(409, br#"{"error":"name taken","code":"name_taken"}"#);
        assert_eq!((e.status(), e.code()), (Some(409), Some("name_taken")));
        let e = ProjectPatchError::from_answer(502, b"bad gateway");
        assert_eq!((e.status(), e.code()), (Some(502), None));
        assert!(matches!(ProjectPatchError::from_answer(401, b"{}"), ProjectPatchError::Api(ApiError::Unauthorized(_))));
    }

    #[test]
    fn fr_l26_a_project_decodes_with_and_without_the_new_fields() {
        let old: Project = serde_json::from_value(json!({"name": "p", "created_at": 1, "computers": ["local"]})).unwrap();
        assert_eq!((old.repository, old.member_defaults, old.archived), (None, None, false));
        let new: Project = serde_json::from_value(json!({
            "name": "p", "repository": "r", "default_branch": "develop",
            "member_defaults": {"agent": "codex", "mode": "plan"}, "character_style": "crew", "archived": false
        }))
        .unwrap();
        assert_eq!(new.default_branch.as_deref(), Some("develop"));
        assert_eq!(new.member_defaults.unwrap().mode.as_deref(), Some("plan"));
        let nulls: Project = serde_json::from_value(json!({"name": "p", "member_defaults": null, "character_style": null})).unwrap();
        assert_eq!(nulls.member_defaults, None);
    }
}

#[cfg(test)]
mod usage_summary_tests {
    use super::*;

    #[test]
    fn fr_u7_summary_path_carries_days_day_and_project() {
        assert_eq!(usage_summary_path(&UsagePeriod::Days(7), None), "/usage/summary?days=7");
        assert_eq!(
            usage_summary_path(&UsagePeriod::Day("2026-11-06".into()), Some("a b")),
            "/usage/summary?day=2026-11-06&project=a+b"
        );
    }

    #[test]
    fn fr_u7_summary_decodes_server_shape_and_null_cost() {
        let s: UsageSummary = serde_json::from_value(json!({
            "from": "2026-11-05", "to": "2026-11-06",
            "days": [{"day": "2026-11-06", "input_tokens": 10, "output_tokens": 5, "total_tokens": 15,
                      "turns": 2, "cost_usd": null, "sessions": 1,
                      "by_agent": [{"agent": "codex", "input_tokens": 10, "output_tokens": 5, "total_tokens": 15, "turns": 2, "cost_usd": null}]}],
            "input_tokens": 10, "output_tokens": 5, "total_tokens": 15, "turns": 2, "cost_usd": null,
            "by_agent": [], "cost_note": "no price table"
        }))
        .unwrap();
        assert_eq!(s.days[0].by_agent[0].agent, "codex");
        assert_eq!(s.total_tokens, 15);
        assert_eq!(s.cost_usd, None);
        assert_eq!(s.cost_note.as_deref(), Some("no price table"));
    }

    #[test]
    fn fr_u7_summary_decode_is_tolerant_of_missing_fields() {
        let s: UsageSummary = serde_json::from_value(json!({})).unwrap();
        assert_eq!(s, UsageSummary::default());
        let u: SessionUsage = serde_json::from_value(json!({"session_id": "s"})).unwrap();
        assert!(!u.reported, "a missing flag is unknown, not reported");
        let u: SessionUsage = serde_json::from_value(json!({"session_id": "s", "total_tokens": 9, "reported": true})).unwrap();
        assert!(u.reported && u.total_tokens == 9);
    }
}

// ---- harness detection (FR-A6.1) -------------------------------------------------------------

/// One entry of `POST /api/computers/{id}/harnesses/detect`. Every field but `harness` is
/// tolerant: a missing `version` or `path` is unknown, never invented.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct DetectedHarness {
    pub harness: String,
    #[serde(default)]
    pub installed: bool,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    /// Epoch ms of the probe.
    #[serde(default)]
    pub detected_at: Option<i64>,
}

/// `POST /computers/{id}/harnesses/detect`, below `/api`.
pub(crate) fn detect_path(computer_id: &str) -> String {
    format!("/computers/{}/harnesses/detect", seg(computer_id))
}

impl Api {
    /// Probe a computer's harnesses now. `local` is this server. Another computer answers
    /// 501 `not_implemented` (`code()` says so) and an unknown id 404.
    pub async fn detect_harnesses(&self, computer_id: &str) -> ApiResult<Vec<DetectedHarness>> {
        let raw = self.post(&detect_path(computer_id), json!({})).await?;
        serde_json::from_slice(&raw.body).map_err(|e| ApiError::Decode(e.to_string()))
    }
}

#[cfg(test)]
mod detect_tests {
    use super::*;

    #[test]
    fn fr_a6_1_detect_decodes_server_shape_and_nulls() {
        let v: Vec<DetectedHarness> = serde_json::from_value(json!([
            {"harness": "claude-code", "installed": true, "version": "2.1.4", "path": "/opt/x/claude", "detected_at": 1761900000000i64},
            {"harness": "codex", "installed": false, "version": null, "path": null, "detected_at": 1761900000000i64},
            {"harness": "omp"}
        ]))
        .unwrap();
        assert_eq!(v[0].path.as_deref(), Some("/opt/x/claude"));
        assert_eq!(v[0].detected_at, Some(1761900000000));
        assert!(!v[1].installed && v[1].version.is_none() && v[1].path.is_none());
        assert_eq!(v[2], DetectedHarness { harness: "omp".into(), ..Default::default() });
    }

    #[test]
    fn fr_a6_1_detect_path_escapes_the_id() {
        assert_eq!(detect_path("local"), "/computers/local/harnesses/detect");
        assert_eq!(detect_path("a/b c"), "/computers/a%2Fb%20c/harnesses/detect");
    }

    #[test]
    fn fr_a6_1_not_implemented_maps_to_status_and_code() {
        let raw = Raw {
            status: StatusCode::NOT_IMPLEMENTED,
            body: Bytes::from_static(br#"{"error":"not yet","code":"not_implemented"}"#),
        };
        let e = Api::check(raw).unwrap_err();
        assert_eq!(e.status(), Some(501));
        assert_eq!(e.code(), Some("not_implemented"));
    }
}

// ---- hub devices, read only (FR-C1 / FR-C2, D39, #490) ------------------------

/// What a device says it runs (`app` of the hub's device). Tolerant: a hint only.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct HubDeviceApp {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub version: String,
}

/// One entry of `GET /api/hub/devices`: the hub's `Device` fields the UI needs, flattened, plus
/// the server's own flags. The hub reports no operating system, so none is shown.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct HubDevice {
    pub endpoint_id: String,
    #[serde(default)]
    pub name: String,
    /// `main_server`, `computer` or `client`; empty when the server did not say.
    #[serde(default)]
    pub role: String,
    /// `None` when the hub did not report it.
    #[serde(default)]
    pub online: Option<bool>,
    #[serde(default)]
    pub last_seen: Option<i64>,
    #[serde(default)]
    pub app: Option<HubDeviceApp>,
    /// This server itself.
    #[serde(default)]
    pub this_server: bool,
    /// The computer registered for this device, if any.
    #[serde(default)]
    pub computer_id: Option<String>,
    /// On this server's devices allow-list.
    #[serde(default)]
    pub allowed: bool,
}

/// A hub call failed. 503 means the server runs with the hub off, which is a state and not a
/// failure; the message of the server is never carried to the screen.
#[derive(Debug)]
pub enum HubCallError {
    Off,
    Api(ApiError),
}

impl HubCallError {
    pub fn status(&self) -> Option<u16> {
        match self {
            HubCallError::Off => Some(503),
            HubCallError::Api(e) => e.status(),
        }
    }
}

impl From<ApiError> for HubCallError {
    fn from(e: ApiError) -> Self {
        if e.status() == Some(503) {
            HubCallError::Off
        } else {
            HubCallError::Api(e)
        }
    }
}

/// `/hub/devices`, below `/api`.
pub(crate) fn hub_devices_path() -> &'static str {
    "/hub/devices"
}

impl Api {
    /// The account's devices as the hub integration lists them.
    pub async fn hub_devices(&self) -> Result<Vec<HubDevice>, HubCallError> {
        Ok(self.get(hub_devices_path()).await?)
    }
}

#[cfg(test)]
mod hub_device_tests {
    use super::*;

    #[test]
    fn fr_c2_hub_devices_decode_the_server_shape_and_missing_fields() {
        let v: Vec<HubDevice> = serde_json::from_value(json!([
            {"endpoint_id": "abc", "name": "studio", "role": "computer", "created_at": 1, "last_seen": 5,
             "online": true, "app": {"kind": "ember-node", "version": "0.9", "services": []},
             "this_server": false, "computer_id": "studio", "allowed": true},
            {"endpoint_id": "def"}
        ]))
        .unwrap();
        assert_eq!(v[0].computer_id.as_deref(), Some("studio"));
        assert_eq!(v[0].app.as_ref().map(|a| a.kind.as_str()), Some("ember-node"));
        assert!(v[0].online == Some(true) && v[0].allowed && !v[0].this_server);
        assert_eq!(v[1], HubDevice { endpoint_id: "def".into(), ..Default::default() });
        assert_eq!(v[1].online, None);
    }

    #[test]
    fn fr_c2_hub_devices_path_is_fixed() {
        assert_eq!(hub_devices_path(), "/hub/devices");
    }

    #[test]
    fn fr_c2_a_503_is_the_typed_hub_off_error() {
        let raw = Raw {
            status: StatusCode::SERVICE_UNAVAILABLE,
            body: Bytes::from_static(br#"{"error":"the hub is off"}"#),
        };
        let e: HubCallError = Api::check(raw).unwrap_err().into();
        assert!(matches!(e, HubCallError::Off));
        let raw = Raw { status: StatusCode::BAD_REQUEST, body: Bytes::from_static(br#"{"error":"x"}"#) };
        let e: HubCallError = Api::check(raw).unwrap_err().into();
        assert_eq!(e.status(), Some(400));
    }
}

// ---- Enrol code flow (FR-C1, FR-C2, FR-C2c, FR-C2g, D39) ----

/// The answer to minting a one-time enrol code. The code and the command hold the secret, so
/// `Debug` prints neither them nor the fingerprint (FR-C2g). The server sends `code` once.
/// `door`, `server` and `server_fingerprint` are optional: older servers omit them.
#[derive(Clone, Deserialize, PartialEq)]
pub struct EnrolMinted {
    pub id: String,
    pub code: String,
    pub expires_ms: i64,
    /// The line to run on the other machine, exactly as the server gave it.
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub warning: Option<String>,
    /// `on` when present. A closed door mints nothing (409 `door_off`).
    #[serde(default)]
    pub door: Option<String>,
    /// The server's full transport peer id; the server pins it in `command`. Omitted when the
    /// transport is off. Never printed.
    #[serde(default)]
    pub server_peer_id: Option<String>,
    /// The door URL the node should use.
    #[serde(default)]
    pub server: Option<String>,
    /// The server's short fingerprint, for comparison on screen only.
    #[serde(default)]
    pub server_fingerprint: Option<String>,
}

impl fmt::Debug for EnrolMinted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnrolMinted")
            .field("id", &self.id)
            .field("code", &"<redacted>")
            .field("expires_ms", &self.expires_ms)
            .field("command", &"<redacted>")
            .field("door", &self.door)
            .finish_non_exhaustive()
    }
}

/// The state of an enrolment, as the admin status route spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrolState {
    Waiting,
    AwaitingConfirmation,
    Joined,
    Denied,
    Expired,
    /// A state this client does not know; shown as a failure, never guessed.
    #[serde(other)]
    Unknown,
}

/// What a node says about itself. Untrusted display data: show it as plain text.
#[derive(Clone, Default, Deserialize, PartialEq)]
pub struct EnrolNodeInfo {
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub arch: String,
    #[serde(default)]
    pub node_version: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub peer_id: Option<String>,
}

impl fmt::Debug for EnrolNodeInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnrolNodeInfo").field("hostname", &self.hostname).finish_non_exhaustive()
    }
}

/// `GET /computers/enrol-codes/{id}`. The optional fields appear only while the owner can decide.
#[derive(Clone, Deserialize, PartialEq)]
pub struct EnrolStatus {
    pub status: EnrolState,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub node_info: Option<EnrolNodeInfo>,
    #[serde(default)]
    pub observed_addr: Option<String>,
    #[serde(default)]
    pub proposed_url: Option<String>,
    /// Short form of the NODE's peer id, to compare with the node's screen.
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub confirm_deadline_ms: Option<i64>,
    /// How the server will register the computer: `url`, `peer` or `observed`. Appears while the
    /// owner can decide; absent from an older server.
    #[serde(default)]
    pub registered_by: Option<String>,
}

impl fmt::Debug for EnrolStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnrolStatus").field("status", &self.status).finish_non_exhaustive()
    }
}

/// What a confirm answered: `{computer_id, name}`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct EnrolConfirmed {
    pub computer_id: String,
    #[serde(default)]
    pub name: String,
}

/// An enrol call failed. The server's text never reaches the screen; the kind decides the line.
#[derive(Debug)]
pub enum EnrolCallError {
    /// 503: the part of the server behind the call is off.
    Off,
    /// 404: the enrolment is not known (any more).
    NotFound,
    /// 409 `door_off`: the enrol door is not listening, so no code was made.
    DoorOff,
    /// 409 `door_url_unknown`: the door is on but no usable public URL is known; no code was made.
    DoorUrlUnknown,
    /// 409 `name_taken`: choose another name; the enrolment stays awaiting confirmation.
    NameTaken,
    /// 409 `wrong_state`: it is no longer awaiting confirmation.
    WrongState,
    /// 409 `fingerprint_mismatch`: the typed fingerprint differs from the node's (or the node has
    /// no peer id). Nothing changed; the enrolment stays awaiting confirmation.
    FingerprintMismatch,
    /// 409 `fingerprint_required`: the node has only a peer id and no fingerprint was sent.
    FingerprintRequired,
    /// 410: the enrolment expired.
    Expired,
    Api(ApiError),
}

impl EnrolCallError {
    pub fn status(&self) -> Option<u16> {
        match self {
            EnrolCallError::Off => Some(503),
            EnrolCallError::NotFound => Some(404),
            EnrolCallError::NameTaken | EnrolCallError::WrongState | EnrolCallError::FingerprintMismatch | EnrolCallError::FingerprintRequired | EnrolCallError::DoorOff | EnrolCallError::DoorUrlUnknown => Some(409),
            EnrolCallError::Expired => Some(410),
            EnrolCallError::Api(e) => e.status(),
        }
    }
}

impl From<ApiError> for EnrolCallError {
    fn from(e: ApiError) -> Self {
        match &e {
            ApiError::Status { status: 503, .. } => EnrolCallError::Off,
            ApiError::Status { status: 404, .. } => EnrolCallError::NotFound,
            ApiError::Status { status: 410, .. } => EnrolCallError::Expired,
            ApiError::Status { status: 409, message, .. } if message == "door_off" => EnrolCallError::DoorOff,
            ApiError::Status { status: 409, message, .. } if message == "door_url_unknown" => EnrolCallError::DoorUrlUnknown,
            ApiError::Status { status: 409, message, .. } if message == "name_taken" => EnrolCallError::NameTaken,
            ApiError::Status { status: 409, message, .. } if message == "wrong_state" => EnrolCallError::WrongState,
            ApiError::Status { status: 409, message, .. } if message == "fingerprint_mismatch" => EnrolCallError::FingerprintMismatch,
            ApiError::Status { status: 409, message, .. } if message == "fingerprint_required" => EnrolCallError::FingerprintRequired,
            _ => EnrolCallError::Api(e),
        }
    }
}

/// Paths below `/api`. The id is one path segment.
pub(crate) fn enrol_codes_path() -> &'static str {
    "/computers/enrol-codes"
}

pub(crate) fn enrol_code_path(id: &str, tail: &str) -> String {
    format!("{}/{}{tail}", enrol_codes_path(), seg(id))
}

/// `POST` body of a mint: `{name}`.
pub(crate) fn enrol_mint_body(name: &str) -> serde_json::Value {
    json!({ "name": name })
}

/// `POST .../confirm` body: `{name}` and, only when given, `expect_fingerprint` (the text the owner
/// typed from the node's screen; mandatory for a node that reported a peer id and no url). The
/// server refuses other unknown fields (422).
pub(crate) fn enrol_confirm_body(name: &str, expect_fingerprint: Option<&str>) -> serde_json::Value {
    match expect_fingerprint {
        Some(fp) => json!({ "name": name, "expect_fingerprint": fp }),
        None => json!({ "name": name }),
    }
}

impl Api {
    /// Mints a one-time code for a new computer called `name`.
    pub async fn enrol_mint(&self, name: &str) -> Result<EnrolMinted, EnrolCallError> {
        Ok(Self::decode(self.post(enrol_codes_path(), enrol_mint_body(name)).await?)?)
    }

    pub async fn enrol_status(&self, id: &str) -> Result<EnrolStatus, EnrolCallError> {
        Ok(self.get(&enrol_code_path(id, "")).await?)
    }

    /// Confirms the enrolment under `name` (the owner may rename). `expect_fingerprint` is the text
    /// the owner typed from the node's screen, sent only for a peer-only node.
    pub async fn enrol_confirm(&self, id: &str, name: &str, expect_fingerprint: Option<&str>) -> Result<EnrolConfirmed, EnrolCallError> {
        Ok(Self::decode(self.post(&enrol_code_path(id, "/confirm"), enrol_confirm_body(name, expect_fingerprint)).await?)?)
    }

    pub async fn enrol_deny(&self, id: &str) -> Result<(), EnrolCallError> {
        self.post(&enrol_code_path(id, "/deny"), json!({})).await?;
        Ok(())
    }

    /// Cancels a code that is not used yet.
    pub async fn enrol_cancel(&self, id: &str) -> Result<(), EnrolCallError> {
        Self::check(self.send(Method::DELETE, &enrol_code_path(id, ""), None).await?)?;
        Ok(())
    }
}

#[cfg(test)]
mod enrol_tests {
    use super::*;

    const CODE: &str = "SECRETCODE-0123456789abcdef";

    fn minted() -> EnrolMinted {
        serde_json::from_value(json!({
            "id": "e1", "code": CODE, "expires_ms": 5,
            "command": format!("ember-node enrol {CODE} --server http://x:1"),
            "warning": "w", "door": "on", "server": "http://x:1", "server_fingerprint": "ab12 cd34", "server_peer_id": "PEERID9999"
        }))
        .unwrap()
    }

    #[test]
    fn fr_c2_enrol_minted_debug_hides_the_code_command_and_fingerprint() {
        let m = minted();
        let d = format!("{m:?}");
        assert!(!d.contains(CODE) && !d.contains("ember-node enrol") && !d.contains("ab12") && !d.contains("PEERID9999"), "{d}");
        assert!(d.contains("e1"));
        assert_eq!(m.code, CODE);
    }

    #[test]
    fn fr_c2_enrol_minted_decodes_the_old_shape_without_door_keys() {
        let m: EnrolMinted = serde_json::from_value(json!({"id":"e","code":"c","expires_ms":1,"command":"x","warning":"w"})).unwrap();
        assert_eq!((m.door, m.server, m.server_fingerprint), (None, None, None));
    }

    #[test]
    fn fr_c2_enrol_status_decodes_every_state_and_missing_fields() {
        for (s, want) in [
            ("waiting", EnrolState::Waiting),
            ("awaiting_confirmation", EnrolState::AwaitingConfirmation),
            ("joined", EnrolState::Joined),
            ("denied", EnrolState::Denied),
            ("expired", EnrolState::Expired),
            ("something_new", EnrolState::Unknown),
        ] {
            let st: EnrolStatus = serde_json::from_value(json!({"status": s, "name": "lab"})).unwrap();
            assert_eq!(st.status, want);
            assert!(st.node_info.is_none() && st.observed_addr.is_none() && st.fingerprint.is_none());
        }
        let st: EnrolStatus = serde_json::from_value(json!({
            "status": "awaiting_confirmation", "name": "lab", "observed_addr": "10.0.0.5:5000",
            "node_info": {"hostname": "box", "os": "linux", "arch": "x86_64", "node_version": "0.1"},
            "fingerprint": "ab12 cd34", "confirm_deadline_ms": 9, "proposed_url": "http://10.0.0.5:8741"
        }))
        .unwrap();
        assert_eq!(st.node_info.as_ref().map(|n| n.hostname.as_str()), Some("box"));
        assert!(!format!("{st:?}").contains("ab12"));
        assert_eq!(st.registered_by, None);
        let st: EnrolStatus = serde_json::from_value(json!({"status": "awaiting_confirmation", "registered_by": "peer"})).unwrap();
        assert_eq!(st.registered_by.as_deref(), Some("peer"));
    }

    #[test]
    fn fr_c2_enrol_paths_and_bodies_are_fixed_and_carry_no_code_or_fingerprint() {
        assert_eq!(enrol_codes_path(), "/computers/enrol-codes");
        assert_eq!(enrol_code_path("e1", "/confirm"), "/computers/enrol-codes/e1/confirm");
        assert_eq!(enrol_mint_body("lab"), json!({"name": "lab"}));
        let c = enrol_confirm_body("lab", None);
        assert_eq!(c, json!({"name": "lab"}));
        assert!(c.get("expect_fingerprint").is_none());
        assert_eq!(enrol_confirm_body("lab", Some("ab12 cd34")), json!({"name": "lab", "expect_fingerprint": "ab12 cd34"}));
        assert!(!enrol_code_path("a/b", "").contains("a/b"));
    }

    #[test]
    fn fr_c2_enrol_errors_are_typed_by_status_and_key() {
        let st = |status: u16, message: &str| ApiError::Status { status, message: message.into(), code: None };
        assert!(matches!(EnrolCallError::from(st(503, "x")), EnrolCallError::Off));
        assert!(matches!(EnrolCallError::from(st(404, "x")), EnrolCallError::NotFound));
        assert!(matches!(EnrolCallError::from(st(410, "expired")), EnrolCallError::Expired));
        assert!(matches!(EnrolCallError::from(st(409, "name_taken")), EnrolCallError::NameTaken));
        assert!(matches!(EnrolCallError::from(st(409, "wrong_state")), EnrolCallError::WrongState));
        assert!(matches!(EnrolCallError::from(st(409, "fingerprint_mismatch")), EnrolCallError::FingerprintMismatch));
        assert_eq!(EnrolCallError::FingerprintMismatch.status(), Some(409));
        assert!(matches!(EnrolCallError::from(st(409, "fingerprint_required")), EnrolCallError::FingerprintRequired));
        assert_eq!(EnrolCallError::FingerprintRequired.status(), Some(409));
        assert!(matches!(EnrolCallError::from(st(409, "door_off")), EnrolCallError::DoorOff));
        assert!(matches!(EnrolCallError::from(st(409, "door_url_unknown")), EnrolCallError::DoorUrlUnknown));
        assert_eq!(EnrolCallError::from(st(400, "bad")).status(), Some(400));
    }
}

// ---- device scope (FR-N8, #366) --------------------------------------------------------------

/// What an admitted device may call over the transport (server `devices::Scope`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DeviceScope {
    #[default]
    Full,
    ReadOnly,
    ChatOnly,
}

/// One row of `GET /api/devices` (server `devices::Device`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRecord {
    pub peer_id: String,
    pub name: String,
    pub created_at: i64,
    pub source: String,
    /// Absent on a server older than #366, which means `full`.
    #[serde(default)]
    pub scope: DeviceScope,
}

#[cfg(test)]
mod device_scope_tests {
    use super::*;

    #[test]
    fn scope_wire_names_match_the_server() {
        assert_eq!(serde_json::to_value(DeviceScope::ReadOnly).unwrap(), "read_only");
        assert_eq!(serde_json::to_value(DeviceScope::ChatOnly).unwrap(), "chat_only");
        let d: DeviceRecord =
            serde_json::from_str(r#"{"peer_id":"p","name":"n","created_at":1,"source":"local"}"#).unwrap();
        assert_eq!(d.scope, DeviceScope::Full);
    }
}
