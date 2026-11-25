//! Ends the window when the process that started it ends (`FR-W5b`, #1128).
//!
//! The `ember-app --vscode-window` helper starts this binary with `--exit-on-stdin-eof` and a
//! pipe on stdin whose write end it holds until the window exits. The operating system closes
//! that end when the helper ends for any reason (a normal exit, SIGTERM, SIGKILL, a crash, or
//! the Windows equivalents), so a reader of stdin sees end of file, and the window exits instead
//! of showing a workbench with no gateway behind it. The pipe carries no data; any bytes that do
//! arrive are discarded.

use std::io::{ErrorKind, Read};
use std::thread::JoinHandle;
use std::time::Duration;

/// How long the event loop has to exit on its own after the parent is gone before the process
/// is ended directly (a wedged UI thread must not keep a dead window open).
pub const PARENT_GONE_EXIT_GRACE: Duration = Duration::from_secs(3);

/// Read `reader` until end of file (or a read error, which means the same here: the parent's end
/// is unusable), then call `on_gone` once. Bytes read are discarded and an interrupted read is
/// retried.
pub fn watch_until_eof<R: Read>(mut reader: R, on_gone: impl FnOnce()) {
    let mut buf = [0u8; 256];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(_) => continue,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    on_gone();
}

/// Watch this process's stdin on its own thread and call `on_gone` when it reaches end of file.
/// On Windows a read from a pipe whose writer has closed reports `ERROR_BROKEN_PIPE`, which the
/// standard library returns as a zero-length read, so the same code ends there too.
pub fn spawn_stdin_watcher(on_gone: impl FnOnce() + Send + 'static) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("parent-watch".to_owned())
        .spawn(move || watch_until_eof(std::io::stdin().lock(), on_gone))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::mpsc;

    /// SPEC FR-W5b (#1128): the watcher on a real OS pipe keeps waiting while the writer is
    /// open (bytes on the pipe do not end it) and signals exactly when the writer closes, the
    /// way the helper's end closes when the helper dies.
    #[test]
    fn fr_w5b_watcher_signals_when_the_pipe_writer_closes() {
        let (reader, mut writer) = std::io::pipe().expect("os pipe");
        let (tx, rx) = mpsc::channel();
        let watcher = std::thread::spawn(move || watch_until_eof(reader, move || tx.send(()).unwrap()));

        writer.write_all(b"not a signal\n").unwrap();
        writer.flush().unwrap();
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "the watcher ended while the writer was still open"
        );

        drop(writer);
        rx.recv_timeout(Duration::from_secs(5)).expect("no exit signal after the writer closed");
        watcher.join().unwrap();
        assert!(rx.try_recv().is_err(), "the exit signal came more than once");
    }

    /// SPEC FR-W5b (#1128): a read error ends the watch like end of file, and an interrupted
    /// read does not.
    #[test]
    fn fr_w5b_watcher_treats_a_read_error_as_gone_and_retries_interrupts() {
        struct Script(Vec<std::io::Result<usize>>);
        impl Read for Script {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                self.0.remove(0)
            }
        }
        let mut gone = false;
        watch_until_eof(
            Script(vec![
                Err(ErrorKind::Interrupted.into()),
                Ok(3),
                Err(ErrorKind::BrokenPipe.into()),
            ]),
            || gone = true,
        );
        assert!(gone);
    }
}
