//! The PTY of a persistent session, and the PTY keeper that lets its process outlive ember node.
//!
//! A session is started with `portable-pty` (the program becomes a session leader with the PTY
//! as its controlling terminal), after which ember node keeps only its own duplicate of the
//! master fd ([`Pty`]). It does not use portable-pty's writer: dropping that writer sends
//! `\n` + EOF to the program.
//!
//! **Keeper.** When the last master fd closes, the kernel hangs up the terminal and the shell
//! receives SIGHUP. So that a session survives an ember node restart (FR-P6, best effort), node
//! hands a duplicate of the master to a tiny per-session keeper process — `ember-node
//! __keep-pty` — started in its own session (`setsid`), so it is outside node's process group and
//! is not killed with it. The keeper does nothing but hold the fd and hand it back over a unix
//! socket (`SCM_RIGHTS`) to whoever asks: the restarted node. It exits when the session's process
//! is gone or when node tells it to (`Q`). While no node runs, nobody reads the PTY: the program
//! keeps running until the kernel's PTY buffer is full, then blocks on output until node is back.
//!
//! Why a keeper and not a holder that also owns the terminal model (as `tmux`/`dtach` do): the
//! keeper is ~100 lines with no protocol beyond "send me the fd", stays a few hundred kilobytes
//! of RSS, and keeps a single session engine in node. The cost is that scrollback is only carried
//! over a *graceful* node restart (node writes a snapshot on SIGTERM/SIGINT and feeds it to the
//! re-adopted session's model); after a crash the re-adopted session starts with an empty model.
//! Exit status of a re-adopted session is unknown (the process is no longer node's child).

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use portable_pty::{native_pty_system, CommandBuilder};

use crate::proto::PtySize;

/// The master side of a session's PTY.
pub struct Pty {
    fd: OwnedFd,
}

impl Pty {
    pub fn from_fd(fd: OwnedFd) -> Self {
        Self { fd }
    }

    pub fn raw(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// A blocking reader (its own fd).
    pub fn reader(&self) -> io::Result<File> {
        Ok(File::from(self.fd.try_clone()?))
    }

    /// A blocking writer (its own fd).
    pub fn writer(&self) -> io::Result<File> {
        Ok(File::from(self.fd.try_clone()?))
    }

    pub fn resize(&self, size: PtySize) -> io::Result<()> {
        let ws = libc::winsize { ws_row: size.rows, ws_col: size.cols, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: TIOCSWINSZ on our own master fd with a valid winsize.
        if unsafe { libc::ioctl(self.raw(), libc::TIOCSWINSZ as _, &ws as *const libc::winsize) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The terminal's foreground process group (the job the user is running), if any.
    pub fn foreground_pgrp(&self) -> Option<i32> {
        // SAFETY: plain query on our own fd.
        match unsafe { libc::tcgetpgrp(self.raw()) } {
            p if p > 0 => Some(p),
            _ => None,
        }
    }
}

/// Start `argv` on a new PTY. Returns the master and the session leader's pid. The caller reaps
/// the pid (`waitpid`).
pub fn spawn(
    argv: &[String],
    cwd: &Path,
    env: &std::collections::BTreeMap<String, String>,
    env_clear: bool,
    size: PtySize,
    session_id: &str,
) -> anyhow::Result<(Pty, u32)> {
    anyhow::ensure!(!argv.is_empty(), "empty argv");
    let pair = native_pty_system().openpty(portable_pty::PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut cmd = CommandBuilder::from_argv(argv.iter().map(Into::into).collect());
    cmd.cwd(cwd);
    if env_clear {
        cmd.env_clear();
    }
    if !env.contains_key("TERM") {
        cmd.env("TERM", "xterm-256color");
    }
    if !env.contains_key("COLORTERM") {
        cmd.env("COLORTERM", "truecolor");
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    // Lets programs (and a nested ember-term) know they run inside a persistent session.
    cmd.env("EMBER_TERM_ID", session_id);
    // Never leak the node's credentials into the session.
    cmd.env_remove("EMBER_NODE_TOKEN");
    let child = pair.slave.spawn_command(cmd)?;
    // The parent must not keep the slave open, or the reader never sees end-of-file.
    drop(pair.slave);
    let pid = child.process_id().ok_or_else(|| anyhow::anyhow!("pty child has no pid"))?;
    // Reaped by the session with waitpid, which reports the exact signal number.
    drop(child);
    let raw = pair.master.as_raw_fd().ok_or_else(|| anyhow::anyhow!("pty master has no fd"))?;
    // SAFETY: `raw` is open for as long as `pair.master` lives; we duplicate it (CLOEXEC) first.
    let fd = unsafe { BorrowedFd::borrow_raw(raw) }.try_clone_to_owned()?;
    drop(pair.master);
    Ok((Pty { fd }, pid))
}

/// Is `pid` still a live process (or one we may not signal)?
pub fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks existence and permission.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

// ---------------------------------------------------------------------------------------------
// Keeper

/// The hidden subcommand of the `ember-node` binary that runs a keeper.
pub const KEEPER_SUBCOMMAND: &str = "__keep-pty";

/// Unix socket paths are limited to ~104 bytes (macOS); longer ones are refused up front.
const MAX_SOCKET_PATH: usize = 100;

/// A keeper started by this node run.
pub struct KeeperHandle {
    pub socket: PathBuf,
    pub pid: u32,
}

/// Start a keeper holding a duplicate of `pty`'s master. `exe` is the `ember-node` binary.
pub fn spawn_keeper(exe: &Path, socket: &Path, pty: &Pty, session_pid: u32) -> io::Result<KeeperHandle> {
    if socket.as_os_str().len() > MAX_SOCKET_PATH {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("socket path too long: {}", socket.display())));
    }
    let raw = pty.raw();
    let mut cmd = std::process::Command::new(exe);
    cmd.arg(KEEPER_SUBCOMMAND)
        .arg("--socket")
        .arg(socket)
        .arg("--pid")
        .arg(session_pid.to_string())
        .arg("--fd")
        .arg(raw.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            // Let the master fd survive exec in the child only.
            if libc::fcntl(raw, libc::F_SETFD, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            // Own session: not in node's process group, no controlling terminal.
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    let pid = child.id();
    // Reap it whenever it exits, so it never lingers as a zombie of this node.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(KeeperHandle { socket: socket.to_path_buf(), pid })
}

/// Ask the keeper at `socket` for the master fd.
pub fn fetch_from_keeper(socket: &Path) -> io::Result<OwnedFd> {
    let mut s = UnixStream::connect(socket)?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    s.write_all(b"F")?;
    recv_fd(&s)
}

/// Tell the keeper at `socket` to exit (the session ended or was killed). Best effort.
pub fn stop_keeper(socket: &Path) {
    if let Ok(mut s) = UnixStream::connect(socket) {
        let _ = s.write_all(b"Q");
    }
}

/// `ember-node __keep-pty --socket <path> --pid <session pid> --fd <master fd>`.
/// Returns the process exit code.
pub fn keeper_main(args: &[String]) -> i32 {
    let mut socket = None;
    let mut pid = None;
    let mut fd = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--socket" => socket = it.next().map(PathBuf::from),
            "--pid" => pid = it.next().and_then(|v| v.parse::<u32>().ok()),
            "--fd" => fd = it.next().and_then(|v| v.parse::<RawFd>().ok()),
            _ => {}
        }
    }
    let (Some(socket), Some(pid), Some(fd)) = (socket, pid, fd) else {
        eprintln!("usage: ember-node {KEEPER_SUBCOMMAND} --socket <path> --pid <pid> --fd <fd>");
        return 2;
    };
    // SAFETY: the fd was inherited from node for exactly this purpose and nothing else owns it.
    let master = unsafe { OwnedFd::from_raw_fd(fd) };
    let _ = std::fs::remove_file(&socket);
    let listener = match UnixListener::bind(&socket) {
        Ok(l) => l,
        Err(_) => return 1,
    };
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600));
    }
    let watched = socket.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(2));
        if !alive(pid) {
            let _ = std::fs::remove_file(&watched);
            std::process::exit(0);
        }
    });
    for conn in listener.incoming() {
        let Ok(mut c) = conn else { continue };
        let _ = c.set_read_timeout(Some(Duration::from_secs(5)));
        let mut cmd = [0u8; 1];
        if c.read_exact(&mut cmd).is_err() {
            continue;
        }
        match cmd[0] {
            b'F' => {
                let _ = send_fd(&c, master.as_raw_fd());
            }
            b'Q' => break,
            _ => {}
        }
    }
    let _ = std::fs::remove_file(&socket);
    0
}

/// Room for one `SCM_RIGHTS` fd, aligned for `cmsghdr`.
type CmsgBuf = [u64; 8];

fn send_fd(sock: &UnixStream, fd: RawFd) -> io::Result<()> {
    let mut byte = [b'F'];
    let mut iov = libc::iovec { iov_base: byte.as_mut_ptr().cast(), iov_len: 1 };
    let mut cbuf: CmsgBuf = [0; 8];
    // SAFETY: standard SCM_RIGHTS construction into a zeroed, aligned, large-enough buffer.
    unsafe {
        let space = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as _) as usize;
        debug_assert!(space <= std::mem::size_of::<CmsgBuf>());
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1 as _;
        msg.msg_control = cbuf.as_mut_ptr().cast();
        msg.msg_controllen = space as _;
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        if cmsg.is_null() {
            return Err(io::Error::other("no room for control message"));
        }
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as _) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(cmsg).cast::<RawFd>(), fd);
        if libc::sendmsg(sock.as_raw_fd(), &msg, 0) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn recv_fd(sock: &UnixStream) -> io::Result<OwnedFd> {
    let mut byte = [0u8; 1];
    let mut iov = libc::iovec { iov_base: byte.as_mut_ptr().cast(), iov_len: 1 };
    let mut cbuf: CmsgBuf = [0; 8];
    // SAFETY: recvmsg into a zeroed, aligned buffer; the control message is validated before use.
    unsafe {
        let space = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as _) as usize;
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1 as _;
        msg.msg_control = cbuf.as_mut_ptr().cast();
        msg.msg_controllen = space as _;
        let n = libc::recvmsg(sock.as_raw_fd(), &mut msg, 0);
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "keeper closed without an fd"));
        }
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        if cmsg.is_null() || (*cmsg).cmsg_level != libc::SOL_SOCKET || (*cmsg).cmsg_type != libc::SCM_RIGHTS {
            return Err(io::Error::other("keeper sent no fd"));
        }
        let fd = std::ptr::read_unaligned(libc::CMSG_DATA(cmsg).cast::<RawFd>());
        if fd < 0 {
            return Err(io::Error::other("keeper sent an invalid fd"));
        }
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        Ok(OwnedFd::from_raw_fd(fd))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fds_cross_a_unix_socket() {
        let (a, b) = UnixStream::pair().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"through the socket").unwrap();
        let f = File::open(&path).unwrap();
        send_fd(&a, f.as_raw_fd()).unwrap();
        let got = recv_fd(&b).unwrap();
        let mut s = String::new();
        File::from(got).read_to_string(&mut s).unwrap();
        assert_eq!(s, "through the socket");
    }
}
