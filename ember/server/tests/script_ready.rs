//! The shared `common::write_exec_ready` helper (#546, #555): a script it wrote can be executed
//! at once, the probe marker never runs the body, and concurrent writers beside a forking thread
//! never see "text file busy".
#![cfg(unix)]

mod common;

use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

#[test]
fn a_written_script_runs_and_receives_its_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("echo-args");
    common::write_exec_ready(&script, "#!/bin/sh\necho \"$#:$1:$2\"\n");
    let out = Command::new(&script).args(["one", "two words"]).output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "2:one:two words\n");
    // Content without a shebang gets `#!/bin/sh`, and `exec` in the body still works.
    let plain = dir.path().join("plain");
    common::write_exec_ready(&plain, "exec echo \"$@\"\n");
    let out = Command::new(&plain).arg("x").output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "x\n");
    // No temp file is left beside the script.
    let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names.len(), 2, "{names:?}");
}

#[test]
fn the_probe_marker_exits_zero_without_running_the_body() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("marks");
    common::write_exec_ready(&script, "#!/bin/sh\ntouch \"$(dirname \"$0\")/ran\"\n");
    // The helper itself probed the script once while writing it.
    assert!(!dir.path().join("ran").exists(), "the probe run executed the body");
    let status = Command::new(&script).arg(common::PROBE_ARG).status().unwrap();
    assert!(status.success());
    assert!(!dir.path().join("ran").exists(), "the marker argument ran the body");
    // Any other argument, and no argument, run the body.
    assert!(Command::new(&script).arg("--other").status().unwrap().success());
    assert!(dir.path().join("ran").exists());
}

fn write_many(dir: &Path, thread: usize, n: usize) -> usize {
    let mut busy = 0;
    for i in 0..n {
        let p = dir.join(format!("s-{thread}-{i}"));
        busy += common::write_exec_ready_counting(&p, &format!("#!/bin/sh\necho {thread}-{i}\n"));
        let out = Command::new(&p).output().expect("a script the helper wrote is executable");
        assert_eq!(String::from_utf8_lossy(&out.stdout), format!("{thread}-{i}\n"));
    }
    busy
}

/// The stress test of the ETXTBSY hypothesis: 8 threads write 200 scripts in all while another
/// thread keeps forking. The helper must never hand a busy script back to its caller. It prints
/// how many busy retries it needed; the count is not asserted (it can legitimately be zero).
#[test]
fn concurrent_writers_beside_a_forking_thread_never_see_text_file_busy() {
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let forker = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut forks = 0usize;
            while !stop.load(Ordering::Relaxed) {
                Command::new("sh").args(["-c", "true"]).status().unwrap();
                forks += 1;
            }
            forks
        })
    };
    let busy = Arc::new(AtomicUsize::new(0));
    let writers: Vec<_> = (0..8)
        .map(|t| {
            let (dir, busy) = (dir.clone(), busy.clone());
            std::thread::spawn(move || {
                busy.fetch_add(write_many(dir.path(), t, 25), Ordering::Relaxed);
            })
        })
        .collect();
    for w in writers {
        w.join().expect("a writer saw an error from write_exec_ready or from executing its script");
    }
    stop.store(true, Ordering::Relaxed);
    let forks = forker.join().unwrap();
    let line = format!("script_ready: 200 scripts, {forks} concurrent forks, {} busy retries", busy.load(Ordering::Relaxed));
    eprintln!("{line}");
    // The test harness captures print!/eprintln! only; a direct write to the real stdout reaches the
    // CI log of a passing test, as a GitHub `notice` annotation.
    {
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = writeln!(out, "::notice title=script_ready::{line}");
        let _ = out.flush();
    }
    // `cargo test` hides stderr of a passing test, which would throw away the only measurement of
    // the ETXTBSY hypothesis (#546, #555). On GitHub Actions the line is appended to the job summary
    // instead; without the variable (local runs) nothing is written.
    if let Some(path) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(path) {
            let _ = writeln!(f, "{line}");
        }
    }
}
