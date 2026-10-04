//! Starting commands, with pipes or under a PTY, and streaming their output (FR-X1).
//!
//! Every command runs as the leader of its own process group (pipes: `setpgid(0, 0)`; PTY: the
//! session leader that `portable-pty` creates), so a kill reaches the whole tree a shell spawned.
//! Output events are delivered in order and [`ExecEvent::Exit`] is always last: after the process
//! exits, remaining output is drained for up to [`DRAIN_GRACE`] (a background grandchild can
//! hold the pipe open indefinitely).

use std::io::{Read, Write};
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portable_pty::{native_pty_system, CommandBuilder, MasterPty};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::ChildStdin;
use tokio::sync::{mpsc, oneshot};

use crate::proto::{CommandSpec, ExecEvent, Program, PtySize};

pub const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// A started command: its output stream and the means to control it.
pub struct Running {
    pub pid: u32,
    /// Stdout/Stderr events, then exactly one Exit.
    pub events: mpsc::Receiver<ExecEvent>,
    pub control: Control,
}

pub enum Control {
    Pipe { stdin: Option<ChildStdin> },
    Pty { writer: Arc<Mutex<Box<dyn Write + Send>>>, master: Arc<Mutex<Box<dyn MasterPty + Send>>> },
}

impl Control {
    pub async fn write_stdin(&mut self, data: Vec<u8>) -> anyhow::Result<()> {
        match self {
            Control::Pipe { stdin: Some(s) } => {
                s.write_all(&data).await?;
                s.flush().await?;
            }
            Control::Pipe { stdin: None } => anyhow::bail!("stdin is closed"),
            Control::Pty { writer, .. } => {
                let w = writer.clone();
                tokio::task::spawn_blocking(move || -> std::io::Result<()> {
                    let mut w = w.lock().unwrap();
                    w.write_all(&data)?;
                    w.flush()
                })
                .await??;
            }
        }
        Ok(())
    }

    pub fn close_stdin(&mut self) {
        if let Control::Pipe { stdin } = self {
            stdin.take();
        }
    }

    pub fn resize(&self, size: PtySize) -> anyhow::Result<()> {
        match self {
            Control::Pty { master, .. } => {
                master.lock().unwrap().resize(pty_size(size))?;
                Ok(())
            }
            Control::Pipe { .. } => anyhow::bail!("resize needs a PTY"),
        }
    }
}

/// Signal a process group. Errors (e.g. it already exited) are ignored.
pub fn kill_group(pid: u32, signal: i32) {
    // SAFETY: plain syscall; a stale pid at worst signals nothing (ESRCH).
    unsafe {
        libc::killpg(pid as libc::pid_t, signal);
    }
}

pub fn argv(program: &Program) -> anyhow::Result<Vec<String>> {
    match program {
        Program::Argv(v) if v.is_empty() => anyhow::bail!("empty argv"),
        Program::Argv(v) => Ok(v.clone()),
        Program::Shell(s) => Ok(vec!["/bin/sh".into(), "-c".into(), s.clone()]),
    }
}

fn exit_event_from_wait(status: libc::c_int) -> ExecEvent {
    if libc::WIFEXITED(status) {
        ExecEvent::Exit { code: Some(libc::WEXITSTATUS(status)), signal: None }
    } else if libc::WIFSIGNALED(status) {
        ExecEvent::Exit { code: None, signal: Some(libc::WTERMSIG(status)) }
    } else {
        ExecEvent::Exit { code: None, signal: None }
    }
}

async fn pump<R: AsyncRead + Unpin>(
    mut r: R,
    tx: mpsc::Sender<ExecEvent>,
    wrap: fn(Vec<u8>) -> ExecEvent,
) {
    let mut buf = vec![0u8; 32 * 1024];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if tx.send(wrap(buf[..n].to_vec())).await.is_err() {
                    break;
                }
            }
        }
    }
}

/// Start with stdout and stderr on separate pipes. With `stdin == false`, stdin is `/dev/null`.
pub fn spawn_pipes(spec: &CommandSpec, cwd: &Path, stdin: bool) -> anyhow::Result<Running> {
    let argv = argv(&spec.program)?;
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(cwd)
        .stdin(if stdin { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if spec.env_clear {
        cmd.env_clear();
    }
    cmd.envs(&spec.env);
    let mut child = cmd.spawn().map_err(|e| anyhow::anyhow!("spawn {}: {e}", argv[0]))?;
    let pid = child.id().unwrap_or_default();

    let (tx, rx) = mpsc::channel(256);
    let out = tokio::spawn(pump(child.stdout.take().unwrap(), tx.clone(), |data| ExecEvent::Stdout { data }));
    let err = tokio::spawn(pump(child.stderr.take().unwrap(), tx.clone(), |data| ExecEvent::Stderr { data }));
    let stdin = child.stdin.take();
    tokio::spawn(async move {
        let status = child.wait().await;
        let _ = tokio::time::timeout(DRAIN_GRACE, async {
            let _ = out.await;
            let _ = err.await;
        })
        .await;
        let ev = match status {
            Ok(s) => {
                use std::os::unix::process::ExitStatusExt;
                ExecEvent::Exit { code: s.code(), signal: s.signal() }
            }
            Err(e) => ExecEvent::Error { message: format!("wait: {e}") },
        };
        let _ = tx.send(ev).await;
    });
    Ok(Running { pid, events: rx, control: Control::Pipe { stdin } })
}

fn pty_size(s: PtySize) -> portable_pty::PtySize {
    portable_pty::PtySize { rows: s.rows, cols: s.cols, pixel_width: 0, pixel_height: 0 }
}

/// Start under a pseudo-terminal. All output arrives as `Stdout`.
pub fn spawn_pty(spec: &CommandSpec, cwd: &Path, size: PtySize) -> anyhow::Result<Running> {
    let argv = argv(&spec.program)?;
    let pair = native_pty_system().openpty(pty_size(size))?;
    let mut cmd = CommandBuilder::from_argv(argv.iter().map(Into::into).collect());
    cmd.cwd(cwd);
    if spec.env_clear {
        cmd.env_clear();
    }
    if !spec.env.contains_key("TERM") {
        cmd.env("TERM", "xterm-256color");
    }
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    let child = pair.slave.spawn_command(cmd)?;
    // The parent must not keep the slave open, or the reader never sees end-of-file.
    drop(pair.slave);
    let pid = child.process_id().ok_or_else(|| anyhow::anyhow!("pty child has no pid"))?;
    // The child is reaped below with waitpid, which reports the exact signal number.
    drop(child);

    let mut reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;
    let (tx, rx) = mpsc::channel(256);
    let (done_tx, done_rx) = oneshot::channel::<()>();
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 32 * 1024];
            loop {
                match reader.read(&mut buf) {
                    // Linux reports EIO once the slave side is closed.
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.blocking_send(ExecEvent::Stdout { data: buf[..n].to_vec() }).is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = done_tx.send(());
        });
    }
    tokio::spawn(async move {
        let status = tokio::task::spawn_blocking(move || {
            let mut status: libc::c_int = 0;
            loop {
                // SAFETY: waiting on our own child pid.
                let r = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, 0) };
                if r == -1 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return if r == -1 { Err(std::io::Error::last_os_error()) } else { Ok(status) };
            }
        })
        .await;
        let _ = tokio::time::timeout(DRAIN_GRACE, done_rx).await;
        let ev = match status {
            Ok(Ok(s)) => exit_event_from_wait(s),
            Ok(Err(e)) => ExecEvent::Error { message: format!("waitpid: {e}") },
            Err(e) => ExecEvent::Error { message: format!("waitpid: {e}") },
        };
        let _ = tx.send(ev).await;
    });
    Ok(Running {
        pid,
        events: rx,
        control: Control::Pty { writer: Arc::new(Mutex::new(writer)), master: Arc::new(Mutex::new(pair.master)) },
    })
}
