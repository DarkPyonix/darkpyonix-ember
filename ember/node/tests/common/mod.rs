//! Shared test helper: write a shell script and make sure it can be executed (#546, #555).
//!
//! A test that writes a script and runs it a moment later, in a multi-threaded test process, can
//! fail with "text file busy" (ETXTBSY): another thread forks while this file is still open for
//! writing, and the forked child holds the descriptor until it execs. Closing the handle here does
//! not close that window. [provisional: the cause is a hypothesis, not established.] So the helper
//! does not trust the write: it executes the script once with a marker argument that makes it exit
//! at once, retries while that fails with ETXTBSY, and only then moves it into place.
//!
//! This is a copy of `ember/server/tests/common/mod.rs` (the crates share no test support).
#![allow(dead_code)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// The argument that makes a script written by this helper exit 0 before its body runs.
pub const PROBE_ARG: &str = "--ember-probe-noop";
/// The guard line the helper puts right after the shebang. It fires only for `PROBE_ARG`.
const GUARD: &str = "[ \"$1\" = --ember-probe-noop ] && exit 0";
const MAX_TRIES: usize = 50;
const WAIT: Duration = Duration::from_millis(20);
/// ETXTBSY on Linux and macOS. Compared by number so the helper does not depend on a recent std.
const ETXTBSY: i32 = 26;

static SEQ: AtomicUsize = AtomicUsize::new(0);

/// The script text with the guard line inserted after the shebang. Content with no shebang gets
/// `#!/bin/sh`. Only shell interpreters are accepted, because the guard is shell syntax.
fn with_guard(content: &str) -> String {
    match content.strip_prefix("#!") {
        Some(rest) => {
            let (line, body) = rest.split_once('\n').expect("a script needs a newline after its shebang");
            let interpreter = line.split_whitespace().next().unwrap_or("");
            assert!(interpreter.ends_with("sh"), "write_exec_ready only writes shell scripts, got `#!{line}`");
            format!("#!{line}\n{GUARD}\n{body}")
        }
        None => format!("#!/bin/sh\n{GUARD}\n{content}"),
    }
}

/// Write the executable script `content` at `path`, and return only when it can be executed.
///
/// Contract:
/// 1. A guard line `[ "$1" = --ember-probe-noop ] && exit 0` is inserted after the shebang
///    (or after an added `#!/bin/sh`). The file is therefore not byte-identical to `content`.
/// 2. The script is written to a sibling temp name (`File::create`, `write_all`, `sync_all`, the
///    handle dropped), chmod 0o755.
/// 3. It is executed with `PROBE_ARG` (stdio null), which exits 0 without running the body. While
///    that spawn fails with ETXTBSY it retries, at most 50 times with 20 ms between tries.
///    Any other spawn error, a non-zero probe exit, or 50 busy tries panics with a message.
/// 4. The temp file is renamed onto `path` (replacing any file there; a running copy is never
///    written to).
///
/// Returns the number of busy retries it needed.
pub fn write_exec_ready_counting(path: &Path, content: &str) -> usize {
    let name = path.file_name().expect("a file name").to_string_lossy().into_owned();
    let tmp = path.with_file_name(format!(".{name}.ready-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
    {
        let mut f = std::fs::File::create(&tmp).unwrap();
        f.write_all(with_guard(content).as_bytes()).unwrap();
        f.sync_all().unwrap();
    }
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut busy = 0;
    loop {
        let probe = Command::new(&tmp)
            .arg(PROBE_ARG)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match probe {
            Ok(status) => {
                assert!(status.success(), "the probe run of {} exited with {status}", tmp.display());
                break;
            }
            Err(e) if e.raw_os_error() == Some(ETXTBSY) || e.kind().to_string().contains("busy") => {
                busy += 1;
                assert!(busy < MAX_TRIES, "{} stayed busy (ETXTBSY) for {MAX_TRIES} tries", tmp.display());
                std::thread::sleep(WAIT);
            }
            Err(e) => panic!("cannot execute {}: error kind {:?}: {e}", tmp.display(), e.kind()),
        }
    }
    std::fs::rename(&tmp, path).unwrap();
    busy
}

/// `write_exec_ready_counting` without the count.
pub fn write_exec_ready(path: &Path, content: &str) {
    write_exec_ready_counting(path, content);
}
