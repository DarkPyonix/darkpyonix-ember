//! A guard for tests that start `ember-shim` processes (#923, SPEC FR-S8).
//!
//! It finds what a test started by markers the test controls: a shim's command line carries
//! `--dir <the test's shim dir>`, and the shim calls `setsid`, so its agent and every process the
//! agent starts (also one left behind after the agent ended) are in the shim's session. A
//! background thread records the shim pids from the metadata files in the directory while the
//! test runs, so the session of a shim that has already exited is still known at the end.
//!
//! On drop (also while the test panics) the guard kills every such process. When the test did
//! not panic, a process still running [`GRACE`] after the test's own cleanup fails the test.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long processes that were told to end (an acknowledged shim, a killed agent group) may
/// take to leave the process table. A shim that lingers waits for at least a minute in these
/// tests (the default is an hour), so a leak cannot hide inside this grace.
pub const GRACE: Duration = Duration::from_secs(20);

/// How often the metadata files are read to record shim pids.
const RECORD_EVERY: Duration = Duration::from_millis(10);

#[derive(Debug, Clone)]
pub struct Proc {
    pub pid: u32,
    pub sid: u32,
    pub stat: String,
    pub command: String,
}

pub struct ShimGuard {
    dirs: Vec<PathBuf>,
    sids: Arc<Mutex<BTreeSet<u32>>>,
    stop: Arc<AtomicBool>,
    recorder: Option<std::thread::JoinHandle<()>>,
}

impl ShimGuard {
    /// Guard the shims started with any of `dirs` as their shim directory.
    pub fn new(dirs: &[&Path]) -> ShimGuard {
        let dirs: Vec<PathBuf> = dirs.iter().map(|d| d.to_path_buf()).collect();
        let sids = Arc::new(Mutex::new(BTreeSet::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let recorder = {
            let (dirs, sids, stop) = (dirs.clone(), sids.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    record(&dirs, &sids);
                    std::thread::sleep(RECORD_EVERY);
                }
            })
        };
        ShimGuard { dirs, sids, stop, recorder: Some(recorder) }
    }

    /// Record the shims whose metadata is in the directories now (the background thread does
    /// this too; a test calls it right after a start to be sure).
    pub fn track(&self) {
        record(&self.dirs, &self.sids);
    }

    fn mentions(&self, command: &str) -> bool {
        self.dirs.iter().any(|d| command.contains(&*d.to_string_lossy()))
    }

    /// The live processes this test started: shims of its directories and every process in the
    /// session of one of its shims. Zombies are left out (they have ended and only wait for
    /// their parent to reap them).
    pub fn leftovers(&self) -> Vec<Proc> {
        let procs = processes();
        let me = std::process::id();
        let by_pid: BTreeMap<u32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
        let mut sids = self.sids.lock().unwrap().clone();
        for p in &procs {
            if self.mentions(&p.command) {
                sids.insert(p.pid);
            }
        }
        // A session counts when its leader is one of this test's shims, or when the leader has
        // ended (its pid is not reused while the session has members).
        let ours = |sid: u32| match by_pid.get(&sid) {
            Some(leader) if !leader.stat.starts_with('Z') => self.mentions(&leader.command),
            _ => true,
        };
        procs
            .iter()
            .filter(|p| p.pid != me && !p.stat.starts_with('Z'))
            .filter(|p| self.mentions(&p.command) || (sids.contains(&p.sid) && ours(p.sid)))
            .cloned()
            .collect()
    }
}

impl Drop for ShimGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.recorder.take() {
            let _ = t.join();
        }
        self.track();
        let panicking = std::thread::panicking();
        let deadline = Instant::now() + if panicking { Duration::ZERO } else { GRACE };
        let mut left = self.leftovers();
        while !left.is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
            left = self.leftovers();
        }
        for p in &left {
            // SAFETY: plain signal to a process this test started (matched by its own markers).
            unsafe {
                libc::kill(p.pid as libc::pid_t, libc::SIGKILL);
            }
        }
        if !left.is_empty() && !panicking {
            panic!(
                "#923: processes started from {:?} were still running {GRACE:?} after the test's cleanup (killed now): {left:#?}",
                self.dirs
            );
        }
    }
}

fn record(dirs: &[PathBuf], sids: &Mutex<BTreeSet<u32>>) {
    for d in dirs {
        for m in ember_server::session_shim::scan(d) {
            sids.lock().unwrap().insert(m.shim_pid);
        }
    }
}

/// The process table: pid, state and full command line from `ps` (macOS and Linux), and the
/// session id from `getsid`.
pub fn processes() -> Vec<Proc> {
    let out = std::process::Command::new("ps")
        .args(["-A", "-ww", "-o", "pid=,stat=,command="])
        .output()
        .expect("ps runs");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid, rest) = line.split_once(char::is_whitespace)?;
            let rest = rest.trim_start();
            let (stat, command) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            let pid: u32 = pid.parse().ok()?;
            // SAFETY: getsid only reads the process table.
            let sid = unsafe { libc::getsid(pid as libc::pid_t) };
            if sid < 0 {
                return None;
            }
            Some(Proc { pid, sid: sid as u32, stat: stat.to_string(), command: command.trim().to_string() })
        })
        .collect()
}

/// True once `pid` has left the process table or is a zombie.
pub fn ended(pid: u32) -> bool {
    !processes().iter().any(|p| p.pid == pid && !p.stat.starts_with('Z'))
}

/// Wait until `pid` has ended; panics after [`GRACE`].
pub async fn wait_ended(pid: u32, what: &str) {
    let deadline = Instant::now() + GRACE;
    while !ended(pid) {
        assert!(Instant::now() < deadline, "{what} (pid {pid}) still runs after {GRACE:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
