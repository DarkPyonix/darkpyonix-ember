//! Per-session shim (SPEC FR-S8, INTENT D17): agents outlive the ember server.
//!
//! Each session's agent runs under an `ember-shim` process (the containerd-shim pattern). The
//! shim owns the agent's stdio, buffers its stdout, and listens on a Unix socket in the shim
//! directory. The server connects, reads output, and acknowledges offsets. If the server dies or
//! is replaced, the shim keeps the agent running; the next server scans the directory and
//! reattaches, replaying everything after the last acknowledged offset.
//!
//! Files in the shim directory, per session `<id>`:
//! - `<id>.sock`: the shim's control socket.
//! - `<id>.json`: [`ShimMeta`], written by the shim.
//! - `<id>.log`: the agent's stderr.
//! - `<id>.spec.json`: [`ShimSpec`], mode 0600, deleted by the shim once read (it can hold
//!   secrets in `env`, which is why it is not passed on the command line).
//!
//! Wire protocol (all on the one socket):
//! - client to shim: `ATTACH <offset>\n` first, then `IN <len>\n<bytes>`, `ACK <offset>\n`,
//!   `KILL\n`.
//! - shim to client: `O <start> <len>\n<bytes>` (stdout bytes at absolute offset `start`) and
//!   `X <reason>\n` (the agent ended; reason is `exit:<code>`, `signal:<n>` or `memory-limit`).
//!
//! A new `ATTACH` supersedes the previous connection. The buffer is bounded
//! ([`BUFFER_CAP`]); bytes older than the cap and not acknowledged are dropped, and the frame
//! start offset tells the client about the gap.

use std::collections::HashMap;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{unix::OwnedWriteHalf, UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot, watch, Mutex, Notify};

/// Most unacknowledged stdout bytes a shim keeps.
pub const BUFFER_CAP: usize = 64 * 1024 * 1024;

/// What to run, written by the server and read by the shim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShimSpec {
    pub session: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    /// Resident memory (agent and its descendants) above which the shim kills this session.
    pub mem_limit_bytes: Option<u64>,
    /// How often the memory limit is checked (default 1000 ms).
    pub mem_poll_ms: Option<u64>,
    /// How long the shim waits for the final acknowledgement after the agent ends
    /// (default 3600 s).
    pub linger_secs: Option<u64>,
}

/// Post-exit linger: the spec's value, else `EMBER_SHIM_LINGER_SECS`, else 3600 s (FR-S8).
pub fn linger_secs(spec: Option<u64>, env: Option<&str>) -> u64 {
    spec.or_else(|| env.and_then(|v| v.trim().parse().ok())).unwrap_or(3600)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShimMeta {
    pub session: String,
    pub shim_pid: u32,
    pub agent_pid: u32,
}

fn sock_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.sock"))
}
fn meta_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.json"))
}
fn spec_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.spec.json"))
}
fn log_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.log"))
}

fn cleanup(dir: &Path, id: &str) {
    for p in [sock_path(dir, id), meta_path(dir, id), spec_path(dir, id)] {
        let _ = std::fs::remove_file(p);
    }
}

// ---------------------------------------------------------------------------------------------
// Shim side
// ---------------------------------------------------------------------------------------------

struct Inner {
    /// Absolute offset of `data[0]`.
    base: u64,
    data: Vec<u8>,
    exit: Option<String>,
    memory_killed: bool,
}

struct Shared {
    inner: StdMutex<Inner>,
    tx: watch::Sender<u64>,
    stdin: Mutex<Option<tokio::process::ChildStdin>>,
    kill: Notify,
    generation: AtomicU64,
}

impl Shared {
    fn bump(&self) {
        self.tx.send_modify(|v| *v += 1);
    }
}

/// Run a shim until its agent has ended and the last output was acknowledged. Called by the
/// `ember-shim` binary.
pub async fn run_shim(dir: &Path, id: &str) -> anyhow::Result<()> {
    let spec: ShimSpec = serde_json::from_slice(&std::fs::read(spec_path(dir, id))?)?;
    let _ = std::fs::remove_file(spec_path(dir, id));
    let stderr = std::fs::OpenOptions::new().create(true).append(true).open(log_path(dir, id))?;

    let mut cmd = tokio::process::Command::new(&spec.program);
    cmd.args(&spec.args)
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&spec.cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(stderr)
        .process_group(0);
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to start {}: {e}", spec.program.display()))?;
    let agent_pid = child.id().unwrap_or(0);
    let stdout = child.stdout.take().expect("stdout is piped");
    let stdin = child.stdin.take();

    let (tx, _rx) = watch::channel(0u64);
    let shared = Arc::new(Shared {
        inner: StdMutex::new(Inner { base: 0, data: Vec::new(), exit: None, memory_killed: false }),
        tx,
        stdin: Mutex::new(stdin),
        kill: Notify::new(),
        generation: AtomicU64::new(0),
    });

    let meta = ShimMeta { session: id.to_string(), shim_pid: std::process::id(), agent_pid };
    std::fs::write(meta_path(dir, id), serde_json::to_vec(&meta)?)?;
    let _ = std::fs::remove_file(sock_path(dir, id));
    let listener = UnixListener::bind(sock_path(dir, id))?;

    // stdout reader
    {
        let shared = shared.clone();
        tokio::spawn(async move {
            let mut out = stdout;
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                match out.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        {
                            let mut g = shared.inner.lock().unwrap();
                            g.data.extend_from_slice(&buf[..n]);
                            if g.data.len() > BUFFER_CAP {
                                let drop_n = g.data.len() - BUFFER_CAP;
                                g.data.drain(..drop_n);
                                g.base += drop_n as u64;
                            }
                        }
                        shared.bump();
                    }
                }
            }
        });
    }

    // waiter: records the exit, honours KILL
    let linger = Duration::from_secs(linger_secs(spec.linger_secs, std::env::var("EMBER_SHIM_LINGER_SECS").ok().as_deref()));
    {
        let shared = shared.clone();
        let dir = dir.to_path_buf();
        let id = id.to_string();
        tokio::spawn(async move {
            let status = tokio::select! {
                s = child.wait() => s,
                _ = shared.kill.notified() => {
                    kill_group(agent_pid);
                    child.wait().await
                }
            };
            // Let the stdout reader drain what the agent wrote last.
            tokio::time::sleep(Duration::from_millis(100)).await;
            let reason = {
                let mut g = shared.inner.lock().unwrap();
                let r = if g.memory_killed {
                    "memory-limit".to_string()
                } else {
                    match status {
                        Ok(s) => match (s.code(), s.signal()) {
                            (Some(c), _) => format!("exit:{c}"),
                            (None, Some(sig)) => format!("signal:{sig}"),
                            _ => "exit:unknown".to_string(),
                        },
                        Err(e) => format!("error:{e}"),
                    }
                };
                g.exit = Some(r.clone());
                r
            };
            let _ = reason;
            shared.bump();
            tokio::time::sleep(linger).await;
            cleanup(&dir, &id);
            std::process::exit(0);
        });
    }

    // memory watcher
    if let Some(limit) = spec.mem_limit_bytes {
        let shared = shared.clone();
        let poll = Duration::from_millis(spec.mem_poll_ms.unwrap_or(1000));
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(poll).await;
                if shared.inner.lock().unwrap().exit.is_some() {
                    return;
                }
                if let Some(rss) = tree_rss_bytes(agent_pid).await {
                    if rss > limit {
                        shared.inner.lock().unwrap().memory_killed = true;
                        shared.kill.notify_one();
                        return;
                    }
                }
            }
        });
    }

    loop {
        let (stream, _) = listener.accept().await?;
        let shared = shared.clone();
        let gen = shared.generation.fetch_add(1, Ordering::SeqCst) + 1;
        shared.bump(); // wakes the superseded connection
        let dir = dir.to_path_buf();
        let id = id.to_string();
        tokio::spawn(async move {
            let _ = serve(stream, shared, gen, dir, id).await;
        });
    }
}

fn kill_group(pid: u32) {
    if pid > 0 {
        // SAFETY: plain signal to the agent's own process group (it is its group leader).
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
}

/// Resident memory of `root` and its descendants, from `ps` (works on macOS and Linux).
async fn tree_rss_bytes(root: u32) -> Option<u64> {
    let out = tokio::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,rss="])
        .output()
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut rows: Vec<(u32, u32, u64)> = Vec::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        if let (Some(p), Some(pp), Some(r)) = (it.next(), it.next(), it.next()) {
            if let (Ok(p), Ok(pp), Ok(r)) = (p.parse(), pp.parse(), r.parse()) {
                rows.push((p, pp, r));
            }
        }
    }
    let mut in_tree: HashMap<u32, u64> = HashMap::new();
    let mut changed = true;
    while changed {
        changed = false;
        for &(p, pp, r) in &rows {
            if !in_tree.contains_key(&p) && (p == root || in_tree.contains_key(&pp)) {
                in_tree.insert(p, r);
                changed = true;
            }
        }
    }
    if in_tree.is_empty() {
        return None;
    }
    Some(in_tree.values().sum::<u64>() * 1024)
}

async fn serve(
    stream: UnixStream,
    shared: Arc<Shared>,
    gen: u64,
    dir: PathBuf,
    id: String,
) -> anyhow::Result<()> {
    let (r, mut w) = stream.into_split();
    let mut r = BufReader::new(r);
    let mut line = String::new();
    r.read_line(&mut line).await?;
    let mut sent: u64 = line
        .trim()
        .strip_prefix("ATTACH ")
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("bad attach line"))?;

    let (closed_tx, mut closed_rx) = oneshot::channel::<()>();
    {
        let shared = shared.clone();
        tokio::spawn(async move {
            let _ = read_commands(r, &shared, gen, &dir, &id).await;
            let _ = closed_tx.send(());
        });
    }

    let mut rx = shared.tx.subscribe();
    let mut exit_sent = false;
    loop {
        if shared.generation.load(Ordering::SeqCst) != gen {
            return Ok(());
        }
        rx.borrow_and_update();
        enum Next {
            Data(u64, Vec<u8>),
            Exit(String),
            Wait,
        }
        let next = {
            let g = shared.inner.lock().unwrap();
            let end = g.base + g.data.len() as u64;
            if sent < g.base {
                sent = g.base;
            }
            if sent < end {
                let s = (sent - g.base) as usize;
                let e = g.data.len().min(s + 65536);
                Next::Data(sent, g.data[s..e].to_vec())
            } else if let (Some(x), false) = (&g.exit, exit_sent) {
                Next::Exit(x.clone())
            } else {
                Next::Wait
            }
        };
        match next {
            Next::Data(start, d) => {
                w.write_all(format!("O {start} {}\n", d.len()).as_bytes()).await?;
                w.write_all(&d).await?;
                sent = start + d.len() as u64;
            }
            Next::Exit(x) => {
                w.write_all(format!("X {x}\n").as_bytes()).await?;
                exit_sent = true;
            }
            Next::Wait => {
                tokio::select! {
                    c = rx.changed() => { if c.is_err() { return Ok(()); } }
                    _ = &mut closed_rx => return Ok(()),
                }
            }
        }
    }
}

async fn read_commands(
    mut r: BufReader<tokio::net::unix::OwnedReadHalf>,
    shared: &Arc<Shared>,
    gen: u64,
    dir: &Path,
    id: &str,
) -> anyhow::Result<()> {
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        if shared.generation.load(Ordering::SeqCst) != gen {
            return Ok(());
        }
        let l = line.trim();
        if let Some(n) = l.strip_prefix("IN ") {
            let n: usize = n.parse()?;
            let mut buf = vec![0u8; n];
            r.read_exact(&mut buf).await?;
            if let Some(stdin) = shared.stdin.lock().await.as_mut() {
                stdin.write_all(&buf).await?;
                stdin.flush().await?;
            }
        } else if let Some(n) = l.strip_prefix("ACK ") {
            let n: u64 = n.parse()?;
            let done = {
                let mut g = shared.inner.lock().unwrap();
                let end = g.base + g.data.len() as u64;
                let upto = n.min(end);
                if upto > g.base {
                    let k = (upto - g.base) as usize;
                    g.data.drain(..k);
                    g.base = upto;
                }
                g.exit.is_some() && n >= end
            };
            if done {
                cleanup(dir, id);
                std::process::exit(0);
            }
        } else if l == "KILL" {
            shared.kill.notify_one();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Server side
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShimEvent {
    /// Agent stdout at absolute offset `start`. Pass `start + data.len()` to
    /// [`ShimClient::ack`] once it is persisted.
    Output { start: u64, data: Vec<u8> },
    /// The agent ended: `exit:<code>`, `signal:<n>` or `memory-limit`.
    Exited(String),
}

/// A server's connection to one shim. Dropping it detaches; the agent keeps running.
pub struct ShimClient {
    pub session: String,
    pub events: mpsc::Receiver<ShimEvent>,
    writer: Arc<Mutex<OwnedWriteHalf>>,
}

impl ShimClient {
    pub async fn send_stdin(&self, bytes: &[u8]) -> anyhow::Result<()> {
        let mut w = self.writer.lock().await;
        w.write_all(format!("IN {}\n", bytes.len()).as_bytes()).await?;
        w.write_all(bytes).await?;
        Ok(())
    }

    /// Tell the shim everything before `offset` is stored, so it may drop it.
    pub async fn ack(&self, offset: u64) -> anyhow::Result<()> {
        self.writer.lock().await.write_all(format!("ACK {offset}\n").as_bytes()).await?;
        Ok(())
    }

    /// Kill the agent's process group (the session ends).
    pub async fn kill(&self) -> anyhow::Result<()> {
        self.writer.lock().await.write_all(b"KILL\n").await?;
        Ok(())
    }
}

/// Connect to a running shim and replay output from `from_offset`.
pub async fn attach(dir: &Path, id: &str, from_offset: u64) -> anyhow::Result<ShimClient> {
    let stream = UnixStream::connect(sock_path(dir, id)).await?;
    let (r, mut w) = stream.into_split();
    w.write_all(format!("ATTACH {from_offset}\n").as_bytes()).await?;
    let (tx, rx) = mpsc::channel(256);
    tokio::spawn(async move {
        let mut r = BufReader::new(r);
        let mut line = String::new();
        loop {
            line.clear();
            match r.read_line(&mut line).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let l = line.trim();
            if let Some(rest) = l.strip_prefix("O ") {
                let mut it = rest.split(' ');
                let (Some(s), Some(n)) = (it.next(), it.next()) else { return };
                let (Ok(start), Ok(n)) = (s.parse::<u64>(), n.parse::<usize>()) else { return };
                let mut data = vec![0u8; n];
                if r.read_exact(&mut data).await.is_err() {
                    return;
                }
                if tx.send(ShimEvent::Output { start, data }).await.is_err() {
                    return;
                }
            } else if let Some(x) = l.strip_prefix("X ") {
                if tx.send(ShimEvent::Exited(x.to_string())).await.is_err() {
                    return;
                }
            }
        }
    });
    Ok(ShimClient { session: id.to_string(), events: rx, writer: Arc::new(Mutex::new(w)) })
}

/// Start a shim (detached from this process, in its own session) and attach to it.
pub async fn spawn_shim(
    shim_bin: &Path,
    dir: &Path,
    spec: &ShimSpec,
) -> anyhow::Result<ShimClient> {
    std::fs::create_dir_all(dir)?;
    let id = spec.session.clone();
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(spec_path(dir, &id))?;
    std::io::Write::write_all(&mut f, &serde_json::to_vec(spec)?)?;
    drop(f);
    let mut cmd = std::process::Command::new(shim_bin);
    cmd.arg("--dir")
        .arg(dir)
        .arg("--id")
        .arg(&id)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: setsid is async-signal-safe and runs between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to start {}: {e}", shim_bin.display()))?;
    // Reap the shim if it ends while this server is still alive.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    for _ in 0..100 {
        if let Ok(c) = attach(dir, &id, 0).await {
            return Ok(c);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("shim for session {id} did not come up (see {})", log_path(dir, &id).display())
}

/// Sessions that have shim metadata in `dir`.
pub fn scan(dir: &Path) -> Vec<ShimMeta> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.ends_with(".json") && !name.ends_with(".spec.json") {
            if let Ok(bytes) = std::fs::read(e.path()) {
                if let Ok(m) = serde_json::from_slice::<ShimMeta>(&bytes) {
                    out.push(m);
                }
            }
        }
    }
    out
}

pub enum Reattach {
    /// The shim is alive; output continues from the requested offset.
    Live(ShimClient),
    /// No live shim: resume the session through the agent's native resume (FR-S2).
    Resume,
}

/// Reattach to a session's shim, or say it must be resumed natively. Stale files of a dead shim
/// are removed.
pub async fn attach_or_resume(dir: &Path, id: &str, from_offset: u64) -> Reattach {
    match attach(dir, id, from_offset).await {
        Ok(c) => Reattach::Live(c),
        Err(_) => {
            cleanup(dir, id);
            Reattach::Resume
        }
    }
}

/// Server startup: reattach every shim found under `dir`. `offsets` maps session id to the last
/// acknowledged offset (missing means 0).
pub async fn reattach_all(dir: &Path, offsets: &HashMap<String, u64>) -> Vec<(String, Reattach)> {
    let mut out = Vec::new();
    for m in scan(dir) {
        let from = offsets.get(&m.session).copied().unwrap_or(0);
        let r = attach_or_resume(dir, &m.session, from).await;
        out.push((m.session, r));
    }
    out
}

/// Whether a process id is alive (used by tests and diagnostics).
pub fn pid_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks existence.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}
