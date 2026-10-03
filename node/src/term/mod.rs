//! Persistent terminal sessions (SPEC §P, FR-P1–FR-P6; design: `docs/design/TERMINALS.md`).
//!
//! Unlike `/v1/exec`, whose PTY lives exactly as long as the WebSocket that started it, a
//! persistent session is owned by ember node: it is created by one request, any number of
//! clients attach and detach over WebSockets, and the process keeps running when every client
//! is gone. Clients are the VS Code window (through `ember-term`), the Ember editor, agents and
//! people.
//!
//! - [`screen`]: the terminal model (alacritty_terminal) — snapshot on attach, 10,000 lines of
//!   scrollback.
//! - [`session`]: one session — PTY threads, clients, input order, control, size policy.
//! - [`pty`]: PTY spawn and the keeper process that lets a session outlive a node restart.
//! - [`store`]: metadata on disk, so a restarted node re-adopts or reports what died.
//!
//! Lifetimes: a session survives client disconnects and ember server restarts (ember server is
//! only a client). It survives an **ember node** restart only if its keeper is alive (the
//! default when a state dir is configured; disabled with `EMBER_NODE_KEEP_PTY=0`), and then only
//! best effort: scrollback carries over a graceful restart (SIGTERM/SIGINT), not a crash, and
//! the exit status of a re-adopted process is unknown. Sessions whose PTY cannot be recovered
//! are listed as `lost`.

pub mod pty;
pub mod screen;
pub mod session;
pub mod store;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use crate::jobs::{now_ms, Jobs, Removal};
use crate::proto::*;

use self::pty::Pty;
use self::session::{Child, Session, TermRecord};
use self::store::Store;

/// Finished (exited or lost) sessions kept in the listing; older ones are forgotten.
pub const MAX_FINISHED: usize = 100;
/// Finished sessions whose screen is kept for re-attach (a task's output, FR-P5).
pub const MAX_FINISHED_SCREENS: usize = 20;
pub const DEFAULT_SIZE: PtySize = PtySize { rows: 24, cols: 80 };

/// What sessions share: persistence and the node event log.
pub struct Shared {
    store: Option<Store>,
    events: Arc<Jobs>,
}

impl Shared {
    fn save(&self, rec: &TermRecord) {
        if let Some(store) = &self.store {
            if let Err(e) = store.save(rec) {
                tracing::warn!(term = %rec.id, "could not persist terminal record: {e}");
            }
        }
    }

    fn emit(&self, kind: NodeEventKind) {
        self.events.emit(kind);
    }
}

pub struct Terms {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    shared: Arc<Shared>,
    /// The `ember-node` binary used to start PTY keepers; `None` disables them.
    keeper_exe: Option<PathBuf>,
}

/// The default program: the user's shell, as a login shell on macOS (as VS Code does there).
pub fn default_argv() -> Vec<String> {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
    if cfg!(target_os = "macos") {
        vec![shell, "-l".into()]
    } else {
        vec![shell]
    }
}

impl Terms {
    /// Load the records of a previous run from `state_dir` (if any): running sessions whose
    /// keeper still holds their PTY are re-adopted, the others are reported `lost`.
    pub fn new(state_dir: Option<&Path>, keeper_exe: Option<PathBuf>, events: Arc<Jobs>) -> Arc<Self> {
        let store = state_dir.and_then(|d| match Store::open(d) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::warn!(dir = %d.display(), "terminal state dir unusable, sessions will not be persisted: {e}");
                None
            }
        });
        let keeper_exe = if store.is_some() { keeper_exe } else { None };
        let shared = Arc::new(Shared { store, events });
        let terms = Arc::new(Terms { sessions: Mutex::new(HashMap::new()), shared, keeper_exe });
        terms.load();
        terms.prune();

        let weak: Weak<Terms> = Arc::downgrade(&terms);
        let _ = std::thread::Builder::new().name("terms-housekeeping".into()).spawn(move || loop {
            std::thread::sleep(Duration::from_secs(5));
            let Some(t) = weak.upgrade() else { return };
            let all: Vec<_> = t.sessions.lock().unwrap().values().cloned().collect();
            for s in all {
                s.tick();
            }
            t.prune();
        });
        terms
    }

    fn load(&self) {
        let Some(store) = &self.shared.store else { return };
        for mut rec in store.load_all() {
            let session = if rec.state == TermState::Running {
                self.readopt(&mut rec)
            } else {
                Session::inert(self.shared.clone(), rec.clone())
            };
            self.sessions.lock().unwrap().insert(rec.id.clone(), session);
        }
    }

    /// Recover a session from its keeper, or mark it lost.
    fn readopt(&self, rec: &mut TermRecord) -> Arc<Session> {
        let store = self.shared.store.as_ref().expect("load only runs with a store");
        let fd = match (rec.pid, &rec.keeper_socket) {
            (Some(pid), Some(sock)) if pty::alive(pid) => match pty::fetch_from_keeper(sock) {
                Ok(fd) => Some(fd),
                Err(e) => {
                    tracing::warn!(term = %rec.id, "keeper unreachable: {e}");
                    None
                }
            },
            _ => None,
        };
        let restore = store.take_snapshot(&rec.id);
        if let Some(fd) = fd {
            rec.adopted = true;
            match Session::start(self.shared.clone(), rec.clone(), Pty::from_fd(fd), Child::Adopted, restore) {
                Ok(s) => {
                    tracing::info!(term = %rec.id, pid = ?rec.pid, "re-adopted terminal session");
                    self.shared.emit(NodeEventKind::TermStarted { term: s.info() });
                    return s;
                }
                Err(e) => tracing::warn!(term = %rec.id, "could not re-adopt: {e:#}"),
            }
        }
        if let Some(sock) = &rec.keeper_socket {
            pty::stop_keeper(sock);
        }
        rec.state = TermState::Lost;
        rec.pid = None;
        rec.finished_ms = Some(now_ms());
        self.shared.save(rec);
        tracing::info!(term = %rec.id, "terminal session lost with the previous ember node");
        let s = Session::inert(self.shared.clone(), rec.clone());
        self.shared.emit(NodeEventKind::TermFinished { term: s.info() });
        s
    }

    /// Forget the oldest finished sessions beyond [`MAX_FINISHED`] and drop screens beyond
    /// [`MAX_FINISHED_SCREENS`].
    fn prune(&self) {
        let mut finished: Vec<(u64, Arc<Session>)> = self
            .sessions
            .lock()
            .unwrap()
            .values()
            .filter_map(|s| s.finished_ms().map(|t| (t, s.clone())))
            .collect();
        finished.sort_by_key(|f| std::cmp::Reverse(f.0));
        for (i, (_, s)) in finished.iter().enumerate() {
            if i >= MAX_FINISHED {
                self.sessions.lock().unwrap().remove(&s.id);
                if let Some(store) = &self.shared.store {
                    store.remove(&s.id);
                }
            } else if i >= MAX_FINISHED_SCREENS {
                s.drop_screen();
            }
        }
    }

    /// Start a session (or return the running one with the same `key`).
    pub fn create(&self, req: &TermCreateRequest, cwd: PathBuf) -> anyhow::Result<TermCreateResponse> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(key) = &req.key {
            if let Some(s) = sessions.values().find(|s| s.is_running() && s.key().as_deref() == Some(key.as_str())) {
                return Ok(TermCreateResponse { created: false, term: s.info() });
            }
        }
        let argv = match &req.program {
            Some(p) => crate::exec::argv(p)?,
            None => default_argv(),
        };
        let size = req.size.filter(|s| s.rows > 0 && s.cols > 0).unwrap_or(DEFAULT_SIZE);
        let id = uuid::Uuid::new_v4().simple().to_string();
        let (pty, pid) = pty::spawn(&argv, &cwd, &req.env, req.env_clear, size, &id)?;
        let keeper = match (&self.keeper_exe, &self.shared.store) {
            (Some(exe), Some(store)) => match pty::spawn_keeper(exe, &store.socket_path(&id), &pty, pid) {
                Ok(k) => Some(k.socket),
                Err(e) => {
                    tracing::warn!(term = %id, "no PTY keeper; the session will not survive a node restart: {e}");
                    None
                }
            },
            _ => None,
        };
        let now = now_ms();
        let rec = TermRecord {
            id: id.clone(),
            key: req.key.clone(),
            title: req.title.clone(),
            origin: req.origin,
            project: req.project.clone(),
            argv,
            cwd,
            tags: req.tags.clone(),
            pid: Some(pid),
            state: TermState::Running,
            exit_code: None,
            signal: None,
            size,
            created_ms: now,
            finished_ms: None,
            last_activity_ms: now,
            keeper_socket: keeper,
            adopted: false,
        };
        let session = Session::start(self.shared.clone(), rec, pty, Child::Ours, None)?;
        let info = session.info();
        sessions.insert(id.clone(), session);
        drop(sessions);
        tracing::info!(term = %id, pid, origin = info.origin.as_str(), "terminal session started");
        self.shared.emit(NodeEventKind::TermStarted { term: info.clone() });
        Ok(TermCreateResponse { created: true, term: info })
    }

    pub fn get(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions.lock().unwrap().get(id).cloned()
    }

    /// Sessions matching `q`, oldest first.
    pub fn list(&self, q: &TermListQuery) -> Vec<TermInfo> {
        let all: Vec<_> = self.sessions.lock().unwrap().values().cloned().collect();
        let mut v: Vec<TermInfo> = all
            .iter()
            .map(|s| s.info())
            .filter(|i| q.project.as_ref().is_none_or(|p| i.project.as_ref() == Some(p)))
            .filter(|i| q.origin.is_none_or(|o| i.origin == o))
            .filter(|i| q.running.is_none_or(|r| (i.state == TermState::Running) == r))
            .collect();
        v.sort_by_key(|i| i.created_ms);
        v
    }

    /// Signal a session. False if unknown.
    pub fn kill(&self, id: &str, signal: Option<i32>) -> bool {
        match self.get(id) {
            Some(s) => {
                s.kill(signal);
                true
            }
            None => false,
        }
    }

    /// Forget a finished session.
    pub fn remove(&self, id: &str) -> Removal {
        let mut sessions = self.sessions.lock().unwrap();
        match sessions.get(id) {
            Some(s) if s.is_running() => Removal::StillRunning,
            Some(_) => {
                sessions.remove(id);
                if let Some(store) = &self.shared.store {
                    store.remove(id);
                }
                Removal::Removed
            }
            None => Removal::Unknown,
        }
    }

    /// Graceful node shutdown: write each running session's snapshot, so a re-adopting node
    /// restores its scrollback and screen.
    pub fn shutdown(&self) {
        let Some(store) = &self.shared.store else { return };
        let all: Vec<_> = self.sessions.lock().unwrap().values().cloned().collect();
        for s in all {
            if let Some(snap) = s.shutdown_snapshot() {
                if let Err(e) = store.save_snapshot(&s.id, &snap) {
                    tracing::warn!(term = %s.id, "could not save snapshot: {e}");
                }
                self.shared.save(&s.record());
            }
        }
    }
}
