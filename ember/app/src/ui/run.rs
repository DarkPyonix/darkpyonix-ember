//! FR-E2, #448: Project runner for editor Run tab and Action::Run / Action::Stop.
//!
//! Spawns local build/run commands for the open file or project, streams output lines,
//! and supports clean process termination via child.kill().

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use dioxus_compose::html::prelude::dioxus_signals::WritableExt;
use dioxus_compose::prelude::SyncSignal;

/// Current status of a run session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunStatus {
    Idle,
    Running { command: String },
    Finished { command: String, code: Option<i32> },
    Stopped { command: String },
}

impl RunStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Idle => "대기",
            Self::Running { .. } => "실행 중",
            Self::Finished { code: Some(0), .. } => "성공",
            Self::Finished { .. } => "오류",
            Self::Stopped { .. } => "중지됨",
        }
    }
}

/// A runner session that executes a child process and collects its output.
#[derive(Clone, Default)]
pub struct RunSession {
    inner: Arc<Mutex<RunSessionInner>>,
}

struct RunSessionInner {
    status: RunStatus,
    lines: Vec<String>,
    child: Option<Child>,
}

impl Default for RunSessionInner {
    fn default() -> Self {
        Self {
            status: RunStatus::Idle,
            lines: Vec::new(),
            child: None,
        }
    }
}

impl RunSession {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot current status and collected output lines.
    pub fn snapshot(&self) -> (RunStatus, Vec<String>) {
        let lock = self.inner.lock().unwrap();
        (lock.status.clone(), lock.lines.clone())
    }

    /// True if a child process is currently running.
    pub fn is_running(&self) -> bool {
        let lock = self.inner.lock().unwrap();
        matches!(lock.status, RunStatus::Running { .. })
    }

    /// Clear output lines and reset status if not running.
    pub fn clear(&self) {
        let mut lock = self.inner.lock().unwrap();
        lock.lines.clear();
        if !matches!(lock.status, RunStatus::Running { .. }) {
            lock.status = RunStatus::Idle;
        }
    }

    /// Stop the running child process.
    pub fn stop(&self) {
        let mut lock = self.inner.lock().unwrap();
        if let Some(mut child) = lock.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let RunStatus::Running { command } = &lock.status {
            let cmd = command.clone();
            lock.lines.push("[프로세스가 중지되었습니다]".to_string());
            lock.status = RunStatus::Stopped { command: cmd };
        }
    }

    /// Start a command, terminating any previous running process.
    pub fn start(&self, prog: &str, args: &[String], cwd: &str, mut revision: SyncSignal<u64>) -> Result<(), String> {
        let mut lock = self.inner.lock().unwrap();
        if let Some(mut prev) = lock.child.take() {
            let _ = prev.kill();
            let _ = prev.wait();
        }
        let full_cmd = if args.is_empty() {
            prog.to_string()
        } else {
            format!("{prog} {}", args.join(" "))
        };
        lock.lines.push(format!("$ {full_cmd}"));
        lock.status = RunStatus::Running { command: full_cmd.clone() };

        let mut cmd = Command::new(prog);
        cmd.args(args);
        if !cwd.is_empty() {
            cmd.current_dir(cwd);
        }
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let err_msg = format!("실행 실패: {e}");
                lock.lines.push(err_msg.clone());
                lock.status = RunStatus::Finished { command: full_cmd, code: None };
                return Err(err_msg);
            }
        };

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        lock.child = Some(child);
        drop(lock);

        let inner_clone = self.inner.clone();
        let cmd_clone = full_cmd.clone();

        // Background reader thread
        std::thread::spawn(move || {
            let mut threads = Vec::new();

            if let Some(out) = stdout {
                let inner = inner_clone.clone();
                let mut rev_out = revision;
                threads.push(std::thread::spawn(move || {
                    let reader = BufReader::new(out);
                    for line in reader.lines().flatten() {
                        inner.lock().unwrap().lines.push(line);
                        *rev_out.write() += 1;
                    }
                }));
            }

            if let Some(err) = stderr {
                let inner = inner_clone.clone();
                let mut rev_err = revision;
                threads.push(std::thread::spawn(move || {
                    let reader = BufReader::new(err);
                    for line in reader.lines().flatten() {
                        inner.lock().unwrap().lines.push(line);
                        *rev_err.write() += 1;
                    }
                }));
            }

            for t in threads {
                let _ = t.join();
            }

            let mut lock = inner_clone.lock().unwrap();
            let exit_code = if let Some(mut ch) = lock.child.take() {
                ch.wait().ok().and_then(|st| st.code())
            } else {
                None
            };
            if matches!(lock.status, RunStatus::Running { .. }) {
                match exit_code {
                    Some(c) => lock.lines.push(format!("[프로세스 종료: 코드 {c}]")),
                    None => lock.lines.push("[프로세스 종료]".to_string()),
                }
                lock.status = RunStatus::Finished { command: cmd_clone, code: exit_code };
            }
            drop(lock);
            *revision.write() += 1;
        });

        Ok(())
    }
}

/// Detect the default program and arguments to run based on current active file or project folder.
pub fn detect_run_command(current_file: Option<&str>, folder: &str) -> (String, Vec<String>) {
    if let Some(file) = current_file {
        if file.ends_with(".py") {
            return ("python3".into(), vec![file.to_string()]);
        }
        if file.ends_with(".sh") {
            return ("sh".into(), vec![file.to_string()]);
        }
        if file.ends_with(".js") {
            return ("node".into(), vec![file.to_string()]);
        }
        if file.ends_with(".rs") {
            return ("cargo".into(), vec!["run".into()]);
        }
    }
    let cargo_toml = std::path::Path::new(folder).join("Cargo.toml");
    if cargo_toml.exists() {
        ("cargo".into(), vec!["run".into()])
    } else if let Some(file) = current_file {
        ("sh".into(), vec![file.to_string()])
    } else {
        ("cargo".into(), vec!["run".into()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_run_command_by_extension() {
        assert_eq!(detect_run_command(Some("/w/main.py"), "/w"), ("python3".into(), vec!["/w/main.py".into()]));
        assert_eq!(detect_run_command(Some("/w/script.sh"), "/w"), ("sh".into(), vec!["/w/script.sh".into()]));
        assert_eq!(detect_run_command(Some("/w/app.js"), "/w"), ("node".into(), vec!["/w/app.js".into()]));
        assert_eq!(detect_run_command(Some("/w/main.rs"), "/w"), ("cargo".into(), vec!["run".into()]));
    }

    #[test]
    fn run_session_lifecycle() {
        let session = RunSession::new();
        assert_eq!(session.snapshot().0, RunStatus::Idle);
        assert!(!session.is_running());

        // Stop while idle is safe
        session.stop();
        assert_eq!(session.snapshot().0, RunStatus::Idle);

        // Clear resets
        session.clear();
        assert!(session.snapshot().1.is_empty());
    }
}
