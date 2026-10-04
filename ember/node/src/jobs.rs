//! Background jobs (FR-X4) and the node → server event log (which also carries persistent
//! terminal start/finish events, see [`crate::term`]).
//!
//! A job is owned by the daemon, not by the request or connection that started it, so it keeps
//! running when that connection drops or the session moves to another computer. Its combined
//! output is kept in a bounded ring buffer. Start and completion are appended to an in-memory
//! event log with sequence numbers; a server that reconnects asks for `after=<last seq>` and
//! receives what it missed, as long as it is still within the last [`EVENT_HISTORY`] events.
//!
//! Jobs do **not** survive a daemon restart (open problem; see the crate docs).

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::broadcast;

use crate::exec::{self, kill_group};
use crate::proto::*;

/// Bytes of output kept per job.
pub const TAIL_BYTES: usize = 256 * 1024;
/// Events kept for replay.
pub const EVENT_HISTORY: usize = 1024;

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

struct Job {
    info: JobInfo,
    tail: VecDeque<u8>,
}

impl Job {
    fn tail_string(&self) -> String {
        let (a, b) = self.tail.as_slices();
        let mut v = Vec::with_capacity(a.len() + b.len());
        v.extend_from_slice(a);
        v.extend_from_slice(b);
        String::from_utf8_lossy(&v).into_owned()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removal {
    Removed,
    StillRunning,
    Unknown,
}

struct EventLog {
    next_seq: u64,
    history: VecDeque<NodeEvent>,
}

pub struct Jobs {
    boot_id: String,
    jobs: Mutex<HashMap<String, Job>>,
    log: Mutex<EventLog>,
    tx: broadcast::Sender<NodeEvent>,
}

impl Jobs {
    pub fn new() -> Arc<Self> {
        let (tx, _) = broadcast::channel(EVENT_HISTORY);
        Arc::new(Self {
            boot_id: uuid::Uuid::new_v4().to_string(),
            jobs: Mutex::new(HashMap::new()),
            log: Mutex::new(EventLog { next_seq: 1, history: VecDeque::new() }),
            tx,
        })
    }

    pub fn boot_id(&self) -> &str {
        &self.boot_id
    }

    pub(crate) fn emit(&self, kind: NodeEventKind) {
        let mut log = self.log.lock().unwrap();
        let ev = NodeEvent { seq: log.next_seq, boot_id: self.boot_id.clone(), kind };
        log.next_seq += 1;
        log.history.push_back(ev.clone());
        if log.history.len() > EVENT_HISTORY {
            log.history.pop_front();
        }
        // Sent under the log lock so that `subscribe` sees each event exactly once.
        let _ = self.tx.send(ev);
    }

    /// Events with `seq > after` still in history, plus a receiver for everything after them.
    pub fn subscribe(&self, after: u64) -> (Vec<NodeEvent>, broadcast::Receiver<NodeEvent>) {
        let log = self.log.lock().unwrap();
        let rx = self.tx.subscribe();
        let backlog = log.history.iter().filter(|e| e.seq > after).cloned().collect();
        (backlog, rx)
    }

    pub fn start(self: &Arc<Self>, req: &JobRequest, cwd: PathBuf) -> anyhow::Result<JobInfo> {
        let running = exec::spawn_pipes(&req.command, &cwd, false)?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let info = JobInfo {
            id: id.clone(),
            label: req.label.clone(),
            program: req.command.program.clone(),
            cwd,
            pid: Some(running.pid),
            state: JobState::Running,
            exit_code: None,
            signal: None,
            started_ms: now_ms(),
            finished_ms: None,
            output_bytes: 0,
        };
        self.jobs.lock().unwrap().insert(id.clone(), Job { info: info.clone(), tail: VecDeque::new() });
        self.emit(NodeEventKind::JobStarted { job: info.clone() });
        tracing::info!(job = %id, pid = running.pid, "job started");

        let this = self.clone();
        let mut events = running.events;
        tokio::spawn(async move {
            while let Some(ev) = events.recv().await {
                let mut jobs = this.jobs.lock().unwrap();
                let Some(job) = jobs.get_mut(&id) else { continue };
                match ev {
                    ExecEvent::Stdout { data } | ExecEvent::Stderr { data } => {
                        job.info.output_bytes += data.len() as u64;
                        job.tail.extend(data);
                        let excess = job.tail.len().saturating_sub(TAIL_BYTES);
                        job.tail.drain(..excess);
                    }
                    ExecEvent::Exit { code, signal } => {
                        job.info.state = if signal.is_some() { JobState::Signaled } else { JobState::Exited };
                        job.info.exit_code = code;
                        job.info.signal = signal;
                        job.info.finished_ms = Some(now_ms());
                        job.info.pid = None;
                        let (info, tail) = (job.info.clone(), job.tail_string());
                        drop(jobs);
                        tracing::info!(job = %id, ?code, ?signal, "job finished");
                        this.emit(NodeEventKind::JobFinished { job: info, tail });
                        break;
                    }
                    ExecEvent::Error { message } => {
                        job.tail.extend(format!("\n[ember-node] {message}\n").into_bytes());
                    }
                    ExecEvent::Started { .. } => {}
                }
            }
        });
        Ok(info)
    }

    pub fn list(&self) -> Vec<JobInfo> {
        let mut v: Vec<_> = self.jobs.lock().unwrap().values().map(|j| j.info.clone()).collect();
        v.sort_by_key(|j| j.started_ms);
        v
    }

    /// The job, with at most `tail_bytes` of its output tail.
    pub fn get(&self, id: &str, tail_bytes: Option<usize>) -> Option<JobDetail> {
        let jobs = self.jobs.lock().unwrap();
        let j = jobs.get(id)?;
        let mut tail = j.tail_string();
        if let Some(n) = tail_bytes {
            if tail.len() > n {
                let mut cut = tail.len() - n;
                while !tail.is_char_boundary(cut) {
                    cut += 1;
                }
                tail = tail[cut..].to_string();
            }
        }
        Some(JobDetail { info: j.info.clone(), tail })
    }

    /// Signal a running job's process group. Returns false if the job is unknown.
    pub fn kill(&self, id: &str, signal: Option<i32>) -> bool {
        let jobs = self.jobs.lock().unwrap();
        match jobs.get(id) {
            Some(j) => {
                if let Some(pid) = j.info.pid {
                    kill_group(pid, signal.unwrap_or(libc::SIGTERM));
                }
                true
            }
            None => false,
        }
    }

    /// Forget a finished job.
    pub fn remove(&self, id: &str) -> Removal {
        let mut jobs = self.jobs.lock().unwrap();
        match jobs.get(id) {
            Some(j) if j.info.state == JobState::Running => Removal::StillRunning,
            Some(_) => {
                jobs.remove(id);
                Removal::Removed
            }
            None => Removal::Unknown,
        }
    }
}
