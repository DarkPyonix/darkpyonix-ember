//! The daemon's HTTP/WebSocket API. Routes carry no version prefix; every route except
//! `/health` requires `Authorization: Bearer <EMBER_NODE_TOKEN>`.
//!
//! | Method | Path | Body → Response |
//! | ------ | ---- | --------------- |
//! | GET  | `/health` | → [`Health`] (no auth) |
//! | GET  | `/env` | → [`EnvInfo`] |
//! | POST | `/fs/stat` | [`PathRequest`] → [`Stat`] |
//! | POST | `/fs/read` | [`ReadRequest`] → [`ReadResponse`] |
//! | POST | `/fs/write` | [`WriteRequest`] → [`WriteResponse`] (412 on precondition) |
//! | POST | `/fs/list` | [`PathRequest`] → [`ListResponse`] |
//! | POST | `/fs/lstat` | [`PathRequest`] → [`Stat`] (final symlink not followed) |
//! | POST | `/fs/readlink` | [`PathRequest`] → [`ReadlinkResponse`] |
//! | POST | `/fs/symlink` | [`SymlinkRequest`] → [`Stat`] |
//! | POST | `/fs/mkdir` | [`MkdirRequest`] → [`Stat`] |
//! | POST | `/fs/remove` | [`RemoveRequest`] → 204 |
//! | POST | `/fs/rename` | [`RenameRequest`] → 204 |
//! | POST | `/fs/setattr` | [`SetAttrRequest`] → [`Stat`] (chmod / truncate / utimes) |
//! | POST | `/fs/pwrite` | [`PwriteRequest`] → [`Stat`] (in-place positional write) |
//! | POST | `/fs/glob` | [`GlobRequest`] → [`GlobResponse`] |
//! | POST | `/fs/grep` | [`GrepRequest`] → [`GrepResponse`] |
//! | POST | `/git/log` | [`git_log::GitLogRequest`] → [`git_log::GitLogResponse`] |
//! | POST | `/git/branches` | [`GitRepoRequest`] → [`git_branches::GitBranchesResponse`] (FR-G9: local branches with upstream ahead/behind, last commit, default and worktree) |
//! | POST | `/git/prs` | [`git_prs::GitPrsRequest`] → [`git_prs::GitPrsResponse`] (FR-G8, through `gh pr list`; 409 when gh is missing, not logged in or the repository has no GitHub remote) |
//! | POST | `/git/status` | [`GitRepoRequest`] → [`GitStatus`] (404 `not_a_repo`; 409 for a bare repository or dubious ownership) |
//! | POST | `/git/diff` | [`GitDiffRequest`] → [`GitDiff`] (`include_untracked` adds all untracked files; 413 `too_large` past 8 MiB of git output) |
//! | POST | `/git/stage`, `/git/unstage` | [`GitPathsRequest`] → 204 |
//! | POST | `/git/apply` | [`GitApplyRequest`] → 204 (patch to the index; 409 if it does not apply) |
//! | POST | `/git/commit` | [`GitCommitRequest`] → [`GitCommitResponse`] (409 on an empty index) |
//! | POST | `/git/checkout` | [`GitCheckoutRequest`] → 204 |
//! | POST | `/git/branch` | [`GitBranchRequest`] → 204 (creates and checks out) |
//! | POST | `/git/fetch`, `/git/pull`, `/git/push` | [`GitRemoteRequest`] → [`GitOutput`] (409 with a Korean message and git's text; 400 for a remote that is not in `git remote`; 504 `timeout` after 60 s; pull is fast-forward only; none of them holds the write lock across the network) |
//! | POST | `/git/stash/{list,push,apply,pop,drop}` | FR-G6 stash list and operations (409 on conflicts) |
//! | POST | `/git/conflict`, `/git/resolve` | conflict blocks and resolution (FR-G7) |
//! | POST | `/git/conflicts` | [`GitRepoRequest`] → [`GitConflicts`] |
//! | GET (WS) | `/exec` | send [`ExecRequest`], then [`ExecInput`]s; receive [`ExecEvent`]s |
//! | GET (WS) | `/exec-server` | raw byte relay to `codex exec-server --listen stdio` ([`crate::exec_server`]) |
//! | GET (WS) | `/egress` | one proxied TCP connection as a raw SOCKS5 byte stream ([`crate::egress`]; 403 when disabled) |
//! | POST | `/jobs` | [`JobRequest`] → [`JobInfo`] |
//! | GET  | `/jobs` | → `[JobInfo]` |
//! | GET  | `/jobs/{id}?tail=<bytes>` | → [`JobDetail`] |
//! | POST | `/jobs/{id}/kill` | [`KillRequest`] → 204 |
//! | DELETE | `/jobs/{id}` | → 204 (409 while running) |
//! | GET (WS) | `/events?after=<seq>` | receive [`NodeEvent`]s |
//! | GET  | `/ports` | → [`PortsResponse`] (FR-P7: listening TCP ports of processes under node-owned terms and jobs) |
//! | POST | `/terms` | [`TermCreateRequest`] → [`TermCreateResponse`] (201 created, 200 key matched) |
//! | GET  | `/terms?project=&origin=&running=` | → `[TermInfo]` |
//! | GET  | `/terms/{id}` | → [`TermInfo`] |
//! | GET  | `/terms/{id}/snapshot` | → [`TermSnapshot`] |
//! | GET (WS) | `/terms/{id}/attach` | send [`TermHello`], then [`TermInput`]s; receive [`TermEvent`]s |
//! | POST | `/terms/{id}/control` | [`TermControlRequest`] → 204 |
//! | POST | `/terms/{id}/kill` | [`KillRequest`] → 204 (default SIGHUP, then SIGKILL) |
//! | DELETE | `/terms/{id}` | → 204 (409 while running) |
//!
//! Closing the `/exec` socket kills the command's process group; jobs are unaffected by any
//! connection. Persistent terminal sessions (`/terms`, [`crate::term`]) are owned by the
//! daemon: closing an attach socket only detaches. Errors are [`ErrorBody`] with a matching
//! status code; file errors also carry the portable `errno` name (409 for `EEXIST` /
//! `ENOTEMPTY`, 400 for `ENOTDIR` / `EISDIR` / `EINVAL`).
//!
//! The router is transport-agnostic: [`serve`] accepts any [`axum::serve::Listener`], so the same
//! API runs over TCP and over the peer-to-peer transport ([`crate::transport`], service
//! `ember-node`, peer allow-list in front). The remote browser's SOCKS5 egress (FR-R1) is the
//! `/egress` route, so it rides the same authenticated connection; an optional plain SOCKS5
//! listener is in [`crate::egress`].

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;

use crate::config::NodeConfig;
use crate::exec::{self, kill_group};
use crate::fs::{self, FsError};
use crate::git::{self, *};
use crate::git::git_stash::{self, GitStashEntry, GitStashIndexRequest, GitStashOperation, GitStashPushRequest};
use crate::git_conflict::{self, GitConflict, GitConflictRequest, GitResolveRequest};
use crate::jobs::{Jobs, Removal};
use crate::policy::{PathPolicy, PolicyError};
use crate::proto::*;
use crate::term::Terms;

struct Inner {
    token: String,
    policy: PathPolicy,
    jobs: Arc<Jobs>,
    terms: Arc<Terms>,
    egress: Arc<crate::egress::EgressPolicy>,
    /// Serialises writes so a hash precondition and the rename that follows it are atomic with
    /// respect to other writes through this daemon.
    write_lock: tokio::sync::Mutex<()>,
}

/// Daemon state; cheap to clone.
#[derive(Clone)]
pub struct Node(Arc<Inner>);

impl Node {
    pub fn new(config: NodeConfig) -> anyhow::Result<Self> {
        anyhow::ensure!(!config.token.is_empty(), "token must not be empty");
        let jobs = Jobs::new();
        let terms = Terms::new(config.state_dir.as_deref(), config.pty_keeper.clone(), jobs.clone());
        Ok(Self(Arc::new(Inner {
            token: config.token,
            policy: PathPolicy::with_denied(config.roots, config.denied)?,
            jobs,
            terms,
            egress: Arc::new(config.egress),
            write_lock: tokio::sync::Mutex::new(()),
        })))
    }

    pub fn jobs(&self) -> &Arc<Jobs> {
        &self.0.jobs
    }

    pub fn terms(&self) -> &Arc<Terms> {
        &self.0.terms
    }

    pub fn policy(&self) -> &PathPolicy {
        &self.0.policy
    }

    pub fn egress_policy(&self) -> &Arc<crate::egress::EgressPolicy> {
        &self.0.egress
    }
}

/// Serve the API on any listener until the future is dropped.
pub async fn serve<L>(listener: L, node: Node) -> std::io::Result<()>
where
    L: axum::serve::Listener,
    L::Addr: std::fmt::Debug,
{
    axum::serve(listener, router(node)).await
}

pub fn router(node: Node) -> Router {
    let authed = Router::new()
        .route("/env", get(env))
        .route("/metrics", get(metrics))
        .route("/fs/stat", post(stat))
        .route("/fs/read", post(read))
        .route("/fs/write", post(write))
        .route("/fs/list", post(list))
        .route("/fs/lstat", post(lstat))
        .route("/fs/readlink", post(readlink))
        .route("/fs/symlink", post(symlink))
        .route("/fs/mkdir", post(mkdir))
        .route("/fs/remove", post(remove))
        .route("/fs/rename", post(rename))
        .route("/fs/setattr", post(setattr))
        .route("/fs/pwrite", post(pwrite))
        .route("/fs/glob", post(glob))
        .route("/fs/grep", post(grep))
        // FR-G5: history route.
        .route("/git/log", post(git_log_handler))
        .route("/git/branches", post(git_branches_handler))
        .route("/git/prs", post(git_prs_handler))
        // End FR-G5.
        .route("/git/status", post(git_status))
        .route("/git/diff", post(git_diff))
        .route("/git/stage", post(git_stage))
        .route("/git/unstage", post(git_unstage))
        .route("/git/apply", post(git_apply))
        .route("/git/commit", post(git_commit))
        .route("/git/checkout", post(git_checkout))
        .route("/git/branch", post(git_branch))
        .route("/git/fetch", post(git_fetch))
        .route("/git/pull", post(git_pull))
        .route("/git/push", post(git_push))
        .route("/git/conflicts", post(git_conflicts))
        // FR-G6 stash routes.
        .route("/git/stash/list", post(git_stash_list))
        .route("/git/stash/push", post(git_stash_push))
        .route("/git/stash/apply", post(git_stash_apply))
        .route("/git/stash/pop", post(git_stash_pop))
        .route("/git/stash/drop", post(git_stash_drop))
        // End FR-G6 stash routes.
        // FR-G7 routes begin.
        .route("/git/conflict", post(git_conflict))
        .route("/git/resolve", post(git_resolve))
        // FR-G7 routes end.
        .route("/exec", get(exec_ws))
        .route("/exec-server", get(crate::exec_server::ws))
        .route("/egress", get(crate::egress::ws))
        .route("/jobs", get(list_jobs).post(start_job))
        .route("/jobs/{id}", get(get_job).delete(remove_job))
        .route("/jobs/{id}/kill", post(kill_job))
        .route("/events", get(events_ws))
        .route("/ports", get(list_ports))
        .route("/terms", get(list_terms).post(create_term))
        .route("/terms/{id}", get(get_term).delete(remove_term))
        .route("/terms/{id}/snapshot", get(term_snapshot))
        .route("/terms/{id}/attach", get(term_attach_ws))
        .route("/terms/{id}/control", post(term_control))
        .route("/terms/{id}/kill", post(kill_term))
        .route_layer(middleware::from_fn_with_state(node.clone(), auth));
    Router::new()
        .route("/health", get(health))
        .merge(authed)
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
        .with_state(node)
}

/// Constant-time comparison, so the token cannot be recovered by timing.
fn token_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn auth(State(node): State<Node>, req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| token_eq(t.as_bytes(), node.0.token.as_bytes()));
    if ok {
        next.run(req).await
    } else {
        ApiError::new(StatusCode::UNAUTHORIZED, ErrorCode::Unauthorized, "missing or invalid bearer token")
            .into_response()
    }
}

// ---------------------------------------------------------------------------------------------
// Errors

struct ApiError(StatusCode, ErrorBody);

impl ApiError {
    fn new(status: StatusCode, code: ErrorCode, msg: impl Into<String>) -> Self {
        ApiError(status, ErrorBody { code, error: msg.into(), actual_sha256: None, errno: None })
    }

    fn with_errno(mut self, errno: Option<&str>) -> Self {
        self.1.errno = errno.map(str::to_string);
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
    }
}

impl From<PolicyError> for ApiError {
    fn from(e: PolicyError) -> Self {
        match &e {
            PolicyError::NotAbsolute(_) => ApiError::new(StatusCode::BAD_REQUEST, ErrorCode::BadRequest, e.to_string()),
            PolicyError::Outside(_) => ApiError::new(StatusCode::FORBIDDEN, ErrorCode::ForbiddenPath, e.to_string()),
            PolicyError::Io(_, io) if io.kind() == std::io::ErrorKind::NotFound => {
                ApiError::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, e.to_string())
            }
            PolicyError::Io(..) => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal, e.to_string()),
        }
    }
}

impl From<FsError> for ApiError {
    fn from(e: FsError) -> Self {
        let errno = e.errno();
        let err = match e {
            FsError::Policy(p) => p.into(),
            FsError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
                ApiError::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, io.to_string())
            }
            FsError::Io(io) => match errno {
                Some("EEXIST" | "ENOTEMPTY") => ApiError::new(StatusCode::CONFLICT, ErrorCode::BadRequest, io.to_string()),
                Some("ENOTDIR" | "EISDIR" | "EINVAL" | "ENAMETOOLONG" | "ELOOP") => {
                    ApiError::new(StatusCode::BAD_REQUEST, ErrorCode::BadRequest, io.to_string())
                }
                _ => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal, io.to_string()),
            },
            FsError::BadRequest(m) => ApiError::new(StatusCode::BAD_REQUEST, ErrorCode::BadRequest, m),
            e @ FsError::Precondition { .. } => {
                let FsError::Precondition { actual, .. } = &e else { unreachable!() };
                let actual = actual.clone();
                let mut err = ApiError::new(StatusCode::PRECONDITION_FAILED, ErrorCode::PreconditionFailed, e.to_string());
                err.1.actual_sha256 = actual;
                err
            }
        };
        err.with_errno(errno)
    }
}

impl From<GitError> for ApiError {
    fn from(e: GitError) -> Self {
        match e {
            GitError::Policy(p) => p.into(),
            GitError::NotARepo(m) => ApiError::new(StatusCode::NOT_FOUND, ErrorCode::NotARepo, m),
            GitError::Bad(m) => ApiError::new(StatusCode::BAD_REQUEST, ErrorCode::BadRequest, m),
            GitError::Conflict(m) => ApiError::new(StatusCode::CONFLICT, ErrorCode::Conflict, m),
            GitError::Internal(m) => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal, m),
            GitError::Timeout(m) => ApiError::new(StatusCode::GATEWAY_TIMEOUT, ErrorCode::Timeout, m),
            GitError::TooLarge(m) => ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, ErrorCode::TooLarge, m),
        }
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

/// Run a blocking file operation on the blocking pool.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, FsError> + Send + 'static,
) -> ApiResult<T> {
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => Ok(Json(r?)),
        Err(e) => Err(ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal, e.to_string())),
    }
}

// ---------------------------------------------------------------------------------------------
// Handlers

async fn health() -> Json<Health> {
    Json(Health { ok: true, version: env!("CARGO_PKG_VERSION").into() })
}

async fn env(State(n): State<Node>) -> Json<EnvInfo> {
    Json(crate::envinfo::describe(n.0.policy.roots().to_vec()).await)
}

async fn metrics(Query(q): Query<MetricsQuery>) -> ApiResult<MetricsSample> {
    let top = q.top.unwrap_or(crate::metrics::DEFAULT_TOP).min(crate::metrics::MAX_TOP);
    match tokio::task::spawn_blocking(move || crate::metrics::sample(top)).await {
        Ok(s) => Ok(Json(s)),
        Err(e) => Err(ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal, e.to_string())),
    }
}

async fn stat(State(n): State<Node>, Json(r): Json<PathRequest>) -> ApiResult<Stat> {
    blocking(move || fs::stat(&n.0.policy, &r.path)).await
}

async fn read(State(n): State<Node>, Json(r): Json<ReadRequest>) -> ApiResult<ReadResponse> {
    blocking(move || fs::read(&n.0.policy, &r)).await
}

async fn write(State(n): State<Node>, Json(r): Json<WriteRequest>) -> ApiResult<WriteResponse> {
    let _guard = n.0.write_lock.lock().await;
    let n2 = n.clone();
    blocking(move || fs::write(&n2.0.policy, &r)).await
}

async fn list(State(n): State<Node>, Json(r): Json<PathRequest>) -> ApiResult<ListResponse> {
    blocking(move || fs::list(&n.0.policy, &r.path)).await
}

async fn lstat(State(n): State<Node>, Json(r): Json<PathRequest>) -> ApiResult<Stat> {
    blocking(move || fs::lstat(&n.0.policy, &r.path)).await
}

async fn readlink(State(n): State<Node>, Json(r): Json<PathRequest>) -> ApiResult<ReadlinkResponse> {
    blocking(move || fs::readlink(&n.0.policy, &r.path)).await
}

// Namespace and attribute changes take the write lock too, so they never interleave with a
// write's precondition check and rename.

async fn symlink(State(n): State<Node>, Json(r): Json<SymlinkRequest>) -> ApiResult<Stat> {
    let _guard = n.0.write_lock.lock().await;
    let n2 = n.clone();
    blocking(move || fs::symlink(&n2.0.policy, &r)).await
}

async fn mkdir(State(n): State<Node>, Json(r): Json<MkdirRequest>) -> ApiResult<Stat> {
    let _guard = n.0.write_lock.lock().await;
    let n2 = n.clone();
    blocking(move || fs::mkdir(&n2.0.policy, &r)).await
}

async fn remove(State(n): State<Node>, Json(r): Json<RemoveRequest>) -> Result<StatusCode, ApiError> {
    let _guard = n.0.write_lock.lock().await;
    let n2 = n.clone();
    blocking(move || fs::remove(&n2.0.policy, &r)).await.map(|_| StatusCode::NO_CONTENT)
}

async fn rename(State(n): State<Node>, Json(r): Json<RenameRequest>) -> Result<StatusCode, ApiError> {
    let _guard = n.0.write_lock.lock().await;
    let n2 = n.clone();
    blocking(move || fs::rename(&n2.0.policy, &r)).await.map(|_| StatusCode::NO_CONTENT)
}

// Git handlers: each resolves the repository through the path policy, then runs the `git` CLI
// ([`crate::git`]). Operations that change the index or work tree take the write lock like the file writes do;
// fetch, push and the network half of pull do not, because they can take a long time.

// FR-G5: history handler.
async fn git_log_handler(State(n): State<Node>, Json(r): Json<git_log::GitLogRequest>) -> ApiResult<git_log::GitLogResponse> {
    Ok(Json(git_log::log(&n.0.policy, &r).await?))
}
// End FR-G5.

// FR-G9: branch list handler.
async fn git_branches_handler(State(n): State<Node>, Json(r): Json<GitRepoRequest>) -> ApiResult<git_branches::GitBranchesResponse> {
    Ok(Json(git_branches::list(&n.0.policy, &r).await?))
}
// End FR-G9.

// FR-G8: pull request list handler (runs `gh`, holds no lock).
async fn git_prs_handler(State(n): State<Node>, Json(r): Json<git_prs::GitPrsRequest>) -> ApiResult<git_prs::GitPrsResponse> {
    Ok(Json(git_prs::list(&n.0.policy, &r).await?))
}
// End FR-G8.

async fn git_status(State(n): State<Node>, Json(r): Json<GitRepoRequest>) -> ApiResult<GitStatus> {
    Ok(Json(git::status(&n.0.policy, &r).await?))
}

async fn git_diff(State(n): State<Node>, Json(r): Json<GitDiffRequest>) -> ApiResult<GitDiff> {
    Ok(Json(git::diff(&n.0.policy, &r).await?))
}

async fn git_stage(State(n): State<Node>, Json(r): Json<GitPathsRequest>) -> Result<StatusCode, ApiError> {
    let _guard = n.0.write_lock.lock().await;
    git::stage(&n.0.policy, &r).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn git_unstage(State(n): State<Node>, Json(r): Json<GitPathsRequest>) -> Result<StatusCode, ApiError> {
    let _guard = n.0.write_lock.lock().await;
    git::unstage(&n.0.policy, &r).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn git_apply(State(n): State<Node>, Json(r): Json<GitApplyRequest>) -> Result<StatusCode, ApiError> {
    let _guard = n.0.write_lock.lock().await;
    git::apply(&n.0.policy, &r).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn git_commit(State(n): State<Node>, Json(r): Json<GitCommitRequest>) -> ApiResult<GitCommitResponse> {
    let _guard = n.0.write_lock.lock().await;
    Ok(Json(git::commit(&n.0.policy, &r).await?))
}

async fn git_checkout(State(n): State<Node>, Json(r): Json<GitCheckoutRequest>) -> Result<StatusCode, ApiError> {
    let _guard = n.0.write_lock.lock().await;
    git::checkout(&n.0.policy, &r).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn git_branch(State(n): State<Node>, Json(r): Json<GitBranchRequest>) -> Result<StatusCode, ApiError> {
    let _guard = n.0.write_lock.lock().await;
    git::branch(&n.0.policy, &r).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn git_fetch(State(n): State<Node>, Json(r): Json<GitRemoteRequest>) -> ApiResult<GitOutput> {
    Ok(Json(git::fetch(&n.0.policy, &r).await?))
}

async fn git_pull(State(n): State<Node>, Json(r): Json<GitRemoteRequest>) -> ApiResult<GitOutput> {
    // The network fetch runs without the lock; only the local fast-forward takes it.
    let f = git::pull_fetch(&n.0.policy, &r).await?;
    let _guard = n.0.write_lock.lock().await;
    let m = git::pull_merge(&n.0.policy, &GitRepoRequest { repo: r.repo.clone() }).await?;
    let output = [f.output, m.output].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n");
    Ok(Json(GitOutput { output }))
}

async fn git_push(State(n): State<Node>, Json(r): Json<GitRemoteRequest>) -> ApiResult<GitOutput> {
    Ok(Json(git::push(&n.0.policy, &r).await?))
}

async fn git_conflicts(State(n): State<Node>, Json(r): Json<GitRepoRequest>) -> ApiResult<GitConflicts> {
    Ok(Json(git::conflicts(&n.0.policy, &r).await?))
}

// FR-G6 stash handlers.
async fn git_stash_list(State(n): State<Node>, Json(r): Json<GitRepoRequest>) -> ApiResult<Vec<GitStashEntry>> {
    Ok(Json(git_stash::list(&n.0.policy, &r).await?))
}

async fn git_stash_push(State(n): State<Node>, Json(r): Json<GitStashPushRequest>) -> Result<StatusCode, ApiError> {
    let _guard = n.0.write_lock.lock().await;
    git_stash::push(&n.0.policy, &r).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn git_stash_apply(State(n): State<Node>, Json(r): Json<GitStashIndexRequest>) -> Result<StatusCode, ApiError> {
    let _guard = n.0.write_lock.lock().await;
    git_stash::change(&n.0.policy, &r, GitStashOperation::Apply).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn git_stash_pop(State(n): State<Node>, Json(r): Json<GitStashIndexRequest>) -> Result<StatusCode, ApiError> {
    let _guard = n.0.write_lock.lock().await;
    git_stash::change(&n.0.policy, &r, GitStashOperation::Pop).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn git_stash_drop(State(n): State<Node>, Json(r): Json<GitStashIndexRequest>) -> Result<StatusCode, ApiError> {
    let _guard = n.0.write_lock.lock().await;
    git_stash::change(&n.0.policy, &r, GitStashOperation::Drop).await?;
    Ok(StatusCode::NO_CONTENT)
}
// End FR-G6 stash handlers.

// FR-G7 handlers.
async fn git_conflict(State(n): State<Node>, Json(r): Json<GitConflictRequest>) -> ApiResult<GitConflict> {
    Ok(Json(git_conflict::conflict(&n.0.policy, &r).await?))
}

async fn git_resolve(State(n): State<Node>, Json(r): Json<GitResolveRequest>) -> Result<StatusCode, ApiError> {
    let _guard = n.0.write_lock.lock().await;
    git_conflict::resolve(&n.0.policy, &r).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn setattr(State(n): State<Node>, Json(r): Json<SetAttrRequest>) -> ApiResult<Stat> {
    let _guard = n.0.write_lock.lock().await;
    let n2 = n.clone();
    blocking(move || fs::setattr(&n2.0.policy, &r)).await
}

async fn pwrite(State(n): State<Node>, Json(r): Json<PwriteRequest>) -> ApiResult<Stat> {
    let _guard = n.0.write_lock.lock().await;
    let n2 = n.clone();
    blocking(move || fs::pwrite(&n2.0.policy, &r)).await
}

async fn glob(State(n): State<Node>, Json(r): Json<GlobRequest>) -> ApiResult<GlobResponse> {
    blocking(move || fs::glob(&n.0.policy, &r)).await
}

async fn grep(State(n): State<Node>, Json(r): Json<GrepRequest>) -> ApiResult<GrepResponse> {
    blocking(move || fs::grep(&n.0.policy, &r)).await
}

async fn start_job(State(n): State<Node>, Json(r): Json<JobRequest>) -> Result<(StatusCode, Json<JobInfo>), ApiError> {
    let cwd = n.0.policy.resolve_existing(&r.command.cwd)?;
    let info = n
        .0
        .jobs
        .start(&r, cwd)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, ErrorCode::BadRequest, format!("{e:#}")))?;
    Ok((StatusCode::CREATED, Json(info)))
}

async fn list_jobs(State(n): State<Node>) -> Json<Vec<JobInfo>> {
    Json(n.0.jobs.list())
}

#[derive(Deserialize)]
struct TailQuery {
    tail: Option<usize>,
}

async fn get_job(State(n): State<Node>, Path(id): Path<String>, Query(q): Query<TailQuery>) -> ApiResult<JobDetail> {
    n.0.jobs
        .get(&id, q.tail)
        .map(Json)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, format!("no job {id}")))
}

async fn kill_job(
    State(n): State<Node>,
    Path(id): Path<String>,
    body: Option<Json<KillRequest>>,
) -> Result<StatusCode, ApiError> {
    let signal = body.and_then(|Json(b)| b.signal);
    if n.0.jobs.kill(&id, signal) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, format!("no job {id}")))
    }
}

async fn remove_job(State(n): State<Node>, Path(id): Path<String>) -> Result<StatusCode, ApiError> {
    match n.0.jobs.remove(&id) {
        Removal::Removed => Ok(StatusCode::NO_CONTENT),
        Removal::Unknown => Err(ApiError::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, format!("no job {id}"))),
        Removal::StillRunning => Err(ApiError::new(StatusCode::CONFLICT, ErrorCode::BadRequest, "job is still running")),
    }
}

// ---------------------------------------------------------------------------------------------
// WebSockets

async fn send_json<T: serde::Serialize>(ws: &mut WebSocket, v: &T) -> bool {
    let text = serde_json::to_string(v).expect("serialisable");
    ws.send(Message::Text(text.into())).await.is_ok()
}

async fn exec_ws(State(n): State<Node>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| exec_session(n, socket))
}

async fn exec_session(node: Node, mut ws: WebSocket) {
    let req: ExecRequest = loop {
        match ws.recv().await {
            Some(Ok(Message::Text(t))) => match serde_json::from_str(&t) {
                Ok(r) => break r,
                Err(e) => {
                    send_json(&mut ws, &ExecEvent::Error { message: format!("bad exec request: {e}") }).await;
                    return;
                }
            },
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            _ => return,
        }
    };
    let started = node
        .0
        .policy
        .resolve_existing(&req.command.cwd)
        .map_err(|e| anyhow::anyhow!("cwd: {e}"))
        .and_then(|cwd| match req.pty {
            Some(size) => exec::spawn_pty(&req.command, &cwd, size),
            None => exec::spawn_pipes(&req.command, &cwd, true),
        });
    let mut running = match started {
        Ok(r) => r,
        Err(e) => {
            send_json(&mut ws, &ExecEvent::Error { message: format!("{e:#}") }).await;
            return;
        }
    };
    let pid = running.pid;
    tracing::debug!(pid, pty = req.pty.is_some(), "exec started");
    if !send_json(&mut ws, &ExecEvent::Started { pid }).await {
        kill_group(pid, libc::SIGKILL);
        return;
    }

    let (mut sink, mut stream) = ws.split();
    let mut finished = false;
    loop {
        tokio::select! {
            ev = running.events.recv() => {
                let Some(ev) = ev else { finished = true; break };
                let last = matches!(ev, ExecEvent::Exit { .. } | ExecEvent::Error { .. });
                let text = serde_json::to_string(&ev).expect("serialisable");
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
                if last {
                    finished = true;
                    break;
                }
            }
            msg = stream.next() => {
                let input = match msg {
                    Some(Ok(Message::Text(t))) => serde_json::from_str::<ExecInput>(&t),
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => continue,
                };
                let res = match input {
                    Ok(ExecInput::Stdin { data }) => running.control.write_stdin(data).await,
                    Ok(ExecInput::CloseStdin) => { running.control.close_stdin(); Ok(()) }
                    Ok(ExecInput::Resize { size }) => running.control.resize(size),
                    Ok(ExecInput::Kill { signal }) => { kill_group(pid, signal.unwrap_or(libc::SIGTERM)); Ok(()) }
                    Err(e) => Err(anyhow::anyhow!("bad input: {e}")),
                };
                if let Err(e) = res {
                    tracing::debug!(pid, "exec input: {e:#}");
                }
            }
        }
    }
    if !finished {
        // The controlling connection went away: a foreground command does not outlive it.
        tracing::debug!(pid, "exec connection closed; killing process group");
        kill_group(pid, libc::SIGKILL);
    }
    let _ = sink.close().await;
    // Read whatever the client still sends until it answers the close. Dropping the socket with
    // unread input makes the kernel reset the connection, and the client then loses the exit
    // event it has not read yet (seen on Linux CI).
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while let Some(Ok(msg)) = stream.next().await {
            if matches!(msg, Message::Close(_)) {
                break;
            }
        }
    })
    .await;
}

#[derive(Deserialize)]
struct EventsQuery {
    #[serde(default)]
    after: u64,
}

async fn events_ws(State(n): State<Node>, Query(q): Query<EventsQuery>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| events_session(n, socket, q.after))
}

async fn events_session(node: Node, mut ws: WebSocket, after: u64) {
    let (backlog, mut rx) = node.0.jobs.subscribe(after);
    let mut last = after;
    for ev in backlog {
        last = ev.seq;
        if !send_json(&mut ws, &ev).await {
            return;
        }
    }
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(ev) if ev.seq <= last => continue,
                Ok(ev) => {
                    last = ev.seq;
                    if !send_json(&mut ws, &ev).await { return; }
                }
                // Too slow: close so the client reconnects with `after=<last>` and replays.
                Err(RecvError::Lagged(_)) | Err(RecvError::Closed) => break,
            },
            msg = ws.recv() => match msg {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                _ => continue,
            },
        }
    }
    let _ = ws.send(Message::Close(None)).await;
}

// ---------------------------------------------------------------------------------------------
// Persistent terminal sessions

fn no_term(id: &str) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, format!("no terminal session {id}"))
}

async fn create_term(
    State(n): State<Node>,
    Json(r): Json<TermCreateRequest>,
) -> Result<(StatusCode, Json<TermCreateResponse>), ApiError> {
    let cwd = n.0.policy.resolve_existing(&r.cwd)?;
    let terms = n.0.terms.clone();
    // Spawning forks and may start a keeper: keep it off the async workers.
    let res = tokio::task::spawn_blocking(move || terms.create(&r, cwd))
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal, e.to_string()))?
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, ErrorCode::BadRequest, format!("{e:#}")))?;
    let status = if res.created { StatusCode::CREATED } else { StatusCode::OK };
    Ok((status, Json(res)))
}

/// FR-P7: best effort, never an error. The owned roots are the running terms and jobs.
async fn list_ports(State(n): State<Node>) -> Json<PortsResponse> {
    let mut roots = Vec::new();
    for t in n.0.terms.list(&TermListQuery::default()) {
        if let (Some(pid), TermState::Running) = (t.pid, t.state) {
            roots.push(crate::ports::Root { pid, owner: PortOwner { kind: PortOwnerKind::Term, id: t.id } });
        }
    }
    for j in n.0.jobs.list() {
        if let (Some(pid), JobState::Running) = (j.pid, j.state) {
            roots.push(crate::ports::Root { pid, owner: PortOwner { kind: PortOwnerKind::Job, id: j.id } });
        }
    }
    let ports = crate::ports::listening(roots).await;
    Json(PortsResponse { ports, supported: crate::ports::ports_supported() })
}

async fn list_terms(State(n): State<Node>, Query(q): Query<TermListQuery>) -> Json<Vec<TermInfo>> {
    Json(n.0.terms.list(&q))
}

async fn get_term(State(n): State<Node>, Path(id): Path<String>) -> ApiResult<TermInfo> {
    n.0.terms.get(&id).map(|s| Json(s.info())).ok_or_else(|| no_term(&id))
}

async fn term_snapshot(State(n): State<Node>, Path(id): Path<String>) -> ApiResult<TermSnapshot> {
    let s = n.0.terms.get(&id).ok_or_else(|| no_term(&id))?;
    s.snapshot().map(Json).ok_or_else(|| {
        ApiError::new(StatusCode::CONFLICT, ErrorCode::BadRequest, "this session has no screen (lost, or finished before the node restarted)")
    })
}

async fn term_control(
    State(n): State<Node>,
    Path(id): Path<String>,
    Json(r): Json<TermControlRequest>,
) -> Result<StatusCode, ApiError> {
    let s = n.0.terms.get(&id).ok_or_else(|| no_term(&id))?;
    s.control(r.client, r.take)
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|e| ApiError::new(StatusCode::CONFLICT, ErrorCode::BadRequest, e))
}

async fn kill_term(
    State(n): State<Node>,
    Path(id): Path<String>,
    body: Option<Json<KillRequest>>,
) -> Result<StatusCode, ApiError> {
    let signal = body.and_then(|Json(b)| b.signal);
    if n.0.terms.kill(&id, signal) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(no_term(&id))
    }
}

async fn remove_term(State(n): State<Node>, Path(id): Path<String>) -> Result<StatusCode, ApiError> {
    match n.0.terms.remove(&id) {
        Removal::Removed => Ok(StatusCode::NO_CONTENT),
        Removal::Unknown => Err(no_term(&id)),
        Removal::StillRunning => Err(ApiError::new(StatusCode::CONFLICT, ErrorCode::BadRequest, "session is still running")),
    }
}

async fn term_attach_ws(State(n): State<Node>, Path(id): Path<String>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| term_attach_session(n, id, socket))
}

async fn term_attach_session(node: Node, id: String, mut ws: WebSocket) {
    let hello: TermHello = loop {
        match ws.recv().await {
            Some(Ok(Message::Text(t))) => match serde_json::from_str(&t) {
                Ok(h) => break h,
                Err(e) => {
                    send_json(&mut ws, &TermEvent::Error { message: format!("bad hello: {e}") }).await;
                    return;
                }
            },
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            _ => return,
        }
    };
    let Some(session) = node.0.terms.get(&id) else {
        send_json(&mut ws, &TermEvent::Error { message: format!("no terminal session {id}") }).await;
        return;
    };
    let (client, mut rx) = match session.attach(hello) {
        Ok(x) => x,
        Err(message) => {
            send_json(&mut ws, &TermEvent::Error { message }).await;
            return;
        }
    };
    tracing::debug!(term = %id, client, "terminal client attached");
    let (mut sink, mut stream) = ws.split();
    let mut saw_last = false;
    let mut dropped = false;
    loop {
        tokio::select! {
            ev = rx.recv() => {
                let Some(ev) = ev else { dropped = true; break };
                let last = matches!(ev, TermEvent::Exit { .. } | TermEvent::Error { .. });
                let text = serde_json::to_string(&ev).expect("serialisable");
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
                if last {
                    saw_last = true;
                    break;
                }
            }
            msg = stream.next() => {
                let input = match msg {
                    Some(Ok(Message::Text(t))) => serde_json::from_str::<TermInput>(&t),
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => continue,
                };
                match input {
                    Ok(TermInput::Input { data }) => session.input(client, data),
                    Ok(TermInput::Resize { size }) => session.resize(client, size),
                    Ok(TermInput::TakeControl) => {
                        if let Err(e) = session.control(client, true) {
                            tracing::debug!(term = %id, client, "take control: {e}");
                        }
                    }
                    Ok(TermInput::ReleaseControl) => {
                        let _ = session.control(client, false);
                    }
                    Ok(TermInput::Kill { signal }) => session.kill(signal),
                    Ok(TermInput::Detach) => break,
                    Err(e) => tracing::debug!(term = %id, client, "bad terminal input: {e}"),
                }
            }
        }
    }
    // Detach only: the session (and its process) is owned by the daemon, not by this socket.
    session.detach(client);
    if !saw_last && dropped && session.is_running() {
        let ev = TermEvent::Error { message: "dropped: this client fell behind; re-attach for a fresh snapshot".into() };
        let _ = sink.send(Message::Text(serde_json::to_string(&ev).expect("serialisable").into())).await;
    }
    let _ = sink.close().await;
    tracing::debug!(term = %id, client, "terminal client detached");
}
