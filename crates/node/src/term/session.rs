//! One persistent terminal session: its PTY, terminal model, attached clients and the input /
//! control / size policy of FR-P4.
//!
//! Concurrency: a session is a mutex around [`Inner`] plus three plain threads (PTY reader, PTY
//! writer, exit waiter). Everything that changes the stream — output fed to the model, an attach
//! taking a snapshot, input, resize — happens under the one lock, so
//!
//! - a snapshot and the live stream that follows it never overlap or leave a gap, and
//! - input from all clients reaches the PTY in the order the lock was taken (arrival order);
//!   it is queued to the writer thread under the lock, so a program that stops reading its
//!   input never blocks the session.
//!
//! Each client has a bounded event queue. A client that falls behind by more than
//! [`CLIENT_QUEUE`] events is dropped (it gets an `error` and can re-attach for a fresh
//! snapshot); a slow client never stalls the program or the other clients.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use super::pty::{self, Pty};
use super::screen::Screen;
use super::Shared;
use crate::exec::{kill_group, DRAIN_GRACE};
use crate::jobs::now_ms;
use crate::proto::*;

/// Events queued per client before it is considered too slow and dropped.
pub const CLIENT_QUEUE: usize = 512;
/// Default `kill`: SIGHUP, then SIGKILL after this long.
pub const KILL_GRACE: Duration = Duration::from_secs(3);
/// A session without output or input for this long releases its model's row cache.
const IDLE_COMPACT_MS: u64 = 10_000;

static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);

/// The persisted part of a session (`<state>/terms/<id>.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermRecord {
    pub id: String,
    #[serde(default)]
    pub key: Option<String>,
    /// The requested title.
    #[serde(default)]
    pub title: Option<String>,
    pub origin: TermOrigin,
    #[serde(default)]
    pub project: Option<String>,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    #[serde(default)]
    pub tags: std::collections::BTreeMap<String, String>,
    pub pid: Option<u32>,
    pub state: TermState,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub size: PtySize,
    pub created_ms: u64,
    pub finished_ms: Option<u64>,
    pub last_activity_ms: u64,
    /// The keeper holding a duplicate of the PTY master, if one was started.
    #[serde(default)]
    pub keeper_socket: Option<PathBuf>,
    #[serde(default)]
    pub adopted: bool,
}

/// Who reaps the session's process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Child {
    /// Started by this node run: `waitpid` reports its exit status.
    Ours,
    /// Re-adopted after a node restart: not our child, so its exit is noticed by polling and its
    /// status is unknown.
    Adopted,
}

struct Client {
    id: u64,
    device: String,
    kind: Option<String>,
    pid: Option<u32>,
    read_only: bool,
    size: Option<PtySize>,
    attached_ms: u64,
    tx: mpsc::Sender<TermEvent>,
    /// False until its `attached` and `snapshot` events are queued, so nothing precedes them.
    ready: bool,
}

impl Client {
    fn public(&self) -> TermClient {
        TermClient {
            client: self.id,
            device: self.device.clone(),
            kind: self.kind.clone(),
            pid: self.pid,
            read_only: self.read_only,
            size: self.size,
            attached_ms: self.attached_ms,
        }
    }
}

struct Inner {
    rec: TermRecord,
    screen: Option<Screen>,
    pty: Option<Arc<Pty>>,
    writer: Option<std::sync::mpsc::Sender<Vec<u8>>>,
    clients: Vec<Client>,
    controller: Option<u64>,
    /// The most recently active client (input, or an `active` attach).
    active: Option<u64>,
    compacted: bool,
}

pub struct Session {
    pub id: String,
    inner: Mutex<Inner>,
    shared: Arc<Shared>,
}

fn valid(size: PtySize) -> bool {
    size.rows > 0 && size.cols > 0
}

impl Session {
    /// A running session on `pty`. `restore` is a snapshot from a graceful node shutdown, fed to
    /// the fresh model so the scrollback carries over.
    pub fn start(shared: Arc<Shared>, rec: TermRecord, pty: Pty, child: Child, restore: Option<Vec<u8>>) -> anyhow::Result<Arc<Self>> {
        let mut screen = Screen::new(rec.size.cols, rec.size.rows);
        if let Some(bytes) = restore {
            screen.feed(&bytes);
            screen.take_replies();
        }
        let reader = pty.reader()?;
        let writer = pty.writer()?;
        let (wtx, wrx) = std::sync::mpsc::channel::<Vec<u8>>();
        let pid = rec.pid;
        let session = Arc::new(Session {
            id: rec.id.clone(),
            inner: Mutex::new(Inner {
                rec,
                screen: Some(screen),
                pty: Some(Arc::new(pty)),
                writer: Some(wtx),
                clients: Vec::new(),
                controller: None,
                active: None,
                compacted: false,
            }),
            shared,
        });
        session.persist();

        let short = &session.id[..8.min(session.id.len())];
        std::thread::Builder::new().name(format!("term-{short}-w")).spawn(move || {
            use std::io::Write;
            let mut w = writer;
            for data in wrx {
                if w.write_all(&data).and_then(|_| w.flush()).is_err() {
                    break;
                }
            }
        })?;

        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        {
            let s = session.clone();
            std::thread::Builder::new().name(format!("term-{short}-r")).spawn(move || {
                use std::io::Read;
                let mut r = reader;
                let mut buf = vec![0u8; 32 * 1024];
                loop {
                    match r.read(&mut buf) {
                        // Linux reports EIO once the slave side is closed.
                        Ok(0) => break,
                        Ok(n) => s.on_output(&buf[..n]),
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                let _ = done_tx.send(());
            })?;
        }
        {
            let s = session.clone();
            std::thread::Builder::new().name(format!("term-{short}-x")).spawn(move || {
                let Some(pid) = pid else { return };
                let (code, signal) = match child {
                    Child::Ours => wait_status(pid),
                    Child::Adopted => {
                        while pty::alive(pid) {
                            std::thread::sleep(Duration::from_millis(500));
                        }
                        (None, None)
                    }
                };
                let _ = done_rx.recv_timeout(DRAIN_GRACE);
                s.on_exit(code, signal);
            })?;
        }
        Ok(session)
    }

    /// A session with no process: finished, or lost in a node restart.
    pub fn inert(shared: Arc<Shared>, rec: TermRecord) -> Arc<Self> {
        Arc::new(Session {
            id: rec.id.clone(),
            inner: Mutex::new(Inner {
                rec,
                screen: None,
                pty: None,
                writer: None,
                clients: Vec::new(),
                controller: None,
                active: None,
                compacted: true,
            }),
            shared,
        })
    }

    fn persist(&self) {
        let rec = self.inner.lock().unwrap().rec.clone();
        self.shared.save(&rec);
    }

    pub fn info(&self) -> TermInfo {
        self.inner.lock().unwrap().info()
    }

    pub fn record(&self) -> TermRecord {
        self.inner.lock().unwrap().rec.clone()
    }

    pub fn is_running(&self) -> bool {
        self.inner.lock().unwrap().rec.state == TermState::Running
    }

    pub fn key(&self) -> Option<String> {
        self.inner.lock().unwrap().rec.key.clone()
    }

    pub fn finished_ms(&self) -> Option<u64> {
        self.inner.lock().unwrap().rec.finished_ms
    }

    pub fn snapshot(&self) -> Option<TermSnapshot> {
        let mut g = self.inner.lock().unwrap();
        let size = g.rec.size;
        let screen = g.screen.as_mut()?;
        Some(TermSnapshot { size, data: screen.snapshot(), text: screen.text() })
    }

    /// Snapshot bytes for a graceful node shutdown (running sessions only).
    pub fn shutdown_snapshot(&self) -> Option<Vec<u8>> {
        let mut g = self.inner.lock().unwrap();
        if g.rec.state != TermState::Running {
            return None;
        }
        g.screen.as_mut().map(|s| s.snapshot())
    }

    /// Release memory of a finished session's model (keeps the record).
    pub fn drop_screen(&self) {
        let mut g = self.inner.lock().unwrap();
        if g.rec.state != TermState::Running {
            g.screen = None;
        }
    }

    /// Periodic housekeeping.
    pub fn tick(&self) {
        let mut g = self.inner.lock().unwrap();
        if g.rec.state == TermState::Running
            && !g.compacted
            && now_ms().saturating_sub(g.rec.last_activity_ms) > IDLE_COMPACT_MS
        {
            if let Some(s) = g.screen.as_mut() {
                s.compact();
            }
            g.compacted = true;
        }
    }

    fn on_output(&self, data: &[u8]) {
        let mut g = self.inner.lock().unwrap();
        g.rec.last_activity_ms = now_ms();
        g.compacted = false;
        let mut title_changed = None;
        let mut replies = Vec::new();
        if let Some(screen) = g.screen.as_mut() {
            let before = screen.title();
            screen.feed(data);
            let after = screen.title();
            if before != after {
                title_changed = Some(after);
            }
            replies = screen.take_replies();
        }
        // Attached interactive clients answer terminal queries themselves; with none, the model
        // answers so a program that asks (e.g. for the cursor position) does not hang.
        if !replies.is_empty() && g.clients.iter().all(|c| c.read_only) {
            if let Some(w) = &g.writer {
                let _ = w.send(replies);
            }
        }
        g.broadcast(TermEvent::Output { data: data.to_vec() });
        if let Some(title) = title_changed {
            g.broadcast(TermEvent::Title { title });
        }
    }

    fn on_exit(&self, code: Option<i32>, signal: Option<i32>) {
        let (rec, info) = {
            let mut g = self.inner.lock().unwrap();
            if g.rec.state != TermState::Running {
                return;
            }
            g.rec.state = TermState::Exited;
            g.rec.pid = None;
            g.rec.exit_code = code;
            g.rec.signal = signal;
            g.rec.finished_ms = Some(now_ms());
            g.send_all(TermEvent::Exit { code, signal });
            // Dropping the senders ends every client's stream after its `exit`.
            g.clients.clear();
            g.controller = None;
            g.active = None;
            g.writer = None;
            g.pty = None;
            (g.rec.clone(), g.info())
        };
        tracing::info!(term = %self.id, ?code, ?signal, "terminal session finished");
        if let Some(sock) = &rec.keeper_socket {
            pty::stop_keeper(sock);
        }
        self.shared.save(&rec);
        self.shared.emit(NodeEventKind::TermFinished { term: info });
    }

    /// Attach a client. Its queue starts with `attached`, then (if asked) `snapshot`, then the
    /// live stream; for a finished session, `attached`, `snapshot`, `exit`.
    pub fn attach(&self, hello: TermHello) -> Result<(u64, mpsc::Receiver<TermEvent>), String> {
        let mut g = self.inner.lock().unwrap();
        if g.rec.state == TermState::Lost {
            return Err(format!("terminal session {} was lost when ember node restarted", self.id));
        }
        let id = NEXT_CLIENT.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(CLIENT_QUEUE);
        let running = g.rec.state == TermState::Running;
        if running {
            g.clients.push(Client {
                id,
                device: hello.device.clone(),
                kind: hello.kind.clone(),
                pid: hello.pid,
                read_only: hello.read_only,
                size: hello.size.filter(|s| valid(*s)),
                attached_ms: now_ms(),
                tx: tx.clone(),
                ready: false,
            });
            if hello.active && !hello.read_only {
                g.active = Some(id);
            }
            // May resize the PTY and the model to this client, before the snapshot is taken.
            g.apply_size();
        }
        let info = g.info();
        let _ = tx.try_send(TermEvent::Attached { client: id, term: info });
        if hello.snapshot {
            let size = g.rec.size;
            if let Some(screen) = g.screen.as_mut() {
                let _ = tx.try_send(TermEvent::Snapshot { size, data: screen.snapshot() });
            }
        }
        if !running {
            let _ = tx.try_send(TermEvent::Exit { code: g.rec.exit_code, signal: g.rec.signal });
            return Ok((id, rx));
        }
        if let Some(controller) = g.controller_client() {
            let _ = tx.try_send(TermEvent::Control { controller: Some(controller) });
        }
        if let Some(c) = g.clients.iter_mut().find(|c| c.id == id) {
            c.ready = true;
        }
        let clients = g.public_clients();
        g.broadcast(TermEvent::Clients { clients });
        Ok((id, rx))
    }

    /// Keystrokes from `client`. Refusals are reported on that client's own stream.
    pub fn input(&self, client: u64, data: Vec<u8>) {
        let mut g = self.inner.lock().unwrap();
        let refusal = if g.rec.state != TermState::Running {
            Some(TermRefusal::NotRunning)
        } else {
            match g.clients.iter().find(|c| c.id == client) {
                None => return,
                Some(c) if c.read_only => Some(TermRefusal::ReadOnly),
                Some(_) if g.controller.is_some_and(|ctl| ctl != client) => Some(TermRefusal::Controlled),
                Some(_) => None,
            }
        };
        if let Some(reason) = refusal {
            let controller = g.controller_client();
            g.send_to(client, TermEvent::Refused { reason, controller });
            return;
        }
        // Every attached terminal answers queries (cursor position, device attributes, focus);
        // only the size owner's answers reach the program, so it gets exactly one.
        if g.owner_id() != Some(client) && is_terminal_report(&data) {
            return;
        }
        g.active = Some(client);
        g.rec.last_activity_ms = now_ms();
        g.compacted = false;
        g.apply_size();
        if let Some(w) = &g.writer {
            let _ = w.send(data);
        }
    }

    /// `client`'s viewport changed.
    pub fn resize(&self, client: u64, size: PtySize) {
        if !valid(size) {
            return;
        }
        let mut g = self.inner.lock().unwrap();
        if let Some(c) = g.clients.iter_mut().find(|c| c.id == client) {
            c.size = Some(size);
        }
        g.apply_size();
    }

    /// Take (`true`) or release control. Taking always succeeds for an interactive client,
    /// also from another controller; releasing is a no-op unless `client` has control.
    pub fn control(&self, client: u64, take: bool) -> Result<(), String> {
        let mut g = self.inner.lock().unwrap();
        if g.rec.state != TermState::Running {
            return Err("the session is not running".into());
        }
        let Some(c) = g.clients.iter().find(|c| c.id == client) else {
            return Err(format!("client {client} is not attached"));
        };
        if take {
            if c.read_only {
                return Err("a read-only client cannot take control".into());
            }
            g.controller = Some(client);
            g.active = Some(client);
        } else if g.controller == Some(client) {
            g.controller = None;
        } else {
            return Ok(());
        }
        let controller = g.controller_client();
        g.broadcast(TermEvent::Control { controller });
        g.apply_size();
        Ok(())
    }

    /// Detach a client. Never ends the session.
    pub fn detach(&self, client: u64) {
        let mut g = self.inner.lock().unwrap();
        let before = g.clients.len();
        g.clients.retain(|c| c.id != client);
        if g.clients.len() != before {
            g.membership_changed();
        }
    }

    /// Signal the session: the terminal's foreground job and the shell's process group. With no
    /// signal: SIGHUP (what closing a terminal sends), then SIGKILL after [`KILL_GRACE`].
    pub fn kill(self: &Arc<Self>, signal: Option<i32>) {
        let (pid, pty) = {
            let g = self.inner.lock().unwrap();
            if g.rec.state != TermState::Running {
                return;
            }
            (g.rec.pid, g.pty.clone())
        };
        let Some(pid) = pid else { return };
        signal_session(pid, pty.as_deref(), signal.unwrap_or(libc::SIGHUP));
        if signal.is_none() {
            let s = self.clone();
            std::thread::spawn(move || {
                std::thread::sleep(KILL_GRACE);
                if s.is_running() {
                    signal_session(pid, pty.as_deref(), libc::SIGKILL);
                }
            });
        }
    }
}

fn signal_session(pid: u32, pty: Option<&Pty>, signal: i32) {
    if let Some(fg) = pty.and_then(|p| p.foreground_pgrp()) {
        if fg as u32 != pid {
            kill_group(fg as u32, signal);
        }
    }
    kill_group(pid, signal);
}

/// `waitpid` on our own child; (exit code, signal).
fn wait_status(pid: u32) -> (Option<i32>, Option<i32>) {
    let mut status: libc::c_int = 0;
    loop {
        // SAFETY: waiting on our own child pid.
        let r = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, 0) };
        if r == -1 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        if r == -1 {
            return (None, None);
        }
        break;
    }
    if libc::WIFEXITED(status) {
        (Some(libc::WEXITSTATUS(status)), None)
    } else if libc::WIFSIGNALED(status) {
        (None, Some(libc::WTERMSIG(status)))
    } else {
        (None, None)
    }
}

impl Inner {
    fn info(&self) -> TermInfo {
        let r = &self.rec;
        let title = r
            .title
            .clone()
            .or_else(|| self.screen.as_ref().and_then(|s| s.title()))
            .unwrap_or_else(|| {
                r.argv
                    .first()
                    .map(|p| p.rsplit('/').next().unwrap_or(p).to_string())
                    .unwrap_or_else(|| "terminal".into())
            });
        TermInfo {
            id: r.id.clone(),
            key: r.key.clone(),
            title,
            origin: r.origin,
            project: r.project.clone(),
            argv: r.argv.clone(),
            cwd: r.cwd.clone(),
            pid: r.pid,
            state: r.state,
            exit_code: r.exit_code,
            signal: r.signal,
            size: r.size,
            created_ms: r.created_ms,
            finished_ms: r.finished_ms,
            last_activity_ms: r.last_activity_ms,
            tags: r.tags.clone(),
            clients: self.public_clients(),
            controller: self.controller_client(),
            survives_node_restart: r.state == TermState::Running && r.keeper_socket.is_some(),
            adopted: r.adopted,
            has_screen: self.screen.is_some(),
        }
    }

    fn public_clients(&self) -> Vec<TermClient> {
        self.clients.iter().map(Client::public).collect()
    }

    fn controller_client(&self) -> Option<TermClient> {
        let id = self.controller?;
        self.clients.iter().find(|c| c.id == id).map(Client::public)
    }

    /// Whose size the PTY follows: the controller, else the most recently active client, else
    /// the most recently attached client that reported a size.
    fn owner_id(&self) -> Option<u64> {
        let present = |id: u64| self.clients.iter().any(|c| c.id == id);
        self.controller
            .filter(|id| present(*id))
            .or_else(|| self.active.filter(|id| present(*id)))
            .or_else(|| self.clients.iter().rev().find(|c| c.size.is_some()).map(|c| c.id))
    }

    fn apply_size(&mut self) {
        if self.rec.state != TermState::Running {
            return;
        }
        let Some(owner) = self.owner_id() else { return };
        let Some(size) = self.clients.iter().find(|c| c.id == owner).and_then(|c| c.size) else { return };
        if size == self.rec.size {
            return;
        }
        if let Some(p) = &self.pty {
            if let Err(e) = p.resize(size) {
                tracing::debug!(term = %self.rec.id, "resize: {e}");
            }
        }
        if let Some(s) = self.screen.as_mut() {
            s.resize(size.cols, size.rows);
        }
        self.rec.size = size;
        self.broadcast(TermEvent::Resized { size });
    }

    /// Queue `ev` for every ready client; returns whether a client was dropped for being slow.
    fn send_all(&mut self, ev: TermEvent) -> bool {
        let before = self.clients.len();
        self.clients.retain(|c| {
            if !c.ready {
                return true;
            }
            match c.tx.try_send(ev.clone()) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    tracing::info!(client = c.id, device = %c.device, "terminal client too slow; dropped");
                    false
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            }
        });
        self.clients.len() != before
    }

    fn broadcast(&mut self, ev: TermEvent) {
        if self.send_all(ev) {
            self.membership_changed();
        }
    }

    fn send_to(&mut self, client: u64, ev: TermEvent) {
        if let Some(c) = self.clients.iter().find(|c| c.id == client) {
            let _ = c.tx.try_send(ev);
        }
    }

    /// After clients left: fix control and activity, tell the rest, re-apply the size policy.
    fn membership_changed(&mut self) {
        let present = |s: &Self, id: u64| s.clients.iter().any(|c| c.id == id);
        if let Some(ctl) = self.controller {
            if !present(self, ctl) {
                self.controller = None;
                self.send_all(TermEvent::Control { controller: None });
            }
        }
        if let Some(a) = self.active {
            if !present(self, a) {
                self.active = None;
            }
        }
        let clients = self.public_clients();
        self.send_all(TermEvent::Clients { clients });
        self.apply_size();
    }
}

/// Is `data` made only of terminal *answers* (cursor position, device attributes, mode reports,
/// focus events, OSC/DCS replies) rather than typed keys? Shift+F3 (`ESC [1;2R`) has the same
/// shape as a cursor position report; it is only affected for clients that are not the size
/// owner.
pub fn is_terminal_report(data: &[u8]) -> bool {
    let mut i = 0;
    let mut any = false;
    while i < data.len() {
        if data[i] != 0x1b || i + 1 >= data.len() {
            return false;
        }
        match data[i + 1] {
            b'[' => {
                let start = i + 2;
                let mut j = start;
                while j < data.len() && (0x30..=0x3f).contains(&data[j]) {
                    j += 1;
                }
                let params = &data[start..j];
                let mut k = j;
                while k < data.len() && (0x20..=0x2f).contains(&data[k]) {
                    k += 1;
                }
                if k >= data.len() {
                    return false;
                }
                let prefix = params.first().copied();
                let ok = match (data[k], &data[j..k]) {
                    (b'R', []) => !params.is_empty(),
                    (b'n', []) => !params.is_empty(),
                    (b'c', []) => matches!(prefix, Some(b'?' | b'>' | b'=')),
                    (b't', []) => !params.is_empty(),
                    (b'y', [b'$']) => true,
                    (b'u', []) => prefix == Some(b'?'),
                    (b'I' | b'O', []) => params.is_empty(),
                    _ => false,
                };
                if !ok {
                    return false;
                }
                i = k + 1;
            }
            b']' | b'P' | b'_' => {
                let mut j = i + 2;
                let mut end = None;
                while j < data.len() {
                    if data[j] == 0x07 {
                        end = Some(j + 1);
                        break;
                    }
                    if data[j] == 0x1b && j + 1 < data.len() && data[j + 1] == b'\\' {
                        end = Some(j + 2);
                        break;
                    }
                    j += 1;
                }
                match end {
                    Some(e) => i = e,
                    None => return false,
                }
            }
            _ => return false,
        }
        any = true;
    }
    any
}

#[cfg(test)]
mod tests {
    use super::is_terminal_report as r;

    #[test]
    fn terminal_reports_are_told_apart_from_keys() {
        assert!(r(b"\x1b[12;40R"));
        assert!(r(b"\x1b[?1;2c\x1b[>0;276;0c"));
        assert!(r(b"\x1b[0n"));
        assert!(r(b"\x1b[?2004;1$y"));
        assert!(r(b"\x1b]11;rgb:0000/0000/0000\x1b\\"));
        assert!(r(b"\x1b[I"));
        assert!(r(b"\x1bP1$r0m\x1b\\"));
        assert!(!r(b"ls\r"));
        assert!(!r(b"\x1b[A")); // arrow up
        assert!(!r(b"\x1b[<0;10;5M")); // mouse click
        assert!(!r(b"\x1b")); // a lone Escape key
        assert!(!r(b"\x1b[13;5u")); // kitty-encoded Ctrl+Enter
        assert!(!r(b"\x1b[12;40Rx"));
        assert!(!r(b""));
    }
}
