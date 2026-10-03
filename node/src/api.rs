//! The daemon's HTTP/WebSocket API. All routes are under `/v1`; every route except
//! `/v1/health` requires `Authorization: Bearer <EMBER_NODE_TOKEN>`.
//!
//! | Method | Path | Body → Response |
//! | ------ | ---- | --------------- |
//! | GET  | `/v1/health` | → [`Health`] (no auth) |
//! | GET  | `/v1/env` | → [`EnvInfo`] |
//! | POST | `/v1/fs/stat` | [`PathRequest`] → [`Stat`] |
//! | POST | `/v1/fs/read` | [`ReadRequest`] → [`ReadResponse`] |
//! | POST | `/v1/fs/write` | [`WriteRequest`] → [`WriteResponse`] (412 on precondition) |
//! | POST | `/v1/fs/list` | [`PathRequest`] → [`ListResponse`] |
//! | POST | `/v1/fs/glob` | [`GlobRequest`] → [`GlobResponse`] |
//! | POST | `/v1/fs/grep` | [`GrepRequest`] → [`GrepResponse`] |
//! | GET (WS) | `/v1/exec` | send [`ExecRequest`], then [`ExecInput`]s; receive [`ExecEvent`]s |
//! | POST | `/v1/jobs` | [`JobRequest`] → [`JobInfo`] |
//! | GET  | `/v1/jobs` | → `[JobInfo]` |
//! | GET  | `/v1/jobs/{id}?tail=<bytes>` | → [`JobDetail`] |
//! | POST | `/v1/jobs/{id}/kill` | [`KillRequest`] → 204 |
//! | DELETE | `/v1/jobs/{id}` | → 204 (409 while running) |
//! | GET (WS) | `/v1/events?after=<seq>` | receive [`NodeEvent`]s |
//!
//! Closing the `/v1/exec` socket kills the command's process group; jobs are unaffected by any
//! connection. Errors are [`ErrorBody`] with a matching status code.
//!
//! The router is transport-agnostic: [`serve`] accepts any [`axum::serve::Listener`], so the same
//! API can run over TCP today and over another stream transport (e.g. QUIC streams) later.
//! A SOCKS5 egress for the remote browser (FR-R1) is planned as a separate listener next to this
//! one, not as a route here.

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
use crate::jobs::{Jobs, Removal};
use crate::policy::{PathPolicy, PolicyError};
use crate::proto::*;

struct Inner {
    token: String,
    policy: PathPolicy,
    jobs: Arc<Jobs>,
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
        Ok(Self(Arc::new(Inner {
            token: config.token,
            policy: PathPolicy::new(config.roots)?,
            jobs: Jobs::new(),
            write_lock: tokio::sync::Mutex::new(()),
        })))
    }

    pub fn jobs(&self) -> &Arc<Jobs> {
        &self.0.jobs
    }

    pub fn policy(&self) -> &PathPolicy {
        &self.0.policy
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
        .route("/v1/env", get(env))
        .route("/v1/fs/stat", post(stat))
        .route("/v1/fs/read", post(read))
        .route("/v1/fs/write", post(write))
        .route("/v1/fs/list", post(list))
        .route("/v1/fs/glob", post(glob))
        .route("/v1/fs/grep", post(grep))
        .route("/v1/exec", get(exec_ws))
        .route("/v1/jobs", get(list_jobs).post(start_job))
        .route("/v1/jobs/{id}", get(get_job).delete(remove_job))
        .route("/v1/jobs/{id}/kill", post(kill_job))
        .route("/v1/events", get(events_ws))
        .route_layer(middleware::from_fn_with_state(node.clone(), auth));
    Router::new()
        .route("/v1/health", get(health))
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
        ApiError(status, ErrorBody { code, error: msg.into(), actual_sha256: None })
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
        match e {
            FsError::Policy(p) => p.into(),
            FsError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
                ApiError::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, io.to_string())
            }
            FsError::Io(io) => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal, io.to_string()),
            FsError::BadRequest(m) => ApiError::new(StatusCode::BAD_REQUEST, ErrorCode::BadRequest, m),
            e @ FsError::Precondition { .. } => {
                let FsError::Precondition { actual, .. } = &e else { unreachable!() };
                let actual = actual.clone();
                let mut err = ApiError::new(StatusCode::PRECONDITION_FAILED, ErrorCode::PreconditionFailed, e.to_string());
                err.1.actual_sha256 = actual;
                err
            }
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
    Json(Health { ok: true, version: env!("CARGO_PKG_VERSION").into(), protocol: PROTOCOL_VERSION })
}

async fn env(State(n): State<Node>) -> Json<EnvInfo> {
    Json(crate::envinfo::describe(n.0.policy.roots().to_vec()).await)
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
