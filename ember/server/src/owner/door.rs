//! The claim door (SPEC `FR-S12`; INTENT D37 "Owner presence", #784, #815).
//!
//! On unix a socket `<data dir>/owner.sock`, mode 0600, removed at stop. On Windows a named pipe
//! `\\.\pipe\ember-owner-<hash of the data dir>` ([`super::pipe_name`]), whose only access
//! entry is the server's user and which refuses remote clients (`door/pipe.rs`). One connection is
//! one claim. The key lives while the connection stays open: a closed connection, a crashed app
//! or a stopped server voids it. There is no timer and no refresh. Both doors speak the same
//! protocol and run the same checks; only the peer lookup differs.
//!
//! # Protocol
//!
//! One JSON object per line, at most [`MAX_LINE`] bytes.
//!
//! Claimant to server:
//!
//! | Line | When |
//! | ---- | ---- |
//! | `{"op":"claim","name":"<what the claimant calls itself>"}` | first line, within [`HELLO_TIMEOUT`] |
//! | `{"op":"confirm","code":"<code from the server console>"}` | while the claim is pending |
//! | `{"op":"approve","claim":<id>}`, `{"op":"deny","claim":<id>}` | a holder, about another pending claim |
//!
//! Server to claimant:
//!
//! | Line | Meaning |
//! | ---- | ------- |
//! | `{"state":"refused","reason":"<why>"}` | no key; the connection closes |
//! | `{"state":"confirm","claim":<id>,"holders":<n>}` | pending: send the code, or wait for a holder |
//! | `{"state":"wrong_code","left":<n>}` | the code was wrong; `n` tries are left |
//! | `{"state":"granted","claim":<id>,"key":"eok_..."}` | the key; it lives while this connection does |
//! | `{"event":...}` | an [`OwnerEvent`], to holders only |
//!
//! # Checks on every claim
//!
//! 1. The peer runs as the server's user (`SO_PEERCRED` on Linux, `getpeereid` on macOS; on
//!    Windows the pipe's access list admits the server's user only, and the user of the
//!    process `GetNamedPipeClientProcessId` names is compared with the server's).
//! 2. The peer's process, and each of its ancestors, is outside every process tree Ember
//!    started: the server itself (agents without a shim, the local terminals) and each session
//!    shim. A process the table does not show is refused. On Windows the table is the process
//!    snapshot, and the walk ends at a parent that has exited ([`tree_verdict_exited_parents`]).
//! 3. Unless [`Confirm::Off`]: the claim is confirmed once, by the code this server prints on
//!    its console, or by a holder of a live key.
//!
//! Check 2 is not a boundary. A process can daemonise out of its tree. It stops the attack the
//! issue names (read a file, call curl) and check 3 stops the rest, as long as the claimant
//! cannot read the server's console. The boundary against code that tries is the agent sandbox,
//! which hides this socket with the rest of the data folder.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot, watch};

use super::{Claimant, Confirm, Owner, OwnerEvent, SOCKET_FILE};

#[cfg(windows)]
mod pipe;
#[cfg(windows)]
pub use pipe::{current_user_sid, pipe_dacl, pipe_exists, sid_is_current_user, snapshot_table, OwnerDoor};

/// The longest line either side sends.
pub const MAX_LINE: u64 = 4096;
/// How long a connection has to send its `claim` line.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a pending claim waits for its confirmation.
pub const CONFIRM_TIMEOUT: Duration = Duration::from_secs(120);
/// Wrong codes a pending claim may send before it is refused.
pub const CODE_TRIES: u32 = 3;
/// Claims that may wait for confirmation at once; one more is refused as `busy`.
pub const MAX_PENDING: usize = 4;
/// Claim connections open at once; one more is refused as `busy`.
pub const MAX_CONNECTIONS: usize = 16;

/// The socket's path inside `data_dir`.
pub fn socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SOCKET_FILE)
}

/// One row of the process table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub ppid: u32,
    pub exe: String,
}

/// The process table: pid to parent and executable.
pub type ProcTable = HashMap<u32, Proc>;

/// What the parent walk found for a claimant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeVerdict {
    /// Neither the process nor an ancestor is a denied root.
    Outside,
    /// The process, or an ancestor, is this denied root.
    Inside(u32),
    /// The process, or an ancestor, is missing from the table. Refused.
    Unknown,
}

/// Walks from `pid` up through its parents. `roots` are the processes whose trees get no key.
pub fn tree_verdict(pid: u32, table: &ProcTable, roots: &[u32]) -> TreeVerdict {
    let mut cur = pid;
    // A depth bound, so a table with a parent loop cannot hang the claim.
    for _ in 0..256 {
        if roots.contains(&cur) {
            return TreeVerdict::Inside(cur);
        }
        if cur <= 1 {
            return TreeVerdict::Outside;
        }
        match table.get(&cur) {
            Some(p) => cur = p.ppid,
            None => return TreeVerdict::Unknown,
        }
    }
    TreeVerdict::Unknown
}

/// [`tree_verdict`] for Windows, where a process whose parent exits keeps the parent's pid
/// (nothing reparents it, as unix does to pid 1). The claimant itself must be in the table;
/// an ancestor missing from it ended the walk: that tree is left. Explorer's parent, for one,
/// has always exited, so the unix rule would refuse every app started from the desktop.
///
/// This is the same exit as daemonising on unix (a process can leave its tree on purpose) and
/// no weaker: check 2 is not a boundary on either platform. A reused pid can send the walk
/// into an unrelated tree; it then refuses a claim that was outside or passes one whose real
/// parent had already exited, which the rule above passes anyway.
pub fn tree_verdict_exited_parents(pid: u32, table: &ProcTable, roots: &[u32]) -> TreeVerdict {
    if roots.contains(&pid) {
        return TreeVerdict::Inside(pid);
    }
    let Some(me) = table.get(&pid) else { return TreeVerdict::Unknown };
    let mut cur = me.ppid;
    // A depth bound, so a parent loop (reused pids) cannot hang the claim.
    for _ in 0..256 {
        if roots.contains(&cur) {
            return TreeVerdict::Inside(cur);
        }
        // Pid 0 is the idle process, the parent of the roots of the snapshot.
        if cur == 0 {
            return TreeVerdict::Outside;
        }
        // Back at the claimant: a loop through reused pids.
        if cur == pid {
            return TreeVerdict::Unknown;
        }
        match table.get(&cur) {
            Some(p) => cur = p.ppid,
            None => return TreeVerdict::Outside,
        }
    }
    TreeVerdict::Unknown
}

/// Parses `ps -A -o pid=,ppid=,comm=` output.
pub fn parse_ps(text: &str) -> ProcTable {
    let mut out = ProcTable::new();
    for line in text.lines() {
        let line = line.trim_start();
        let Some((pid, rest)) = line.split_once(char::is_whitespace) else { continue };
        let rest = rest.trim_start();
        let (ppid, exe) = match rest.split_once(char::is_whitespace) {
            Some((p, e)) => (p, e.trim()),
            None => (rest, ""),
        };
        if let (Ok(pid), Ok(ppid)) = (pid.parse::<u32>(), ppid.parse::<u32>()) {
            out.insert(pid, Proc { ppid, exe: exe.to_string() });
        }
    }
    out
}

/// The process table of this platform: [`ps_table`] on unix, the process snapshot on Windows
/// ([`snapshot_table`]).
pub fn process_table() -> ProcTable {
    #[cfg(unix)]
    {
        ps_table()
    }
    #[cfg(windows)]
    {
        snapshot_table()
    }
}

/// The process table from `ps`, as the shim's memory watch reads it (macOS and Linux). Empty
/// when `ps` cannot run, which refuses every claim.
#[cfg(unix)]
pub fn ps_table() -> ProcTable {
    match std::process::Command::new("ps").args(["-A", "-o", "pid=,ppid=,comm="]).output() {
        Ok(out) if out.status.success() => parse_ps(&String::from_utf8_lossy(&out.stdout)),
        Ok(out) => {
            tracing::warn!("owner door: ps exited with {}", out.status);
            ProcTable::new()
        }
        Err(e) => {
            tracing::warn!("owner door: cannot run ps: {e}");
            ProcTable::new()
        }
    }
}

/// The denied roots of a running server: the server process and every session shim in
/// `shim_dir`.
pub fn server_roots(shim_dir: Option<PathBuf>) -> Arc<dyn Fn() -> Vec<u32> + Send + Sync> {
    let me = std::process::id();
    Arc::new(move || {
        let mut roots = vec![me];
        if let Some(dir) = &shim_dir {
            for m in crate::session_shim::scan(dir) {
                roots.push(m.shim_pid);
                roots.push(m.agent_pid);
            }
        }
        roots.retain(|p| *p > 1);
        roots
    })
}

/// How the door is set up. [`DoorConfig::for_server`] is what `main.rs` uses; tests replace
/// the parts they drive.
#[derive(Clone)]
pub struct DoorConfig {
    pub confirm: Confirm,
    /// The processes whose trees get no key.
    pub roots: Arc<dyn Fn() -> Vec<u32> + Send + Sync>,
    /// The process table.
    pub procs: Arc<dyn Fn() -> ProcTable + Send + Sync>,
    /// Where a confirmation code is shown: `(code, claimant)`. The server console.
    pub console: Arc<dyn Fn(&str, &Claimant) + Send + Sync>,
    pub confirm_timeout: Duration,
}

impl DoorConfig {
    /// The server's door: confirmation from [`super::CONFIRM_ENV`], the roots of
    /// [`server_roots`], the table from [`process_table`], and the code printed on standard
    /// output.
    pub fn for_server(shim_dir: Option<PathBuf>) -> DoorConfig {
        DoorConfig {
            confirm: Confirm::from_value(std::env::var(super::CONFIRM_ENV).ok().as_deref()),
            roots: server_roots(shim_dir),
            procs: Arc::new(process_table),
            // Printed, not logged: a log line may be shipped or kept; the console is where
            // the owner is.
            console: Arc::new(|code: &str, who: &Claimant| {
                println!(
                    "ember owner code: {code}  (asked by pid {} {}, \"{}\"; enter it there within {} s, or ignore it if this was not you)",
                    who.pid,
                    who.exe,
                    who.name,
                    CONFIRM_TIMEOUT.as_secs()
                );
            }),
            confirm_timeout: CONFIRM_TIMEOUT,
        }
    }
}

struct Pending {
    decide: oneshot::Sender<bool>,
}

struct Inner {
    owner: Arc<Owner>,
    cfg: DoorConfig,
    pending: Mutex<HashMap<u64, Pending>>,
    connections: tokio::sync::Semaphore,
    stop: watch::Receiver<bool>,
}

/// The bound claim socket. Dropping it closes the socket, removes the file and voids every key.
#[cfg(unix)]
pub struct OwnerDoor {
    path: PathBuf,
    owner: Arc<Owner>,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

#[cfg(unix)]
impl OwnerDoor {
    /// Binds `<data_dir>/owner.sock` (mode 0600) and starts accepting claims. A socket file left
    /// by a server that was killed is replaced; one a live server answers on is an error.
    /// Must be called inside a tokio runtime.
    pub fn bind(data_dir: &Path, owner: Arc<Owner>, cfg: DoorConfig) -> io::Result<OwnerDoor> {
        let path = socket_path(data_dir);
        if std::fs::symlink_metadata(&path).is_ok() {
            if std::os::unix::net::UnixStream::connect(&path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("{} is held by a running server", path.display()),
                ));
            }
            std::fs::remove_file(&path)?;
        }
        let listener = UnixListener::bind(&path)?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        let (stop_tx, stop_rx) = watch::channel(false);
        let inner = Arc::new(Inner {
            owner: owner.clone(),
            cfg,
            pending: Mutex::new(HashMap::new()),
            connections: tokio::sync::Semaphore::new(MAX_CONNECTIONS),
            stop: stop_rx,
        });
        let task = tokio::spawn(accept_loop(listener, inner));
        Ok(OwnerDoor { path, owner, stop: stop_tx, task })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(unix)]
impl Drop for OwnerDoor {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.task.abort();
        self.owner.void_all();
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
async fn accept_loop(listener: UnixListener, inner: Arc<Inner>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let inner = inner.clone();
                tokio::spawn(async move { serve_conn(inner, stream).await });
            }
            Err(e) => {
                tracing::warn!("owner door: accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

/// One line, without its newline. `None` at end of stream. A line longer than [`MAX_LINE`], or
/// one that is not UTF-8, is an error.
async fn read_line<R: AsyncBufRead + Unpin>(r: &mut R) -> io::Result<Option<String>> {
    let mut buf = Vec::new();
    let n = (&mut *r).take(MAX_LINE).read_until(b'\n', &mut buf).await?;
    if n == 0 {
        return Ok(None);
    }
    if buf.last() != Some(&b'\n') {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long or cut short"));
    }
    String::from_utf8(buf)
        .map(|s| Some(s.trim().to_string()))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "line is not UTF-8"))
}

async fn send<W: AsyncWrite + Unpin>(w: &mut W, v: Value) -> io::Result<()> {
    let mut line = v.to_string();
    line.push('\n');
    w.write_all(line.as_bytes()).await?;
    w.flush().await
}

async fn refuse<W: AsyncWrite + Unpin>(w: &mut W, reason: &str) {
    let _ = send(w, json!({ "state": "refused", "reason": reason })).await;
    let _ = w.shutdown().await;
}

/// A fresh confirmation code: eight symbols shown as `XXXX-XXXX`, from an alphabet without
/// look-alikes (about 39 bits).
fn new_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTVWXYZ23456789";
    let bytes = super::random_32();
    let mut s = String::new();
    for (i, b) in bytes.iter().take(8).enumerate() {
        if i == 4 {
            s.push('-');
        }
        s.push(ALPHABET[*b as usize % ALPHABET.len()] as char);
    }
    s
}

/// A code as typed: upper case, without dashes and spaces.
fn normal_code(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect()
}

/// What the parent walk says about `pid`, as a refusal reason. Logged here, so both doors log
/// alike.
fn check_tree(pid: u32, verdict: TreeVerdict) -> Result<(), &'static str> {
    match verdict {
        TreeVerdict::Outside => Ok(()),
        TreeVerdict::Inside(root) => {
            tracing::warn!(pid, root, "owner claim refused: the claimant runs inside an Ember agent or terminal tree");
            Err("agent_tree")
        }
        TreeVerdict::Unknown => {
            tracing::warn!(pid, "owner claim refused: the claimant is not in the process table");
            Err("unknown_process")
        }
    }
}

/// The claimant behind `stream`, or why it gets no key.
#[cfg(unix)]
fn check_peer(inner: &Inner, stream: &UnixStream) -> Result<(u32, String), &'static str> {
    let cred = stream.peer_cred().map_err(|_| "no_peer_credentials")?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    if cred.uid() != unsafe { libc::geteuid() } {
        return Err("other_user");
    }
    let pid = match cred.pid() {
        Some(p) if p > 1 => p as u32,
        _ => return Err("no_peer_pid"),
    };
    let table = (inner.cfg.procs)();
    let roots = (inner.cfg.roots)();
    check_tree(pid, tree_verdict(pid, &table, &roots))?;
    let exe = table.get(&pid).map(|p| p.exe.clone()).unwrap_or_default();
    Ok((pid, exe))
}

#[cfg(unix)]
async fn serve_conn(inner: Arc<Inner>, stream: UnixStream) {
    let Ok(_permit) = inner.connections.try_acquire() else {
        let (_, mut w) = stream.into_split();
        refuse(&mut w, "busy").await;
        return;
    };
    let peer = check_peer(&inner, &stream);
    let (r, w) = stream.into_split();
    serve_split(&inner, r, w, peer).await;
}

/// One claim connection after the peer checks, on either door. The caller holds the
/// connection permit for as long as this runs.
async fn serve_split<R, W>(inner: &Arc<Inner>, r: R, mut w: W, peer: Result<(u32, String), &'static str>)
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    // Lines are read by their own task, so the loops below wait on a channel and lose no
    // partial line when another branch wins.
    let (tx, mut lines) = mpsc::channel::<Value>(8);
    let reader = tokio::spawn(async move {
        let mut r = BufReader::new(r);
        while let Ok(Some(line)) = read_line(&mut r).await {
            let Ok(v) = serde_json::from_str::<Value>(&line) else { break };
            if tx.send(v).await.is_err() {
                break;
            }
        }
    });
    claim(inner, &mut w, &mut lines, peer).await;
    reader.abort();
    let _ = w.shutdown().await;
}

async fn claim<W: AsyncWrite + Unpin>(
    inner: &Arc<Inner>,
    w: &mut W,
    lines: &mut mpsc::Receiver<Value>,
    peer: Result<(u32, String), &'static str>,
) {
    // The claim line is read before a refusal is sent, so a claimant's first write never meets
    // a closed socket and it always reads the reason.
    let hello = match tokio::time::timeout(HELLO_TIMEOUT, lines.recv()).await {
        Ok(Some(v)) if v["op"] == "claim" => v,
        _ => return refuse(w, "bad_request").await,
    };
    let (pid, exe) = match peer {
        Ok(p) => p,
        Err(reason) => return refuse(w, reason).await,
    };
    let name: String = hello["name"].as_str().unwrap_or("").chars().filter(|c| !c.is_control()).take(64).collect();
    let who = Claimant { pid, exe, name };
    let id = inner.owner.next_claim();

    if inner.cfg.confirm == Confirm::Strict && !confirmed(inner, w, lines, id, &who).await {
        return;
    }

    // Subscribed before the grant, so no event about a later claim is missed.
    let mut events = inner.owner.subscribe();
    let key = inner.owner.grant(id, who);
    let mut stop = inner.stop.clone();
    if send(w, json!({ "state": "granted", "claim": id, "key": key })).await.is_ok() {
        drop(key);
        loop {
            tokio::select! {
                line = lines.recv() => match line {
                    Some(v) => holder_op(inner, id, &v),
                    None => break,
                },
                ev = events.recv() => match ev {
                    Ok(ev) => {
                        let own = matches!(&ev,
                            OwnerEvent::OwnerClaimed { claim, .. } | OwnerEvent::OwnerGone { claim } if *claim == id);
                        if !own {
                            let Ok(v) = serde_json::to_value(&ev) else { continue };
                            if send(w, v).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
                _ = stop.changed() => break,
            }
        }
    }
    inner.owner.void(id);
}

/// A holder's line about another pending claim.
fn holder_op(inner: &Inner, holder: u64, v: &Value) {
    let approve = match v["op"].as_str() {
        Some("approve") => true,
        Some("deny") => false,
        _ => return,
    };
    let Some(claim) = v["claim"].as_u64() else { return };
    // Only a key that is still live decides.
    if !inner.owner.is_live(holder) {
        return;
    }
    let pending = inner.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&claim);
    if let Some(p) = pending {
        tracing::info!(claim, holder, approve, "owner claim decided by a holder");
        let _ = p.decide.send(approve);
    }
}

/// Waits for the confirmation of claim `id`. True when it is confirmed. On false the refusal
/// was already sent.
async fn confirmed<W: AsyncWrite + Unpin>(
    inner: &Arc<Inner>,
    w: &mut W,
    lines: &mut mpsc::Receiver<Value>,
    id: u64,
    who: &Claimant,
) -> bool {
    let (decide_tx, mut decide_rx) = oneshot::channel::<bool>();
    // The lock is released before any await.
    let admitted = {
        let mut pending = inner.pending.lock().unwrap_or_else(|e| e.into_inner());
        if pending.len() >= MAX_PENDING {
            false
        } else {
            pending.insert(id, Pending { decide: decide_tx });
            true
        }
    };
    if !admitted {
        refuse(w, "busy").await;
        return false;
    }
    let code = new_code();
    (inner.cfg.console)(&code, who);
    let holders = inner.owner.holders().len();
    inner.owner.announce(OwnerEvent::ClaimPending {
        claim: id,
        pid: who.pid,
        exe: who.exe.clone(),
        name: who.name.clone(),
    });
    let mut stop = inner.stop.clone();
    let deadline = tokio::time::sleep(inner.cfg.confirm_timeout);
    tokio::pin!(deadline);
    let mut left = CODE_TRIES;
    let mut holder_gone = false;
    let outcome: Result<(), &'static str> =
        if send(w, json!({ "state": "confirm", "claim": id, "holders": holders })).await.is_err() {
            Err("gone")
        } else {
            loop {
                tokio::select! {
                    line = lines.recv() => {
                        let Some(v) = line else { break Err("gone") };
                        if v["op"] != "confirm" {
                            continue;
                        }
                        let typed = normal_code(v["code"].as_str().unwrap_or(""));
                        if crate::auth::token_eq(&typed, &normal_code(&code)) {
                            break Ok(());
                        }
                        left -= 1;
                        tracing::warn!(claim = id, pid = who.pid, left, "owner claim: wrong code");
                        if left == 0 {
                            break Err("wrong_code");
                        }
                        if send(w, json!({ "state": "wrong_code", "left": left })).await.is_err() {
                            break Err("gone");
                        }
                    }
                    decision = &mut decide_rx, if !holder_gone => match decision {
                        Ok(true) => break Ok(()),
                        Ok(false) => break Err("denied"),
                        // The sender is dropped only with the pending entry; keep waiting for
                        // the code.
                        Err(_) => holder_gone = true,
                    },
                    _ = &mut deadline => break Err("timeout"),
                    _ = stop.changed() => break Err("server_stopping"),
                }
            }
        };
    inner.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    inner.owner.announce(OwnerEvent::ClaimClosed { claim: id });
    match outcome {
        Ok(()) => true,
        Err(reason) => {
            tracing::info!(claim = id, pid = who.pid, reason, "owner claim not confirmed");
            refuse(w, reason).await;
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(rows: &[(u32, u32)]) -> ProcTable {
        rows.iter().map(|(p, pp)| (*p, Proc { ppid: *pp, exe: String::new() })).collect()
    }

    #[test]
    fn fr_s12_tree_walk_finds_a_denied_ancestor() {
        // 1 <- 10 (server) <- 20 (shell) <- 30 (curl); 1 <- 40 (app)
        let t = table(&[(1, 0), (10, 1), (20, 10), (30, 20), (40, 1)]);
        assert_eq!(tree_verdict(30, &t, &[10]), TreeVerdict::Inside(10));
        assert_eq!(tree_verdict(10, &t, &[10]), TreeVerdict::Inside(10));
        assert_eq!(tree_verdict(40, &t, &[10]), TreeVerdict::Outside);
        assert_eq!(tree_verdict(30, &t, &[]), TreeVerdict::Outside);
    }

    #[test]
    fn fr_s12_tree_walk_refuses_what_the_table_does_not_show() {
        let t = table(&[(1, 0), (20, 10)]);
        assert_eq!(tree_verdict(99, &t, &[10]), TreeVerdict::Unknown);
        // The parent 10 is the root, so the walk ends there before it misses a row.
        assert_eq!(tree_verdict(20, &t, &[10]), TreeVerdict::Inside(10));
        assert_eq!(tree_verdict(20, &t, &[5]), TreeVerdict::Unknown);
        // A loop in the table ends as unknown.
        let l = table(&[(7, 8), (8, 7)]);
        assert_eq!(tree_verdict(7, &l, &[5]), TreeVerdict::Unknown);
    }

    #[test]
    fn fr_s12_windows_walk_ends_at_an_exited_parent() {
        // 4 (System) <- 600 (server) <- 700 (shell) <- 800 (curl); 900 (explorer, parent gone)
        // <- 910 (app); 0 is the idle process.
        let t = table(&[(4, 0), (600, 4), (700, 600), (800, 700), (900, 512), (910, 900)]);
        assert_eq!(tree_verdict_exited_parents(800, &t, &[600]), TreeVerdict::Inside(600));
        assert_eq!(tree_verdict_exited_parents(600, &t, &[600]), TreeVerdict::Inside(600));
        // The unix rule refuses the app started from the desktop; the Windows rule does not.
        assert_eq!(tree_verdict(910, &t, &[600]), TreeVerdict::Unknown);
        assert_eq!(tree_verdict_exited_parents(910, &t, &[600]), TreeVerdict::Outside);
        // A root that is not in the table still counts: its pid is enough.
        assert_eq!(tree_verdict_exited_parents(910, &t, &[512]), TreeVerdict::Inside(512));
        // The claimant itself must be in the table.
        assert_eq!(tree_verdict_exited_parents(999, &t, &[600]), TreeVerdict::Unknown);
        // A loop through reused pids ends as unknown.
        let l = table(&[(7, 8), (8, 9), (9, 8)]);
        assert_eq!(tree_verdict_exited_parents(7, &l, &[5]), TreeVerdict::Unknown);
        // So does a process that names itself as its parent.
        let s = table(&[(7, 7)]);
        assert_eq!(tree_verdict_exited_parents(7, &s, &[5]), TreeVerdict::Unknown);
    }

    #[test]
    fn fr_s12_ps_output_is_parsed_with_spaces_in_the_command() {
        let t = parse_ps("    1     0 /sbin/launchd\n  412     1 /Applications/Ember App.app/Contents/MacOS/ember\nbad line\n 77 412\n");
        assert_eq!(t[&1], Proc { ppid: 0, exe: "/sbin/launchd".into() });
        assert_eq!(t[&412].exe, "/Applications/Ember App.app/Contents/MacOS/ember");
        assert_eq!(t[&77], Proc { ppid: 412, exe: String::new() });
        assert_eq!(t.len(), 3);
    }

    #[test]
    fn fr_s12_codes_compare_without_case_dashes_or_spaces() {
        let c = new_code();
        assert_eq!(c.len(), 9);
        assert_eq!(c.as_bytes()[4], b'-');
        assert_eq!(normal_code(&c.to_lowercase().replace('-', " ")), normal_code(&c));
        assert_eq!(normal_code("ab-cd 23"), "ABCD23");
    }
}
