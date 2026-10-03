//! Mount and unmount command lines, and mount point checks. Pure where possible so the exact
//! commands are unit-tested.
//!
//! Options (macOS `mount_nfs(8)`, Linux `nfs(5)`):
//!
//! | Option | Why |
//! |-|-|
//! | `vers=3,tcp,port=P,mountport=P` | nfsserve speaks NFSv3 + MOUNT on one TCP port, no portmapper |
//! | `soft,intr` | a lost server returns an error instead of retrying forever (`intr` is a no-op on modern Linux; `soft` alone does it) |
//! | `timeo=T,retrans=R` | `T` tenths of a second per try, `R` retries: an outage surfaces after about T×R/10 s |
//! | `retrycnt=0` (macOS) | fail the mount itself at once instead of retrying in the background |
//! | `deadtimeout=S` (macOS) | after S s unresponsive the kernel force-unmounts it: no zombie mount if ember server dies |
//! | `locallocks` (macOS) / `nolock` (Linux) | no NLM lock server in nfsserve; locks stay local |
//! | `nonegnamecache` (macOS) | a file created on the node is not hidden by a cached "not found" |
//! | `actimeo=A` | kernel attribute cache lifetime, matched to the mount's own TTL |
//! | `nobrowse` (macOS) | keep it out of Finder's sidebar |
//!
//! Exact option names were taken from the man pages as remembered and the nfsserve README; they
//! are **[U]** until the commands are run on the Mac mini and the Pi.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// The tunables that end up on the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NfsOptions {
    pub port: u16,
    /// Per-try timeout, tenths of a second.
    pub timeo_ds: u32,
    pub retrans: u32,
    /// macOS `deadtimeout`, seconds.
    pub deadtimeout_s: u32,
    /// Kernel attribute cache, seconds (0 = `noac`).
    pub actimeo_s: u32,
    pub rsize: u32,
    pub wsize: u32,
}

impl NfsOptions {
    pub fn new(port: u16) -> NfsOptions {
        NfsOptions {
            port,
            timeo_ds: 50,
            retrans: 2,
            deadtimeout_s: 60,
            actimeo_s: 1,
            rsize: 1024 * 1024,
            wsize: 1024 * 1024,
        }
    }

    /// How long a syscall can wait on a lost server before it fails.
    pub fn worst_case_wait(&self) -> Duration {
        Duration::from_millis(self.timeo_ds as u64 * 100 * (self.retrans as u64 + 1))
    }
}

/// A command to run: program and arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    pub program: PathBuf,
    pub args: Vec<String>,
}

impl Cmd {
    fn new(program: &str, args: Vec<String>) -> Cmd {
        Cmd { program: program.into(), args }
    }

    pub fn to_tokio(&self) -> tokio::process::Command {
        let mut c = tokio::process::Command::new(&self.program);
        c.args(&self.args).stdin(std::process::Stdio::null()).kill_on_drop(true);
        c
    }
}

/// The server OS the command is for (a parameter so both are testable everywhere).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    MacOs,
    Linux,
}

impl Os {
    pub fn current() -> Option<Os> {
        if cfg!(target_os = "macos") {
            Some(Os::MacOs)
        } else if cfg!(target_os = "linux") {
            Some(Os::Linux)
        } else {
            None
        }
    }
}

/// `mount_nfs` / `mount -t nfs` for a loopback nfsserve export at `at`.
pub fn nfs_mount(os: Os, o: &NfsOptions, at: &Path) -> Cmd {
    let ac = if o.actimeo_s == 0 { "noac".to_string() } else { format!("actimeo={}", o.actimeo_s) };
    let at = at.to_string_lossy().into_owned();
    match os {
        Os::MacOs => {
            let opts = [
                "vers=3".to_string(),
                "tcp".into(),
                format!("port={}", o.port),
                format!("mountport={}", o.port),
                "soft".into(),
                "intr".into(),
                format!("timeo={}", o.timeo_ds),
                format!("retrans={}", o.retrans),
                "retrycnt=0".into(),
                format!("deadtimeout={}", o.deadtimeout_s),
                "locallocks".into(),
                "nonegnamecache".into(),
                ac,
                format!("rsize={}", o.rsize),
                format!("wsize={}", o.wsize),
                "nobrowse".into(),
            ]
            .join(",");
            Cmd::new("/sbin/mount_nfs", vec!["-o".into(), opts, "127.0.0.1:/".into(), at])
        }
        Os::Linux => {
            // Needs root (or an fstab `user` entry): the Linux default is FUSE, this is the fallback.
            let opts = [
                "vers=3".to_string(),
                "proto=tcp".into(),
                format!("port={}", o.port),
                format!("mountport={}", o.port),
                "mountproto=tcp".into(),
                "soft".into(),
                format!("timeo={}", o.timeo_ds),
                format!("retrans={}", o.retrans),
                "nolock".into(),
                ac,
                format!("rsize={}", o.rsize),
                format!("wsize={}", o.wsize),
            ]
            .join(",");
            Cmd::new("/bin/mount", vec!["-t".into(), "nfs".into(), "-o".into(), opts, "127.0.0.1:/".into(), at])
        }
    }
}

/// Unmount `at`. `force` for a mount whose server is gone (`umount -f`; on Linux a lazy detach,
/// or `fusermount3 -u -z` for FUSE).
pub fn unmount(os: Os, fuse: bool, at: &Path, force: bool) -> Cmd {
    let at = at.to_string_lossy().into_owned();
    match (os, fuse) {
        (Os::MacOs, _) => {
            let mut args = Vec::new();
            if force {
                args.push("-f".to_string());
            }
            args.push(at);
            Cmd::new("/sbin/umount", args)
        }
        (Os::Linux, true) => {
            let mut args = vec!["-u".to_string()];
            if force {
                args.push("-z".into());
            }
            args.push(at);
            Cmd::new("fusermount3", args)
        }
        (Os::Linux, false) => {
            let mut args = Vec::new();
            if force {
                args.push("-f".to_string());
                args.push("-l".to_string());
            }
            args.push(at);
            Cmd::new("/bin/umount", args)
        }
    }
}

/// What the mount point needs before mounting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountPoint {
    /// An empty directory: mount on it.
    Ready,
    /// Already a mount point (a stale mount from a crashed server, or someone else's).
    Mounted,
    /// Missing: create these directories (outermost first), then mount.
    Create(Vec<PathBuf>),
}

/// Why a mount point cannot be used, with what to do about it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MountPointError {
    #[error("{0} exists on this server and is not empty; mounting the computer's project there would hide it. Move it away, or set EMBER_MOUNT_SHADOW=1 to mount over it")]
    NotEmpty(PathBuf),
    #[error("{0} exists on this server and is not a directory")]
    NotADirectory(PathBuf),
    #[error("{path} cannot be created by this user ({reason}). {hint}")]
    NeedsSetup { path: PathBuf, reason: String, hint: String },
}

/// Inspect `at` (blocking: it touches the filesystem). `allow_shadow` permits mounting over a
/// non-empty directory.
pub fn inspect(os: Os, at: &Path, allow_shadow: bool) -> Result<MountPoint, MountPointError> {
    use std::os::unix::fs::MetadataExt;
    match std::fs::symlink_metadata(at) {
        Ok(m) if !m.is_dir() => Err(MountPointError::NotADirectory(at.into())),
        Ok(m) => {
            let parent_dev = at.parent().and_then(|p| std::fs::metadata(p).ok()).map(|p| p.dev());
            if parent_dev.is_some_and(|d| d != m.dev()) {
                return Ok(MountPoint::Mounted);
            }
            let empty = std::fs::read_dir(at).map(|mut r| r.next().is_none()).unwrap_or(false);
            if empty || allow_shadow {
                Ok(MountPoint::Ready)
            } else {
                Err(MountPointError::NotEmpty(at.into()))
            }
        }
        Err(_) => {
            // Walk up to the nearest existing ancestor; it must be writable by us.
            let mut missing = vec![at.to_path_buf()];
            let mut cur = at;
            while let Some(p) = cur.parent() {
                if p.exists() {
                    if !writable(p) {
                        return Err(MountPointError::NeedsSetup {
                            path: at.into(),
                            reason: format!("{} is not writable", p.display()),
                            hint: setup_hint(os, at),
                        });
                    }
                    break;
                }
                missing.push(p.to_path_buf());
                cur = p;
            }
            missing.reverse();
            Ok(MountPoint::Create(missing))
        }
    }
}

fn writable(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else { return false };
    // SAFETY: `c` is a valid NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), libc::W_OK | libc::X_OK) == 0 }
}

/// The one-time privileged step that makes `at` usable, per OS.
pub fn setup_hint(os: Os, at: &Path) -> String {
    let first = at.components().nth(1).map(|c| c.as_os_str().to_string_lossy().into_owned()).unwrap_or_default();
    let p = at.display();
    match os {
        Os::MacOs if first == "home" => format!(
            "On macOS /home is an automount point: once, comment out the `/home auto_home` line in \
             /etc/auto_master, run `sudo automount -vc`, then `sudo mkdir -p {p} && sudo chown $(id -un) {p}` \
             (or run scripts/ember-mount-setup.sh {p})."
        ),
        Os::MacOs if !matches!(first.as_str(), "Users" | "Volumes" | "private" | "opt" | "usr" | "tmp" | "var") => format!(
            "On macOS a new top-level directory (/{first}) needs a line `{first}<TAB>System/Volumes/Data/{first}` in /etc/synthetic.conf and a \
             reboot, then `sudo mkdir -p {p} && sudo chown $(id -un) {p}` (or run scripts/ember-mount-setup.sh {p})."
        ),
        _ => format!("Once, as an administrator: `sudo mkdir -p {p} && sudo chown $(id -un) {p}` (or run scripts/ember-mount-setup.sh {p})."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_mount_is_a_soft_loopback_nfsv3_mount() {
        let o = NfsOptions::new(40123);
        let c = nfs_mount(Os::MacOs, &o, Path::new("/Users/me/proj"));
        assert_eq!(c.program, Path::new("/sbin/mount_nfs"));
        assert_eq!(c.args[0], "-o");
        assert_eq!(
            c.args[1],
            "vers=3,tcp,port=40123,mountport=40123,soft,intr,timeo=50,retrans=2,retrycnt=0,deadtimeout=60,\
             locallocks,nonegnamecache,actimeo=1,rsize=1048576,wsize=1048576,nobrowse"
        );
        assert_eq!(&c.args[2..], ["127.0.0.1:/", "/Users/me/proj"]);
        assert!(!c.args[1].contains("hard"));
        assert_eq!(o.worst_case_wait(), Duration::from_secs(15));
    }

    #[test]
    fn linux_nfs_mount_and_noac() {
        let mut o = NfsOptions::new(7);
        o.actimeo_s = 0;
        let c = nfs_mount(Os::Linux, &o, Path::new("/home/pi/p"));
        assert_eq!(c.program, Path::new("/bin/mount"));
        assert_eq!(&c.args[..3], ["-t", "nfs", "-o"]);
        assert!(c.args[3].starts_with("vers=3,proto=tcp,port=7,mountport=7,mountproto=tcp,soft,"));
        assert!(c.args[3].contains(",nolock,noac,"));
        assert_eq!(&c.args[4..], ["127.0.0.1:/", "/home/pi/p"]);
    }

    #[test]
    fn unmount_commands() {
        let at = Path::new("/x/p");
        assert_eq!(unmount(Os::MacOs, false, at, false).args, ["/x/p"]);
        assert_eq!(unmount(Os::MacOs, false, at, true).args, ["-f", "/x/p"]);
        let f = unmount(Os::Linux, true, at, true);
        assert_eq!(f.program, Path::new("fusermount3"));
        assert_eq!(f.args, ["-u", "-z", "/x/p"]);
        assert_eq!(unmount(Os::Linux, false, at, true).args, ["-f", "-l", "/x/p"]);
    }

    #[test]
    fn mount_point_inspection() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().canonicalize().unwrap();
        // Missing: created from the first missing ancestor down.
        let deep = base.join("a/b/proj");
        assert_eq!(inspect(Os::Linux, &deep, false).unwrap(), MountPoint::Create(vec![base.join("a"), base.join("a/b"), deep.clone()]));
        // Empty directory: ready. Non-empty: refused unless shadowing is allowed.
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(inspect(Os::Linux, &deep, false).unwrap(), MountPoint::Ready);
        std::fs::write(deep.join("f"), "x").unwrap();
        assert!(matches!(inspect(Os::Linux, &deep, false), Err(MountPointError::NotEmpty(_))));
        assert_eq!(inspect(Os::Linux, &deep, true).unwrap(), MountPoint::Ready);
        assert!(matches!(inspect(Os::Linux, &deep.join("f"), true), Err(MountPointError::NotADirectory(_))));
    }

    #[test]
    fn setup_hints_name_the_privileged_step() {
        assert!(setup_hint(Os::MacOs, Path::new("/home/pi/proj")).contains("auto_master"));
        assert!(setup_hint(Os::MacOs, Path::new("/srv/proj")).contains("synthetic.conf"));
        let h = setup_hint(Os::MacOs, Path::new("/Users/other/proj"));
        assert!(h.contains("sudo mkdir -p /Users/other/proj") && !h.contains("synthetic"));
        assert!(setup_hint(Os::Linux, Path::new("/Users/me/proj")).contains("sudo chown"));
    }
}
