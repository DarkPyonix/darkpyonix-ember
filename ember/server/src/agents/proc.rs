//! The one place agent processes are spawned (SPEC FR-S8).
//!
//! With a shim runtime installed ([`install_runtime`], done by the server's `main`), an agent
//! starts under `ember-shim` and outlives the server; its stdout and stdin are bridged to plain
//! `AsyncRead`/`AsyncWrite` ends, so the adapters' protocol code does not change. Without a
//! runtime, or with `EMBER_SESSION_SHIM=off`, the agent is a direct child with `kill_on_drop` as
//! before.
//!
//! The shim is used only for a start that carries its session id in the environment
//! ([`SESSION_ENV`], added by `Sessions::ensure_live`), so adapters started bare (tests, tools)
//! stay direct.
//!
//! Output offsets: the stdout bridge stores the offset after the last complete line it handed to
//! the adapter, and acknowledges it to the shim. A new server attaching to the same session
//! resumes from that offset, so no complete line is lost or repeated.
//!
//! Commit-gated acknowledgement: a session whose event pump called [`require_commit`] does not
//! acknowledge a line when the adapter asks for the next one, but only when the pump has stored
//! every event sent so far ([`CommitGate::commit_if_idle`]). A crash between reading a line and
//! storing its event then leaves the line for the next server to replay.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::Context;
use tokio::io::{duplex, AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::process::{Child, Command};
use tokio::sync::watch;

use crate::session_shim::{self, Reattach, ShimClient, ShimEvent, ShimSpec};
use crate::store::Store;

/// Environment key carrying the Ember session id into [`spawn_agent`]; removed before the agent
/// starts.
pub const SESSION_ENV: &str = "EMBER_SESSION_ID";

pub type AgentStdin = Box<dyn AsyncWrite + Send + Unpin>;
pub type AgentStdout = Box<dyn AsyncRead + Send + Unpin>;

/// Where shims live and where their offsets are stored.
#[derive(Clone)]
pub struct ShimRuntime {
    /// The `ember-shim` binary.
    pub bin: PathBuf,
    /// The shim directory (sockets, metadata).
    pub dir: PathBuf,
    pub store: Arc<Store>,
}

static RUNTIME: RwLock<Option<Arc<ShimRuntime>>> = RwLock::new(None);

pub fn install_runtime(rt: ShimRuntime) {
    *RUNTIME.write().unwrap() = Some(Arc::new(rt));
}

pub fn clear_runtime() {
    *RUNTIME.write().unwrap() = None;
}

/// `EMBER_SESSION_SHIM=off` keeps direct spawning.
pub fn shim_enabled() -> bool {
    std::env::var("EMBER_SESSION_SHIM").map(|v| v != "off").unwrap_or(true)
}

/// Per-session commit gates, registered by [`require_commit`] and picked up when the stdout
/// bridge of that session is built.
static GATES: Mutex<Option<HashMap<String, Arc<CommitGate>>>> = Mutex::new(None);

/// Acknowledges a session's shim output only after the events it produced were stored.
pub struct CommitGate {
    /// End offset of the last line the adapter has fully handed on (it asked for the next one).
    ready: AtomicU64,
    /// The highest offset already acknowledged.
    sent: AtomicU64,
    /// The agent ended and its output was read to the end.
    exited: AtomicBool,
    exit_sent: AtomicBool,
    store_failed: AtomicBool,
    notify: tokio::sync::Notify,
    acks: Mutex<Option<tokio::sync::mpsc::UnboundedSender<Ack>>>,
}

impl CommitGate {
    fn new() -> CommitGate {
        CommitGate {
            ready: AtomicU64::new(0),
            sent: AtomicU64::new(0),
            exited: AtomicBool::new(false),
            exit_sent: AtomicBool::new(false),
            store_failed: AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
            acks: Mutex::new(None),
        }
    }

    /// Resolves when the reader made more output ready to acknowledge (or the agent ended).
    pub async fn notified(&self) {
        self.notify.notified().await
    }

    /// Acknowledge everything handled so far, if `idle()` says the pump has stored every event
    /// sent to it. The offset is read before `idle` runs, so an event sent after the offset was
    /// taken cannot be covered by it.
    pub fn commit_if_idle(&self, idle: impl FnOnce() -> bool) {
        let ready = self.ready.load(Ordering::SeqCst);
        let exited = self.exited.load(Ordering::SeqCst);
        if !idle() {
            return;
        }
        self.send(ready, exited);
    }

    /// Acknowledge everything handled so far unconditionally. For a pump that has stored
    /// everything it was given (and for tests).
    pub fn commit_ready(&self) {
        let (ready, exited) = (self.ready.load(Ordering::SeqCst), self.exited.load(Ordering::SeqCst));
        self.send(ready, exited);
    }

    /// Keep all remaining output available for replay after an event could not be stored.
    pub fn store_failed(&self) {
        self.store_failed.store(true, Ordering::SeqCst);
    }

    fn send(&self, ready: u64, exited: bool) {
        if self.store_failed.load(Ordering::SeqCst) {
            return;
        }
        let Some(tx) = self.acks.lock().unwrap().clone() else { return };
        if ready > self.sent.fetch_max(ready, Ordering::SeqCst) {
            let _ = tx.send(Ack::Offset(ready));
        }
        if exited && !self.exit_sent.swap(true, Ordering::SeqCst) {
            let _ = tx.send(Ack::Exited);
        }
    }
}

/// Make the next stdout bridge of `session` acknowledge only on commit. Called by the session's
/// event pump before it starts the agent; the returned gate is the pump's handle.
pub fn require_commit(session: &str) -> Arc<CommitGate> {
    let gate = Arc::new(CommitGate::new());
    GATES.lock().unwrap().get_or_insert_with(HashMap::new).insert(session.to_string(), gate.clone());
    gate
}

/// Forget the gate of `session` (the pump ended).
pub fn release_commit(session: &str) {
    if let Some(m) = GATES.lock().unwrap().as_mut() {
        m.remove(session);
    }
}

/// [`release_commit`], but only when the registered gate is still `gate` (a newer run of the
/// session may have replaced it).
pub fn release_commit_of(session: &str, gate: &Arc<CommitGate>) {
    if let Some(m) = GATES.lock().unwrap().as_mut() {
        if m.get(session).is_some_and(|g| Arc::ptr_eq(g, gate)) {
            m.remove(session);
        }
    }
}

fn gate_of(session: &str) -> Option<Arc<CommitGate>> {
    GATES.lock().unwrap().as_ref()?.get(session).cloned()
}

fn active_runtime() -> Option<Arc<ShimRuntime>> {
    if !shim_enabled() {
        return None;
    }
    RUNTIME.read().unwrap().clone()
}

/// Whether recorded metadata identifies a shim that has died while its server is still running.
pub fn shim_died(session: &str) -> bool {
    active_runtime().is_some_and(|rt| {
        session_shim::scan(&rt.dir)
            .into_iter()
            .any(|meta| meta.session == session && !session_shim::pid_alive(meta.shim_pid))
    })
}

/// The `ember-shim` binary next to the running server (or `EMBER_SHIM_BIN`).
pub fn find_shim_bin() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("EMBER_SHIM_BIN") {
        return Some(PathBuf::from(p));
    }
    let exe = std::env::current_exe().ok()?;
    let p = exe.parent()?.join("ember-shim");
    p.is_file().then_some(p)
}

pub struct AgentCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Applied on top of the server's environment. May contain [`SESSION_ENV`], and
    /// [`session_shim::ENV_CLEAR`] to start from an empty environment.
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
    /// A live shim of the same session may be attached instead of starting a new agent. Only for
    /// protocols with no handshake state (Claude Code's stream-json). For others a live shim of
    /// the session is stopped and the agent restarts through its native resume.
    pub reattachable: bool,
    /// Memory limit of the agent's process tree in MiB, enforced by the shim (FR-S8). `None`
    /// falls back to `EMBER_SESSION_MEM_LIMIT_MB`, then to no limit. Applies when a shim is
    /// started; a rejoined shim keeps the limit it was started with.
    pub mem_limit_mb: Option<u64>,
}

/// The limit in bytes for a shim: the request's MiB value, else `env_mb` (the text of
/// `EMBER_SESSION_MEM_LIMIT_MB`). Zero and unparsable values mean no limit.
pub fn mem_limit_bytes(request_mb: Option<u64>, env_mb: Option<&str>) -> Option<u64> {
    request_mb
        .or_else(|| env_mb.and_then(|v| v.trim().parse::<u64>().ok()))
        .filter(|mb| *mb > 0)
        .map(|mb| mb.saturating_mul(1024 * 1024))
}

pub struct AgentProc {
    pub stdin: AgentStdin,
    pub stdout: AgentStdout,
    /// Direct children only; a shim keeps the agent's stderr in its own log file.
    pub stderr: Option<Box<dyn AsyncRead + Send + Unpin>>,
    pub pid: Option<u32>,
    /// An already running agent was attached instead of started.
    pub reattached: bool,
    pub handle: ProcHandle,
}

pub enum ProcHandle {
    Direct(Child),
    Shim { client: Arc<ShimClient>, exited: watch::Receiver<bool> },
}

impl ProcHandle {
    /// Wait up to `grace` for the agent to exit (after its stdin was closed), then kill it.
    pub async fn shutdown(self, grace: Duration) -> anyhow::Result<()> {
        match self {
            ProcHandle::Direct(mut child) => match tokio::time::timeout(grace, child.wait()).await {
                Ok(status) => {
                    status?;
                }
                Err(_) => child.kill().await?,
            },
            ProcHandle::Shim { client, mut exited } => {
                let _ = client.close_stdin().await;
                if tokio::time::timeout(grace, exited.wait_for(|v| *v)).await.is_err() {
                    client.kill().await?;
                }
            }
        }
        Ok(())
    }
}

/// Attach to an existing session without starting a new process (FR-S8).
pub async fn reattach_agent(env: &[(String, String)]) -> anyhow::Result<Option<AgentProc>> {
    let Some(rt) = active_runtime() else { return Ok(None) };
    let Some(session) = env.iter().find(|(k, _)| k == SESSION_ENV).map(|(_, v)| v.clone()) else { return Ok(None) };
    if !safe_id(&session) { return Ok(None) }
    let stored = rt.store.shim_offset(&session)?;
    let Ok(client) = session_shim::attach(&rt.dir, &session, stored).await else {
        return Ok(None);
    };
    let pid = agent_pid(&rt.dir, &session).await;
    Ok(Some(bridge(rt, session, client, pid, true, stored)))
}

/// Start (or reattach) an agent process.
pub async fn spawn_agent(mut cmd: AgentCommand) -> anyhow::Result<AgentProc> {
    let session = cmd.env.iter().find(|(k, _)| k == SESSION_ENV).map(|(_, v)| v.clone());
    cmd.env.retain(|(k, _)| k != SESSION_ENV);
    match (active_runtime(), session) {
        (Some(rt), Some(session)) if safe_id(&session) => spawn_shimmed(rt, session, cmd).await,
        _ => spawn_direct(cmd),
    }
}

fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn spawn_direct(cmd: AgentCommand) -> anyhow::Result<AgentProc> {
    let mut command = Command::new(&cmd.program);
    session_shim::apply_env(&mut command, &cmd.env);
    let mut child = command
        .args(&cmd.args)
        .current_dir(&cmd.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to start {}: {e}", cmd.program.display()))?;
    let stdin = child.stdin.take().context("agent stdin")?;
    let stdout = child.stdout.take().context("agent stdout")?;
    let stderr = child.stderr.take().context("agent stderr")?;
    Ok(AgentProc {
        stdin: Box::new(stdin),
        stdout: Box::new(stdout),
        stderr: Some(Box::new(stderr)),
        pid: child.id(),
        reattached: false,
        handle: ProcHandle::Direct(child),
    })
}

async fn spawn_shimmed(rt: Arc<ShimRuntime>, session: String, cmd: AgentCommand) -> anyhow::Result<AgentProc> {
    let dir = rt.dir.clone();
    let stored = rt.store.shim_offset(&session)?;
    let (client, reattached) = match session_shim::attach_or_resume(&dir, &session, stored).await {
        Reattach::Live(c) if cmd.reattachable => (c, true),
        Reattach::Live(c) => {
            // A live agent whose protocol cannot be rejoined: stop it, then start again through
            // the agent's native resume.
            stop_shim(c, &dir, &session).await;
            rt.store.clear_shim_offset(&session)?;
            (start_shim(&rt, &session, &cmd).await?, false)
        }
        Reattach::Resume => {
            rt.store.clear_shim_offset(&session)?;
            (start_shim(&rt, &session, &cmd).await?, false)
        }
    };
    let pid = agent_pid(&dir, &session).await;
    Ok(bridge(rt, session, client, pid, reattached, if reattached { stored } else { 0 }))
}

async fn start_shim(rt: &ShimRuntime, session: &str, cmd: &AgentCommand) -> anyhow::Result<ShimClient> {
    let spec = ShimSpec {
        session: session.to_string(),
        program: cmd.program.clone(),
        args: cmd.args.clone(),
        cwd: cmd.cwd.clone(),
        env: cmd.env.clone(),
        mem_limit_bytes: mem_limit_bytes(
            cmd.mem_limit_mb,
            std::env::var("EMBER_SESSION_MEM_LIMIT_MB").ok().as_deref(),
        ),
        mem_poll_ms: std::env::var("EMBER_SESSION_MEM_POLL_MS").ok().and_then(|v| v.parse().ok()),
        linger_secs: None,
    };
    session_shim::spawn_shim(&rt.bin, &rt.dir, &spec).await
}

/// Kill a shim's agent, drain its end and acknowledge it so the shim exits and cleans up.
async fn stop_shim(mut c: ShimClient, dir: &Path, id: &str) {
    let _ = c.kill().await;
    while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_secs(5), c.events.recv()).await {
        if let ShimEvent::Exited(_) = ev {
            let _ = c.ack(u64::MAX).await;
            break;
        }
    }
    drop(c);
    wait_gone(dir, id).await;
}

/// Wait until the shim of `id` no longer accepts connections.
async fn wait_gone(dir: &Path, id: &str) {
    for _ in 0..100 {
        if session_shim::attach(dir, id, u64::MAX).await.is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn agent_pid(dir: &Path, id: &str) -> Option<u32> {
    for _ in 0..20 {
        if let Some(m) = session_shim::scan(dir).into_iter().find(|m| m.session == id) {
            return Some(m.agent_pid);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    None
}

enum Chunk {
    Data { start: u64, data: Vec<u8> },
    Eof,
}

enum Ack {
    /// Everything before this offset was handled by the adapter.
    Offset(u64),
    /// The agent ended and its output was read to the end.
    Exited,
}

/// The agent's stdout as seen by an adapter. It hands out at most one line per read, so when the
/// adapter asks for more, the previous line has been fully handled: that is the moment its end
/// offset is acknowledged and stored (a crash then repeats at most the line in flight, and loses
/// none).
struct ShimReader {
    rx: tokio::sync::mpsc::Receiver<Chunk>,
    cur: Vec<u8>,
    off: usize,
    /// Absolute offset of `cur[off]`.
    pos: u64,
    pending: Option<u64>,
    acks: tokio::sync::mpsc::UnboundedSender<Ack>,
    /// Set when the session acknowledges on commit instead of on the next read.
    gate: Option<Arc<CommitGate>>,
    eof: bool,
}

impl AsyncRead for ShimReader {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        use std::task::Poll;
        let this = &mut *self;
        if let Some(p) = this.pending.take() {
            match &this.gate {
                Some(g) => {
                    g.ready.store(p, Ordering::SeqCst);
                    g.notify.notify_one();
                }
                None => {
                    let _ = this.acks.send(Ack::Offset(p));
                }
            }
        }
        loop {
            if this.off < this.cur.len() {
                let rest = &this.cur[this.off..];
                let mut n = rest.len().min(buf.remaining());
                if let Some(i) = rest[..n].iter().position(|b| *b == b'\n') {
                    n = i + 1;
                    this.pending = Some(this.pos + n as u64);
                }
                buf.put_slice(&rest[..n]);
                this.off += n;
                this.pos += n as u64;
                return Poll::Ready(Ok(()));
            }
            if this.eof {
                return Poll::Ready(Ok(()));
            }
            match this.rx.poll_recv(cx) {
                Poll::Ready(Some(Chunk::Data { start, data })) => {
                    // The shim reports a gap (start ahead) when it dropped unacknowledged bytes.
                    let skip = this.pos.saturating_sub(start).min(data.len() as u64) as usize;
                    this.pos = this.pos.max(start);
                    this.cur = data;
                    this.off = skip;
                }
                Poll::Ready(Some(Chunk::Eof)) => {
                    this.eof = true;
                    match &this.gate {
                        Some(g) => {
                            g.exited.store(true, Ordering::SeqCst);
                            g.notify.notify_one();
                        }
                        None => {
                            let _ = this.acks.send(Ack::Exited);
                        }
                    }
                }
                Poll::Ready(None) => this.eof = true,
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Turn a shim connection into stdin/stdout ends and keep the stored offset current.
fn bridge(
    rt: Arc<ShimRuntime>,
    session: String,
    mut client: ShimClient,
    pid: Option<u32>,
    reattached: bool,
    from: u64,
) -> AgentProc {
    let mut events = std::mem::replace(&mut client.events, tokio::sync::mpsc::channel(1).1);
    let client = Arc::new(client);
    let (exited_tx, exited_rx) = watch::channel(false);

    // stdout: shim events -> ShimReader. The relay ends (detaching, agent left running) when the
    // reader is gone.
    let (chunk_tx, chunk_rx) = tokio::sync::mpsc::channel::<Chunk>(64);
    let (ack_tx, mut ack_rx) = tokio::sync::mpsc::unbounded_channel::<Ack>();
    tokio::spawn(async move {
        while let Some(ev) = events.recv().await {
            let (chunk, ended) = match ev {
                ShimEvent::Output { start, data } => (Chunk::Data { start, data }, None),
                ShimEvent::Exited(reason) => (Chunk::Eof, Some(reason)),
            };
            if let Some(reason) = &ended {
                tracing::info!(target: "ember::shim", "agent ended: {reason}");
                let _ = exited_tx.send(true);
            }
            if chunk_tx.send(chunk).await.is_err() || ended.is_some() {
                return;
            }
        }
    });
    {
        let (client, rt, session) = (client.clone(), rt.clone(), session.clone());
        tokio::spawn(async move {
            while let Some(a) = ack_rx.recv().await {
                match a {
                    Ack::Offset(o) => {
                        let _ = rt.store.set_shim_offset(&session, o);
                        let _ = client.ack(o).await;
                    }
                    Ack::Exited => {
                        let _ = rt.store.clear_shim_offset(&session);
                        let _ = client.ack(u64::MAX).await;
                        return;
                    }
                }
            }
        });
    }
    let gate = gate_of(&session);
    if let Some(g) = &gate {
        *g.acks.lock().unwrap() = Some(ack_tx.clone());
        g.ready.store(from, Ordering::SeqCst);
        g.sent.store(from, Ordering::SeqCst);
        g.exited.store(false, Ordering::SeqCst);
        g.exit_sent.store(false, Ordering::SeqCst);
    }
    let out_r = ShimReader {
        rx: chunk_rx,
        cur: Vec::new(),
        off: 0,
        pos: from,
        pending: None,
        acks: ack_tx,
        gate,
        eof: false,
    };

    // stdin: duplex -> shim. EOF here is not a close: a dropped server must leave the agent
    // running. Closing is explicit, in [`ProcHandle::shutdown`].
    let (in_w, mut in_r) = duplex(64 * 1024);
    {
        let client = client.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                match in_r.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        if client.send_stdin(&buf[..n]).await.is_err() {
                            return;
                        }
                    }
                }
            }
        });
    }

    AgentProc {
        stdin: Box::new(in_w),
        stdout: Box::new(out_r),
        stderr: None,
        pid,
        reattached,
        handle: ProcHandle::Shim { client, exited: exited_rx },
    }
}

/// Server startup (SPEC FR-S8): the sessions whose shim is alive, with their stored offsets
/// already applied by the next [`spawn_agent`] of the same session. Shims of dead sessions are
/// cleaned up; the returned ids are the live ones.
pub async fn live_shim_sessions(rt: &ShimRuntime) -> anyhow::Result<Vec<String>> {
    let offsets: HashMap<String, u64> = rt.store.shim_offsets()?;
    let mut live = Vec::new();
    for (id, r) in session_shim::reattach_all(&rt.dir, &offsets).await {
        match r {
            // Dropping the probe connection detaches; the real attach happens at spawn.
            Reattach::Live(_) => live.push(id),
            Reattach::Resume => {
                let _ = rt.store.clear_shim_offset(&id);
            }
        }
    }
    Ok(live)
}

/// Server startup (SPEC FR-S8) for an agent that cannot be rejoined as it runs (Codex): read what
/// the surviving agent wrote since the stored offset, up to `max` or until `done` accepts a
/// line, then stop the agent and forget its offset. The session resumes natively on its next
/// message. Returns the complete lines read; empty when the session has no live shim.
pub async fn drain_surviving_lines(
    rt: &ShimRuntime,
    session: &str,
    max: Duration,
    done: impl Fn(&str) -> bool,
) -> anyhow::Result<Vec<String>> {
    if !safe_id(session) {
        return Ok(Vec::new());
    }
    let stored = rt.store.shim_offset(session)?;
    let Reattach::Live(mut c) = session_shim::attach_or_resume(&rt.dir, session, stored).await else {
        rt.store.clear_shim_offset(session)?;
        return Ok(Vec::new());
    };
    let deadline = tokio::time::Instant::now() + max;
    let mut pos = stored;
    let mut buf: Vec<u8> = Vec::new();
    let mut lines = Vec::new();
    let mut exited = false;
    'read: loop {
        match tokio::time::timeout_at(deadline, c.events.recv()).await {
            Ok(Some(ShimEvent::Output { start, data })) => {
                if start > pos {
                    // The shim dropped unacknowledged bytes: what is buffered is a torn line.
                    buf.clear();
                    pos = start;
                }
                let skip = (pos - start).min(data.len() as u64) as usize;
                buf.extend_from_slice(&data[skip..]);
                pos = start + data.len() as u64;
                while let Some(i) = buf.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=i).collect();
                    let line = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
                    let stop = done(&line);
                    lines.push(line);
                    if stop {
                        break 'read;
                    }
                }
            }
            Ok(Some(ShimEvent::Exited(_))) => {
                exited = true;
                break;
            }
            Ok(None) | Err(_) => break,
        }
    }
    if exited {
        let _ = c.ack(u64::MAX).await;
        drop(c);
        wait_gone(&rt.dir, session).await;
    } else {
        stop_shim(c, &rt.dir, session).await;
    }
    rt.store.clear_shim_offset(session)?;
    Ok(lines)
}
